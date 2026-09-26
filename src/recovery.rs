use crate::domain::{
    DecisionActionBinding, DecisionControlPolicy, DecisionDisposition, DecisionEvidenceState,
    DecisionExplanation, DecisionNextAction, DecisionObservedRevision, DecisionOwner,
    DecisionOwnership, DecisionPrerequisite, DecisionSubject, ProcessGenerationAnchor,
    ProcessIdentity, RestartBatchMembership, RestartCandidateResult, RestartPreview,
    RestartPreviewClassification, RestartPreviewSession, RestartPreviewSnapshot,
    DECISION_SCHEMA_V1,
};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestartGate {
    CandidateUnavailable,
    NativeBindingUnavailable,
    NativeHistoryMissing,
    CurrentCapabilityMissing,
    IsolatedValidation,
    CurrentAttemptMissing,
    CurrentGenerationMissing,
    CurrentConfigurationMissing,
    ReviewDeliveryMissing,
    QueueHeld,
    ControlHeld,
    AttentionHeld,
    AttemptNotRunning,
    WorkflowPhaseUnsupported,
    TerminalTask,
    FreshOnlyRole,
    MetadataMalformed,
    ReplacementLimitReached,
}

impl RestartGate {
    pub(crate) fn reason_code(self) -> &'static str {
        match self {
            Self::CandidateUnavailable => "restart.candidate_unavailable",
            Self::NativeBindingUnavailable => "restart.native_binding_unavailable",
            Self::NativeHistoryMissing => "restart.native_history_missing",
            Self::CurrentCapabilityMissing => "restart.current_capability_missing",
            Self::IsolatedValidation => "restart.isolated_validation",
            Self::CurrentAttemptMissing => "restart.current_attempt_missing",
            Self::CurrentGenerationMissing => "restart.current_generation_missing",
            Self::CurrentConfigurationMissing => "restart.current_configuration_missing",
            Self::ReviewDeliveryMissing => "restart.review_delivery_missing",
            Self::QueueHeld => "restart.project_queue_held",
            Self::ControlHeld => "restart.control_held",
            Self::AttentionHeld => "restart.human_hold",
            Self::AttemptNotRunning => "restart.attempt_not_running",
            Self::WorkflowPhaseUnsupported => "restart.workflow_phase_unsupported",
            Self::TerminalTask => "restart.task_complete",
            Self::FreshOnlyRole => "restart.fresh_only_role",
            Self::MetadataMalformed => "restart.metadata_malformed",
            Self::ReplacementLimitReached => "restart.replacement_limit_reached",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::CandidateUnavailable => "candidate is not parked for this resume mode",
            Self::NativeBindingUnavailable => {
                "verified exited state or captured restart intent is unavailable"
            }
            Self::NativeHistoryMissing => "same-generation native history is unavailable",
            Self::CurrentCapabilityMissing => {
                "current Supported frozen capability identity is unavailable"
            }
            Self::IsolatedValidation => "isolated validation sessions are never restored",
            Self::CurrentAttemptMissing => {
                "task attempt is terminal, stale, archived, or no longer parked"
            }
            Self::CurrentGenerationMissing => "role generation was replaced or is stale",
            Self::CurrentConfigurationMissing => "saved policy or configuration is stale",
            Self::ReviewDeliveryMissing => {
                "review is complete, replaced, stale, or not durably delivered"
            }
            Self::QueueHeld => "project queue does not permit resume",
            Self::ControlHeld => "a manager stop or manager change still holds this attempt",
            Self::AttentionHeld => {
                "task is paused, input-required, terminal, or no longer parked for restoration"
            }
            Self::AttemptNotRunning => "attempt is not previously running",
            Self::WorkflowPhaseUnsupported => "workflow phase is not resumable",
            Self::TerminalTask => "task is awaiting review, terminal, cancelled, or archived",
            Self::FreshOnlyRole => "final verifier sessions are always fresh and cannot resume",
            Self::MetadataMalformed => {
                "durable restart accounting is malformed and requires explicit recovery"
            }
            Self::ReplacementLimitReached => {
                "three proven replacement failures exhausted exact automatic retry authority"
            }
        }
    }

    fn owner(self) -> DecisionOwner {
        match self {
            Self::NativeHistoryMissing => DecisionOwner::Provider,
            Self::CurrentCapabilityMissing | Self::CurrentConfigurationMissing => {
                DecisionOwner::Human
            }
            Self::QueueHeld | Self::ControlHeld | Self::AttentionHeld => DecisionOwner::Human,
            Self::CandidateUnavailable
            | Self::NativeBindingUnavailable
            | Self::IsolatedValidation
            | Self::CurrentAttemptMissing
            | Self::CurrentGenerationMissing
            | Self::ReviewDeliveryMissing
            | Self::AttemptNotRunning
            | Self::WorkflowPhaseUnsupported
            | Self::TerminalTask
            | Self::FreshOnlyRole
            | Self::MetadataMalformed
            | Self::ReplacementLimitReached => DecisionOwner::Service,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RestartSessionFacts {
    pub(crate) session_id: String,
    pub(crate) role_generation_id: String,
    pub(crate) attempt_id: String,
    pub(crate) task_id: String,
    project_id: String,
    session_status: String,
    launch_state: String,
    desired_running: bool,
    validation_cell: Option<String>,
    native_history: bool,
    current_capability: bool,
    capability_identity: bool,
    role: String,
    generation_status: String,
    attempt_status: String,
    phase: String,
    lifecycle: String,
    attention: String,
    archived: bool,
    latest_attempt: bool,
    latest_generation: bool,
    current_configuration: bool,
    preparation_review_delivered: bool,
    admission_review_authorized: bool,
    queue_open: bool,
    controls_clear: bool,
    active_restored_peer: bool,
    pub(crate) task_version: i64,
    project_version: i64,
    configuration_revision: i64,
    selected_checks_revision: i64,
    plan_hash: Option<String>,
    candidate_hash: Option<String>,
    candidate_state: Option<String>,
    candidate_reason: Option<String>,
    candidate_source: Option<String>,
    candidate_result: Option<RestartCandidateResult>,
    candidate_metadata_error: Option<String>,
    unresolved_recovery: bool,
    process_identity_json: Option<String>,
    recovery_process_group_id: Option<i32>,
    recovery_anchor_json: Option<String>,
    launch_boot_identity: Option<String>,
    process_group_quiescent: bool,
    recorded_processes: Vec<(u32, String, i32)>,
    admission_proof: Option<serde_json::Value>,
    trip_readiness_error: Option<String>,
}

impl RestartSessionFacts {
    fn considered_for_preparation(&self) -> bool {
        (self.desired_running
            || matches!(self.session_status.as_str(), "launch_reserved" | "running"))
            && self.session_status != "interrupt_requested"
    }

    fn terminal(&self) -> bool {
        self.archived
            || matches!(
                self.lifecycle.as_str(),
                "awaiting_review" | "done" | "cancelled"
            )
    }

    fn frozen_identity_gate(&self) -> Option<RestartGate> {
        if !self.native_history {
            Some(RestartGate::NativeHistoryMissing)
        } else if !self.current_capability || !self.capability_identity {
            Some(RestartGate::CurrentCapabilityMissing)
        } else {
            None
        }
    }

    fn preparation_gate(&self) -> Option<RestartGate> {
        if self.validation_cell.is_some() && self.attempt_status == "capability_validation" {
            Some(RestartGate::IsolatedValidation)
        } else if self.role == "final_verifier" || self.role == "final_reviewer" {
            Some(RestartGate::FreshOnlyRole)
        } else if self.candidate_metadata_error.is_some() {
            Some(RestartGate::MetadataMalformed)
        } else if let Some(gate) = self.frozen_identity_gate() {
            Some(gate)
        } else if !self.latest_attempt {
            Some(RestartGate::CurrentAttemptMissing)
        } else if self.terminal() {
            Some(RestartGate::TerminalTask)
        } else if !matches!(self.attention.as_str(), "none" | "restart_parked") {
            Some(RestartGate::AttentionHeld)
        } else if !matches!(self.attempt_status.as_str(), "running" | "restart_parked") {
            Some(RestartGate::AttemptNotRunning)
        } else if !matches!(
            self.generation_status.as_str(),
            "running" | "exited" | "launch_reserved" | "stopping"
        ) || !self.latest_generation
        {
            Some(RestartGate::CurrentGenerationMissing)
        } else if !self.current_configuration {
            Some(RestartGate::CurrentConfigurationMissing)
        } else if !self.preparation_review_delivered {
            Some(RestartGate::ReviewDeliveryMissing)
        } else if !matches!(
            self.phase.as_str(),
            "planning"
                | "plan_review"
                | "awaiting_plan_approval"
                | "awaiting_implementation_authorization"
                | "implementation"
                | "code_review"
                | "checks"
                | "final_review"
                | "manager_handoff"
        ) {
            Some(RestartGate::WorkflowPhaseUnsupported)
        } else {
            None
        }
    }

    pub(crate) fn admission_gate(&self, automatic: bool) -> Option<RestartGate> {
        let Some(state) = self.candidate_state.as_deref() else {
            return Some(RestartGate::CandidateUnavailable);
        };
        if !matches!(state, "parked" | "queued_capacity" | "failed")
            || (automatic && state == "failed")
        {
            return Some(RestartGate::CandidateUnavailable);
        }
        if self.role == "final_verifier" || self.role == "final_reviewer" {
            return Some(RestartGate::FreshOnlyRole);
        }
        if self.candidate_metadata_error.is_some() {
            return Some(RestartGate::MetadataMalformed);
        }
        if self
            .candidate_result
            .as_ref()
            .is_some_and(|result| result.restart.replacement_failures >= 3)
        {
            return Some(RestartGate::ReplacementLimitReached);
        }
        if self.session_status != "exited" || !self.desired_running {
            return Some(RestartGate::NativeBindingUnavailable);
        }
        if let Some(gate) = self.frozen_identity_gate() {
            return Some(gate);
        }
        if self.validation_cell.is_some() {
            return Some(RestartGate::IsolatedValidation);
        }
        if !self.latest_attempt
            || !(matches!(
                self.attempt_status.as_str(),
                "restart_parked" | "needs_input"
            ) || (self.attempt_status == "running" && self.active_restored_peer))
            || !matches!(self.lifecycle.as_str(), "in_progress" | "validation")
            || self.archived
        {
            return Some(RestartGate::CurrentAttemptMissing);
        }
        if self.generation_status != "exited" || !self.latest_generation {
            return Some(RestartGate::CurrentGenerationMissing);
        }
        if !self.current_configuration {
            return Some(RestartGate::CurrentConfigurationMissing);
        }
        if !self.queue_open {
            return Some(RestartGate::QueueHeld);
        }
        if !self.admission_review_authorized {
            return Some(RestartGate::ReviewDeliveryMissing);
        }
        if !self.controls_clear {
            return Some(RestartGate::ControlHeld);
        }
        if self.attention != "restart_parked"
            && !(self.active_restored_peer && self.attention == "none")
            && !(!automatic && state == "failed" && self.attention == "resume_failed")
        {
            return Some(RestartGate::AttentionHeld);
        }
        None
    }

    fn potential_admission_gate(&self) -> Option<RestartGate> {
        if !matches!(
            self.candidate_state.as_deref(),
            Some("pending_reconciliation" | "parked" | "queued_capacity" | "failed")
        ) {
            return Some(RestartGate::CandidateUnavailable);
        }
        if self.role == "final_verifier" || self.role == "final_reviewer" {
            return Some(RestartGate::FreshOnlyRole);
        }
        if self.candidate_metadata_error.is_some() {
            return Some(RestartGate::MetadataMalformed);
        }
        if self
            .candidate_result
            .as_ref()
            .is_some_and(|result| result.restart.replacement_failures >= 3)
        {
            return Some(RestartGate::ReplacementLimitReached);
        }
        if !matches!(self.session_status.as_str(), "running" | "exited") || !self.desired_running {
            return Some(RestartGate::NativeBindingUnavailable);
        }
        if let Some(gate) = self.frozen_identity_gate() {
            return Some(gate);
        }
        if self.validation_cell.is_some() {
            return Some(RestartGate::IsolatedValidation);
        }
        if !self.latest_attempt
            || !(matches!(
                self.attempt_status.as_str(),
                "restart_parked" | "needs_input"
            ) || (self.attempt_status == "running" && self.active_restored_peer))
            || !matches!(self.lifecycle.as_str(), "in_progress" | "validation")
            || self.archived
        {
            return Some(RestartGate::CurrentAttemptMissing);
        }
        if !matches!(
            self.generation_status.as_str(),
            "running" | "stopping" | "exited"
        ) || !self.latest_generation
        {
            return Some(RestartGate::CurrentGenerationMissing);
        }
        if !self.current_configuration {
            return Some(RestartGate::CurrentConfigurationMissing);
        }
        if !self.queue_open {
            return Some(RestartGate::QueueHeld);
        }
        if !self.admission_review_authorized {
            return Some(RestartGate::ReviewDeliveryMissing);
        }
        if !self.controls_clear {
            return Some(RestartGate::ControlHeld);
        }
        if self.attention != "restart_parked"
            && !(self.active_restored_peer && self.attention == "none")
            && !(self.candidate_state.as_deref() == Some("failed")
                && self.attention == "resume_failed")
        {
            return Some(RestartGate::AttentionHeld);
        }
        None
    }
}

pub(crate) fn restart_session_facts(
    connection: &Connection,
    session_id: Option<&str>,
) -> Result<Vec<RestartSessionFacts>> {
    let mut statement = connection.prepare(
        "SELECT s.id,rg.id,a.id,t.id,p.id,s.status,s.launch_state,s.desired_running,
                s.validation_cell,s.native_session_id IS NOT NULL AND s.native_session_id!='',
                EXISTS(SELECT 1 FROM capabilities current_capability
                  WHERE current_capability.rowid=(SELECT latest.rowid FROM capabilities latest
                    WHERE latest.provider=s.provider AND latest.executable_version=s.executable_version
                      AND latest.role=rg.role AND latest.mode='interactive_pty'
                    ORDER BY latest.checked_at DESC,latest.rowid DESC LIMIT 1)
                    AND current_capability.config_hash=s.capability_key
                    AND current_capability.status='supported' AND current_capability.proof_json!='{}'),
                s.capability_identity_json IS NOT NULL,rg.role,rg.status,a.status,a.phase,t.lifecycle,
                t.attention,t.archived_at IS NOT NULL,
                a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id
                      ORDER BY latest.created_at DESC LIMIT 1),
                NOT EXISTS(SELECT 1 FROM role_generations newer WHERE newer.attempt_id=a.id
                  AND newer.role=rg.role AND newer.lane_id=rg.lane_id AND newer.generation>rg.generation),
                EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=t.id AND rs.role=rg.role
                  AND rs.revision=rg.config_revision AND rs.effective_generation_id=rg.id)
                  OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                    SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id
                      AND lg.effective_generation_id=rg.id)),
                CASE WHEN rg.role IN ('explorer','plan_reviewer','code_reviewer','final_verifier')
                  THEN EXISTS(SELECT 1 FROM review_requests review WHERE review.session_id=s.id
                    AND review.role_generation_id=rg.id AND review.delivery_state='delivered') ELSE 1 END,
                rg.role NOT IN ('explorer','plan_reviewer','code_reviewer','final_verifier')
                  OR rg.role='explorer' OR EXISTS(SELECT 1 FROM review_requests review
                    WHERE review.session_id=s.id AND review.role_generation_id=rg.id
                      AND review.delivery_state='delivered'),
                p.queue_paused=0,
                NOT EXISTS(SELECT 1 FROM controls control WHERE control.attempt_id=a.id
                  AND control.kind IN ('manager_stop','manager_change')
                  AND control.state NOT IN ('finished','cancelled','superseded','rejected')),
                EXISTS(SELECT 1 FROM restart_candidates admitted JOIN sessions running
                  ON running.id=admitted.session_id WHERE admitted.attempt_id=a.id
                  AND admitted.state='resumed' AND running.status='running'),
                t.version,p.version,rg.config_revision,a.selected_checks_revision,a.plan_hash,
                a.candidate_hash,restart.state,restart.reason,restart.source,restart.result_json,
                EXISTS(SELECT 1 FROM recovery_records recovery WHERE recovery.session_id=s.id
                  AND recovery.state='attention_required'),
                s.process_identity_json,s.recovery_process_group_id,s.recovery_anchor_json,
                s.launch_boot_identity,COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)
         FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN projects p ON p.id=t.project_id
         LEFT JOIN restart_candidates restart ON restart.session_id=s.id
         WHERE (?1 IS NULL OR s.id=?1) ORDER BY s.created_at,s.id",
    )?;
    let rows = statement
        .query_map(params![session_id], |row| {
            let session_id = row.get::<_, String>(0)?;
            let candidate_state = row.get::<_, Option<String>>(33)?;
            let candidate_result_raw = row.get::<_, Option<String>>(36)?;
            let parsed_candidate = candidate_result_raw.as_deref().map(|raw| {
                RestartCandidateResult::parse(raw).and_then(|result| {
                    if let Some(state) = candidate_state.as_deref() {
                        result.validate_candidate_state(state, &session_id)?;
                    }
                    Ok(result)
                })
            });
            let (candidate_result, candidate_metadata_error) = match parsed_candidate {
                Some(Ok(result)) => (Some(result), None),
                Some(Err(error)) => (None, Some(format!("{error:#}"))),
                None => (None, None),
            };
            Ok(RestartSessionFacts {
                session_id,
                role_generation_id: row.get(1)?,
                attempt_id: row.get(2)?,
                task_id: row.get(3)?,
                project_id: row.get(4)?,
                session_status: row.get(5)?,
                launch_state: row.get(6)?,
                desired_running: row.get(7)?,
                validation_cell: row.get(8)?,
                native_history: row.get(9)?,
                current_capability: row.get(10)?,
                capability_identity: row.get(11)?,
                role: row.get(12)?,
                generation_status: row.get(13)?,
                attempt_status: row.get(14)?,
                phase: row.get(15)?,
                lifecycle: row.get(16)?,
                attention: row.get(17)?,
                archived: row.get(18)?,
                latest_attempt: row.get(19)?,
                latest_generation: row.get(20)?,
                current_configuration: row.get(21)?,
                preparation_review_delivered: row.get(22)?,
                admission_review_authorized: row.get(23)?,
                queue_open: row.get(24)?,
                controls_clear: row.get(25)?,
                active_restored_peer: row.get(26)?,
                task_version: row.get(27)?,
                project_version: row.get(28)?,
                configuration_revision: row.get(29)?,
                selected_checks_revision: row.get(30)?,
                plan_hash: row.get(31)?,
                candidate_hash: row.get(32)?,
                candidate_state,
                candidate_reason: row.get(34)?,
                candidate_source: row.get(35)?,
                candidate_result,
                candidate_metadata_error,
                unresolved_recovery: row.get(37)?,
                process_identity_json: row.get(38)?,
                recovery_process_group_id: row.get(39)?,
                recovery_anchor_json: row.get(40)?,
                launch_boot_identity: row.get(41)?,
                process_group_quiescent: row.get(42)?,
                recorded_processes: Vec::new(),
                admission_proof: None,
                trip_readiness_error: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn attach_recorded_processes(
    connection: &Connection,
    facts: &mut [RestartSessionFacts],
    session_id: Option<&str>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT session_id,pid,native_start_marker,process_group_id
         FROM session_processes WHERE (?1 IS NULL OR session_id=?1)
         ORDER BY session_id,pid",
    )?;
    let rows = statement
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_session: HashMap<String, Vec<(u32, String, i32)>> = HashMap::new();
    for (session_id, pid, start, group) in rows {
        by_session
            .entry(session_id)
            .or_default()
            .push((pid, start, group));
    }
    for item in facts {
        item.recorded_processes = by_session.remove(&item.session_id).unwrap_or_default();
    }
    Ok(())
}

pub fn capture_desired_running_before_drain(store: &Store) -> Result<Vec<String>> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "UPDATE sessions SET desired_running=0 WHERE status IN ('launch_reserved','running')",
        [],
    )?;
    let mut statement = transaction
        .prepare("SELECT id FROM sessions WHERE status='running' ORDER BY created_at")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for id in &ids {
        transaction.execute(
            "UPDATE sessions SET desired_running=1,updated_at=?1 WHERE id=?2",
            params![now, id],
        )?;
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','restart.desired_running.captured','instance','local',?3,?4)",
        params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),serde_json::json!({"sessions":ids}).to_string(),now],
    )?;
    transaction.commit()?;
    Ok(ids)
}

