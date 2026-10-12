//! Deep per-process details for the selected process, plus a few heuristics
//! ("findings") that point at *why* it is busy.

#[cfg(not(windows))]
use std::collections::HashMap;

/// Cumulative counters straight from Win32; `None` = access denied / not reported.
#[derive(Default, Clone)]
pub struct Raw {
    pub handles: Option<u32>,
    pub priority: Option<&'static str>,
    pub private: Option<u64>,
    pub working_set: Option<u64>,
    pub peak_ws: Option<u64>,
    pub page_faults: Option<u32>,
    /// (kernel, user) CPU time in 100 ns units.
    pub times: Option<(u64, u64)>,
    /// read/write/other: ops then bytes.
    pub io: Option<[u64; 6]>,
}

/// Raw values plus per-second rates against the previous sample of the same process.
#[derive(Default, Clone)]
pub struct Inspect {
    pub raw: Raw,
    /// Whole-machine CPU % (all cores = 100), split by mode.
    pub kernel_pct: Option<f32>,
    pub user_pct: Option<f32>,
    pub faults_s: Option<f64>,
    /// read/write/other: ops/s then bytes/s.
    pub io_s: Option<[f64; 6]>,
    pub services: Vec<String>,
}

pub fn derive(
    prev: Option<&Raw>,
    raw: Raw,
    secs: f64,
    cores: usize,
    services: Vec<String>,
) -> Inspect {
    let per_s = |a: u64, b: u64| b.saturating_sub(a) as f64 / secs;
    let cpu = |a: u64, b: u64| {
        (b.saturating_sub(a) as f64 / 1e7 / secs / cores.max(1) as f64 * 100.0) as f32
    };
    let p = prev.cloned().unwrap_or_default();
    Inspect {
        kernel_pct: p.times.zip(raw.times).map(|(a, b)| cpu(a.0, b.0)),
        user_pct: p.times.zip(raw.times).map(|(a, b)| cpu(a.1, b.1)),
        faults_s: p
            .page_faults
            .zip(raw.page_faults)
            .map(|(a, b)| per_s(a as u64, b as u64)),
        io_s: p
            .io
            .zip(raw.io)
            .map(|(a, b)| std::array::from_fn(|i| per_s(a[i], b[i]))),
        raw,
        services,
    }
}

/// Plain-language hints. Thresholds are rules of thumb, not science.
// ponytail: fixed thresholds; make them relative to machine baseline if they misfire.
pub fn findings(
    i: &Inspect,
    cpu: f32,
    threads: Option<u32>,
    mem_growth: Option<f64>,
    net: Option<(f64, f64)>,
    gpu: f32,
) -> Vec<String> {
    let mut f = Vec::new();
    if let (Some(k), Some(u)) = (i.kernel_pct, i.user_pct)
        && k + u > 1.0
        && k > u
    {
        f.push(format!(
            "{:.0}% of its CPU is kernel time: heavy system calls, file/network I/O or driver work, not its own code",
            k / (k + u) * 100.0
        ));
    }
    if cpu > 1.0
        && i.user_pct.is_some_and(|u| u > 1.0)
        && i.kernel_pct.is_some_and(|k| k < cpu * 0.2)
    {
        f.push("CPU is almost all user time: the program's own computation (busy loop, rendering, scripts)".into());
    }
    if i.faults_s.is_some_and(|x| x > 5000.0) {
        f.push(format!(
            "{:.0} page faults/s: touching lots of memory or memory-mapped files",
            i.faults_s.unwrap()
        ));
    }
    if let Some(io) = i.io_s {
        let (disk, other) = (io[3] + io[4], io[5]);
        if other > 1024.0 * 1024.0 && other > disk {
            f.push(
                "most I/O is 'other' (not read/write): device control, IPC or network calls".into(),
            );
        }
        if io[0] + io[1] > 2000.0 {
            f.push(format!(
                "{:.0} read/write ops/s: many small I/O operations (logging, polling, scanning)",
                io[0] + io[1]
            ));
        }
    }
    if let Some(g) = mem_growth
        && g > 50.0 * 1024.0 * 1024.0
    {
        f.push(format!(
            "memory grew {:.0} MiB in the last minute: allocation burst or leak",
            g / 1024.0 / 1024.0
        ));
    }
    if i.raw.handles.is_some_and(|h| h > 10_000) {
        f.push(format!(
            "{} handles: unusually high, possible handle leak",
            i.raw.handles.unwrap()
        ));
    }
    if threads.is_some_and(|t| t > 300) {
        f.push(format!(
            "{} threads: very high, thread-pool explosion or leak",
            threads.unwrap()
        ));
    }
    if let Some((tx, rx)) = net
        && tx + rx > 1024.0 * 1024.0
    {
        f.push(format!(
            "moving {} on the network ({})",
            crate::ui::rate(tx + rx),
            if tx > rx {
                "mostly uploading"
            } else {
                "mostly downloading"
            }
        ));
    }
    if gpu > 30.0 {
        f.push(format!(
            "keeping a GPU engine {gpu:.0}% busy: rendering, video encode/decode or GPU compute"
        ));
    }
    if !i.services.is_empty() {
        f.push(format!(
            "hosts services: {} — the load comes from one of these",
            i.services.join(", ")
        ));
    }
    f
}

