//! Native source rule `appkit.autorelease.thread-loop-no-pool` (Orthic-Labs/rightkit#893,
//! heardright#292): a long-lived `std::thread` loop that calls AppKit, Accessibility or
//! CoreFoundation without an `objc2::rc::autoreleasepool` drained once per iteration.
//!
//! A pool-less polling thread leaks every autoreleased object it touches: HeardRight's
//! frontmost-app poller held 2.18M `NSRunningApplication` objects (1.1 GB) after 62 hours.
//!
//! The catalog fingerprints are single-pattern regular expressions and cannot match a
//! closure body, a loop body and the absence of a pool inside it, and a regular
//! expression that cannot finish reports no match (a silent clean). This rule is
//! therefore evaluated here over a lexed view of the file (the comment and literal mask
//! of [`source_non_code`], the same one the `codeOnly` catalog patterns use). It runs only
//! when the scanned catalog declares the [`CAPABILITY`] capability, so the protected
//! catalog digest still controls whether the rule is active. Findings have the shape of
//! source findings and are excepted through the usual plain entries in
//! `.rightkit/ownership-exceptions.json`.
//!
//! What counts:
//!
//! - Scope: `.rs` files whose path says macOS (`/macos/`, `macos_`, `_macos.rs`,
//!   `/macos.rs`) or that name a non-negated `target_os = "macos"` in a `cfg` attribute.
//! - Spawn: `thread::spawn(..)`, `.spawn(..)`, `.spawn_scoped(..)` whose argument is a
//!   closure, or a bare function name defined in the same file.
//! - Long-lived: every `loop` and `while` in that body (no loop is proven bounded), also
//!   inside same-file helper functions the body calls (up to three hops).
//! - Call: a path from `objc2_app_kit`, `objc2_application_services`,
//!   `objc2_core_foundation`, `core_foundation(_sys)`, `accessibility_sys`,
//!   `objc2_foundation::NS*`; `NS*`, `AX*` or `CF*` associated or function calls;
//!   `msg_send!` / `msg_send_id!`.
//! - Pool: an `autoreleasepool(..)` call lying inside the loop body that wraps the call.
//!   A pool around the whole loop is not enough: it only drains when the loop exits.
//!
//! Not seen: helpers defined in other files, calls through trait objects or closures held
//! in variables, and methods called on a value obtained outside the loop.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::json::{field, items, str_of};
use crate::source::{extname, source_non_code};

/// Capability the finding is reported under (and an exception entry names).
pub const CAPABILITY: &str = "appkit-autorelease-pool";
/// Owning package of [`CAPABILITY`] in the catalog.
pub const PACKAGE: &str = "rightkit-ownership";
/// Stable rule id (an exception entry's optional `fingerprint`).
pub const FINGERPRINT: &str = "appkit.autorelease.thread-loop-no-pool";
/// The remedy every finding states.
pub const FIX: &str = "drain objc2::rc::autoreleasepool once per loop iteration";

/// Same-file helper hops followed from a spawned body or a loop body.
const MAX_HELPER_DEPTH: usize = 3;

/// The owning package when the catalog declares [`CAPABILITY`]; the rule is inert otherwise.
pub fn declared_package(manifest: &Value) -> Option<String> {
    items(field(manifest, "capabilities"))
        .iter()
        .find(|c| str_of(field(c, "capability")) == CAPABILITY)
        .map(|c| str_of(field(c, "package")).to_string())
}

/// Does the path say the file is macOS-only code?
pub fn path_is_macos(rel: &str) -> bool {
    let p = format!("/{}", rel.replace('\\', "/").to_ascii_lowercase());
    p.contains("/macos/")
        || p.contains("macos_")
        || p.ends_with("_macos.rs")
        || p.ends_with("/macos.rs")
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

enum Kind {
    /// A call into AppKit, Accessibility or CoreFoundation; the path as written.
    Api(String),
    /// `autoreleasepool(..)`: byte offsets of its parentheses.
    Pool { open: usize, close: usize },
    /// A plain, `Self::`, `self::`, `super::` or `self.` call that may name a same-file fn.
    Call(String),
    /// `loop` or `while`: byte offsets of the body braces.
    Loop { close: usize },
    /// `fn name`: byte offsets of the body braces.
    FnDef {
        name: String,
        open: usize,
        close: usize,
    },
    /// A spawn call: byte offsets of its parentheses.
    Spawn { open: usize, close: usize },
}

struct Tok {
    pos: usize,
    kind: Kind,
}

struct Src<'a> {
    text: &'a str,
    b: &'a [u8],
    non_code: Vec<bool>,
}

