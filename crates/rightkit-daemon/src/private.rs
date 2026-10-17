//! Private per-run child service: a secret passed over stdin, checked in constant time on every
//! frame, and a child that exits when stdin closes.
//!
//! The pattern, for an app that runs its own engine as a child it alone may talk to:
//!
//! 1. The parent makes a per-run [`Suite`](crate::Suite) ([`Suite::per_run_temp`](crate::Suite::per_run_temp))
//!    and a [`PrivateSecret`], starts the child with [`PrivateChild::spawn`] (stdin piped, the
//!    secret written to it as the first line, never in an argument or an environment variable),
//!    and tells the child the suite root by an argument.
//! 2. The child reads the secret with [`PrivateSecret::read_from_stdin`], hosts its service with
//!    [`ServiceConfig::private_secret`](crate::ServiceConfig::private_secret), and calls
//!    [`exit_when_stdin_closes`] (or [`watch_stdin_eof`]) so it ends when the parent does, however
//!    the parent ended (a closed pipe is the one signal that survives a crash or `SIGKILL`).
//! 3. The parent connects with [`ClientConfig::secret`](crate::ClientConfig::secret). The `hello`
//!    frame and every `request` frame then carry the secret as a top-level `secret` field; the
//!    host refuses a frame without it (or with a wrong one) before a handler runs, answers
//!    `unauthenticated`, and closes the connection.
//!
//! The secret is in addition to the transport's own same-user check, not a replacement: it keeps
//! another process of the same user, which can open the socket, from being served by a child it
//! did not start. It is 256 bits from the OS random source, compared in constant time over a
//! fixed-length buffer, redacted in `Debug` and wiped from memory on drop (best effort: the
//! text copies made to build a frame are not wiped).

use rightkit_child_process::{OwnedChild, OwnedCommand};
use std::io::{self, BufRead, Read, Write};
use std::process::Stdio;
use subtle::ConstantTimeEq;

/// Length of the secret in bytes before hex encoding.
pub const SECRET_BYTES: usize = 32;
/// Length of the secret as sent: lowercase hex.
pub const SECRET_HEX_LEN: usize = SECRET_BYTES * 2;

/// Why a secret could not be produced or read.
#[derive(Debug)]
pub enum SecretError {
    /// The OS random source failed.
    Random,
    /// The line was not exactly [`SECRET_HEX_LEN`] lowercase hex digits.
    Malformed,
    /// The stream ended before a secret line arrived.
    Missing,
    /// Reading failed.
    Io(io::Error),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Random => f.write_str("the OS random source failed"),
            SecretError::Malformed => {
                write!(
                    f,
                    "the secret must be {SECRET_HEX_LEN} lowercase hex digits"
                )
            }
            SecretError::Missing => f.write_str("stdin closed before a secret arrived"),
            SecretError::Io(e) => write!(f, "reading the secret: {e}"),
        }
    }
}
impl std::error::Error for SecretError {}

/// A per-run shared secret. Never printed; compare with [`verify`](Self::verify) only.
#[derive(Clone)]
pub struct PrivateSecret {
    /// ASCII lowercase hex, always [`SECRET_HEX_LEN`] bytes.
    hex: [u8; SECRET_HEX_LEN],
}

impl std::fmt::Debug for PrivateSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateSecret(<redacted>)")
    }
}

