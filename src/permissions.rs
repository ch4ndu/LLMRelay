use crate::domain::{
    OperationResult, PermissionDecision, PermissionLifetime, PermissionRequestDto,
    PermissionRuleDto, Provider, RoleContext, RoleKind,
};
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

pub const APPLICATION_DECISION_SECONDS: i64 = 24 * 60 * 60;
pub const PROVIDER_PERMISSION_TIMEOUT_SECONDS: u64 = APPLICATION_DECISION_SECONDS as u64 + 30;
pub const LOCAL_PERMISSION_TIMEOUT_SECONDS: u64 = PROVIDER_PERMISSION_TIMEOUT_SECONDS + 30;
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub enum BridgeStart {
    Immediate(serde_json::Value),
    Pending { request_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FamilyCandidate {
    executable_kind: String,
    executable_value: String,
    display_family: String,
    canonical_executable: String,
    registered_root: String,
    repository_identity: String,
    worktree_path: String,
    session_preview: serde_json::Value,
    project_preview: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ServiceCheckFamily {
    pub executable_kind: String,
    pub executable_value: String,
    pub display_family: String,
    pub registered_root: String,
    pub repository_identity: String,
    pub worktree_path: String,
    pub preview: serde_json::Value,
}

#[derive(Clone, Debug)]
struct RequestContext {
    native_session_id: String,
    workspace: PathBuf,
    registered_root: PathBuf,
    repository_identity: String,
    policy_fingerprint: String,
}

pub fn provider_response(allow: bool, message: Option<&str>) -> serde_json::Value {
    let mut decision = serde_json::json!({"behavior": if allow { "allow" } else { "deny" }});
    if let Some(message) = message.filter(|message| !message.trim().is_empty()) {
        decision["message"] = serde_json::Value::String(message.to_owned());
    }
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision
        }
    })
}

pub fn expire_prior_boot(store: &Store, service_boot_id: &str) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = {
        let mut statement = transaction.prepare(
            "SELECT id FROM permission_requests
             WHERE service_boot_id!=?1 AND delivery_state NOT IN ('delivered','unknown','expired','not_delivered')",
        )?;
        let rows = statement
            .query_map(params![service_boot_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut changed = 0;
    for id in ids {
        changed += expire_request(
            &transaction,
            &id,
            "service restarted before local response delivery",
            &now,
            Some("expired"),
        )?;
    }
    transaction.commit()?;
    Ok(changed)
}

pub fn validate_request_boot(store: &Store, request_id: &str, service_boot_id: &str) -> Result<()> {
    let connection = store.lock()?;
    let current: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM permission_requests WHERE id=?1 AND service_boot_id=?2)",
        params![request_id, service_boot_id],
        |row| row.get(0),
    )?;
    if !current {
        bail!("permission request belongs to a prior service boot or is unknown")
    }
    Ok(())
}

pub fn expire_deadlines(store: &Store) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = {
        let mut statement = transaction.prepare(
            "SELECT id FROM permission_requests
             WHERE deadline_at<=?1 AND delivery_state NOT IN ('delivered','unknown','expired','not_delivered','deny_required')",
        )?;
        let rows = statement
            .query_map(params![now], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut changed = 0;
    for id in ids {
        changed += expire_request(
            &transaction,
            &id,
            "permission request deadline elapsed",
            &now,
            None,
        )?;
    }
    transaction.commit()?;
    Ok(changed)
}

pub fn expire_session(store: &Store, session_id: &str, reason: &str) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = permission_ids(
        &transaction,
        "SELECT id FROM permission_requests WHERE session_id=?1 AND delivery_state NOT IN ('delivered','unknown','expired','not_delivered','deny_required')",
        session_id,
    )?;
    let mut changed = 0;
    for id in ids {
        changed += expire_request(&transaction, &id, reason, &now, None)?;
    }
    transaction.commit()?;
    Ok(changed)
}

pub fn expire_connection(store: &Store, connection_nonce: &str, reason: &str) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let ids = permission_ids(
        &transaction,
        "SELECT id FROM permission_requests WHERE connection_nonce=?1 AND delivery_state NOT IN ('delivered','unknown','expired','not_delivered')",
        connection_nonce,
    )?;
    let mut changed = 0;
    for id in ids {
        changed += expire_request(&transaction, &id, reason, &now, Some("not_delivered"))?;
    }
    transaction.commit()?;
    Ok(changed)
}

pub fn expire_generation(
    transaction: &Transaction<'_>,
    generation_id: &str,
    reason: &str,
) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let ids = permission_ids(
        transaction,
        "SELECT id FROM permission_requests WHERE role_generation_id=?1 AND delivery_state NOT IN ('delivered','unknown','expired','not_delivered','deny_required')",
        generation_id,
    )?;
    let mut changed = 0;
    for id in ids {
        changed += expire_request(transaction, &id, reason, &now, None)?;
    }
    Ok(changed)
}

pub fn expire_session_in_transaction(
    transaction: &Transaction<'_>,
    session_id: &str,
    reason: &str,
) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let ids = permission_ids(
        transaction,
        "SELECT id FROM permission_requests WHERE session_id=?1 AND delivery_state NOT IN ('delivered','unknown','expired','not_delivered','deny_required')",
        session_id,
    )?;
    let mut changed = 0;
    for id in ids {
        changed += expire_request(transaction, &id, reason, &now, None)?;
    }
    Ok(changed)
}

