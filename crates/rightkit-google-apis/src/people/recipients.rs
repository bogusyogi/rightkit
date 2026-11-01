use super::*;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Recipient {
    pub name: String,
    pub email: String,
    pub photo_url: String,
}

/// Merge results from all three People sources and return up to 12 deduplicated
/// Recipient candidates ranked by source priority (contacts → directory → other).
///
/// * An empty `query` returns `Ok(vec![])` immediately — callers should use
///   `warm_people_cache` for the zero-query warm-up instead.
/// * A 4xx from any one source is treated as empty (best-effort; auth errors
///   propagated by the shared client still bubble up as `Err`).
/// * Dedup is by lowercased email; first occurrence (in source-priority order) wins.
pub async fn search_recipients(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    query: &str,
) -> Result<Vec<Recipient>, String> {
    if query.is_empty() {
        return Ok(vec![]);
    }

    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    // Helper: fetch one source, convert 4xx → empty vec, propagate auth/transient.
    async fn fetch_source(
        g: &Google,
        url: &str,
        query_params: &[(&str, String)],
    ) -> Result<Vec<SearchResult>, String> {
        match g.get_json::<SearchResp>(url, query_params).await {
            Ok(r) => Ok(r.results),
            Err(GoogleError::Http { status, .. }) if (400..500).contains(&status) => Ok(vec![]),
            Err(e) => Err(String::from(e)),
        }
    }

    let contacts_query = [
        ("query", query.to_string()),
        ("readMask", "names,emailAddresses,photos".to_string()),
    ];
    let directory_query = [
        ("query", query.to_string()),
        ("readMask", "names,emailAddresses,photos".to_string()),
        (
            "sources",
            "DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE".to_string(),
        ),
    ];
    let other_query = [
        ("query", query.to_string()),
        ("readMask", "names,emailAddresses,photos".to_string()),
    ];

    // Issue all three in parallel; collect results in priority order.
    let (contacts_res, directory_res, other_res) = tokio::join!(
        fetch_source(&g, SEARCH_CONTACTS_URL, &contacts_query),
        fetch_source(&g, SEARCH_DIRECTORY_URL, &directory_query),
        fetch_source(&g, SEARCH_OTHER_CONTACTS_URL, &other_query),
    );

    // Surface first hard error (auth / transient); 4xx already swallowed above.
    let contacts_results = contacts_res?;
    let directory_results = directory_res?;
    let other_results = other_res?;

    // Flatten results from each source in priority order into Recipient candidates.
    // One Person → one Recipient per emailAddress.
    Ok(collect_recipients(
        &[&contacts_results, &directory_results, &other_results],
        12,
    ))
}

/// Flatten ranked sources into deduped `Recipient` candidates, capped at `limit`.
/// One Person → one Recipient per non-empty emailAddress; lowercased-email dedup
/// keeps the first occurrence (caller controls priority by source order).
/// Extracted from `search_recipients` so the test fixture in
/// `recipient_tests.rs` can exercise the same dedup/flatten logic without going
/// through the async/network path.
pub fn collect_recipients(sources: &[&[SearchResult]], limit: usize) -> Vec<Recipient> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut out: Vec<Recipient> = Vec::new();

    for results in sources {
        for sr in *results {
            let Some(person) = sr.person.as_ref() else {
                continue;
            };
            let name = person
                .names
                .first()
                .map(|n| n.display_name.clone())
                .unwrap_or_default();
            let photo_url = person
                .photos
                .first()
                .map(|p| p.url.clone())
                .unwrap_or_default();

            for ea in &person.email_addresses {
                if ea.value.is_empty() {
                    continue;
                }
                let key = ea.value.to_lowercase();
                if seen.contains_key(&key) {
                    continue;
                }
                seen.insert(key, ());
                out.push(Recipient {
                    name: name.clone(),
                    email: ea.value.clone(),
                    photo_url: photo_url.clone(),
                });
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// Prime Google's server-side People cache by issuing an empty-query search
/// against personal contacts and the directory. Errors are intentionally
/// silenced — this is a best-effort warm-up, not a data fetch.
pub async fn warm_people_cache(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<(), String> {
    let g = Google::new(
        client_id.to_string(),
        client_secret.to_string(),
        refresh_token.to_string(),
    );

    let contacts_q = [
        ("query", String::new()),
        ("readMask", "emailAddresses".to_string()),
    ];
    let directory_q = [
        ("query", String::new()),
        ("readMask", "emailAddresses".to_string()),
        (
            "sources",
            "DIRECTORY_SOURCE_TYPE_DOMAIN_PROFILE".to_string(),
        ),
    ];

    // Best-effort: log errors but keep non-fatal.
    if let Err(e) = g
        .get_json::<SearchResp>(SEARCH_CONTACTS_URL, &contacts_q)
        .await
    {
        eprintln!("[people] warm_people_cache contacts warm-up failed: {e}");
    }
    if let Err(e) = g
        .get_json::<SearchResp>(SEARCH_DIRECTORY_URL, &directory_q)
        .await
    {
        eprintln!("[people] warm_people_cache directory warm-up failed: {e}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Contact groups (user labels in Google Contacts) — expand to member emails
// ---------------------------------------------------------------------------
