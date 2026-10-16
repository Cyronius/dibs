use std::collections::{HashMap, HashSet};

pub type Procs = HashMap<u32, (u32, String)>;

#[cfg(windows)]
#[path = "sys/windows.rs"]
mod imp;
#[cfg(unix)]
#[path = "sys/unix.rs"]
mod imp;

pub use imp::*;

pub const WINDOWS: bool = cfg!(windows);

pub fn tree(root: u32, table: Option<&Procs>) -> HashSet<u32> {
    let owned;
    let table = match table {
        Some(t) => t,
        None => {
            owned = procs();
            &owned
        }
    };
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, (ppid, _)) in table {
        kids.entry(*ppid).or_default().push(*pid);
    }
    let (mut out, mut todo) = (HashSet::from([root]), vec![root]);
    while let Some(p) = todo.pop() {
        for c in kids.get(&p).into_iter().flatten() {
            if out.insert(*c) {
                todo.push(*c);
            }
        }
    }
    out
}

pub fn ancestors(table: Option<&Procs>) -> HashSet<u32> {
    let owned;
    let table = match table {
        Some(t) => t,
        None => {
            owned = procs();
            &owned
        }
    };
    let (mut pid, mut out) = (std::process::id(), HashSet::new());
    while pid != 0 && out.insert(pid) {
        pid = table.get(&pid).map(|e| e.0).unwrap_or(0);
    }
    out
}

pub fn cpu_busy(a: Option<[f64; 3]>, b: Option<[f64; 3]>) -> f64 {
    match (a, b) {
        (Some(a), Some(b)) => {
            let total = b[2] - a[2];
            if total > 0.0 {
                (100.0 * (total - (b[1] - a[1])) / total).clamp(0.0, 100.0)
            } else {
                0.0
            }
        }
        _ => loadavg_pct(), // macOS has no cheap system CPU counter
    }
}
