# GenRight asks for rightkit-http and rightkit-tts (2026-10-11)

Source: GenRight chat, derived from GenRight call sites by a read-only lane (file:line cited against genright/; not re-verified by GenRight). Owner: RightKit lead. Resolves gap ledger #4 (http) and #5 (tts).

## rightkit-http (additive, 0.2.6)

Defaults that change GenRight behaviour if adopted as-is:
- `max_redirects` 0 (client.rs:86): GenRight follows redirects (HF resolve, CDN); GenRight sets `ClientConfig { max_redirects: 10 }`.
- `retry_unsafe_statuses` true (client.rs:106): would re-send paid POSTs on 429/5xx. NEED `Request::retry_unsafe_statuses(self, bool) -> Self`, and document whether `.retry(RetryPolicy::none())` fully disables it.
- `max_response_bytes` config-only, 32 MiB; GenRight reads outputs up to 1 GiB. NEED `Request::max_response_bytes(self, usize) -> Self`.
- `Client::stream` ignores `Request::timeout` (client.rs:321): fix.
- `Request::header` appends (client.rs:127). NEED `Request::set_header(self, name, value) -> Self` (case-insensitive replace).
- NEED `Request::put(url)` and `Request::delete(url)` constructors (Method::Put/Delete exist).

(a) Multipart (framing: CRLF, Content-Disposition on every part, Content-Type only on file parts; names/filenames sanitised of quote/CR/LF; pinned boundary must be supported for byte-exact tests):
```rust
pub struct Multipart;
impl Multipart {
  pub fn new() -> Self;                                   // random boundary
  pub fn with_boundary(b: impl Into<String>) -> Self;     // pinned
  pub fn text(self, name: &str, value: &str) -> Self;
  pub fn file_bytes(self, name: &str, filename: &str, content_type: &str, bytes: impl Into<Vec<u8>>) -> Self;
  pub fn file_path(self, name: &str, path: &Path, content_type: &str) -> Result<Self, HttpError>;
  pub fn content_type(&self) -> String;
  pub fn into_body(self) -> Result<Vec<u8>, HttpError>;   // error if boundary occurs in a part
}
impl Request { pub fn multipart(self, form: Multipart) -> Result<Self, HttpError>; }
```

(b) Range/resume download to a path:
```rust
#[derive(Default)] pub struct DownloadOptions<'a> {
  pub headers: Vec<(String,String)>, pub expected_sha256: Option<String>, pub expected_bytes: Option<u64>,
  pub resume: bool, pub cancel: Option<&'a AtomicBool>,
  pub on_progress: Option<&'a mut dyn FnMut(&DownloadProgress)>, pub max_reconnects: u32 }
pub struct DownloadProgress { pub downloaded: u64, pub total: Option<u64>, pub resumed_from: u64 }
pub struct DownloadReport { pub path: PathBuf, pub bytes: u64, pub sha256: String, pub resumed_from: u64, pub attempts: u32 }
pub enum DownloadError { Http(HttpError), Io(String), Cancelled{partial_bytes:u64}, Integrity{expected:String,actual:String}, LengthMismatch{expected:u64,actual:u64} }
impl Client { pub fn download_to_path(&self, url: &str, dest: &Path, opts: DownloadOptions<'_>) -> Result<DownloadReport, DownloadError>; }
```
Semantics: `Range: bytes=<len>-` with `If-Range` on the first ETag; 206 at len appends, 200 truncates and restarts, 416 with len==total verifies only; mid-body failures reconnect from current length up to max_reconnects; cancel checked per read and keeps `.part`; integrity/length mismatch deletes `.part`; success fsyncs then renames `.part` to dest.

## rightkit-tts / rightkit-qts (0.2.0 wire)

(a) On `protocol.rs Request::Synth` after `voice_prompt` (old workers ignore unknown keys; None => Base):
```rust
#[serde(default)] pub mode: Option<Mode>,
#[serde(default)] pub instruct: Option<String>,
#[serde(default)] pub speaker: Option<String>,
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Mode { #[default] Base, CustomVoice, VoiceDesign }
```
Reply unchanged. rightkit-qts: `PresetSpeaker::parse(&str) -> Option<Self>`; `SynthOptions::validate(&self) -> Result<(), QtsError>`.
BLOCKING: `rightkit-tts-worker` exits 64 on unknown args (bin/rightkit-tts-worker.rs:10-13); it must accept `--mode` and load per mode.

(b) Interior dead-air cap: `pub max_interior_silence_ms: Option<usize>` on `SynthOptions` (default None = unchanged; GenRight sets Some(450)). Mono PCM16 at 24 kHz; silence is |sample| <= 327 (same floor as trim_edge_silence); every interior silent run longer than max is shortened to exactly max, keeping the first max/2 and last max - max/2 samples; applied per chunk right after trim_edge_silence (client.rs:464), before the join pause.

(c) Per-mode engine loading with a separate vocoder path: `Qwen3TtsEngine::from_model_dir_for_mode_with_vocoder(dir, vocoder, mode)`; worker error prefix `model_missing: <mode>`; a 1.7B mode manifest.

(d) Data root: rightkit-tts `try_data_root()` uses RIGHTKIT_DATA_ROOT, then GENRIGHT_DATA_ROOT, then `$HOME/Orthic Labs/RightKit`; GenRight uses `dirs::data_dir()/app.orthiclabs.genright` or GENRIGHT_APP_DATA. NEED a constructor/config taking the data root from the caller (resolve through rightkit_service::Suite where possible).

Outcome if delivered: about 870 of genright-qts's 1,183 lines delete; GenRight keeps onnxruntime init and thin model-dir wrappers.
