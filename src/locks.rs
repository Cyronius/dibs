use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::core::{self, describe, display, held, jbool, jf, jstr, mine, now, read_json, write_json, Me, Problem, R};
use crate::sys::{self, Procs};

pub struct Holder {
    pub dir: PathBuf,
    pub o: Value,
}

impl Holder {
    pub fn s(&self, k: &str) -> &str {
        jstr(&self.o, k)
    }
    pub fn is_hold(&self) -> bool {
        jbool(&self.o, "hold")
    }
    pub fn eta(&self) -> Option<f64> {
        jf(&self.o, "eta").filter(|e| *e != 0.0)
    }
}

pub fn rdir(name: &str) -> PathBuf {
    held().join(name)
}

fn is_stale(d: &Path, own: Option<&Value>) -> bool {
    let Some(own) = own else {
        // owner.json not written yet, or a crash right after mkdir
        return fs::metadata(d)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e > Duration::from_secs(30));
    };
    let pid = |k: &str| own.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    if jbool(own, "hold") {
        if pid("claude_pid") != 0 && !sys::alive(pid("claude_pid")) {
            return true;
        }
        // a watched hold lasts exactly as long as its process, whatever the ETA says
        return match pid("watch_pid") {
            0 => now() > core::stale_at(own).unwrap_or(f64::MAX),
            w => !sys::alive(w),
        };
    }
    !sys::alive(pid("pid"))
}

fn break_lock(d: &Path) -> bool {
    let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let grave = d.with_file_name(format!("{name}.stale.{}.{}", std::process::id(), (now() * 1e9) as u64));
    if fs::rename(d, &grave).is_err() {
        return false;
    }
    if !is_stale(&grave, read_json(&grave.join("owner.json")).as_ref()) {
        // someone re-took it between our read and rename
        if fs::rename(&grave, d).is_ok() {
            return false;
        }
    }
    let _ = fs::remove_dir_all(&grave);
    true
}

pub fn holders(name: &str) -> Vec<Holder> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir(rdir(name)) else { return out };
    let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        let digits = d.file_name().is_some_and(|n| !n.is_empty() && n.to_string_lossy().chars().all(|c| c.is_ascii_digit()));
        if !d.is_dir() || !digits {
            continue;
        }
        let own = read_json(&d.join("owner.json"));
        if is_stale(&d, own.as_ref()) {
            if let Some(o) = &own {
                eprintln!("dibs: {name} was held by {}, which is gone; taking it over", display(o));
            }
            break_lock(&d);
            continue;
        }
        out.push(Holder { dir: d, o: own.unwrap_or_else(|| json!({"name": "?", "t0": now()})) });
    }
    out
}

pub fn held_names() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(held())
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn blocking(name: &str, cap: usize, sid: Option<&str>, table: Option<&Procs>, agent: Option<&str>) -> Vec<Holder> {
    // holders that stop this session's agent from getting a slot now
    let hs = holders(name);
    let total = hs.len();
    let others: Vec<Holder> = hs.into_iter().filter(|h| !mine(&h.o, sid, table, agent)).collect();
    if !others.is_empty() && total >= cap {
        others
    } else {
        vec![]
    }
}

pub fn session_holds(sid: &str, agent: &str) -> Vec<Holder> {
    if sid.is_empty() {
        return vec![];
    }
    held_names()
        .iter()
        .flat_map(|n| holders(n))
        .filter(|h| h.s("session") == sid && (!h.is_hold() || core::covers(&h.o, sid, agent)))
        .collect()
}

// a hold taken through the CLI can't know which subagent ran it; the hook that watched the call does
pub fn stamp_agent(sid: &str, agent: &str, names: &[String], since: f64) -> usize {
    let mut n = 0;
    for name in names {
        for h in holders(name) {
            if h.is_hold() && h.s("session") == sid && h.s("agent").is_empty() && jf(&h.o, "t0").unwrap_or(0.0) >= since {
                let mut o = h.o.clone();
                o["agent"] = json!(agent);
                write_json(&h.dir.join("owner.json"), &o);
                n += 1;
            }
        }
    }
    n
}

pub fn release_agent(sid: &str, agent: &str) -> usize {
    let mut n = 0;
    for name in held_names() {
        let hs: Vec<Holder> = holders(&name).into_iter().filter(|h| h.s("session") == sid && h.s("agent") == agent).collect();
        n += hs.len();
        release_holders(&hs.iter().collect::<Vec<_>>());
    }
    n
}

pub fn waiter(name: &str, me: &Me) -> PathBuf {
    let p = rdir(name).join(format!("wait.{}.json", std::process::id()));
    let mut o = me.to_json();
    o["pid"] = json!(std::process::id());
    o["t0"] = json!(now());
    write_json(&p, &o);
    p
}

