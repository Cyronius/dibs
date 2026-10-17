use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::core::{self, describe, hhmm, jbool, jf, jstr, now, read_json, state, write_json, Cfg, Me, Problem, MACHINE, R};
use crate::{hook, locks, probes, sys, VERSION};

const VALUE_FLAGS: [&str; 6] = ["--label", "--tag", "--eta", "--path", "--disk", "--pid"];

fn usage() -> String {
    format!(
        "dibs {VERSION}: take turns with shared resources on this machine.

  dibs run <res ...> [--label <what>] [--tag <worker>] [--eta 40m] [--new] -- <cmd ...>
        run <cmd> holding the resources; they free when it exits
  dibs run <res ...> --preemptible [--requeue] [--yield-signal] -- <cmd ...>
        when another session queues, create $DIBS_YIELD_FILE (and with --yield-signal send
        SIGTERM / CTRL_BREAK); with --requeue, exit code 75 hands over, then reruns <cmd>
  dibs run --measure [<res ...>] [--eta 30m] -- <cmd ...>
        take the whole machine, wait until it's quiet, run <cmd>, print CLEAN or NOISY
  dibs hold <res ...> | --measure  --eta 45m [--label <what>] [--pid <n>]
        keep resources across several commands (--pid: lasts while process <n> runs, past the ETA)
  dibs release <res ...> | --measure | --all [--force]           end a hold (--force: someone else's)
  dibs extend <res ...> --eta 20m                                push out your hold's ETA
  dibs wait <res ...> | --measure | --pressure | --disk <GB>     block until free, print one FREE line
  dibs status [--activity]                                       who holds what, until when, who waits
  dibs disk claim <GB> --eta 2h [--path <dir>] | check <GB> | release [--all]
  dibs doctor                                                    check the setup
  dibs nice [--all]                                              lower Claude Code's CPU priority

A resource is any name: gpu, npu, network, testdb, port-3000. Your session's name is recorded for you.
Config (optional): {} and .dibs.toml in a repo.
",
        core::user_config().display()
    )
}

fn has(a: &[String], f: &str) -> bool {
    a.iter().any(|x| x == f)
}

fn opt<'a>(a: &'a [String], name: &str) -> Option<&'a str> {
    let i = a.iter().position(|x| x == name)?;
    a.get(i + 1).map(String::as_str)
}

fn positional(a: &[String]) -> Vec<String> {
    let (mut out, mut skip) = (vec![], false);
    for x in a {
        if skip {
            skip = false;
        } else if VALUE_FLAGS.contains(&x.as_str()) {
            skip = true;
        } else if !x.starts_with('-') {
            out.push(x.clone());
        }
    }
    out
}

fn me_from(a: &[String]) -> Me {
    core::who(opt(a, "--label"), opt(a, "--tag"))
}

fn eta_from(a: &[String], required: bool) -> R<Option<f64>> {
    match opt(a, "--eta") {
        Some(e) => core::parse_dur(e).map(Some),
        None if required => Err(Problem::new("a hold needs --eta, e.g. --eta 40m")),
        None => Ok(None),
    }
}

type Target = (String, usize, bool);

fn targets(a: &[String], cfg: &Cfg) -> R<Vec<Target>> {
    let names = core::resolve(&positional(a), cfg, has(a, "--new"))?;
    if has(a, "--measure") {
        let mut every: Vec<String> = names.into_iter().chain(cfg.resources()).chain([MACHINE.to_string()]).collect();
        every.sort();
        every.dedup();
        return Ok(every.into_iter().map(|n| (cfg.capacity(&n), n)).map(|(c, n)| (n, c, true)).collect());
    }
    if names.is_empty() {
        return Err(Problem::new("name at least one resource, e.g. `dibs run gpu -- python train.py`"));
    }
    Ok(names.into_iter().map(|n| (cfg.capacity(&n), n)).map(|(c, n)| (n, c, false)).collect())
}

