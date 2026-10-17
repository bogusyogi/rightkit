//! Sending: `prepare-upload` to a peer, then each accepted file as an upload.

use super::error::{Error, peer_io};
use super::net::{self, Wire};
use super::proto::{
    self, DeviceInfo, FileMeta, PrepareUploadRequest, PrepareUploadResponse, PulseExtra,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Where to send: a device found on the network.
#[derive(Clone, Debug)]
pub struct Peer {
    pub ip: IpAddr,
    pub port: u16,
    pub https: bool,
    pub fingerprint: String,
    pub alias: String,
}

/// What the user chose to send.
#[derive(Clone, Debug)]
pub enum SendItem {
    Path(PathBuf),
    Text(String),
}

#[derive(Clone, Debug)]
enum Source {
    File(PathBuf),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    /// Name as the receiver sees it; folders keep their structure ("Dir/a.txt").
    pub name: String,
    pub size: u64,
    pub mime: String,
    source: Source,
}

impl Entry {
    /// Opens the entry's content for reading: the file from disk, or the message text.
    ///
    /// Each call returns a fresh reader positioned at the start. A file that cannot be opened
    /// (moved, deleted, permission) returns the I/O error; nothing is cached, so a changed file is
    /// read as it is now.
    pub fn open_reader(&self) -> io::Result<Box<dyn Read + Send>> {
        match &self.source {
            Source::Text(text) => Ok(Box::new(io::Cursor::new(text.clone().into_bytes()))),
            Source::File(path) => Ok(Box::new(File::open(path)?)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Waiting for the other device to accept.
    Waiting,
    Sending,
    /// A message (text only) is on the other device's screen. No longer
    /// reported early: a message waits for the other side's answer like a file.
    Delivered,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    pub files_done: usize,
    pub files_total: usize,
    pub current: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    Declined,
    Cancelled,
}

const MAX_ENTRIES: usize = 20_000;

/// The files behind the user's choice. Folders are walked (symbolic links
/// inside them are skipped); a chosen path that is a link is followed.
pub fn expand(items: &[SendItem]) -> Result<Vec<Entry>, Error> {
    let mut entries: Vec<Entry> = Vec::new();
    for item in items {
        match item {
            SendItem::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                entries.push(Entry {
                    id: format!("f{}", entries.len()),
                    name: format!("{}.txt", proto::random_hex(4)),
                    size: text.len() as u64,
                    mime: "text/plain".to_string(),
                    source: Source::Text(text.clone()),
                });
            }
            SendItem::Path(path) => {
                let metadata = std::fs::metadata(path).map_err(|e| Error::Unreadable {
                    path: path.display().to_string(),
                    detail: e.to_string(),
                })?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_string());
                if metadata.is_dir() {
                    walk(path, &name, &mut entries)?;
                } else if metadata.is_file() {
                    push_file(&mut entries, path, name, metadata.len());
                } else {
                    return Err(Error::NotFileOrFolder {
                        path: path.display().to_string(),
                    });
                }
            }
        }
        if entries.len() > MAX_ENTRIES {
            return Err(Error::TooManyFiles { max: MAX_ENTRIES });
        }
    }
    if entries.is_empty() {
        return Err(Error::NothingToSend);
    }
    Ok(entries)
}

fn push_file(entries: &mut Vec<Entry>, path: &Path, name: String, size: u64) {
    let mime = proto::mime_for(&name).to_string();
    entries.push(Entry {
        id: format!("f{}", entries.len()),
        name,
        size,
        mime,
        source: Source::File(path.to_path_buf()),
    });
}

fn walk(directory: &Path, prefix: &str, entries: &mut Vec<Entry>) -> Result<(), Error> {
    let mut stack = vec![(directory.to_path_buf(), prefix.to_string())];
    while let Some((dir, relative)) = stack.pop() {
        let listing = std::fs::read_dir(&dir).map_err(|e| Error::Unreadable {
            path: dir.display().to_string(),
            detail: e.to_string(),
        })?;
        for child in listing.flatten() {
            let Ok(kind) = child.file_type() else {
                continue;
            };
            let name = format!("{relative}/{}", child.file_name().to_string_lossy());
            if kind.is_symlink() {
                continue;
            } else if kind.is_dir() {
                stack.push((child.path(), name));
            } else if kind.is_file() {
                let size = child.metadata().map(|m| m.len()).unwrap_or(0);
                push_file(entries, &child.path(), name, size);
            }
            if entries.len() > MAX_ENTRIES {
                return Err(Error::TooManyFiles { max: MAX_ENTRIES });
            }
        }
    }
    Ok(())
}

fn exchange(
    peer: &Peer,
    target: &str,
    json: Option<&[u8]>,
    timeout: Duration,
    register: &dyn Fn(Option<TcpStream>),
    sent: &mut dyn FnMut(),
) -> Result<net::Reply, Error> {
    let mut wire = net::connect(peer.ip, peer.port, peer.https, &peer.fingerprint, timeout)
        .map_err(|e| {
            peer_io(&peer.alias, &e, |peer, detail| Error::PeerUnreachable {
                peer,
                detail,
            })
        })?;
    register(wire.tcp().try_clone().ok());
    let payload = json.unwrap_or(&[]);
    let host = format!("{}:{}", peer.ip, peer.port);
    let result = net::call(
        &mut wire,
        "POST",
        &host,
        target,
        json.map(|_| "application/json"),
        payload.len() as u64,
        &mut |w| {
            w.write_all(payload)?;
            w.flush()?;
            sent();
            Ok(())
        },
    );
    wire.finish();
    register(None);
    result.map_err(|e| {
        peer_io(&peer.alias, &e, |peer, detail| Error::PeerLost {
            peer,
            detail,
        })
    })
}

fn query(session: &str, file: &str, token: &str) -> String {
    format!(
        "{}/upload?sessionId={}&fileId={}&token={}",
        proto::API,
        proto::url_encode(session),
        proto::url_encode(file),
        proto::url_encode(token)
    )
}

/// Ask the peer to accept `entries`, and send them once it does. Blocks.
/// `register` is told each connection's socket so another thread can break a
/// wait by shutting it down; `on` hears progress after every chunk.
pub fn deliver(
    me: &DeviceInfo,
    peer: &Peer,
    entries: &[Entry],
    clipboard: bool,
    cancel: &AtomicBool,
    register: &dyn Fn(Option<TcpStream>),
    on: &mut dyn FnMut(Progress),
) -> Result<Outcome, Error> {
    // Sends go only over a verified TLS connection to a pinned identity.
    if !peer.https {
        return Err(Error::PeerPlaintext {
            peer: peer.alias.clone(),
        });
    }
    if !net::valid_pin(&peer.fingerprint) {
        return Err(Error::InvalidIdentity {
            peer: peer.alias.clone(),
        });
    }
    let mut progress = Progress {
        phase: Phase::Waiting,
        done: 0,
        total: entries.iter().map(|e| e.size).sum(),
        files_done: 0,
        files_total: entries.len(),
        current: None,
    };
    on(progress.clone());

    let mut files = BTreeMap::new();
    for entry in entries {
        files.insert(
            entry.id.clone(),
            FileMeta {
                id: entry.id.clone(),
                file_name: entry.name.clone(),
                size: entry.size,
                file_type: entry.mime.clone(),
                sha256: None,
                preview: match &entry.source {
                    Source::Text(text) => Some(text.clone()),
                    Source::File(_) => None,
                },
                metadata: None,
            },
        );
    }
    let mut info = me.clone();
    info.announce = None;
    let pulse = clipboard.then_some(PulseExtra { clipboard: true });
    let body = serde_json::to_vec(&PrepareUploadRequest { info, files, pulse }).map_err(|e| {
        Error::Internal {
            detail: e.to_string(),
        }
    })?;
    let prepare = format!("{}/prepare-upload", proto::API);
    // The other person may take a while to say yes. A message counts as sent
    // only once they have accepted it (or allow this device), like a file.
    let reply = exchange(
        peer,
        &prepare,
        Some(&body),
        Duration::from_secs(190),
        register,
        &mut || {},
    );
    if cancel.load(Ordering::Relaxed) {
        return Ok(Outcome::Cancelled);
    }
    let reply = reply?;
    let session = match reply.status {
        200 => serde_json::from_slice::<PrepareUploadResponse>(&reply.body).map_err(|_| {
            Error::PeerReply {
                peer: peer.alias.clone(),
            }
        })?,
        204 => return Ok(Outcome::Done),
        403 => return Ok(Outcome::Declined),
        401 => {
            return Err(Error::PeerNeedsPin {
                peer: peer.alias.clone(),
            });
        }
        409 => {
            return Err(Error::PeerBusy {
                peer: peer.alias.clone(),
            });
        }
        413 => {
            return Err(Error::PeerTooLarge {
                peer: peer.alias.clone(),
            });
        }
        429 => {
            return Err(Error::PeerRateLimited {
                peer: peer.alias.clone(),
            });
        }
        other => {
            return Err(Error::PeerAnswer {
                peer: peer.alias.clone(),
                status: other,
            });
        }
    };
    // A partial accept sends only the files it wants.
    progress.files_total = entries
        .iter()
        .filter(|e| session.files.contains_key(&e.id))
        .count();
    progress.total = entries
        .iter()
        .filter(|e| session.files.contains_key(&e.id))
        .map(|e| e.size)
        .sum();
    progress.phase = Phase::Sending;
    on(progress.clone());

    for entry in entries {
        let Some(token) = session.files.get(&entry.id) else {
            continue;
        };
        if cancel.load(Ordering::Relaxed) {
            send_cancel(peer, &session.session_id);
            return Ok(Outcome::Cancelled);
        }
        progress.current = Some(entry.name.clone());
        on(progress.clone());
        let target = query(&session.session_id, &entry.id, token);
        let result = upload_one(peer, &target, entry, cancel, register, &mut progress, on);
        match result {
            Ok(()) => {
                progress.files_done += 1;
                on(progress.clone());
            }
            Err(error) => {
                if cancel.load(Ordering::Relaxed) {
                    send_cancel(peer, &session.session_id);
                    return Ok(Outcome::Cancelled);
                }
                return Err(error);
            }
        }
    }
    Ok(Outcome::Done)
}

fn upload_one(
    peer: &Peer,
    target: &str,
    entry: &Entry,
    cancel: &AtomicBool,
    register: &dyn Fn(Option<TcpStream>),
    progress: &mut Progress,
    on: &mut dyn FnMut(Progress),
) -> Result<(), Error> {
    let mut wire = net::connect(
        peer.ip,
        peer.port,
        peer.https,
        &peer.fingerprint,
        Duration::from_secs(120),
    )
    .map_err(|e| {
        peer_io(&peer.alias, &e, |peer, detail| Error::PeerUnreachable {
            peer,
            detail,
        })
    })?;
    register(wire.tcp().try_clone().ok());
    let host = format!("{}:{}", peer.ip, peer.port);
    let result = net::call(
        &mut wire,
        "POST",
        &host,
        target,
        Some("application/octet-stream"),
        entry.size,
        &mut |w: &mut Wire| match &entry.source {
            Source::Text(text) => {
                w.write_all(text.as_bytes())?;
                progress.done += text.len() as u64;
                on(progress.clone());
                Ok(())
            }
            Source::File(path) => {
                let mut file = File::open(path)?;
                let mut buffer = vec![0u8; 64 * 1024];
                let mut remaining = entry.size;
                while remaining > 0 {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                    let want = remaining.min(buffer.len() as u64) as usize;
                    let n = file.read(&mut buffer[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "the file got shorter",
                        ));
                    }
                    w.write_all(&buffer[..n])?;
                    remaining -= n as u64;
                    progress.done += n as u64;
                    on(progress.clone());
                }
                Ok(())
            }
        },
    );
    wire.finish();
    register(None);
    let reply = result.map_err(|e| {
        peer_io(&peer.alias, &e, |_, detail| Error::FileStopped {
            file: entry.name.clone(),
            detail,
        })
    })?;
    match reply.status {
        200 | 204 => Ok(()),
        403 => Err(Error::FileRefused {
            peer: peer.alias.clone(),
            file: entry.name.clone(),
        }),
        422 => Err(Error::FileDamaged {
            peer: peer.alias.clone(),
            file: entry.name.clone(),
        }),
        other => Err(Error::PeerAnswer {
            peer: peer.alias.clone(),
            status: other,
        }),
    }
}

/// Tell the peer the session is over. Best effort.
fn send_cancel(peer: &Peer, session: &str) {
    let target = format!(
        "{}/cancel?sessionId={}",
        proto::API,
        proto::url_encode(session)
    );
    let _ = net::request_json(
        peer.ip,
        peer.port,
        peer.https,
        &peer.fingerprint,
        "POST",
        &target,
        None,
        Duration::from_secs(5),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> DeviceInfo {
        DeviceInfo {
            alias: "Me".into(),
            version: "2.0".into(),
            device_model: None,
            device_type: None,
            fingerprint: "f".repeat(64),
            port: 53317,
            protocol: "https".into(),
            download: false,
            announce: None,
        }
    }

    #[test]
    fn nothing_is_sent_over_plain_http() {
        let peer = Peer {
            ip: "127.0.0.1".parse().unwrap(),
            port: 9,
            https: false,
            fingerprint: "a".repeat(64),
            alias: "Plain".into(),
        };
        let entries = expand(&[SendItem::Text("hi".into())]).unwrap();
        let cancel = AtomicBool::new(false);
        let result = deliver(&me(), &peer, &entries, false, &cancel, &|_| {}, &mut |_| {});
        assert_eq!(
            result,
            Err(Error::PeerPlaintext {
                peer: "Plain".into()
            })
        );
    }

    #[test]
    fn a_peer_without_a_valid_fingerprint_is_refused_before_connecting() {
        let peer = Peer {
            ip: "127.0.0.1".parse().unwrap(),
            port: 9,
            https: true,
            fingerprint: "not-hex".into(),
            alias: "Odd".into(),
        };
        let entries = expand(&[SendItem::Text("hi".into())]).unwrap();
        let cancel = AtomicBool::new(false);
        let result = deliver(&me(), &peer, &entries, false, &cancel, &|_| {}, &mut |_| {});
        assert_eq!(result, Err(Error::InvalidIdentity { peer: "Odd".into() }));
    }

    #[test]
    fn empty_selections_and_missing_paths_are_typed() {
        assert!(matches!(expand(&[]), Err(Error::NothingToSend)));
        assert!(matches!(
            expand(&[SendItem::Path("/definitely/not/here/rightkit".into())]),
            Err(Error::Unreadable { .. })
        ));
    }

    #[test]
    fn open_reader_reads_text_and_files_and_reports_missing_files() {
        let entries = expand(&[SendItem::Text("hello".into())]).unwrap();
        let mut body = String::new();
        entries[0]
            .open_reader()
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "hello");

        let dir = std::env::temp_dir().join(format!("rk-send-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, b"file body").unwrap();
        let entries = expand(&[SendItem::Path(path.clone())]).unwrap();
        let mut bytes = Vec::new();
        entries[0]
            .open_reader()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"file body");

        std::fs::remove_file(&path).unwrap();
        assert!(entries[0].open_reader().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
