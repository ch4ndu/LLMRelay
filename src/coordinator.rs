use crate::domain::{
    DecisionActionBinding, DecisionControlPolicy, DecisionDisposition, DecisionEvidenceState,
    DecisionExplanation, DecisionNextAction, DecisionObservedRevision, DecisionOwner,
    DecisionOwnership, DecisionPrerequisite, DecisionSubject, RoleKind, DECISION_SCHEMA_V1,
};
use crate::operations::Application;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

/// Executes at most one durable workflow action. The foreground server calls this
/// repeatedly; the public scheduler command calls the same seam for deterministic
/// inspection and capability exercises.
pub fn tick(app: &Application) -> Result<serde_json::Value> {
    if let Some(value) =
        reconcile_one_setup_retained_first_turn_stop_timeout(&app.store, &app.supervisor)?
    {
        return Ok(value);
    }
    app.supervisor.reconcile()?;
    crate::permissions::expire_deadlines(&app.store)?;
    if !app.dispatch_enabled() {
        return Ok(serde_json::json!({"action":"draining","status":app.drain_status()?}));
    }
    if let Some(value) = reconcile_one_codex_stop_idle(app)? {
        return Ok(value);
    }
    if let Some(value) = complete_one_setup_retained_first_turn(app)? {
        return Ok(value);
    }
    if let Some(value) = process_one_manager_control(app)? {
        return Ok(value);
    }
    if let Some(value) = app.auto_resume_one()? {
        return Ok(value);
    }
    if let Some(value) = quiesce_retired_attempt_role(app)? {
        return Ok(value);
    }
    if let Some(value) = match process_one_control(app) {
        Ok(value) => value,
        Err(error) => disposition_selected_control_failure(app, &error)?,
    } {
        if value.get("action").and_then(|action| action.as_str()) != Some("one_step_enabled")
            && is_committed_action(&value)
        {
            if let Some(attempt) = value.get("attempt_id").and_then(|value| value.as_str()) {
                mark_attempt_served(app, attempt)?;
            }
        }
        return Ok(value);
    }
    if let Some(value) = advance_one_switch(app)? {
        return Ok(value);
    }
    if let Some(value) = retry_one_guidance(app)? {
        if value
            .get("engine_generated")
            .and_then(|value| value.as_bool())
            == Some(true)
            && value.get("state").and_then(|value| value.as_str()) != Some("queued")
        {
            if let Some(attempt) = value.get("attempt_id").and_then(|value| value.as_str()) {
                mark_attempt_served(app, attempt)?;
            }
        }
        return Ok(value);
    }
    if let Some(value) = retry_one_rework(app)? {
        return Ok(value);
    }
    let mut visible_wait: Option<(serde_json::Value, DecisionExplanation)> = None;
    let mut fallback_decision: Option<DecisionExplanation> = None;
    let attempt_ids = active_attempts(app)?
        .into_iter()
        .map(|attempt| attempt.id)
        .collect::<Vec<_>>();
    for attempt_id in attempt_ids {
        let Some((attempt, evaluated)) = classify_attempt_for_tick(app, &attempt_id)? else {
            continue;
        };
        let AttemptDecision {
            legacy,
            explanation,
            flow,
            advance,
        } = evaluated;
        match flow {
            AttemptDecisionFlow::Skip => {
                if explanation.reason_code == "workflow.manager_control_active"
                    && visible_wait.is_none()
                {
                    visible_wait = Some((legacy, explanation));
                } else if fallback_decision.is_none() {
                    fallback_decision = Some(explanation);
                }
                continue;
            }
            AttemptDecisionFlow::Observe => {}
        }
        let advanced = match advance {
            AttemptAdvance::None => Ok(legacy),
            AttemptAdvance::ConsumeBlockedResult => match consume_blocked_result(app, &attempt.id)?
            {
                Some(value) => return Ok(value),
                None => Ok(legacy),
            },
            AttemptAdvance::Planning(selection) => advance_planning(app, &attempt, selection),
            AttemptAdvance::Review(selection) => advance_review(app, &attempt, selection),
            AttemptAdvance::Implementation(selection) => {
                advance_implementation(app, &attempt, selection)
            }
            AttemptAdvance::Checks(selection) => advance_checks(app, &attempt, selection),
            AttemptAdvance::Handoff(selection) => advance_handoff(app, &attempt, selection),
        };
        let value = match advanced {
            Ok(value) => value,
            Err(error) if format!("{error:#}").contains("capacity") => {
                let refreshed = refreshed_attempt_decision(app, &attempt.id)?;
                if fallback_decision.is_none() {
                    fallback_decision = refreshed;
                }
                continue;
            }
            Err(error) if format!("{error:#}").contains("attempt is not dispatchable") => {
                let refreshed = refreshed_attempt_decision(app, &attempt.id)?;
                if fallback_decision.is_none() {
                    fallback_decision = refreshed;
                }
                continue;
            }
            Err(error) if format!("{error:#}").contains("capability") => {
                set_attention(app, &attempt.task_id, &attempt.id, "blocked")?;
                let refreshed = refreshed_attempt_decision(app, &attempt.id)?;
                if fallback_decision.is_none() {
                    fallback_decision = refreshed;
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        if is_committed_action(&value) {
            mark_attempt_served(app, &attempt.id)?;
            return Ok(value);
        }
        let explanation = refreshed_attempt_decision(app, &attempt.id)?.unwrap_or_else(|| {
            unknown_phase_decision(&attempt, Vec::new(), "workflow.attempt_no_longer_active")
                .explanation
        });
        if legacy_wait_is_visible(&value) {
            if visible_wait.is_none() {
                visible_wait = Some((value, explanation));
            }
        } else if fallback_decision.is_none() {
            fallback_decision = Some(explanation);
        }
    }
    let scheduler = app.scheduler.claim_next_with_decision()?;
    if let Some(plan) = scheduler.plan {
        return Ok(
            serde_json::json!({"action":"workspace_created","task_id":plan.task_id,"attempt_id":plan.attempt_id}),
        );
    }
    if let Some((legacy, explanation)) = visible_wait {
        return attach_decision(legacy, &explanation);
    }
    if let Some(explanation) = fallback_decision {
        return attach_decision(serde_json::json!({"action":"idle"}), &explanation);
    }
    if let Some(explanation) = scheduler.decision {
        return attach_decision(serde_json::json!({"action":"idle"}), &explanation);
    }
    Ok(serde_json::json!({"action":"idle"}))
}

pub(crate) fn reconcile_one_setup_retained_first_turn_stop_timeout(
    store: &crate::store::Store,
    supervisor: &crate::supervisor::Supervisor,
) -> Result<Option<serde_json::Value>> {
    let Some(receipt) = store.setup_retained_first_turn_stop_timeout_candidate()? else {
        return Ok(None);
    };
    Ok(
        match supervisor.reconcile_setup_retained_first_turn_stop_timeout(&receipt)? {
            crate::supervisor::SetupRetainedFirstTurnStopTimeoutOutcome::Quiescent => {
                Some(serde_json::json!({
                    "action":"setup_retained_first_turn_stop_quiescent",
                    "session_id":receipt.session_id,
                    "state":"exited",
                    "automatic_resignal":false
                }))
            }
            crate::supervisor::SetupRetainedFirstTurnStopTimeoutOutcome::TimedOut => {
                Some(serde_json::json!({
                    "action":"setup_retained_first_turn_stop_timed_out",
                    "session_id":receipt.session_id,
                    "state":"recovery_required",
                    "automatic_resignal":false
                }))
            }
            crate::supervisor::SetupRetainedFirstTurnStopTimeoutOutcome::Stale => None,
        },
    )
}

fn reconcile_one_codex_stop_idle(app: &Application) -> Result<Option<serde_json::Value>> {
    let receipt = {
        let connection = app.store.lock()?;
        crate::store::eligible_codex_stop_idle_reconciliation(&connection, None)?
    };
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    if !matches!(
        app.retained_session_native_idle_ready(&receipt.session_id),
        Ok(true)
    ) {
        return Ok(None);
    }
    if !app.store.mark_codex_stop_idle_candidate(&receipt)? {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "action":"codex_stop_idle_reconciled",
        "session_id":receipt.session_id,
        "role_generation_id":receipt.generation_id,
        "transcript_epoch":receipt.transcript_epoch,
        "stop_event_rowid":receipt.stop_rowid,
        "state":"idle_candidate",
        "completion_inferred":false
    })))
}

fn complete_one_setup_retained_first_turn(app: &Application) -> Result<Option<serde_json::Value>> {
    let receipt = {
        let connection = app.store.lock()?;
        crate::store::eligible_setup_retained_first_turn_stop(&connection, None)?
    };
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    if !matches!(
        app.retained_session_native_idle_ready(&receipt.session_id),
        Ok(true)
    ) {
        return Ok(None);
    }
    let outcome = app.request_setup_retained_first_turn_stop(&receipt)?;
    if outcome == crate::supervisor::InterruptOutcome::Stale {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "action":if outcome == crate::supervisor::InterruptOutcome::Requested {
            "setup_retained_first_turn_stop_requested"
        } else {
            "setup_retained_first_turn_stop_already_requested"
        },
        "session_id":receipt.session_id,
        "role_generation_id":receipt.generation_id,
        "transcript_epoch":receipt.transcript_epoch,
        "stop_event_rowid":receipt.stop_rowid,
        "state":"interrupt_requested",
        "resume_spent":false,
        "replacement_control_created":false,
        "signal_attempted":outcome == crate::supervisor::InterruptOutcome::Requested
    })))
}