impl Drop for PrivateSecret {
    fn drop(&mut self) {
        for byte in self.hex.iter_mut() {
            // SAFETY: a valid, aligned, exclusive reference to one byte.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

fn hex_digit(nibble: u8) -> u8 {
    b"0123456789abcdef"[usize::from(nibble & 0x0f)]
}

impl PrivateSecret {
    /// A fresh secret from the OS random source.
    pub fn generate() -> Result<Self, SecretError> {
        let mut raw = [0u8; SECRET_BYTES];
        getrandom::fill(&mut raw).map_err(|_| SecretError::Random)?;
        let mut hex = [0u8; SECRET_HEX_LEN];
        for (index, byte) in raw.iter().enumerate() {
            hex[index * 2] = hex_digit(byte >> 4);
            hex[index * 2 + 1] = hex_digit(*byte);
        }
        for byte in raw.iter_mut() {
            // SAFETY: as in `Drop`.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        Ok(Self { hex })
    }

    /// Parse the text form (exactly [`SECRET_HEX_LEN`] lowercase hex digits; no surrounding
    /// whitespace).
    pub fn from_hex(text: &str) -> Result<Self, SecretError> {
        let bytes = text.as_bytes();
        if bytes.len() != SECRET_HEX_LEN
            || !bytes
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            return Err(SecretError::Malformed);
        }
        let mut hex = [0u8; SECRET_HEX_LEN];
        hex.copy_from_slice(bytes);
        Ok(Self { hex })
    }

    /// The text form, for putting in a frame or on a pipe. Do not log it.
    pub fn expose(&self) -> &str {
        // Only ASCII hex digits are ever stored.
        std::str::from_utf8(&self.hex).unwrap_or("")
    }

    /// Constant-time check of a presented secret. The comparison always runs over the full
    /// fixed-length buffer, and a presented value of the wrong length fails without an early
    /// return, so neither content nor length-prefix match leaks through timing.
    pub fn verify(&self, presented: &str) -> bool {
        let presented = presented.as_bytes();
        let mut padded = [0u8; SECRET_HEX_LEN];
        let take = presented.len().min(SECRET_HEX_LEN);
        padded[..take].copy_from_slice(&presented[..take]);
        let same = self.hex[..].ct_eq(&padded[..]);
        let length_ok = (presented.len() as u64).ct_eq(&(SECRET_HEX_LEN as u64));
        bool::from(same & length_ok)
    }

    /// Read the secret from the first line of `reader`, at most [`SECRET_HEX_LEN`] plus a line
    /// ending long. For the child's stdin; the generic form exists for tests.
    pub fn read_from(reader: &mut impl BufRead) -> Result<Self, SecretError> {
        let mut line = String::new();
        let read = (&mut *reader)
            .take((SECRET_HEX_LEN + 2) as u64)
            .read_line(&mut line)
            .map_err(SecretError::Io)?;
        if read == 0 {
            return Err(SecretError::Missing);
        }
        let result = Self::from_hex(line.trim_end_matches(['\n', '\r']));
        // Best effort: do not leave the secret text in the temporary line.
        // SAFETY: overwriting every byte of the string with ASCII zero keeps it valid UTF-8.
        unsafe {
            for byte in line.as_bytes_mut() {
                std::ptr::write_volatile(byte, b'0');
            }
        }
        result
    }

    /// Read the secret from this process's stdin. Call it once, first, before anything else
    /// reads stdin; later reads (see [`watch_stdin_eof`]) continue after the secret line.
    pub fn read_from_stdin() -> Result<Self, SecretError> {
        Self::read_from(&mut io::stdin().lock())
    }
}

/// Call `on_eof` on a background thread when `reader` reaches end of file or fails. Anything the
/// parent writes after the secret is read and dropped.
pub fn watch_reader_eof<R, F>(mut reader: R, on_eof: F) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
    F: FnOnce() + Send + 'static,
{
    std::thread::spawn(move || {
        let mut sink = [0u8; 256];
        loop {
            match reader.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        on_eof();
    })
}

/// Call `on_eof` when this process's stdin closes: the parent exited or let go of the pipe.
/// Call after [`PrivateSecret::read_from_stdin`].
pub fn watch_stdin_eof<F>(on_eof: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    watch_reader_eof(io::stdin(), on_eof)
}

/// End this process with `code` when stdin closes. The child's whole shutdown policy, for a
/// service with nothing to flush; a service that must flush uses [`watch_stdin_eof`] and its own
/// exit path.
pub fn exit_when_stdin_closes(code: i32) -> std::thread::JoinHandle<()> {
    watch_stdin_eof(move || std::process::exit(code))
}

/// A child started with a [`PrivateSecret`] on its stdin. Dropping it closes the pipe (the
/// child's cue to exit) and then the owned child is terminated by its own `Drop`.
#[derive(Debug)]
pub struct PrivateChild {
    stdin: Option<std::process::ChildStdin>,
    secret: PrivateSecret,
    child: OwnedChild,
}

impl PrivateChild {
    /// Start `command` with a fresh secret. Stdin is replaced with a pipe; the secret is
    /// written as the first line and the pipe stays open for the child's lifetime. Put nothing
    /// secret in the arguments or environment.
    pub fn spawn(command: OwnedCommand) -> io::Result<Self> {
        let secret = PrivateSecret::generate().map_err(io::Error::other)?;
        Self::spawn_with(command, secret)
    }

    /// [`spawn`](Self::spawn) with a secret the caller made.
    pub fn spawn_with(command: OwnedCommand, secret: PrivateSecret) -> io::Result<Self> {
        Self::spawn_with_extra(command, secret, None)
    }

    /// [`spawn_with`](Self::spawn_with) for a child whose handshake is more than the secret: the
    /// secret line, then `extra_line` (for example a path or a second key), both written before
    /// this returns. `extra_line` must not contain a newline; a line that does is refused with
    /// `InvalidInput` before the child starts.
    pub fn spawn_with_extra(
        mut command: OwnedCommand,
        secret: PrivateSecret,
        extra_line: Option<&str>,
    ) -> io::Result<Self> {
        if let Some(line) = extra_line {
            check_line(line)?;
        }
        command.command_mut().stdin(Stdio::piped());
        let mut child = command.spawn()?;
        let Some(mut stdin) = child.take_stdin() else {
            let _ = child.terminate_tree();
            return Err(io::Error::other("the child's stdin was not piped"));
        };
        let mut written = stdin
            .write_all(secret.expose().as_bytes())
            .and_then(|()| stdin.write_all(b"\n"));
        if let Some(line) = extra_line {
            if written.is_ok() {
                written = stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.write_all(b"\n"));
            }
        }
        let written = written.and_then(|()| stdin.flush());
        if let Err(error) = written {
            let _ = child.terminate_tree();
            return Err(error);
        }
        Ok(Self {
            stdin: Some(stdin),
            secret,
            child,
        })
    }

    /// Write one more line to the child's stdin after the handshake and flush it. Fails with
    /// `BrokenPipe` once the stdin is closed. `line` must not contain a newline.
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        check_line(line)?;
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the child's stdin is closed",
            ));
        };
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// The secret to give a [`ClientConfig`](crate::ClientConfig).
    pub fn secret(&self) -> &PrivateSecret {
        &self.secret
    }

