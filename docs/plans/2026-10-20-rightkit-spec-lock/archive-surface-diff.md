# Item-level public surface diff: published archive vs working tree (2026-10-20)
Method: grep '^\s*pub (fn|struct|enum|trait|const|type|mod|use)' over src/**/*.rs, normalised to 'file: signature-head', set diff. '>' = only in tree (unpublished), '<' = only in archive (removed in tree).

## rightkit-tauri-shell-0.4.0  (tree-only items: 207, archive-only items: 0)
> enum InspectRule
> fn is_inspect_item
> fn needs_handler
> fn on_second_launch
> fn reset
> fn resolve_handled
> fn set_graft
> fn set_inspect
> fn set_registered
> fn set_suppress
> fn snapshot
> mod chain
> pub async fn rightkit_logs_clear
> pub async fn rightkit_logs_collect
> pub async fn rightkit_logs_write
> pub async fn rk_shell_associations_set
> pub async fn rk_shell_associations_status
> pub async fn rk_shell_open_paths_ready
> pub const DEFAULT_APPS
> pub const DEFAULT_SET_TIMEOUT
> pub const DEFAULT_TTL
> pub const DENY_CSP
> pub const ENV_ADDITIONAL_BROWSER_ARGUMENTS
> pub const ENV_USER_DATA_FOLDER
> pub const EVENT_OPEN_PATHS
> pub const EVENT_PRINT_ERROR
> pub const LINK_NEW_WINDOW_ITEM_NAMES
> pub const MAX_ARGUMENTS
> pub const MAX_ARGUMENT_BYTES
> pub const MAX_EVENT_BYTES
> pub const MAX_EVENT_NAME_BYTES
> pub const MAX_PREVIEWS
> pub const MAX_PREVIEW_BYTES
> pub const MAX_QUEUED
> pub const MAX_SET_TIMEOUT
> pub const MAX_SUBMENU_DEPTH
> pub const MAX_TOTAL_BYTES
> pub const MAX_TTL
> pub const MIN_SET_TIMEOUT
> pub const ORDER_FLUSH_TIMEOUT
> pub const SRCDOC_FRAME
> pub const SUITE_KEY_FRAGMENTS
> pub const TOKEN_HEX_LEN
> pub const fn
> pub enum CallerError
> pub enum Denial
> pub enum FrameKind
> pub enum Os
> pub enum OverrideRole
> pub enum PrintUi
> pub enum SettingsPane
> pub fn accelerator
> pub fn all_groups_set
> pub fn apply_state
> pub fn arg
> pub fn candidate_paths
> pub fn check
> pub fn check_state
> pub fn clamp_timeout
> pub fn classes_subkey
> pub fn classify
> pub fn classify_frame
> pub fn collect
> pub fn csp
> pub fn decode_reg_sz
> pub fn default_apps_uri_for
> pub fn deliver_argv
> pub fn deliver_candidates
> pub fn disabled
> pub fn effective_windows_handler
> pub fn emit
> pub fn ensure_own_bundle
> pub fn entry_ids
> pub fn event_name
> pub fn file_paths_from_args
> pub fn file_paths_from_argv
> pub fn file_paths_from_launch
> pub fn flush
> pub fn from_launch
> pub fn from_paths
> ... (207 total, truncated)

## rightkit-daemon-0.3.0  (tree-only items: 22, archive-only items: 0)
> pub enum SpecialKind
> pub fn complete
> pub fn ephemeral_specials
> pub fn into_outcome
> pub fn is_retryable
> pub fn link_not_created
> pub fn links
> pub fn migrate_from_with
> pub fn migrated_links
> pub fn omitted_optional_links
> pub fn optional_links
> pub fn outcome
> pub fn preserve_links
> pub fn skip_ephemeral_specials
> pub fn skipped_specials
> pub fn strict
> pub struct LinkNotCreated
> pub struct MigrateOptions
> pub struct MigratedLink
> pub struct MigrationReport
> pub struct OmittedLink
> pub struct SkippedSpecial

## rightkit-fs-0.4.4  (tree-only items: 7, archive-only items: 0)
> pub enum DirPublishPolicy
> pub fn atomic_write_with_access_denied_retry
> pub fn create_with_access_denied_retry
> pub fn policy
> pub fn publish_dir_with
> pub fn publish_file_with_access_denied_retry
> pub struct AccessDeniedRetry

## rightkit-job-queue-0.4.0  (tree-only items: 52, archive-only items: 0)
> enum Joined
> fn discard
> fn is_head
> fn join
> fn lock_error
> pub const DEFAULT_QUERY_LIMIT
> pub const MAX_QUERY_LIMIT
> pub enum FifoScope
> pub enum HandshakeMode
> pub enum JobOrder
> pub fn after
> pub fn attempts_at_least
> pub fn attempts_below
> pub fn call_timeout_raw
> pub fn cancel_flag
> pub fn coalesce_asc
> pub fn coalesce_desc
> pub fn count
> pub fn delete_terminal
> pub fn emit
> pub fn event_emitter
> pub fn ext_eq
> pub fn ext_not_null
> pub fn ext_null
> pub fn fifo_scope
> pub fn is_connected
> pub fn is_empty
> pub fn label
> pub fn len
> pub fn limit
> pub fn message
> pub fn method
> pub fn mode
> pub fn order
> pub fn query
> pub fn register_with
> pub fn report
> pub fn set_label
> pub fn set_message
> pub fn set_progress
> pub fn status_in
> pub fn with_fifo_scope
> pub fn with_handshake
> pub struct ActivityOptions
> pub struct ActivitySnapshot
> pub struct EventEmitter
> pub struct HandshakeCall
> pub struct JobCursor
> pub struct JobPage
> pub struct JobQuery
> pub type ActivityRegistry
> struct TicketSlot

