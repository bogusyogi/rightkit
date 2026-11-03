//! Tracked short links: destination checks, UTM tagging, deterministic link ids, and the pieces of
//! a public redirect route (link-id shape, redirect-target check, referrer host reduction).

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::protocol::PLATFORMS;
use rightkit_social_publish::util::{
    bad, in_txn, non_empty, parse_input, require_channel, to_value, WriteTxn,
};
use serde_json::Value;

use crate::protocol::{
    AttributionRow, Campaign, CampaignListInput, CampaignListOutput, CampaignPutInput,
    LinkAttributionInput, LinkAttributionOutput, LinkCreateInput, LinkCreateOutput, LinkGetInput,
    LinkKey, LinkListInput, LinkListOutput, LinkRecordVisitInput, LinkRecordVisitOutput, LinkVisit,
    Scope, TrackedLink,
};
use crate::Fail;

/// Longest slug accepted (campaign, variant and UTM values).
pub const SLUG_MAX: usize = 64;
/// `utm_medium` when the policy names none.
pub const DEFAULT_MEDIUM: &str = "social";

/// A lower-case slug: 1 to `SLUG_MAX` characters from `a-z`, `0-9`, `_` and `-`. Uppercase is refused,
/// not folded, so the stored value is always the value the caller sent.
pub fn slug(value: &str, name: &str) -> Result<(), Fail> {
    let ok = !value.is_empty()
        && value.len() <= SLUG_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{name} must be a lower-case slug of 1 to {SLUG_MAX} characters from a-z, 0-9, _ and -, got {value}"
        )))
    }
}

/// The https link check for tracked-link destinations (`name` leads each message). Scheme,
/// credentials and the public-host test are `rightkit_http::validate_public_https_url`; on top of
/// it, whitespace/control characters and any port are refused.
pub fn public_https(url: &str, name: &str) -> Result<(), Fail> {
    use rightkit_http::UrlPolicyError;
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad(format!(
            "{name} must not contain spaces or control characters"
        )));
    }
    rightkit_http::validate_public_https_url(url).map_err(|why| match why {
        UrlPolicyError::NotHttps => ("url_not_https", format!("{name} must be an https URL")),
        other => bad(format!("{name} is not a plain public https host: {other}")),
    })?;
    let authority = url[8..].split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains(':') {
        return Err(bad(format!(
            "{name} must name a plain host, with no user, port or credentials"
        )));
    }
    Ok(())
}

/// Appends the UTM parameters to the destination. The query goes before any fragment, and the join
/// character follows whatever the destination already ends with. A destination that already names a
/// `utm_` key is `utm_conflict`: the policy never duplicates a key.
pub fn final_url(destination: &str, params: &[(&str, &str)]) -> Result<String, Fail> {
    let (base, fragment) = match destination.find('#') {
        Some(i) => (&destination[..i], &destination[i..]),
        None => (destination, ""),
    };
    if let Some(i) = base.find('?') {
        for part in base[i + 1..].split('&') {
            let key = part.split('=').next().unwrap_or("");
            if key.to_ascii_lowercase().starts_with("utm_") {
                return Err((
                    "utm_conflict",
                    format!(
                        "destination already has {key}; remove it from the destination or the utm policy cannot apply"
                    ),
                ));
            }
        }
    }
    let joiner = if !base.contains('?') {
        "?"
    } else if base.ends_with('?') || base.ends_with('&') {
        ""
    } else {
        "&"
    };
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Ok(format!("{base}{joiner}{}{fragment}", query.join("&")))
}

/// UTM overrides a link request may carry. Absent fields take their defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UtmPolicy {
    /// `utm_source`; default: the platform.
    #[serde(default)]
    pub source: Option<String>,
    /// `utm_medium`; default: [`DEFAULT_MEDIUM`].
    #[serde(default)]
    pub medium: Option<String>,
    /// `utm_campaign`; default: the campaign id.
    #[serde(default)]
    pub campaign: Option<String>,
    /// `utm_content`; default: the variant id.
    #[serde(default)]
    pub content: Option<String>,
}