fn permission_ids(connection: &Connection, sql: &str, value: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map(params![value], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn expire_request(
    connection: &Connection,
    request_id: &str,
    reason: &str,
    now: &str,
    terminal_delivery: Option<&str>,
) -> Result<usize> {
    let delivery: Option<String> = connection
        .query_row(
            "SELECT delivery_state FROM permission_requests WHERE id=?1",
            params![request_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(delivery) = delivery else {
        return Ok(0);
    };
    let (next_delivery, event_code) = if delivery == "reserved" {
        ("unknown", "permission.response.delivery_unknown")
    } else if delivery == "not_reserved"
        || (delivery == "deny_required" && terminal_delivery.is_some())
    {
        match terminal_delivery {
            Some("not_delivered") => ("not_delivered", "permission.response.not_delivered"),
            Some(_) => ("expired", "permission.response.expired"),
            None => ("deny_required", "permission.response.deny_required"),
        }
    } else {
        return Ok(0);
    };
    let changed = connection.execute(
        "UPDATE permission_requests SET
           state=CASE WHEN state='pending' THEN 'expired' ELSE state END,
           decision_kind=CASE WHEN state='pending' THEN COALESCE(decision_kind,'expired') ELSE decision_kind END,
           decision_actor=CASE WHEN state='pending' THEN COALESCE(decision_actor,'service_policy') ELSE decision_actor END,
           decision_reason=CASE WHEN state='pending' THEN COALESCE(decision_reason,?1) ELSE decision_reason END,
           decided_at=CASE WHEN state='pending' THEN COALESCE(decided_at,?2) ELSE decided_at END,
           delivery_state=?3,delivery_unknown_at=CASE WHEN ?3='unknown' THEN ?2 ELSE delivery_unknown_at END,
           delivery_reason=?1,revision=revision+1,updated_at=?2
         WHERE id=?4 AND delivery_state=?5",
        params![reason, now, next_delivery, request_id, delivery],
    )?;
    if changed == 1 {
        permission_lifecycle_event(
            connection,
            request_id,
            event_code,
            serde_json::json!({
                "delivery_state":next_delivery,
                "reason":reason,
                "local_transport_only":true
            }),
            now,
        )?;
    }
    Ok(changed)
}

fn permission_lifecycle_event(
    connection: &Connection,
    request_id: &str,
    event_code: &str,
    detail: serde_json::Value,
    now: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service',?3,'permission_request',?4,?5,?6)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            event_code,
            request_id,
            detail.to_string(),
            now
        ],
    )?;
    Ok(())
}

pub fn begin_request(
    store: &Store,
    context: &RoleContext,
    payload: &serde_json::Value,
    service_boot_id: &str,
    connection_nonce: &str,
) -> Result<BridgeStart> {
    store.require_execution_unheld("permission requests")?;
    let source_tool_name = payload
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim);
    let tool_name_complete =
        source_tool_name.is_some_and(|value| !value.is_empty() && value.len() <= 256);
    let tool_name = source_tool_name
        .unwrap_or("unknown")
        .chars()
        .take(256)
        .collect::<String>();
    let raw_input = payload
        .get("tool_input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let raw_bytes = serde_json::to_vec(&raw_input)?;
    let input_digest = hex::encode(Sha256::digest(&raw_bytes));
    let bounded_input = if raw_bytes.len() <= MAX_INPUT_BYTES {
        raw_input.clone()
    } else {
        serde_json::json!({"omitted":true,"reason":"provider input exceeded 64 KiB","sha256":input_digest})
    };
    let requested_access = payload
        .get("requested_access")
        .or_else(|| payload.get("permission_suggestions"))
        .or_else(|| raw_input.get("sandbox_permissions"))
        .cloned()
        .map(|value| {
            if serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= 8192) {
                value
            } else {
                serde_json::json!({
                    "omitted":true,
                    "reason":"provider requested-access field exceeded 8 KiB"
                })
            }
        });
    let reason = payload
        .get("reason")
        .or_else(|| raw_input.get("reason"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() <= 8192)
        .map(str::to_owned);
    let (request_context, context_error) = if context.role == RoleKind::Implementer {
        match request_context(store, context, payload) {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        }
    } else {
        (None, None)
    };
    let review_error = if !tool_name_complete {
        Some("Provider tool name was missing, empty, or too large to represent exactly".to_owned())
    } else if raw_input.is_null() {
        Some(
            "Provider tool input was missing or null, so the requested action is unknown"
                .to_owned(),
        )
    } else if raw_bytes.len() > MAX_INPUT_BYTES {
        Some(
            "Provider input exceeded 64 KiB and could not be fully represented for human review"
                .to_owned(),
        )
    } else if let Err(error) = validate_exact_action(context.provider, &tool_name, &raw_input) {
        Some(format!("{error:#}"))
    } else if let Some(request_context) = request_context.as_ref() {
        validate_call_cwd(&raw_input, &request_context.workspace)
            .err()
            .map(|error| format!("{error:#}"))
    } else {
        None
    };
    let (family, family_error, mut command_display) =
        match (request_context.as_ref(), review_error.as_ref()) {
            (Some(request_context), None) => {
                match classify_family(context.provider, &tool_name, &raw_input, request_context) {
                    Ok((candidate, display)) => (Some(candidate), None, display),
                    Err(error) => (
                        None,
                        Some(format!("{error:#}")),
                        command_text(context.provider, &tool_name, &raw_input),
                    ),
                }
            }
            (_, Some(error)) => (
                None,
                Some(format!("No approval is available because {error}")),
                None,
            ),
            (None, None) => (
                None,
                Some(format!(
                "Always approve is unavailable because {}",
                context_error.as_deref().unwrap_or(
                    "native session, policy, or managed-worktree identity could not be established"
                )
            )),
                command_text(context.provider, &tool_name, &raw_input),
            ),
        };
    if command_display
        .as_ref()
        .is_some_and(|value| value.len() > MAX_INPUT_BYTES)
    {
        command_display = None;
    }
    let now = Utc::now();
    let deadline = now + Duration::seconds(APPLICATION_DECISION_SECONDS);
    let request_id = uuid::Uuid::new_v4().to_string();
    let invocation_nonce = payload
        .get("hook_invocation_nonce")
        .or_else(|| payload.get("hook_id"))
        .or_else(|| payload.get("invocation_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.len() <= 256)
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let actionable = context.role == RoleKind::Implementer
        && request_context.is_some()
        && review_error.is_none();
    let initial_state = if actionable {
        "pending"
    } else {
        "policy_denied"
    };
    let initial_reason = if context.role != RoleKind::Implementer {
        Some("Only the Implementer role may receive a human-granted action permission".to_owned())
    } else if let Some(error) = context_error {
        Some(format!(
            "{error}; no grant was authorized. Use the faithful native terminal prompt"
        ))
    } else if let Some(error) = review_error {
        Some(format!(
            "{error}; no grant was authorized. Use the faithful native terminal prompt"
        ))
    } else {
        None
    };
    let native_session_id = request_context
        .as_ref()
        .map(|value| value.native_session_id.clone())
        .or_else(|| {
            payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let workspace = request_context
        .as_ref()
        .map(|value| value.workspace.to_string_lossy().into_owned())
        .unwrap_or_default();
    let policy = request_context
        .as_ref()
        .map(|value| value.policy_fingerprint.clone())
        .unwrap_or_default();
    let family_json = family.as_ref().map(serde_json::to_string).transpose()?;
    let mut connection = store.lock()?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO permission_requests(id,hook_invocation_nonce,connection_nonce,provider,project_id,task_id,attempt_id,session_id,role_generation_id,role,service_boot_id,native_session_id,cwd,policy_fingerprint,tool_name,input_digest,input_json,requested_access_json,reason,command_display,family_json,family_unavailable_reason,created_at,deadline_at,state,decision_reason,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?23)",
        params![
            request_id,
            invocation_nonce,
            connection_nonce,
            context.provider.to_string(),
            context.project_id,
            context.task_id,
            context.attempt_id,
            context.session_id,
            context.role_generation_id,
            context.role.to_string(),
            service_boot_id,
            native_session_id,
            workspace,
            policy,
            tool_name,
            input_digest,
            bounded_input.to_string(),
            requested_access.as_ref().map(serde_json::Value::to_string),
            reason,
            command_display,
            family_json,
            family_error,
            now.to_rfc3339(),
            deadline.to_rfc3339(),
            initial_state,
            initial_reason
        ],
    )?;
    if !actionable {
        transaction.execute(
            "UPDATE permission_requests SET decision_kind='policy_denied',decision_actor='service_policy',
                    decided_at=?1,delivery_state='reserved',delivery_reserved_at=?1,reserved_behavior='deny',
                    consumed_at=?1 WHERE id=?2",
            params![now.to_rfc3339(), request_id],
        )?;
        permission_lifecycle_event(
            &transaction,
            &request_id,
            "permission.response.reserved",
            serde_json::json!({
                "behavior":"deny",
                "decision_kind":"policy_denied",
                "local_transport_only":true
            }),
            &now.to_rfc3339(),
        )?;
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,actor_id,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'hook',?3,?4,'permission_request',?5,?6,?7)",
        params![
            uuid::Uuid::new_v4().to_string(),
            invocation_nonce,
            context.role_generation_id,
            if actionable {
                "permission.requested"
            } else {
                "permission.policy_denied"
            },
            request_id,
            serde_json::json!({"provider":context.provider,"role":context.role,"tool_name":tool_name,"input_digest":input_digest}).to_string(),
            now.to_rfc3339()
        ],
    )?;
    if actionable {
        if let (Some(candidate), Some(request_context)) =
            (family.as_ref(), request_context.as_ref())
        {
            if let Some(rule_id) = matching_rule(&transaction, context, request_context, candidate)?
            {
                transaction.execute(
                    "UPDATE permission_requests SET state='approved_rule',decision_kind='matching_rule',matching_rule_id=?1,decision_actor='human_rule',decision_reason='matched active human-created command-family rule',decided_at=?2,revision=revision+1,updated_at=?2 WHERE id=?3 AND state='pending'",
                    params![rule_id,now.to_rfc3339(),request_id],
                )?;
                transaction.execute(
                    "INSERT INTO audit_events(id,operation_id,actor_kind,actor_id,event_code,entity_kind,entity_id,detail_json,created_at)
                     VALUES(?1,?2,'service',?3,'permission.rule.candidate_matched','permission_rule',?4,?5,?6)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        invocation_nonce,
                        context.role_generation_id,
                        rule_id,
                        serde_json::json!({"permission_request_id":request_id,"input_digest":input_digest,"authorization_reserved":false}).to_string(),
                        now.to_rfc3339()
                    ],
                )?;
            }
        }
    }
    transaction.commit()?;
    if !actionable {
        Ok(BridgeStart::Immediate(provider_response(
            false,
            initial_reason.as_deref(),
        )))
    } else {
        Ok(BridgeStart::Pending { request_id })
    }
}

pub fn consume_ready_response(
    store: &Store,
    request_id: &str,
    service_boot_id: &str,
    connection_nonce: &str,
    credential: &str,
    credential_context: &RoleContext,
) -> Result<Option<serde_json::Value>> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let credential_permissions: Option<String> = transaction
        .query_row(
            "SELECT permissions_json FROM role_credentials
             WHERE token_hash=?1 AND role_generation_id=?2 AND revoked_at IS NULL",
            params![
                crate::auth::hash_secret(credential),
                credential_context.role_generation_id
            ],
            |row| row.get(0),
        )
        .optional()?;
    let credential_current = credential_permissions
        .as_deref()
        .and_then(|permissions| serde_json::from_str::<Vec<String>>(permissions).ok())
        .is_some_and(|permissions| permissions == credential_context.permissions);
    struct ReadyRequest {
        state: String,
        deadline: String,
        boot: String,
        connection: String,
        provider: String,
        project: String,
        task: String,
        attempt: String,
        session: String,
        generation: String,
        role: String,
        native: String,
        cwd: String,
        policy: String,
        matching_rule: Option<String>,
        family_json: Option<String>,
        session_state: String,
        current_native: String,
        current_policy: String,
        delivery_state: String,
        session_provider: String,
        generation_state: String,
        generation_role: String,
        generation_provider: String,
        workspace: String,
        workspace_state: String,
        workspace_identity: String,
        registered_root: String,
        project_identity: String,
        configuration_revision: i64,
    }
    let row: Option<ReadyRequest> = transaction
        .query_row(
            "SELECT pr.state,pr.deadline_at,pr.service_boot_id,pr.connection_nonce,pr.provider,pr.project_id,pr.task_id,
                    pr.attempt_id,pr.session_id,pr.role_generation_id,pr.role,pr.native_session_id,pr.cwd,pr.policy_fingerprint,
                    pr.matching_rule_id,pr.family_json,s.status,COALESCE(s.native_session_id,''),COALESCE(s.capability_key,''),
                    pr.delivery_state,s.provider,rg.status,rg.role,rg.provider,w.path,w.state,w.repository_identity,p.repository_path,
                    p.repository_identity,a.configuration_revision
             FROM permission_requests pr JOIN sessions s ON s.id=pr.session_id
             JOIN role_generations rg ON rg.id=pr.role_generation_id
             JOIN attempts a ON a.id=pr.attempt_id JOIN tasks t ON t.id=pr.task_id
             JOIN projects p ON p.id=pr.project_id JOIN workspaces w ON w.attempt_id=a.id
             WHERE pr.id=?1 AND rg.id=s.role_generation_id AND rg.attempt_id=a.id
               AND a.task_id=t.id AND t.project_id=p.id",
            params![request_id],
            |row| {
                Ok(ReadyRequest {
                    state: row.get(0)?,
                    deadline: row.get(1)?,
                    boot: row.get(2)?,
                    connection: row.get(3)?,
                    provider: row.get(4)?,
                    project: row.get(5)?,
                    task: row.get(6)?,
                    attempt: row.get(7)?,
                    session: row.get(8)?,
                    generation: row.get(9)?,
                    role: row.get(10)?,
                    native: row.get(11)?,
                    cwd: row.get(12)?,
                    policy: row.get(13)?,
                    matching_rule: row.get(14)?,
                    family_json: row.get(15)?,
                    session_state: row.get(16)?,
                    current_native: row.get(17)?,
                    current_policy: row.get(18)?,
                    delivery_state: row.get(19)?,
                    session_provider: row.get(20)?,
                    generation_state: row.get(21)?,
                    generation_role: row.get(22)?,
                    generation_provider: row.get(23)?,
                    workspace: row.get(24)?,
                    workspace_state: row.get(25)?,
                    workspace_identity: row.get(26)?,
                    registered_root: row.get(27)?,
                    project_identity: row.get(28)?,
                    configuration_revision: row.get(29)?,
                })
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(Some(provider_response(
            false,
            Some("Permission request authority is no longer current"),
        )));
    };
    if !matches!(
        row.delivery_state.as_str(),
        "not_reserved" | "deny_required"
    ) {
        transaction.commit()?;
        return Ok(Some(provider_response(
            false,
            Some("Permission response was already reserved, delivered, or invalidated"),
        )));
    }
    let mut state = row.state.clone();
    let mut invalid = row.delivery_state == "deny_required"
        || !credential_current
        || row.boot != service_boot_id
        || row.connection != connection_nonce
        || row.role != "implementer"
        || row.generation_role != "implementer"
        || row.provider != row.session_provider
        || row.provider != row.generation_provider
        || row.native.is_empty()
        || row.native != row.current_native
        || row.policy.is_empty()
        || row.policy != row.current_policy
        || row.session_state != "running"
        || matches!(row.generation_state.as_str(), "replaced" | "revoked")
        || row.deadline <= now
        || row.project != credential_context.project_id
        || row.task != credential_context.task_id
        || row.attempt != credential_context.attempt_id
        || row.session != credential_context.session_id
        || row.generation != credential_context.role_generation_id
        || row.provider != credential_context.provider.to_string()
        || credential_context.role != RoleKind::Implementer
        || row.configuration_revision != credential_context.configuration_revision;
    if invalid && state == "pending" {
        state = "expired".to_owned();
        transaction.execute(
            "UPDATE permission_requests SET state='expired',decision_kind='expired',decision_actor='service_policy',
                    decision_reason='request delivery fence changed or deadline elapsed',decided_at=?1,
                    revision=revision+1,updated_at=?1
             WHERE id=?2 AND state='pending' AND delivery_state IN ('not_reserved','deny_required')",
            params![now, request_id],
        )?;
    }
    if state == "pending" {
        transaction.commit()?;
        return Ok(None);
    }
    if matches!(state.as_str(), "approved_once" | "approved_rule") && !invalid {
        let canonical_workspace = canonical_text(Path::new(&row.workspace)).ok();
        let canonical_cwd = canonical_text(Path::new(&row.cwd)).ok();
        let canonical_root = canonical_text(Path::new(&row.registered_root)).ok();
        let inspected_identity = canonical_workspace
            .as_deref()
            .and_then(|workspace| crate::workspace::inspect(Path::new(workspace)).ok())
            .map(|repository| repository.identity);
        let current_request_context = match (canonical_workspace.as_ref(), canonical_root.as_ref())
        {
            (Some(workspace), Some(root))
                if canonical_cwd.as_ref() == Some(workspace)
                    && row.workspace_state == "ready"
                    && row.workspace_identity == row.project_identity
                    && inspected_identity.as_ref() == Some(&row.project_identity) =>
            {
                Some(RequestContext {
                    native_session_id: row.native.clone(),
                    workspace: PathBuf::from(workspace),
                    registered_root: PathBuf::from(root),
                    repository_identity: row.project_identity.clone(),
                    policy_fingerprint: row.policy.clone(),
                })
            }
            _ => None,
        };
        invalid = current_request_context.is_none();
        if state == "approved_rule" && !invalid {
            invalid = match (
                row.matching_rule.as_deref(),
                row.family_json.as_deref(),
                current_request_context.as_ref(),
            ) {
                (Some(rule_id), Some(family_json), Some(request_context)) => {
                    match serde_json::from_str::<FamilyCandidate>(family_json) {
                        Ok(family) => !matching_rule_current(
                            &transaction,
                            rule_id,
                            credential_context,
                            request_context,
                            &family,
                        )?,
                        Err(_) => true,
                    }
                }
                _ => true,
            };
        }
        if invalid {
            transaction.execute(
                "UPDATE permission_requests SET delivery_reason='request delivery fence changed, the saved rule was revoked, or managed-worktree identity changed',updated_at=?1
                 WHERE id=?2 AND delivery_state IN ('not_reserved','deny_required')",
                params![now, request_id],
            )?;
        }
    }
    let allow = matches!(state.as_str(), "approved_once" | "approved_rule") && !invalid;
    let behavior = if allow { "allow" } else { "deny" };
    if allow && state == "approved_rule" {
        let rule_id = row.matching_rule.as_deref().ok_or_else(|| {
            anyhow!("approved reusable-rule response has no matching rule identity")
        })?;
        if transaction.execute(
            "UPDATE permission_rules SET use_count=use_count+1,last_used_at=?1,revision=revision+1
             WHERE id=?2 AND revoked_at IS NULL",
            params![now, rule_id],
        )? != 1
        {
            bail!("matching permission rule changed before response reservation")
        }
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','permission.rule.authorization_reserved','permission_rule',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                rule_id,
                serde_json::json!({
                    "permission_request_id":request_id,
                    "authorization_reserved":true,
                    "local_delivery_pending":true
                }).to_string(),
                now
            ],
        )?;
    }
    if transaction.execute(
        "UPDATE permission_requests SET delivery_state='reserved',delivery_reserved_at=?1,
                reserved_behavior=?2,consumed_at=?1,delivery_reason=CASE WHEN ?2='deny' THEN
                  COALESCE(delivery_reason,'permission was denied, expired, or invalidated') ELSE delivery_reason END,
                revision=revision+1,updated_at=?1
         WHERE id=?3 AND delivery_state IN ('not_reserved','deny_required')",
        params![now, behavior, request_id],
    )? != 1
    {
        bail!("permission response was already reserved")
    }
    permission_lifecycle_event(
        &transaction,
        request_id,
        "permission.response.reserved",
        serde_json::json!({
            "behavior":behavior,
            "one_shot":true,
            "local_delivery_pending":true,
            "native_command_execution_proven":false
        }),
        &now,
    )?;
    transaction.commit()?;
    Ok(Some(provider_response(
        allow,
        (!allow)
            .then_some("Permission was denied, expired, or invalidated; no action was authorized"),
    )))
}

pub fn reserve_connection_denial(
    store: &Store,
    connection_nonce: &str,
    reason: &str,
) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let request: Option<String> = transaction
        .query_row(
            "SELECT id FROM permission_requests
             WHERE connection_nonce=?1 AND delivery_state IN ('not_reserved','deny_required')
             ORDER BY created_at DESC LIMIT 1",
            params![connection_nonce],
            |row| row.get(0),
        )
        .optional()?;
    let Some(request_id) = request else {
        transaction.commit()?;
        return Ok(0);
    };
    let changed = transaction.execute(
        "UPDATE permission_requests SET
           state=CASE WHEN state='pending' THEN 'expired' ELSE state END,
           decision_kind=CASE WHEN state='pending' THEN 'credential_invalidated' ELSE decision_kind END,
           decision_actor=CASE WHEN state='pending' THEN 'service_policy' ELSE decision_actor END,
           decision_reason=CASE WHEN state='pending' THEN ?1 ELSE decision_reason END,
           decided_at=CASE WHEN state='pending' THEN ?2 ELSE decided_at END,
           delivery_state='reserved',delivery_reserved_at=?2,reserved_behavior='deny',
           delivery_reason=?1,consumed_at=?2,revision=revision+1,updated_at=?2
         WHERE id=?3 AND delivery_state IN ('not_reserved','deny_required')",
        params![reason, now, request_id],
    )?;
    if changed == 1 {
        permission_lifecycle_event(
            &transaction,
            &request_id,
            "permission.response.reserved",
            serde_json::json!({
                "behavior":"deny",
                "reason":reason,
                "one_shot":true,
                "local_delivery_pending":true,
                "native_command_execution_proven":false
            }),
            &now,
        )?;
    }
    transaction.commit()?;
    Ok(changed)
}