#[cfg(not(windows))]
pub fn raw(_pid: u32) -> Raw {
    Raw::default()
}

#[cfg(not(windows))]
pub fn thread_counts() -> HashMap<u32, u32> {
    HashMap::new()
}

#[cfg(not(windows))]
pub fn services(_pid: u32) -> Vec<String> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn relaunch_as_admin() -> bool {
    false
}

#[cfg(windows)]
pub use imp::{raw, relaunch_as_admin, services, thread_counts};

#[cfg(windows)]
mod imp {
    use super::Raw;
    use std::collections::HashMap;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Services::*;
    use windows_sys::Win32::System::Threading::*;

    fn ft(f: FILETIME) -> u64 {
        (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
    }

    pub fn raw(pid: u32) -> Raw {
        let mut r = Raw::default();
        // SAFETY: handle checked for null and closed; out-structs are sized/zeroed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return r;
            }
            let mut n = 0u32;
            if GetProcessHandleCount(h, &mut n) != 0 {
                r.handles = Some(n);
            }
            r.priority = match GetPriorityClass(h) {
                IDLE_PRIORITY_CLASS => Some("idle"),
                BELOW_NORMAL_PRIORITY_CLASS => Some("below normal"),
                NORMAL_PRIORITY_CLASS => Some("normal"),
                ABOVE_NORMAL_PRIORITY_CLASS => Some("above normal"),
                HIGH_PRIORITY_CLASS => Some("high"),
                REALTIME_PRIORITY_CLASS => Some("realtime"),
                _ => None,
            };
            let mut m: PROCESS_MEMORY_COUNTERS_EX = zeroed();
            m.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            if K32GetProcessMemoryInfo(h, (&mut m as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), m.cb)
                != 0
            {
                r.private = Some(m.PrivateUsage as u64);
                r.working_set = Some(m.WorkingSetSize as u64);
                r.peak_ws = Some(m.PeakWorkingSetSize as u64);
                r.page_faults = Some(m.PageFaultCount);
            }
            let (mut c, mut e, mut k, mut u) = (zeroed(), zeroed(), zeroed(), zeroed());
            if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) != 0 {
                r.times = Some((ft(k), ft(u)));
            }
            let mut io: IO_COUNTERS = zeroed();
            if GetProcessIoCounters(h, &mut io) != 0 {
                r.io = Some([
                    io.ReadOperationCount,
                    io.WriteOperationCount,
                    io.OtherOperationCount,
                    io.ReadTransferCount,
                    io.WriteTransferCount,
                    io.OtherTransferCount,
                ]);
            }
            CloseHandle(h);
        }
        r
    }

    /// One toolhelp snapshot gives every process's thread count.
    pub fn thread_counts() -> HashMap<u32, u32> {
        let mut out = HashMap::new();
        // SAFETY: snapshot handle checked and closed; entry has dwSize set.
        unsafe {
            let s = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if s == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut e: PROCESSENTRY32W = zeroed();
            e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(s, &mut e);
            while ok != 0 {
                out.insert(e.th32ProcessID, e.cntThreads);
                ok = Process32NextW(s, &mut e);
            }
            CloseHandle(s);
        }
        out
    }

    /// Running Win32 services hosted by `pid` (e.g. inside svchost.exe).
    pub fn services(pid: u32) -> Vec<String> {
        let mut out = Vec::new();
        // SAFETY: SCM handle checked and closed; buffer sized from the API's own answer.
        unsafe {
            let scm = OpenSCManagerW(null(), null(), SC_MANAGER_ENUMERATE_SERVICE);
            if scm.is_null() {
                return out;
            }
            let (mut need, mut count, mut resume) = (0u32, 0u32, 0u32);
            let mut buf: Vec<u64> = Vec::new();
            for _ in 0..3 {
                let ok = EnumServicesStatusExW(
                    scm,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_ACTIVE,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                    &mut need,
                    &mut count,
                    &mut resume,
                    null(),
                );
                if ok != 0 {
                    let list = buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>();
                    for i in 0..count as usize {
                        let s = &*list.add(i);
                        if s.ServiceStatusProcess.dwProcessId == pid {
                            out.push(wstr(s.lpServiceName));
                        }
                    }
                    break;
                }
                resume = 0;
                buf = vec![0; need as usize / 8 + 1];
            }
            CloseServiceHandle(scm);
        }
        out
    }

    unsafe fn wstr(p: *const u16) -> String {
        let mut n = 0;
        // SAFETY: caller passes a NUL-terminated wide string owned by the API buffer.
        unsafe {
            while *p.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
        }
    }

    /// Starts this exe again via UAC ("runas"). True if the elevated copy launched.
    pub fn relaunch_as_admin() -> bool {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (verb, file) = (w("runas"), w(&exe.to_string_lossy()));
        // SAFETY: NUL-terminated strings that outlive the call.
        let r =
            unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), null(), null(), 1) };
        r as usize > 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_rates_from_two_samples() {
        let a = Raw {
            times: Some((0, 0)),
            page_faults: Some(100),
            io: Some([0; 6]),
            ..Default::default()
        };
        // 2 s, 4 cores: 1 s kernel + 3 s user = 1 core-s + 3 core-s.
        let b = Raw {
            times: Some((10_000_000, 30_000_000)),
            page_faults: Some(300),
            io: Some([10, 0, 0, 2048, 0, 0]),
            ..Default::default()
        };
        let i = derive(Some(&a), b, 2.0, 4, vec![]);
        assert_eq!(i.kernel_pct, Some(12.5));
        assert_eq!(i.user_pct, Some(37.5));
        assert_eq!(i.faults_s, Some(100.0));
        assert_eq!(i.io_s.unwrap()[0], 5.0);
        assert_eq!(i.io_s.unwrap()[3], 1024.0);
        // first sample has no rates
        assert!(derive(None, a, 1.0, 4, vec![]).kernel_pct.is_none());
    }

    #[test]
    fn findings_explain_kernel_heavy_and_services() {
        let i = Inspect {
            kernel_pct: Some(30.0),
            user_pct: Some(5.0),
            services: vec!["wuauserv".into()],
            ..Default::default()
        };
        let f = findings(&i, 35.0, Some(20), None, None, 0.0);
        assert!(f[0].contains("kernel time"));
        assert!(f.last().unwrap().contains("wuauserv"));
        let f = findings(&Inspect::default(), 0.0, None, None, None, 80.0);
        assert!(f[0].contains("GPU engine 80% busy"));
        assert!(findings(&Inspect::default(), 0.0, None, None, None, 0.0).is_empty());
    }
}