pub fn prepare_restart_candidates(store: &Store) -> Result<Vec<serde_json::Value>> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction()?;
    let rows = restart_session_facts(&transaction, None)?
        .into_iter()
        .filter(RestartSessionFacts::considered_for_preparation)
        .collect::<Vec<_>>();
    let mut results = Vec::new();
    for facts in rows {
        if let Some(error) = facts.candidate_metadata_error.as_deref() {
            bail!(
                "restart candidate {} has invalid durable accounting: {error}",
                facts.session_id
            )
        }
        if facts.candidate_state.as_deref().is_some_and(|state| {
            !matches!(
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
            )
        }) {
            bail!(
                "restart candidate {} has an unsupported durable state",
                facts.session_id
            )
        }
        if facts.candidate_state.as_deref() == Some("admitting") {
            results.push(serde_json::json!({
                "session_id":facts.session_id,
                "attempt_id":facts.attempt_id,
                "task_id":facts.task_id,
                "source":facts.candidate_source,
                "state":"admitting"
            }));
            continue;
        }
        let source = if facts.desired_running {
            "planned_shutdown"
        } else {
            "unclean_shutdown"
        };
        let reason = facts.preparation_gate().map(RestartGate::message);
        let state = if reason.is_some() {
            "skipped"
        } else if facts.session_status == "exited" {
            "parked"
        } else {
            "pending_reconciliation"
        };
        let state = match (facts.candidate_state.as_deref(), state) {
            (Some("failed"), "parked") => "failed",
            (Some("queued_capacity"), "parked") => "queued_capacity",
            (_, state) => state,
        };
        let persisted_reason = if matches!(state, "failed" | "queued_capacity") {
            facts.candidate_reason.clone().ok_or_else(|| {
                anyhow::anyhow!("restart candidate state {state} has no persisted reason")
            })?
        } else {
            reason
                .unwrap_or("eligible exact native binding is parked")
                .to_owned()
        };
        let mut result = facts
            .candidate_result
            .clone()
            .unwrap_or(RestartCandidateResult::parse("{}")?);
        if state != "queued_capacity" && result.restart.active_batch().is_some() {
            result.restart.terminalize_batch();
        }
        result.set("role", serde_json::Value::String(facts.role.clone()));
        result.set(
            "prior_status",
            serde_json::Value::String(facts.session_status.clone()),
        );
        transaction.execute(
            "INSERT INTO restart_candidates(session_id,attempt_id,task_id,source,state,reason,result_json,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)
             ON CONFLICT(session_id) DO UPDATE SET source=excluded.source,state=excluded.state,reason=excluded.reason,
               result_json=excluded.result_json,updated_at=excluded.updated_at",
            params![&facts.session_id,&facts.attempt_id,&facts.task_id,source,state,persisted_reason,result.encode()?,now],
        )?;
        if facts.validation_cell.is_none()
            && facts.attempt_status == "running"
            && facts.attention == "none"
            && !facts.archived
            && !matches!(
                facts.lifecycle.as_str(),
                "awaiting_review" | "done" | "cancelled"
            )
        {
            transaction.execute(
                "UPDATE attempts SET status='restart_parked',updated_at=?1 WHERE id=?2",
                params![now, &facts.attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='restart_parked',updated_at=?1 WHERE id=?2",
                params![now, &facts.task_id],
            )?;
        }
        if state == "skipped" {
            transaction.execute(
                "UPDATE sessions SET desired_running=0 WHERE id=?1",
                params![&facts.session_id],
            )?;
        } else if state != "parked" {
            transaction.execute(
                "UPDATE sessions SET desired_running=1 WHERE id=?1",
                params![&facts.session_id],
            )?;
        }
        results.push(serde_json::json!({"session_id":facts.session_id,"attempt_id":facts.attempt_id,"task_id":facts.task_id,"source":source,"state":state}));
    }
    transaction.commit()?;
    Ok(results)
}