pub fn mark_response_delivered(store: &Store, connection_nonce: &str) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let request: Option<(String, String)> = transaction
        .query_row(
            "SELECT id,COALESCE(reserved_behavior,'deny') FROM permission_requests
             WHERE connection_nonce=?1 AND delivery_state='reserved'
             ORDER BY created_at DESC LIMIT 1",
            params![connection_nonce],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((request_id, behavior)) = request else {
        transaction.commit()?;
        return Ok(0);
    };
    let changed = transaction.execute(
        "UPDATE permission_requests SET delivery_state='delivered',delivered_at=?1,
                delivery_reason='response bytes written and flushed to the authenticated local hook connection; native command execution is not proven',
                revision=revision+1,updated_at=?1 WHERE id=?2 AND delivery_state='reserved'",
        params![now, request_id],
    )?;
    if changed == 1 {
        permission_lifecycle_event(
            &transaction,
            &request_id,
            "permission.response.local_transport_delivered",
            serde_json::json!({
                "behavior":behavior,
                "socket_write_flushed":true,
                "native_command_execution_proven":false
            }),
            &now,
        )?;
    }
    transaction.commit()?;
    Ok(changed)
}

pub fn mark_response_delivery_unknown(
    store: &Store,
    connection_nonce: &str,
    reason: &str,
) -> Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let request: Option<String> = transaction
        .query_row(
            "SELECT id FROM permission_requests
             WHERE connection_nonce=?1 AND delivery_state='reserved'
             ORDER BY created_at DESC LIMIT 1",
            params![connection_nonce],
            |row| row.get(0),
        )
        .optional()?;
    let Some(request_id) = request else {
        transaction.commit()?;
        return Ok(0);
    };
    let changed = transaction.execute(
        "UPDATE permission_requests SET delivery_state='unknown',delivery_unknown_at=?1,
                delivery_reason=?2,revision=revision+1,updated_at=?1
         WHERE id=?3 AND delivery_state='reserved'",
        params![now, reason, request_id],
    )?;
    if changed == 1 {
        permission_lifecycle_event(
            &transaction,
            &request_id,
            "permission.response.delivery_unknown",
            serde_json::json!({
                "reason":reason,
                "replay_allowed":false,
                "native_command_execution_proven":false
            }),
            &now,
        )?;
    }
    transaction.commit()?;
    Ok(changed)
}

