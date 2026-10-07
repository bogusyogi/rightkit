//! Platform filter for manifest files. Filters are free-form tokens split on
//! `-`: `macos`, `windows`, `linux`, `aarch64`, `x86_64`, or whole target
//! triples (`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`). Every
//! recognised OS/arch token must match the host; a filter with no recognised
//! token never matches (fail closed).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

impl Platform {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        }
    }

    pub fn new(os: &str, arch: &str) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
        }
    }

    pub fn matches(&self, filter: &str) -> bool {
        let mut recognised = false;
        for token in filter.to_ascii_lowercase().split('-') {
            let os = match token {
                "macos" | "darwin" | "apple" => Some("macos"),
                "windows" | "win" => Some("windows"),
                "linux" => Some("linux"),
                _ => None,
            };
            let arch = match token {
                "aarch64" | "arm64" => Some("aarch64"),
                "x86_64" | "x64" | "amd64" => Some("x86_64"),
                _ => None,
            };
            if let Some(os) = os {
                recognised = true;
                if os != self.os {
                    return false;
                }
            }
            if let Some(arch) = arch {
                recognised = true;
                if arch != self.arch {
                    return false;
                }
            }
        }
        recognised
    }

    /// Empty filter list = all platforms; otherwise any filter may match.
    pub fn allows(&self, filters: &[String]) -> bool {
        filters.is_empty() || filters.iter().any(|f| self.matches(f))
    }
}
