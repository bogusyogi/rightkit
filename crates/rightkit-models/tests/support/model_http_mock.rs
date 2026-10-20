//! Model fixture translation over existing rightkit-http testing transport.
//! No listener, parser, worker or server lifecycle is implemented here.
#![allow(dead_code)]
use rightkit_http::testing::{DeclaredLengthResponse, MockServer as HttpServer, RecordedRequest};
use serde_json::Value;
use std::ops::Deref;

#[derive(Clone)]
pub struct Request(RecordedRequest);
impl Deref for Request {
    type Target = RecordedRequest;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Request {
    pub fn route(&self) -> &str {
        self.0.path()
    }
    pub fn is(&self, method: &str, route: &str) -> bool {
        self.method.eq_ignore_ascii_case(method) && self.route() == route
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}
#[derive(Clone)]
pub struct Reply {
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    ignore_range: bool,
    cut_after: Option<u64>,
}
impl Reply {
    pub fn bytes(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type: content_type.into(),
            headers: vec![],
            body: body.into(),
            ignore_range: false,
            cut_after: None,
        }
    }
    pub fn text(value: &str) -> Self {
        Self::bytes("text/plain; charset=utf-8", value.as_bytes().to_vec())
    }
    pub fn json(value: Value) -> Self {
        Self::status(200, value)
    }
    pub fn status(status: u16, value: Value) -> Self {
        let mut reply = Self::bytes("application/json", value.to_string().into_bytes());
        reply.status = status;
        reply
    }
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    pub fn ignore_range(mut self) -> Self {
        self.ignore_range = true;
        self
    }
    pub fn cut_after(mut self, bytes: u64) -> Self {
        self.cut_after = Some(bytes);
        self
    }
    fn response(self, request: &Request) -> DeclaredLengthResponse {
        let len = self.body.len() as u64;
        let ranged = request.method.eq_ignore_ascii_case("GET")
            || request.method.eq_ignore_ascii_case("HEAD");
        let plan = if self.status == 200 && ranged && !self.ignore_range {
            request
                .header("if-range")
                .is_none_or(|v| if_range_holds(v, &self.headers))
                .then(|| plan_range(request.header("range"), len))
                .flatten()
                .unwrap_or(BodyPlan::Full)
        } else {
            BodyPlan::Full
        };
        let (status, body, range) = match plan {
            BodyPlan::Full => (self.status, &self.body[..], None),
            BodyPlan::Partial(a, b) => (
                206,
                &self.body[a as usize..=b as usize],
                Some(format!("bytes {a}-{b}/{len}")),
            ),
            BodyPlan::Unsatisfiable => (416, &[][..], Some(format!("bytes */{len}"))),
        };
        let take = if request.method.eq_ignore_ascii_case("HEAD") {
            0
        } else {
            self.cut_after
                .map_or(body.len(), |n| (n as usize).min(body.len()))
        };
        let mut response = DeclaredLengthResponse::new(status, body[..take].to_vec(), body.len())
            .header("Content-Type", &self.content_type);
        for (name, value) in &self.headers {
            response = response.header(name, value);
        }
        if let Some(range) = range {
            response = response
                .header("Content-Range", &range)
                .header("Accept-Ranges", "bytes");
        }
        response
    }
}
pub struct MockServer {
    server: HttpServer,
    pub base: String,
}
impl MockServer {
    pub fn start_with(
        handler: impl Fn(&Request, &str) -> Option<Reply> + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let server = HttpServer::start_declared_length(move |request, _| {
            let request = Request(request.clone());
            let base = format!(
                "http://{}",
                request.header("host").expect("model client sends Host")
            );
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&request, &base)))
                .unwrap_or_else(|_| {
                    Some(Reply::status(
                        500,
                        serde_json::json!({"error":"mock handler panicked"}),
                    ))
                })
                .unwrap_or_else(|| Reply::status(404, serde_json::json!({"error":"unmocked"})))
                .response(&request)
        });
        let base = server.base_url();
        Ok(Self { server, base })
    }
    pub fn start_static(routes: Vec<(Option<String>, String, Reply)>) -> std::io::Result<Self> {
        Self::start_with(move |request, _| {
            routes
                .iter()
                .find(|(method, path, _)| {
                    method
                        .as_ref()
                        .is_none_or(|method| request.method.eq_ignore_ascii_case(method))
                        && request.route() == path
                })
                .map(|(_, _, reply)| reply.clone())
        })
    }
    pub fn url(&self, path: &str) -> String {
        self.server.url(path)
    }
    pub fn seen(&self) -> Vec<Request> {
        self.server.requests().into_iter().map(Request).collect()
    }
}

// Range/If-Range fixture behavior retained from rightkit-qa::mock; transport remains HTTP-owned.
/// How a reply's body is sent for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BodyPlan {
    Full,
    /// Inclusive byte range of the body.
    Partial(u64, u64),
    Unsatisfiable,
}

/// Parse a single `bytes=` range against a body of `len` bytes. `None` means
/// "ignore the header" (absent, malformed or multi-range: serve the full body).
fn plan_range(range: Option<&str>, len: u64) -> Option<BodyPlan> {
    let spec = range?.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if a.is_empty() {
        if !digits(b) {
            return None;
        }
        let n: u64 = b.parse().ok()?;
        if n == 0 || len == 0 {
            return Some(BodyPlan::Unsatisfiable);
        }
        return Some(BodyPlan::Partial(len.saturating_sub(n), len - 1));
    }
    if !digits(a) || !(b.is_empty() || digits(b)) {
        return None;
    }
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() {
        None
    } else {
        Some(b.parse::<u64>().ok()?)
    };
    if end.is_some_and(|e| e < start) {
        return None;
    }
    if start >= len {
        return Some(BodyPlan::Unsatisfiable);
    }
    Some(BodyPlan::Partial(
        start,
        end.map_or(len - 1, |e| e.min(len - 1)),
    ))
}

/// `If-Range` holds when it equals the reply's strong `ETag` or its `Last-Modified`.
fn if_range_holds(if_range: &str, headers: &[(String, String)]) -> bool {
    let v = if_range.trim();
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.trim())
    };
    if v.starts_with('"') || v.starts_with("W/") {
        !v.starts_with("W/") && get("etag").is_some_and(|e| e == v && !e.starts_with("W/"))
    } else {
        get("last-modified").is_some_and(|m| m == v)
    }
}