fn matching_rule_current(
    transaction: &Transaction<'_>,
    rule_id: &str,
    context: &RoleContext,
    request: &RequestContext,
    family: &FamilyCandidate,
) -> Result<bool> {
    let rule: Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
        String,
        String,
    )> = transaction
        .query_row(
            "SELECT lifetime,session_id,role_generation_id,native_session_id,registered_root,repository_identity,
                    worktree_path,executable_kind,executable_value
             FROM permission_rules
             WHERE id=?1 AND project_id=?2 AND provider=?3 AND role=?4 AND policy_fingerprint=?5
               AND revoked_at IS NULL",
            params![
                rule_id,
                context.project_id,
                context.provider.to_string(),
                context.role.to_string(),
                request.policy_fingerprint
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .optional()?;
    let Some((lifetime, session, generation, native, root, identity, worktree, kind, executable)) =
        rule
    else {
        return Ok(false);
    };
    if canonical_text(Path::new(&root)).ok().as_deref()
        != Some(request.registered_root.to_string_lossy().as_ref())
        || root != family.registered_root
        || identity != request.repository_identity
        || identity != family.repository_identity
        || kind != family.executable_kind
        || executable != family.executable_value
    {
        return Ok(false);
    }
    let scope_matches = if lifetime == "session" {
        session.as_deref() == Some(context.session_id.as_str())
            && generation.as_deref() == Some(context.role_generation_id.as_str())
            && native.as_deref() == Some(request.native_session_id.as_str())
            && worktree
                .as_deref()
                .and_then(|path| canonical_text(Path::new(path)).ok())
                .as_deref()
                == Some(request.workspace.to_string_lossy().as_ref())
    } else {
        lifetime == "project"
    };
    Ok(scope_matches
        && project_scope_current(
            transaction,
            &context.project_id,
            &context.attempt_id,
            request,
            family,
        )?)
}

pub fn apply_human_decision(
    transaction: &Transaction<'_>,
    operation_id: &str,
    request_id: &str,
    expected_revision: i64,
    decision: PermissionDecision,
    lifetime: Option<PermissionLifetime>,
    reason: &str,
) -> Result<OperationResult> {
    let held: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id='database-restore-hold' AND state='attention_required')",
        [],
        |row| row.get(0),
    )?;
    if held {
        bail!("permission decisions are disabled by the database restore hold")
    }
    if reason.len() > 8192 {
        bail!("permission decision reason is limited to 8192 characters")
    }
    let now = Utc::now().to_rfc3339();
    let (
        provider,
        project,
        attempt,
        role,
        session,
        generation,
        native,
        registered_root,
        identity,
        worktree,
        policy,
        family_json,
        deadline,
        tool_name,
        input_json,
    ): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        String,
    ) = transaction
        .query_row(
            "SELECT pr.provider,pr.project_id,pr.attempt_id,pr.role,pr.session_id,pr.role_generation_id,pr.native_session_id,p.repository_path,p.repository_identity,pr.cwd,pr.policy_fingerprint,pr.family_json,pr.deadline_at,pr.tool_name,pr.input_json
             FROM permission_requests pr JOIN projects p ON p.id=pr.project_id
             JOIN sessions s ON s.id=pr.session_id JOIN role_generations rg ON rg.id=pr.role_generation_id
             JOIN attempts a ON a.id=pr.attempt_id JOIN workspaces w ON w.attempt_id=a.id
             WHERE pr.id=?1 AND pr.revision=?2 AND pr.state='pending' AND pr.role='implementer'
               AND rg.id=s.role_generation_id AND rg.status NOT IN ('replaced','revoked')
               AND s.status='running' AND s.native_session_id=pr.native_session_id
               AND s.capability_key=pr.policy_fingerprint AND w.state='ready'
               AND w.repository_identity=p.repository_identity",
            params![request_id, expected_revision],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("permission request is stale, non-actionable, or already decided")
        })?;
    if deadline <= now {
        bail!("permission request deadline elapsed")
    }
    if decision != PermissionDecision::Deny {
        let provider_kind: Provider = provider.parse().map_err(|error: String| anyhow!(error))?;
        let exact_input: serde_json::Value = serde_json::from_str(&input_json)
            .context("permission request no longer contains its exact structured action")?;
        validate_exact_action(provider_kind, &tool_name, &exact_input)
            .context("permission request is not complete enough to approve")?;
    }
    let next_state = match decision {
        PermissionDecision::Deny => "denied",
        PermissionDecision::ApproveOnce => "approved_once",
        PermissionDecision::AlwaysApprove => "approved_rule",
    };
    let mut rule_id = None;
    if decision == PermissionDecision::AlwaysApprove {
        let lifetime = lifetime
            .ok_or_else(|| anyhow!("Always approve requires Session or Project lifetime"))?;
        let family: FamilyCandidate = serde_json::from_str(
            family_json
                .as_deref()
                .ok_or_else(|| anyhow!("this request has no safely reusable command family"))?,
        )?;
        if family.registered_root != canonical_text(Path::new(&registered_root))?
            || family.repository_identity != identity
            || family.worktree_path != canonical_text(Path::new(&worktree))?
        {
            bail!("permission scope preview no longer matches the registered project or worktree")
        }
        let current = RequestContext {
            native_session_id: native.clone(),
            workspace: PathBuf::from(&family.worktree_path),
            registered_root: PathBuf::from(&family.registered_root),
            repository_identity: family.repository_identity.clone(),
            policy_fingerprint: policy.clone(),
        };
        if !project_scope_current(transaction, &project, &attempt, &current, &family)? {
            bail!("permission command family or managed-worktree identity changed before approval")
        }
        let id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO permission_rules(id,provider,project_id,role,lifetime,session_id,role_generation_id,native_session_id,registered_root,repository_identity,worktree_path,executable_kind,executable_value,display_family,policy_fingerprint,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,'authenticated_human',?16)",
            params![
                id,
                provider,
                project,
                role,
                lifetime.to_string(),
                (lifetime == PermissionLifetime::Session).then_some(session.as_str()),
                (lifetime == PermissionLifetime::Session).then_some(generation.as_str()),
                (lifetime == PermissionLifetime::Session).then_some(native.as_str()),
                family.registered_root,
                family.repository_identity,
                (lifetime == PermissionLifetime::Session)
                    .then_some(family.worktree_path.as_str()),
                family.executable_kind,
                family.executable_value,
                family.display_family,
                policy,
                now
            ],
        )?;
        rule_id = Some(id);
    } else if lifetime.is_some() {
        bail!("lifetime applies only to Always approve")
    }
    if transaction.execute(
        "UPDATE permission_requests SET state=?1,decision_kind=?2,decision_actor='authenticated_human',decision_reason=?3,matching_rule_id=?4,decided_at=?5,revision=revision+1,updated_at=?5
         WHERE id=?6 AND revision=?7 AND state='pending' AND delivery_state='not_reserved'",
        params![
            next_state,
            match decision {
                PermissionDecision::Deny => "deny",
                PermissionDecision::ApproveOnce => "approve_once",
                PermissionDecision::AlwaysApprove => "always_approve",
            },
            reason,
            rule_id,
            now,
            request_id,
            expected_revision
        ],
    )? != 1
    {
        bail!("permission request changed while the decision was applied")
    }
    Ok(OperationResult {
        operation_id: operation_id.to_owned(),
        entity_kind: "permission_request".to_owned(),
        entity_id: request_id.to_owned(),
        version: Some(expected_revision + 1),
        state: next_state.to_owned(),
        detail: serde_json::json!({"matching_rule_id":rule_id,"human_only":true}),
    })
}

