//! rightkit-http (default features): real requests over loopback sockets to a server this probe starts. Proves the
//! public-web address policy refuses loopback by default (the SSRF guard) and that an explicit loopback allowance
//! reaches it; exact request framing (method, target, auth, JSON body, user agent); retry with backoff through 503s;
//! a response-size bound; secret scrubbing of error text.
use rightkit_http::{
    form_encode, percent_decode, percent_encode_component, AddressPolicy, Client, ClientConfig,
    HttpError, Request, RetryPolicy,
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SECRET: &str = "sk-test-secret-7788";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-http: {what}"))
    }
}

fn e<T: std::fmt::Debug>(what: &str) -> impl FnOnce(T) -> String + '_ {
    move |err| format!("rightkit-http: {what}: {err:?}")
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Seen> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            if k == "content-length" {
                len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Seen {
        method,
        target,
        headers,
        body,
    })
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(e("bind"))?;
    listener.set_nonblocking(true).map_err(e("nonblocking"))?;
    let port = listener.local_addr().map_err(e("local_addr"))?.port();
    let base = format!("http://127.0.0.1:{port}");
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let flaky = Arc::new(AtomicUsize::new(0));

    let (seen_s, stop_s, flaky_s) = (seen.clone(), stop.clone(), flaky.clone());
    let server = std::thread::spawn(move || {
        while !stop_s.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let Ok(req) = read_request(&mut stream) else {
                        continue;
                    };
                    let target = req.target.clone();
                    seen_s.lock().unwrap_or_else(|p| p.into_inner()).push(req);
                    let _ = if target.starts_with("/echo") {
                        respond(
                            &mut stream,
                            200,
                            "application/json",
                            "",
                            br#"{"ok":true,"n":7}"#,
                        )
                    } else if target.starts_with("/flaky") {
                        if flaky_s.fetch_add(1, Ordering::SeqCst) < 2 {
                            respond(
                                &mut stream,
                                503,
                                "text/plain",
                                "Retry-After: 0\r\n",
                                b"busy",
                            )
                        } else {
                            respond(&mut stream, 200, "text/plain", "", b"recovered")
                        }
                    } else if target.starts_with("/big") {
                        respond(
                            &mut stream,
                            200,
                            "application/octet-stream",
                            "",
                            &[b'z'; 4096],
                        )
                    } else if target.starts_with("/leak") {
                        respond(
                            &mut stream,
                            500,
                            "text/plain",
                            "",
                            format!("bad token {SECRET}").as_bytes(),
                        )
                    } else {
                        respond(&mut stream, 404, "text/plain", "", b"nope")
                    };
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    let finish = |result: Result<Value, String>| {
        stop.store(true, Ordering::Release);
        let _ = server.join();
        result
    };

    let result = (|| -> Result<Value, String> {
        // The default public-web policy refuses loopback and sends nothing.
        let guarded = Client::new(ClientConfig {
            retry: RetryPolicy::none(),
            ..ClientConfig::default()
        });
        match guarded.send(&Request::get(format!("{base}/echo"))) {
            Err(HttpError::AddressDenied { .. }) => {}
            other => {
                return Err(format!(
                "rightkit-http: loopback was not refused by the default address policy: {other:?}"
            ))
            }
        }
        ensure(
            seen.lock().map_err(|_| "poisoned")?.is_empty(),
            "a refused request still reached the server",
        )?;

        let loopback = AddressPolicy::default()
            .allow_if(vec!["127.0.0.1".into(), "localhost".into()], |ip| {
                ip.is_loopback()
            });
        let config = ClientConfig {
            retry: RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(20),
                max_backoff: Duration::from_millis(200),
                jitter: 0.0,
                ..RetryPolicy::default()
            },
            request_timeout: Duration::from_secs(5),
            response_timeout: Some(Duration::from_secs(5)),
            ..ClientConfig::default()
        }
        .address_policy(loopback);
        let client = Client::new(config);

        // Exact request framing and a JSON response.
        let resp = client
            .send(
                &Request::post(format!("{base}/echo?x=1"))
                    .bearer("sk-test")
                    .json(&json!({ "hello": "world" }))
                    .map_err(e("json body"))?,
            )
            .map_err(e("POST /echo"))?;
        ensure(resp.status == 200, "POST status")?;
        ensure(
            resp.json::<Value>().map_err(e("response json"))?["n"] == 7,
            "response body",
        )?;
        let first = seen.lock().map_err(|_| "poisoned")?[0].clone();
        ensure(
            first.method == "POST" && first.target == "/echo?x=1",
            &format!("request line was {} {}", first.method, first.target),
        )?;
        ensure(
            first.header("authorization") == Some("Bearer sk-test"),
            "authorization header",
        )?;
        ensure(
            first.header("content-type") == Some("application/json"),
            "content-type header",
        )?;
        ensure(
            first
                .header("user-agent")
                .is_some_and(|u| u.starts_with("rightkit-http/")),
            "user-agent header",
        )?;
        ensure(
            serde_json::from_slice::<Value>(&first.body).map_err(e("body json"))?
                == json!({ "hello": "world" }),
            "request body",
        )?;

        // Form bodies are percent-encoded.
        client
            .send(&Request::post(format!("{base}/echo")).form(&[("a b", "c&d=e"), ("u", "é")]))
            .map_err(e("form POST"))?;
        let form = seen
            .lock()
            .map_err(|_| "poisoned")?
            .last()
            .cloned()
            .ok_or("rightkit-http: form request not seen")?;
        ensure(
            String::from_utf8_lossy(&form.body) == "a+b=c%26d%3De&u=%C3%A9",
            &format!("form body was {:?}", String::from_utf8_lossy(&form.body)),
        )?;
        ensure(
            form_encode(&[("k", "v w")]) == "k=v+w"
                && percent_decode(&percent_encode_component("a/b c")) == "a/b c",
            "form/percent helpers",
        )?;

        // Two 503s then success: the client retried with backoff.
        let started = std::time::Instant::now();
        let ok = client
            .send(&Request::get(format!("{base}/flaky")))
            .map_err(e("GET /flaky"))?;
        ensure(
            ok.status == 200 && ok.text() == "recovered" && ok.attempts == 3,
            &format!("flaky ended status {} attempts {}", ok.status, ok.attempts),
        )?;
        ensure(
            flaky.load(Ordering::SeqCst) == 3,
            "the server did not see exactly three flaky attempts",
        )?;
        let retry_ms = started.elapsed().as_millis() as u64;

        // A response past the size bound is refused.
        match client.send(&Request::get(format!("{base}/big")).max_response_bytes(100)) {
            Err(HttpError::BodyTooLarge { limit: 100 }) => {}
            other => {
                return Err(format!(
                    "rightkit-http: an oversized body was not refused: {other:?}"
                ))
            }
        }

        // A failing response that echoes a secret never surfaces it in the error.
        let no_retry = Client::new(ClientConfig {
            retry: RetryPolicy::none(),
            ..client_config_again()?
        });
        match no_retry.send(
            &Request::get(format!("{base}/leak"))
                .bearer(SECRET)
                .redact_value(SECRET),
        ) {
            Err(err) => {
                let text = format!("{err} {err:?}");
                ensure(
                    !text.contains(SECRET),
                    &format!("the error leaked the secret: {text}"),
                )?;
                ensure(
                    matches!(err, HttpError::Status { status: 500, .. }),
                    &format!("expected Status 500, got {err:?}"),
                )?;
            }
            Ok(r) => {
                return Err(format!(
                    "rightkit-http: a 500 was returned as success: {}",
                    r.status
                ))
            }
        }
        Ok(
            json!({ "requests_seen": seen.lock().map_err(|_| "poisoned")?.len(), "retry_ms": retry_ms, "flaky_attempts": 3 }),
        )
    })();
    finish(result)
}

fn client_config_again() -> Result<ClientConfig, String> {
    let loopback =
        AddressPolicy::default().allow_if(vec!["127.0.0.1".into()], |ip| ip.is_loopback());
    Ok(ClientConfig {
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..ClientConfig::default()
    }
    .address_policy(loopback))
}
