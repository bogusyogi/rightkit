//! ffmpeg/ffprobe resolution. Only the engine bundle's `<resource_dir>/runtime/`
//! is ever used. Test-mode builds additionally honour `CUTRIGHT_FFMPEG` /
//! `CUTRIGHT_FFPROBE` so development can run before the editing ffmpeg is
//! bundled; release builds compile that path out.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_ffmpeg as rk;

use crate::error::ProjectError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Bounds catalog verification and its build probe.
pub(crate) const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Bounds the encoder listing and the hardware qualification encode.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Refuses a bundled tool that is not the build RightKit approves.
fn require_approved(path: &Path) -> Result<(), ProjectError> {
    use rightkit_ffmpeg as rk;
    let result = if path.file_stem().is_some_and(|stem| stem == "ffprobe") {
        rk::verify_ffprobe_or_development(path, rk::DECODE_VARIANT, VERIFY_TIMEOUT).map(|_| ())
    } else {
        rk::verify_ffmpeg_or_development(path, rk::EXPORT_ASS_ZIMG_VARIANT, VERIFY_TIMEOUT)
            .or_else(|_| {
                rk::verify_ffmpeg_or_development(path, rk::EXPORT_ASS_VARIANT, VERIFY_TIMEOUT)
            })
            .map(|_| ())
    };
    result.map_err(|e| {
        ProjectError::MediaToolMissing(format!("{} is not an approved build: {e}", path.display()))
    })
}

#[cfg(feature = "test-mode")]
fn env_override(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(feature = "test-mode"))]
fn env_override(_var: &str) -> Option<PathBuf> {
    None
}

impl MediaTools {
    /// The catalog-verified (or, in a development build, development) ffmpeg handle.
    pub(crate) fn ffmpeg_handle(&self) -> Result<rk::FfmpegHandle, ProjectError> {
        rk::verify_ffmpeg_or_development(&self.ffmpeg, rk::EXPORT_ASS_ZIMG_VARIANT, VERIFY_TIMEOUT)
            .or_else(|_| {
                rk::verify_ffmpeg_or_development(
                    &self.ffmpeg,
                    rk::EXPORT_ASS_VARIANT,
                    VERIFY_TIMEOUT,
                )
            })
            .map_err(|e| ProjectError::MediaToolMissing(e.to_string()))
    }

    /// The near-lossless intermediate encode for `format` on this machine, chosen by
    /// RightKit's platform policy.
    pub fn intermediate(
        &self,
        format: rk::encoders::IntermediateFormat,
    ) -> Result<rk::encoders::IntermediateEncode, ProjectError> {
        let binary = self.ffmpeg_handle()?;
        Ok(rk::encoders::intermediate_for(
            &binary,
            format,
            PROBE_TIMEOUT,
            &rk::CancellationToken::new(),
        )?)
    }

    /// Resolves both tools from the bundle (and, with test-mode, the env overrides).
    /// A bundled tool must be the LGPL build RightKit's catalog approves on every host; a
    /// test-mode override is a development binary and is not checked.
    pub fn resolve(resource_dir: Option<&Path>) -> Result<Self, ProjectError> {
        let ffmpeg_override = env_override("RIGHTKIT_FFMPEG");
        let ffprobe_override = env_override("RIGHTKIT_FFPROBE");
        let (check_ffmpeg, check_ffprobe) = (ffmpeg_override.is_none(), ffprobe_override.is_none());
        let tools = Self::resolve_with(resource_dir, ffmpeg_override, ffprobe_override)?;
        // Every host verifies: the shipped app is LGPL catalog builds only, so a bundled file that is not
        // the approved build (a GPL or system ffmpeg dropped into `runtime/`) is refused here, never run.
        if check_ffmpeg {
            require_approved(&tools.ffmpeg)?;
        }
        if check_ffprobe {
            require_approved(&tools.ffprobe)?;
        }
        Ok(tools)
    }

    /// Resolution with explicit overrides (testable without touching the environment).
    pub(crate) fn resolve_with(
        resource_dir: Option<&Path>,
        ffmpeg_override: Option<PathBuf>,
        ffprobe_override: Option<PathBuf>,
    ) -> Result<Self, ProjectError> {
        Ok(Self {
            ffmpeg: pick("ffmpeg", resource_dir, ffmpeg_override)?,
            ffprobe: pick("ffprobe", resource_dir, ffprobe_override)?,
        })
    }
}

fn pick(
    name: &str,
    resource_dir: Option<&Path>,
    override_path: Option<PathBuf>,
) -> Result<PathBuf, ProjectError> {
    let path = match override_path {
        Some(p) => p,
        None => {
            let dir = resource_dir.ok_or_else(|| {
                ProjectError::MediaToolMissing(format!(
                    "{name} not found: the engine is not running from an app bundle"
                ))
            })?;
            dir.join("runtime")
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
        }
    };
    if path.is_file() {
        Ok(path)
    } else {
        Err(ProjectError::MediaToolMissing(format!(
            "{name} not found at {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn resolves_from_bundle_runtime_dir() {
        let d = tempfile::tempdir().unwrap();
        let sfx = std::env::consts::EXE_SUFFIX;
        touch(&d.path().join(format!("runtime/ffmpeg{sfx}")));
        touch(&d.path().join(format!("runtime/ffprobe{sfx}")));
        let t = MediaTools::resolve_with(Some(d.path()), None, None).unwrap();
        assert!(t.ffmpeg.starts_with(d.path().join("runtime")));
        assert!(t.ffprobe.starts_with(d.path().join("runtime")));
    }

    #[test]
    fn missing_tool_is_typed() {
        let d = tempfile::tempdir().unwrap();
        let e = MediaTools::resolve_with(Some(d.path()), None, None).unwrap_err();
        assert_eq!(e.code(), "media_tool_missing");
        let e = MediaTools::resolve_with(None, None, None).unwrap_err();
        assert_eq!(e.code(), "media_tool_missing");
    }

    #[test]
    fn override_must_exist_and_wins_over_bundle() {
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("x");
        touch(&fake);
        let t = MediaTools::resolve_with(None, Some(fake.clone()), Some(fake.clone())).unwrap();
        assert_eq!(t.ffmpeg, fake);
        let e =
            MediaTools::resolve_with(None, Some(d.path().join("nope")), Some(fake)).unwrap_err();
        assert_eq!(e.code(), "media_tool_missing");
    }
}
