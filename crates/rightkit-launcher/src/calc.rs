//! The launcher's calculator: a small arithmetic parser, so typed text is never handed to an
//! evaluator, plus unit conversion.
//!
//! Grammar: sum = product (("+"|"-") product)*; product = power (("*"|"/") power)*;
//! power = unary ("^" power)?; unary = "-" unary | postfix; postfix = atom "%"*;
//! atom = number | "(" sum ")" | name | name "(" sum ")".

/// The formatted result, or `None` when the text is not arithmetic. A bare number is not
/// offered: it is a search, not a sum.
pub fn evaluate(text: &str) -> Option<String> {
    let mut parser = Parser::new(text);
    let value = parser.parse()?;
    (value.is_finite() && parser.used_operator).then(|| format(value))
}

/// Whole numbers in full, anything else to ten significant digits (C's `%.10g`).
pub fn format(value: f64) -> String {
    if value == value.round() && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    significant(value, 10)
}

/// C's `%.<digits>g`: that many significant digits, trailing zeros dropped, an exponent when
/// the number is very small or too large for the digits.
pub fn significant(value: f64, digits: i32) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let exponent = value.abs().log10().floor() as i32;
    if !(-5..digits).contains(&exponent) {
        let places = (digits - 1).max(0) as usize;
        let text = format!("{value:.places$e}");
        let (mantissa, power) = text.split_once('e').unwrap_or((&text, "0"));
        let mantissa = if mantissa.contains('.') {
            mantissa.trim_end_matches('0').trim_end_matches('.')
        } else {
            mantissa
        };
        let power: i32 = power.parse().unwrap_or(0);
        let sign = if power < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", power.abs());
    }
    let decimals = (digits - 1 - exponent).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

struct Parser {
    chars: Vec<char>,
    index: usize,
    used_operator: bool,
}

impl Parser {
    fn new(text: &str) -> Self {
        let chars = text
            .chars()
            .map(|c| match c {
                '×' | 'x' | 'X' => '*',
                '÷' => '/',
                '−' | '–' => '-',
                c => c,
            })
            .filter(|c| !c.is_whitespace())
            .collect();
        Self {
            chars,
            index: 0,
            used_operator: false,
        }
    }

    fn parse(&mut self) -> Option<f64> {
        if self.chars.is_empty() || self.chars.len() >= 200 {
            return None;
        }
        let value = self.sum()?;
        (self.index == self.chars.len()).then_some(value)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut left = self.product()?;
        while let Some(c @ ('+' | '-')) = self.peek() {
            self.index += 1;
            self.used_operator = true;
            let right = self.product()?;
            left = if c == '+' { left + right } else { left - right };
        }
        Some(left)
    }

    fn product(&mut self) -> Option<f64> {
        let mut left = self.power()?;
        while let Some(c @ ('*' | '/')) = self.peek() {
            self.index += 1;
            self.used_operator = true;
            if c == '*' && self.peek() == Some('*') {
                // "**" is a power.
                self.index += 1;
                let right = self.unary()?;
                left = left.powf(right);
                continue;
            }
            let right = self.power()?;
            if c == '/' && right == 0.0 {
                return None;
            }
            left = if c == '*' { left * right } else { left / right };
        }
        Some(left)
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.unary()?;
        if self.peek() == Some('^') {
            self.index += 1;
            self.used_operator = true;
            let exponent = self.power()?;
            return Some(base.powf(exponent));
        }
        Some(base)
    }

