use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::process::Command;

use super::Procs;
use crate::core::now;

fn ps(fields: &str) -> Vec<Vec<String>> {
    let out = Command::new("ps").args(["-A", "-o", fields]).output().map(|o| o.stdout).unwrap_or_default();
    let n = fields.split(',').count();
    String::from_utf8_lossy(&out)
        .lines()
        .map(|l| {
            let mut fields = l.split_whitespace();
            let mut row: Vec<String> = fields.by_ref().take(n - 1).map(String::from).collect();
            row.push(fields.collect::<Vec<_>>().join(" "));
            row
        })
        .filter(|r| r.len() == n && r.last().is_some_and(|s| !s.is_empty()))
        .collect()
}

fn base(comm: &str) -> String {
    comm.rsplit('/').next().unwrap_or(comm).to_lowercase()
}

pub fn procs() -> Procs {
    let mut out = HashMap::new();
    for r in ps("pid=,ppid=,comm=") {
        if let (Ok(p), Ok(pp)) = (r[0].parse::<u32>(), r[1].parse::<u32>()) {
            out.insert(p, (pp, base(&r[2])));
        }
    }
    out
}

pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn cputime(s: &str) -> f64 {
    let (days, rest) = s.rsplit_once('-').map(|(d, r)| (d.parse::<f64>().unwrap_or(0.0), r)).unwrap_or((0.0, s));
    rest.split(':').fold(0.0, |acc, p| acc * 60.0 + p.parse::<f64>().unwrap_or(0.0)) + days * 86400.0
}

pub fn proc_times() -> HashMap<u32, (String, f64)> {
    ps("pid=,time=,comm=")
        .into_iter()
        .filter_map(|r| Some((r[0].parse::<u32>().ok()?, (base(&r[2]), cputime(&r[1])))))
        .collect()
}

pub fn lower_priority(pid: u32) -> bool {
    unsafe {
        let cur = libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t);
        libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, cur.max(10)) == 0
    }
}

pub fn normal_priority_flags() -> u32 {
    0 // a child can't undo an inherited nice without privileges
}

pub fn new_group_flags() -> u32 {
    0
}

pub fn ask_to_stop(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

pub fn ignore_interrupts() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
}

pub fn cpu_sample() -> Option<[f64; 3]> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let f: Vec<f64> = text.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    (f.len() >= 4).then(|| [now(), f[3] + f.get(4).copied().unwrap_or(0.0), f.iter().sum()])
}

pub fn loadavg_pct() -> f64 {
    let mut l = [0f64; 1];
    unsafe {
        libc::getloadavg(l.as_mut_ptr(), 1);
    }
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    (100.0 * l[0] / cpus).min(100.0)
}

pub fn mem() -> (Option<f64>, Option<f64>) {
    if let Ok(t) = fs::read_to_string("/proc/meminfo") {
        let kb = t.lines().find(|l| l.starts_with("MemAvailable:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok());
        return (kb.map(|k| k / 2f64.powi(20)), None);
    }
    if cfg!(target_os = "macos") {
        let out = Command::new("vm_stat").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let num = |key: &str| -> Option<f64> {
            let rest = &out[out.find(key)? + key.len()..];
            rest.trim_start_matches(':').split_whitespace().next()?.trim_end_matches('.').parse().ok()
        };
        let page = out.split("page size of ").nth(1).and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
        if let (Some(page), Some(a), Some(b), Some(c)) = (page, num("Pages free"), num("Pages inactive"), num("Pages speculative")) {
            return (Some((a + b + c) * page / 2f64.powi(30)), None);
        }
    }
    (None, None)
}

pub fn local_hhmm(t: f64) -> String {
    unsafe {
        let secs = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

pub fn disk_free_gb(p: &Path) -> f64 {
    let Ok(c) = CString::new(p.to_string_lossy().as_bytes()) else { return 0.0 };
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return 0.0;
        }
        s.f_bavail as f64 * s.f_frsize as f64 / 2f64.powi(30)
    }
}

pub fn gpu_by_pid(_sample_s: f64) -> HashMap<u32, f64> {
    HashMap::new()
}

fn net_bytes() -> Option<f64> {
    let text = fs::read_to_string("/proc/net/dev").ok()?;
    let mut total = 0.0;
    for l in text.lines().skip(2) {
        let (name, data) = l.split_once(':')?;
        if name.trim() != "lo" {
            let f: Vec<f64> = data.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            total += f.first()? + f.get(8)?;
        }
    }
    Some(total)
}

pub fn net_mbps(sample_s: f64) -> Option<f64> {
    let a = net_bytes()?;
    std::thread::sleep(std::time::Duration::from_secs_f64(sample_s));
    Some((net_bytes()? - a) * 8.0 / 1e6 / sample_s)
}
