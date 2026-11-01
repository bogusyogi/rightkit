//! What a copied text is, and which copies are secrets: colour, link, code, else text, plus
//! whole-text secret patterns. Pure code, no OS calls and no disk, so every platform gives the
//! same answer on the same text. The secret patterns are written out by hand (no regex crate).

/// What a plain text turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    Text,
    /// A web address or other link: the host (lower case, no `www.`) and the scheme.
    Link {
        host: String,
        scheme: String,
    },
    /// A colour, normalised to `#RRGGBB` or `#RRGGBBAA`.
    Colour {
        hex: String,
    },
    /// Source code, with a best guess at the language when the first line gives one.
    Code {
        language: Option<&'static str>,
    },
}

/// Schemes a link may have, besides the ones the web uses.
const SCHEMES: [&str; 15] = [
    "http", "https", "ftp", "ftps", "mailto", "tel", "file", "ssh", "git", "slack", "zoommtg",
    "vscode", "obsidian", "notion", "spotify",
];

/// The longest link the detection accepts.
const LINK_MAX: usize = 2048;

/// Runs the rules in order on `text`: colour, link, code, else text.
pub fn detect(text: &str) -> Detected {
    let trimmed = text.trim();
    if let Some(hex) = colour(trimmed) {
        return Detected::Colour { hex };
    }
    if let Some((host, scheme)) = link(trimmed) {
        return Detected::Link { host, scheme };
    }
    if looks_like_code(text) {
        return Detected::Code {
            language: language_hint(text),
        };
    }
    Detected::Text
}

// ---- colour ------------------------------------------------------------------------------

/// The colour `text` is, as `#RRGGBB` (or `#RRGGBBAA`), when it is nothing but a colour.
pub fn colour(text: &str) -> Option<String> {
    if let Some(digits) = text.strip_prefix('#') {
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return match digits.len() {
            3 | 4 => {
                let mut out = String::from("#");
                for c in digits.chars() {
                    out.push(c.to_ascii_uppercase());
                    out.push(c.to_ascii_uppercase());
                }
                Some(out)
            }
            6 | 8 => Some(format!("#{}", digits.to_ascii_uppercase())),
            _ => None,
        };
    }
    // Bare hex is a colour only when it cannot be a number: six digits, one a letter.
    if text.len() == 6
        && text.chars().all(|c| c.is_ascii_hexdigit())
        && text.chars().any(|c| c.is_ascii_alphabetic())
    {
        return Some(format!("#{}", text.to_ascii_uppercase()));
    }
    colour_function(text)
}

/// `rgb(...)`, `rgba(...)`, `hsl(...)` and `hsla(...)`.
fn colour_function(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let open = lower.find('(')?;
    if !lower.ends_with(')') {
        return None;
    }
    let name = lower[..open].trim();
    let inside = &lower[open + 1..lower.len() - 1];
    let args: Vec<&str> = inside.split(',').map(str::trim).collect();
    if args.len() != 3 && args.len() != 4 {
        return None;
    }
    let alpha = match args.get(3) {
        Some(text) => {
            let value = text.parse::<f64>().ok()?;
            if !(0.0..=1.0).contains(&value) {
                return None;
            }
            value
        }
        None => 1.0,
    };
    let (r, g, b) = match name {
        "rgb" | "rgba" => {
            let channel = |text: &str| text.parse::<u32>().ok().filter(|v| *v <= 255);
            (
                channel(args[0])? as u8,
                channel(args[1])? as u8,
                channel(args[2])? as u8,
            )
        }
        "hsl" | "hsla" => {
            let hue = args[0].parse::<f64>().ok()?;
            let percent = |text: &str| {
                let value = text.strip_suffix('%')?.trim().parse::<f64>().ok()?;
                (0.0..=100.0).contains(&value).then_some(value / 100.0)
            };
            hsl_to_rgb(hue, percent(args[1])?, percent(args[2])?)
        }
        _ => return None,
    };
    let mut out = format!("#{r:02X}{g:02X}{b:02X}");
    if alpha < 1.0 {
        out.push_str(&format!("{:02X}", (alpha * 255.0).round() as u8));
    }
    Some(out)
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let shift = lightness - chroma / 2.0;
    let byte = |v: f64| ((v + shift) * 255.0).round().clamp(0.0, 255.0) as u8;
    (byte(r), byte(g), byte(b))
}

// ---- link --------------------------------------------------------------------------------

/// The host and scheme of `text` when it is a single link.
pub fn link(text: &str) -> Option<(String, String)> {
    if text.is_empty() || text.len() > LINK_MAX || text.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("www.") {
        let host = host_of(&lower);
        let bare = host.strip_prefix("www.").unwrap_or(&host);
        return bare
            .contains('.')
            .then(|| (bare.to_string(), "http".to_string()));
    }
    let colon = lower.find(':')?;
    let scheme = &lower[..colon];
    let mut letters = scheme.chars();
    let valid = letters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && letters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid || !SCHEMES.contains(&scheme) {
        return None;
    }
    let rest = &lower[colon + 1..];
    if rest.is_empty() {
        return None;
    }
    let host = match rest.strip_prefix("//") {
        Some(after) => host_of(after),
        None if scheme == "mailto" => rest
            .rsplit_once('@')
            .map(|(_, domain)| domain.split(['?', '#']).next().unwrap_or("").to_string())
            .unwrap_or_default(),
        None => String::new(),
    };
    // A web address needs a real host: one with a dot in it.
    if matches!(scheme, "http" | "https")
        && (rest.strip_prefix("//").is_none() || !host.contains('.'))
    {
        return None;
    }
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    Some((host, scheme.to_string()))
}