/// The four UTM values of one link, after defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utm {
    /// `utm_source`.
    pub source: String,
    /// `utm_medium`.
    pub medium: String,
    /// `utm_campaign`.
    pub campaign: String,
    /// `utm_content`.
    pub content: String,
}

impl Utm {
    /// Apply `policy`: each given value (which must be a slug), else the default for that field.
    pub fn resolve(
        policy: Option<&UtmPolicy>,
        platform: &str,
        campaign: &str,
        variant_id: &str,
    ) -> Result<Utm, Fail> {
        let given = |value: Option<&String>, name: &str| -> Result<Option<String>, Fail> {
            match value {
                Some(v) => slug(v, name).map(|()| Some(v.clone())),
                None => Ok(None),
            }
        };
        let p = policy.cloned().unwrap_or_default();
        Ok(Utm {
            source: given(p.source.as_ref(), "utm_source")?.unwrap_or_else(|| platform.to_string()),
            medium: given(p.medium.as_ref(), "utm_medium")?
                .unwrap_or_else(|| DEFAULT_MEDIUM.to_string()),
            campaign: given(p.campaign.as_ref(), "utm_campaign")?
                .unwrap_or_else(|| campaign.to_string()),
            content: given(p.content.as_ref(), "utm_content")?
                .unwrap_or_else(|| variant_id.to_string()),
        })
    }

    /// The `(key, value)` query pairs, in the order they are appended.
    pub fn params(&self) -> [(&'static str, &str); 4] {
        [
            ("utm_source", self.source.as_str()),
            ("utm_medium", self.medium.as_str()),
            ("utm_campaign", self.campaign.as_str()),
            ("utm_content", self.content.as_str()),
        ]
    }
}

/// `lnk_` plus 32 hex digits (SHA-256, first 128 bits) over the seven inputs joined with `0x1f`.
pub fn link_id_of(
    venture: &str,
    channel: &str,
    account: &str,
    campaign_id: &str,
    variant_id: &str,
    platform: &str,
    destination: &str,
) -> String {
    let parts = [
        venture,
        channel,
        account,
        campaign_id,
        variant_id,
        platform,
        destination,
    ];
    let mut bytes = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            bytes.push(0x1f);
        }
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("lnk_{}", &rightkit_fs::digest::sha256_bytes(&bytes)[..32])
}