## rightkit-child-process-0.5.2  (tree-only items: 22, archive-only items: 0)
> enum Stream
> fn capture_failure
> fn inject_read_fault
> fn pipe_stdio
> fn stderr_streaming
> fn stdout_streaming
> fn take
> fn wait_exit_then_sweep
> mod test_fault
> pub fn allow_breakaway
> pub fn allow_silent_breakaway
> pub fn contain_current_process_with
> pub fn input
> pub fn poll
> pub fn run_command
> pub fn run_streaming
> pub fn stderr_tail_cap
> pub fn stdout_tail_cap
> pub fn timeout
> pub struct StreamOptions
> pub struct StreamOutput
> type ChunkSink

## rightkit-license-0.2.3  (tree-only items: 165, archive-only items: 0)
> const ENVELOPE_KEY
> const ENVELOPE_VERSION
> const MUTATION_WAIT
> const STATUS_WAIT
> enum Adopted
> enum Answer
> enum Fenced
> enum Level
> enum Loaded
> enum PersistOutcome
> enum Reconciled
> enum RecordState
> enum RecordStatus
> enum Refusal
> enum TrialAnswer
> enum Written
> fn acquire
> fn acquire_polled
> fn activate
> fn activation_token
> fn active
> fn authorized
> fn clock_error
> fn core
> fn deactivate
> fn encode
> fn enter
> fn evaluate
> fn fenced
> fn flag_unresolved
> fn flagged_unresolved
> fn from_map
> fn held_mask
> fn import_legacy
> fn import_legacy_json
> fn is_lock_held
> fn is_tombstone
> fn is_unresolved
> fn lease_activate
> fn lease_issue
> fn lease_remove
> fn lease_renew
> fn lock_and_read
> fn new
> fn none
> fn origin
> fn outcome_result
> fn parse
> fn parse_rfc3339
> fn parse_secret
> fn pause
> fn read
> fn read_unlocked
> fn reconcile
> fn refresh
> fn request_trial
> fn run_fenced
> fn settle
> fn signed_token
> fn status
> fn to_ms
> fn tombstone
> fn trial
> fn try_acquire_shared
> fn unresolved
> fn unverifiable
> fn valid_host
> fn with_busy
> fn with_wait
> fn write
> fn write_unlocked
> pub const DEFAULT_HEADER_TTL
> pub const DEFAULT_RECORD_ACCOUNT
> pub const ENGINE_BUSY_TEXT
> pub const ENGINE_STORE_TEXT
> pub const ENGINE_UNRESOLVED_TEXT
> pub enum CommandCode
> pub enum LicenseError
> pub enum LicenseMode
> pub enum LicenseSetup
> ... (165 total, truncated)

## rightkit-credentials-0.3.0  (tree-only items: 11, archive-only items: 0)
> fn is_held
> fn mutation_in_flight
> pub fn is_idle
> pub fn new_trusted
> pub fn requirement
> pub fn resolve_bundled_login_item
> pub fn rotation_status
> pub fn team
> pub fn team_id
> pub struct HelperTrust
> pub struct RotationStatus

## rightkit-logs-0.3.3  (tree-only items: 23, archive-only items: 0)
> pub const KNOWN_SECRET_MARKER
> pub fn add
> pub fn drain
> pub fn extend
> pub fn finish
> pub fn from_values
> pub fn has_pending
> pub fn is_exposed
> pub fn last_error
> pub fn raw_path
> pub fn redact
> pub fn redact_counted
> pub fn sink
> pub fn spawn_drainer
> pub fn stderr_file
> pub fn with_compact_at
> pub fn with_known_secrets
> pub fn with_prefix
> pub fn with_raw_path
> pub struct DetachedLog
> pub struct DrainReport
> pub struct Drainer
> pub struct KnownSecrets

## rightkit-http-0.3.6  (tree-only items: 0, archive-only items: 0)

## rightkit-db-0.5.0  (tree-only items: 0, archive-only items: 0)

## rightkit-app-settings-0.2.4  (tree-only items: 0, archive-only items: 0)

## rightkit-crash-0.3.3  (tree-only items: 0, archive-only items: 0)

## rightkit-device-0.3.0  (tree-only items: 0, archive-only items: 0)

## rightkit-sidecar-0.3.0  (tree-only items: 0, archive-only items: 0)

## rightkit-qa-0.7.0  (tree-only items: 44, archive-only items: 0)
> fn bind_test_mode
> fn env_for
> fn write_scenario_binding
> pub const FAKE_FAILURE_EXIT
> pub const MAX_FILES
> pub const MAX_QUEUE
> pub const SCRIPT_ARG
> pub const SCRIPT_ENV
> pub const STORE_VAR
> pub const VERSION
> pub enum DialogAnswer
> pub enum Hang
> pub fn answer_next_dialog
> pub fn dialog_report
> pub fn ensure_clean
> pub fn expand
> pub fn from_json
> pub fn from_toml
> pub fn parse_ack
> pub fn parse_report
> pub fn perform
> pub fn push_js
> pub fn report_js
> pub fn reset_dialog_queue
> pub fn reset_js
> pub fn roots
> pub fn to_wire
> pub fn with_call
> pub fn with_fixture_root
> pub mod dialog
> pub mod fake_cli
> pub struct Chunk
> pub struct DialogAck
> pub struct DialogRefusal
> pub struct DialogReport
> pub struct DialogRoots
> pub struct DialogSeam
> pub struct ExpectStdin
> pub struct Invocation
> pub struct Rule
> pub struct Script
> pub struct SpawnChild
> pub struct When
> pub struct WriteFile

## rightkit-agent-tools-0.2.0  (tree-only items: 0, archive-only items: 0)
