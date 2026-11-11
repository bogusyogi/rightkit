// rightkit-daemon-remote 0.1.0 registry consumer, outside the workspace, pinning
// `rightkit-daemon-remote =0.1.0` with the `remote` and `relay` features (so `link` too) and
// `rightkit-daemon =0.8.0` with default features for the handler contract the device route calls.
//
// Proves, in process (the daemon core is transport-free; no listener, no TLS, no network):
// `Daemon::open` over a per-run state directory with `DeviceCalls` built from a rightkit-daemon
// `MethodTable` and `Handler`; `issue_enrolment_code`; enrolment over `POST /v1/devices` signed
// with `remote::signing::sign_request` (proof of possession, `remote::client::DeviceKey` for the
// public key); a signed `POST /v1/call/echo` that reaches the handler as `Caller::Device` with its
// role; refusal of a local-only method (403), of an unsigned request (401) and of a replayed nonce
// (401); `add_public_route` (served without a signature, `/v1` refused); `list_devices`,
// `metrics`; `revoke_device` and the `device_revoked` answer after it; and one `rk-relay/1` round
// trip through `relay::RelayCore` between two `RelaySession`s of one circle (challenge, ticket
// authentication, a sealed `Put` delivered live, `Stored`, the de-duplicated `Deliver`).
// Does not prove: `remote::serve`/`serve_blocking` (TLS listener), uploads, OAuth and webhook
// routes, pairing, the `rk-link/1` link client, relay persistence or quotas.
// State: one per-run directory under the OS temp directory, removed on exit. No real user data.
use rightkit_daemon::contract::{Caller, MethodPolicy, MethodTable};
use rightkit_daemon::{CallContext, Emit, Handler, ServiceError};
use rightkit_daemon_remote::relay::{
    issue_ticket, Class, ClientFrame, Event, MailboxId, Outgoing, RelayCore, RelayLimits,
    RelaySession, SealError, Sealer, ServerFrame,
};
use rightkit_daemon_remote::remote::client::DeviceKey;
use rightkit_daemon_remote::remote::signing::{sign_request, H_NONCE, H_SIGNATURE, H_TIMESTAMP};
use rightkit_daemon_remote::remote::{
    Clock, Daemon, DaemonConfig, DeviceCalls, Request, Response, SystemClock,
};
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        println!("FAIL: {what}");
        std::process::exit(1);
    }
}

struct RunDir(PathBuf);

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Echo;

impl Handler for Echo {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        let device = match cx.origin() {
            Caller::Device { id, .. } => id.clone(),
            Caller::LocalApp { .. } => return Err(ServiceError::new("local", "local caller")),
        };
        match method {
            "echo" => Ok(json!({ "echo": params, "device": device, "role": cx.role() })),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn signed(
    key: &Ed25519KeyPair,
    auth: &str,
    path: &str,
    nonce: &str,
    now: u64,
    body: &[u8],
) -> Request {
    let mut headers = BTreeMap::new();
    headers.insert("authorization".to_string(), auth.to_string());
    headers.insert(H_TIMESTAMP.to_string(), now.to_string());
    headers.insert(H_NONCE.to_string(), nonce.to_string());
    headers.insert(
        H_SIGNATURE.to_string(),
        sign_request(key, "POST", path, now, nonce, body),
    );
    headers.insert("content-type".to_string(), "application/json".to_string());
    Request {
        method: "POST".into(),
        path_and_query: path.into(),
        headers,
        body: body.to_vec(),
        ..Request::default()
    }
}

fn remote_host() -> Result<(), Box<dyn Error>> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let run = RunDir(std::env::temp_dir().join(format!(
        "rk-daemon-remote-consumer-{}-{nanos}",
        std::process::id()
    )));
    let table = MethodTable::new()
        .method(
            "echo",
            MethodPolicy::local(["alpha"]).and_remote(["role:operator"]),
        )
        .method("local_only", MethodPolicy::local(["alpha"]));
    let daemon = Daemon::open(
        DaemonConfig::new(run.0.join("state")),
        Arc::new(SystemClock),
        None,
    )?
    .with_calls(DeviceCalls::new(table, Arc::new(Echo)));
    check(
        daemon.list_devices().is_empty(),
        "a fresh daemon has no device",
    );

    let code = daemon.issue_enrolment_code(&["role:operator"], 600)?;
    check(
        code.starts_with("rk-"),
        "issue_enrolment_code returns a single-use code",
    );

    let seed = [7u8; 32];
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|e| format!("{e:?}"))?;
    let public_b64 = DeviceKey::from_seed(&seed)?.public_key_b64();
    let now = SystemClock.now_secs();

