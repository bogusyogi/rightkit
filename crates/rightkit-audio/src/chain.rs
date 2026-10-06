//! The configurable dialogue chain: high-pass -> denoise -> compressor -> de-esser ->
//! limiter. Ported from `video-media::audio_finish::dialogue_chain_filter` and
//! `video-project::audio_profile`, extended with denoise and typed stages.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::AudioError;
use crate::filter::{db_to_linear, render_chain, Filter};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HighPass {
    pub hz: f64,
    #[serde(default = "default_poles")]
    pub poles: u8,
}

fn default_poles() -> u8 {
    2
}
fn default_residual_floor() -> f64 {
    -38.0
}
fn default_adaptivity() -> f64 {
    0.5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Denoise {
    None,
    /// FFT denoiser (built in). `noise_floor_db` seeds the estimate; `track` adapts it.
    Afftdn {
        reduction_db: f64,
        noise_floor_db: f64,
        /// ffmpeg's `rf`: the denoiser never pushes noise below this (default -38 dB is
        /// too high for quiet rooms, so presets set it explicitly).
        #[serde(default = "default_residual_floor")]
        residual_floor_db: f64,
        /// ffmpeg's `ad`: how fast the noise estimate adapts, `0..=1`.
        #[serde(default = "default_adaptivity")]
        adaptivity: f64,
        track: bool,
    },
    /// Non-local-means denoiser (built in, slower, gentler on tonal content).
    Anlmdn {
        strength: f64,
    },
    /// RNNoise. Needs a model file that the caller supplies (none is bundled: see
    /// `docs/audio-finish.md` for the licence note).
    Arnndn {
        model: String,
        mix: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Compressor {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    #[serde(default)]
    pub makeup_db: f64,
}

/// ffmpeg `deesser` (a built-in sibilance reducer; present in the editing build).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DeEsser {
    /// `0..=1` how strongly the sibilant band is reduced.
    pub intensity: f64,
    /// `0..=1` maximum reduction.
    pub max_reduction: f64,
    /// `0..=1` ffmpeg's normalised detection frequency.
    pub frequency: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Limiter {
    /// Ceiling in dBFS (e.g. `-1.5`). ffmpeg's `alimiter` floor is -24 dB.
    pub ceiling_db: f64,
    #[serde(default = "default_limiter_attack")]
    pub attack_ms: f64,
    #[serde(default = "default_limiter_release")]
    pub release_ms: f64,
}

fn default_limiter_attack() -> f64 {
    5.0
}
fn default_limiter_release() -> f64 {
    50.0
}

/// Downward expander (agate): pulls the residual noise floor down inside pauses
/// that spectral denoise leaves behind. Runs after denoise, before compression.
/// `range` caps attenuation (linear gain, 0..1); the room-tone layer is padded by
/// the same depth so injected tone lands on the floor the gate leaves behind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expander {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    #[serde(default = "default_expander_range")]
    pub range: f64,
}

fn default_expander_range() -> f64 {
    0.1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DialogueChain {
    pub high_pass: Option<HighPass>,
    pub denoise: Denoise,
    #[serde(default)]
    pub expander: Option<Expander>,
    pub compressor: Option<Compressor>,
    pub deesser: Option<DeEsser>,
    pub limiter: Limiter,
}

fn check(name: &str, v: f64, lo: f64, hi: f64) -> Result<(), AudioError> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(AudioError::Invalid(format!(
            "{name} = {v} outside {lo}..={hi}"
        )))
    }
}

impl DialogueChain {
    pub fn validate(&self) -> Result<(), AudioError> {
        if let Some(hp) = &self.high_pass {
            check("high_pass.hz", hp.hz, 20.0, 500.0)?;
            if !(1..=2).contains(&hp.poles) {
                return Err(AudioError::Invalid("high_pass.poles must be 1 or 2".into()));
            }
        }
        match &self.denoise {
            Denoise::None => {}
            Denoise::Afftdn {
                reduction_db,
                noise_floor_db,
                residual_floor_db,
                adaptivity,
                ..
            } => {
                check("denoise.reduction_db", *reduction_db, 0.01, 97.0)?;
                check("denoise.noise_floor_db", *noise_floor_db, -80.0, -20.0)?;
                check(
                    "denoise.residual_floor_db",
                    *residual_floor_db,
                    -80.0,
                    -20.0,
                )?;
                check("denoise.adaptivity", *adaptivity, 0.0, 1.0)?;
            }
            Denoise::Anlmdn { strength } => check("denoise.strength", *strength, 0.00001, 10.0)?,
            Denoise::Arnndn { model, mix } => {
                check("denoise.mix", *mix, -1.0, 1.0)?;
                if !Path::new(model).is_file() {
                    return Err(AudioError::MissingInput(model.into()));
                }
            }
        }
        if let Some(g) = &self.expander {
            check("expander.threshold_db", g.threshold_db, -80.0, 0.0)?;
            check("expander.ratio", g.ratio, 1.0, 60.0)?;
            check("expander.attack_ms", g.attack_ms, 0.01, 2000.0)?;
            check("expander.release_ms", g.release_ms, 0.01, 9000.0)?;
            check("expander.range", g.range, 0.0001, 1.0)?;
        }
        if let Some(c) = &self.compressor {
            check("compressor.threshold_db", c.threshold_db, -60.0, 0.0)?;
            check("compressor.ratio", c.ratio, 1.0, 20.0)?;
            check("compressor.attack_ms", c.attack_ms, 0.01, 2000.0)?;
            check("compressor.release_ms", c.release_ms, 0.01, 9000.0)?;
            check("compressor.makeup_db", c.makeup_db, 0.0, 36.0)?;
        }
        if let Some(d) = &self.deesser {
            check("deesser.intensity", d.intensity, 0.0, 1.0)?;
            check("deesser.max_reduction", d.max_reduction, 0.0, 1.0)?;
            check("deesser.frequency", d.frequency, 0.0, 1.0)?;
        }
        check("limiter.ceiling_db", self.limiter.ceiling_db, -24.0, 0.0)?;
        check("limiter.attack_ms", self.limiter.attack_ms, 0.1, 80.0)?;
        check("limiter.release_ms", self.limiter.release_ms, 1.0, 8000.0)?;
        Ok(())
    }

    /// High-pass and denoise only: what a room-tone sample must pass through so it
    /// matches the processed dialogue's floor.
    pub fn conditioning_filters(&self) -> Vec<Filter> {
        let mut out = Vec::new();
        if let Some(hp) = &self.high_pass {
            out.push(
                Filter::new("highpass")
                    .num("f", hp.hz)
                    .opt("poles", hp.poles),
            );
        }
        match &self.denoise {
            Denoise::None => {}
            Denoise::Afftdn {
                reduction_db,
                noise_floor_db,
                residual_floor_db,
                adaptivity,
                track,
            } => out.push(
                Filter::new("afftdn")
                    .num("nr", *reduction_db)
                    .num("nf", *noise_floor_db)
                    .num("rf", *residual_floor_db)
                    .num("ad", *adaptivity)
                    .opt("tn", u8::from(*track)),
            ),
            Denoise::Anlmdn { strength } => {
                out.push(Filter::new("anlmdn").num("s", *strength));
            }
            Denoise::Arnndn { model, mix } => out.push(
                Filter::new("arnndn")
                    .quoted("m", &model.replace('\\', "/"))
                    .num("mix", *mix),
            ),
        }
        out
    }

    /// Compressor and de-esser: dynamics before the limiter. The expander leads here,
    /// not in conditioning: the room-tone layer is rendered through the conditioning
    /// filters to match timbre, and a live gate on its stream breaks the loop/fill
    /// graph — the layer instead gets a static pad of the gate's `range` depth.
    pub fn dynamics_filters(&self) -> Vec<Filter> {
        let mut out = Vec::new();
        if let Some(g) = &self.expander {
            out.push(
                Filter::new("agate")
                    .opt("threshold", format!("{}dB", g.threshold_db))
                    .num("ratio", g.ratio)
                    .num("attack", g.attack_ms)
                    .num("release", g.release_ms)
                    .num("range", g.range),
            );
        }
        if let Some(c) = &self.compressor {
            out.push(
                Filter::new("acompressor")
                    .opt(
                        "threshold",
                        format!("{}dB", crate::filter::num(c.threshold_db)),
                    )
                    .num("ratio", c.ratio)
                    .num("attack", c.attack_ms)
                    .num("release", c.release_ms)
                    .num("makeup", db_to_linear(c.makeup_db).clamp(1.0, 64.0)),
            );
        }
        if let Some(d) = &self.deesser {
            out.push(
                Filter::new("deesser")
                    .num("i", d.intensity)
                    .num("m", d.max_reduction)
                    .num("f", d.frequency),
            );
        }
        out
    }

    /// The limiter. Explicitly `level=0` (alimiter's default auto-level would raise gain)
    /// and `latency=1` (no lookahead delay, so audio stays in sync with video).
    pub fn limiter_filter(&self) -> Filter {
        Filter::new("alimiter")
            .num(
                "limit",
                db_to_linear(self.limiter.ceiling_db).clamp(0.0625, 1.0),
            )
            .num("attack", self.limiter.attack_ms)
            .num("release", self.limiter.release_ms)
            .opt("level", 0)
            .opt("latency", 1)
    }

    /// All filters in order (without room tone, which mixes between dynamics and limiter).
    pub fn filters(&self) -> Vec<Filter> {
        let mut out = self.conditioning_filters();
        out.extend(self.dynamics_filters());
        out.push(self.limiter_filter());
        out
    }

    /// Comma-joined `-af` text for the whole chain.
    pub fn af(&self) -> String {
        render_chain(&self.filters())
    }
}

