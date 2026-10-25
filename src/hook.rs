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

// Bash `DIBS_OVERRIDE='why' cmd` or PowerShell `$env:DIBS_OVERRIDE = 'why'; cmd`, only at the very start
pub fn override_of(cmd: &str) -> (Option<String>, &str) {
    static B: OnceLock<Regex> = OnceLock::new();
    static P: OnceLock<Regex> = OnceLock::new();
    let bash = B.get_or_init(|| Regex::new(r#"^\s*DIBS_OVERRIDE=(?:'([^']*)'|"([^"]*)"|([^\s;'"]+))?(?:\s+|$)"#).unwrap());
    let ps = P.get_or_init(|| Regex::new(r#"^\s*\$[Ee][Nn][Vv]:DIBS_OVERRIDE\s*=\s*(?:'([^']*)'|"([^"]*)")\s*;"#).unwrap());
    for re in [bash, ps] {
        if let Some(c) = re.captures(cmd) {
            let why = (1..=3).find_map(|i| c.get(i)).map_or("", |m| m.as_str()).trim();
            return ((!why.is_empty()).then(|| why.to_string()), &cmd[c.get(0).map_or(0, |m| m.end())..]);
        }
    }
    (None, cmd)
}

fn overrides_file() -> PathBuf {
    state("overrides.jsonl")
}

// newest first
pub fn overrides(since: f64) -> Vec<Value> {
    let text = std::fs::read_to_string(overrides_file()).unwrap_or_default();
    let mut out: Vec<Value> =
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| core::jf(v, "t").unwrap_or(0.0) >= since).collect();
    out.reverse();
    out
}

fn log_override(sid: &str, agent: &str, why: &str, cmd: &str) {
    use std::io::Write;
    let p = overrides_file();
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > 256 * 1024) {
        let keep: Vec<String> = overrides(now() - 86400.0).iter().rev().map(Value::to_string).collect();
        let _ = std::fs::write(&p, keep.join("\n") + "\n");
    }
    // only on an override, so the process walk stays off the hook's fast path
    let name = jstr(&core::session_info(core::claude_pid(None), sid), "name").to_string();
    let line = json!({"t": now(), "session": sid, "agent": agent, "name": name, "reason": why,
                      "cmd": cmd.chars().take(120).collect::<String>()});
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{line}");
    }
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

struct Reason {
    text: String,
    wait: String,
}

// Only recognized heavy launches are subject to normal-mode pressure admission.
fn heavy(cmd: &str, cfg: &Cfg) -> bool {
    let (verb, _, _) = invocation(cmd);
    if verb.as_deref().is_some_and(|v| v == "run" || v == "hold") { return true; }
    if cfg.matches.values().any(|r| r.is_match(cmd)) { return true; }
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, r"(?:^|[;&|\n]\s*|\s)(?:cargo\s+(?:build|test|bench)|npm\s+(?:run\s+build|test)|ninja|make|cmake\s+--build|python[0-9.]*\s+\S*(?:train|bench)\S*)\b").is_match(cmd)
}

// the second value is the override reason, when it is what let the command past the pressure check
fn blockers(cmd: &str, sid: &str, cfg: &Cfg, agent: &str) -> (Vec<Reason>, Option<String>) {
    let (why, cmd) = override_of(cmd);
    let (verb, names, toks) = invocation(cmd);
    let measure = toks.iter().any(|t| t == "--measure");
    let skip = match &verb {
        Some(v) => PASSIVE.contains(&v.as_str()),
        None => segments(cmd).iter().all(|s| trivial().is_match(s)),
    };
    if skip {
        return (vec![], None);
    }
    let (mut out, held) = (vec![], locks::held_names());
    let table = if held.is_empty() { Default::default() } else { sys::procs() };
    let m = if held.iter().any(|n| n == MACHINE) { locks::blocking(MACHINE, 1, Some(sid), Some(&table), Some(agent)) } else { vec![] };
    if let Some(h) = m.first() {
        out.push(Reason { text: format!("the whole machine is reserved for a measurement by {}", describe(&h.o)), wait: "--measure".into() });
    } else {
        for name in held.iter().filter(|n| *n != MACHINE) {
            let matched = cfg.matches.get(name).is_some_and(|r| r.is_match(cmd));
            // The measurement CLI owns draining active work after taking the
            // machine gate. A shared-resource wait would otherwise return FREE
            // immediately and cause an endless hook retry.
            if measure || !(names.contains(name) || matched) {
                continue;
            }
            let b = locks::policy_blocking(name, cfg, Some(sid), Some(&table), Some(agent));
            if let Some(first) = b.first() {
                out.push(Reason { text: format!("{name} is held by {} (exclusive resource)", describe(&first.o)), wait: name.clone() });
            }
        }
    }
    let mut used = None;
    if cfg.flag("hook", "pressure") && heavy(cmd, cfg) {
        let p = probes::sustained_pressure(cfg);
        if !p.is_empty() {
            if why.is_some() {
                used = why;
            } else {
                out.push(Reason { text: format!("the machine is under pressure (sustained load; new heavy work): {}", p.join(", ")), wait: "--pressure".into() });
            }
        }
    }
    (out, used)
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
            names.extend(locks::held_names());
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
    let escape = if pressure {
        "\nOnly the pressure part can be overridden, and only when waiting costs money (a billed cloud resource sits idle) or the user told you not to wait: rerun as DIBS_OVERRIDE='<why>' <command> (PowerShell: $env:DIBS_OVERRIDE='<why>'; <command>). dibs status shows every override."
    } else {
        ""
    };
    deny_text(&format!(
        "WAIT, not a refusal: {}.\nRun in a background Bash: {cmd}\nWhen it prints FREE, rerun this exact command unchanged. Tell the user what you're waiting on and until when.\nDon't work around it (different tool, renamed command, or skipping the check). Meanwhile, work that doesn't need the machine (reading, editing) is fine.{escape}",
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
        names.extend(locks::held_names());
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
    let plain = override_of(cmd).1;
    if event == "PostToolUse" {
        stamp_hold(plain, &data, sid, agent, &cfg);
        return Ok(());
    }
    seen(sid);
    if let Some(h) = foreign_holds(plain, sid, &cfg, agent).first() {
        deny_foreign(&invocation(plain).0.unwrap_or_default(), h);
        return Ok(());
    }
    let (r, used) = blockers(cmd, sid, &cfg, agent);
    if r.is_empty() {
        if let Some(why) = used { log_override(sid, agent, &why, cmd); }
        note_hold(plain, &data, agent);
    } else {
        deny(&r);
    }
    Ok(())
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