    let body = serde_json::to_vec(&json!({ "publicKey": public_b64, "label": "consumer" }))?;
    let enrolled = daemon.handle(signed(
        &pair,
        &format!("RK-Enrol {code}"),
        "/v1/devices",
        "n-enrol",
        now,
        &body,
    ));
    check(enrolled.status == 201, "signed enrolment is answered 201");
    let device = enrolled.body["deviceId"]
        .as_str()
        .ok_or("enrolment answer has no deviceId")?
        .to_string();
    check(
        device.starts_with("dev_"),
        "the enrolment answer names the device",
    );

    let auth = format!("RK-Device {device}");
    let call = daemon.handle(signed(
        &pair,
        &auth,
        "/v1/call/echo",
        "n-call-1",
        now,
        br#"{"x":1}"#,
    ));
    check(call.status == 200, "a signed device call is answered 200");
    check(
        call.body["echo"] == json!({ "x": 1 }) && call.body["device"] == json!(device),
        "the rightkit-daemon Handler ran with the device as Caller::Device",
    );
    check(
        call.body["role"] == json!("operator"),
        "CallContext::role carries the enrolled role scope",
    );

    let local_only = daemon.handle(signed(
        &pair,
        &auth,
        "/v1/call/local_only",
        "n-call-2",
        now,
        b"{}",
    ));
    check(
        local_only.status == 403 && local_only.body["error"]["code"] == "forbidden",
        "a local-only method is refused to a device with 403 forbidden",
    );

    let replay = daemon.handle(signed(
        &pair,
        &auth,
        "/v1/call/echo",
        "n-call-1",
        now,
        br#"{"x":1}"#,
    ));
    check(replay.status == 401, "a replayed nonce is refused with 401");

    let mut unsigned = signed(&pair, &auth, "/v1/call/echo", "n-call-3", now, b"{}");
    unsigned.headers.remove(H_SIGNATURE);
    check(
        daemon.handle(unsigned).status == 401,
        "an unsigned device call is refused with 401",
    );

    daemon.add_public_route("GET", "/hello", 0, |_req: &Request| {
        Response::json(200, json!({ "hello": true }))
    })?;
    let hello = daemon.handle(Request {
        method: "GET".into(),
        path_and_query: "/hello".into(),
        ..Request::default()
    });
    check(
        hello.status == 200 && hello.body["hello"] == json!(true),
        "a public route is served without a signature",
    );
    check(
        daemon
            .add_public_route("GET", "/v1/hello", 0, |_req: &Request| Response::empty(204))
            .is_err(),
        "a public route under /v1 is refused",
    );

    let devices = daemon.list_devices();
    check(devices.len() == 1, "list_devices shows the enrolled device");
    check(daemon.metrics().devices == 1, "metrics count one device");

    check(
        daemon.revoke_device(&device)?,
        "revoke_device revokes the device",
    );
    let revoked = daemon.handle(signed(
        &pair,
        &auth,
        "/v1/call/echo",
        "n-call-4",
        now,
        b"{}",
    ));
    check(
        revoked.status == 401 && revoked.body["error"] == "device_revoked",
        "a revoked device is answered 401 device_revoked",
    );
    drop(daemon);
    drop(run);
    Ok(())
}

/// A stand-in cipher: the relay never reads the bytes, so the recipe only needs a reversible one.
struct Xor;

impl Sealer for Xor {
    fn seal(&self, _to: &MailboxId, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
        Ok(plaintext.iter().map(|b| b ^ 0x5a).collect())
    }
    fn open(&self, _from: &MailboxId, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
        Ok(sealed.iter().map(|b| b ^ 0x5a).collect())
    }
}

