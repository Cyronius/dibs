use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use regex_lite::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::core::{now, read_json, state, write_json, Cfg};
use crate::sys;

const GPU_IGNORE: [&str; 7] = ["dwm.exe", "explorer.exe", "code.exe", "chrome.exe", "msedge.exe", "firefox.exe", "claude.exe"];

fn sleep(s: f64) {
    thread::sleep(Duration::from_secs_f64(s));
}

pub fn cpu_pct() -> f64 {
    // one reading shared by every session's hook, so most calls cost no sleep
    let Some(mut cur) = sys::cpu_sample() else { return sys::cpu_busy(None, None) };
    let sample = |v: &Value| -> Option<[f64; 3]> {
        let a = v.as_array()?;
        Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?])
    };
    let cache = read_json(&state("pressure.json"));
    let cached = cache.as_ref().and_then(|c| Some((sample(c.get("s")?)?, c.get("pct")?.as_f64()?)));
    if let Some((s, pct)) = cached {
        if cur[0] - s[0] < 2.0 {
            return pct;
        }
    }
    let prev = match cached {
        Some((s, _)) if cur[0] - s[0] <= 60.0 => s,
        _ => {
            let p = cur;
            sleep(0.3);
            cur = sys::cpu_sample().unwrap_or(p);
            p
        }
    };
    let pct = sys::cpu_busy(Some(prev), Some(cur));
    write_json(&state("pressure.json"), &json!({"s": cur, "pct": pct}));
    pct
}

pub fn pressure(cfg: &Cfg) -> Vec<String> {
    let (avail, commit) = sys::mem();
    let (mut out, cpu) = (vec![], cpu_pct());
    if cpu > cfg.f("machine", "cpu_max") {
        out.push(format!("CPU {cpu:.0}% (limit {}%)", cfg.f("machine", "cpu_max")));
    }
    if let Some(a) = avail.filter(|a| *a < cfg.f("machine", "ram_floor_gb")) {
        out.push(format!("free RAM {a:.1} GB (floor {} GB)", cfg.f("machine", "ram_floor_gb")));
    }
    if let Some(c) = commit.filter(|c| *c > cfg.f("machine", "commit_max")) {
        out.push(format!("memory commit {c:.0}% (limit {}%)", cfg.f("machine", "commit_max")));
    }
    out
}

fn capture(spec: &Value, timeout_s: f64) -> Option<String> {
    let mut c = match spec {
        Value::String(s) if cfg!(windows) => {
            let mut c = Command::new("cmd");
            c.args(["/C", s]);
            c
        }
        Value::String(s) => {
            let mut c = Command::new("sh");
            c.args(["-c", s]);
            c
        }
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            let mut c = Command::new(parts.first()?);
            c.args(&parts[1..]);
            c
        }
        _ => return None,
    };
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut buf = vec![];
        let _ = pipe.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let end = now() + timeout_s;
    while child.try_wait().ok()?.is_none() {
        if now() > end {
            let _ = child.kill();
            break;
        }
        sleep(0.05);
    }
    let _ = child.wait();
    reader.join().ok()
}

fn ignore_rx(items: &Value) -> Option<Regex> {
    let pats: Vec<&str> = items.as_array()?.iter().filter_map(Value::as_str).collect();
    (!pats.is_empty()).then(|| RegexBuilder::new(&pats.join("|")).case_insensitive(true).build().ok()).flatten()
}

fn command_users(p: &Value, mine: &HashSet<u32>) -> Vec<String> {
    let Some(out) = capture(&p["command"], p["timeout_s"].as_f64().unwrap_or(60.0)) else { return vec![] };
    let ignore = ignore_rx(&p["ignore"]);
    let mut found = vec![];
    match p["parser"].as_str().unwrap_or("lines") {
        "xrt" => {
            // `xrt-smi examine -r all`: a |pid |ctx| row, then the process name
            let row = Regex::new(r"^\s*\|(\d+)\s*\|(\d+)\s*\|").unwrap();
            let lines: Vec<&str> = out.lines().collect();
            for (i, ln) in lines.iter().enumerate().take(lines.len().saturating_sub(1)) {
                if let Some(m) = row.captures(ln) {
                    let pid: u32 = m[1].parse().unwrap_or(0);
                    if !mine.contains(&pid) {
                        let name = lines[i + 1].trim().trim_matches('|').split('|').next().unwrap_or("").trim();
                        found.push(format!("{name}:{pid} NPU context"));
                    }
                }
            }
        }
        "nvidia" => {
            // nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader
            for ln in out.lines() {
                let (pid, name) = ln.split_once(',').unwrap_or((ln, ""));
                if let Ok(p) = pid.trim().parse::<u32>() {
                    if !mine.contains(&p) {
                        let base = name.trim().rsplit(['/', '\\']).next().unwrap_or("");
                        found.push(format!("{base}:{}", pid.trim()));
                    }
                }
            }
        }
        _ => found = out.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| l.chars().take(80).collect()).collect(),
    }
    let mut v: Vec<String> = found.into_iter().filter(|f| !ignore.as_ref().is_some_and(|r| r.is_match(f))).collect();
    v.sort();
    v.dedup();
    v
}

