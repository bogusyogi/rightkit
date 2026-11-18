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

/// Authenticate a write-excluded Windows PE32+ executable and match its signed CLI identity.
/// The signer policy, product and role must come from trusted configuration. Retain the same
/// file and its ancestors through launch and bind the suspended process before resuming it.
#[cfg(windows)]
pub fn verify_held_windows_cli_identity(
    file: &std::fs::File,
    publisher: &rightkit_daemon::WindowsPublisherPolicy<'_>,
    product_id: &str,
    role: WindowsCliRole,
) -> Result<(), rightkit_daemon::SignatureError> {
    verify_held_windows_cli_image(file, publisher, product_id, role, None)
}

/// Authenticate the held CLI and require its signed PE32+ static import search flags and
/// embedded process manifest. Supply the write-excluded file handle from trusted acquisition
/// and retain it and its ancestors through launch. The publisher policy, product, role and
/// exact audited manifest bytes must come from trusted configuration. The audited manifest
/// must exclude private assembly and file redirection. Bind the suspended process to the
/// held image before resuming. External redirection and effective DLL origins require
/// separate checks.
#[cfg(windows)]
pub fn verify_held_windows_cli_identity_with_dependency_policy(
    file: &std::fs::File,
    publisher: &rightkit_daemon::WindowsPublisherPolicy<'_>,
    product_id: &str,
    role: WindowsCliRole,
    approved_manifest: &[u8],
) -> Result<(), rightkit_daemon::SignatureError> {
    verify_held_windows_cli_image(file, publisher, product_id, role, Some(approved_manifest))
}

#[cfg(windows)]
fn verify_held_windows_cli_image(
    file: &std::fs::File,
    publisher: &rightkit_daemon::WindowsPublisherPolicy<'_>,
    product_id: &str,
    role: WindowsCliRole,
    approved_manifest: Option<&[u8]>,
) -> Result<(), rightkit_daemon::SignatureError> {
    use object::read::ReadCache;

    rightkit_daemon::verify_held_windows_publisher(file, publisher)?;
    let cache = ReadCache::new(HeldFileReader { file, offset: 0 });
    let image = cli_pe(&cache)
        .filter(|image| matches_windows_cli_identity(image.identity, product_id, role))
        .ok_or_else(|| {
            rightkit_daemon::SignatureError::NotSigned(
                "signed executable has no matching CLI product and role".into(),
            )
        })?;
    if let Some(approved) = approved_manifest {
        image
            .verify_dependency_policy(&cache, approved)
            .ok_or_else(|| {
                rightkit_daemon::SignatureError::NotSigned(
                    "signed executable does not meet the CLI dependency policy".into(),
                )
            })?;
    }
    Ok(())
}

#[cfg(windows)]
struct HeldFileReader<'a> {
    file: &'a std::fs::File,
    offset: u64,
}

#[cfg(windows)]
impl object::read::ReadCacheOps for HeldFileReader<'_> {
    fn len(&mut self) -> Result<u64, ()> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|_| ())
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
struct CliPe<'data> {
    identity: &'data [u8],
    sections: object::read::pe::SectionTable<'data>,
    directories: object::read::pe::DataDirectories<'data>,
    headers_end: u64,
    file_alignment: u64,
    section_alignment: u64,
    image_end: u64,
}

#[cfg(windows)]
fn cli_pe<'data, R: object::read::ReadRef<'data>>(data: R) -> Option<CliPe<'data>> {
    use object::pe;
    use object::read::pe::{ImageNtHeaders, ImageOptionalHeader};
    use object::LittleEndian as LE;

    let dos = pe::ImageDosHeader::parse(data).ok()?;
    let mut section_table_offset = u64::from(dos.nt_headers_offset());
    if !(std::mem::size_of::<pe::ImageDosHeader>() as u64..=1_048_576)
        .contains(&section_table_offset)
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
    Some(CliPe {
        identity: data.read_bytes_at(offset, size).ok()?,
        sections: table,
        directories,
        headers_end,
        file_alignment: alignment,
        section_alignment: u64::from(optional.section_alignment()),
        image_end: u64::from(optional.size_of_image()),
    })
}