fn wait_for_machine() {
    let mut last = 0.0;
    loop {
        let b = locks::blocking(MACHINE, 1, None, None, None);
        let Some(h) = b.first() else { return };
        if now() - last > 300.0 {
            println!("dibs: waiting, the machine is reserved for a measurement by {}", describe(&h.o));
            last = now();
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn exit_code(s: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return 128 + sig;
        }
    }
    s.code().unwrap_or(1)
}

struct Yield<'a> {
    names: &'a [String],
    sid: String,
    signal: bool,
}

fn spawn_and_wait(cmd: &[String], normal_priority: bool, y: Option<&Yield>) -> i32 {
    let exe = if sys::WINDOWS { core::which(&cmd[0]).map(|p| p.to_string_lossy().into_owned()).unwrap_or(cmd[0].clone()) } else { cmd[0].clone() };
    let mut c = Command::new(exe);
    c.args(&cmd[1..]);
    let yield_file = y.map(|_| state("yield").join(format!("{}.{}", std::process::id(), (now() * 1e9) as u64)));
    if let Some(f) = &yield_file {
        let _ = fs::create_dir_all(state("yield"));
        c.env("DIBS_YIELD_FILE", f);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let flags = if normal_priority { sys::normal_priority_flags() } else { 0 } | if y.is_some_and(|y| y.signal) { sys::new_group_flags() } else { 0 };
        if flags != 0 {
            c.creation_flags(flags);
        }
    }
    #[cfg(not(windows))]
    let _ = normal_priority;
    sys::ignore_interrupts();
    let Some(y) = y else {
        return match c.status() {
            Ok(s) => exit_code(s),
            Err(e) => {
                eprintln!("dibs: can't run {}: {e}", cmd[0]);
                127
            }
        };
    };
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            eprintln!("dibs: can't run {}: {e}", cmd[0]);
            return 127;
        }
    };
    let (mut asked, mut tick) = (false, 0u32);
    let rc = loop {
        match child.try_wait() {
            Ok(Some(s)) => break exit_code(s),
            Ok(None) => {}
            Err(_) => break 1,
        }
        std::thread::sleep(Duration::from_millis(200));
        tick += 1;
        if !asked && tick % 5 == 0 && y.names.iter().any(|n| locks::others_waiting(n, &y.sid)) {
            asked = true;
            println!("dibs: another session is waiting; asking {} to yield", cmd[0]);
            if let Some(f) = &yield_file {
                let _ = fs::write(f, "");
            }
            if y.signal {
                sys::ask_to_stop(child.id());
            }
        }
    };
    if let Some(f) = &yield_file {
        let _ = fs::remove_file(f);
    }
    rc
}

// after a yield, let a waiting session take the lock before this one queues again
fn await_handoff(names: &[String], sid: &str) {
    let t0 = now();
    while now() - t0 < 60.0
        && names.iter().any(|n| locks::others_waiting(n, sid) && !locks::holders(n).iter().any(|h| h.s("session") != sid))
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_secs(1));
}