    /// The owned child, for waiting, termination and the rest of its lifecycle.
    pub fn child(&self) -> &OwnedChild {
        &self.child
    }

    /// Mutable access to the owned child.
    pub fn child_mut(&mut self) -> &mut OwnedChild {
        &mut self.child
    }

    /// Close the child's stdin: the polite request to exit. Idempotent.
    pub fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }
}

/// A line sent on stdin must hold no newline: a newline would split one handshake line in two.
fn check_line(line: &str) -> io::Result<()> {
    if line.contains('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a stdin line cannot contain a newline",
        ));
    }
    Ok(())
}

impl Drop for PrivateChild {
    fn drop(&mut self) {
        // Field order would drop the pipe first anyway; do it explicitly so the child hears
        // the polite signal before the owned child's terminate-on-drop.
        self.close_stdin();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_hex_and_distinct() {
        let a = PrivateSecret::generate().unwrap();
        let b = PrivateSecret::generate().unwrap();
        assert_eq!(a.expose().len(), SECRET_HEX_LEN);
        assert!(a.expose().bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.expose(), b.expose());
    }

    #[test]
    fn verify_accepts_only_the_exact_secret() {
        let secret = PrivateSecret::generate().unwrap();
        assert!(secret.verify(secret.expose()));
        assert!(!secret.verify(""));
        assert!(!secret.verify("x"));
        let mut wrong = secret.expose().to_string();
        wrong.replace_range(0..1, if wrong.starts_with('0') { "1" } else { "0" });
        assert!(!secret.verify(&wrong));
        // A truncation and an extension of the right secret both fail.
        assert!(!secret.verify(&secret.expose()[..SECRET_HEX_LEN - 1]));
        assert!(!secret.verify(&format!("{}0", secret.expose())));
        // Zero padding must not match a secret of zeros shorter than the buffer.
        assert!(!secret.verify(&"0".repeat(SECRET_HEX_LEN - 1)));
    }

    #[test]
    fn debug_never_shows_the_secret() {
        let secret = PrivateSecret::generate().unwrap();
        assert!(!format!("{secret:?}").contains(secret.expose()));
    }

    #[test]
    fn from_hex_rejects_malformed_text() {
        assert!(PrivateSecret::from_hex("").is_err());
        assert!(PrivateSecret::from_hex(&"A".repeat(SECRET_HEX_LEN)).is_err());
        assert!(PrivateSecret::from_hex(&"g".repeat(SECRET_HEX_LEN)).is_err());
        assert!(PrivateSecret::from_hex(&"a".repeat(SECRET_HEX_LEN)).is_ok());
    }

    #[test]
    fn read_from_takes_the_first_line_only() {
        let secret = PrivateSecret::generate().unwrap();
        let input = format!("{}\nrest\n", secret.expose()).into_bytes();
        let mut reader = io::Cursor::new(input);
        let read = PrivateSecret::read_from(&mut reader).unwrap();
        assert!(secret.verify(read.expose()));
    }

    #[test]
    fn read_from_reports_missing_and_oversized_input() {
        let mut empty = io::Cursor::new(Vec::<u8>::new());
        assert!(matches!(
            PrivateSecret::read_from(&mut empty),
            Err(SecretError::Missing)
        ));
        let mut long = io::Cursor::new("a".repeat(10_000).into_bytes());
        assert!(matches!(
            PrivateSecret::read_from(&mut long),
            Err(SecretError::Malformed)
        ));
    }

    #[test]
    fn eof_watcher_fires_when_the_reader_ends() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = watch_reader_eof(io::Cursor::new(b"ignored bytes".to_vec()), move || {
            tx.send(()).unwrap();
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        handle.join().unwrap();
    }
}
