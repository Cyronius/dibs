use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex_lite::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};

use crate::sys::{self, Procs};

pub const MACHINE: &str = "machine";
const CLAUDE_EXES: [&str; 2] = ["claude.exe", "claude"];

pub struct Problem(pub String);
pub type R<T> = Result<T, Problem>;

impl Problem {
    pub fn new(m: impl AsRef<str>) -> Problem {
        Problem(format!("dibs: {}", m.as_ref()))
    }
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

pub fn home_dir() -> PathBuf {
    env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}

pub fn jstr<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

pub fn jf(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

pub fn jbool(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

// ---- paths ---------------------------------------------------------------------------------------

pub fn home() -> &'static Path {
    static H: OnceLock<PathBuf> = OnceLock::new();
    H.get_or_init(|| {
        if let Some(h) = env("DIBS_HOME") {
            return PathBuf::from(h);
        }
        if cfg!(windows) {
            return env("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home_dir().join("AppData/Local")).join("dibs");
        }
        if cfg!(target_os = "macos") {
            return home_dir().join("Library/Application Support/dibs");
        }
        env("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".local/state")).join("dibs")
    })
}

pub fn state(name: &str) -> PathBuf {
    home().join("state").join(name)
}

pub fn held() -> PathBuf {
    state("held")
}

pub fn user_config() -> PathBuf {
    home().join("config.toml")
}

pub fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

pub fn write_json(p: &Path, v: &Value) {
    if let Some(d) = p.parent() {
        let _ = fs::create_dir_all(d);
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = p.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    if fs::write(&tmp, v.to_string()).is_err() {
        return;
    }
    for _ in 0..50 {
        if fs::rename(&tmp, p).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20)); // Windows: a reader has it open for a moment
    }
    let _ = fs::remove_file(&tmp);
}

pub fn parse_dur(s: &str) -> R<f64> {
    let t = s.trim().to_lowercase();
    let (num, unit) = match t.char_indices().find(|(_, c)| c.is_alphabetic()) {
        Some((i, _)) => (t[..i].trim(), &t[i..]),
        None => (t.as_str(), ""),
    };
    let mult = match unit {
        "" | "m" => 60.0,
        "s" => 1.0,
        "h" => 3600.0,
        "d" => 86400.0,
        _ => 0.0,
    };
    match num.parse::<f64>() {
        Ok(n) if mult > 0.0 && !num.is_empty() && !num.starts_with(['+', '-']) => Ok(n * mult),
        _ => Err(Problem::new(format!("bad duration '{s}': use e.g. 90s, 40m, 2h"))),
    }
}

pub fn hhmm(t: Option<f64>) -> String {
    t.filter(|t| *t > 0.0).map(sys::local_hhmm).unwrap_or_else(|| "?".into())
}

// ---- config --------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct Cfg {
    pub v: Value,
    pub files: Vec<String>,
    pub matches: HashMap<String, Regex>,
}

impl Cfg {
    pub fn f(&self, a: &str, b: &str) -> f64 {
        self.v[a][b].as_f64().unwrap_or(0.0)
    }
    pub fn flag(&self, a: &str, b: &str) -> bool {
        self.v[a][b].as_bool().unwrap_or(false)
    }
    pub fn resources(&self) -> Vec<String> {
        let mut r: Vec<String> = self.v["resource"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        r.sort();
        r
    }
    pub fn res(&self, name: &str) -> &Value {
        &self.v["resource"][name]
    }
    pub fn capacity(&self, name: &str) -> usize {
        if name == MACHINE {
            return 1;
        }
        self.res(name)["capacity"].as_u64().unwrap_or(1) as usize
    }
}

pub fn defaults() -> Value {
    json!({
        "machine": {"cpu_max": 90, "ram_floor_gb": 8, "commit_max": 95, "quiet_cpu_max": 25,
                    "lower_claude_priority": cfg!(windows),
                    "disk": {"path": home_dir().to_string_lossy(), "floor_gb": 15}},
        "hook": {"enabled": true, "pressure": true, "wait_inline_s": 240},
        "measure": {"noise_core_pct": 50, "ignore": ["system idle process", "idle", "kernel_task", "dwm.exe"]},
        "resource": {},
    })
}

pub fn default_cfg() -> Cfg {
    Cfg { v: defaults(), files: vec![], matches: HashMap::new() }
}

fn merge(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in y {
                match x.get_mut(&k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        x.insert(k, v);
                    }
                }
            }
        }
        (a, b) => *a = b,
    }
}

