//! A read-only view of the Windows registry that the host app supplies.
//!
//! The leftovers scan and the startup-item listing read a few keys (`Software\<vendor>\<product>`,
//! the Run keys and their `StartupApproved` state). This crate takes no registry dependency: the
//! host implements `RegistryRead` over the registry crate it already has, and everything decided
//! about what those keys mean lives here. The trait is plain data, so it compiles on every host.

/// The two hives these readers look in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hive {
    /// `HKEY_CURRENT_USER`.
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE`.
    LocalMachine,
}

/// Which registry view a key is opened in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// No view flag: the opening process's own view (`KEY_READ`).
    Default,
    /// The 64-bit view (`KEY_READ | KEY_WOW64_64KEY`).
    Wow64,
    /// The 32-bit view (`KEY_READ | KEY_WOW64_32KEY`).
    Wow32,
}

/// The data type of a registry value, as far as these readers care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// `REG_SZ`.
    String,
    /// `REG_EXPAND_SZ`.
    ExpandString,
    /// Anything else (binary, number, multi-string).
    Other,
}

/// One value of a key: its name, type and raw bytes.
#[derive(Clone, Debug)]
pub struct Value {
    pub name: String,
    pub kind: ValueKind,
    pub bytes: Vec<u8>,
}

impl Value {
    /// `REG_SZ` and `REG_EXPAND_SZ` data as text with the trailing NULs removed; `None` for any
    /// other type. Odd bytes and unpaired UTF-16 units never panic.
    pub fn text(&self) -> Option<String> {
        matches!(self.kind, ValueKind::String | ValueKind::ExpandString)
            .then(|| utf16_text(&self.bytes))
    }
}

/// UTF-16 little-endian bytes as text, lossy, without trailing NULs.
pub fn utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

/// What the readers ask of the registry. Every method is read-only; a key that is absent or cannot
/// be opened answers `None`.
pub trait RegistryRead {
    /// The names of the subkeys of `path` (backslash separated, below the hive).
    fn subkey_names(&self, hive: Hive, view: View, path: &str) -> Option<Vec<String>>;

    /// The values of the key `path`; an entry that could not be read is `Err(())`.
    #[allow(clippy::type_complexity)]
    fn values(&self, hive: Hive, view: View, path: &str) -> Option<Vec<Result<Value, ()>>>;
}

/// A registry with nothing in it, for hosts that are not Windows.
pub struct NoRegistry;

impl RegistryRead for NoRegistry {
    fn subkey_names(&self, _hive: Hive, _view: View, _path: &str) -> Option<Vec<String>> {
        None
    }

    fn values(&self, _hive: Hive, _view: View, _path: &str) -> Option<Vec<Result<Value, ()>>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_drops_nuls_and_other_types() {
        let units: Vec<u8> = "abc\0"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let value = Value {
            name: "x".into(),
            kind: ValueKind::String,
            bytes: units.clone(),
        };
        assert_eq!(value.text().as_deref(), Some("abc"));
        let binary = Value {
            kind: ValueKind::Other,
            ..value
        };
        assert_eq!(binary.text(), None);
        assert_eq!(utf16_text(&[0x61]), "");
    }
}