pub fn revoke_rule(
    transaction: &Transaction<'_>,
    operation_id: &str,
    rule_id: &str,
    expected_revision: i64,
    reason: &str,
) -> Result<OperationResult> {
    if reason.len() > 8192 {
        bail!("revocation reason is limited to 8192 characters")
    }
    let now = Utc::now().to_rfc3339();
    if transaction.execute(
        "UPDATE permission_rules SET revoked_at=?1,revoked_by='authenticated_human',revoke_reason=?2,revision=revision+1
         WHERE id=?3 AND revision=?4 AND revoked_at IS NULL",
        params![now, reason, rule_id, expected_revision],
    )? != 1
    {
        bail!("permission rule is stale, unknown, or already revoked")
    }
    Ok(OperationResult {
        operation_id: operation_id.to_owned(),
        entity_kind: "permission_rule".to_owned(),
        entity_id: rule_id.to_owned(),
        version: Some(expected_revision + 1),
        state: "revoked".to_owned(),
        detail: serde_json::json!({
            "future_decisions_only":true,
            "already_dispatched_actions_cannot_be_undone":true
        }),
    })
}

pub fn state_rows(
    connection: &Connection,
) -> Result<(Vec<PermissionRequestDto>, Vec<PermissionRuleDto>)> {
    let requests = {
        let mut statement = connection.prepare(
            "SELECT id,project_id,task_id,attempt_id,session_id,role_generation_id,role,provider,native_session_id,tool_name,input_json,requested_access_json,reason,command_display,family_json,family_unavailable_reason,created_at,deadline_at,state,revision,decision_kind,decision_actor,decision_reason,decided_at,matching_rule_id,delivery_state,delivery_reserved_at,reserved_behavior,delivered_at,delivery_unknown_at,delivery_reason,consumed_at
             FROM permission_requests
             ORDER BY CASE state WHEN 'pending' THEN 0 ELSE 1 END,created_at DESC LIMIT 200",
        )?;
        let rows = statement.query_map([], |row| {
            let role: String = row.get(6)?;
            let provider: String = row.get(7)?;
            let family: Option<String> = row.get(14)?;
            Ok(PermissionRequestDto {
                id: row.get(0)?,
                project_id: row.get(1)?,
                task_id: row.get(2)?,
                attempt_id: row.get(3)?,
                session_id: row.get(4)?,
                role_generation_id: row.get(5)?,
                role: role.parse().unwrap_or(RoleKind::Implementer),
                provider: provider.parse().unwrap_or(Provider::Codex),
                native_session_id: row.get(8)?,
                tool_name: row.get(9)?,
                input: serde_json::from_str(&row.get::<_, String>(10)?)
                    .unwrap_or(serde_json::Value::Null),
                requested_access: row
                    .get::<_, Option<String>>(11)?
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or(serde_json::Value::Null),
                reason: row.get(12)?,
                command_display: row.get(13)?,
                family_preview: family
                    .and_then(|value| serde_json::from_str::<FamilyCandidate>(&value).ok())
                    .map(|family| {
                        serde_json::json!({
                            "session":family.session_preview,
                            "project":family.project_preview
                        })
                    })
                    .unwrap_or(serde_json::Value::Null),
                family_unavailable_reason: row.get(15)?,
                created_at: row.get(16)?,
                deadline_at: row.get(17)?,
                state: row.get(18)?,
                revision: row.get(19)?,
                decision_kind: row.get(20)?,
                decision_actor: row.get(21)?,
                decision_reason: row.get(22)?,
                decided_at: row.get(23)?,
                matching_rule_id: row.get(24)?,
                delivery_state: row.get(25)?,
                delivery_reserved_at: row.get(26)?,
                reserved_behavior: row.get(27)?,
                delivered_at: row.get(28)?,
                delivery_unknown_at: row.get(29)?,
                delivery_reason: row.get(30)?,
                consumed_at: row.get(31)?,
            })
        })?;
        let collected = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    let rules = {
        let mut statement = connection.prepare(
            "SELECT id,provider,project_id,role,lifetime,session_id,display_family,registered_root,repository_identity,worktree_path,executable_kind,executable_value,policy_fingerprint,created_at,revoked_at,last_used_at,use_count,revision,native_session_id
             FROM permission_rules ORDER BY created_at DESC LIMIT 200",
        )?;
        let rows = statement.query_map([], |row| {
            let provider: String = row.get(1)?;
            let role: String = row.get(3)?;
            let lifetime: String = row.get(4)?;
            Ok(PermissionRuleDto {
                id: row.get(0)?,
                provider: provider.parse().unwrap_or(Provider::Codex),
                project_id: row.get(2)?,
                role: role.parse().unwrap_or(RoleKind::Implementer),
                lifetime: if lifetime == "project" {
                    PermissionLifetime::Project
                } else {
                    PermissionLifetime::Session
                },
                session_id: row.get(5)?,
                display_family: row.get(6)?,
                scope: serde_json::json!({
                    "registered_root":row.get::<_,String>(7)?,
                    "repository_identity":row.get::<_,String>(8)?,
                    "worktree":row.get::<_,Option<String>>(9)?,
                    "executable_kind":row.get::<_,String>(10)?,
                    "executable":row.get::<_,String>(11)?,
                    "policy_fingerprint":row.get::<_,String>(12)?,
                    "configuration_binding":"The complete validated frozen configuration fingerprint must remain compatible, including provider executable version, hook and security policy, launch arguments and environment keys, model, and effort.",
                    "native_session":row.get::<_,Option<String>>(18)?,
                    "coverage":if lifetime == "project" {
                        "current and future ready engine-owned worktrees with the same project and Git common-directory identity"
                    } else {
                        "this exact managed role/native session and worktree"
                    }
                }),
                created_at: row.get(13)?,
                revoked_at: row.get(14)?,
                last_used_at: row.get(15)?,
                use_count: row.get(16)?,
                revision: row.get(17)?,
            })
        })?;
        let collected = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };
    Ok((requests, rules))
}

fn request_context(
    store: &Store,
    context: &RoleContext,
    payload: &serde_json::Value,
) -> Result<RequestContext> {
    let (
        native,
        status,
        capability,
        workspace,
        workspace_state,
        registered_root,
        identity,
        generation_status,
    ): (
        Option<String>,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
        String,
    ) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT s.native_session_id,s.status,s.capability_key,w.path,w.state,p.repository_path,p.repository_identity,rg.status
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
             JOIN projects p ON p.id=t.project_id JOIN workspaces w ON w.attempt_id=a.id
             WHERE s.id=?1 AND rg.id=?2 AND a.id=?3 AND t.id=?4 AND p.id=?5",
            params![
                context.session_id,
                context.role_generation_id,
                context.attempt_id,
                context.task_id,
                context.project_id
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?
    };
    if status != "running"
        || generation_status == "replaced"
        || generation_status == "revoked"
        || workspace_state != "ready"
    {
        bail!("permission request is not attached to a current running generation and ready workspace")
    }
    let native = native.filter(|value| !value.is_empty()).ok_or_else(|| {
        anyhow!("managed SessionStart has not latched a native session candidate")
    })?;
    if payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        != Some(native.as_str())
    {
        bail!("permission request native session contradicts the managed SessionStart candidate")
    }
    let capability = capability
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("session has no frozen permission policy fingerprint"))?;
    let workspace =
        std::fs::canonicalize(workspace).context("canonicalize recorded managed worktree")?;
    let registered_root =
        std::fs::canonicalize(registered_root).context("canonicalize registered project root")?;
    validate_call_cwd(payload, &workspace)?;
    let repository = crate::workspace::inspect(&workspace)?;
    if repository.identity != identity {
        bail!("managed worktree no longer has the registered Git common-directory identity")
    }
    Ok(RequestContext {
        native_session_id: native,
        workspace,
        registered_root,
        repository_identity: identity,
        policy_fingerprint: capability,
    })
}