fn quiesce_retired_attempt_role(app: &Application) -> Result<Option<serde_json::Value>> {
    let retired = {
        let connection = app.store.lock()?;
        connection.query_row(
            "SELECT s.id,a.id,rg.role FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE a.status IN ('done','cancelled','rework_staging','reworked') AND s.status='running' ORDER BY s.created_at LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        ).optional()?
    };
    let Some((session, attempt, role)) = retired else {
        return Ok(None);
    };
    app.supervisor.interrupt(&session)?;
    Ok(Some(
        serde_json::json!({"action":"retired_role_interrupt","session_id":session,"attempt_id":attempt,"role":role}),
    ))
}

#[derive(Clone)]
struct Attempt {
    id: String,
    task_id: String,
    project_id: String,
    phase: String,
    status: String,
    attention: String,
    task_version: i64,
    configuration_revision: i64,
    selected_checks_revision: i64,
    title: String,
    description: String,
    criteria: serde_json::Value,
    plan_hash: Option<String>,
    candidate_hash: Option<String>,
}

struct LaneDispatch {
    lane_key: String,
    profile_session: String,
    prompt_context: serde_json::Value,
}

struct ImplementationLaneState {
    configured: i64,
    unyielded: i64,
    request_id: Option<String>,
    capsule: Option<String>,
}

const INTEGRATION_READY_NOTICE: &str = "All required implementation lanes have accepted persisted yield receipts and their exact current writers are positively quiescent. The exact ordered integration request can now proceed through request-integration; do not fabricate a request, report, or approval.";
const CANDIDATE_FROZEN_NOTICE: &str =
    "Implementation candidate is frozen. Inspect the evidence and propose phase code_review.";
const CONFORMANCE_NOTICE: &str = "Required checks are current. Load current role context and submit exact candidate-bound manager conformance after confirming every required lane is yielded and writer-quiescent. Do not fabricate evidence or clear a human hold.";
const FINAL_EXPLORER_NOTICE: &str = "Manager conformance is current. Load current role context and record the exact candidate-bound final Explorer activation decision. Do not bypass an activated Explorer or any human authorization gate.";

fn active_attempts(app: &Application) -> Result<Vec<Attempt>> {
    let connection = app.store.lock()?;
    active_attempts_from_connection(&connection)
}

fn active_attempts_from_connection(connection: &Connection) -> Result<Vec<Attempt>> {
    attempts_from_connection(connection, false)
}

fn attempts_from_connection(
    connection: &Connection,
    include_skipped: bool,
) -> Result<Vec<Attempt>> {
    let mut statement = connection.prepare(
        "SELECT a.id,a.task_id,t.project_id,a.phase,a.status,t.attention,t.version,
                a.configuration_revision,a.selected_checks_revision,t.title,t.description,
                t.acceptance_criteria_json,a.plan_hash,a.candidate_hash
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         WHERE t.lifecycle IN ('in_progress','validation','awaiting_review')
           AND (a.status IN ('running','workspace_reserved','needs_input')
             OR (?1=1 AND a.status='materialization_pending'
               AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=a.task_id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
               AND EXISTS(SELECT 1 FROM rework_intents ri WHERE ri.new_attempt_id=a.id
                 AND ri.state NOT IN ('completed','cancelled')))
             OR (?1=1 AND a.status='restart_parked'
               AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=a.task_id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1))
             OR (a.status IN ('held','needs_recovery')
               AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=a.task_id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
               AND (?1=1 OR t.attention!='none'
                 OR EXISTS(SELECT 1 FROM restart_candidates candidate
                   WHERE candidate.attempt_id=a.id
                     AND candidate.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
                 OR EXISTS(SELECT 1 FROM controls control WHERE control.attempt_id=a.id
                   AND control.kind IN ('manager_stop','manager_change')
                   AND control.state NOT IN ('finished','cancelled','superseded','rejected')))))
         ORDER BY COALESCE(a.last_coordinator_at,''),a.created_at",
    )?;
    let rows = statement
        .query_map(params![include_skipped], |row| {
            Ok(Attempt {
                id: row.get(0)?,
                task_id: row.get(1)?,
                project_id: row.get(2)?,
                phase: row.get(3)?,
                status: row.get(4)?,
                attention: row.get(5)?,
                task_version: row.get(6)?,
                configuration_revision: row.get(7)?,
                selected_checks_revision: row.get(8)?,
                title: row.get(9)?,
                description: row.get(10)?,
                criteria: serde_json::from_str(&row.get::<_, String>(11)?).unwrap_or_default(),
                plan_hash: row.get(12)?,
                candidate_hash: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AttemptDecisionFlow {
    Skip,
    Observe,
}

enum AttemptAdvance {
    None,
    ConsumeBlockedResult,
    Planning(PlanningSelection),
    Review(ReviewSelection),
    Implementation(ImplementationSelection),
    Checks(ChecksSelection),
    Handoff(HandoffSelection),
}

enum PlanningSelection {
    FreezePlan,
    QuiesceCompletedManager { session_id: String },
    WaitForManagerPlan,
    DispatchManager,
    ApplyManagerProposal,
    WaitForManagerProposal,
}

struct ReviewSelection {
    kind: &'static str,
    role: RoleKind,
    action: ReviewAction,
}

enum ReviewAction {
    QuiesceCompleted { session_id: String },
    ConsumeResult,
    Dispatch,
    Wait { request_id: String, state: String },
}

enum ImplementationSelection {
    ApplyManagerProposal,
    WaitForManagerStop {
        session_id: String,
    },
    DispatchManager,
    WaitForLaneAdmission {
        reviewed_lanes: usize,
    },
    HandleYieldedWriter(YieldedLaneWriterState),
    DispatchLane {
        dispatch: LaneDispatch,
        configured: i64,
        unyielded: i64,
    },
    WaitForRequiredYields {
        configured: i64,
        unyielded: i64,
    },
    NotifyIntegrationReady,
    WaitForIntegrationRequest {
        configured: i64,
    },
    DispatchIntegration {
        request_id: String,
        capsule: String,
    },
    QuiesceCompletedImplementer {
        session_id: String,
    },
    FreezeCandidate,
    DispatchImplementer,
    StopManagerForCandidate,
    WaitForManagerTransition,
}

enum ChecksSelection {
    MissingCandidate,
    CheckFailed { check_id: String, status: String },
    RunCheck { check_id: String },
    NoSelectedChecks,
    CheckExecutionUnsettled,
    WaitForManagerStop { session_id: String },
    DispatchConformanceManager,
    StopManagerForConformance,
    ConformanceMissing,
    DispatchFinalExplorerManager,
    StopManagerForFinalExplorer,
    WaitForFinalExplorerDecision,
    WaitForActivatedExplorer,
    Complete(ManagerCompletionBoundary),
}

enum HandoffSelection {
    Complete(ManagerCompletionBoundary),
    WaitForManagerStop { session_id: String },
    DispatchManager,
    StopManager,
    WaitForManagerHandoff,
}

struct AttemptDecision {
    legacy: serde_json::Value,
    explanation: DecisionExplanation,
    flow: AttemptDecisionFlow,
    advance: AttemptAdvance,
}

pub(crate) fn read_only_decisions(connection: &Connection) -> Result<Vec<DecisionExplanation>> {
    let mut decisions = attempts_from_connection(connection, true)?
        .into_iter()
        .map(|attempt| Ok(evaluate_attempt(connection, &attempt)?.explanation))
        .collect::<Result<Vec<_>>>()?;
    decisions.extend(crate::recovery::read_only_restart_decisions(connection)?);
    Ok(decisions)
}

fn evaluate_attempt(connection: &Connection, attempt: &Attempt) -> Result<AttemptDecision> {
    let mut decision = evaluate_attempt_owner(connection, attempt)?;
    let controls =
        crate::workflow::ordinary_control_policy(connection, &attempt.task_id, Some(&attempt.id))?;
    for control in controls {
        if !decision
            .explanation
            .control_policy
            .allowed_controls
            .contains(&control)
        {
            decision
                .explanation
                .control_policy
                .allowed_controls
                .push(control);
        }
    }
    Ok(decision)
}

fn evaluate_attempt_owner(connection: &Connection, attempt: &Attempt) -> Result<AttemptDecision> {
    let mut prerequisites = Vec::new();
    if attempt.attention != "none" {
        let owner = if attempt.attention == "queued_capacity" {
            DecisionOwner::Service
        } else {
            DecisionOwner::Human
        };
        return Ok(blocked_attempt_decision(
            attempt,
            "workflow.task_attention_required",
            if attempt.attention == "queued_capacity" {
                DecisionDisposition::RetryDeferred
            } else {
                DecisionDisposition::Held
            },
            DecisionEvidenceState::Pending,
            owner,
            serde_json::json!({"attention":attempt.attention}),
            None,
            prerequisites,
            serde_json::json!({
                "action":"held",
                "attempt_id":attempt.id,
                "for":"task_attention",
                "attention":attempt.attention,
            }),
            AttemptDecisionFlow::Skip,
            None,
            Vec::new(),
        ));
    }
    prerequisites.push(attempt_prerequisite(
        "workflow.task_attention_clear",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({"attention":"none"}),
        None,
    ));
    let restart: Option<(String, String, Option<String>)> = connection
        .query_row(
            "SELECT candidate.session_id,candidate.state,session.role_generation_id
             FROM restart_candidates candidate
             LEFT JOIN sessions session ON session.id=candidate.session_id
             WHERE candidate.attempt_id=?1
               AND candidate.state NOT IN ('resumed','released_fresh_dispatch','cancelled')
             ORDER BY candidate.created_at,candidate.session_id LIMIT 1",
            params![attempt.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((session_id, state, role_generation_id)) = restart {
        let mut decision = blocked_attempt_decision(
            attempt,
            "restart.hold_active",
            DecisionDisposition::Held,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"session_id":session_id,"state":state}),
            None,
            prerequisites,
            serde_json::json!({
                "action":"held",
                "attempt_id":attempt.id,
                "for":"restart_candidate",
                "session_id":session_id,
                "state":state,
            }),
            AttemptDecisionFlow::Skip,
            None,
            Vec::new(),
        );
        decision.explanation.subject.session_id = Some(session_id.clone());
        decision.explanation.subject.role_generation_id = role_generation_id.clone();
        decision.explanation.ownership.binding.session_id = Some(session_id);
        decision.explanation.ownership.binding.role_generation_id = role_generation_id;
        return Ok(decision);
    }
    prerequisites.push(attempt_prerequisite(
        "restart.hold_absent",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Service,
        serde_json::json!({"attempt_id":attempt.id}),
        None,
    ));
    let manager_control: Option<(String, String, String, String, Option<String>)> = connection
        .query_row(
            "SELECT id,kind,state,payload_json,role_generation_id FROM controls WHERE attempt_id=?1
               AND kind IN ('manager_stop','manager_change')
               AND state NOT IN ('finished','cancelled','superseded','rejected')
             ORDER BY created_at,id LIMIT 1",
            params![attempt.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    if let Some((control_id, kind, state, payload, role_generation_id)) = manager_control {
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
        let mut decision = blocked_attempt_decision(
            attempt,
            "workflow.manager_control_active",
            DecisionDisposition::Held,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"control_id":control_id,"kind":kind,"state":state}),
            None,
            prerequisites,
            serde_json::json!({
                "action":"held",
                "attempt_id":attempt.id,
                "for":"manager_control",
                "control_id":control_id,
                "kind":kind,
                "state":state,
                "next_action":payload.get("next_action").cloned().unwrap_or(serde_json::Value::Null),
            }),
            AttemptDecisionFlow::Skip,
            None,
            Vec::new(),
        );
        decision.explanation.subject.role_generation_id = role_generation_id.clone();
        decision.explanation.ownership.binding.control_id = Some(control_id);
        decision.explanation.ownership.binding.role_generation_id = role_generation_id;
        return Ok(decision);
    }
    prerequisites.push(attempt_prerequisite(
        "workflow.manager_control_absent",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Service,
        serde_json::json!({"attempt_id":attempt.id}),
        None,
    ));
    if attempt.status == "materialization_pending" {
        let rework_state: Option<String> = connection
            .query_row(
                "SELECT state FROM rework_intents WHERE new_attempt_id=?1
                 AND state NOT IN ('completed','cancelled')",
                params![attempt.id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(rework_state) = rework_state {
            return Ok(blocked_attempt_decision(
                attempt,
                "workflow.rework_materialization_pending",
                DecisionDisposition::Waiting,
                DecisionEvidenceState::Pending,
                DecisionOwner::Service,
                serde_json::json!({"status":attempt.status,"rework_state":rework_state}),
                None,
                prerequisites,
                serde_json::json!({"action":"held","attempt_id":attempt.id,"for":"rework_materialization","state":rework_state}),
                AttemptDecisionFlow::Skip,
                None,
                Vec::new(),
            ));
        }
    }
    if matches!(attempt.status.as_str(), "held" | "needs_recovery") {
        return Ok(blocked_attempt_decision(
            attempt,
            if attempt.status == "needs_recovery" {
                "workflow.attempt_needs_recovery"
            } else {
                "workflow.attempt_held"
            },
            DecisionDisposition::Held,
            DecisionEvidenceState::Pending,
            DecisionOwner::Human,
            serde_json::json!({"status":attempt.status,"phase":attempt.phase}),
            None,
            prerequisites,
            serde_json::json!({"action":"held","attempt_id":attempt.id,"status":attempt.status}),
            AttemptDecisionFlow::Skip,
            None,
            Vec::new(),
        ));
    }
    if let Err(error) = crate::trip::require_project_ready(connection, &attempt.project_id) {
        return Ok(blocked_attempt_decision(
            attempt,
            "workflow.project_readiness_stale",
            DecisionDisposition::Held,
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            serde_json::json!({"project_id":attempt.project_id}),
            Some(format!("{error:#}")),
            prerequisites,
            serde_json::json!({
                "action":"held",
                "attempt_id":attempt.id,
                "for":"project_trip_readiness",
                "reason":format!("{error:#}"),
            }),
            AttemptDecisionFlow::Skip,
            Some(DecisionNextAction {
                operation: "inspect_project".into(),
                enabled: true,
                owner: DecisionOwner::Human,
                binding: DecisionActionBinding {
                    project_id: Some(attempt.project_id.clone()),
                    ..DecisionActionBinding::default()
                },
                accounting_note: None,
            }),
            vec!["inspect_project".into()],
        ));
    }
    prerequisites.push(attempt_prerequisite(
        "workflow.project_ready",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({"project_id":attempt.project_id}),
        None,
    ));
    if let Err(error) = crate::trip::require_attempt_ready(connection, &attempt.id, None) {
        return Ok(blocked_attempt_decision(
            attempt,
            "workflow.attempt_readiness_stale",
            DecisionDisposition::Held,
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            serde_json::json!({"attempt_id":attempt.id}),
            Some(format!("{error:#}")),
            prerequisites,
            serde_json::json!({
                "action":"held",
                "attempt_id":attempt.id,
                "for":"project_trip_readiness",
                "reason":format!("{error:#}"),
            }),
            AttemptDecisionFlow::Skip,
            None,
            Vec::new(),
        ));
    }
    prerequisites.push(attempt_prerequisite(
        "workflow.attempt_ready",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({"attempt_id":attempt.id}),
        None,
    ));
    if eligible_blocked_result(connection, &attempt.id)?.is_some() {
        let mut decision =
            ready_attempt_decision(attempt, prerequisites, "workflow.blocked_result_available");
        decision.advance = AttemptAdvance::ConsumeBlockedResult;
        return Ok(decision);
    }
    match attempt.phase.as_str() {
        "awaiting_plan_approval" => {
            let Some(plan_hash) = attempt.plan_hash.clone() else {
                return Ok(blocked_attempt_decision(
                    attempt,
                    "workflow.plan_snapshot_missing",
                    DecisionDisposition::Held,
                    DecisionEvidenceState::Missing,
                    DecisionOwner::Service,
                    serde_json::json!({"attempt_id":attempt.id}),
                    None,
                    prerequisites,
                    waiting(attempt, "human_plan_approval"),
                    AttemptDecisionFlow::Skip,
                    None,
                    Vec::new(),
                ));
            };
            Ok(blocked_attempt_decision(
                attempt,
                "workflow.awaiting_human_plan_approval",
                DecisionDisposition::Waiting,
                DecisionEvidenceState::Pending,
                DecisionOwner::Human,
                serde_json::json!({"plan_hash":plan_hash}),
                None,
                prerequisites,
                waiting(attempt, "human_plan_approval"),
                AttemptDecisionFlow::Skip,
                Some(DecisionNextAction {
                    operation: "approve_plan".into(),
                    enabled: true,
                    owner: DecisionOwner::Human,
                    binding: DecisionActionBinding {
                        project_id: Some(attempt.project_id.clone()),
                        task_id: Some(attempt.task_id.clone()),
                        attempt_id: Some(attempt.id.clone()),
                        expected_task_version: Some(attempt.task_version),
                        plan_hash: Some(plan_hash),
                        ..DecisionActionBinding::default()
                    },
                    accounting_note: None,
                }),
                vec!["approve_plan".into()],
            ))
        }
        "awaiting_implementation_authorization" => Ok(blocked_attempt_decision(
            attempt,
            "workflow.awaiting_human_implementation_authorization",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Human,
            serde_json::json!({"plan_hash":attempt.plan_hash}),
            None,
            prerequisites,
            waiting(attempt, "human_implementation_authorization"),
            AttemptDecisionFlow::Skip,
            Some(DecisionNextAction {
                operation: "authorize_implementation".into(),
                enabled: attempt.plan_hash.is_some(),
                owner: DecisionOwner::Human,
                binding: attempt_binding(attempt),
                accounting_note: None,
            }),
            vec!["authorize_implementation".into()],
        )),
        "awaiting_human_review" => Ok(blocked_attempt_decision(
            attempt,
            "workflow.awaiting_human_review",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Human,
            serde_json::json!({"candidate_hash":attempt.candidate_hash}),
            None,
            prerequisites,
            waiting(attempt, "human_accept_or_rework"),
            AttemptDecisionFlow::Skip,
            None,
            vec!["human_review".into()],
        )),
        "plan_review" | "code_review" | "final_review" => {
            evaluate_review(connection, attempt, prerequisites)
        }
        "planning" => evaluate_planning(connection, attempt, prerequisites),
        "implementation" => evaluate_implementation(connection, attempt, prerequisites),
        "checks" => evaluate_checks(connection, attempt, prerequisites),
        "manager_handoff" => evaluate_handoff(connection, attempt, prerequisites),
        other => Ok(blocked_attempt_decision(
            attempt,
            "workflow.phase_unknown",
            DecisionDisposition::Held,
            DecisionEvidenceState::Unknown,
            DecisionOwner::Service,
            serde_json::json!({"phase":other}),
            None,
            prerequisites,
            serde_json::json!({"action":"held","attempt_id":attempt.id,"phase":other}),
            AttemptDecisionFlow::Observe,
            None,
            Vec::new(),
        )),
    }
}

fn classify_planning(connection: &Connection, attempt: &Attempt) -> Result<PlanningSelection> {
    if attempt.plan_hash.is_none() {
        if crate::store::eligible_manager_plan(connection, &attempt.id, true)?.is_some() {
            return Ok(PlanningSelection::FreezePlan);
        }
        if let Some(candidate) =
            crate::store::eligible_manager_plan(connection, &attempt.id, false)?
        {
            if candidate.session_status == "running"
                && candidate.readiness_state != "idle_candidate"
            {
                return Ok(PlanningSelection::QuiesceCompletedManager {
                    session_id: candidate.session_id,
                });
            }
        }
        return Ok(if active_role_on(connection, &attempt.id, "manager")? {
            PlanningSelection::WaitForManagerPlan
        } else {
            PlanningSelection::DispatchManager
        });
    }
    Ok(
        if eligible_manager_transition(connection, attempt, "plan_review")?.is_some() {
            PlanningSelection::ApplyManagerProposal
        } else {
            PlanningSelection::WaitForManagerProposal
        },
    )
}

fn evaluate_planning(
    connection: &Connection,
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
) -> Result<AttemptDecision> {
    let selection = classify_planning(connection, attempt)?;
    let mut decision = match &selection {
        PlanningSelection::FreezePlan | PlanningSelection::QuiesceCompletedManager { .. } => {
            ready_attempt_decision(
                attempt,
                prerequisites,
                "workflow.manager_plan_result_available",
            )
        }
        PlanningSelection::WaitForManagerPlan => blocked_attempt_decision(
            attempt,
            "workflow.current_manager_plan_pending",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"role":"manager"}),
            None,
            prerequisites,
            waiting(attempt, "current_manager_plan"),
            AttemptDecisionFlow::Observe,
            None,
            vec!["manager_stop".into(), "manager_change".into()],
        ),
        PlanningSelection::DispatchManager => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Manager,
            "workflow.manager_plan_dispatch_available",
        )?,
        PlanningSelection::ApplyManagerProposal => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.coordinator_action_available",
        ),
        PlanningSelection::WaitForManagerProposal => blocked_attempt_decision(
            attempt,
            "workflow.manager_plan_transition_pending",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"plan_hash":attempt.plan_hash}),
            None,
            prerequisites,
            waiting(attempt, "manager_plan_and_transition"),
            AttemptDecisionFlow::Observe,
            None,
            vec!["manager_stop".into(), "manager_change".into()],
        ),
    };
    decision.advance = AttemptAdvance::Planning(selection);
    Ok(decision)
}

fn classify_review(connection: &Connection, attempt: &Attempt) -> Result<ReviewSelection> {
    let (kind, role) = match attempt.phase.as_str() {
        "plan_review" => ("plan", RoleKind::PlanReviewer),
        "code_review" => ("code", RoleKind::CodeReviewer),
        "final_review" => ("final", RoleKind::FinalReviewer),
        phase => bail!("unknown review phase {phase}"),
    };
    let role_name = role.to_string();
    if let Some(session_id) = running_result_session_on(connection, &attempt.id, &role_name)? {
        return Ok(ReviewSelection {
            kind,
            role,
            action: ReviewAction::QuiesceCompleted { session_id },
        });
    }
    if eligible_review_result(connection, attempt, kind, &role_name)?.is_some() {
        return Ok(ReviewSelection {
            kind,
            role,
            action: ReviewAction::ConsumeResult,
        });
    }
    let request: Option<(String, String)> = connection
        .query_row(
            "SELECT id,delivery_state FROM review_requests
             WHERE attempt_id=?1 AND review_kind=?2
               AND delivery_state IN ('reserved','launching','delivered','ambiguous')
             ORDER BY created_at DESC,id DESC LIMIT 1",
            params![attempt.id, kind],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(ReviewSelection {
        kind,
        role,
        action: match request {
            None => ReviewAction::Dispatch,
            Some((_, state)) if state == "reserved" => ReviewAction::Dispatch,
            Some((request_id, state)) => ReviewAction::Wait { request_id, state },
        },
    })
}

fn evaluate_review(
    connection: &Connection,
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
) -> Result<AttemptDecision> {
    let selection = classify_review(connection, attempt)?;
    let mut decision = match &selection.action {
        ReviewAction::QuiesceCompleted { .. } | ReviewAction::ConsumeResult => {
            ready_attempt_decision(attempt, prerequisites, "workflow.review_result_available")
        }
        ReviewAction::Dispatch => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            selection.role,
            "workflow.review_dispatch_available",
        )?,
        ReviewAction::Wait { request_id, state } => {
            let (reason, evidence_state, disposition) = if state == "ambiguous" {
                (
                    "workflow.review_delivery_ambiguous",
                    DecisionEvidenceState::Uncertain,
                    DecisionDisposition::Held,
                )
            } else if matches!(state.as_str(), "delivered" | "launching") {
                (
                    "workflow.review_result_pending",
                    DecisionEvidenceState::Pending,
                    DecisionDisposition::Waiting,
                )
            } else {
                (
                    "workflow.review_request_held",
                    DecisionEvidenceState::Unknown,
                    DecisionDisposition::Held,
                )
            };
            let mut decision = blocked_attempt_decision(
                attempt,
                reason,
                disposition,
                evidence_state,
                if disposition == DecisionDisposition::Waiting {
                    DecisionOwner::Provider
                } else {
                    DecisionOwner::Service
                },
                serde_json::json!({
                    "review_request_id":request_id,
                    "state":state,
                    "kind":selection.kind,
                }),
                None,
                prerequisites,
                if disposition == DecisionDisposition::Waiting {
                    serde_json::json!({"action":"waiting","for":"review_result","request_id":request_id})
                } else {
                    serde_json::json!({"action":"held","reason":reason,"request_id":request_id})
                },
                AttemptDecisionFlow::Observe,
                None,
                Vec::new(),
            );
            decision.explanation.ownership.binding.review_request_id = Some(request_id.clone());
            decision
        }
    };
    decision.advance = AttemptAdvance::Review(selection);
    Ok(decision)
}

fn classify_implementation(
    connection: &Connection,
    attempt: &Attempt,
) -> Result<ImplementationSelection> {
    if eligible_manager_transition(connection, attempt, "code_review")?.is_some() {
        return Ok(ImplementationSelection::ApplyManagerProposal);
    }
    if let Some(session_id) = manager_interrupt_pending_on(connection, &attempt.id)? {
        return Ok(ImplementationSelection::WaitForManagerStop { session_id });
    }
    if !active_role_on(connection, &attempt.id, "manager")? {
        return Ok(ImplementationSelection::DispatchManager);
    }
    if attempt.candidate_hash.is_none() {
        let reviewed_lanes = crate::trip::reviewed_parallel_lane_count(connection, &attempt.id)?;
        let lane_state = implementation_lane_state_on(connection, attempt)?;
        let no_configured_lanes = lane_state.is_none();
        if no_configured_lanes {
            if let Some(reviewed_lanes) = reviewed_lanes {
                return Ok(ImplementationSelection::WaitForLaneAdmission { reviewed_lanes });
            }
        }
        if let Some(lane_state) = lane_state {
            let configured = lane_state.configured;
            if let Some(writer) = yielded_lane_writer_state(connection, attempt)? {
                return Ok(ImplementationSelection::HandleYieldedWriter(writer));
            }
            let unyielded = lane_state.unyielded;
            if unyielded != 0 {
                if let Some(dispatch) = next_implementation_lane_on(connection, attempt)? {
                    return Ok(ImplementationSelection::DispatchLane {
                        dispatch,
                        configured,
                        unyielded,
                    });
                }
                return Ok(ImplementationSelection::WaitForRequiredYields {
                    configured,
                    unyielded,
                });
            }
            if manager_notice_pending_on(connection, attempt, INTEGRATION_READY_NOTICE, true)? {
                return Ok(ImplementationSelection::NotifyIntegrationReady);
            }
            let Some(request_id) = lane_state.request_id else {
                return Ok(ImplementationSelection::WaitForIntegrationRequest { configured });
            };
            if !has_result_on(connection, &attempt.id, "implementer", "candidate_ready")?
                && !active_role_on(connection, &attempt.id, "implementer")?
            {
                let capsule = lane_state
                    .capsule
                    .ok_or_else(|| anyhow!("integration request is missing its capsule"))?;
                return Ok(ImplementationSelection::DispatchIntegration {
                    request_id,
                    capsule,
                });
            }
        }
        if has_result_on(connection, &attempt.id, "implementer", "candidate_ready")? {
            return Ok(
                if let Some(session_id) =
                    running_result_session_on(connection, &attempt.id, "implementer")?
                {
                    ImplementationSelection::QuiesceCompletedImplementer { session_id }
                } else {
                    ImplementationSelection::FreezeCandidate
                },
            );
        }
        if no_configured_lanes && !active_role_on(connection, &attempt.id, "implementer")? {
            return Ok(ImplementationSelection::DispatchImplementer);
        }
    } else if crate::store::eligible_manager_service_stop(connection, &attempt.id, &attempt.phase)?
        .is_some()
    {
        return Ok(ImplementationSelection::StopManagerForCandidate);
    }
    Ok(ImplementationSelection::WaitForManagerTransition)
}

fn evaluate_implementation(
    connection: &Connection,
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
) -> Result<AttemptDecision> {
    let selection = classify_implementation(connection, attempt)?;
    let mut decision = match &selection {
        ImplementationSelection::ApplyManagerProposal => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.manager_transition_available",
        ),
        ImplementationSelection::WaitForManagerStop { session_id } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_service_stop_quiescence",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"session_id":session_id}),
            serde_json::json!({
                "action":"waiting",
                "for":"manager_service_stop_quiescence",
                "attempt_id":attempt.id,
                "session_id":session_id,
            }),
        ),
        ImplementationSelection::DispatchManager => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Manager,
            "workflow.manager_dispatch_available",
        )?,
        ImplementationSelection::WaitForLaneAdmission { reviewed_lanes } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_lane_admission",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"reviewed_lanes":reviewed_lanes,"configured_lanes":0}),
            serde_json::json!({
                "action":"waiting",
                "for":"manager_lane_admission",
                "attempt_id":attempt.id,
                "reviewed_lanes":reviewed_lanes,
                "configured_lanes":0,
            }),
        ),
        ImplementationSelection::HandleYieldedWriter(writer) => match writer {
            YieldedLaneWriterState::Invalid { lane_key } => phase_wait_decision(
                attempt,
                prerequisites,
                "workflow.accepted_lane_yield_receipts",
                DecisionDisposition::Waiting,
                DecisionEvidenceState::Missing,
                DecisionOwner::Provider,
                serde_json::json!({"lane_key":lane_key}),
                serde_json::json!({
                    "action":"waiting",
                    "for":"accepted_lane_yield_receipts",
                    "attempt_id":attempt.id,
                    "lane_key":lane_key,
                }),
            ),
            YieldedLaneWriterState::NotQuiescent {
                generation_status,
                session_status,
                ..
            } if generation_status == "running" && session_status == "running" => {
                ready_attempt_decision(
                    attempt,
                    prerequisites,
                    "workflow.yielded_lane_interrupt_available",
                )
            }
            YieldedLaneWriterState::NotQuiescent {
                lane_key,
                session_id,
                generation_status,
                session_status,
            } => phase_wait_decision(
                attempt,
                prerequisites,
                "workflow.yielded_lane_quiescence",
                DecisionDisposition::Waiting,
                DecisionEvidenceState::Pending,
                DecisionOwner::Service,
                serde_json::json!({
                    "lane_key":lane_key,
                    "session_id":session_id,
                    "generation_status":generation_status,
                    "session_status":session_status,
                }),
                serde_json::json!({
                    "action":"waiting",
                    "for":"yielded_lane_quiescence",
                    "attempt_id":attempt.id,
                    "lane_key":lane_key,
                    "session_id":session_id,
                    "generation_status":generation_status,
                    "session_status":session_status,
                }),
            ),
        },
        ImplementationSelection::DispatchLane { .. } => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Implementer,
            "workflow.implementation_lane_dispatch_available",
        )?,
        ImplementationSelection::WaitForRequiredYields {
            configured,
            unyielded,
        } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.required_lane_yields",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"configured_lanes":configured,"unyielded_lanes":unyielded}),
            serde_json::json!({
                "action":"waiting",
                "for":"required_lane_yields",
                "attempt_id":attempt.id,
                "configured_lanes":configured,
                "unyielded_lanes":unyielded,
            }),
        ),
        ImplementationSelection::NotifyIntegrationReady => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.coordinator_action_available",
        ),
        ImplementationSelection::WaitForIntegrationRequest { configured } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_integration_request",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"configured_lanes":configured}),
            serde_json::json!({
                "action":"waiting",
                "for":"manager_integration_request",
                "attempt_id":attempt.id,
                "configured_lanes":configured,
            }),
        ),
        ImplementationSelection::DispatchIntegration { .. } => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Implementer,
            "workflow.integration_dispatch_available",
        )?,
        ImplementationSelection::QuiesceCompletedImplementer { .. }
        | ImplementationSelection::FreezeCandidate => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.candidate_freeze_available",
        ),
        ImplementationSelection::DispatchImplementer => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Implementer,
            "workflow.implementer_dispatch_available",
        )?,
        ImplementationSelection::StopManagerForCandidate => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.manager_service_stop_available",
        ),
        ImplementationSelection::WaitForManagerTransition => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.implementation_or_manager_transition",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"candidate_hash":attempt.candidate_hash}),
            waiting(attempt, "implementation_or_manager_transition"),
        ),
    };
    decision.advance = AttemptAdvance::Implementation(selection);
    Ok(decision)
}

fn classify_checks(connection: &Connection, attempt: &Attempt) -> Result<ChecksSelection> {
    let Some(candidate) = attempt.candidate_hash.as_deref() else {
        return Ok(ChecksSelection::MissingCandidate);
    };
    if let Some((check_id, status)) = blocked_check_on(connection, attempt, candidate)? {
        return Ok(ChecksSelection::CheckFailed { check_id, status });
    }
    if let Some(check_id) = next_required_check_on(connection, attempt, candidate)? {
        return Ok(ChecksSelection::RunCheck { check_id });
    }
    let (configured, unsettled) = selected_check_completion_on(connection, attempt, candidate)?;
    if configured == 0 {
        return Ok(ChecksSelection::NoSelectedChecks);
    }
    if unsettled {
        return Ok(ChecksSelection::CheckExecutionUnsettled);
    }
    if !manager_conformance_ready_on(connection, attempt)? {
        if let Some(session_id) = manager_interrupt_pending_on(connection, &attempt.id)? {
            return Ok(ChecksSelection::WaitForManagerStop { session_id });
        }
        if !active_role_on(connection, &attempt.id, "manager")? {
            return Ok(ChecksSelection::DispatchConformanceManager);
        }
        if crate::store::eligible_manager_service_stop(connection, &attempt.id, &attempt.phase)?
            .is_some()
        {
            return Ok(ChecksSelection::StopManagerForConformance);
        }
        return Ok(ChecksSelection::ConformanceMissing);
    }
    match final_explorer_ready_on(connection, attempt, candidate)? {
        None => {
            if let Some(session_id) = manager_interrupt_pending_on(connection, &attempt.id)? {
                return Ok(ChecksSelection::WaitForManagerStop { session_id });
            }
            if !active_role_on(connection, &attempt.id, "manager")? {
                return Ok(ChecksSelection::DispatchFinalExplorerManager);
            }
            if crate::store::eligible_manager_service_stop(connection, &attempt.id, &attempt.phase)?
                .is_some()
            {
                return Ok(ChecksSelection::StopManagerForFinalExplorer);
            }
            return Ok(ChecksSelection::WaitForFinalExplorerDecision);
        }
        Some(false) => return Ok(ChecksSelection::WaitForActivatedExplorer),
        Some(true) => {}
    }
    Ok(ChecksSelection::Complete(manager_completion_boundary(
        connection, attempt,
    )?))
}

fn evaluate_checks(
    connection: &Connection,
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
) -> Result<AttemptDecision> {
    let selection = classify_checks(connection, attempt)?;
    let candidate = attempt.candidate_hash.as_deref();
    let mut decision = match &selection {
        ChecksSelection::MissingCandidate => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.frozen_candidate_missing",
            DecisionDisposition::Held,
            DecisionEvidenceState::Missing,
            DecisionOwner::Service,
            serde_json::json!({"phase":attempt.phase}),
            serde_json::json!({"action":"held","reason":"frozen_candidate_missing"}),
        ),
        ChecksSelection::CheckFailed { check_id, status } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.check_failure",
            DecisionDisposition::Held,
            DecisionEvidenceState::Uncertain,
            DecisionOwner::Human,
            serde_json::json!({"check_id":check_id,"status":status}),
            serde_json::json!({
                "action":"held",
                "reason":"check_failure",
                "suite":check_id,
                "status":status,
            }),
        ),
        ChecksSelection::RunCheck { check_id } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.selected_check_authority_unobserved",
            DecisionDisposition::Held,
            DecisionEvidenceState::Unknown,
            DecisionOwner::Service,
            serde_json::json!({"check_id":check_id,"evaluated":false}),
            serde_json::json!({
                "action":"waiting",
                "for":"selected_check_authority",
                "attempt_id":attempt.id,
                "check_id":check_id,
            }),
        ),
        ChecksSelection::NoSelectedChecks => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.no_plan_selected_trip_checks",
            DecisionDisposition::Held,
            DecisionEvidenceState::Missing,
            DecisionOwner::Human,
            serde_json::json!({"selected_checks_revision":attempt.selected_checks_revision}),
            serde_json::json!({"action":"held","reason":"no_plan_selected_trip_checks"}),
        ),
        ChecksSelection::CheckExecutionUnsettled => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.check_execution_unsettled",
            DecisionDisposition::Held,
            DecisionEvidenceState::Uncertain,
            DecisionOwner::Service,
            serde_json::json!({"candidate_hash":candidate}),
            serde_json::json!({"action":"held","reason":"check_failure"}),
        ),
        ChecksSelection::WaitForManagerStop { session_id } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_service_stop_quiescence",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"session_id":session_id}),
            serde_json::json!({
                "action":"waiting",
                "for":"manager_service_stop_quiescence",
                "attempt_id":attempt.id,
                "session_id":session_id,
            }),
        ),
        ChecksSelection::DispatchConformanceManager => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Manager,
            "workflow.manager_conformance_dispatch_available",
        )?,
        ChecksSelection::StopManagerForConformance => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.manager_conformance_stop_available",
        ),
        ChecksSelection::ConformanceMissing => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_conformance_or_lane_yield_missing",
            DecisionDisposition::Held,
            DecisionEvidenceState::Missing,
            DecisionOwner::Provider,
            serde_json::json!({"candidate_hash":candidate}),
            serde_json::json!({
                "action":"held",
                "reason":"manager_conformance_or_lane_yield_missing",
            }),
        ),
        ChecksSelection::DispatchFinalExplorerManager => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Manager,
            "workflow.final_explorer_decision_dispatch_available",
        )?,
        ChecksSelection::StopManagerForFinalExplorer => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.final_explorer_decision_stop_available",
        ),
        ChecksSelection::WaitForFinalExplorerDecision => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_final_explorer_decision",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"candidate_hash":candidate}),
            serde_json::json!({
                "action":"waiting",
                "for":"manager_final_explorer_decision",
                "attempt_id":attempt.id,
                "candidate_hash":candidate,
            }),
        ),
        ChecksSelection::WaitForActivatedExplorer => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.activated_final_explorer_evidence",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"candidate_hash":candidate}),
            serde_json::json!({
                "action":"waiting",
                "for":"activated_final_explorer_evidence",
                "attempt_id":attempt.id,
                "candidate_hash":candidate,
            }),
        ),
        ChecksSelection::Complete(boundary) => manager_boundary_decision(
            attempt,
            prerequisites,
            "completed_checks_and_final_explorer",
            "workflow.completed_checks_transaction_unobserved",
            boundary,
        ),
    };
    decision.advance = AttemptAdvance::Checks(selection);
    Ok(decision)
}