fn cmd_run(a: &[String]) -> R<i32> {
    let split = a.iter().position(|x| x == "--");
    let Some(i) = split.filter(|i| *i + 1 < a.len()) else {
        return Err(Problem::new("put the command after --, e.g. `dibs run gpu -- python train.py`"));
    };
    let (pre, cmd) = (&a[..i], &a[i + 1..]);
    let cfg = core::load_config(None)?;
    let measure = has(pre, "--measure");
    let (tg, me, eta) = (targets(pre, &cfg)?, me_from(pre), eta_from(pre, false)?);
    let names: Vec<String> = tg.iter().filter(|t| t.0 != MACHINE).map(|t| t.0.clone()).collect();
    let quiet = measure.then(|| cfg.f("machine", "quiet_cpu_max"));
    let kind = if measure { "measure" } else { "run" };
    let (preemptible, requeue, signal) = (has(pre, "--preemptible"), has(pre, "--requeue"), has(pre, "--yield-signal"));
    if (requeue || signal) && !preemptible {
        return Err(Problem::new("--requeue and --yield-signal go with --preemptible"));
    }
    if preemptible && measure {
        return Err(Problem::new("a measurement can't be preemptible"));
    }
    let yielder = preemptible.then(|| Yield { names: &names, sid: me.session.clone(), signal });
    let mut held: Vec<PathBuf> = vec![];
    let result = (|| -> R<i32> {
        loop {
            loop {
                if !measure {
                    wait_for_machine();
                }
                probes::wait_quiet(&cfg, &names, quiet);
                for (n, cap, ex) in &tg {
                    let ask = locks::Ask { cmd, cap: *cap, eta, hold: false, kind, exclusive: *ex, watch_pid: None, preemptible };
                    held.extend(locks::acquire(n, &me, &ask)?);
                }
                let r = probes::activity(&cfg, &names, if measure { 10.0 } else { 3.0 }, quiet);
                if r.is_empty() {
                    break;
                }
                println!("dibs: other work started meanwhile ({}); backing off", r.join("; "));
                locks::release(&held);
                held.clear();
            }
            if !measure {
                let rc = spawn_and_wait(cmd, false, yielder.as_ref());
                if requeue && rc == 75 {
                    locks::release(&held);
                    held.clear();
                    println!("dibs: {} yielded; queueing again", names.join(", "));
                    await_handoff(&names, &me.session);
                    continue;
                }
                return Ok(rc);
            }
            println!("dibs: CLEAN");
            let mon = probes::Monitor::start(&cfg, &names);
            let rc = spawn_and_wait(cmd, true, None);
            let mut r: HashSet<String> = probes::activity(&cfg, &names, 10.0, quiet).into_iter().collect();
            r.extend(mon.finish());
            let mut r: Vec<String> = r.into_iter().collect();
            r.sort();
            if r.is_empty() {
                println!("dibs: CLEAN after");
            } else {
                println!("dibs: NOISY after [{}]", r.iter().map(|x| format!("'{x}'")).collect::<Vec<_>>().join(", "));
            }
            return Ok(rc);
        }
    })();
    locks::release(&held);
    result
}

fn cmd_hold(a: &[String]) -> R<i32> {
    let cfg = core::load_config(None)?;
    let (tg, me, eta) = (targets(a, &cfg)?, me_from(a), eta_from(a, true)?.unwrap_or(0.0));
    let have: HashSet<String> = locks::session_holds(&me.session, &me.agent).iter().map(|h| h.s("resource").to_string()).collect();
    if !have.is_empty() && !tg.iter().all(|t| have.contains(&t.0)) {
        let mut h: Vec<&String> = have.iter().collect();
        h.sort();
        println!(
            "dibs: warning: this session already holds {h:?} and now asks for more. Take a hold's whole set at once, or two sessions can deadlock until an ETA passes."
        );
    }
    let watch_pid = match opt(a, "--pid") {
        Some(p) => {
            let n = p.parse::<u32>().map_err(|_| Problem::new("--pid wants a process id, e.g. --pid 4242"))?;
            if !sys::alive(n) {
                return Err(Problem::new(format!("pid {n} isn't running")));
            }
            Some(n)
        }
        None => None,
    };
    for (n, cap, ex) in &tg {
        let ask = locks::Ask {
            cmd: &["hold".to_string()],
            cap: *cap,
            eta: Some(eta),
            hold: true,
            kind: if *ex { "measure" } else { "hold" },
            exclusive: *ex,
            watch_pid,
            preemptible: false,
        };
        locks::acquire(n, &me, &ask)?;
    }
    let all = tg.iter().map(|t| t.0.as_str()).collect::<Vec<_>>();
    let shown = if has(a, "--measure") { "--measure".to_string() } else { all.join(" ") };
    let until = match watch_pid {
        Some(w) => format!("while pid {w} runs (ETA ~{})", hhmm(Some(now() + eta))),
        None => format!("until ~{}", hhmm(Some(now() + eta))),
    };
    println!("HELD: {} {until}. Run `dibs release {shown}` when done.", all.join(", "));
    Ok(0)
}

