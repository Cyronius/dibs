import os, re, subprocess, sys, time

WINDOWS = sys.platform == "win32"
MAC = sys.platform == "darwin"

try:
    import psutil
except ImportError:
    psutil = None

if WINDOWS:
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenProcess.restype = wintypes.HANDLE
    k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]
    k32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    k32.SetPriorityClass.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    k32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    k32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    ntdll = ctypes.WinDLL("ntdll")
    ntdll.NtQueryInformationProcess.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.ULONG,
                                                ctypes.POINTER(wintypes.ULONG)]

    class PE32(ctypes.Structure):
        _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD), ("pid", wintypes.DWORD),
                    ("heap", ctypes.c_size_t), ("mod", wintypes.DWORD), ("threads", wintypes.DWORD),
                    ("ppid", wintypes.DWORD), ("pri", ctypes.c_long), ("flags", wintypes.DWORD),
                    ("exe", ctypes.c_wchar * 260)]

    class MEMSTAT(ctypes.Structure):
        _fields_ = [("len", wintypes.DWORD), ("load", wintypes.DWORD)] + \
                   [(n, ctypes.c_ulonglong) for n in ("tot", "avail", "totpf", "availpf", "totv", "availv", "ext")]

    class USTR(ctypes.Structure):
        _fields_ = [("len", wintypes.USHORT), ("max", wintypes.USHORT), ("buf", ctypes.c_wchar_p)]

    class PDHITEM(ctypes.Structure):
        _fields_ = [("name", ctypes.c_wchar_p), ("status", wintypes.DWORD), ("value", ctypes.c_double)]

NORMAL_PRIORITY, BELOW_NORMAL_PRIORITY = 0x20, 0x4000
QUERY = 0x1000                                   # PROCESS_QUERY_LIMITED_INFORMATION


