//! An in-process proxy for tests: records `CONNECT host:port` lines, relays the tunnel, and
//! answers plain absolute-form requests (`GET http://host/path`) with a fixed body.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct TestProxy {
    port: u16,
    connects: Arc<Mutex<Vec<String>>>,
    forwards: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestProxy {
    pub fn start() -> Self {
        Self::start_to(None)
    }

    /// A proxy that tunnels every `CONNECT`, whatever its target, to `backend`. The CONNECT
    /// target is still recorded as sent, so a test sees whether it was a name or an address.
    pub fn start_to(backend: Option<std::net::SocketAddr>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let port = listener.local_addr().unwrap().port();
        let connects = Arc::new(Mutex::new(Vec::new()));
        let forwards = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (c, f, s) = (connects.clone(), forwards.clone(), stop.clone());
        let thread = thread::spawn(move || {
            for conn in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = conn else { continue };
                let (c, f) = (c.clone(), f.clone());
                thread::spawn(move || {
                    let _ = handle(stream, &c, &f, backend);
                });
            }
        });
        Self {
            port,
            connects,
            forwards,
            stop,
            thread: Some(thread),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Targets of `CONNECT` requests, as sent (`host:port`).
    pub fn connects(&self) -> Vec<String> {
        self.connects.lock().unwrap().clone()
    }

    /// Request lines of plain (absolute-form) requests.
    pub fn forwards(&self) -> Vec<String> {
        self.forwards.lock().unwrap().clone()
    }

    pub fn total(&self) -> usize {
        self.connects().len() + self.forwards().len()
    }
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 {
            break;
        }
        head.push(byte[0]);
        if head.len() > 16 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn handle(
    mut client: TcpStream,
    connects: &Mutex<Vec<String>>,
    forwards: &Mutex<Vec<String>>,
    backend: Option<std::net::SocketAddr>,
) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(10)))?;
    let head = read_head(&mut client)?;
    let first = head.lines().next().unwrap_or("").to_string();
    if let Some(rest) = first.strip_prefix("CONNECT ") {
        let target = rest.split_whitespace().next().unwrap_or("").to_string();
        connects.lock().unwrap().push(target.clone());
        let connected = match backend {
            Some(addr) => TcpStream::connect(addr),
            None => TcpStream::connect(&target),
        };
        let Ok(upstream) = connected else {
            client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")?;
            return Ok(());
        };
        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
        let mut up_read = upstream.try_clone()?;
        let mut client_write = client.try_clone()?;
        let down = thread::spawn(move || {
            let _ = std::io::copy(&mut up_read, &mut client_write);
            let _ = client_write.shutdown(std::net::Shutdown::Both);
        });
        let mut up_write = upstream;
        let _ = std::io::copy(&mut client, &mut up_write);
        let _ = up_write.shutdown(std::net::Shutdown::Both);
        let _ = down.join();
    } else {
        forwards.lock().unwrap().push(first);
        client.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nvia-proxy",
        )?;
    }
    Ok(())
}

/// A raw TCP/HTTP server: the handler gets the connection, the request head as text and the
/// zero-based connection index, and writes whatever bytes it likes.
pub struct RawServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RawServer {
    pub fn start(handler: impl Fn(&mut TcpStream, &str, usize) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind raw server");
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);
        let s = stop.clone();
        let thread = thread::spawn(move || {
            let mut index = 0usize;
            for conn in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = conn else { continue };
                let (h, i) = (handler.clone(), index);
                index += 1;
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    h(&mut stream, &String::from_utf8_lossy(&head), i);
                });
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