fn classify_handoff(connection: &Connection, attempt: &Attempt) -> Result<HandoffSelection> {
    if exact_handoff_ready_on(connection, attempt)? {
        return Ok(HandoffSelection::Complete(manager_completion_boundary(
            connection, attempt,
        )?));
    }
    if let Some(session_id) = manager_interrupt_pending_on(connection, &attempt.id)? {
        return Ok(HandoffSelection::WaitForManagerStop { session_id });
    }
    if !active_role_on(connection, &attempt.id, "manager")? {
        return Ok(HandoffSelection::DispatchManager);
    }
    if crate::store::eligible_manager_service_stop(connection, &attempt.id, &attempt.phase)?
        .is_some()
    {
        return Ok(HandoffSelection::StopManager);
    }
    Ok(HandoffSelection::WaitForManagerHandoff)
}

fn evaluate_handoff(
    connection: &Connection,
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
) -> Result<AttemptDecision> {
    let selection = classify_handoff(connection, attempt)?;
    let mut decision = match &selection {
        HandoffSelection::Complete(boundary) => manager_boundary_decision(
            attempt,
            prerequisites,
            "completed_final_review_handoff",
            "workflow.completed_handoff_transaction_unobserved",
            boundary,
        ),
        HandoffSelection::WaitForManagerStop { session_id } => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_service_stop_quiescence",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"session_id":session_id}),
            serde_json::json!({
                "action":"waiting",
                "for":"manager_service_stop_quiescence",
                "attempt_id":attempt.id,
                "session_id":session_id,
            }),
        ),
        HandoffSelection::DispatchManager => role_dispatch_decision(
            connection,
            attempt,
            prerequisites,
            RoleKind::Manager,
            "workflow.handoff_manager_dispatch_available",
        )?,
        HandoffSelection::StopManager => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.handoff_manager_stop_available",
        ),
        HandoffSelection::WaitForManagerHandoff => phase_wait_decision(
            attempt,
            prerequisites,
            "workflow.manager_handoff",
            DecisionDisposition::Waiting,
            DecisionEvidenceState::Pending,
            DecisionOwner::Provider,
            serde_json::json!({"candidate_hash":attempt.candidate_hash}),
            waiting(attempt, "manager_handoff"),
        ),
    };
    decision.advance = AttemptAdvance::Handoff(selection);
    Ok(decision)
}

fn manager_boundary_decision(
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
    obligation: &str,
    transaction_reason: &str,
    boundary: &ManagerCompletionBoundary,
) -> AttemptDecision {
    match boundary {
        ManagerCompletionBoundary::Ready => phase_wait_decision(
            attempt,
            prerequisites,
            transaction_reason,
            DecisionDisposition::Held,
            DecisionEvidenceState::Unknown,
            DecisionOwner::Service,
            serde_json::json!({"evaluated":false,"candidate_hash":attempt.candidate_hash}),
            serde_json::json!({
                "action":"waiting",
                "for":"owner_transaction_recheck",
                "attempt_id":attempt.id,
                "obligation":obligation,
            }),
        ),
        ManagerCompletionBoundary::RequestStop { .. } => ready_attempt_decision(
            attempt,
            prerequisites,
            "workflow.manager_service_stop_available",
        ),
        ManagerCompletionBoundary::Waiting {
            reason,
            session_id,
            generation_status,
            session_status,
            readiness,
            quiescent,
        } => {
            let mut evidence = serde_json::Map::new();
            evidence.insert(
                "obligation".into(),
                serde_json::Value::String(obligation.into()),
            );
            let mut legacy = serde_json::Map::from_iter([
                ("action".into(), serde_json::Value::String("waiting".into())),
                ("for".into(), serde_json::Value::String((*reason).into())),
                (
                    "attempt_id".into(),
                    serde_json::Value::String(attempt.id.clone()),
                ),
                (
                    "obligation".into(),
                    serde_json::Value::String(obligation.into()),
                ),
            ]);
            for (key, value) in [
                ("session_id", session_id),
                ("manager_generation_status", generation_status),
                ("manager_session_status", session_status),
                ("manager_readiness", readiness),
            ] {
                if let Some(value) = value {
                    evidence.insert(key.into(), serde_json::Value::String(value.clone()));
                    legacy.insert(key.into(), serde_json::Value::String(value.clone()));
                }
            }
            if let Some(value) = quiescent {
                evidence.insert("process_group_quiescent".into(), (*value).into());
                legacy.insert("process_group_quiescent".into(), (*value).into());
            }
            phase_wait_decision(
                attempt,
                prerequisites,
                manager_boundary_reason_code(reason),
                DecisionDisposition::Waiting,
                DecisionEvidenceState::Pending,
                DecisionOwner::Service,
                serde_json::Value::Object(evidence),
                serde_json::Value::Object(legacy),
            )
        }
    }
}

fn manager_boundary_reason_code(reason: &str) -> &'static str {
    match reason {
        "current_manager_completion_boundary" => "workflow.current_manager_completion_boundary",
        "manager_service_stop_quiescence" => "workflow.manager_service_stop_quiescence",
        "manager_quiescence_or_recovery" => "workflow.manager_quiescence_or_recovery",
        "manager_safe_completion_boundary" => "workflow.manager_safe_completion_boundary",
        _ => "workflow.manager_completion_boundary_unknown",
    }
}

#[allow(clippy::too_many_arguments)]
fn phase_wait_decision(
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
    reason_code: &str,
    disposition: DecisionDisposition,
    state: DecisionEvidenceState,
    owner: DecisionOwner,
    evidence: serde_json::Value,
    legacy: serde_json::Value,
) -> AttemptDecision {
    blocked_attempt_decision(
        attempt,
        reason_code,
        disposition,
        state,
        owner,
        evidence,
        None,
        prerequisites,
        legacy,
        AttemptDecisionFlow::Observe,
        None,
        Vec::new(),
    )
}

fn role_dispatch_decision(
    connection: &Connection,
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
    role: RoleKind,
    ready_reason: &str,
) -> Result<AttemptDecision> {
    if role == RoleKind::Manager {
        return Ok(blocked_attempt_decision(
            attempt,
            "workflow.manager_dispatch_route_unobserved",
            DecisionDisposition::Held,
            DecisionEvidenceState::Unknown,
            DecisionOwner::Service,
            serde_json::json!({
                "evaluated":false,
                "phase":attempt.phase,
                "role":role,
                "intended_reason_code":ready_reason,
            }),
            None,
            prerequisites,
            waiting(attempt, "manager_resume_or_fresh_dispatch_classification"),
            AttemptDecisionFlow::Observe,
            None,
            Vec::new(),
        ));
    }
    let Some((provider, capacity_available)) =
        role_capacity_for_attempt(connection, attempt, role)?
    else {
        return Ok(unknown_phase_decision(
            attempt,
            prerequisites,
            "workflow.role_capacity_unobserved",
        ));
    };
    let mut prerequisites = prerequisites;
    prerequisites.push(attempt_prerequisite(
        if capacity_available {
            "workflow.provider_capacity_available"
        } else {
            "workflow.provider_capacity"
        },
        if capacity_available {
            DecisionEvidenceState::Satisfied
        } else {
            DecisionEvidenceState::Pending
        },
        DecisionOwner::Service,
        serde_json::json!({"provider":provider,"role":role}),
        None,
    ));
    Ok(blocked_attempt_decision(
        attempt,
        "workflow.role_admission_transaction_unobserved",
        DecisionDisposition::Held,
        DecisionEvidenceState::Unknown,
        DecisionOwner::Service,
        serde_json::json!({
            "evaluated":false,
            "phase":attempt.phase,
            "provider":provider,
            "role":role,
            "intended_reason_code":ready_reason,
        }),
        None,
        prerequisites,
        waiting(attempt, "owner_admission_transaction"),
        AttemptDecisionFlow::Observe,
        None,
        Vec::new(),
    ))
}

fn role_capacity_for_attempt(
    connection: &Connection,
    attempt: &Attempt,
    role: RoleKind,
) -> Result<Option<(String, bool)>> {
    let role_name = role.to_string();
    let provider = connection
        .query_row(
            "SELECT json_extract(profile_json,'$.provider') FROM trip_attempt_profiles
             WHERE attempt_id=?1 AND role=?2",
            params![attempt.id, role_name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(provider) = provider else {
        return Ok(None);
    };
    Ok(Some((
        provider.clone(),
        crate::store::role_capacity_available(connection, &provider, role, None, None)?,
    )))
}

fn unknown_phase_decision(
    attempt: &Attempt,
    prerequisites: Vec<DecisionPrerequisite>,
    reason_code: &str,
) -> AttemptDecision {
    blocked_attempt_decision(
        attempt,
        reason_code,
        DecisionDisposition::Held,
        DecisionEvidenceState::Unknown,
        DecisionOwner::Service,
        serde_json::json!({"phase":attempt.phase,"evaluated":false}),
        None,
        prerequisites,
        waiting(attempt, "owner_prerequisite_evaluation"),
        AttemptDecisionFlow::Observe,
        None,
        Vec::new(),
    )
}

fn ready_attempt_decision(
    attempt: &Attempt,
    mut prerequisites: Vec<DecisionPrerequisite>,
    reason_code: &str,
) -> AttemptDecision {
    prerequisites.push(attempt_prerequisite(
        reason_code,
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Service,
        serde_json::json!({"phase":attempt.phase}),
        None,
    ));
    AttemptDecision {
        legacy: serde_json::json!({
            "action":"waiting",
            "attempt_id":attempt.id,
            "phase":attempt.phase,
            "for":"coordinator_action",
        }),
        explanation: attempt_explanation(
            attempt,
            reason_code,
            DecisionDisposition::Ready,
            None,
            prerequisites,
            DecisionOwnership {
                owner: DecisionOwner::Service,
                state: attempt.status.clone(),
                binding: attempt_binding(attempt),
            },
            None,
            DecisionControlPolicy::default(),
        ),
        flow: AttemptDecisionFlow::Observe,
        advance: AttemptAdvance::None,
    }
}

#[allow(clippy::too_many_arguments)]
fn blocked_attempt_decision(
    attempt: &Attempt,
    reason_code: &str,
    disposition: DecisionDisposition,
    state: DecisionEvidenceState,
    owner: DecisionOwner,
    evidence: serde_json::Value,
    message: Option<String>,
    mut prerequisites: Vec<DecisionPrerequisite>,
    legacy: serde_json::Value,
    flow: AttemptDecisionFlow,
    next_action: Option<DecisionNextAction>,
    allowed_controls: Vec<String>,
) -> AttemptDecision {
    let blocker = attempt_prerequisite(reason_code, state, owner, evidence, message);
    let disabled_reason_code = next_action
        .as_ref()
        .is_none_or(|action| !action.enabled)
        .then(|| reason_code.into());
    prerequisites.push(blocker.clone());
    prerequisites.push(attempt_prerequisite(
        "workflow.remaining_phase_prerequisites",
        DecisionEvidenceState::Unknown,
        DecisionOwner::Service,
        serde_json::json!({"evaluated":false,"short_circuit":reason_code}),
        None,
    ));
    AttemptDecision {
        legacy,
        explanation: attempt_explanation(
            attempt,
            reason_code,
            disposition,
            Some(blocker),
            prerequisites,
            DecisionOwnership {
                owner,
                state: attempt.status.clone(),
                binding: attempt_binding(attempt),
            },
            next_action,
            DecisionControlPolicy {
                allowed_controls,
                disabled_reason_code,
            },
        ),
        flow,
        advance: AttemptAdvance::None,
    }
}

fn attempt_prerequisite(
    code: &str,
    state: DecisionEvidenceState,
    owner: DecisionOwner,
    evidence: serde_json::Value,
    message: Option<String>,
) -> DecisionPrerequisite {
    DecisionPrerequisite {
        code: code.into(),
        state,
        owner,
        evidence,
        message,
    }
}

fn attempt_binding(attempt: &Attempt) -> DecisionActionBinding {
    DecisionActionBinding {
        project_id: Some(attempt.project_id.clone()),
        task_id: Some(attempt.task_id.clone()),
        attempt_id: Some(attempt.id.clone()),
        expected_task_version: Some(attempt.task_version),
        selected_checks_revision: decision_uses_check_revision(&attempt.phase)
            .then_some(attempt.selected_checks_revision),
        plan_hash: attempt.plan_hash.clone(),
        candidate_hash: attempt.candidate_hash.clone(),
        ..DecisionActionBinding::default()
    }
}

fn attempt_explanation(
    attempt: &Attempt,
    reason_code: &str,
    disposition: DecisionDisposition,
    primary_blocker: Option<DecisionPrerequisite>,
    prerequisites: Vec<DecisionPrerequisite>,
    ownership: DecisionOwnership,
    next_action: Option<DecisionNextAction>,
    control_policy: DecisionControlPolicy,
) -> DecisionExplanation {
    DecisionExplanation {
        decision_schema: DECISION_SCHEMA_V1,
        reason_code: reason_code.into(),
        disposition,
        subject: DecisionSubject {
            project_id: Some(attempt.project_id.clone()),
            task_id: Some(attempt.task_id.clone()),
            attempt_id: Some(attempt.id.clone()),
            ..DecisionSubject::default()
        },
        observed_revision: attempt_observed_revision(attempt),
        primary_blocker,
        prerequisites,
        ownership,
        next_action,
        control_policy,
    }
}

fn attempt_observed_revision(attempt: &Attempt) -> DecisionObservedRevision {
    DecisionObservedRevision {
        task_version: Some(attempt.task_version),
        project_version: None,
        attempt_phase: Some(attempt.phase.clone()),
        configuration_revision: Some(attempt.configuration_revision),
        selected_checks_revision: decision_uses_check_revision(&attempt.phase)
            .then_some(attempt.selected_checks_revision),
        plan_hash: attempt.plan_hash.clone(),
        candidate_hash: attempt.candidate_hash.clone(),
    }
}

fn decision_uses_check_revision(phase: &str) -> bool {
    matches!(
        phase,
        "checks" | "final_review" | "manager_handoff" | "awaiting_human_review"
    )
}

fn active_role_on(connection: &Connection, attempt: &str, role: &str) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role=?2
           AND status IN ('launch_reserved','running','stopping'))",
        params![attempt, role],
        |row| row.get(0),
    )?)
}

fn attach_decision(
    mut value: serde_json::Value,
    decision: &DecisionExplanation,
) -> Result<serde_json::Value> {
    value
        .as_object_mut()
        .ok_or_else(|| anyhow!("coordinator outcome must be an object"))?
        .insert("decision".into(), serde_json::to_value(decision)?);
    Ok(value)
}

fn classify_attempt_for_tick(
    app: &Application,
    attempt_id: &str,
) -> Result<Option<(Attempt, AttemptDecision)>> {
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attempt = active_attempts_from_connection(&transaction)?
        .into_iter()
        .find(|attempt| attempt.id == attempt_id);
    let Some(attempt) = attempt else {
        transaction.commit()?;
        return Ok(None);
    };
    let decision = evaluate_attempt(&transaction, &attempt)?;
    if matches!(
        decision.explanation.disposition,
        DecisionDisposition::Waiting
            | DecisionDisposition::Held
            | DecisionDisposition::RetryDeferred
    ) {
        audit_coordinator_decision(
            &transaction,
            &decision.explanation,
            &Utc::now().to_rfc3339(),
        )?;
    }
    transaction.commit()?;
    Ok(Some((attempt, decision)))
}

fn refreshed_attempt_decision(
    app: &Application,
    attempt_id: &str,
) -> Result<Option<DecisionExplanation>> {
    Ok(classify_attempt_for_tick(app, attempt_id)?.map(|(_, decision)| decision.explanation))
}

fn audit_coordinator_decision(
    connection: &Connection,
    decision: &DecisionExplanation,
    now: &str,
) -> Result<()> {
    let task_id = decision
        .subject
        .task_id
        .as_deref()
        .ok_or_else(|| anyhow!("coordinator decision omitted its task identity"))?;
    let canonical = decision.canonical_value();
    let previous: Option<String> = connection
        .query_row(
            "SELECT detail_json FROM audit_events
             WHERE event_code='decision.explanation.changed'
               AND entity_kind='task' AND entity_id=?1
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    if previous
        .as_deref()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|value| value.get("canonical").cloned())
        .as_ref()
        == Some(&canonical)
    {
        return Ok(());
    }
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','decision.explanation.changed','task',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            task_id,
            serde_json::json!({"canonical":canonical,"explanation":decision}).to_string(),
            now,
        ],
    )?;
    Ok(())
}

fn legacy_wait_is_visible(value: &serde_json::Value) -> bool {
    matches!(
        value.get("for").and_then(serde_json::Value::as_str),
        Some(
            "manager_lane_admission"
                | "implementation_lane_capacity"
                | "required_lane_yields"
                | "yielded_lane_quiescence"
                | "accepted_lane_yield_receipts"
                | "manager_integration_request"
                | "manager_service_stop_quiescence"
                | "current_manager_completion_boundary"
                | "manager_quiescence_or_recovery"
                | "manager_safe_completion_boundary"
                | "completed_checks_manager_boundary_recheck"
                | "completed_handoff_manager_boundary_recheck"
        )
    )
}

fn is_committed_action(value: &serde_json::Value) -> bool {
    !matches!(
        value.get("action").and_then(|value| value.as_str()),
        Some(
            "waiting"
                | "held"
                | "idle"
                | "guidance_queued"
                | "draining_control"
                | "recovery_required_for_cancel"
                | "control_rejected",
        ) | None
    )
}

