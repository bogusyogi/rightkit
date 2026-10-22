//! Accessibility evidence for UI scenarios: text contrast (WCAG 2.x), the keyboard-focus indicator,
//! and role/name assertions over the control server's `ax` nodes.
//!
//! The page-side reads are small JS snippets (`contrast_probe_js`, `focus_probe_js`) sent through
//! the control server's `eval`. Everything that decides a verdict is pure Rust over the JSON they
//! return, so it is unit-tested without an app: colour parsing, background compositing, the contrast
//! ratio, the focus-indicator verdict and the role/name matcher.
use crate::util::{err, Error, Result};
use serde_json::{json, Map, Value};

// ------------------------------------------------------------------- colour

/// A colour in sRGB: channels 0..=255, alpha 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const WHITE: Rgba = Rgba {
        r: 255.0,
        g: 255.0,
        b: 255.0,
        a: 1.0,
    };
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    /// `self` painted over `under` (source-over). The result's alpha is the union of both.
    pub fn over(self, under: Rgba) -> Rgba {
        let a = self.a + under.a * (1.0 - self.a);
        if a <= 0.0 {
            return Rgba::TRANSPARENT;
        }
        let mix = |f: f64, b: f64| (f * self.a + b * under.a * (1.0 - self.a)) / a;
        Rgba {
            r: mix(self.r, under.r),
            g: mix(self.g, under.g),
            b: mix(self.b, under.b),
            a,
        }
    }

    pub fn hex(&self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            self.r.round() as u8,
            self.g.round() as u8,
            self.b.round() as u8
        )
    }
}

fn channel(token: &str, scale: f64) -> Option<f64> {
    let t = token.trim();
    if let Some(p) = t.strip_suffix('%') {
        return p
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v / 100.0 * scale).clamp(0.0, scale));
    }
    t.parse::<f64>().ok().map(|v| v.clamp(0.0, scale))
}

fn alpha(token: &str) -> Option<f64> {
    let t = token.trim();
    if let Some(p) = t.strip_suffix('%') {
        return p
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v / 100.0).clamp(0.0, 1.0));
    }
    t.parse::<f64>().ok().map(|v| v.clamp(0.0, 1.0))
}

/// Parse a CSS colour as the browser reports it from `getComputedStyle`: `rgb()`/`rgba()` (comma or
/// space syntax, optional `/ alpha`), `color(srgb r g b / a)`, `#rgb`, `#rgba`, `#rrggbb`,
/// `#rrggbbaa` and `transparent`. Anything else (named colours, `oklch`, ...) is `None`.
pub fn parse_css_color(text: &str) -> Option<Rgba> {
    let t = text.trim().to_ascii_lowercase();
    if t == "transparent" {
        return Some(Rgba::TRANSPARENT);
    }
    if let Some(hex) = t.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let nib = |i: usize| {
            u8::from_str_radix(&hex[i..i + 1], 16)
                .ok()
                .map(|v| f64::from(v * 17))
        };
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f64::from);
        return match hex.len() {
            3 => Some(Rgba {
                r: nib(0)?,
                g: nib(1)?,
                b: nib(2)?,
                a: 1.0,
            }),
            4 => Some(Rgba {
                r: nib(0)?,
                g: nib(1)?,
                b: nib(2)?,
                a: nib(3)? / 255.0,
            }),
            6 => Some(Rgba {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: 1.0,
            }),
            8 => Some(Rgba {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: byte(6)? / 255.0,
            }),
            _ => None,
        };
    }
    let open = t.find('(')?;
    let close = t.rfind(')')?;
    let (func, args) = (t[..open].trim(), &t[open + 1..close]);
    let (main, slash_alpha) = match args.split_once('/') {
        Some((m, a)) => (m, Some(a)),
        None => (args, None),
    };
    let parts: Vec<&str> = main
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    match func {
        "rgb" | "rgba" => {
            let (rgb, comma_alpha) = match parts.len() {
                3 => (&parts[..3], None),
                4 => (&parts[..3], Some(parts[3])),
                _ => return None,
            };
            let a = match (slash_alpha, comma_alpha) {
                (Some(a), None) | (None, Some(a)) => alpha(a)?,
                (None, None) => 1.0,
                _ => return None,
            };
            Some(Rgba {
                r: channel(rgb[0], 255.0)?,
                g: channel(rgb[1], 255.0)?,
                b: channel(rgb[2], 255.0)?,
                a,
            })
        }
        "color" => {
            if parts.len() != 4 || parts[0] != "srgb" {
                return None;
            }
            let a = slash_alpha.map(alpha).unwrap_or(Some(1.0))?;
            Some(Rgba {
                r: channel(parts[1], 1.0)? * 255.0,
                g: channel(parts[2], 1.0)? * 255.0,
                b: channel(parts[3], 1.0)? * 255.0,
                a,
            })
        }
        _ => None,
    }
}

