//! TypeScript-subset type stripping for `run_program`.
//!
//! QuickJS runs JavaScript only. The model is shown a `.d.ts`, so it writes
//! TypeScript; this module erases the type syntax and leaves plain JavaScript.
//! Erased spans are replaced with spaces (newlines kept), so line and column
//! numbers in runtime errors still match what the model wrote.
//!
//! Supported subset (anything else is passed through and surfaces as a typed
//! syntax error from the runtime, never as a silent misparse):
//!   - `: Type` on `const`/`let`/`var` declarators, function and arrow
//!     parameters, `catch (e: T)`, and function / arrow return types
//!   - optional parameters (`x?: T`)
//!   - `expr as Type`, `expr satisfies Type`, non-null `expr!`
//!   - `interface X { .. }` and `type X = ..;` declarations (optionally
//!     prefixed by `export` / `declare`)
//!   - generic parameters on `function` declarations and explicit type
//!     arguments on calls / `new` (`new Map<string, number>()`)
//! Not supported: enums, namespaces, classes with modifiers, decorators,
//! conditional types, arrow generics (`<T>(x) =>`).
//!
//! This is a scanner, not a parser: it only needs to find where a type starts
//! and where it ends, which is much smaller than the TypeScript grammar.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Ident,
    Num,
    Str,
    Tmpl,
    Regex,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
}

const MULTI_PUNCT: [&str; 25] = [
    "...", "===", "!==", "**=", "&&=", "||=", "??=", "=>", "==", "!=", "&&", "||", "??", "?.",
    "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "**",
];

const REGEX_PREFIX_KEYWORDS: [&str; 14] = [
    "return",
    "typeof",
    "case",
    "do",
    "else",
    "in",
    "of",
    "instanceof",
    "new",
    "delete",
    "void",
    "throw",
    "yield",
    "await",
];

const NON_EXPRESSION_WORDS: [&str; 24] = [
    "return",
    "typeof",
    "case",
    "do",
    "else",
    "in",
    "of",
    "instanceof",
    "new",
    "delete",
    "void",
    "throw",
    "yield",
    "await",
    "const",
    "let",
    "var",
    "function",
    "if",
    "while",
    "for",
    "switch",
    "import",
    "export",
];

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn is_ident_part(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

fn skip_quoted(bytes: &[u8], mut i: usize, quote: u8) -> Result<usize, String> {
    i += 1;
    loop {
        if i >= bytes.len() {
            return Err("unterminated string literal".to_string());
        }
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return Ok(i + 1);
        }
        i += 1;
    }
}