pub fn find_repo_config(start: Option<&str>) -> Option<PathBuf> {
    let p = match start {
        Some(s) => PathBuf::from(s),
        None => std::env::current_dir().ok()?,
    };
    let p = fs::canonicalize(&p).unwrap_or(p);
    p.ancestors().map(|d| d.join(".dibs.toml")).find(|f| f.is_file()).map(|f| unverbatim(&f))
}

fn unverbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

fn load_toml(p: &Path) -> R<Value> {
    let text = fs::read_to_string(p).map_err(|e| Problem::new(format!("{}: {e}", p.display())))?;
    toml::from_str::<Value>(&text).map_err(|e| Problem::new(format!("{}: {e}", p.display())))
}

pub fn valid_name(n: &str) -> bool {
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    n.len() <= 64 && n.chars().next().is_some_and(ok) && n.chars().all(|c| ok(c) || matches!(c, '.' | '_' | '-'))
}

pub fn load_config(cwd: Option<&str>) -> R<Cfg> {
    let (mut v, mut files) = (defaults(), vec![]);
    for p in [Some(user_config()), find_repo_config(cwd)].into_iter().flatten() {
        if p.is_file() {
            merge(&mut v, load_toml(&p)?);
            files.push(p.to_string_lossy().into_owned());
        }
    }
    let mut matches = HashMap::new();
    for (name, r) in v["resource"].as_object().cloned().unwrap_or_default() {
        if !valid_name(&name) || name == MACHINE {
            let why = if name == MACHINE { format!("; '{MACHINE}' is reserved") } else { String::new() };
            return Err(Problem::new(format!(
                "resource name '{name}' in config: use lowercase letters, digits, '.', '_', '-'{why}"
            )));
        }
        if !r.is_object() {
            return Err(Problem::new(format!("[resource.{name}] must be a table")));
        }
        if r.get("capacity").is_some_and(|c| !c.as_u64().is_some_and(|n| n >= 1)) {
            return Err(Problem::new(format!("[resource.{name}] capacity must be a whole number of 1 or more")));
        }
        if let Some(m) = r.get("match").and_then(Value::as_str).filter(|m| !m.is_empty()) {
            let rx = RegexBuilder::new(m).case_insensitive(true).build();
            matches.insert(name.clone(), rx.map_err(|e| Problem::new(format!("[resource.{name}] match: {e}")))?);
        }
    }
    Ok(Cfg { v, files, matches })
}

// ---- names ---------------------------------------------------------------------------------------

pub fn lev(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + usize::from(ca != *cb)));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn known_names(cfg: &Cfg) -> HashSet<String> {
    let mut known: HashSet<String> = cfg.resources().into_iter().collect();
    if let Some(Value::Object(m)) = read_json(&state("known.json")) {
        known.extend(m.keys().cloned());
    }
    if let Ok(rd) = fs::read_dir(held()) {
        known.extend(rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()));
    }
    known.remove(MACHINE);
    known
}

pub fn remember(names: &[String]) {
    let cutoff = now() - 90.0 * 86400.0;
    let mut k: Map<String, Value> = match read_json(&state("known.json")) {
        Some(Value::Object(m)) => m.into_iter().filter(|(_, t)| t.as_f64().unwrap_or(0.0) > cutoff).collect(),
        _ => Map::new(),
    };
    for n in names.iter().filter(|n| *n != MACHINE) {
        k.insert(n.clone(), json!(now()));
    }
    write_json(&state("known.json"), &Value::Object(k));
}

