use std::fs;

const CORPUS_SCHEMA_VERSION: &str = "coderight.eval-corpus.v1";
const CORPUS_REPORT_SCHEMA_VERSION: &str = "coderight.corpus-bakeoff.v1";
const MAX_CORPUS_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusBakeoffRequest {
    manifest_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusManifest {
    schema_version: String,
    suite_namespace: String,
    candidate_roles: Vec<String>,
    baseline_role: String,
    tasks: Vec<CorpusTask>,
    #[serde(default)]
    gate: ExecutedBakeoffGateBody,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusTask {
    case_nonce: String,
    source: CorpusTaskSource,
    verifier: CorpusVerifier,
    #[serde(default)]
    context_tokens: u64,
    #[serde(default = "default_context_class")]
    context_class: ContextClass,
    #[serde(default)]
    context_cohort: Option<String>,
    #[serde(default)]
    candidate_cost_limit_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum CorpusTaskSource {
    Private {
        #[serde(rename = "instructionPath")]
        instruction_path: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
    },
    Harbor {
        #[serde(rename = "taskRoot")]
        task_root: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum CorpusVerifier {
    ExactResponse {
        #[serde(rename = "expectedOutputPath")]
        expected_output_path: String,
    },
    Command {
        command: Vec<String>,
        #[serde(default, rename = "requiredArtifacts")]
        required_artifacts: Vec<CorpusRequiredArtifact>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorpusRequiredArtifact {
    name: String,
    path: String,
    expected_output_path: String,
}

#[derive(Debug, Deserialize)]
struct HarborTaskToml {
    schema_version: String,
    task: HarborTaskIdentity,
}

#[derive(Debug, Deserialize)]
struct HarborTaskIdentity {
    name: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CorpusSourceCounts {
    private: usize,
    harbor: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CorpusBakeoffResponse {
    schema_version: &'static str,
    manifest_sha256: String,
    task_count: usize,
    sources: CorpusSourceCounts,
    report: BakeoffReport,
    decision: BakeoffGateDecision,
    /// Additive daemon resource snapshot (`coderight.resource-metrics.v1`
    /// shape) captured when the report is produced. Existing consumers keep
    /// working: this field only adds to the response.
    resource_metrics: serde_json::Value,
}

struct LoadedCorpus {
    body: ExecutedBakeoffBody,
    manifest_sha256: String,
    sources: CorpusSourceCounts,
}

#[derive(Debug)]
struct CorpusLoadError {
    status: StatusCode,
    message: String,
}

impl CorpusLoadError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
        }
    }
}

async fn run_corpus_bakeoff_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(request): Json<CorpusBakeoffRequest>,
) -> Result<Json<CorpusBakeoffResponse>, (StatusCode, Json<Value>)> {
    enforce_trusted_source_authority(&headers)
        .map_err(|status| corpus_http_error(status, "trusted operator authority required"))?;
    let loaded = load_corpus_manifest(Path::new(&request.manifest_path))
        .map_err(|error| corpus_http_error(error.status, &error.message))?;
    let timeout_seconds = loaded.body.timeout_seconds;
    let task_count = loaded.body.tasks.len();
    // Snapshot resource state before execution consumes `state`; the snapshot
    // then rides along with the report additively.
    let resource_metrics = crate::api_runtime::resource_metrics_json(&state).await;
    let execution = tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        run_live_executed_bakeoff(state, loaded.body),
    )
    .await
    .map_err(|_| corpus_http_error(StatusCode::GATEWAY_TIMEOUT, "corpus bakeoff timed out"))?
    .map_err(|status| corpus_http_error(status, "corpus bakeoff execution failed"))?;
    Ok(Json(CorpusBakeoffResponse {
        schema_version: CORPUS_REPORT_SCHEMA_VERSION,
        manifest_sha256: loaded.manifest_sha256,
        task_count,
        sources: loaded.sources,
        report: execution.report,
        decision: execution.decision,
        resource_metrics,
    }))
}

fn corpus_http_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": message})))
}

fn load_corpus_manifest(path: &Path) -> Result<LoadedCorpus, CorpusLoadError> {
    let manifest_path = fs::canonicalize(path)
        .map_err(|_| CorpusLoadError::bad("corpus manifest does not exist"))?;
    let metadata = fs::metadata(&manifest_path)
        .map_err(|_| CorpusLoadError::bad("corpus manifest metadata is unavailable"))?;
    if !metadata.is_file() {
        return Err(CorpusLoadError::bad("corpus manifest must be a file"));
    }
    if metadata.len() > MAX_CORPUS_MANIFEST_BYTES {
        return Err(CorpusLoadError::too_large("corpus manifest exceeds 1 MiB"));
    }
    let raw_bytes = fs::read(&manifest_path)
        .map_err(|_| CorpusLoadError::bad("corpus manifest is unreadable"))?;
    if raw_bytes.len() as u64 > MAX_CORPUS_MANIFEST_BYTES {
        return Err(CorpusLoadError::too_large("corpus manifest exceeds 1 MiB"));
    }
    let raw = String::from_utf8(raw_bytes)
        .map_err(|_| CorpusLoadError::bad("corpus manifest must be UTF-8 JSON"))?;
    let manifest: CorpusManifest = serde_json::from_str(&raw)
        .map_err(|error| CorpusLoadError::bad(format!("invalid corpus manifest: {error}")))?;
    if manifest.schema_version != CORPUS_SCHEMA_VERSION {
        return Err(CorpusLoadError::bad(format!(
            "unsupported corpus schemaVersion {}; expected {CORPUS_SCHEMA_VERSION}",
            manifest.schema_version
        )));
    }
    if manifest.tasks.is_empty() || manifest.tasks.len() > MAX_TASKS {
        return Err(CorpusLoadError::bad(format!(
            "corpus must contain 1..={MAX_TASKS} tasks"
        )));
    }
    let root = manifest_path
        .parent()
        .ok_or_else(|| CorpusLoadError::bad("corpus manifest has no parent directory"))?;
    let mut tasks = Vec::with_capacity(manifest.tasks.len());
    let mut sources = CorpusSourceCounts::default();
    let mut corpus_digest = rightkit_fs::digest::Sha256Hasher::new();
    digest_framed(&mut corpus_digest, b"manifest", raw.as_bytes());
    for task in manifest.tasks {
        let (instruction_path, workspace_root) = match task.source {
            CorpusTaskSource::Private {
                instruction_path,
                workspace_root,
            } => {
                sources.private += 1;
                (
                    resolve_corpus_file(root, &instruction_path, "private instruction")?,
                    resolve_corpus_dir(root, &workspace_root, "private workspace")?,
                )
            }
            CorpusTaskSource::Harbor {
                task_root,
                workspace_root,
            } => {
                sources.harbor += 1;
                let task_root = resolve_corpus_dir(root, &task_root, "Harbor task root")?;
                validate_harbor_task(&task_root)?;
                (
                    resolve_corpus_file(&task_root, "instruction.md", "Harbor instruction")?,
                    resolve_corpus_dir(root, &workspace_root, "Harbor workspace")?,
                )
            }
        };
        let (git_root, git_identity) = clean_workspace_git_identity(&workspace_root)?;
        let prompt = read_bounded_utf8(
            &instruction_path,
            MAX_PROMPT_BYTES_PER_TASK as u64,
            "instruction",
        )?;
        let (expected_output, coding_verifier, expected_bytes) = match task.verifier {
            CorpusVerifier::ExactResponse {
                expected_output_path,
            } => {
                let expected_path =
                    resolve_corpus_file(root, &expected_output_path, "isolated verifier input")?;
                require_verifier_outside_git(&expected_path, &git_root)?;
                let expected = read_bounded_utf8(
                    &expected_path,
                    MAX_EXPECTED_OUTPUT_BYTES_PER_TASK as u64,
                    "verifier input",
                )?;
                (Some(expected.clone()), None, vec![expected])
            }
            CorpusVerifier::Command {
                command,
                required_artifacts,
            } => {
                let mut expected_bytes = Vec::with_capacity(required_artifacts.len());
                let mut artifacts = Vec::with_capacity(required_artifacts.len());
                for artifact in required_artifacts {
                    let expected_path = resolve_corpus_file(
                        root,
                        &artifact.expected_output_path,
                        "isolated verifier artifact input",
                    )?;
                    require_verifier_outside_git(&expected_path, &git_root)?;
                    let expected = read_bounded_utf8(
                        &expected_path,
                        MAX_EXPECTED_OUTPUT_BYTES_PER_TASK as u64,
                        "verifier artifact input",
                    )?;
                    expected_bytes.push(expected.clone());
                    artifacts.push(ExecutedRequiredArtifactBody {
                        name: artifact.name,
                        path: artifact.path,
                        expected_output: expected,
                    });
                }
                (
                    None,
                    Some(ExecutedCodingVerifierBody {
                        command,
                        required_artifacts: artifacts,
                    }),
                    expected_bytes,
                )
            }
        };
        digest_framed(&mut corpus_digest, b"case", task.case_nonce.as_bytes());
        digest_framed(&mut corpus_digest, b"instruction", prompt.as_bytes());
        digest_framed(&mut corpus_digest, b"git", git_identity.as_bytes());
        for expected in &expected_bytes {
            digest_framed(&mut corpus_digest, b"expected", expected.as_bytes());
        }
        tasks.push(ExecutedBakeoffTaskBody {
            case_nonce: task.case_nonce,
            prompt,
            expected_output,
            coding_verifier,
            workspace_root: Some(workspace_root.to_string_lossy().to_string()),
            context_tokens: task.context_tokens,
            context_class: task.context_class,
            context_cohort: task.context_cohort,
            candidate_cost_limit_usd: task.candidate_cost_limit_usd,
        });
    }
    let workspace_root = tasks
        .first()
        .and_then(|task| task.workspace_root.clone())
        .ok_or_else(|| CorpusLoadError::bad("corpus has no workspace"))?;
    Ok(LoadedCorpus {
        body: ExecutedBakeoffBody {
            membrane_comparison: None,
            suite_namespace: manifest.suite_namespace,
            workspace_root,
            candidate_roles: manifest.candidate_roles,
            baseline_role: manifest.baseline_role,
            tasks,
            gate: manifest.gate,
            timeout_seconds: manifest.timeout_seconds,
        },
        manifest_sha256: corpus_digest.finalize(),
        sources,
    })
}

fn require_verifier_outside_git(path: &Path, git_root: &Path) -> Result<(), CorpusLoadError> {
    if path.starts_with(git_root) {
        return Err(CorpusLoadError::bad(
            "isolated verifier input must be outside the agent workspace Git repository",
        ));
    }
    Ok(())
}

fn clean_workspace_git_identity(workspace: &Path) -> Result<(PathBuf, String), CorpusLoadError> {
    let git = |args: &[&str]| {
        let spec = super::ManagedCommandSpec {
            program: "git".into(),
            argv: args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: workspace.to_path_buf(),
            env: Vec::new(),
        };
        let result = std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .map_err(|_| "runtime unavailable".to_string())
                .and_then(|runtime| runtime.block_on(super::run_managed_command(spec)))
        })
        .join()
        .map_err(|_| "managed Git worker panicked".to_string())
        .and_then(|result| result);
        result
            .map_err(|_| CorpusLoadError::bad("task workspace Git command is unavailable"))
            .and_then(|(status, stdout)| {
                (status == 0).then_some(stdout).ok_or_else(|| {
                    CorpusLoadError::bad("task workspace must belong to a Git repository")
                })
            })
    };
    let root_output = git(&["rev-parse", "--show-toplevel"])?;
    let git_root = fs::canonicalize(root_output.trim())
        .map_err(|_| CorpusLoadError::bad("task workspace Git root is unavailable"))?;
    let status = git(&["status", "--porcelain", "--untracked-files=all"])?;
    if !status.is_empty() {
        return Err(CorpusLoadError::bad(
            "task workspace Git repository must be clean so the evaluated snapshot is reproducible",
        ));
    }
    let head_output = git(&["rev-parse", "HEAD"])?;
    let head = head_output.trim().to_string();
    if head.len() != 40 || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CorpusLoadError::bad(
            "task workspace Git HEAD is unavailable",
        ));
    }
    let relative = workspace
        .strip_prefix(&git_root)
        .map_err(|_| CorpusLoadError::bad("task workspace is outside its Git repository"))?;
    Ok((
        git_root,
        format!("head={head};subdir={}", relative.to_string_lossy()),
    ))
}

