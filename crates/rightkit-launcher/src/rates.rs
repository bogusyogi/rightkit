//! Currency and crypto conversions for the launcher (the money half of the Mac's
//! `LauncherConversion`, and its `LauncherRates`): "100 usd in eur", "0.5 btc to usd". Rates
//! come from two fixed https addresses (open.er-api.com for currencies, CoinGecko for coins),
//! at most once a day, and are kept in `rates.json` in the launcher's folder. A failed or
//! offline fetch leaves the cache as it was, so the conversions simply do not appear. Only
//! fixed addresses are requested: nothing the user typed is sent anywhere.
//!
//! Called by the launcher only while the `launcherCurrency` switch is on. Unit conversions
//! stay in `launcher_calc.rs`, which is asked first.

use crate::calc as launcher_calc;
use crate::config as launcher_config;
use crate::host;
use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Upper-case symbol and CoinGecko id (the Mac's `LauncherRates.coinIDs`).
const COINS: [(&str, &str); 7] = [
    ("BTC", "bitcoin"),
    ("ETH", "ethereum"),
    ("SOL", "solana"),
    ("DOGE", "dogecoin"),
    ("LTC", "litecoin"),
    ("XRP", "ripple"),
    ("ADA", "cardano"),
];

const FIAT_HOST: &str = "open.er-api.com";
const FIAT_PATH: &str = "/v6/latest/USD";
const COIN_HOST: &str = "api.coingecko.com";
const CACHE_FILE: &str = "rates.json";
/// A day: the cache is fetched again once it is this old.
const MAX_AGE: u64 = 86_400;
/// An hour: a fetch attempt is not repeated sooner, so a failing network is not hammered.
const RETRY_AFTER: u64 = 3_600;
const TIMEOUT_MS: i32 = 10_000;
/// The largest response body read; `http::get` refuses anything larger than this too.
const BODY_MAX: usize = 512 * 1024;
const CACHE_MAX: usize = 64 * 1024;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// True while a background refresh runs; a second start then does nothing.
static FETCHING: AtomicBool = AtomicBool::new(false);
/// Unix seconds of the last fetch attempt in this process; 0 before the first.
static LAST_ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// The cached rates: units of each currency per US dollar, US dollars per coin, and the
/// time they were fetched (unix seconds).
pub struct Rates {
    per_usd: BTreeMap<String, f64>,
    usd_per_coin: BTreeMap<String, f64>,
    fetched_at: u64,
}

/// The cached rates in `dir`, or `None` when there is no readable cache.
pub fn load(dir: &Path) -> Option<Rates> {
    let file = std::fs::File::open(dir.join(CACHE_FILE)).ok()?;
    let mut bytes = Vec::new();
    file.take(CACHE_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let root = json::parse(&bytes, CACHE_MAX)?;
    let fetched = root
        .get("fetched")
        .and_then(Value::as_f64)
        .filter(|secs| *secs >= 0.0)?;
    Some(Rates {
        per_usd: numbers(root.get("perUSD"))?,
        usd_per_coin: numbers(root.get("usdPerCoin"))?,
        fetched_at: fetched as u64,
    })
}

/// Writes the cache into `dir`, atomically. A failed write is ignored: the next fetch
/// replaces the cache anyway.
pub fn save(dir: &Path, rates: &Rates) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let text = format!(
        "{{\"fetched\":{},\"perUSD\":{},\"usdPerCoin\":{}}}",
        rates.fetched_at,
        object(&rates.per_usd),
        object(&rates.usd_per_coin),
    );
    let _ = rightkit_fs::atomic_write(&dir.join(CACHE_FILE), text.as_bytes());
}

/// True when the rates are a day old or more.
fn stale(rates: &Rates, now: u64) -> bool {
    now.saturating_sub(rates.fetched_at) >= MAX_AGE
}

/// Starts a background refresh of the cache in `dir`. It fetches only when the cache is a day
/// old and the last attempt was over an hour ago. Returns at once: the launcher thread never
/// waits on the network.
pub fn refresh_in_background(dir: &Path) {
    if FETCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let dir = dir.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("rightkit-launcher-rates".to_string())
        .spawn(move || {
            refresh(&dir);
            FETCHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        FETCHING.store(false, Ordering::Release);
        host::log_info("launcher_rates", &[("result", "no_thread")]);
    }
}

/// One pass on the refresh thread: fetch and save when the cache is stale and the last
/// attempt is over an hour old.
fn refresh(dir: &Path) {
    let now = host::now_secs();
    if load(dir).is_some_and(|rates| !stale(&rates, now)) {
        return;
    }
    if now.saturating_sub(LAST_ATTEMPT.load(Ordering::Relaxed)) < RETRY_AFTER {
        return;
    }
    LAST_ATTEMPT.store(now, Ordering::Relaxed);
    if let Some(fresh) = fetch() {
        save(dir, &fresh);
        // An open panel answers money conversions from now on, not only the next time.
        host::rates_ready();
    }
}

/// Both tables. The currency table is required (it must hold USD, as on the Mac); coin
/// prices may be missing, and then the coin conversions do not appear.
fn fetch() -> Option<Rates> {
    let fiat = get_json(FIAT_HOST, FIAT_PATH)?;
    let per_usd = numbers(fiat.get("rates"))?;
    if !per_usd.contains_key("USD") {
        return None;
    }
    let ids: Vec<&str> = COINS.iter().map(|(_, id)| *id).collect();
    let prices = get_json(
        COIN_HOST,
        &format!(
            "/api/v3/simple/price?ids={}&vs_currencies=usd",
            ids.join(",")
        ),
    );
    let usd_per_coin: BTreeMap<String, f64> = COINS
        .iter()
        .filter_map(|(symbol, id)| {
            let price = prices
                .as_ref()?
                .path(&[*id, "usd"])?
                .as_f64()
                .filter(|p| *p > 0.0)?;
            Some(((*symbol).to_string(), price))
        })
        .collect();
    Some(Rates {
        per_usd,
        usd_per_coin,
        fetched_at: host::now_secs(),
    })
}

/// A JSON body from a fixed https address; `None` on any failure or a status other than 200.
fn get_json(host: &str, path: &str) -> Option<Value> {
    let response = host::https_get(
        host,
        path,
        &[],
        std::time::Duration::from_millis(TIMEOUT_MS as u64),
    )?;
    if response.status != 200 {
        return None;
    }
    json::parse(&response.body, BODY_MAX)
}

/// The positive numbers of a JSON object, by key; `None` when `value` is not an object.
fn numbers(value: Option<&Value>) -> Option<BTreeMap<String, f64>> {
    let Value::Object(members) = value? else {
        return None;
    };
    Some(
        members
            .iter()
            .filter_map(|(code, amount)| {
                let amount = amount.as_f64().filter(|a| *a > 0.0)?;
                Some((code.clone(), amount))
            })
            .collect(),
    )
}

/// A JSON object text of the numbers in `map`.
fn object(map: &BTreeMap<String, f64>) -> String {
    let members: Vec<String> = map
        .iter()
        .map(|(code, amount)| format!("{}:{amount}", launcher_config::quoted(code)))
        .collect();
    format!("{{{}}}", members.join(","))
}

/// The answer and its detail line for "<amount> <currency or coin> to|in|as <currency or
/// coin>", worded as the Mac does: ("86.42 EUR", "USD to EUR · rates from Oct 11, 2026").
/// `None` when the text is not such a conversion or a rate is missing. The answer is the
/// number, a space, then the code; the code has no space, so the number is everything before
/// the last space.
pub fn convert(text: &str, rates: &Rates) -> Option<(String, String)> {
    let lowered = text.to_lowercase().replace('°', "");
    let tokens: Vec<&str> = lowered.split_whitespace().collect();
    if tokens.len() < 3 || !["to", "in", "as"].contains(&tokens[tokens.len() - 2]) {
        return None;
    }
    let target = tokens[tokens.len() - 1];
    let source = tokens[..tokens.len() - 2].join(" ");
    let (amount, from) = launcher_calc::amount_and_unit(&source)?;
    let from_money = money(&from, rates)?;
    let to_money = money(target, rates)?;
    let usd = usd_value(amount, &from_money, rates)?;
    let value = from_usd(usd, &to_money, rates)?;
    let answer = format!("{} {}", shown_money(value, &to_money), to_money.code());
    let detail = format!(
        "{} to {} · rates from {}",
        from.to_uppercase(),
        to_money.code(),
        date_text(rates.fetched_at),
    );
    Some((answer, detail))
}

enum Money {
    Fiat(String),
    Coin(String),
}

impl Money {
    fn code(&self) -> &str {
        match self {
            Money::Fiat(code) | Money::Coin(code) => code.as_str(),
        }
    }
}

/// The money a token names: a coin with a price, or a currency with a rate (USD always).
fn money(token: &str, rates: &Rates) -> Option<Money> {
    let code = token.to_uppercase();
    if COINS.iter().any(|(symbol, _)| *symbol == code) && rates.usd_per_coin.contains_key(&code) {
        return Some(Money::Coin(code));
    }
    (code == "USD" || rates.per_usd.contains_key(&code)).then_some(Money::Fiat(code))
}

fn usd_value(amount: f64, money: &Money, rates: &Rates) -> Option<f64> {
    match money {
        Money::Fiat(code) if code.as_str() == "USD" => Some(amount),
        Money::Fiat(code) => Some(amount / *rates.per_usd.get(code)?),
        Money::Coin(symbol) => Some(amount * *rates.usd_per_coin.get(symbol)?),
    }
}

fn from_usd(usd: f64, money: &Money, rates: &Rates) -> Option<f64> {
    match money {
        Money::Fiat(code) if code.as_str() == "USD" => Some(usd),
        Money::Fiat(code) => Some(usd * *rates.per_usd.get(code)?),
        Money::Coin(symbol) => Some(usd / *rates.usd_per_coin.get(symbol)?),
    }
}

/// Currencies to two decimals; coins to four, or eight significant digits below a cent (the
/// Mac's `%.2f`, `%.4f` and `%.8g`).
fn shown_money(value: f64, money: &Money) -> String {
    match money {
        Money::Fiat(_) => format!("{value:.2}"),
        Money::Coin(_) if value >= 0.01 => format!("{value:.4}"),
        Money::Coin(_) => launcher_calc::significant(value, 8),
    }
}

/// The cache's date as the Mac shows it in an English locale, such as "Oct 11, 2026". The
/// day is counted in UTC.
fn date_text(fetched_at: u64) -> String {
    let (year, month, day) = rightkit_time::civil_from_days((fetched_at / 86_400) as i64);
    format!("{} {day}, {year}", MONTHS[month.clamp(1, 12) as usize - 1])
}