impl<'a> Src<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            b: text.as_bytes(),
            non_code: source_non_code(text, true),
        }
    }

    fn word_end(&self, mut i: usize) -> usize {
        while i < self.b.len() && is_word(self.b[i]) {
            i += 1;
        }
        i
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.b.len() && (self.b[i].is_ascii_whitespace() || self.non_code[i]) {
            i += 1;
        }
        i
    }

    /// Skip trivia before a literal without skipping the literal's non-code mask.
    fn skip_trivia(&self, mut i: usize) -> usize {
        loop {
            while self.b.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if self.b[i..].starts_with(b"//") {
                while i < self.b.len() && self.b[i] != b'\n' {
                    i += 1;
                }
            } else if self.b[i..].starts_with(b"/*") {
                i += 2;
                let mut depth = 1usize;
                while i < self.b.len() && depth > 0 {
                    if self.b[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if self.b[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            } else {
                return i;
            }
        }
    }

    /// Index just after the last non-whitespace byte before `i`.
    fn skip_ws_back(&self, mut i: usize) -> usize {
        while i > 0 && (self.b[i - 1].is_ascii_whitespace() || self.non_code[i - 1]) {
            i -= 1;
        }
        i
    }

    fn word_ending_at(&self, end: usize) -> &str {
        let mut start = end;
        while start > 0 && is_word(self.b[start - 1]) {
            start -= 1;
        }
        self.text.get(start..end).unwrap_or("")
    }

    /// The closing bracket matching the code bracket at `open`.
    fn match_close(&self, open: usize) -> Option<usize> {
        let (o, c) = match *self.b.get(open)? {
            b'{' => (b'{', b'}'),
            b'(' => (b'(', b')'),
            b'[' => (b'[', b']'),
            _ => return None,
        };
        let mut depth = 0usize;
        let mut i = open;
        while i < self.b.len() {
            if !self.non_code[i] {
                if self.b[i] == o {
                    depth += 1;
                } else if self.b[i] == c {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                }
            }
            i += 1;
        }
        None
    }

    /// The first code `{` at or after `from`, skipping parenthesised and bracketed groups;
    /// `None` when a `;` comes first.
    fn find_block_open(&self, from: usize) -> Option<usize> {
        let mut i = from;
        while i < self.b.len() {
            if self.non_code[i] {
                i += 1;
                continue;
            }
            match self.b[i] {
                b'{' => return Some(i),
                b';' => return None,
                b'(' | b'[' => {
                    i = self.match_close(i)? + 1;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Body braces of the `fn` whose name ends at `after_name`.
    fn fn_body(&self, after_name: usize) -> Option<(usize, usize)> {
        let mut i = self.skip_ws(after_name);
        if self.b.get(i) == Some(&b'<') {
            let mut depth = 0usize;
            while i < self.b.len() {
                match self.b[i] {
                    b'-' if self.b.get(i + 1) == Some(&b'>') => {
                        i += 2;
                        continue;
                    }
                    b'<' => depth += 1,
                    b'>' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            i = self.skip_ws(i);
        }
        if self.b.get(i) != Some(&b'(') || self.non_code[i] {
            return None;
        }
        let params_close = self.match_close(i)?;
        let open = self.find_block_open(params_close + 1)?;
        let close = self.match_close(open)?;
        Some((open, close))
    }

    /// Byte range of the closure body inside a spawn call's parentheses: from just after
    /// the closure parameters to the closing parenthesis.
    fn closure_body(&self, open: usize, close: usize) -> Option<(usize, usize)> {
        let mut depth = 0usize;
        let mut i = open + 1;
        while i < close {
            if self.non_code[i] {
                i += 1;
                continue;
            }
            match self.b[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'|' if depth == 0 => {
                    let before = self.skip_ws_back(i);
                    let starts_closure = before > 0
                        && (self.b[before - 1] == b'('
                            || self.b[before - 1] == b','
                            || self.word_ending_at(before) == "move");
                    if starts_closure {
                        let mut j = i + 1;
                        if self.b.get(j) == Some(&b'|') {
                            j += 1;
                        } else {
                            while j < close && !(self.b[j] == b'|' && !self.non_code[j]) {
                                j += 1;
                            }
                            j += 1;
                        }
                        return Some((j.min(close), close));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Does a `cfg` attribute (or `cfg!`) name `target_os = "macos"` without a `not(..)`?
    fn names_macos_cfg(&self) -> bool {
        for (idx, _) in self.text.match_indices("target_os") {
            if self.non_code[idx] || (idx > 0 && is_word(self.b[idx - 1])) {
                continue;
            }
            let eq = self.skip_ws(idx + "target_os".len());
            if self.b.get(eq) != Some(&b'=') {
                continue;
            }
            // Comments are trivia too; the literal itself must remain visible here.
            let quote = self.skip_trivia(eq + 1);
            if !self
                .text
                .get(quote..)
                .is_some_and(|rest| rest.starts_with("\"macos\""))
            {
                continue;
            }
            let attr_start = self
                .text
                .get(..idx)
                .and_then(|head| head.rfind('#'))
                .unwrap_or(0);
            let segment = self.text.get(attr_start..idx).unwrap_or("");
            if !negated_in_attr(segment) {
                return true;
            }
        }
        false
    }

    fn is_self_receiver(&self, prev_end: usize) -> bool {
        if prev_end == 0 || self.b[prev_end - 1] != b'.' {
            return false;
        }
        let before_dot = self.skip_ws_back(prev_end - 1);
        self.word_ending_at(before_dot) == "self"
    }

    fn tokenize(&self) -> Vec<Tok> {
        let n = self.b.len();
        let mut out: Vec<Tok> = Vec::new();
        let mut i = 0usize;
        while i < n {
            if self.non_code[i] {
                i += 1;
                continue;
            }
            let c = self.b[i];
            if c.is_ascii_digit() {
                i = self.word_end(i);
                continue;
            }
            if !(c.is_ascii_alphabetic() || c == b'_') {
                i += 1;
                continue;
            }
            let start = i;
            let mut end = self.word_end(i);
            while self.b[end..].starts_with(b"::")
                && end + 2 < n
                && (self.b[end + 2].is_ascii_alphabetic() || self.b[end + 2] == b'_')
                && !self.non_code[end + 2]
            {
                end = self.word_end(end + 2);
            }
            let chain = self.text.get(start..end).unwrap_or("");
            let segs: Vec<&str> = chain.split("::").collect();
            let last = segs[segs.len() - 1];
            let next_at = self.skip_ws(end);
            let next = self.b.get(next_at).copied();
            let prev_end = self.skip_ws_back(start);
            let prev = if prev_end > 0 {
                Some(self.b[prev_end - 1])
            } else {
                None
            };
            let after_dot = prev == Some(b'.');
            i = end;

            if segs.len() == 1
                && (last == "loop" || last == "while")
                && !after_dot
                && prev != Some(b'#')
            {
                if let Some(open) = self.find_block_open(end) {
                    if let Some(close) = self.match_close(open) {
                        out.push(Tok {
                            pos: start,
                            kind: Kind::Loop { close },
                        });
                    }
                }
                continue;
            }
            if segs.len() == 1 && last == "fn" && !after_dot {
                let name_at = self.skip_ws(end);
                if name_at < n && (self.b[name_at].is_ascii_alphabetic() || self.b[name_at] == b'_')
                {
                    let name_end = self.word_end(name_at);
                    if let Some((open, close)) = self.fn_body(name_end) {
                        out.push(Tok {
                            pos: start,
                            kind: Kind::FnDef {
                                name: self.text.get(name_at..name_end).unwrap_or("").to_string(),
                                open,
                                close,
                            },
                        });
                    }
                    i = name_end;
                }
                continue;
            }
            if last == "autoreleasepool" && next == Some(b'(') && !after_dot {
                if let Some(close) = self.match_close(next_at) {
                    out.push(Tok {
                        pos: start,
                        kind: Kind::Pool {
                            open: next_at,
                            close,
                        },
                    });
                }
                continue;
            }
            if (last == "spawn" || last == "spawn_scoped") && next == Some(b'(') {
                if let Some(close) = self.match_close(next_at) {
                    out.push(Tok {
                        pos: start,
                        kind: Kind::Spawn {
                            open: next_at,
                            close,
                        },
                    });
                }
                continue;
            }
            let called = next == Some(b'(')
                || (next == Some(b'!')
                    && matches!(
                        self.b.get(self.skip_ws(next_at + 1)),
                        Some(b'(' | b'[' | b'{')
                    ))
                || (next == Some(b'.') && {
                    let method = self.skip_ws(next_at + 1);
                    let end = self.word_end(method);
                    end > method && self.b.get(self.skip_ws(end)) == Some(&b'(')
                });
            if !after_dot && called && is_api(&segs, next) {
                let name = if next == Some(b'!') {
                    format!("{chain}!")
                } else {
                    chain.to_string()
                };
                out.push(Tok {
                    pos: start,
                    kind: Kind::Api(name),
                });
                continue;
            }
            let local = if segs.len() == 1 {
                !after_dot || self.is_self_receiver(prev_end)
            } else {
                segs.len() == 2 && matches!(segs[0], "Self" | "self" | "super")
            };
            if next == Some(b'(') && local {
                out.push(Tok {
                    pos: start,
                    kind: Kind::Call(last.to_string()),
                });
            }
        }
        out
    }
}

/// Walks an attribute prefix tracking `ident(` nesting; true when `not(` is open at its end.
fn negated_in_attr(segment: &str) -> bool {
    let b = segment.as_bytes();
    let mut stack: Vec<&str> = Vec::new();
    for (i, byte) in b.iter().enumerate() {
        if *byte == b'(' {
            let mut s = i;
            while s > 0 && is_word(b[s - 1]) {
                s -= 1;
            }
            stack.push(segment.get(s..i).unwrap_or(""));
        } else if *byte == b')' {
            stack.pop();
        }
    }
    stack.contains(&"not")
}

fn prefixed(segment: &str, prefix: &str) -> bool {
    segment
        .strip_prefix(prefix)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

fn starts_lower(segment: &str) -> bool {
    segment
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
}

/// Is this path a call into AppKit, Accessibility or CoreFoundation?
fn is_api(segs: &[&str], next: Option<u8>) -> bool {
    let first = segs[0];
    let last = segs[segs.len() - 1];
    if segs.len() > 1
        && matches!(
            first,
            "objc2_app_kit"
                | "objc2_application_services"
                | "objc2_core_foundation"
                | "core_foundation"
                | "core_foundation_sys"
                | "accessibility_sys"
        )
    {
        return true;
    }
    if first == "objc2_foundation" && segs.len() > 1 && prefixed(segs[1], "NS") {
        return true;
    }
    if segs.len() == 1 && matches!(first, "msg_send" | "msg_send_id") && next == Some(b'!') {
        return true;
    }
    let call_shape =
        (segs.len() == 1 && next == Some(b'(')) || (segs.len() > 1 && starts_lower(last));
    call_shape && (prefixed(first, "NS") || prefixed(first, "AX") || prefixed(first, "CF"))
}

struct Analysis<'a> {
    src: Src<'a>,
    toks: Vec<Tok>,
    fns: HashMap<String, Vec<(usize, usize)>>,
}

impl Analysis<'_> {
    /// Ranges to search for loops: the root and the same-file helpers it calls.
    fn reachable(&self, root: (usize, usize)) -> Vec<(usize, usize)> {
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        seen.insert(root);
        let mut order = vec![root];
        let mut frontier = vec![root];
        for _ in 0..MAX_HELPER_DEPTH {
            let mut next_frontier: Vec<(usize, usize)> = Vec::new();
            for (lo, hi) in &frontier {
                for tok in &self.toks {
                    if tok.pos < *lo || tok.pos >= *hi {
                        continue;
                    }
                    if let Kind::Call(name) = &tok.kind {
                        if let Some(bodies) = self.fns.get(name) {
                            for body in bodies {
                                if seen.insert(*body) {
                                    order.push(*body);
                                    next_frontier.push(*body);
                                }
                            }
                        }
                    }
                }
            }
            frontier = next_frontier;
        }
        order
    }

    /// The first API called in `lo..hi` (directly or through same-file helpers) that no
    /// `autoreleasepool(..)` lying inside that range wraps.
    fn uncovered(
        &self,
        lo: usize,
        hi: usize,
        depth: usize,
        visited: &mut HashSet<(usize, usize)>,
    ) -> Option<String> {
        let pools: Vec<(usize, usize)> = self
            .toks
            .iter()
            .filter_map(|t| match &t.kind {
                Kind::Pool { open, close } if t.pos >= lo && *close <= hi => Some((*open, *close)),
                _ => None,
            })
            .collect();
        let covered = |p: usize| pools.iter().any(|(o, c)| p > *o && p < *c);
        for tok in &self.toks {
            if tok.pos < lo || tok.pos >= hi {
                continue;
            }
            match &tok.kind {
                Kind::Api(name) if !covered(tok.pos) => return Some(name.clone()),
                Kind::Call(name) if depth < MAX_HELPER_DEPTH && !covered(tok.pos) => {
                    if let Some(bodies) = self.fns.get(name) {
                        for body in bodies {
                            if visited.insert(*body) {
                                if let Some(api) =
                                    self.uncovered(body.0, body.1, depth + 1, visited)
                                {
                                    return Some(api);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Body ranges a spawn call runs: its closure, or same-file fns named as arguments.
    fn spawn_roots(&self, open: usize, close: usize) -> Vec<(usize, usize)> {
        if let Some(range) = self.src.closure_body(open, close) {
            return vec![range];
        }
        let mut roots: Vec<(usize, usize)> = Vec::new();
        let args = self.src.text.get(open + 1..close).unwrap_or("");
        for arg in args.split(',') {
            let name = arg.trim();
            if name.is_empty() || !name.bytes().all(|c| is_word(c) || c == b':') {
                continue;
            }
            let last = name.rsplit("::").next().unwrap_or(name);
            if let Some(bodies) = self.fns.get(last) {
                roots.extend(bodies.iter().copied());
            }
        }
        roots
    }

    /// Is `pos` inside a spawn call other than the one opened at `own_open`, within `lo..hi`?
    fn in_nested_spawn(&self, pos: usize, lo: usize, hi: usize, own_open: usize) -> bool {
        self.toks.iter().any(|t| match &t.kind {
            Kind::Spawn { open, close } => {
                t.pos >= lo && t.pos < hi && *open != own_open && *open < pos && pos < *close
            }
            _ => false,
        })
    }

    fn line_of(&self, pos: usize) -> usize {
        1 + self.src.b[..pos].iter().filter(|c| **c == b'\n').count()
    }

    fn findings(&self, package: &str) -> Vec<Map<String, Value>> {
        let mut out: Vec<Map<String, Value>> = Vec::new();
        let mut ordinals: HashMap<String, u64> = HashMap::new();
        for tok in &self.toks {
            let Kind::Spawn { open, close } = &tok.kind else {
                continue;
            };
            let mut hit: Option<(usize, String)> = None;
            'search: for root in self.spawn_roots(*open, *close) {
                for (lo, hi) in self.reachable(root) {
                    for lp in &self.toks {
                        let Kind::Loop { close: body_close } = &lp.kind else {
                            continue;
                        };
                        if lp.pos < lo
                            || lp.pos >= hi
                            || self.in_nested_spawn(lp.pos, lo, hi, *open)
                        {
                            continue;
                        }
                        let mut visited: HashSet<(usize, usize)> = HashSet::new();
                        if let Some(api) = self.uncovered(lp.pos, *body_close, 0, &mut visited) {
                            hit = Some((lp.pos, api));
                            break 'search;
                        }
                    }
                }
            }
            let Some((loop_pos, api)) = hit else {
                continue;
            };
            let line = self.line_of(tok.pos);
            let line_text = self
                .src
                .text
                .lines()
                .nth(line - 1)
                .unwrap_or("")
                .to_string();
            let identity = format!("{FINGERPRINT}\0{line_text}");
            let ordinal = *ordinals.get(&identity).unwrap_or(&0);
            ordinals.insert(identity, ordinal + 1);
            let mut finding = Map::new();
            finding.insert("fingerprint".into(), json!(FINGERPRINT));
            finding.insert("capability".into(), json!(CAPABILITY));
            finding.insert("package".into(), json!(package));
            finding.insert("tier".into(), json!("block"));
            finding.insert("kind".into(), json!("source"));
            finding.insert("line".into(), json!(line));
            finding.insert("lineText".into(), json!(line_text));
            finding.insert("ordinal".into(), json!(ordinal));
            finding.insert(
                "text".into(),
                json!(format!(
                    "thread spawned here loops (line {}) calling {api} with no autoreleasepool inside the loop body: {FIX}",
                    self.line_of(loop_pos)
                )),
            );
            out.push(finding);
        }
        out
    }
}

/// Findings for one file. `package` is the catalog's owner of [`CAPABILITY`]
/// ([`declared_package`]); the caller adds `file`.
pub fn findings(rel: &str, bytes: &[u8], package: &str) -> Vec<Map<String, Value>> {
    if extname(rel) != ".rs" {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(bytes);
    if !text.contains("spawn") {
        return Vec::new();
    }
    let src = Src::new(&text);
    if !(path_is_macos(rel) || src.names_macos_cfg()) {
        return Vec::new();
    }
    let toks = src.tokenize();
    let mut fns: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for tok in &toks {
        if let Kind::FnDef { name, open, close } = &tok.kind {
            fns.entry(name.clone()).or_default().push((*open, *close));
        }
    }
    Analysis { src, toks, fns }.findings(package)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSITIVE: &str =
        include_str!("../tests/fixtures/appkit-autorelease/positive_frontmost_poll.rs.txt");
    const POOL_IN_LOOP: &str =
        include_str!("../tests/fixtures/appkit-autorelease/negative_pool_in_loop.rs.txt");
    const POOL_WRAPS_BODY: &str =
        include_str!("../tests/fixtures/appkit-autorelease/negative_pool_wraps_body.rs.txt");
    const AX_POLL: &str =
        include_str!("../tests/fixtures/appkit-autorelease/positive_ax_poll.rs.txt");

    fn run(rel: &str, text: &str) -> Vec<Map<String, Value>> {
        findings(rel, text.as_bytes(), PACKAGE)
    }

    #[test]
    fn predicates_types_and_comment_separated_spawns() {
        for (text, expected) in [
            (
                include_str!(
                    "../tests/fixtures/appkit-autorelease/positive_while_condition.rs.txt"
                ),
                1,
            ),
            (
                include_str!("../tests/fixtures/appkit-autorelease/negative_type_only.rs.txt"),
                0,
            ),
            (
                include_str!("../tests/fixtures/appkit-autorelease/positive_comment_spawn.rs.txt"),
                1,
            ),
        ] {
            assert_eq!(run("src/macos/poll.rs", text).len(), expected, "{text}");
        }
    }

    fn line_containing(text: &str, needle: &str) -> usize {
        text.lines().position(|l| l.contains(needle)).unwrap() + 1
    }

    #[test]
    fn frontmost_poll_without_pool_fails_at_the_spawn_line() {
        let found = run("src/macos/frontmost.rs", POSITIVE);
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f["fingerprint"], FINGERPRINT);
        assert_eq!(f["capability"], CAPABILITY);
        assert_eq!(f["package"], PACKAGE);
        assert_eq!(f["tier"], "block");
        assert_eq!(f["kind"], "source");
        assert_eq!(f["line"], line_containing(POSITIVE, ".spawn(move"));
        let text = f["text"].as_str().unwrap();
        assert!(text.contains("NSWorkspace::sharedWorkspace"), "{text}");
        assert!(text.contains(FIX), "{text}");
    }

    #[test]
    fn finding_is_a_valid_source_occurrence() {
        let mut f = Value::Object(run("src/macos/frontmost.rs", POSITIVE).remove(0));
        f["file"] = json!("src/macos/frontmost.rs");
        assert!(crate::approval::occurrence_hash(&f).is_ok());
    }

    #[test]
    fn pool_inside_the_loop_or_around_its_body_is_clean() {
        assert!(run("src/macos/frontmost.rs", POOL_IN_LOOP).is_empty());
        assert!(run("src/macos/frontmost.rs", POOL_WRAPS_BODY).is_empty());
    }

    #[test]
    fn same_code_outside_macos_scope_is_clean() {
        assert!(run("src/platform/poll.rs", POSITIVE).is_empty());
        assert!(run("src/platform/poll.rs", AX_POLL).is_empty());
    }

    #[test]
    fn ax_polling_loop_without_pool_fails() {
        let found = run("src/macos/observation.rs", AX_POLL);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0]["line"], line_containing(AX_POLL, "thread::spawn"));
        assert!(found[0]["text"]
            .as_str()
            .unwrap()
            .contains("AXUIElementCreateSystemWide"));
    }

    #[test]
    fn macos_scope_follows_path_and_cfg() {
        for rel in [
            "crates/x/src/macos/focus/mod.rs",
            "src/macos_focus_continuity.rs",
            "src/focus_macos.rs",
            "src/macos.rs",
        ] {
            assert!(path_is_macos(rel), "{rel}");
            assert_eq!(run(rel, POSITIVE).len(), 1, "{rel}");
        }
        assert!(!path_is_macos("src/windows/poll.rs"));
        let gated = format!("#![cfg(target_os = \"macos\")]\n{POSITIVE}");
        assert_eq!(run("src/poll.rs", &gated).len(), 1);
        for trivia in [
            " ",
            "/* platform */",
            "/* outer /* nested */ end */",
            "// platform\n",
        ] {
            let gated = format!("#![cfg(target_os ={trivia}\"macos\")]\n{POSITIVE}");
            assert_eq!(run("src/poll.rs", &gated).len(), 1, "{trivia}");
            let other = gated.replace("\"macos\"", "\"windows\"");
            assert!(run("src/poll.rs", &other).is_empty(), "{trivia}");
        }
        let item_gated = format!("#[cfg(all(unix, target_os = \"macos\"))]\n{POSITIVE}");
        assert_eq!(run("src/poll.rs", &item_gated).len(), 1);
        let negated = format!("#[cfg(not(target_os = \"macos\"))]\n{POSITIVE}");
        assert!(run("src/poll.rs", &negated).is_empty());
        let other_os = format!("#[cfg(target_os = \"windows\")]\n{POSITIVE}");
        assert!(run("src/poll.rs", &other_os).is_empty());
        let mention = format!(
            "// target_os = \"macos\"\nconst S: &str = \"target_os = \\\"macos\\\"\";\n{POSITIVE}"
        );
        assert!(run("src/poll.rs", &mention).is_empty());
    }

    #[test]
    fn a_pool_around_the_whole_loop_never_drains_and_still_fails() {
        let src = r#"
fn start() {
    std::thread::spawn(move || {
        objc2::rc::autoreleasepool(|_| {
            loop {
                let app = NSWorkspace::sharedWorkspace().frontmostApplication();
                sleep();
            }
        });
    });
}
"#;
        assert_eq!(run("src/macos/a.rs", src).len(), 1);
    }

    #[test]
    fn a_pool_that_misses_one_call_still_fails() {
        let src = r#"
fn start() {
    std::thread::spawn(|| {
        loop {
            autoreleasepool(|_| { NSWorkspace::sharedWorkspace(); });
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(id);
        }
    });
}
"#;
        let found = run("src/macos/a.rs", src);
        assert_eq!(found.len(), 1);
        assert!(found[0]["text"]
            .as_str()
            .unwrap()
            .contains("NSRunningApplication::runningApplicationsWithBundleIdentifier"));
    }

    #[test]
    fn spawn_forms_are_recognised() {
        let body = "|| { loop { let a = NSWorkspace::sharedWorkspace(); } }";
        for call in [
            format!("thread::spawn({body});"),
            format!("std::thread::spawn(move {body});"),
            format!("std::thread::Builder::new().name(\"n\".into()).spawn({body}).unwrap();"),
            format!("builder.spawn_scoped(scope, {body}).unwrap();"),
            format!("scope.spawn({body});"),
        ] {
            let src = format!("fn f() {{\n    {call}\n}}\n");
            assert_eq!(run("src/macos/a.rs", &src).len(), 1, "{call}");
        }
    }

    #[test]
    fn spawn_of_a_named_fn_and_same_file_helpers_are_followed() {
        let named = r#"
fn start() { std::thread::spawn(poll_loop); }
fn poll_loop() {
    loop { let a = NSWorkspace::sharedWorkspace(); }
}
"#;
        assert_eq!(run("src/macos/a.rs", named).len(), 1);
        let helper = r#"
fn start() {
    std::thread::spawn(move || loop {
        snapshot();
    });
}
fn snapshot() -> Option<String> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication();
    None
}
"#;
        assert_eq!(run("src/macos/a.rs", helper).len(), 1);
        let pooled_helper = helper.replace(
            "    let app = NSWorkspace::sharedWorkspace().frontmostApplication();\n    None",
            "    autoreleasepool(|_| { NSWorkspace::sharedWorkspace().frontmostApplication(); None })",
        );
        assert!(run("src/macos/a.rs", &pooled_helper).is_empty());
        let pooled_call = r#"
fn start() {
    std::thread::spawn(move || loop {
        autoreleasepool(|_| snapshot());
    });
}
fn snapshot() { NSWorkspace::sharedWorkspace(); }
"#;
        assert!(run("src/macos/a.rs", pooled_call).is_empty());
    }

    #[test]
    fn while_loops_and_core_foundation_calls_count() {
        let src = r#"
fn start(flag: Flag) {
    thread::spawn(move || {
        while flag.on() {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, 0);
        }
    });
}
"#;
        let found = run("src/macos/a.rs", src);
        assert_eq!(found.len(), 1);
        assert!(found[0]["text"]
            .as_str()
            .unwrap()
            .contains("CFRunLoopRunInMode"));
    }

    #[test]
    fn msg_send_and_foundation_paths_count() {
        let msg = "fn s() { thread::spawn(|| { loop { let v: i32 = unsafe { msg_send![obj, count] }; } }); }";
        assert_eq!(run("src/macos/a.rs", msg).len(), 1);
        let foundation = "fn s() { thread::spawn(|| { loop { let s = objc2_foundation::NSString::from_str(\"x\"); } }); }";
        assert_eq!(run("src/macos/a.rs", foundation).len(), 1);
    }

    #[test]
    fn non_appkit_loops_bounded_spawns_and_text_mentions_are_clean() {
        let plain = "fn s() { thread::spawn(|| { loop { work(); sleep(); } }); }";
        assert!(run("src/macos/a.rs", plain).is_empty());
        let no_loop = "fn s() { thread::spawn(|| { let a = NSWorkspace::sharedWorkspace(); }); }";
        assert!(run("src/macos/a.rs", no_loop).is_empty());
        let outside =
            "fn s() { thread::spawn(|| { work(); }); loop { NSWorkspace::sharedWorkspace(); } }";
        assert!(run("src/macos/a.rs", outside).is_empty());
        let mentions = "fn s() { thread::spawn(|| { loop { // NSWorkspace::sharedWorkspace()\n let s = \"NSWorkspace::sharedWorkspace()\"; } }); }";
        assert!(run("src/macos/a.rs", mentions).is_empty());
        let method_only =
            "fn s() { thread::spawn(|| { loop { thing.CFRunLoopRun(); thing.NSWorkspace(); } }); }";
        assert!(run("src/macos/a.rs", method_only).is_empty());
        assert!(findings("src/macos/a.swift", POSITIVE.as_bytes(), PACKAGE).is_empty());
    }

    #[test]
    fn nested_spawns_report_once_and_repeated_lines_get_ordinals() {
        let nested = "fn s() { thread::spawn(|| { thread::spawn(|| { loop { NSWorkspace::sharedWorkspace(); } }); }); }";
        assert_eq!(run("src/macos/a.rs", nested).len(), 1);
        let twice = "fn a() {\n    thread::spawn(|| loop { NSWorkspace::sharedWorkspace(); });\n}\nfn b() {\n    thread::spawn(|| loop { NSWorkspace::sharedWorkspace(); });\n}\n";
        let found = run("src/macos/a.rs", twice);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["ordinal"], 0);
        assert_eq!(found[1]["ordinal"], 1);
    }

    #[test]
    fn the_embedded_catalog_declares_the_capability() {
        assert_eq!(
            declared_package(crate::catalog_value()).as_deref(),
            Some(PACKAGE)
        );
        assert_eq!(declared_package(&json!({"capabilities": []})), None);
    }
}