/// WCAG relative luminance of an opaque colour.
pub fn relative_luminance(c: Rgba) -> f64 {
    let lin = |v: f64| {
        let s = v / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio (1.0 to 21.0) of `fg` on `bg`. A translucent `fg` is composited over `bg`
/// first; a translucent `bg` over white (the page canvas).
pub fn contrast_ratio(fg: Rgba, bg: Rgba) -> f64 {
    let bg = bg.over(Rgba::WHITE);
    let fg = fg.over(bg);
    let (a, b) = (relative_luminance(fg), relative_luminance(bg));
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// The colour actually behind an element: `chain` holds the computed `background-color` of the
/// element then each ancestor up to the root (innermost first). They are painted over a white canvas.
pub fn effective_background(chain: &[Rgba]) -> Rgba {
    chain
        .iter()
        .rev()
        .fold(Rgba::WHITE, |under, layer| layer.over(under))
}

/// WCAG "large text": at least 24 CSS px, or at least 18.66 px (14 pt) and bold (weight 700+).
pub fn is_large_text(font_px: f64, font_weight: f64) -> bool {
    font_px >= 24.0 || (font_px >= 18.66 && font_weight >= 700.0)
}

/// The minimum ratio for a conformance `level` (`AA` or `AAA`, any case) and text size.
pub fn contrast_threshold(level: &str, large: bool) -> Result<f64> {
    match (level.trim().to_ascii_uppercase().as_str(), large) {
        ("AA", false) => Ok(4.5),
        ("AA", true) => Ok(3.0),
        ("AAA", false) => Ok(7.0),
        ("AAA", true) => Ok(4.5),
        _ => err(format!("contrast level must be AA or AAA, got '{level}'")),
    }
}

/// Page-side read for [`contrast_report`]: the element's text colour, font size and weight, and the
/// computed `background-color` of it and every ancestor. `selector` is the first match.
pub fn contrast_probe_js(selector: &str) -> String {
    let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into());
    format!(
        "const el=document.querySelector({sel});if(!el)return {{found:false}};\
         const cs=getComputedStyle(el);const chain=[];let hasImage=false;\
         for(let n=el;n;n=n.parentElement){{const s=getComputedStyle(n);chain.push(s.backgroundColor);\
         if(s.backgroundImage&&s.backgroundImage!=='none')hasImage=true;}}\
         return {{found:true,color:cs.color,fontSize:parseFloat(cs.fontSize),fontWeight:parseFloat(cs.fontWeight)||400,\
         opacity:parseFloat(cs.opacity),chain,hasImage}};"
    )
}

/// How a contrast assertion is read: an explicit `min` wins; otherwise `level` (default `AA`) with
/// `large` (default: decided from the element's font size and weight).
#[derive(Debug, Clone, Default)]
pub struct ContrastOptions {
    pub level: Option<String>,
    pub min: Option<f64>,
    pub large: Option<bool>,
    /// Report the ratio without failing when it is below the minimum.
    pub report_only: bool,
}

/// Turn the page's reply to [`contrast_probe_js`] into a verdict. `Err` when the element is missing,
/// a colour cannot be read, or the ratio is below the minimum (unless `report_only`).
pub fn contrast_report(selector: &str, probe: &Value, opts: &ContrastOptions) -> Result<Value> {
    if probe.get("found").and_then(Value::as_bool) != Some(true) {
        return err(format!("contrast: no element matches '{selector}'"));
    }
    let text = |k: &str| probe.get(k).and_then(Value::as_str).unwrap_or_default();
    let fg = parse_css_color(text("color")).ok_or_else(|| {
        Error(format!(
            "contrast: cannot read text colour '{}' of '{selector}'",
            text("color")
        ))
    })?;
    let mut chain = Vec::new();
    for c in probe
        .get("chain")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let s = c.as_str().unwrap_or_default();
        chain.push(parse_css_color(s).ok_or_else(|| {
            Error(format!(
                "contrast: cannot read background colour '{s}' behind '{selector}'"
            ))
        })?);
    }
    let bg = effective_background(&chain);
    // The element's own opacity dims its text against the background.
    let opacity = probe
        .get("opacity")
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    let fg = Rgba {
        a: fg.a * opacity,
        ..fg
    };
    let ratio = contrast_ratio(fg, bg);
    let px = probe
        .get("fontSize")
        .and_then(Value::as_f64)
        .unwrap_or(16.0);
    let weight = probe
        .get("fontWeight")
        .and_then(Value::as_f64)
        .unwrap_or(400.0);
    let large = opts.large.unwrap_or_else(|| is_large_text(px, weight));
    let level = opts.level.clone().unwrap_or_else(|| "AA".into());
    let min = match opts.min {
        Some(m) => m,
        None => contrast_threshold(&level, large)?,
    };
    let has_image = probe
        .get("hasImage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let passes = ratio >= min;
    if !passes && !opts.report_only {
        return err(format!(
            "contrast of '{selector}' is {ratio:.2}:1, below the {min}:1 minimum (text {}, background {}{})",
            fg.over(bg).hex(),
            bg.hex(),
            if has_image { "; a background image is also present, so the measured background is approximate" } else { "" }
        ));
    }
    Ok(json!({
        "ratio": (ratio * 100.0).round() / 100.0, "min": min, "passes": passes, "large": large,
        "fg": fg.over(bg).hex(), "bg": bg.hex(), "font_px": px, "font_weight": weight,
        "background_image": has_image,
    }))
}

// -------------------------------------------------------------------- focus

/// Page-side read of the focused element's indicator: its computed outline and box-shadow, whether
/// `:focus-visible` matches, and its identity. With `selector`, `matches` says whether the focused
/// element is the one the selector names.
pub fn focus_probe_js(selector: Option<&str>) -> String {
    let sel = selector
        .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "null".into()))
        .unwrap_or_else(|| "null".into());
    format!(
        "const sel={sel};const el=document.activeElement;\
         if(!el||el===document.body||el===document.documentElement)return {{focused:false,matches:null}};\
         const cs=getComputedStyle(el);const r=el.getBoundingClientRect();let fv=null;\
         try{{fv=el.matches(':focus-visible');}}catch(e){{}}\
         return {{focused:true,matches:sel?el.matches(sel):null,tag:el.tagName.toLowerCase(),id:el.id||null,\
         role:el.getAttribute('role'),name:(el.getAttribute('aria-label')||el.innerText||el.value||'').trim().slice(0,80),\
         focusVisible:fv,outlineStyle:cs.outlineStyle,outlineWidth:cs.outlineWidth,outlineColor:cs.outlineColor,\
         outlineOffset:cs.outlineOffset,boxShadow:cs.boxShadow,rect:[r.x,r.y,r.width,r.height]}};"
    )
}

