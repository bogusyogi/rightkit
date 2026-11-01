//! FFmpeg decode transport: the reference [`TextureProvider`].
//!
//! Decodes exact source frames by piping `rawvideo/rgba` out of the bundle's
//! FFmpeg (the same binary the export path uses), rasterizes titles through the
//! same `drawtext` chains the export graph builds, and keeps a byte-bounded
//! LRU of decoded pixels. Requests carry optional deadlines; `cancel_all`
//! invalidates in-flight and queued work so a stale request can only fail,
//! never serve.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::process::ChildStdout;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use rightkit_compositor::gpu::{wgpu, GpuContext};
use rightkit_ffmpeg::FfmpegCommand;
use rightkit_timeline::{Fps, SourceId, Ticks};
use rightkit_video_project::worker::{spawn_ffmpeg, OwnedChild, StdioMode, StdioPolicy};
use thiserror::Error;

use crate::native::executor::texture_from_rgba;
use crate::native::mapper::{ComposeError, TextureRequirement};
use crate::native::transport::{decode_size, TextureProvider};

/// Registered source media bound to a verified file path.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub source: SourceId,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Per-source colour chain the export applies before composition (SDR conversion,
    /// correction, shot match, grade, LUT), prepended to the decode filter so the preview
    /// shows the same colour. `None` or empty decodes the source as is.
    pub pre_filter: Option<String>,
    /// Identity of `pre_filter` and of any file it reads; part of every pixel cache key, so a
    /// changed grade never serves a frame decoded under the old one.
    pub pre_signature: Option<String>,
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("decode cancelled (stale generation)")]
    Cancelled,
    #[error("decode deadline exceeded")]
    Deadline,
    #[error("cannot run ffmpeg at {0}: {1}")]
    Spawn(PathBuf, String),
    #[error("ffmpeg exited with {status}: {stderr}")]
    Failed { status: String, stderr: String },
    #[error("frame payload was {got} bytes, expected {want}")]
    BadFrame { got: usize, want: usize },
    #[error("no registered source for {0}")]
    MissingSource(String),
    #[error("no rasterizer registered for overlay {0}")]
    MissingTitle(String),
    /// A source crop that is empty, non-finite, or outside the source frame.
    #[error("invalid source crop for {0}: {1}")]
    InvalidCrop(String, String),
}

impl From<DecodeError> for ComposeError {
    fn from(e: DecodeError) -> Self {
        ComposeError::Transport(e.to_string())
    }
}

/// Byte-bounded LRU of decoded RGBA pixels keyed by texture id.
pub struct PixelLru {
    map: HashMap<String, (u64, Vec<u8>)>,
    clock: u64,
    bytes: usize,
    budget: usize,
}

