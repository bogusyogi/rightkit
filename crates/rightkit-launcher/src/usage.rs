//! Ranking for command launchers. Callers supply Unix seconds and their history size.
use crate::json::Value;
use std::collections::BTreeMap;

/// Launcher choices ranked by frequency and time since last use.
#[derive(Default)]
pub struct Usage {
    /// Key to (count, last use in seconds since 1970).
    records: BTreeMap<String, (f64, f64)>,
}

impl Usage {
    pub fn from_document(document: Option<Value>) -> Self {
        let mut records = BTreeMap::new();
        if let Some(Value::Object(members)) = document {
            for (key, value) in members {
                let count = value.get("count").and_then(Value::as_f64);
                let last = value.get("last").and_then(Value::as_f64);
                if let (Some(count), Some(last)) = (count, last) {
                    records.insert(key, (count, last));
                }
            }
        }
        Self { records }
    }

    fn decayed(&(count, last): &(f64, f64), now: f64) -> f64 {
        let days = (now - last).max(0.0) / 86_400.0;
        count / (1.0 + days / 14.0)
    }

    /// Points added to a fuzzy score, at most 80.
    pub fn bonus(&self, key: &str, now: f64) -> i32 {
        match self.records.get(key) {
            Some(record) => (Self::decayed(record, now).min(20.0) * 4.0) as i32,
            None => 0,
        }
    }

    /// Keys with the prefix, most used first, without the prefix.
    pub fn top(&self, prefix: &str, limit: usize, now: f64) -> Vec<String> {
        let mut found: Vec<(&String, f64)> = self
            .records
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, record)| (key, Self::decayed(record, now)))
            .collect();
        found.sort_by(|a, b| b.1.total_cmp(&a.1));
        found
            .into_iter()
            .take(limit)
            .map(|(key, _)| key[prefix.len()..].to_string())
            .collect()
    }

    pub fn record(&mut self, key: &str, now: f64, capacity: usize) {
        let entry = self.records.entry(key.to_string()).or_insert((0.0, now));
        entry.0 += 1.0;
        entry.1 = now;
        if self.records.len() > capacity {
            // Drop the least used so the file stays small.
            let mut all: Vec<(String, f64)> = self
                .records
                .iter()
                .map(|(key, record)| (key.clone(), Self::decayed(record, now)))
                .collect();
            all.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (key, _) in all.into_iter().skip(capacity) {
                self.records.remove(&key);
            }
        }
    }

    /// Saved `usage.json` with ordered keys and timestamps rounded to seconds.
    pub fn to_json(&self) -> String {
        let mut body = String::from("{");
        for (i, (key, (count, last))) in self.records.iter().enumerate() {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!(
                "{}:{{\"count\":{count},\"last\":{}}}",
                crate::config::quoted(key),
                last.round()
            ));
        }
        body.push('}');
        body
    }
}
