use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use regex_lite::{Regex, RegexBuilder};
use serde_json::{json, Value};

use crate::core::{self, describe, jstr, now, read_json, state, write_json, Cfg, MACHINE, R};
use crate::{locks, probes, sys};

const PASSIVE: [&str; 10] = ["status", "wait", "release", "extend", "disk", "nice", "doctor", "help", "version", "session-start"];
const VALUE_FLAGS: [&str; 5] = ["--label", "--tag", "--eta", "--path", "--disk"];

fn rx(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| RegexBuilder::new(pat).case_insensitive(true).build().unwrap())
}

fn trivial() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, r"^\s*(?:cd|ls|dir|cat|type|head|tail|grep|rg|find|wc|pwd|echo|which|where|date|true|Get-Content|Get-ChildItem|Get-Item|Get-Location|Select-String|Test-Path|Write-Host|Write-Output|git\s+(?:status|log|diff|show|branch|rev-parse|remote|config\s+--get|ls-files|blame)|gh\s+(?:pr|issue|run|repo)\s+(?:view|list|status|checks|diff))\b")
}

fn invoke() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, r#"^\s*(?:&\s*)?["']?(?:[^\s"']*[\\/])?dibs(?:\.exe)?["']?\s+([a-z-]+)(.*)$"#)
}

fn segments(cmd: &str) -> Vec<&str> {
    static R: OnceLock<Regex> = OnceLock::new();
    let split = R.get_or_init(|| Regex::new(r"&&|\|\||[;|\n]").unwrap());
    split.split(cmd).filter(|s| !s.trim().is_empty()).collect()
}

// only a segment that starts by running dibs counts; a mention inside a string or heredoc doesn't
pub fn invocation(cmd: &str) -> (Option<String>, Vec<String>, Vec<String>) {
    for s in segments(cmd) {
        let Some(m) = invoke().captures(s) else { continue };
        let rest = m.get(2).map_or("", |g| g.as_str());
        let pre = rest.split(" -- ").next().unwrap_or("");
        let toks: Vec<String> = shlex::split(pre).unwrap_or_else(|| pre.split_whitespace().map(String::from).collect());
        let (mut names, mut skip) = (vec![], false);
        for t in &toks {
            if skip {
                skip = false;
            } else if VALUE_FLAGS.contains(&t.as_str()) {
                skip = true;
            } else if !t.starts_with('-') {
                names.push(t.to_lowercase());
            }
        }
        return (Some(m[1].to_lowercase()), names, toks);
    }
    (None, vec![], vec![])
}

#[derive(PartialEq)]
enum Clears {
    At(f64),
    Unknown,
    Pressure,
}

struct Reason {
    text: String,
    clears: Clears,
    wait: String,
}

fn blockers(cmd: &str, sid: &str, cfg: &Cfg, agent: &str) -> Vec<Reason> {
    let (verb, names, toks) = invocation(cmd);
    let measure = toks.iter().any(|t| t == "--measure");
    let skip = match &verb {
        Some(v) => PASSIVE.contains(&v.as_str()),
        None => segments(cmd).iter().all(|s| trivial().is_match(s)),
    };
    if skip {
        return vec![];
    }
    let (mut out, held) = (vec![], locks::held_names());
    let table = if held.is_empty() { Default::default() } else { sys::procs() };
    let m = if held.iter().any(|n| n == MACHINE) { locks::blocking(MACHINE, 1, Some(sid), Some(&table), Some(agent)) } else { vec![] };
    if let Some(h) = m.first() {
        let clears = h.eta().map_or(Clears::Unknown, Clears::At);
        out.push(Reason { text: format!("the whole machine is reserved for a measurement by {}", describe(&h.o)), clears, wait: "--measure".into() });
    } else {
        for name in held.iter().filter(|n| *n != MACHINE) {
            let matched = cfg.matches.get(name).is_some_and(|r| r.is_match(cmd));
            if !(names.contains(name) || measure || matched) {
                continue;
            }
            let b = locks::blocking(name, cfg.capacity(name), Some(sid), Some(&table), Some(agent));
            if let Some(first) = b.first() {
                let eta = b.iter().map(|h| h.eta().unwrap_or(9e99)).fold(f64::MAX, f64::min);
                out.push(Reason { text: format!("{name} is held by {}", describe(&first.o)), clears: Clears::At(eta), wait: name.clone() });
            }
        }
    }
    if cfg.flag("hook", "pressure") && !(!held.is_empty() && !locks::session_holds(sid, agent).is_empty()) {
        // a holder owns the machine, so pressure doesn't hold it back
        let p = probes::pressure(cfg);
        if !p.is_empty() {
            out.push(Reason { text: format!("the machine is under pressure: {}", p.join(", ")), clears: Clears::Pressure, wait: "--pressure".into() });
        }
    }
    out
}

// holds a subagent must not end or extend: its siblings' and the main agent's
fn foreign_holds(cmd: &str, sid: &str, cfg: &Cfg, agent: &str) -> Vec<locks::Holder> {
    let (verb, mut names, toks) = invocation(cmd);
    let has = |f: &str| toks.iter().any(|t| t == f);
    let verb = verb.unwrap_or_default();
    if agent.is_empty() || !(verb == "release" || verb == "extend") || has("--force") {
        return vec![];
    }
    if verb == "release" {
        if has("--measure") {
            names.push(MACHINE.into());
            names.extend(cfg.resources());
        }
        if has("--all") {
            names.extend(locks::held_names());
        }
    }
    names.sort();
    names.dedup();
    names.iter().flat_map(|n| locks::holders(n)).filter(|h| h.s("session") == sid && h.is_hold() && h.s("agent") != agent).collect()
}

