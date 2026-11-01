//! Per-platform request shapes for the publish core: path templates and the body field that carries
//! the caption. Two families: `container` (instagram, threads: create_container, poll_status,
//! publish, read_permalink) and `simple` (x, linkedin, youtube, bluesky, mastodon, facebook: create,
//! read_permalink). The paths are the ones a mock platform server serves;
//! a real provider adapter replaces this table, not the phase machine. Bluesky is the first such
//! adapter: `bluesky.rs` builds its requests, and its entry here carries no paths. Mastodon is the
//! second (`mastodon.rs`), LinkedIn the third (`linkedin.rs`), Threads the fourth (`threads.rs`, a
//! container-family platform whose phases skip `poll_status`), Facebook the fifth (`facebook.rs`, a
//! simple-family platform), X the sixth (`x.rs`, a simple-family platform) and Instagram the seventh
//! (`instagram.rs`, a container-family platform with the full `poll_status` phase) and YouTube the eighth
//! (`youtube.rs`, a simple-family platform), with the same empty entry. No platform is driven by the generic
//! paths below any more; they remain for a platform added without a provider module.
use rightkit_http::percent_encode;
use serde_json::{json, Map, Value};

use crate::wire::PublishPayload;

pub struct Spec {
    pub platform: &'static str,
    /// True for the container family.
    pub container: bool,
    /// The request body field that carries the caption.
    pub text_field: &'static str,
    create: &'static str,
    read: &'static str,
}

static SPECS: [Spec; 10] = [
    // Instagram is driven by instagram.rs (`<base>/v<version>/<user>/media`, `.../media_publish` and
    // `<base>/v<version>/<container or media id>`); this entry only says it starts at `create_container`, so it
    // carries no paths. The mock's generic `/instagram/{a}/media` container routes stay served (the analytics
    // pull still reads `/instagram/{a}/media/{id}/insights`), but the engine no longer posts to them.
    Spec {
        platform: "instagram",
        container: true,
        text_field: "caption",
        create: "",
        read: "",
    },
    // Threads is driven by threads.rs (`<base>/v1.0/<user>/threads`, `.../threads_publish` and
    // `<base>/v1.0/<media id>`); this entry only says it starts at `create_container`, so it carries no
    // paths. The mock's generic `/threads/{a}/...` container routes stay served, but the engine no longer
    // uses them.
    Spec {
        platform: "threads",
        container: true,
        text_field: "text",
        create: "",
        read: "",
    },
    // X is driven by x.rs (`<base>/2/tweets` and `<base>/2/tweets/<id>`); this entry only says it is a
    // simple-family platform, so it carries no paths. The mock's generic `/x/{a}/tweets` routes stay served
    // (the analytics pull still reads `/x/{a}/tweets/{id}/analytics`), but the engine no longer posts to them.
    Spec {
        platform: "x",
        container: false,
        text_field: "text",
        create: "",
        read: "",
    },
    // LinkedIn is driven by linkedin.rs (`<base>/rest/posts`, with the version headers); this entry only
    // says it is a simple-family platform, so it carries no paths.
    Spec {
        platform: "linkedin",
        container: false,
        text_field: "text",
        create: "",
        read: "",
    },
    // YouTube is driven by youtube.rs (the resumable upload at `<base>/upload/youtube/v3/videos` and the
    // `videos.list` read-back); this entry only says it is a simple-family platform, so it carries no paths.
    Spec {
        platform: "youtube",
        container: false,
        text_field: "title",
        create: "",
        read: "",
    },
    // Bluesky is driven by bluesky.rs (XRPC createRecord and getRecord at `<base>/xrpc/...`); this entry
    // only says it is a simple-family platform, so it carries no paths.
    Spec {
        platform: "bluesky",
        container: false,
        text_field: "text",
        create: "",
        read: "",
    },
    // Mastodon is driven by mastodon.rs (`<base>/api/v1/statuses` and `<base>/api/v2/instance`); this
    // entry only says it is a simple-family platform, so it carries no paths.
    Spec {
        platform: "mastodon",
        container: false,
        text_field: "text",
        create: "",
        read: "",
    },
    // Facebook is driven by facebook.rs (`<base>/v<version>/<page>/feed` and `<base>/v<version>/<post id>`);
    // this entry only says it is a simple-family platform, so it carries no paths.
    Spec {
        platform: "facebook",
        container: false,
        text_field: "message",
        create: "",
        read: "",
    },
    // TikTok is driven by tiktok.rs (the container family: create_container, poll_status, read_permalink).
    Spec {
        platform: "tiktok",
        container: true,
        text_field: "title",
        create: "",
        read: "",
    },
    // Pinterest is driven by pinterest.rs (`<base>/v5/pins`); a simple-family platform with no paths here.
    Spec {
        platform: "pinterest",
        container: false,
        text_field: "description",
        create: "",
        read: "",
    },
];

/// The spec for a platform, or None when the publish core does not drive it.
pub fn spec(platform: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.platform == platform)
}

/// The first phase of a new job.
pub fn first_phase(spec: &Spec) -> &'static str {
    if spec.container {
        "create_container"
    } else {
        "create"
    }
}

/// Phases whose request creates something on the platform. An ambiguous result on one of these is
/// `outcome_unknown`, never a blind retry.
pub fn is_write_phase(phase: &str) -> bool {
    matches!(phase, "create_container" | "create" | "publish")
}

fn fill(template: &str, account: &str, id: &str) -> String {
    template
        .replace("{a}", &percent_encode(account))
        .replace("{id}", &percent_encode(id))
}

impl Spec {
    pub fn create_path(&self, account: &str) -> String {
        fill(self.create, account, "")
    }

    pub fn read_path(&self, account: &str, post_id: &str) -> String {
        fill(self.read, account, post_id)
    }

    pub fn recent_path(&self, account: &str) -> String {
        format!("/{}/{}/recent", self.platform, percent_encode(account))
    }

    /// The body of the create request.
    pub fn create_body(&self, payload: &PublishPayload) -> Value {
        let mut body = Map::new();
        body.insert(
            self.text_field.to_string(),
            Value::String(payload.caption.clone()),
        );
        let media: Vec<Value> = payload
            .media
            .iter()
            .map(|m| json!({ "hash": m.hash, "kind": m.kind, "url": m.url }))
            .collect();
        if !media.is_empty() {
            body.insert("media".to_string(), Value::Array(media));
        }
        if payload.settings.as_object().is_some_and(|o| !o.is_empty()) {
            body.insert("settings".to_string(), payload.settings.clone());
        }
        Value::Object(body)
    }
}