fn skip_template(bytes: &[u8], mut i: usize) -> Result<usize, String> {
    i += 1;
    loop {
        if i >= bytes.len() {
            return Err("unterminated template literal".to_string());
        }
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => return Ok(i + 1),
            b'$' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                i += 2;
                let mut depth = 1usize;
                while depth > 0 {
                    if i >= bytes.len() {
                        return Err("unterminated template expression".to_string());
                    }
                    match bytes[i] {
                        b'{' => {
                            depth += 1;
                            i += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            i += 1;
                        }
                        b'"' | b'\'' => i = skip_quoted(bytes, i, bytes[i])?,
                        b'`' => i = skip_template(bytes, i)?,
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let bytes = src.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    let mut out: Vec<Tok> = Vec::new();
    while i < n {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            match src[i + 2..].find("*/") {
                Some(end) => i = i + 2 + end + 2,
                None => return Err("unterminated block comment".to_string()),
            }
            continue;
        }
        let start = i;
        if c == b'"' || c == b'\'' {
            i = skip_quoted(bytes, i, c)?;
            out.push(Tok {
                kind: Kind::Str,
                start,
                end: i,
            });
            continue;
        }
        if c == b'`' {
            i = skip_template(bytes, i)?;
            out.push(Tok {
                kind: Kind::Tmpl,
                start,
                end: i,
            });
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && i + 1 < n && bytes[i + 1].is_ascii_digit()) {
            i += 1;
            while i < n
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'.'
                    || bytes[i] == b'_'
                    || ((bytes[i] == b'+' || bytes[i] == b'-')
                        && matches!(bytes[i - 1], b'e' | b'E')
                        && !src[start..i].starts_with("0x")))
            {
                i += 1;
            }
            out.push(Tok {
                kind: Kind::Num,
                start,
                end: i,
            });
            continue;
        }
        if is_ident_start(c) {
            i += 1;
            while i < n && is_ident_part(bytes[i]) {
                i += 1;
            }
            out.push(Tok {
                kind: Kind::Ident,
                start,
                end: i,
            });
            continue;
        }
        if c == b'/' {
            let regex_allowed = match out.last() {
                None => true,
                Some(prev) => match prev.kind {
                    Kind::Punct => !matches!(&src[prev.start..prev.end], ")" | "]" | "}"),
                    Kind::Ident => REGEX_PREFIX_KEYWORDS.contains(&&src[prev.start..prev.end]),
                    _ => false,
                },
            };
            if regex_allowed {
                i += 1;
                let mut in_class = false;
                loop {
                    if i >= n || bytes[i] == b'\n' {
                        return Err("unterminated regular expression".to_string());
                    }
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'[' => {
                            in_class = true;
                            i += 1;
                        }
                        b']' => {
                            in_class = false;
                            i += 1;
                        }
                        b'/' if !in_class => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                while i < n && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push(Tok {
                    kind: Kind::Regex,
                    start,
                    end: i,
                });
                continue;
            }
        }
        let rest = &src[i..];
        let mut matched = 1usize;
        for op in MULTI_PUNCT {
            if rest.starts_with(op) {
                // `?.` followed by a digit is a ternary, not optional chaining.
                if op == "?." && rest.len() > 2 && rest.as_bytes()[2].is_ascii_digit() {
                    continue;
                }
                matched = op.len();
                break;
            }
        }
        i += matched;
        out.push(Tok {
            kind: Kind::Punct,
            start,
            end: i,
        });
    }
    Ok(out)
}

struct Stripper<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    ranges: Vec<(usize, usize)>,
}

impl<'a> Stripper<'a> {
    fn text(&self, i: usize) -> &str {
        if i < self.toks.len() {
            &self.src[self.toks[i].start..self.toks[i].end]
        } else {
            ""
        }
    }

    fn is(&self, i: usize, want: &str) -> bool {
        i < self.toks.len()
            && matches!(self.toks[i].kind, Kind::Punct | Kind::Ident)
            && self.text(i) == want
    }

    fn is_kind(&self, i: usize, kind: Kind) -> bool {
        i < self.toks.len() && self.toks[i].kind == kind
    }

    fn newline_between(&self, a: usize, b: usize) -> bool {
        if b >= self.toks.len() || b == 0 {
            return false;
        }
        let from = self.toks[a].end;
        let to = self.toks[b].start;
        from <= to && self.src[from..to].contains('\n')
    }

    fn strip(&mut self, from: usize, to: usize) {
        if from < to {
            self.ranges.push((from, to));
        }
    }

    /// Index of the bracket matching the opener at `open`.
    fn matching(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        while i < self.toks.len() {
            if self.toks[i].kind == Kind::Punct {
                match self.text(i) {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        None
    }

    /// Index of the `>` matching the `<` at `open` (bracketed groups skipped).
    fn matching_angle(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        let limit = (open + 400).min(self.toks.len());
        while i < limit {
            if self.toks[i].kind == Kind::Punct {
                match self.text(i) {
                    "<" => depth += 1,
                    ">" => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i);
                        }
                    }
                    "(" | "[" | "{" => {
                        i = self.matching(i)?;
                    }
                    ";" | "&&" | "||" | ")" | "]" | "}" => return None,
                    _ => {}
                }
            }
            i += 1;
        }
        None
    }

    fn skip_type_atom(&self, mut i: usize) -> Option<usize> {
        if i >= self.toks.len() {
            return None;
        }
        while matches!(
            self.text(i),
            "keyof" | "typeof" | "readonly" | "unique" | "infer" | "asserts"
        ) && self.toks[i].kind == Kind::Ident
            && (self.is_kind(i + 1, Kind::Ident)
                || self.is(i + 1, "(")
                || self.is(i + 1, "[")
                || self.is(i + 1, "{"))
        {
            i += 1;
        }
        match self.toks[i].kind {
            Kind::Ident => {
                i += 1;
                while self.is(i, ".") && self.is_kind(i + 1, Kind::Ident) {
                    i += 2;
                }
                if self.is(i, "<") {
                    let close = self.matching_angle(i)?;
                    i = close + 1;
                }
                Some(i)
            }
            Kind::Str | Kind::Num | Kind::Tmpl => Some(i + 1),
            Kind::Punct => match self.text(i) {
                "{" | "[" => Some(self.matching(i)? + 1),
                "(" => {
                    let close = self.matching(i)?;
                    let mut j = close + 1;
                    if self.is(j, "=>") {
                        j = self.skip_type(j + 1)?;
                    }
                    Some(j)
                }
                "-" if self.is_kind(i + 1, Kind::Num) => Some(i + 2),
                _ => None,
            },
            Kind::Regex => None,
        }
    }

    /// Index just past the type starting at `i`.
    fn skip_type(&self, mut i: usize) -> Option<usize> {
        if self.is(i, "|") || self.is(i, "&") {
            i += 1;
        }
        loop {
            i = self.skip_type_atom(i)?;
            while self.is(i, "[") && !self.newline_between(i - 1, i) {
                i = self.matching(i)? + 1;
            }
            if self.is(i, "|") || self.is(i, "&") {
                i += 1;
                continue;
            }
            return Some(i);
        }
    }

    /// Parses a parameter list whose `(` is at `open`; records the erased
    /// annotations and returns the index just past the `)`.
    fn param_list(&mut self, open: usize) -> Option<usize> {
        let close = self.matching(open)?;
        let mut pending: Vec<(usize, usize)> = Vec::new();
        let mut k = open + 1;
        while k < close {
            if self.is(k, "...") {
                k += 1;
            }
            if self.is(k, "{") || self.is(k, "[") {
                k = self.matching(k)? + 1;
            } else if self.is_kind(k, Kind::Ident) {
                k += 1;
            } else {
                return None;
            }
            if self.is(k, "?") && (self.is(k + 1, ":") || self.is(k + 1, ",") || k + 1 == close) {
                pending.push((k, k + 1));
                k += 1;
            }
            if self.is(k, ":") {
                let end = self.skip_type(k + 1)?;
                pending.push((k, end));
                k = end;
            }
            if self.is(k, "=") {
                k += 1;
                // Default value: skip to the next top-level comma.
                while k < close && !self.is(k, ",") {
                    if self.is(k, "(") || self.is(k, "[") || self.is(k, "{") {
                        k = self.matching(k)?;
                    }
                    k += 1;
                }
            }
            if self.is(k, ",") {
                k += 1;
            } else if k != close {
                return None;
            }
        }
        for (from, to) in pending {
            self.strip(from, to);
        }
        Some(close + 1)
    }

    /// One `const`/`let`/`var` declarator starting at `k` (pattern plus an
    /// optional annotation). Returns the index after the annotation.
    fn declarator(&mut self, mut k: usize) -> usize {
        if self.is(k, "{") || self.is(k, "[") {
            match self.matching(k) {
                Some(close) => k = close + 1,
                None => return k,
            }
        } else if self.is_kind(k, Kind::Ident) {
            k += 1;
        } else {
            return k;
        }
        if self.is(k, "!") && self.is(k + 1, ":") {
            self.strip(k, k + 1);
            k += 1;
        }
        if self.is(k, ":") {
            if let Some(end) = self.skip_type(k + 1) {
                self.strip(k, end);
                k = end;
            }
        }
        k
    }

    fn statement_end(&self, start: usize) -> usize {
        let mut depth = 0i32;
        let mut i = start;
        while i < self.toks.len() {
            if i > start && depth == 0 && self.newline_between(i - 1, i) {
                return i;
            }
            if self.toks[i].kind == Kind::Punct {
                match self.text(i) {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    ";" if depth == 0 => return i + 1,
                    _ => {}
                }
            }
            i += 1;
        }
        self.toks.len()
    }

    fn prev_is_expression_end(&self, i: usize) -> bool {
        if i == 0 {
            return false;
        }
        let prev = i - 1;
        match self.toks[prev].kind {
            Kind::Ident => !NON_EXPRESSION_WORDS.contains(&self.text(prev)),
            Kind::Num | Kind::Str | Kind::Tmpl | Kind::Regex => true,
            Kind::Punct => matches!(self.text(prev), ")" | "]" | "}"),
        }
    }

    fn run(&mut self) {
        let mut depth = 0i32;
        let mut decls: Vec<i32> = Vec::new();
        let mut i = 0usize;
        while i < self.toks.len() {
            let tok_kind = self.toks[i].kind;
            let word = self.text(i).to_string();
            if tok_kind == Kind::Ident {
                match word.as_str() {
                    "export" | "declare"
                        if matches!(
                            self.text(i + 1),
                            "interface" | "type" | "const" | "let" | "var" | "function" | "async"
                        ) =>
                    {
                        if word == "declare"
                            && matches!(self.text(i + 1), "const" | "let" | "var" | "function")
                        {
                            let end = self.statement_end(i);
                            self.strip(i, end);
                            i = end;
                            continue;
                        }
                        self.strip(i, i + 1);
                        i += 1;
                        continue;
                    }
                    "interface"
                        if self.is_kind(i + 1, Kind::Ident)
                            && (self.is(i + 2, "{")
                                || self.is(i + 2, "<")
                                || self.is(i + 2, "extends")) =>
                    {
                        let mut j = i + 2;
                        while j < self.toks.len() && !self.is(j, "{") {
                            j += 1;
                        }
                        if let Some(close) = self.matching(j) {
                            self.strip(i, close + 1);
                            i = close + 1;
                            continue;
                        }
                    }
                    "type"
                        if self.is_kind(i + 1, Kind::Ident)
                            && (self.is(i + 2, "=") || self.is(i + 2, "<")) =>
                    {
                        let mut j = i + 2;
                        if self.is(j, "<") {
                            match self.matching_angle(j) {
                                Some(close) => j = close + 1,
                                None => {
                                    i += 1;
                                    continue;
                                }
                            }
                        }
                        if self.is(j, "=") {
                            if let Some(mut end) = self.skip_type(j + 1) {
                                if self.is(end, ";") {
                                    end += 1;
                                }
                                self.strip(i, end);
                                i = end;
                                continue;
                            }
                        }
                    }
                    "const" | "let" | "var" => {
                        let after = self.declarator(i + 1);
                        decls.push(depth);
                        i = after;
                        continue;
                    }
                    "function" => {
                        let mut k = i + 1;
                        if self.is(k, "*") {
                            k += 1;
                        }
                        if self.is_kind(k, Kind::Ident) && !self.is(k, "(") {
                            k += 1;
                        }
                        if self.is(k, "<") {
                            if let Some(close) = self.matching_angle(k) {
                                self.strip(k, close + 1);
                                k = close + 1;
                            }
                        }
                        if self.is(k, "(") {
                            if let Some(after) = self.param_list(k) {
                                k = after;
                                if self.is(k, ":") {
                                    if let Some(end) = self.skip_type(k + 1) {
                                        self.strip(k, end);
                                        k = end;
                                    }
                                }
                                i = k;
                                continue;
                            }
                        }
                    }
                    "catch" if self.is(i + 1, "(") => {
                        if let Some(after) = self.param_list(i + 1) {
                            i = after;
                            continue;
                        }
                    }
                    "as" | "satisfies" if self.prev_is_expression_end(i) => {
                        if let Some(end) = self.skip_type(i + 1) {
                            self.strip(i, end);
                            i = end;
                            continue;
                        }
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            if tok_kind == Kind::Punct {
                match word.as_str() {
                    "(" => {
                        if let Some(close) = self.matching(i) {
                            let after = close + 1;
                            let arrow = self.is(after, "=>")
                                || (self.is(after, ":")
                                    && self
                                        .skip_type(after + 1)
                                        .is_some_and(|end| self.is(end, "=>")));
                            if arrow {
                                if let Some(mut k) = self.param_list(i) {
                                    if self.is(k, ":") {
                                        if let Some(end) = self.skip_type(k + 1) {
                                            self.strip(k, end);
                                            k = end;
                                        }
                                    }
                                    i = k;
                                    continue;
                                }
                            }
                        }
                        depth += 1;
                    }
                    "[" | "{" => depth += 1,
                    ")" | "]" | "}" => {
                        depth -= 1;
                        while decls.last().is_some_and(|d| *d > depth) {
                            decls.pop();
                        }
                    }
                    ";" => {
                        while decls.last().is_some_and(|d| *d >= depth) {
                            decls.pop();
                        }
                    }
                    "," if decls.last() == Some(&depth) => {
                        let after = self.declarator(i + 1);
                        i = after.max(i + 1);
                        continue;
                    }
                    "!" if self.prev_is_expression_end(i)
                        && !self.newline_between(i - 1, i)
                        && (i + 1 >= self.toks.len()
                            || matches!(
                                self.text(i + 1),
                                "." | "[" | ")" | "," | ";" | "]" | "}" | "?."
                            )) =>
                    {
                        self.strip(i, i + 1);
                    }
                    "<" if i > 0
                        && self.toks[i - 1].kind == Kind::Ident
                        && !NON_EXPRESSION_WORDS.contains(&self.text(i - 1)) =>
                    {
                        if let Some(close) = self.matching_angle(i) {
                            if self.is(close + 1, "(") && self.type_args_look_valid(i + 1, close) {
                                self.strip(i, close + 1);
                                i = close + 1;
                                continue;
                            }
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
    }

    fn type_args_look_valid(&self, from: usize, to: usize) -> bool {
        (from..to).all(|i| match self.toks[i].kind {
            Kind::Ident | Kind::Str | Kind::Num => true,
            Kind::Punct => matches!(
                self.text(i),
                "," | "."
                    | "|"
                    | "&"
                    | "["
                    | "]"
                    | "{"
                    | "}"
                    | ":"
                    | ";"
                    | "("
                    | ")"
                    | "?"
                    | "<"
                    | ">"
                    | "=>"
            ),
            _ => false,
        })
    }

    fn render(&self) -> Result<String, String> {
        let mut out = self.src.as_bytes().to_vec();
        for (from, to) in &self.ranges {
            let start = self.toks[*from].start;
            let end = self.toks[*to - 1].end;
            for byte in &mut out[start..end] {
                if *byte != b'\n' && *byte != b'\r' {
                    *byte = b' ';
                }
            }
        }
        String::from_utf8(out)
            .map_err(|error| format!("type stripping produced invalid text: {error}"))
    }
}

/// Erase TypeScript type syntax from `source`, returning plain JavaScript.
pub fn strip_typescript(source: &str) -> Result<String, String> {
    let toks = lex(source).map_err(|error| format!("TypeScript subset: {error}"))?;
    let mut stripper = Stripper {
        src: source,
        toks,
        ranges: Vec::new(),
    };
    stripper.run();
    stripper.render()
}