    fn unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some('-') => {
                self.index += 1;
                self.used_operator = true;
                Some(-self.unary()?)
            }
            Some('+') => {
                self.index += 1;
                self.unary()
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Option<f64> {
        let mut value = self.atom()?;
        while self.peek() == Some('%') {
            self.index += 1;
            self.used_operator = true;
            value /= 100.0;
        }
        Some(value)
    }

    fn atom(&mut self) -> Option<f64> {
        let c = self.peek()?;
        if c == '(' {
            self.index += 1;
            let inner = self.sum()?;
            if self.peek() != Some(')') {
                return None;
            }
            self.index += 1;
            return Some(inner);
        }
        if c.is_ascii_digit() || c == '.' {
            return self.number();
        }
        if !c.is_alphabetic() {
            return None;
        }
        let mut name = String::new();
        while let Some(letter) = self.peek().filter(|l| l.is_alphabetic()) {
            name.push(letter);
            self.index += 1;
        }
        match name.to_lowercase().as_str() {
            "pi" => Some(std::f64::consts::PI),
            "e" => Some(std::f64::consts::E),
            "sqrt" => {
                self.used_operator = true;
                if self.peek() != Some('(') {
                    return None;
                }
                self.index += 1;
                let inner = self.sum()?;
                if self.peek() != Some(')') || inner < 0.0 {
                    return None;
                }
                self.index += 1;
                Some(inner.sqrt())
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        let mut literal = String::new();
        let mut dots = 0;
        while let Some(c) = self.peek() {
            if !(c.is_ascii_digit() || c == '.' || c == ',') {
                break;
            }
            if c == '.' {
                dots += 1;
            }
            if c != ',' {
                literal.push(c);
            }
            self.index += 1;
        }
        if dots > 1 || literal == "." {
            return None;
        }
        literal.parse().ok()
    }
}

// ---------------------------------------------------------------- unit conversion

/// A conversion's row: the title ("1.609 mi"), what Return copies (the number alone), and
/// the line under the title.
pub struct Conversion {
    pub title: String,
    pub copy: String,
    pub detail: String,
}

#[derive(Clone, Copy)]
enum Scale {
    /// Linear to a base unit of its group (length, mass, volume, data, time, speed).
    Linear(&'static str, f64),
    Celsius,
    Fahrenheit,
    Kelvin,
}

const LINEAR: [(&[&str], &str, f64); 36] = [
    (&["m", "meter", "meters"], "length", 1.0),
    (&["km", "kilometer", "kilometers"], "length", 1_000.0),
    (&["cm"], "length", 0.01),
    (&["mm"], "length", 0.001),
    (&["mi", "mile", "miles"], "length", 1_609.344),
    (&["yd", "yard", "yards"], "length", 0.9144),
    (&["ft", "foot", "feet"], "length", 0.3048),
    (&["inch", "inches"], "length", 0.0254),
    (&["g", "gram", "grams"], "mass", 1.0),
    (&["mg"], "mass", 0.001),
    (
        &["kg", "kilo", "kilos", "kilogram", "kilograms"],
        "mass",
        1_000.0,
    ),
    (&["lb", "lbs", "pound", "pounds"], "mass", 453.592_37),
    (&["oz", "ounce", "ounces"], "mass", 28.349_523_125),
    (&["ml", "milliliter", "milliliters"], "volume", 1.0),
    (
        &["l", "liter", "liters", "litre", "litres"],
        "volume",
        1_000.0,
    ),
    (&["gal", "gallon", "gallons"], "volume", 3_785.411_784),
    (&["cup", "cups"], "volume", 236.588_236_5),
    (&["floz"], "volume", 29.573_529_562_5),
    (&["tbsp"], "volume", 14.786_764_781_25),
    (&["tsp"], "volume", 4.928_921_593_75),
    (&["kb"], "data", 1_000.0),
    (&["mb"], "data", 1_000_000.0),
    (&["gb"], "data", 1_000_000_000.0),
    (&["tb"], "data", 1_000_000_000_000.0),
    (&["kib"], "data", 1_024.0),
    (&["mib"], "data", 1_048_576.0),
    (&["gib"], "data", 1_073_741_824.0),
    (&["s", "sec", "secs", "second", "seconds"], "time", 1.0),
    (&["min", "mins", "minute", "minutes"], "time", 60.0),
    (&["h", "hr", "hrs", "hour", "hours"], "time", 3_600.0),
    (&["d", "day", "days"], "time", 86_400.0),
    (&["wk", "week", "weeks"], "time", 604_800.0),
    (&["mps"], "speed", 1.0),
    (&["kph", "kmh"], "speed", 1_000.0 / 3_600.0),
    (&["mph"], "speed", 0.447_04),
    (&["knot", "knots"], "speed", 0.514_444),
];

fn unit(name: &str) -> Option<Scale> {
    match name {
        "c" | "celsius" => return Some(Scale::Celsius),
        "f" | "fahrenheit" => return Some(Scale::Fahrenheit),
        "k" | "kelvin" => return Some(Scale::Kelvin),
        _ => {}
    }
    LINEAR
        .iter()
        .find(|(names, _, _)| names.contains(&name))
        .map(|&(_, group, factor)| Scale::Linear(group, factor))
}

fn to_celsius(value: f64, scale: Scale) -> Option<f64> {
    match scale {
        Scale::Celsius => Some(value),
        Scale::Fahrenheit => Some((value - 32.0) * 5.0 / 9.0),
        Scale::Kelvin => Some(value - 273.15),
        Scale::Linear(..) => None,
    }
}

fn from_celsius(celsius: f64, scale: Scale) -> Option<f64> {
    match scale {
        Scale::Celsius => Some(celsius),
        Scale::Fahrenheit => Some(celsius * 9.0 / 5.0 + 32.0),
        Scale::Kelvin => Some(celsius + 273.15),
        Scale::Linear(..) => None,
    }
}

fn converted(value: f64, from: Scale, to: Scale) -> Option<f64> {
    if let (Scale::Linear(group_a, factor_a), Scale::Linear(group_b, factor_b)) = (from, to) {
        return (group_a == group_b).then(|| value * factor_a / factor_b);
    }
    from_celsius(to_celsius(value, from)?, to)
}

/// "12km" or "12 km" gives (12, "km"). A thousands separator is allowed.
pub fn amount_and_unit(text: &str) -> Option<(f64, String)> {
    let end = text
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == ','))
        .map_or(text.len(), |(at, _)| at);
    let value: f64 = text[..end].replace(',', "").parse().ok()?;
    let unit = text[end..].trim();
    (!unit.is_empty()).then(|| (value, unit.to_string()))
}

fn shown(value: f64) -> String {
    if value == 0.0 {
        "0".to_string()
    } else if value.abs() >= 100.0 {
        format!("{value:.2}")
    } else {
        significant(value, 4)
    }
}

/// `None` unless the text is "`number` `unit` to|in|as `unit`" and both sides convert. The detail
/// line is the English "amount from to target".
pub fn convert(text: &str) -> Option<Conversion> {
    convert_with(text, |amount, from, target| {
        format!("{amount} {from} to {target}")
    })
}

/// Like [`convert`], with the detail line built by `describe(amount, from, target)` so the host
/// can translate it.
pub fn convert_with(
    text: &str,
    describe: impl Fn(&str, &str, &str) -> String,
) -> Option<Conversion> {
    let lowered = text.to_lowercase().replace('°', "");
    let tokens: Vec<&str> = lowered.split_whitespace().collect();
    if tokens.len() < 3 || !["to", "in", "as"].contains(&tokens[tokens.len() - 2]) {
        return None;
    }
    let target = tokens[tokens.len() - 1];
    let source = tokens[..tokens.len() - 2].join(" ");
    let (amount, from) = amount_and_unit(&source)?;
    let value = converted(amount, unit(&from)?, unit(target)?)?;
    let result = shown(value);
    Some(Conversion {
        title: format!("{result} {target}"),
        detail: describe(&shown(amount), &from, target),
        copy: result,
    })
}
