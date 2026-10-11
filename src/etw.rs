//! Per-process network bytes from the Microsoft-Windows-Kernel-Network ETW provider.
//! This is real traffic (TCP + UDP, IPv4 + IPv6), attributed by the kernel.
//! Creating a trace session needs administrator rights.

// Decoders are only called by the Windows path but tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// pid -> (bytes sent, bytes received) since the last `take()`.
static COUNTS: LazyLock<Mutex<HashMap<u32, (u64, u64)>>> = LazyLock::new(Default::default);

/// Event id -> Some(true) for send, Some(false) for receive.
/// TCPv4 10/11, TCPv6 26/27, UDPv4 42/43, UDPv6 58/59; payload starts with PID, size (u32 each).
fn direction(id: u16) -> Option<bool> {
    match id {
        10 | 26 | 42 | 58 => Some(true),
        11 | 27 | 43 | 59 => Some(false),
        _ => None,
    }
}

fn add(pid: u32, sent: bool, size: u32) {
    let mut m = COUNTS.lock().unwrap();
    let e = m.entry(pid).or_default();
    if sent {
        e.0 += size as u64;
    } else {
        e.1 += size as u64;
    }
}

/// Drains the counters accumulated since the previous call.
pub fn take() -> HashMap<u32, (u64, u64)> {
    std::mem::take(&mut *COUNTS.lock().unwrap())
}

#[cfg(not(windows))]
pub fn start() -> Result<(), String> {
    Err("per-process network needs Windows ETW".into())
}

#[cfg(not(windows))]
pub fn stop() {}

#[cfg(windows)]
pub use imp::{start, stop};

#[cfg(windows)]
mod imp {
    use std::mem::{offset_of, size_of, zeroed};
    use std::ptr::null;
    use windows_sys::Win32::System::Diagnostics::Etw::*;
    use windows_sys::core::GUID;

    const PROVIDER: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
    const KEYWORD_IPV4_IPV6: u64 = 0x30;

    fn name() -> Vec<u16> {
        "reswizz-net".encode_utf16().chain([0]).collect()
    }

    /// The API wants the session name stored right after the properties struct.
    #[repr(C)]
    struct Props {
        p: EVENT_TRACE_PROPERTIES,
        name: [u16; 64],
    }

    fn props() -> Box<Props> {
        // SAFETY: plain C structs; all-zero is the documented starting state.
        let mut b: Box<Props> = Box::new(unsafe { zeroed() });
        b.p.Wnode.BufferSize = size_of::<Props>() as u32;
        b.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        b.p.Wnode.ClientContext = 1; // QPC timestamps
        b.p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        b.p.LoggerNameOffset = offset_of!(Props, name) as u32;
        b
    }

    /// Real-time sessions outlive the process, so this also cleans up after a crash.
    pub fn stop() {
        let mut p = props();
        // SAFETY: valid name and properties buffer; failure (no session) is fine.
        unsafe {
            ControlTraceW(
                CONTROLTRACE_HANDLE { Value: 0 },
                name().as_ptr(),
                &mut p.p,
                EVENT_TRACE_CONTROL_STOP,
            )
        };
    }

    pub fn start() -> Result<(), String> {
        stop();
        let mut h = CONTROLTRACE_HANDLE { Value: 0 };
        let mut p = props();
        // SAFETY: out-handle, NUL-terminated name and a correctly sized properties buffer.
        match unsafe { StartTraceW(&mut h, name().as_ptr(), &mut p.p) } {
            0 => {}
            5 => return Err("needs administrator".into()),
            e => return Err(format!("StartTrace error {e}")),
        }
        // SAFETY: h is the session just started; PROVIDER outlives the call.
        let e = unsafe {
            EnableTraceEx2(
                h,
                &PROVIDER,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                TRACE_LEVEL_INFORMATION as u8,
                KEYWORD_IPV4_IPV6,
                0,
                0,
                null(),
            )
        };
        if e != 0 {
            stop();
            return Err(format!("EnableTrace error {e}"));
        }
        std::thread::spawn(|| {
            let mut n = name();
            // SAFETY: logfile struct zeroed then filled per OpenTraceW docs; `n` outlives the trace.
            unsafe {
                let mut log: EVENT_TRACE_LOGFILEW = zeroed();
                log.LoggerName = n.as_mut_ptr();
                log.Anonymous1.ProcessTraceMode =
                    PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
                log.Anonymous2.EventRecordCallback = Some(on_event);
                let t = OpenTraceW(&mut log);
                if t.Value != u64::MAX {
                    ProcessTrace(&t, 1, null(), null()); // blocks until stop()
                    CloseTrace(t);
                }
            }
        });
        Ok(())
    }

    unsafe extern "system" fn on_event(r: *mut EVENT_RECORD) {
        // SAFETY: ETW passes a valid record for the duration of the callback.
        let r = unsafe { &*r };
        let Some(sent) = super::direction(r.EventHeader.EventDescriptor.Id) else {
            return;
        };
        if r.UserDataLength < 8 {
            return;
        }
        let d = r.UserData as *const u32;
        // SAFETY: at least 8 bytes of user data, checked above.
        let (pid, size) = unsafe { (d.read_unaligned(), d.add(1).read_unaligned()) };
        super::add(pid, sent, size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_event_ids_and_drains() {
        assert_eq!(direction(10), Some(true));
        assert_eq!(direction(59), Some(false));
        assert_eq!(direction(12), None);
        add(7, true, 100);
        add(7, false, 40);
        add(7, true, 1);
        assert_eq!(take().get(&7), Some(&(101, 40)));
        assert!(!take().contains_key(&7));
    }
}
