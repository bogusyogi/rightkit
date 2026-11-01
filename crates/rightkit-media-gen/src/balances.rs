//! studio.balances: live provider balances for NB2, fal, WaveSpeed and EvoLink.
//!
//! `studio.balances.list` (optional input `{providers: [..]}`) returns an array
//! of `{provider, label, balance, unit, fetched_at, error?}` and never fails as
//! a whole: a provider without a key, or whose endpoint is unreachable or
//! refuses the key, reports `balance: null` and an `error` string.
//!
//! Endpoints (the free balance calls from the baseline
//! `tools/lib/provider-credits.mjs` and `tools/lib/nb2-credits.mjs`):
//! - nanobanana (NB2): `GET https://api.nanobananaapi.ai/api/v1/common/credit`,
//!   `Authorization: Bearer`, reply `{code: 200, data: <credits>}`.
//! - fal: `GET https://api.fal.ai/v1/account/billing?expand=credits`,
//!   `Authorization: Key`, reply `credits.current_balance` + `credits.currency`.
//!   fal may require an admin-scope key for this endpoint.
//! - wavespeed: `GET https://api.wavespeed.ai/api/v3/balance`, Bearer, reply
//!   `{code: 200, data: {balance: <usd>}}`.
//! - evolink: `GET https://api.evolink.ai/v1/credits`, Bearer, reply
//!   `{success: true, data: {token: {remaining_credits}, user: {remaining_credits}}}`
//!   (the token balance wins, as in the baseline).
//!
//! Each key is read through `crate::credentials::get` and sent only to
//! its own provider host. Errors are redacted of the key.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::host::http::{Http, Request};

use crate::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &["studio.balances.list"];

/// Provider ids use the credential account names (NB2 is `nanobanana`).
pub const PROVIDERS: &[&str] = &["nanobanana", "fal", "wavespeed", "evolink"];

// Production hosts. Each provider's `RIGHTKIT_<P>_BASE` override (the one its adapter reads) replaces
// the host; the path after it is the same either way.
const NB_BASE: &str = "https://api.nanobananaapi.ai";
const NB_PATH: &str = "/api/v1/common/credit";
const FAL_BASE: &str = "https://api.fal.ai/v1";
const FAL_PATH: &str = "/account/billing?expand=credits";
const WAVESPEED_BASE: &str = "https://api.wavespeed.ai";
const WAVESPEED_PATH: &str = "/api/v3/balance";
const EVOLINK_BASE: &str = "https://api.evolink.ai";
const EVOLINK_PATH: &str = "/v1/credits";

/// Full endpoint URLs; `production()` builds them, tests point them at a local mock.
#[derive(Clone)]
pub struct Endpoints {
    pub nanobanana: String,
    pub fal: String,
    pub wavespeed: String,
    pub evolink: String,
}

impl Endpoints {
    /// Production bases, each overridable in development builds only (`providers::base_url_override`).
    pub fn production() -> Self {
        use crate::providers::base_url_override as base;
        Self {
            nanobanana: format!("{}{NB_PATH}", base("RIGHTKIT_NANOBANANA_BASE", NB_BASE)),
            fal: format!("{}{FAL_PATH}", base("RIGHTKIT_FAL_REST_BASE", FAL_BASE)),
            wavespeed: format!(
                "{}{WAVESPEED_PATH}",
                base("RIGHTKIT_WAVESPEED_BASE", WAVESPEED_BASE)
            ),
            evolink: format!(
                "{}{EVOLINK_PATH}",
                base("RIGHTKIT_EVOLINK_BASE", EVOLINK_BASE)
            ),
        }
    }
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.balances.list" => {
            let only = providers_filter(input)?;
            let keys = |p: &str| crate::credentials::get(ctx, p);
            Ok(list_with(
                &keys,
                &Endpoints::production(),
                &only,
                &now_iso(),
            ))
        }
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

fn providers_filter(input: &Value) -> GenResult<Vec<&'static str>> {
    let Some(v) = input.get("providers").filter(|v| !v.is_null()) else {
        return Ok(PROVIDERS.to_vec());
    };
    let arr = v.as_array().ok_or_else(|| {
        GenError::new(
            "invalid_input",
            "providers must be an array of provider ids",
        )
    })?;
    let mut out = Vec::new();
    for item in arr {
        let id = item.as_str().unwrap_or("");
        let id = if id == "nb2" { "nanobanana" } else { id };
        let found = PROVIDERS.iter().find(|p| **p == id).ok_or_else(|| {
            GenError::new(
                "invalid_input",
                format!("unknown balance provider: {}", item),
            )
        })?;
        if !out.contains(found) {
            out.push(*found);
        }
    }
    Ok(out)
}

/// Fetch the balances of `only`, in parallel, in that order.
pub fn list_with(
    keys: &(dyn Fn(&str) -> GenResult<Option<String>> + Sync),
    endpoints: &Endpoints,
    only: &[&'static str],
    fetched_at: &str,
) -> Value {
    let rows: Vec<Value> = std::thread::scope(|s| {
        let handles: Vec<_> = only
            .iter()
            .map(|p| s.spawn(move || row(p, keys, endpoints, fetched_at)))
            .collect();
        handles
            .into_iter()
            .zip(only)
            .map(|(h, p)| {
                h.join()
                    .unwrap_or_else(|_| failure(p, fetched_at, "balance lookup crashed".into()))
            })
            .collect()
    });
    Value::Array(rows)
}

fn label(provider: &str) -> &'static str {
    match provider {
        "nanobanana" => "NB2",
        "fal" => "fal.ai",
        "wavespeed" => "WaveSpeed",
        _ => "EvoLink",
    }
}

