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

/// Authenticode result for the exe (embedded or catalog signature).
#[derive(Clone, Debug, PartialEq)]
pub enum Sig {
    /// Valid; the signer's display name.
    Signed(String),
    Unsigned,
    /// Signature present but not trusted; why.
    Bad(String),
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
    /// `None` = not checked (no exe path or file unreadable).
    pub sig: Option<Sig>,
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
        sig: None,
    }
}

/// Folder part of a Windows exe path.
pub fn dir_of(exe: &str) -> &str {
    exe.rsplit_once(['\\', '/']).map_or("", |x| x.0)
}

/// Windows binaries that malware likes to name itself after, with their real home.
const SYSTEM_NAMES: &[(&str, &str)] = &[
    ("svchost.exe", "\\windows\\system32"),
    ("lsass.exe", "\\windows\\system32"),
    ("csrss.exe", "\\windows\\system32"),
    ("smss.exe", "\\windows\\system32"),
    ("services.exe", "\\windows\\system32"),
    ("wininit.exe", "\\windows\\system32"),
    ("winlogon.exe", "\\windows\\system32"),
    ("spoolsv.exe", "\\windows\\system32"),
    ("taskhostw.exe", "\\windows\\system32"),
    ("dllhost.exe", "\\windows\\system32"),
    ("conhost.exe", "\\windows\\system32"),
    ("rundll32.exe", "\\windows\\system32"),
    ("explorer.exe", "\\windows"),
];

