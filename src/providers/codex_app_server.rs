//! Exact local-stdio conversation contract. Native qualification is separate
//! from protocol inspection and from a project's activation decision.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const EXACT_VERSION: &str = "codex-cli 0.167.0";
pub const ADAPTER_REVISION: &str = "codex-local-stdio-conversation-v1";
pub const POLICY: &str = "conversation_read_only_v1";
pub const SCHEMA_HASH: &str = "021462a9bacd117f4cd40c256a4888619d059cec6a65b795198d521ba660e554";
pub const MAX_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_INPUT_BYTES: usize = 12 * 1024;
pub const MAX_OPERATION_BYTES: usize = 16 * 1024;
pub const MAX_RECEIPT_BYTES: usize = 128 * 1024;
pub const MAX_EVENT_BYTES: usize = 16 * 1024;
pub const MAX_EVENTS: i64 = 4096;
pub const MAX_PROJECTION_BYTES: i64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationProfileRevision {
    pub id: String,
    pub project_id: String,
    pub execution_config_revision_id: String,
    pub model: String,
    pub effort: String,
    pub instructions: String,
    pub instructions_hash: String,
    pub profile_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationNativeIdentity {
    pub executable: crate::domain::ExecutableFingerprint,
    pub executable_version: String,
    pub schema_hash: String,
    pub adapter_revision: String,
    pub adapter_hash: String,
    pub profile: ConversationProfileRevision,
    pub repository_identity: String,
    pub repository_root: PathBuf,
    pub guidance_hash: String,
    pub execution_config_hash: String,
    pub policy_hash: String,
    pub credential_contract: String,
    pub configuration_hash: String,
    pub release_binding: String,
    pub instance_id: String,
    pub restore_epoch: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileInput {
    pub operation_id: String,
    pub project_id: String,
    pub execution_config_revision_id: String,
    pub model: String,
    pub effort: String,
    pub instructions: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationWorkspaceIdentity {
    pub project_id: String,
    pub storage_scope: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationSummary {
    pub id: String,
    pub project_id: String,
    pub admission_id: String,
    pub state: String,
    pub generation: String,
    pub session_id: Option<String>,
    pub reservation_state: Option<String>,
    pub active_turn_id: Option<String>,
    pub binding_valid: bool,
    pub is_probe: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationAdmissionSummary {
    pub id: String,
    pub profile_revision_id: String,
    pub recorded_state: String,
    pub evidence_hash: Option<String>,
    pub probe_invocation_id: Option<String>,
    pub live_eligibility_checked: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationEventProjection {
    pub sequence: i64,
    pub kind: String,
    pub payload: Value,
    pub turn_id: Option<String>,
    pub operation_id: Option<String>,
    pub observed_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationRequestProjection {
    pub generation: String,
    pub thread_id: String,
    pub turn_id: String,
    pub request_id: String,
    pub method: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationDetail {
    pub conversation: ConversationSummary,
    pub profile: ConversationProfileRevision,
    pub admission: ConversationAdmissionSummary,
    pub unresolved_operation: bool,
    pub unresolved_operation_id: Option<String>,
    pub requests: Vec<ConversationRequestProjection>,
    pub events: Vec<ConversationEventProjection>,
    pub next_after: i64,
    pub has_more: bool,
    pub history_coverage: &'static str,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationMessageSource {
    Assistant {
        conversation_id: String,
        turn_id: Option<String>,
        item_id: Option<String>,
    },
    User {
        conversation_id: String,
        operation_id: String,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationMessageProjection {
    pub id: String,
    pub source: ConversationMessageSource,
    pub first_sequence: i64,
    pub last_sequence: i64,
    pub text: String,
    pub content_hash: String,
    pub selectable: bool,
    pub coverage: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationMessages {
    pub messages: Vec<ConversationMessageProjection>,
    pub through_sequence: i64,
    pub history_coverage: &'static str,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSourceSelection {
    pub message_id: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSourceInput {
    pub conversation_id: String,
    pub messages: Vec<ConversationSourceSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenConversationMessage {
    pub id: String,
    pub source: ConversationMessageSource,
    pub first_sequence: i64,
    pub last_sequence: i64,
    pub coverage: String,
    pub text: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationSourceAttribution {
    pub identity_hash: String,
    pub profile_revision_id: String,
    pub profile_hash: String,
    pub model: String,
    pub effort: String,
    pub executable_version: String,
    pub adapter_revision: String,
    pub adapter_hash: String,
    pub schema_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationSourceBundle {
    pub format_revision: u32,
    pub project_id: String,
    pub conversation_id: String,
    pub attribution: ConversationSourceAttribution,
    pub through_sequence: i64,
    pub history_coverage: String,
    pub messages: Vec<FrozenConversationMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenConversationSource {
    pub bundle: ConversationSourceBundle,
    pub bundle_hash: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskSources {
    pub project_id: String,
    pub task_id: String,
    pub conversation_source: Option<FrozenConversationSource>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversationReceiptLookup {
    pub operation_id: String,
    pub request_hash: String,
    pub found: bool,
    pub kind: Option<String>,
    pub delivery_state: Option<String>,
    pub conversation_id: Option<String>,
    pub generation: Option<String>,
    pub profile_revision_id: Option<String>,
    pub admission_id: Option<String>,
    pub state: Option<String>,
    pub quiescence: Option<String>,
    pub retained_uncertainty_session_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationOperation {
    WithKnowledge {
        operation_id: String,
        operation: Box<ConversationOperation>,
        project_id: String,
        expected_consumer_version: i64,
        selection: Vec<crate::domain::KnowledgeSelection>,
        preview_fingerprint: String,
    },
    WorkspaceIdentity {
        project_id: String,
    },
    TaskSources {
        project_id: String,
        task_id: String,
    },
    List {
        project_id: String,
        cursor: Option<String>,
    },
    Profiles {
        project_id: String,
        cursor: Option<String>,
    },
    Admissions {
        project_id: String,
        cursor: Option<String>,
    },
    Detail {
        project_id: String,
        conversation_id: String,
        after: i64,
    },
    Messages {
        project_id: String,
        conversation_id: String,
    },
    Receipt {
        project_id: String,
        operation_id: String,
        original_request: Value,
        request_hash: String,
    },
    CreateProfile {
        input: ProfileInput,
    },
    Prepare {
        operation_id: String,
        profile_revision_id: String,
    },
    Probe {
        operation_id: String,
        admission_id: String,
    },
    Activate {
        operation_id: String,
        admission_id: String,
        evidence_hash: String,
    },
    Start {
        operation_id: String,
        admission_id: String,
        input: String,
    },
    Submit {
        operation_id: String,
        conversation_id: String,
        input: String,
    },
    Steer {
        operation_id: String,
        conversation_id: String,
        expected_turn_id: String,
        input: String,
    },
    Interrupt {
        operation_id: String,
        conversation_id: String,
    },
    InterruptTurn {
        operation_id: String,
        conversation_id: String,
        expected_session_id: String,
        expected_generation: String,
        expected_turn_id: String,
    },
    DisconnectIdle {
        operation_id: String,
        conversation_id: String,
        expected_session_id: String,
        expected_generation: String,
    },
    Resume {
        operation_id: String,
        conversation_id: String,
    },
    Reconcile {
        operation_id: String,
        conversation_id: String,
    },
    Deny {
        operation_id: String,
        conversation_id: String,
        generation: String,
        thread_id: String,
        turn_id: String,
        request_id: String,
    },
    Inspect {
        conversation_id: String,
        after: i64,
    },
}

impl ConversationOperation {
    pub fn validate(&self) -> Result<()> {
        // This also caps identifiers and rejects oversized nested requests.
        let limit = if matches!(self, Self::Receipt { .. }) {
            MAX_RECEIPT_BYTES
        } else {
            MAX_OPERATION_BYTES
        };
        if serde_json::to_vec(self)?.len() > limit {
            bail!("Conversation request is too large; shorten the input.")
        }
        let value = serde_json::to_value(self)?;
        if let Some(fields) = value.as_object() {
            for (key, value) in fields {
                if key != "action" && key != "input" && key != "original_request" {
                    if let Some(text) = value.as_str() {
                        bounded(text, 128, key)?;
                    }
                }
            }
        }
        match self {
            Self::WithKnowledge {
                operation_id,
                operation,
                project_id,
                expected_consumer_version,
                selection,
                preview_fingerprint,
            } => {
                operation.validate()?;
                bounded(project_id, 128, "project ID")?;
                bounded(preview_fingerprint, 128, "preview fingerprint")?;
                let request = crate::knowledge::conversation_preview_request(
                    operation,
                    project_id,
                    *expected_consumer_version,
                    selection.clone(),
                )?;
                let value = serde_json::to_value(operation)?;
                if value["operation_id"].as_str() != Some(operation_id.as_str()) {
                    bail!("Keep the same operation identity when confirming knowledge")
                }
                crate::knowledge::validate_preview_request(&request)
            }
            Self::WorkspaceIdentity { project_id } => bounded(project_id, 128, "project ID"),
            Self::TaskSources {
                project_id,
                task_id,
            } => {
                bounded(project_id, 128, "project ID")?;
                bounded(task_id, 128, "task ID")
            }
            Self::List { project_id, cursor }
            | Self::Profiles { project_id, cursor }
            | Self::Admissions { project_id, cursor } => {
                bounded(project_id, 128, "project ID")?;
                if let Some(cursor) = cursor {
                    bounded(cursor, 128, "list cursor")?;
                }
                Ok(())
            }
            Self::Detail {
                project_id,
                conversation_id,
                after,
            } => {
                bounded(project_id, 128, "project ID")?;
                bounded(conversation_id, 128, "conversation ID")?;
                if !(0..=MAX_EVENTS).contains(after) {
                    bail!("Invalid conversation event cursor.")
                }
                Ok(())
            }
            Self::Messages {
                project_id,
                conversation_id,
            } => {
                bounded(project_id, 128, "project ID")?;
                bounded(conversation_id, 128, "conversation ID")
            }
            Self::Receipt {
                project_id,
                operation_id,
                request_hash,
                ..
            } => {
                bounded(project_id, 128, "project ID")?;
                bounded(operation_id, 128, "operation ID")?;
                if request_hash.len() != 64 || !request_hash.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    bail!("Invalid conversation request hash.")
                }
                Ok(())
            }
            Self::CreateProfile { input } => {
                bounded(&input.project_id, 128, "project ID")?;
                bounded(
                    &input.execution_config_revision_id,
                    128,
                    "execution configuration ID",
                )?;
                bounded(&input.model, 128, "model")?;
                bounded(&input.effort, 32, "effort")?;
                if input.instructions.len() > MAX_INPUT_BYTES || input.instructions.contains('\0') {
                    bail!("Invalid conversation instructions; use at most 12 KiB without NUL characters.")
                }
                bounded(&input.operation_id, 128, "operation ID")
            }
            Self::Inspect {
                conversation_id,
                after,
            } => {
                bounded(conversation_id, 128, "conversation ID")?;
                if *after < 0 {
                    bail!("Event cursor must not be negative.")
                }
                Ok(())
            }
            Self::Start {
                input,
                operation_id,
                ..
            }
            | Self::Submit {
                input,
                operation_id,
                ..
            }
            | Self::Steer {
                input,
                operation_id,
                ..
            } => {
                bounded(input, MAX_INPUT_BYTES, "message")?;
                bounded(operation_id, 128, "operation ID")
            }
            Self::InterruptTurn {
                operation_id,
                conversation_id,
                expected_session_id,
                expected_generation,
                expected_turn_id,
            } => {
                bounded(conversation_id, 128, "conversation ID")?;
                bounded(expected_session_id, 128, "session ID")?;
                bounded(expected_generation, 128, "generation")?;
                bounded(expected_turn_id, 128, "turn ID")?;
                bounded(operation_id, 128, "operation ID")
            }
            Self::DisconnectIdle {
                operation_id,
                conversation_id,
                expected_session_id,
                expected_generation,
            } => {
                bounded(conversation_id, 128, "conversation ID")?;
                bounded(expected_session_id, 128, "session ID")?;
                bounded(expected_generation, 128, "generation")?;
                bounded(operation_id, 128, "operation ID")
            }
            Self::Prepare { operation_id, .. }
            | Self::Probe { operation_id, .. }
            | Self::Activate { operation_id, .. }
            | Self::Interrupt { operation_id, .. }
            | Self::Resume { operation_id, .. }
            | Self::Reconcile { operation_id, .. }
            | Self::Deny { operation_id, .. } => bounded(operation_id, 128, "operation ID"),
        }
    }
}

pub fn bounded(value: &str, limit: usize, label: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > limit || value.contains('\0') {
        bail!("Invalid conversation {label}; provide 1–{limit} bytes without NUL characters.")
    }
    Ok(())
}

pub(crate) struct PreparedConversation {
    pub identity: ConversationNativeIdentity,
    pub arguments: Vec<String>,
    pub config: BTreeMap<String, Value>,
    pub guidance: String,
}

pub(crate) fn trusted_config(
    root: &Path,
    denied_reads: &[PathBuf],
    disabled_mcp: &[String],
) -> Result<BTreeMap<String, Value>> {
    let root = root
        .canonicalize()
        .context("resolve conversation repository")?;
    if denied_reads.is_empty() {
        bail!("Conversation protected-read floor is missing.")
    }
    let mut filesystem = serde_json::Map::new();
    filesystem.insert(":root".into(), json!("read"));
    filesystem.insert(root.to_string_lossy().into_owned(), json!("read"));
    for path in denied_reads {
        if !path.is_absolute() {
            bail!("Protected conversation paths must be absolute.")
        }
        filesystem.insert(
            path.to_str()
                .context("Protected conversation paths must be UTF-8")?
                .to_owned(),
            json!("deny"),
        );
    }
    let mut config = BTreeMap::from([
        ("default_permissions".into(), json!(POLICY)),
        (
            format!("permissions.{POLICY}"),
            json!({"filesystem":filesystem,"network":{"enabled":false}}),
        ),
        ("approval_policy".into(), json!("never")),
        ("cli_auth_credentials_store".into(), json!("file")),
        ("web_search".into(), json!("disabled")),
        ("model_provider".into(), json!("openai")),
        ("project_doc_max_bytes".into(), json!(0)),
        ("features.hooks".into(), json!(false)),
        ("features.multi_agent".into(), json!(false)),
        ("features.multi_agent_v2".into(), json!(false)),
        ("features.network_proxy".into(), json!(false)),
        ("features.skip_host_skill_discovery".into(), json!(true)),
        (
            "mcp_servers".into(),
            Value::Object(
                disabled_mcp
                    .iter()
                    .map(|name| (name.clone(), json!({"enabled":false})))
                    .collect(),
            ),
        ),
    ]);
    for feature in [
        "api_key_model_discovery",
        "apps",
        "apps_early_resource_discovery",
        "browser_use",
        "browser_use_external",
        "browser_use_full_cdp_access",
        "codex_apps_mcp_2026_07_28",
        "computer_use",
        "daemon_auto_start",
        "enable_mcp_apps",
        "external_agent_memory_import",
        "image_generation",
        "incremental_exec_tools",
        "in_app_browser",
        "in_app_local_automation",
        "interrupt_on_cancel",
        "memories",
        "plugins",
        "recommended_plugins",
        "remote_plugin",
        "realtime_conversation",
        "skill_mcp_dependency_install",
        "skill_search",
        "tool_call_mcp_elicitation",
        "use_xaa",
        "exec_permission_approvals",
        "request_permissions_tool",
    ] {
        config.insert(format!("features.{feature}"), json!(false));
    }
    Ok(config)
}

pub(crate) fn initialize(id: &str) -> Value {
    json!({"id":id,"method":"initialize","params":{"clientInfo":{"name":"llmrelay","version":crate::VERSION},"capabilities":{"experimentalApi":false}}})
}

pub(crate) fn thread_request(
    id: &str,
    prepared: &PreparedConversation,
    thread: Option<&str>,
) -> Value {
    let mut params = json!({"model":prepared.identity.profile.model,"modelProvider":"openai",
        "cwd":prepared.identity.repository_root,"approvalPolicy":"never",
        "config":prepared.config,"developerInstructions":format!("{}\n{}",prepared.identity.profile.instructions,prepared.guidance)});
    if let Some(thread) = thread {
        params["threadId"] = json!(thread);
    }
    json!({"id":id,"method":if thread.is_some(){"thread/resume"}else{"thread/start"},"params":params})
}

pub(crate) fn turn_request(
    operation: &str,
    thread: &str,
    input: &str,
    effort: &str,
    steer: Option<&str>,
) -> Value {
    let mut params = json!({"threadId":thread,"input":[{"type":"text","text":input}]});
    if let Some(turn) = steer {
        params["expectedTurnId"] = json!(turn);
    } else {
        params["clientUserMessageId"] = json!(operation);
        params["effort"] = json!(effort);
    }
    // Never supply sandboxPolicy/permissions here: doing so would replace the
    // named filesystem AND network policy installed on start/resume.
    json!({"id":operation,"method":if steer.is_some(){"turn/steer"}else{"turn/start"},"params":params})
}

#[derive(Debug)]
pub(crate) enum Projection {
    Response {
        id: String,
        result: Value,
        failed: bool,
    },
    Event {
        thread: String,
        turn: Option<String>,
        kind: &'static str,
        payload: Value,
    },
    Deny {
        id: Value,
        thread: String,
        turn: String,
        method: String,
        response: Value,
    },
    Ignore,
}

fn usage_counts(value: &Value) -> Value {
    json!({"input":value["inputTokens"].as_u64(),"cached_input":value["cachedInputTokens"].as_u64(),
        "output":value["outputTokens"].as_u64(),"reasoning_output":value["reasoningOutputTokens"].as_u64(),
        "total":value["totalTokens"].as_u64(),"unavailable_values":"null_means_missing_or_invalid"})
}

pub(crate) fn project(frame: &[u8]) -> Result<Projection> {
    if frame.len() > MAX_FRAME_BYTES {
        bail!("Native conversation frame exceeds the limit.")
    }
    let value: Value =
        serde_json::from_slice(frame).context("invalid native conversation frame")?;
    if value.get("method").is_none() {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .context("native response ID is missing")?;
        return Ok(Projection::Response {
            id: id.into(),
            result: value.get("result").cloned().unwrap_or(Value::Null),
            failed: value.get("error").is_some(),
        });
    }
    let method = value["method"].as_str().context("invalid native method")?;
    let p = &value["params"];
    let thread = p["threadId"].as_str().unwrap_or_default().to_owned();
    let turn = p["turnId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| p["turn"]["id"].as_str().map(str::to_owned));
    if let Some(id) = value.get("id") {
        // No provider request can acquire an allow from this policy. Unknown
        // request types receive a protocol error rather than a guessed response.
        let response = match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                json!({"id":id,"result":{"decision":"decline"}})
            }
            "item/permissions/requestApproval" => {
                json!({"id":id,"result":{"permissions":{},"scope":"turn"}})
            }
            _ => {
                json!({"id":id,"error":{"code":-32601,"message":"Request is unavailable under conversation_read_only_v1"}})
            }
        };
        return Ok(Projection::Deny {
            id: id.clone(),
            thread,
            turn: turn.unwrap_or_default(),
            method: method.into(),
            response,
        });
    }
    let (kind, payload) = match method {
        "item/agentMessage/delta" => (
            "agent_message",
            json!({"item_id":p["itemId"],"delta":p["delta"]}),
        ),
        "turn/started" => ("turn_started", json!({"status":p["turn"]["status"]})),
        "turn/completed" => {
            if !matches!(
                p["turn"]["status"].as_str(),
                Some("completed" | "interrupted" | "failed")
            ) {
                bail!("Native completion lacks a supported terminal outcome")
            }
            ("turn_completed", json!({"status":p["turn"]["status"]}))
        }
        "thread/tokenUsage/updated" => (
            "usage",
            json!({"last":usage_counts(&p["tokenUsage"]["last"]),"cumulative":usage_counts(&p["tokenUsage"]["total"]),"coverage":"partial_native_observation"}),
        ),
        "item/started" | "item/completed" => {
            let output = p["item"]["aggregatedOutput"].as_str().unwrap_or_default();
            let denied =
                output.contains("Operation not permitted") || output.contains("Permission denied");
            (
                "tool_activity",
                json!({"item_id":p["item"]["id"],"type":p["item"]["type"],"phase":method,
                "exit_code":p["item"]["exitCode"],"native_permission_error":denied,
                "output_hash":hex::encode(sha2::Sha256::digest(output.as_bytes())),
                "command_hash":p["item"]["command"].as_str().map(|command|hex::encode(sha2::Sha256::digest(command.as_bytes())))}),
            )
        }
        "error" => (
            "error",
            json!({"message":"The native conversation reported an error. Inspect the conversation before submitting again."}),
        ),
        _ => return Ok(Projection::Ignore),
    };
    if serde_json::to_vec(&payload)?.len() > MAX_EVENT_BYTES {
        bail!("Native conversation event exceeds the projection limit.")
    }
    Ok(Projection::Event {
        thread,
        turn,
        kind,
        payload,
    })
}