fn mark_attempt_served(app: &Application, attempt: &str) -> Result<()> {
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    let budget: i64 = transaction.query_row(
        "SELECT step_budget FROM attempts WHERE id=?1",
        params![attempt],
        |row| row.get(0),
    )?;
    transaction.execute(
        "UPDATE attempts SET last_coordinator_at=?1,step_budget=CASE WHEN step_budget>0 THEN step_budget-1 ELSE step_budget END WHERE id=?2",
        params![now, attempt],
    )?;
    if budget == 1 {
        transaction.execute(
            "UPDATE attempts SET status='held',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        transaction.execute("UPDATE tasks SET attention='paused',version=version+1,updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
    }
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','coordinator.action.committed','attempt',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),attempt,serde_json::json!({"single_step_completed":budget==1}).to_string(),now])?;
    transaction.commit()?;
    Ok(())
}

fn advance_planning(
    app: &Application,
    attempt: &Attempt,
    selection: PlanningSelection,
) -> Result<serde_json::Value> {
    match selection {
        PlanningSelection::FreezePlan => {
            let snapshot = app.reviews.freeze(&attempt.id, "plan")?;
            Ok(
                serde_json::json!({"action":"plan_frozen","attempt_id":attempt.id,"snapshot":snapshot}),
            )
        }
        PlanningSelection::QuiesceCompletedManager { session_id } => {
            app.interrupt_completed_role(&session_id)?;
            Ok(serde_json::json!({
                "action":"quiescing_completed_role",
                "role":"manager",
                "session_id":session_id,
                "completion":"current_plan_ready"
            }))
        }
        PlanningSelection::WaitForManagerPlan => Ok(waiting(attempt, "current_manager_plan")),
        PlanningSelection::DispatchManager => {
            let prompt = crate::workflow_resources::render(
                RoleKind::Manager,
                &serde_json::json!({"task_id":attempt.task_id,"attempt_id":attempt.id,"phase":attempt.phase,"title":attempt.title,"description":attempt.description,"acceptance_criteria":attempt.criteria,"required_outcome":"plan_ready"}),
            )?;
            let (launch, resumed) = dispatch_or_resume_manager(app, &attempt.id, &prompt)?;
            Ok(serde_json::json!({
                "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                "attempt_id":attempt.id,
                "session_id":launch.session_id
            }))
        }
        PlanningSelection::ApplyManagerProposal => {
            Ok(apply_manager_proposal(app, attempt, "plan_review")?
                .unwrap_or_else(|| waiting(attempt, "manager_plan_and_transition")))
        }
        PlanningSelection::WaitForManagerProposal => {
            Ok(waiting(attempt, "manager_plan_and_transition"))
        }
    }
}

fn advance_implementation(
    app: &Application,
    attempt: &Attempt,
    selection: ImplementationSelection,
) -> Result<serde_json::Value> {
    match selection {
        ImplementationSelection::ApplyManagerProposal => {
            Ok(apply_manager_proposal(app, attempt, "code_review")?
                .unwrap_or_else(|| waiting(attempt, "implementation_or_manager_transition")))
        }
        ImplementationSelection::WaitForManagerStop { session_id } => Ok(serde_json::json!({
            "action":"waiting",
            "for":"manager_service_stop_quiescence",
            "attempt_id":attempt.id,
            "session_id":session_id,
        })),
        ImplementationSelection::DispatchManager => {
            let feedback = latest_rework_feedback(app, &attempt.id)?;
            let prompt = crate::workflow_resources::render(
                RoleKind::Manager,
                &serde_json::json!({
                    "task_id":attempt.task_id,
                    "attempt_id":attempt.id,
                    "phase":attempt.phase,
                    "title":attempt.title,
                    "description":attempt.description,
                    "acceptance_criteria":attempt.criteria,
                    "rework_feedback":feedback,
                    "plan_hash":attempt.plan_hash,
                    "coordination_contract":"remain in this fresh attempt context; after the exact candidate is frozen, inspect evidence and propose code_review"
                }),
            )?;
            let (launch, resumed) = dispatch_or_resume_manager(app, &attempt.id, &prompt)?;
            Ok(serde_json::json!({
                "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
                "context":"fresh_implementation_attempt"
            }))
        }
        ImplementationSelection::WaitForLaneAdmission { reviewed_lanes } => Ok(serde_json::json!({
            "action":"waiting",
            "for":"manager_lane_admission",
            "attempt_id":attempt.id,
            "reviewed_lanes":reviewed_lanes,
            "configured_lanes":0,
        })),
        ImplementationSelection::HandleYieldedWriter(writer) => match writer {
            YieldedLaneWriterState::Invalid { lane_key } => Ok(serde_json::json!({
                "action":"waiting",
                "for":"accepted_lane_yield_receipts",
                "attempt_id":attempt.id,
                "lane_key":lane_key,
            })),
            YieldedLaneWriterState::NotQuiescent {
                lane_key,
                session_id,
                generation_status,
                session_status,
            } if generation_status == "running" && session_status == "running" => {
                app.interrupt_completed_role(&session_id)?;
                Ok(serde_json::json!({
                    "action":"yielded_lane_interrupt",
                    "attempt_id":attempt.id,
                    "lane_key":lane_key,
                    "session_id":session_id,
                }))
            }
            YieldedLaneWriterState::NotQuiescent {
                lane_key,
                session_id,
                generation_status,
                session_status,
            } => Ok(serde_json::json!({
                "action":"waiting",
                "for":"yielded_lane_quiescence",
                "attempt_id":attempt.id,
                "lane_key":lane_key,
                "session_id":session_id,
                "generation_status":generation_status,
                "session_status":session_status,
            })),
        },
        ImplementationSelection::DispatchLane {
            dispatch,
            configured,
            unyielded,
        } => {
            let prompt =
                crate::workflow_resources::render(RoleKind::Implementer, &dispatch.prompt_context)?;
            match app.dispatch_implementation_lane(&attempt.id, &dispatch.lane_key, &prompt) {
                Ok(launch) => Ok(serde_json::json!({
                    "action":"implementation_lane_dispatched",
                    "attempt_id":attempt.id,
                    "lane_key":dispatch.lane_key,
                    "session_id":launch.session_id,
                    "invocation":"initial",
                    "profile_session":dispatch.profile_session,
                })),
                Err(error) if format!("{error:#}").contains("capacity") => {
                    let lane_status = implementation_lane_waiting(app, &attempt.id)?;
                    Ok(serde_json::json!({
                        "action":"waiting",
                        "for":"implementation_lane_capacity",
                        "attempt_id":attempt.id,
                        "configured_lanes":configured,
                        "unyielded_lanes":unyielded,
                        "lane_status":lane_status,
                    }))
                }
                Err(error) => Err(error),
            }
        }
        ImplementationSelection::WaitForRequiredYields {
            configured,
            unyielded,
        } => {
            let lane_status = implementation_lane_waiting(app, &attempt.id)?;
            Ok(serde_json::json!({
                "action":"waiting",
                "for":"required_lane_yields",
                "attempt_id":attempt.id,
                "configured_lanes":configured,
                "unyielded_lanes":unyielded,
                "lane_status":lane_status,
            }))
        }
        ImplementationSelection::NotifyIntegrationReady => {
            if queue_manager_notice(app, attempt, INTEGRATION_READY_NOTICE, true)? {
                Ok(serde_json::json!({
                    "action":"manager_integration_ready_notified",
                    "attempt_id":attempt.id,
                }))
            } else {
                Ok(waiting(attempt, "manager_integration_request"))
            }
        }
        ImplementationSelection::WaitForIntegrationRequest { configured } => {
            Ok(serde_json::json!({
                "action":"waiting",
                "for":"manager_integration_request",
                "attempt_id":attempt.id,
                "configured_lanes":configured,
            }))
        }
        ImplementationSelection::DispatchIntegration {
            request_id,
            capsule,
        } => {
            let lane_receipts = {
                let connection = app.store.lock()?;
                let mut statement=connection.prepare("SELECT json_object('lane_key',lane_key,'source_hashes',json(source_hashes_json),'yield_receipt',json(receipt_json)) FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state='yielded' ORDER BY lane_key")?;
                let rows = statement
                    .query_map(params![attempt.id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter()
                    .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                    .collect::<serde_json::Result<Vec<_>>>()?
            };
            let prompt = crate::workflow_resources::render(
                RoleKind::Implementer,
                &serde_json::json!({
                    "task_id":attempt.task_id,
                    "attempt_id":attempt.id,
                    "phase":attempt.phase,
                    "title":attempt.title,
                    "description":attempt.description,
                    "acceptance_criteria":attempt.criteria,
                    "plan_hash":attempt.plan_hash,
                    "integration_capsule":serde_json::from_str::<serde_json::Value>(&capsule)?,
                    "lane_receipts":lane_receipts,
                    "required_outcome":"candidate_ready",
                    "integration_only":true,
                }),
            )?;
            let launch = app.dispatch_attempt_role(&attempt.id, RoleKind::Implementer, &prompt)?;
            let connection = app.store.lock()?;
            let changed = connection.execute(
                "UPDATE trip_integration_requests SET state='dispatched',dispatched_at=?1
                 WHERE id=?2 AND attempt_id=?3 AND state='requested'",
                params![Utc::now().to_rfc3339(), request_id, attempt.id],
            )?;
            if changed != 1 {
                bail!("manager integration request changed during dispatch")
            }
            Ok(serde_json::json!({
                "action":"integration_implementer_dispatched",
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
                "integration_request_id":request_id,
            }))
        }
        ImplementationSelection::QuiesceCompletedImplementer { session_id } => {
            app.supervisor.interrupt(&session_id)?;
            Ok(serde_json::json!({
                "action":"quiescing_completed_role",
                "role":"implementer",
                "session_id":session_id,
            }))
        }
        ImplementationSelection::FreezeCandidate => {
            let snapshot = app.reviews.freeze(&attempt.id, "candidate")?;
            queue_manager_notice(app, attempt, CANDIDATE_FROZEN_NOTICE, false)?;
            Ok(serde_json::json!({
                "action":"candidate_frozen",
                "attempt_id":attempt.id,
                "snapshot":snapshot,
            }))
        }
        ImplementationSelection::DispatchImplementer => {
            let feedback = latest_rework_feedback(app, &attempt.id)?;
            let prompt = crate::workflow_resources::render(
                RoleKind::Implementer,
                &serde_json::json!({
                    "task_id":attempt.task_id,
                    "attempt_id":attempt.id,
                    "phase":attempt.phase,
                    "title":attempt.title,
                    "description":attempt.description,
                    "acceptance_criteria":attempt.criteria,
                    "rework_feedback":feedback,
                    "required_outcome":"candidate_ready",
                }),
            )?;
            let launch = app.dispatch_attempt_role(&attempt.id, RoleKind::Implementer, &prompt)?;
            Ok(serde_json::json!({
                "action":"implementer_dispatched",
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
            }))
        }
        ImplementationSelection::StopManagerForCandidate => {
            queue_manager_notice(app, attempt, CANDIDATE_FROZEN_NOTICE, false)?;
            Ok(quiesce_manager_for_obligation(
                app,
                attempt,
                "current_frozen_candidate_code_review",
            )?
            .unwrap_or_else(|| waiting(attempt, "implementation_or_manager_transition")))
        }
        ImplementationSelection::WaitForManagerTransition => {
            if attempt.candidate_hash.is_some() {
                queue_manager_notice(app, attempt, CANDIDATE_FROZEN_NOTICE, false)?;
            }
            Ok(waiting(attempt, "implementation_or_manager_transition"))
        }
    }
}
enum YieldedLaneWriterState {
    Invalid {
        lane_key: String,
    },
    NotQuiescent {
        lane_key: String,
        session_id: String,
        generation_status: String,
        session_status: String,
    },
}

fn implementation_lane_state_on(
    connection: &Connection,
    attempt: &Attempt,
) -> Result<Option<ImplementationLaneState>> {
    let configured: i64 = connection.query_row(
        "SELECT COUNT(*) FROM implementation_lanes WHERE attempt_id=?1",
        params![attempt.id],
        |row| row.get(0),
    )?;
    if configured == 0 {
        return Ok(None);
    }
    crate::trip::require_configured_lanes_match_reviewed(connection, &attempt.id)?;
    let unyielded = connection.query_row(
        "SELECT COUNT(*) FROM implementation_lanes
         WHERE attempt_id=?1 AND required=1 AND state!='yielded'",
        params![attempt.id],
        |row| row.get(0),
    )?;
    let request = connection
        .query_row(
            "SELECT id,capsule_json FROM trip_integration_requests
             WHERE attempt_id=?1 AND state IN ('requested','dispatched')",
            params![attempt.id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(Some(ImplementationLaneState {
        configured,
        unyielded,
        request_id: request.as_ref().map(|value| value.0.clone()),
        capsule: request.map(|value| value.1),
    }))
}

fn yielded_lane_writer_state(
    connection: &Connection,
    attempt: &Attempt,
) -> Result<Option<YieldedLaneWriterState>> {
    let invalid = connection
        .query_row(
            "SELECT l.lane_key FROM implementation_lanes l
             WHERE l.attempt_id=?1 AND l.required=1 AND l.state='yielded' AND
               (l.yielded_at IS NULL OR l.receipt_json='{}' OR json_valid(l.receipt_json)=0
                OR NOT EXISTS(SELECT 1 FROM lane_generations lg
                  JOIN role_generations rg ON rg.id=lg.effective_generation_id
                  JOIN sessions s ON s.role_generation_id=rg.id AND s.lane_id=l.id
                  WHERE lg.lane_id=l.id AND rg.attempt_id=l.attempt_id
                    AND rg.role='implementer' AND rg.lane_id=l.id
                    AND rg.created_at<=l.yielded_at AND s.created_at<=l.yielded_at
                    AND s.id=(SELECT candidate.id FROM sessions candidate
                      WHERE candidate.role_generation_id=rg.id
                      ORDER BY candidate.created_at DESC,candidate.rowid DESC LIMIT 1)))
             ORDER BY l.lane_key LIMIT 1",
            params![attempt.id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(lane_key) = invalid {
        return Ok(Some(YieldedLaneWriterState::Invalid { lane_key }));
    }
    Ok(connection
        .query_row(
            "SELECT l.lane_key,s.id,rg.status,s.status
             FROM implementation_lanes l JOIN lane_generations lg ON lg.lane_id=l.id
             JOIN role_generations rg ON rg.id=lg.effective_generation_id
             JOIN sessions s ON s.role_generation_id=rg.id AND s.lane_id=l.id
             WHERE l.attempt_id=?1 AND l.required=1 AND l.state='yielded'
               AND l.yielded_at IS NOT NULL AND l.receipt_json!='{}'
               AND rg.attempt_id=l.attempt_id AND rg.role='implementer' AND rg.lane_id=l.id
               AND rg.created_at<=l.yielded_at AND s.created_at<=l.yielded_at
               AND s.id=(SELECT candidate.id FROM sessions candidate
                 WHERE candidate.role_generation_id=rg.id
                 ORDER BY candidate.created_at DESC,candidate.rowid DESC LIMIT 1)
               AND NOT(rg.status='exited' AND s.status='exited'
                 AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1)
             ORDER BY l.lane_key LIMIT 1",
            params![attempt.id],
            |row| {
                Ok(YieldedLaneWriterState::NotQuiescent {
                    lane_key: row.get(0)?,
                    session_id: row.get(1)?,
                    generation_status: row.get(2)?,
                    session_status: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn next_implementation_lane_on(
    connection: &Connection,
    attempt: &Attempt,
) -> Result<Option<LaneDispatch>> {
    let row: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT l.lane_key,
                    json_object('id',l.id,'lane_key',l.lane_key,
                      'owned_paths',json(l.owned_paths_json),
                      'shared_paths',json(l.shared_paths_json),
                      'protected_paths',json(l.protected_paths_json),
                      'dependencies',json(l.dependencies_json),
                      'source_hashes',json(l.source_hashes_json),
                      'frozen_seams_hash',l.frozen_seams_hash),
                    p.plan_json,json_extract(ap.profile_json,'$.session')
             FROM implementation_lanes l
             JOIN attempts a ON a.id=l.attempt_id
             JOIN trip_structured_plans p ON p.id=a.structured_plan_id
               AND p.plan_hash=a.plan_hash AND p.approved_at IS NOT NULL
               AND p.implementation_authorized_at IS NOT NULL
             JOIN trip_attempt_profiles ap ON ap.attempt_id=a.id AND ap.role='implementer'
             WHERE l.attempt_id=?1 AND l.required=1 AND l.state='admitted'
               AND NOT EXISTS(SELECT 1 FROM role_generations rg WHERE rg.lane_id=l.id)
               AND NOT EXISTS(SELECT 1 FROM json_each(l.dependencies_json) dependency
                 JOIN implementation_lanes prerequisite
                   ON prerequisite.attempt_id=l.attempt_id
                  AND prerequisite.lane_key=dependency.value
                 WHERE prerequisite.state!='yielded')
             ORDER BY l.lane_key LIMIT 1",
            params![attempt.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((lane_key, lane, structured_plan, profile_session)) = row else {
        return Ok(None);
    };
    if profile_session != "retained" {
        bail!("implementation lane profile violates the retained-session workflow contract")
    }
    Ok(Some(LaneDispatch {
        lane_key,
        profile_session,
        prompt_context: serde_json::json!({
            "task_id":attempt.task_id,
            "attempt_id":attempt.id,
            "phase":attempt.phase,
            "title":attempt.title,
            "description":attempt.description,
            "acceptance_criteria":attempt.criteria,
            "plan_hash":attempt.plan_hash,
            "approved_structured_plan":serde_json::from_str::<serde_json::Value>(&structured_plan)?,
            "implementation_lane":serde_json::from_str::<serde_json::Value>(&lane)?,
            "required_outcome":"yield_lane",
            "dispatch_contract":{
                "invocation":"initial",
                "profile_session":"retained",
                "automatic_retry":false,
                "manager_integration_required":true
            }
        }),
    }))
}

fn implementation_lane_waiting(app: &Application, attempt_id: &str) -> Result<serde_json::Value> {
    let connection = app.store.lock()?;
    let profile_session: String = connection.query_row(
        "SELECT json_extract(profile_json,'$.session') FROM trip_attempt_profiles
         WHERE attempt_id=?1 AND role='implementer'",
        params![attempt_id],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT l.lane_key,l.state,
                NOT EXISTS(SELECT 1 FROM json_each(l.dependencies_json) dependency
                  JOIN implementation_lanes prerequisite
                    ON prerequisite.attempt_id=l.attempt_id
                   AND prerequisite.lane_key=dependency.value
                  WHERE prerequisite.state!='yielded'),
                (SELECT rg.status FROM role_generations rg WHERE rg.lane_id=l.id
                  ORDER BY rg.generation DESC LIMIT 1),
                (SELECT s.status FROM sessions s JOIN role_generations rg
                  ON rg.id=s.role_generation_id WHERE rg.lane_id=l.id
                  ORDER BY rg.generation DESC,s.created_at DESC LIMIT 1),
                (SELECT s.launch_state FROM sessions s JOIN role_generations rg
                  ON rg.id=s.role_generation_id WHERE rg.lane_id=l.id
                  ORDER BY rg.generation DESC,s.created_at DESC LIMIT 1),
                (SELECT rc.state FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
                  JOIN role_generations rg ON rg.id=s.role_generation_id WHERE rg.lane_id=l.id
                  ORDER BY rc.created_at DESC LIMIT 1)
         FROM implementation_lanes l
         WHERE l.attempt_id=?1 AND l.required=1 AND l.state!='yielded'
         ORDER BY l.lane_key",
    )?;
    let rows = statement
        .query_map(params![attempt_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut initial_dispatch_eligible = Vec::new();
    let mut dependency_waiting = Vec::new();
    let mut active = Vec::new();
    let mut failed_waiting = Vec::new();
    let mut retained_restart_eligible = Vec::new();
    for (lane, state, dependencies_yielded, generation, session, launch, restart) in rows {
        if generation.is_none() {
            if dependencies_yielded && state == "admitted" {
                initial_dispatch_eligible.push(lane);
            } else {
                dependency_waiting.push(lane);
            }
            continue;
        }
        let detail = serde_json::json!({
            "lane_key":lane,
            "lane_state":state,
            "generation_status":generation,
            "session_status":session,
            "launch_state":launch,
            "restart_state":restart,
            "automatic_retry":false,
            "recovery":"explicit public recovery or exact retained resume required"
        });
        if detail["generation_status"] == "running"
            || detail["generation_status"] == "launch_reserved"
            || detail["generation_status"] == "stopping"
        {
            active.push(detail);
        } else {
            if matches!(
                detail["restart_state"].as_str(),
                Some("parked" | "queued_capacity")
            ) {
                retained_restart_eligible.push(detail["lane_key"].clone());
            }
            failed_waiting.push(detail);
        }
    }
    Ok(serde_json::json!({
        "profile_session":profile_session,
        "initial_dispatch_eligible":initial_dispatch_eligible,
        "dependency_waiting":dependency_waiting,
        "active":active,
        "failed_lanes_waiting":failed_waiting,
        "retained_restart_eligible":retained_restart_eligible,
        "retained_resume_policy":"parked or queued_capacity rows are eligible for the existing restart control; every other resume is revalidated by the existing public exact-resume path; the coordinator does not retry or replace a started lane"
    }))
}

fn advance_review(
    app: &Application,
    attempt: &Attempt,
    selection: ReviewSelection,
) -> Result<serde_json::Value> {
    let role = selection.role.to_string();
    match selection.action {
        ReviewAction::QuiesceCompleted { session_id } => {
            app.supervisor.interrupt(&session_id)?;
            Ok(serde_json::json!({
                "action":"quiescing_completed_role",
                "role":role,
                "session_id":session_id,
            }))
        }
        ReviewAction::ConsumeResult => {
            if let Some(value) = consume_review_result(app, attempt, selection.kind)? {
                Ok(value)
            } else {
                advance_review_request(app, attempt, selection.kind, selection.role)
            }
        }
        ReviewAction::Dispatch | ReviewAction::Wait { .. } => {
            advance_review_request(app, attempt, selection.kind, selection.role)
        }
    }
}

fn advance_review_request(
    app: &Application,
    attempt: &Attempt,
    kind: &str,
    role: RoleKind,
) -> Result<serde_json::Value> {
    let prompt = crate::workflow_resources::render(
        role,
        &serde_json::json!({
            "task_id":attempt.task_id,
            "attempt_id":attempt.id,
            "phase":attempt.phase,
            "review_kind":kind,
            "plan_hash":attempt.plan_hash,
            "candidate_hash":attempt.candidate_hash,
            "title":attempt.title,
            "description":attempt.description,
            "acceptance_criteria":attempt.criteria,
        }),
    )?;
    let handoff = serde_json::json!({
        "task_id":attempt.task_id,
        "attempt_id":attempt.id,
        "phase":attempt.phase,
        "plan_hash":attempt.plan_hash,
        "candidate_hash":attempt.candidate_hash,
    });
    let request = app
        .reviews
        .reserve_request(&attempt.id, kind, &prompt, handoff)?;
    match request.state.as_str() {
        "reserved" | "nondelivered" => {
            let launch = app.dispatch_reserved_review(&request)?;
            Ok(serde_json::json!({
                "action":"review_dispatched",
                "kind":kind,
                "request_id":request.request_id,
                "session_id":launch.session_id,
            }))
        }
        "delivered" | "launching" => Ok(serde_json::json!({
            "action":"waiting",
            "for":"review_result",
            "request_id":request.request_id,
        })),
        "ambiguous" => Ok(serde_json::json!({
            "action":"held",
            "reason":"review_delivery_ambiguous",
            "request_id":request.request_id,
        })),
        state => Ok(serde_json::json!({
            "action":"held",
            "reason":format!("review_request_{state}"),
            "request_id":request.request_id,
        })),
    }
}
fn consume_review_result(
    app: &Application,
    attempt: &Attempt,
    kind: &str,
) -> Result<Option<serde_json::Value>> {
    let expected_role = match kind {
        "plan" => "plan_reviewer",
        "code" => "code_reviewer",
        "final" => "final_verifier",
        _ => bail!("unknown review kind"),
    };
    let now = Utc::now().to_rfc3339();
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = eligible_review_result(&transaction, attempt, kind, expected_role)?;
    let Some((request_id, result_id, verdict, summary, metadata)) = row else {
        return Ok(None);
    };
    if !["approved", "request_changes", "needs_rework"].contains(&verdict.as_str()) {
        bail!("review returned an unsupported verdict")
    }
    if kind == "final" && verdict == "request_changes" {
        let repair_allowed:bool=transaction.query_row(
            "SELECT a.final_repair_round=0 AND EXISTS(SELECT 1 FROM review_requests code
               WHERE code.attempt_id=a.id AND code.review_kind='code' AND code.candidate_hash=a.candidate_hash
                 AND code.verdict='approved' AND code.delivery_state='finished')
             FROM attempts a WHERE a.id=?1",params![attempt.id],|row|row.get(0)
        )?;
        if !repair_allowed {
            bail!("final request_changes exceeds the one dedicated repair cycle or lacks retained code approval")
        }
        transaction.execute(
            "UPDATE attempts SET final_repair_round=1 WHERE id=?1 AND final_repair_round=0",
            params![attempt.id],
        )?;
    }
    transaction.execute("UPDATE review_requests SET delivery_state='finished',verdict=?1,feedback=?2,updated_at=?3 WHERE id=?4 AND delivery_state='delivered'",params![verdict,summary,now,request_id])?;
    let phase = crate::workflow::review_transition_phase(kind, &verdict)?;
    let clearing = verdict != "approved";
    transaction.execute(
        "UPDATE attempts SET phase=?1,
         plan_hash=CASE WHEN ?2 AND ?3='plan' THEN NULL ELSE plan_hash END,
         plan_approved_at=CASE WHEN ?2 AND ?3='plan' THEN NULL ELSE plan_approved_at END,
         candidate_hash=CASE WHEN ?2 THEN NULL ELSE candidate_hash END,
         accepted_snapshot_id=CASE WHEN ?2 THEN NULL ELSE accepted_snapshot_id END,
         updated_at=?4 WHERE id=?5",
        params![phase, clearing, kind, now, attempt.id],
    )?;
    transaction.execute(
        "UPDATE role_results SET consumed_at=?1 WHERE id=?2 AND consumed_at IS NULL",
        params![now, result_id],
    )?;
    if kind == "plan" && verdict != "approved" {
        crate::store::record_plan_rejection(
            &transaction,
            &attempt.id,
            &request_id,
            attempt
                .plan_hash
                .as_deref()
                .ok_or_else(|| anyhow!("plan rejection lost its frozen plan hash"))?,
            &verdict,
            &summary,
            &now,
        )?;
    }
    if kind == "plan" && verdict == "approved" {
        transaction.execute(
            "UPDATE trip_structured_plans SET review_request_id=?1,reviewed_at=?2
             WHERE attempt_id=?3 AND plan_hash=?4 AND reviewed_at IS NULL",
            params![request_id, now, attempt.id, attempt.plan_hash],
        )?;
    }
    transaction.execute(
        "UPDATE tasks SET version=version+1,
         attention=CASE
           WHEN ?1='needs_rework' AND attention NOT IN ('paused','pause_requested','needs_recovery')
             THEN 'needs_input'
           ELSE attention
         END,
         updated_at=?2 WHERE id=?3",
        params![verdict, now, attempt.task_id],
    )?;
    let op = uuid::Uuid::new_v4().to_string();
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','review.result.applied','review_request',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),op,request_id,serde_json::json!({"kind":kind,"verdict":verdict,"metadata":serde_json::from_str::<serde_json::Value>(&metadata)?}).to_string(),now])?;
    transaction.commit()?;
    Ok(Some(
        serde_json::json!({"action":"review_applied","kind":kind,"verdict":verdict,"request_id":request_id,"phase":phase}),
    ))
}

fn eligible_review_result(
    connection: &Connection,
    attempt: &Attempt,
    kind: &str,
    expected_role: &str,
) -> Result<Option<(String, String, String, String, String)>> {
    Ok(connection
        .query_row(
            "SELECT r.id,rr.id,rr.outcome,rr.summary,rr.metadata_json FROM review_requests r
             JOIN role_results rr ON rr.role_generation_id=r.role_generation_id AND rr.session_id=r.session_id
             JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN sessions s ON s.id=rr.session_id
             WHERE r.attempt_id=?1 AND r.review_kind=?2 AND r.delivery_state='delivered' AND rg.role=?3
               AND s.status='exited'
               AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
               AND json_extract(rr.metadata_json,'$.review_kind')=r.review_kind
               AND json_extract(rr.metadata_json,'$.candidate_hash')=r.candidate_hash
             ORDER BY rr.created_at LIMIT 1",
            params![attempt.id, kind, expected_role],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?)
}

fn blocked_check_on(
    connection: &Connection,
    attempt: &Attempt,
    candidate: &str,
) -> Result<Option<(String, String)>> {
    Ok(connection
        .query_row(
            "SELECT COALESCE(check_id,suite_name),status FROM check_runs
             WHERE attempt_id=?1 AND candidate_hash=?2
               AND selected_check_revision=(SELECT selected_checks_revision FROM attempts WHERE id=?1)
               AND (status IN ('launch_ambiguous','launch_failed','precondition_failed')
                 OR launch_state='capture_failed'
                 OR (status='finished' AND COALESCE(exit_code,-1)!=0))
             ORDER BY created_at DESC LIMIT 1",
            params![attempt.id, candidate],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn next_required_check_on(
    connection: &Connection,
    attempt: &Attempt,
    candidate: &str,
) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT selected.check_id FROM trip_selected_checks selected
             JOIN attempts current ON current.id=selected.attempt_id
             WHERE selected.attempt_id=?1
               AND selected.revision=current.selected_checks_revision AND selected.required=1
               AND NOT EXISTS(SELECT 1 FROM check_runs run
                 WHERE run.attempt_id=selected.attempt_id AND run.candidate_hash=?2
                   AND run.check_id=selected.check_id
                   AND run.selected_check_revision=selected.revision
                   AND run.status='finished' AND run.exit_code=0
                   AND run.freshness_state='current')
             ORDER BY selected.check_id LIMIT 1",
            params![attempt.id, candidate],
            |row| row.get(0),
        )
        .optional()?)
}

fn selected_check_completion_on(
    connection: &Connection,
    attempt: &Attempt,
    candidate: &str,
) -> Result<(i64, bool)> {
    Ok(connection.query_row(
        "SELECT (SELECT COUNT(*) FROM trip_selected_checks selected
                   JOIN attempts current ON current.id=selected.attempt_id
                   WHERE selected.attempt_id=?1
                     AND selected.revision=current.selected_checks_revision
                     AND selected.required=1),
                EXISTS(SELECT 1 FROM check_runs
                  WHERE attempt_id=?1 AND candidate_hash=?2
                    AND status IN ('running','recovery_required'))",
        params![attempt.id, candidate],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

fn manager_conformance_ready_on(connection: &Connection, attempt: &Attempt) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts current JOIN trip_conformance_receipts receipt
           ON receipt.attempt_id=current.id AND receipt.revision=current.manager_conformance_revision
           AND receipt.candidate_hash=current.candidate_hash WHERE current.id=?1)
         AND NOT EXISTS(SELECT 1 FROM implementation_lanes
           WHERE attempt_id=?1 AND required=1 AND state!='yielded')
         AND NOT EXISTS(SELECT 1 FROM role_generations
           WHERE attempt_id=?1 AND role='implementer'
             AND status IN ('launch_reserved','running','stopping'))",
        params![attempt.id],
        |row| row.get(0),
    )?)
}

fn final_explorer_ready_on(
    connection: &Connection,
    attempt: &Attempt,
    candidate: &str,
) -> Result<Option<bool>> {
    Ok(connection
        .query_row(
            "SELECT activated=0 OR outcome_json IS NOT NULL FROM trip_explorer_decisions
             WHERE attempt_id=?1 AND stage='final' AND candidate_hash=?2
             ORDER BY created_at DESC LIMIT 1",
            params![attempt.id, candidate],
            |row| row.get(0),
        )
        .optional()?)
}

fn advance_checks(
    app: &Application,
    attempt: &Attempt,
    selection: ChecksSelection,
) -> Result<serde_json::Value> {
    match selection {
        ChecksSelection::MissingCandidate => bail!("checks require a frozen candidate"),
        ChecksSelection::CheckFailed { check_id, status } => {
            set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
            Ok(serde_json::json!({
                "action":"held",
                "reason":"check_failure",
                "suite":check_id,
                "status":status,
            }))
        }
        ChecksSelection::RunCheck { check_id } => {
            if !app.checks.selected_authorized(&attempt.id, &check_id)? {
                set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
                return Ok(serde_json::json!({
                    "action":"held",
                    "reason":"selected_check_requires_service_permission",
                    "check_id":check_id,
                }));
            }
            let result = app.checks.run_selected(&attempt.id, &check_id)?;
            if result.get("passed").and_then(serde_json::Value::as_bool) != Some(true) {
                set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
            }
            Ok(serde_json::json!({
                "action":"check_finished",
                "check_id":check_id,
                "result":result,
            }))
        }
        ChecksSelection::NoSelectedChecks => {
            set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
            Ok(serde_json::json!({
                "action":"held",
                "reason":"no_plan_selected_trip_checks",
            }))
        }
        ChecksSelection::CheckExecutionUnsettled => Ok(serde_json::json!({
            "action":"held",
            "reason":"check_failure",
        })),
        ChecksSelection::WaitForManagerStop { session_id } => Ok(serde_json::json!({
            "action":"waiting",
            "for":"manager_service_stop_quiescence",
            "attempt_id":attempt.id,
            "session_id":session_id,
        })),
        ChecksSelection::DispatchConformanceManager => {
            let prompt = crate::workflow_resources::render(
                RoleKind::Manager,
                &serde_json::json!({
                    "task_id":attempt.task_id,
                    "attempt_id":attempt.id,
                    "phase":attempt.phase,
                    "candidate_hash":attempt.candidate_hash,
                    "coordination_contract":"load current role context and submit exact current manager conformance; preserve all human holds and do not fabricate check or lane evidence"
                }),
            )?;
            let (launch, resumed) = dispatch_or_resume_manager(app, &attempt.id, &prompt)?;
            Ok(serde_json::json!({
                "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
                "context":"checks_conformance",
            }))
        }
        ChecksSelection::StopManagerForConformance | ChecksSelection::ConformanceMissing => {
            queue_manager_notice(app, attempt, CONFORMANCE_NOTICE, false)?;
            if let Some(value) =
                quiesce_manager_for_obligation(app, attempt, "current_checks_conformance")?
            {
                return Ok(value);
            }
            set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
            Ok(serde_json::json!({
                "action":"held",
                "reason":"manager_conformance_or_lane_yield_missing",
            }))
        }
        ChecksSelection::DispatchFinalExplorerManager => {
            let prompt = crate::workflow_resources::render(
                RoleKind::Manager,
                &serde_json::json!({
                    "task_id":attempt.task_id,
                    "attempt_id":attempt.id,
                    "phase":attempt.phase,
                    "candidate_hash":attempt.candidate_hash,
                    "coordination_contract":"load current role context and record the exact current final Explorer activation decision; preserve explicit Explorer and human authorization gates"
                }),
            )?;
            let (launch, resumed) = dispatch_or_resume_manager(app, &attempt.id, &prompt)?;
            Ok(serde_json::json!({
                "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
                "context":"checks_final_explorer_decision",
            }))
        }
        ChecksSelection::StopManagerForFinalExplorer
        | ChecksSelection::WaitForFinalExplorerDecision => {
            queue_manager_notice(app, attempt, FINAL_EXPLORER_NOTICE, false)?;
            if let Some(value) =
                quiesce_manager_for_obligation(app, attempt, "current_final_explorer_decision")?
            {
                return Ok(value);
            }
            Ok(serde_json::json!({
                "action":"waiting",
                "for":"manager_final_explorer_decision",
                "attempt_id":attempt.id,
                "candidate_hash":attempt.candidate_hash,
            }))
        }
        ChecksSelection::WaitForActivatedExplorer => Ok(serde_json::json!({
            "action":"waiting",
            "for":"activated_final_explorer_evidence",
            "attempt_id":attempt.id,
            "candidate_hash":attempt.candidate_hash,
        })),
        ChecksSelection::Complete(boundary) => {
            if let Some(wait) = advance_manager_boundary(
                app,
                attempt,
                "completed_checks_and_final_explorer",
                &boundary,
            )? {
                return Ok(wait);
            }
            let candidate = attempt
                .candidate_hash
                .as_deref()
                .ok_or_else(|| anyhow!("checks require a frozen candidate"))?;
            if !consume_completed_checks(app, attempt, candidate)? {
                return Ok(serde_json::json!({
                    "action":"waiting",
                    "for":"completed_checks_manager_boundary_recheck",
                    "attempt_id":attempt.id,
                    "candidate_hash":candidate,
                }));
            }
            Ok(serde_json::json!({
                "action":"checks_approved",
                "attempt_id":attempt.id,
                "phase":"final_review",
            }))
        }
    }
}
fn advance_handoff(
    app: &Application,
    attempt: &Attempt,
    selection: HandoffSelection,
) -> Result<serde_json::Value> {
    match selection {
        HandoffSelection::Complete(boundary) => {
            if let Some(wait) =
                advance_manager_boundary(app, attempt, "completed_final_review_handoff", &boundary)?
            {
                return Ok(wait);
            }
            let snapshot = app.reviews.freeze(&attempt.id, "accepted")?;
            let snapshot_id = snapshot["snapshot_id"]
                .as_str()
                .ok_or_else(|| anyhow!("accepted snapshot result omitted its identity"))?;
            if !consume_completed_handoff(app, attempt, snapshot_id)? {
                return Ok(serde_json::json!({
                    "action":"waiting",
                    "for":"completed_handoff_manager_boundary_recheck",
                    "attempt_id":attempt.id,
                    "candidate_hash":attempt.candidate_hash,
                    "orphaned_snapshot_id":snapshot_id,
                    "snapshot_reusable":true,
                }));
            }
            Ok(serde_json::json!({
                "action":"human_review_ready",
                "attempt_id":attempt.id,
                "snapshot":snapshot,
            }))
        }
        HandoffSelection::WaitForManagerStop { session_id } => Ok(serde_json::json!({
            "action":"waiting",
            "for":"manager_service_stop_quiescence",
            "attempt_id":attempt.id,
            "session_id":session_id,
        })),
        HandoffSelection::DispatchManager => {
            let prompt = crate::workflow_resources::render(
                RoleKind::Manager,
                &serde_json::json!({
                    "task_id":attempt.task_id,
                    "attempt_id":attempt.id,
                    "phase":attempt.phase,
                    "candidate_hash":attempt.candidate_hash,
                    "coordination_contract":"inspect the exact final-review request and checks, report handoff_ready, then propose human_review"
                }),
            )?;
            let (launch, resumed) = dispatch_or_resume_manager(app, &attempt.id, &prompt)?;
            Ok(serde_json::json!({
                "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
                "context":"final_handoff",
            }))
        }
        HandoffSelection::StopManager | HandoffSelection::WaitForManagerHandoff => {
            let final_review: String = {
                let connection = app.store.lock()?;
                connection.query_row(
                    "SELECT json_object('final_review_request_id',id,'candidate_hash',candidate_hash,'reviewer_generation_id',role_generation_id,'verdict',verdict,'delivery_state',delivery_state)
                     FROM review_requests
                     WHERE attempt_id=?1 AND review_kind='final' AND candidate_hash=?2
                       AND verdict='approved' AND delivery_state='finished'
                     ORDER BY updated_at DESC LIMIT 1",
                    params![attempt.id, attempt.candidate_hash],
                    |row| row.get(0),
                )?
            };
            queue_manager_notice(
                app,
                attempt,
                &format!(
                    "Final review approved. Read context and report handoff_ready using this exact approved tuple: {final_review}. Inspect the exact candidate and checks before proposing phase human_review."
                ),
                false,
            )?;
            if let Some(value) =
                quiesce_manager_for_obligation(app, attempt, "current_final_review_handoff")?
            {
                return Ok(value);
            }
            Ok(waiting(attempt, "manager_handoff"))
        }
    }
}
fn apply_manager_proposal(
    app: &Application,
    attempt: &Attempt,
    target: &str,
) -> Result<Option<serde_json::Value>> {
    let now = Utc::now().to_rfc3339();
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let proposal = eligible_manager_transition(&transaction, attempt, target)?;
    let Some((id, payload)) = proposal else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&payload)?;
    if value
        .get("evidence")
        .and_then(|v| v.as_array())
        .is_none_or(Vec::is_empty)
    {
        bail!("manager transition proposal has no structured evidence")
    }
    transaction.execute(
        "UPDATE attempts SET phase=?1,updated_at=?2 WHERE id=?3 AND phase=?4",
        params![target, now, attempt.id, attempt.phase],
    )?;
    transaction.execute(
        "UPDATE controls SET state='finished',updated_at=?1 WHERE id=?2 AND state='proposed'",
        params![now, id],
    )?;
    transaction.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2",
        params![now, attempt.task_id],
    )?;
    transaction.commit()?;
    Ok(Some(
        serde_json::json!({"action":"manager_transition_applied","proposal_id":id,"phase":target}),
    ))
}

fn eligible_manager_transition(
    connection: &Connection,
    attempt: &Attempt,
    target: &str,
) -> Result<Option<(String, String)>> {
    Ok(connection
        .query_row(
            "SELECT c.id,c.payload_json FROM controls c JOIN role_generations rg ON rg.id=c.role_generation_id
             JOIN sessions s ON s.role_generation_id=rg.id JOIN attempts a ON a.id=c.attempt_id
             JOIN tasks t ON t.id=a.task_id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager'
             WHERE c.attempt_id=?1 AND c.kind='transition_proposal' AND c.state='proposed'
               AND rg.role='manager' AND json_extract(c.payload_json,'$.phase')=?2
               AND json_extract(c.payload_json,'$.source_phase')=a.phase
               AND COALESCE(json_extract(c.payload_json,'$.plan_hash'),'')=COALESCE(a.plan_hash,'')
               AND COALESCE(json_extract(c.payload_json,'$.candidate_hash'),'')=COALESCE(a.candidate_hash,'')
               AND json_extract(c.payload_json,'$.role_generation_id')=rg.id
               AND c.expected_version=t.version
               AND json_extract(c.payload_json,'$.expected_task_version')=t.version
               AND ((rg.status='exited' AND s.status='exited'
                      AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1)
                    OR (rg.status='running' AND s.status='running'
                      AND s.readiness_state='idle_candidate'))
             ORDER BY c.created_at LIMIT 1",
            params![attempt.id, target],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn retry_one_guidance(app: &Application) -> Result<Option<serde_json::Value>> {
    Ok(app
        .roles
        .retry_ready_guidance()?
        .into_iter()
        .find(|value| value.get("state").and_then(|state| state.as_str()) != Some("queued")))
}

fn retry_one_rework(app: &Application) -> Result<Option<serde_json::Value>> {
    let id = {
        let connection = app.store.lock()?;
        connection.query_row("SELECT new_attempt_id FROM rework_intents WHERE state IN ('reserved','materializing') ORDER BY created_at LIMIT 1",[],|row|row.get::<_,String>(0)).optional()?
    };
    let Some(id) = id else { return Ok(None) };
    {
        let connection = app.store.lock()?;
        let project: String = connection.query_row(
            "SELECT t.project_id FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
            params![id],
            |row| row.get(0),
        )?;
        crate::trip::require_project_ready(&connection, &project)?;
    }
    if !app.prepare_rework(&id)? {
        return Ok(None);
    }
    Ok(Some(
        serde_json::json!({"action":"rework_materialized","attempt_id":id}),
    ))
}

fn fail_manager_control(
    app: &Application,
    id: &str,
    reason: &str,
    next_action: &str,
) -> Result<()> {
    let connection = app.store.lock()?;
    connection.execute(
        "UPDATE controls SET state='failed',
                payload_json=json_set(payload_json,'$.failure',?1,'$.next_action',?2),updated_at=?3
         WHERE id=?4 AND kind IN ('manager_stop','manager_change')",
        params![reason, next_action, Utc::now().to_rfc3339(), id],
    )?;
    Ok(())
}

enum ManagerChangeBoundary {
    Quiescent,
    NativeIdle(crate::roles::ManagerSafeIdleBoundary),
}

fn manager_change_boundary(
    app: &Application,
    control_id: &str,
    attempt: &str,
    old_generation: &str,
) -> Result<Option<ManagerChangeBoundary>> {
    let unblocked: bool = {
        let connection = app.store.lock()?;
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
             WHERE a.id=?1 AND a.status='running' AND t.attention='none'
               AND t.archived_at IS NULL
               AND NOT EXISTS(SELECT 1 FROM controls other
                 WHERE other.attempt_id=a.id AND other.id!=?2
                   AND other.state NOT IN ('finished','cancelled','superseded','rejected')))",
            params![attempt, control_id],
            |row| row.get(0),
        )?
    };
    if !unblocked {
        return Ok(None);
    }
    let quiescent: bool = {
        let connection = app.store.lock()?;
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE role_generation_id=?1)
               AND NOT EXISTS(SELECT 1 FROM sessions WHERE role_generation_id=?1
                 AND (status!='exited' OR COALESCE(json_extract(exit_json,'$.process_group_quiescent'),0)!=1))",
            params![old_generation],
            |row| row.get(0),
        )?
    };
    if quiescent {
        return Ok(Some(ManagerChangeBoundary::Quiescent));
    }
    let boundary: Option<(String, i64)> = {
        let connection = app.store.lock()?;
        connection
            .query_row(
                "SELECT s.id,stop.rowid FROM sessions s
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN role_credentials credential ON credential.role_generation_id=rg.id
                   AND credential.revoked_at IS NULL
                 JOIN hook_events stop ON stop.session_id=s.id
                   AND stop.role_generation_id=rg.id AND stop.event_name='Stop'
                   AND stop.native_session_id=s.native_session_id
                   AND stop.rowid=(SELECT MAX(latest.rowid) FROM hook_events latest
                     WHERE latest.session_id=s.id)
                 WHERE s.role_generation_id=?1 AND rg.attempt_id=?2 AND rg.role='manager'
                   AND rg.status='running' AND s.status='running'
                   AND s.readiness_state='idle_candidate'
                   AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
                   AND s.id=(SELECT latest.id FROM sessions latest
                     WHERE latest.role_generation_id=rg.id
                     ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
                   AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                     WHERE permission.session_id=s.id AND permission.consumed_at IS NULL
                       AND permission.delivery_state NOT IN ('expired','not_delivered'))
                   AND NOT EXISTS(SELECT 1 FROM input_leases lease
                     WHERE lease.session_id=s.id AND lease.revoked_at IS NULL
                       AND julianday(lease.expires_at)>julianday('now'))
                   AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
                     WHERE guidance.role_generation_id=rg.id
                       AND guidance.state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))
                   AND NOT EXISTS(SELECT 1 FROM controls other
                     WHERE other.attempt_id=?2 AND other.id!=?3
                       AND other.state NOT IN ('finished','cancelled','superseded','rejected'))
                   AND NOT EXISTS(SELECT 1 FROM restart_candidates restart
                     WHERE restart.attempt_id=?2
                       AND restart.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
                   AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                     WHERE recovery.attempt_id=?2 AND recovery.state='attention_required')
                   AND NOT EXISTS(SELECT 1 FROM freeze_intents freeze
                     WHERE freeze.attempt_id=?2
                       AND freeze.state IN ('reserved','capturing','recovery_required'))
                   AND NOT EXISTS(SELECT 1 FROM check_runs check_run
                     WHERE check_run.attempt_id=?2
                       AND check_run.status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))
                 ORDER BY s.created_at DESC,s.rowid DESC LIMIT 1",
                params![old_generation, attempt, control_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
    };
    let Some((session, stop_event_rowid)) = boundary else {
        return Ok(None);
    };
    // idle_candidate is assigned only by the correlated native SessionStart,
    // submit, Stop, hook/registry, and permission contract. The supervisor adds
    // the independent captured-process and descendant inventory check.
    if !matches!(app.manager_control_native_idle_ready(&session), Ok(true)) {
        return Ok(None);
    }
    Ok(Some(ManagerChangeBoundary::NativeIdle(
        crate::roles::ManagerSafeIdleBoundary {
            control_id: control_id.into(),
            session_id: session,
            stop_event_rowid,
        },
    )))
}

fn manager_change_interrupt_session(
    app: &Application,
    old_generation: &str,
) -> Result<Option<String>> {
    let connection = app.store.lock()?;
    connection
        .query_row(
            "SELECT id FROM sessions WHERE role_generation_id=?1
               AND status IN ('running','recovery_required')
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            params![old_generation],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn process_one_manager_control(app: &Application) -> Result<Option<serde_json::Value>> {
    let control = {
        let connection = app.store.lock()?;
        let emergency_stop_all: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM controls WHERE kind IN ('pause_now','cancel')
               AND state IN ('requested','draining'))",
            [],
            |row| row.get(0),
        )?;
        if emergency_stop_all {
            return Ok(None);
        }
        connection.query_row(
            "SELECT c.id,c.attempt_id,c.kind,c.state,c.payload_json
             FROM controls c WHERE (c.kind='manager_stop' AND c.state='held')
                OR (c.kind='manager_change' AND c.state IN ('waiting_safe_boundary','switch_requested'))
             ORDER BY c.created_at LIMIT 1",
            [],
            |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, row.get::<_, String>(4)?,
            )),
        ).optional()?
    };
    let Some((id, attempt, kind, _state, payload_json)) = control else {
        return Ok(None);
    };
    let payload: serde_json::Value = match serde_json::from_str(&payload_json) {
        Ok(payload) => payload,
        Err(error) => {
            fail_manager_control(
                app,
                &id,
                &format!("invalid durable manager control payload: {error}"),
                "Submit a fresh manager control with the current task version.",
            )?;
            return Ok(Some(
                serde_json::json!({"action":"manager_control_failed","control_id":id,"attempt_id":attempt}),
            ));
        }
    };
    if kind == "manager_stop" {
        let Some(session) = payload
            .get("manager_session_id")
            .and_then(serde_json::Value::as_str)
        else {
            fail_manager_control(
                app,
                &id,
                "manager stop is missing its captured manager session",
                "Submit Stop manager again with the current task version.",
            )?;
            return Ok(Some(
                serde_json::json!({"action":"manager_stop_failed","control_id":id,"attempt_id":attempt}),
            ));
        };
        let quiescent: bool = {
            let connection = app.store.lock()?;
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND status='exited'
                   AND COALESCE(json_extract(exit_json,'$.process_group_quiescent'),0)=1)",
                params![session],
                |row| row.get(0),
            )?
        };
        if quiescent {
            let connection = app.store.lock()?;
            connection.execute(
                "UPDATE controls SET payload_json=json_set(payload_json,'$.quiescent',true,
                         '$.native_resume_forbidden',true,
                         '$.resume_fence_generation_id',(SELECT role_generation_id FROM sessions WHERE id=?1),
                         '$.next_action',?2),updated_at=?3
                 WHERE id=?4 AND kind='manager_stop' AND state='held'",
                params![session, "Manager is verified stopped. Continue manager releases only this manager hold; Change manager may now replace it.", Utc::now().to_rfc3339(), id],
            )?;
            return Ok(Some(
                serde_json::json!({"action":"manager_stop_quiescent","control_id":id,"attempt_id":attempt,"quiescent":true}),
            ));
        }
        if payload
            .get("signal_state")
            .and_then(serde_json::Value::as_str)
            == Some("pending")
        {
            if let Err(error) = app.interrupt_completed_role(session) {
                fail_manager_control(
                    app,
                    &id,
                    &format!("manager-only interrupt request failed: {error:#}"),
                    "Resolve the manager process ownership failure, then submit a fresh Stop manager control.",
                )?;
                return Ok(Some(
                    serde_json::json!({"action":"manager_stop_failed","control_id":id,"attempt_id":attempt}),
                ));
            }
            let connection = app.store.lock()?;
            connection.execute(
                "UPDATE controls SET payload_json=json_set(payload_json,'$.signal_state','requested','$.next_action',?1),updated_at=?2
                 WHERE id=?3 AND kind='manager_stop' AND state='held'",
                params!["Wait for the captured manager process group to report a verified exit.", Utc::now().to_rfc3339(), id],
            )?;
            return Ok(Some(
                serde_json::json!({"action":"manager_stop_interrupt_requested","control_id":id,"attempt_id":attempt}),
            ));
        }
        return Ok(Some(
            serde_json::json!({"action":"manager_stop_waiting_for_quiescence","control_id":id,"attempt_id":attempt}),
        ));
    }

    let parsed = (
        payload
            .get("old_generation_id")
            .and_then(serde_json::Value::as_str),
        payload
            .get("settings_revision")
            .and_then(serde_json::Value::as_i64),
        payload
            .get("switch_operation_id")
            .and_then(serde_json::Value::as_str),
        payload
            .get("expected_task_version")
            .and_then(serde_json::Value::as_i64),
    );
    let (Some(old_generation), Some(settings_revision), Some(operation_id), Some(expected_version)) =
        parsed
    else {
        fail_manager_control(
            app,
            &id,
            "manager change is missing a captured generation, revision, operation, or version",
            "Submit Change manager again with the current task version.",
        )?;
        return Ok(Some(
            serde_json::json!({"action":"manager_change_failed","control_id":id,"attempt_id":attempt}),
        ));
    };
    let safe_boundary =
        payload.get("mode").and_then(serde_json::Value::as_str) == Some("safe_boundary");
    let boundary = if safe_boundary {
        let Some(boundary) = manager_change_boundary(app, &id, &attempt, old_generation)? else {
            return Ok(Some(
                serde_json::json!({"action":"manager_change_waiting_safe_boundary","control_id":id,"attempt_id":attempt,"old_generation_id":old_generation}),
            ));
        };
        Some(boundary)
    } else {
        None
    };
    let safe_idle = match boundary.as_ref() {
        Some(ManagerChangeBoundary::NativeIdle(boundary)) => Some(boundary),
        Some(ManagerChangeBoundary::Quiescent) | None => None,
    };
    let interrupt_session = match safe_idle {
        Some(boundary) => Some(boundary.session_id.clone()),
        None if matches!(boundary.as_ref(), Some(ManagerChangeBoundary::Quiescent)) => None,
        None => manager_change_interrupt_session(app, old_generation)?,
    };
    let switch = app.roles.request_manager_change(
        operation_id,
        &attempt,
        old_generation,
        settings_revision,
        expected_version,
        safe_idle,
    );
    let switch = match switch {
        Ok(switch) => switch,
        Err(error) => {
            fail_manager_control(
                app,
                &id,
                &format!("manager replacement was not eligible: {error:#}"),
                "Prepare and activate exact capability proof for the requested manager revision, then submit a fresh Change manager control.",
            )?;
            return Ok(Some(
                serde_json::json!({"action":"manager_change_failed","control_id":id,"attempt_id":attempt,"current_manager_retained":true}),
            ));
        }
    };
    if let Some(session) = interrupt_session {
        if let Err(error) = app.interrupt_completed_role(&session) {
            let reason = format!("manager-only replacement stop request failed: {error:#}");
            app.store
                .fail_manager_switch_after_signal_failure(&switch, &id, &reason)?;
            return Ok(Some(
                serde_json::json!({"action":"manager_change_failed","control_id":id,"attempt_id":attempt,"switch_intent_id":switch,"recovery_required":true,"current_manager_retained":false}),
            ));
        }
    }
    let connection = app.store.lock()?;
    let changed = connection.execute(
        "UPDATE controls SET state='switching',
                payload_json=json_set(payload_json,'$.switch_intent_id',?1,'$.next_action',?2),updated_at=?3
         WHERE id=?4 AND kind='manager_change' AND state IN ('waiting_safe_boundary','switch_requested')",
        params![
            switch,
            "Old authority is revoked. Wait for verified quiescence, then the captured replacement may launch.",
            Utc::now().to_rfc3339(),
            id
        ],
    )?;
    if changed != 1 {
        bail!("manager change control was superseded while its exact replacement was created")
    }
    Ok(Some(
        serde_json::json!({"action":"manager_change_authority_revoked","control_id":id,"attempt_id":attempt,"switch_intent_id":switch}),
    ))
}

fn advance_one_switch(app: &Application) -> Result<Option<serde_json::Value>> {
    let intent = {
        let connection = app.store.lock()?;
        connection.query_row("SELECT si.id,si.state,si.handoff_json,si.role,si.attempt_id
            FROM switch_intents si JOIN attempts a ON a.id=si.attempt_id JOIN tasks t ON t.id=a.task_id
            WHERE (si.state='stopping_old' OR (si.state='ready_for_dispatch' AND a.status='running' AND t.attention='none'))
              AND (si.role!='manager' OR NOT EXISTS(
                SELECT 1 FROM controls c WHERE c.attempt_id=si.attempt_id
                  AND c.kind IN ('manager_stop','manager_change')
                  AND c.state NOT IN ('finished','cancelled','superseded','rejected')
                  AND (c.kind!='manager_change' OR c.state!='switching'
                    OR json_extract(c.payload_json,'$.switch_intent_id') IS NOT si.id)
              ))
            ORDER BY si.created_at LIMIT 1",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?))).optional()?
    };
    let Some((id, state, handoff, role, attempt)) = intent else {
        return Ok(None);
    };
    if state == "stopping_old" {
        let value = match app.roles.finish_switch(&id) {
            Ok(value) => value,
            Err(error)
                if format!("{error:#}").contains("active or process ownership is unknown")
                    || format!("{error:#}").contains("verified quiescent process-group exit") =>
            {
                return Ok(Some(
                    serde_json::json!({"action":"switch_waiting_for_quiescence","intent_id":id,"attempt_id":attempt}),
                ))
            }
            Err(error) => {
                return disposition_switch_failure(
                    app,
                    &id,
                    &attempt,
                    &role,
                    "stopping_old",
                    &format!("replacement quiescence failed: {error:#}"),
                );
            }
        };
        return Ok(Some(
            serde_json::json!({"action":"switch_old_quiescent","result":value}),
        ));
    }
    let prompt = match (|| -> Result<String> {
        let role = role
            .parse::<RoleKind>()
            .map_err(|error: String| anyhow!(error))?;
        let handoff = serde_json::from_str::<serde_json::Value>(&handoff)?;
        crate::workflow_resources::render(
            role,
            &serde_json::json!({"attempt_id":attempt,"switch_intent_id":id,"structured_handoff":handoff}),
        )
    })() {
        Ok(prompt) => prompt,
        Err(error) => {
            return disposition_switch_failure(
                app,
                &id,
                &attempt,
                &role,
                "ready_for_dispatch",
                &format!("replacement preparation failed: {error:#}"),
            );
        }
    };
    let launch = match app.dispatch_switch(&id, &prompt) {
        Ok(launch) => launch,
        Err(error) => {
            return disposition_switch_failure(
                app,
                &id,
                &attempt,
                &role,
                "ready_for_dispatch",
                &format!("replacement dispatch failed: {error:#}"),
            );
        }
    };
    Ok(Some(
        serde_json::json!({"action":"switch_dispatched","intent_id":id,"session_id":launch.session_id}),
    ))
}

fn unresolved_workspace_for_claim(
    transaction: &rusqlite::Transaction<'_>,
    attempt_id: &str,
) -> Result<Option<String>> {
    let mut statement = transaction.prepare(
        "SELECT w.id FROM workspaces w JOIN claims c ON c.attempt_id=w.attempt_id
         WHERE w.attempt_id=?1 AND c.state IN ('unknown','stopping')
           AND w.state IN ('reserved','unknown','recovery_required')
         ORDER BY w.created_at",
    )?;
    let workspaces = statement
        .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((workspaces.len() == 1)
        .then(|| workspaces.into_iter().next())
        .flatten())
}

fn disposition_selected_control_failure(
    app: &Application,
    error: &anyhow::Error,
) -> Result<Option<serde_json::Value>> {
    let reason = format!("{error:#}");
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let control: Option<(String, String)> = transaction
        .query_row(
            "SELECT c.id,c.attempt_id FROM controls c
             WHERE c.state IN ('requested','draining') AND c.kind!='transition_proposal'
             ORDER BY c.created_at LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((control_id, attempt_id)) = control else {
        transaction.commit()?;
        return Err(anyhow!(reason));
    };
    let uncertain = ["ownership", "process", "quiescen", "uncertain", "worktree"]
        .iter()
        .any(|needle| reason.to_ascii_lowercase().contains(needle));
    let workspace_id = uncertain
        .then(|| unresolved_workspace_for_claim(&transaction, &attempt_id))
        .transpose()?
        .flatten();
    let now = Utc::now().to_rfc3339();
    let recovery_required = workspace_id.is_some();
    transaction.execute(
        "UPDATE controls SET state=?1,
                payload_json=json_set(payload_json,'$.failure',?2,'$.next_action',?3),updated_at=?4
         WHERE id=?5 AND state IN ('requested','draining')",
        params![
            if recovery_required { "recovery_required" } else { "rejected" },
            reason,
            if recovery_required {
                "Use the exact workspace reservation recovery action; generic control recovery cannot dispose repository ownership."
            } else {
                "Submit a corrected, newly authorized control."
            },
            now,
            control_id
        ],
    )?;
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','control.dispositioned_after_failure','control',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), control_id,
            serde_json::json!({
                "attempt_id":attempt_id,
                "reason":reason,
                "recovery_required":recovery_required,
                "workspace_id":workspace_id,
            }).to_string(), now
        ],
    )?;
    transaction.commit()?;
    drop(connection);
    if let Some(workspace_id) = workspace_id {
        crate::scheduler::record_workspace_reservation_recovery(
            &app.store,
            &attempt_id,
            &workspace_id,
            "selected control encountered uncertain repository ownership",
            serde_json::json!({
                "stage":"control_disposition",
                "control_id":control_id,
                "reason":reason,
            }),
        )?;
    }
    Ok(Some(serde_json::json!({
        "action":if recovery_required {"control_workspace_recovery_required"} else {"control_rejected"},
        "control_id":control_id,"attempt_id":attempt_id,"reason":reason,
    })))
}

fn disposition_switch_failure(
    app: &Application,
    intent_id: &str,
    attempt_id: &str,
    role: &str,
    stage: &str,
    reason: &str,
) -> Result<Option<serde_json::Value>> {
    let now = Utc::now().to_rfc3339();
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = transaction.execute(
        "UPDATE switch_intents SET state='rejected',updated_at=?1
         WHERE id=?2 AND state IN ('stopping_old','ready_for_dispatch')",
        params![now, intent_id],
    )?;
    if changed != 1 {
        transaction.commit()?;
        return Ok(None);
    }
    transaction.execute(
        "UPDATE controls SET state='rejected',
                payload_json=json_set(payload_json,'$.failure',?1,'$.switch_recovery_state','rejected','$.next_action',?2),updated_at=?3
         WHERE attempt_id=?4 AND kind='manager_change' AND state='switching'
           AND json_extract(payload_json,'$.switch_intent_id')=?5",
        params![reason, "Review this rejected switch and submit a corrected, versioned replacement.", now, attempt_id, intent_id],
    )?;
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','switch.dispositioned_after_failure','switch_intent',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            intent_id,
            serde_json::json!({
                "attempt_id":attempt_id,
                "role":role,
                "stage":stage,
                "reason":reason,
                "recovery_required":false,
            }).to_string(),
            now
        ],
    )?;
    transaction.commit()?;
    Ok(Some(serde_json::json!({
        "action":"switch_rejected","intent_id":intent_id,"attempt_id":attempt_id,
        "role":role,"reason":reason,
    })))
}

fn process_one_control(app: &Application) -> Result<Option<serde_json::Value>> {
    let control = {
        let connection = app.store.lock()?;
        connection.query_row("SELECT c.id,c.attempt_id,c.kind,c.payload_json,a.task_id FROM controls c JOIN attempts a ON a.id=c.attempt_id WHERE c.state IN ('requested','draining') AND c.kind!='transition_proposal' ORDER BY c.created_at LIMIT 1",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?))).optional()?
    };
    let Some((id, attempt, kind, payload, task)) = control else {
        return Ok(None);
    };
    let rework_before_lock = unfinished_rework(app, &attempt)?;
    let _rework_guard = rework_before_lock
        .as_ref()
        .map(|_| app.rework_control_guard())
        .transpose()?;
    let rework = unfinished_rework(app, &attempt)?;
    let mut controlled_attempts = vec![attempt.as_str()];
    if let Some((parent, _)) = &rework {
        controlled_attempts.push(parent.as_str());
    }
    let active = active_sessions(app, &controlled_attempts, kind != "cancel")?;
    match kind.as_str() {
        "continue" => {
            let outcome = finish_continue_with_restart_hold(app, &id, &task, &attempt);
            match outcome.as_ref() {
                Ok(Some(ContinueRefusal::Ownership(disposition))) => {
                    return Ok(Some(disposition.clone()));
                }
                Ok(Some(ContinueRefusal::Stale(reason))) => {
                    return Ok(Some(serde_json::json!({
                        "action":"continue_rejected","control_id":id,"attempt_id":attempt,"reason":reason
                    })));
                }
                _ => {}
            }
            if let Err(error) = outcome {
                let reason = format!("{error:#}");
                if !reason.starts_with("restart hold release") {
                    return Err(error);
                }
                let connection = app.store.lock()?;
                connection.execute(
                    "UPDATE controls SET state='rejected',payload_json=?1,updated_at=?2 WHERE id=?3",
                    params![
                        serde_json::json!({"reason":reason.clone(),"restart_hold_retained":true}).to_string(),
                        Utc::now().to_rfc3339(),
                        id
                    ],
                )?;
                connection.execute(
                    "UPDATE attempts SET status='restart_parked',updated_at=?1 WHERE id=?2",
                    params![Utc::now().to_rfc3339(), attempt],
                )?;
                connection.execute(
                    "UPDATE tasks SET attention='restart_parked',updated_at=?1 WHERE id=?2",
                    params![Utc::now().to_rfc3339(), task],
                )?;
                return Ok(Some(serde_json::json!({
                    "action":"continue_rejected",
                    "control_id":id,
                    "attempt_id":attempt,
                    "reason":reason,
                    "restart_hold_retained":true
                })));
            }
            Ok(Some(
                serde_json::json!({"action":"continued","control_id":id,"attempt_id":attempt}),
            ))
        }
        "run_next" => {
            finish_control(app, &id, &task, &attempt, "none", "running", Some(1), None)?;
            Ok(Some(
                serde_json::json!({"action":"one_step_enabled","control_id":id,"attempt_id":attempt}),
            ))
        }
        "retry" => {
            let connection = app.store.lock()?;
            connection.execute(
                "UPDATE check_runs SET status='retry_superseded',finished_at=COALESCE(finished_at,?1)
                 WHERE id=(SELECT cr.id FROM check_runs cr JOIN attempts a ON a.id=cr.attempt_id
                   WHERE cr.attempt_id=?2 AND cr.candidate_hash=a.candidate_hash
                     AND (cr.status IN ('launch_ambiguous','launch_failed','precondition_failed')
                       OR cr.launch_state='capture_failed' OR (cr.status='finished' AND COALESCE(cr.exit_code,-1)!=0))
                   ORDER BY cr.created_at DESC LIMIT 1)",
                params![Utc::now().to_rfc3339(),attempt],
            )?;
            drop(connection);
            finish_control(app, &id, &task, &attempt, "none", "running", None, None)?;
            Ok(Some(
                serde_json::json!({"action":"retry_enabled","control_id":id,"attempt_id":attempt}),
            ))
        }
        "pause_after_role" if active.iter().any(|(_, role)| role != "manager") => Ok(Some(
            serde_json::json!({"action":"draining_control","control_id":id,"attempt_id":attempt,"active":active}),
        )),
        "pause_after_role" => {
            finish_control(app, &id, &task, &attempt, "paused", "held", None, None)?;
            Ok(Some(
                serde_json::json!({"action":"paused","control_id":id,"attempt_id":attempt}),
            ))
        }
        "pause_now" | "cancel" => {
            if !active.is_empty() {
                for (session, _) in &active {
                    app.supervisor.interrupt(session)?
                }
                let connection = app.store.lock()?;
                connection.execute(
                    "UPDATE controls SET state='draining',updated_at=?1 WHERE id=?2",
                    params![Utc::now().to_rfc3339(), id],
                )?;
                return Ok(Some(
                    serde_json::json!({"action":"interrupting_for_control","control_id":id,"attempt_id":attempt,"active":active}),
                ));
            }
            if kind == "cancel" && rework.is_none() {
                let connection = app.store.lock()?;
                let (process_recovery, workspace_recovery): (bool, bool) = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1
                       AND state='attention_required'
                       AND (session_id IS NOT NULL OR json_extract(detail_json,'$.check_id') IS NOT NULL)),
                            EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1
                       AND state='attention_required'
                       AND json_extract(detail_json,'$.kind')='workspace_reservation')",
                    params![attempt],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                if process_recovery || workspace_recovery {
                    connection.execute(
                        "UPDATE controls SET state='recovery_required',
                                payload_json=json_set(payload_json,'$.next_action',?1),updated_at=?2
                         WHERE id=?3 AND state IN ('requested','draining')",
                        params![
                            if workspace_recovery {
                                "Use cancel_workspace_reservation; generic cancel never disposes uncertain worktree ownership."
                            } else {
                                "Resolve exact process recovery before retrying or cancelling this control."
                            },
                            Utc::now().to_rfc3339(),
                            id
                        ],
                    )?;
                    return Ok(Some(serde_json::json!({
                        "action":"recovery_required_for_cancel",
                        "control_id":id,
                        "attempt_id":attempt,
                        "public_resolution":if workspace_recovery {"cancel_workspace_reservation"} else {"resolve_recovery_cancel"}
                    })));
                }
                let unresolved_workspace: Option<String> = connection.query_row(
                    "SELECT CASE WHEN COUNT(*)=1 THEN MIN(w.id) END
                         FROM workspaces w JOIN claims c ON c.attempt_id=w.attempt_id
                         WHERE w.attempt_id=?1 AND c.state IN ('unknown','stopping')
                           AND w.state IN ('reserved','unknown','recovery_required')",
                    params![attempt],
                    |row| row.get(0),
                )?;
                if let Some(workspace_id) = unresolved_workspace {
                    connection.execute(
                        "UPDATE controls SET state='recovery_required',
                                payload_json=json_set(payload_json,'$.next_action',?1),updated_at=?2
                         WHERE id=?3 AND state IN ('requested','draining')",
                        params![
                            "Use cancel_workspace_reservation; generic cancel never disposes uncertain worktree ownership.",
                            Utc::now().to_rfc3339(),
                            id
                        ],
                    )?;
                    drop(connection);
                    crate::scheduler::record_workspace_reservation_recovery(
                        &app.store,
                        &attempt,
                        &workspace_id,
                        "cancellation found uncertain repository ownership before any cancellation side effect",
                        serde_json::json!({
                            "stage":"cancel_control",
                            "control_id":id,
                            "reason":"claim ownership is unknown or stopping",
                        }),
                    )?;
                    return Ok(Some(serde_json::json!({
                        "action":"control_workspace_recovery_required",
                        "control_id":id,
                        "attempt_id":attempt,
                        "workspace_id":workspace_id,
                        "public_resolution":"cancel_workspace_reservation",
                    })));
                }
            }
            let cancel_ownership = if kind == "cancel" {
                rework
                    .as_ref()
                    .map(|(parent, _)| {
                        let connection = app.store.lock()?;
                        rework_cancel_ownership(&connection, &attempt, parent)
                    })
                    .transpose()?
            } else {
                None
            };
            if cancel_ownership.as_ref().is_some_and(|ownership| {
                !ownership.ordinary_claim_only
                    && (ownership.recovery_records == 0
                        || ownership.process_recovery_records == 0
                        || ownership.uncovered_uncertain_claims != 0
                        || ownership.unrecorded_process_ownership != 0)
            }) {
                bail!("rework cancellation found uncertain ownership without an exact unresolved recovery record")
            }
            let parent_claim_quiescent = cancel_ownership
                .as_ref()
                .map(|ownership| ownership.ordinary_claim_only);
            if kind == "cancel" {
                finish_control(
                    app,
                    &id,
                    &task,
                    &attempt,
                    "none",
                    "cancelled",
                    None,
                    cancel_ownership.as_ref(),
                )?;
            } else {
                finish_control(app, &id, &task, &attempt, "paused", "held", None, None)?
            }
            Ok(Some(serde_json::json!({
                "action":kind,
                "control_id":id,
                "attempt_id":attempt,
                "quiescent":parent_claim_quiescent.unwrap_or(true),
                "repository_claim_retained":parent_claim_quiescent == Some(false)
            })))
        }
        "checkpoint_switch" => {
            let connection = app.store.lock()?;
            connection.execute("UPDATE controls SET state='rejected',payload_json=?1,updated_at=?2 WHERE id=?3",params![serde_json::json!({"request":serde_json::from_str::<serde_json::Value>(&payload)?,"reason":"use typed role_switch_request with immutable fence"}).to_string(),Utc::now().to_rfc3339(),id])?;
            Ok(Some(
                serde_json::json!({"action":"control_rejected","control_id":id,"attempt_id":attempt,"reason":"typed_switch_required"}),
            ))
        }
        _ => bail!("unknown control kind {kind}"),
    }
}

