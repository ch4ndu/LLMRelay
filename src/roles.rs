use crate::store::Store;
use crate::supervisor::Supervisor;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

#[derive(Clone)]
pub struct RoleService {
    store: Store,
    supervisor: Supervisor,
    runtime: Option<RoleSwitchRuntime>,
}

#[derive(Clone)]
struct RoleSwitchRuntime {
    hooks: crate::providers::HookAssets,
    role_socket: std::path::PathBuf,
    executable: std::path::PathBuf,
}

pub(crate) struct ManagerSafeIdleBoundary {
    pub control_id: String,
    pub session_id: String,
    pub stop_event_rowid: i64,
}

/// Only a human replacement interrupts the old role and retires its provider holds.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SwitchOrigin {
    HumanReplacement,
    ManagerChange,
}

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const MAX_GUIDANCE_INPUT_BYTES: usize = 64 * 1024;

/// Guidance that cannot be submitted literally, found before any lease, reservation or write.
#[derive(Debug)]
pub(crate) struct GuidanceNotDeliverable(String);

impl std::fmt::Display for GuidanceNotDeliverable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GuidanceNotDeliverable {}

pub(crate) fn guidance_submission(
    provider: &str,
    capability_identity: Option<&str>,
    body: &str,
) -> Result<String> {
    let submitted = match provider.parse::<crate::domain::Provider>() {
        Ok(crate::domain::Provider::Claude) => {
            let compatibility = capability_identity
                .and_then(|identity| serde_json::from_str::<serde_json::Value>(identity).ok())
                .and_then(|identity| {
                    serde_json::from_value::<crate::provider_compatibility::AuthorityBinding>(
                        identity["compatibility"].clone(),
                    )
                    .ok()
                });
            crate::providers::claude::literal_guidance_submission(compatibility.as_ref(), body)
                .map_err(|error| GuidanceNotDeliverable(format!("{error:#}")))?
        }
        Ok(crate::domain::Provider::Codex) => body.to_owned(),
        Err(_) => {
            return Err(GuidanceNotDeliverable(format!(
                "guidance delivery is undefined for provider {provider}"
            ))
            .into())
        }
    };
    if submitted.len() + BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len() + 1
        > MAX_GUIDANCE_INPUT_BYTES
    {
        return Err(GuidanceNotDeliverable(
            "guidance exceeds the 64 KiB input limit in the form its provider must submit".into(),
        )
        .into());
    }
    Ok(submitted)
}

fn manager_preplan_snapshot_valid(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    attempt: &str,
    old_generation: &str,
) -> Result<bool> {
    let row: Option<(
        String,
        String,
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        i64,
        String,
        String,
        i64,
        String,
        String,
    )> = transaction
        .query_row(
            "SELECT s.manifest_json,s.source_role_generation_id,s.source_settings_revision,
                    s.workspace_id,s.workspace_hash,a.phase,a.plan_hash,a.scope_hash,
                    a.configuration_hash,t.id,t.title,t.priority,t.description,
                    t.acceptance_criteria_json,rg.config_revision,rs.config_json,
                    w.repository_identity || char(31) || w.base_revision || char(31) ||
                    w.worktree_head || char(31) || w.policy_json || char(31) || a.workflow_hash
             FROM snapshots s JOIN attempts a ON a.id=s.attempt_id JOIN tasks t ON t.id=a.task_id
             JOIN role_generations rg ON rg.id=?3 AND rg.attempt_id=a.id AND rg.role='manager'
             JOIN role_settings rs ON rs.task_id=t.id AND rs.role='manager'
               AND rs.effective_generation_id=rg.id
             JOIN workspaces w ON w.attempt_id=a.id AND w.state='ready'
             WHERE s.id=?1 AND s.attempt_id=?2 AND s.kind='manager_pre_plan' AND s.complete=1
               AND s.workspace_id=w.id AND s.workspace_hash=s.manifest_hash",
            params![snapshot_id, attempt, old_generation],
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
                    row.get(15)?,
                    row.get(16)?,
                ))
            },
        )
        .optional()?;
    let Some((
        manifest_json,
        source_generation,
        source_revision,
        workspace_id,
        _workspace_hash,
        phase,
        plan_hash,
        scope_hash,
        configuration_hash,
        task_id,
        title,
        priority,
        description,
        acceptance_json,
        current_revision,
        manager_config,
        workspace_parts,
    )) = row
    else {
        return Ok(false);
    };
    if source_generation != old_generation
        || source_revision != current_revision
        || phase != "planning"
        || plan_hash.is_some()
    {
        return Ok(false);
    }
    let mut parts = workspace_parts.split(char::from(31));
    let (
        Some(repository_identity),
        Some(base_revision),
        Some(worktree_head),
        Some(policy),
        Some(workflow_hash),
    ) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    )
    else {
        return Ok(false);
    };
    if parts.next().is_some() {
        return Ok(false);
    }
    let acceptance_criteria: serde_json::Value = serde_json::from_str(&acceptance_json)?;
    let manager_config: serde_json::Value = serde_json::from_str(&manager_config)?;
    let manager_config_hash = crate::store::json_hash(&manager_config)?;
    let workspace_policy_hash = crate::store::json_hash(&policy)?;
    let task_input_hash = crate::store::json_hash(&serde_json::json!({
        "task_id":&task_id,
        "title":&title,
        "description":&description,
        "acceptance_criteria":&acceptance_criteria,
        "priority":priority,
    }))?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_json)?;
    let expected_scope_hash = serde_json::json!(scope_hash);
    let expected_configuration_hash = serde_json::json!(configuration_hash);
    Ok(
        manifest.get("schema").and_then(serde_json::Value::as_i64) == Some(1)
            && manifest.get("kind").and_then(serde_json::Value::as_str) == Some("manager_pre_plan")
            && manifest.get("task_id").and_then(serde_json::Value::as_str)
                == Some(task_id.as_str())
            && manifest
                .get("attempt_id")
                .and_then(serde_json::Value::as_str)
                == Some(attempt)
            && manifest
                .get("task_input_hash")
                .and_then(serde_json::Value::as_str)
                == Some(task_input_hash.as_str())
            && manifest.get("attempt_scope_hash") == Some(&expected_scope_hash)
            && manifest.get("attempt_configuration_hash") == Some(&expected_configuration_hash)
            && manifest
                .get("workflow_hash")
                .and_then(serde_json::Value::as_str)
                == Some(workflow_hash)
            && manifest
                .get("manager_generation_id")
                .and_then(serde_json::Value::as_str)
                == Some(old_generation)
            && manifest
                .get("manager_settings_revision")
                .and_then(serde_json::Value::as_i64)
                == Some(current_revision)
            && manifest
                .get("manager_config_hash")
                .and_then(serde_json::Value::as_str)
                == Some(manager_config_hash.as_str())
            && manifest
                .get("workspace_id")
                .and_then(serde_json::Value::as_str)
                == Some(workspace_id.as_str())
            && manifest
                .get("workspace_repository_identity")
                .and_then(serde_json::Value::as_str)
                == Some(repository_identity)
            && manifest
                .get("workspace_base_revision")
                .and_then(serde_json::Value::as_str)
                == Some(base_revision)
            && manifest
                .get("workspace_head")
                .and_then(serde_json::Value::as_str)
                == Some(worktree_head)
            && manifest
                .get("workspace_policy_hash")
                .and_then(serde_json::Value::as_str)
                == Some(workspace_policy_hash.as_str()),
    )
}

