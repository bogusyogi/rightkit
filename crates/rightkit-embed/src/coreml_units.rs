//! Core ML compute-unit choice per sequence length. Plain data and parsing with no Core ML
//! dependency, so the type exists on every platform (an `EmbedderConfig` carries it everywhere)
//! and its tests run anywhere. Only a Core ML bundle on macOS acts on it.

use std::fmt;
use std::str::FromStr;

use crate::e40::BUCKETS;

/// Longest graph (in tokens) the default map sends to the Neural Engine; longer graphs use the GPU.
pub const NEURAL_ENGINE_MAX_TOKENS: usize = 128;

/// Which Core ML compute units run one function (`MLComputeUnits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CoreMlComputeUnit {
    /// `MLComputeUnitsAll`: Core ML chooses among CPU, GPU and Neural Engine.
    All,
    /// `MLComputeUnitsCPUAndNeuralEngine`.
    CpuAndNe,
    /// `MLComputeUnitsCPUAndGPU`.
    CpuAndGpu,
    /// `MLComputeUnitsCPUOnly`: the parity reference.
    CpuOnly,
}

impl CoreMlComputeUnit {
    /// Canonical spelling accepted by [`FromStr`]: `all`, `cpu_and_ne`, `cpu_and_gpu`, `cpu_only`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::CpuAndNe => "cpu_and_ne",
            Self::CpuAndGpu => "cpu_and_gpu",
            Self::CpuOnly => "cpu_only",
        }
    }
}

impl fmt::Display for CoreMlComputeUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An unreadable compute-unit or unit-map specification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid Core ML compute units: {0}")]
pub struct CoreMlUnitsParseError(String);

impl FromStr for CoreMlComputeUnit {
    type Err = CoreMlUnitsParseError;

    /// Case-insensitive; `-`, `_` and spaces are ignored, so `CPU_AND_NE`, `cpu-and-ne` and
    /// `cpuandne` are the same. Accepts `all`, `cpu_and_ne` (`ne`), `cpu_and_gpu` (`gpu`) and
    /// `cpu_only` (`cpu`).
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let key: String = text
            .chars()
            .filter(|c| !matches!(c, '-' | '_' | ' '))
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match key.as_str() {
            "all" => Ok(Self::All),
            "cpuandne" | "cpuandneuralengine" | "ne" => Ok(Self::CpuAndNe),
            "cpuandgpu" | "gpu" => Ok(Self::CpuAndGpu),
            "cpuonly" | "cpu" => Ok(Self::CpuOnly),
            _ => Err(CoreMlUnitsParseError(format!(
                "`{}` is not one of all, cpu_and_ne, cpu_and_gpu, cpu_only",
                text.trim()
            ))),
        }
    }
}

/// The compute units of each Core ML function (`embed_32` .. `embed_512`).
///
/// [`Self::per_length_default`] is what a Core ML bundle uses unless
/// [`EmbedderConfig::with_coreml_units`](crate::EmbedderConfig::with_coreml_units) says
/// otherwise. Each function is loaded once on its own unit and kept resident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreMlUnits {
    by_length: [CoreMlComputeUnit; BUCKETS.len()],
}

impl CoreMlUnits {
    /// The measured default (Mac matrix, RightKit issue #81): `cpu_and_ne` for graphs of up to
    /// 128 tokens (32, 64, 128; p50 about 2.3-3.0 ms) and `cpu_and_gpu` for 256 and 512
    /// (p50 about 5.1 ms and 9.8-18.7 ms). The first `cpu_and_gpu` call per length can take
    /// up to 51 s cold (shader build), which is why load pre-warms every function.
    pub fn per_length_default() -> Self {
        let mut by_length = [CoreMlComputeUnit::CpuAndNe; BUCKETS.len()];
        for (unit, length) in by_length.iter_mut().zip(BUCKETS) {
            if length > NEURAL_ENGINE_MAX_TOKENS {
                *unit = CoreMlComputeUnit::CpuAndGpu;
            }
        }
        Self { by_length }
    }