pub fn reconcile_restart_candidates(store: &Store) -> Result<Vec<serde_json::Value>> {
    let pending = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT session_id,result_json FROM restart_candidates
             WHERE state='pending_reconciliation' ORDER BY created_at,session_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut results = Vec::new();
    for (session, expected_result) in pending {
        match verify_session_quiescent(store, &session) {
            Ok(verified) => {
                let now = Utc::now().to_rfc3339();
                let mut connection = store.lock()?;
                let tx = connection.transaction()?;
                let current: Option<(String, String, String)> = tx
                    .query_row(
                        "SELECT attempt_id,task_id,result_json FROM restart_candidates
                         WHERE session_id=?1 AND state='pending_reconciliation' AND result_json=?2",
                        params![session, expected_result],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                let Some((attempt, task, result_json)) = current else {
                    tx.rollback()?;
                    continue;
                };
                let mut candidate_result = RestartCandidateResult::parse(&result_json)?;
                candidate_result.validate_candidate_state("pending_reconciliation", &session)?;
                let queued = candidate_result.restart.active_batch().is_some();
                if let Some(batch) = candidate_result.restart.batch.as_mut() {
                    if batch.membership.active() {
                        batch.membership = RestartBatchMembership::Queued;
                    }
                }
                candidate_result.restart.admission = None;
                candidate_result.set("verification", verified.clone());
                tx.execute("UPDATE recovery_records SET state='resolved_quiescent',resolved_at=?1,updated_at=?1,detail_json=json_set(detail_json,'$.machine_verification',json(?2)) WHERE session_id=?3 AND state='attention_required'",params![now,verified.to_string(),session])?;
                tx.execute("UPDATE sessions SET status='exited',launch_state='finished',readiness_state='unknown',exit_json=?1,interrupt_requested_at=NULL,updated_at=?2 WHERE id=?3",params![serde_json::json!({"process_group_quiescent":true,"source":"restart_machine_reconciliation","verification":verified}).to_string(),now,session])?;
                tx.execute("UPDATE role_generations SET status='exited',updated_at=?1 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2) AND status NOT IN ('replaced','revoked')",params![now,session])?;
                tx.execute("UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1) WHERE role_generation_id=(SELECT role_generation_id FROM sessions WHERE id=?2)",params![now,session])?;
                tx.execute(
                    "UPDATE attempts SET status='restart_parked',updated_at=?1 WHERE id=?2",
                    params![now, attempt],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention='restart_parked',updated_at=?1 WHERE id=?2",
                    params![now, task],
                )?;
                tx.execute(
                    "UPDATE restart_candidates SET state=?1,reason=?2,result_json=?3,updated_at=?4
                     WHERE session_id=?5 AND state='pending_reconciliation' AND result_json=?6",
                    params![
                        if queued { "queued_capacity" } else { "parked" },
                        if queued {
                            "machine-verified quiescent; durable batch member returned to its queue"
                        } else {
                            "machine-verified quiescent; exact native resume is available"
                        },
                        candidate_result.encode()?,
                        now,
                        session,
                        expected_result
                    ],
                )?;
                tx.commit()?;
                results.push(serde_json::json!({"session_id":session,"state":if queued{"queued_capacity"}else{"parked"},"verification":verified}));
            }
            Err(error) => {
                let reason = format!("{error:#}");
                let mut connection = store.lock()?;
                let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let result_json: Option<String> = tx
                    .query_row(
                        "SELECT result_json FROM restart_candidates
                         WHERE session_id=?1 AND state='pending_reconciliation' AND result_json=?2",
                        params![session, expected_result],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(result_json) = result_json else {
                    tx.rollback()?;
                    continue;
                };
                let mut candidate_result = RestartCandidateResult::parse(&result_json)?;
                candidate_result.validate_candidate_state("pending_reconciliation", &session)?;
                candidate_result.restart.terminalize_batch();
                candidate_result.set(
                    "verification",
                    serde_json::Value::String("uncertain_or_occupied".to_owned()),
                );
                tx.execute(
                    "UPDATE restart_candidates SET state='blocked',reason=?1,result_json=?2,updated_at=?3
                     WHERE session_id=?4 AND state='pending_reconciliation' AND result_json=?5",
                    params![
                        reason,
                        candidate_result.encode()?,
                        Utc::now().to_rfc3339(),
                        session,
                        expected_result
                    ],
                )?;
                tx.commit()?;
                results.push(
                    serde_json::json!({"session_id":session,"state":"blocked","reason":reason}),
                );
            }
        }
    }
    Ok(results)
}

pub fn reconcile_prior_boot(store: &Store) -> Result<Vec<serde_json::Value>> {
    let mut results = store.release_unconsumed_launch_permits(
        None,
        "service restarted before launch permit was consumed",
    )?;
    let mut connection = store.lock()?;
    let mut statement=connection.prepare("SELECT id,process_identity_json,launch_state FROM sessions WHERE status IN ('launch_reserved','running')")?;
    let sessions = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let prior_launch_states: std::collections::HashMap<_, _> = sessions
        .iter()
        .map(|(id, _, launch_state)| (id.clone(), launch_state.clone()))
        .collect();
    for (id, identity, launch_state) in sessions {
        if launch_state == "reserved" {
            let now = Utc::now().to_rfc3339();
            let attempt: String = connection.query_row(
                "SELECT attempt_id FROM role_generations WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?1)",
                params![id],
                |row| row.get(0),
            )?;
            let resume: Option<(String,Option<String>,Option<String>,Option<String>,Option<String>,Option<i64>,Option<i32>)> = connection.query_row(
                "SELECT id,prior_exit_json,prior_transcript_epoch,prior_launch_boot_identity,prior_recovery_anchor_json,prior_recovery_root_pid,prior_recovery_process_group_id FROM resume_invocations WHERE session_id=?1
                 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?1) AND state='reserved'",
                params![id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
            ).optional()?;
            if let Some((
                _resume_id,
                prior_exit,
                Some(prior_epoch),
                prior_boot,
                prior_anchor,
                prior_root,
                prior_group,
            )) = resume
            {
                connection.execute(
                    "UPDATE resume_invocations SET state='proven_nondelivery',error='service restarted before native resume spawn began',updated_at=?1
                     WHERE session_id=?2 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?2) AND state='reserved'",
                    params![now,id],
                )?;
                connection.execute(
                    "UPDATE sessions SET status='exited',launch_state='finished',launch_error='resume proven not delivered before service restart',exit_json=?1,
                       transcript_epoch=?2,launch_boot_identity=?3,recovery_anchor_json=?4,recovery_root_pid=?5,recovery_process_group_id=?6,updated_at=?7 WHERE id=?8",
                    params![prior_exit,prior_epoch,prior_boot,prior_anchor,prior_root,prior_group,now,id],
                )?;
                connection.execute("UPDATE role_generations SET status='exited',updated_at=?1 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",params![now,id])?;
                results.push(serde_json::json!({"session_id":id,"attempt_id":attempt,"state":"resume_proven_nondelivery","replacement_allowed":false,"exact_retry_allowed":true}));
                continue;
            }
            if identity.is_none() {
                connection.execute(
                "UPDATE sessions SET status='launch_failed',launch_state='failed',
                        launch_error='service restarted before native spawn began',
                        exit_json=?1,readiness_state='unknown',updated_at=?2 WHERE id=?3",
                params![serde_json::json!({"delivery":"proven_nondelivery","reason":"launch reservation never entered spawning state"}).to_string(),now,id],
            )?;
                connection.execute(
                    "UPDATE role_generations SET status='launch_failed',updated_at=?1
                 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",
                    params![now, id],
                )?;
                connection.execute(
                    "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)
                 WHERE role_generation_id=(SELECT role_generation_id FROM sessions WHERE id=?2)",
                    params![now, id],
                )?;
                connection.execute(
                "UPDATE resume_invocations SET state='proven_nondelivery',error='service restarted before native spawn began',updated_at=?1
                 WHERE session_id=?2 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?2) AND state='reserved'",
                params![now, id],
            )?;
                connection.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?2,'service','session.launch.proven_nondelivery','session',?3,?4,?5)",
                params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),id,serde_json::json!({"attempt_id":attempt,"launch_state":"reserved","replacement_allowed":true}).to_string(),now],
            )?;
                results.push(serde_json::json!({"session_id":id,"attempt_id":attempt,"state":"proven_nondelivery","replacement_allowed":true}));
                continue;
            }
        }
        let parsed = identity
            .as_ref()
            .and_then(|value| serde_json::from_str::<crate::domain::ProcessIdentity>(value).ok());
        let state = match parsed.as_ref() {
            Some(process) => process_state(process)?,
            None => "identity_missing",
        };
        let now = Utc::now().to_rfc3339();
        let attempt:String=connection.query_row("SELECT attempt_id FROM role_generations WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?1)",params![id],|row|row.get(0))?;
        connection.execute("UPDATE sessions SET status='recovery_required',launch_state=CASE WHEN launch_state='reserved' THEN 'delivery_unknown' ELSE launch_state END,readiness_state='unknown',updated_at=?1 WHERE id=?2",params![now,id])?;
        connection.execute(
            "UPDATE resume_invocations SET state='delivery_unknown',error='service restarted before process ownership was reconciled',updated_at=?1
             WHERE session_id=?2 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?2) AND state IN ('reserved','spawning','running')",
            params![now, id],
        )?;
        connection.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        connection.execute("UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
        connection.execute(
            "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
            params![now, attempt],
        )?;
        connection.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at) SELECT ?1,?2,?3,'attention_required',?4,?5,?6,?6 WHERE NOT EXISTS(SELECT 1 FROM recovery_records WHERE session_id=?2 AND attempt_id=?3 AND state='attention_required' AND json_extract(detail_json,'$.kind') IS NULL AND json_extract(detail_json,'$.launch_state') IS NOT NULL AND json_extract(detail_json,'$.observation') IS NOT NULL AND json_extract(detail_json,'$.replacement_allowed')=0 AND json_extract(detail_json,'$.descendant_state')='unknown_until_explicit_reconciliation')",params![uuid::Uuid::new_v4().to_string(),id,attempt,identity,serde_json::json!({"observation":state,"launch_state":launch_state,"replacement_allowed":false,"descendant_state":"unknown_until_explicit_reconciliation"}).to_string(),now])?;
        results.push(serde_json::json!({"session_id":id,"attempt_id":attempt,"state":state,"replacement_allowed":false}));
    }
    let interrupted_admissions = {
        let mut statement = connection.prepare(
            "SELECT session_id,attempt_id,task_id,result_json FROM restart_candidates
             WHERE state='admitting' ORDER BY created_at,session_id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (session, attempt, task, raw_result) in interrupted_admissions {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut candidate_result =
            RestartCandidateResult::parse(&raw_result).with_context(|| {
                format!("restart candidate {session} has invalid interrupted admission accounting")
            })?;
        candidate_result
            .validate_candidate_state("admitting", &session)
            .with_context(|| {
                format!("restart candidate {session} has invalid interrupted admission accounting")
            })?;
        let admission = candidate_result.restart.admission.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "restart candidate {session} is admitting without durable admission ownership"
            )
        })?;
        let (
            session_status,
            session_launch_state,
            resume_count,
            transcript_epoch,
            role_generation,
            generation_status,
            attempt_status,
            task_attention,
            task_version,
            current_configuration,
            controls_clear,
        ): (String, String, i64, String, String, String, String, String, i64, bool, bool) = tx
            .query_row(
                "SELECT s.status,s.launch_state,s.resume_count,s.transcript_epoch,s.role_generation_id,
                        rg.status,a.status,t.attention,t.version,
                        EXISTS(SELECT 1 FROM role_settings setting
                          WHERE setting.task_id=t.id AND setting.role=rg.role
                            AND setting.revision=rg.config_revision
                            AND setting.effective_generation_id=rg.id)
                          OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                            SELECT 1 FROM lane_generations lane WHERE lane.lane_id=rg.lane_id
                              AND lane.effective_generation_id=rg.id)),
                        NOT EXISTS(SELECT 1 FROM controls control WHERE control.attempt_id=a.id
                          AND ((control.kind IN ('pause_now','pause_after_role','cancel')
                                AND control.state IN ('requested','draining','recovery_required'))
                            OR (control.kind IN ('manager_stop','manager_change')
                                AND control.state NOT IN ('finished','cancelled','superseded','rejected'))))
                 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE s.id=?1 AND a.id=?2 AND t.id=?3",
                params![session, attempt, task],
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
                    ))
                },
            )?;
        let invocation = tx
            .query_row(
                "SELECT state,transcript_epoch,prior_transcript_epoch FROM resume_invocations
                 WHERE session_id=?1 AND resume_ordinal=?2",
                params![session, admission.expected_resume_ordinal],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let static_authority = admission.attempt_id == attempt
            && admission.role_generation_id == role_generation
            && admission.expected_task_version == task_version
            && current_configuration
            && controls_clear;
        let admission_authority = static_authority
            && generation_status == "exited"
            && attempt_status == "running"
            && task_attention == "none";
        let absent_invocation_nondelivery = invocation.is_none()
            && transcript_epoch == admission.prior_transcript_epoch
            && session_status == "exited"
            && resume_count + 1 == i64::from(admission.expected_resume_ordinal);
        let recorded_invocation_nondelivery =
            invocation.as_ref().is_some_and(|(state, epoch, prior)| {
                state == "proven_nondelivery"
                    && epoch != &admission.prior_transcript_epoch
                    && prior.as_deref() == Some(admission.prior_transcript_epoch.as_str())
                    && transcript_epoch == admission.prior_transcript_epoch
                    && session_status == "exited"
                    && resume_count == i64::from(admission.expected_resume_ordinal)
            });
        let proven_failure = absent_invocation_nondelivery || recorded_invocation_nondelivery;
        if proven_failure {
            candidate_result.restart.replacement_failures = candidate_result
                .restart
                .replacement_failures
                .saturating_add(1)
                .min(3);
        }
        let capped = candidate_result.restart.replacement_failures >= 3;
        candidate_result.restart.terminalize_batch();
        let recovery_id = if proven_failure {
            None
        } else {
            // Only the general prior-boot incident or this exact admission can carry its action.
            let existing: Option<String> = tx.query_row(
                "SELECT id FROM recovery_records WHERE session_id=?1 AND attempt_id=?2 AND state='attention_required'
                   AND ((json_extract(detail_json,'$.kind') IS NULL
                     AND json_extract(detail_json,'$.launch_state') IS NOT NULL
                     AND json_extract(detail_json,'$.observation') IS NOT NULL
                     AND json_extract(detail_json,'$.replacement_allowed')=0
                     AND json_extract(detail_json,'$.descendant_state')='unknown_until_explicit_reconciliation')
                    OR (json_extract(detail_json,'$.kind')='restart_admission_interrupted'
                      AND json_extract(detail_json,'$.admission_id')=?3))
                 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                params![session,attempt,admission.id],|row|row.get(0)
            ).optional()?;
            if let Some(id) = existing {
                Some(id)
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                let now = Utc::now().to_rfc3339();
                tx.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
                    SELECT ?1,s.id,?2,'attention_required',s.process_identity_json,?3,?4,?4 FROM sessions s WHERE s.id=?5",
                    params![id,attempt,serde_json::json!({"kind":"restart_admission_interrupted","admission_id":admission.id,"expected_resume_ordinal":admission.expected_resume_ordinal,"invocation_state":session_launch_state,"launch_state":prior_launch_states.get(&session).unwrap_or(&session_launch_state)}).to_string(),now,session])?;
                Some(id)
            }
        };
        let original_launch_state: Option<String> = if let Some(id) = recovery_id.as_deref() {
            tx.query_row("SELECT json_extract(detail_json,'$.launch_state') FROM recovery_records WHERE id=?1",
                params![id],|row|row.get(0)).optional()?.flatten()
        } else {
            None
        };
        let authority_current = if proven_failure {
            admission_authority
        } else {
            static_authority
        };
        candidate_result.set(
            "startup_admission_reconciliation",
            serde_json::json!({
                "delivery":if proven_failure{"proven_nondelivery_or_preflight"}else{"uncertain_or_delivered"},
                "authority":if authority_current {"current"}else{"stale"},
                "invocation_state":original_launch_state.as_deref().or_else(|| prior_launch_states.get(&session).map(String::as_str)).unwrap_or(&session_launch_state),
                "expected_resume_ordinal":admission.expected_resume_ordinal,
                "prior_transcript_epoch":admission.prior_transcript_epoch,
                "admission_id":admission.id,
                "role_generation_id":admission.role_generation_id,
                "recovery_id":recovery_id,
            }),
        );
        let state = if proven_failure && admission_authority && !capped {
            "failed"
        } else {
            "blocked"
        };
        let reason = if capped {
            "three proven preflight or nondelivery failures exhausted exact retry authority after service restart".to_owned()
        } else if proven_failure && !admission_authority {
            "service restart proved nondelivery but exact admission authority became stale"
                .to_owned()
        } else if proven_failure {
            "service restart proved the interrupted exact-resume admission was not delivered"
                .to_owned()
        } else {
            "service restart left exact-resume delivery uncertain; automatic retry is forbidden"
                .to_owned()
        };
        let replacement_failures = candidate_result.restart.replacement_failures;
        let now = Utc::now().to_rfc3339();
        let changed = tx.execute(
            "UPDATE restart_candidates SET state=?1,reason=?2,result_json=?3,updated_at=?4
             WHERE session_id=?5 AND state='admitting' AND result_json=?6",
            params![
                state,
                reason,
                candidate_result.encode()?,
                now,
                session,
                raw_result
            ],
        )?;
        if changed != 1 {
            tx.rollback()?;
            continue;
        }
        let active_restored: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
             WHERE rc.attempt_id=?1 AND rc.session_id!=?2 AND rc.state='resumed' AND s.status='running')",
            params![attempt, session],
            |row| row.get(0),
        )?;
        let retryable_failure = proven_failure && admission_authority && !capped;
        tx.execute(
            "UPDATE attempts SET status=CASE WHEN ?1 THEN 'running' WHEN ?2 THEN 'needs_input' WHEN ?3 THEN 'restart_parked' ELSE 'needs_recovery' END,updated_at=?4 WHERE id=?5",
            params![active_restored,retryable_failure,proven_failure,now,attempt],
        )?;
        tx.execute(
            "UPDATE tasks SET attention=CASE WHEN ?1 THEN 'none' WHEN ?2 THEN 'resume_failed' WHEN ?3 THEN 'restart_parked' ELSE 'needs_recovery' END,updated_at=?4 WHERE id=?5",
            params![active_restored,retryable_failure,proven_failure,now,task],
        )?;
        if !proven_failure && !active_restored {
            tx.execute(
                "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
                params![now, attempt],
            )?;
        }
        tx.commit()?;
        results.push(serde_json::json!({
            "session_id":session,"attempt_id":attempt,"state":state,
            "delivery":if proven_failure{"proven_nondelivery_or_preflight"}else{"uncertain_or_delivered"},
            "replacement_failures":replacement_failures,
        }));
    }
    let checks = {
        let mut statement =
            connection.prepare("SELECT cr.id,cr.attempt_id,cr.launch_state,(SELECT COUNT(*) FROM check_processes cp WHERE cp.check_id=cr.id) FROM check_runs cr WHERE cr.status IN ('launch_reserved','running')")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let inventory = if checks.is_empty() {
        Vec::new()
    } else {
        process_inventory()?
    };
    for (check_id, attempt, launch_state, identity_count) in checks {
        if launch_state == "reserved" && identity_count == 0 {
            let now = Utc::now().to_rfc3339();
            connection.execute("UPDATE check_runs SET status='launch_failed',launch_state='proven_nondelivery',launch_error='service restarted before spawn began',evidence_json=?1,finished_at=?2 WHERE id=?3 AND status='launch_reserved'",
                params![serde_json::json!({"delivery":"proven_nondelivery","reason":"launch intent never entered spawning state"}).to_string(),now,check_id])?;
            results.push(serde_json::json!({"check_id":check_id,"attempt_id":attempt,"state":"proven_nondelivery","replacement_allowed":true}));
            continue;
        }
        let recorded = check_identities(&connection, &check_id)?;
        let live = recorded
            .iter()
            .filter(|(pid, start, _)| {
                inventory
                    .iter()
                    .any(|item| item.0 == *pid && item.2 == *start)
            })
            .map(|(pid, _, _)| *pid)
            .collect::<Vec<_>>();
        let now = Utc::now().to_rfc3339();
        let detail = serde_json::json!({
            "check_id":check_id,
            "observation":if live.is_empty(){"recorded_processes_absent"}else{"survivor_unattached"},
            "live_pids":live,
            "replacement_allowed":false
        });
        connection.execute(
            "UPDATE check_runs SET status='recovery_required',launch_state=CASE WHEN launch_state='reserved' THEN 'delivery_unknown' ELSE launch_state END,evidence_json=?1 WHERE id=?2 AND status IN ('launch_reserved','running')",
            params![detail.to_string(), check_id],
        )?;
        connection.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        connection.execute("UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
        connection.execute(
            "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
            params![now, attempt],
        )?;
        connection.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at) VALUES(?1,NULL,?2,'attention_required',?3,?4,?4)",params![uuid::Uuid::new_v4().to_string(),attempt,detail.to_string(),now])?;
        results.push(serde_json::json!({"check_id":check_id,"attempt_id":attempt,"state":"recovery_required","live_pids":live,"replacement_allowed":false}));
    }
    let freezes = {
        let mut statement = connection.prepare(
            "SELECT id,attempt_id,kind FROM freeze_intents WHERE state IN ('reserved','capturing')",
        )?;
        let pending_freezes = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        pending_freezes
    };
    for (id, attempt, kind) in freezes {
        let now = Utc::now().to_rfc3339();
        connection.execute("UPDATE freeze_intents SET state='abandoned',error='service restarted before atomic snapshot publication',updated_at=?1 WHERE id=?2 AND state IN ('reserved','capturing')",params![now,id])?;
        connection.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','snapshot.freeze.abandoned_after_restart','freeze_intent',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),id,serde_json::json!({"attempt_id":attempt,"kind":kind,"published":false}).to_string(),now])?;
        results.push(serde_json::json!({"freeze_intent_id":id,"attempt_id":attempt,"state":"abandoned","published":false}));
    }
    Ok(results)
}