/// The host in what follows `//` (or the start of a `www.` address): the part before the
/// first `/`, `?` or `#`, without any user name or port.
fn host_of(text: &str) -> String {
    let authority = text.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.starts_with('[') {
        return authority
            .split_once(']')
            .map_or(authority, |(inside, _)| inside)
            .trim_start_matches('[')
            .to_string();
    }
    authority.split(':').next().unwrap_or("").to_string()
}

// ---- code --------------------------------------------------------------------------------

/// Words that open a line of code.
const KEYWORDS: [&str; 21] = [
    "func", "fn", "def", "class", "struct", "enum", "impl", "import", "from", "const", "let",
    "var", "public", "private", "#include", "using", "package", "return", "if", "for", "while",
];

/// Two or more lines with at least two of the signs of code.
fn looks_like_code(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 2 {
        return false;
    }
    let filled: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    let mut signs = 0;
    // Indented: at least 30 percent of the non-empty lines start with two spaces or a tab.
    let indented = filled
        .iter()
        .filter(|l| l.starts_with("  ") || l.starts_with('\t'))
        .count();
    if !filled.is_empty() && indented * 100 >= filled.len() * 30 {
        signs += 1;
    }
    // Two or more lines that end the way a statement does.
    let endings = lines
        .iter()
        .filter(|l| l.trim_end().ends_with([';', '{', '}', ')']))
        .count();
    if endings >= 2 {
        signs += 1;
    }
    if lines.iter().any(|l| starts_with_keyword(l.trim_start())) {
        signs += 1;
    }
    let joined = text;
    if ["=>", "->", "::", "</", "/>"]
        .iter()
        .any(|mark| joined.contains(mark))
        || (joined.contains("{ ") && joined.contains(" }"))
    {
        signs += 1;
    }
    signs >= 2
}

fn starts_with_keyword(line: &str) -> bool {
    KEYWORDS.iter().any(|word| {
        line.strip_prefix(word).is_some_and(|rest| {
            !rest
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

/// A guess at the language from the first line; cosmetic.
fn language_hint(text: &str) -> Option<&'static str> {
    let first = text.lines().next()?.trim();
    let lower = first.to_ascii_lowercase();
    if lower.starts_with("#!") {
        return Some(if lower.contains("sh") {
            "bash"
        } else {
            "script"
        });
    }
    if lower.starts_with("<?php") {
        return Some("php");
    }
    if lower.starts_with("<!doctype html") {
        return Some("html");
    }
    if first.starts_with('{') && first.contains('"') {
        return Some("json");
    }
    None
}

// ---- secrets -----------------------------------------------------------------------------

/// Whether the whole of `text`, trimmed, is one of the high-confidence secrets:
/// a private key, an AWS access key id, a GitHub or Slack token, a JWT, a Stripe live key or
/// a Bearer header line. A secret inside a longer text does not count; this is a safety net,
/// not a promise to find every secret.
pub fn is_secret(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    let token = |s: &str, ok: fn(char) -> bool| !s.is_empty() && s.chars().all(ok);
    let alnum = |c: char| c.is_ascii_alphanumeric();
    // -----BEGIN [A-Z ]*PRIVATE KEY-----
    if let Some(rest) = t.strip_prefix("-----BEGIN ") {
        let header = rest.lines().next().unwrap_or("");
        if let Some(kind) = header.strip_suffix("PRIVATE KEY-----")
            && kind.chars().all(|c| c.is_ascii_uppercase() || c == ' ')
        {
            return true;
        }
    }
    // (AKIA|ASIA)[0-9A-Z]{16}
    for prefix in ["AKIA", "ASIA"] {
        if let Some(rest) = t.strip_prefix(prefix)
            && rest.len() == 16
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        {
            return true;
        }
    }
    // gh[pousr]_[A-Za-z0-9]{36,}
    if let Some(rest) = t.strip_prefix("gh")
        && let Some(rest) = rest
            .strip_prefix(['p', 'o', 'u', 's', 'r'])
            .and_then(|r| r.strip_prefix('_'))
        && rest.len() >= 36
        && token(rest, alnum)
    {
        return true;
    }
    // xox[abpr]-[A-Za-z0-9-]{10,}
    if let Some(rest) = t.strip_prefix("xox")
        && let Some(rest) = rest
            .strip_prefix(['a', 'b', 'p', 'r'])
            .and_then(|r| r.strip_prefix('-'))
        && rest.len() >= 10
        && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return true;
    }
    // [sr]k_live_[A-Za-z0-9]{20,}
    if let Some(rest) = t
        .strip_prefix(['s', 'r'])
        .and_then(|r| r.strip_prefix("k_live_"))
        && rest.len() >= 20
        && token(rest, alnum)
    {
        return true;
    }
    // ^Bearer [A-Za-z0-9._~+/=-]{20,}$
    if let Some(rest) = t.strip_prefix("Bearer ")
        && rest.len() >= 20
        && rest.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '=' | '-')
        })
    {
        return true;
    }
    // Three base64url parts separated by dots, the first starting `eyJ`.
    if t.starts_with("eyJ") {
        let parts: Vec<&str> = t.split('.').collect();
        let url_safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=');
        if parts.len() == 3 && parts.iter().all(|p| token(p, url_safe)) {
            return true;
        }
    }
    false
}
