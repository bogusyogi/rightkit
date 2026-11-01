use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const HEAD_BYTES: usize = 32 * 1024;
pub const TAIL_BYTES: usize = 96 * 1024;
pub const RETAINED_BYTES: usize = HEAD_BYTES + TAIL_BYTES;
const ROTATE_BYTES: usize = 8 * 1024 * 1024;
static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

/// One bounded preview buffer: first 32 KiB & last 96 KiB of raw output.
#[derive(Debug, Default)]
pub struct BoundedOutput {
    head: Vec<u8>,
    tail: Vec<u8>,
    total_bytes: u64,
}

impl BoundedOutput {
    pub fn push(&mut self, chunk: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len() as u64);
        let head_remaining = HEAD_BYTES.saturating_sub(self.head.len());
        self.head
            .extend_from_slice(&chunk[..chunk.len().min(head_remaining)]);
        self.tail.extend_from_slice(chunk);
        if self.tail.len() > TAIL_BYTES {
            let excess = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..excess);
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn truncated(&self) -> bool {
        self.total_bytes as usize > self.head.len() + self.tail.len()
    }

    pub fn preview_bytes(&self) -> Vec<u8> {
        if !self.truncated() {
            // Contiguous raw stream: `head` covers [0, head.len()) and `tail`
            // covers [total - tail.len(), total). They may overlap (e.g. a
            // 40 KiB stream is wholly inside `tail`), so never concatenate
            // blindly or the overlap is duplicated and incremental cursors
            // read the wrong offsets.
            let total = self.total_bytes as usize;
            let tail_start = total.saturating_sub(self.tail.len());
            let mut full = Vec::with_capacity(total);
            full.extend_from_slice(&self.head[..self.head.len().min(tail_start)]);
            full.extend_from_slice(&self.tail);
            return full;
        }
        let mut preview = self.head.clone();
        preview.extend_from_slice(b"\n\n[output truncated; tail follows]\n\n");
        preview.extend_from_slice(&self.tail);
        preview
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolRef {
    pub id: String,
    pub size_bytes: u64,
    pub segments: Vec<PathBuf>,
    pub truncated_preview: bool,
}

struct Segment {
    temp: PathBuf,
    file: File,
    bytes: usize,
}

/// Raw-byte spooler. It writes before any UTF-8 expansion, incrementally hashes
/// every input byte, syncs siblings, then content-addresses published segments.
pub struct OutputSpool {
    root: PathBuf,
    hasher: rightkit_fs::digest::Sha256Hasher,
    bytes: u64,
    segments: Vec<Segment>,
}

impl OutputSpool {
    pub fn create(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let mut spool = Self {
            root,
            hasher: rightkit_fs::digest::Sha256Hasher::new(),
            bytes: 0,
            segments: Vec::new(),
        };
        spool.rotate()?;
        Ok(spool)
    }

    pub fn push(&mut self, mut chunk: &[u8]) -> io::Result<()> {
        self.hasher.update(chunk);
        self.bytes = self.bytes.saturating_add(chunk.len() as u64);
        while !chunk.is_empty() {
            let segment = self
                .segments
                .last_mut()
                .expect("spool always has a segment");
            let room = ROTATE_BYTES.saturating_sub(segment.bytes).max(1);
            let take = room.min(chunk.len());
            segment.file.write_all(&chunk[..take])?;
            segment.bytes += take;
            chunk = &chunk[take..];
            if segment.bytes == ROTATE_BYTES && !chunk.is_empty() {
                self.rotate()?;
            }
        }
        Ok(())
    }

    pub fn finish(mut self, truncated_preview: bool) -> io::Result<SpoolRef> {
        let id = std::mem::take(&mut self.hasher).finalize();
        let mut segments = Vec::with_capacity(self.segments.len());
        for (index, mut segment) in self.segments.drain(..).enumerate() {
            segment.file.flush()?;
            segment.file.sync_all()?;
            drop(segment.file);
            let published = self.root.join(format!("{id}.{index}"));
            // Content-addressed, exclusive publish (durable: syncs the file and
            // the directory). An existing identical segment wins.
            match rightkit_fs::publish_file_with(
                &segment.temp,
                &published,
                rightkit_fs::AtomicWriteOptions {
                    replace: rightkit_fs::ReplaceMode::NoReplace,
                    ..rightkit_fs::AtomicOptions::default().into()
                },
            ) {
                Ok(()) => {}
                Err(rightkit_fs::FsError::AlreadyExists(_)) => fs::remove_file(&segment.temp)?,
                Err(error) => return Err(error.into()),
            }
            segments.push(published);
        }
        Ok(SpoolRef {
            id,
            size_bytes: self.bytes,
            segments,
            truncated_preview,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        let index = self.segments.len();
        let (temp, file) = loop {
            let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
            let temp = self.root.join(format!(
                ".output-spool-{}-{index}-{nonce}.tmp",
                std::process::id()
            ));
            match File::options().write(true).create_new(true).open(&temp) {
                Ok(file) => break (temp, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        self.segments.push(Segment {
            temp,
            file,
            bytes: 0,
        });
        Ok(())
    }
}

impl Drop for OutputSpool {
    fn drop(&mut self) {
        for segment in &self.segments {
            let _ = fs::remove_file(&segment.temp);
        }
    }
}

pub fn spool_path(root: &Path, reference: &SpoolRef) -> PathBuf {
    root.join(&reference.id)
}
