//! Product and role bytes for the Authenticode-covered `.rkcli` PE section.

/// Name of the initialized, read-only PE section containing the record.
pub const WINDOWS_CLI_IDENTITY_SECTION: &[u8; 8] = b".rkcli\0\0";
/// Size of the record before any alignment padding added by the linker.
pub const WINDOWS_CLI_IDENTITY_LEN: usize = 128;
/// Maximum allowed raw size of the identity section, including padding.
pub const WINDOWS_CLI_SECTION_MAX: usize = 4096;

const SCHEMA: &[u8; 6] = b"RKCLI\x01";
const PRODUCT_START: usize = 8;

/// Role bound to the executable by its code signature.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowsCliRole {
    /// Installed one-shot command entry point.
    Command = 1,
    /// Installed resident peer, with separate admission.
    Resident = 2,
}

/// Build the exact record for a trusted product identifier and role.
/// Product identifiers use ASCII lower-case letters, digits, dots and hyphens.
pub const fn windows_cli_identity(
    product_id: &str,
    role: WindowsCliRole,
) -> Option<[u8; WINDOWS_CLI_IDENTITY_LEN]> {
    let product = product_id.as_bytes();
    if product.is_empty() || product.len() > WINDOWS_CLI_IDENTITY_LEN - PRODUCT_START {
        return None;
    }
    let mut record = [0; WINDOWS_CLI_IDENTITY_LEN];
    let mut index = 0;
    while index < SCHEMA.len() {
        record[index] = SCHEMA[index];
        index += 1;
    }
    record[6] = role as u8;
    record[7] = product.len() as u8;
    index = 0;
    while index < product.len() {
        let byte = product[index];
        if !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-') {
            return None;
        }
        record[PRODUCT_START + index] = byte;
        index += 1;
    }
    Some(record)
}

/// Check bytes from the one verified, Authenticode-covered identity section. The caller
/// must authenticate the file and validate the PE section bounds before calling this.
pub fn matches_windows_cli_identity(
    section: &[u8],
    product_id: &str,
    role: WindowsCliRole,
) -> bool {
    let Some(expected) = windows_cli_identity(product_id, role) else {
        return false;
    };
    if section.len() > WINDOWS_CLI_SECTION_MAX {
        return false;
    }
    section
        .strip_prefix(expected.as_slice())
        .is_some_and(|padding| padding.iter().all(|&byte| byte == 0))
}

/// Read the CLI identity from an already authenticated, write-excluded Windows PE32+
/// executable. Keep the same file handle open from signature verification through this call.
/// Return false for malformed images, unrecognized layouts and identity mismatches.
#[cfg(windows)]
pub fn authenticated_windows_cli_identity_matches(
    file: &std::fs::File,
    product_id: &str,
    role: WindowsCliRole,
) -> bool {
    use object::read::ReadCache;

    let cache = ReadCache::new(HeldFileReader { file, offset: 0 });
    cli_identity_section(&cache)
        .is_some_and(|section| matches_windows_cli_identity(section, product_id, role))
}

#[cfg(windows)]
struct HeldFileReader<'a> {
    file: &'a std::fs::File,
    offset: u64,
}

#[cfg(windows)]
impl object::read::ReadCacheOps for HeldFileReader<'_> {
    fn len(&mut self) -> Result<u64, ()> {
        self.file.metadata().map(|metadata| metadata.len()).map_err(|_| ())
    }

    fn seek(&mut self, offset: u64) -> Result<u64, ()> {
        self.offset = offset;
        Ok(offset)
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, ()> {
        use std::os::windows::fs::FileExt as _;

        // Windows selects the offset and reads in the same operation, even if another
        // reader shares the handle. Keep the parser's logical cursor independent.
        let count = self.file.seek_read(buffer, self.offset).map_err(|_| ())?;
        self.offset = self.offset.checked_add(count as u64).ok_or(())?;
        Ok(count)
    }

    fn read_exact(&mut self, mut buffer: &mut [u8]) -> Result<(), ()> {
        while !buffer.is_empty() {
            let count = self.read(buffer)?;
            if count == 0 {
                return Err(());
            }
            buffer = &mut buffer[count..];
        }
        Ok(())
    }
}