fn release_names(a: &[String], cfg: &Cfg) -> Vec<String> {
    let mut names: Vec<String> = positional(a).iter().map(|n| n.to_lowercase()).collect();
    if has(a, "--measure") {
        names.push(MACHINE.into());
        names.extend(cfg.resources());
    }
    if has(a, "--all") {
        names.extend(locks::held_names());
    }
    names.sort();
    names.dedup();
    names
}

fn cmd_release(a: &[String]) -> R<i32> {
    let (cfg, sid, agent) = (core::load_config(None)?, core::my_session(), core::my_agent());
    let names = release_names(a, &cfg);
    if names.is_empty() {
        return Err(Problem::new("name what to release, or pass --measure or --all"));
    }
    for n in names {
        let hs = locks::holders(&n);
        let (ours, rest): (Vec<_>, Vec<_>) = hs.iter().partition(|h| core::controls(&h.o, &sid, &agent) || has(a, "--force"));
        locks::release_holders(&ours);
        if !ours.is_empty() {
            println!("{n}: released");
        } else if let (Some(h), false) = (rest.first(), has(a, "--all")) {
            let who = if h.s("session") == sid { "agent" } else { "session" };
            println!("{n}: held by {}; only that {who}, or you with --force, can release it", describe(&h.o));
        } else if !has(a, "--all") {
            println!("{n}: already free");
        }
    }
    Ok(0)
}

fn cmd_extend(a: &[String]) -> R<i32> {
    let eta = core::parse_dur(opt(a, "--eta").unwrap_or("20m"))?;
    let (sid, agent) = (core::my_session(), core::my_agent());
    for n in positional(a).iter().map(|x| x.to_lowercase()) {
        let ours: Vec<_> = locks::holders(&n).into_iter().filter(|h| core::controls(&h.o, &sid, &agent)).collect();
        for h in &ours {
            let mut o = h.o.clone();
            o["eta"] = json!(now() + eta);
            write_json(&h.dir.join("owner.json"), &o);
        }
        if ours.is_empty() {
            println!("{n}: not held by this session");
        } else {
            println!("{n}: ETA now ~{}", hhmm(Some(now() + eta)));
        }
    }
    Ok(0)
}

fn disk_path(cfg: &Cfg) -> PathBuf {
    match cfg.v["machine"]["disk"]["path"].as_str().filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => core::home_dir(),
    }
}

fn dir_gb(p: &Path) -> f64 {
    fn walk(p: &Path) -> u64 {
        fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => walk(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            }).sum())
            .unwrap_or(0)
    }
    walk(p) as f64 / 2f64.powi(30)
}

struct Claim {
    file: PathBuf,
    v: Value,
    outstanding: f64,
}

fn claims(exclude_session: Option<&str>) -> Vec<Claim> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir(state("disk")) else { return out };
    for e in rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")) {
        let p = e.path();
        let c = read_json(&p);
        let dead = match &c {
            None => true,
            Some(c) => {
                let cp = c.get("claude_pid").and_then(Value::as_u64).unwrap_or(0) as u32;
                (cp != 0 && !sys::alive(cp)) || now() > core::stale_at(c).unwrap_or(0.0)
            }
        };
        let Some(c) = c.filter(|_| !dead) else {
            let _ = fs::remove_file(&p);
            continue;
        };
        let got = match c.get("path").and_then(Value::as_str) {
            Some(path) if Path::new(path).exists() => dir_gb(Path::new(path)),
            _ => 0.0,
        };
        let outstanding = (jf(&c, "gb").unwrap_or(0.0) - got).max(0.0);
        if exclude_session.is_none_or(|s| jstr(&c, "session") != s) {
            out.push(Claim { file: p, v: c, outstanding });
        }
    }
    out
}

fn disk_room(cfg: &Cfg, gb: f64) -> (bool, f64, f64, f64) {
    let free = sys::disk_free_gb(&disk_path(cfg));
    let others: f64 = claims(Some(&core::my_session())).iter().map(|c| c.outstanding).sum();
    let floor = cfg.v["machine"]["disk"]["floor_gb"].as_f64().unwrap_or(15.0);
    (free - others - floor >= gb, free, others, floor)
}

