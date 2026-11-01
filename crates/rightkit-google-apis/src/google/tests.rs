use super::*;

#[test]
fn error_classes_are_body_free_strings() {
    assert_eq!(GoogleError::Auth("x".into()).class(), "auth");
    assert_eq!(GoogleError::RateLimited("x".into()).class(), "rate_limited");
    assert_eq!(GoogleError::Transient("x".into()).class(), "transient");
    assert_eq!(
        GoogleError::Http {
            status: 404,
            body: "x".into()
        }
        .class(),
        "http"
    );
    assert_eq!(GoogleError::Parse("x".into()).class(), "parse");
}

#[test]
fn token_cache_key_is_stable_and_secret_free() {
    let g = Google::new("client".into(), "secret".into(), "refresh".into());
    let key = g.token_cache_key();
    assert_eq!(key.len(), 64);
    assert_eq!(
        key,
        Google::new("client".into(), "secret".into(), "refresh".into()).token_cache_key()
    );
    assert_ne!(
        key,
        Google::new("client".into(), "secret".into(), "other".into()).token_cache_key()
    );
    assert!(!key.contains("secret"));
    assert!(!key.contains("refresh"));
}

#[test]
fn token_cache_key_keeps_its_exact_digest() {
    assert_eq!(
        token_cache_key("id", "sec", "rt"),
        "fc27b0c404afeef1acabb94fb9137ca54812a4e9792ce6115c7797359ab39938"
    );
}

#[test]
fn matching_google_clients_share_access_tokens() {
    let a = Google::new("client".into(), "secret".into(), "refresh".into());
    let b = Google::new("client".into(), "secret".into(), "refresh".into());
    let c = Google::new("client".into(), "secret".into(), "other".into());
    assert!(Arc::ptr_eq(&a.tokens().unwrap(), &b.tokens().unwrap()));
    assert!(!Arc::ptr_eq(&a.tokens().unwrap(), &c.tokens().unwrap()));
}

#[test]
fn forgetting_a_refresh_token_drops_its_access_tokens() {
    let a = Google::new("client".into(), "secret".into(), "forgotten".into());
    let held = a.tokens().unwrap();
    forget_cached_access_token("client", "secret", "forgotten");
    assert!(!Arc::ptr_eq(&held, &a.tokens().unwrap()));
}

#[test]
fn proxy_fallback_tries_the_environment_proxy_when_the_system_one_is_refused() {
    let tried = std::cell::Cell::new(0);
    let built = build_with_proxy_fallback(|proxy| {
        tried.set(tried.get() + 1);
        match proxy {
            ProxyMode::System => Err(HttpError::InvalidRequest("unsupported".into())),
            ProxyMode::Environment => Ok("environment"),
            _ => Err(HttpError::InvalidRequest("unexpected mode".into())),
        }
    });
    assert_eq!(built.unwrap(), "environment");
    assert_eq!(tried.get(), 2);

    tried.set(0);
    let built = build_with_proxy_fallback(|proxy| {
        tried.set(tried.get() + 1);
        assert!(matches!(proxy, ProxyMode::System));
        Ok::<_, HttpError>("system")
    });
    assert_eq!(built.unwrap(), "system");
    assert_eq!(tried.get(), 1);
}

#[test]
fn query_is_appended_form_encoded_and_respects_existing_query() {
    let q = [("q", "a b&c".to_string()), ("n", "2".to_string())];
    assert_eq!(
        with_query("https://x.test/v1", &q),
        "https://x.test/v1?q=a+b%26c&n=2"
    );
    assert_eq!(
        with_query("https://x.test/v1?k=1", &q[1..]),
        "https://x.test/v1?k=1&n=2"
    );
    assert_eq!(with_query("https://x.test/v1", &[]), "https://x.test/v1");
}

#[test]
fn shared_http_client_builds_with_the_shared_policy() {
    if let Err(e) = http_client() {
        panic!("shared client failed to build: {e}");
    }
}

fn transport(connect_phase: bool, phase: rightkit_http::Phase) -> HttpError {
    HttpError::Transport {
        message: "reset".into(),
        connect_phase,
        phase,
    }
}

#[test]
fn a_post_that_failed_after_connect_is_uncertain_and_never_transient() {
    use rightkit_http::Phase;
    for error in [
        transport(false, Phase::Headers),
        transport(false, Phase::Body),
        HttpError::Timeout {
            message: "response timed out".into(),
            phase: Phase::Headers,
        },
        HttpError::Cancelled,
    ] {
        let classified = classify_send_error(Method::Post, &error);
        assert!(matches!(classified, GoogleError::Uncertain(_)), "{error}");
        assert!(classified.delivery_uncertain());
        assert_eq!(classified.class(), "uncertain");
    }
}

#[test]
fn a_connect_phase_failure_sent_nothing_so_it_stays_transient() {
    use rightkit_http::Phase;
    let classified = classify_send_error(Method::Post, &transport(true, Phase::Connect));
    assert!(matches!(classified, GoogleError::Transient(_)));
    assert!(!classified.delivery_uncertain());
    let timeout = HttpError::Timeout {
        message: "connect timed out".into(),
        phase: Phase::Connect,
    };
    assert!(matches!(
        classify_send_error(Method::Patch, &timeout),
        GoogleError::Transient(_)
    ));
}

#[test]
fn an_idempotent_request_stays_transient_after_connect() {
    use rightkit_http::Phase;
    let classified = classify_send_error(Method::Get, &transport(false, Phase::Body));
    assert!(matches!(classified, GoogleError::Transient(_)));
}

#[test]
fn a_5xx_answer_counts_as_uncertain_delivery_for_a_send() {
    assert!(GoogleError::Http {
        status: 503,
        body: "x".into()
    }
    .delivery_uncertain());
    assert!(!GoogleError::Http {
        status: 400,
        body: "x".into()
    }
    .delivery_uncertain());
    assert!(!GoogleError::RateLimited("x".into()).delivery_uncertain());
}
