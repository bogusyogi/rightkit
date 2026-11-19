// rightkit-dictation-text-pipeline consumed by the exact registry pin in Cargo.toml, outside the
// workspace. Public API only.
use std::{fs, io};

use rightkit_app_settings::{FileFormat, Schema};
use rightkit_dictation_core::{app_paths, settings::SettingsBlob};
use rightkit_dictation_text_pipeline::settings::{
    self, configure_disk_settings, replace_runtime_config, DiskSettingsOptions,
};
use rightkit_dictation_text_pipeline::vocabulary::{
    built_in_terms, configure_product_terms, core_product_terms, ProductTerms,
};

fn check(ok: bool, what: &str) -> bool {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
    }
    ok
}

fn check_disk_settings() -> io::Result<bool> {
    let root = std::env::temp_dir().join(format!(
        "rightkit-dictation-settings-{}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let path = root.join("settings.json");
    let result = (|| -> io::Result<bool> {
        let first =
            br#"{"input_device":"disk-mic","diagnostic_audio_capture":"invalid","is_pro":true}"#;
        fs::write(&path, first)?;
        let mut all = check(
            app_paths::set_app_data_root(root.clone()),
            "fixture data root",
        );
        all &= check(
            configure_disk_settings(
                DiskSettingsOptions::new(Schema::new("consumer", 1)).format(FileFormat::Flat),
            ),
            "configure settings store once",
        );
        all &= check(
            settings::input_device().as_deref() == Some("disk-mic")
                && !settings::diagnostic_audio_capture()
                && !settings::is_pro()
                && fs::read(&path)?.as_slice() == first,
            "store repairs a field in memory and preserves valid settings without authorizing Pro",
        );
        fs::write(&path, br#"{"input_device":"external-mic"}"#)?;
        all &= check(
            settings::input_device().as_deref() == Some("external-mic"),
            "read-only store follows external edits",
        );
        replace_runtime_config(SettingsBlob {
            input_device: Some("native-mic".into()),
            is_pro: Some(true),
            ..SettingsBlob::default()
        });
        fs::write(
            &path,
            br#"{"input_device":"later-disk-mic","is_pro":false}"#,
        )?;
        all &= check(
            settings::input_device().as_deref() == Some("native-mic") && settings::is_pro(),
            "native configuration takes precedence over later disk edits",
        );
        Ok(all)
    })();
    if path.is_file() {
        fs::remove_file(path)?;
    }
    fs::remove_dir(root)?;
    result
}

fn main() -> io::Result<()> {
    let mut all = true;
    all &= check(
        built_in_terms().is_empty(),
        "no product terms until configured",
    );
    configure_product_terms(ProductTerms {
        built_in: &["NoteWell", "NoteWell stop"],
        core: &["NoteWell"],
    });
    all &= check(
        built_in_terms().len() == 2 && core_product_terms() == ["NoteWell"],
        "configured product terms read back",
    );
    if !(all & check_disk_settings()?) {
        std::process::exit(1);
    }
    Ok(())
}