impl PixelLru {
    pub fn new(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            clock: 0,
            bytes: 0,
            budget,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn get(&mut self, key: &str) -> Option<&Vec<u8>> {
        self.clock += 1;
        if let Some(entry) = self.map.get_mut(key) {
            entry.0 = self.clock;
            return Some(&entry.1);
        }
        None
    }

    pub fn insert(&mut self, key: String, pixels: Vec<u8>) {
        if let Some(old) = self.map.remove(&key) {
            self.bytes -= old.1.len();
        }
        let bytes = pixels.len();
        if bytes > self.budget {
            return;
        }
        self.clock += 1;
        self.map.insert(key, (self.clock, pixels));
        self.bytes += bytes;
        while self.bytes > self.budget {
            let Some(evict) = self
                .map
                .iter()
                .min_by_key(|(_, (age, _))| *age)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((_, px)) = self.map.remove(&evict) {
                self.bytes -= px.len();
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }
}

/// Pixel window `(x, y, width, height)` a normalized crop selects on a source
/// frame of `width` x `height`. Edges are snapped to even pixels so the window
/// is exact under 4:2:0 chroma subsampling (FFmpeg would otherwise move it),
/// and clamped to the frame.
fn crop_window_px(
    id: &SourceId,
    width: u32,
    height: u32,
    crop: crate::Rect,
) -> Result<(u32, u32, u32, u32), DecodeError> {
    let bad = |why: &str| DecodeError::InvalidCrop(id.to_string(), why.to_string());
    let vals = [crop.x, crop.y, crop.width, crop.height];
    if vals.iter().any(|v| !v.is_finite()) {
        return Err(bad("non-finite value"));
    }
    if !(0.0..=1.0).contains(&crop.x)
        || !(0.0..=1.0).contains(&crop.y)
        || crop.width <= 0.0
        || crop.height <= 0.0
        || crop.x + crop.width > 1.0 + f64::EPSILON
        || crop.y + crop.height > 1.0 + f64::EPSILON
    {
        return Err(bad("window outside the source frame"));
    }
    let (w, h) = (width as f64, height as f64);
    let x = ((crop.x * w).floor() as u32) & !1;
    let y = ((crop.y * h).floor() as u32) & !1;
    let cw = ((crop.width * w).round() as u32).max(2) & !1;
    let ch = ((crop.height * h).round() as u32).max(2) & !1;
    let cw = cw.min(width.saturating_sub(x) & !1);
    let ch = ch.min(height.saturating_sub(y) & !1);
    if cw < 2 || ch < 2 {
        return Err(bad("window smaller than two pixels"));
    }
    Ok((x, y, cw, ch))
}

/// Ticks -> `ss.ssssss` seconds for ffmpeg args.
fn secs(t: Ticks) -> String {
    let micros = t.to_units(1_000_000);
    format!("{}.{:06}", micros / 1_000_000, micros.rem_euclid(1_000_000))
}

/// Reads a child to EOF on stdout, honoring a deadline and the decoder's
/// cancel generation. Kills the child on either.
fn read_frames(
    child: &mut OwnedChild,
    deadline: Option<Instant>,
    generation: &AtomicU64,
    expected_gen: u64,
) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    let mut stdout = child.take_stdout().expect("stdout piped");
    loop {
        let mut buf = [0u8; 1 << 16];
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = child.terminate_tree();
                return Err(DecodeError::Failed {
                    status: "io".into(),
                    stderr: e.to_string(),
                });
            }
        }
        if generation.load(Ordering::SeqCst) != expected_gen {
            let _ = child.terminate_tree();
            return Err(DecodeError::Cancelled);
        }
        if deadline.is_some_and(|d| Instant::now() > d) {
            let _ = child.terminate_tree();
            return Err(DecodeError::Deadline);
        }
    }
    let status = child.wait().map_err(|e| DecodeError::Failed {
        status: "wait".into(),
        stderr: e.to_string(),
    })?;
    if !status.success() {
        return Err(DecodeError::Failed {
            status: status.to_string(),
            stderr: "ffmpeg decode failed".into(),
        });
    }
    Ok(out)
}

/// One persistent decode pipe: ffmpeg streams scaled RGBA frames from a
/// `-ss` start, resampled onto the timeline frame grid by an `fps` filter so
/// consecutive output-frame requests consume consecutive piped frames.
/// `next_pts` is the source-time the next emitted frame answers.
struct SourceStream {
    child: OwnedChild,
    stdout: ChildStdout,
    frame_bytes: usize,
    next_pts: Ticks,
    frame_ticks: Ticks,
    /// Decode generation at open; a bump poisons this pipe.
    gen: u64,
    /// Stream hit EOF; further reads would hang, so the next request respawns.
    done: bool,
}

impl Drop for SourceStream {
    fn drop(&mut self) {
        // Kill the whole tree first; the call also reaps the direct child.
        let _ = self.child.terminate_tree();
    }
}

/// How far ahead of the stream head a request may sit before respawning is
/// cheaper than read-and-discard.
const MAX_DROP_FRAMES: i64 = 6;

/// What the decode loop does next for one request.
enum StreamAction {
    /// Open a fresh pipe at the request time.
    Respawn,
    /// Read-and-discard `n` frames to reach the request.
    Drop(i64),
    /// The next emitted frame answers the request.
    Read,
}

/// Decoded pixels for one [`TextureRequirement`] on the prefetch bus.
pub struct BusPixels {
    pub px: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

struct BusInner {
    map: HashMap<String, std::sync::Arc<BusPixels>>,
    order: VecDeque<String>,
    bytes: usize,
    budget: usize,
}

/// Cross-thread bus of decoded texture pixels keyed by texture id. One decode
/// worker produces ahead of the playhead; render workers `wait` for entries
/// instead of spawning duplicate decodes. Byte-bounded, oldest-first eviction
/// — frames behind the playhead age out naturally.
pub struct DecodeBus {
    inner: Mutex<BusInner>,
    cv: Condvar,
}

impl DecodeBus {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(BusInner {
                map: HashMap::new(),
                order: VecDeque::new(),
                bytes: 0,
                budget: budget_bytes,
            }),
            cv: Condvar::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<std::sync::Arc<BusPixels>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .get(key)
            .cloned()
    }

