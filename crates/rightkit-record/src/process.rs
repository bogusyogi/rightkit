//! The out-of-process helper seam over the [`protocol`](crate::protocol).
//!
//! [`ProcessBackend`] is the core's [`Backend`] for a helper process: it sends
//! requests on one stream and reads responses and events from the other.
//! [`serve_backend`] is the helper's side, serving any [`Backend`] over the same
//! two streams. Spawning, killing and restarting helpers belong to
//! rightkit-child-process. This module speaks the protocol over the streams it is given.
//!
//! A helper that dies, closes its stream without `bye`, or sends a frame that
//! breaks the protocol is reported as a [`HelperEvent::Failed`]. The recorder then
//! ends the take through its normal failure path, so the take is quarantined or
//! invalid, never complete.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use rightkit_contracts::types::record_session::{CaptureBorder, RecorderOs};
use rightkit_sidecar::length_prefixed::{read_frame_bytes, write_frame};
use rightkit_sidecar::FrameError;
use serde_json::{json, Value};

use crate::backend::{
    Backend, BackendPlan, Failure, Finished, HelperEvent, HelperIdentity, Permission, Source,
    StartInfo,
};
use crate::error::{Code, RecordError, Result};
use crate::protocol::{
    bye_frame, decode_error, decode_event, decode_finished, decode_identity, decode_sources,
    decode_start_info, encode_error, encode_finished, encode_plan, encode_sources,
    encode_start_info, event_frame, identity_result, Identity, MAX_FRAME_BYTES,
};

/// How often a serving helper drains its backend's events while no request arrives.
const SERVE_TICK: Duration = Duration::from_millis(10);

/// A [`Backend`] whose capture runs in a helper process, reached over its stdio.
///
/// `permission()` returns the value read at connect time. It does not re-probe
/// the helper, because the trait method cannot fail and cannot wait on the wire.
pub struct ProcessBackend {
    link: Mutex<Link>,
    events: Receiver<HelperEvent>,
    identity: Identity,
    /// The border the helper reported when the take started.
    border: Option<CaptureBorder>,
}

/// The request side of the connection: the writer and the response queue.
struct Link {
    writer: Box<dyn Write + Send>,
    responses: Receiver<Value>,
    next_id: u64,
}

impl Link {
    /// Sends one request and waits for its response. A helper that is gone, or
    /// whose stream cannot be written, is `Code::Helper`.
    fn call(&mut self, op: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let mut frame = json!({ "kind": "request", "id": id, "op": op });
        if let (Value::Object(target), Value::Object(extra)) = (&mut frame, params) {
            target.extend(extra);
        }
        write_frame(&mut self.writer, &frame, MAX_FRAME_BYTES)
            .map_err(|e| helper_error(format!("sending '{op}' failed: {e}")))?;
        loop {
            let response = self
                .responses
                .recv()
                .map_err(|_| helper_error(format!("the helper ended before answering '{op}'")))?;
            if response["id"].as_u64() != Some(id) {
                continue;
            }
            return if response["ok"] == true {
                Ok(response["result"].clone())
            } else {
                Err(decode_error(&response["error"]))
            };
        }
    }
}

impl ProcessBackend {
    /// Connects to a helper: `reader` carries its frames to the core, `writer`
    /// carries requests to it. Reads the helper's identity before returning.
    pub fn connect<R, W>(reader: R, writer: W) -> Result<Self>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let (response_tx, responses) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        thread::spawn(move || read_loop(reader, response_tx, event_tx));
        let mut link = Link {
            writer: Box::new(writer),
            responses,
            next_id: 0,
        };
        let hello = link.call("hello", json!({}))?;
        let identity = decode_identity(&hello).map_err(protocol_error)?;
        Ok(ProcessBackend {
            link: Mutex::new(link),
            events,
            identity,
            border: None,
        })
    }

    /// One request at a time. The link is locked for the whole round trip.
    fn call(&self, op: &str, params: Value) -> Result<Value> {
        let mut link: MutexGuard<'_, Link> = self.link.lock().unwrap_or_else(|p| p.into_inner());
        link.call(op, params)
    }
}

impl Backend for ProcessBackend {
    fn os(&self) -> RecorderOs {
        self.identity.os
    }

    fn os_build(&self) -> String {
        self.identity.os_build.clone()
    }