#[cfg(windows)]
impl<'data> CliPe<'data> {
    fn verify_dependency_policy<R: object::read::ReadRef<'data>>(
        &self,
        data: R,
        approved_manifest: &[u8],
    ) -> Option<()> {
        use object::pe;
        use object::LittleEndian as LE;

        const MAX_RESOURCE_DIRECTORY: u64 = 1024 * 1024;
        const MAX_LOAD_CONFIG: u64 = 4096;
        const SYSTEM32_ONLY: u16 = 0x0800;
        const FLAGS_OFFSET: usize =
            std::mem::offset_of!(pe::ImageLoadConfigDirectory64, dependent_load_flags);

        self.validate_virtual_sections()?;
        let config = self
            .directories
            .get(pe::IMAGE_DIRECTORY_ENTRY_LOAD_CONFIG)?;
        let resources = self.directories.get(pe::IMAGE_DIRECTORY_ENTRY_RESOURCE)?;
        let (config_rva, config_size) = config.address_range();
        let (resource_rva, resource_size) = resources.address_range();
        let config_rva = u64::from(config_rva);
        let config_size = u64::from(config_size);
        let resource_rva = u64::from(resource_rva);
        let resource_size = u64::from(resource_size);
        if !((FLAGS_OFFSET + 2) as u64..=MAX_LOAD_CONFIG).contains(&config_size)
            || !(1..=MAX_RESOURCE_DIRECTORY).contains(&resource_size)
            || config_rva % 8 != 0
            || resource_rva % 4 != 0
            || (config_rva < resource_rva + resource_size
                && resource_rva < config_rva + config_size)
        {
            return None;
        }
        let config = self.read_backed_rva(data, config_rva, config_size)?;
        let declared_size = u32::from_le_bytes(config[..4].try_into().ok()?);
        let flags = u16::from_le_bytes(config[FLAGS_OFFSET..FLAGS_OFFSET + 2].try_into().ok()?);
        if u64::from(declared_size) != config_size || flags != SYSTEM32_ONLY {
            return None;
        }

        let resources = self.read_backed_rva(data, resource_rva, resource_size)?;
        let manifest = process_manifest(resources, resource_rva)?;
        (!manifest.is_empty() && manifest == approved_manifest).then_some(())
    }

    fn validate_virtual_sections(&self) -> Option<()> {
        use object::LittleEndian as LE;

        let alignment = self.section_alignment;
        if alignment < 4096
            || alignment < self.file_alignment
            || !alignment.is_power_of_two()
            || self.image_end % alignment != 0
        {
            return None;
        }
        let mut previous_end = self.headers_end.next_multiple_of(alignment);
        for section in self.sections.iter() {
            let start = u64::from(section.virtual_address.get(LE));
            let virtual_size = section.virtual_size.get(LE);
            let raw_size = section.size_of_raw_data.get(LE);
            let size = u64::from(virtual_size.max(raw_size));
            let end = start + size.next_multiple_of(alignment);
            if start < previous_end || start % alignment != 0 || end > self.image_end {
                return None;
            }
            previous_end = end;
        }
        Some(())
    }

    fn read_backed_rva<R: object::read::ReadRef<'data>>(
        &self,
        data: R,
        rva: u64,
        size: u64,
    ) -> Option<&'data [u8]> {
        use object::LittleEndian as LE;

        // The virtual ranges have already been checked for overlaps. The full requested
        // range must be inside both the virtual contents and raw bytes of one signed section.
        for section in self.sections.iter() {
            let start = u64::from(section.virtual_address.get(LE));
            let virtual_size = section.virtual_size.get(LE);
            let raw_size = section.size_of_raw_data.get(LE);
            let backed = u64::from(virtual_size.min(raw_size));
            if rva >= start && rva + size <= start + backed {
                let offset = u64::from(section.pointer_to_raw_data.get(LE)) + (rva - start);
                return data.read_bytes_at(offset, size).ok();
            }
        }
        None
    }
}

#[cfg(windows)]
fn process_manifest(resources: &[u8], resource_rva: u64) -> Option<&[u8]> {
    use object::pe;
    use object::read::pe::{ResourceDirectory, ResourceDirectoryTable};
    use object::LittleEndian as LE;

    fn numeric_table_end(table: &ResourceDirectoryTable<'_>, offset: u32) -> Option<u64> {
        let ids = table.entries.iter().map(|entry| entry.name_or_id.get(LE));
        if offset % 4 != 0
            || table.header.number_of_named_entries.get(LE) != 0
            || ids.clone().any(|id| id > u32::from(u16::MAX))
            || !ids
                .clone()
                .zip(ids.skip(1))
                .all(|(left, right)| left < right)
        {
            return None;
        }
        Some(
            u64::from(offset)
                + std::mem::size_of::<pe::ImageResourceDirectory>() as u64
                + std::mem::size_of_val(table.entries) as u64,
        )
    }

    let directory = ResourceDirectory::new(resources);
    let root = directory.root().ok()?;
    let root_end = numeric_table_end(&root, 0)?;
    let manifest_type = root
        .entries
        .iter()
        .find(|entry| entry.name_or_id.get(LE) == u32::from(pe::RT_MANIFEST))?;
    if u64::from(manifest_type.data_offset()) < root_end {
        return None;
    }
    let names = manifest_type.data(directory).ok()?.table()?;
    let names_end = numeric_table_end(&names, manifest_type.data_offset())?;
    let [process] = names.entries else {
        return None;
    };
    if process.name_or_id.get(LE) != 1 || u64::from(process.data_offset()) < names_end {
        return None;
    }
    let languages = process.data(directory).ok()?.table()?;
    let languages_end = numeric_table_end(&languages, process.data_offset())?;
    let [language] = languages.entries else {
        return None;
    };
    if language.data_offset() % 4 != 0 || u64::from(language.data_offset()) < languages_end {
        return None;
    }
    let entry = language.data(directory).ok()?.data()?;
    let entry_end = u64::from(language.data_offset())
        + std::mem::size_of::<pe::ImageResourceDataEntry>() as u64;
    let offset = u64::from(entry.offset_to_data.get(LE)).checked_sub(resource_rva)?;
    if entry.reserved.get(LE) != 0 || offset % 4 != 0 || offset < entry_end {
        return None;
    }
    let end = offset + u64::from(entry.size.get(LE));
    resources.get(usize::try_from(offset).ok()?..usize::try_from(end).ok()?)
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
        assert!(matches_windows_cli_identity(
            &section,
            product,
            WindowsCliRole::Command
        ));
        assert!(!matches_windows_cli_identity(
            &section,
            product,
            WindowsCliRole::Resident
        ));
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
        assert!(!matches_windows_cli_identity(
            &section,
            product,
            WindowsCliRole::Command
        ));
        section[5] = 1;
        section[129] = 1;
        assert!(!matches_windows_cli_identity(
            &section,
            product,
            WindowsCliRole::Command
        ));
        assert!(windows_cli_identity("bad/product", WindowsCliRole::Command).is_none());
    }
}
