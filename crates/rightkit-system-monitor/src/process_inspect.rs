//! On-demand inspection of one process: its listening ports and its open files.
//!
//! Nothing here runs in the background. The caller names one process and gets one
//! answer, built from that process alone and bounded in time and size. A list is
//! `None` when it could not be read; an empty list is only ever a real answer.
//!
//! macOS asks `/usr/sbin/lsof` for the one process (read-only, no elevation, fixed
//! path, no shell) instead of declaring libproc socket structs by hand. Windows reads
//! the IP Helper tables for the process's listening TCP ports and bound UDP ports.
//! Open files are not listed on Windows: enumerating handles needs undocumented calls.
//! Other platforms answer with a note.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// The whole call, including the platform query, stops at this.
pub const TIME_LIMIT: Duration = Duration::from_secs(2);
pub const MAX_PORTS: usize = 200;
pub const MAX_FILES: usize = 500;
pub const MAX_CONNECTIONS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Port {
    /// "tcp" or "udp".
    pub protocol: String,
    /// "IPv4" or "IPv6".
    pub family: String,
    /// The local address as the system reports it, e.g. "127.0.0.1" or "::".
    pub address: String,
    pub port: u16,
}

/// One established connection of the process: where it talks to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Connection {
    /// "tcp" or "udp".
    pub protocol: String,
    /// "IPv4" or "IPv6".
    pub family: String,
    /// The remote address as the system reports it. This is an address, not a host name:
    /// no reverse lookup is made, so nothing leaves the computer to resolve it.
    pub remote_address: String,
    pub remote_port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInspection {
    /// None when the listening ports could not be read.
    pub listening: Option<Vec<Port>>,
    /// Established connections (the remote ends the process talks to). None when they
    /// could not be read.
    pub connections: Option<Vec<Connection>>,
    /// None when the open files could not be read.
    pub open_files: Option<Vec<String>>,
    /// Why a list is missing or incomplete, and when a list was capped.
    pub notes: Vec<String>,
}

/// What a platform query found before the caps are applied.
#[derive(Default)]
struct Parts {
    listening: Option<Vec<Port>>,
    connections: Option<Vec<Connection>>,
    open_files: Option<Vec<String>>,
    notes: Vec<String>,
}

/// The start time and owner (uid) of `pid` right now, or None when no such process exists.
/// A start time of 0 means the platform reported none.
pub fn live_identity(pid: u32) -> Option<(u64, Option<u32>)> {
    let mut system = System::new().ok()?;
    let target = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[target]),
        true,
        ProcessRefreshKind::everything(),
    );
    system.process(target).map(|p| {
        // The numeric owner exists on unix only; Windows identifies owners by SID.
        #[cfg(unix)]
        let owner = p.user_id().map(|u| **u);
        #[cfg(not(unix))]
        let owner: Option<u32> = None;
        (p.start_time(), owner)
    })
}

/// The listening ports and open files of one process, within `TIME_LIMIT`.
pub fn inspect(pid: u32) -> ProcessInspection {
    let parts = gather(pid, Instant::now() + TIME_LIMIT);
    let mut notes = parts.notes;
    let listening = bounded(parts.listening, MAX_PORTS, "listening ports", &mut notes);
    let open_files = bounded(parts.open_files, MAX_FILES, "open files", &mut notes);
    let connections = bounded(
        parts.connections,
        MAX_CONNECTIONS,
        "connections",
        &mut notes,
    );
    ProcessInspection {
        listening,
        connections,
        open_files,
        notes,
    }
}

/// Keeps the first `max` items and says so in a note when the list was longer.
fn bounded<T>(
    list: Option<Vec<T>>,
    max: usize,
    what: &str,
    notes: &mut Vec<String>,
) -> Option<Vec<T>> {
    list.map(|mut items| {
        if items.len() > max {
            items.truncate(max);
            notes.push(format!("Showing the first {max} {what}."));
        }
        items
    })
}

#[cfg(target_os = "macos")]
fn gather(pid: u32, deadline: Instant) -> Parts {
    darwin::gather(pid, deadline)
}