    fn helper(&self) -> HelperIdentity {
        self.identity.helper.clone()
    }

    fn permission(&self) -> Permission {
        self.identity.permission
    }

    fn sources(&self) -> Result<Vec<Source>> {
        let result = self.call("sources", json!({}))?;
        decode_sources(&result).map_err(protocol_error)
    }

    fn start(&mut self, plan: &BackendPlan) -> Result<StartInfo> {
        let encoded = encode_plan(plan).map_err(|e| RecordError::new(Code::ArgInvalid, e))?;
        let result = self.call("start", json!({ "plan": encoded }))?;
        self.border = result
            .get("border")
            .and_then(|border| serde_json::from_value(border.clone()).ok());
        decode_start_info(&result).map_err(protocol_error)
    }

    fn border(&self) -> Option<CaptureBorder> {
        self.border.clone()
    }

    fn poll(&mut self) -> Vec<HelperEvent> {
        self.events.try_iter().collect()
    }

    fn stop(&mut self) -> Result<Finished> {
        let result = self.call("stop", json!({}))?;
        decode_finished(&result).map_err(protocol_error)
    }

    fn abort(&mut self) {
        // The helper is torn down whatever it answers. A lost helper cannot answer.
        let _ = self.call("abort", json!({}));
    }
}

fn helper_error(message: String) -> RecordError {
    RecordError::new(Code::Helper, message)
}

fn protocol_error(detail: String) -> RecordError {
    RecordError::new(Code::Helper, format!("protocol: {detail}"))
}

/// Reads the helper's frames until `bye`, a lost stream, or a protocol violation.
/// Responses go to the request side. Events go to the recorder through `poll`.
/// A lost or broken helper is reported as a failure event, then the thread ends.
/// Ending drops the response sender, so a request in flight fails.
fn read_loop<R: Read>(mut input: R, responses: Sender<Value>, events: Sender<HelperEvent>) {
    loop {
        let body = match read_frame_bytes(&mut input, MAX_FRAME_BYTES) {
            Ok(Some(body)) => body,
            // End of stream between frames, or inside one, without `bye`.
            Ok(None) => return lost(&events),
            Err(FrameError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return lost(&events)
            }
            Err(error) => return violation(&events, error.to_string()),
        };
        let frame: Value = match serde_json::from_slice(&body) {
            Ok(frame) => frame,
            Err(error) => return violation(&events, format!("frame is not JSON: {error}")),
        };
        match frame["kind"].as_str() {
            Some("response") => {
                if responses.send(frame).is_err() {
                    return;
                }
            }
            Some("event") => match decode_event(&frame["event"]) {
                Ok(event) => {
                    if events.send(event).is_err() {
                        return;
                    }
                }
                Err(detail) => return violation(&events, detail),
            },
            // The helper finished on purpose. Nothing follows.
            Some("bye") => return,
            other => return violation(&events, format!("unknown frame kind {other:?}")),
        }
    }
}

fn lost(events: &Sender<HelperEvent>) {
    let _ = events.send(HelperEvent::Failed(Failure::HelperLost));
}

fn violation(events: &Sender<HelperEvent>, detail: String) {
    let _ = events.send(HelperEvent::Failed(Failure::Protocol(detail)));
}

/// Serves `backend` to a core over `input` and `output` until the core says
/// `stop` or `abort`, or closes its side.
///
/// Requests are answered in order. Events from the backend are written between
/// requests, and always before the response that follows them. After `stop` or
/// `abort` the helper writes `bye` and returns. If the input ends or breaks, or
/// the output cannot be written, the backend is aborted and the error returned.
pub fn serve_backend<B, R, W>(backend: &mut B, input: R, mut output: W) -> io::Result<()>
where
    B: Backend,
    R: Read + Send + 'static,
    W: Write,
{
    let (frames_tx, frames) = mpsc::channel();
    thread::spawn(move || read_requests(input, frames_tx));
    match serve_loop(backend, &frames, &mut output) {
        Ok(()) => Ok(()),
        Err(error) => {
            backend.abort();
            Err(error)
        }
    }
}

type RequestItem = std::result::Result<Option<Vec<u8>>, FrameError>;

