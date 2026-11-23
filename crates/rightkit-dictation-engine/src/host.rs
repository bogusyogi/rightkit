//! Startup configuration shared by host-branded dictation sidecars.

use std::path::PathBuf;

use anyhow::{anyhow, ensure};
use rightkit_dictation_command_host::host as command_host;
pub use rightkit_dictation_command_host::host::HostIdentity as CommandHostIdentity;
use rightkit_dictation_core::{app_paths, settings as core_settings};
use rightkit_dictation_inference::{configure_asr_stderr_prefix, file_transcribe};
use rightkit_dictation_runtime::{crash_capture, host as runtime_host};
pub use rightkit_dictation_runtime::{
    crash_capture::CrashOptions,
    host::{CliIdentity, EngineIdentity, ServiceLogNames},
};
use rightkit_dictation_text_pipeline::{l3_cleanup, settings, text_polish, vocabulary};
pub use rightkit_dictation_text_pipeline::{
    l3_cleanup::WireIdentity, settings::DiskSettingsOptions, vocabulary::ProductTerms,
};
pub use rightkit_text_polish::text_pipeline::BrandHomophones;

/// Values supplied by an installed sidecar before it starts any work.
pub struct EngineHostConfig<'a> {
    pub app_data_dir_name: &'static str,
    /// Explicit root when provided by the launcher. `None` uses the installed app data root.
    pub app_data_root: Option<PathBuf>,
    pub legal_contract: Option<&'a str>,
    pub disk_settings: Option<DiskSettingsOptions>,
    pub product_terms: ProductTerms,
    pub wire_identity: WireIdentity,
    pub brand_homophones: Option<BrandHomophones<'static>>,
    pub command_host: CommandHostIdentity,
    pub asr_stderr_prefix: &'static str,
    pub identity: EngineIdentity,
    pub cli: Option<CliIdentity>,
    pub service_logs: ServiceLogNames,
    pub crash_version: &'static str,
    pub crash_options: CrashOptions,
}

/// Install the host configuration before reads, workers or engine startup. An error
/// is a startup failure. The caller must exit on an error.
pub fn configure_host(config: EngineHostConfig<'_>) -> anyhow::Result<()> {
    if let Some(json) = config.legal_contract {
        core_settings::configure_legal_contract(json).map_err(|error| anyhow!(error))?;
    }
    ensure!(
        app_paths::configure_app_data_dir_name(config.app_data_dir_name),
        "dictation app data folder already configured"
    );
    let root = config
        .app_data_root
        .unwrap_or_else(app_paths::production_data_root);
    ensure!(
        app_paths::set_app_data_root(root),
        "dictation app data root already configured"
    );

    if let Some(options) = config.disk_settings {
        ensure!(
            settings::configure_disk_settings(options),
            "dictation settings store already configured"
        );
    }
    ensure!(
        vocabulary::configure_product_terms(config.product_terms),
        "dictation product vocabulary already configured"
    );
    ensure!(
        l3_cleanup::configure_wire_identity(config.wire_identity),
        "dictation cleanup identity already configured"
    );
    if let Some(rules) = config.brand_homophones {
        ensure!(
            text_polish::configure_brand_homophones(rules),
            "dictation homophones already configured"
        );
    }

    ensure!(
        command_host::configure(config.command_host)
            && command_host::set_paste_owner_name(config.command_host.display_name),
        "dictation command host already configured"
    );
    #[cfg(target_os = "macos")]
    ensure!(
        rightkit_automation::desktop::macos::set_own_window_owner_prefix(
            config.command_host.process_name_prefix,
        ),
        "dictation desktop identity already configured"
    );

    ensure!(
        file_transcribe::configure_app_display_name(config.identity.product_name)
            && configure_asr_stderr_prefix(config.asr_stderr_prefix),
        "dictation inference identity already configured"
    );
    ensure!(
        runtime_host::configure_identity(config.identity)
            && runtime_host::configure_service_log_names(config.service_logs),
        "dictation runtime already configured"
    );
    if let Some(cli) = config.cli {
        ensure!(
            runtime_host::configure_cli(cli),
            "dictation engine CLI already configured"
        );
    }
    ensure!(
        crash_capture::configure_crash_identity(config.identity.product_name, config.crash_version)
            && crash_capture::configure_crash_options(config.crash_options),
        "dictation crash capture already configured"
    );
    Ok(())
}