fn deny_text(msg: &str) {
    println!("{}", json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": msg}}));
}

fn deny_foreign(verb: &str, h: &locks::Holder) {
    deny_text(&format!(
        "{} is held by {}; only that agent, or the main agent, can {verb} it. Leave it, or ask the agent that took it. If the user told you to take it over, pass --force.",
        h.s("resource"),
        describe(&h.o)
    ));
}

fn deny(reasons: &[Reason]) {
    let mut waits: Vec<&str> = vec![];
    for r in reasons.iter().filter(|r| r.wait != "--pressure") {
        if !waits.contains(&r.wait.as_str()) {
            waits.push(&r.wait);
        }
    }
    let pressure = reasons.iter().any(|r| r.wait == "--pressure");
    let mut cmd = format!("dibs wait {}", if waits.is_empty() { "--pressure".to_string() } else { waits.join(" ") });
    if !waits.is_empty() && pressure {
        cmd.push_str(" && dibs wait --pressure");
    }
    deny_text(&format!(
        "WAIT, not a refusal: {}.\nRun in a background Bash: {cmd}\nWhen it prints FREE, rerun this exact command unchanged. Tell the user what you're waiting on and until when.\nDon't work around it (different tool, renamed command, or skipping the check). Meanwhile, work that doesn't need the machine (reading, editing) is fine.",
        reasons.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("; ")
    ));
}

fn seen(sid: &str) {
    let mut s = read_json(&state("hook.json")).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    if now() - s.get(sid).and_then(Value::as_f64).unwrap_or(0.0) > 60.0 {
        s.retain(|_, t| now() - t.as_f64().unwrap_or(0.0) < 86400.0);
        s.insert(sid.to_string(), json!(now()));
        write_json(&state("hook.json"), &Value::Object(s));
    }
}

fn call_file(data: &Value) -> Option<PathBuf> {
    let id: String = jstr(data, "tool_use_id").chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    (!id.is_empty()).then(|| state("hold-calls").join(format!("{id}.json")))
}

// the CLI can't tell which subagent ran `dibs hold`; remember when the call began so PostToolUse can stamp it
fn note_hold(cmd: &str, data: &Value, agent: &str) {
    let Some(p) = call_file(data) else { return };
    if agent.is_empty() || invocation(cmd).0.as_deref() != Some("hold") {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(state("hold-calls")) {
        for e in rd.flatten() {
            if e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d > Duration::from_secs(3600)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    write_json(&p, &json!({"t": now()}));
}

fn stamp_hold(cmd: &str, data: &Value, sid: &str, agent: &str, cfg: &Cfg) {
    let Some(p) = call_file(data) else { return };
    let Some(began) = read_json(&p).and_then(|v| v.get("t").and_then(Value::as_f64)) else { return };
    if agent.is_empty() {
        return;
    }
    let _ = std::fs::remove_file(&p);
    let (_, mut names, toks) = invocation(cmd);
    if toks.iter().any(|t| t == "--measure") {
        names.push(MACHINE.into());
        names.extend(cfg.resources());
    }
    names.sort();
    names.dedup();
    locks::stamp_agent(sid, agent, &names, began);
}

fn run() -> R<()> {
    let mut input = vec![];
    let _ = std::io::stdin().read_to_end(&mut input);
    let data: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
    let cmd = data["tool_input"]["command"].as_str().unwrap_or("");
    let (sid, agent) = (jstr(&data, "session_id"), jstr(&data, "agent_id"));
    let event = data.get("hook_event_name").and_then(Value::as_str).unwrap_or("PreToolUse");
    let cfg = core::load_config(data.get("cwd").and_then(Value::as_str))?;
    if !cfg.flag("hook", "enabled") {
        return Ok(());
    }
    if event == "SubagentStop" {
        if !agent.is_empty() {
            locks::release_agent(sid, agent);
        }
        return Ok(());
    }
    if cmd.trim().is_empty() {
        return Ok(());
    }
    if event == "PostToolUse" {
        stamp_hold(cmd, &data, sid, agent, &cfg);
        return Ok(());
    }
    seen(sid);
    if let Some(h) = foreign_holds(cmd, sid, &cfg, agent).first() {
        deny_foreign(&invocation(cmd).0.unwrap_or_default(), h);
        return Ok(());
    }
    let deadline = now() + cfg.f("hook", "wait_inline_s");
    loop {
        let r = blockers(cmd, sid, &cfg, agent);
        if r.is_empty() {
            note_hold(cmd, &data, agent);
            return Ok(());
        }
        let far = r.iter().any(|x| match x.clears {
            Clears::Pressure => false,
            Clears::Unknown => true,
            Clears::At(t) => t > deadline,
        });
        if now() >= deadline || far {
            deny(&r);
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

// a broken guard must never stop work
pub fn main() -> i32 {
    match std::panic::catch_unwind(run) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("dibs hook error (command allowed): {}", e.0),
        Err(_) => eprintln!("dibs hook error (command allowed): internal error"),
    }
    0
}