fn failure(provider: &str, fetched_at: &str, error: String) -> Value {
    json!({
        "provider": provider,
        "label": label(provider),
        "balance": Value::Null,
        "unit": Value::Null,
        "fetched_at": fetched_at,
        "error": error,
    })
}

fn row(
    provider: &'static str,
    keys: &(dyn Fn(&str) -> GenResult<Option<String>> + Sync),
    endpoints: &Endpoints,
    fetched_at: &str,
) -> Value {
    let key = match keys(provider) {
        Ok(Some(k)) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => {
            return failure(
                provider,
                fetched_at,
                format!("no {} API key set", label(provider)),
            )
        }
        Err(e) => {
            return failure(
                provider,
                fetched_at,
                format!("could not read the {} key: {}", label(provider), e.message),
            )
        }
    };
    match fetch(provider, &key, endpoints) {
        Ok((balance, unit)) => json!({
            "provider": provider,
            "label": label(provider),
            "balance": balance,
            "unit": unit,
            "fetched_at": fetched_at,
        }),
        Err(msg) => failure(
            provider,
            fetched_at,
            rightkit_logs::KnownSecrets::from_values([key.as_str()]).redact(&msg),
        ),
    }
}

fn fetch(provider: &str, key: &str, endpoints: &Endpoints) -> Result<(f64, String), String> {
    let (url, auth) = match provider {
        "nanobanana" => (&endpoints.nanobanana, format!("Bearer {key}")),
        "fal" => (&endpoints.fal, format!("Key {key}")),
        "wavespeed" => (&endpoints.wavespeed, format!("Bearer {key}")),
        _ => (&endpoints.evolink, format!("Bearer {key}")),
    };
    let http = Http::new(Duration::from_secs(5), Duration::from_secs(10));
    // The body is read inside `send`, so a body failure (including an over-limit body) reports as
    // "balance request failed"; "balance read failed" covers only invalid UTF-8.
    let resp = http
        .send(
            Request::get(url)
                .secret_header("Authorization", &auth)
                .max_response_bytes(1 << 20),
        )
        .map_err(|e| format!("{} balance request failed: {e}", label(provider)))?;
    let status = resp.status;
    let text = String::from_utf8(resp.body)
        .map_err(|e| format!("{} balance read failed: {e}", label(provider)))?;
    if !(200..300).contains(&status) {
        let snippet: String = text.chars().take(200).collect();
        let hint = if provider == "fal" && (status == 401 || status == 403) {
            " (the fal billing endpoint may need an admin-scope key)"
        } else {
            ""
        };
        return Err(format!(
            "{} balance request failed: HTTP {status}: {snippet}{hint}",
            label(provider)
        ));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| format!("{} balance reply was not JSON", label(provider)))?;
    parse(provider, &v)
}

fn parse(provider: &str, v: &Value) -> Result<(f64, String), String> {
    let missing = || format!("{} balance reply had no balance", label(provider));
    let num = |x: Option<&Value>| {
        x.and_then(|x| {
            x.as_f64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
    };
    match provider {
        "nanobanana" => {
            if let Some(code) = v.get("code").and_then(Value::as_u64) {
                if code != 200 {
                    return Err(format!(
                        "NB2 balance request failed: {}",
                        v.get("msg").and_then(Value::as_str).unwrap_or("error")
                    ));
                }
            }
            Ok((num(v.get("data")).ok_or_else(missing)?, "credits".into()))
        }
        "fal" => {
            let c = v.get("credits").ok_or_else(missing)?;
            let unit = c
                .get("currency")
                .and_then(Value::as_str)
                .unwrap_or("USD")
                .to_string();
            Ok((num(c.get("current_balance")).ok_or_else(missing)?, unit))
        }
        "wavespeed" => {
            if let Some(code) = v.get("code").and_then(Value::as_u64) {
                if code != 200 {
                    return Err(format!(
                        "WaveSpeed balance request failed: {}",
                        v.get("message").and_then(Value::as_str).unwrap_or("error")
                    ));
                }
            }
            Ok((
                num(v.get("data").and_then(|d| d.get("balance"))).ok_or_else(missing)?,
                "USD".into(),
            ))
        }
        _ => {
            if v.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(format!(
                    "EvoLink credits request failed: {}",
                    v.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("not successful")
                ));
            }
            let d = v.get("data");
            let token = num(d
                .and_then(|d| d.get("token"))
                .and_then(|t| t.get("remaining_credits")));
            let user = num(d
                .and_then(|d| d.get("user"))
                .and_then(|t| t.get("remaining_credits")));
            Ok((token.or(user).ok_or_else(missing)?, "credits".into()))
        }
    }
}

/// UTC time as `YYYY-MM-DDTHH:MM:SSZ` (civil-from-days, no time crate here).
fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    iso_from_secs(secs)
}

fn iso_from_secs(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