    /// Every function on `unit` (forces `all`, `cpu_and_ne`, `cpu_and_gpu` or `cpu_only`).
    pub fn uniform(unit: CoreMlComputeUnit) -> Self {
        Self {
            by_length: [unit; BUCKETS.len()],
        }
    }

    /// This map with the function of `length` tokens moved to `unit`. `None` when `length` is
    /// not one of 32, 64, 128, 256, 512.
    pub fn with_length(mut self, length: usize, unit: CoreMlComputeUnit) -> Option<Self> {
        let index = BUCKETS.iter().position(|bucket| *bucket == length)?;
        self.by_length[index] = unit;
        Some(self)
    }

    /// The unit of the function of `length` tokens; `None` for a length that is not a graph.
    pub fn unit_for(&self, length: usize) -> Option<CoreMlComputeUnit> {
        BUCKETS
            .iter()
            .position(|bucket| *bucket == length)
            .map(|index| self.by_length[index])
    }

    /// `(length, unit)` for every function, shortest first.
    pub fn entries(&self) -> impl Iterator<Item = (usize, CoreMlComputeUnit)> + '_ {
        BUCKETS.iter().copied().zip(self.by_length.iter().copied())
    }

    /// True when every function runs on `cpu_only`.
    pub fn is_cpu_only(&self) -> bool {
        self.by_length
            .iter()
            .all(|unit| *unit == CoreMlComputeUnit::CpuOnly)
    }
}

impl From<CoreMlComputeUnit> for CoreMlUnits {
    /// Every function on `unit`, like [`CoreMlUnits::uniform`].
    fn from(unit: CoreMlComputeUnit) -> Self {
        Self::uniform(unit)
    }
}

impl Default for CoreMlUnits {
    fn default() -> Self {
        Self::per_length_default()
    }
}

impl fmt::Display for CoreMlUnits {
    /// `32=cpu_and_ne,64=cpu_and_ne,...`: parses back to the same map.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, (length, unit)) in self.entries().enumerate() {
            if position > 0 {
                f.write_str(",")?;
            }
            write!(f, "{length}={unit}")?;
        }
        Ok(())
    }
}

impl FromStr for CoreMlUnits {
    type Err = CoreMlUnitsParseError;

    /// A caller-facing override string (a setting or environment variable):
    ///
    /// - `default` or `per_length`: [`Self::per_length_default`];
    /// - a bare unit (`all`, `cpu_and_ne`, `cpu_and_gpu`, `cpu_only`): every function on it;
    /// - `length=unit` pairs separated by commas (`256=cpu_only,512=cpu_only`): those lengths
    ///   override the per-length default, the others keep it. A length may appear once.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let fail = |why: String| CoreMlUnitsParseError(why);
        if text.is_empty() {
            return Err(fail("empty specification".into()));
        }
        let key = text.to_ascii_lowercase().replace(['-', ' '], "_");
        if key == "default" || key == "per_length" {
            return Ok(Self::per_length_default());
        }
        if !text.contains('=') {
            return text.parse().map(Self::uniform);
        }
        let mut units = Self::per_length_default();
        let mut seen = Vec::new();
        for pair in text.split(',') {
            let (length, unit) = pair
                .split_once('=')
                .ok_or_else(|| fail(format!("`{}` is not length=unit", pair.trim())))?;
            let length: usize = length
                .trim()
                .parse()
                .map_err(|_| fail(format!("`{}` is not a token length", length.trim())))?;
            if seen.contains(&length) {
                return Err(fail(format!("length {length} is given twice")));
            }
            seen.push(length);
            units = units.with_length(length, unit.parse()?).ok_or_else(|| {
                fail(format!(
                    "{length} is not a function length (one of {BUCKETS:?})"
                ))
            })?;
        }
        Ok(units)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CoreMlComputeUnit::{All, CpuAndGpu, CpuAndNe, CpuOnly};

