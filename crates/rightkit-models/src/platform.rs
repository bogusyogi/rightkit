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
        let filter = filter.to_ascii_lowercase();
        // Android's target triple is `aarch64-linux-android`: its `linux` token names the kernel,
        // not the OS, so it does not veto an Android host when `android` is also present.
        let names_android = filter.split('-').any(|token| token == "android");
        for token in filter.split('-') {
            let os = match token {
                "macos" | "darwin" | "apple" => Some("macos"),
                "windows" | "win" => Some("windows"),
                "linux" if names_android => Some("android"),
                "linux" => Some("linux"),
                "android" => Some("android"),
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

#[cfg(test)]
mod android_tests {
    use super::*;

    #[test]
    fn android_filters_match_only_android_hosts_and_the_triple_is_not_vetoed_by_linux() {
        let phone = Platform::new("android", "aarch64");
        for filter in ["android", "android-aarch64", "aarch64-linux-android", "android-arm64"] {
            assert!(phone.matches(filter), "{filter}");
        }
        assert!(!phone.matches("android-x86_64"));
        assert!(!phone.matches("aarch64-linux"));
        assert!(!phone.matches("linux"));
        assert!(!phone.matches("macos-aarch64"));
        let linux = Platform::new("linux", "aarch64");
        assert!(linux.matches("aarch64-linux"));
        assert!(!linux.matches("android"));
        assert!(!linux.matches("aarch64-linux-android"));
        let emulator = Platform::new("android", "x86_64");
        assert!(emulator.matches("x86_64-linux-android") && !emulator.matches("arm64-android"));
        assert!(phone.allows(&[]));
        assert!(phone.allows(&["windows".into(), "android".into()]));
        assert!(!phone.allows(&["windows".into()]));
    }
}