fn px_value(token: &str) -> Option<f64> {
    token
        .trim()
        .strip_suffix("px")
        .and_then(|v| v.trim().parse::<f64>().ok())
}

/// Split on whitespace outside parentheses (a computed `box-shadow` colour holds spaces).
fn top_level_tokens(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Split on commas outside parentheses (one entry per shadow).
fn top_level_commas(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in text.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// True when a computed `box-shadow` paints something: a shadow with a non-transparent colour and a
/// non-zero offset, blur or spread. `none` and fully transparent shadows are not an indicator.
pub fn box_shadow_visible(box_shadow: &str) -> bool {
    let text = box_shadow.trim();
    if text.is_empty() || text == "none" {
        return false;
    }
    top_level_commas(text).iter().any(|one| {
        let tokens = top_level_tokens(one);
        let colour = tokens.iter().find_map(|t| parse_css_color(t));
        let lengths: Vec<f64> = tokens.iter().filter_map(|t| px_value(t)).collect();
        // A shadow with no parsable colour uses currentColor, which is visible.
        colour.is_none_or(|c| c.a > 0.0) && lengths.iter().any(|v| *v != 0.0)
    })
}

/// Verdict over the reply to [`focus_probe_js`]: does the focused element show a visible indicator,
/// and which kind (`outline`, `box-shadow`, or `none`). A focused element with neither is a failed
/// focus-visibility check (`visible = false`), not an error; the step's `expect` decides.
pub fn focus_ring_verdict(probe: &Value) -> Value {
    if probe.get("focused").and_then(Value::as_bool) != Some(true) {
        return json!({"focused": false, "visible": false, "kind": "none", "reason": "no element has keyboard focus"});
    }
    let text = |k: &str| probe.get(k).and_then(Value::as_str).unwrap_or_default();
    let style = text("outlineStyle");
    let width = px_value(text("outlineWidth")).unwrap_or(0.0);
    let colour_visible = parse_css_color(text("outlineColor")).is_none_or(|c| c.a > 0.0);
    let outline = !matches!(style, "" | "none" | "hidden") && width > 0.0 && colour_visible;
    let shadow = box_shadow_visible(text("boxShadow"));
    let (kind, reason) = if outline {
        (
            "outline",
            format!("outline {style} {width}px {}", text("outlineColor")),
        )
    } else if shadow {
        ("box-shadow", format!("box-shadow {}", text("boxShadow")))
    } else {
        (
            "none",
            format!(
                "outline is '{style} {}' and box-shadow is '{}'",
                text("outlineWidth"),
                text("boxShadow")
            ),
        )
    };
    json!({
        "focused": true, "visible": kind != "none", "kind": kind, "reason": reason,
        "matches": probe.get("matches").cloned().unwrap_or(Value::Null),
        "focus_visible": probe.get("focusVisible").cloned().unwrap_or(Value::Null),
        "tag": probe.get("tag").cloned().unwrap_or(Value::Null),
        "id": probe.get("id").cloned().unwrap_or(Value::Null),
        "role": probe.get("role").cloned().unwrap_or(Value::Null),
        "name": probe.get("name").cloned().unwrap_or(Value::Null),
        "rect": probe.get("rect").cloned().unwrap_or(Value::Null),
        "outline": {"style": style, "width": text("outlineWidth"), "color": text("outlineColor"), "offset": text("outlineOffset")},
        "box_shadow": text("boxShadow"),
    })
}

// ----------------------------------------------------------------------- ax

/// A role/name assertion over the control server's `ax` nodes (`{role, name, disabled, ...}`).
#[derive(Debug, Clone, Default)]
pub struct AxQuery {
    /// ARIA or implicit role, compared case-insensitively.
    pub role: Option<String>,
    /// Accessible name, equal after trimming and collapsing whitespace.
    pub name: Option<String>,
    /// Accessible name contains this text (case-insensitive).
    pub name_contains: Option<String>,
    /// The accessible name must not be empty.
    pub named: bool,
    pub disabled: Option<bool>,
    /// At least this many nodes must match (default 1).
    pub min_count: Option<usize>,
    pub max_count: Option<usize>,
}

impl AxQuery {
    pub fn from_params(p: &Map<String, Value>) -> Result<Self> {
        let s = |k: &str| p.get(k).and_then(Value::as_str).map(String::from);
        let n = |k: &str| p.get(k).and_then(Value::as_u64).map(|v| v as usize);
        let q = AxQuery {
            role: s("role"),
            name: s("name"),
            name_contains: s("name_contains"),
            named: p.get("named").and_then(Value::as_bool).unwrap_or(false),
            disabled: p.get("disabled").and_then(Value::as_bool),
            min_count: n("min_count"),
            max_count: n("max_count"),
        };
        if q.role.is_none() && q.name.is_none() && q.name_contains.is_none() && !q.named {
            return err("ui_assert_ax needs at least one of role, name, name_contains or named");
        }
        Ok(q)
    }

    fn matches(&self, node: &Value) -> bool {
        let role = node.get("role").and_then(Value::as_str).unwrap_or_default();
        let name = collapse(node.get("name").and_then(Value::as_str).unwrap_or_default());
        self.role
            .as_deref()
            .is_none_or(|r| role.eq_ignore_ascii_case(r.trim()))
            && self.name.as_deref().is_none_or(|n| name == collapse(n))
            && self
                .name_contains
                .as_deref()
                .is_none_or(|n| name.to_lowercase().contains(&collapse(n).to_lowercase()))
            && (!self.named || !name.is_empty())
            && self.disabled.is_none_or(|d| {
                node.get("disabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    == d
            })
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The nodes that satisfy `q`.
pub fn ax_find<'a>(nodes: &'a [Value], q: &AxQuery) -> Vec<&'a Value> {
    nodes.iter().filter(|n| q.matches(n)).collect()
}

/// Assert `q` over `nodes`. Returns `{count, matches}`; the error lists the roles and names that
/// exist so a mismatch can be read without a second run.
pub fn ax_assert(nodes: &[Value], q: &AxQuery) -> Result<Value> {
    let found = ax_find(nodes, q);
    let (min, max) = (q.min_count.unwrap_or(1), q.max_count);
    if found.len() < min || max.is_some_and(|m| found.len() > m) {
        let seen: Vec<String> = nodes
            .iter()
            .take(40)
            .map(|n| {
                format!(
                    "{}:'{}'",
                    n.get("role").and_then(Value::as_str).unwrap_or("?"),
                    collapse(n.get("name").and_then(Value::as_str).unwrap_or_default())
                )
            })
            .collect();
        return err(format!(
            "ax: expected {min}{} node(s) matching {q:?}, found {} among {} node(s): [{}]",
            max.map(|m| format!("..{m}")).unwrap_or_default(),
            found.len(),
            nodes.len(),
            seen.join(", ")
        ));
    }
    Ok(json!({"count": found.len(), "matches": found}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
            a: 1.0,
        }
    }

    #[test]
    fn parses_the_colour_forms_a_browser_reports() {
        assert_eq!(parse_css_color("rgb(12, 34, 56)"), Some(rgb(12, 34, 56)));
        assert_eq!(
            parse_css_color("rgba(0, 0, 0, 0)"),
            Some(Rgba {
                a: 0.0,
                ..rgb(0, 0, 0)
            })
        );
        assert_eq!(
            parse_css_color("rgb(10 20 30 / 50%)"),
            Some(Rgba {
                a: 0.5,
                ..rgb(10, 20, 30)
            })
        );
        assert_eq!(parse_css_color("#fff"), Some(Rgba::WHITE));
        assert_eq!(parse_css_color("#102030"), Some(rgb(16, 32, 48)));
        assert_eq!(
            parse_css_color("#10203080").map(|c| (c.a * 255.0).round()),
            Some(128.0)
        );
        assert_eq!(parse_css_color("transparent"), Some(Rgba::TRANSPARENT));
        let c = parse_css_color("color(srgb 1 0.5 0 / 0.25)").unwrap();
        assert_eq!((c.r, c.g.round(), c.b, c.a), (255.0, 128.0, 0.0, 0.25));
        for bad in ["", "red", "oklch(0.5 0.1 20)", "rgb(1, 2)", "#12", "#ggg"] {
            assert_eq!(parse_css_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn contrast_ratio_matches_the_wcag_reference_values() {
        let black = rgb(0, 0, 0);
        assert!((contrast_ratio(black, Rgba::WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(Rgba::WHITE, Rgba::WHITE) - 1.0).abs() < 1e-9);
        // Order does not matter: the ratio is light over dark.
        assert_eq!(
            contrast_ratio(black, Rgba::WHITE),
            contrast_ratio(Rgba::WHITE, black)
        );
        // #767676 is the lightest grey that passes AA on white; #777777 just fails.
        assert!(contrast_ratio(rgb(0x76, 0x76, 0x76), Rgba::WHITE) >= 4.5);
        assert!(contrast_ratio(rgb(0x77, 0x77, 0x77), Rgba::WHITE) < 4.5);
    }

    #[test]
    fn translucent_layers_composite_before_the_ratio() {
        // Half-transparent black text on white reads as mid grey: 21:1 down to about 4:1.
        let half = Rgba {
            a: 0.5,
            ..rgb(0, 0, 0)
        };
        let r = contrast_ratio(half, Rgba::WHITE);
        assert!(r > 3.8 && r < 4.2, "{r}");
        // Background chain, innermost first: transparent, then 50% black, then the page (transparent).
        let chain = [
            Rgba::TRANSPARENT,
            Rgba {
                a: 0.5,
                ..rgb(0, 0, 0)
            },
            Rgba::TRANSPARENT,
        ];
        let bg = effective_background(&chain);
        assert_eq!(bg.hex(), "#808080");
        assert_eq!(effective_background(&[]).hex(), "#ffffff");
        // The inner opaque layer wins.
        assert_eq!(
            effective_background(&[rgb(1, 2, 3), rgb(9, 9, 9)]).hex(),
            "#010203"
        );
    }

    #[test]
    fn large_text_and_thresholds() {
        assert!(is_large_text(24.0, 400.0));
        assert!(is_large_text(18.66, 700.0));
        assert!(!is_large_text(18.66, 400.0));
        assert!(!is_large_text(16.0, 700.0));
        assert_eq!(contrast_threshold("aa", false).unwrap(), 4.5);
        assert_eq!(contrast_threshold("AA", true).unwrap(), 3.0);
        assert_eq!(contrast_threshold("AAA", false).unwrap(), 7.0);
        assert_eq!(contrast_threshold("AAA", true).unwrap(), 4.5);
        assert!(contrast_threshold("A", false).is_err());
    }

    fn probe(color: &str, chain: &[&str], px: f64) -> Value {
        json!({"found": true, "color": color, "chain": chain, "fontSize": px, "fontWeight": 400, "opacity": 1, "hasImage": false})
    }

    #[test]
    fn contrast_report_passes_fails_and_reports() {
        let ok = contrast_report(
            "#a",
            &probe(
                "rgb(0, 0, 0)",
                &["rgba(0, 0, 0, 0)", "rgb(255, 255, 255)"],
                16.0,
            ),
            &ContrastOptions::default(),
        )
        .unwrap();
        assert_eq!(ok["ratio"], json!(21.0));
        assert_eq!(ok["passes"], json!(true));
        let low = probe("rgb(119, 119, 119)", &["rgb(255, 255, 255)"], 16.0);
        let e = contrast_report("#a", &low, &ContrastOptions::default()).unwrap_err();
        assert!(e.0.contains("#a") && e.0.contains("4.5:1"), "{e}");
        // The same colours pass as large text, with an explicit lower minimum, or when only reporting.
        assert!(contrast_report(
            "#a",
            &low,
            &ContrastOptions {
                large: Some(true),
                ..Default::default()
            }
        )
        .is_ok());
        assert!(contrast_report(
            "#a",
            &low,
            &ContrastOptions {
                min: Some(4.0),
                ..Default::default()
            }
        )
        .is_ok());
        let rep = contrast_report(
            "#a",
            &low,
            &ContrastOptions {
                report_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rep["passes"], json!(false));
        // A large font size picks the large threshold on its own.
        assert_eq!(
            contrast_report(
                "#a",
                &probe("rgb(119, 119, 119)", &["rgb(255, 255, 255)"], 28.0),
                &ContrastOptions::default()
            )
            .unwrap()["large"],
            json!(true)
        );
        // Missing element and unreadable colours are errors, not silent passes.
        assert!(
            contrast_report("#a", &json!({"found": false}), &ContrastOptions::default()).is_err()
        );
        assert!(contrast_report(
            "#a",
            &probe("oklch(0.5 0.1 20)", &[], 16.0),
            &ContrastOptions::default()
        )
        .is_err());
        assert!(contrast_report(
            "#a",
            &probe("rgb(0,0,0)", &["lab(1 2 3)"], 16.0),
            &ContrastOptions::default()
        )
        .is_err());
    }

    #[test]
    fn probes_quote_the_selector() {
        let js = contrast_probe_js("a[href=\"x\"]");
        assert!(js.contains(r#"querySelector("a[href=\"x\"]")"#), "{js}");
        assert!(focus_probe_js(None).contains("const sel=null;"));
        assert!(focus_probe_js(Some("#go")).contains(r##"const sel="#go";"##));
    }

    #[test]
    fn box_shadow_is_an_indicator_only_when_it_paints() {
        assert!(box_shadow_visible("rgb(0, 95, 204) 0px 0px 0px 2px"));
        assert!(box_shadow_visible(
            "rgba(0, 0, 0, 0.3) 0px 1px 4px 0px, rgb(0, 95, 204) 0px 0px 0px 3px"
        ));
        assert!(!box_shadow_visible("none"));
        assert!(!box_shadow_visible(""));
        assert!(!box_shadow_visible("rgba(0, 0, 0, 0) 0px 0px 0px 2px"));
        assert!(!box_shadow_visible("rgb(0, 95, 204) 0px 0px 0px 0px"));
    }

    #[test]
    fn focus_verdict_reads_outline_shadow_or_neither() {
        let base = json!({"focused": true, "matches": true, "tag": "button", "id": "go", "focusVisible": true,
            "outlineStyle": "auto", "outlineWidth": "1px", "outlineColor": "rgb(16, 16, 16)", "outlineOffset": "0px", "boxShadow": "none"});
        let v = focus_ring_verdict(&base);
        assert_eq!(
            (v["visible"].clone(), v["kind"].clone()),
            (json!(true), json!("outline"))
        );
        assert_eq!(v["id"], json!("go"));
        let mut shadow = base.clone();
        shadow["outlineStyle"] = json!("none");
        shadow["outlineWidth"] = json!("0px");
        shadow["boxShadow"] = json!("rgb(0, 95, 204) 0px 0px 0px 2px");
        assert_eq!(focus_ring_verdict(&shadow)["kind"], json!("box-shadow"));
        let mut none = shadow.clone();
        none["boxShadow"] = json!("none");
        let v = focus_ring_verdict(&none);
        assert_eq!(
            (v["visible"].clone(), v["kind"].clone()),
            (json!(false), json!("none"))
        );
        // A zero-width or transparent outline is no outline.
        let mut zero = base.clone();
        zero["outlineWidth"] = json!("0px");
        assert_eq!(focus_ring_verdict(&zero)["visible"], json!(false));
        let mut clear = base;
        clear["outlineColor"] = json!("rgba(0, 0, 0, 0)");
        assert_eq!(focus_ring_verdict(&clear)["visible"], json!(false));
        let unfocused = focus_ring_verdict(&json!({"focused": false}));
        assert_eq!(
            (unfocused["focused"].clone(), unfocused["visible"].clone()),
            (json!(false), json!(false))
        );
    }

    fn nodes() -> Vec<Value> {
        vec![
            json!({"role": "button", "name": "Save  project", "disabled": false}),
            json!({"role": "button", "name": "", "disabled": true}),
            json!({"role": "textbox", "name": "Title", "disabled": false}),
            json!({"role": "heading", "name": "Settings"}),
        ]
    }

    fn query(v: Value) -> AxQuery {
        AxQuery::from_params(v.as_object().unwrap()).unwrap()
    }

    #[test]
    fn ax_role_and_name_matching() {
        let n = nodes();
        assert_eq!(ax_find(&n, &query(json!({"role": "BUTTON"}))).len(), 2);
        // Names compare after whitespace is collapsed.
        assert_eq!(
            ax_find(
                &n,
                &query(json!({"role": "button", "name": "Save project"}))
            )
            .len(),
            1
        );
        assert_eq!(
            ax_find(&n, &query(json!({"name_contains": "save"}))).len(),
            1
        );
        assert_eq!(
            ax_find(&n, &query(json!({"role": "button", "named": true}))).len(),
            1
        );
        assert_eq!(
            ax_find(&n, &query(json!({"role": "button", "disabled": true}))).len(),
            1
        );
        assert_eq!(ax_find(&n, &query(json!({"name": "save"}))).len(), 0);
    }

    #[test]
    fn ax_assert_counts_and_explains_a_miss() {
        let n = nodes();
        assert_eq!(
            ax_assert(&n, &query(json!({"role": "textbox", "name": "Title"}))).unwrap()["count"],
            json!(1)
        );
        let e = ax_assert(&n, &query(json!({"role": "button", "name": "Delete"}))).unwrap_err();
        assert!(
            e.0.contains("Delete") && e.0.contains("textbox:'Title'"),
            "{e}"
        );
        assert!(ax_assert(&n, &query(json!({"role": "button", "max_count": 1}))).is_err());
        assert!(ax_assert(&n, &query(json!({"role": "button", "min_count": 2}))).is_ok());
        assert!(ax_assert(
            &n,
            &query(json!({"role": "link", "min_count": 0, "max_count": 0}))
        )
        .is_ok());
        assert!(AxQuery::from_params(json!({"min_count": 1}).as_object().unwrap()).is_err());
    }
}
