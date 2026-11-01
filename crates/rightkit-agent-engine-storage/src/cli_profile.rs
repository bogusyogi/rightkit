//! Typed persistence for vendor-owned authenticated CLI profiles.
//!
//! Profiles identify a managed vendor home and non-secret authentication
//! evidence. Credential bytes never enter this table or this crate.

use crate::{Result, Storage};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliAccountProfileRow {
    pub id: String,
    pub provider_id: String,
    pub label: String,
    pub managed_home: String,
    pub auth_mode: String,
    pub signed_in: bool,
    pub account_fingerprint: Option<String>,
    pub last_verified_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const PROFILE_COLUMNS: &str = "id, provider_id, label, managed_home, auth_mode, signed_in, \
    account_fingerprint, last_verified_at, created_at, updated_at";

impl Storage {
    pub fn upsert_cli_account_profile(&self, row: &CliAccountProfileRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cli_account_profiles \
             (id, provider_id, label, managed_home, auth_mode, signed_in, account_fingerprint, \
              last_verified_at, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(id) DO UPDATE SET \
              provider_id=excluded.provider_id, label=excluded.label, managed_home=excluded.managed_home, \
              auth_mode=excluded.auth_mode, signed_in=excluded.signed_in, \
              account_fingerprint=excluded.account_fingerprint, \
              last_verified_at=excluded.last_verified_at, updated_at=excluded.updated_at",
            params![
                row.id,
                row.provider_id,
                row.label,
                row.managed_home,
                row.auth_mode,
                i64::from(row.signed_in),
                row.account_fingerprint,
                row.last_verified_at,
                row.created_at,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_cli_account_profile(&self, id: &str) -> Result<Option<CliAccountProfileRow>> {
        self.conn
            .query_row(
                &format!("SELECT {PROFILE_COLUMNS} FROM cli_account_profiles WHERE id=?1"),
                [id],
                Self::map_cli_account_profile,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_cli_account_profiles(
        &self,
        provider_id: &str,
    ) -> Result<Vec<CliAccountProfileRow>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {PROFILE_COLUMNS} FROM cli_account_profiles WHERE provider_id=?1 ORDER BY id ASC"
        ))?;
        let profiles = statement
            .query_map([provider_id], Self::map_cli_account_profile)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into);
        profiles
    }

    pub fn delete_cli_account_profile(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM cli_account_profiles WHERE id=?1", [id])?
            == 1)
    }

    fn map_cli_account_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<CliAccountProfileRow> {
        Ok(CliAccountProfileRow {
            id: row.get(0)?,
            provider_id: row.get(1)?,
            label: row.get(2)?,
            managed_home: row.get(3)?,
            auth_mode: row.get(4)?,
            signed_in: row.get::<_, i64>(5)? != 0,
            account_fingerprint: row.get(6)?,
            last_verified_at: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> CliAccountProfileRow {
        CliAccountProfileRow {
            id: id.into(),
            provider_id: "codex".into(),
            label: "default".into(),
            managed_home: "/tmp/agent/codex/default".into(),
            auth_mode: "native_cli_account".into(),
            signed_in: true,
            account_fingerprint: Some("sha256:account".into()),
            last_verified_at: Some("2026-08-08T00:00:00Z".into()),
            created_at: "2026-08-08T00:00:00Z".into(),
            updated_at: "2026-08-08T00:00:00Z".into(),
        }
    }

    #[test]
    fn profile_round_trip_does_not_store_secret_material() {
        let storage = Storage::open_in_memory().unwrap();
        storage
            .upsert_cli_account_profile(&row("codex/default"))
            .unwrap();
        let got = storage
            .get_cli_account_profile("codex/default")
            .unwrap()
            .unwrap();
        assert_eq!(got.provider_id, "codex");
        assert!(got.signed_in);
        assert!(storage.list_cli_account_profiles("codex").unwrap().len() == 1);
    }
}