#[cfg(windows)]
fn cli_identity_section<'data, R: object::read::ReadRef<'data>>(data: R) -> Option<&'data [u8]> {
    use object::pe;
    use object::read::pe::{ImageNtHeaders, ImageOptionalHeader};
    use object::LittleEndian as LE;

    let dos = pe::ImageDosHeader::parse(data).ok()?;
    let mut section_table_offset = u64::from(dos.nt_headers_offset());
    if !(std::mem::size_of::<pe::ImageDosHeader>() as u64..=1_048_576).contains(&section_table_offset)
    {
        return None;
    }
    let (nt, directories) = pe::ImageNtHeaders64::parse(data, &mut section_table_offset).ok()?;
    let file_header = nt.file_header();
    let executable_flags = pe::IMAGE_FILE_EXECUTABLE_IMAGE | pe::IMAGE_FILE_DLL;
    let section_count = u64::from(file_header.number_of_sections.get(LE));
    if file_header.characteristics.get(LE) & executable_flags != pe::IMAGE_FILE_EXECUTABLE_IMAGE
        || !matches!(
            file_header.machine.get(LE),
            pe::IMAGE_FILE_MACHINE_AMD64 | pe::IMAGE_FILE_MACHINE_ARM64
        )
        || !(1..=96).contains(&section_count)
    {
        return None;
    }

    let optional = nt.optional_header();
    let headers_end = u64::from(optional.size_of_headers());
    let table_len = section_count * std::mem::size_of::<pe::ImageSectionHeader>() as u64;
    let alignment = u64::from(optional.file_alignment());
    let cert = directories.get(pe::IMAGE_DIRECTORY_ENTRY_SECURITY)?;
    let cert_offset = u64::from(cert.virtual_address.get(LE));
    let cert_size = u64::from(cert.size.get(LE));
    if !(512..=65_536).contains(&alignment)
        || !alignment.is_power_of_two()
        || section_table_offset + table_len > headers_end
        || headers_end % alignment != 0
        || cert_offset < headers_end
        || cert_offset % 8 != 0
        || cert_size < 8
        || cert_size % 8 != 0
        || cert_offset + cert_size != data.len().ok()?
    {
        return None;
    }

    let table = nt.sections(data, section_table_offset).ok()?;
    let mut ranges = Vec::with_capacity(section_count as usize);
    let mut identity_range = None;
    for section in table.iter() {
        let offset = u64::from(section.pointer_to_raw_data.get(LE));
        let size = u64::from(section.size_of_raw_data.get(LE));
        if size != 0 {
            if offset < headers_end
                || offset % alignment != 0
                || size % alignment != 0
                || offset + size > cert_offset
            {
                return None;
            }
            ranges.push((offset, offset + size));
        }
        if &section.name == WINDOWS_CLI_IDENTITY_SECTION {
            let required = pe::IMAGE_SCN_CNT_INITIALIZED_DATA | pe::IMAGE_SCN_MEM_READ;
            let forbidden = pe::IMAGE_SCN_CNT_UNINITIALIZED_DATA
                | pe::IMAGE_SCN_CNT_CODE
                | pe::IMAGE_SCN_MEM_EXECUTE
                | pe::IMAGE_SCN_MEM_WRITE;
            let flags = section.characteristics.get(LE);
            if identity_range.is_some()
                || flags & (required | forbidden) != required
                || section.virtual_size.get(LE) as usize != WINDOWS_CLI_IDENTITY_LEN
                || !(WINDOWS_CLI_IDENTITY_LEN as u64..=WINDOWS_CLI_SECTION_MAX as u64)
                    .contains(&size)
            {
                return None;
            }
            identity_range = Some((offset, size));
        }
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    let (offset, size) = identity_range?;
    data.read_bytes_at(offset, size).ok()
}

#[cfg(windows)]
#[doc(hidden)]
pub fn retain_windows_cli_identity(record: &'static [u8; WINDOWS_CLI_IDENTITY_LEN]) {
    // SAFETY: the reference points to the initialized, aligned, live static record.
    // A reachable volatile load keeps the section referenced under LTO and linker GC.
    let _ = unsafe { std::ptr::read_volatile(record) };
}

/// Emit the installed Windows CLI identity. Call once in the executable's normal entry
/// point, before branching on arguments. The executable must be signed after linking.
#[macro_export]
macro_rules! declare_windows_cli_identity {
    ($product_id:expr, $role:expr $(,)?) => {{
        #[cfg(windows)]
        {
            #[used]
            #[unsafe(link_section = ".rkcli")]
            static IDENTITY: [u8; $crate::windows_identity::WINDOWS_CLI_IDENTITY_LEN] =
                match $crate::windows_identity::windows_cli_identity($product_id, $role) {
                    Some(record) => record,
                    None => panic!("invalid Windows CLI product identifier"),
                };
            $crate::windows_identity::retain_windows_cli_identity(&IDENTITY);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_identity_requires_product_role_schema_and_zero_padding() {
        let product = "app.example.product";
        let mut section = windows_cli_identity(product, WindowsCliRole::Command)
            .unwrap()
            .to_vec();
        section.resize(512, 0);
        assert!(matches_windows_cli_identity(&section, product, WindowsCliRole::Command));
        assert!(!matches_windows_cli_identity(&section, product, WindowsCliRole::Resident));
        assert!(!matches_windows_cli_identity(
            &section,
            "app.example.other",
            WindowsCliRole::Command,
        ));
        assert!(!matches_windows_cli_identity(
            &section[..127],
            product,
            WindowsCliRole::Command,
        ));
        section[5] = 2;
        assert!(!matches_windows_cli_identity(&section, product, WindowsCliRole::Command));
        section[5] = 1;
        section[129] = 1;
        assert!(!matches_windows_cli_identity(&section, product, WindowsCliRole::Command));
        assert!(windows_cli_identity("bad/product", WindowsCliRole::Command).is_none());
    }
}
