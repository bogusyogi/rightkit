#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_unix_epoch() {
        let dt = from_unix(0);
        assert_eq!(dt.year, 1970);
        assert_eq!(dt.month, 1);
        assert_eq!(dt.day, 1);
        assert_eq!(dt.hour, 0);
        assert_eq!(dt.weekday, "Thursday");
    }

    #[test]
    fn from_unix_known_date() {
        // 1780272000 = 20605 days from epoch = 2026-06-01T00:00:00Z
        let dt = from_unix(1780272000);
        assert_eq!(dt.year, 2026);
        assert_eq!(dt.month, 6);
        assert_eq!(dt.day, 1);
    }

    #[test]
    fn parse_iso8601_utc() {
        // 2026-06-02T00:00:00Z = 20606 * 86400 = 1780358400
        assert_eq!(parse_iso8601("2026-06-02T00:00:00Z"), Some(1780358400));
    }

    #[test]
    fn parse_iso8601_with_offset() {
        // 2026-06-02T05:30:00+05:30 is the same UTC moment as 2026-06-02T00:00:00Z
        let base = parse_iso8601("2026-06-02T05:30:00+05:30").unwrap();
        assert_eq!(base, 1780358400);
    }

    #[test]
    fn parse_iso8601_without_zone_is_utc_and_rejects_bad_ranges() {
        assert_eq!(parse_iso8601("2026-06-02T00:00:00"), Some(1780358400));
        assert_eq!(parse_iso8601("2026-06-02T00:00:00.5Z"), Some(1780358400));
        assert_eq!(parse_iso8601("2026-13-02T00:00:00Z"), None);
        assert_eq!(parse_iso8601("2026-06-02T25:00:00"), None);
        assert_eq!(parse_iso8601("not a date at all"), None);
    }

    #[test]
    fn tz_offset_known() {
        assert_eq!(tz_offset("UTC").unwrap().0, 0);
        assert_eq!(tz_offset("Asia/Tokyo").unwrap().0, 9 * 3600);
        assert_eq!(tz_offset("America/New_York").unwrap().0, -5 * 3600);
        assert_eq!(tz_offset("Asia/Kolkata").unwrap().0, 19800);
    }

    #[test]
    fn tz_offset_literal() {
        assert_eq!(tz_offset("+05:30").unwrap().0, 19800);
        assert_eq!(tz_offset("UTC-8").unwrap().0, -8 * 3600);
    }

    #[tokio::test]
    async fn current_time_returns_utc() {
        use crate::coding_tools::context::ToolContext;
        use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
        let tool = TimeTool;
        let ctx = ToolContext::new("test", "/tmp", PermissionMode::Ask, WorkspaceTrust::Trusted);
        let result = tool
            .execute(serde_json::json!({"operation": "current_time"}), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("utc"));
    }

    #[tokio::test]
    async fn convert_tokyo_to_london() {
        use crate::coding_tools::context::ToolContext;
        use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
        let tool = TimeTool;
        let ctx = ToolContext::new("test", "/tmp", PermissionMode::Ask, WorkspaceTrust::Trusted);
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "convert",
                    "datetime": "2026-06-02T09:00:00Z",
                    "to_timezone": "Asia/Tokyo"
                }),
                &ctx,
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("18:00:00")); // UTC+9
    }

    #[tokio::test]
    async fn diff_positive() {
        use crate::coding_tools::context::ToolContext;
        use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
        let tool = TimeTool;
        let ctx = ToolContext::new("test", "/tmp", PermissionMode::Ask, WorkspaceTrust::Trusted);
        let result = tool
            .execute(
                serde_json::json!({
                    "operation": "diff",
                    "start": "2026-06-02T00:00:00Z",
                    "end": "2026-06-03T06:30:00Z"
                }),
                &ctx,
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("1d 6h 30m"));
    }
}