fn classify_family(
    provider: Provider,
    tool_name: &str,
    input: &serde_json::Value,
    context: &RequestContext,
) -> Result<(FamilyCandidate, Option<String>)> {
    let (argv, display) = command_argv(provider, tool_name, input)?;
    validate_call_cwd(input, &context.workspace)?;
    let executable = argv
        .first()
        .ok_or_else(|| anyhow!("empty commands cannot become reusable rules"))?;
    if executable.contains('=') && !executable.contains('/') {
        bail!("environment assignments or prefixes cannot become reusable rules")
    }
    let basename = Path::new(executable)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(executable);
    if is_interpreter_or_wrapper(basename) {
        bail!("shell, interpreter, environment, or privilege wrappers cannot become reusable rules")
    }
    let (kind, value, canonical) = resolve_executable(executable, &context.workspace)?;
    let display_family = if kind == "project_relative" {
        format!("./{value} with all arguments")
    } else {
        format!("{canonical} with all arguments")
    };
    let warning = if provider == Provider::Codex {
        "The requested rule trusts this executable family even if its content later changes, with all current and future arguments. The enforced Codex profile remains active; Codex Implementer still requires exact current native validation. Native approvals may be reused without an inbox request, and app Revoke affects app-owned rules only. This request does not grant blanket agent access, confine command effects to cwd, or itself establish provider capability or production support."
    } else {
        "The requested rule trusts this executable family even if its content later changes, with all current and future arguments. It does not grant blanket agent access, confine command effects to cwd, or establish provider capability or production support."
    };
    let session_preview = serde_json::json!({
        "lifetime":"session",
        "command_family":display_family,
        "arguments":"all",
        "provider":provider,
        "role":"implementer",
        "native_session":context.native_session_id,
        "worktree":context.workspace,
        "coverage":"this exact managed role/native session and worktree",
        "configuration_binding":"The complete validated frozen configuration fingerprint must remain compatible, including model and effort.",
        "warning":warning
    });
    let project_preview = serde_json::json!({
        "lifetime":"project",
        "command_family":display_family,
        "arguments":"all",
        "provider":provider,
        "role":"implementer",
        "registered_root":context.registered_root,
        "coverage":"current and future ready engine-owned worktrees with the same project and Git common-directory identity",
        "configuration_binding":"The complete validated frozen configuration fingerprint must remain compatible, including model and effort.",
        "warning":warning
    });
    Ok((
        FamilyCandidate {
            executable_kind: kind,
            executable_value: value,
            display_family,
            canonical_executable: canonical,
            registered_root: context.registered_root.to_string_lossy().into_owned(),
            repository_identity: context.repository_identity.clone(),
            worktree_path: context.workspace.to_string_lossy().into_owned(),
            session_preview,
            project_preview,
        },
        display,
    ))
}

pub(crate) fn service_check_family(
    connection: &Connection,
    attempt_id: &str,
    command_kind: &str,
    executable: Option<&str>,
    cwd: &str,
) -> Result<ServiceCheckFamily> {
    if command_kind != "structured_argv" {
        bail!("exact shell commands remain exact-only and cannot become reusable families")
    }
    let executable = executable.ok_or_else(|| anyhow!("structured check executable is missing"))?;
    let basename = Path::new(executable)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(executable);
    if is_interpreter_or_wrapper(basename) {
        bail!("shell, interpreter, environment, or privilege wrappers remain exact-only")
    }
    let (project_id, root, identity, worktree): (String, String, String, String) = connection
        .query_row(
            "SELECT p.id,p.repository_path,p.repository_identity,w.path
             FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
             JOIN workspaces w ON w.attempt_id=a.id
             WHERE a.id=?1 AND p.internal_purpose IS NULL AND w.state='ready'",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("selected check has no current service-owned project worktree"))?;
    let worktree =
        std::fs::canonicalize(&worktree).context("canonicalize selected-check worktree")?;
    let registered_root =
        std::fs::canonicalize(&root).context("canonicalize registered project root")?;
    let inspected = crate::workspace::inspect(&worktree)?;
    if inspected.identity != identity {
        bail!("selected-check worktree no longer has the registered repository identity")
    }
    let execution_cwd = crate::trip::contained_path(&worktree, cwd, true)?;
    if !execution_cwd.is_dir() {
        bail!("selected check working directory must be a contained directory")
    }
    let (kind, value, canonical) =
        resolve_executable_in_root(executable, &execution_cwd, &worktree)?;
    let display_family = if kind == "project_relative" {
        format!("./{value} with all arguments")
    } else {
        format!("{canonical} with all arguments")
    };
    let preview = serde_json::json!({
        "source":"service_check",
        "lifetime":"project",
        "project_id":project_id,
        "command_family":display_family,
        "arguments":"all current and future arguments after independent check selection/review",
        "registered_root":registered_root,
        "repository_identity":identity,
        "worktree":worktree,
        "coverage":"service-owned selected checks for this project and Git worktree identity only",
        "warning":"This rule replaces only the repetitive service-check permission decision. Current check selection, candidate, inputs, working directory, freshness, and build-owner gates still apply. It is distinct from native agent sandbox permissions and grants no shell or agent access."
    });
    Ok(ServiceCheckFamily {
        executable_kind: kind,
        executable_value: value,
        display_family,
        registered_root: registered_root.to_string_lossy().into_owned(),
        repository_identity: identity,
        worktree_path: worktree.to_string_lossy().into_owned(),
        preview,
    })
}

pub(crate) fn matching_service_check_rule(
    connection: &Connection,
    attempt_id: &str,
    family: &ServiceCheckFamily,
) -> Result<Option<(String, i64)>> {
    let matched: Option<(String, i64, String, Option<String>)> = connection
        .query_row(
            "SELECT r.id,r.revision,w.path,r.revoked_at FROM trip_check_permission_rules r
             JOIN attempts a ON a.id=?1 JOIN tasks t ON t.id=a.task_id
             JOIN projects p ON p.id=t.project_id JOIN workspaces w ON w.attempt_id=a.id
             WHERE r.project_id=p.id AND r.source='service_check'
               AND r.registered_root=?2 AND r.repository_identity=?3
               AND r.executable_kind=?4 AND r.executable_value=?5
               AND p.repository_identity=r.repository_identity AND p.internal_purpose IS NULL
               AND w.state='ready' AND w.repository_identity=r.repository_identity
             ORDER BY r.created_at DESC,r.rowid DESC LIMIT 1",
            params![
                attempt_id,
                family.registered_root,
                family.repository_identity,
                family.executable_kind,
                family.executable_value
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    Ok(matched.and_then(|(id, revision, workspace, revoked_at)| {
        (revoked_at.is_none()
            && canonical_text(Path::new(&workspace)).ok().as_deref()
                == Some(family.worktree_path.as_str()))
        .then_some((id, revision))
    }))
}

fn is_interpreter_or_wrapper(basename: &str) -> bool {
    if [
        "sh",
        "bash",
        "zsh",
        "fish",
        "dash",
        "ksh",
        "csh",
        "tcsh",
        "env",
        "sudo",
        "command",
        "builtin",
        "nohup",
        "python",
        "pythonw",
        "pypy",
        "node",
        "nodejs",
        "deno",
        "bun",
        "ruby",
        "perl",
        "php",
        "lua",
        "luajit",
        "rscript",
        "R",
        "julia",
        "java",
        "groovy",
        "scala",
        "kotlin",
        "swift",
        "pwsh",
        "powershell",
        "osascript",
    ]
    .contains(&basename)
    {
        return true;
    }
    [
        "python", "pythonw", "pypy", "ruby", "perl", "php", "lua", "node", "nodejs",
    ]
    .into_iter()
    .any(|prefix| {
        basename.strip_prefix(prefix).is_some_and(|suffix| {
            suffix
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
        })
    })
}

fn command_argv(
    provider: Provider,
    tool_name: &str,
    input: &serde_json::Value,
) -> Result<(Vec<String>, Option<String>)> {
    if provider == Provider::Claude && tool_name != "Bash" {
        bail!("only Claude Bash PermissionRequest inputs have a reusable command adapter")
    }
    if provider == Provider::Codex
        && !matches!(
            tool_name.to_ascii_lowercase().as_str(),
            "shell" | "shell_command" | "exec_command" | "command" | "bash"
        )
    {
        bail!("this Codex tool has no reusable local command adapter")
    }
    let command = consistent_command_field(input)?
        .ok_or_else(|| anyhow!("provider input has no recognized command field"))?;
    match command {
        serde_json::Value::String(script) => Ok((simple_argv(script)?, Some(script.clone()))),
        serde_json::Value::Array(values) if provider == Provider::Codex => {
            let argv = values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("command argv contains a non-string value"))
                })
                .collect::<Result<Vec<_>>>()?;
            if argv.len() == 3
                && matches!(argv[1].as_str(), "-c" | "-lc")
                && allowed_shell(&argv[0])?
            {
                let parsed = simple_argv(&argv[2])?;
                if parsed.first().is_some_and(|value| {
                    ["sh", "bash", "zsh"].contains(
                        &Path::new(value)
                            .file_name()
                            .and_then(|part| part.to_str())
                            .unwrap_or(value),
                    )
                }) {
                    bail!("nested interpreter chains cannot become reusable rules")
                }
                Ok((parsed, Some(argv[2].clone())))
            } else if argv.iter().any(|value| {
                value
                    .chars()
                    .any(|character| matches!(character, '\n' | '\r' | '\0'))
            }) {
                bail!("multiline or NUL-containing argv cannot become a reusable rule")
            } else {
                Ok((argv.clone(), None))
            }
        }
        _ => bail!("provider command shape is not supported for reusable matching"),
    }
}

