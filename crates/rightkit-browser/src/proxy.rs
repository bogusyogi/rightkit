//! In-process pinning proxy. Chrome is started with `--proxy-server` pointing
//! here (loopback included, no bypass), so Chrome never resolves names itself:
//! this proxy resolves once through [`NetworkPolicy::pin`], checks the resolved
//! addresses against the policy, and connects to the vetted address. A name
//! that flips from public to private between check and connect (DNS
//! rebinding), or a request that bypassed page interception (workers), can
//! therefore never reach a blocked address.
use crate::policy::{BrowserEvent, Guard};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// One connection the proxy made, with the address it pinned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedConnection {
    pub host: String,
    pub port: u16,
    pub ip: IpAddr,
}

pub(crate) struct PinProxy {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<Vec<PinnedConnection>>>,
    task: JoinHandle<()>,
}

impl Drop for PinProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PinProxy {
    pub async fn start(guard: Arc<Guard>) -> std::io::Result<PinProxy> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let log: Arc<Mutex<Vec<PinnedConnection>>> = Arc::default();
        let l = log.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((conn, _)) = listener.accept().await else {
                    break;
                };
                let (guard, log) = (guard.clone(), l.clone());
                tokio::spawn(async move {
                    let _ = serve(conn, guard, log).await;
                });
            }
        });
        Ok(PinProxy { addr, log, task })
    }
}

async fn refuse(conn: &mut TcpStream, code: &str, why: &str) {
    let body = format!("blocked by rightkit-browser network policy: {why}");
    let _ = conn
        .write_all(
            format!(
                "HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
}

async fn serve(
    mut conn: TcpStream,
    guard: Arc<Guard>,
    log: Arc<Mutex<Vec<PinnedConnection>>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let rest = buf[head_end..].to_vec();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("HTTP/1.1"),
    );

    let is_connect = method.eq_ignore_ascii_case("CONNECT");
    let (host, port, forward_head) = if is_connect {
        let (h, p) = target.rsplit_once(':').unwrap_or((target, "443"));
        (h.to_string(), p.parse::<u16>().unwrap_or(443), None)
    } else {
        let Ok(u) = url::Url::parse(target) else {
            refuse(
                &mut conn,
                "400 Bad Request",
                "proxy requests must use absolute URLs",
            )
            .await;
            return Ok(());
        };
        let Some(h) = u.host_str() else {
            refuse(&mut conn, "400 Bad Request", "no host").await;
            return Ok(());
        };
        let path = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_string(),
        };
        let mut out = format!("{method} {path} {version}\r\n");
        for l in lines.filter(|l| !l.is_empty()) {
            let name = l.split(':').next().unwrap_or("").to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "connection" | "proxy-connection" | "keep-alive" | "proxy-authorization"
            ) {
                continue;
            }
            out.push_str(l);
            out.push_str("\r\n");
        }
        out.push_str("Connection: close\r\n\r\n");
        (
            h.to_string(),
            u.port_or_known_default().unwrap_or(80),
            Some(out),
        )
    };

    let ips = match guard.network.pin(&host).await {
        Ok(ips) => ips,
        Err(reason) => {
            guard.emit(BrowserEvent::Denied {
                session_id: guard.session_id.clone(),
                what: "Connect".into(),
                reason: reason.clone(),
            });
            refuse(&mut conn, "403 Forbidden", &reason).await;
            return Ok(());
        }
    };
    let mut upstream = None;
    for ip in ips {
        if let Ok(Ok(s)) = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(SocketAddr::new(ip, port)),
        )
        .await
        {
            log.lock().unwrap().push(PinnedConnection {
                host: host.clone(),
                port,
                ip,
            });
            upstream = Some(s);
            break;
        }
        log.lock().unwrap().push(PinnedConnection {
            host: host.clone(),
            port,
            ip,
        });
    }
    let Some(mut upstream) = upstream else {
        refuse(&mut conn, "502 Bad Gateway", "connect failed").await;
        return Ok(());
    };
    match forward_head {
        None => {
            conn.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            upstream.write_all(&rest).await?;
        }
        Some(h) => {
            upstream.write_all(h.as_bytes()).await?;
            upstream.write_all(&rest).await?;
        }
    }
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
    Ok(())
}
