//! Bundled sd.cpp image profiles (`engine: sdcpp`), installed by the local-image installer.
//!
//! The runtime reads a profile from the model root that
//! `rightkit_local_gen::paths::local_image` resolves. The installer
//! (`rightkit_local_gen::profiles`) writes to that same root, so what studio's
//! Models UI installs is what the runtime reads. This module routes studio's
//! install, cancel, uninstall and rollback commands to that installer and reuses
//! studio's install session and record plumbing from [`super::install`].
//!

use std::path::{Path, PathBuf};

use rightkit_local_gen::paths::{self, EnvValues};
use rightkit_local_gen::profiles::{self, InstallProgress, ProfileSpec};
use rightkit_local_gen::store::Store as LocalStore;
use rightkit_local_gen::{settings, LocalImagePaths};
use rightkit_media_gen::{Context, GenError, GenResult};
use serde_json::{json, Value};

use super::install::{self, Plan, Session};

/// The local-image paths the runtime resolves for this app data root.
pub fn runtime_paths(ctx: &Context) -> LocalImagePaths {
    let env = EnvValues::from_process();
    let saved = settings::read(&ctx.data_dir).model_root.map(PathBuf::from);
    paths::local_image(
        &ctx.data_dir,
        &paths::content_root(&env),
        saved.as_deref(),
        &env,
        &[],
    )
}

/// The bundled profile with this id.
pub fn spec(id: &str) -> GenResult<&'static ProfileSpec> {
    profiles::profile(id)
        .map_err(|e| GenError::new("invalid_input", e))?
        .ok_or_else(|| GenError::new("model_not_found", format!("no local-image profile: {id}")))
}

/// Components a profile still needs in the runtime root (empty when installed).
#[cfg(test)]
pub fn missing_components(root: &Path, spec: &ProfileSpec) -> Vec<String> {
    let dir = rightkit_local_gen::model_store::profile_dir(root, &spec.id, spec.revision);
    spec.components
        .iter()
        .filter(|c| c.required)
        .filter(|c| !dir.join(&c.file).is_file())
        .map(|c| c.file.clone())
        .collect()
}

/// Starts a background install of a profile through the local-image installer.
pub fn start(ctx: &Context, id: &str) -> GenResult<Value> {
    let spec = spec(id)?;
    let paths = runtime_paths(ctx);
    let total: u64 = spec
        .components
        .iter()
        .filter(|c| c.required)
        .filter_map(|c| c.bytes)
        .map(|b| b.max(0) as u64)
        .sum();
    let plan = Plan {
        revision: rightkit_local_gen::model_store::sdcpp_revision(spec.revision),
        dest: profiles::profile_dir(&paths, &spec.id, spec.revision),
        total,
        files: json!([]),
    };
    install::launch(ctx, id, plan, move |_ctx, _id, sess| {
        run_install(spec, &paths, sess)
    })
}

fn run_install(
    spec: &'static ProfileSpec,
    paths: &LocalImagePaths,
    sess: &Session,
) -> GenResult<PathBuf> {
    let db = LocalStore::open(&paths.database_path).map_err(|e| GenError::new("io", e))?;
    let records = db
        .components_for_profile(&spec.id, spec.revision)
        .map_err(|e| GenError::new("io", e))?;
    profiles::install_profile(
        paths,
        spec,
        &records,
        |record| db.upsert_component(record),
        |progress: &InstallProgress| {
            *sess.progress.lock().unwrap() = json!({
                "bytes_done": progress.bytes_done,
                "bytes_total": progress.bytes_total,
                "file": progress.file,
            });
        },
        std::sync::Arc::clone(&sess.pause),
        std::sync::Arc::clone(&sess.cancel),
    )
    .map_err(|message| {
        // The installer reports `code: message`; the code decides the install state.
        let code = message.split(':').next().unwrap_or("io").trim().to_string();
        GenError::new(code, message)
    })?;
    Ok(profiles::profile_dir(paths, &spec.id, spec.revision))
}

/// Cancels an sd.cpp install: a running one stops, a paused one has its partial discarded.
pub fn cancel(ctx: &Context, id: &str) -> GenResult<Value> {
    let spec = spec(id)?;
    let paths = runtime_paths(ctx);
    install::cancel(ctx, id, || {
        profiles::discard_partial(&paths, spec).map_err(|e| GenError::new("io", e))
    })
}

/// Removes an sd.cpp profile's installed files through the local-image installer.
pub fn uninstall(ctx: &Context, id: &str) -> GenResult<Value> {
    let spec = spec(id)?;
    let paths = runtime_paths(ctx);
    install::uninstall(ctx, id, || {
        profiles::uninstall_profile(&paths, spec).map_err(|e| GenError::new("io", e))
    })
}

/// sd.cpp profiles keep one fixed revision folder, so there is no earlier revision to roll back to.
pub fn rollback(id: &str) -> GenResult<Value> {
    Err(GenError::new(
        "no_rollback",
        format!("{id}: sd.cpp profiles keep one fixed revision folder; reinstall the catalogue revision instead"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_profile_resolves_to_a_spec() {
        let catalogue = profiles::catalogue().expect("catalogue");
        assert!(!catalogue.profiles.is_empty());
        for profile in &catalogue.profiles {
            assert_eq!(spec(&profile.id).expect("profile").id, profile.id);
        }
    }

    #[test]
    fn unknown_profile_is_not_found() {
        let err = spec("no-such-profile").err().expect("error");
        assert_eq!(err.code, "model_not_found");
    }

    #[test]
    fn rollback_is_refused_for_fixed_folders() {
        let err = rollback("any").err().expect("error");
        assert_eq!(err.code, "no_rollback");
    }

    #[test]
    fn missing_components_lists_absent_required_files() {
        let root =
            std::env::temp_dir().join(format!("rightkit-sdcpp-missing-{}", std::process::id()));
        let spec = spec(&profiles::catalogue().unwrap().profiles[0].id).unwrap();
        let missing = missing_components(&root, spec);
        let required = spec.components.iter().filter(|c| c.required).count();
        assert_eq!(missing.len(), required);
    }
}
