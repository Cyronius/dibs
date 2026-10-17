use std::collections::HashMap;
use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, SYSTEMTIME};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, SetConsoleCtrlHandler, CTRL_BREAK_EVENT};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, GetSystemTimes, OpenProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
    NORMAL_PRIORITY_CLASS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

use super::Procs;
use crate::core::now;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

fn ft(f: &FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

pub fn procs() -> Procs {
    let mut out = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        let mut e: PROCESSENTRY32W = zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            out.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase()));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    out
}

pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        code == 259 // STILL_ACTIVE
    }
}

pub fn proc_times() -> HashMap<u32, (String, f64)> {
    let mut out = HashMap::new();
    for (pid, (_, exe)) in procs() {
        unsafe {
            let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                continue;
            }
            let mut t: [FILETIME; 4] = zeroed();
            if GetProcessTimes(h, &mut t[0], &mut t[1], &mut t[2], &mut t[3]) != 0 {
                out.insert(pid, (exe, (ft(&t[2]) + ft(&t[3])) as f64 / 1e7));
            }
            CloseHandle(h);
        }
    }
    out
}

pub fn lower_priority(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = SetPriorityClass(h, BELOW_NORMAL_PRIORITY_CLASS) != 0;
        CloseHandle(h);
        ok
    }
}

pub fn normal_priority_flags() -> u32 {
    NORMAL_PRIORITY_CLASS
}

pub fn new_group_flags() -> u32 {
    0x0000_0200 // CREATE_NEW_PROCESS_GROUP: lets CTRL_BREAK reach only the child
}

pub fn ask_to_stop(pid: u32) {
    unsafe {
        GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
    }
}

// the parent ignores Ctrl-C while a child runs, so the child decides and the parent still releases its locks
pub fn ignore_interrupts() {
    unsafe {
        SetConsoleCtrlHandler(None, 1);
    }
}

pub fn cpu_sample() -> Option<[f64; 3]> {
    unsafe {
        let (mut i, mut k, mut u): (FILETIME, FILETIME, FILETIME) = zeroed();
        if GetSystemTimes(&mut i, &mut k, &mut u) == 0 {
            return None;
        }
        Some([now(), ft(&i) as f64, (ft(&k) + ft(&u)) as f64]) // kernel time includes idle
    }
}

pub fn loadavg_pct() -> f64 {
    0.0
}

pub fn mem() -> (Option<f64>, Option<f64>) {
    unsafe {
        let mut m: MEMORYSTATUSEX = zeroed();
        m.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) == 0 {
            return (None, None);
        }
        let commit = 100.0 * (m.ullTotalPageFile - m.ullAvailPageFile) as f64 / m.ullTotalPageFile as f64;
        (Some(m.ullAvailPhys as f64 / 2f64.powi(30)), Some(commit))
    }
}

pub fn local_hhmm(t: f64) -> String {
    unsafe {
        let ticks = ((t + 11_644_473_600.0) * 1e7) as u64;
        let f = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let (mut utc, mut local): (SYSTEMTIME, SYSTEMTIME) = zeroed();
        if FileTimeToSystemTime(&f, &mut utc) == 0 || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0 {
            return "?".into();
        }
        format!("{:02}:{:02}", local.wHour, local.wMinute)
    }
}

pub fn disk_free_gb(p: &Path) -> f64 {
    let mut free = 0u64;
    unsafe {
        GetDiskFreeSpaceExW(wide(p.as_os_str()).as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut());
    }
    free as f64 / 2f64.powi(30)
}

fn pdh_array(counter: &str, sample_s: f64) -> Vec<(String, f64)> {
    let mut out = vec![];
    unsafe {
        let (mut q, mut c) = (0isize, 0isize);
        if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
            return out;
        }
        let path = wide(OsStr::new(counter));
        if PdhAddEnglishCounterW(q, path.as_ptr(), 0, &mut c) == 0 {
            PdhCollectQueryData(q);
            std::thread::sleep(std::time::Duration::from_secs_f64(sample_s)); // a rate counter needs two collections
            PdhCollectQueryData(q);
            let (mut size, mut count) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, std::ptr::null_mut());
            let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if size > 0 && PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, items) == 0 {
                for it in std::slice::from_raw_parts(items, count as usize) {
                    if it.FmtValue.CStatus <= 1 {
                        let name = if it.szName.is_null() {
                            String::new()
                        } else {
                            let mut n = 0;
                            while *it.szName.add(n) != 0 {
                                n += 1;
                            }
                            String::from_utf16_lossy(std::slice::from_raw_parts(it.szName, n))
                        };
                        out.push((name, it.FmtValue.Anonymous.doubleValue));
                    }
                }
            }
        }
        PdhCloseQuery(q);
    }
    out
}

pub fn gpu_by_pid(sample_s: f64) -> HashMap<u32, f64> {
    let mut per = HashMap::new();
    for (name, v) in pdh_array("\\GPU Engine(*)\\Utilization Percentage", sample_s) {
        let pid = name.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse::<u32>().ok());
        if let Some(p) = pid {
            *per.entry(p).or_insert(0.0) += v;
        }
    }
    per
}

pub fn net_mbps(sample_s: f64) -> Option<f64> {
    let rows = pdh_array("\\Network Interface(*)\\Bytes Total/sec", sample_s);
    (!rows.is_empty()).then(|| rows.iter().map(|r| r.1).sum::<f64>() * 8.0 / 1e6)
}
