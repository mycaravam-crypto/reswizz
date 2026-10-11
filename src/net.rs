//! TCP/UDP endpoint enumeration via IP Helper (`GetExtended{Tcp,Udp}Table`).
//! These are open sockets, not traffic.

// Decoders are only called by the Windows path but tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

#[derive(Clone)]
pub struct Endpoint {
    pub proto: &'static str,
    pub local: SocketAddr,
    pub remote: Option<SocketAddr>,
    pub state: &'static str,
    pub pid: u32,
}

/// Port fields are u32 with the port in the low 16 bits, network byte order.
fn port(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

/// IPv4 fields are u32 holding the address bytes in network order.
fn v4(addr: u32, p: u32) -> SocketAddr {
    SocketAddrV4::new(Ipv4Addr::from(addr.to_ne_bytes()), port(p)).into()
}

fn v6(addr: [u8; 16], scope: u32, p: u32) -> SocketAddr {
    SocketAddrV6::new(Ipv6Addr::from(addr), port(p), 0, scope).into()
}

fn tcp_state(s: u32) -> &'static str {
    match s {
        1 => "CLOSED",
        2 => "LISTEN",
        3 => "SYN_SENT",
        4 => "SYN_RCVD",
        5 => "ESTABLISHED",
        6 => "FIN_WAIT1",
        7 => "FIN_WAIT2",
        8 => "CLOSE_WAIT",
        9 => "CLOSING",
        10 => "LAST_ACK",
        11 => "TIME_WAIT",
        12 => "DELETE_TCB",
        _ => "UNKNOWN",
    }
}

#[cfg(not(windows))]
pub fn endpoints() -> Result<Vec<Endpoint>, String> {
    Err("endpoint enumeration is only implemented on Windows".into())
}

#[cfg(windows)]
pub fn endpoints() -> Result<Vec<Endpoint>, String> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::*;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    /// Calls a size-probing Win32 table API until the buffer fits, then returns its rows.
    fn table<R: Copy>(call: impl Fn(*mut c_void, *mut u32) -> u32) -> Result<Vec<R>, String> {
        let mut buf = vec![0u64; 4096]; // u64 for alignment
        let mut size = (buf.len() * 8) as u32;
        for _ in 0..5 {
            match call(buf.as_mut_ptr().cast(), &mut size) {
                NO_ERROR => {
                    // Layout: u32 count, then rows (4-byte aligned).
                    let base = buf.as_ptr().cast::<u8>();
                    // SAFETY: the API filled `buf` with a count followed by `count` rows of R.
                    unsafe {
                        let n = base.cast::<u32>().read() as usize;
                        let rows = base.add(4).cast::<R>();
                        return Ok((0..n).map(|i| rows.add(i).read_unaligned()).collect());
                    }
                }
                ERROR_INSUFFICIENT_BUFFER => buf = vec![0; (size as usize).div_ceil(8) + 1],
                e => return Err(format!("IP Helper error {e}")),
            }
        }
        Err("IP Helper table kept growing".into())
    }

    let tcp = |af: u16| {
        move |p, s| unsafe { GetExtendedTcpTable(p, s, 0, af as u32, TCP_TABLE_OWNER_PID_ALL, 0) }
    };
    let udp = |af: u16| {
        move |p, s| unsafe { GetExtendedUdpTable(p, s, 0, af as u32, UDP_TABLE_OWNER_PID, 0) }
    };

    let mut out = Vec::new();
    for r in table::<MIB_TCPROW_OWNER_PID>(tcp(AF_INET))? {
        out.push(Endpoint {
            proto: "TCP",
            local: v4(r.dwLocalAddr, r.dwLocalPort),
            remote: Some(v4(r.dwRemoteAddr, r.dwRemotePort)),
            state: tcp_state(r.dwState),
            pid: r.dwOwningPid,
        });
    }
    for r in table::<MIB_TCP6ROW_OWNER_PID>(tcp(AF_INET6))? {
        out.push(Endpoint {
            proto: "TCP",
            local: v6(r.ucLocalAddr, r.dwLocalScopeId, r.dwLocalPort),
            remote: Some(v6(r.ucRemoteAddr, r.dwRemoteScopeId, r.dwRemotePort)),
            state: tcp_state(r.dwState),
            pid: r.dwOwningPid,
        });
    }
    for r in table::<MIB_UDPROW_OWNER_PID>(udp(AF_INET))? {
        out.push(Endpoint {
            proto: "UDP",
            local: v4(r.dwLocalAddr, r.dwLocalPort),
            remote: None,
            state: "",
            pid: r.dwOwningPid,
        });
    }
    for r in table::<MIB_UDP6ROW_OWNER_PID>(udp(AF_INET6))? {
        out.push(Endpoint {
            proto: "UDP",
            local: v6(r.ucLocalAddr, r.dwLocalScopeId, r.dwLocalPort),
            remote: None,
            state: "",
            pid: r.dwOwningPid,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_network_byte_order() {
        // 127.0.0.1:8080 as Windows lays it out in memory.
        let addr = u32::from_ne_bytes([127, 0, 0, 1]);
        let p = u32::from(8080u16.to_be());
        assert_eq!(v4(addr, p).to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn formats_ipv6() {
        let p = u32::from(443u16.to_be());
        assert_eq!(
            v6(Ipv6Addr::LOCALHOST.octets(), 0, p).to_string(),
            "[::1]:443"
        );
        assert_eq!(
            v6(Ipv6Addr::LOCALHOST.octets(), 3, p).to_string(),
            "[::1%3]:443"
        );
    }

    #[test]
    fn maps_tcp_states() {
        assert_eq!(tcp_state(2), "LISTEN");
        assert_eq!(tcp_state(5), "ESTABLISHED");
        assert_eq!(tcp_state(11), "TIME_WAIT");
        assert_eq!(tcp_state(99), "UNKNOWN");
    }

    #[cfg(windows)]
    #[test]
    fn enumerates_live_endpoints() {
        let eps = endpoints().unwrap();
        assert!(eps.iter().any(|e| e.proto == "TCP" && e.state == "LISTEN"));
        assert!(eps.iter().any(|e| e.proto == "UDP"));
    }
}