fn unfinished_rework(app: &Application, attempt: &str) -> Result<Option<(String, String)>> {
    let connection = app.store.lock()?;
    Ok(connection
        .query_row(
            "SELECT parent_attempt_id,state FROM rework_intents
             WHERE new_attempt_id=?1 AND state NOT IN ('completed','cancelled')",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

#[derive(Debug)]
struct ReworkCancelOwnership {
    complete_snapshot: Vec<String>,
    expected_parent_running_claims: i64,
    other_active_claims: i64,
    active_sessions: i64,
    active_checks: i64,
    recovery_records: i64,
    process_recovery_records: i64,
    uncovered_uncertain_claims: i64,
    unrecorded_process_ownership: i64,
    ordinary_claim_only: bool,
}

impl ReworkCancelOwnership {
    fn same_snapshot(&self, other: &Self) -> bool {
        self.complete_snapshot == other.complete_snapshot
            && self.expected_parent_running_claims == other.expected_parent_running_claims
            && self.other_active_claims == other.other_active_claims
            && self.active_sessions == other.active_sessions
            && self.active_checks == other.active_checks
            && self.recovery_records == other.recovery_records
            && self.process_recovery_records == other.process_recovery_records
            && self.uncovered_uncertain_claims == other.uncovered_uncertain_claims
            && self.unrecorded_process_ownership == other.unrecorded_process_ownership
            && self.ordinary_claim_only == other.ordinary_claim_only
    }
}

fn rework_cancel_ownership(
    connection: &rusqlite::Connection,
    child: &str,
    parent: &str,
) -> Result<ReworkCancelOwnership> {
    let mut statement = connection.prepare(
        "SELECT ownership_key FROM (
           SELECT 'claim:' || c.id || ':' || c.attempt_id || ':' || c.state AS ownership_key
             FROM claims c WHERE c.attempt_id IN (?1,?2)
               AND c.state IN ('reserved','launching','running','unknown','stopping')
           UNION ALL
           SELECT 'session:' || s.id || ':' || rg.attempt_id || ':' || s.status || ':' ||
                  COALESCE(s.process_identity_json,'') || ':' || COALESCE(s.recovery_root_pid,'') || ':' ||
                  COALESCE(s.recovery_process_group_id,'') || ':' || COALESCE(s.recovery_anchor_json,'') || ':' ||
                  COALESCE(s.launch_boot_identity,'')
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id IN (?1,?2)
               AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
           UNION ALL
           SELECT 'session-process:' || sp.session_id || ':' || sp.pid || ':' ||
                  sp.native_start_marker || ':' || sp.process_group_id || ':' || COALESCE(sp.parent_pid,'')
             FROM session_processes sp JOIN sessions s ON s.id=sp.session_id
             JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id IN (?1,?2)
               AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
           UNION ALL
           SELECT 'check:' || cr.id || ':' || cr.attempt_id || ':' || cr.status || ':' ||
                  COALESCE(cr.recovery_root_pid,'') || ':' || COALESCE(cr.recovery_process_group_id,'') || ':' ||
                  COALESCE(cr.recovery_anchor_json,'') || ':' || COALESCE(cr.launch_boot_identity,'')
             FROM check_runs cr WHERE cr.attempt_id IN (?1,?2)
               AND cr.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')
           UNION ALL
           SELECT 'check-process:' || cp.check_id || ':' || cp.pid || ':' ||
                  cp.native_start_marker || ':' || cp.process_group_id || ':' || COALESCE(cp.parent_pid,'')
             FROM check_processes cp JOIN check_runs cr ON cr.id=cp.check_id
             WHERE cr.attempt_id IN (?1,?2)
               AND cr.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')
           UNION ALL
           SELECT 'recovery:' || r.id || ':' || r.attempt_id || ':' ||
                  COALESCE(r.session_id,'') || ':' || COALESCE(r.process_identity_json,'') || ':' || r.detail_json
             FROM recovery_records r WHERE r.attempt_id IN (?1,?2)
               AND r.state='attention_required') ORDER BY ownership_key",
    )?;
    let complete_snapshot = statement
        .query_map(params![child, parent], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (
        expected_parent_running_claims,
        other_active_claims,
        active_sessions,
        active_checks,
        recovery_records,
        process_recovery_records,
        uncovered_uncertain_claims,
        unrecorded_process_ownership,
    ): (i64, i64, i64, i64, i64, i64, i64, i64) = connection.query_row(
        "SELECT
           (SELECT COUNT(*) FROM claims WHERE attempt_id=?2 AND state='running'),
           (SELECT COUNT(*) FROM claims WHERE attempt_id IN (?1,?2)
             AND state IN ('reserved','launching','running','unknown','stopping')
             AND NOT (attempt_id=?2 AND state='running')),
           (SELECT COUNT(*) FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id IN (?1,?2)
               AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')),
           (SELECT COUNT(*) FROM check_runs WHERE attempt_id IN (?1,?2)
             AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous')),
           (SELECT COUNT(*) FROM recovery_records WHERE attempt_id IN (?1,?2)
             AND state='attention_required'),
           (SELECT COUNT(*) FROM recovery_records WHERE attempt_id IN (?1,?2)
             AND state='attention_required'
             AND (session_id IS NOT NULL OR json_extract(detail_json,'$.check_id') IS NOT NULL)),
           (SELECT COUNT(*) FROM claims c WHERE c.attempt_id IN (?1,?2)
             AND c.state IN ('reserved','launching','running','unknown','stopping')
             AND NOT (c.attempt_id=?2 AND c.state='running')
             AND NOT EXISTS(SELECT 1 FROM recovery_records r
               WHERE r.attempt_id=c.attempt_id AND r.state='attention_required'
                 AND (r.session_id IS NOT NULL OR json_extract(r.detail_json,'$.check_id') IS NOT NULL))),
           (SELECT COUNT(*) FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id IN (?1,?2)
               AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
               AND (s.status!='recovery_required' OR NOT EXISTS(
                 SELECT 1 FROM recovery_records r WHERE r.session_id=s.id AND r.attempt_id=rg.attempt_id
                   AND r.state='attention_required')))
           + (SELECT COUNT(*) FROM check_runs cr WHERE cr.attempt_id IN (?1,?2)
               AND cr.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')
               AND (cr.status!='recovery_required' OR NOT EXISTS(
                 SELECT 1 FROM recovery_records r WHERE r.session_id IS NULL
                   AND r.attempt_id=cr.attempt_id AND r.state='attention_required'
                   AND json_extract(r.detail_json,'$.check_id')=cr.id)))",
        params![child, parent],
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
    )?;
    let ordinary_claim_only = expected_parent_running_claims == 1
        && other_active_claims == 0
        && active_sessions == 0
        && active_checks == 0
        && recovery_records == 0;
    Ok(ReworkCancelOwnership {
        complete_snapshot,
        expected_parent_running_claims,
        other_active_claims,
        active_sessions,
        active_checks,
        recovery_records,
        process_recovery_records,
        uncovered_uncertain_claims,
        unrecorded_process_ownership,
        ordinary_claim_only,
    })
}

fn active_sessions(
    app: &Application,
    attempts: &[&str],
    include_recovery: bool,
) -> Result<Vec<(String, String)>> {
    let connection = app.store.lock()?;
    let parent = attempts.get(1).copied();
    let mut statement=connection.prepare("SELECT s.id,rg.role FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE (rg.attempt_id=?1 OR rg.attempt_id=?2) AND (s.status IN ('launch_reserved','running','interrupt_requested') OR (?3 AND s.status='recovery_required'))")?;
    let rows = statement
        .query_map(params![attempts[0], parent, include_recovery], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn manager_interrupt_pending_on(connection: &Connection, attempt: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT s.id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.task_id=a.task_id
               AND rs.role='manager' AND rs.effective_generation_id=rg.id
             WHERE rg.attempt_id=?1 AND rg.role='manager' AND rg.status='running'
               AND s.status='interrupt_requested'
             ORDER BY rg.generation DESC,s.created_at DESC LIMIT 1",
            params![attempt],
            |row| row.get(0),
        )
        .optional()?)
}

enum ManagerCompletionBoundary {
    Ready,
    RequestStop {
        session_id: String,
        generation_status: String,
        session_status: String,
        readiness: String,
        quiescent: bool,
    },
    Waiting {
        reason: &'static str,
        session_id: Option<String>,
        generation_status: Option<String>,
        session_status: Option<String>,
        readiness: Option<String>,
        quiescent: Option<bool>,
    },
}

fn manager_completion_boundary(
    connection: &Connection,
    attempt: &Attempt,
) -> Result<ManagerCompletionBoundary> {
    let current: Option<(String, String, String, String, bool)> = connection
        .query_row(
            "SELECT s.id,rg.status,s.status,s.readiness_state,
                    COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
             FROM attempts a JOIN tasks t ON t.id=a.task_id
             JOIN role_settings rs ON rs.task_id=t.id AND rs.role='manager'
             JOIN role_generations rg ON rg.id=rs.effective_generation_id
               AND rg.attempt_id=a.id AND rg.role='manager'
             JOIN sessions s ON s.role_generation_id=rg.id
             WHERE a.id=?1 AND a.phase=?2 AND a.status='running'
               AND t.attention='none' AND t.archived_at IS NULL
               AND s.id=(SELECT latest.id FROM sessions latest
                 WHERE latest.role_generation_id=rg.id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)",
            params![attempt.id, attempt.phase],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((session_id, generation_status, session_status, readiness, quiescent)) = current
    else {
        return Ok(ManagerCompletionBoundary::Waiting {
            reason: "current_manager_completion_boundary",
            session_id: None,
            generation_status: None,
            session_status: None,
            readiness: None,
            quiescent: None,
        });
    };
    if generation_status == "running"
        && session_status == "running"
        && readiness == "idle_candidate"
    {
        let credential_current: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_credentials credential
               ON credential.role_generation_id=s.role_generation_id
               WHERE s.id=?1 AND credential.revoked_at IS NULL)",
            params![session_id],
            |row| row.get(0),
        )?;
        if credential_current {
            return Ok(ManagerCompletionBoundary::Ready);
        }
    }
    if generation_status == "exited" && session_status == "exited" && quiescent {
        return Ok(ManagerCompletionBoundary::Ready);
    }
    if session_status == "interrupt_requested" {
        return Ok(ManagerCompletionBoundary::Waiting {
            reason: "manager_service_stop_quiescence",
            session_id: Some(session_id),
            generation_status: None,
            session_status: None,
            readiness: None,
            quiescent: None,
        });
    }
    if generation_status == "running"
        && session_status == "running"
        && readiness == "busy_unresolved_hook_work"
        && crate::store::eligible_manager_service_stop(connection, &attempt.id, &attempt.phase)?
            .is_some()
    {
        return Ok(ManagerCompletionBoundary::RequestStop {
            session_id,
            generation_status,
            session_status,
            readiness,
            quiescent,
        });
    }
    Ok(ManagerCompletionBoundary::Waiting {
        reason: if session_status == "exited" && !quiescent {
            "manager_quiescence_or_recovery"
        } else {
            "manager_safe_completion_boundary"
        },
        session_id: Some(session_id),
        generation_status: Some(generation_status),
        session_status: Some(session_status),
        readiness: Some(readiness),
        quiescent: Some(quiescent),
    })
}

fn advance_manager_boundary(
    app: &Application,
    attempt: &Attempt,
    obligation: &str,
    boundary: &ManagerCompletionBoundary,
) -> Result<Option<serde_json::Value>> {
    match boundary {
        ManagerCompletionBoundary::Ready => Ok(None),
        ManagerCompletionBoundary::RequestStop {
            session_id,
            generation_status,
            session_status,
            readiness,
            quiescent,
        } => match quiesce_manager_for_obligation(app, attempt, obligation)? {
            Some(value) => Ok(Some(value)),
            None => Ok(Some(serde_json::json!({
                "action":"waiting",
                "for":"manager_safe_completion_boundary",
                "attempt_id":attempt.id,
                "session_id":session_id,
                "obligation":obligation,
                "manager_generation_status":generation_status,
                "manager_session_status":session_status,
                "manager_readiness":readiness,
                "process_group_quiescent":quiescent,
            }))),
        },
        ManagerCompletionBoundary::Waiting {
            reason,
            session_id,
            generation_status,
            session_status,
            readiness,
            quiescent,
        } => {
            let mut value = serde_json::json!({
                "action":"waiting",
                "for":reason,
                "attempt_id":attempt.id,
                "obligation":obligation,
            });
            let object = value
                .as_object_mut()
                .ok_or_else(|| anyhow!("manager boundary outcome must be an object"))?;
            for (key, value) in [
                ("session_id", session_id.as_ref()),
                ("manager_generation_status", generation_status.as_ref()),
                ("manager_session_status", session_status.as_ref()),
                ("manager_readiness", readiness.as_ref()),
            ] {
                if let Some(value) = value {
                    object.insert(key.into(), serde_json::Value::String(value.clone()));
                }
            }
            if let Some(quiescent) = quiescent {
                object.insert("process_group_quiescent".into(), (*quiescent).into());
            }
            Ok(Some(value))
        }
    }
}
fn consume_completed_checks(app: &Application, attempt: &Attempt, candidate: &str) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (eligible, final_repair_round): (bool, i64) = transaction.query_row(
        "SELECT
           a.phase='checks' AND a.status='running' AND a.candidate_hash=?2
           AND t.attention='none' AND t.archived_at IS NULL
           AND (SELECT COUNT(*) FROM trip_selected_checks selected
                WHERE selected.attempt_id=a.id AND selected.revision=a.selected_checks_revision
                  AND selected.required=1)>0
           AND NOT EXISTS(SELECT 1 FROM trip_selected_checks selected
             WHERE selected.attempt_id=a.id AND selected.revision=a.selected_checks_revision
               AND selected.required=1
               AND NOT EXISTS(SELECT 1 FROM check_runs run
                 WHERE run.attempt_id=a.id AND run.candidate_hash=a.candidate_hash
                   AND run.check_id=selected.check_id
                   AND run.selected_check_revision=selected.revision
                   AND run.status='finished' AND run.exit_code=0
                   AND run.freshness_state='current'))
           AND EXISTS(SELECT 1 FROM trip_conformance_receipts conformance
             WHERE conformance.attempt_id=a.id
               AND conformance.revision=a.manager_conformance_revision
               AND conformance.candidate_hash=a.candidate_hash)
           AND EXISTS(SELECT 1 FROM trip_explorer_decisions explorer
             WHERE explorer.attempt_id=a.id AND explorer.stage='final'
               AND explorer.candidate_hash=a.candidate_hash
               AND (explorer.activated=0 OR explorer.outcome_json IS NOT NULL))
           AND NOT EXISTS(SELECT 1 FROM implementation_lanes lane
             WHERE lane.attempt_id=a.id AND lane.required=1 AND lane.state!='yielded')
           AND NOT EXISTS(SELECT 1 FROM role_generations writer
             WHERE writer.attempt_id=a.id AND writer.role='implementer'
               AND writer.status IN ('launch_reserved','running','stopping'))
           AND EXISTS(SELECT 1 FROM role_settings rs
             JOIN role_generations manager ON manager.id=rs.effective_generation_id
             JOIN sessions session ON session.role_generation_id=manager.id
             WHERE rs.task_id=t.id AND rs.role='manager' AND manager.attempt_id=a.id
               AND manager.role='manager'
               AND session.id=(SELECT latest.id FROM sessions latest
                 WHERE latest.role_generation_id=manager.id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
               AND ((manager.status='running' AND session.status='running'
                       AND session.readiness_state='idle_candidate'
                       AND EXISTS(SELECT 1 FROM role_credentials credential
                         WHERE credential.role_generation_id=manager.id
                           AND credential.revoked_at IS NULL))
                 OR (manager.status='exited' AND session.status='exited'
                       AND COALESCE(json_extract(session.exit_json,'$.process_group_quiescent'),0)=1))),
           a.final_repair_round
         FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
        params![attempt.id, candidate],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if !eligible {
        return Ok(false);
    }
    if final_repair_round == 1 {
        let retained_repair_evidence: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_requests r JOIN attempts a ON a.id=r.attempt_id
               WHERE r.attempt_id=?1 AND r.review_kind='code' AND r.candidate_hash=a.candidate_hash
                 AND r.verdict='approved' AND r.delivery_state='finished')
             AND EXISTS(SELECT 1 FROM attempts a JOIN trip_conformance_receipts c ON c.attempt_id=a.id
               AND c.revision=a.manager_conformance_revision AND c.candidate_hash=a.candidate_hash
               WHERE a.id=?1)",
            params![attempt.id],
            |row| row.get(0),
        )?;
        if !retained_repair_evidence {
            bail!("second final verifier requires retained code approval and refreshed manager conformance for the repaired candidate")
        }
        let changed = transaction.execute(
            "UPDATE review_budgets SET extension_allowance=1,version=version+1
             WHERE attempt_id=?1 AND review_kind='final' AND initial_allowance=1
               AND extension_allowance=0 AND spent=1",
            params![attempt.id],
        )?;
        if changed != 1 {
            let already: bool = transaction.query_row(
                "SELECT extension_allowance=1 AND spent=1 FROM review_budgets
                 WHERE attempt_id=?1 AND review_kind='final'",
                params![attempt.id],
                |row| row.get(0),
            )?;
            if !already {
                bail!("conditional final-review allowance is not in the required one-used plus one-repair state")
            }
        }
    } else if final_repair_round > 1 {
        bail!("final repair round exceeds the product v0.9.0 contract")
    }
    if transaction.execute(
        "UPDATE attempts SET phase='final_review',updated_at=?1
         WHERE id=?2 AND phase='checks' AND status='running' AND candidate_hash=?3",
        params![now, attempt.id, candidate],
    )? != 1
    {
        return Ok(false);
    }
    if transaction.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1
         WHERE id=?2 AND attention='none' AND archived_at IS NULL",
        params![now, attempt.task_id],
    )? != 1
    {
        return Ok(false);
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','attempt.phase.changed','attempt',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            attempt.id,
            serde_json::json!({"phase":"final_review"}).to_string(),
            now
        ],
    )?;
    transaction.commit()?;
    Ok(true)
}

fn consume_completed_handoff(
    app: &Application,
    attempt: &Attempt,
    snapshot_id: &str,
) -> Result<bool> {
    let Some(candidate) = attempt.candidate_hash.as_deref() else {
        return Ok(false);
    };
    let now = Utc::now().to_rfc3339();
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let eligible: bool = transaction.query_row(
        "SELECT a.phase='manager_handoff' AND a.status='running' AND a.candidate_hash=?2
           AND a.accepted_snapshot_id=?3 AND snapshot.kind='accepted'
           AND snapshot.complete=1 AND snapshot.manifest_hash=a.candidate_hash
           AND t.attention='none' AND t.archived_at IS NULL
           AND EXISTS(SELECT 1 FROM role_results result
             JOIN role_generations manager ON manager.id=result.role_generation_id
             JOIN role_settings rs ON rs.effective_generation_id=manager.id AND rs.role='manager'
             JOIN review_requests review
               ON review.id=json_extract(result.metadata_json,'$.final_review_request_id')
             WHERE manager.attempt_id=a.id AND manager.role='manager'
               AND result.outcome='handoff_ready' AND result.consumed_at IS NULL
               AND json_extract(result.metadata_json,'$.candidate_hash')=a.candidate_hash
               AND review.attempt_id=a.id AND review.review_kind='final'
               AND review.candidate_hash=a.candidate_hash AND review.verdict='approved'
               AND review.delivery_state='finished')
           AND EXISTS(SELECT 1 FROM trip_conformance_receipts conformance
             WHERE conformance.attempt_id=a.id
               AND conformance.revision=a.manager_conformance_revision
               AND conformance.candidate_hash=a.candidate_hash)
           AND NOT EXISTS(SELECT 1 FROM implementation_lanes lane
             WHERE lane.attempt_id=a.id AND lane.required=1 AND lane.state!='yielded')
           AND NOT EXISTS(SELECT 1 FROM role_generations writer
             WHERE writer.attempt_id=a.id AND writer.role='implementer'
               AND writer.status IN ('launch_reserved','running','stopping'))
           AND EXISTS(SELECT 1 FROM role_settings current
             JOIN role_generations manager ON manager.id=current.effective_generation_id
             JOIN sessions session ON session.role_generation_id=manager.id
             WHERE current.task_id=t.id AND current.role='manager' AND manager.attempt_id=a.id
               AND manager.role='manager'
               AND session.id=(SELECT latest.id FROM sessions latest
                 WHERE latest.role_generation_id=manager.id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
               AND ((manager.status='running' AND session.status='running'
                       AND session.readiness_state='idle_candidate'
                       AND EXISTS(SELECT 1 FROM role_credentials credential
                         WHERE credential.role_generation_id=manager.id
                           AND credential.revoked_at IS NULL))
                 OR (manager.status='exited' AND session.status='exited'
                       AND COALESCE(json_extract(session.exit_json,'$.process_group_quiescent'),0)=1)))
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN snapshots snapshot ON snapshot.id=a.accepted_snapshot_id
         WHERE a.id=?1",
        params![attempt.id, candidate, snapshot_id],
        |row| row.get(0),
    )?;
    if !eligible {
        transaction.execute(
            "UPDATE attempts SET accepted_snapshot_id=NULL,updated_at=?1
             WHERE id=?2 AND phase='manager_handoff' AND accepted_snapshot_id=?3",
            params![now, attempt.id, snapshot_id],
        )?;
        transaction.execute(
            "UPDATE freeze_intents SET state='abandoned',
               error='completed handoff authority changed after accepted snapshot capture',updated_at=?1
             WHERE attempt_id=?2 AND kind='accepted' AND result_snapshot_id=?3 AND state='complete'",
            params![now, attempt.id, snapshot_id],
        )?;
        transaction.commit()?;
        return Ok(false);
    }
    if transaction.execute(
        "UPDATE attempts SET phase='awaiting_human_review',updated_at=?1
         WHERE id=?2 AND phase='manager_handoff' AND candidate_hash=?3
           AND accepted_snapshot_id=?4",
        params![now, attempt.id, candidate, snapshot_id],
    )? != 1
    {
        return Ok(false);
    }
    if transaction.execute(
        "UPDATE tasks SET lifecycle='awaiting_review',attention='needs_human_review',
           version=version+1,updated_at=?1
         WHERE id=?2 AND attention='none' AND archived_at IS NULL",
        params![now, attempt.task_id],
    )? != 1
    {
        return Ok(false);
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','attempt.phase.changed','attempt',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            attempt.id,
            serde_json::json!({"phase":"awaiting_human_review"}).to_string(),
            now
        ],
    )?;
    transaction.commit()?;
    Ok(true)
}

fn quiesce_manager_for_obligation(
    app: &Application,
    attempt: &Attempt,
    obligation: &str,
) -> Result<Option<serde_json::Value>> {
    let receipt = {
        let connection = app.store.lock()?;
        crate::store::eligible_manager_service_stop(&connection, &attempt.id, &attempt.phase)?
    };
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    let outcome = app.request_manager_service_stop(&attempt.id, &attempt.phase, &receipt)?;
    if outcome == crate::supervisor::InterruptOutcome::Stale {
        return Ok(None);
    }
    Ok(Some(serde_json::json!({
        "action":if outcome == crate::supervisor::InterruptOutcome::Requested {
            "manager_service_stop_requested"
        } else {
            "manager_service_stop_already_requested"
        },
        "attempt_id":attempt.id,
        "session_id":receipt.session_id,
        "role_generation_id":receipt.generation_id,
        "transcript_epoch":receipt.transcript_epoch,
        "stop_event_rowid":receipt.stop_rowid,
        "obligation":obligation,
        "state":"interrupt_requested",
        "signal_attempted":outcome == crate::supervisor::InterruptOutcome::Requested,
        "completion_inferred":false,
        "automatic_escalation":false
    })))
}

fn dispatch_or_resume_manager(
    app: &Application,
    attempt: &str,
    prompt: &str,
) -> Result<(crate::domain::ValidationLaunchResult, bool)> {
    let (resumable, current_manager) =
        {
            let connection = app.store.lock()?;
            let resumable = connection.query_row(
            "SELECT s.id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.task_id=a.task_id
               AND rs.role='manager' AND rs.effective_generation_id=rg.id
             WHERE rg.attempt_id=?1 AND rg.role='manager' AND rg.status='exited'
               AND s.status='exited' AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
               AND s.capability_identity_json IS NOT NULL
               AND EXISTS(SELECT 1 FROM capabilities current_capability
                 WHERE current_capability.rowid=(
                   SELECT latest_capability.rowid FROM capabilities latest_capability
                   WHERE latest_capability.provider=s.provider
                     AND latest_capability.executable_version=s.executable_version
                     AND latest_capability.role=rg.role
                     AND latest_capability.mode='interactive_pty'
                   ORDER BY latest_capability.checked_at DESC,
                            latest_capability.rowid DESC LIMIT 1)
                   AND current_capability.config_hash=s.capability_key
                   AND current_capability.status='supported'
                   AND current_capability.proof_json!='{}')
               AND json_extract(s.exit_json,'$.process_group_quiescent')=1
               AND NOT EXISTS(SELECT 1 FROM restart_candidates rc WHERE rc.session_id=s.id
                 AND rc.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
             ORDER BY rg.generation DESC,s.created_at DESC LIMIT 1",
            params![attempt],
            |row| row.get::<_, String>(0),
        ).optional()?;
            let current_manager = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_generations rg JOIN attempts a ON a.id=rg.attempt_id
             JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role='manager'
               AND rs.effective_generation_id=rg.id
             WHERE rg.attempt_id=?1 AND rg.role='manager')",
            params![attempt],
            |row| row.get(0),
        )?;
            (resumable, current_manager)
        };
    if let Some(session) = resumable {
        return Ok((app.resume_role_session(&session, "")?, true));
    }
    if current_manager {
        bail!("current manager native resume capability or quiescence is unavailable; explicit replacement is required")
    }
    Ok((
        app.dispatch_attempt_role(attempt, RoleKind::Manager, prompt)?,
        false,
    ))
}
fn has_result_on(
    connection: &Connection,
    attempt: &str,
    role: &str,
    outcome: &str,
) -> Result<bool> {
    if role == "manager" && outcome == "plan_ready" {
        return Ok(crate::store::eligible_manager_plan(connection, attempt, true)?.is_some());
    }
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role=rg.role WHERE rg.attempt_id=?1 AND rg.role=?2 AND rr.outcome=?3 AND rr.consumed_at IS NULL AND rr.created_at>=a.updated_at)",params![attempt,role,outcome],|row|row.get(0))?)
}
fn running_result_session_on(
    connection: &Connection,
    attempt: &str,
    role: &str,
) -> Result<Option<String>> {
    Ok(connection.query_row("SELECT s.id FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN sessions s ON s.id=rr.session_id JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role=rg.role WHERE rg.attempt_id=?1 AND rg.role=?2 AND s.status='running' AND rr.consumed_at IS NULL AND rr.created_at>=a.updated_at ORDER BY rr.created_at DESC LIMIT 1",params![attempt,role],|row|row.get(0)).optional()?)
}
fn exact_handoff_ready_on(connection: &Connection, attempt: &Attempt) -> Result<bool> {
    let Some(candidate) = attempt.candidate_hash.as_deref() else {
        return Ok(false);
    };
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id
           JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager'
           JOIN review_requests r ON r.id=json_extract(rr.metadata_json,'$.final_review_request_id')
           WHERE rg.attempt_id=?1 AND rg.role='manager' AND rr.outcome='handoff_ready' AND rr.consumed_at IS NULL
             AND json_extract(rr.metadata_json,'$.candidate_hash')=?2 AND r.attempt_id=?1 AND r.review_kind='final'
             AND r.candidate_hash=?2 AND r.verdict='approved' AND r.delivery_state='finished')
         AND EXISTS(SELECT 1 FROM attempts a JOIN trip_conformance_receipts c
           ON c.attempt_id=a.id AND c.revision=a.manager_conformance_revision AND c.candidate_hash=a.candidate_hash
           WHERE a.id=?1 AND a.candidate_hash=?2)
         AND NOT EXISTS(SELECT 1 FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state!='yielded')
         AND NOT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role='implementer'
           AND status IN ('launch_reserved','running','stopping'))",
        params![attempt.id,candidate],|row|row.get(0)
    )?)
}
fn latest_rework_feedback(app: &Application, attempt: &str) -> Result<Option<String>> {
    let connection = app.store.lock()?;
    Ok(connection
        .query_row(
            "SELECT feedback FROM rework_intents WHERE new_attempt_id=?1",
            params![attempt],
            |row| row.get(0),
        )
        .optional()?)
}

