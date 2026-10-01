use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        })
    }
}

impl FromStr for Provider {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "claude" | "claude-code" => Ok(Self::Claude),
            _ => Err(format!(
                "unsupported provider {value:?}; expected codex or claude"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleKind {
    Manager,
    Explorer,
    PlanReviewer,
    Implementer,
    CodeReviewer,
    #[serde(rename = "final_verifier", alias = "final_reviewer")]
    FinalReviewer,
}

impl RoleKind {
    pub fn is_reviewer(self) -> bool {
        matches!(
            self,
            Self::PlanReviewer | Self::CodeReviewer | Self::FinalReviewer
        )
    }

    /// The role name shown to users.
    pub fn label(self) -> &'static str {
        match self {
            Self::Manager => "Manager",
            Self::Explorer => "Explorer",
            Self::PlanReviewer => "Plan reviewer",
            Self::Implementer => "Implementer",
            Self::CodeReviewer => "Code reviewer",
            Self::FinalReviewer => "Final verifier",
        }
    }
}

impl fmt::Display for RoleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manager => "manager",
            Self::Explorer => "explorer",
            Self::PlanReviewer => "plan_reviewer",
            Self::Implementer => "implementer",
            Self::CodeReviewer => "code_reviewer",
            Self::FinalReviewer => "final_verifier",
        })
    }
}

impl FromStr for RoleKind {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().replace('-', "_").as_str() {
            "manager" => Ok(Self::Manager),
            "explorer" => Ok(Self::Explorer),
            "plan_reviewer" => Ok(Self::PlanReviewer),
            "implementer" => Ok(Self::Implementer),
            "code_reviewer" => Ok(Self::CodeReviewer),
            "final_verifier" | "final_reviewer" => Ok(Self::FinalReviewer),
            _ => Err(format!("unsupported role {value:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Unverified,
    Supported,
    Limited,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchConfig {
    pub provider: Provider,
    pub role: RoleKind,
    pub executable: PathBuf,
    pub executable_version: String,
    pub model: String,
    pub effort: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub environment_keys: Vec<String>,
    pub permission_policy: String,
    #[serde(default)]
    pub security_policy: serde_json::Value,
    pub hook_revision: String,
    pub capability_status: CapabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<crate::provider_compatibility::ProviderCompatibilityBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutableFingerprint {
    pub canonical_path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub modified_seconds: i64,
    pub modified_nanos: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityIdentity {
    pub provider: Provider,
    pub executable: ExecutableFingerprint,
    pub executable_version: String,
    pub role: RoleKind,
    pub model: String,
    pub effort: String,
    pub permission_policy: String,
    pub security_policy: serde_json::Value,
    pub effective_argv: Vec<String>,
    pub environment_contract: Vec<String>,
    pub hook_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_status: Option<CapabilityStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<crate::provider_compatibility::AuthorityBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObservedProcessIdentity {
    pub pid: u32,
    pub parent_pid: u32,
    pub process_group_id: i32,
    pub native_start_marker: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub process_group_id: i32,
    pub native_start_marker: String,
    pub observed_started_at: String,
}

/// Identity retained for the lifetime of one accepted human-control socket.
/// The native connector establishes the PID/start pair before the supervisor
/// consults process ancestry, so a later inventory row cannot silently stand
/// in for a reused numeric PID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerProcessIdentity {
    pub pid: u32,
    pub native_start_marker: String,
}

impl PeerProcessIdentity {
    pub fn new(pid: u32, native_start_marker: String) -> Result<Self, String> {
        if pid == 0 || native_start_marker.trim().is_empty() || native_start_marker.len() > 512 {
            return Err("control peer identity is incomplete".to_owned());
        }
        Ok(Self {
            pid,
            native_start_marker,
        })
    }
}

/// Immutable identity captured when a human terminal attaches to a running
/// provider session. It deliberately includes every value which makes an old
/// terminal capability unsafe after a resume or replacement.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct AttachmentBinding {
    pub session_id: String,
    pub role_generation_id: String,
    pub transcript_epoch: String,
    pub process: ProcessIdentity,
}

impl AttachmentBinding {
    /// The browser routes only already-issued identifiers.  Keep this check at
    /// both ends of the handoff so an attach command cannot turn arbitrary
    /// control-socket input into a new binding.
    pub fn validate_route_identifiers(&self) -> Result<(), String> {
        for (label, value) in [
            ("session", &self.session_id),
            ("role generation", &self.role_generation_id),
            ("transcript epoch", &self.transcript_epoch),
        ] {
            uuid::Uuid::parse_str(value).map_err(|_| format!("{label} must be a UUID"))?;
        }
        if self.process.pid == 0 || self.process.process_group_id <= 0 {
            return Err("attachment process identity is incomplete".to_owned());
        }
        if self.process.native_start_marker.trim().is_empty()
            || self.process.native_start_marker.len() > 512
            || self.process.observed_started_at.trim().is_empty()
            || self.process.observed_started_at.len() > 512
        {
            return Err("attachment process identity is malformed".to_owned());
        }
        Ok(())
    }
}

/// A cmux watch stays connected to output without taking the service's human
/// input lease. Keyboard control is a distinct, explicit route intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmuxAttachmentMode {
    Watch,
    Control,
}

impl CmuxAttachmentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Control => "control",
        }
    }
}

impl Default for CmuxAttachmentMode {
    fn default() -> Self {
        Self::Watch
    }
}

/// Persisted association between one service-owned attachment binding and one
/// cmux surface.  The surface is never authority: the control connection still
/// verifies this full binding before it can view or acquire input.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CmuxAttachmentRoute {
    pub id: String,
    pub service_boot_id: String,
    pub binding: AttachmentBinding,
    pub mode: CmuxAttachmentMode,
    pub workspace_id: Option<String>,
    pub surface_id: Option<String>,
    pub surface_state: String,
    pub attachment_state: String,
    pub resume_state: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A task-scoped cmux workspace is presentation state only.  Its generation
/// advances after proven loss or authenticated discard; it has no authority
/// over provider execution, workflow transitions, or native resume.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CmuxTaskWorkspace {
    pub id: String,
    pub service_boot_id: String,
    pub task_id: String,
    pub generation: i64,
    pub workspace_id: Option<String>,
    pub opening_surface_id: Option<String>,
    pub state: String,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The durable, mode-neutral association between one exact provider binding
/// and one physical cmux terminal surface.  `binding_revision` names the
/// physical-surface generation for this logical session slot: it advances for
/// every replacement surface, including a replacement of the same immutable
/// provider tuple after its attachment ended or failed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CmuxSessionSurface {
    pub id: String,
    pub task_workspace_id: String,
    pub service_boot_id: String,
    pub binding: AttachmentBinding,
    pub binding_revision: i64,
    pub workspace_id: Option<String>,
    pub surface_id: Option<String>,
    pub surface_state: String,
    pub attachment_state: String,
    pub desired_input_state: String,
    pub actual_input_state: String,
    pub control_revision: i64,
    pub applied_revision: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmuxKeyboardControlAction {
    Acquire,
    Release,
}

impl CmuxKeyboardControlAction {
    pub fn desired_input_state(self) -> &'static str {
        match self {
            Self::Acquire => "control",
            Self::Release => "view_only",
        }
    }
}

/// The browser receives no lease material.  It can only see the durable
/// desired/applied revisions and the actual state reported by the exact
/// attachment connection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CmuxViewOutcome {
    pub state: String,
    pub message: String,
    pub retry_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<CmuxSessionSurface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_output: Option<TranscriptPage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CmuxKeyboardControlOutcome {
    pub state: String,
    pub message: String,
    pub surface: CmuxSessionSurface,
}