#[cfg(windows)]
fn gather(pid: u32, deadline: Instant) -> Parts {
    windows::gather(pid, deadline)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn gather(_pid: u32, _deadline: Instant) -> Parts {
    Parts {
        notes: vec!["Process inspection is not available on this platform.".into()],
        ..Parts::default()
    }
}

/// macOS: `lsof -nP -p <pid> -F ftnPT`, run only for the current user's processes.
#[cfg(target_os = "macos")]
mod darwin {
    use std::collections::HashSet;
    use std::time::Instant;

    use rightkit_child_process::{RunOptions, run};

    use super::{Connection, Parts, Port, live_identity};

    /// Fixed path; the arguments are a list, never a shell string.
    const LSOF: &str = "/usr/sbin/lsof";
    /// Output read from lsof; more than this is cut and noted.
    const OUTPUT_CAP: usize = 1024 * 1024;

    fn unknown(note: &str) -> Parts {
        Parts {
            notes: vec![note.to_string()],
            ..Parts::default()
        }
    }

    pub fn gather(pid: u32, deadline: Instant) -> Parts {
        let Some((_, owner)) = live_identity(pid) else {
            return unknown("That process has already exited.");
        };
        // SAFETY: getuid has no preconditions and cannot fail.
        let me = unsafe { libc::getuid() };
        if owner != Some(me) {
            return unknown("Needs the process's owner");
        }
        let run = match run_lsof(pid, deadline) {
            Ok(run) => run,
            Err(error) => return unknown(&format!("lsof could not run: {error}")),
        };
        if run.text.is_empty() {
            return unknown(if run.timed_out {
                "lsof did not answer within 2 seconds."
            } else {
                "lsof could not read this process."
            });
        }
        let mut notes = Vec::new();
        if run.timed_out {
            notes.push("lsof did not finish within 2 seconds; the lists may be incomplete.".into());
        }
        if run.truncated {
            notes.push("lsof output was over 1 MiB; the lists may be incomplete.".into());
        }
        let (listening, open_files, connections) = parse(&run.text);
        Parts {
            listening: Some(listening),
            connections: Some(connections),
            open_files: Some(open_files),
            notes,
        }
    }

    struct Run {
        text: String,
        timed_out: bool,
        truncated: bool,
    }

    /// Runs lsof until it ends or the deadline passes, when its tree is stopped. Whatever it
    /// printed is kept, up to the cap.
    fn run_lsof(pid: u32, deadline: Instant) -> Result<Run, String> {
        let pid_text = pid.to_string();
        let options = RunOptions::new(deadline.saturating_duration_since(Instant::now()))
            .stdout_cap(OUTPUT_CAP)
            .stderr_cap(0);
        let output = run(
            LSOF,
            ["-nP", "-p", pid_text.as_str(), "-F", "ftnPT"],
            &options,
        )
        .map_err(|e| e.to_string())?;
        let timed_out = output.timed_out;
        let truncated = output.stdout.truncated;
        let mut text = String::from_utf8_lossy(&output.stdout.bytes).into_owned();
        if truncated {
            // A record cut in the middle would parse as a wrong port; drop the partial line.
            if let Some(end) = text.rfind('\n') {
                text.truncate(end + 1);
            }
        }
        Ok(Run {
            text,
            timed_out,
            truncated,
        })
    }

    /// One lsof record: a file descriptor or process entry with its fields.
    #[derive(Default, PartialEq)]
    struct Record {
        /// `t` field: IPv4, IPv6, REG, DIR, CHR, unix, PIPE and so on.
        kind: String,
        /// `P` field: TCP or UDP for sockets.
        protocol: String,
        /// `n` field: a path for files, `address:port` for sockets.
        name: String,
        /// `TST=` field: a TCP state such as LISTEN.
        state: String,
    }

    fn parse(text: &str) -> (Vec<Port>, Vec<String>, Vec<Connection>) {
        let mut records: Vec<Record> = Vec::new();
        let mut current = Record::default();
        for line in text.lines() {
            let mut chars = line.chars();
            let Some(tag) = chars.next() else {
                continue;
            };
            let value = chars.as_str();
            match tag {
                // `p` starts a process entry and `f` a descriptor; each ends the record before it.
                'p' | 'f' => {
                    if current != Record::default() {
                        records.push(std::mem::take(&mut current));
                    }
                }
                't' => current.kind = value.to_string(),
                'P' => current.protocol = value.to_string(),
                'n' => current.name = value.to_string(),
                'T' => {
                    if let Some(state) = value.strip_prefix("ST=") {
                        current.state = state.to_string();
                    }
                }
                _ => {}
            }
        }
        if current != Record::default() {
            records.push(current);
        }

        let mut ports: Vec<Port> = Vec::new();
        let mut seen_ports: HashSet<Port> = HashSet::new();
        let mut files: Vec<String> = Vec::new();
        let mut seen_files: HashSet<String> = HashSet::new();
        let mut connections: Vec<Connection> = Vec::new();
        let mut seen_connections: HashSet<Connection> = HashSet::new();
        for record in &records {
            match record.kind.as_str() {
                "IPv4" | "IPv6" => {
                    if let Some(port) = socket_port(record)
                        && seen_ports.insert(port.clone())
                    {
                        ports.push(port);
                    }
                    if let Some(connection) = socket_connection(record)
                        && seen_connections.insert(connection.clone())
                    {
                        connections.push(connection);
                    }
                }
                "REG" | "DIR" | "CHR" | "BLK"
                    if record.name.starts_with('/') && seen_files.insert(record.name.clone()) =>
                {
                    files.push(record.name.clone());
                }
                _ => {}
            }
        }
        (ports, files, connections)
    }

    /// A connected socket's remote end: a TCP socket in ESTABLISHED, or a connected UDP
    /// socket. `local->remote` names are the only ones lsof prints for a connected socket.
    fn socket_connection(record: &Record) -> Option<Connection> {
        let (_, remote) = record.name.split_once("->")?;
        let protocol = match record.protocol.as_str() {
            "TCP" if record.state == "ESTABLISHED" => "tcp",
            "UDP" => "udp",
            _ => return None,
        };
        let (address, port) = remote.rsplit_once(':')?;
        let port: u16 = port.parse().ok()?;
        let address = address.trim_start_matches('[').trim_end_matches(']');
        let family = if record.kind == "IPv6" {
            "IPv6"
        } else {
            "IPv4"
        };
        Some(Connection {
            protocol: protocol.to_string(),
            family: family.to_string(),
            remote_address: address.to_string(),
            remote_port: port,
        })
    }

    /// A bound TCP socket in LISTEN, or a bound UDP socket. Connected sockets are skipped.
    fn socket_port(record: &Record) -> Option<Port> {
        if record.name.contains("->") {
            return None;
        }
        let protocol = match record.protocol.as_str() {
            "TCP" if record.state == "LISTEN" => "tcp",
            "UDP" => "udp",
            _ => return None,
        };
        let (address, port) = record.name.rsplit_once(':')?;
        let port: u16 = port.parse().ok()?;
        let address = address.trim_start_matches('[').trim_end_matches(']');
        let family = if record.kind == "IPv6" {
            "IPv6"
        } else {
            "IPv4"
        };
        Some(Port {
            protocol: protocol.to_string(),
            family: family.to_string(),
            address: address.to_string(),
            port,
        })
    }
}

/// Windows: listening TCP ports and bound UDP ports from the IP Helper owner-PID tables.
/// Open files are not available here.
#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::Instant;

    use super::{Connection, Parts, Port};

    const NO_ERROR: u32 = 0;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    /// TCP_TABLE_CLASS: listeners with their owning pid.
    const TCP_TABLE_OWNER_PID_LISTENER: u32 = 3;
    /// UDP_TABLE_CLASS: bound endpoints with their owning pid.
    const UDP_TABLE_OWNER_PID: u32 = 1;
    /// TCP_TABLE_CLASS: every connection with its owning pid.
    const TCP_TABLE_OWNER_PID_ALL: u32 = 5;
    /// MIB_TCP_STATE_LISTEN.
    const TCP_STATE_LISTEN: u32 = 2;
    /// MIB_TCP_STATE_ESTAB.
    const TCP_STATE_ESTABLISHED: u32 = 5;

    #[link(name = "iphlpapi")]
    #[allow(non_snake_case)]
    unsafe extern "system" {
        fn GetExtendedTcpTable(
            table: *mut c_void,
            size: *mut u32,
            order: i32,
            family: u32,
            class: u32,
            reserved: u32,
        ) -> u32;
        fn GetExtendedUdpTable(
            table: *mut c_void,
            size: *mut u32,
            order: i32,
            family: u32,
            class: u32,
            reserved: u32,
        ) -> u32;
    }

    // Layouts of MIB_TCPROW_OWNER_PID, MIB_TCP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID and
    // MIB_UDP6ROW_OWNER_PID from iprtrmib.h / tcpmib.h / udpmib.h. Each table starts with
    // a u32 entry count, followed by the rows. Ports are network order in the low 16 bits.
    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TcpRow4 {
        state: u32,
        local_addr: u32,
        local_port: u32,
        remote_addr: u32,
        remote_port: u32,
        owner: u32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TcpRow6 {
        local_addr: [u8; 16],
        local_scope: u32,
        local_port: u32,
        remote_addr: [u8; 16],
        remote_scope: u32,
        remote_port: u32,
        state: u32,
        owner: u32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UdpRow4 {
        local_addr: u32,
        local_port: u32,
        owner: u32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UdpRow6 {
        local_addr: [u8; 16],
        local_scope: u32,
        local_port: u32,
        owner: u32,
    }

    /// Calls an IP Helper table function until its buffer is large enough.
    fn table(call: impl Fn(*mut c_void, *mut u32) -> u32) -> Result<Vec<u8>, u32> {
        let mut size: u32 = 0;
        for _ in 0..4 {
            let mut buffer = vec![0u8; size as usize];
            let code = call(buffer.as_mut_ptr().cast(), &mut size);
            if code == NO_ERROR {
                buffer.truncate(size as usize);
                return Ok(buffer);
            }
            if code != ERROR_INSUFFICIENT_BUFFER {
                return Err(code);
            }
        }
        Err(ERROR_INSUFFICIENT_BUFFER)
    }

    /// The rows of a table buffer, bounded by both its count and its length.
    fn rows<T: Copy>(bytes: &[u8]) -> Vec<T> {
        let size = std::mem::size_of::<T>();
        if bytes.len() < 4 {
            return Vec::new();
        }
        let mut head = [0u8; 4];
        head.copy_from_slice(&bytes[..4]);
        let available = (bytes.len() - 4) / size;
        let count = (u32::from_ne_bytes(head) as usize).min(available);
        (0..count)
            .map(|i| {
                let offset = 4 + i * size;
                // SAFETY: `offset + size` lies inside `bytes`, checked above; the read is unaligned.
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().add(offset).cast::<T>()) }
            })
            .collect()
    }

    fn port_of(raw: u32) -> u16 {
        u16::from_be(raw as u16)
    }

    fn listeners(pid: u32) -> Result<Vec<Port>, u32> {
        let mut out: Vec<Port> = Vec::new();
        let tcp4 = table(|buf, size| unsafe {
            GetExtendedTcpTable(buf, size, 0, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0)
        })?;
        for row in rows::<TcpRow4>(&tcp4) {
            if row.owner == pid && row.state == TCP_STATE_LISTEN {
                out.push(Port {
                    protocol: "tcp".into(),
                    family: "IPv4".into(),
                    address: Ipv4Addr::from(row.local_addr.to_ne_bytes()).to_string(),
                    port: port_of(row.local_port),
                });
            }
        }
        let tcp6 = table(|buf, size| unsafe {
            GetExtendedTcpTable(buf, size, 0, AF_INET6, TCP_TABLE_OWNER_PID_LISTENER, 0)
        })?;
        for row in rows::<TcpRow6>(&tcp6) {
            if row.owner == pid && row.state == TCP_STATE_LISTEN {
                out.push(Port {
                    protocol: "tcp".into(),
                    family: "IPv6".into(),
                    address: Ipv6Addr::from(row.local_addr).to_string(),
                    port: port_of(row.local_port),
                });
            }
        }
        let udp4 = table(|buf, size| unsafe {
            GetExtendedUdpTable(buf, size, 0, AF_INET, UDP_TABLE_OWNER_PID, 0)
        })?;
        for row in rows::<UdpRow4>(&udp4) {
            if row.owner == pid {
                out.push(Port {
                    protocol: "udp".into(),
                    family: "IPv4".into(),
                    address: Ipv4Addr::from(row.local_addr.to_ne_bytes()).to_string(),
                    port: port_of(row.local_port),
                });
            }
        }
        let udp6 = table(|buf, size| unsafe {
            GetExtendedUdpTable(buf, size, 0, AF_INET6, UDP_TABLE_OWNER_PID, 0)
        })?;
        for row in rows::<UdpRow6>(&udp6) {
            if row.owner == pid {
                out.push(Port {
                    protocol: "udp".into(),
                    family: "IPv6".into(),
                    address: Ipv6Addr::from(row.local_addr).to_string(),
                    port: port_of(row.local_port),
                });
            }
        }
        out.sort_by_key(|port| port.port);
        Ok(out)
    }

    /// The remote ends of the process's established TCP connections (the IP Helper tables
    /// hold no per-process UDP peer). Addresses only; nothing is resolved to a name.
    fn established(pid: u32) -> Result<Vec<Connection>, u32> {
        let mut out: Vec<Connection> = Vec::new();
        let tcp4 = table(|buf, size| unsafe {
            GetExtendedTcpTable(buf, size, 0, AF_INET, TCP_TABLE_OWNER_PID_ALL, 0)
        })?;
        for row in rows::<TcpRow4>(&tcp4) {
            if row.owner == pid && row.state == TCP_STATE_ESTABLISHED {
                out.push(Connection {
                    protocol: "tcp".into(),
                    family: "IPv4".into(),
                    remote_address: Ipv4Addr::from(row.remote_addr.to_ne_bytes()).to_string(),
                    remote_port: port_of(row.remote_port),
                });
            }
        }
        let tcp6 = table(|buf, size| unsafe {
            GetExtendedTcpTable(buf, size, 0, AF_INET6, TCP_TABLE_OWNER_PID_ALL, 0)
        })?;
        for row in rows::<TcpRow6>(&tcp6) {
            if row.owner == pid && row.state == TCP_STATE_ESTABLISHED {
                out.push(Connection {
                    protocol: "tcp".into(),
                    family: "IPv6".into(),
                    remote_address: Ipv6Addr::from(row.remote_addr).to_string(),
                    remote_port: port_of(row.remote_port),
                });
            }
        }
        out.sort_by(|a, b| {
            (&a.remote_address, a.remote_port).cmp(&(&b.remote_address, b.remote_port))
        });
        out.dedup();
        Ok(out)
    }

    pub fn gather(pid: u32, _deadline: Instant) -> Parts {
        let mut notes = vec!["Open files are not available on Windows.".to_string()];
        let connections = match established(pid) {
            Ok(list) => Some(list),
            Err(code) => {
                notes.push(format!(
                    "The connection table could not be read (Windows error {code})."
                ));
                None
            }
        };
        match listeners(pid) {
            Ok(ports) => Parts {
                listening: Some(ports),
                connections,
                open_files: None,
                notes,
            },
            Err(code) => {
                notes.push(format!(
                    "The network table could not be read (Windows error {code})."
                ));
                Parts {
                    listening: None,
                    connections,
                    open_files: None,
                    notes,
                }
            }
        }
    }
}
