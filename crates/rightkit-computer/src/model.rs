//! Platform-neutral wire types shared by the macOS and (future) Windows backends.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub window_id: u32,
    pub pid: i32,
    pub owner: String,
    pub title: String,
    pub layer: i32,
    pub on_screen: bool,
    pub bounds: Rect,
}

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub role: String,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub frame: Option<Rect>,
    pub actions: Vec<String>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }
    pub fn render(&self, out: &mut String, depth: usize) {
        use std::fmt::Write;
        let _ = write!(out, "{}{}", "  ".repeat(depth), self.role);
        if let Some(t) = &self.title {
            let _ = write!(out, " \"{t}\"");
        }
        if let Some(v) = &self.value {
            let v: String = v.chars().take(60).collect();
            let _ = write!(out, " = {v:?}");
        }
        if !self.actions.is_empty() {
            let _ = write!(out, " [{}]", self.actions.join(","));
        }
        out.push('\n');
        for c in &self.children {
            c.render(out, depth + 1);
        }
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub accessibility: bool,
    pub screen_recording: bool,
}

#[derive(Debug)]
pub enum Error {
    PermissionDenied(&'static str),
    NotFound(String),
    Ax(i32, &'static str),
    Unsupported(&'static str),
    Blocked(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::PermissionDenied(p) => write!(f, "permission denied: {p}"),
            Error::NotFound(s) => write!(f, "not found: {s}"),
            Error::Ax(c, op) => write!(f, "accessibility error {c} during {op}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::Blocked(s) => write!(f, "blocked by safety policy: {s}"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