fn digest_framed(digest: &mut rightkit_fs::digest::Sha256Hasher, label: &[u8], bytes: &[u8]) {
    digest.update(&(label.len() as u64).to_be_bytes());
    digest.update(label);
    digest.update(&(bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn validate_harbor_task(task_root: &Path) -> Result<(), CorpusLoadError> {
    let task_toml = resolve_corpus_file(task_root, "task.toml", "Harbor task.toml")?;
    let raw = read_bounded_utf8(&task_toml, MAX_CORPUS_MANIFEST_BYTES, "Harbor task.toml")?;
    let metadata: HarborTaskToml = toml::from_str(&raw)
        .map_err(|error| CorpusLoadError::bad(format!("invalid Harbor task.toml: {error}")))?;
    if !metadata.schema_version.starts_with("1.") || metadata.task.name.trim().is_empty() {
        return Err(CorpusLoadError::bad(
            "Harbor task.toml requires schema_version 1.x and a non-empty task.name",
        ));
    }
    Ok(())
}

fn resolve_corpus_file(root: &Path, raw: &str, label: &str) -> Result<PathBuf, CorpusLoadError> {
    let path = resolve_corpus_member(root, raw, label)?;
    if !path.is_file() {
        return Err(CorpusLoadError::bad(format!("{label} must be a file")));
    }
    Ok(path)
}

fn resolve_corpus_dir(root: &Path, raw: &str, label: &str) -> Result<PathBuf, CorpusLoadError> {
    let path = resolve_corpus_member(root, raw, label)?;
    if !path.is_dir() {
        return Err(CorpusLoadError::bad(format!("{label} must be a directory")));
    }
    Ok(path)
}

fn resolve_corpus_member(root: &Path, raw: &str, label: &str) -> Result<PathBuf, CorpusLoadError> {
    let relative = Path::new(raw.trim());
    if raw.trim().is_empty() || relative.is_absolute() {
        return Err(CorpusLoadError::bad(format!(
            "{label} must be a non-empty relative path"
        )));
    }
    let root =
        fs::canonicalize(root).map_err(|_| CorpusLoadError::bad("corpus root is unavailable"))?;
    let resolved = fs::canonicalize(root.join(relative))
        .map_err(|_| CorpusLoadError::bad(format!("{label} does not exist")))?;
    if !resolved.starts_with(&root) {
        return Err(CorpusLoadError::bad(format!(
            "{label} escapes the corpus root"
        )));
    }
    Ok(resolved)
}

fn read_bounded_utf8(path: &Path, max: u64, label: &str) -> Result<String, CorpusLoadError> {
    let metadata = fs::metadata(path)
        .map_err(|_| CorpusLoadError::bad(format!("{label} metadata is unavailable")))?;
    if metadata.len() > max {
        return Err(CorpusLoadError::too_large(format!(
            "{label} exceeds {max} bytes"
        )));
    }
    let bytes =
        fs::read(path).map_err(|_| CorpusLoadError::bad(format!("{label} is unreadable")))?;
    if bytes.len() as u64 > max {
        return Err(CorpusLoadError::too_large(format!(
            "{label} exceeds {max} bytes"
        )));
    }
    String::from_utf8(bytes).map_err(|_| CorpusLoadError::bad(format!("{label} must be UTF-8")))
}

/// Coverage for corpus_bakeoff.rs's pure filesystem/hashing helpers that
/// aren't already exercised end-to-end by `corpus_bakeoff_tests.rs` (kept as
/// a separate module name since both are `include!`d into the same
/// `surface.rs` translation unit).
#[cfg(test)]
mod corpus_bakeoff_pure_helper_tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cr-corpus-helper-{name}-{}-{}",
            std::process::id(),
            CORPUS_HELPER_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    static CORPUS_HELPER_SEQUENCE: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    #[test]
    fn digest_framed_is_length_prefixed_and_order_sensitive() {
        let mut d1 = rightkit_fs::digest::Sha256Hasher::new();
        digest_framed(&mut d1, b"a", b"bc");
        digest_framed(&mut d1, b"d", b"ef");

        // Swapping the split between the two frames (same concatenated bytes)
        // must produce a different digest because each frame is length-prefixed.
        let mut d2 = rightkit_fs::digest::Sha256Hasher::new();
        digest_framed(&mut d2, b"ab", b"c");
        digest_framed(&mut d2, b"d", b"ef");

        assert_ne!(d1.finalize(), d2.finalize());

        // Same calls in the same order must be deterministic.
        let mut d3 = rightkit_fs::digest::Sha256Hasher::new();
        digest_framed(&mut d3, b"a", b"bc");
        digest_framed(&mut d3, b"d", b"ef");
        let mut d4 = rightkit_fs::digest::Sha256Hasher::new();
        digest_framed(&mut d4, b"a", b"bc");
        digest_framed(&mut d4, b"d", b"ef");
        assert_eq!(d3.finalize(), d4.finalize());
    }

    #[test]
    fn read_bounded_utf8_rejects_oversized_and_non_utf8_files() {
        let dir = tmp_dir("read-bounded");
        let small = dir.join("small.txt");
        fs::write(&small, "hello").unwrap();
        assert_eq!(read_bounded_utf8(&small, 10, "small").unwrap(), "hello");

        let too_big = dir.join("big.txt");
        fs::write(&too_big, "0123456789").unwrap();
        let err = read_bounded_utf8(&too_big, 5, "big").unwrap_err();
        assert_eq!(err.status, StatusCode::PAYLOAD_TOO_LARGE);

        let invalid_utf8 = dir.join("invalid.bin");
        fs::write(&invalid_utf8, [0xff, 0xfe, 0xfd]).unwrap();
        let err = read_bounded_utf8(&invalid_utf8, 100, "invalid").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("must be UTF-8"));

        let missing = dir.join("missing.txt");
        let err = read_bounded_utf8(&missing, 100, "missing").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_corpus_member_rejects_absolute_and_escaping_paths() {
        let dir = tmp_dir("resolve-member");
        fs::write(dir.join("inside.txt"), "ok").unwrap();

        let resolved = resolve_corpus_member(&dir, "inside.txt", "member").unwrap();
        assert_eq!(resolved, fs::canonicalize(dir.join("inside.txt")).unwrap());

        let err = resolve_corpus_member(&dir, "", "member").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("non-empty relative path"));

        // A POSIX-rooted path is absolute on Unix but merely root-relative on
        // Windows, where it fails the containment check instead. Either way it
        // must be rejected — the contract is that it never resolves.
        let err = resolve_corpus_member(&dir, "/etc/passwd", "member").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);

        let err = resolve_corpus_member(&dir, "../outside.txt", "member").unwrap_err();
        assert!(
            err.message.contains("does not exist") || err.message.contains("escapes"),
            "unexpected message: {}",
            err.message
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_corpus_file_and_dir_enforce_the_expected_entry_kind() {
        let dir = tmp_dir("resolve-kind");
        fs::write(dir.join("a-file.txt"), "x").unwrap();
        fs::create_dir_all(dir.join("a-dir")).unwrap();

        assert!(resolve_corpus_file(&dir, "a-file.txt", "file").is_ok());
        let err = resolve_corpus_file(&dir, "a-dir", "file").unwrap_err();
        assert!(err.message.contains("must be a file"));

        assert!(resolve_corpus_dir(&dir, "a-dir", "dir").is_ok());
        let err = resolve_corpus_dir(&dir, "a-file.txt", "dir").unwrap_err();
        assert!(err.message.contains("must be a directory"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn require_verifier_outside_git_rejects_paths_inside_the_git_root() {
        let git_root = tmp_dir("git-root");
        let inside = git_root.join("expected.txt");
        fs::write(&inside, "expected").unwrap();
        let err = require_verifier_outside_git(&inside, &git_root).unwrap_err();
        assert!(err.message.contains("outside the agent workspace"));

        let outside_dir = tmp_dir("outside");
        let outside_file = outside_dir.join("expected.txt");
        fs::write(&outside_file, "expected").unwrap();
        assert!(require_verifier_outside_git(&outside_file, &git_root).is_ok());

        let _ = fs::remove_dir_all(&git_root);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    #[test]
    fn validate_harbor_task_requires_1x_schema_and_non_empty_name() {
        let dir = tmp_dir("harbor-task");
        fs::write(
            dir.join("task.toml"),
            "schema_version = \"1.0\"\n[task]\nname = \"do-the-thing\"\n",
        )
        .unwrap();
        assert!(validate_harbor_task(&dir).is_ok());

        let bad_schema = tmp_dir("harbor-task-bad-schema");
        fs::write(
            bad_schema.join("task.toml"),
            "schema_version = \"2.0\"\n[task]\nname = \"do-the-thing\"\n",
        )
        .unwrap();
        let err = validate_harbor_task(&bad_schema).unwrap_err();
        assert!(err.message.contains("schema_version 1.x"));

        let blank_name = tmp_dir("harbor-task-blank-name");
        fs::write(
            blank_name.join("task.toml"),
            "schema_version = \"1.0\"\n[task]\nname = \"   \"\n",
        )
        .unwrap();
        let err = validate_harbor_task(&blank_name).unwrap_err();
        assert!(err.message.contains("non-empty task.name"));

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&bad_schema);
        let _ = fs::remove_dir_all(&blank_name);
    }
}

include!("corpus_bakeoff_tests.rs");