fn process_state(identity: &crate::domain::ProcessIdentity) -> Result<&'static str> {
    let pgid = unsafe { libc::getpgid(identity.pid as libc::pid_t) };
    if pgid < 0 {
        return Ok("root_absent_descendants_unknown");
    }
    if pgid != identity.process_group_id {
        return Ok("pid_or_process_group_drift");
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &identity.pid.to_string()])
        .output()?;
    if output.status.success()
        && String::from_utf8(output.stdout)?.trim() == identity.native_start_marker
    {
        Ok("survivor_unattached")
    } else {
        Ok("start_identity_drift")
    }
}

pub(crate) fn admission_proof_snapshot(
    connection: &Connection,
    session: &str,
) -> Result<Option<serde_json::Value>> {
    let row: Option<(
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<String>,
    )> = connection
        .query_row(
            "SELECT rc.result_json,rg.id,s.transcript_epoch,s.launch_state,s.recovery_anchor_json,
                s.launch_boot_identity,s.recovery_process_group_id,s.process_identity_json
         FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
         JOIN role_generations rg ON rg.id=s.role_generation_id WHERE rc.session_id=?1",
            params![session],
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
        )
        .optional()?;
    let Some((raw, generation, epoch, launch_state, anchor, boot, group, identity)) = row else {
        return Ok(None);
    };
    let result = RestartCandidateResult::parse(&raw)?;
    let Some(incident) = result.get("startup_admission_reconciliation") else {
        return Ok(None);
    };
    if incident["delivery"] != "uncertain_or_delivered" {
        return Ok(None);
    }
    let Some(recovery_id) = incident["recovery_id"].as_str() else {
        return Ok(Some(
            serde_json::json!({"applicable":false,"reason":"interrupted admission recovery record missing"}),
        ));
    };
    if incident["role_generation_id"].as_str() != Some(generation.as_str()) {
        return Ok(Some(
            serde_json::json!({"recovery_id":recovery_id,"applicable":false,"reason":"role generation changed"}),
        ));
    }
    let record: Option<(String,String,Option<String>,Option<String>)> = connection.query_row(
        "SELECT state,attempt_id,process_identity_json,detail_json FROM recovery_records WHERE id=?1 AND session_id=?2",
        params![recovery_id,session],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
    ).optional()?;
    let Some((state, record_attempt, record_identity, record_detail)) = record else {
        return Ok(Some(
            serde_json::json!({"recovery_id":recovery_id,"applicable":false,"reason":"recovery record missing"}),
        ));
    };
    let candidate_attempt: String = connection.query_row(
        "SELECT attempt_id FROM restart_candidates WHERE session_id=?1",
        params![session],
        |row| row.get(0),
    )?;
    let Some(admission_id) = incident["admission_id"]
        .as_str()
        .filter(|id| !id.is_empty())
    else {
        return Ok(Some(
            serde_json::json!({"recovery_id":recovery_id,"applicable":false,"reason":"recovery record no longer matches"}),
        ));
    };
    let Some(ordinal) = incident["expected_resume_ordinal"].as_u64() else {
        return Ok(Some(
            serde_json::json!({"recovery_id":recovery_id,"applicable":false,"reason":"exact resume ordinal missing"}),
        ));
    };
    let record_matches_incident = record_detail
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .is_some_and(|detail| {
            (detail["kind"] == "restart_admission_interrupted"
                && detail["admission_id"] == admission_id
                && detail["expected_resume_ordinal"] == ordinal)
                || (detail["kind"].is_null()
                    && !detail["launch_state"].is_null()
                    && !detail["observation"].is_null()
                    && (detail["replacement_allowed"] == false
                        || detail["replacement_allowed"] == 0)
                    && detail["descendant_state"] == "unknown_until_explicit_reconciliation")
        });
    if record_attempt != candidate_attempt
        || !matches!(state.as_str(), "attention_required" | "resolved_quiescent")
        || !record_matches_incident
    {
        return Ok(Some(
            serde_json::json!({"recovery_id":recovery_id,"applicable":false,"reason":"recovery record no longer matches"}),
        ));
    }
    let invocation: Option<(String,Option<String>,Option<String>)> = connection.query_row(
        "SELECT transcript_epoch,prior_transcript_epoch,prior_recovery_anchor_json FROM resume_invocations WHERE session_id=?1 AND resume_ordinal=?2",
        params![session,ordinal],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))
    ).optional()?;
    let Some((invocation_epoch, prior_epoch, prior_anchor)) = invocation else {
        return Ok(Some(
            serde_json::json!({"recovery_id":recovery_id,"applicable":false,"reason":"current invocation missing"}),
        ));
    };
    let parsed_prior_anchor = prior_anchor
        .as_deref()
        .map(serde_json::from_str::<ProcessGenerationAnchor>)
        .transpose();
    let parsed_current_anchor = anchor
        .as_deref()
        .map(serde_json::from_str::<ProcessGenerationAnchor>)
        .transpose();
    let current_anchor = match (&parsed_current_anchor, &parsed_prior_anchor) {
        (Ok(Some(current)), Ok(Some(prior))) => current != prior,
        (Ok(Some(_)), Ok(None)) => true,
        _ => false,
    };
    let current = invocation_epoch == epoch
        && prior_epoch.as_deref() == incident["prior_transcript_epoch"].as_str()
        && incident["invocation_state"]
            .as_str()
            .is_some_and(|state| !state.is_empty())
        && parsed_prior_anchor.is_ok()
        && parsed_current_anchor.is_ok()
        && record_identity
            .as_deref()
            .is_none_or(|raw| serde_json::from_str::<ProcessIdentity>(raw).is_ok())
        && identity
            .as_deref()
            .is_none_or(|raw| serde_json::from_str::<ProcessIdentity>(raw).is_ok())
        && record_identity
            .as_ref()
            .is_none_or(|recorded| identity.as_ref() == Some(recorded));
    if state == "resolved_quiescent" && current {
        return Ok(None);
    }
    let mut statement = connection.prepare("SELECT pid,native_start_marker,process_group_id FROM session_processes WHERE session_id=?1 ORDER BY pid,native_start_marker")?;
    let processes = statement
        .query_map(params![session], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(
        serde_json::json!({"recovery_id":recovery_id,"applicable":current,
        "current_anchor":current_anchor,
        "invocation_epoch":invocation_epoch,"session_epoch":epoch,"prior_epoch":prior_epoch,
        "prior_anchor":prior_anchor,"anchor":anchor,"boot":boot,"group":group,
        "identity":identity,"processes":processes,"launch_state":launch_state,
        "invocation_state":incident["invocation_state"]}),
    ))
}