    #[test]
    fn the_default_map_is_ne_up_to_128_and_gpu_above() {
        let units = CoreMlUnits::per_length_default();
        let entries: Vec<_> = units.entries().collect();
        assert_eq!(
            entries,
            vec![
                (32, CpuAndNe),
                (64, CpuAndNe),
                (128, CpuAndNe),
                (256, CpuAndGpu),
                (512, CpuAndGpu),
            ]
        );
        assert_eq!(units, CoreMlUnits::default());
        assert_eq!(units.unit_for(128), Some(CpuAndNe));
        assert_eq!(units.unit_for(256), Some(CpuAndGpu));
        assert_eq!(units.unit_for(48), None, "embed_48 is not served");
        assert!(!units.is_cpu_only());
    }

    #[test]
    fn a_uniform_map_forces_one_unit() {
        for unit in [All, CpuAndNe, CpuAndGpu, CpuOnly] {
            let units = CoreMlUnits::uniform(unit);
            assert!(units.entries().all(|(_, u)| u == unit));
        }
        assert!(CoreMlUnits::uniform(CpuOnly).is_cpu_only());
        assert!(!CoreMlUnits::uniform(All).is_cpu_only());
    }

    #[test]
    fn unit_names_parse_loosely_and_round_trip() {
        for unit in [All, CpuAndNe, CpuAndGpu, CpuOnly] {
            assert_eq!(unit.name().parse::<CoreMlComputeUnit>(), Ok(unit));
            assert_eq!(unit.to_string(), unit.name());
        }
        assert_eq!("CPU-and-NE".parse::<CoreMlComputeUnit>(), Ok(CpuAndNe));
        assert_eq!(" cpuAndGpu ".parse::<CoreMlComputeUnit>(), Ok(CpuAndGpu));
        assert_eq!("cpu".parse::<CoreMlComputeUnit>(), Ok(CpuOnly));
        assert!("ane".parse::<CoreMlComputeUnit>().is_err());
        assert!("".parse::<CoreMlComputeUnit>().is_err());
    }

    #[test]
    fn override_strings_parse() {
        assert_eq!(
            "default".parse::<CoreMlUnits>().unwrap(),
            CoreMlUnits::per_length_default()
        );
        assert_eq!(
            "cpu_only".parse::<CoreMlUnits>().unwrap(),
            CoreMlUnits::uniform(CpuOnly)
        );
        assert_eq!(
            "ALL".parse::<CoreMlUnits>().unwrap(),
            CoreMlUnits::uniform(All)
        );
        let mixed: CoreMlUnits = "256=cpu_only, 32=all".parse().unwrap();
        assert_eq!(mixed.unit_for(32), Some(All));
        assert_eq!(
            mixed.unit_for(64),
            Some(CpuAndNe),
            "unlisted keeps the default"
        );
        assert_eq!(mixed.unit_for(256), Some(CpuOnly));
        assert_eq!(mixed.unit_for(512), Some(CpuAndGpu));
    }

    #[test]
    fn bad_override_strings_are_refused() {
        for bad in [
            "",
            "  ",
            "fast",
            "48=all",
            "256=fast",
            "x=all",
            "256",
            "256=all,256=cpu_only",
            "256=all,,",
            "all,512=all",
        ] {
            assert!(
                bad.parse::<CoreMlUnits>().is_err(),
                "`{bad}` must not parse"
            );
        }
    }

    #[test]
    fn display_parses_back_to_the_same_map() {
        let units = CoreMlUnits::per_length_default();
        assert_eq!(
            units.to_string(),
            "32=cpu_and_ne,64=cpu_and_ne,128=cpu_and_ne,256=cpu_and_gpu,512=cpu_and_gpu"
        );
        assert_eq!(units.to_string().parse::<CoreMlUnits>().unwrap(), units);
        let forced = CoreMlUnits::uniform(All);
        assert_eq!(forced.to_string().parse::<CoreMlUnits>().unwrap(), forced);
    }
}