#[derive(Clone, Debug)]
pub struct CmuxAttachmentControlDirective {
    pub disposition: CmuxAttachmentControlDisposition,
    pub desired_input_state: String,
    pub desired_revision: i64,
    pub applied_revision: i64,
    pub actual_input_state: String,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CmuxAttachmentControlDisposition {
    Apply,
    Hold,
    Retire,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessGenerationAnchor {
    pub pid: u32,
    pub process_group_id: i32,
    pub native_start_marker: String,
    pub boot_identity: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LaunchHandshake {
    ProviderSpawned {
        leader: ProcessGenerationAnchor,
        provider: ProcessGenerationAnchor,
    },
    PreProviderSpawnFailed {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationLaunchRequest {
    pub operation_id: String,
    pub cell: String,
    pub provider: Provider,
    pub role: RoleKind,
    pub project_path: PathBuf,
    pub model: String,
    pub effort: String,
    pub prompt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowValidationRequest {
    pub launch: ValidationLaunchRequest,
    pub task_id: String,
    #[serde(default)]
    pub attempt_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationLaunchResult {
    pub session_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role_generation_id: String,
    pub launch: LaunchConfig,
    pub process: ProcessIdentity,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleContext {
    pub project_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role_generation_id: String,
    pub session_id: String,
    pub credential_id: String,
    pub transcript_epoch: String,
    pub role: RoleKind,
    pub provider: Provider,
    pub configuration_revision: i64,
    pub lane_id: String,
    pub permissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleResultReport {
    pub operation_id: String,
    pub outcome: String,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(
        default = "empty_json_object",
        deserialize_with = "deserialize_json_object"
    )]
    pub metadata: serde_json::Value,
}

const MAX_RUNTIME_REPORT_BYTES: usize = 16 * 1024;
const MAX_RUNTIME_ATOM_BYTES: usize = 512;
const MAX_RUNTIME_OUTCOMES: usize = 16;
const RUNTIME_RESULTS: &[&str] = &[
    "succeeded",
    "denied",
    "invocation_prevented",
    "refused",
    "unavailable",
    "unexpected",
];
const RUNTIME_FAILURES: &[&str] = &[
    "os_denial",
    "provider_denial",
    "shell_construction",
    "model_refusal",
    "observation_unavailable",
    "unexpected_result",
    "authentication_missing",
];
const RUNTIME_FIELDS: &[&str] = &[
    "operation-id",
    "status",
    "nonce",
    "model-evidence",
    "sandbox-identity",
    "session-mode",
    "target-data-accessed",
    "fallback-observed",
    "authentication-observed",
    "failure-category",
    "cmux-stderr-hex",
];

fn runtime_atom(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_RUNTIME_ATOM_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
    {
        return Err(format!(
            "runtime-v1 --{name} must be a bounded decoded ASCII atom"
        ));
    }
    Ok(())
}

fn runtime_cmux_stderr_hex(value: &str) -> Result<(), String> {
    if value == "empty" {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > 8 * 1024
        || value.len() % 2 != 0
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(
            "runtime-v1 --cmux-stderr-hex must be lowercase-or-uppercase hexadecimal or empty"
                .into(),
        );
    }
    Ok(())
}

fn runtime_bool(name: &str, value: String) -> Result<bool, String> {
    match value.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("runtime-v1 --{name} must be true or false")),
    }
}

fn runtime_bool_field(
    fields: &mut std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<bool, String> {
    runtime_bool(name, runtime_field(fields, name)?)
}

fn runtime_outcome(value: String) -> Result<serde_json::Value, String> {
    let fields = value.split(',').collect::<Vec<_>>();
    if fields.len() != 6 {
        return Err("runtime-v1 --actual must contain exactly six comma-separated fields".into());
    }
    for field in &fields {
        runtime_atom("actual", field)?;
    }
    let attempted = match fields[1] {
        "true" => true,
        "false" => false,
        _ => return Err("runtime-v1 actual attempted must be true or false".into()),
    };
    let exit = if fields[2] == "null" {
        None
    } else {
        Some(
            fields[2]
                .parse::<i64>()
                .map_err(|_| "runtime-v1 actual exit must be an integer or null")?,
        )
    };
    let result = fields[3];
    let denial = fields[4];
    let authentication = fields[5];
    if !RUNTIME_RESULTS.contains(&result)
        || !["none", "os", "provider", "shell", "model", "unavailable"].contains(&denial)
        || !["native_session", "none"].contains(&authentication)
    {
        return Err("runtime-v1 actual uses an unsupported observation enum".into());
    }
    let consistent = if !attempted {
        exit.is_none()
            && authentication == "none"
            && matches!(
                (result, denial),
                ("refused", "model") | ("unavailable", "unavailable")
            )
    } else {
        match result {
            "succeeded" => exit == Some(0) && denial == "none",
            "denied" => exit != Some(0) && matches!(denial, "os" | "provider"),
            "invocation_prevented" => denial == "shell",
            "refused" => denial == "model",
            "unavailable" => denial == "unavailable",
            "unexpected" => true,
            _ => false,
        }
    };
    if !consistent {
        return Err("runtime-v1 actual fields describe an inconsistent observation".into());
    }
    Ok(serde_json::json!({
        "operation_id":fields[0], "attempted":attempted, "exit_status":exit,
        "result":result, "denial_source":denial, "authentication_source":authentication
    }))
}

fn runtime_field(
    fields: &mut std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<String, String> {
    fields
        .remove(name)
        .ok_or_else(|| format!("runtime-v1 requires --{name}"))
}

fn runtime_outcomes(actual: Vec<String>) -> Result<serde_json::Value, String> {
    let mut outcomes = actual
        .into_iter()
        .map(runtime_outcome)
        .collect::<Result<Vec<_>, _>>()?;
    outcomes.sort_by(|left, right| {
        left["operation_id"]
            .as_str()
            .cmp(&right["operation_id"].as_str())
    });
    if outcomes
        .windows(2)
        .any(|pair| pair[0]["operation_id"] == pair[1]["operation_id"])
    {
        return Err("runtime-v1 actual operation IDs must be unique".into());
    }
    Ok(serde_json::Value::Array(outcomes))
}

fn runtime_role_report(
    mut fields: std::collections::BTreeMap<String, String>,
    actual: Vec<String>,
) -> Result<RoleResultReport, String> {
    let operation_id = runtime_field(&mut fields, "operation-id")?;
    let status = runtime_field(&mut fields, "status")?;
    let (history_nonce, observation) = match status.as_str() {
        "passed" => {
            if actual.is_empty() {
                return Err("runtime-v1 passed requires actual outcomes".into());
            }
            let nonce = runtime_field(&mut fields, "nonce")?;
            let session = runtime_field(&mut fields, "session-mode")?;
            if !["fresh", "retained"].contains(&session.as_str()) {
                return Err("runtime-v1 session-mode must be fresh or retained".into());
            }
            let sandbox_identity = fields.remove("sandbox-identity");
            let mut observation = serde_json::json!({
                "cell":"trip_runtime_probe", "status":"passed", "nonce":&nonce,
                "model_evidence":runtime_field(&mut fields, "model-evidence")?,
                "session_mode_observed":session,
                "target_data_accessed":runtime_bool_field(&mut fields, "target-data-accessed")?,
                "fallback_observed":runtime_bool_field(&mut fields, "fallback-observed")?,
                "authentication_observed":runtime_bool_field(&mut fields, "authentication-observed")?,
                "actual_outcomes":runtime_outcomes(actual)?
            });
            if let Some(sandbox_identity) = sandbox_identity {
                observation["effective_sandbox_identity"] =
                    serde_json::Value::String(sandbox_identity);
            }
            if let Some(stderr) = fields.remove("cmux-stderr-hex") {
                observation["cmux_socket_stderr_hex"] = serde_json::Value::String(stderr);
            }
            (Some(nonce), observation)
        }
        "failed" => {
            if actual.is_empty() {
                return Err("runtime-v1 failed requires actual outcomes".into());
            }
            let category = runtime_field(&mut fields, "failure-category")?;
            if !RUNTIME_FAILURES.contains(&category.as_str()) {
                return Err("runtime-v1 failure-category is unsupported".into());
            }
            let mut observation = serde_json::json!({
                "cell":"trip_runtime_probe", "status":"failed",
                "failure_category":category, "actual_outcomes":runtime_outcomes(actual)?
            });
            if let Some(stderr) = fields.remove("cmux-stderr-hex") {
                observation["cmux_socket_stderr_hex"] = serde_json::Value::String(stderr);
            }
            (None, observation)
        }
        "missing_context" if actual.is_empty() => (
            None,
            serde_json::json!({"cell":"trip_runtime_probe","status":"missing_context"}),
        ),
        "missing_context" => return Err("runtime-v1 missing_context accepts no outcomes".into()),
        _ => return Err("runtime-v1 status is unsupported".into()),
    };
    if !fields.is_empty() {
        return Err(format!(
            "runtime-v1 {status} contains a forbidden or duplicate-status field"
        ));
    }
    let mut metadata = serde_json::json!({
        "runtime_report_format":"runtime-v1", "validation_observation":observation
    });
    if let Some(nonce) = history_nonce {
        metadata["history_nonce"] = serde_json::json!(nonce);
    }
    Ok(RoleResultReport {
        operation_id,
        outcome: "capability_observed".into(),
        summary: "runtime-v1 native observation".into(),
        evidence: Vec::new(),
        metadata,
    })
}

pub fn parse_runtime_role_report_args(args: &[String]) -> Result<RoleResultReport, String> {
    if args.len() > 32 || args.iter().map(String::len).sum::<usize>() > MAX_RUNTIME_REPORT_BYTES {
        return Err("runtime-v1 command exceeds its bounded argument limits".into());
    }
    let mut fields = std::collections::BTreeMap::new();
    let mut actual = Vec::new();
    let mut marker = false;
    for argument in args {
        if argument == "--runtime-v1" {
            if std::mem::replace(&mut marker, true) {
                return Err("runtime-v1 marker is duplicated".into());
            }
            continue;
        }
        let (flag, value) = argument
            .split_once('=')
            .ok_or_else(|| "runtime-v1 fields require --name=value syntax".to_owned())?;
        let name = flag
            .strip_prefix("--")
            .ok_or_else(|| "runtime-v1 fields require --name=value syntax".to_owned())?;
        match name {
            "actual" => {
                actual.push(value.to_owned());
                continue;
            }
            _ => {}
        }
        if !RUNTIME_FIELDS.contains(&name) {
            return Err("runtime-v1 command contains an unknown field".into());
        }
        if name == "cmux-stderr-hex" {
            runtime_cmux_stderr_hex(value)?;
        } else {
            runtime_atom(name, value)?;
        }
        if fields.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err("runtime-v1 command contains a duplicate field".into());
        }
    }
    if !marker {
        return Err("runtime-v1 marker is required".into());
    }
    if actual.len() > MAX_RUNTIME_OUTCOMES {
        return Err("runtime-v1 report exceeds its bounded outcome count".into());
    }
    runtime_role_report(fields, actual)
}

fn empty_json_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn deserialize_json_object<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("metadata must be a JSON object"))
    }
}

macro_rules! role_report_shell_quoting_guidance {
    () => {
        r#"Run the direct standalone command `<exact-executable> role report --json '<literal JSON>'`, replacing `<exact-executable>` with the executable shown by this surface and keeping that executable exact. The command restrictions in this paragraph apply only to invoking `role report`; they do not remove separately granted role permissions or task instructions. In an ordinary task review, separately authorized read-only workspace inspection remains permitted, but this paragraph does not authorize inspection during setup, validation, or retained-recall contexts. For the report invocation itself, do not use pipes, redirects, heredocs, scratch files, command substitutions, helper subprocesses, or extra native commands or reports. Inside the INNER `reportJSON` placed within the shell command's outer single quotes, encode every apostrophe in JSON string values as the JSON Unicode escape `\u0027` (one actual inner JSON escape, not the double-escaped inner literal `\\u0027`). Inner syntax-only fragment example, never evidence or a complete report: `{"summary":"probe didn\u0027t run","command":"/bin/sh -lc \u0027printf %s ok\u0027"}`. The shell command STRING delivered to native Bash must contain the literal six-character sequence `\u0027` within `reportJSON`, not a raw apostrophe. When expressing that command string in the OUTER native-tool input JSON, JSON-escape the backslash itself as `\\u0027` so it survives the outer decode: outer representation `\\u0027` becomes `\u0027` in the decoded shell command, then the inner `reportJSON` decode restores the apostrophe in the report value. Shell parsing removes the surrounding shell single quotes and preserves each literal six-character `\u0027` sequence unchanged in the `reportJSON` argument before the subsequent inner JSON decode restores the apostrophes. Illustrative native tool-input JSON fragment, syntax only and not executable evidence or a complete report: `{"command":"<exact-executable> role report --json '{\"summary\":\"probe didn\\u0027t run\",\"command\":\"/bin/sh -lc \\u0027printf %s ok\\u0027\"}'"}`. The warning against a double-escaped literal applies to the INNER `reportJSON`; it does not remove the required backslash escaping at the OUTER native-tool input JSON transport layer. Before submission, check that the decoded shell command's payload inside the outer shell single quotes contains no raw apostrophe and that the two decodes preserve the exact original values. Correctly encoded and quoted shell metacharacters remain data; do not strip or alter `actual_outcomes.command` semantics."#
    };
}

pub const ROLE_REPORT_SHELL_QUOTING_GUIDANCE: &str = role_report_shell_quoting_guidance!();

pub const ROLE_RESULT_REPORT_CONTRACT: &str = concat!(
    r#"Role result reporting contract:
"#,
    role_report_shell_quoting_guidance!(),
    r#"
Required fields: `operation_id` is non-empty and unique for each new logical report (reuse the same ID only for an identical replay); `outcome` is required; `summary` is non-empty and at most 65536 bytes. Optional `evidence` is an array of strings (at most 128 entries and 8192 bytes per string). Omitted `metadata` becomes an empty object; when explicitly supplied it must be an object.
Minimal valid literal JSON example (replace every angle-bracket placeholder; this is syntax guidance, not an approval): `{"operation_id":"<new-unique-operation-id>","outcome":"<allowed-outcome-for-role>","summary":"<non-empty-summary>","evidence":[],"metadata":{}}`
Normal manager outcomes: `plan_ready`, `handoff_ready`, `blocked`, or `needs_input`. `plan_ready` requires `metadata.plan` to be a non-empty string after trimming, of at most 262144 bytes, plus `metadata.structured_plan` with exact `outcomes_scope`, `classification`, `ownership`, `acceptance_criteria`, `test_policy`, `verification_matrix`, `documentation`, `restrictions`, `explorer_disposition`, `unresolved_decisions`, `config_revision_id`, and `conformance` fields. `handoff_ready` requires `metadata.candidate_hash` and `metadata.final_review_request_id` to be non-empty strings.
Explorer outcomes are `evidence_ready`, `blocked`, or `needs_input`; each requires `metadata.explorer_decision_id`. Explorer evidence is non-approving and cannot replace plan, code, final, or human authority.
Normal plan, code, and final reviewer outcomes: `approved`, `request_changes`, or `needs_rework`. `metadata.review_request_id` and `metadata.candidate_hash` must be non-empty strings containing the exact current delivered request values; `metadata.review_kind` must be the non-empty string `plan`, `code`, or `final` matching the current review. The server transactionally checks the request, session, and generation before accepting the report.
Normal implementer outcomes: `candidate_ready`, `blocked`, or `needs_input`.
Use `capability_observed` only during explicitly authorized validation and include `metadata.validation_observation` as a bounded non-empty string or non-empty object. Null, empty, array, numeric, and boolean observations are invalid; when an observation object includes `cell`, it must match the server-owned cell. Isolated validation accepts only `capability_observed`; workflow validation retains the normal role outcomes above. This is not a production-outcome bypass. Never force approval or invent evidence. Independent human and workflow gates remain authoritative."#
);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookEnvelope {
    pub provider: Provider,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptFrame {
    pub epoch: String,
    pub sequence: u64,
    pub captured_at: String,
    pub encoding: String,
    pub data: String,
    pub gap: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub frames: Vec<TranscriptFrame>,
    pub next_epoch: Option<String>,
    pub next_sequence: u64,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecycle {
    Backlog,
    Ready,
    InProgress,
    Validation,
    AwaitingReview,
    Done,
    Cancelled,
}

impl fmt::Display for TaskLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Backlog => "backlog",
            Self::Ready => "ready",
            Self::InProgress => "in_progress",
            Self::Validation => "validation",
            Self::AwaitingReview => "awaiting_review",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleOverride {
    pub provider: Provider,
    pub model: String,
    pub effort: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectDto {
    pub id: String,
    pub display_name: String,
    pub repository_path: PathBuf,
    pub repository_identity: PathBuf,
    pub base_revision: String,
    pub queue_paused: bool,
    pub version: i64,
    pub settings: serde_json::Value,
    pub trip: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: i64,
    pub manual_order: i64,
    pub lifecycle: String,
    pub attention: String,
    pub version: i64,
    pub archived: bool,
    pub can_archive: bool,
    pub recipe_provenance: Option<serde_json::Value>,
    pub permission_waiting: bool,
    pub role_overrides: serde_json::Value,
    pub dependencies: Vec<serde_json::Value>,
    pub active_attempt: Option<serde_json::Value>,
    pub role_settings: Vec<serde_json::Value>,
    pub reviews: Vec<serde_json::Value>,
    pub snapshots: Vec<serde_json::Value>,
    pub review_budgets: Vec<serde_json::Value>,
    pub legacy: serde_json::Value,
    /// What the unfinished task is waiting for and whether its work is
    /// actually advancing. Absent for finished tasks and tasks without work.
    #[serde(default)]
    pub progress: Option<TaskProgress>,
}

/// Where a task stands, derived from authoritative workflow evidence. Hook
/// traffic and process liveness are reported separately and never count as
/// progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    /// Stable code of the current decision.
    pub reason_code: String,
    /// Plain explanation of the wait.
    pub waiting_reason: String,
    /// `you`, `llmrelay`, `agent` or `external`.
    pub responsible: String,
    pub responsible_role: Option<RoleKind>,
    /// The user-facing label of the next supported step and its exact target;
    /// never an internal operation identifier.
    pub next_operation: Option<String>,
    pub next_target: Option<AttentionTarget>,
    /// When the unresolved thing the task waits on was created: a permission
    /// request, hold, recovery, control, review request or the agent step in
    /// progress. `None` while the task can act now, or when not recorded.
    pub waiting_since: Option<String>,
    /// The newest authoritative workflow evidence: an accepted, unretired
    /// report, decision, confirmed delivery, boundary, finished review,
    /// capture, recovery or audited phase change.
    pub last_meaningful_at: Option<String>,
    pub last_meaningful_event: Option<String>,
    /// Newest hook from any of the attempt's agents; activity, not progress.
    pub last_agent_activity_at: Option<String>,
    /// `no_live_agent`; `agent_live_idle` when an agent is running and no hook
    /// is newer than the newest evidence (which does not prove it is idle);
    /// `agent_active_without_progress` when its hooks are newer than any
    /// recorded workflow step.
    pub activity: String,
}

pub const DECISION_SCHEMA_V1: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionDisposition {
    Ready,
    Waiting,
    Held,
    RetryDeferred,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionEvidenceState {
    Satisfied,
    Missing,
    Stale,
    Pending,
    Uncertain,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOwner {
    Service,
    Human,
    Provider,
    External,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionSubject {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_generation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionObservedRevision {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_checks_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionPrerequisite {
    pub code: String,
    pub state: DecisionEvidenceState,
    pub owner: DecisionOwner,
    #[serde(default)]
    pub evidence: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionActionBinding {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role_generation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_task_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_project_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_instance_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_checks_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_queue_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionOwnership {
    pub owner: DecisionOwner,
    pub state: String,
    #[serde(default)]
    pub binding: DecisionActionBinding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionNextAction {
    pub operation: String,
    pub enabled: bool,
    pub owner: DecisionOwner,
    pub binding: DecisionActionBinding,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounting_note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DecisionControlPolicy {
    #[serde(default)]
    pub allowed_controls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason_code: Option<String>,
}

/// A descriptive snapshot of owner-evaluated facts. It is never mutation authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionExplanation {
    pub decision_schema: u8,
    pub reason_code: String,
    pub disposition: DecisionDisposition,
    pub subject: DecisionSubject,
    pub observed_revision: DecisionObservedRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_blocker: Option<DecisionPrerequisite>,
    pub prerequisites: Vec<DecisionPrerequisite>,
    pub ownership: DecisionOwnership,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<DecisionNextAction>,
    pub control_policy: DecisionControlPolicy,
}

impl DecisionExplanation {
    pub fn canonical_value(&self) -> serde_json::Value {
        serde_json::json!({
            "decision_schema": self.decision_schema,
            "reason_code": self.reason_code,
            "disposition": self.disposition,
            "subject": self.subject,
            "observed_revision": self.observed_revision,
            "primary_blocker": self.primary_blocker.as_ref().map(|item| serde_json::json!({
                "code": item.code,
                "state": item.state,
                "owner": item.owner,
                "evidence": canonical_decision_evidence(&item.evidence),
            })),
            "prerequisites": self.prerequisites.iter().map(|item| serde_json::json!({
                "code": item.code,
                "state": item.state,
                "owner": item.owner,
                "evidence": canonical_decision_evidence(&item.evidence),
            })).collect::<Vec<_>>(),
            "ownership": self.ownership,
            "next_action": self.next_action.as_ref().map(|action| serde_json::json!({
                "operation": action.operation,
                "enabled": action.enabled,
                "owner": action.owner,
                "binding": action.binding,
            })),
            "control_policy": self.control_policy,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPreviewClassification {
    Resumable,
    FreshOnly,
    Blocked,
    Uncertain,
    AwaitingApproval,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestartPreviewSession {
    pub classification: RestartPreviewClassification,
    pub can_resume_now: bool,
    pub could_resume_after_confirmed_shutdown: bool,
    pub decision: DecisionExplanation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestartPreviewSnapshot {
    pub captured_at: String,
    pub process_inventory: DecisionEvidenceState,
    pub boot_identity: DecisionEvidenceState,
    pub dispatch_enabled: bool,
    pub draining: bool,
    pub revalidation_required: bool,
    pub notice: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestartPreview {
    pub decision_schema: u8,
    pub snapshot: RestartPreviewSnapshot,
    pub sessions: Vec<RestartPreviewSession>,
}

pub(crate) const RESTART_METADATA_KEY: &str = "llmrelay_restart_v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestartBatchMembership {
    Queued,
    Admitting,
    Completed,
    Terminated,
}

impl RestartBatchMembership {
    pub(crate) fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Admitting)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestartBatchV1 {
    pub(crate) operation_id: String,
    pub(crate) ordinal: u8,
    pub(crate) members: Vec<String>,
    pub(crate) membership: RestartBatchMembership,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestartAdmissionV1 {
    pub(crate) id: String,
    pub(crate) attempt_id: String,
    pub(crate) role_generation_id: String,
    pub(crate) expected_task_version: i64,
    pub(crate) prior_candidate_state: String,
    pub(crate) prior_transcript_epoch: String,
    pub(crate) expected_resume_ordinal: u32,
    pub(crate) requested_by: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestartMetadataV1 {
    pub(crate) version: u8,
    pub(crate) batch: Option<RestartBatchV1>,
    pub(crate) batch_history: Vec<RestartBatchV1>,
    pub(crate) replacement_failures: u8,
    pub(crate) capacity_deferrals: u32,
    pub(crate) next_due_at: Option<String>,
    pub(crate) admission: Option<RestartAdmissionV1>,
}

impl Default for RestartMetadataV1 {
    fn default() -> Self {
        Self {
            version: 1,
            batch: None,
            batch_history: Vec::new(),
            replacement_failures: 0,
            capacity_deferrals: 0,
            next_due_at: None,
            admission: None,
        }
    }
}

impl RestartMetadataV1 {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.version != 1 {
            anyhow::bail!("restart metadata version {} is unsupported", self.version)
        }
        if self.replacement_failures > 3 {
            anyhow::bail!("restart replacement failure count exceeds the supported cap")
        }
        if let Some(next_due_at) = self.next_due_at.as_deref() {
            chrono::DateTime::parse_from_rfc3339(next_due_at)
                .map_err(|_| anyhow::anyhow!("restart metadata next_due_at is malformed"))?;
        }
        for batch in self.batch.iter().chain(self.batch_history.iter()) {
            if batch.operation_id.trim().is_empty()
                || batch.operation_id.len() > 512
                || batch.members.is_empty()
                || batch.members.len() > 4
                || batch.ordinal == 0
                || usize::from(batch.ordinal) > batch.members.len()
                || batch.members[usize::from(batch.ordinal) - 1]
                    .trim()
                    .is_empty()
            {
                anyhow::bail!("restart batch metadata is incomplete or out of bounds")
            }
            let mut unique = std::collections::HashSet::new();
            if batch.members.iter().any(|member| {
                member.trim().is_empty() || member.len() > 512 || !unique.insert(member)
            }) {
                anyhow::bail!("restart batch metadata contains blank or duplicate members")
            }
        }
        if self
            .batch_history
            .iter()
            .any(|batch| batch.membership.active())
        {
            anyhow::bail!("restart batch history contains active authority")
        }
        if let Some(admission) = self.admission.as_ref() {
            if admission.id.trim().is_empty()
                || admission.attempt_id.trim().is_empty()
                || admission.role_generation_id.trim().is_empty()
                || admission.expected_task_version < 1
                || !matches!(
                    admission.prior_candidate_state.as_str(),
                    "parked" | "queued_capacity" | "failed"
                )
                || admission.prior_transcript_epoch.trim().is_empty()
                || admission.expected_resume_ordinal == 0
                || !matches!(admission.requested_by.as_str(), "human" | "auto")
            {
                anyhow::bail!("restart admission metadata is incomplete")
            }
        }
        if let Some(batch) = self.batch.as_ref() {
            if (batch.membership == RestartBatchMembership::Admitting) != self.admission.is_some() {
                anyhow::bail!("restart batch and admission metadata disagree")
            }
        }
        Ok(())
    }

    pub(crate) fn active_batch(&self) -> Option<&RestartBatchV1> {
        self.batch
            .as_ref()
            .filter(|batch| batch.membership.active())
    }

    pub(crate) fn terminalize_batch(&mut self) {
        if let Some(batch) = self.batch.as_mut() {
            if batch.membership.active() {
                batch.membership = RestartBatchMembership::Terminated;
            }
        }
        self.admission = None;
    }

    pub(crate) fn begin_batch(&mut self, batch: RestartBatchV1) -> anyhow::Result<()> {
        if self.active_batch().is_some() {
            anyhow::bail!("an active restart batch membership already owns this candidate")
        }
        if let Some(previous) = self.batch.replace(batch) {
            self.batch_history.push(previous);
        }
        self.validate()
    }
}

/// A restart candidate owns a versioned namespace inside its existing result
/// object. Parsing is deliberately strict once that namespace exists so partial
/// durable accounting cannot silently become a fresh set of counters.
#[derive(Clone, Debug)]
pub(crate) struct RestartCandidateResult {
    root: serde_json::Map<String, serde_json::Value>,
    pub(crate) restart: RestartMetadataV1,
}

impl RestartCandidateResult {
    pub(crate) fn parse(raw: &str) -> anyhow::Result<Self> {
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|error| anyhow::anyhow!("restart candidate result is malformed: {error}"))?;
        let serde_json::Value::Object(root) = value else {
            anyhow::bail!("restart candidate result must be a JSON object")
        };
        let restart = match root.get(RESTART_METADATA_KEY) {
            None => RestartMetadataV1::default(),
            Some(value) => {
                let object = value.as_object().ok_or_else(|| {
                    anyhow::anyhow!("restart metadata namespace must be a JSON object")
                })?;
                for required in [
                    "version",
                    "batch",
                    "batch_history",
                    "replacement_failures",
                    "capacity_deferrals",
                    "next_due_at",
                    "admission",
                ] {
                    if !object.contains_key(required) {
                        anyhow::bail!("restart metadata is partial: missing {required}")
                    }
                }
                serde_json::from_value::<RestartMetadataV1>(value.clone())
                    .map_err(|error| anyhow::anyhow!("restart metadata is malformed: {error}"))?
            }
        };
        restart.validate()?;
        Ok(Self { root, restart })
    }

    pub(crate) fn validate_candidate_state(
        &self,
        state: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        if !matches!(
            state,
            "pending_reconciliation"
                | "parked"
                | "queued_capacity"
                | "failed"
                | "blocked"
                | "skipped"
                | "admitting"
                | "resumed"
                | "released_fresh_dispatch"
                | "cancelled"
        ) {
            anyhow::bail!("restart candidate state is unsupported")
        }
        for batch in self
            .restart
            .batch
            .iter()
            .chain(self.restart.batch_history.iter())
        {
            if batch.members[usize::from(batch.ordinal) - 1] != session_id {
                anyhow::bail!("restart candidate and exact batch membership disagree")
            }
        }
        if (state == "admitting") != self.restart.admission.is_some() {
            anyhow::bail!("restart candidate state and admission ownership disagree")
        }
        if let Some(batch) = self.restart.active_batch() {
            let matches_state = match state {
                "queued_capacity" | "pending_reconciliation" => {
                    batch.membership == RestartBatchMembership::Queued
                }
                "admitting" => batch.membership == RestartBatchMembership::Admitting,
                _ => false,
            };
            if !matches_state {
                anyhow::bail!("restart candidate state and active batch membership disagree")
            }
        }
        Ok(())
    }

    pub(crate) fn set(&mut self, key: &str, value: serde_json::Value) {
        if key != RESTART_METADATA_KEY {
            self.root.insert(key.to_owned(), value);
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.root.get(key)
    }

    pub(crate) fn encode(mut self) -> anyhow::Result<String> {
        self.restart.validate()?;
        self.root.insert(
            RESTART_METADATA_KEY.to_owned(),
            serde_json::to_value(self.restart)?,
        );
        Ok(serde_json::Value::Object(self.root).to_string())
    }
}

fn canonical_decision_evidence(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .filter(|(key, _)| {
                    key.ends_with("_id")
                        || key.ends_with("_code")
                        || key.ends_with("_hash")
                        || key.ends_with("_state")
                        || key.ends_with("_status")
                        || key.ends_with("_revision")
                        || key.ends_with("_identity")
                        || key.ends_with("_count")
                        || matches!(
                            key.as_str(),
                            "attention"
                                | "candidate_state"
                                | "claims"
                                | "evaluated"
                                | "kind"
                                | "lane_key"
                                | "lifecycle"
                                | "phase"
                                | "process_group_quiescent"
                                | "profile_session"
                                | "provider"
                                | "queue_paused"
                                | "role"
                                | "run_next_requested"
                                | "short_circuit"
                                | "state"
                                | "status"
                        )
                })
                .map(|(key, value)| (key.clone(), canonical_decision_evidence(value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_decision_evidence).collect())
        }
        _ => value.clone(),
    }
}

/// A current, non-authoritative explanation of the next liveness transition.
/// Commands must still recheck the immutable bindings in a transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationActionKind {
    ExactResume,
    FreshAccountedRetry,
    ReplaceStaleAuthority,
    WaitForExit,
    WaitForCapacity,
    WaitForService,
    RecoverOwnership,
    RetryGracefulStop,
    ForceStopExactProcess,
    PrepareCorrectedRuntime,
    AuthorizeImplementation,
    MigrateAttempt,
    AuthorizeAdditionalExplorer,
    RecoverSetupApply,
    RecoverWorkspaceReservation,
    ContinueFreshDispatch,
    StartManagedLegacyAttempt,
    RefreshAndReconcile,
    AuthorizationRequired,
    TerminalIncomplete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContinuationAction {
    pub kind: ContinuationActionKind,
    pub enabled: bool,
    pub reason: String,
    pub owner: String,
    pub waiting_for: Option<String>,
    pub since: Option<String>,
    pub deadline_at: Option<String>,
    pub operation: String,
    #[serde(default)]
    pub binding: serde_json::Value,
    pub accounting_note: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    ApproveOnce,
    AlwaysApprove,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLifetime {
    Session,
    Project,
}

impl fmt::Display for PermissionLifetime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Session => "session",
            Self::Project => "project",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRequestDto {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub session_id: String,
    pub role_generation_id: String,
    pub role: RoleKind,
    pub provider: Provider,
    pub native_session_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub requested_access: serde_json::Value,
    pub reason: Option<String>,
    pub command_display: Option<String>,
    pub family_preview: serde_json::Value,
    pub family_unavailable_reason: Option<String>,
    pub created_at: String,
    pub deadline_at: String,
    pub state: String,
    pub revision: i64,
    pub decision_kind: Option<String>,
    pub decision_actor: Option<String>,
    pub decision_reason: Option<String>,
    pub decided_at: Option<String>,
    pub matching_rule_id: Option<String>,
    pub delivery_state: String,
    pub delivery_reserved_at: Option<String>,
    pub reserved_behavior: Option<String>,
    pub delivered_at: Option<String>,
    pub delivery_unknown_at: Option<String>,
    pub delivery_reason: Option<String>,
    pub consumed_at: Option<String>,
    /// Pending an application decision with no observed native resolution.
    pub actionable: bool,
    /// Whether this request's own PermissionRequest hook was identified
    /// exactly; without it a native answer can never be observed.
    pub native_correlation_available: bool,
    pub native_resolution: Option<PermissionNativeResolutionDto>,
}

/// The provider hook that proved the native prompt for a request was answered
/// outside LLMRelay. It records no LLMRelay decision and no delivery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionNativeResolutionDto {
    pub kind: PermissionNativeResolutionKind,
    pub hook_event_id: String,
    pub tool_use_id: String,
    pub observed_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionNativeResolutionKind {
    /// `PostToolUse`: the provider reports the tool call finished.
    ToolFinished,
    /// `PostToolUseFailure`: the provider reports the tool call failed.
    ToolFailed,
    /// `PermissionDenied`: the provider denied the call natively.
    NativeDenied,
}

impl PermissionNativeResolutionKind {
    pub fn from_hook_event(event_name: &str) -> Option<Self> {
        match event_name {
            "PostToolUse" => Some(Self::ToolFinished),
            "PostToolUseFailure" => Some(Self::ToolFailed),
            "PermissionDenied" => Some(Self::NativeDenied),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolFinished => "tool_finished",
            Self::ToolFailed => "tool_failed",
            Self::NativeDenied => "native_denied",
        }
    }
}

impl FromStr for PermissionNativeResolutionKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "tool_finished" => Ok(Self::ToolFinished),
            "tool_failed" => Ok(Self::ToolFailed),
            "native_denied" => Ok(Self::NativeDenied),
            other => Err(format!("unknown native permission resolution {other}")),
        }
    }
}

/// The newest native turn of a session's current invocation, taken only from
/// trusted hooks of that exact session, generation, native identity and
/// transcript epoch. Written input bytes are not a turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeTurnDto {
    /// The trusted `UserPromptSubmit` that opened the turn, when one exists.
    pub accepted_hook_event_id: Option<String>,
    pub accepted_at: Option<String>,
    /// Set when the provider ended this turn with a supported failure hook;
    /// a later Stop, tool activity or accepted turn clears it.
    pub failure: Option<NativeTurnFailureDto>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeTurnFailureDto {
    /// The turn's first failure hook, stable across duplicate deliveries.
    pub hook_event_id: String,
    pub kind: NativeTurnFailureKind,
    /// The provider's own error value, bounded.
    pub provider_error: String,
    pub details: Option<String>,
    pub observed_at: String,
}

/// A generic wait the provider announced with a Notification hook in the
/// current turn. It names no operation, so it is answered only in the agent's
/// own output and never becomes an application approval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativePromptDto {
    /// The wait's first notification, stable across repeated reminders.
    pub hook_event_id: String,
    pub kind: NativePromptKind,
    pub observed_at: String,
}

/// Notification types of the admitted Claude Code release that announce a
/// wait for the user; other types claim no wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePromptKind {
    PermissionPrompt,
    ElicitationDialog,
    ElicitationUrlDialog,
    AgentNeedsInput,
}

impl FromStr for NativePromptKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "permission_prompt" => Ok(Self::PermissionPrompt),
            "elicitation_dialog" => Ok(Self::ElicitationDialog),
            "elicitation_url_dialog" => Ok(Self::ElicitationUrlDialog),
            "agent_needs_input" => Ok(Self::AgentNeedsInput),
            other => Err(format!(
                "notification type {other} does not announce a wait"
            )),
        }
    }
}

/// Claude `StopFailure` error values of the admitted Claude Code release. Any
/// other value is `Unknown`; its text stays in `provider_error`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTurnFailureKind {
    AuthenticationFailed,
    OauthOrgNotAllowed,
    AccountOnHold,
    VerificationRequired,
    BillingError,
    RateLimit,
    Overloaded,
    InvalidRequest,
    ModelNotFound,
    ServerError,
    MaxOutputTokens,
    CloudCredentialError,
    Unknown,
}

impl NativeTurnFailureKind {
    pub fn from_provider_error(value: &str) -> Self {
        match value {
            "authentication_failed" => Self::AuthenticationFailed,
            "oauth_org_not_allowed" => Self::OauthOrgNotAllowed,
            "account_on_hold" => Self::AccountOnHold,
            "verification_required" => Self::VerificationRequired,
            "billing_error" => Self::BillingError,
            "rate_limit" => Self::RateLimit,
            "overloaded" => Self::Overloaded,
            "invalid_request" => Self::InvalidRequest,
            "model_not_found" => Self::ModelNotFound,
            "server_error" => Self::ServerError,
            "max_output_tokens" => Self::MaxOutputTokens,
            "cloud_credential_error" => Self::CloudCredentialError,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRuleDto {
    pub id: String,
    pub provider: Provider,
    pub project_id: String,
    pub role: RoleKind,
    pub lifetime: PermissionLifetime,
    pub session_id: Option<String>,
    pub display_family: String,
    pub scope: serde_json::Value,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub last_used_at: Option<String>,
    pub use_count: i64,
    pub revision: i64,
}

/// Dashboard attention groups, rendered in declaration order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionCategory {
    Permission,
    Decision,
    Recovery,
    Compatibility,
    Blocked,
    AwaitingAcceptance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskAttentionTarget {
    pub project_id: String,
    pub task_id: String,
    pub task_version: i64,
}

/// The exact entity an attention item opens. The dashboard refuses a target
/// that no longer matches its latest snapshot instead of retargeting it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttentionTarget {
    Task(TaskAttentionTarget),
    Attempt {
        project_id: String,
        task_id: String,
        attempt_id: String,
        phase: String,
        plan_hash: Option<String>,
        candidate_hash: Option<String>,
    },
    Session {
        project_id: String,
        task_id: String,
        attempt_id: String,
        session_id: String,
        role_generation_id: String,
    },
    PermissionRequest {
        project_id: String,
        task_id: String,
        attempt_id: String,
        session_id: String,
        request_id: String,
        request_revision: i64,
    },
    RecoveryRecord {
        project_id: String,
        task_id: String,
        attempt_id: String,
        recovery_id: String,
    },
    ProjectSetup {
        project_id: String,
        setup_operation_id: Option<String>,
    },
    /// One role's settings in a task's Agent settings.
    RoleSettings {
        project_id: String,
        task_id: String,
        role: RoleKind,
        settings_revision: i64,
    },
    /// The service-wide Diagnostics page.
    Diagnostics,
}

/// What opening an attention item lets the user do. The dashboard shows this
/// label on the item's button and routes only to the item's exact target.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionActionKind {
    ReviewPlan,
    ReviewRequest,
    ReviewResult,
    AnswerQuestion,
    OpenAgentOutput,
    OpenProjectSetup,
    OpenAgentSettings,
    OpenDiagnostics,
    ResolveIssue,
}

impl AttentionActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReviewPlan => "Review plan",
            Self::ReviewRequest => "Review request",
            Self::ReviewResult => "Review result",
            Self::AnswerQuestion => "Answer question",
            Self::OpenAgentOutput => "Open agent output",
            Self::OpenProjectSetup => "Open project setup",
            Self::OpenAgentSettings => "Open agent settings",
            Self::OpenDiagnostics => "Open diagnostics",
            Self::ResolveIssue => "Resolve issue",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttentionAction {
    pub kind: AttentionActionKind,
    pub label: String,
}

impl From<AttentionActionKind> for AttentionAction {
    fn from(kind: AttentionActionKind) -> Self {
        Self {
            kind,
            label: kind.label().to_owned(),
        }
    }
}

/// Presentation of one reason for the user's attention, dated by the enclosing
/// snapshot cursor. It grants nothing: every action rechecks its own revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttentionItem {
    /// Stable while the same entity needs the same attention.
    pub id: String,
    pub category: AttentionCategory,
    /// Plain statement of what is waiting; never an internal identifier.
    pub title: String,
    pub reason: String,
    /// The affected task's title, when the item belongs to one task.
    pub task_title: Option<String>,
    /// The affected role, for example `implementer`, when one role is involved.
    pub role: Option<RoleKind>,
    pub action: AttentionAction,
    /// `None` when no current dashboard entity can act on the item: the
    /// offline-released restore hold, or a binding to a superseded entity.
    pub target: Option<AttentionTarget>,
    /// Tasks fenced by the instance restore hold; empty for other items.
    pub held_tasks: Vec<TaskAttentionTarget>,
    /// Raw diagnostic text for collapsed technical details; never primary copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppStateDto {
    pub schema: u8,
    pub generated_at: String,
    /// Committed state revision read under the same store guard as every
    /// projected row. A canonical decimal string, so browsers never round it.
    pub revision: String,
    pub projects: Vec<ProjectDto>,
    pub tasks: Vec<TaskDto>,
    pub profile_sets: Vec<serde_json::Value>,
    pub task_recipes: Vec<serde_json::Value>,
    pub recipe_schedules: Vec<serde_json::Value>,
    pub production_role_restrictions: Vec<serde_json::Value>,
    pub capabilities: Vec<serde_json::Value>,
    pub active_sessions: Vec<serde_json::Value>,
    pub controls: Vec<serde_json::Value>,
    pub guidance: Vec<serde_json::Value>,
    pub check_suites: Vec<serde_json::Value>,
    pub checks: Vec<serde_json::Value>,
    pub switches: Vec<serde_json::Value>,
    pub recovery: Vec<serde_json::Value>,
    pub history: Vec<serde_json::Value>,
    pub resources: serde_json::Value,
    pub instance_settings: serde_json::Value,
    pub restart_candidates: Vec<serde_json::Value>,
    pub permission_requests: Vec<PermissionRequestDto>,
    pub permission_rules: Vec<PermissionRuleDto>,
    pub trip_setups: Vec<serde_json::Value>,
    pub trip_explorer: Vec<serde_json::Value>,
    pub trip_lanes: Vec<serde_json::Value>,
    pub trip_checks: Vec<serde_json::Value>,
    pub trip_task_verification: Vec<serde_json::Value>,
    #[serde(default)]
    pub decisions: Vec<DecisionExplanation>,
    pub continuation_actions: Vec<ContinuationAction>,
    pub attention: Vec<AttentionItem>,
    /// One entry per task that has attention items, so the task's board card
    /// and its header offer the same next step. Derived from `attention`.
    #[serde(default)]
    pub task_actions: Vec<TaskAction>,
}

/// The next step a task offers, taken from the first of its attention items in
/// precedence order. Opening it routes to that item's exact target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskAction {
    pub task_id: String,
    /// The attention item the action opens.
    pub item_id: String,
    pub action: AttentionAction,
    /// Every attention item that names this task, in precedence order.
    pub item_ids: Vec<String>,
}

impl AttentionTarget {
    /// The task this target belongs to, when it belongs to one.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::Task(target) => Some(&target.task_id),
            Self::Attempt { task_id, .. }
            | Self::Session { task_id, .. }
            | Self::PermissionRequest { task_id, .. }
            | Self::RecoveryRecord { task_id, .. }
            | Self::RoleSettings { task_id, .. } => Some(task_id),
            Self::ProjectSetup { .. } | Self::Diagnostics => None,
        }
    }
}

/// Groups attention items by task, keeping the item order as precedence.
pub fn task_actions(items: &[AttentionItem]) -> Vec<TaskAction> {
    let mut actions: Vec<TaskAction> = Vec::new();
    for item in items {
        let Some(task_id) = item.target.as_ref().and_then(AttentionTarget::task_id) else {
            continue;
        };
        match actions.iter_mut().find(|action| action.task_id == task_id) {
            Some(action) => action.item_ids.push(item.id.clone()),
            None => actions.push(TaskAction {
                task_id: task_id.to_owned(),
                item_id: item.id.clone(),
                action: item.action.clone(),
                item_ids: vec![item.id.clone()],
            }),
        }
    }
    actions
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TripHumanAction {
    InspectProject {
        project_id: String,
    },
    BeginSetup {
        project_id: String,
        expected_project_version: i64,
        host_manager: RoleOverride,
    },
    SaveSetupDraft {
        setup_operation_id: String,
        expected_project_version: i64,
        proposal: serde_json::Value,
    },
    ReviseSetupDraft {
        setup_operation_id: String,
        expected_project_version: i64,
        proposal: serde_json::Value,
    },
    StopSetupManager {
        setup_operation_id: String,
        expected_project_version: i64,
    },
    ChangeSetupManager {
        setup_operation_id: String,
        expected_project_version: i64,
        host_manager: RoleOverride,
    },
    AuthorizeSetupProbes {
        setup_operation_id: String,
        proposal_hash: String,
    },
    PrepareRuntimeAdmission {
        project_id: String,
        #[serde(default)]
        task_id: Option<String>,
        #[serde(default)]
        role: Option<RoleKind>,
        #[serde(default)]
        settings_revision: Option<i64>,
        #[serde(default)]
        cmux_socket_path: Option<String>,
        expected_version: i64,
    },
    AuthorizeRuntimeAdmission {
        admission_id: String,
        scope_hash: String,
    },
    PublishRuntimeProof {
        admission_id: String,
        role: RoleKind,
    },
    FinalizeInstallation {
        setup_operation_id: String,
        proposal_hash: String,
    },
    AuthorizeInstallation {
        setup_operation_id: String,
        proposal_hash: String,
        approved_preimages_hash: String,
        final_source_set_hash: String,
    },
    ApplyInstallation {
        setup_operation_id: String,
        proposal_hash: String,
    },
    RecoverInstallation {
        setup_operation_id: String,
    },
    AdoptInstallation {
        project_id: String,
        expected_project_version: i64,
        configuration: serde_json::Value,
    },
    MigrateAttempt {
        task_id: String,
        attempt_id: String,
        expected_task_version: i64,
        reviewed_plan_hash: String,
        config_revision_id: String,
    },
    AuthorizeCheck {
        attempt_id: String,
        check_id: String,
        selected_revision: i64,
        exact_command_hash: String,
        scope_hash: String,
        #[serde(default = "approved_check_decision")]
        decision: String,
        lifetime: String,
    },
    RevokeCheckPermissionRule {
        rule_id: String,
        expected_revision: i64,
    },
    ExtendReviewBudget {
        task_id: String,
        attempt_id: String,
        expected_task_version: i64,
        review_kind: String,
        additional: i64,
    },
    AuthorizeImplementation {
        task_id: String,
        attempt_id: String,
        expected_task_version: i64,
        plan_hash: String,
    },
    AuthorizeAdditionalExplorer {
        task_id: String,
        attempt_id: String,
        expected_task_version: i64,
        stage: String,
        justification: String,
    },
    /// Your one-shot recovery of a final-repair attempt held in `needs_input`
    /// by an ordinary `needs_rework` code review of its repaired candidate.
    /// Every identity must equal the binding the service derives from the
    /// ledger; the result returns all derived bindings.
    AuthorizeFinalRepairRecheck {
        task_id: String,
        attempt_id: String,
        expected_task_version: i64,
        approved_code_request_id: String,
        prior_candidate_hash: String,
        final_request_id: String,
        rejected_code_request_id: String,
        reviewer_generation_id: String,
    },
}

fn approved_check_decision() -> String {
    "approved".to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HumanCommand {
    UpsertProfileSet {
        operation_id: String,
        project_id: String,
        profile_set_id: Option<String>,
        expected_version: Option<i64>,
        name: String,
        roles: serde_json::Value,
    },
    ArchiveProfileSet {
        operation_id: String,
        project_id: String,
        profile_set_id: String,
        expected_version: i64,
    },
    UpsertTaskRecipe {
        operation_id: String,
        project_id: String,
        recipe_id: Option<String>,
        expected_version: Option<i64>,
        name: String,
        title: String,
        description: String,
        acceptance_criteria: Vec<String>,
        priority: i64,
        profile_revision_id: String,
        required_check_ids: Vec<String>,
    },
    ArchiveTaskRecipe {
        operation_id: String,
        project_id: String,
        recipe_id: String,
        expected_version: i64,
    },
    CreateDraftFromRecipe {
        operation_id: String,
        project_id: String,
        recipe_id: String,
        recipe_revision_id: String,
        expected_recipe_version: i64,
    },
    UpsertRecipeSchedule {
        operation_id: String,
        project_id: String,
        schedule_id: Option<String>,
        expected_version: Option<i64>,
        name: String,
        recipe_revision_id: String,
        cadence: String,
        anchor_utc: String,
    },
    PauseRecipeSchedule {
        operation_id: String,
        project_id: String,
        schedule_id: String,
        expected_version: i64,
    },
    ResumeRecipeSchedule {
        operation_id: String,
        project_id: String,
        schedule_id: String,
        expected_version: i64,
    },
    ArchiveRecipeSchedule {
        operation_id: String,
        project_id: String,
        schedule_id: String,
        expected_version: i64,
    },
    Trip {
        operation_id: String,
        #[serde(flatten)]
        action: TripHumanAction,
    },
    AddProject {
        operation_id: String,
        path: PathBuf,
        display_name: String,
    },
    RelinkProject {
        operation_id: String,
        project_id: String,
        path: PathBuf,
        expected_version: i64,
    },
    CreateTask {
        operation_id: String,
        project_id: String,
        title: String,
        description: String,
        acceptance_criteria: Vec<String>,
        priority: i64,
        ready: bool,
        #[serde(default)]
        role_overrides: serde_json::Value,
    },
    UpdateTask {
        operation_id: String,
        task_id: String,
        expected_version: i64,
        title: String,
        description: String,
        acceptance_criteria: Vec<String>,
        priority: i64,
        manual_order: i64,
        #[serde(default)]
        role_overrides: serde_json::Value,
    },
    MakeReady {
        operation_id: String,
        task_id: String,
        expected_version: i64,
    },
    SetQueuePaused {
        operation_id: String,
        project_id: String,
        expected_version: i64,
        paused: bool,
    },
    UpdateProjectSettings {
        operation_id: String,
        project_id: String,
        expected_version: i64,
        settings: serde_json::Value,
    },
    UpsertCheckSuite {
        operation_id: String,
        project_id: String,
        expected_version: i64,
        name: String,
        position: i64,
        executable: PathBuf,
        arguments: Vec<String>,
        timeout_seconds: u64,
        enabled: bool,
    },
    RemoveCheckSuite {
        operation_id: String,
        project_id: String,
        expected_version: i64,
        name: String,
    },
    AddDependency {
        operation_id: String,
        task_id: String,
        depends_on_task_id: String,
        expected_version: i64,
    },
    RecordIntegration {
        operation_id: String,
        task_id: String,
        depends_on_task_id: String,
        git_ref: String,
        expected_version: i64,
    },
    SetRoleSettings {
        operation_id: String,
        task_id: String,
        role: RoleKind,
        expected_version: i64,
        config: RoleOverride,
    },
    ActivateTaskProfile {
        operation_id: String,
        task_id: String,
        role: RoleKind,
        settings_revision: i64,
        expected_version: i64,
    },
    Control {
        operation_id: String,
        task_id: String,
        expected_version: i64,
        action: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
    ApprovePlan {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        expected_version: i64,
        plan_hash: String,
    },
    ReviewVerdict {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        expected_version: i64,
        review_kind: String,
        candidate_hash: String,
        verdict: String,
        feedback: String,
    },
    ExtendReviewBudget {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        expected_version: i64,
        review_kind: String,
        additional: i64,
    },
    HumanReview {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        expected_version: i64,
        decision: String,
        feedback: String,
        carry_plan_approval: bool,
    },
    Guidance {
        operation_id: String,
        task_id: String,
        role_generation_id: String,
        expected_version: i64,
        body: String,
    },
    ApplyTransition {
        operation_id: String,
        task_id: String,
        proposal_id: String,
        expected_version: i64,
    },
    ResolveRecovery {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        recovery_id: String,
        session_id: Option<String>,
        expected_version: i64,
        decision: String,
        evidence: String,
    },
    RetryWorkspaceReservation {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        workspace_id: String,
        expected_version: i64,
    },
    CancelWorkspaceReservation {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        workspace_id: String,
        expected_version: i64,
    },
    RetryGracefulStop {
        operation_id: String,
        task_id: String,
        session_id: String,
        role_generation_id: String,
        transcript_epoch: String,
        process_identity: serde_json::Value,
        expected_version: i64,
    },
    ForceStopExactProcess {
        operation_id: String,
        task_id: String,
        session_id: String,
        role_generation_id: String,
        transcript_epoch: String,
        process_identity: serde_json::Value,
        expected_version: i64,
    },
    NormalizeLegacyTask {
        operation_id: String,
        task_id: String,
        expected_version: i64,
    },
    /// Your explicit approval that listed guidance files in the active
    /// attempt's workspace may keep their approved new content. Every binding
    /// must still be current: task version, attempt, approved plan, project
    /// configuration revision, the exact workspace policy being amended, and
    /// each file's pinned and new hash.
    ReauthorizeAttemptGuidance {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        expected_version: i64,
        plan_hash: String,
        config_revision_id: String,
        policy_hash: String,
        files: Vec<GuidanceReauthorization>,
    },
    AbandonUnconfirmedGuidance {
        operation_id: String,
        #[serde(flatten)]
        delivery: UnconfirmedGuidanceAbandonment,
    },
    CancelStaleRestartCandidate {
        operation_id: String,
        #[serde(flatten)]
        candidate: StaleRestartCandidateCancellation,
    },
    /// Your explicit request for a fresh planning attempt after the attempt's
    /// terminal nonapproving code or final review. The rejected candidate is
    /// the new attempt's source only; it never becomes accepted authority.
    ReplanAfterTerminalReview {
        operation_id: String,
        task_id: String,
        attempt_id: String,
        expected_version: i64,
        review_request_id: String,
        role_result_id: String,
        candidate_hash: String,
        snapshot_id: String,
        reason: String,
    },
    Archive {
        operation_id: String,
        task_id: String,
        expected_version: i64,
    },
    Restore {
        operation_id: String,
        task_id: String,
        expected_version: i64,
    },
    SetAutoResume {
        operation_id: String,
        expected_version: i64,
        enabled: bool,
    },
    DecidePermission {
        operation_id: String,
        request_id: String,
        expected_revision: i64,
        decision: PermissionDecision,
        #[serde(default)]
        lifetime: Option<PermissionLifetime>,
        #[serde(default)]
        reason: String,
    },
    RevokePermissionRule {
        operation_id: String,
        rule_id: String,
        expected_revision: i64,
        #[serde(default)]
        reason: String,
    },
}

impl HumanCommand {
    pub fn operation_id(&self) -> &str {
        match self {
            Self::UpsertProfileSet { operation_id, .. }
            | Self::ArchiveProfileSet { operation_id, .. }
            | Self::UpsertTaskRecipe { operation_id, .. }
            | Self::ArchiveTaskRecipe { operation_id, .. }
            | Self::CreateDraftFromRecipe { operation_id, .. }
            | Self::UpsertRecipeSchedule { operation_id, .. }
            | Self::PauseRecipeSchedule { operation_id, .. }
            | Self::ResumeRecipeSchedule { operation_id, .. }
            | Self::ArchiveRecipeSchedule { operation_id, .. }
            | Self::Trip { operation_id, .. }
            | Self::AddProject { operation_id, .. }
            | Self::RelinkProject { operation_id, .. }
            | Self::CreateTask { operation_id, .. }
            | Self::UpdateTask { operation_id, .. }
            | Self::MakeReady { operation_id, .. }
            | Self::SetQueuePaused { operation_id, .. }
            | Self::AddDependency { operation_id, .. }
            | Self::UpdateProjectSettings { operation_id, .. }
            | Self::UpsertCheckSuite { operation_id, .. }
            | Self::RemoveCheckSuite { operation_id, .. }
            | Self::RecordIntegration { operation_id, .. }
            | Self::SetRoleSettings { operation_id, .. }
            | Self::ActivateTaskProfile { operation_id, .. }
            | Self::Control { operation_id, .. }
            | Self::ApprovePlan { operation_id, .. }
            | Self::ReviewVerdict { operation_id, .. }
            | Self::ExtendReviewBudget { operation_id, .. }
            | Self::HumanReview { operation_id, .. }
            | Self::Guidance { operation_id, .. }
            | Self::ApplyTransition { operation_id, .. }
            | Self::ResolveRecovery { operation_id, .. }
            | Self::RetryWorkspaceReservation { operation_id, .. }
            | Self::CancelWorkspaceReservation { operation_id, .. }
            | Self::RetryGracefulStop { operation_id, .. }
            | Self::ForceStopExactProcess { operation_id, .. }
            | Self::NormalizeLegacyTask { operation_id, .. }
            | Self::ReauthorizeAttemptGuidance { operation_id, .. }
            | Self::AbandonUnconfirmedGuidance { operation_id, .. }
            | Self::CancelStaleRestartCandidate { operation_id, .. }
            | Self::ReplanAfterTerminalReview { operation_id, .. }
            | Self::Archive { operation_id, .. }
            | Self::Restore { operation_id, .. }
            | Self::SetAutoResume { operation_id, .. }
            | Self::DecidePermission { operation_id, .. }
            | Self::RevokePermissionRule { operation_id, .. } => operation_id,
        }
    }
}

/// One guidance file whose pinned hash moves from `previous_sha256` to the
/// `sha256` of its approved content now in the attempt's workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuidanceReauthorization {
    pub path: String,
    pub previous_sha256: String,
    pub sha256: String,
}

/// Your explicit decision to stop waiting on one guidance delivery whose
/// outcome was never confirmed. Every recorded binding must be restated
/// exactly; the delivery session must have exited with its processes proven
/// absent. The outcome stays unknown: the message is never marked delivered
/// or replayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnconfirmedGuidanceAbandonment {
    pub task_id: String,
    pub expected_version: i64,
    pub attempt_id: String,
    pub guidance_id: String,
    pub role_generation_id: String,
    pub delivery_session_id: String,
    pub delivery_transcript_epoch: String,
    pub delivery_resume_invocation_id: Option<String>,
    pub expected_state: String,
    pub reason: String,
}

/// Your explicit decision to retire one skipped restart candidate that can
/// never be restored but still holds its attempt. Every recorded field must be
/// restated exactly, including the SHA-256 of its stored `result_json`; its
/// session must have exited with its processes proven absent. Nothing is
/// resumed, released or dispatched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleRestartCandidateCancellation {
    pub task_id: String,
    pub expected_version: i64,
    pub attempt_id: String,
    pub session_id: String,
    pub role_generation_id: String,
    pub expected_state: String,
    pub expected_source: String,
    pub expected_requested_by: Option<String>,
    pub expected_updated_at: String,
    pub expected_result_sha256: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationResult {
    pub operation_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub version: Option<i64>,
    pub state: String,
    #[serde(default)]
    pub detail: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapabilityProofInput {
    pub operation_id: String,
    pub session_id: String,
    pub evidence_reference: String,
    pub direct_write_denied: bool,
    pub compound_denied: bool,
    pub redirect_denied: bool,
    pub sentinel_relative_path: String,
    pub native_resume_session_id: String,
    pub history_nonce: String,
    #[serde(default)]
    pub workspace_write_observed: bool,
    #[serde(default)]
    pub workspace_probe_relative_path: Option<PathBuf>,
    #[serde(default)]
    pub workspace_probe_sha256: Option<String>,
    #[serde(default)]
    pub original_repo_write_denied: bool,
    #[serde(default)]
    pub service_data_write_denied: bool,
    #[serde(default)]
    pub human_control_denied: bool,
    #[serde(default)]
    pub denied_sentinel_paths: Vec<PathBuf>,
    #[serde(default)]
    pub runtime_scope: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RolePeerProvenance {
    pub peer_pid: u32,
    pub peer_process_group_id: i32,
    pub peer_start_marker: String,
    pub managed_root_pid: u32,
    pub managed_root_start_marker: String,
    pub state: String,
}

#[cfg(test)]
mod task_action_tests {
    use super::*;

    fn item(id: &str, task: Option<&str>, kind: AttentionActionKind) -> AttentionItem {
        AttentionItem {
            id: id.into(),
            category: AttentionCategory::Decision,
            title: id.into(),
            reason: String::new(),
            task_title: None,
            role: None,
            action: kind.into(),
            target: task.map(|task_id| {
                AttentionTarget::Task(TaskAttentionTarget {
                    project_id: "p".into(),
                    task_id: task_id.into(),
                    task_version: 1,
                })
            }),
            held_tasks: Vec::new(),
            details: None,
        }
    }

    #[test]
    fn each_task_offers_its_first_item_and_counts_the_rest() {
        let actions = task_actions(&[
            item("permission", Some("a"), AttentionActionKind::ReviewRequest),
            item("unbound", None, AttentionActionKind::ResolveIssue),
            item("question", Some("b"), AttentionActionKind::AnswerQuestion),
            item("blocked", Some("a"), AttentionActionKind::ResolveIssue),
        ]);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].task_id, "a");
        assert_eq!(actions[0].item_id, "permission");
        assert_eq!(actions[0].action.kind, AttentionActionKind::ReviewRequest);
        assert_eq!(actions[0].item_ids, ["permission", "blocked"]);
        assert_eq!(actions[1].task_id, "b");
        assert_eq!(actions[1].item_ids, ["question"]);
    }
}