    /// Waits for `key` until `deadline`; returns `None` on timeout.
    pub fn wait(&self, key: &str, deadline: Instant) -> Option<std::sync::Arc<BusPixels>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(f) = inner.map.get(key) {
                return Some(std::sync::Arc::clone(f));
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self
                .cv
                .wait_timeout(inner, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            inner = guard;
        }
    }

    pub fn insert(&self, key: String, pixels: BusPixels) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = inner.map.remove(&key) {
            inner.bytes -= old.px.len();
            inner.order.retain(|k| k != &key);
        }
        let bytes = pixels.px.len();
        if bytes > inner.budget {
            return;
        }
        inner.bytes += bytes;
        inner.order.push_back(key.clone());
        inner.map.insert(key, std::sync::Arc::new(pixels));
        while inner.bytes > inner.budget {
            let Some(evict) = inner.order.pop_front() else {
                break;
            };
            if let Some(old) = inner.map.remove(&evict) {
                inner.bytes -= old.px.len();
            }
        }
        self.cv.notify_all();
    }
}

/// Reference decoder/transport: FFmpeg-piped source frames plus prerendered
/// title drawtext chains, behind a byte-bounded LRU.
pub struct FfmpegDecoder {
    ffmpeg: PathBuf,
    sources: HashMap<SourceId, SourceSpec>,
    /// overlay id -> drawtext filter chain (text files already on disk).
    titles: HashMap<String, Vec<String>>,
    cache: PixelLru,
    generation: std::sync::Arc<AtomicU64>,
    /// Timeline fps: source streams resample onto this frame grid.
    fps: Fps,
    /// Live per-source decode pipes, restarted on discontinuity.
    streams: HashMap<SourceId, SourceStream>,
    /// ffmpeg processes spawned by this decoder (instrumentation hook).
    spawns: std::sync::Arc<AtomicU64>,
    /// Shared prefetch bus: consulted before decoding so render workers reuse
    /// the decode worker's output instead of spawning duplicates.
    bus: Option<std::sync::Arc<DecodeBus>>,
}

impl FfmpegDecoder {
    /// `cache_budget` bounds decoded pixel bytes; 256 MiB covers several
    /// full-HD frames plus margin.
    pub fn new(
        ffmpeg: PathBuf,
        sources: Vec<SourceSpec>,
        cache_budget: usize,
        fps: Fps,
        spawns: std::sync::Arc<AtomicU64>,
        bus: Option<std::sync::Arc<DecodeBus>>,
    ) -> Self {
        Self {
            ffmpeg,
            sources: sources.into_iter().map(|s| (s.source.clone(), s)).collect(),
            titles: HashMap::new(),
            cache: PixelLru::new(cache_budget),
            generation: std::sync::Arc::new(AtomicU64::new(0)),
            fps,
            streams: HashMap::new(),
            spawns,
            bus,
        }
    }

    /// Key of a requirement's pixels in the decoder cache and on the prefetch bus: its texture id,
    /// plus the colour signature of the source for source frames. Use it for every bus access.
    pub fn pixel_key(&self, requirement: &TextureRequirement) -> String {
        let id = requirement.texture_id();
        let signature = match requirement {
            TextureRequirement::SourceFrame { source, .. } => self
                .sources
                .get(source)
                .and_then(|s| s.pre_signature.as_deref()),
            TextureRequirement::RasterizedTitle { .. } => None,
        };
        match signature {
            Some(sig) => format!("{id}#{sig}"),
            None => id.to_string(),
        }
    }

    /// Registers the drawtext chain for one overlay (export-identical filters,
    /// produced by `rightkit_timeline::title::title_filters`; the caller writes
    /// the returned text files first).
    pub fn register_title(&mut self, overlay_id: impl Into<String>, filters: Vec<String>) {
        self.titles.insert(overlay_id.into(), filters);
    }

    /// Stale-request cancellation: every in-flight or subsequent decode of the
    /// old generation fails fast, live source streams are killed, and the
    /// pixel cache is dropped.
    pub fn cancel_all(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cache.clear();
        self.streams.clear();
    }