/// Reads request frames on their own thread so the serving loop can drain events
/// while it waits. Ends after the first frame that is not a complete body.
fn read_requests<R: Read>(mut input: R, frames: Sender<RequestItem>) {
    loop {
        let item = read_frame_bytes(&mut input, MAX_FRAME_BYTES);
        let done = !matches!(item, Ok(Some(_)));
        if frames.send(item).is_err() || done {
            return;
        }
    }
}

fn serve_loop<B: Backend, W: Write>(
    backend: &mut B,
    frames: &Receiver<RequestItem>,
    output: &mut W,
) -> io::Result<()> {
    loop {
        match frames.recv_timeout(SERVE_TICK) {
            Ok(Ok(Some(body))) => {
                let request: Value = serde_json::from_slice(&body).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("request is not JSON: {e}"),
                    )
                })?;
                if serve_request(backend, output, &request)? == Served::Finished {
                    return Ok(());
                }
            }
            // The core closed its side or sent a broken frame. Nothing more is answered.
            Ok(_) | Err(RecvTimeoutError::Disconnected) => {
                backend.abort();
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        write_events(backend, output)?;
    }
}

#[derive(PartialEq, Eq)]
enum Served {
    Continue,
    Finished,
}

fn serve_request<B: Backend, W: Write>(
    backend: &mut B,
    output: &mut W,
    request: &Value,
) -> io::Result<Served> {
    let id = request["id"].as_u64().unwrap_or(0);
    let result = match request["op"].as_str() {
        Some("hello") => Ok(identity_result(&Identity {
            os: backend.os(),
            os_build: backend.os_build(),
            helper: backend.helper(),
            permission: backend.permission(),
        })),
        Some("sources") => backend.sources().map(|sources| encode_sources(&sources)),
        Some("start") => match crate::protocol::decode_plan(&request["plan"]) {
            Ok(plan) => backend.start(&plan).map(|info| {
                let mut encoded = encode_start_info(&info);
                if let (Some(border), Some(object)) = (backend.border(), encoded.as_object_mut()) {
                    object.insert("border".into(), json!(border));
                }
                encoded
            }),
            Err(detail) => Err(RecordError::new(Code::ArgInvalid, detail)),
        },
        Some("stop") => {
            let result = backend.stop();
            write_events(backend, output)?;
            respond(
                output,
                id,
                result.map(|finished| encode_finished(&finished)),
            )?;
            write_frame(output, &bye_frame(), MAX_FRAME_BYTES).map_err(frame_io)?;
            return Ok(Served::Finished);
        }
        Some("abort") => {
            backend.abort();
            write_events(backend, output)?;
            respond(output, id, Ok(json!({})))?;
            write_frame(output, &bye_frame(), MAX_FRAME_BYTES).map_err(frame_io)?;
            return Ok(Served::Finished);
        }
        other => Err(RecordError::new(
            Code::ArgInvalid,
            format!("unknown operation {other:?}"),
        )),
    };
    write_events(backend, output)?;
    respond(output, id, result)?;
    Ok(Served::Continue)
}

/// Writes every event the backend has queued. Drains until the backend has none.
fn write_events<B: Backend, W: Write>(backend: &mut B, output: &mut W) -> io::Result<()> {
    loop {
        let batch = backend.poll();
        if batch.is_empty() {
            return Ok(());
        }
        for event in &batch {
            write_frame(output, &event_frame(event), MAX_FRAME_BYTES).map_err(frame_io)?;
        }
    }
}

fn respond<W: Write>(
    output: &mut W,
    id: u64,
    result: std::result::Result<Value, RecordError>,
) -> io::Result<()> {
    let frame = match result {
        Ok(value) => json!({ "kind": "response", "id": id, "ok": true, "result": value }),
        Err(error) => json!({
            "kind": "response",
            "id": id,
            "ok": false,
            "error": encode_error(&error),
        }),
    };
    write_frame(output, &frame, MAX_FRAME_BYTES).map_err(frame_io)
}