fn manager_notice_pending_on(
    connection: &Connection,
    attempt: &Attempt,
    body: &str,
    one_shot: bool,
) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_generations rg
           JOIN attempts a ON a.id=rg.attempt_id
           JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role='manager'
             AND rs.effective_generation_id=rg.id
           WHERE rg.attempt_id=?1 AND rg.role='manager' AND rg.status='running'
             AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
               WHERE guidance.attempt_id=?1 AND guidance.role_generation_id=rg.id
                 AND guidance.body=?2
                 AND (?3 OR guidance.state IN
                   ('queued','delivery_reserved','written_awaiting_submit','submitted'))))",
        params![attempt.id, body, one_shot],
        |row| row.get(0),
    )?)
}

fn queue_manager_notice(
    app: &Application,
    attempt: &Attempt,
    body: &str,
    one_shot: bool,
) -> Result<bool> {
    let connection = app.store.lock()?;
    let generation:Option<String>=connection.query_row("SELECT rg.id FROM role_generations rg JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role='manager' AND rs.effective_generation_id=rg.id WHERE rg.attempt_id=?1 AND rg.role='manager' AND rg.status='running' ORDER BY rg.generation DESC LIMIT 1",params![attempt.id],|row|row.get(0)).optional()?;
    if let Some(generation) = generation {
        let changed=connection.execute("INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,reason,created_at) SELECT ?1,?2,?3,?4,'queued','awaiting_supported_idle_boundary',?6 WHERE NOT EXISTS(SELECT 1 FROM guidance_messages WHERE attempt_id=?2 AND role_generation_id=?3 AND body=?4 AND (?5 OR state IN ('queued','delivery_reserved','written_awaiting_submit','submitted')))",params![uuid::Uuid::new_v4().to_string(),attempt.id,generation,body,one_shot,Utc::now().to_rfc3339()])?;
        return Ok(changed == 1);
    }
    Ok(false)
}
fn set_attention(app: &Application, task: &str, attempt: &str, attention: &str) -> Result<()> {
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "UPDATE tasks SET attention=?1,version=version+1,updated_at=?2 WHERE id=?3",
        params![attention, now, task],
    )?;
    transaction.execute(
        "UPDATE attempts SET status='needs_input',updated_at=?1 WHERE id=?2",
        params![now, attempt],
    )?;
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','attempt.attention.changed','attempt',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),attempt,serde_json::json!({"attention":attention}).to_string(),now])?;
    transaction.commit()?;
    Ok(())
}
fn set_phase(app: &Application, task: &str, attempt: &str, phase: &str) -> Result<()> {
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "UPDATE attempts SET phase=?1,updated_at=?2 WHERE id=?3",
        params![phase, now, attempt],
    )?;
    transaction.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2",
        params![now, task],
    )?;
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','attempt.phase.changed','attempt',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),attempt,serde_json::json!({"phase":phase}).to_string(),now])?;
    transaction.commit()?;
    Ok(())
}
fn finish_control(
    app: &Application,
    id: &str,
    task: &str,
    attempt: &str,
    attention: &str,
    status: &str,
    step_budget: Option<i64>,
    cancel_ownership: Option<&ReworkCancelOwnership>,
) -> Result<()> {
    let mut connection = app.store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    finish_control_in_transaction(
        &tx,
        id,
        task,
        attempt,
        attention,
        status,
        step_budget,
        cancel_ownership,
        &now,
    )?;
    tx.commit()?;
    Ok(())
}