    pub fn cache_stats(&self) -> (usize, usize) {
        (self.cache.len(), self.cache.bytes())
    }

    /// Shared counter of ffmpeg processes spawned by this decoder; handed to
    /// the session so tests and diagnostics can count decode spawns.
    pub fn spawn_counter(&self) -> std::sync::Arc<AtomicU64> {
        std::sync::Arc::clone(&self.spawns)
    }

    fn run_ffmpeg(
        &self,
        args: &[String],
        deadline: Option<Instant>,
    ) -> Result<Vec<u8>, DecodeError> {
        if deadline.is_some_and(|d| Instant::now() > d) {
            return Err(DecodeError::Deadline);
        }
        let gen = self.generation.load(Ordering::SeqCst);
        let mut typed = FfmpegCommand::new();
        typed.texts(["-v", "error"]).texts(args.iter().cloned());
        self.spawns.fetch_add(1, Ordering::Relaxed);
        let mut child = spawn_ffmpeg(
            &self.ffmpeg,
            &typed,
            StdioPolicy {
                stdin: StdioMode::Null,
                stdout: StdioMode::Piped,
                stderr: StdioMode::Piped,
            },
        )
        .map_err(|e| DecodeError::Spawn(self.ffmpeg.clone(), e.to_string()))?;
        read_frames(&mut child, deadline, &self.generation, gen)
    }