pub fn activity(cfg: &Cfg, names: &[String], window: f64, quiet_cpu: Option<f64>) -> Vec<String> {
    let specs: Vec<(&String, &Value)> = names
        .iter()
        .map(|n| (n, cfg.res(n)))
        .filter(|(_, s)| s.get("probe").is_some_and(|p| !p.is_null()) || s.get("resident").is_some_and(|r| !r.is_null()))
        .collect();
    if specs.is_empty() && quiet_cpu.is_none() {
        return vec![];
    }
    let table = sys::procs();
    let mut mine = sys::tree(std::process::id(), Some(&table));
    mine.extend(sys::ancestors(Some(&table)));
    let need = specs.iter().any(|(_, s)| s["probe"]["kind"] == "process");
    let a = if need { sys::proc_times() } else { HashMap::new() };
    let s0 = sys::cpu_sample();
    sleep(window);
    let b = if need { sys::proc_times() } else { HashMap::new() };
    let s1 = sys::cpu_sample();
    let mut found = vec![];
    for (n, s) in &specs {
        let p = &s["probe"];
        let kind = p["kind"].as_str().or_else(|| (!p["command"].is_null()).then_some("command")).unwrap_or("");
        match kind {
            "process" => {
                let rx = ignore_rx(&p["names"]);
                let min = p["min_pct"].as_f64().unwrap_or(5.0) / 100.0 * window;
                for (pid, (exe, t)) in &b {
                    let Some((_, t0)) = a.get(pid) else { continue };
                    if !mine.contains(pid) && rx.as_ref().is_some_and(|r| r.is_match(exe)) && t - t0 > min {
                        found.push(format!("{n}: {exe}:{pid} {:.0}% CPU", 100.0 * (t - t0) / window));
                    }
                }
            }
            "command" => found.extend(command_users(p, &mine).into_iter().map(|u| format!("{n}: {u}"))),
            "gpu-counters" => {
                let mut ign: HashSet<String> = GPU_IGNORE.iter().map(|s| s.to_string()).collect();
                ign.extend(p["ignore"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase));
                for (pid, v) in sys::gpu_by_pid(1.0) {
                    let exe = table.get(&pid).map(|e| e.1.clone()).unwrap_or_else(|| "?".into());
                    if v >= p["min_pct"].as_f64().unwrap_or(20.0) && !mine.contains(&pid) && !ign.contains(&exe) {
                        found.push(format!("{n}: {exe}:{pid} GPU {v:.0}%"));
                    }
                }
            }
            "net-counters" => {
                if let Some(mbps) = sys::net_mbps(1.0).filter(|m| *m >= p["busy_mbps"].as_f64().unwrap_or(400.0)) {
                    found.push(format!("{n}: link busy at {mbps:.0} Mbit/s"));
                }
            }
            _ => {}
        }
        let res: HashSet<String> = s["resident"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase).collect();
        for (pid, (_, exe)) in &table {
            if res.contains(exe) && !mine.contains(pid) {
                found.push(format!("{n}: {exe}:{pid} resident"));
            }
        }
    }
    if let Some(q) = quiet_cpu {
        let busy = sys::cpu_busy(s0, s1);
        if busy > q {
            found.push(format!("total CPU {busy:.0}% (quiet means under {q}%)"));
        }
    }
    found.sort();
    found.dedup();
    found
}

pub fn wait_quiet(cfg: &Cfg, names: &[String], quiet_cpu: Option<f64>) {
    let mut last = 0.0;
    loop {
        let r = activity(cfg, names, if quiet_cpu.is_some() { 10.0 } else { 3.0 }, quiet_cpu);
        if r.is_empty() {
            return;
        }
        if now() - last > 300.0 {
            println!("dibs: waiting for other work to finish: {}", r.join("; "));
            last = now();
        }
    }
}

pub struct Monitor {
    pub found: Arc<Mutex<HashSet<String>>>,
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(cfg: &Cfg, names: &[String]) -> Monitor {
        let (cfg, names) = (cfg.clone(), names.to_vec());
        let found = Arc::new(Mutex::new(HashSet::new()));
        let (tx, rx) = mpsc::channel::<()>();
        let sink = found.clone();
        let every = 30.0;
        let thread = thread::spawn(move || {
            let ignore: HashSet<String> =
                cfg.v["measure"]["ignore"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase).collect();
            let limit = cfg.f("measure", "noise_core_pct") / 100.0 * every;
            let mut prev = sys::proc_times();
            while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs_f64(every)) {
                let table = sys::procs();
                let mut mine = sys::tree(std::process::id(), Some(&table));
                mine.extend(sys::ancestors(Some(&table)));
                let cur = sys::proc_times();
                let mut hits = vec![];
                for (pid, (exe, t)) in &cur {
                    if !mine.contains(pid) && !ignore.contains(exe) && prev.get(pid).is_some_and(|p| t - p.1 > limit) {
                        hits.push(format!("{exe}:{pid}"));
                    }
                }
                hits.extend(activity(&cfg, &names, 0.5, None));
                sink.lock().unwrap().extend(hits);
                prev = cur;
            }
        });
        Monitor { found, stop: tx, thread: Some(thread) }
    }

    pub fn finish(mut self) -> HashSet<String> {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.found.lock().unwrap().clone()
    }
}
