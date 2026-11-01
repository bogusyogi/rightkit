//! rightkit-app-settings: open a settings store on disk, update it, reopen it and read the update back; a damaged file is refused.
use rightkit_app_settings::{OnCorrupt, Schema, Store, StoreOptions};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
    font_px: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            font_px: 16,
        }
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("app_settings")?;
    let path = dir.join("settings.json");
    let opts = || StoreOptions::new(Schema::new("rke2e", 1));
    let store = Store::<Prefs>::open_path(&path, opts()).map_err(|e| format!("open: {e}"))?;
    if store.get() != Prefs::default() {
        return Err(format!(
            "a new store does not hold the defaults: {:?}",
            store.get()
        ));
    }
    store
        .update(|p| {
            p.theme = "dark".into();
            p.font_px = 20;
            Ok(())
        })
        .map_err(|e| format!("update: {e}"))?;
    drop(store);
    let reopened = Store::<Prefs>::open_path(&path, opts()).map_err(|e| format!("reopen: {e}"))?;
    let got = reopened.get();
    if got.theme != "dark" || got.font_px != 20 {
        return Err(format!("the update did not persist: {got:?}"));
    }
    drop(reopened);
    let damaged = dir.join("damaged.json");
    std::fs::write(&damaged, b"{ not json").map_err(|e| e.to_string())?;
    let refused =
        Store::<Prefs>::open_path(&damaged, opts().on_corrupt(OnCorrupt::Refuse)).is_err();
    if !refused {
        return Err("a damaged settings file was opened instead of refused".into());
    }
    Ok(json!({ "persisted": got.theme, "font_px": got.font_px, "damaged_refused": refused }))
}