    /// Opens a streaming decode of `spec` starting at `from`, resampled onto
    /// the timeline frame grid so each emitted frame answers one request.
    fn open_stream(
        &self,
        spec: &SourceSpec,
        from: Ticks,
        size: (u32, u32),
    ) -> Result<SourceStream, DecodeError> {
        let (w, h) = size;
        // The colour chain runs first, on the source's own frames, as in the export graph.
        let mut vf = String::new();
        if let Some(pre) = spec.pre_filter.as_deref().filter(|c| !c.is_empty()) {
            vf.push_str(pre);
            vf.push(',');
        }
        vf.push_str(&format!("fps={}", self.fps.ffmpeg_rational()));
        // Same-size output skips the scale filter: it is pure cost when the
        // decode grid already matches the source.
        if (w, h) != (spec.width, spec.height) {
            vf.push_str(&format!(",scale={w}:{h}:flags=bilinear"));
        }
        let mut typed = FfmpegCommand::new();
        typed
            .texts(["-v", "error", "-ss"])
            .text(secs(from))
            .input(&spec.path)
            .text("-vf")
            .text(&vf)
            .texts(["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        self.spawns.fetch_add(1, Ordering::Relaxed);
        let mut child = spawn_ffmpeg(
            &self.ffmpeg,
            &typed,
            StdioPolicy {
                stdin: StdioMode::Null,
                stdout: StdioMode::Piped,
                stderr: StdioMode::Null,
            },
        )
        .map_err(|e| DecodeError::Spawn(self.ffmpeg.clone(), e.to_string()))?;
        let stdout = child.take_stdout().expect("stdout piped");
        Ok(SourceStream {
            child,
            stdout,
            frame_bytes: (w * h * 4) as usize,
            next_pts: from,
            frame_ticks: self.fps.frame_to_ticks(1),
            gen: self.generation.load(Ordering::SeqCst),
            done: false,
        })
    }

    /// Reads the next frame `stream` emits; `Ok(false)` means the source
    /// ended (stream marked `done`). Cancellation and deadline are checked
    /// between pipe reads so stale work fails instead of serving.
    fn stream_read(
        generation: &AtomicU64,
        stream: &mut SourceStream,
        buf: &mut Vec<u8>,
        deadline: Option<Instant>,
    ) -> Result<bool, DecodeError> {
        if generation.load(Ordering::SeqCst) != stream.gen {
            return Err(DecodeError::Cancelled);
        }
        if deadline.is_some_and(|d| Instant::now() > d) {
            stream.done = true;
            return Err(DecodeError::Deadline);
        }
        buf.clear();
        if buf.capacity() < stream.frame_bytes {
            buf.reserve(stream.frame_bytes);
        }
        // `take` bounds the fill to exactly one frame; `read_to_end` writes
        // into uninit spare capacity, so no 8 MB memset per frame.
        let want = stream.frame_bytes;
        match stream.stdout.by_ref().take(want as u64).read_to_end(buf) {
            Ok(n) if n == want => Ok(true),
            Ok(_) => {
                // EOF mid-frame: the source ended.
                stream.done = true;
                Ok(false)
            }
            Err(e) => {
                stream.done = true;
                Err(DecodeError::Failed {
                    status: "io".into(),
                    stderr: e.to_string(),
                })
            }
        }
    }

    fn decode_source_frame(
        &mut self,
        id: &SourceId,
        source_time: Ticks,
        quality_scale: f32,
        deadline: Option<Instant>,
    ) -> Result<(Vec<u8>, u32, u32), DecodeError> {
        let spec = self
            .sources
            .get(id)
            .ok_or_else(|| DecodeError::MissingSource(id.to_string()))?
            .clone();
        let (w, h) = decode_size(spec.width, spec.height, quality_scale);
        let want = (w * h * 4) as usize;
        // The live stream serves `source_time` directly when it is the next
        // grid point; a small forward gap is read-and-discard; anything else
        // (seek, clip cut, backward jump, dead pipe) respawns at the request.
        let mut respawns = 0;
        loop {
            let mut action = StreamAction::Read;
            if let Some(stream) = self.streams.get(id) {
                if stream.done
                    || stream.gen != self.generation.load(Ordering::SeqCst)
                    || stream.frame_bytes != want
                {
                    action = StreamAction::Respawn;
                } else {
                    let gap = self
                        .fps
                        .ticks_to_frame(Ticks(source_time.0 - stream.next_pts.0));
                    if !(0..=MAX_DROP_FRAMES).contains(&gap) {
                        action = StreamAction::Respawn;
                    } else if gap > 0 {
                        action = StreamAction::Drop(gap);
                    }
                }
            } else {
                action = StreamAction::Respawn;
            }
            match action {
                StreamAction::Respawn => {
                    respawns += 1;
                    if respawns > 2 {
                        return Err(DecodeError::BadFrame { got: 0, want });
                    }
                    self.streams
                        .insert(id.clone(), self.open_stream(&spec, source_time, (w, h))?);
                }
                StreamAction::Drop(gap) => {
                    let stream = self.streams.get_mut(id).expect("stream checked");
                    let mut scratch = Vec::new();
                    for _ in 0..gap {
                        if !Self::stream_read(&self.generation, stream, &mut scratch, deadline)? {
                            break;
                        }
                        stream.next_pts.0 += stream.frame_ticks.0;
                    }
                }
                StreamAction::Read => {
                    let stream = self.streams.get_mut(id).expect("stream checked");
                    let mut raw = Vec::new();
                    if !Self::stream_read(&self.generation, stream, &mut raw, deadline)? {
                        // Source ended before answering: respawn once at the
                        // exact request (edge of media), then report honestly.
                        continue;
                    }
                    stream.next_pts.0 += stream.frame_ticks.0;
                    if raw.len() != want {
                        return Err(DecodeError::BadFrame {
                            got: raw.len(),
                            want,
                        });
                    }
                    return Ok((raw, w, h));
                }
            }
        }
    }

    /// Decodes one frame of a cropped source window: `crop` is applied by
    /// FFmpeg before scaling, so the returned texture is the window itself at
    /// decode quality. Each cropped request is a one-shot decode (the persistent
    /// streams are full-frame), keyed by the crop in the texture id.
    fn decode_cropped_frame(
        &self,
        id: &SourceId,
        source_time: Ticks,
        quality_scale: f32,
        crop: crate::Rect,
        deadline: Option<Instant>,
    ) -> Result<(Vec<u8>, u32, u32), DecodeError> {
        let spec = self
            .sources
            .get(id)
            .ok_or_else(|| DecodeError::MissingSource(id.to_string()))?;
        let (x, y, cw, ch) = crop_window_px(id, spec.width, spec.height, crop)?;
        let (w, h) = decode_size(cw, ch, quality_scale);
        let want = (w * h * 4) as usize;
        let mut vf = String::new();
        if let Some(pre) = spec.pre_filter.as_deref().filter(|c| !c.is_empty()) {
            vf.push_str(pre);
            vf.push(',');
        }
        vf.push_str(&format!("crop={cw}:{ch}:{x}:{y}"));
        if (w, h) != (cw, ch) {
            vf.push_str(&format!(",scale={w}:{h}:flags=bilinear"));
        }
        let mut typed = FfmpegCommand::new();
        typed
            .texts(["-v", "error", "-ss"])
            .text(secs(source_time))
            .input(&spec.path)
            .text("-vf")
            .text(&vf)
            .texts(["-frames:v", "1"])
            .texts(["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        if deadline.is_some_and(|d| Instant::now() > d) {
            return Err(DecodeError::Deadline);
        }
        let gen = self.generation.load(Ordering::SeqCst);
        self.spawns.fetch_add(1, Ordering::Relaxed);
        let mut child = spawn_ffmpeg(
            &self.ffmpeg,
            &typed,
            StdioPolicy {
                stdin: StdioMode::Null,
                stdout: StdioMode::Piped,
                stderr: StdioMode::Piped,
            },
        )
        .map_err(|e| DecodeError::Spawn(self.ffmpeg.clone(), e.to_string()))?;
        let raw = read_frames(&mut child, deadline, &self.generation, gen)?;
        if raw.len() != want {
            return Err(DecodeError::BadFrame {
                got: raw.len(),
                want,
            });
        }
        Ok((raw, w, h))
    }

    /// Texture dimensions a source requirement decodes to, cropped or not.
    fn source_dims(
        &self,
        source: &SourceId,
        quality_scale: f32,
        crop: crate::Rect,
    ) -> Result<(u32, u32), ComposeError> {
        let spec = self
            .sources
            .get(source)
            .ok_or_else(|| ComposeError::Transport(format!("no source {source}")))?;
        if crop == crate::Rect::FULL {
            return Ok(decode_size(spec.width, spec.height, quality_scale));
        }
        let (_, _, cw, ch) = crop_window_px(source, spec.width, spec.height, crop)
            .map_err(|e| ComposeError::Transport(e.to_string()))?;
        Ok(decode_size(cw, ch, quality_scale))
    }

    fn rasterize_title(
        &mut self,
        overlay_id: &str,
        output_size: (u32, u32),
        deadline: Option<Instant>,
    ) -> Result<(Vec<u8>, u32, u32), DecodeError> {
        let filters = self
            .titles
            .get(overlay_id)
            .ok_or_else(|| DecodeError::MissingTitle(overlay_id.to_string()))?;
        let (w, h) = output_size;
        let mut args = vec![
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("color=c=black@0.0:s={w}x{h},format=rgba"),
        ];
        if !filters.is_empty() {
            args.push("-vf".into());
            args.push(filters.join(","));
        }
        args.extend([
            "-frames:v".into(),
            "1".into(),
            "-f".into(),
            "rawvideo".into(),
            "-pix_fmt".into(),
            "rgba".into(),
            "-".into(),
        ]);
        let raw = self.run_ffmpeg(&args, deadline)?;
        let want = (w * h * 4) as usize;
        if raw.len() != want {
            return Err(DecodeError::BadFrame {
                got: raw.len(),
                want,
            });
        }
        Ok((raw, w, h))
    }

    /// Produces raw RGBA pixels for one requirement — source frames come off
    /// the persistent per-source stream, titles rasterize via the one-shot
    /// path. Used by the prefetch bus, which caches pixels itself.
    pub fn produce_pixels(
        &mut self,
        requirement: &TextureRequirement,
        output_size: (u32, u32),
        deadline: Option<Instant>,
    ) -> Result<(Vec<u8>, u32, u32), DecodeError> {
        match requirement {
            TextureRequirement::SourceFrame {
                source,
                source_time,
                quality_scale,
                crop,
                ..
            } => {
                if *crop == crate::Rect::FULL {
                    self.decode_source_frame(source, *source_time, *quality_scale, deadline)
                } else {
                    self.decode_cropped_frame(source, *source_time, *quality_scale, *crop, deadline)
                }
            }
            TextureRequirement::RasterizedTitle { overlay_id, .. } => {
                self.rasterize_title(overlay_id, output_size, deadline)
            }
        }
    }

    /// Like [`TextureProvider::provide`] but with an optional deadline;
    /// exceeded deadlines kill the ffmpeg child.
    pub fn provide_deadline(
        &mut self,
        context: &GpuContext,
        requirement: &TextureRequirement,
        output_size: (u32, u32),
        deadline: Option<Instant>,
    ) -> Result<wgpu::Texture, ComposeError> {
        let key = self.pixel_key(requirement);
        let cached_dims = match requirement {
            TextureRequirement::SourceFrame {
                source,
                quality_scale,
                crop,
                ..
            } => self.source_dims(source, *quality_scale, *crop)?,
            TextureRequirement::RasterizedTitle { .. } => output_size,
        };
        if let Some(px) = self.cache.get(&key) {
            let (w, h) = cached_dims;
            return Ok(texture_from_rgba(context, px, w, h, "cached-frame"));
        }
        // Prefetch bus: a render worker waits briefly for the decode worker's
        // output rather than respawning a duplicate decode itself.
        if let Some(bus) = &self.bus {
            let wait_until =
                deadline.unwrap_or_else(|| Instant::now() + Duration::from_millis(250));
            if let Some(bp) = bus.wait(&key, wait_until) {
                return Ok(texture_from_rgba(context, &bp.px, bp.w, bp.h, "bus-frame"));
            }
        }
        let (px, w, h) = self.produce_pixels(requirement, output_size, deadline)?;
        self.cache.insert(key, px.clone());
        Ok(texture_from_rgba(context, &px, w, h, "decoded-frame"))
    }
}

impl TextureProvider for FfmpegDecoder {
    fn provide(
        &mut self,
        context: &GpuContext,
        requirement: &TextureRequirement,
        output_size: (u32, u32),
    ) -> Result<wgpu::Texture, ComposeError> {
        self.provide_deadline(context, requirement, output_size, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a 64x64 lossless clip: black left half, white right half.
    /// Returns `None` (after printing SKIP) when no ffmpeg is on PATH.
    fn halves_clip(name: &str) -> Option<PathBuf> {
        let path = std::env::temp_dir().join(format!("rightkit-crop-{name}.mkv"));
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:r=30:d=1,drawbox=x=32:y=0:w=32:h=64:color=white:t=fill",
                "-frames:v",
                "1",
                "-c:v",
                "ffv1",
            ])
            .arg(&path)
            .status();
        match status {
            Ok(s) if s.success() => Some(path),
            _ => {
                println!("SKIP: ffmpeg unavailable; crop pixel test not run");
                None
            }
        }
    }

    /// `RIGHTKIT_FFMPEG` when it names an existing file.
    fn editing_ffmpeg() -> Option<PathBuf> {
        std::env::var_os("RIGHTKIT_FFMPEG")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    }

    fn mean_red(px: &[u8]) -> f64 {
        let n = px.len() / 4;
        px.as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[0] as f64)
            .sum::<f64>()
            / n as f64
    }

    #[test]
    fn cropped_decode_returns_the_selected_window_pixels() {
        let Some(path) = halves_clip("halves") else {
            return;
        };
        // The decoder verifies its binary through RightKit; a bare PATH name is not a file.
        let Some(ffmpeg) = editing_ffmpeg() else {
            println!("SKIP: staged editing ffmpeg unavailable; crop pixel test not run");
            return;
        };
        let source = SourceId::new("a".repeat(64)).unwrap();
        let mut dec = FfmpegDecoder::new(
            ffmpeg,
            vec![SourceSpec {
                source: source.clone(),
                path,
                width: 64,
                height: 64,
                pre_filter: None,
                pre_signature: None,
            }],
            64 << 20,
            Fps::new(30, 1).unwrap(),
            std::sync::Arc::new(AtomicU64::new(0)),
            None,
        );
        let request = |crop: crate::Rect| TextureRequirement::SourceFrame {
            texture_id: "t".into(),
            input: 0,
            source: source.clone(),
            source_time: Ticks(0),
            quality_scale: 1.0,
            crop,
        };
        let right = crate::Rect {
            x: 0.5,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        };
        let left = crate::Rect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        };

        let (px, w, h) = dec
            .produce_pixels(&request(right), (64, 64), None)
            .expect("right half decodes");
        assert_eq!((w, h), (32, 64));
        assert_eq!(px.len(), (w * h * 4) as usize);
        assert!(mean_red(&px) > 240.0, "right window must be white");

        let (px, w, h) = dec
            .produce_pixels(&request(left), (64, 64), None)
            .expect("left half decodes");
        assert_eq!((w, h), (32, 64));
        assert!(mean_red(&px) < 15.0, "left window must be black");

        let bad = crate::Rect {
            x: 0.9,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        };
        assert!(matches!(
            dec.produce_pixels(&request(bad), (64, 64), None),
            Err(DecodeError::InvalidCrop(_, _))
        ));
    }
}