pub fn waiter_records(name: &str) -> Vec<Value> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir(rdir(name)) else { return out };
    for e in rd.flatten() {
        let (p, fname) = (e.path(), e.file_name().to_string_lossy().into_owned());
        if !(fname.starts_with("wait.") && fname.ends_with(".json")) {
            continue;
        }
        match read_json(&p) {
            Some(w) if sys::alive(w.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32) => out.push(w),
            _ => {
                let _ = fs::remove_file(&p);
            }
        }
    }
    out
}

pub fn waiters(name: &str) -> Vec<String> {
    waiter_records(name).iter().map(display).collect()
}

pub fn others_waiting(name: &str, sid: &str) -> bool {
    waiter_records(name).iter().any(|w| jstr(w, "session") != sid)
}

pub struct Ask<'a> {
    pub cmd: &'a [String],
    pub cap: usize,
    pub eta: Option<f64>,
    pub hold: bool,
    pub kind: &'a str,
    pub exclusive: bool,
    pub watch_pid: Option<u32>,
    pub preemptible: bool,
}

pub fn acquire(name: &str, me: &Me, ask: &Ask) -> R<Vec<PathBuf>> {
    let rd = rdir(name);
    fs::create_dir_all(&rd).map_err(|e| Problem::new(format!("can't write {}: {e}", rd.display())))?;
    let mut owner = me.to_json();
    let o = owner.as_object_mut().unwrap();
    o.insert("pid".into(), json!(if ask.hold { me.claude_pid } else { std::process::id() }));
    o.insert("resource".into(), json!(name));
    o.insert("kind".into(), json!(ask.kind));
    o.insert("hold".into(), json!(ask.hold));
    o.insert("t0".into(), json!(now()));
    if let Some(w) = ask.watch_pid {
        o.insert("watch_pid".into(), json!(w));
    }
    if ask.preemptible {
        o.insert("preemptible".into(), json!(true));
    }
    o.insert("eta".into(), json!(ask.eta.map(|e| now() + e)));
    o.insert("cmd".into(), json!(ask.cmd.join(" ").chars().take(300).collect::<String>()));

    let take = |i: usize| -> Option<PathBuf> {
        let d = rd.join(i.to_string());
        fs::create_dir(&d).ok()?;
        write_json(&d.join("owner.json"), &owner);
        Some(d)
    };
    let (t0, mut last, mut w) = (now(), 0.0, None::<PathBuf>);
    let (mut got, mut todo): (Vec<PathBuf>, Vec<usize>) = (vec![], (0..ask.cap).collect());
    let result = (|| -> R<Vec<PathBuf>> {
        loop {
            let table = sys::procs();
            let hs: Vec<Holder> = holders(name).into_iter().filter(|h| !got.contains(&h.dir)).collect();
            if hs.iter().any(|h| mine(&h.o, None, Some(&table), None)) {
                return Ok(std::mem::take(&mut got)); // re-entrant: our session's hold, or our parent's run
            }
            if ask.exclusive {
                while let Some(d) = todo.first().and_then(|i| take(*i)) {
                    got.push(d);
                    todo.remove(0);
                }
                if todo.is_empty() {
                    break;
                }
            } else if let Some(d) = (0..ask.cap).find_map(|i| take(i)) {
                got.push(d);
                break;
            }
            w.get_or_insert_with(|| waiter(name, me));
            if !hs.is_empty() && now() - last > 120.0 {
                println!("dibs: waiting for {name}, held by {}", describe(&hs[0].o));
                last = now();
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        if now() - t0 > 5.0 {
            println!("dibs: got {name} after {:.0} s", now() - t0);
        }
        core::remember(&[name.to_string()]);
        Ok(std::mem::take(&mut got))
    })();
    if let Some(w) = w {
        let _ = fs::remove_file(w);
    }
    if result.is_err() {
        release(&got);
    }
    result
}

pub fn release(dirs: &[PathBuf]) {
    for d in dirs.iter().rev() {
        let _ = fs::remove_file(d.join("owner.json"));
        let _ = fs::remove_dir(d);
    }
}

// for holders listed earlier: the slot may have been taken over since, and the new owner's lock must survive
pub fn release_holders(hs: &[&Holder]) {
    for h in hs.iter().rev() {
        let name = h.dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let grave = h.dir.with_file_name(format!("{name}.rel.{}.{}", std::process::id(), (now() * 1e9) as u64));
        // Windows: a reader may have owner.json open for a moment
        if !(0..5).any(|i| {
            if i > 0 {
                std::thread::sleep(Duration::from_millis(20));
            }
            fs::rename(&h.dir, &grave).is_ok()
        }) {
            continue;
        }
        let same = read_json(&grave.join("owner.json")).is_none_or(|o| jf(&o, "t0") == jf(&h.o, "t0") && jstr(&o, "session") == h.s("session"));
        if same || fs::rename(&grave, &h.dir).is_err() {
            let _ = fs::remove_dir_all(&grave);
        }
    }
}
