#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::read_file::ReadFileTool;
    use crate::coding_tools::undo_edit::UndoEditTool;
    use rightkit_agent_policy::{ContentOrigin, ContentProvenance, PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new(
            "sess1",
            root,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    async fn read_hashline(root: &std::path::Path, file: &std::path::Path, line: usize) -> String {
        let result = ReadFileTool
            .execute(json!({"path": file.to_str().unwrap()}), &ctx(root))
            .await;
        assert!(!result.is_error, "{}", result.content);
        result
            .content
            .lines()
            .nth(line - 1)
            .expect("hashline exists")
            .to_string()
    }

    #[tokio::test]
    async fn apply_patch_simple_replacement() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world\nfoo bar\n").unwrap();

        let patch = format!("*** {}\n--- foo bar\n+++ baz qux", file.to_str().unwrap());

        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(!result.is_error, "error: {}", result.content);
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("baz qux"));
        assert!(!content.contains("foo bar"));
    }

    #[tokio::test]
    async fn apply_patch_multiple_hunks() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("multi.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let patch = format!(
            "*** {}\n--- alpha\n+++ ALPHA\n--- gamma\n+++ GAMMA",
            file.to_str().unwrap()
        );

        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(!result.is_error, "error: {}", result.content);
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("ALPHA"));
        assert!(content.contains("GAMMA"));
        assert!(!content.contains("alpha"));
        assert!(!content.contains("gamma"));
    }

    #[tokio::test]
    async fn apply_patch_hashline_anchor_edits_matching_duplicate_line() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("dupes.txt");
        std::fs::write(&file, "same\nsame\n").unwrap();
        let second_line_anchor = read_hashline(dir.path(), &file, 2).await;

        let patch = format!(
            "*** {}\n--- {}\n+++ target",
            file.to_str().unwrap(),
            second_line_anchor
        );

        let result = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(!result.is_error, "error: {}", result.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "same\ntarget\n");
    }

    #[tokio::test]
    async fn apply_patch_hashline_anchor_stale_errors_with_fresh_anchors_without_relocating() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("stale.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();
        let stale_anchor = read_hashline(dir.path(), &file, 2).await;
        std::fs::write(&file, "alpha\nchanged\ngamma\nbeta\n").unwrap();

        let patch = format!(
            "*** {}\n--- {}\n+++ BETA",
            file.to_str().unwrap(),
            stale_anchor
        );

        let result = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(result.is_error);
        assert!(
            result.content.contains("Stale hashline anchor"),
            "{}",
            result.content
        );
        assert!(
            result.content.contains("Fresh anchors"),
            "{}",
            result.content
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "alpha\nchanged\ngamma\nbeta\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_no_match_returns_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("nomatch.txt");
        std::fs::write(&file, "some content\n").unwrap();

        let patch = format!(
            "*** {}\n--- this line does not exist",
            file.to_str().unwrap()
        );

        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("not found") || result.content.contains("Failed"));
    }

    #[tokio::test]
    async fn apply_patch_snapshot_can_be_undone() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("undo.txt");
        std::fs::write(&file, "before\n").unwrap();

        let patch = "*** undo.txt\n--- before\n+++ after";
        let tool = ApplyPatchTool;
        // Undo authority requires the mutation receipt from the same
        // session-scoped FileService the writer used; production binds one
        // service per engine, so the two contexts share it here.
        let file_service = crate::coding_tools::file_service::FileService::new();
        let writer_ctx = ctx(dir.path()).with_file_service(file_service.clone());
        // The engine writes the restore point from `write_targets` before the call runs;
        // the tool never snapshots itself.
        let store = rightkit_agent_engine_history::HistoryStore::new(dir.path(), "sess1");
        for target in tool.write_targets(&json!({"patch": patch}), &writer_ctx) {
            let prior = std::fs::read_to_string(&target).unwrap();
            store.snapshot(&target, &prior).unwrap();
        }
        let result = tool.execute(json!({"patch": patch}), &writer_ctx).await;

        assert!(!result.is_error, "error: {}", result.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "after\n");

        let undone = UndoEditTool
            .execute(
                json!({"path": "undo.txt"}),
                &ctx(dir.path()).with_file_service(file_service),
            )
            .await;

        assert!(!undone.is_error, "error: {}", undone.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "before\n");
    }

    #[tokio::test]
    async fn apply_patch_rejects_workspace_escape() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "outside\n").unwrap();

        let patch = format!(
            "*** {}\n--- outside\n+++ changed",
            outside_file.to_string_lossy()
        );
        let tool = ApplyPatchTool;
        let result = tool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("escapes the workspace"));
        assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "outside\n");
    }

    #[tokio::test]
    async fn policy_target_backstop_runs_before_any_patch_hunk_is_written() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("ordinary.txt"), "before\n").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "safe\n").unwrap();
        let ctx = ctx(dir.path()).with_content_provenance(ContentProvenance::new(
            ContentOrigin::WebDerived,
            "browser_fetch",
        ));

        let patch =
            "*** ordinary.txt\n--- before\n+++ after\n*** CLAUDE.md\n--- safe\n+++ injected";
        let result = ApplyPatchTool.execute(json!({"patch": patch}), &ctx).await;

        assert!(result.is_error, "{}", result.content);
        assert!(result.content.contains("requires explicit approval"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("ordinary.txt")).unwrap(),
            "before\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(),
            "safe\n"
        );
    }

    /// Successful single-file patch records one `Modified` mutation. The patch
    /// pre-existed before the call, so `Created` is wrong. The post-edit
    /// guard parses this file once and uses the typed finding to decide on
    /// the re-entry (or not).
    #[tokio::test]
    async fn apply_patch_records_a_modified_mutation_on_success() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("already.txt");
        std::fs::write(&f, "alpha\nbeta\n").unwrap();
        let patch = format!("*** {}\n--- alpha\n+++ ALPHA", f.to_string_lossy());
        let r = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(r.mutations.len(), 1, "{:?}", r.mutations);
        assert_eq!(r.mutations[0].path, f);
        assert_eq!(r.mutations[0].kind, WorkspaceMutationKind::Modified);
    }

    /// Successful create-from-empty patch records one `Created` mutation.
    /// Distinguishable from in-place modification by the kind, so the engine
    /// can know to skip "diff against prior content" unless the kind is
    /// `Modified`.
    #[tokio::test]
    async fn apply_patch_records_a_created_mutation_for_new_file() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("new.txt");
        let patch = format!("*** {}\n+++ hello\n+++ world", f.to_string_lossy());
        let r = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(r.mutations.len(), 1, "{:?}", r.mutations);
        assert_eq!(r.mutations[0].path, f);
        assert_eq!(r.mutations[0].kind, WorkspaceMutationKind::Created);
    }

    /// EFF-006: a later hunk that fails during prepare must prevent every
    /// file mutation. Multi-file patching does not begin commit until all
    /// staged content and expected preimages have validated.
    #[tokio::test]
    async fn apply_patch_prepare_failure_leaves_every_target_unchanged() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        std::fs::write(&a, "{}\n").unwrap();
        std::fs::write(&b, "{}\n").unwrap();
        let patch = format!(
            "*** {}\n+++ updated\n*** {}\n--- does_not_exist\n+++ whatever",
            a.to_string_lossy(),
            b.to_string_lossy()
        );
        let r = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;
        assert!(
            r.is_error,
            "patch should fail during prepare: {}",
            r.content
        );
        assert!(
            r.mutations.is_empty(),
            "prepare failure cannot land mutations"
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "{}\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "{}\n");
    }

    /// A patch with NO successful hunks must report `is_error == true` AND
    /// `mutations.is_empty()`. The post-edit guard has no paths to act on,
    /// so it must short-circuit and let the next turn repair the broken
    /// patch format rather than inferring a stale set of paths.
    #[tokio::test]
    async fn apply_patch_with_only_failing_hunks_has_no_mutations() {
        let dir = tempdir().unwrap();
        let patch = "*** /no/file/here\n+++ nothing";
        let r = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;
        assert!(r.is_error);
        assert!(
            r.mutations.is_empty(),
            "no mutations expected on fully-failing patch: {:?}",
            r.mutations
        );
    }

    // ------------------------------------------------------------------
    // Graduated rung ladder fixtures. Each case is constructed so no
    // stricter rung can match, asserting the line lands exactly at the
    // rung named in the test (modelled on seek_sequence's pass order).
    // ------------------------------------------------------------------

    /// Extract the found index from a `SeekOutcome`, panicking on
    /// `Ambiguous`/`NotFound` — helper for tests that only exercise the
    /// single-match path.
    fn found_index(outcome: SeekOutcome) -> Option<usize> {
        match outcome {
            SeekOutcome::Found(idx) => Some(idx),
            SeekOutcome::Ambiguous { .. } => panic!("expected a single match, got Ambiguous"),
            SeekOutcome::NotFound => None,
        }
    }

    #[test]
    fn seek_line_exact_rung_wins_over_lenient_ones() {
        let lines = vec!["foo", "foo  ", "  foo"];
        assert_eq!(found_index(seek_line(&lines, "foo", 0)), Some(0));
    }

    #[test]
    fn seek_line_trailing_whitespace_rung() {
        // No exact match: only the trailing spaces differ.
        let lines = vec!["bar", "hello world   "];
        assert_eq!(found_index(seek_line(&lines, "hello world", 0)), Some(1));
    }

    #[test]
    fn seek_line_trim_rung() {
        // Trailing rung cannot match: leading indentation differs too.
        let lines = vec!["bar", "    anchor"];
        assert_eq!(found_index(seek_line(&lines, "anchor", 0)), Some(1));
    }

    #[test]
    fn seek_line_punctuation_rung() {
        // Whitespace rungs cannot match: the drift is a smart quote.
        let lines = vec!["bar", "let s = \u{201c}x\u{201d};"];
        assert_eq!(found_index(seek_line(&lines, "let s = \"x\";", 0)), Some(1));
    }

    #[test]
    fn seek_line_absent_target_returns_none() {
        let lines = vec!["alpha", "beta"];
        assert_eq!(found_index(seek_line(&lines, "gamma", 0)), None);
    }

    #[test]
    fn seek_line_ambiguous_exact_rung_names_both_candidates() {
        // CR-37: two identical exact candidates must not silently pick the
        // first — the rung has to report ambiguity.
        let lines = vec!["dup", "other", "dup"];
        match seek_line(&lines, "dup", 0) {
            SeekOutcome::Ambiguous {
                rung_label,
                line_indices,
            } => {
                assert_eq!(rung_label, "exact");
                assert_eq!(line_indices, vec![0, 2]);
            }
            _ => panic!("expected Ambiguous, got a different outcome"),
        }
    }

    #[test]
    fn seek_line_ambiguous_normalized_rung_does_not_guess() {
        // CR-37: the exact rung finds zero candidates, so the ladder falls
        // through to the normalized rung, where two candidates tie. That
        // must be reported, never guessed by taking the first.
        // Two distinct curly-quote code points that both normalize to the
        // same ASCII single quote, so both count as matches for the target.
        let lines = vec!["let s = \u{2018}x\u{2019};", "let s = \u{201a}x\u{201b};"];
        match seek_line(&lines, "let s = 'x';", 0) {
            SeekOutcome::Ambiguous {
                rung_label,
                line_indices,
            } => {
                assert_eq!(rung_label, "Unicode-punctuation-normalized");
                assert_eq!(line_indices, vec![0, 1]);
            }
            _ => panic!("expected Ambiguous at the normalized rung"),
        }
    }

    #[test]
    fn whitespace_drift_removal_lands_on_trailing_rung() {
        let original = "hello world   \nfoo\n";
        let patch = vec![
            PatchLine::Remove("hello world".into()),
            PatchLine::Add("goodbye".into()),
        ];
        let updated = apply_to_content(original, &patch).expect("trailing drift must land");
        assert_eq!(updated, "goodbye\nfoo\n");
    }

    #[test]
    fn indentation_drift_context_lands_on_trim_rung() {
        // Context line indented with 6 spaces in the file, 2 in the patch.
        let original = "start\n      anchor\nend\n";
        let patch = vec![
            PatchLine::Context("  anchor".into()),
            PatchLine::Remove("end".into()),
        ];
        let updated = apply_to_content(original, &patch).expect("trim drift must land");
        assert_eq!(updated, "start\n      anchor\n");
    }

    #[test]
    fn unicode_punctuation_drift_lands_on_normalized_rung() {
        // Em-dash in the file, ASCII dash in the patch.
        let original = "range: 1 \u{2014} 10\n";
        let patch = vec![
            PatchLine::Remove("range: 1 - 10".into()),
            PatchLine::Add("range: 2 - 20".into()),
        ];
        let updated = apply_to_content(original, &patch).expect("punctuation drift must land");
        assert_eq!(updated, "range: 2 - 20\n");
    }

    #[test]
    fn genuinely_absent_line_still_fails_with_delta_context() {
        let original = "hello world\n";
        let patch = vec![PatchLine::Remove("hello worlds".into())];
        let error = apply_to_content(original, &patch).expect_err("a near miss is still a miss");
        assert!(error.contains("not found"), "{error}");
        // Structured delta context: nearest candidate with distance.
        assert!(error.contains("Nearest match at line 1"), "{error}");
        assert!(error.contains("distance: 1"), "{error}");
    }

    // ------------------------------------------------------------------
    // CR-37: ambiguous multi-match must fail rather than guess, at every
    // rung of the ladder, and precedence between rungs must still hold once
    // ambiguity detection is added.
    // ------------------------------------------------------------------

    #[test]
    fn ambiguous_exact_duplicate_block_fails_naming_both_ranges() {
        // The same line appears twice verbatim; the exact rung must refuse
        // to guess which one is meant instead of silently taking the first.
        let original = "same\nkeep\nsame\n";
        let patch = vec![PatchLine::Remove("same".into())];
        let error = apply_to_content(original, &patch)
            .expect_err("duplicate exact match must be reported, not guessed");
        assert!(error.contains("is ambiguous"), "{error}");
        assert!(error.contains("exact"), "{error}");
        assert!(error.contains("line 1"), "{error}");
        assert!(error.contains("line 3"), "{error}");
    }

    #[test]
    fn exact_unique_match_wins_even_when_normalized_form_is_ambiguous() {
        // "target" appears exactly once, but its trimmed form ("target")
        // also equals a second, differently-indented line. The exact rung
        // must resolve to the single exact hit and never even consult the
        // looser rungs where the duplicate would surface — proving rung
        // precedence still holds now that each rung counts candidates.
        let original = "target\nkeep\n  target\n";
        let patch = vec![
            PatchLine::Remove("target".into()),
            PatchLine::Add("replaced".into()),
        ];
        let updated = apply_to_content(original, &patch)
            .expect("exact rung must win outright despite looser-rung ambiguity");
        assert_eq!(updated, "replaced\nkeep\n  target\n");
    }

    #[test]
    fn zero_exact_matches_falls_through_to_ambiguous_normalized_rung() {
        // No exact match anywhere (both candidates carry a smart quote), so
        // the ladder must fall through past the exact/trailing/trim rungs.
        // At the normalized rung two candidates tie, and that must be
        // reported as ambiguous rather than guessed.
        let original = "let s = \u{2018}x\u{2019};\nkeep\nlet s = \u{201a}x\u{201b};\n";
        let patch = vec![PatchLine::Remove("let s = 'x';".into())];
        let error = apply_to_content(original, &patch)
            .expect_err("normalized-rung ambiguity must be reported, not guessed");
        assert!(error.contains("is ambiguous"), "{error}");
        assert!(error.contains("Unicode-punctuation-normalized"), "{error}");
        assert!(error.contains("line 1"), "{error}");
        assert!(error.contains("line 3"), "{error}");
    }

    #[tokio::test]
    async fn apply_patch_tool_tolerates_trailing_whitespace_drift() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("drift.txt");
        std::fs::write(&file, "hello world   \nkeep\n").unwrap();

        let patch = format!(
            "*** {}\n--- hello world\n+++ goodbye",
            file.to_str().unwrap()
        );
        let result = ApplyPatchTool
            .execute(json!({"patch": patch}), &ctx(dir.path()))
            .await;

        assert!(!result.is_error, "error: {}", result.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "goodbye\nkeep\n");
    }
}
