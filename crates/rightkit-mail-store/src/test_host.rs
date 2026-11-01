//! A host for unit tests: fixed key, a per-process temp data folder, development channel.
use std::path::{Path, PathBuf};

use crate::host::{Channel, DirKind, KeyRotation, MailStoreHost};

struct TestHost;

struct NoRotation;

impl KeyRotation for NoRotation {
    fn commit(&self) -> Result<(), String> {
        Ok(())
    }

    fn abort(&self) {}
}

impl MailStoreHost for TestHost {
    fn app_slug(&self) -> &str {
        "mailstore-test"
    }

    fn product_name(&self) -> &str {
        "TestApp"
    }

    fn get_or_create_db_key(&self) -> Result<String, String> {
        Ok("dGVzdC1rZXktMDAwMDAwMDAwMDAwMDAwMDAwMDA=".to_string())
    }

    fn read_pending_db_key(&self) -> Option<String> {
        None
    }

    fn promote_pending_db_key(&self, _used: &str) -> Result<(), String> {
        Ok(())
    }

    fn new_db_key(&self) -> Result<String, String> {
        Ok("bmV3LXRlc3Qta2V5LTAwMDAwMDAwMDAwMDAwMDAwMDA=".to_string())
    }

    fn begin_key_rotation(&self, _new_key: &str) -> Result<Box<dyn KeyRotation>, String> {
        Ok(Box::new(NoRotation))
    }

    fn data_dir_checked(&self) -> Result<(PathBuf, DirKind), String> {
        let dir = std::env::temp_dir().join(format!("mailstore-test-{}", std::process::id()));
        Ok((dir, DirKind::Override))
    }

    fn build(&self) -> (Channel, String) {
        (Channel::Dev, "test".to_string())
    }

    fn append_rotating_log(&self, _path: &Path, _max: u64, _line: &str) -> std::io::Result<()> {
        Ok(())
    }
}

#[ctor::ctor(unsafe)]
fn register_test_host() {
    let _ = crate::host::set_host(Box::new(TestHost));
}