/// Hints about where the executable lives; `on_disk` = the exe file still exists.
// ponytail: path heuristics only; add Authenticode (WinVerifyTrust) if these are not enough.
pub fn location_findings(exe: &str, on_disk: bool) -> Vec<String> {
    let mut f = Vec::new();
    let low = exe.to_lowercase();
    let (dir, name) = low.rsplit_once(['\\', '/']).unwrap_or(("", &low));
    if !on_disk {
        f.push("the exe file is gone from disk: deleted or replaced after start".into());
    }
    if let Some((_, home)) = SYSTEM_NAMES.iter().find(|(n, _)| *n == name) {
        let ok = dir.ends_with(home)
            || (home.ends_with("system32") && dir.ends_with("\\windows\\syswow64"));
        if !ok {
            f.push(format!(
                "named like a Windows system file ({name}) but not in its usual folder: suspicious"
            ));
        }
    }
    let where_ = if low.contains("\\temp\\") || low.contains("\\tmp\\") {
        Some("a temp folder: installers do this, malware too")
    } else if low.contains("\\downloads\\") {
        Some("Downloads: something you started by hand")
    } else if low.contains("\\users\\public\\") || low.contains("\\$recycle.bin\\") {
        Some("a shared/odd folder (Public, Recycle Bin): rarely legitimate")
    } else if low.contains("\\appdata\\") {
        Some(
            "user AppData: normal for per-user apps (Teams, Discord, updaters), check it if you don't know it",
        )
    } else {
        None
    };
    if let Some(w) = where_ {
        f.push(format!("runs from {w}"));
    }
    f
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
    match &i.sig {
        Some(Sig::Unsigned) => f.push(
            "exe has no digital signature: fine for small tools and your own builds, rare for system or big-vendor software".into(),
        ),
        Some(Sig::Bad(why)) => f.push(format!(
            "signature is INVALID ({why}): suspicious, the file may have been tampered with"
        )),
        _ => {}
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
pub fn signature(_exe: &str) -> Option<Sig> {
    None
}

#[cfg(not(windows))]
pub fn relaunch_as_admin() -> bool {
    false
}

#[cfg(windows)]
pub use imp::{raw, relaunch_as_admin, services, signature, thread_counts};

#[cfg(windows)]
mod imp {
    use super::{Raw, Sig};
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

    /// Checks the embedded Authenticode signature, then the system catalogs (most of
    /// System32 is catalog-signed). Revocation is not checked online: no network calls.
    pub fn signature(exe: &str) -> Option<Sig> {
        use windows_sys::Win32::Foundation::*;
        use windows_sys::Win32::Security::Cryptography::Catalog::*;
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_SHA1_ALGORITHM, BCRYPT_SHA256_ALGORITHM,
        };
        use windows_sys::Win32::Security::WinTrust::*;
        use windows_sys::Win32::Storage::FileSystem::*;

        let path: Vec<u16> = exe.encode_utf16().chain([0]).collect();
        let mut file = WINTRUST_FILE_INFO {
            cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: path.as_ptr(),
            ..Default::default()
        };
        let (r, signer) = verify(WTD_CHOICE_FILE, WINTRUST_DATA_0 { pFile: &mut file });
        if !matches!(r, TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN) {
            return sig(r, signer);
        }
        // SAFETY: handles checked and released; buffers outlive every call that reads them.
        unsafe {
            let h = CreateFileW(
                path.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut out = Some(Sig::Unsigned);
            for alg in [BCRYPT_SHA256_ALGORITHM, BCRYPT_SHA1_ALGORITHM] {
                let mut admin = 0isize;
                if CryptCATAdminAcquireContext2(&mut admin, null(), alg, null(), 0) == 0 {
                    continue;
                }
                let (mut hash, mut n) = ([0u8; 64], 64u32);
                let cat =
                    if CryptCATAdminCalcHashFromFileHandle2(admin, h, &mut n, hash.as_mut_ptr(), 0)
                        != 0
                    {
                        CryptCATAdminEnumCatalogFromHash(admin, hash.as_ptr(), n, 0, null_mut())
                    } else {
                        0
                    };
                let mut info = CATALOG_INFO {
                    cbStruct: size_of::<CATALOG_INFO>() as u32,
                    ..Default::default()
                };
                if cat != 0 && CryptCATCatalogInfoFromContext(cat, &mut info, 0) != 0 {
                    let tag: Vec<u16> = hash[..n as usize]
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<String>()
                        .encode_utf16()
                        .chain([0])
                        .collect();
                    let mut c = WINTRUST_CATALOG_INFO {
                        cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
                        pcwszCatalogFilePath: info.wszCatalogFile.as_ptr(),
                        pcwszMemberTag: tag.as_ptr(),
                        pcwszMemberFilePath: path.as_ptr(),
                        hMemberFile: h,
                        pbCalculatedFileHash: hash.as_mut_ptr(),
                        cbCalculatedFileHash: n,
                        hCatAdmin: admin,
                        ..Default::default()
                    };
                    let (r, signer) =
                        verify(WTD_CHOICE_CATALOG, WINTRUST_DATA_0 { pCatalog: &mut c });
                    out = sig(r, signer);
                }
                if cat != 0 {
                    CryptCATAdminReleaseCatalogContext(admin, cat, 0);
                }
                CryptCATAdminReleaseContext(admin, 0);
                if cat != 0 {
                    break;
                }
            }
            CloseHandle(h);
            out
        }
    }

    /// One WinVerifyTrust round trip: (status, signer display name).
    fn verify(
        choice: u32,
        what: windows_sys::Win32::Security::WinTrust::WINTRUST_DATA_0,
    ) -> (i32, Option<String>) {
        use windows_sys::Win32::Security::Cryptography::{
            CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW,
        };
        use windows_sys::Win32::Security::WinTrust::*;
        let mut d = WINTRUST_DATA {
            cbStruct: size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: choice,
            Anonymous: what,
            dwStateAction: WTD_STATEACTION_VERIFY,
            dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
            ..Default::default()
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: `d` and what it points to outlive both calls; state is closed below.
        unsafe {
            let r = WinVerifyTrust(
                null_mut(),
                &mut action,
                (&mut d as *mut WINTRUST_DATA).cast(),
            );
            let mut signer = None;
            let pd = WTHelperProvDataFromStateData(d.hWVTStateData);
            if !pd.is_null() {
                let s = WTHelperGetProvSignerFromChain(pd, 0, 0, 0);
                if !s.is_null() && (*s).csCertChain > 0 {
                    let cert = (*(*s).pasCertChain).pCert;
                    let mut buf = [0u16; 256];
                    let n = CertGetNameStringW(
                        cert,
                        CERT_NAME_SIMPLE_DISPLAY_TYPE,
                        0,
                        null(),
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                    );
                    if n > 1 {
                        signer = Some(String::from_utf16_lossy(&buf[..n as usize - 1]));
                    }
                }
            }
            d.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(
                null_mut(),
                &mut action,
                (&mut d as *mut WINTRUST_DATA).cast(),
            );
            (r, signer)
        }
    }

    fn sig(r: i32, signer: Option<String>) -> Option<Sig> {
        use windows_sys::Win32::Foundation::*;
        let why = match r {
            0 => {
                return Some(Sig::Signed(
                    signer.unwrap_or_else(|| "unknown signer".into()),
                ));
            }
            TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN => return Some(Sig::Unsigned),
            // 0x8007xxxx = plain Win32 error (access denied, file locked): could not check
            CRYPT_E_FILE_ERROR => return None,
            _ if (r as u32) >> 16 == 0x8007 => return None,
            TRUST_E_BAD_DIGEST => "file changed after signing".into(),
            CERT_E_UNTRUSTEDROOT => "untrusted root, self-signed?".into(),
            TRUST_E_EXPLICIT_DISTRUST => "certificate explicitly distrusted".into(),
            CERT_E_REVOKED => "certificate revoked".into(),
            CERT_E_EXPIRED => "certificate expired".into(),
            _ => format!("error 0x{:08X}", r as u32),
        };
        Some(Sig::Bad(match signer {
            Some(s) => format!("{why}, signer {s}"),
            None => why,
        }))
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
        let bad = Inspect {
            sig: Some(Sig::Bad("file changed after signing".into())),
            ..Default::default()
        };
        assert!(findings(&bad, 0.0, None, None, None, 0.0)[0].contains("INVALID"));
        let ok = Inspect {
            sig: Some(Sig::Signed("Microsoft Windows".into())),
            ..Default::default()
        };
        assert!(findings(&ok, 0.0, None, None, None, 0.0).is_empty());
    }

    #[test]
    fn location_flags_odd_folders_and_fake_system_names() {
        assert_eq!(
            dir_of(r"C:\Windows\System32\svchost.exe"),
            r"C:\Windows\System32"
        );
        assert!(location_findings(r"C:\Windows\System32\svchost.exe", true).is_empty());
        assert!(location_findings(r"C:\Windows\SysWOW64\rundll32.exe", true).is_empty());
        assert!(location_findings(r"C:\Windows\explorer.exe", true).is_empty());
        assert!(location_findings(r"C:\Program Files\Foo\foo.exe", true).is_empty());
        let fake = location_findings(r"C:\Users\me\AppData\Local\Temp\svchost.exe", true);
        assert!(fake[0].contains("system file"));
        assert!(fake[1].contains("temp"));
        assert!(location_findings(r"C:\Users\me\Downloads\x.exe", false)[0].contains("gone"));
    }
}

#[cfg(all(test, windows))]
mod live {
    use super::*;

    #[test]
    fn verifies_embedded_and_catalog_signatures() {
        for exe in [
            r"C:\Windows\System32\svchost.exe",
            r"C:\Windows\System32\calc.exe",
            r"C:\Windows\explorer.exe",
        ] {
            let s = signature(exe);
            eprintln!("{exe}: {s:?}");
            assert!(
                matches!(s, Some(Sig::Signed(ref p)) if p.contains("Microsoft")),
                "{exe}"
            );
        }
        let dir = std::env::temp_dir().join("reswizz-sig-test.exe");
        std::fs::write(&dir, b"MZ not a real program").unwrap();
        let s = signature(&dir.display().to_string());
        std::fs::remove_file(&dir).ok();
        assert_eq!(s, Some(Sig::Unsigned));
        assert_eq!(signature(r"C:\no\such\file.exe"), None);
    }
}