fn validate_exact_action(
    provider: Provider,
    tool_name: &str,
    input: &serde_json::Value,
) -> Result<()> {
    if provider == Provider::Claude && tool_name != "Bash" {
        bail!("only a complete Claude Bash action can be represented by this permission bridge")
    }
    if provider == Provider::Codex
        && !matches!(
            tool_name.to_ascii_lowercase().as_str(),
            "shell" | "shell_command" | "exec_command" | "command" | "bash"
        )
    {
        bail!("this Codex tool has no exact-action adapter for one-time approval")
    }
    let command = consistent_command_field(input)?
        .ok_or_else(|| anyhow!("provider input has no recognized command, cmd, or argv action"))?;
    match command {
        serde_json::Value::String(script) if !script.trim().is_empty() => Ok(()),
        serde_json::Value::String(_) => {
            bail!("provider command is empty and cannot be approved")
        }
        serde_json::Value::Array(values) if provider == Provider::Codex => {
            if values.is_empty() {
                bail!("provider command argv is empty and cannot be approved")
            }
            let argv = values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| anyhow!("provider command argv contains a non-string value"))
                })
                .collect::<Result<Vec<_>>>()?;
            if argv
                .first()
                .is_none_or(|executable| executable.trim().is_empty())
            {
                bail!("provider command argv has no nonempty executable")
            }
            if argv.len() == 3
                && matches!(argv[1], "-c" | "-lc")
                && Path::new(argv[0])
                    .file_name()
                    .and_then(|part| part.to_str())
                    .is_some_and(|part| ["sh", "bash", "zsh"].contains(&part))
                && argv[2].trim().is_empty()
            {
                bail!("provider shell argv has an empty script")
            }
            Ok(())
        }
        serde_json::Value::Array(_) => {
            bail!("Claude Bash requires one complete command string")
        }
        _ => bail!("provider command has the wrong type for exact one-time approval"),
    }
}

fn consistent_command_field(input: &serde_json::Value) -> Result<Option<&serde_json::Value>> {
    let fields = ["command", "cmd", "argv"]
        .into_iter()
        .filter_map(|name| input.get(name))
        .collect::<Vec<_>>();
    let Some(first) = fields.first().copied() else {
        return Ok(None);
    };
    if fields.iter().skip(1).any(|value| *value != first) {
        bail!("provider input contains contradictory command, cmd, or argv fields")
    }
    Ok(Some(first))
}