fn admission_uses_prior_generation(proof: &serde_json::Value) -> bool {
    proof["current_anchor"] != true
}

pub fn verify_session_quiescent(store: &Store, session: &str) -> Result<serde_json::Value> {
    let (recorded, group_anchor, anchor, launch_boot, admission_proof) = {
        let connection = store.lock()?;
        let admission_proof = admission_proof_snapshot(&connection, session)?;
        let mut statement=connection.prepare("SELECT pid,native_start_marker,process_group_id FROM session_processes WHERE session_id=?1")?;
        let rows = statement
            .query_map(params![session], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let (group, anchor, boot) = connection.query_row(
            "SELECT recovery_process_group_id,recovery_anchor_json,launch_boot_identity FROM sessions WHERE id=?1",
            params![session],
            |row| Ok((row.get::<_, Option<i32>>(0)?,row.get::<_, Option<String>>(1)?,row.get::<_, Option<String>>(2)?)),
        )?;
        (rows, group, anchor, boot, admission_proof)
    };
    let inventory = process_inventory()?;
    let stale_prior = admission_proof
        .as_ref()
        .is_some_and(admission_uses_prior_generation);
    if admission_proof
        .as_ref()
        .is_some_and(|proof| proof["applicable"] != true)
    {
        bail!("interrupted admission proof no longer matches its exact current invocation")
    }
    let evidence = verify_generation_absent(
        "session",
        session,
        if stale_prior { &[] } else { &recorded },
        if stale_prior { None } else { group_anchor },
        if stale_prior { None } else { anchor.as_deref() },
        launch_boot.as_deref(),
        &inventory,
    )?;
    Ok(
        serde_json::json!({"session_id":session,"verification":evidence,"admission_proof":admission_proof}),
    )
}

#[derive(Clone, Debug)]
pub(crate) enum SessionProcessObservation {
    Quiescent(serde_json::Value),
    Live(serde_json::Value),
    Uncertain(String),
}

#[derive(Clone, Debug)]
struct ProcessSnapshot {
    inventory: std::result::Result<Vec<(u32, i32, String)>, String>,
    boot_identity: std::result::Result<String, String>,
}

fn capture_process_snapshot() -> ProcessSnapshot {
    ProcessSnapshot {
        inventory: process_inventory().map_err(|error| format!("{error:#}")),
        boot_identity: crate::supervisor::system_boot_identity()
            .map_err(|error| format!("{error:#}")),
    }
}

fn observe_processes(
    facts: &RestartSessionFacts,
    snapshot: &ProcessSnapshot,
) -> SessionProcessObservation {
    let session = &facts.session_id;
    let stale_prior = facts
        .admission_proof
        .as_ref()
        .is_some_and(admission_uses_prior_generation);
    if facts
        .admission_proof
        .as_ref()
        .is_some_and(|proof| proof["applicable"] != true)
    {
        return SessionProcessObservation::Uncertain(
            "interrupted admission no longer matches its exact invocation".to_owned(),
        );
    }
    let mut recorded = if stale_prior {
        Vec::new()
    } else {
        facts.recorded_processes.clone()
    };
    if let Some(process_json) = facts
        .process_identity_json
        .as_deref()
        .filter(|_| !stale_prior)
    {
        match serde_json::from_str::<ProcessIdentity>(process_json) {
            Ok(process)
                if !recorded.iter().any(|(pid, start, _)| {
                    *pid == process.pid && start == &process.native_start_marker
                }) =>
            {
                recorded.push((
                    process.pid,
                    process.native_start_marker,
                    process.process_group_id,
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return SessionProcessObservation::Uncertain(format!(
                    "session {session} has malformed exact process identity: {error}"
                ))
            }
        }
    }
    let anchor = match (if stale_prior {
        None
    } else {
        facts.recovery_anchor_json.as_deref()
    })
    .map(serde_json::from_str::<ProcessGenerationAnchor>)
    .transpose()
    {
        Ok(anchor) => anchor,
        Err(error) => {
            return SessionProcessObservation::Uncertain(format!(
                "session {session} has malformed generation anchor: {error}"
            ))
        }
    };
    let inventory = match &snapshot.inventory {
        Ok(inventory) => inventory,
        Err(error) => {
            return SessionProcessObservation::Uncertain(format!(
                "operating-system process inventory was unavailable: {error}"
            ))
        }
    };
    let current_boot = match &snapshot.boot_identity {
        Ok(boot) => boot,
        Err(error) => {
            return SessionProcessObservation::Uncertain(format!(
                "operating-system boot identity was unavailable: {error}"
            ))
        }
    };
    let recorded_boot = anchor
        .as_ref()
        .map(|value| value.boot_identity.as_str())
        .or(facts.launch_boot_identity.as_deref());
    if recorded_boot
        .is_some_and(|boot| crate::supervisor::boot_identity_proves_reboot(boot, current_boot))
    {
        return SessionProcessObservation::Quiescent(serde_json::json!({
            "session_id":session,
            "verification":{
                "source":"operating_system_boot_changed",
                "recorded_boot":recorded_boot,
                "current_boot":current_boot,
                "recorded_identities":recorded.len()
            }
        }));
    }
    let live = inventory
        .iter()
        .filter(|(pid, _, start)| {
            recorded
                .iter()
                .any(|(recorded_pid, recorded_start, _)| {
                    pid == recorded_pid && start == recorded_start
                })
        })
        .map(|(pid, pgid, start)| {
            serde_json::json!({"pid":pid,"process_group_id":pgid,"native_start_marker":start})
        })
        .collect::<Vec<_>>();
    if !live.is_empty() {
        return SessionProcessObservation::Live(serde_json::json!({
            "source":"operating_system_exact_process_inventory",
            "session_id":session,
            "live_processes":live
        }));
    }
    match verify_generation_absent_evidence(
        "session",
        session,
        &recorded,
        if stale_prior {
            None
        } else {
            facts.recovery_process_group_id
        },
        anchor.as_ref(),
        facts.launch_boot_identity.as_deref(),
        current_boot,
        inventory,
    ) {
        Ok(verification) => SessionProcessObservation::Quiescent(
            serde_json::json!({"session_id":session,"verification":verification}),
        ),
        Err(error) => SessionProcessObservation::Uncertain(format!("{error:#}")),
    }
}

/// Classifies one durable session generation from fresh operating-system
/// evidence. Database status alone cannot establish current process absence.
pub(crate) fn observe_session_processes(
    store: &Store,
    session: &str,
) -> Result<SessionProcessObservation> {
    let mut facts = {
        let connection = store.lock()?;
        let mut facts = restart_session_facts(&connection, Some(session))?;
        attach_recorded_processes(&connection, &mut facts, Some(session))?;
        if let Some(item) = facts.first_mut() {
            item.admission_proof = admission_proof_snapshot(&connection, session)?;
        }
        facts
    };
    let facts = facts
        .pop()
        .ok_or_else(|| anyhow::anyhow!("session {session} does not exist"))?;
    Ok(observe_processes(&facts, &capture_process_snapshot()))
}

pub(crate) fn restart_preview(
    store: &Store,
    dispatch_enabled: bool,
    draining: bool,
) -> Result<RestartPreview> {
    let (mut facts, restore_hold) = {
        let mut connection = store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let restore_hold = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records
             WHERE id='database-restore-hold' AND state='attention_required')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let mut facts = restart_session_facts(&transaction, None)?;
        attach_recorded_processes(&transaction, &mut facts, None)?;
        for item in &mut facts {
            item.admission_proof = admission_proof_snapshot(&transaction, &item.session_id)?;
            item.trip_readiness_error =
                crate::trip::require_attempt_ready(&transaction, &item.attempt_id, None)
                    .err()
                    .map(|error| format!("{error:#}"));
        }
        transaction.commit()?;
        (facts, restore_hold)
    };
    let process_snapshot = capture_process_snapshot();
    let captured_at = Utc::now().to_rfc3339();
    let sessions = facts
        .drain(..)
        .map(|facts| {
            let observation = observe_processes(&facts, &process_snapshot);
            preview_session(facts, observation, restore_hold, dispatch_enabled, draining)
        })
        .collect();
    Ok(RestartPreview {
        decision_schema: DECISION_SCHEMA_V1,
        snapshot: RestartPreviewSnapshot {
            captured_at,
            process_inventory: if process_snapshot.inventory.is_ok() {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Uncertain
            },
            boot_identity: if process_snapshot.boot_identity.is_ok() {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Uncertain
            },
            dispatch_enabled,
            draining,
            revalidation_required: true,
            notice: "Database and operating-system evidence are read-only snapshots, not one atomic cross-system observation; every action revalidates current authority. Fresh-only is descriptive and does not authorize dispatch or provider-session consumption."
                .to_owned(),
        },
        sessions,
    })
}

pub(crate) fn read_only_restart_decisions(
    connection: &Connection,
) -> Result<Vec<DecisionExplanation>> {
    let now = Utc::now();
    restart_session_facts(connection, None)?
        .into_iter()
        .filter(|facts| facts.candidate_state.is_some())
        .map(|mut facts| {
            facts.trip_readiness_error =
                crate::trip::require_attempt_ready(connection, &facts.attempt_id, None)
                    .err()
                    .map(|error| format!("{error:#}"));
            let continue_allowed = crate::workflow::ordinary_control_policy(
                connection,
                &facts.task_id,
                Some(&facts.attempt_id),
            )?
            .iter()
            .any(|control| control == "continue");
            let valid_bound = facts
                .candidate_result
                .as_ref()
                .and_then(|result| result.get("startup_admission_reconciliation"))
                .and_then(|incident| incident["recovery_id"].as_str())
                .is_some_and(|recovery_id| {
                    crate::workflow::validate_recovery_identity(
                        connection,
                        recovery_id,
                        &facts.task_id,
                        &facts.attempt_id,
                        Some(&facts.session_id),
                    )
                    .is_ok()
                });
            recorded_restart_decision(facts, now, continue_allowed, valid_bound)
        })
        .collect()
}

fn recorded_restart_decision(
    facts: RestartSessionFacts,
    now: chrono::DateTime<Utc>,
    continue_allowed: bool,
    valid_bound: bool,
) -> Result<DecisionExplanation> {
    let state = facts.candidate_state.as_deref().unwrap_or("missing");
    let result = facts.candidate_result.as_ref();
    let metadata = result.map(|result| &result.restart);
    let native_resume_forbidden = result
        .and_then(|result| result.get("native_resume_forbidden"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let due = metadata
        .and_then(|metadata| metadata.next_due_at.as_deref())
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| anyhow::anyhow!("restart candidate next_due_at is malformed"))?;
    let replacement_failures = metadata
        .map(|metadata| metadata.replacement_failures)
        .unwrap_or_default();
    let incident = result.and_then(|result| result.get("startup_admission_reconciliation"));
    let recovery_id = incident.and_then(|value| value["recovery_id"].as_str());
    let bound_recovery = valid_bound && facts.unresolved_recovery;
    let capacity_deferrals = metadata
        .map(|metadata| metadata.capacity_deferrals)
        .unwrap_or_default();
    let admission_gate = facts.admission_gate(false);
    // A pending Continue preserves the fresh route but disables another request.
    let fresh_dispatch_valid = facts.session_status == "exited"
        && facts.process_group_quiescent
        && !facts.unresolved_recovery
        && facts.attempt_status == "restart_parked"
        && facts.attention == "restart_parked";
    let (reason_code, disposition, evidence_state, owner, message, operation, enabled) = if facts
        .candidate_metadata_error
        .is_some()
    {
        (
            RestartGate::MetadataMalformed.reason_code(),
            DecisionDisposition::Held,
            DecisionEvidenceState::Uncertain,
            DecisionOwner::Service,
            facts
                .candidate_metadata_error
                .as_deref()
                .unwrap_or(RestartGate::MetadataMalformed.message()),
            "refresh_and_reconcile",
            false,
        )
    } else if matches!(state, "resumed" | "released_fresh_dispatch" | "cancelled") {
        (
            "restart.complete",
            DecisionDisposition::Terminal,
            DecisionEvidenceState::Satisfied,
            DecisionOwner::Service,
            facts
                .candidate_reason
                .as_deref()
                .unwrap_or("restart membership reached a terminal disposition"),
            "none",
            false,
        )
    } else if facts.attempt_status == "needs_recovery"
        && facts.attention == "needs_recovery"
        && !facts.unresolved_recovery
    {
        (
                "restart.ownership_reconciliation_required",
                DecisionDisposition::Held,
                DecisionEvidenceState::Uncertain,
                DecisionOwner::Service,
                "the attempt requires recovery, but this session has no bound recovery record; reconcile ownership before Continue. Ordinary task controls may still offer Cancel",
                "refresh_and_reconcile",
                false,
            )
    } else if bound_recovery {
        (
            "restart.recovery_required",
            DecisionDisposition::Held,
            DecisionEvidenceState::Uncertain,
            DecisionOwner::Human,
            "interrupted exact-resume delivery requires resolution of its bound recovery record",
            "resolve_recovery",
            true,
        )
    } else if replacement_failures >= 3 {
        (
            RestartGate::ReplacementLimitReached.reason_code(),
            DecisionDisposition::Held,
            DecisionEvidenceState::Stale,
            DecisionOwner::Service,
            RestartGate::ReplacementLimitReached.message(),
            if fresh_dispatch_valid {
                "continue"
            } else {
                "resolve_recovery"
            },
            fresh_dispatch_valid && continue_allowed,
        )
    } else if incident.is_some_and(|value| {
        value["delivery"] == "proven_nondelivery_or_preflight" && value["authority"] == "stale"
    }) {
        (
            "restart.admission_authority_stale",
            DecisionDisposition::Held,
            DecisionEvidenceState::Stale,
            DecisionOwner::Service,
            "delivery was proven absent, but exact task or control authority changed before restart reconciliation",
            if fresh_dispatch_valid { "continue" } else { "refresh_and_reconcile" },
            fresh_dispatch_valid && continue_allowed,
        )
    } else if state == "admitting" {
        (
            "restart.admission_in_progress",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            "exact resume admission is durably owned and has no elapsed-time expiration",
            "wait_for_service",
            false,
        )
    } else if state == "pending_reconciliation" {
        (
            "restart.reconciliation_pending",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            "recorded process ownership must reconcile before any restart admission",
            "wait_for_service",
            false,
        )
    } else if state == "queued_capacity" && due.is_some_and(|due| due > now) {
        (
            "restart.capacity_deferred",
            DecisionDisposition::RetryDeferred,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            "exact resume remains queued until its persisted capacity due time",
            "wait_for_capacity",
            false,
        )
    } else if matches!(state, "parked" | "queued_capacity" | "failed")
        && admission_gate.is_none()
        && facts.trip_readiness_error.is_none()
        && !native_resume_forbidden
    {
        if state == "queued_capacity" {
            (
                "restart.queued_due",
                DecisionDisposition::Waiting,
                DecisionEvidenceState::Satisfied,
                DecisionOwner::Service,
                "the durable restart member is due for one serialized coordinator admission",
                "wait_for_service",
                false,
            )
        } else {
            (
                "restart.resumable_now",
                DecisionDisposition::Ready,
                DecisionEvidenceState::Satisfied,
                DecisionOwner::Human,
                "recorded facts permit a revalidated exact-resume request",
                "restart_resume",
                true,
            )
        }
    } else if fresh_dispatch_valid
        && (native_resume_forbidden
            || matches!(state, "blocked" | "skipped")
            || matches!(admission_gate, Some(RestartGate::FreshOnlyRole)))
    {
        (
            "restart.fresh_dispatch_hold_releasable",
            DecisionDisposition::Held,
            DecisionEvidenceState::Satisfied,
            DecisionOwner::Human,
            "positive recorded quiescence permits only explicit release of the fresh-dispatch hold",
            "continue",
            continue_allowed,
        )
    } else if let Some(gate) = admission_gate {
        (
            gate.reason_code(),
            DecisionDisposition::Held,
            DecisionEvidenceState::Stale,
            gate.owner(),
            gate.message(),
            "refresh_and_reconcile",
            false,
        )
    } else if let Some(error) = facts.trip_readiness_error.as_deref() {
        (
            "restart.trip_not_ready",
            DecisionDisposition::Held,
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            error,
            "refresh_and_reconcile",
            false,
        )
    } else {
        (
            "restart.recovery_required",
            DecisionDisposition::Held,
            DecisionEvidenceState::Uncertain,
            DecisionOwner::Human,
            facts
                .candidate_reason
                .as_deref()
                .unwrap_or("restart ownership requires explicit recovery"),
            "resolve_recovery",
            false,
        )
    };
    let binding = DecisionActionBinding {
        project_id: Some(facts.project_id.clone()),
        task_id: Some(facts.task_id.clone()),
        attempt_id: Some(facts.attempt_id.clone()),
        session_id: Some(facts.session_id.clone()),
        role_generation_id: Some(facts.role_generation_id.clone()),
        recovery_id: if bound_recovery {
            recovery_id.map(str::to_owned)
        } else {
            None
        },
        expected_task_version: Some(facts.task_version),
        expected_project_version: Some(facts.project_version),
        settings_revision: Some(facts.configuration_revision),
        selected_checks_revision: Some(facts.selected_checks_revision),
        candidate_state: facts.candidate_state.clone(),
        plan_hash: facts.plan_hash.clone(),
        candidate_hash: facts.candidate_hash.clone(),
        ..DecisionActionBinding::default()
    };
    let primary = DecisionPrerequisite {
        code: reason_code.to_owned(),
        state: evidence_state,
        owner,
        evidence: serde_json::json!({
            "candidate_state":state,
            "replacement_failure_count":replacement_failures,
            "capacity_deferral_count":capacity_deferrals,
            "next_due_at":metadata.and_then(|metadata| metadata.next_due_at.as_deref()),
            "batch_operation_id":metadata.and_then(|metadata| metadata.batch.as_ref()).map(|batch| batch.operation_id.as_str()),
            "batch_membership":metadata.and_then(|metadata| metadata.batch.as_ref()).map(|batch| batch.membership),
            "process_group_quiescent":facts.process_group_quiescent,
            "attempt_status":facts.attempt_status.as_str(),
            "task_attention":facts.attention.as_str(),
            "unresolved_recovery":facts.unresolved_recovery,
        }),
        message: Some(message.to_owned()),
    };
    let ready = disposition == DecisionDisposition::Ready;
    Ok(DecisionExplanation {
        decision_schema: DECISION_SCHEMA_V1,
        reason_code: reason_code.to_owned(),
        disposition,
        subject: DecisionSubject {
            project_id: Some(facts.project_id),
            task_id: Some(facts.task_id),
            attempt_id: Some(facts.attempt_id),
            session_id: Some(facts.session_id),
            role_generation_id: Some(facts.role_generation_id),
            recovery_id: if bound_recovery { recovery_id.map(str::to_owned) } else { None },
        },
        observed_revision: DecisionObservedRevision {
            task_version: Some(facts.task_version),
            project_version: Some(facts.project_version),
            attempt_phase: Some(facts.phase),
            configuration_revision: Some(facts.configuration_revision),
            selected_checks_revision: Some(facts.selected_checks_revision),
            plan_hash: facts.plan_hash,
            candidate_hash: facts.candidate_hash,
        },
        primary_blocker: (!ready).then_some(primary.clone()),
        prerequisites: vec![primary],
        ownership: DecisionOwnership {
            owner,
            state: reason_code.to_owned(),
            binding: binding.clone(),
        },
        next_action: (operation != "none").then_some(DecisionNextAction {
            operation: operation.to_owned(),
            enabled,
            owner: if enabled { DecisionOwner::Human } else { owner },
            binding,
            accounting_note: Some(
                "Recorded restart status is descriptive; every action revalidates durable authority."
                    .to_owned(),
            ),
        }),
        control_policy: DecisionControlPolicy {
            allowed_controls: enabled.then(|| vec![operation.to_owned()]).unwrap_or_default(),
            disabled_reason_code: (!enabled).then_some(reason_code.to_owned()),
        },
    })
}

fn preview_session(
    facts: RestartSessionFacts,
    observation: SessionProcessObservation,
    restore_hold: bool,
    dispatch_enabled: bool,
    draining: bool,
) -> RestartPreviewSession {
    let explicit_approval = facts.lifecycle == "awaiting_review"
        || matches!(
            facts.phase.as_str(),
            "awaiting_plan_approval" | "awaiting_implementation_authorization"
        )
        || !facts.queue_open
        || !facts.controls_clear
        || (matches!(
            facts.attention.as_str(),
            "paused" | "needs_input" | "resume_failed"
        ) && !(facts.candidate_state.as_deref() == Some("failed")
            && facts.attention == "resume_failed"));
    let completed = facts.archived || matches!(facts.lifecycle.as_str(), "done" | "cancelled");
    let process_uncertain = matches!(&observation, SessionProcessObservation::Uncertain(_));
    let process_live = matches!(&observation, SessionProcessObservation::Live(_));
    let preparation_gate = facts.preparation_gate();
    let consumed_candidate = matches!(
        facts.candidate_state.as_deref(),
        Some("resumed" | "cancelled")
    );
    let current_running_cycle = facts.session_status == "running";

    let (
        classification,
        can_resume_now,
        could_resume_after_confirmed_shutdown,
        reason_code,
        primary,
    ) = if completed || (consumed_candidate && !current_running_cycle) {
        (
            RestartPreviewClassification::Complete,
            false,
            false,
            "restart.complete".to_owned(),
            preview_blocker(
                "restart.complete",
                DecisionEvidenceState::Satisfied,
                DecisionOwner::Service,
                "this session has no pending restart action",
                serde_json::json!({"candidate_state":facts.candidate_state.as_deref()}),
            ),
        )
    } else if restore_hold {
        (
            RestartPreviewClassification::Blocked,
            false,
            false,
            "restart.restore_hold_active".to_owned(),
            preview_blocker(
                "restart.restore_hold_active",
                DecisionEvidenceState::Pending,
                DecisionOwner::Human,
                "database restore reconciliation holds execution",
                serde_json::json!({"state":"attention_required"}),
            ),
        )
    } else if facts.unresolved_recovery || process_uncertain {
        let message = match &observation {
            SessionProcessObservation::Uncertain(reason) => reason.clone(),
            _ => "recorded process ownership remains unresolved".to_owned(),
        };
        (
            RestartPreviewClassification::Uncertain,
            false,
            false,
            "restart.process_evidence_uncertain".to_owned(),
            preview_blocker(
                "restart.process_evidence_uncertain",
                DecisionEvidenceState::Uncertain,
                DecisionOwner::Service,
                &message,
                serde_json::json!({"unresolved_recovery":facts.unresolved_recovery}),
            ),
        )
    } else if explicit_approval
        && matches!(
            facts.session_status.as_str(),
            "launch_reserved" | "interrupt_requested"
        )
    {
        (
            RestartPreviewClassification::AwaitingApproval,
            false,
            false,
            "restart.awaiting_human_authority".to_owned(),
            preview_blocker(
                "restart.awaiting_human_authority",
                DecisionEvidenceState::Pending,
                DecisionOwner::Human,
                "workflow approval, task attention, queue, or manager control holds restoration",
                serde_json::json!({"phase":facts.phase.as_str(),"lifecycle":facts.lifecycle.as_str(),"attention":facts.attention.as_str(),"queue_paused":!facts.queue_open,"control_held":!facts.controls_clear}),
            ),
        )
    } else if matches!(
        facts.session_status.as_str(),
        "launch_reserved" | "interrupt_requested"
    ) {
        (
                RestartPreviewClassification::FreshOnly,
                false,
                false,
                "restart.not_captured_for_exact_resume".to_owned(),
                preview_blocker(
                    "restart.not_captured_for_exact_resume",
                    DecisionEvidenceState::Missing,
                    DecisionOwner::Service,
                    "launch-reserved and interrupt-requested sessions are not captured as exact restart intent",
                    serde_json::json!({"session_status":facts.session_status.as_str(),"launch_state":facts.launch_state.as_str()}),
                ),
            )
    } else if facts.candidate_state.is_some() && !consumed_candidate {
        let gate = if process_live {
            facts.potential_admission_gate()
        } else {
            facts.admission_gate(false)
        };
        if let Some(gate) = gate {
            preview_gate_outcome(gate, &facts)
        } else if let Some(error) = facts.trip_readiness_error.as_deref() {
            preview_trip_block(error)
        } else if process_live {
            (
                RestartPreviewClassification::Resumable,
                false,
                true,
                "restart.eligible_after_confirmed_shutdown".to_owned(),
                preview_blocker(
                    "restart.process_still_live",
                    DecisionEvidenceState::Pending,
                    DecisionOwner::Service,
                    "the exact process is live; admission requires later proven quiescence",
                    observation_evidence(&observation),
                ),
            )
        } else if !dispatch_enabled || draining {
            (
                RestartPreviewClassification::Blocked,
                false,
                false,
                "restart.service_dispatch_disabled".to_owned(),
                preview_blocker(
                    "restart.service_dispatch_disabled",
                    DecisionEvidenceState::Pending,
                    DecisionOwner::Service,
                    "the service is draining or dispatch is disabled",
                    serde_json::json!({"dispatch_enabled":dispatch_enabled,"draining":draining}),
                ),
            )
        } else {
            (
                RestartPreviewClassification::Resumable,
                true,
                false,
                "restart.resumable_now".to_owned(),
                preview_blocker(
                    "restart.resumable_now",
                    DecisionEvidenceState::Satisfied,
                    DecisionOwner::Human,
                    "current exact restart authority is established for revalidated admission",
                    serde_json::json!({"candidate_state":facts.candidate_state.as_deref()}),
                ),
            )
        }
    } else if let Some(gate) = preparation_gate {
        preview_gate_outcome(gate, &facts)
    } else if let Some(error) = facts.trip_readiness_error.as_deref() {
        preview_trip_block(error)
    } else if explicit_approval {
        (
            RestartPreviewClassification::AwaitingApproval,
            false,
            false,
            "restart.awaiting_human_authority".to_owned(),
            preview_blocker(
                "restart.awaiting_human_authority",
                DecisionEvidenceState::Pending,
                DecisionOwner::Human,
                "workflow approval, task attention, queue, or manager control holds restoration",
                serde_json::json!({"phase":facts.phase.as_str(),"lifecycle":facts.lifecycle.as_str(),"attention":facts.attention.as_str(),"queue_paused":!facts.queue_open,"control_held":!facts.controls_clear}),
            ),
        )
    } else if facts.session_status == "running" && process_live {
        (
                RestartPreviewClassification::Resumable,
                false,
                true,
                "restart.eligible_after_confirmed_shutdown".to_owned(),
                preview_blocker(
                    "restart.process_still_live",
                    DecisionEvidenceState::Pending,
                    DecisionOwner::Service,
                    "the live session could become eligible only after drain captures it and quiescence is proven",
                    observation_evidence(&observation),
                ),
            )
    } else if facts.session_status == "running" {
        (
            RestartPreviewClassification::Uncertain,
            false,
            false,
            "restart.database_process_mismatch".to_owned(),
            preview_blocker(
                "restart.database_process_mismatch",
                DecisionEvidenceState::Uncertain,
                DecisionOwner::Service,
                "the database reports running while fresh process evidence reports quiescence",
                observation_evidence(&observation),
            ),
        )
    } else {
        (
            RestartPreviewClassification::FreshOnly,
            false,
            false,
            "restart.not_captured".to_owned(),
            preview_blocker(
                "restart.not_captured",
                DecisionEvidenceState::Missing,
                DecisionOwner::Service,
                "this session has no durable exact restart candidate",
                serde_json::json!({"session_status":facts.session_status.as_str()}),
            ),
        )
    };

    let binding = DecisionActionBinding {
        project_id: Some(facts.project_id.clone()),
        task_id: Some(facts.task_id.clone()),
        attempt_id: Some(facts.attempt_id.clone()),
        session_id: Some(facts.session_id.clone()),
        role_generation_id: Some(facts.role_generation_id.clone()),
        expected_task_version: Some(facts.task_version),
        expected_project_version: Some(facts.project_version),
        settings_revision: Some(facts.configuration_revision),
        selected_checks_revision: Some(facts.selected_checks_revision),
        candidate_state: facts.candidate_state.clone(),
        plan_hash: facts.plan_hash.clone(),
        candidate_hash: facts.candidate_hash.clone(),
        ..DecisionActionBinding::default()
    };
    let mut prerequisites = preview_prerequisites(&facts, &observation, restore_hold);
    if !prerequisites.iter().any(|item| item.code == primary.code) {
        prerequisites.insert(0, primary.clone());
    }
    let owner = primary.owner;
    let primary_blocker = (!can_resume_now).then_some(primary);
    RestartPreviewSession {
        classification,
        can_resume_now,
        could_resume_after_confirmed_shutdown,
        decision: DecisionExplanation {
            decision_schema: DECISION_SCHEMA_V1,
            reason_code,
            disposition: match classification {
                RestartPreviewClassification::Resumable if can_resume_now => {
                    DecisionDisposition::Ready
                }
                RestartPreviewClassification::Resumable
                | RestartPreviewClassification::FreshOnly
                | RestartPreviewClassification::AwaitingApproval => DecisionDisposition::Waiting,
                RestartPreviewClassification::Blocked
                | RestartPreviewClassification::Uncertain => DecisionDisposition::Held,
                RestartPreviewClassification::Complete => DecisionDisposition::Terminal,
            },
            subject: DecisionSubject {
                project_id: Some(facts.project_id),
                task_id: Some(facts.task_id),
                attempt_id: Some(facts.attempt_id),
                session_id: Some(facts.session_id),
                role_generation_id: Some(facts.role_generation_id),
                recovery_id: None,
            },
            observed_revision: DecisionObservedRevision {
                task_version: Some(facts.task_version),
                project_version: Some(facts.project_version),
                attempt_phase: Some(facts.phase),
                configuration_revision: Some(facts.configuration_revision),
                selected_checks_revision: Some(facts.selected_checks_revision),
                plan_hash: facts.plan_hash,
                candidate_hash: facts.candidate_hash,
            },
            primary_blocker,
            prerequisites,
            ownership: DecisionOwnership {
                owner,
                state: if can_resume_now {
                    "exact_restart_authority_current"
                } else {
                    "preview_only_revalidation_required"
                }
                .to_owned(),
                binding: binding.clone(),
            },
            next_action: can_resume_now.then_some(DecisionNextAction {
                operation: "restart_resume".to_owned(),
                enabled: true,
                owner: DecisionOwner::Human,
                binding,
                accounting_note: Some(
                    "Preview grants no authority; admission re-reads all facts before native resume."
                        .to_owned(),
                ),
            }),
            control_policy: DecisionControlPolicy {
                allowed_controls: can_resume_now
                    .then(|| vec!["restart_resume".to_owned()])
                    .unwrap_or_default(),
                disabled_reason_code: (!can_resume_now).then_some(
                    "restart.preview_requires_revalidation_or_prerequisite".to_owned(),
                ),
            },
        },
    }
}

fn preview_gate_outcome(
    gate: RestartGate,
    facts: &RestartSessionFacts,
) -> (
    RestartPreviewClassification,
    bool,
    bool,
    String,
    DecisionPrerequisite,
) {
    let classification = match gate {
        RestartGate::TerminalTask if facts.lifecycle == "awaiting_review" => {
            RestartPreviewClassification::AwaitingApproval
        }
        RestartGate::TerminalTask => RestartPreviewClassification::Complete,
        RestartGate::CurrentAttemptMissing
            if facts.lifecycle == "awaiting_review"
                || matches!(
                    facts.phase.as_str(),
                    "awaiting_plan_approval" | "awaiting_implementation_authorization"
                ) =>
        {
            RestartPreviewClassification::AwaitingApproval
        }
        RestartGate::QueueHeld | RestartGate::ControlHeld | RestartGate::AttentionHeld => {
            RestartPreviewClassification::AwaitingApproval
        }
        RestartGate::CandidateUnavailable
            if matches!(
                facts.candidate_state.as_deref(),
                Some("skipped" | "released_fresh_dispatch")
            ) =>
        {
            RestartPreviewClassification::FreshOnly
        }
        RestartGate::CandidateUnavailable
        | RestartGate::AttemptNotRunning
        | RestartGate::WorkflowPhaseUnsupported
        | RestartGate::MetadataMalformed
        | RestartGate::ReplacementLimitReached => RestartPreviewClassification::Blocked,
        RestartGate::NativeBindingUnavailable
        | RestartGate::NativeHistoryMissing
        | RestartGate::CurrentCapabilityMissing
        | RestartGate::IsolatedValidation
        | RestartGate::CurrentAttemptMissing
        | RestartGate::CurrentGenerationMissing
        | RestartGate::CurrentConfigurationMissing
        | RestartGate::ReviewDeliveryMissing
        | RestartGate::FreshOnlyRole => RestartPreviewClassification::FreshOnly,
    };
    let state = if matches!(
        classification,
        RestartPreviewClassification::AwaitingApproval
    ) {
        DecisionEvidenceState::Pending
    } else {
        DecisionEvidenceState::Stale
    };
    (
        classification,
        false,
        false,
        gate.reason_code().to_owned(),
        preview_blocker(
            gate.reason_code(),
            state,
            if matches!(
                classification,
                RestartPreviewClassification::AwaitingApproval
            ) {
                DecisionOwner::Human
            } else {
                gate.owner()
            },
            gate.message(),
            serde_json::json!({"candidate_state":facts.candidate_state.as_deref(),"session_status":facts.session_status.as_str(),"desired_running":facts.desired_running}),
        ),
    )
}

fn preview_trip_block(
    error: &str,
) -> (
    RestartPreviewClassification,
    bool,
    bool,
    String,
    DecisionPrerequisite,
) {
    (
        RestartPreviewClassification::Blocked,
        false,
        false,
        "restart.trip_not_ready".to_owned(),
        preview_blocker(
            "restart.trip_not_ready",
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            error,
            serde_json::json!({"ready":false}),
        ),
    )
}

fn preview_prerequisites(
    facts: &RestartSessionFacts,
    observation: &SessionProcessObservation,
    restore_hold: bool,
) -> Vec<DecisionPrerequisite> {
    let review_authorized = if facts.candidate_state.is_some() {
        facts.admission_review_authorized
    } else {
        facts.preparation_review_delivered
    };
    let human_holds_clear = facts.queue_open
        && facts.controls_clear
        && (matches!(facts.attention.as_str(), "none" | "restart_parked")
            || (facts.candidate_state.as_deref() == Some("failed")
                && facts.attention == "resume_failed"));
    vec![
        preview_blocker(
            "restart.restore_hold_absent",
            if restore_hold {
                DecisionEvidenceState::Pending
            } else {
                DecisionEvidenceState::Satisfied
            },
            DecisionOwner::Human,
            "database restore execution hold",
            serde_json::json!({"active":restore_hold}),
        ),
        preview_blocker(
            "restart.process_quiescence",
            match observation {
                SessionProcessObservation::Quiescent(_) => DecisionEvidenceState::Satisfied,
                SessionProcessObservation::Live(_) => DecisionEvidenceState::Pending,
                SessionProcessObservation::Uncertain(_) => DecisionEvidenceState::Uncertain,
            },
            DecisionOwner::Service,
            "fresh process evidence is a snapshot and must be revalidated at admission",
            observation_evidence(observation),
        ),
        preview_blocker(
            "restart.capture",
            if facts.candidate_state.is_some() {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Missing
            },
            DecisionOwner::Service,
            "durable restart capture",
            serde_json::json!({"candidate_state":facts.candidate_state.as_deref(),"candidate_source":facts.candidate_source.as_deref()}),
        ),
        preview_blocker(
            "restart.native_history",
            if facts.native_history {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Missing
            },
            DecisionOwner::Provider,
            "same-generation native history",
            serde_json::json!({"present":facts.native_history}),
        ),
        preview_blocker(
            "restart.current_supported_capability",
            if facts.current_capability && facts.capability_identity {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Stale
            },
            DecisionOwner::Human,
            "current Supported evidence for the frozen capability identity",
            serde_json::json!({"supported":facts.current_capability,"identity_recorded":facts.capability_identity}),
        ),
        preview_blocker(
            "restart.latest_attempt_and_generation",
            if facts.latest_attempt && facts.latest_generation {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Stale
            },
            DecisionOwner::Service,
            "latest attempt and role generation",
            serde_json::json!({"latest_attempt":facts.latest_attempt,"latest_generation":facts.latest_generation,"attempt_status":facts.attempt_status.as_str(),"generation_status":facts.generation_status.as_str()}),
        ),
        preview_blocker(
            "restart.current_configuration",
            if facts.current_configuration {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Stale
            },
            DecisionOwner::Human,
            "exact saved role configuration authority",
            serde_json::json!({"current":facts.current_configuration,"configuration_revision":facts.configuration_revision}),
        ),
        preview_blocker(
            "restart.review_delivery",
            if review_authorized {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Missing
            },
            DecisionOwner::Service,
            "role review delivery authority",
            serde_json::json!({"authorized":review_authorized,"role":facts.role.as_str()}),
        ),
        preview_blocker(
            "restart.trip_readiness",
            if facts.trip_readiness_error.is_none() {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Stale
            },
            DecisionOwner::Human,
            facts
                .trip_readiness_error
                .as_deref()
                .unwrap_or("current TRIP workflow readiness is established"),
            serde_json::json!({"ready":facts.trip_readiness_error.is_none()}),
        ),
        preview_blocker(
            "restart.human_and_queue_holds",
            if human_holds_clear {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Pending
            },
            DecisionOwner::Human,
            "project queue and manager controls",
            serde_json::json!({"queue_paused":!facts.queue_open,"control_held":!facts.controls_clear,"attention":facts.attention.as_str()}),
        ),
        preview_blocker(
            "restart.provider_authentication_and_reset",
            DecisionEvidenceState::Unknown,
            DecisionOwner::Provider,
            "provider authentication and reset limits are not probed by preview",
            serde_json::json!({"evaluated":false}),
        ),
        preview_blocker(
            "restart.launch_preflight",
            DecisionEvidenceState::Unknown,
            DecisionOwner::Service,
            "launch-time preflight is intentionally not executed by preview",
            serde_json::json!({"evaluated":false}),
        ),
    ]
}

fn observation_evidence(observation: &SessionProcessObservation) -> serde_json::Value {
    match observation {
        SessionProcessObservation::Quiescent(evidence)
        | SessionProcessObservation::Live(evidence) => evidence.clone(),
        SessionProcessObservation::Uncertain(reason) => {
            serde_json::json!({"state":"uncertain","reason":reason})
        }
    }
}

fn preview_blocker(
    code: &str,
    state: DecisionEvidenceState,
    owner: DecisionOwner,
    message: &str,
    evidence: serde_json::Value,
) -> DecisionPrerequisite {
    DecisionPrerequisite {
        code: code.to_owned(),
        state,
        owner,
        evidence,
        message: Some(message.to_owned()),
    }
}

pub fn verify_attempt_quiescent(store: &Store, attempt: &str) -> Result<serde_json::Value> {
    let (sessions, checks, claims, freezes) = {
        let connection = store.lock()?;
        let mut statement=connection.prepare("SELECT DISTINCT session_id FROM recovery_records WHERE attempt_id=?1 AND session_id IS NOT NULL AND state='attention_required'")?;
        let rows = statement
            .query_map(params![attempt], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut check_statement=connection.prepare("SELECT DISTINCT json_extract(detail_json,'$.check_id') FROM recovery_records WHERE attempt_id=?1 AND session_id IS NULL AND state='attention_required' AND json_extract(detail_json,'$.check_id') IS NOT NULL")?;
        let checks = check_statement
            .query_map(params![attempt], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut claim_statement = connection.prepare(
            "SELECT json_extract(detail_json,'$.claim_id'),json_extract(detail_json,'$.prior_state')
             FROM recovery_records WHERE attempt_id=?1 AND state='attention_required'
               AND json_extract(detail_json,'$.kind')='database_restore_claim'",
        )?;
        let claims = claim_statement
            .query_map(params![attempt], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut freeze_statement = connection.prepare(
            "SELECT freeze.id FROM recovery_records recovery
             JOIN freeze_intents freeze ON freeze.id=json_extract(recovery.detail_json,'$.freeze_id')
             WHERE recovery.attempt_id=?1 AND recovery.state='attention_required'
               AND json_extract(recovery.detail_json,'$.kind')='database_restore_freeze'
               AND freeze.state='recovery_required'",
        )?;
        let freezes = freeze_statement
            .query_map(params![attempt], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        (rows, checks, claims, freezes)
    };
    if sessions.is_empty() && checks.is_empty() && claims.is_empty() && freezes.is_empty() {
        bail!("attempt has no unresolved recorded process ownership")
    }
    let session_evidence = sessions
        .iter()
        .map(|session| verify_session_quiescent(store, session))
        .collect::<Result<Vec<_>>>()?;
    let check_evidence = checks
        .iter()
        .map(|check| verify_check_quiescent(store, check))
        .collect::<Result<Vec<_>>>()?;
    let claim_evidence = claims
        .iter()
        .map(|(claim, prior_state)| {
            if prior_state != "reserved" {
                bail!(
                    "claim {claim} was {prior_state} without session or check ownership; quiescence is unknown"
                )
            }
            Ok(serde_json::json!({
                "claim_id":claim,
                "source":"restore_recorded_prelaunch_reservation"
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({
        "attempt_id":attempt,
        "sessions":session_evidence,
        "checks":check_evidence,
        "claims":claim_evidence,
        "freeze_intents":freezes.iter().map(|freeze|serde_json::json!({
            "freeze_id":freeze,
            "source":"restore_recorded_local_freeze_interruption"
        })).collect::<Vec<_>>(),
    }))
}

pub fn verify_check_quiescent(store: &Store, check_id: &str) -> Result<serde_json::Value> {
    let (recorded, group_anchor, anchor, launch_boot) = {
        let connection = store.lock()?;
        let recorded = check_identities(&connection, check_id)?;
        let (group, anchor, boot) = connection.query_row(
            "SELECT recovery_process_group_id,recovery_anchor_json,launch_boot_identity FROM check_runs WHERE id=?1",
            params![check_id],
            |row| Ok((row.get::<_, Option<i32>>(0)?,row.get::<_, Option<String>>(1)?,row.get::<_, Option<String>>(2)?)),
        )?;
        (recorded, group, anchor, boot)
    };
    let inventory = process_inventory()?;
    let evidence = verify_generation_absent(
        "check",
        check_id,
        &recorded,
        group_anchor,
        anchor.as_deref(),
        launch_boot.as_deref(),
        &inventory,
    )?;
    Ok(serde_json::json!({"check_id":check_id,"verification":evidence}))
}

fn verify_generation_absent(
    kind: &str,
    id: &str,
    recorded: &[(u32, String, i32)],
    group: Option<i32>,
    anchor_json: Option<&str>,
    launch_boot: Option<&str>,
    inventory: &[(u32, i32, String)],
) -> Result<serde_json::Value> {
    let current_boot = crate::supervisor::system_boot_identity()?;
    let anchor = anchor_json
        .map(serde_json::from_str::<crate::domain::ProcessGenerationAnchor>)
        .transpose()
        .map_err(|error| {
            anyhow::anyhow!("{kind} {id} has a malformed generation anchor: {error}")
        })?;
    verify_generation_absent_evidence(
        kind,
        id,
        recorded,
        group,
        anchor.as_ref(),
        launch_boot,
        &current_boot,
        inventory,
    )
}

pub fn verify_generation_absent_evidence(
    kind: &str,
    _id: &str,
    recorded: &[(u32, String, i32)],
    group: Option<i32>,
    anchor: Option<&crate::domain::ProcessGenerationAnchor>,
    launch_boot: Option<&str>,
    current_boot: &str,
    inventory: &[(u32, i32, String)],
) -> Result<serde_json::Value> {
    let recorded_boot = anchor
        .map(|value| value.boot_identity.as_str())
        .or(launch_boot);
    if recorded_boot
        .is_some_and(|boot| crate::supervisor::boot_identity_proves_reboot(boot, current_boot))
    {
        return Ok(
            serde_json::json!({"source":"operating_system_boot_changed","recorded_boot":recorded_boot,"current_boot":current_boot,"recorded_identities":recorded.len()}),
        );
    }
    let matching = inventory
        .iter()
        .filter(|(pid, _, start)| {
            recorded
                .iter()
                .any(|item| item.0 == *pid && item.1 == *start)
        })
        .collect::<Vec<_>>();
    if !matching.is_empty() {
        bail!(
            "recorded {kind} process identities remain live: {}",
            serde_json::to_string(&matching)?
        )
    }
    if let Some(anchor) = anchor {
        if anchor.process_group_id <= 0 || anchor.pid as i32 != anchor.process_group_id {
            bail!("the recorded {kind} generation anchor is not a process-group leader; same-boot absence is unknown")
        }
        if let Some(current) = inventory.iter().find(|item| item.0 == anchor.pid) {
            if current.2 == anchor.native_start_marker {
                bail!("the original {kind} generation anchor remains live, including any process-group drift")
            }
            if current.1 == anchor.process_group_id {
                if current.0 as i32 == current.1 {
                    return Ok(
                        serde_json::json!({"source":"process_group_leader_generation_replaced","anchor_pid":anchor.pid,"process_group_id":anchor.process_group_id,"replacement_start":current.2,"recorded_identities":recorded.len()}),
                    );
                }
                bail!("the {kind} anchor PID has a different start identity but is not positively observed as the replacement group leader; absence is uncertain")
            }
            let occupied = inventory
                .iter()
                .filter(|item| item.1 == anchor.process_group_id)
                .collect::<Vec<_>>();
            if !occupied.is_empty() {
                bail!("{kind} anchor PID was reused in another group while the recorded group remains occupied; absence is uncertain: {}", serde_json::to_string(&occupied)?)
            }
            return Ok(
                serde_json::json!({"source":"generation_anchor_pid_reused_group_absent","anchor_pid":anchor.pid,"process_group_id":anchor.process_group_id,"recorded_identities":recorded.len()}),
            );
        }
        let occupied = inventory
            .iter()
            .filter(|item| item.1 == anchor.process_group_id)
            .collect::<Vec<_>>();
        if !occupied.is_empty() {
            bail!("{kind} generation anchor is absent but its process group remains occupied; absence is uncertain: {}", serde_json::to_string(&occupied)?)
        }
        return Ok(
            serde_json::json!({"source":"generation_anchor_and_group_absent","anchor_pid":anchor.pid,"process_group_id":anchor.process_group_id,"recorded_identities":recorded.len()}),
        );
    }
    let group = group.ok_or_else(|| anyhow::anyhow!(
        "no durable process identities, generation anchor, process-group anchor, or prior-boot proof exist for this {kind}; quiescence is unknown"
    ))?;
    let occupied = inventory
        .iter()
        .filter(|item| item.1 == group)
        .collect::<Vec<_>>();
    if !occupied.is_empty() {
        bail!(
            "the unversioned {kind} process group is occupied; quiescence is unknown: {}",
            serde_json::to_string(&occupied)?
        )
    }
    bail!("the numeric {kind} process group is empty, but no generation-bound anchor proves same-boot absence")
}

fn check_identities(
    connection: &rusqlite::Connection,
    check_id: &str,
) -> Result<Vec<(u32, String, i32)>> {
    let mut statement = connection.prepare(
        "SELECT pid,native_start_marker,process_group_id FROM check_processes WHERE check_id=?1",
    )?;
    let rows = statement
        .query_map(params![check_id], |row| {
            Ok((
                row.get::<_, i64>(0)? as u32,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn process_inventory() -> Result<Vec<(u32, i32, String)>> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid=,lstart="])
        .output()?;
    if !output.status.success() {
        bail!("process inventory failed; quiescence remains unknown")
    }
    parse_process_inventory(&output.stdout)
}

pub fn parse_process_inventory(output: &[u8]) -> Result<Vec<(u32, i32, String)>> {
    let mut values = Vec::new();
    for line in String::from_utf8(output.to_vec())?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 7 {
            bail!("process inventory contained a malformed row; quiescence remains unknown")
        }
        let (Ok(pid), Ok(pgid)) = (fields[0].parse::<u32>(), fields[1].parse::<i32>()) else {
            bail!("process inventory contained an unparseable identity row; quiescence remains unknown")
        };
        values.push((pid, pgid, fields[2..7].join(" ")));
    }
    if values.is_empty() {
        bail!("process inventory was empty; quiescence remains unknown")
    }
    Ok(values)
}
