use super::SkillError;

pub fn normalize_skill_pack_github_url(raw: &str) -> Result<String, SkillError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SkillError::Install(
            "GitHub skill pack URL cannot be empty".to_string(),
        ));
    }
    let parsed = url::Url::parse(trimmed)
        .map_err(|e| SkillError::Install(format!("invalid GitHub URL: {e}")))?;
    match parsed.host_str() {
        Some("github.com") => {
            let parts: Vec<&str> = parsed.path().trim_start_matches('/').split('/').collect();
            if parts.len() < 5 || !parts[2].eq_ignore_ascii_case("blob") {
                return Err(SkillError::Install(
                    "skill pack URL must be a github.com/<owner>/<repo>/blob/<ref>/<path> link"
                        .to_string(),
                ));
            }
            let mut raw_url = parsed.clone();
            let _ = raw_url.set_host(Some("raw.githubusercontent.com"));
            let new_path = format!(
                "/{}/{}/{}/{}",
                parts[0],
                parts[1],
                parts[3],
                parts[4..].join("/")
            );
            raw_url.set_path(&new_path);
            Ok(raw_url.to_string())
        }
        Some("raw.githubusercontent.com") => Ok(trimmed.to_string()),
        Some(other) => Err(SkillError::Install(format!(
            "skill pack URL must be hosted on github.com or raw.githubusercontent.com (got {other})"
        ))),
        None => Err(SkillError::Install(
            "skill pack URL must include a host".to_string(),
        )),
    }
}