fn cmd_disk(a: &[String]) -> R<i32> {
    let cfg = core::load_config(None)?;
    let sub = a.first().map(String::as_str).unwrap_or("");
    if sub == "claim" || sub == "check" {
        let Some(gb) = a.get(1).and_then(|g| g.parse::<f64>().ok()) else {
            return Err(Problem::new(format!("usage: dibs disk {sub} <GB>")));
        };
        let (ok, free, others, floor) = disk_room(&cfg, gb);
        let msg = format!(
            "{gb:.0} GB needed: {free:.0} GB free on {}, {others:.0} GB claimed by others, {floor} GB floor",
            disk_path(&cfg).display()
        );
        if !ok {
            println!("WAIT: not enough disk. {msg}. Run in a background Bash: dibs wait --disk {gb:.0}, then retry.");
            return Ok(3);
        }
        if sub == "check" {
            println!("OK: {msg}");
            return Ok(0);
        }
        let mut o = me_from(a).to_json();
        let path = opt(a, "--path").map(|p| fs::canonicalize(p).unwrap_or(PathBuf::from(p)).to_string_lossy().trim_start_matches(r"\\?\").to_string());
        o["gb"] = json!(gb);
        o["path"] = json!(path);
        o["t0"] = json!(now());
        o["eta"] = json!(now() + core::parse_dur(opt(a, "--eta").unwrap_or("2h"))?);
        write_json(&state("disk").join(format!("{}-{}.json", std::process::id(), (now() * 1e9) as u64)), &o);
        println!("CLAIMED: {msg}");
        return Ok(0);
    }
    if sub == "release" {
        let mut n = 0;
        for c in claims(None) {
            if has(a, "--all") || jstr(&c.v, "session") == core::my_session() {
                let _ = fs::remove_file(&c.file);
                n += 1;
            }
        }
        println!("released {n} claim(s)");
        return Ok(0);
    }
    print!("{}", usage());
    Ok(2)
}

fn cmd_wait(a: &[String]) -> R<i32> {
    let cfg = core::load_config(None)?;
    if has(a, "--disk") {
        let gb: f64 = opt(a, "--disk").and_then(|g| g.parse().ok()).ok_or_else(|| Problem::new("usage: dibs wait --disk <GB>"))?;
        while !disk_room(&cfg, gb).0 {
            std::thread::sleep(Duration::from_secs(30));
        }
        println!("FREE: {gb:.0} GB of disk available");
        return Ok(0);
    }
    if has(a, "--pressure") {
        let mut calm: Option<f64> = None;
        loop {
            if !probes::pressure(&cfg).is_empty() {
                calm = None;
            } else if let Some(c) = calm {
                if now() - c >= 30.0 {
                    println!("FREE: CPU and memory have been under the limits for 30 s");
                    return Ok(0);
                }
            } else {
                calm = Some(now());
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }
    let names = core::resolve(&positional(a), &cfg, has(a, "--new"))?;
    if names.is_empty() && !has(a, "--measure") {
        return Err(Problem::new("name what to wait for, e.g. `dibs wait gpu`"));
    }
    let me = me_from(a);
    let ws: Vec<PathBuf> = names.iter().map(|n| locks::waiter(n, &me)).collect();
    loop {
        let busy = names.iter().any(|n| !locks::blocking(n, cfg.capacity(n), None, None, None).is_empty())
            || !locks::blocking(MACHINE, 1, None, None, None).is_empty();
        if !busy {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    for w in ws {
        let _ = fs::remove_file(w);
    }
    println!("FREE: {}", if names.is_empty() { "the machine".to_string() } else { names.join(", ") });
    Ok(0)
}

fn cmd_status(a: &[String]) -> R<i32> {
    let cfg = core::load_config(None)?;
    let held = locks::held_names();
    let mut shown: Vec<String> = cfg.resources().into_iter().chain(held.iter().cloned()).filter(|n| n != MACHINE).collect();
    shown.sort();
    shown.dedup();
    if held.iter().any(|n| n == MACHINE) {
        shown.insert(0, MACHINE.into());
    }
    let tag = |h: &locks::Holder| {
        if h.is_hold() {
            " [hold]"
        } else if jbool(&h.o, "preemptible") {
            " [preemptible]"
        } else {
            ""
        }
    };
    for n in shown {
        let (hs, cap, ws) = (locks::holders(&n), cfg.capacity(&n), locks::waiters(&n));
        if n == MACHINE && hs.is_empty() {
            continue;
        }
        let label = if cap > 1 { format!("{n} {}/{cap}", hs.len()) } else { n.clone() };
        let first = hs.first().map(|h| format!("{}{}", describe(&h.o), tag(h))).unwrap_or_else(|| "free".into());
        let waiting = if ws.is_empty() { String::new() } else { format!("   waiting: {}", ws.join(", ")) };
        println!("{label:<16} {first}{waiting}");
        for h in hs.iter().skip(1) {
            println!("{:<16} {}{}", "", describe(&h.o), tag(h));
        }
    }
    for c in claims(None) {
        let why = jstr(&c.v, "purpose");
        let why = if why.is_empty() { String::new() } else { format!(" ('{why}')") };
        let to = c.v.get("path").and_then(Value::as_str).map(|p| format!(" -> {p}")).unwrap_or_default();
        println!(
            "{:<16} {}{why}: {:.0} of {:.0} GB outstanding, ETA {}{to}",
            "disk",
            core::display(&c.v),
            c.outstanding,
            jf(&c.v, "gb").unwrap_or(0.0),
            hhmm(jf(&c.v, "eta"))
        );
    }
    let (avail, commit) = sys::mem();
    let mut parts = vec![format!("CPU {:.0}%", probes::cpu_pct())];
    if let Some(a) = avail {
        parts.push(format!("free RAM {a:.1} GB"));
    }
    if let Some(c) = commit {
        parts.push(format!("commit {c:.0}%"));
    }
    parts.push(format!("disk free {:.0} GB", sys::disk_free_gb(&disk_path(&cfg))));
    let p = probes::pressure(&cfg);
    println!("{:<16} {}   pressure: {}", format!("now {}", hhmm(Some(now()))), parts.join("  "), if p.is_empty() { "ok".into() } else { p.join(", ") });
    for o in hook::overrides(now() - 3600.0).iter().take(5) {
        let name = Some(jstr(o, "name")).filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| jstr(o, "session").chars().take(8).collect());
        let cmd: String = core::short_cmd(jstr(o, "cmd")).chars().take(60).collect();
        println!("{:<16} {name} at {}: '{}' ({cmd})", "override", hhmm(jf(o, "t")), jstr(o, "reason"));
    }
    if has(a, "--activity") {
        let r = probes::activity(&cfg, &cfg.resources(), 10.0, None);
        println!("{:<16} {}", "activity", if r.is_empty() { "quiet".into() } else { r.join("; ") });
    }
    Ok(0)
}

fn cmd_nice(a: &[String]) -> R<i32> {
    let table = sys::procs();
    let mut pids: Vec<u32> = if has(a, "--all") {
        table.iter().filter(|(_, (_, exe))| exe == "claude.exe" || exe == "claude").map(|(p, _)| *p).collect()
    } else {
        vec![core::claude_pid(Some(&table))]
    };
    pids.sort();
    for p in pids.into_iter().filter(|p| *p != 0) {
        println!("claude {p}: {}", if sys::lower_priority(p) { "lowered" } else { "FAILED" });
    }
    Ok(0)
}

fn checks() -> Vec<String> {
    let mut problems = vec![];
    let ok = fs::create_dir_all(core::held()).is_ok() && {
        write_json(&state("probe.json"), &json!({"t": now()}));
        state("probe.json").is_file()
    };
    if !ok {
        problems.push(format!("can't write {}", core::home().display()));
    }
    problems
}

fn cmd_session_start() -> R<i32> {
    let cfg = match core::load_config(None) {
        Ok(c) => c,
        Err(e) => {
            println!("{} (dibs is using defaults until this is fixed)", e.0);
            core::default_cfg()
        }
    };
    if cfg.flag("machine", "lower_claude_priority") {
        let cpid = core::claude_pid(None);
        if cpid != 0 {
            sys::lower_priority(cpid);
        }
    }
    let problems = checks();
    if !problems.is_empty() {
        println!("dibs: {}. Run `dibs doctor` for details.", problems.join("; "));
    }
    Ok(0)
}

fn cmd_doctor() -> R<i32> {
    let row = |k: &str, v: String| println!("{k:<10} {v}");
    row("dibs", format!("{VERSION}  {}", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()));
    row("home", core::home().display().to_string());
    let cfg = match core::load_config(None) {
        Ok(c) => {
            let files = if c.files.is_empty() { format!("none, using defaults (create {} to add some)", core::user_config().display()) } else { c.files.join(", ") };
            row("config", files);
            c
        }
        Err(e) => {
            row("config", format!("ERROR {}", e.0));
            core::default_cfg()
        }
    };
    for p in checks() {
        row("state", format!("ERROR {p}"));
    }
    if core::in_claude() {
        let me = core::who(None, None);
        let pid = if me.claude_pid == 0 { "not found".to_string() } else { me.claude_pid.to_string() };
        row("session", format!("{} (claude pid {pid}, session {})", me.name, me.session.chars().take(8).collect::<String>()));
        let last = read_json(&state("hook.json")).and_then(|s| s.get(&me.session).and_then(Value::as_f64));
        row("hook", match last {
            Some(t) => format!("ran {:.0} s ago in this session", now() - t),
            None => "hasn't run in this session yet: is the dibs plugin enabled? (`/plugin`)".into(),
        });
        if !me.agent.is_empty() {
            row("agent", me.agent);
        }
    } else {
        row("session", format!("not inside Claude Code; holds belong to {} (set DIBS_SESSION to split)", core::my_session()));
    }
    for n in cfg.resources() {
        let r = cfg.res(&n);
        let mut bits = vec![format!("capacity {}", cfg.capacity(&n))];
        if r.get("match").is_some_and(|m| !m.is_null()) {
            bits.push("match".into());
        }
        let p = &r["probe"];
        if p.as_object().is_some_and(|o| !o.is_empty()) {
            let tool = p["command"].as_str().and_then(|c| c.split_whitespace().next()).map(|t| t.trim_matches(['"', '\''])).unwrap_or("");
            let missing = if tool.is_empty() || core::which(tool).is_some() || Path::new(tool).exists() { String::new() } else { format!(" (MISSING: {tool})") };
            bits.push(format!("probe {}{missing}", p["kind"].as_str().unwrap_or("command")));
        }
        row(&format!("  {n}"), bits.join(", "));
    }
    let p = probes::pressure(&cfg);
    row("pressure", if p.is_empty() { "ok".into() } else { p.join(", ") });
    Ok(0)
}

pub fn main(a: Vec<String>) -> i32 {
    let Some(verb) = a.first().map(String::as_str) else {
        print!("{}", usage());
        return 2;
    };
    let rest = &a[1..];
    let r = match verb {
        "-h" | "--help" | "help" => {
            print!("{}", usage());
            Ok(0)
        }
        "version" | "--version" | "-V" => {
            println!("{VERSION}");
            Ok(0)
        }
        "hook" => return crate::hook::main(),
        "run" => cmd_run(rest),
        "hold" => cmd_hold(rest),
        "release" => cmd_release(rest),
        "extend" => cmd_extend(rest),
        "wait" => cmd_wait(rest),
        "status" => cmd_status(rest),
        "disk" => cmd_disk(rest),
        "nice" => cmd_nice(rest),
        "doctor" => cmd_doctor(),
        "session-start" => cmd_session_start(),
        _ => {
            eprintln!("dibs: unknown command '{verb}'\n\n{}", usage());
            Ok(2)
        }
    };
    match r {
        Ok(code) => code,
        Err(p) => {
            eprintln!("{}", p.0);
            1
        }
    }
}