impl RoleService {
    pub fn new(store: Store, supervisor: Supervisor) -> Self {
        Self {
            store,
            supervisor,
            runtime: None,
        }
    }

    pub fn with_runtime(
        mut self,
        hooks: crate::providers::HookAssets,
        role_socket: std::path::PathBuf,
        executable: std::path::PathBuf,
    ) -> Self {
        self.runtime = Some(RoleSwitchRuntime {
            hooks,
            role_socket,
            executable,
        });
        self
    }

    /// Creates the narrow manager handoff record used when planning has not
    /// produced a completed plan yet, then applies the ordinary immutable
    /// switch fences.  This is deliberately not a general checkpoint bypass:
    /// completed-plan manager switches continue through their existing plan
    /// lineage validation below.
    pub(crate) fn request_manager_change(
        &self,
        operation_id: &str,
        attempt: &str,
        old_generation: &str,
        settings_revision: i64,
        expected_task_version: i64,
        safe_idle: Option<&ManagerSafeIdleBoundary>,
    ) -> Result<String> {
        let checkpoint = self.manager_change_checkpoint(attempt, old_generation)?;
        self.request_switch_inner(
            operation_id,
            attempt,
            "manager",
            old_generation,
            settings_revision,
            &checkpoint,
            serde_json::json!({
                "kind":"manager_change",
                "checkpoint_kind":"manager_pre_plan_or_completed_plan",
                "old_generation_id":old_generation,
                "requested_settings_revision":settings_revision
            }),
            expected_task_version,
            SwitchOrigin::ManagerChange,
            safe_idle,
        )
    }