pub(crate) fn command_text(
    provider: Provider,
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<String> {
    validate_exact_action(provider, tool_name, input).ok()?;
    let command = consistent_command_field(input).ok().flatten()?;
    match command {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Array(_) if provider == Provider::Codex => None,
        _ => None,
    }
}

pub(crate) fn is_direct_role_command(
    provider: Provider,
    tool_name: &str,
    input: &serde_json::Value,
    role_executable: &Path,
    operation: &str,
) -> bool {
    let Ok((argv, _)) = command_argv(provider, tool_name, input) else {
        return false;
    };
    if argv
        .first()
        .is_none_or(|executable| Path::new(executable) != role_executable)
    {
        return false;
    }
    match operation {
        "context" => argv.len() == 3 && argv[1..] == ["role", "context"],
        "report" => {
            (argv.len() == 5
                && argv[1..4] == ["role", "report", "--json"]
                && !argv[4].trim().is_empty())
                || (argv.len() >= 4
                    && argv[1..3] == ["role", "report"]
                    && crate::domain::parse_runtime_role_report_args(&argv[3..]).is_ok())
        }
        _ => false,
    }
}

fn validate_call_cwd(input: &serde_json::Value, expected: &Path) -> Result<()> {
    let fields = [input.get("cwd"), input.get("workdir")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    for value in fields {
        let cwd = value
            .as_str()
            .ok_or_else(|| anyhow!("per-call cwd/workdir must be a string"))?;
        if std::fs::canonicalize(cwd).ok().as_deref() != Some(expected) {
            bail!("per-call cwd/workdir contradicts the recorded managed worktree")
        }
    }
    Ok(())
}

fn simple_argv(script: &str) -> Result<Vec<String>> {
    if script.is_empty()
        || script
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        bail!("multiline, empty, or NUL-containing shell input cannot become a reusable rule")
    }
    let mut argv = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in script.chars() {
        if escaped {
            word.push(character);
            started = true;
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None
                } else {
                    word.push(character)
                }
                started = true;
            }
            Some('"') => {
                if character == '"' {
                    quote = None
                } else if character == '\\' {
                    escaped = true
                } else if matches!(character, '$' | '\u{60}') {
                    bail!("double-quoted substitutions cannot become reusable rules")
                } else {
                    word.push(character)
                }
                started = true;
            }
            _ if character == '\'' || character == '"' => {
                quote = Some(character);
                started = true;
            }
            _ if character == '\\' => {
                escaped = true;
                started = true;
            }
            _ if character.is_whitespace() => {
                if started {
                    argv.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ if matches!(
                character,
                ';' | '&'
                    | '|'
                    | '<'
                    | '>'
                    | '$'
                    | '\u{60}'
                    | '('
                    | ')'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '['
                    | ']'
            ) =>
            {
                bail!("shell controls, substitutions, redirects, or expansions cannot become reusable rules")
            }
            _ => {
                word.push(character);
                started = true;
            }
        }
    }
    if escaped || quote.is_some() {
        bail!("unterminated quote or escape cannot become a reusable rule")
    }
    if started {
        argv.push(word)
    }
    if argv.is_empty() {
        bail!("empty command cannot become a reusable rule")
    }
    Ok(argv)
}

fn allowed_shell(value: &str) -> Result<bool> {
    let canonical = std::fs::canonicalize(value).ok();
    let installed = std::env::var_os("SHELL").and_then(|value| std::fs::canonicalize(value).ok());
    let allowed = ["/bin/sh", "/bin/bash", "/bin/zsh"]
        .into_iter()
        .filter_map(|value| std::fs::canonicalize(value).ok())
        .collect::<Vec<_>>();
    Ok(canonical.as_ref().is_some_and(|candidate| {
        installed.as_ref() == Some(candidate) || allowed.contains(candidate)
    }))
}

fn resolve_executable(value: &str, cwd: &Path) -> Result<(String, String, String)> {
    resolve_executable_in_root(value, cwd, cwd)
}

fn resolve_executable_in_root(
    value: &str,
    cwd: &Path,
    identity_root: &Path,
) -> Result<(String, String, String)> {
    let path = Path::new(value);
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        bail!("parent traversal cannot become a reusable executable family")
    }
    if path.is_absolute() || value.contains('/') {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            ensure_no_symlink_components(cwd, path)?;
            cwd.join(path)
        };
        let metadata = std::fs::symlink_metadata(&joined)?;
        if metadata.file_type().is_symlink() {
            bail!("symlink executables cannot become reusable rules")
        }
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            bail!("command executable is not a regular file")
        }
        let canonical = joined
            .canonicalize()
            .with_context(|| format!("resolve command executable {}", joined.display()))?;
        if let Ok(relative) = canonical.strip_prefix(identity_root) {
            let supplied_relative = if path.is_absolute() {
                joined.strip_prefix(identity_root).map_err(|_| {
                    anyhow!("absolute executable reaches the worktree through an alias")
                })?
            } else {
                canonical.strip_prefix(identity_root)?
            };
            ensure_no_symlink_components(identity_root, supplied_relative)?;
            if relative.as_os_str().is_empty()
                || relative.components().any(|part| {
                    matches!(
                        part,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
            {
                bail!("relative executable identity is unsafe")
            }
            return Ok((
                "project_relative".to_owned(),
                relative.to_string_lossy().into_owned(),
                canonical.to_string_lossy().into_owned(),
            ));
        }
        if !path.is_absolute() {
            bail!("relative executable escapes the managed worktree")
        }
        return Ok((
            "absolute".to_owned(),
            canonical.to_string_lossy().into_owned(),
            canonical.to_string_lossy().into_owned(),
        ));
    }
    bail!(
        "Always approve is unavailable for bare PATH command names because the native shell's executable resolution is unknown; use an exact absolute executable path or a verified project-relative executable"
    )
}

fn matching_rule(
    transaction: &Transaction<'_>,
    context: &RoleContext,
    request: &RequestContext,
    family: &FamilyCandidate,
) -> Result<Option<String>> {
    let mut statement = transaction.prepare(
        "SELECT id,lifetime,session_id,role_generation_id,native_session_id,registered_root,repository_identity,worktree_path,executable_kind,executable_value
         FROM permission_rules
         WHERE project_id=?1 AND provider=?2 AND role=?3 AND policy_fingerprint=?4
           AND revoked_at IS NULL ORDER BY created_at",
    )?;
    let rows = statement
        .query_map(
            params![
                context.project_id,
                context.provider.to_string(),
                context.role.to_string(),
                request.policy_fingerprint
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (id, lifetime, session, generation, native, root, identity, worktree, kind, executable) in
        rows
    {
        if canonical_text(Path::new(&root)).ok().as_deref()
            != Some(request.registered_root.to_string_lossy().as_ref())
            || identity != request.repository_identity
            || kind != family.executable_kind
            || executable != family.executable_value
        {
            continue;
        }
        let matched = if lifetime == "session" {
            session.as_deref() == Some(context.session_id.as_str())
                && generation.as_deref() == Some(context.role_generation_id.as_str())
                && native.as_deref() == Some(request.native_session_id.as_str())
                && worktree
                    .as_deref()
                    .and_then(|path| canonical_text(Path::new(path)).ok())
                    .as_deref()
                    == Some(request.workspace.to_string_lossy().as_ref())
        } else if lifetime == "project" {
            project_scope_current(
                transaction,
                &context.project_id,
                &context.attempt_id,
                request,
                family,
            )?
        } else {
            false
        };
        if matched {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn project_scope_current(
    transaction: &Transaction<'_>,
    project_id: &str,
    attempt_id: &str,
    request: &RequestContext,
    family: &FamilyCandidate,
) -> Result<bool> {
    let current: Option<(String, String, String, String)> = transaction
        .query_row(
            "SELECT p.repository_path,p.repository_identity,w.path,w.state
             FROM projects p JOIN tasks t ON t.project_id=p.id
             JOIN attempts a ON a.task_id=t.id JOIN workspaces w ON w.attempt_id=a.id
             WHERE p.id=?1 AND a.id=?2",
            params![project_id, attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((root, identity, workspace, state)) = current else {
        return Ok(false);
    };
    if state != "ready"
        || identity != family.repository_identity
        || canonical_text(Path::new(&workspace)).ok().as_deref()
            != Some(request.workspace.to_string_lossy().as_ref())
        || canonical_text(Path::new(&root)).ok().as_deref() != Some(family.registered_root.as_str())
    {
        return Ok(false);
    }
    let inspected = crate::workspace::inspect(&request.workspace)?;
    if inspected.identity != family.repository_identity {
        return Ok(false);
    }
    if family.executable_kind == "project_relative" {
        let relative = Path::new(&family.executable_value);
        if relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Ok(false);
        }
        if ensure_no_symlink_components(&request.workspace, relative).is_err() {
            return Ok(false);
        }
        let joined = request.workspace.join(relative);
        let Ok(metadata) = std::fs::symlink_metadata(&joined) else {
            return Ok(false);
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
        {
            return Ok(false);
        }
        let Ok(resolved) = joined.canonicalize() else {
            return Ok(false);
        };
        if resolved
            .strip_prefix(&request.workspace)
            .ok()
            .filter(|observed| *observed == relative)
            .is_none()
        {
            return Ok(false);
        }
    } else {
        if family.canonical_executable != family.executable_value {
            return Ok(false);
        }
        let path = Path::new(&family.executable_value);
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return Ok(false);
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || canonical_text(path).ok().as_deref() != Some(family.executable_value.as_str())
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn canonical_text(path: &Path) -> Result<String> {
    Ok(path.canonicalize()?.to_string_lossy().into_owned())
}

fn ensure_no_symlink_components(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current)
            .with_context(|| format!("inspect command path {}", current.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("command path traverses symlink {}", current.display())
        }
        if index + 1 < count && !metadata.is_dir() {
            bail!("command path traverses non-directory {}", current.display())
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{command_argv, is_direct_role_command, simple_argv};
    use crate::domain::{parse_runtime_role_report_args, Provider};
    use std::path::Path;

    #[test]
    fn permission_family_lexer_keeps_all_arguments_without_shell_widening() {
        assert_eq!(
            simple_argv("./tool deploy --target 'one environment'").unwrap(),
            ["./tool", "deploy", "--target", "one environment"]
        );
        for unsafe_command in [
            "./tool deploy && git push",
            "./tool $(whoami)",
            "./tool deploy > result",
            "./tool \"$TOKEN\"",
        ] {
            assert!(simple_argv(unsafe_command).is_err(), "{unsafe_command}");
        }

        let executable = Path::new("/opt/agenticjira");
        let exact_command = r#"/bin/sh -lc '/usr/bin/true ; /usr/bin/touch -- '\''/tmp/runtime-compound-nonce.txt'\'''"#;
        let report = serde_json::json!({
            "operation_id":"encoded-apostrophe-roundtrip",
            "outcome":"capability_observed",
            "summary":"don't alter quoted command data",
            "evidence":[],
            "metadata":{"validation_observation":{"actual_outcomes":[{
                "command":exact_command
            }]}}
        });
        let literal_json = report.to_string().replace('\'', r"\u0027");
        assert!(literal_json.contains(r"don\u0027t"));
        assert!(literal_json.contains(r"-lc \u0027/usr/bin/true"));
        assert!(literal_json.contains(r"\\"));
        assert!(!literal_json.contains('\''));
        let encoded_command = format!(
            "{} role report --json '{}'",
            executable.display(),
            literal_json
        );
        let tool_input_json =
            serde_json::to_string(&serde_json::json!({"command":encoded_command})).unwrap();
        assert!(tool_input_json.contains(r"\\u0027"));
        let input: serde_json::Value = serde_json::from_str(&tool_input_json).unwrap();
        assert_eq!(input["command"], encoded_command);
        let (argv, _) = command_argv(Provider::Codex, "shell", &input).unwrap();
        assert_eq!(argv[4], literal_json);
        assert!(is_direct_role_command(
            Provider::Codex,
            "shell",
            &input,
            executable,
            "report"
        ));
        let decoded: serde_json::Value = serde_json::from_str(&argv[4]).unwrap();
        assert_eq!(decoded["summary"], "don't alter quoted command data");
        assert_eq!(
            decoded["metadata"]["validation_observation"]["actual_outcomes"][0]["command"],
            exact_command
        );

        let raw_outer_unicode_escape = tool_input_json.replace(r"\\u0027", r"\u0027");
        let malformed_input: serde_json::Value =
            serde_json::from_str(&raw_outer_unicode_escape).unwrap();
        let malformed_command = malformed_input["command"].as_str().unwrap();
        assert!(malformed_command.contains("don't alter quoted command data"));
        assert!(!malformed_command.contains(r"\u0027"));
        assert!(command_argv(Provider::Codex, "shell", &malformed_input).is_err());

        let runtime_args = [
            "--runtime-v1",
            "--status=passed",
            "--operation-id=runtime-report-1",
            "--nonce=nonce-1",
            "--model-evidence=claude-model",
            "--session-mode=fresh",
            "--target-data-accessed=false",
            "--fallback-observed=false",
            "--authentication-observed=true",
            "--actual=z_probe,true,1,denied,os,native_session",
            "--actual=a_probe,true,0,succeeded,none,native_session",
        ]
        .map(str::to_owned);
        let report = parse_runtime_role_report_args(&runtime_args).unwrap();
        let outcomes = report.metadata["validation_observation"]["actual_outcomes"]
            .as_array()
            .unwrap();
        assert_eq!(outcomes[0]["operation_id"], "a_probe");
        assert_eq!(outcomes[1]["operation_id"], "z_probe");
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.get("command").is_none()));
        assert_eq!(report.metadata["runtime_report_format"], "runtime-v1");
        assert_eq!(report.metadata["history_nonce"], "nonce-1");
        assert!(report.metadata["validation_observation"]
            .get("effective_sandbox_identity")
            .is_none());
        assert!(report.evidence.is_empty());

        let provider_pre_spawn = [
            "--runtime-v1",
            "--status=failed",
            "--operation-id=runtime-provider-denial-1",
            "--failure-category=provider_denial",
            "--actual=write_probe,true,null,denied,provider,native_session",
        ]
        .map(str::to_owned);
        let provider_report = parse_runtime_role_report_args(&provider_pre_spawn).unwrap();
        assert_eq!(
            provider_report.metadata["validation_observation"]["actual_outcomes"][0],
            serde_json::json!({
                "operation_id":"write_probe",
                "attempted":true,
                "exit_status":null,
                "result":"denied",
                "denial_source":"provider",
                "authentication_source":"native_session"
            })
        );
        let mut invalid_provider_pre_spawn = provider_pre_spawn.to_vec();
        invalid_provider_pre_spawn[4] =
            "--actual=write_probe,true,null,invocation_prevented,provider,native_session".into();
        assert!(parse_runtime_role_report_args(&invalid_provider_pre_spawn).is_err());

        let mut reordered = runtime_args.to_vec();
        reordered.swap(9, 10);
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::to_value(parse_runtime_role_report_args(&reordered).unwrap()).unwrap()
        );
        let mut supplied_identity = runtime_args.to_vec();
        supplied_identity.insert(5, "--sandbox-identity=provider-exposed".into());
        assert_eq!(
            parse_runtime_role_report_args(&supplied_identity)
                .unwrap()
                .metadata["validation_observation"]["effective_sandbox_identity"],
            "provider-exposed"
        );
        let mut empty_identity = runtime_args.to_vec();
        empty_identity.insert(5, "--sandbox-identity=".into());
        assert!(parse_runtime_role_report_args(&empty_identity).is_err());
        let runtime_command = format!(
            "{} role report {}",
            executable.display(),
            runtime_args.join(" ")
        );
        let runtime_input = serde_json::json!({"command":runtime_command});
        assert!(is_direct_role_command(
            Provider::Claude,
            "Bash",
            &runtime_input,
            executable,
            "report"
        ));
        for invalid in [
            format!("{} suffix", runtime_input["command"].as_str().unwrap()),
            format!(
                "{} --nonce=bad%atom",
                runtime_input["command"].as_str().unwrap()
            ),
            format!(
                "{} --status=passed",
                runtime_input["command"].as_str().unwrap()
            ),
        ] {
            assert!(!is_direct_role_command(
                Provider::Claude,
                "Bash",
                &serde_json::json!({"command":invalid}),
                executable,
                "report"
            ));
        }
        let missing_context = [
            "--runtime-v1".to_owned(),
            "--operation-id=runtime-missing".to_owned(),
            "--status=missing_context".to_owned(),
        ];
        let missing_report = parse_runtime_role_report_args(&missing_context).unwrap();
        assert_eq!(
            missing_report.metadata["validation_observation"],
            serde_json::json!({"cell":"trip_runtime_probe","status":"missing_context"})
        );
        assert!(missing_report.metadata.get("history_nonce").is_none());
        let mut forbidden_missing = missing_context.to_vec();
        forbidden_missing.push("--sandbox-identity=not-allowed".into());
        assert!(parse_runtime_role_report_args(&forbidden_missing).is_err());
    }
}