/// `lnk_` plus 32 hex digits, the only shape `link.create` makes. Anything else is refused before the store.
pub fn is_link_id(text: &str) -> bool {
    text.strip_prefix("lnk_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// The host of a `Referer` header value, lower-case, with no scheme, user info, port, path or query; `None` when
/// it is not a plain host name. Only this much of a referrer is ever stored.
pub fn referrer_host(referer: &str) -> Option<String> {
    let rest = referer.trim().split_once("://").map(|(_, r)| r)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.bytes().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    let host = host.to_ascii_lowercase();
    let plain = !host.is_empty()
        && host.len() <= 100
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-');
    plain.then_some(host)
}

/// Only an `http` or `https` URL with no space or control character may be a redirect target.
pub fn redirect_target_ok(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && lower.len() > 8
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &["campaign.put", "link.create", "link.record_visit"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "campaign.put",
    "campaign.list",
    "link.create",
    "link.get",
    "link.list",
    "link.record_visit",
    "link.attribution",
];

/// What the host app provides: the `campaigns`, `tracked_links` and `link_visits` tables, each keyed by
/// venture, channel and account. Every method is one read or one write on the host's own database, on the
/// caller's transaction. No method makes a network call and nothing here redirects a visitor.
pub trait LinksStore: WriteTxn {
    /// Platforms accepted besides the account platforms (those that only have drafts, no account).
    fn draft_only_platforms(&self) -> &[&str];
    /// Whether a calendar entry with this id exists in the scope.
    fn entry_known(&self, scope: &Scope, entry_id: &str) -> Result<bool, Fail>;
    /// The stored campaign, if any.
    fn load_campaign(&self, scope: &Scope, campaign_id: &str) -> Result<Option<Campaign>, Fail>;
    /// Stores a new campaign at revision 1 (`created_at` and `updated_at` are the campaign's).
    fn insert_campaign(&self, campaign: &Campaign) -> Result<(), Fail>;
    /// Writes an edited campaign (its `revision` is the new one) only where the stored revision equals
    /// `expected_revision`. Returns whether a row changed.
    fn update_campaign(&self, campaign: &Campaign, expected_revision: i64) -> Result<bool, Fail>;
    /// Every campaign in the scope, ordered by campaign id.
    fn list_campaigns(&self, scope: &Scope) -> Result<Vec<Campaign>, Fail>;
    /// The stored link, if any.
    fn load_link(&self, scope: &Scope, link_id: &str) -> Result<Option<TrackedLink>, Fail>;
    /// Stores one link.
    fn insert_link(&self, link: &TrackedLink) -> Result<(), Fail>;
    /// Links in the scope matching the filters (an absent filter matches all), ordered by campaign id,
    /// variant id, platform and link id.
    fn list_links(
        &self,
        scope: &Scope,
        campaign_id: Option<&str>,
        calendar_entry_id: Option<&str>,
    ) -> Result<Vec<TrackedLink>, Fail>;
    /// The stored visit, if any.
    fn load_visit(&self, scope: &Scope, visit_id: &str) -> Result<Option<LinkVisit>, Fail>;
    /// Stores one visit.
    fn insert_visit(&self, visit: &LinkVisit) -> Result<(), Fail>;
    /// Per link of one campaign, the count of its visits, ordered by variant id, platform and link id.
    fn attribution_rows(
        &self,
        scope: &Scope,
        campaign_id: &str,
    ) -> Result<Vec<AttributionRow>, Fail>;
}

impl UtmPolicy {
    /// Checks each override that is present: it must be a lower-case slug.
    pub fn validate(&self) -> Result<(), Fail> {
        for (name, value) in [
            ("utm_source", self.source.as_deref()),
            ("utm_medium", self.medium.as_deref()),
            ("utm_campaign", self.campaign.as_deref()),
            ("utm_content", self.content.as_deref()),
        ] {
            if let Some(v) = value {
                slug(v, name)?;
            }
        }
        Ok(())
    }
}

fn conflict(msg: impl Into<String>) -> Fail {
    ("revision_conflict", msg.into())
}

fn require_entry(store: &dyn LinksStore, scope: &Scope, entry_id: &str) -> Result<(), Fail> {
    if !store.entry_known(scope, entry_id)? {
        return Err((
            "not_found",
            format!("no calendar entry {entry_id} for this scope"),
        ));
    }
    Ok(())
}

/// A platform is an account platform or a draft-only platform, as the drafts store accepts.
fn platform_ok(store: &dyn LinksStore, platform: &str) -> Result<(), Fail> {
    let draft_only = store.draft_only_platforms();
    if PLATFORMS.contains(&platform) || draft_only.contains(&platform) {
        return Ok(());
    }
    let allowed: Vec<&str> = PLATFORMS.iter().chain(draft_only.iter()).copied().collect();
    Err(bad(format!(
        "platform must be one of {}, got {platform}",
        allowed.join(", ")
    )))
}

/// Trimmed text, or None when absent or blank.
fn trimmed_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The clock for this command (`now` when given) and the same instant as stored text.
fn stamp_of(now: Option<&str>) -> Result<String, Fail> {
    let secs = time::now_secs(now)?;
    Ok(time::format_instant(secs))
}

fn check_revision(id: &str, current: i64, expected: i64) -> Result<(), Fail> {
    if current != expected {
        return Err(conflict(format!(
            "{id} is at revision {current}; expected_revision {expected} is stale"
        )));
    }
    Ok(())
}

fn campaign_not_found(campaign_id: &str) -> Fail {
    (
        "not_found",
        format!("no campaign {campaign_id} for this scope"),
    )
}

fn link_not_found(link_id: &str) -> Fail {
    (
        "not_found",
        format!("no tracked link {link_id} for this scope"),
    )
}

/// Names one link: by its id, or by the inputs it was derived from. Exactly one must be given.
fn resolve_link_id(
    store: &dyn LinksStore,
    scope: &Scope,
    link_id: Option<&str>,
    key: Option<&LinkKey>,
) -> Result<String, Fail> {
    match (link_id, key) {
        (Some(id), None) => {
            non_empty(id, "link_id")?;
            Ok(id.trim().to_string())
        }
        (None, Some(k)) => {
            slug(&k.campaign_id, "campaign_id")?;
            slug(&k.variant_id, "variant_id")?;
            platform_ok(store, &k.platform)?;
            let destination = k.destination.trim();
            non_empty(destination, "destination")?;
            Ok(link_id_of(
                &scope.venture,
                &scope.channel,
                &scope.account,
                &k.campaign_id,
                &k.variant_id,
                &k.platform,
                destination,
            ))
        }
        _ => Err(bad("give exactly one of link_id or link")),
    }
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(store: &dyn LinksStore, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "campaign.put" => campaign_put(store, input),
        "campaign.list" => campaign_list(store, input),
        "link.create" => link_create(store, input),
        "link.get" => link_get(store, input),
        "link.list" => link_list(store, input),
        "link.record_visit" => link_record_visit(store, input),
        "link.attribution" => link_attribution(store, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `campaign.put`: create or edit one campaign. A slug `utm_campaign` is validated when given. Editing
/// keeps the stored `utm_campaign` when the request omits it.
fn campaign_put(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: CampaignPutInput = parse_input(input, "campaign.put")?;
    slug(&req.campaign_id, "campaign_id")?;
    let name = req.name.trim().to_string();
    non_empty(&name, "name")?;
    if let Some(utm) = req.utm_campaign.as_deref() {
        slug(utm, "utm_campaign")?;
    }
    let stamp = stamp_of(req.now.as_deref())?;

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        match store.load_campaign(&req.scope, &req.campaign_id)? {
            None => {
                match req.expected_revision {
                    None | Some(0) => {}
                    Some(n) => {
                        return Err(conflict(format!(
                        "campaign {} does not exist (revision 0); expected_revision {n} is stale",
                        req.campaign_id
                    )))
                    }
                }
                let utm_campaign = req
                    .utm_campaign
                    .clone()
                    .unwrap_or_else(|| req.campaign_id.clone());
                store.insert_campaign(&Campaign {
                    scope: req.scope.clone(),
                    campaign_id: req.campaign_id.clone(),
                    name: name.clone(),
                    utm_campaign,
                    revision: 1,
                    created_at: stamp.clone(),
                    updated_at: stamp.clone(),
                })?;
            }
            Some(cur) => {
                let expected = req.expected_revision.ok_or_else(|| {
                    (
                        "revision_required",
                        format!(
                            "campaign {} exists: expected_revision is required",
                            req.campaign_id
                        ),
                    )
                })?;
                check_revision(
                    &format!("campaign {}", req.campaign_id),
                    cur.revision,
                    expected,
                )?;
                let utm_campaign = req
                    .utm_campaign
                    .clone()
                    .unwrap_or_else(|| cur.utm_campaign.clone());
                let changed = store.update_campaign(
                    &Campaign {
                        scope: req.scope.clone(),
                        campaign_id: req.campaign_id.clone(),
                        name: name.clone(),
                        utm_campaign,
                        revision: cur.revision + 1,
                        created_at: cur.created_at.clone(),
                        updated_at: stamp.clone(),
                    },
                    expected,
                )?;
                if !changed {
                    return Err(conflict(format!(
                        "campaign {} changed while it was written",
                        req.campaign_id
                    )));
                }
            }
        }
        store
            .load_campaign(&req.scope, &req.campaign_id)?
            .ok_or_else(|| campaign_not_found(&req.campaign_id))
    })?;
    to_value(&saved)
}

/// `campaign.list`: every campaign in one scope, ordered by campaign id.
fn campaign_list(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: CampaignListInput = parse_input(input, "campaign.list")?;
    require_channel(store, &req.scope)?;
    let campaigns = store.list_campaigns(&req.scope)?;
    to_value(&CampaignListOutput {
        scope: req.scope,
        campaigns,
    })
}

/// `link.create`: cuts one link. Checks run in this order and the first failure writes nothing:
/// campaign and variant slugs, platform, destination (non-empty, https, plain host), UTM overrides,
/// campaign exists in scope, calendar entry exists in scope when given, no `utm_` key already on the
/// destination, then the stored link: same inputs and same final URL and entry return it unchanged.
fn link_create(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: LinkCreateInput = parse_input(input, "link.create")?;
    slug(&req.campaign_id, "campaign_id")?;
    slug(&req.variant_id, "variant_id")?;
    platform_ok(store, &req.platform)?;
    let destination = req.destination.trim().to_string();
    non_empty(&destination, "destination")?;
    public_https(&destination, "destination")?;
    if let Some(policy) = req.utm.as_ref() {
        policy.validate()?;
    }
    let calendar_entry_id = trimmed_opt(req.calendar_entry_id.as_deref());
    let stamp = stamp_of(req.now.as_deref())?;

    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let campaign = store
            .load_campaign(&req.scope, &req.campaign_id)?
            .ok_or_else(|| campaign_not_found(&req.campaign_id))?;
        if let Some(entry) = calendar_entry_id.as_deref() {
            require_entry(store, &req.scope, entry)?;
        }
        let utm = Utm::resolve(
            req.utm.as_ref(),
            &req.platform,
            &campaign.utm_campaign,
            &req.variant_id,
        )?;
        let url = final_url(&destination, &utm.params())?;
        let id = link_id_of(
            &req.scope.venture,
            &req.scope.channel,
            &req.scope.account,
            &req.campaign_id,
            &req.variant_id,
            &req.platform,
            &destination,
        );

        let candidate = TrackedLink {
            scope: req.scope.clone(),
            link_id: id.clone(),
            campaign_id: req.campaign_id.clone(),
            variant_id: req.variant_id.clone(),
            platform: req.platform.clone(),
            destination: destination.clone(),
            calendar_entry_id: calendar_entry_id.clone(),
            utm_source: utm.source.clone(),
            utm_medium: utm.medium.clone(),
            utm_campaign: utm.campaign.clone(),
            utm_content: utm.content.clone(),
            final_url: url,
            created_at: stamp.clone(),
        };

        if let Some(stored) = store.load_link(&req.scope, &id)? {
            if stored.final_url != candidate.final_url
                || stored.calendar_entry_id != candidate.calendar_entry_id
            {
                return Err((
                    "link_conflict",
                    format!(
                        "link {id} exists with final URL {} and calendar entry {}; the request names {} and {}",
                        stored.final_url,
                        stored.calendar_entry_id.as_deref().unwrap_or("none"),
                        candidate.final_url,
                        candidate.calendar_entry_id.as_deref().unwrap_or("none")
                    ),
                ));
            }
            return Ok(LinkCreateOutput {
                link: stored,
                created: false,
            });
        }

        store.insert_link(&candidate)?;
        let saved = store
            .load_link(&req.scope, &id)?
            .ok_or_else(|| link_not_found(&id))?;
        Ok(LinkCreateOutput {
            link: saved,
            created: true,
        })
    })?;
    to_value(&out)
}

/// `link.get`: one link in scope, by id or by its inputs.
fn link_get(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: LinkGetInput = parse_input(input, "link.get")?;
    let id = resolve_link_id(store, &req.scope, req.link_id.as_deref(), req.link.as_ref())?;
    require_channel(store, &req.scope)?;
    let link = store
        .load_link(&req.scope, &id)?
        .ok_or_else(|| link_not_found(&id))?;
    to_value(&link)
}

/// `link.list`: links in one scope, filtered by campaign and/or calendar entry. One filter is required.
fn link_list(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: LinkListInput = parse_input(input, "link.list")?;
    if req.campaign_id.is_none() && req.calendar_entry_id.is_none() {
        return Err(bad("link.list needs campaign_id or calendar_entry_id"));
    }
    if let Some(campaign) = req.campaign_id.as_deref() {
        slug(campaign, "campaign_id")?;
    }
    require_channel(store, &req.scope)?;
    let links = store.list_links(
        &req.scope,
        req.campaign_id.as_deref(),
        req.calendar_entry_id.as_deref(),
    )?;
    to_value(&LinkListOutput {
        scope: req.scope,
        links,
    })
}

/// `link.record_visit`: appends one visit. A `visit_id` already stored is ignored and the stored visit is
/// returned with `recorded` false; the same visit id on another link is `visit_conflict`.
fn link_record_visit(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: LinkRecordVisitInput = parse_input(input, "link.record_visit")?;
    let visit_id = req.visit_id.trim().to_string();
    non_empty(&visit_id, "visit_id")?;
    let source_note = req.source_note.trim().to_string();
    let visited_at = req
        .visited_at
        .as_deref()
        .map(|s| time::parse_instant(s, "visited_at").map(time::format_instant))
        .transpose()?;
    let stamp = stamp_of(req.now.as_deref())?;
    let visited_at = visited_at.unwrap_or(stamp);

    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let link_id =
            resolve_link_id(store, &req.scope, req.link_id.as_deref(), req.link.as_ref())?;
        store
            .load_link(&req.scope, &link_id)?
            .ok_or_else(|| link_not_found(&link_id))?;

        if let Some(stored) = store.load_visit(&req.scope, &visit_id)? {
            if stored.link_id != link_id {
                return Err((
                    "visit_conflict",
                    format!(
                        "visit {visit_id} is already recorded for link {}",
                        stored.link_id
                    ),
                ));
            }
            return Ok(LinkRecordVisitOutput {
                visit: stored,
                recorded: false,
            });
        }

        store.insert_visit(&LinkVisit {
            scope: req.scope.clone(),
            visit_id: visit_id.clone(),
            link_id: link_id.clone(),
            visited_at: visited_at.clone(),
            source_note: source_note.clone(),
        })?;
        let saved = store
            .load_visit(&req.scope, &visit_id)?
            .ok_or_else(|| ("not_found", format!("visit {visit_id} was not stored")))?;
        Ok(LinkRecordVisitOutput {
            visit: saved,
            recorded: true,
        })
    })?;
    to_value(&out)
}

/// `link.attribution`: per link of one campaign, the plain count of visits, with the variant, account,
/// platform and calendar entry that cut it. Counts only. The campaign must exist in scope.
fn link_attribution(store: &dyn LinksStore, input: Value) -> Result<Value, Fail> {
    let req: LinkAttributionInput = parse_input(input, "link.attribution")?;
    slug(&req.campaign_id, "campaign_id")?;
    require_channel(store, &req.scope)?;
    store
        .load_campaign(&req.scope, &req.campaign_id)?
        .ok_or_else(|| campaign_not_found(&req.campaign_id))?;
    let links = store.attribution_rows(&req.scope, &req.campaign_id)?;
    let total_visits = links.iter().map(|l| l.visits).sum();
    to_value(&LinkAttributionOutput {
        scope: req.scope,
        campaign_id: req.campaign_id,
        links,
        total_visits,
    })
}