enum ContinueRefusal {
    Ownership(serde_json::Value),
    Stale(&'static str),
}

fn disposition_continue_ownership_in(
    tx: &Transaction<'_>,
    id: &str,
    task: &str,
    attempt: &str,
    reason: &str,
) -> Result<serde_json::Value> {
    let workspace_id = unresolved_workspace_for_claim(tx, attempt)?;
    let recovery_required = workspace_id.is_some();
    let now = Utc::now().to_rfc3339();
    let changed = tx.execute(
        "UPDATE controls SET state=?1,
                payload_json=json_set(payload_json,'$.failure',?2,'$.next_action',?3),updated_at=?4
         WHERE id=?5 AND attempt_id=?6 AND kind='continue' AND state IN ('requested','draining')
           AND EXISTS(SELECT 1 FROM attempts WHERE id=?6 AND task_id=?7)",
        params![
            if recovery_required { "recovery_required" } else { "rejected" },
            reason,
            if recovery_required {
                "Use the exact workspace reservation recovery action; generic control recovery cannot dispose repository ownership."
            } else {
                "Submit a corrected, newly authorized control."
            },
            now, id, attempt, task,
        ],
    )?;
    if changed != 1 {
        bail!("selected Continue control changed before ownership disposition")
    }
    tx.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','control.dispositioned_after_failure','control',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), id,
            serde_json::json!({
                "attempt_id":attempt,
                "reason":reason,
                "recovery_required":recovery_required,
                "workspace_id":workspace_id,
            }).to_string(), now,
        ],
    )?;
    if let Some(workspace_id) = &workspace_id {
        crate::scheduler::record_workspace_reservation_recovery_in(
            tx,
            attempt,
            Some(task),
            workspace_id,
            "selected control encountered uncertain repository ownership",
            serde_json::json!({
                "stage":"control_disposition",
                "control_id":id,
                "reason":reason,
            }),
        )?;
    }
    Ok(serde_json::json!({
        "action":if recovery_required {"control_workspace_recovery_required"} else {"control_rejected"},
        "control_id":id,"attempt_id":attempt,"reason":reason,
    }))
}