pub fn resolve(raw: &[String], cfg: &Cfg, allow_new: bool) -> R<Vec<String>> {
    let mut out = vec![];
    for r in raw {
        let n = r.to_lowercase();
        if !valid_name(&n) {
            return Err(Problem::new(format!(
                "'{r}' isn't a resource name: use lowercase letters, digits, '.', '_', '-' (max 64)"
            )));
        }
        if n == MACHINE {
            return Err(Problem::new(format!("'{MACHINE}' is reserved for --measure")));
        }
        let known = known_names(cfg);
        if !known.contains(&n) && !allow_new {
            let mut close: Vec<&String> =
                known.iter().filter(|k| lev(&n, k) <= if k.len() <= 4 { 1 } else { 2 }).collect();
            close.sort();
            if let Some(c) = close.first() {
                return Err(Problem::new(format!(
                    "there's no resource '{n}' yet. Did you mean '{c}'? Pass --new to use '{n}' anyway."
                )));
            }
        }
        out.push(n);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

// ---- identity ------------------------------------------------------------------------------------

pub fn my_session() -> String {
    // outside Claude, all of one user's processes form one session unless DIBS_SESSION splits them
    env("CLAUDE_CODE_SESSION_ID")
        .or_else(|| env("DIBS_SESSION"))
        .unwrap_or_else(|| format!("user-{}", env("USERNAME").or_else(|| env("USER")).unwrap_or_default()))
}

pub fn my_agent() -> String {
    env("DIBS_AGENT").map(|a| a.trim().to_string()).unwrap_or_default()
}

pub fn in_claude() -> bool {
    env("CLAUDE_CODE_SESSION_ID").is_some()
}

pub fn covers(own: &Value, sid: &str, agent: &str) -> bool {
    // a main-agent hold covers its whole session; a subagent's hold covers only that subagent
    let a = jstr(own, "agent");
    !sid.is_empty() && jstr(own, "session") == sid && (a.is_empty() || a == agent)
}

pub fn controls(own: &Value, sid: &str, agent: &str) -> bool {
    // who may end or extend a hold: its own agent, or the main agent of its session
    jstr(own, "session") == sid && (agent.is_empty() || jstr(own, "agent") == agent)
}

pub fn claude_pid(table: Option<&Procs>) -> u32 {
    if let Some(p) = env("CLAUDE_PID").and_then(|s| s.parse::<u32>().ok()).filter(|p| sys::alive(*p)) {
        return p;
    }
    let owned;
    let table = match table {
        Some(t) => t,
        None => {
            owned = sys::procs();
            &owned
        }
    };
    let mut pid = std::process::id();
    for _ in 0..30 {
        let (ppid, exe) = table.get(&pid).cloned().unwrap_or((0, String::new()));
        if CLAUDE_EXES.contains(&exe.as_str()) {
            return pid;
        }
        if ppid == 0 || ppid == pid {
            break;
        }
        pid = ppid;
    }
    0
}

fn sessions_dir() -> PathBuf {
    env("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".claude")).join("sessions")
}

pub fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        env("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD".into()).split(';').map(|s| s.to_lowercase()).collect()
    } else {
        vec![]
    };
    let p = Path::new(name);
    let dirs: Vec<PathBuf> = if p.components().count() > 1 {
        vec![PathBuf::new()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect()
    };
    for d in dirs {
        let base = d.join(name);
        if base.is_file() && (exts.is_empty() || exts.iter().any(|e| name.to_lowercase().ends_with(e))) {
            return Some(base);
        }
        for e in &exts {
            let c = PathBuf::from(format!("{}{e}", base.display()));
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn agents_by_session() -> &'static HashMap<String, Value> {
    // documented fallback for when Claude Code's internal sessions file moves or changes shape
    static A: OnceLock<HashMap<String, Value>> = OnceLock::new();
    A.get_or_init(|| {
        let mut out = HashMap::new();
        if let Some(exe) = which("claude") {
            let o = std::process::Command::new(exe).args(["agents", "--json"]).output();
            if let Some(Value::Array(list)) = o.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()) {
                for s in list {
                    if !jstr(&s, "sessionId").is_empty() && !jstr(&s, "name").is_empty() {
                        out.insert(jstr(&s, "sessionId").to_string(), s);
                    }
                }
            }
        }
        out
    })
}

pub fn session_info(cpid: u32, sid: &str) -> Value {
    if cpid != 0 {
        if let Some(info) = read_json(&sessions_dir().join(format!("{cpid}.json"))) {
            if !jstr(&info, "name").is_empty() && (sid.is_empty() || jstr(&info, "sessionId") == sid) {
                return info;
            }
        }
    }
    if !sid.is_empty() && env("DIBS_AGENTS_LOOKUP").as_deref() != Some("0") {
        return agents_by_session().get(sid).cloned().unwrap_or(Value::Null);
    }
    Value::Null
}

pub fn split_label(label: &str, name: &str) -> (Option<String>, String) {
    // agents often write "<session>/<worker>: <purpose>"; keep the worker and the purpose
    if let Some((who, what)) = label.split_once(':') {
        let who = who.trim();
        let (first, worker) = who.split_once('/').unwrap_or((who, ""));
        if first.chars().count() >= 4 && !who.contains(' ') && name.to_lowercase().starts_with(&first.to_lowercase()) {
            let w = worker.trim();
            return ((!w.is_empty()).then(|| w.to_string()), what.trim().to_string());
        }
    }
    (None, label.trim().to_string())
}

pub struct Me {
    pub claude_pid: u32,
    pub session: String,
    pub agent: String,
    pub name: String,
    pub project: String,
    pub tag: Option<String>,
    pub purpose: String,
}

impl Me {
    pub fn to_json(&self) -> Value {
        json!({"claude_pid": self.claude_pid, "session": self.session, "agent": self.agent, "name": self.name,
               "project": self.project, "tag": self.tag, "purpose": self.purpose})
    }
}

pub fn who(label: Option<&str>, tag: Option<&str>) -> Me {
    let sid = my_session();
    let cpid = if in_claude() { claude_pid(None) } else { 0 };
    let info = if in_claude() { session_info(cpid, &sid) } else { Value::Null };
    let cwd = || std::env::current_dir().unwrap_or_default();
    let dir = if jstr(&info, "cwd").is_empty() { cwd() } else { PathBuf::from(jstr(&info, "cwd")) };
    let project = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut tag = tag.map(String::from);
    let name = if !jstr(&info, "name").is_empty() {
        jstr(&info, "name").to_string()
    } else if in_claude() {
        format!("{} ({project})", sid.chars().take(8).collect::<String>())
    } else {
        let n = env("DIBS_NAME")
            .or_else(|| tag.clone())
            .unwrap_or_else(|| format!("{} ({project})", env("USERNAME").or_else(|| env("USER")).unwrap_or_default()));
        if tag.as_deref() == Some(n.as_str()) {
            tag = None;
        }
        n
    };
    let (t2, purpose) = split_label(label.unwrap_or(""), &name);
    let agent = my_agent();
    let agent7: String = agent.chars().take(7).collect();
    Me {
        claude_pid: cpid,
        session: sid,
        tag: tag.or(t2).or((!agent7.is_empty()).then_some(agent7)),
        agent,
        name,
        project,
        purpose: purpose.chars().take(80).collect(),
    }
}

pub fn display(own: &Value) -> String {
    let cpid = own.get("claude_pid").and_then(Value::as_u64).unwrap_or(0) as u32;
    let live = if cpid != 0 { jstr(&session_info(cpid, jstr(own, "session")), "name").to_string() } else { String::new() };
    let name = if !live.is_empty() {
        live
    } else if !jstr(own, "name").is_empty() {
        jstr(own, "name").to_string()
    } else {
        "?".into()
    };
    match own.get("tag").and_then(Value::as_str).filter(|t| !t.is_empty() && *t != name) {
        Some(t) => format!("{name}/{t}"),
        None => name,
    }
}

pub fn short_cmd(cmd: &str) -> String {
    // a long scratchpad path tells a waiter nothing; the file name and the arguments do
    cmd.split_whitespace()
        .map(|t| {
            let t = t.trim_matches(['"', '\'']);
            t.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(t)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn stale_at(own: &Value) -> Option<f64> {
    let eta = jf(own, "eta").filter(|e| *e != 0.0)?;
    let span = (eta - jf(own, "t0").unwrap_or(eta)).max(0.0);
    Some(eta + 600f64.max(0.25 * span))
}

pub fn describe(own: &Value) -> String {
    let purpose = jstr(own, "purpose");
    let what = if purpose.is_empty() { short_cmd(jstr(own, "cmd")) } else { purpose.to_string() };
    let when = match jf(own, "eta").filter(|e| *e != 0.0) {
        Some(eta) => format!("until ~{} (stale at {})", hhmm(Some(eta)), hhmm(stale_at(own))),
        None => format!("since {}", hhmm(jf(own, "t0"))),
    };
    let what = if what.is_empty() { String::new() } else { format!(" ('{}')", what.chars().take(60).collect::<String>()) };
    format!("{}{what} {when}", display(own))
}

pub fn mine(own: &Value, sid: Option<&str>, table: Option<&Procs>, agent: Option<&str>) -> bool {
    // subagents share the session id, so a sibling's run lock isn't ours; a covering hold or an ancestor's run lock is
    let sid = sid.map(String::from).unwrap_or_else(my_session);
    if sid.is_empty() || jstr(own, "session") != sid {
        return false;
    }
    if jbool(own, "hold") {
        return covers(own, &sid, &agent.map(String::from).unwrap_or_else(my_agent));
    }
    let pid = own.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
    sys::ancestors(table).contains(&pid)
}