def _ps(fields: str) -> list:
    try:
        out = subprocess.run(["ps", "-A", "-o", fields], capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    return [ln.split(None, len(fields.split(",")) - 1) for ln in out.splitlines() if ln.strip()]


# ---- processes -----------------------------------------------------------------------------------

def procs() -> dict:
    if WINDOWS:
        snap, out, e = k32.CreateToolhelp32Snapshot(2, 0), {}, PE32()
        e.dwSize = ctypes.sizeof(PE32)
        ok = k32.Process32FirstW(snap, ctypes.byref(e))
        while ok:
            out[e.pid] = (e.ppid, e.exe.lower())
            ok = k32.Process32NextW(snap, ctypes.byref(e))
        k32.CloseHandle(snap)
        return out
    if psutil:
        return {p.info["pid"]: (p.info["ppid"] or 0, (p.info["name"] or "").lower())
                for p in psutil.process_iter(["pid", "ppid", "name"])}
    out = {}
    for row in _ps("pid=,ppid=,comm="):
        if len(row) == 3 and row[0].isdigit() and row[1].isdigit():
            out[int(row[0])] = (int(row[1]), os.path.basename(row[2].strip()).lower())
    return out


def alive(pid) -> bool:
    if not pid:
        return False
    if WINDOWS:
        h = k32.OpenProcess(QUERY, False, int(pid))
        if not h:
            return False
        code = wintypes.DWORD()
        k32.GetExitCodeProcess(h, ctypes.byref(code))
        k32.CloseHandle(h)
        return code.value == 259
    try:
        os.kill(int(pid), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


def tree(root: int, table=None) -> set:
    table = table or procs()
    kids, out, todo = {}, {root}, [root]
    for pid, (ppid, _) in table.items():
        kids.setdefault(ppid, []).append(pid)
    while todo:
        for c in kids.get(todo.pop(), []):
            if c not in out:
                out.add(c)
                todo.append(c)
    return out


def ancestors(table=None) -> set:
    table, pid, out = table or procs(), os.getpid(), set()
    while pid and pid not in out:
        out.add(pid)
        pid = table.get(pid, (0, ""))[0]
    return out


def _parse_cputime(s: str) -> float:
    days, _, rest = s.strip().rpartition("-")
    secs = 0.0
    for part in rest.split(":"):
        secs = secs * 60 + float(part or 0)
    return secs + (float(days) * 86400 if days else 0.0)


def proc_times() -> dict:
    if WINDOWS:
        out = {}
        for pid, (_, exe) in procs().items():
            h = k32.OpenProcess(QUERY, False, pid)
            if not h:
                continue
            t = [wintypes.FILETIME() for _ in range(4)]
            if k32.GetProcessTimes(h, *map(ctypes.byref, t)):
                out[pid] = (exe, sum((f.dwHighDateTime << 32 | f.dwLowDateTime) for f in t[2:]) / 1e7)
            k32.CloseHandle(h)
        return out
    if psutil:
        out = {}
        for p in psutil.process_iter(["pid", "name", "cpu_times"]):
            ct = p.info.get("cpu_times")
            if ct:
                out[p.info["pid"]] = ((p.info["name"] or "").lower(), ct.user + ct.system)
        return out
    out = {}
    for row in _ps("pid=,time=,comm="):
        if len(row) == 3 and row[0].isdigit():
            try:
                out[int(row[0])] = (os.path.basename(row[2].strip()).lower(), _parse_cputime(row[1]))
            except ValueError:
                pass
    return out


def cmdline(pid: int) -> str:
    if WINDOWS:
        h = k32.OpenProcess(QUERY, False, pid)
        if not h:
            return ""
        try:
            buf, n = ctypes.create_string_buffer(8192), wintypes.ULONG()
            if ntdll.NtQueryInformationProcess(h, 60, buf, 8192, ctypes.byref(n)) == 0:   # ProcessCommandLineInformation
                u = USTR.from_buffer(buf)
                return ctypes.wstring_at(u.buf, u.len // 2) if u.buf else ""
            return ""
        finally:
            k32.CloseHandle(h)
    if psutil:
        try:
            return " ".join(psutil.Process(pid).cmdline())
        except Exception:
            return ""
    try:
        return open(f"/proc/{pid}/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace").strip()
    except OSError:
        rows = subprocess.run(["ps", "-o", "args=", "-p", str(pid)], capture_output=True, text=True).stdout
        return rows.strip()


def lower_priority(pid: int) -> bool:
    if WINDOWS:
        h = k32.OpenProcess(0x0200 | QUERY, False, pid)
        if not h:
            return False
        ok = bool(k32.SetPriorityClass(h, BELOW_NORMAL_PRIORITY))
        k32.CloseHandle(h)
        return ok
    try:
        os.setpriority(os.PRIO_PROCESS, pid, max(10, os.getpriority(os.PRIO_PROCESS, pid)))
        return True
    except OSError:
        return False


def normal_priority_flags() -> int:
    # a POSIX child can't undo an inherited nice without privileges, so only Windows gets this
    return NORMAL_PRIORITY if WINDOWS else 0


# ---- CPU and memory ------------------------------------------------------------------------------

def cpu_sample():
    if WINDOWS:
        i, k, u = wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME()
        k32.GetSystemTimes(ctypes.byref(i), ctypes.byref(k), ctypes.byref(u))
        v = lambda f: (f.dwHighDateTime << 32) | f.dwLowDateTime
        return [time.time(), v(i), v(k) + v(u)]          # kernel time includes idle
    if psutil:
        t = psutil.cpu_times()
        return [time.time(), t.idle + getattr(t, "iowait", 0.0), sum(t)]
    try:
        f = [float(x) for x in open("/proc/stat").readline().split()[1:]]
        return [time.time(), f[3] + (f[4] if len(f) > 4 else 0), sum(f)]
    except OSError:
        return None


def cpu_busy(a, b) -> float:
    if a is None or b is None:
        return min(100.0, 100.0 * os.getloadavg()[0] / (os.cpu_count() or 1))   # macOS without psutil
    total = b[2] - a[2]
    return max(0.0, min(100.0, 100.0 * (total - (b[1] - a[1])) / total)) if total > 0 else 0.0


def mem():
    if WINDOWS:
        m = MEMSTAT()
        m.len = ctypes.sizeof(MEMSTAT)
        k32.GlobalMemoryStatusEx(ctypes.byref(m))
        return m.avail / 2**30, 100.0 * (m.totpf - m.availpf) / m.totpf
    if psutil:
        return psutil.virtual_memory().available / 2**30, None
    try:
        for ln in open("/proc/meminfo"):
            if ln.startswith("MemAvailable:"):
                return int(ln.split()[1]) / 2**20, None
    except OSError:
        pass
    if MAC:
        try:
            out = subprocess.run(["vm_stat"], capture_output=True, text=True, timeout=10).stdout
            page = int(re.search(r"page size of (\d+)", out).group(1))
            pages = sum(int(re.search(rf"{k}:\s+(\d+)", out).group(1)) for k in
                        ("Pages free", "Pages inactive", "Pages speculative"))
            return pages * page / 2**30, None
        except Exception:
            pass
    return None, None


# ---- GPU and network -----------------------------------------------------------------------------

def _pdh_array(counter: str, sample_s: float) -> list:
    pdh = ctypes.WinDLL("pdh")
    q, c = wintypes.HANDLE(), wintypes.HANDLE()
    if pdh.PdhOpenQueryW(None, None, ctypes.byref(q)) != 0:
        return []
    try:
        if pdh.PdhAddEnglishCounterW(q, counter, None, ctypes.byref(c)) != 0:
            return []
        pdh.PdhCollectQueryData(q)
        time.sleep(sample_s)                           # a rate counter needs two collections
        pdh.PdhCollectQueryData(q)
        size, count = wintypes.DWORD(0), wintypes.DWORD(0)
        pdh.PdhGetFormattedCounterArrayW(c, 0x200, ctypes.byref(size), ctypes.byref(count), None)
        buf = ctypes.create_string_buffer(size.value)
        if pdh.PdhGetFormattedCounterArrayW(c, 0x200, ctypes.byref(size), ctypes.byref(count), buf) != 0:
            return []
        return [(it.name or "", it.value) for it in (PDHITEM * count.value).from_buffer(buf) if it.status in (0, 1)]
    finally:
        pdh.PdhCloseQuery(q)


def gpu_by_pid(sample_s: float = 1.0) -> dict:
    if not WINDOWS:
        return {}
    per = {}
    for name, value in _pdh_array("\\GPU Engine(*)\\Utilization Percentage", sample_s):
        m = re.match(r"pid_(\d+)_", name)
        if m:
            per[int(m.group(1))] = per.get(int(m.group(1)), 0.0) + value
    return per


def _net_bytes():
    if psutil:
        c = psutil.net_io_counters()
        return c.bytes_sent + c.bytes_recv
    try:
        total = 0
        for ln in open("/proc/net/dev").readlines()[2:]:
            name, data = ln.split(":", 1)
            if name.strip() != "lo":
                f = data.split()
                total += int(f[0]) + int(f[8])
        return total
    except OSError:
        return None


def net_mbps(sample_s: float = 1.0):
    if WINDOWS:
        rows = _pdh_array("\\Network Interface(*)\\Bytes Total/sec", sample_s)
        return sum(v for _, v in rows) * 8 / 1e6 if rows else None
    a = _net_bytes()
    if a is None:
        return None
    time.sleep(sample_s)
    return (_net_bytes() - a) * 8 / 1e6 / sample_s