#[test]
fn continue_ownership_disposes_only_captured_control_and_workspace() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(
        "CREATE TABLE projects(id TEXT PRIMARY KEY, repository_path TEXT);
         CREATE TABLE tasks(id TEXT PRIMARY KEY, project_id TEXT, attention TEXT, version INTEGER, updated_at TEXT);
         CREATE TABLE attempts(id TEXT PRIMARY KEY, task_id TEXT, status TEXT, updated_at TEXT);
         CREATE TABLE controls(id TEXT PRIMARY KEY, attempt_id TEXT, kind TEXT, state TEXT, payload_json TEXT, updated_at TEXT);
         CREATE TABLE claims(id TEXT PRIMARY KEY, attempt_id TEXT, state TEXT, updated_at TEXT);
         CREATE TABLE workspaces(id TEXT PRIMARY KEY, attempt_id TEXT, repository_identity TEXT, path TEXT, base_revision TEXT, state TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE recovery_records(id TEXT PRIMARY KEY, attempt_id TEXT, state TEXT, detail_json TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE audit_events(id TEXT, operation_id TEXT, actor_kind TEXT, event_code TEXT, entity_kind TEXT, entity_id TEXT, detail_json TEXT, created_at TEXT);
         INSERT INTO projects VALUES('project','/fixture/repository');
         INSERT INTO tasks VALUES('selected-task','project','none',4,'before');
         INSERT INTO tasks VALUES('other-task','project','paused',8,'before');
         INSERT INTO attempts VALUES('selected-attempt','selected-task','held','before');
         INSERT INTO attempts VALUES('other-attempt','other-task','held','before');
         INSERT INTO controls VALUES('other-control','other-attempt','continue','requested','{}','before');
         INSERT INTO controls VALUES('selected-control','selected-attempt','continue','requested','{}','before');
         INSERT INTO claims VALUES('selected-claim','selected-attempt','unknown','before');
         INSERT INTO claims VALUES('other-claim','other-attempt','unknown','before');
         INSERT INTO workspaces VALUES('selected-workspace','selected-attempt','selected-repo','/fixture/selected','base','reserved','before','before');
         INSERT INTO workspaces VALUES('other-workspace','other-attempt','other-repo','/fixture/other','base','reserved','before','before');",
    ).unwrap();
    const OTHER_TUPLE: &str = "SELECT json_array(
        c.id,c.attempt_id,c.kind,c.state,c.payload_json,c.updated_at,
        a.id,a.task_id,a.status,a.updated_at,
        t.id,t.project_id,t.attention,t.version,t.updated_at,
        cl.id,cl.attempt_id,cl.state,cl.updated_at,
        w.id,w.attempt_id,w.repository_identity,w.path,w.base_revision,w.state,w.created_at,w.updated_at)
        FROM controls c JOIN attempts a ON a.id=c.attempt_id JOIN tasks t ON t.id=a.task_id
        JOIN claims cl ON cl.attempt_id=a.id JOIN workspaces w ON w.attempt_id=a.id
        WHERE c.id='other-control'";
    let other_before: String = connection
        .query_row(OTHER_TUPLE, [], |row| row.get(0))
        .unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let outcome = disposition_continue_ownership_in(
        &tx,
        "selected-control",
        "selected-task",
        "selected-attempt",
        "ownership uncertain",
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(outcome["action"], "control_workspace_recovery_required");
    assert_eq!(outcome["control_id"], "selected-control");
    assert_eq!(outcome["attempt_id"], "selected-attempt");
    let selected: (String, String, String, String, String) = connection
        .query_row(
            "SELECT c.state,a.status,t.attention,cl.state,w.state
         FROM controls c JOIN attempts a ON a.id=c.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN claims cl ON cl.attempt_id=a.id JOIN workspaces w ON w.attempt_id=a.id
         WHERE c.id='selected-control'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        selected,
        (
            "recovery_required".into(),
            "needs_recovery".into(),
            "needs_recovery".into(),
            "unknown".into(),
            "recovery_required".into()
        )
    );
    let other_after: String = connection
        .query_row(OTHER_TUPLE, [], |row| row.get(0))
        .unwrap();
    assert_eq!(other_after, other_before);
    let other_recovery: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM recovery_records WHERE attempt_id='other-attempt'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other_recovery, 0);
    let selected_recovery: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM recovery_records WHERE attempt_id='selected-attempt'
         AND json_extract(detail_json,'$.workspace_id')='selected-workspace'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(selected_recovery, 1);
}

fn finish_continue_with_restart_hold(
    app: &Application,
    id: &str,
    task: &str,
    attempt: &str,
) -> Result<Option<ContinueRefusal>> {
    let mut connection = app.store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    let (expected_version, lifecycle, attention, status, phase, archived): (i64, String, String, String, String, bool) = tx.query_row(
        "SELECT c.expected_version,t.lifecycle,t.attention,a.status,a.phase,t.archived_at IS NOT NULL
         FROM controls c JOIN attempts a ON a.id=c.attempt_id JOIN tasks t ON t.id=a.task_id
         WHERE c.id=?1 AND c.attempt_id=?2 AND a.task_id=?3 AND c.kind='continue'
           AND c.state IN ('requested','draining')",
        params![id, attempt, task],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    let version: i64 = tx.query_row(
        "SELECT version FROM tasks WHERE id=?1",
        params![task],
        |row| row.get(0),
    )?;
    let latest_attempt: bool = tx.query_row(
        "SELECT id=(SELECT id FROM attempts WHERE task_id=?2 ORDER BY created_at DESC LIMIT 1)
         FROM attempts WHERE id=?1",
        params![attempt, task],
        |row| row.get(0),
    )?;
    let rework: Option<(String, bool)> = tx
        .query_row(
            "SELECT state,COALESCE(json_extract(result_json,'$.cancellation_pending'),0)
         FROM rework_intents WHERE new_attempt_id=?1 AND state NOT IN ('completed','cancelled')",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let manager_control_active: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
         AND kind IN ('manager_stop','manager_change')
         AND state NOT IN ('finished','cancelled','superseded','rejected'))",
        params![attempt],
        |row| row.get(0),
    )?;
    let original_attention = if attention == "none" && status == "held" {
        "paused"
    } else {
        &attention
    };
    let ordinary = crate::workflow::ordinary_control_allowed(
        &lifecycle,
        original_attention,
        Some(&status),
        Some(&phase),
        rework
            .as_ref()
            .map(|(state, pending)| (state.as_str(), *pending)),
        manager_control_active,
        "continue",
    );
    let parked = crate::workflow::restart_hold_continue_allowed(
        &tx,
        task,
        attempt,
        &lifecycle,
        &attention,
        &status,
        &phase,
        rework
            .as_ref()
            .map(|(state, pending)| (state.as_str(), *pending)),
        manager_control_active,
    )?;
    let reserved_route: Option<(String, String, String, String, i64, String)> = tx
        .query_row(
            "SELECT json_extract(route.detail_json,'$.rejection_event_id'),route.entity_id,
                    json_extract(route.detail_json,'$.role_generation_id'),
                    json_extract(route.detail_json,'$.transcript_epoch'),
                    json_extract(route.detail_json,'$.resume_count'),route.operation_id
             FROM audit_events route JOIN controls control ON control.requested_operation_id=route.operation_id
             WHERE control.id=?1 AND route.event_code='session.resume.fresh_route.reserved'",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()?;
    let reserved_fresh_route =
        if let Some((rejection, session, generation, epoch, count, operation)) = reserved_route {
            crate::workflow::fresh_route_authorized(
                &tx,
                &rejection,
                &session,
                &generation,
                &epoch,
                count,
                attempt,
                task,
                Some(&operation),
            )?
        } else {
            false
        };
    let claim_unknown: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1 AND state='unknown')",
        params![attempt],
        |row| row.get(0),
    )?;
    if claim_unknown {
        let disposition = disposition_continue_ownership_in(
            &tx,
            id,
            task,
            attempt,
            "control cannot normalize task or attempt while repository claim ownership is unknown",
        )?;
        tx.commit()?;
        return Ok(Some(ContinueRefusal::Ownership(disposition)));
    }
    let unresolved_recovery: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1 AND state='attention_required')",
        params![attempt],
        |row| row.get(0),
    )?;
    if unresolved_recovery {
        let disposition = disposition_continue_ownership_in(
            &tx,
            id,
            task,
            attempt,
            "continue cannot normalize task or attempt while unresolved recovery evidence remains",
        )?;
        tx.commit()?;
        return Ok(Some(ContinueRefusal::Ownership(disposition)));
    }
    let eligible = version == expected_version + 1
        && !archived
        && latest_attempt
        && !crate::workflow::pending_continue(&tx, attempt, Some(id))?
        && (ordinary
            || parked
            || (reserved_fresh_route
                && rework.is_none()
                && !manager_control_active
                && attention == "none"
                && status == "needs_input"
                && lifecycle == "in_progress"));
    if !eligible {
        let reason = "Continue control is stale or no longer eligible for this attempt";
        tx.execute(
            "UPDATE controls SET state='rejected',payload_json=json_set(payload_json,'$.reason',?1),updated_at=?2 WHERE id=?3",
            params![reason, now, id],
        )?;
        tx.commit()?;
        return Ok(Some(ContinueRefusal::Stale(reason)));
    }
    require_continue_ownership(&tx, attempt)?;
    crate::store::Store::release_restart_hold_for_fresh_dispatch_in(
        &tx,
        attempt,
        "human_continue",
        &now,
    )?;
    require_continue_ownership(&tx, attempt)?;
    finish_control_in_transaction(&tx, id, task, attempt, "none", "running", None, None, &now)?;
    tx.commit()?;
    Ok(None)
}

fn require_continue_ownership(
    transaction: &rusqlite::Transaction<'_>,
    attempt: &str,
) -> Result<()> {
    let claim_unknown: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1 AND state='unknown')",
        params![attempt],
        |row| row.get(0),
    )?;
    if claim_unknown {
        bail!(
            "control cannot normalize task or attempt while repository claim ownership is unknown"
        )
    }
    let unresolved_recovery: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1 AND state='attention_required')",
        params![attempt],
        |row| row.get(0),
    )?;
    if unresolved_recovery {
        bail!(
            "continue cannot normalize task or attempt while unresolved recovery evidence remains"
        )
    }
    Ok(())
}

fn finish_control_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    task: &str,
    attempt: &str,
    attention: &str,
    status: &str,
    step_budget: Option<i64>,
    cancel_ownership: Option<&ReworkCancelOwnership>,
    now: &str,
) -> Result<()> {
    let rework: Option<(String, String)> = tx
        .query_row(
            "SELECT parent_attempt_id,state FROM rework_intents
             WHERE new_attempt_id=?1 AND state NOT IN ('completed','cancelled')",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let (Some((parent, _)), Some(expected)) = (&rework, cancel_ownership) {
        let current = rework_cancel_ownership(&tx, attempt, parent)?;
        if !current.same_snapshot(expected) {
            bail!("rework cancellation ownership changed before its atomic application")
        }
    }
    if status == "running" {
        let claim_unknown: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1 AND state='unknown')",
            params![attempt],
            |row| row.get(0),
        )?;
        if claim_unknown {
            bail!("control cannot normalize task or attempt while repository claim ownership is unknown")
        }
    }
    let changed = tx.execute("UPDATE controls SET state='finished',updated_at=?1 WHERE id=?2 AND state IN ('requested','draining')",params![now,id])?;
    if changed != 1 {
        bail!("control changed before its atomic application")
    }
    let parent_claim_quiescent = cancel_ownership.map(|ownership| ownership.ordinary_claim_only);
    if let Some((parent, rework_state)) = rework {
        match status {
            "cancelled" => {
                let cancellation_pending = parent_claim_quiescent != Some(true);
                tx.execute(
                    "UPDATE rework_intents SET state=?1,result_json=?2,updated_at=?3
                     WHERE new_attempt_id=?4 AND state NOT IN ('completed','cancelled')",
                    params![
                        if cancellation_pending {
                            "recovery_required"
                        } else {
                            "cancelled"
                        },
                        serde_json::json!({
                            "cancelled_by_control":id,
                            "cancellation_pending":cancellation_pending,
                            "parent_claim_quiescent":parent_claim_quiescent == Some(true),
                            "parent_claim_retained":parent_claim_quiescent != Some(true)
                        })
                        .to_string(),
                        now,
                        attempt
                    ],
                )?;
                tx.execute(
                    "UPDATE workspaces SET state='cancelled',updated_at=?1 WHERE attempt_id IN (?2,?3) AND state!='cancelled'",
                    params![now, attempt, parent],
                )?;
                tx.execute(
                    "UPDATE controls SET state='cancelled',updated_at=?1 WHERE attempt_id=?2 AND id!=?3 AND state IN ('requested','draining','proposed')",
                    params![now, attempt, id],
                )?;
                if cancellation_pending {
                    tx.execute(
                        "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id IN (?2,?3)",
                        params![now, attempt, parent],
                    )?;
                    tx.execute(
                        "UPDATE tasks SET attention='needs_recovery',version=version+1,updated_at=?1 WHERE id=?2",
                        params![now, task],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE attempts SET status='cancelled',updated_at=?1 WHERE id IN (?2,?3)",
                        params![now, attempt, parent],
                    )?;
                    tx.execute(
                        "UPDATE claims SET state='cancelled',updated_at=?1 WHERE attempt_id IN (?2,?3)",
                        params![now, attempt, parent],
                    )?;
                    tx.execute(
                        "UPDATE tasks SET attention='none',lifecycle='cancelled',version=version+1,updated_at=?1 WHERE id=?2",
                        params![now, task],
                    )?;
                }
            }
            "held" => {
                if rework_state == "recovery_required" {
                    tx.execute(
                        "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
                        params![now, attempt],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE rework_intents SET state='paused',updated_at=?1 WHERE new_attempt_id=?2 AND state NOT IN ('completed','cancelled','recovery_required')",
                        params![now, attempt],
                    )?;
                    tx.execute(
                        "UPDATE attempts SET status='held',updated_at=?1 WHERE id=?2",
                        params![now, attempt],
                    )?;
                }
                tx.execute(
                    "UPDATE tasks SET attention='paused',version=version+1,updated_at=?1 WHERE id=?2",
                    params![now, task],
                )?;
            }
            "running" if rework_state == "paused" => {
                tx.execute(
                    "UPDATE rework_intents SET state='reserved',updated_at=?1 WHERE new_attempt_id=?2 AND state='paused'",
                    params![now, attempt],
                )?;
                tx.execute(
                    "UPDATE attempts SET status='materialization_pending',step_budget=COALESCE(?1,step_budget),updated_at=?2 WHERE id=?3",
                    params![step_budget, now, attempt],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention='none',version=version+1,updated_at=?1 WHERE id=?2",
                    params![now, task],
                )?;
            }
            "running" if rework_state == "recovery_required" => {
                tx.execute(
                    "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
                    params![now, attempt],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention='needs_recovery',version=version+1,updated_at=?1 WHERE id=?2",
                    params![now, task],
                )?;
            }
            _ => {
                tx.execute(
                    "UPDATE tasks SET attention=?1,version=version+1,updated_at=?2 WHERE id=?3",
                    params![attention, now, task],
                )?;
                tx.execute(
                    "UPDATE attempts SET status=?1,step_budget=COALESCE(?2,step_budget),updated_at=?3 WHERE id=?4",
                    params![status, step_budget, now, attempt],
                )?;
            }
        }
        let (applied_attention, applied_status, applied_rework_state): (String, String, String) =
            tx.query_row(
                "SELECT t.attention,a.status,ri.state FROM tasks t
                 JOIN attempts a ON a.task_id=t.id JOIN rework_intents ri ON ri.new_attempt_id=a.id
                 WHERE t.id=?1 AND a.id=?2",
                params![task, attempt],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        tx.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','control.applied','control',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),id,serde_json::json!({"attention":applied_attention,"attempt_status":applied_status,"rework_state":applied_rework_state,"unfinished_rework":true,"parent_claim_quiescent":parent_claim_quiescent}).to_string(),now])?;
        return Ok(());
    }
    tx.execute(
        "UPDATE tasks SET attention=?1,lifecycle=CASE WHEN ?2='cancelled' THEN 'cancelled' ELSE lifecycle END,version=version+1,updated_at=?3 WHERE id=?4",
        params![attention, status, now, task],
    )?;
    tx.execute(
        "UPDATE attempts SET status=?1,step_budget=COALESCE(?2,step_budget),updated_at=?3 WHERE id=?4",
        params![status, step_budget, now, attempt],
    )?;
    if status == "cancelled" {
        tx.execute(
            "UPDATE claims SET state='cancelled',updated_at=?1 WHERE attempt_id=?2 AND state NOT IN ('unknown','stopping')",
            params![now, attempt],
        )?;
    }
    tx.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','control.applied','control',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),id,serde_json::json!({"attention":attention,"attempt_status":status}).to_string(),now])?;
    Ok(())
}
fn consume_blocked_result(app: &Application, attempt: &str) -> Result<Option<serde_json::Value>> {
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row = eligible_blocked_result(&transaction, attempt)?;
    if let Some((result_id, outcome, summary)) = row {
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE attempts SET status='needs_input',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        transaction.execute("UPDATE tasks SET attention='needs_input',version=version+1,updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
        transaction.execute(
            "UPDATE controls SET state='superseded',updated_at=?1
             WHERE attempt_id=?2 AND kind='transition_proposal' AND state='proposed'",
            params![now, attempt],
        )?;
        transaction.execute(
            "UPDATE role_results SET consumed_at=?1 WHERE id=?2",
            params![now, result_id],
        )?;
        transaction.commit()?;
        return Ok(Some(
            serde_json::json!({"action":"held","reason":outcome,"summary":summary}),
        ));
    }
    transaction.commit()?;
    Ok(None)
}

fn eligible_blocked_result(
    connection: &Connection,
    attempt: &str,
) -> Result<Option<(String, String, String)>> {
    Ok(connection
        .query_row(
            "SELECT rr.id,rr.outcome,rr.summary FROM role_results rr
             JOIN role_generations rg ON rg.id=rr.role_generation_id
             WHERE rg.attempt_id=?1 AND rr.outcome IN ('blocked','needs_input')
               AND rr.consumed_at IS NULL ORDER BY rr.created_at DESC LIMIT 1",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}
fn waiting(attempt: &Attempt, for_what: &str) -> serde_json::Value {
    serde_json::json!({"action":"waiting","attempt_id":attempt.id,"phase":attempt.phase,"for":for_what})
}