    fn manager_change_checkpoint(&self, attempt: &str, old_generation: &str) -> Result<String> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: (
            String,
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            String,
            i64,
            String,
            String,
            String,
            String,
            String,
            String,
        ) = transaction.query_row(
            "SELECT a.task_id,a.phase,a.plan_hash,t.title,t.description,json(t.acceptance_criteria_json),t.priority,a.scope_hash,
                    a.configuration_hash,w.id,rg.config_revision,rs.config_json,
                    w.repository_identity,w.base_revision,w.worktree_head,w.policy_json,
                    a.workflow_hash
             FROM attempts a JOIN tasks t ON t.id=a.task_id
             JOIN role_generations rg ON rg.id=?2 AND rg.attempt_id=a.id AND rg.role='manager'
             JOIN role_settings rs ON rs.task_id=t.id AND rs.role='manager'
               AND rs.effective_generation_id=rg.id
             JOIN workspaces w ON w.attempt_id=a.id AND w.state='ready'
             WHERE a.id=?1",
            params![attempt, old_generation],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                    row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?,
                    row.get(15)?, row.get(16)?,
                ))
            },
        )?;
        let (
            task_id,
            phase,
            plan_hash,
            title,
            description,
            acceptance_criteria,
            priority,
            scope_hash,
            configuration_hash,
            workspace_id,
            source_revision,
            manager_config,
            repository_identity,
            base_revision,
            worktree_head,
            workspace_policy,
            workflow_hash,
        ) = row;
        let acceptance_criteria: serde_json::Value = serde_json::from_str(&acceptance_criteria)?;
        if let Some(plan_hash) = plan_hash {
            let snapshot = transaction
                .query_row(
                    "SELECT s.id FROM snapshots s JOIN workspaces w ON w.id=s.workspace_id
                     WHERE s.attempt_id=?1 AND s.kind='plan' AND s.complete=1
                       AND s.manifest_hash=?2 AND s.source_role_generation_id=?3
                       AND s.source_settings_revision=?4 AND s.workspace_hash=s.manifest_hash
                       AND w.attempt_id=s.attempt_id AND w.state='ready'",
                    params![attempt, plan_hash, old_generation, source_revision],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| {
                    anyhow!("manager change requires the current completed-plan checkpoint")
                })?;
            transaction.commit()?;
            return Ok(snapshot);
        }
        if phase != "planning" {
            bail!("manager change without a completed plan is allowed only during planning")
        }
        let task_inputs = serde_json::json!({
            "task_id":task_id,
            "title":title,
            "description":description,
            "acceptance_criteria":acceptance_criteria,
            "priority":priority,
        });
        let task_input_hash = crate::store::json_hash(&task_inputs)?;
        let manager_config: serde_json::Value = serde_json::from_str(&manager_config)?;
        let manifest = serde_json::json!({
            "schema":1,
            "kind":"manager_pre_plan",
            "task_id":task_id,
            "attempt_id":attempt,
            "task_input_hash":task_input_hash,
            "attempt_scope_hash":scope_hash,
            "attempt_configuration_hash":configuration_hash,
            "workflow_hash":workflow_hash,
            "manager_generation_id":old_generation,
            "manager_settings_revision":source_revision,
            "manager_config_hash":crate::store::json_hash(&manager_config)?,
            "workspace_id":workspace_id,
            "workspace_repository_identity":repository_identity,
            "workspace_base_revision":base_revision,
            "workspace_head":worktree_head,
            "workspace_policy_hash":crate::store::json_hash(&workspace_policy)?,
        });
        let manifest_hash = crate::store::json_hash(&manifest)?;
        let snapshot_id = transaction
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id=?1 AND kind='manager_pre_plan' AND manifest_hash=?2",
                params![attempt, manifest_hash],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        transaction.execute(
            "INSERT OR IGNORE INTO snapshots(
                id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at,
                original_base,candidate_head,source_role_generation_id,source_settings_revision,
                workspace_id,workspace_hash
             ) VALUES(?1,?2,'manager_pre_plan',?3,?4,?5,1,?6,?3,?7,?8,?9,?10,?4)",
            params![
                snapshot_id,
                attempt,
                base_revision,
                manifest_hash,
                manifest.to_string(),
                now,
                worktree_head,
                old_generation,
                source_revision,
                workspace_id
            ],
        )?;
        transaction.commit()?;
        Ok(snapshot_id)
    }

    pub fn request_switch(
        &self,
        operation_id: &str,
        attempt: &str,
        role: &str,
        old_generation: &str,
        settings_revision: i64,
        snapshot_id: &str,
        handoff: serde_json::Value,
        expected_task_version: i64,
    ) -> Result<String> {
        self.request_switch_inner(
            operation_id,
            attempt,
            role,
            old_generation,
            settings_revision,
            snapshot_id,
            handoff,
            expected_task_version,
            SwitchOrigin::HumanReplacement,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_switch_inner(
        &self,
        operation_id: &str,
        attempt: &str,
        role: &str,
        old_generation: &str,
        settings_revision: i64,
        snapshot_id: &str,
        handoff: serde_json::Value,
        expected_task_version: i64,
        origin: SwitchOrigin,
        safe_idle: Option<&ManagerSafeIdleBoundary>,
    ) -> Result<String> {
        if !handoff.is_object() || serde_json::to_vec(&handoff)?.len() > 256 * 1024 {
            bail!("role switch requires a bounded structured handoff object")
        }
        let role_kind = role
            .parse::<crate::domain::RoleKind>()
            .map_err(|error| anyhow!(error))?;
        let role = role_kind.to_string();
        let request_hash = crate::store::json_hash(
            &serde_json::json!({"attempt":attempt,"role":role,"old":old_generation,"revision":settings_revision,"snapshot":snapshot_id,"handoff":handoff,"expected":expected_task_version}),
        )?;
        {
            let connection = self.store.lock()?;
            if let Some((stored,result))=connection.query_row("SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='role_switch'",params![operation_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).optional()?{if stored!=request_hash{bail!("switch operation ID was reused with different input")}return Ok(serde_json::from_str::<serde_json::Value>(&result)?["intent_id"].as_str().unwrap_or_default().to_owned())}
        }
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            anyhow!("role switch requires current service capability preparation")
        })?;
        let (requested, workspace): (crate::domain::RoleOverride, std::path::PathBuf) = {
            let connection = self.store.lock()?;
            let (config, workspace): (String, String) = connection.query_row(
                "SELECT rs.config_json,w.path FROM attempts a
                 JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=?2 AND rs.revision=?3
                 JOIN workspaces w ON w.attempt_id=a.id AND w.state='ready' WHERE a.id=?1",
                params![attempt, role, settings_revision],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            (serde_json::from_str(&config)?, workspace.into())
        };
        let read_denials = crate::trip::setup_target_read_denials(&self.store, attempt)?;
        let replacement = crate::providers::prepare_role_launch_with_read_denials_and_bundles(
            requested.provider,
            role_kind,
            &requested.model,
            &requested.effort,
            &workspace,
            "replacement eligibility only",
            &runtime.role_socket,
            "normalized-switch-token",
            "normalized-switch-generation",
            "normalized-switch-session",
            None,
            &runtime.hooks,
            &runtime.executable,
            &read_denials,
            &self.store.compatibility_bundles,
        )?
        .config;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored,result))=transaction.query_row("SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='role_switch'",params![operation_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).optional()?{if stored!=request_hash{bail!("switch operation ID was reused with different input")}return Ok(serde_json::from_str::<serde_json::Value>(&result)?["intent_id"].as_str().unwrap_or_default().to_owned())}
        let (task_id, attempt_status, lifecycle, archived): (String, String, String, bool) =
            transaction.query_row(
                "SELECT a.task_id,a.status,t.lifecycle,t.archived_at IS NOT NULL
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
                params![attempt],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        if archived
            || !matches!(attempt_status.as_str(), "running" | "held" | "needs_input")
            || !matches!(
                lifecycle.as_str(),
                "in_progress" | "validation" | "awaiting_review"
            )
        {
            bail!("terminal, archived, or retired attempts cannot switch role authority")
        }
        if role == "manager" && lifecycle == "awaiting_review" {
            bail!("awaiting human review accepts a requested manager revision only for a future task; it cannot switch, resume, or rerun final review")
        }
        let version_ok: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND version=?2)",
            params![task_id, expected_task_version],
            |row| row.get(0),
        )?;
        if !version_ok {
            bail!("task version is stale")
        }
        let freeze_active: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1 AND state IN ('reserved','capturing','recovery_required'))",
            params![attempt], |row| row.get(0),
        )?;
        if freeze_active {
            bail!("role switch is fenced by an active or unresolved snapshot freeze")
        }
        if role == "manager" {
            if let Some(boundary) = safe_idle {
                let current: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM attempts a
                     JOIN tasks t ON t.id=a.task_id
                     JOIN role_generations rg ON rg.id=?2
                     JOIN role_settings current_setting
                       ON current_setting.task_id=t.id AND current_setting.role='manager'
                         AND current_setting.effective_generation_id=rg.id
                     JOIN sessions s ON s.role_generation_id=rg.id
                     JOIN role_credentials credential ON credential.role_generation_id=rg.id
                       AND credential.revoked_at IS NULL
                     WHERE a.id=?1 AND a.status='running' AND t.attention='none'
                       AND t.archived_at IS NULL AND rg.attempt_id=a.id AND rg.role='manager'
                       AND rg.status='running' AND s.id=?4 AND s.status='running'
                       AND s.readiness_state='idle_candidate'
                       AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
                       AND s.id=(SELECT latest.id FROM sessions latest
                         WHERE latest.role_generation_id=rg.id
                         ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
                       AND EXISTS(SELECT 1 FROM hook_events stop
                         WHERE stop.rowid=?5 AND stop.session_id=s.id
                           AND stop.role_generation_id=rg.id AND stop.event_name='Stop'
                           AND stop.native_session_id=s.native_session_id
                           AND stop.rowid=(SELECT MAX(latest.rowid) FROM hook_events latest
                             WHERE latest.session_id=s.id)
                           AND EXISTS(SELECT 1 FROM hook_events start
                             WHERE start.session_id=s.id AND start.role_generation_id=rg.id
                               AND start.event_name='SessionStart'
                               AND start.native_session_id=s.native_session_id
                               AND start.rowid>COALESCE((SELECT resume.hook_event_boundary_rowid
                                 FROM resume_invocations resume
                                 WHERE resume.session_id=s.id
                                   AND resume.transcript_epoch=s.transcript_epoch
                                 ORDER BY resume.resume_ordinal DESC LIMIT 1),
                                 s.initial_hook_event_boundary_rowid)
                               AND start.rowid<stop.rowid)
                           AND EXISTS(SELECT 1 FROM hook_events submit
                             WHERE submit.session_id=s.id AND submit.role_generation_id=rg.id
                               AND submit.event_name='UserPromptSubmit'
                               AND submit.native_session_id=s.native_session_id
                               AND submit.rowid>(SELECT MAX(start.rowid) FROM hook_events start
                                 WHERE start.session_id=s.id AND start.role_generation_id=rg.id
                                   AND start.event_name='SessionStart'
                                   AND start.native_session_id=s.native_session_id
                                   AND start.rowid<stop.rowid)
                               AND submit.rowid<stop.rowid))
                       AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                         WHERE permission.session_id=s.id AND permission.consumed_at IS NULL
                           AND permission.delivery_state NOT IN ('expired','not_delivered')
                           AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                             WHERE native.permission_request_id=permission.id))
                       AND NOT EXISTS(SELECT 1 FROM input_leases lease
                         WHERE lease.session_id=s.id AND lease.revoked_at IS NULL
                           AND julianday(lease.expires_at)>julianday('now'))
                       AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
                         WHERE guidance.role_generation_id=rg.id
                           AND guidance.state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))
                       AND NOT EXISTS(SELECT 1 FROM controls other
                         WHERE other.attempt_id=a.id AND other.id!=?3
                           AND other.state NOT IN ('finished','cancelled','superseded','rejected'))
                       AND NOT EXISTS(SELECT 1 FROM restart_candidates restart
                         WHERE restart.attempt_id=a.id
                           AND restart.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
                       AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                         WHERE recovery.attempt_id=a.id AND recovery.state='attention_required')
                       AND NOT EXISTS(SELECT 1 FROM freeze_intents freeze
                         WHERE freeze.attempt_id=a.id
                           AND freeze.state IN ('reserved','capturing','recovery_required'))
                       AND NOT EXISTS(SELECT 1 FROM check_runs check_run
                         WHERE check_run.attempt_id=a.id
                           AND check_run.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')))",
                    params![attempt, old_generation, &boundary.control_id, &boundary.session_id,
                        boundary.stop_event_rowid],
                    |row| row.get(0),
                )?;
                if !current {
                    bail!("manager safe idle boundary changed before authority revocation")
                }
            }
        }
        crate::trip::require_attempt_ready(&transaction, attempt, None)?;
        let current: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_generations rg JOIN attempts a ON a.id=rg.attempt_id
             WHERE rg.id=?1 AND rg.attempt_id=?2 AND rg.role=?3
               AND (EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=a.task_id AND rs.role=rg.role AND rs.effective_generation_id=rg.id)
                 OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id AND lg.effective_generation_id=rg.id)))
               AND (rg.status='running' OR (rg.status='exited' AND EXISTS(
                 SELECT 1 FROM sessions s WHERE s.role_generation_id=rg.id AND s.status='exited'
                   AND json_extract(s.exit_json,'$.process_group_quiescent')=1))
                 OR (rg.role='manager' AND rg.status='stopping' AND EXISTS(
                   SELECT 1 FROM switch_intents si WHERE si.old_generation_id=rg.id
                     AND si.role='manager' AND si.state='recovery_required'))))",
            params![old_generation, attempt, role],
            |row| row.get(0),
        )?;
        if !current {
            bail!("old role generation is stale")
        }
        let (phase, candidate_hash): (String, Option<String>) = transaction.query_row(
            "SELECT phase,candidate_hash FROM attempts WHERE id=?1",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let supplemental_review = match (role.as_str(), phase.as_str()) {
            ("code_reviewer", "checks") => Some(("code", "code_review")),
            ("final_verifier", "manager_handoff") => Some(("final", "final_review")),
            _ => None,
        };
        let phase_allowed = match role.as_str() {
            "manager" => matches!(
                phase.as_str(),
                "planning"
                    | "plan_review"
                    | "awaiting_plan_approval"
                    | "awaiting_implementation_authorization"
                    | "implementation"
                    | "code_review"
                    | "checks"
                    | "final_review"
                    | "manager_handoff"
            ),
            "plan_reviewer" => phase == "plan_review",
            "implementer" => phase == "implementation",
            "code_reviewer" => phase == "code_review" || supplemental_review.is_some(),
            "explorer" => matches!(
                phase.as_str(),
                "planning" | "implementation" | "code_review" | "checks" | "final_review"
            ),
            "final_verifier" => phase == "final_review" || supplemental_review.is_some(),
            _ => false,
        };
        if !phase_allowed {
            bail!("role switch is not valid in attempt phase {phase}")
        }
        if let Some((review_kind, _)) = supplemental_review {
            let eligible: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_requests r
                   WHERE r.attempt_id=?1 AND r.review_kind=?3
                     AND r.candidate_hash=?2 AND r.delivery_state='finished' AND r.verdict='approved')
                 AND EXISTS(SELECT 1 FROM review_budgets b WHERE b.attempt_id=?1
                   AND b.review_kind=?3 AND b.spent<b.initial_allowance+b.extension_allowance)",
                params![attempt, candidate_hash, review_kind],
                |row| row.get(0),
            )?;
            if !eligible {
                bail!("supplemental {review_kind} review requires an approved exact-candidate review and explicit remaining budget")
            }
        }
        let (config,authority_fence,workspace):(String,String,String)=transaction.query_row("SELECT rs.config_json,rg.authority_generation,w.path FROM role_settings rs JOIN attempts a ON a.task_id=rs.task_id JOIN role_generations rg ON rg.id=?3 JOIN workspaces w ON w.attempt_id=a.id WHERE a.id=?1 AND rs.role=?2 AND rs.revision=?4 AND w.state='ready'",params![attempt,role,old_generation,settings_revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        let requested: crate::domain::RoleOverride = serde_json::from_str(&config)?;
        if replacement.role.to_string() != role
            || replacement.provider != requested.provider
            || replacement.model != requested.model
            || replacement.effort != requested.effort
            || replacement.cwd != std::path::PathBuf::from(workspace)
        {
            bail!("prepared replacement no longer matches the exact requested role revision and workspace")
        }
        crate::trip::current_task_profile_authority_with_bundles(
            &transaction,
            &task_id,
            replacement.role,
            settings_revision,
            &replacement,
            &self.store.compatibility_bundles,
        ).context("requested replacement is pending exact task-profile activation; current role authority remains active")?;
        let manager_preplan = if role == "manager" {
            manager_preplan_snapshot_valid(&transaction, snapshot_id, attempt, old_generation)?
        } else {
            false
        };
        let checkpoint_valid: bool = match role.as_str() {
            "manager" if manager_preplan => true,
            "manager" => transaction.query_row(
                "WITH RECURSIVE manager_checkpoint_lineage(generation_id,inherited) AS (
                   SELECT source.id,0 FROM snapshots s
                   JOIN role_generations source ON source.id=s.source_role_generation_id
                   JOIN attempts a ON a.id=s.attempt_id
                   JOIN workspaces w ON w.attempt_id=s.attempt_id
                   WHERE s.id=?1 AND s.attempt_id=?2 AND s.kind='plan' AND s.complete=1
                     AND source.attempt_id=s.attempt_id AND source.role='manager'
                     AND s.source_settings_revision=source.config_revision
                     AND s.workspace_id=w.id AND s.workspace_hash=s.manifest_hash
                     AND (a.plan_hash IS NULL OR a.plan_hash=s.manifest_hash)
                   UNION
                   SELECT inherited.id,1 FROM manager_checkpoint_lineage lineage
                   JOIN switch_intents si ON si.old_generation_id=lineage.generation_id
                   JOIN role_generations prior ON prior.id=si.old_generation_id
                   JOIN role_generations inherited ON inherited.id=si.new_generation_id
                   WHERE si.attempt_id=?2 AND si.role='manager'
                     AND si.checkpoint_snapshot_id=?1 AND si.state='dispatched'
                     AND prior.attempt_id=si.attempt_id AND prior.role=si.role
                     AND prior.status='replaced' AND si.authority_fence=prior.authority_generation
                     AND inherited.attempt_id=si.attempt_id AND inherited.role=si.role
                     AND inherited.config_revision=si.requested_settings_revision
                     AND EXISTS(SELECT 1 FROM launch_permits lp
                       WHERE lp.switch_intent_id=si.id AND lp.state='consumed'
                         AND lp.attempt_id=si.attempt_id AND lp.role=si.role
                         AND lp.settings_revision=si.requested_settings_revision)
                 ) SELECT EXISTS(SELECT 1 FROM manager_checkpoint_lineage lineage
                   WHERE generation_id=?3 AND (inherited=0 OR EXISTS(
                     SELECT 1 FROM attempts a JOIN snapshots s ON s.attempt_id=a.id
                     WHERE a.id=?2 AND s.id=?1 AND a.plan_hash=s.manifest_hash)))",
                params![snapshot_id,attempt,old_generation], |row| row.get(0),
            )?,
            "implementer" => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM snapshots s JOIN role_generations rg ON rg.id=?3 JOIN workspaces w ON w.attempt_id=s.attempt_id WHERE s.id=?1 AND s.attempt_id=?2 AND s.kind='checkpoint' AND s.complete=1 AND s.source_role_generation_id=rg.id AND s.source_settings_revision=rg.config_revision AND s.workspace_id=w.id AND s.workspace_hash=s.manifest_hash)",
                params![snapshot_id,attempt,old_generation], |row| row.get(0),
            )?,
            _ => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM snapshots s JOIN review_requests r ON r.attempt_id=s.attempt_id AND r.candidate_hash=s.manifest_hash JOIN workspaces w ON w.attempt_id=s.attempt_id WHERE s.id=?1 AND s.attempt_id=?2 AND s.kind IN ('plan','candidate') AND s.complete=1 AND s.workspace_id=w.id AND s.workspace_hash=s.manifest_hash AND r.role_generation_id=?3 AND r.delivery_state IN ('launching','delivered','ambiguous','finished'))",
                params![snapshot_id,attempt,old_generation], |row| row.get(0),
            )?,
        };
        if !checkpoint_valid {
            bail!("switch checkpoint is not the typed immutable state bound to the current role generation")
        }
        let id = uuid::Uuid::new_v4().to_string();
        transaction.execute("INSERT INTO switch_intents(id,attempt_id,role,old_generation_id,requested_settings_revision,checkpoint_snapshot_id,handoff_json,state,created_at,updated_at,operation_id,expected_task_version,config_json,authority_fence) VALUES(?1,?2,?3,?4,?5,?6,?7,'stopping_old',?8,?8,?9,?10,?11,?12)",params![id,attempt,role,old_generation,settings_revision,snapshot_id,handoff.to_string(),now,operation_id,expected_task_version,config,authority_fence])?;
        // A corrected, version-checked switch is the only operation that can
        // supersede a rejected or recovery-held old-generation handoff.
        transaction.execute(
            "UPDATE switch_intents SET state='superseded',updated_at=?1
             WHERE old_generation_id=?2 AND role=?3
               AND state IN ('recovery_required','rejected') AND id!=?4",
            params![now, old_generation, role, id],
        )?;
        if role == "implementer" {
            transaction.execute(
                "UPDATE lane_generations SET pending_settings_revision=?1,updated_at=?2
                 WHERE lane_id=(SELECT lane_id FROM role_generations WHERE id=?3) AND lane_id!='default'",
                params![settings_revision,now,old_generation],
            )?;
        }
        transaction.execute(
            "UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id=?2",
            params![now, old_generation],
        )?;
        crate::permissions::expire_generation(
            &transaction,
            old_generation,
            "role switch revoked the permission request generation",
        )?;
        transaction.execute(
            "UPDATE role_generations SET status='stopping',updated_at=?1 WHERE id=?2",
            params![now, old_generation],
        )?;
        transaction.execute(
            "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
            params![now, task_id, expected_task_version],
        )?;
        if let Some((_, target_phase)) = supplemental_review {
            transaction.execute(
                "UPDATE attempts SET phase=?1,updated_at=?2 WHERE id=?3",
                params![target_phase, now, attempt],
            )?;
        }
        if origin == SwitchOrigin::HumanReplacement {
            crate::store::retire_replaced_role_holds(
                &transaction,
                old_generation,
                &id,
                operation_id,
                &now,
            )?;
        }
        let result = serde_json::json!({"intent_id":id,"state":"stopping_old","supplemental_review":supplemental_review.map(|(kind,_)|kind)});
        transaction.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','role_switch',?2,?3,?4)",params![operation_id,request_hash,result.to_string(),now])?;
        transaction.commit()?;
        drop(connection);
        if origin == SwitchOrigin::HumanReplacement {
            let session = {
                let connection = self.store.lock()?;
                connection.query_row(
                "SELECT id FROM sessions WHERE role_generation_id=?1 AND status='running' ORDER BY created_at DESC LIMIT 1",
                params![old_generation],|row|row.get::<_,String>(0)).optional()?
            };
            if let Some(session) = session {
                self.supervisor
                    .interrupt(&session)
                    .context("switch reserved but old role interrupt failed")?;
            }
        }
        Ok(id)
    }

    pub fn finish_switch(&self, intent_id: &str) -> Result<serde_json::Value> {
        let (old, revision, attempt, role): (String, i64, String, String) = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT old_generation_id,requested_settings_revision,attempt_id,role FROM switch_intents WHERE id=?1 AND state='stopping_old'",params![intent_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?.ok_or_else(||anyhow!("switch intent is stale"))?
        };
        let active = self.supervisor.active_session_ids()?;
        let old_active = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE role_generation_id=?1 AND id IN (SELECT value FROM json_each(?2)))",params![old,serde_json::to_string(&active)?],|row|row.get::<_,bool>(0)).unwrap_or(true)
        };
        if old_active {
            bail!("old role process is active or process ownership is unknown")
        }
        let quiescent: bool = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE role_generation_id=?1 AND status='exited' AND json_extract(exit_json,'$.process_group_quiescent')=1)",params![old],|row|row.get(0))?
        };
        if !quiescent {
            bail!("old role generation lacks a verified quiescent process-group exit")
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE role_generations SET status='replaced',updated_at=?1 WHERE id=?2",
            params![now, old],
        )?;
        transaction.execute("UPDATE role_results SET consumed_at=COALESCE(consumed_at,?1) WHERE role_generation_id=?2",params![now,old])?;
        transaction.execute("UPDATE controls SET state='superseded',updated_at=?1 WHERE role_generation_id=?2 AND kind='transition_proposal' AND state='proposed'",params![now,old])?;
        let review_kind = match role.as_str() {
            "plan_reviewer" => Some("plan"),
            "code_reviewer" => Some("code"),
            "final_verifier" => Some("final"),
            _ => None,
        };
        if let Some(review_kind) = review_kind {
            transaction.execute(
                "UPDATE review_requests
                 SET delivery_state='replaced', ambiguity_state='interrupted_replacement',
                     feedback=COALESCE(feedback,'reviewer was explicitly replaced after verified process quiescence'),
                     updated_at=?1
                 WHERE attempt_id=?2 AND review_kind=?3 AND role_generation_id=?4
                   AND delivery_state IN ('delivered','ambiguous')",
                params![now, attempt, review_kind, old],
            )?;
        }
        let advanced = transaction.execute(
            "UPDATE switch_intents SET state='ready_for_dispatch',updated_at=?1 WHERE id=?2 AND state='stopping_old'",
            params![now, intent_id],
        )?;
        if advanced != 1 {
            bail!("switch intent changed while old process quiescence was verified")
        }
        transaction.commit()?;
        Ok(
            serde_json::json!({"intent_id":intent_id,"settings_revision":revision,"state":"ready_for_dispatch"}),
        )
    }

    pub fn deliver_guidance(&self, guidance_id: &str) -> Result<serde_json::Value> {
        if let Some(cancelled) = self.store.cancel_stale_report_reminder(guidance_id)? {
            return Ok(cancelled);
        }
        self.store.require_execution_unheld("guidance delivery")?;
        let (
            session,
            provider,
            capability_identity,
            body,
            reason,
            attention,
            attempt,
            transcript_epoch,
            resume_invocation,
        ): (
            String,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
            String,
            String,
            Option<String>,
        ) = {
            let connection = self.store.lock()?;
            let delivery = connection
                .query_row(
                    "SELECT s.id,s.provider,s.capability_identity_json,g.body,g.reason,t.attention,a.id,
                       s.transcript_epoch,
                       (SELECT ri.id FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1)
                     FROM guidance_messages g
                 JOIN sessions s ON s.role_generation_id=g.role_generation_id
                 JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE g.id=?1 AND g.state='queued' AND s.status='running'
                   AND s.readiness_state='idle_candidate'
                 ORDER BY s.created_at DESC LIMIT 1",
                    params![guidance_id],
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
            if delivery.is_none() && crate::store::is_report_reminder(&connection, guidance_id)? {
                let current: Option<String> = connection.query_row(
                    "SELECT json_object('action','guidance_delivery_already_claimed',
                        'guidance_id',id,'attempt_id',attempt_id,'session_id',delivery_session_id,
                        'state',state,'engine_generated',json('true'),'report_reminder',json('true'))
                     FROM guidance_messages WHERE id=?1 AND state!='queued'",
                    params![guidance_id],
                    |row| row.get(0),
                ).optional()?;
                if let Some(current) = current {
                    return Ok(serde_json::from_str(&current)?);
                }
            }
            delivery.ok_or_else(|| {
                anyhow!("guidance awaits a live role with a native Stop idle boundary")
            })?
        };
        let report_reminder = {
            let connection = self.store.lock()?;
            crate::store::is_report_reminder(&connection, guidance_id)?
        };
        let engine_generated = report_reminder
            || matches!(
                reason.as_deref(),
                Some("engine_plan_rejection_notice")
                    | Some("engine_plan_rejection_notice_blocked_hold")
            );
        if engine_generated
            && matches!(
                attention.as_str(),
                "paused" | "pause_requested" | "needs_recovery"
            )
        {
            let connection = self.store.lock()?;
            connection.execute(
                "UPDATE guidance_messages SET reason='engine_plan_rejection_notice_blocked_hold'
                 WHERE id=?1 AND state='queued'
                   AND reason IN ('engine_plan_rejection_notice','engine_plan_rejection_notice_blocked_hold')",
                params![guidance_id],
            )?;
            return Ok(serde_json::json!({
                "action":"guidance_queued",
                "guidance_id":guidance_id,
                "session_id":session,
                "attempt_id":attempt,
                "state":"queued",
                "reason":"engine_notice_blocked_by_task_hold",
                "engine_generated":true
            }));
        }
        let held_queued = |held: &crate::store::ProviderFailureHeld| {
            serde_json::json!({"action":"guidance_queued","guidance_id":guidance_id,"session_id":session,"attempt_id":attempt,
                "state":"queued","reason":"provider_failure_hold","hold_id":held.hold_id,"engine_generated":engine_generated})
        };
        let held = {
            let connection = self.store.lock()?;
            crate::store::provider_failure_hold_for_session(&connection, &session)?
        };
        if let Some(held) = held {
            return Ok(held_queued(&held));
        }
        if !self.supervisor.native_idle_ready(&session)? {
            return Ok(
                serde_json::json!({"action":"guidance_queued","guidance_id":guidance_id,"session_id":session,"attempt_id":attempt,"state":"queued","reason":"native_hook_or_tool_descendants_active","engine_generated":engine_generated}),
            );
        }
        let submitted = guidance_submission(&provider, capability_identity.as_deref(), &body)?;
        let submitted_digest = crate::store::json_hash(&submitted)?;
        let (lease, _) = match self.supervisor.acquire_input_for(
            crate::store::InputLeasePurpose::AutomatedGuidance,
            &session,
            &format!("guidance:{guidance_id}"),
            30,
        ) {
            Ok(lease) => lease,
            Err(error) => {
                if let Some(held) = crate::store::ProviderFailureHeld::in_error(&error) {
                    return Ok(held_queued(held));
                }
                return Ok(
                    serde_json::json!({"action":"guidance_queued","guidance_id":guidance_id,"session_id":session,"attempt_id":attempt,
                "state":"queued","reason":"input_owned_or_process_not_ready","detail":format!("{error:#}"),"engine_generated":engine_generated}),
                );
            }
        };
        // All work after acquisition stays in this scope so the exact lease is released on every exit.
        let mut reserved = false;
        let mut pasted = false;
        let delivery = (|| -> Result<Option<serde_json::Value>> {
            reserved = match self.store.reserve_guidance_delivery(
                guidance_id,
                &session,
                &transcript_epoch,
                resume_invocation.as_deref(),
                &submitted,
            ) {
                Ok(reserved) => reserved,
                Err(error) => {
                    return match crate::store::ProviderFailureHeld::in_error(&error) {
                        Some(held) => Ok(Some(held_queued(held))),
                        None => Err(error),
                    }
                }
            };
            if !reserved {
                let connection = self.store.lock()?;
                let held: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM guidance_messages g
                     JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
                     WHERE g.id=?1 AND g.state='queued'
                       AND g.reason IN ('engine_plan_rejection_notice','engine_plan_rejection_notice_blocked_hold')
                       AND t.attention IN ('paused','pause_requested','needs_recovery'))",
                    params![guidance_id],
                    |row| row.get(0),
                )?;
                if !held {
                    bail!("guidance delivery was concurrently claimed")
                }
                connection.execute(
                    "UPDATE guidance_messages SET reason='engine_plan_rejection_notice_blocked_hold'
                     WHERE id=?1 AND state='queued'",
                    params![guidance_id],
                )?;
                return Ok(Some(serde_json::json!({
                    "action":"guidance_queued",
                    "guidance_id":guidance_id,
                    "session_id":session,
                    "attempt_id":attempt,
                    "state":"queued",
                    "reason":"engine_notice_blocked_by_task_hold",
                    "engine_generated":true
                })));
            }
            let mut bytes = Vec::with_capacity(
                BRACKETED_PASTE_START.len() + submitted.len() + BRACKETED_PASTE_END.len(),
            );
            bytes.extend_from_slice(BRACKETED_PASTE_START);
            bytes.extend_from_slice(submitted.as_bytes());
            bytes.extend_from_slice(BRACKETED_PASTE_END);
            self.supervisor
                .write_guidance_input(&session, &lease, guidance_id, &bytes)?;
            pasted = true;
            {
                let connection = self.store.lock()?;
                let changed = connection.execute(
                    "UPDATE guidance_messages SET state='written_awaiting_submit',reason='written_through_verified_input_lease',written_at=?1
                     WHERE id=?2 AND state='delivery_reserved' AND submitted_digest=?3",
                    params![Utc::now().to_rfc3339(),guidance_id,submitted_digest],
                )?;
                if changed != 1 {
                    bail!("guidance delivery reservation changed before submit")
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            self.supervisor
                .write_guidance_input(&session, &lease, guidance_id, b"\r")?;
            Ok(None)
        })();
        let release = if report_reminder {
            self.store
                .release_report_reminder_input(&session, &lease, guidance_id)
        } else {
            self.supervisor.release_input(&session, &lease)
        };
        let delivery = match delivery {
            Err(error)
                if report_reminder
                    && !pasted
                    && error.is::<crate::store::ReportReminderRefused>() =>
            {
                self.store
                    .cancel_report_reminder_before_write(guidance_id, &error.to_string())
                    .map(Some)
            }
            Err(error) if reserved => {
                let recorded = self.store.lock().and_then(|connection| {
                    connection.execute(
                        "UPDATE guidance_messages SET state='delivery_unknown',reason=?1 WHERE id=?2 AND state IN ('delivery_reserved','written_awaiting_submit')",
                        params![format!("{error:#}"),guidance_id],
                    )?;
                    Ok(())
                });
                Err(match recorded {
                    Ok(()) => error.context("guidance write outcome is not confirmed"),
                    Err(recording_error) => error.context(format!(
                        "guidance failure-state recording also failed: {recording_error:#}"
                    )),
                })
            }
            delivery => delivery,
        };
        match (delivery, release) {
            (Ok(Some(queued)), Ok(())) => Ok(queued),
            (Ok(None), Ok(())) => {
                let state: String = self.store.lock()?.query_row(
                    "SELECT state FROM guidance_messages WHERE id=?1",
                    params![guidance_id],
                    |row| row.get(0),
                )?;
                let acknowledged = state == "acknowledged";
                Ok(
                    serde_json::json!({"action":"guidance_delivered","guidance_id":guidance_id,"session_id":session,"attempt_id":attempt,"state":state,"acknowledged":acknowledged,"engine_generated":engine_generated,"report_reminder":report_reminder}),
                )
            }
            (Ok(_), Err(release_error)) => {
                Err(release_error.context("failed to release the guidance input lease"))
            }
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(release_error)) => Err(error.context(format!(
                "the guidance input lease was also not released: {release_error:#}"
            ))),
        }
    }

    pub fn deliver_next_for_session(&self, session_id: &str) -> Result<Option<serde_json::Value>> {
        let guidance: Option<String> = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT g.id FROM guidance_messages g JOIN sessions s ON s.role_generation_id=g.role_generation_id
                 JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE s.id=?1 AND s.readiness_state='idle_candidate' AND g.state='queued'
                 ORDER BY CASE WHEN g.reason IN ('engine_plan_rejection_notice','engine_plan_rejection_notice_blocked_hold')
                   AND t.attention IN ('paused','pause_requested','needs_recovery') THEN 1 ELSE 0 END,
                   g.created_at LIMIT 1",
                params![session_id],|row|row.get(0),
            ).optional()?
        };
        guidance.map(|id| self.deliver_guidance(&id)).transpose()
    }

    pub fn retry_ready_guidance(&self) -> Result<Vec<serde_json::Value>> {
        let sessions = {
            let connection = self.store.lock()?;
            let mut statement=connection.prepare(&format!("SELECT DISTINCT s.id,rg.attempt_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN guidance_messages g ON g.role_generation_id=s.role_generation_id WHERE s.status='running' AND s.readiness_state='idle_candidate' AND g.state='queued' AND {} AND NOT {}", crate::coordinator::coordinator_hold_absent("rg.attempt_id"), crate::store::provider_failure_hold_restricts("rg.attempt_id", "rg.role", "rg.lane_id")))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut outcomes = Vec::new();
        for (session, attempt) in sessions {
            let outcome = self.deliver_next_for_session(&session).map_err(|error| {
                let effect = if error.is::<GuidanceNotDeliverable>() {
                    crate::coordinator::StepEffect::None
                } else {
                    crate::coordinator::StepEffect::Possible
                };
                crate::coordinator::subject_failure(
                    error,
                    &attempt,
                    crate::coordinator::SubjectStep::GuidanceDelivery,
                    effect,
                    serde_json::json!({"session_id":session}),
                )
            })?;
            if let Some(outcome) = outcome {
                let delivered =
                    outcome.get("state").and_then(|state| state.as_str()) != Some("queued");
                outcomes.push(outcome);
                if delivered {
                    break;
                }
            }
        }
        Ok(outcomes)
    }
}