fn frame_io(error: FrameError) -> io::Error {
    match error {
        FrameError::Io(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use rightkit_contracts::types::record_event::DropReason;
    use rightkit_contracts::types::record_session::{BundleStatus, VisibilityState};
    use rightkit_contracts::types::Rect;
    use rightkit_sidecar::length_prefixed::{read_frame_bytes, write_frame_bytes};

    use super::*;
    use crate::backend::{Codec, FrameSample};
    use crate::clock::ManualClock;
    use crate::fake::{frame, FakeBackend};
    use crate::plan::{PixelMasking, ScenarioSpec, ScenarioStep, StartRequest, TargetSpec};
    use crate::recorder::Recorder;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rk-record-process-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn request(out: PathBuf) -> StartRequest {
        let mut request = StartRequest::new(
            TargetSpec {
                app_id: "cutright".into(),
                window_id: 4242,
                pid: 77,
                surfaces: vec![rightkit_contracts::types::record_session::Surface {
                    surface_id: "main".into(),
                    kind: rightkit_contracts::types::record_session::SurfaceKind::MainWindow,
                    parent_surface_id: None,
                    native_id: "4242".into(),
                    supported: true,
                    unsupported_reason: None,
                }],
            },
            ScenarioSpec {
                scenario_id: "demo.one".into(),
                steps: vec![ScenarioStep {
                    step_id: "step-1".into(),
                    label: "Open".into(),
                    say: None,
                }],
            },
            out,
        );
        request.key_source =
            crate::redact::KeyHandle::new(crate::redact::FixedKeySource::new([0x5a; 32]));
        request
    }

    /// Serves `backend` on a thread over two pipes and connects a `ProcessBackend` to it.
    fn connected(backend: FakeBackend) -> ProcessBackend {
        let (request_r, request_w) = std::io::pipe().unwrap();
        let (response_r, response_w) = std::io::pipe().unwrap();
        thread::spawn(move || {
            let mut backend = backend;
            let _ = serve_backend(&mut backend, request_r, response_w);
        });
        ProcessBackend::connect(response_r, request_w).expect("connect to the helper")
    }

    fn sample(index: u64, host_ns: u64) -> FrameSample {
        frame(index, host_ns, host_ns)
    }

    #[test]
    fn identity_and_refusals_cross_the_wire() {
        let mut denied = FakeBackend::macos();
        denied.set_permission(Permission::Denied);
        let mut process = connected(denied);
        assert_eq!(process.os(), RecorderOs::Macos);
        assert_eq!(process.os_build(), "27.0.1");
        assert_eq!(process.permission(), Permission::Denied);

        let scratch = scratch("refusal");
        let plan = BackendPlan {
            out_dir: scratch.join("take"),
            window_id: 4242,
            pid: 77,
            fps: 60,
            scale: 1.0,
            audio: Vec::new(),
            visibility: VisibilityState::OffScreenVisible,
            masking: PixelMasking::Declared(Vec::new()),
            codec: Codec::H264,
        };
        fs::create_dir_all(&plan.out_dir).unwrap();
        let err = process.start(&plan).unwrap_err();
        assert_eq!(err.code, Code::PermissionDenied, "{err}");
    }

    #[test]
    fn a_take_over_the_protocol_matches_the_in_process_backend() {
        let scratch = scratch("take");
        let mut backend = FakeBackend::macos();
        backend.push(vec![
            HelperEvent::Ready(sample(0, 5_000)),
            HelperEvent::Frame(sample(1, 5_000 + 16_666_667)),
            HelperEvent::Dropped {
                host_ns: 5_000 + 33_333_334,
                count: 1,
                interval_ms: 16.7,
                reason: DropReason::Backpressure,
            },
        ]);
        let process = connected(backend);
        let out = scratch.join("take");
        let mut rec = Recorder::open(process, ManualClock::new(1_000_000), &[], 30_000)
            .unwrap()
            .0;
        rec.record_start(request(out.clone()))
            .expect("start over the wire");
        // Events are drained by the recorder's poll; they arrive asynchronously.
        let outcome = rec.record_stop().expect("stop over the wire");
        assert_eq!(outcome.status, BundleStatus::Complete, "{outcome:?}");
        let capture: Value =
            serde_json::from_str(&fs::read_to_string(out.join("capture.json")).unwrap()).unwrap();
        assert_eq!(capture["visibilityState"], "off_screen_visible");
        assert_eq!(capture["codec"], "h264");
        assert!(rec.bundle_verify(&out, &Default::default()).unwrap().ok());
    }

    #[test]
    fn a_helper_that_dies_mid_frame_quarantines_the_take() {
        // A raw helper: it answers hello and start, sends a complete readiness
        // marker, then writes a length prefix and only part of a frame, and exits.
        let mut torn = Vec::new();
        torn.extend_from_slice(&200u32.to_be_bytes());
        torn.extend_from_slice(b"{\"kind\":\"event\",");
        let outcome = run_raw_helper(torn, "died-mid-frame");
        assert_eq!(
            outcome.status,
            BundleStatus::PartialQuarantined,
            "{outcome:?}"
        );
        assert_eq!(outcome.reason.as_deref(), Some("helper_lost"));
    }

    #[test]
    fn a_protocol_violation_after_readiness_is_invalid() {
        let mut bad = Vec::new();
        write_frame_bytes(&mut bad, b"not json at all", MAX_FRAME_BYTES).unwrap();
        let outcome = run_raw_helper(bad, "protocol-violation");
        assert_eq!(outcome.status, BundleStatus::Invalid, "{outcome:?}");
        assert!(outcome.reason.unwrap().starts_with("protocol_violation"));
    }

    /// Runs a take against a scripted helper. After the readiness marker the helper
    /// writes `tail` raw and closes its stream. Returns the take's outcome.
    fn run_raw_helper(tail: Vec<u8>, name: &str) -> crate::recorder::Outcome {
        let (core_request_r, core_request_w) = std::io::pipe().unwrap();
        let (response_r, mut response_w) = std::io::pipe().unwrap();
        let helper = thread::spawn(move || {
            let mut input = core_request_r;
            let identity = identity_result(&Identity {
                os: RecorderOs::Macos,
                os_build: "27.0.1".into(),
                helper: HelperIdentity {
                    identity: "raw-helper".into(),
                    signed: false,
                },
                permission: Permission::Granted,
            });
            let hello = read_request(&mut input);
            answer(&mut response_w, hello, identity);
            let start = read_request(&mut input);
            let info = StartInfo {
                host_ns_at_start: 1_000,
                width: 1600,
                height: 900,
                scale: 1.0,
                bounds_pt: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 520.0,
                },
                dpi: 96,
                codec: "h264".into(),
                os_build: "27.0.1".into(),
                visibility: VisibilityState::OffScreenVisible,
            };
            answer(&mut response_w, start, encode_start_info(&info));
            let ready = event_frame(&HelperEvent::Ready(sample(0, 5_000)));
            write_frame(&mut response_w, &ready, MAX_FRAME_BYTES).unwrap();
            response_w.write_all(&tail).unwrap();
            // Dropping the writer ends the stream, as a crashed helper's would.
        });
        let process = ProcessBackend::connect(response_r, core_request_w).unwrap();
        let out = scratch(name).join("take");
        let mut rec = Recorder::open(process, ManualClock::new(1_000_000), &[], 30_000)
            .unwrap()
            .0;
        rec.record_start(request(out))
            .expect("start against the raw helper");
        helper.join().unwrap();
        // The helper's loss arrives asynchronously; pump until the take ends.
        for _ in 0..500 {
            if rec.record_status().unwrap().outcome.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        rec.record_stop()
            .expect("the ended take reports its outcome")
    }

    fn answer<W: Write>(output: &mut W, id: u64, result: Value) {
        let frame = json!({ "kind": "response", "id": id, "ok": true, "result": result });
        write_frame(output, &frame, MAX_FRAME_BYTES).unwrap();
    }

    /// Reads one request frame from the core and returns its id.
    fn read_request<R: Read>(input: &mut R) -> u64 {
        let body = read_frame_bytes(input, MAX_FRAME_BYTES)
            .unwrap()
            .expect("a request");
        let value: Value = serde_json::from_slice(&body).unwrap();
        value["id"].as_u64().unwrap()
    }

    #[test]
    fn serve_backend_reports_a_bad_start_plan_as_arg_invalid() {
        let (request_r, mut request_w) = std::io::pipe().unwrap();
        let (mut response_r, response_w) = std::io::pipe().unwrap();
        thread::spawn(move || {
            let mut backend = FakeBackend::macos();
            let _ = serve_backend(&mut backend, request_r, response_w);
        });
        write_frame(
            &mut request_w,
            &json!({ "kind": "request", "id": 1, "op": "start", "plan": { "outDir": 3 } }),
            MAX_FRAME_BYTES,
        )
        .unwrap();
        let body = read_frame_bytes(&mut response_r, MAX_FRAME_BYTES)
            .unwrap()
            .unwrap();
        let response: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "arg_invalid");
    }
}