fn frames_for(out: &[Outgoing], conn: u64) -> Vec<ServerFrame> {
    out.iter()
        .filter_map(|o| match o {
            Outgoing::Send(c, f) if *c == conn => Some(f.clone()),
            _ => None,
        })
        .collect()
}

fn authenticate(
    core: &mut RelayCore,
    session: &mut RelaySession<Xor>,
    conn: u64,
    now: u64,
) -> Result<bool, Box<dyn Error>> {
    let challenge = core.connect(conn, now).map_err(|e| format!("{e:?}"))?;
    let Event::Reply(auth) = session.handle(challenge).map_err(|e| format!("{e:?}"))? else {
        return Ok(false);
    };
    let out = core.handle(conn, now, auth);
    for frame in frames_for(&out, conn) {
        if session.handle(frame).map_err(|e| format!("{e:?}"))? == Event::Authed {
            return Ok(true);
        }
    }
    Ok(false)
}

fn relay_round_trip() -> Result<(), Box<dyn Error>> {
    let now = SystemClock.now_secs();
    let owner = Ed25519KeyPair::from_seed_unchecked(&[1u8; 32]).map_err(|e| format!("{e:?}"))?;
    let key_a = Ed25519KeyPair::from_seed_unchecked(&[2u8; 32]).map_err(|e| format!("{e:?}"))?;
    let key_b = Ed25519KeyPair::from_seed_unchecked(&[3u8; 32]).map_err(|e| format!("{e:?}"))?;
    let pub_a: [u8; 32] = DeviceKey::from_seed(&[2u8; 32])?
        .public_key()
        .try_into()
        .map_err(|_| "key")?;
    let pub_b: [u8; 32] = DeviceKey::from_seed(&[3u8; 32])?
        .public_key()
        .try_into()
        .map_err(|_| "key")?;
    let limits = RelayLimits::default();
    let mut a = RelaySession::new(
        key_a,
        issue_ticket(&owner, &pub_a, now + 3600),
        Xor,
        limits.clone(),
    )
    .map_err(|e| format!("{e:?}"))?;
    let mut b = RelaySession::new(
        key_b,
        issue_ticket(&owner, &pub_b, now + 3600),
        Xor,
        limits.clone(),
    )
    .map_err(|e| format!("{e:?}"))?;
    check(
        a.circle() == b.circle(),
        "two devices of one owner share a circle",
    );

    let mut core = RelayCore::new(limits);
    check(
        authenticate(&mut core, &mut a, 1, now)?,
        "device A authenticates with its ticket",
    );
    check(
        authenticate(&mut core, &mut b, 2, now)?,
        "device B authenticates with its ticket",
    );

    let (_id, put) = a
        .put(&b.mailbox(), Class::Small, b"hello over the relay")
        .map_err(|e| format!("{e:?}"))?;
    let out = core.handle(1, now, put.clone());
    check(
        frames_for(&out, 1)
            .iter()
            .any(|f| matches!(f, ServerFrame::Stored { .. })),
        "the relay stores the sealed message for the sender",
    );
    let mut delivered = false;
    for frame in frames_for(&out, 2) {
        if let Event::Message(d) = b.handle(frame.clone()).map_err(|e| format!("{e:?}"))? {
            delivered = d.plaintext == b"hello over the relay" && d.from == a.mailbox();
        }
        check(
            b.handle(frame).map_err(|e| format!("{e:?}"))? == Event::Duplicate,
            "a second delivery of the same id is reported as a duplicate",
        );
    }
    check(delivered, "device B opens the live delivery from device A");
    let ack = b.take_ack().ok_or("device B has nothing to acknowledge")?;
    check(
        matches!(&ack, ClientFrame::Ack { ids } if !ids.is_empty() && ids.iter().all(|i| *i == ids[0])),
        "device B acknowledges the delivered id (once per delivery)",
    );
    check(
        frames_for(&core.handle(2, now, ack), 2)
            .iter()
            .any(|f| matches!(f, ServerFrame::Acked { count } if *count >= 1)),
        "the relay confirms the acknowledgement",
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    remote_host()?;
    relay_round_trip()?;
    println!(
        "OK rightkit-daemon-remote 0.1.0 (registry consumer, remote host in process + rk-relay/1)"
    );
    Ok(())
}
