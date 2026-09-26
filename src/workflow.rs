use crate::domain::{
    AppStateDto, AttentionCategory, AttentionItem, AttentionTarget, ContinuationAction,
    ContinuationActionKind, DecisionActionBinding, DecisionControlPolicy, DecisionDisposition,
    DecisionEvidenceState, DecisionExplanation, DecisionNextAction, DecisionObservedRevision,
    DecisionOwner, DecisionOwnership, DecisionPrerequisite, DecisionSubject, HumanCommand,
    OperationResult, PermissionRequestDto, ProjectDto, RestartCandidateResult, TaskAttentionTarget,
    TaskDto,
};
use crate::store::{json_hash, Store};
use crate::supervisor::GRACEFUL_STOP_SECONDS;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::HashSet;

// A reserved route keeps its captured authority but has already moved reviewer and Explorer ownership.
const FRESH_ROUTE_AUTHORITY: &str = "SELECT EXISTS(
                            SELECT 1 FROM audit_events event
                            JOIN sessions session ON session.id=event.entity_id
                            JOIN role_generations generation ON generation.id=session.role_generation_id
                            JOIN attempts current_attempt ON current_attempt.id=generation.attempt_id
                            WHERE event.event_code='session.resume.rejected'
                              AND event.id=?1 AND session.id=?2 AND generation.id=?3
                              AND session.transcript_epoch=?4 AND session.resume_count=?5
                              AND generation.attempt_id=?6 AND session.status='exited'
                              AND json_extract(event.detail_json,'$.session_id')=session.id
                              AND json_extract(event.detail_json,'$.transcript_epoch')=session.transcript_epoch
                              AND CAST(json_extract(event.detail_json,'$.resume_count') AS INTEGER)=session.resume_count
                              AND json_extract(event.detail_json,'$.role_generation_id')=generation.id
                              AND json_extract(event.detail_json,'$.category') IN (
                                'frozen_runtime_identity_changed',
                                'provider_compatibility_unsupported',
                                'provider_compatibility_contract_changed')
                              AND json_extract(event.detail_json,'$.same_profile_authority')=1
                              AND current_attempt.id=(SELECT latest.id FROM attempts latest
                                WHERE latest.task_id=current_attempt.task_id
                                ORDER BY latest.created_at DESC LIMIT 1)
                              AND current_attempt.phase IS json_extract(event.detail_json,'$.attempt_phase')
                              AND current_attempt.candidate_hash IS json_extract(event.detail_json,'$.candidate_hash')
                              AND current_attempt.plan_hash IS json_extract(event.detail_json,'$.plan_hash')
                              AND json_type(event.detail_json,'$.frozen_capability_key')='text'
                              AND json_type(event.detail_json,'$.observed_capability_key')='text'
                              AND json_extract(event.detail_json,'$.frozen_capability_key') IS NOT
                                  json_extract(event.detail_json,'$.observed_capability_key')
                              AND json_type(event.detail_json,'$.observed_identity')='object'
                              AND json_extract(event.detail_json,'$.observed_identity.provider')=session.provider
                              AND json_extract(event.detail_json,'$.observed_identity.role')=generation.role
                              AND json_extract(event.detail_json,'$.observed_identity.mode')='interactive_pty'
                              AND json_extract(event.detail_json,'$.observed_identity.executable_version')!=''
                              AND json_extract(event.detail_json,'$.observed_identity.model')!=''
                              AND json_extract(event.detail_json,'$.observed_identity.effort')!=''
                              AND EXISTS(SELECT 1 FROM capabilities current_capability
                                WHERE current_capability.rowid=(
                                  SELECT latest_capability.rowid FROM capabilities latest_capability
                                  WHERE latest_capability.provider=json_extract(event.detail_json,'$.observed_identity.provider')
                                    AND latest_capability.executable_version=json_extract(event.detail_json,'$.observed_identity.executable_version')
                                    AND latest_capability.role=json_extract(event.detail_json,'$.observed_identity.role')
                                    AND latest_capability.mode=json_extract(event.detail_json,'$.observed_identity.mode')
                                  ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1)
                                  AND current_capability.status='supported'
                                  AND current_capability.proof_json!='{}'
                                  AND current_capability.config_hash=json_extract(event.detail_json,'$.observed_capability_key')
                                  AND (json_extract(event.detail_json,'$.category')='frozen_runtime_identity_changed'
                                    OR (json_type(current_capability.proof_json,'$.compatibility')='object'
                                      AND json_extract(current_capability.proof_json,'$.compatibility.effective_hash')=
                                        json_extract(event.detail_json,'$.observed_identity.compatibility_hash'))))
                              AND EXISTS(SELECT 1 FROM role_settings profile
                                   WHERE profile.task_id=?7 AND profile.role=generation.role
                                     AND profile.revision=generation.config_revision
                                     AND json_extract(profile.config_json,'$.provider')=
                                         json_extract(event.detail_json,'$.observed_identity.provider')
                                     AND json_extract(profile.config_json,'$.model')=
                                         json_extract(event.detail_json,'$.observed_identity.model')
                                     AND json_extract(profile.config_json,'$.effort')=
                                         json_extract(event.detail_json,'$.observed_identity.effort'))
                              AND (EXISTS(SELECT 1 FROM role_settings setting
                                   WHERE setting.task_id=?7 AND setting.role=generation.role
                                     AND setting.revision=generation.config_revision
                                     AND setting.effective_generation_id=generation.id)
                                   OR (generation.role='implementer' AND generation.lane_id!='default'
                                     AND EXISTS(SELECT 1 FROM lane_generations lane
                                       WHERE lane.lane_id=generation.lane_id
                                         AND lane.effective_generation_id=generation.id)))
                              AND generation.role!='final_verifier'
                              AND (generation.role NOT IN ('plan_reviewer','code_reviewer')
                                   OR EXISTS(SELECT 1 FROM review_requests review
                                     JOIN review_budgets budget
                                       ON budget.attempt_id=review.attempt_id
                                      AND budget.review_kind=review.review_kind
                                    WHERE review.id=json_extract(event.detail_json,'$.review_request_id')
                                      AND review.attempt_id=current_attempt.id
                                      AND review.role_generation_id=generation.id
                                      AND review.session_id=session.id
                                      AND review.delivery_state=CASE WHEN ?8 IS NULL THEN 'delivered' ELSE 'superseded_after_rejected_resume' END
                                      AND review.review_kind=CASE generation.role
                                        WHEN 'plan_reviewer' THEN 'plan' ELSE 'code' END
                                      AND review.candidate_hash IS CASE review.review_kind
                                        WHEN 'plan' THEN current_attempt.plan_hash
                                        ELSE current_attempt.candidate_hash END
                                      AND budget.spent < budget.initial_allowance+budget.extension_allowance))
                              AND (generation.role!='explorer' OR EXISTS(
                                    SELECT 1 FROM trip_explorer_decisions decision
                                     WHERE decision.attempt_id=current_attempt.id
                                       AND decision.activated=1
                                       AND decision.outcome_json IS NULL
                                       AND ((?8 IS NULL AND decision.role_generation_id=generation.id) OR (?8 IS NOT NULL AND decision.role_generation_id IS NULL))
                                       AND (?8 IS NULL OR decision.id=(
                                         SELECT json_extract(route.detail_json,'$.explorer_decision_id')
                                         FROM audit_events route WHERE route.operation_id=?8
                                           AND route.event_code='session.resume.fresh_route.reserved'))
                                       AND decision.candidate_hash IS current_attempt.candidate_hash))
                              AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                                WHERE recovery.session_id=session.id
                                  AND recovery.state='attention_required')
                              AND NOT EXISTS(SELECT 1 FROM role_generations newer
                                WHERE newer.attempt_id=generation.attempt_id
                                  AND newer.role=generation.role
                                  AND newer.lane_id=generation.lane_id
                                  AND newer.generation>generation.generation)
                              AND ((?8 IS NULL AND NOT EXISTS(SELECT 1 FROM audit_events consumed
                                WHERE consumed.event_code='session.resume.fresh_route.reserved'
                                  AND json_extract(consumed.detail_json,'$.rejection_event_id')=event.id))
                                OR (?8 IS NOT NULL AND EXISTS(SELECT 1 FROM audit_events reserved
                                  WHERE reserved.event_code='session.resume.fresh_route.reserved'
                                    AND reserved.operation_id=?8 AND reserved.entity_id=session.id
                                    AND json_extract(reserved.detail_json,'$.rejection_event_id')=event.id
                                    AND json_extract(reserved.detail_json,'$.role_generation_id')=generation.id
                                    AND json_extract(reserved.detail_json,'$.transcript_epoch')=session.transcript_epoch
                                    AND CAST(json_extract(reserved.detail_json,'$.resume_count') AS INTEGER)=session.resume_count
                                    AND json_extract(reserved.detail_json,'$.attempt_id')=current_attempt.id)))
                        )";

pub(crate) fn fresh_route_authorized(
    connection: &Connection,
    rejection_event_id: &str,
    session_id: &str,
    generation_id: &str,
    transcript_epoch: &str,
    resume_count: i64,
    attempt_id: &str,
    task_id: &str,
    reserved_operation_id: Option<&str>,
) -> Result<bool> {
    let allowed = connection.query_row(
        FRESH_ROUTE_AUTHORITY,
        params![
            rejection_event_id,
            session_id,
            generation_id,
            transcript_epoch,
            resume_count,
            attempt_id,
            task_id,
            reserved_operation_id
        ],
        |row| row.get(0),
    )?;
    Ok(allowed)
}

pub(crate) fn ordinary_control_allowed(
    lifecycle: &str,
    attention: &str,
    attempt_status: Option<&str>,
    attempt_phase: Option<&str>,
    rework: Option<(&str, bool)>,
    manager_control_active: bool,
    action: &str,
) -> bool {
    if manager_control_active || matches!(lifecycle, "done" | "cancelled" | "backlog") {
        return false;
    }
    if lifecycle == "ready" {
        return action == "run_next";
    }
    let Some(status) = attempt_status else {
        return false;
    };
    if !matches!(
        attempt_phase,
        Some(
            "planning"
                | "plan_review"
                | "awaiting_plan_approval"
                | "awaiting_implementation_authorization"
                | "implementation"
                | "code_review"
                | "checks"
                | "final_review"
                | "manager_handoff"
                | "awaiting_human_review"
        )
    ) {
        return false;
    }
    if let Some((state, cancellation_pending)) = rework {
        if cancellation_pending {
            return false;
        }
        let permitted = match state {
            "reserved" | "materializing" => {
                matches!(action, "pause_now" | "pause_after_role" | "cancel")
            }
            "paused" | "recovery_required" => matches!(
                action,
                "continue" | "pause_now" | "pause_after_role" | "cancel"
            ),
            _ => false,
        };
        return permitted && !matches!(status, "done" | "cancelled" | "failed");
    }
    if matches!(status, "done" | "cancelled" | "failed") {
        return action == "retry" && rework.is_none();
    }
    match action {
        "pause_now" | "pause_after_role" | "cancel" => true,
        "continue" => {
            (attention == "paused"
                || (attention == "none" && status == "running")
                || (attention == "needs_input"
                    && status == "running"
                    && attempt_phase == Some("planning")))
                && lifecycle != "awaiting_review"
                && !matches!(
                    attempt_phase,
                    Some(
                        "awaiting_plan_approval"
                            | "awaiting_implementation_authorization"
                            | "plan_review"
                            | "code_review"
                            | "final_review"
                    )
                )
        }
        "retry" => attention == "needs_input" && lifecycle != "awaiting_review",
        "run_next" => attention == "paused" && lifecycle != "awaiting_review",
        _ => false,
    }
}

pub(crate) fn restart_hold_continue_allowed(
    connection: &Connection,
    task_id: &str,
    attempt_id: &str,
    lifecycle: &str,
    attention: &str,
    attempt_status: &str,
    attempt_phase: &str,
    rework: Option<(&str, bool)>,
    manager_control_active: bool,
) -> Result<bool> {
    // A parked restart uses ordinary Continue's phase and human gates; its
    // process and candidate safety checks remain at hold release.
    if attention != "restart_parked"
        || attempt_status != "restart_parked"
        || rework.is_some()
        || !ordinary_control_allowed(
            lifecycle,
            "paused",
            Some("running"),
            Some(attempt_phase),
            None,
            manager_control_active,
            "continue",
        )
    {
        return Ok(false);
    }
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM restart_candidates
             WHERE task_id=?1 AND attempt_id=?2
               AND state NOT IN ('resumed','released_fresh_dispatch','cancelled'))",
            params![task_id, attempt_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn pending_continue(
    connection: &Connection,
    attempt_id: &str,
    except_control_id: Option<&str>,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1 AND kind='continue'
             AND state NOT IN ('rejected','finished','cancelled','superseded')
             AND (?2 IS NULL OR id!=?2))",
            params![attempt_id, except_control_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(crate) fn ordinary_control_policy(
    connection: &Connection,
    task_id: &str,
    attempt_id: Option<&str>,
) -> Result<Vec<String>> {
    let task: Option<(String, String, bool)> = connection
        .query_row(
            "SELECT lifecycle,attention,archived_at IS NOT NULL FROM tasks WHERE id=?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((lifecycle, attention, archived)) = task else {
        return Ok(Vec::new());
    };
    if archived {
        return Ok(Vec::new());
    }
    let (status, phase, rework, manager_control_active) = if let Some(attempt_id) = attempt_id {
        let current: Option<(String, String)> = connection
            .query_row(
                "SELECT status,phase FROM attempts WHERE id=?1 AND task_id=?2
             AND id=(SELECT id FROM attempts WHERE task_id=?2 ORDER BY created_at DESC LIMIT 1)",
                params![attempt_id, task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let rework = connection.query_row(
            "SELECT state,COALESCE(json_extract(result_json,'$.cancellation_pending'),0)
             FROM rework_intents WHERE new_attempt_id=?1 AND state NOT IN ('completed','cancelled')",
            params![attempt_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        ).optional()?;
        let manager_control_active: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
             AND kind IN ('manager_stop','manager_change')
             AND state NOT IN ('finished','cancelled','superseded','rejected'))",
            params![attempt_id],
            |row| row.get(0),
        )?;
        (
            current.as_ref().map(|(status, _)| status.clone()),
            current.map(|(_, phase)| phase),
            rework,
            manager_control_active,
        )
    } else {
        (None, None, None, false)
    };
    let mut controls: Vec<String> = [
        "continue",
        "run_next",
        "pause_after_role",
        "pause_now",
        "retry",
        "cancel",
    ]
    .into_iter()
    .filter(|action| {
        ordinary_control_allowed(
            &lifecycle,
            &attention,
            status.as_deref(),
            phase.as_deref(),
            rework
                .as_ref()
                .map(|(state, pending)| (state.as_str(), *pending)),
            manager_control_active,
            action,
        )
    })
    .map(str::to_owned)
    .collect();
    if attention == "restart_parked"
        && status.as_deref() == Some("restart_parked")
        && rework.is_none()
    {
        controls.retain(|control| control != "continue");
    }
    if let (Some(attempt_id), Some(status), Some(phase)) =
        (attempt_id, status.as_deref(), phase.as_deref())
    {
        if restart_hold_continue_allowed(
            connection,
            task_id,
            attempt_id,
            &lifecycle,
            &attention,
            status,
            phase,
            rework
                .as_ref()
                .map(|(state, pending)| (state.as_str(), *pending)),
            manager_control_active,
        )? {
            controls.push("continue".to_owned());
        }
    }
    if let Some(attempt_id) = attempt_id {
        if pending_continue(connection, attempt_id, None)? {
            controls.retain(|control| control != "continue");
        }
    }
    Ok(controls)
}

fn nondelivery_recovery_identity_current(
    connection: &Connection,
    session: &str,
    identity: &str,
) -> Result<bool> {
    let recorded: serde_json::Value = match serde_json::from_str(identity) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let invocation: Option<String> = connection
        .query_row(
            "SELECT ri.process_identity_json FROM resume_invocations ri
         JOIN sessions s ON s.id=ri.session_id AND s.transcript_epoch=ri.transcript_epoch
         WHERE ri.session_id=?1 AND ri.state='proven_nondelivery_cleanup_unknown'
         ORDER BY ri.resume_ordinal DESC LIMIT 1",
            params![session],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if invocation
        .as_deref()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .as_ref()
        == Some(&recorded)
    {
        return Ok(true);
    }
    let (anchor, boot, root, group): (Option<String>, Option<String>, Option<i64>, Option<i32>) =
        connection.query_row(
            "SELECT recovery_anchor_json,launch_boot_identity,recovery_root_pid,recovery_process_group_id
             FROM sessions WHERE id=?1",
            params![session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let mut statement = connection.prepare(
        "SELECT pid,parent_pid,process_group_id,native_start_marker FROM session_processes
         WHERE session_id=?1 ORDER BY pid,native_start_marker",
    )?;
    let members = statement
        .query_map(params![session], |row| {
            Ok(serde_json::json!({
                "pid":row.get::<_,i64>(0)?,
                "parent_pid":row.get::<_,Option<i64>>(1)?,
                "process_group_id":row.get::<_,i32>(2)?,
                "native_start_marker":row.get::<_,String>(3)?
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // This is the exact fallback evidence written for uncertain wrapper cleanup.
    Ok(recorded
        == serde_json::json!({
            "provider_spawned":false,
            "wrapper_anchor":anchor.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok()),
            "launch_boot_identity":boot,
            "root_pid":root,
            "process_group_id":group,
            "observed_members":members
        }))
}

pub(crate) fn validate_recovery_identity(
    connection: &Connection,
    recovery_id: &str,
    task_id: &str,
    attempt_id: &str,
    session_id: Option<&str>,
) -> Result<()> {
    if recovery_id.is_empty() {
        bail!("recovery_id is required")
    }
    let record = connection
        .query_row(
            "SELECT r.session_id, json_extract(r.detail_json,'$.kind'),
                json_extract(r.detail_json,'$.check_id'),
                json_extract(r.detail_json,'$.claim_id'),
                json_extract(r.detail_json,'$.freeze_id'),
                r.process_identity_json,
                EXISTS(SELECT 1 FROM recovery_records newer
                       WHERE newer.id!=r.id AND newer.state='attention_required'
                         AND newer.attempt_id=r.attempt_id
                         AND ((newer.session_id=r.session_id AND r.session_id IS NOT NULL
                               AND COALESCE(json_extract(newer.detail_json,'$.kind'),'prior_boot_process')
                                   =COALESCE(json_extract(r.detail_json,'$.kind'),'prior_boot_process')) OR
                              (r.session_id IS NULL AND newer.session_id IS NULL AND
                               COALESCE(json_extract(newer.detail_json,'$.check_id'),
                                        json_extract(newer.detail_json,'$.claim_id'),
                                        json_extract(newer.detail_json,'$.freeze_id')) =
                               COALESCE(json_extract(r.detail_json,'$.check_id'),
                                        json_extract(r.detail_json,'$.claim_id'),
                                        json_extract(r.detail_json,'$.freeze_id'))))
                         AND (newer.created_at>r.created_at OR
                              (newer.created_at=r.created_at AND newer.rowid>r.rowid)))
         FROM recovery_records r JOIN attempts a ON a.id=r.attempt_id
         WHERE r.id=?1 AND r.attempt_id=?2 AND a.task_id=?3
           AND a.status IN ('needs_recovery','running') AND r.state='attention_required'",
            params![recovery_id, attempt_id, task_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, bool>(6)?,
                ))
            },
        )
        .optional()?;
    let Some(record) = record else {
        let rework: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM rework_intents ri
              JOIN attempts a ON a.id=ri.new_attempt_id
              JOIN tasks t ON t.id=a.task_id
              WHERE ri.id=?1 AND ri.new_attempt_id=?2 AND t.id=?3
                AND ri.state='recovery_required' AND a.status='needs_recovery'
                AND t.attention='needs_recovery' AND t.lifecycle='in_progress')",
            params![recovery_id, attempt_id, task_id],
            |row| row.get(0),
        )?;
        if session_id.is_none() && rework {
            return Ok(());
        }
        bail!("recovery identity is stale or does not match this task and attempt")
    };
    if record.0.as_deref() != session_id || record.6 {
        bail!("recovery record session is mismatched or superseded")
    }
    if let Some(session) = session_id {
        if matches!(
            record.1.as_deref(),
            Some("workspace_reservation" | "graceful_stop_deadline")
        ) {
            bail!("this recovery kind requires its exact dedicated action")
        }
        let current: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                 WHERE s.id=?1 AND rg.attempt_id=?2 AND s.status IN ('recovery_required','exited')
                   )",
            params![session, attempt_id], |row| row.get(0),
        )?;
        let process_current: bool = connection.query_row(
            "SELECT ?2 IS NULL OR COALESCE(process_identity_json=?2,0) FROM sessions WHERE id=?1",
            params![session, record.5],
            |row| row.get(0),
        )?;
        let nondelivery_current =
            if record.1.as_deref() == Some("provider_nondelivery_cleanup_unknown") {
                match record.5.as_deref() {
                    Some(identity) if !identity.is_empty() => {
                        nondelivery_recovery_identity_current(connection, session, identity)?
                    }
                    _ => false,
                }
            } else {
                false
            };
        if !current || !(process_current || nondelivery_current) {
            bail!("recovery session no longer belongs to this attempt")
        }
    } else {
        let current: bool = match (record.1.as_deref(), record.2.as_deref(), record.3.as_deref(), record.4.as_deref()) {
            (Some("database_restore_claim"), _, Some(claim), _) => connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM claims WHERE id=?1 AND attempt_id=?2 AND state IN ('reserved','launching','running','unknown','stopping'))",
                params![claim, attempt_id], |row| row.get(0))?,
            (Some("database_restore_freeze"), _, _, Some(freeze)) => connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE id=?1 AND attempt_id=?2 AND state='recovery_required')",
                params![freeze, attempt_id], |row| row.get(0))?,
            (kind, Some(check), _, _) if !matches!(kind, Some("workspace_reservation" | "graceful_stop_deadline")) => connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM check_runs WHERE id=?1 AND attempt_id=?2 AND status='recovery_required')",
                params![check, attempt_id], |row| row.get(0))?,
            _ => false,
        };
        if !current {
            bail!("recovery record kind or subject is stale or unsupported")
        }
    }
    Ok(())
}

fn requested_manager_revision(payload: &serde_json::Value) -> Result<i64> {
    payload
        .get("settings_revision")
        .and_then(serde_json::Value::as_i64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| anyhow!("manager change requires a positive settings_revision"))
}

fn fresh_resume_rejection_binding(
    payload: &serde_json::Value,
) -> Result<(&str, &str, &str, &str, i64)> {
    let binding = payload
        .get("resume_rejection")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("fresh dispatch requires the projected rejected-session binding"))?;
    let required = |name: &str| {
        binding
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("fresh dispatch rejected-session binding is missing {name}"))
    };
    let resume_count = binding
        .get("resume_count")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| {
            anyhow!("fresh dispatch rejected-session binding is missing resume_count")
        })?;
    Ok((
        required("rejection_event_id")?,
        required("session_id")?,
        required("role_generation_id")?,
        required("transcript_epoch")?,
        resume_count,
    ))
}

fn resume_failure_has_current_replacement_authority(
    transaction: &Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM audit_events event
            JOIN sessions rejected_session ON rejected_session.id=event.entity_id
            JOIN role_generations rejected_generation
              ON rejected_generation.id=rejected_session.role_generation_id
            JOIN attempts rejected_attempt ON rejected_attempt.id=rejected_generation.attempt_id
            JOIN role_generations replacement
              ON replacement.attempt_id=rejected_generation.attempt_id
             AND replacement.role=rejected_generation.role
             AND replacement.lane_id=rejected_generation.lane_id
             AND replacement.generation>rejected_generation.generation
             AND replacement.status IN ('launch_reserved','running','exited','stopping')
             AND (
                EXISTS(SELECT 1 FROM role_settings setting
                        WHERE setting.task_id=?1 AND setting.role=replacement.role
                          AND setting.revision=replacement.config_revision
                          AND setting.effective_generation_id=replacement.id)
                OR (replacement.role='implementer' AND replacement.lane_id!='default'
                    AND EXISTS(SELECT 1 FROM lane_generations lane
                                WHERE lane.lane_id=replacement.lane_id
                                  AND lane.effective_generation_id=replacement.id))
             )
             WHERE event.event_code='session.resume.rejected'
               AND rejected_session.status='exited'
               AND rejected_attempt.id=?2
               AND rejected_attempt.id=(SELECT latest.id FROM attempts latest
                                         WHERE latest.task_id=?1
                                         ORDER BY latest.created_at DESC LIMIT 1)
               AND json_extract(event.detail_json,'$.attempt_id')=rejected_attempt.id
               AND json_extract(event.detail_json,'$.role_generation_id')=rejected_generation.id
               AND json_extract(event.detail_json,'$.session_id')=rejected_session.id
               AND json_extract(event.detail_json,'$.transcript_epoch')=rejected_session.transcript_epoch
               AND json_extract(event.detail_json,'$.category') IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                              WHERE recovery.attempt_id=rejected_attempt.id
                                AND recovery.state='attention_required')
        )",
        params![task_id, attempt_id],
        |row| row.get(0),
    )?)
}

fn require_manager_replacement_authority(
    transaction: &Transaction<'_>,
    runtime: Option<&crate::trip::CapabilityRuntime>,
    task_id: &str,
    attempt_id: &str,
    settings_revision: i64,
) -> Result<String> {
    let runtime = runtime
        .ok_or_else(|| anyhow!("manager change requires current service capability preparation"))?;
    let (latest, config_json, workspace): (i64, String, String) = transaction.query_row(
        "SELECT (SELECT MAX(revision) FROM role_settings WHERE task_id=?1 AND role='manager'),
                rs.config_json,w.path
         FROM role_settings rs JOIN workspaces w ON w.attempt_id=?2 AND w.state='ready'
         WHERE rs.task_id=?1 AND rs.role='manager' AND rs.revision=?3",
        params![task_id, attempt_id, settings_revision],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if latest != settings_revision {
        bail!("manager change requires the latest requested manager revision")
    }
    let requested: crate::domain::RoleOverride = serde_json::from_str(&config_json)?;
    let prepared = crate::providers::prepare_role_launch_with_bundles(
        requested.provider,
        crate::domain::RoleKind::Manager,
        &requested.model,
        &requested.effort,
        std::path::Path::new(&workspace),
        "manager replacement eligibility only",
        &runtime.role_socket,
        "normalized-manager-change-token",
        "normalized-manager-change-generation",
        "normalized-manager-change-session",
        None,
        &runtime.hooks,
        &runtime.executable,
        &runtime.compatibility_bundles,
    )?;
    crate::trip::current_task_profile_authority_with_bundles(
        transaction,
        task_id,
        crate::domain::RoleKind::Manager,
        settings_revision,
        &prepared.config,
        &runtime.compatibility_bundles,
    )
    .map_err(|error| anyhow!(
        "requested manager replacement is pending exact capability proof or activation: {error:#}; the current manager remains effective"
    ))?;
    transaction
        .query_row(
            "SELECT rg.id FROM role_settings rs JOIN role_generations rg
             ON rg.id=rs.effective_generation_id
             WHERE rs.task_id=?1 AND rs.role='manager' AND rg.attempt_id=?2
               AND (rg.status IN ('running','exited')
                    OR (rg.status='stopping' AND EXISTS(
                      SELECT 1 FROM switch_intents si
                       WHERE si.old_generation_id=rg.id AND si.role='manager'
                         AND si.state='recovery_required'
                    )))",
            params![task_id, attempt_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("current manager authority is unavailable for replacement"))
}

fn manager_stop_is_quiescent(transaction: &Transaction<'_>, attempt_id: &str) -> Result<bool> {
    transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                       WHERE rg.attempt_id=?1 AND rg.role='manager')
           AND NOT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                          WHERE rg.attempt_id=?1 AND rg.role='manager'
                            AND (s.status!='exited'
                              OR COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)!=1))",
        params![attempt_id],
        |row| row.get(0),
    ).map_err(Into::into)
}

fn rework_recovery(
    connection: &rusqlite::Connection,
    attempt: &str,
) -> Result<Option<(String, bool)>> {
    Ok(connection
        .query_row(
            "SELECT parent_attempt_id,COALESCE(json_extract(result_json,'$.cancellation_pending'),0)
             FROM rework_intents WHERE new_attempt_id=?1 AND state='recovery_required'",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn rework_recovery_ownership(
    connection: &rusqlite::Connection,
    child: &str,
    parent: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut statement = connection.prepare(
        "SELECT ownership_key,attempt_id FROM (
           SELECT 'claim:' || c.id || ':' || c.state AS ownership_key,c.attempt_id
             FROM claims c WHERE c.attempt_id IN (?1,?2)
               AND c.state IN ('reserved','launching','running','unknown','stopping')
           UNION ALL
           SELECT 'session:' || s.id || ':' || s.status || ':' ||
                  COALESCE(s.process_identity_json,'') || ':' || COALESCE(s.recovery_root_pid,'') || ':' ||
                  COALESCE(s.recovery_process_group_id,'') || ':' || COALESCE(s.recovery_anchor_json,'') || ':' ||
                  COALESCE(s.launch_boot_identity,''),rg.attempt_id
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id IN (?1,?2) AND s.status='recovery_required'
           UNION ALL
           SELECT 'session-process:' || sp.session_id || ':' || sp.pid || ':' ||
                  sp.native_start_marker || ':' || sp.process_group_id || ':' || COALESCE(sp.parent_pid,''),
                  rg.attempt_id
             FROM session_processes sp JOIN sessions s ON s.id=sp.session_id
             JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id IN (?1,?2) AND s.status='recovery_required'
           UNION ALL
           SELECT 'check:' || cr.id || ':' || cr.status || ':' || COALESCE(cr.recovery_root_pid,'') || ':' ||
                  COALESCE(cr.recovery_process_group_id,'') || ':' || COALESCE(cr.recovery_anchor_json,'') || ':' ||
                  COALESCE(cr.launch_boot_identity,''),cr.attempt_id
             FROM check_runs cr WHERE cr.attempt_id IN (?1,?2) AND cr.status='recovery_required'
           UNION ALL
           SELECT 'check-process:' || cp.check_id || ':' || cp.pid || ':' || cp.native_start_marker || ':' ||
                  cp.process_group_id || ':' || COALESCE(cp.parent_pid,''),cr.attempt_id
             FROM check_processes cp JOIN check_runs cr ON cr.id=cp.check_id
             WHERE cr.attempt_id IN (?1,?2) AND cr.status='recovery_required'
           UNION ALL
           SELECT 'recovery:' || r.id || ':' || COALESCE(r.session_id,'') || ':' ||
                  COALESCE(r.process_identity_json,'') || ':' || r.detail_json AS ownership_key,r.attempt_id
             FROM recovery_records r WHERE r.attempt_id IN (?1,?2)
               AND r.state='attention_required') ORDER BY ownership_key",
    )?;
    let ownership = statement
        .query_map(params![child, parent], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut process_statement = connection.prepare(
        "SELECT DISTINCT attempt_id FROM recovery_records
         WHERE attempt_id IN (?1,?2) AND state='attention_required'
           AND (session_id IS NOT NULL OR json_extract(detail_json,'$.check_id') IS NOT NULL)
         ORDER BY attempt_id",
    )?;
    let attempts = process_statement
        .query_map(params![child, parent], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((
        ownership.into_iter().map(|(key, _)| key).collect(),
        attempts,
    ))
}

fn revalidate_admission_proofs(
    connection: &Connection,
    evidence: &serde_json::Value,
) -> Result<()> {
    if let Some(proof) = evidence
        .get("admission_proof")
        .filter(|value| !value.is_null())
    {
        let session = evidence["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("admission proof has no session"))?;
        if crate::recovery::admission_proof_snapshot(connection, session)?.as_ref() != Some(proof) {
            bail!("interrupted admission proof changed during recovery verification")
        }
    }
    if let Some(items) = evidence.as_array() {
        for item in items {
            revalidate_admission_proofs(connection, item)?;
        }
    } else if let Some(object) = evidence.as_object() {
        for item in object.values() {
            revalidate_admission_proofs(connection, item)?;
        }
    }
    Ok(())
}

fn park_resolved_admission(
    transaction: &rusqlite::Transaction<'_>,
    session: &str,
    attempt: &str,
    recovery_id: &str,
    now: &str,
) -> Result<()> {
    let row: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT rc.state,rc.result_json,s.role_generation_id FROM restart_candidates rc
         JOIN sessions s ON s.id=rc.session_id WHERE rc.session_id=?1 AND rc.attempt_id=?2",
            params![session, attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((state, raw, generation)) = row else {
        return Ok(());
    };
    if matches!(
        state.as_str(),
        "resumed" | "released_fresh_dispatch" | "cancelled"
    ) {
        return Ok(());
    }
    let mut result = RestartCandidateResult::parse(&raw)?;
    result.validate_candidate_state(&state, session)?;
    let incident = result.get("startup_admission_reconciliation");
    if !incident.is_some_and(|value| {
        value["recovery_id"] == recovery_id && value["role_generation_id"] == generation
    }) {
        return Ok(());
    }
    let stale = incident.is_some_and(|value| value["authority"] == "stale");
    let capped = result.restart.replacement_failures >= 3;
    result.set("native_resume_forbidden", serde_json::Value::Bool(true));
    result.set(
        "fresh_dispatch_requires_explicit_continue",
        serde_json::Value::Bool(true),
    );
    let next_state = if stale || capped { "blocked" } else { "parked" };
    transaction.execute("UPDATE restart_candidates SET state=?1,reason='verified interrupted admission is parked until explicit Continue',result_json=?2,updated_at=?3 WHERE session_id=?4 AND state=?5",
        params![next_state,result.encode()?,now,session,state])?;
    Ok(())
}

pub fn execute(store: &Store, command: &HumanCommand) -> Result<OperationResult> {
    execute_with_runtime(store, command, None)
}

pub fn execute_with_runtime(
    store: &Store,
    command: &HumanCommand,
    runtime: Option<&crate::trip::CapabilityRuntime>,
) -> Result<OperationResult> {
    let request_hash = json_hash(command)?;
    let operation_id = command.operation_id();
    if operation_id.trim().is_empty() {
        bail!("operation_id is required")
    }
    if let HumanCommand::ResolveRecovery {
        recovery_id,
        task_id,
        attempt_id,
        session_id,
        ..
    } = command
    {
        let connection = store.lock()?;
        if let Some((stored_hash, stored_result)) = connection.query_row(
            "SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='human_command'",
            params![operation_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()? {
            if stored_hash != request_hash { bail!("operation_id was already used for another request") }
            return Ok(serde_json::from_str(&stored_result)?);
        }
        validate_recovery_identity(
            &connection,
            recovery_id,
            task_id,
            attempt_id,
            session_id.as_deref(),
        )?;
    }
    if let HumanCommand::ResolveRecovery {
        recovery_id,
        attempt_id,
        decision,
        ..
    } = command
    {
        if decision == "cancel" {
            let connection = store.lock()?;
            let workspace_recovery: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id=?1 AND attempt_id=?2
                   AND state='attention_required'
                   AND json_extract(detail_json,'$.kind')='workspace_reservation')",
                params![recovery_id, attempt_id],
                |row| row.get(0),
            )?;
            if workspace_recovery {
                bail!("workspace reservation recovery must use cancel_workspace_reservation")
            }
        }
    }
    let recovery_evidence = if let HumanCommand::ResolveRecovery {
        recovery_id,
        attempt_id,
        session_id,
        decision,
        ..
    } = command
    {
        let (process_recovery, rework) = {
            let connection = store.lock()?;
            let process_recovery = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id=?1 AND attempt_id=?2 AND state='attention_required' AND (session_id IS NOT NULL OR json_extract(detail_json,'$.check_id') IS NOT NULL))",
                params![recovery_id, attempt_id], |row| row.get::<_,bool>(0),
            )?;
            (process_recovery, rework_recovery(&connection, attempt_id)?)
        };
        if rework.is_some() && decision == "confirm_quiescent" {
            None
        } else if decision == "cancel" && rework.is_some() {
            if session_id.is_some() {
                bail!("rework cancellation recovery must reconcile all parent and child ownership together")
            }
            let parent = &rework.as_ref().expect("checked rework recovery").0;
            let (ownership, ownership_attempts) = {
                let connection = store.lock()?;
                rework_recovery_ownership(&connection, attempt_id, parent)?
            };
            let verification = ownership_attempts
                .iter()
                .map(|owned_attempt| {
                    crate::recovery::verify_attempt_quiescent(store, owned_attempt)
                })
                .collect::<Result<Vec<_>>>()?;
            Some(serde_json::json!({
                "kind":"rework_cancellation",
                "ownership":ownership,
                "attempt_ids":ownership_attempts,
                "verification":verification
            }))
        } else if decision == "cancel" && process_recovery {
            let (ownership, ownership_attempts) = {
                let connection = store.lock()?;
                rework_recovery_ownership(&connection, attempt_id, attempt_id)?
            };
            let verification = ownership_attempts
                .iter()
                .map(|owned_attempt| {
                    crate::recovery::verify_attempt_quiescent(store, owned_attempt)
                })
                .collect::<Result<Vec<_>>>()?;
            Some(serde_json::json!({
                "kind":"attempt_cancellation",
                "ownership":ownership,
                "attempt_ids":ownership_attempts,
                "verification":verification
            }))
        } else if decision == "confirm_quiescent" {
            Some(match session_id {
                Some(session) => crate::recovery::verify_session_quiescent(store, session)?,
                None => crate::recovery::verify_attempt_quiescent(store, attempt_id)?,
            })
        } else {
            None
        }
    } else {
        None
    };
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(verified) = recovery_evidence.as_ref() {
        revalidate_admission_proofs(&transaction, verified)?;
    }
    if let Some((stored_hash, result)) = transaction
        .query_row(
            "SELECT request_hash, result_json FROM operation_receipts
         WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='human_command'",
            params![operation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if stored_hash != request_hash {
            bail!("operation ID was already used with different input")
        }
        return Ok(serde_json::from_str(&result)?);
    }
    let now = Utc::now().to_rfc3339();
    let result = match command {
        HumanCommand::UpsertProfileSet { .. }
        | HumanCommand::ArchiveProfileSet { .. }
        | HumanCommand::UpsertTaskRecipe { .. }
        | HumanCommand::ArchiveTaskRecipe { .. }
        | HumanCommand::CreateDraftFromRecipe { .. }
        | HumanCommand::UpsertRecipeSchedule { .. }
        | HumanCommand::PauseRecipeSchedule { .. }
        | HumanCommand::ResumeRecipeSchedule { .. }
        | HumanCommand::ArchiveRecipeSchedule { .. } => {
            crate::recipes::apply_command(&transaction, command, &now)?
        }
        HumanCommand::Trip { .. } => {
            bail!("TRIP commands require the application runtime paths")
        }
        HumanCommand::AddProject {
            path, display_name, ..
        } => {
            if display_name.trim().is_empty() {
                bail!("project display name is required")
            }
            let repository = crate::workspace::inspect(path)?;
            let id = uuid::Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,queue_paused,created_at,updated_at,version,settings_json)
                 VALUES(?1,?2,?3,?4,?5,1,?6,?6,1,'{}')",
                params![id, display_name.trim(), repository.root.to_string_lossy(), repository.identity, repository.head, now],
            ).map_err(|error| anyhow!("add project failed; repository may already be registered: {error}"))?;
            crate::trip::register_project_state(&transaction, &id, &now)?;
            result(operation_id, "project", id, Some(1), "created")
        }
        HumanCommand::RelinkProject {
            project_id,
            path,
            expected_version,
            ..
        } => {
            let (identity, active) = transaction.query_row(
                "SELECT repository_identity, EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
                  WHERE t.project_id=projects.id AND a.status NOT IN ('done','cancelled','failed')) FROM projects WHERE id=?1 AND version=?2",
                params![project_id, expected_version], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
            ).optional()?.ok_or_else(|| anyhow!("project version is stale"))?;
            if active {
                bail!("project cannot be relinked while attempts are active")
            }
            let repository = crate::workspace::inspect(path)?;
            if repository.identity != identity {
                bail!("relinked folder is a different physical repository")
            }
            transaction.execute("UPDATE projects SET repository_path=?1,base_revision=?2,version=version+1,updated_at=?3 WHERE id=?4",
                params![repository.root.to_string_lossy(), repository.head, now, project_id])?;
            result(
                operation_id,
                "project",
                project_id.clone(),
                Some(expected_version + 1),
                "relinked",
            )
        }
        HumanCommand::CreateTask {
            project_id,
            title,
            description,
            acceptance_criteria,
            priority,
            ready,
            role_overrides,
            ..
        } => {
            validate_task(title, acceptance_criteria)?;
            let project_settings: String = transaction
                .query_row(
                    "SELECT settings_json FROM projects WHERE id=?1",
                    params![project_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| anyhow!("unknown project {project_id}"))?;
            if *ready {
                crate::trip::require_project_ready(&transaction, project_id)?;
            }
            let lifecycle = if *ready { "ready" } else { "backlog" };
            let defaults: serde_json::Value = serde_json::from_str(&project_settings)?;
            let id = insert_task_rows(
                &transaction,
                project_id,
                title,
                description,
                acceptance_criteria,
                *priority,
                role_overrides,
                Some(&defaults),
                *ready,
                &now,
            )?;
            let state = if *ready {
                match runtime
                    .ok_or_else(|| {
                        anyhow!("Ready admission requires the service capability runtime")
                    })
                    .and_then(|runtime| {
                        crate::trip::require_task_profiles_activated(&transaction, &id, runtime)
                    }) {
                    Ok(()) => "ready",
                    Err(_) => {
                        transaction.execute(
                            "UPDATE tasks SET lifecycle='backlog',attention='needs_input',ready_at=NULL WHERE id=?1",
                            params![id],
                        )?;
                        "profile_pending"
                    }
                }
            } else {
                lifecycle
            };
            result(operation_id, "task", id, Some(1), state)
        }
        HumanCommand::UpdateTask {
            task_id,
            expected_version,
            title,
            description,
            acceptance_criteria,
            priority,
            manual_order,
            role_overrides,
            ..
        } => {
            validate_task(title, acceptance_criteria)?;
            require_task_state(
                &transaction,
                task_id,
                *expected_version,
                &["backlog", "ready"],
            )?;
            let was_ready: bool = transaction.query_row(
                "SELECT lifecycle='ready' FROM tasks WHERE id=?1",
                params![task_id],
                |row| row.get(0),
            )?;
            if !role_overrides.is_null() && !role_overrides.is_object() {
                bail!("role_overrides must be a role-keyed JSON object")
            }
            let overrides = if role_overrides.is_null()
                || role_overrides.as_object().is_some_and(|map| map.is_empty())
            {
                transaction.query_row(
                    "SELECT role_overrides_json FROM tasks WHERE id=?1",
                    params![task_id],
                    |row| row.get::<_, String>(0),
                )?
            } else {
                serde_json::to_string(role_overrides)?
            };
            transaction.execute(
                "UPDATE tasks SET title=?1,description=?2,acceptance_criteria_json=?3,priority=?4,manual_order=?5,
                 role_overrides_json=?6,version=version+1,updated_at=?7 WHERE id=?8 AND version=?9",
                params![title.trim(), description, serde_json::to_string(acceptance_criteria)?, priority, manual_order,
                    overrides, now, task_id, expected_version],
            )?;
            if let Some(map) = role_overrides.as_object().filter(|map| !map.is_empty()) {
                for (role, value) in map {
                    role.parse::<crate::domain::RoleKind>()
                        .map_err(|error: String| anyhow!(error))?;
                    let config: crate::domain::RoleOverride =
                        serde_json::from_value(value.clone())?;
                    if config.model.trim().is_empty() || config.effort.trim().is_empty() {
                        bail!("role {role} model and effort are required")
                    }
                    let revision:i64=transaction.query_row("SELECT COALESCE(MAX(revision),0)+1 FROM role_settings WHERE task_id=?1 AND role=?2",params![task_id,role],|row|row.get(0))?;
                    transaction.execute("INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![uuid::Uuid::new_v4().to_string(),task_id,role,revision,serde_json::to_string(&config)?,now])?;
                }
            }
            let profile_pending = was_ready
                && runtime
                    .ok_or_else(|| {
                        anyhow!("Ready admission requires the service capability runtime")
                    })
                    .and_then(|runtime| {
                        crate::trip::require_task_profiles_activated(&transaction, task_id, runtime)
                    })
                    .is_err();
            if profile_pending {
                transaction.execute(
                    "UPDATE tasks SET lifecycle='backlog',attention='needs_input',ready_at=NULL WHERE id=?1",
                    params![task_id],
                )?;
            }
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                if profile_pending {
                    "profile_pending"
                } else {
                    "updated"
                },
            )
        }
        HumanCommand::MakeReady {
            task_id,
            expected_version,
            ..
        } => {
            require_task_state(&transaction, task_id, *expected_version, &["backlog"])?;
            let unarchived: bool = transaction.query_row(
                "SELECT archived_at IS NULL FROM tasks WHERE id=?1",
                params![task_id],
                |row| row.get(0),
            )?;
            if !unarchived {
                bail!("archived drafts must be restored before Make Ready")
            }
            let project_id: String = transaction.query_row(
                "SELECT project_id FROM tasks WHERE id=?1",
                params![task_id],
                |row| row.get(0),
            )?;
            crate::trip::require_project_ready(&transaction, &project_id)?;
            crate::recipes::require_binding_current(&transaction, task_id)?;
            let (title, criteria, configured): (String, String, i64) = transaction.query_row(
                "SELECT title,acceptance_criteria_json,
                   (SELECT COUNT(DISTINCT role) FROM role_settings WHERE task_id=tasks.id
                    AND role IN ('manager','explorer','plan_reviewer','implementer','code_reviewer','final_verifier'))
                 FROM tasks WHERE id=?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let criteria: Vec<String> = serde_json::from_str(&criteria)?;
            validate_task(&title, &criteria)?;
            if configured != 6 {
                bail!("Ready tasks require current provider, model, and effort settings for all six app roles")
            }
            crate::trip::require_task_profiles_activated(
                &transaction,
                task_id,
                runtime.ok_or_else(|| {
                    anyhow!("Ready admission requires the service capability runtime")
                })?,
            )?;
            let changed = transaction.execute(
                "UPDATE tasks SET lifecycle='ready',ready_at=?1,attention='none',version=version+1,updated_at=?1
                 WHERE id=?2 AND version=?3 AND lifecycle='backlog' AND archived_at IS NULL",
                params![now, task_id, expected_version],
            )?;
            if changed != 1 {
                bail!("task version or lifecycle changed while becoming Ready")
            }
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "ready",
            )
        }
        HumanCommand::SetQueuePaused {
            project_id,
            expected_version,
            paused,
            ..
        } => {
            let changed = transaction.execute(
                "UPDATE projects SET queue_paused=?1,version=version+1,updated_at=?2 WHERE id=?3 AND version=?4",
                params![paused, now, project_id, expected_version],
            )?;
            if changed != 1 {
                bail!("project version is stale")
            }
            result(
                operation_id,
                "project",
                project_id.clone(),
                Some(expected_version + 1),
                if *paused { "paused" } else { "running" },
            )
        }
        HumanCommand::UpdateProjectSettings {
            project_id,
            expected_version,
            settings,
            ..
        } => {
            if !settings.is_object() {
                bail!("project settings must be a JSON object")
            }
            let changed=transaction.execute("UPDATE projects SET settings_json=?1,version=version+1,updated_at=?2 WHERE id=?3 AND version=?4",params![settings.to_string(),now,project_id,expected_version])?;
            if changed != 1 {
                bail!("project version is stale")
            }
            result(
                operation_id,
                "project",
                project_id.clone(),
                Some(expected_version + 1),
                "settings_updated",
            )
        }
        HumanCommand::UpsertCheckSuite {
            project_id,
            expected_version,
            name,
            position,
            executable,
            arguments,
            timeout_seconds,
            enabled,
            ..
        } => {
            if name.trim().is_empty()
                || !executable.is_absolute()
                || arguments.len() > 64
                || arguments.iter().any(|arg| arg.len() > 4096)
                || !(1..=3600).contains(timeout_seconds)
            {
                bail!("invalid bounded check suite")
            }
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND version=?2)",
                params![project_id, expected_version],
                |row| row.get(0),
            )?;
            if !exists {
                bail!("project version is stale")
            }
            let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE t.project_id=?1 AND a.status NOT IN ('done','cancelled','reworked'))",params![project_id],|row|row.get(0))?;
            if active {
                bail!("check suites cannot change while the project owns an active attempt")
            }
            transaction.execute("INSERT INTO check_suites(id,project_id,name,position,executable,arguments_json,timeout_seconds,enabled,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9) ON CONFLICT(project_id,name) DO UPDATE SET position=excluded.position,executable=excluded.executable,arguments_json=excluded.arguments_json,timeout_seconds=excluded.timeout_seconds,enabled=excluded.enabled,version=check_suites.version+1,updated_at=excluded.updated_at",params![uuid::Uuid::new_v4().to_string(),project_id,name,position,executable.to_string_lossy(),serde_json::to_string(arguments)?,timeout_seconds,*enabled,now])?;
            transaction.execute(
                "UPDATE projects SET version=version+1,updated_at=?1 WHERE id=?2",
                params![now, project_id],
            )?;
            result(
                operation_id,
                "project",
                project_id.clone(),
                Some(expected_version + 1),
                "check_suite_updated",
            )
        }
        HumanCommand::RemoveCheckSuite {
            project_id,
            expected_version,
            name,
            ..
        } => {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND version=?2)",
                params![project_id, expected_version],
                |row| row.get(0),
            )?;
            if !exists {
                bail!("project version is stale")
            }
            let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE t.project_id=?1 AND a.status NOT IN ('done','cancelled','reworked'))",params![project_id],|row|row.get(0))?;
            if active {
                bail!("check suites cannot change while the project owns an active attempt")
            }
            if transaction.execute(
                "DELETE FROM check_suites WHERE project_id=?1 AND name=?2",
                params![project_id, name],
            )? != 1
            {
                bail!("check suite is unknown")
            }
            transaction.execute(
                "UPDATE projects SET version=version+1,updated_at=?1 WHERE id=?2",
                params![now, project_id],
            )?;
            result(
                operation_id,
                "project",
                project_id.clone(),
                Some(expected_version + 1),
                "check_suite_removed",
            )
        }
        HumanCommand::AddDependency {
            task_id,
            depends_on_task_id,
            expected_version,
            ..
        } => {
            require_task_state(
                &transaction,
                task_id,
                *expected_version,
                &["backlog", "ready"],
            )?;
            let same_project: bool = transaction.query_row(
                "SELECT a.project_id=b.project_id FROM tasks a JOIN tasks b ON b.id=?2 WHERE a.id=?1",
                params![task_id, depends_on_task_id], |row| row.get(0),
            )?;
            if !same_project {
                bail!("v1 dependencies must belong to the same project")
            }
            let cycle: bool = transaction.query_row(
                "WITH RECURSIVE downstream(id) AS (SELECT task_id FROM task_dependencies WHERE depends_on_task_id=?1
                 UNION SELECT d.task_id FROM task_dependencies d JOIN downstream x ON d.depends_on_task_id=x.id)
                 SELECT EXISTS(SELECT 1 FROM downstream WHERE id=?2)",
                params![task_id, depends_on_task_id], |row| row.get(0),
            )?;
            if cycle || task_id == depends_on_task_id {
                bail!("dependency would create a cycle")
            }
            transaction.execute("INSERT INTO task_dependencies(task_id,depends_on_task_id,created_at) VALUES(?1,?2,?3)", params![task_id, depends_on_task_id, now])?;
            bump_task(&transaction, task_id, *expected_version, &now)?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "dependency_added",
            )
        }
        HumanCommand::RecordIntegration {
            task_id,
            depends_on_task_id,
            git_ref,
            expected_version,
            ..
        } => {
            require_task_state(
                &transaction,
                task_id,
                *expected_version,
                &["backlog", "ready"],
            )?;
            let (repository_path, manifest_json) = transaction.query_row(
                "SELECT p.repository_path,s.manifest_json FROM tasks t JOIN projects p ON p.id=t.project_id
                 JOIN task_dependencies d ON d.task_id=t.id AND d.depends_on_task_id=?2
                 JOIN attempts a ON a.task_id=d.depends_on_task_id JOIN snapshots s ON s.id=a.accepted_snapshot_id
                 WHERE t.id=?1 ORDER BY a.created_at DESC LIMIT 1",
                params![task_id, depends_on_task_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).optional()?.ok_or_else(|| anyhow!("dependency has no accepted snapshot"))?;
            let repository = crate::workspace::inspect(std::path::Path::new(&repository_path))?;
            let commit = crate::workspace::resolve_commit(&repository, git_ref)?;
            if !crate::workspace::is_ancestor(&repository, &commit, &repository.head)? {
                bail!("integration commit is not reachable from the repository's current HEAD")
            }
            let manifest: crate::snapshot::SnapshotManifest = serde_json::from_str(&manifest_json)?;
            let changed = crate::snapshot::verify_integration(&repository, &manifest, &commit)?;
            let manifest_hash = json_hash(&manifest)?;
            transaction.execute("UPDATE task_dependencies SET integration_ref=?1,integration_commit=?2,verified_at=?3,integration_manifest_hash=?4,verification_json=?5 WHERE task_id=?6 AND depends_on_task_id=?7",
                params![git_ref,commit,now,manifest_hash,serde_json::json!({"changed_paths":changed,"repository_identity":repository.identity,"repository_head":repository.head}).to_string(),task_id,depends_on_task_id])?;
            bump_task(&transaction, task_id, *expected_version, &now)?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "integration_recorded",
            )
        }
        HumanCommand::SetRoleSettings {
            task_id,
            role,
            expected_version,
            config,
            ..
        } => {
            require_task_state(
                &transaction,
                task_id,
                *expected_version,
                &[
                    "backlog",
                    "ready",
                    "in_progress",
                    "validation",
                    "awaiting_review",
                ],
            )?;
            let lifecycle: String = transaction.query_row(
                "SELECT lifecycle FROM tasks WHERE id=?1",
                params![task_id],
                |row| row.get(0),
            )?;
            if config.model.trim().is_empty() || config.effort.trim().is_empty() {
                bail!("role model and effort are required")
            }
            let revision: i64 = transaction.query_row("SELECT COALESCE(MAX(revision),0)+1 FROM role_settings WHERE task_id=?1 AND role=?2",
                params![task_id, role.to_string()], |row| row.get(0))?;
            transaction.execute("INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![uuid::Uuid::new_v4().to_string(), task_id, role.to_string(), revision, serde_json::to_string(config)?, now])?;
            bump_task(&transaction, task_id, *expected_version, &now)?;
            let profile_pending = lifecycle == "ready"
                && runtime
                    .ok_or_else(|| {
                        anyhow!("Ready admission requires the service capability runtime")
                    })
                    .and_then(|runtime| {
                        crate::trip::require_task_profiles_activated(&transaction, task_id, runtime)
                    })
                    .is_err();
            if profile_pending {
                transaction.execute(
                    "UPDATE tasks SET lifecycle='backlog',attention='needs_input',ready_at=NULL WHERE id=?1",
                    params![task_id],
                )?;
            }
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                if profile_pending {
                    "settings_pending_project_activation"
                } else {
                    "settings_pending_next_invocation"
                },
            )
        }
        HumanCommand::ActivateTaskProfile { .. } => {
            bail!("task-profile activation requires the service capability runtime")
        }
        HumanCommand::Control {
            task_id,
            expected_version,
            action,
            payload,
            ..
        } => {
            let (lifecycle, attention, archived): (String, String, bool) = transaction.query_row(
                "SELECT lifecycle,attention,archived_at IS NOT NULL FROM tasks WHERE id=?1 AND version=?2",
                params![task_id,expected_version], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            ).optional()?.ok_or_else(|| anyhow!("task version is stale"))?;
            if archived || matches!(lifecycle.as_str(), "done" | "cancelled") {
                bail!("terminal or archived tasks do not accept workflow controls")
            }
            if ![
                "run_next",
                "pause_after_role",
                "pause_now",
                "continue",
                "continue_manager",
                "stop_manager",
                "change_manager_safe_boundary",
                "change_manager_interrupt",
                "retry",
                "cancel",
                "checkpoint_switch",
            ]
            .contains(&action.as_str())
            {
                bail!("unsupported workflow control {action}")
            }
            if lifecycle == "ready" {
                if !ordinary_control_allowed(
                    &lifecycle, &attention, None, None, None, false, action,
                ) {
                    bail!("a Ready task accepts only Run next before an attempt is claimed")
                }
                transaction.execute(
                    "UPDATE tasks SET attention='run_next_requested',version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
                    params![now,task_id,expected_version],
                )?;
                result(
                    operation_id,
                    "task",
                    task_id.clone(),
                    Some(expected_version + 1),
                    "run_next_requested",
                )
            } else {
                let attempt: Option<String> = transaction
                    .query_row(
                        "SELECT id FROM attempts WHERE task_id=?1 ORDER BY created_at DESC LIMIT 1",
                        params![task_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let attempt =
                    attempt.ok_or_else(|| anyhow!("task has no attempt for control {action}"))?;
                if action == "continue" && pending_continue(&transaction, &attempt, None)? {
                    bail!("a Continue control is already pending for this attempt")
                }
                if action == "continue" && attention == "resume_failed" {
                    if payload.get("resume_rejection").is_some() {
                        let (
                            rejection_event_id,
                            rejected_session_id,
                            rejected_generation_id,
                            rejected_epoch,
                            rejected_resume_count,
                        ) = fresh_resume_rejection_binding(payload)?;
                        let fresh_route_allowed = fresh_route_authorized(
                            &transaction,
                            &rejection_event_id,
                            &rejected_session_id,
                            &rejected_generation_id,
                            &rejected_epoch,
                            rejected_resume_count,
                            &attempt,
                            task_id,
                            None,
                        )?;
                        if !fresh_route_allowed {
                            bail!("continue cannot clear a resume failure without the current exact rejected-session fresh-route authority")
                        }
                        let rejected_role: String = transaction.query_row(
                        "SELECT generation.role FROM sessions session
                         JOIN role_generations generation ON generation.id=session.role_generation_id
                         WHERE session.id=?1 AND generation.id=?2",
                        params![rejected_session_id, rejected_generation_id],
                        |row| row.get(0),
                    )?;
                        let explorer_decision_id: Option<String> = if rejected_role == "explorer" {
                            transaction.query_row(
                                "SELECT id FROM trip_explorer_decisions
                                 WHERE attempt_id=?1 AND role_generation_id=?2 AND activated=1
                                   AND outcome_json IS NULL
                                   AND candidate_hash IS (SELECT candidate_hash FROM attempts WHERE id=?1)",
                                params![attempt, rejected_generation_id],
                                |row| row.get(0),
                            ).optional()?
                        } else {
                            None
                        };
                        transaction.execute(
                        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                         VALUES(?1,?2,'human','session.resume.fresh_route.reserved','session',?3,?4,?5)",
                        params![
                            uuid::Uuid::new_v4().to_string(),
                            operation_id,
                            rejected_session_id,
                            serde_json::json!({
                                "rejection_event_id":rejection_event_id,
                                "role_generation_id":rejected_generation_id,
                                "transcript_epoch":rejected_epoch,
                                "resume_count":rejected_resume_count,
                                "attempt_id":attempt,
                                "explorer_decision_id":explorer_decision_id,
                                "fresh_dispatch":"coordinator",
                            }).to_string(),
                            now,
                        ],
                    )?;
                        if matches!(rejected_role.as_str(), "plan_reviewer" | "code_reviewer") {
                            let changed = transaction.execute(
                                "UPDATE review_requests
                             SET delivery_state='superseded_after_rejected_resume',updated_at=?1
                             WHERE id=(SELECT json_extract(detail_json,'$.review_request_id')
                                       FROM audit_events WHERE id=?2)
                               AND attempt_id=?3 AND role_generation_id=?4 AND session_id=?5
                               AND delivery_state='delivered'",
                                params![
                                    now,
                                    rejection_event_id,
                                    attempt,
                                    rejected_generation_id,
                                    rejected_session_id,
                                ],
                            )?;
                            if changed != 1 {
                                bail!("fresh reviewer route no longer has the exact delivered review request")
                            }
                        } else if rejected_role == "explorer" {
                            let changed = transaction.execute(
                            "UPDATE trip_explorer_decisions
                             SET role_generation_id=NULL
                             WHERE attempt_id=?1 AND role_generation_id=?2 AND activated=1
                               AND outcome_json IS NULL
                               AND candidate_hash IS (SELECT candidate_hash FROM attempts WHERE id=?1)",
                            params![attempt, rejected_generation_id],
                        )?;
                            if changed != 1 {
                                bail!("fresh Explorer route no longer has the exact activated decision")
                            }
                        }
                    } else if !resume_failure_has_current_replacement_authority(
                        &transaction,
                        task_id,
                        &attempt,
                    )? {
                        bail!("continue cannot clear a resume failure without the current exact rejected-session fresh-route authority")
                    }
                }
                let (attempt_status, attempt_phase): (String, String) = transaction.query_row(
                    "SELECT status,phase FROM attempts WHERE id=?1",
                    params![attempt],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let unfinished_rework: Option<(String, bool)> = transaction
                    .query_row(
                        "SELECT state,COALESCE(json_extract(result_json,'$.cancellation_pending'),0)
                         FROM rework_intents WHERE new_attempt_id=?1 AND state NOT IN ('completed','cancelled')",
                        params![attempt],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let manager_control_active: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
                     AND kind IN ('manager_stop','manager_change')
                     AND state NOT IN ('finished','cancelled','superseded','rejected'))",
                    params![attempt],
                    |row| row.get(0),
                )?;
                if let Some((rework_state, cancellation_pending)) = &unfinished_rework {
                    if *cancellation_pending {
                        bail!("pending rework cancellation accepts only the recovery cancellation decision")
                    }
                    let allowed = ordinary_control_allowed(
                        &lifecycle,
                        &attention,
                        Some(&attempt_status),
                        Some(&attempt_phase),
                        Some((rework_state, *cancellation_pending)),
                        manager_control_active,
                        action,
                    );
                    if !allowed {
                        if matches!(
                            action.as_str(),
                            "continue_manager"
                                | "stop_manager"
                                | "change_manager_safe_boundary"
                                | "change_manager_interrupt"
                        ) {
                            bail!("unfinished rework fences manager-only stop and change controls until its exact materialization state is resolved")
                        }
                        bail!("unfinished rework accepts only pause, continue, or cancel controls appropriate to its materialization state")
                    }
                }
                if matches!(
                    action.as_str(),
                    "continue" | "run_next" | "pause_after_role" | "pause_now" | "retry" | "cancel"
                ) && !(action == "continue" && attention == "resume_failed")
                    && !ordinary_control_allowed(
                        &lifecycle,
                        &attention,
                        Some(&attempt_status),
                        Some(&attempt_phase),
                        unfinished_rework
                            .as_ref()
                            .map(|(state, pending)| (state.as_str(), *pending)),
                        manager_control_active,
                        action,
                    )
                    && !(action == "continue"
                        && restart_hold_continue_allowed(
                            &transaction,
                            task_id,
                            &attempt,
                            &lifecycle,
                            &attention,
                            &attempt_status,
                            &attempt_phase,
                            unfinished_rework
                                .as_ref()
                                .map(|(state, pending)| (state.as_str(), *pending)),
                            manager_control_active,
                        )?)
                {
                    bail!("workflow control is not available in the current task and attempt state")
                }
                if matches!(attempt_status.as_str(), "done" | "cancelled" | "failed")
                    && action != "retry"
                {
                    bail!("terminal attempts accept only an explicit retry that creates fresh lineage")
                }
                if action == "stop_manager" {
                    if lifecycle == "awaiting_review" {
                        bail!("awaiting human review preserves final evidence and does not accept manager stop")
                    }
                    let session: String = transaction.query_row(
                        "SELECT s.id FROM role_settings rs JOIN role_generations rg
                         ON rg.id=rs.effective_generation_id JOIN sessions s ON s.role_generation_id=rg.id
                         WHERE rs.task_id=?1 AND rs.role='manager' AND rg.attempt_id=?2
                           AND s.status IN ('launch_reserved','running')
                         ORDER BY s.created_at DESC,s.rowid DESC LIMIT 1",
                        params![task_id, attempt],
                        |row| row.get(0),
                    ).optional()?.ok_or_else(|| anyhow!("Stop manager requires the current manager to have an owned live or reserved session"))?;
                    let other_hold: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
                         AND state NOT IN ('finished','cancelled','superseded','rejected')
                         AND NOT (kind IN ('manager_stop','manager_change') AND state='failed'))",
                        params![attempt],
                        |row| row.get(0),
                    )?;
                    if other_hold {
                        bail!("manager stop does not clear another active workflow hold")
                    }
                    transaction.execute(
                        "UPDATE controls SET state='superseded',updated_at=?1 WHERE attempt_id=?2
                         AND kind IN ('manager_stop','manager_change') AND state='failed'",
                        params![now, attempt],
                    )?;
                    transaction.execute(
                        "INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at,requested_operation_id)
                         VALUES(?1,?2,'manager_stop','held',?3,?4,?5,?5,?6)",
                        params![
                            uuid::Uuid::new_v4().to_string(),
                            attempt,
                            expected_version,
                            serde_json::json!({
                                "manager_session_id":session,
                                "signal_state":"pending",
                                "quiescent":false,
                                "next_action":"Signal only the manager, then wait for a verified process-group exit."
                            }).to_string(),
                            now,
                            operation_id
                        ],
                    )?;
                    bump_task(&transaction, task_id, *expected_version, &now)?;
                    result(
                        operation_id,
                        "task",
                        task_id.clone(),
                        Some(expected_version + 1),
                        "manager_stop_held",
                    )
                } else if matches!(
                    action.as_str(),
                    "change_manager_safe_boundary" | "change_manager_interrupt"
                ) {
                    if lifecycle == "awaiting_review" {
                        bail!("awaiting human review accepts the requested manager configuration only as a future revision; it does not dispatch, resume, replace, or rerun final review")
                    }
                    let settings_revision = requested_manager_revision(payload)?;
                    let current_manager = require_manager_replacement_authority(
                        &transaction,
                        runtime,
                        task_id,
                        &attempt,
                        settings_revision,
                    )?;
                    let failed_signal_recovery: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM switch_intents
                         WHERE old_generation_id=?1 AND role='manager'
                           AND state='recovery_required')",
                        params![current_manager],
                        |row| row.get(0),
                    )?;
                    if failed_signal_recovery
                        && action != "change_manager_interrupt"
                        && !manager_stop_is_quiescent(&transaction, &attempt)?
                    {
                        bail!("the prior manager-only signal delivery failed; a fresh recovery requires the explicit Change manager interrupt action after the exact version check")
                    }
                    let source_stop: Option<String> = transaction.query_row(
                        "SELECT id FROM controls WHERE attempt_id=?1 AND kind='manager_stop' AND state='held'
                         ORDER BY created_at DESC LIMIT 1",
                        params![attempt],
                        |row| row.get(0),
                    ).optional()?;
                    if source_stop.is_some() && !manager_stop_is_quiescent(&transaction, &attempt)?
                    {
                        bail!("change manager waits for the stopped manager's verified process-group exit")
                    }
                    let conflicting_hold: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
                         AND state NOT IN ('finished','cancelled','superseded','rejected')
                         AND NOT ((kind='manager_stop' AND state='held')
                           OR (kind IN ('manager_stop','manager_change') AND state='failed')))",
                        params![attempt],
                        |row| row.get(0),
                    )?;
                    if conflicting_hold {
                        bail!("manager change does not clear another active workflow hold")
                    }
                    transaction.execute(
                        "UPDATE controls SET state='superseded',updated_at=?1 WHERE attempt_id=?2
                         AND kind IN ('manager_stop','manager_change') AND state='failed'",
                        params![now, attempt],
                    )?;
                    let mode = if action == "change_manager_interrupt" {
                        "interrupt"
                    } else {
                        "safe_boundary"
                    };
                    transaction.execute(
                        "INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at,requested_operation_id)
                         VALUES(?1,?2,'manager_change',?3,?4,?5,?6,?6,?7)",
                        params![
                            uuid::Uuid::new_v4().to_string(),
                            attempt,
                            if mode == "interrupt" { "switch_requested" } else { "waiting_safe_boundary" },
                            expected_version,
                            serde_json::json!({
                                "mode":mode,
                                "old_generation_id":current_manager,
                                "settings_revision":settings_revision,
                                "switch_operation_id":uuid::Uuid::new_v4().to_string(),
                                "expected_task_version":expected_version + 1,
                                "source_manager_stop_control_id":source_stop.clone(),
                                "next_action":if mode == "interrupt" {
                                    "Revoke old manager authority, interrupt only that manager, and wait for verified quiescence."
                                } else {
                                    "Wait for the current manager's verified native idle boundary, or a verified process-group exit, before authority transfer."
                                }
                            }).to_string(),
                            now,
                            operation_id
                        ],
                    )?;
                    if let Some(stop_id) = source_stop {
                        transaction.execute(
                            "UPDATE controls SET state='superseded',updated_at=?1 WHERE id=?2 AND kind='manager_stop' AND state='held'",
                            params![now, stop_id],
                        )?;
                    }
                    bump_task(&transaction, task_id, *expected_version, &now)?;
                    result(
                        operation_id,
                        "task",
                        task_id.clone(),
                        Some(expected_version + 1),
                        if mode == "interrupt" {
                            "manager_change_interrupt_requested"
                        } else {
                            "manager_change_waiting_safe_boundary"
                        },
                    )
                } else if action == "continue_manager" {
                    if lifecycle == "awaiting_review" {
                        bail!("awaiting human review does not release manager authority or alter the human gate")
                    }
                    let stop_id: String = transaction.query_row(
                        "SELECT id FROM controls WHERE attempt_id=?1 AND kind='manager_stop' AND state='held'
                         ORDER BY created_at DESC LIMIT 1",
                        params![attempt],
                        |row| row.get(0),
                    ).optional()?.ok_or_else(|| anyhow!("continue manager requires this task's durable manager stop hold"))?;
                    let other_hold: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1 AND id!=?2
                         AND state NOT IN ('finished','cancelled','superseded','rejected'))",
                        params![attempt, stop_id],
                        |row| row.get(0),
                    )?;
                    if other_hold {
                        bail!("continue manager does not clear another active workflow hold")
                    }
                    transaction.execute(
                        "UPDATE controls SET state='cancelled',updated_at=?1 WHERE id=?2 AND kind='manager_stop' AND state='held'",
                        params![now, stop_id],
                    )?;
                    bump_task(&transaction, task_id, *expected_version, &now)?;
                    result(
                        operation_id,
                        "task",
                        task_id.clone(),
                        Some(expected_version + 1),
                        "manager_stop_released",
                    )
                } else {
                    transaction.execute("INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at,requested_operation_id) VALUES(?1,?2,?3,'requested',?4,?5,?6,?6,?7)",
                    params![uuid::Uuid::new_v4().to_string(), attempt, action, expected_version, serde_json::to_string(payload)?, now,operation_id])?;
                    let attention = match action.as_str() {
                        "pause_now" => "pause_requested",
                        "cancel" => "pause_requested",
                        "continue" if attention == "restart_parked" => "restart_parked",
                        _ => "none",
                    };
                    transaction.execute("UPDATE tasks SET attention=?1,version=version+1,updated_at=?2 WHERE id=?3 AND version=?4", params![attention, now, task_id, expected_version])?;
                    result(
                        operation_id,
                        "task",
                        task_id.clone(),
                        Some(expected_version + 1),
                        "control_requested",
                    )
                }
            }
        }
        HumanCommand::ApprovePlan {
            task_id,
            attempt_id,
            expected_version,
            plan_hash,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            crate::trip::require_attempt_ready(&transaction, attempt_id, None)?;
            let approved_review: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_requests WHERE attempt_id=?1 AND review_kind='plan' AND candidate_hash=?2 AND verdict='approved' AND delivery_state='finished')",
                params![attempt_id, plan_hash], |row| row.get(0),
            )?;
            if !approved_review {
                bail!("human plan approval requires a finished independent plan-review approval")
            }
            let structured_plan: String = transaction.query_row(
                "SELECT id FROM trip_structured_plans WHERE attempt_id=?1 AND plan_hash=?2
                 AND reviewed_at IS NOT NULL AND approved_at IS NULL",
                params![attempt_id, plan_hash], |row| row.get(0),
            ).optional()?.ok_or_else(|| anyhow!("human plan approval requires the exact independently reviewed structured plan"))?;
            let changed = transaction.execute(
                "UPDATE attempts SET phase='awaiting_implementation_authorization',plan_approved_at=?1,updated_at=?1 WHERE id=?2 AND task_id=?3 AND phase='awaiting_plan_approval' AND plan_hash=?4 AND structured_plan_id=?5",
                params![now, attempt_id, task_id, plan_hash, structured_plan],
            )?;
            if changed != 1 {
                bail!("plan approval is stale or plan hash does not match")
            }
            transaction.execute(
                "UPDATE trip_structured_plans SET approved_at=?1 WHERE id=?2 AND approved_at IS NULL",
                params![now, structured_plan],
            )?;
            bump_task(&transaction, task_id, *expected_version, &now)?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "plan_approved",
            )
        }
        HumanCommand::ReviewVerdict {
            task_id,
            attempt_id,
            expected_version,
            review_kind,
            candidate_hash,
            verdict,
            feedback,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            crate::trip::require_attempt_ready(&transaction, attempt_id, None)?;
            if !["plan", "code", "final"].contains(&review_kind.as_str())
                || !["approved", "request_changes", "needs_rework"].contains(&verdict.as_str())
            {
                bail!("invalid review kind or verdict")
            }
            if verdict != "approved" && feedback.trim().is_empty() {
                bail!("review feedback is required")
            }
            let expected_hash: Option<String> = transaction.query_row("SELECT CASE WHEN ?1='plan' THEN plan_hash ELSE candidate_hash END FROM attempts WHERE id=?2 AND task_id=?3",
                params![review_kind, attempt_id, task_id], |row| row.get(0))?;
            if expected_hash.as_deref() != Some(candidate_hash) {
                bail!("review candidate drifted or hash is stale")
            }
            let expected_role = match review_kind.as_str() {
                "plan" => "plan_reviewer",
                "code" => "code_reviewer",
                "final" => "final_verifier",
                _ => unreachable!(),
            };
            let (request_id, generation, session): (String, String, String) = transaction.query_row(
                "SELECT r.id,r.role_generation_id,r.session_id FROM review_requests r JOIN role_settings rs ON rs.effective_generation_id=r.role_generation_id AND rs.role=?4 WHERE r.attempt_id=?1 AND r.review_kind=?2 AND r.candidate_hash=?3 AND r.delivery_state='delivered'",
                params![attempt_id,review_kind,candidate_hash,expected_role],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            ).optional()?.ok_or_else(||anyhow!("current delivered review request is missing"))?;
            let result_id: String = transaction.query_row(
                "SELECT rr.id FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id
                 JOIN sessions s ON s.id=rr.session_id AND s.role_generation_id=rg.id
                 WHERE rg.id=?1 AND rg.attempt_id=?2 AND rg.role=?3 AND rr.session_id=?4
                   AND rr.outcome=?5 AND json_extract(rr.metadata_json,'$.candidate_hash')=?6
                   AND json_extract(rr.metadata_json,'$.review_kind')=?7 AND rr.consumed_at IS NULL
                   AND json_extract(rr.metadata_json,'$.review_request_id')=?8
                 ORDER BY rr.created_at LIMIT 1",
                params![generation,attempt_id,expected_role,session,verdict,candidate_hash,review_kind,request_id],
                |row| row.get(0),
            ).optional()?.ok_or_else(||anyhow!("review transition requires the current request's authenticated structured role result"))?;
            if review_kind == "final" && verdict == "request_changes" {
                let repair_allowed:bool=transaction.query_row(
                    "SELECT a.final_repair_round=0 AND EXISTS(SELECT 1 FROM review_requests code
                       WHERE code.attempt_id=a.id AND code.review_kind='code' AND code.candidate_hash=a.candidate_hash
                         AND code.verdict='approved' AND code.delivery_state='finished')
                     FROM attempts a WHERE a.id=?1",
                    params![attempt_id],|row|row.get(0)
                )?;
                if !repair_allowed {
                    bail!("final request_changes exceeds the one dedicated repair cycle or lacks retained code approval")
                }
                transaction.execute(
                    "UPDATE attempts SET final_repair_round=1 WHERE id=?1 AND final_repair_round=0",
                    params![attempt_id],
                )?;
            }
            if transaction.execute("UPDATE review_requests SET delivery_state='finished',verdict=?1,feedback=?2,updated_at=?3 WHERE id=?4 AND delivery_state='delivered'",
                params![verdict,feedback,now,request_id])? != 1 {
                bail!("current delivered review request changed before its verdict was applied")
            }
            apply_review_transition(&transaction, attempt_id, review_kind, verdict, &now)?;
            if review_kind == "plan" && verdict == "approved" {
                transaction.execute(
                    "UPDATE trip_structured_plans SET review_request_id=?1,reviewed_at=?2
                     WHERE attempt_id=?3 AND plan_hash=?4 AND reviewed_at IS NULL",
                    params![request_id, now, attempt_id, candidate_hash],
                )?;
            }
            if review_kind == "plan" && verdict != "approved" {
                crate::store::record_plan_rejection(
                    &transaction,
                    attempt_id,
                    &request_id,
                    candidate_hash,
                    verdict,
                    feedback,
                    &now,
                )?;
            }
            if transaction.execute(
                "UPDATE role_results SET consumed_at=?1 WHERE id=?2 AND consumed_at IS NULL",
                params![now, result_id],
            )? != 1
            {
                bail!("current review result changed before its verdict was applied")
            }
            if transaction.execute(
                "UPDATE tasks SET version=version+1,
                 attention=CASE
                   WHEN ?1!='needs_rework' THEN attention
                   WHEN attention IN ('paused','pause_requested','needs_recovery') THEN attention
                   ELSE 'needs_input'
                 END,
                 updated_at=?2 WHERE id=?3 AND version=?4",
                params![verdict, now, task_id, expected_version],
            )? != 1
            {
                bail!("task version is stale")
            }
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                verdict,
            )
        }
        HumanCommand::ExtendReviewBudget {
            task_id,
            attempt_id,
            expected_version,
            review_kind,
            additional,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            if !matches!(review_kind.as_str(), "plan" | "code") || *additional <= 0 {
                bail!("only plan and code review budgets accept positive explicit extensions")
            }
            let current_attempt: bool = transaction.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
                   WHERE a.id=?1 AND a.task_id=?2 AND t.archived_at IS NULL
                     AND t.lifecycle IN ('in_progress','validation','awaiting_review')
                     AND a.status IN ('running','held','needs_input')
                     AND NOT EXISTS(
                       SELECT 1 FROM attempts newer
                       WHERE newer.task_id=a.task_id
                         AND (newer.created_at>a.created_at OR (newer.created_at=a.created_at AND newer.id>a.id))
                     )
                 )",
                params![attempt_id, task_id],
                |row| row.get(0),
            )?;
            if !current_attempt {
                bail!("review budget extensions require the current live task attempt")
            }
            let changed = transaction.execute("UPDATE review_budgets SET extension_allowance=extension_allowance+?1 WHERE attempt_id=?2 AND review_kind=?3 AND initial_allowance+extension_allowance+?1<=5",
                params![additional, attempt_id, review_kind])?;
            if changed != 1 {
                bail!("review budget is missing or the five-call cap would be exceeded")
            }
            bump_task(&transaction, task_id, *expected_version, &now)?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "review_budget_extended",
            )
        }
        HumanCommand::HumanReview {
            task_id,
            attempt_id,
            expected_version,
            decision,
            feedback,
            carry_plan_approval,
            ..
        } => {
            require_task_state(
                &transaction,
                task_id,
                *expected_version,
                &["awaiting_review"],
            )?;
            crate::trip::require_attempt_ready(&transaction, attempt_id, None)?;
            match decision.as_str() {
                "accept" => {
                    let (snapshot,candidate_hash,attempt_status): (Option<String>,Option<String>,String) = transaction.query_row(
                        "SELECT accepted_snapshot_id,candidate_hash,status FROM attempts WHERE id=?1 AND task_id=?2",
                        params![attempt_id, task_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                    )?;
                    if !matches!(attempt_status.as_str(), "running" | "held") {
                        bail!(
                            "accept requires a live running or held attempt without unresolved blockers"
                        )
                    }
                    let Some(snapshot_id) = snapshot else {
                        bail!("accept requires an immutable accepted snapshot")
                    };
                    let snapshot_matches: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE id=?1 AND attempt_id=?2 AND complete=1 AND manifest_hash=?3)",
                        params![snapshot_id,attempt_id,candidate_hash],|row|row.get(0),
                    )?;
                    let final_gate: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM review_requests WHERE attempt_id=?1 AND review_kind='final' AND candidate_hash=?2 AND verdict='approved' AND delivery_state='finished')",params![attempt_id,candidate_hash],|row|row.get(0))?;
                    let checks_ok: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM trip_selected_checks s JOIN attempts a ON a.id=s.attempt_id
                           WHERE s.attempt_id=?1 AND s.revision=a.selected_checks_revision AND s.required=1)
                         AND NOT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND candidate_hash=?2 AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))
                         AND NOT EXISTS(SELECT 1 FROM trip_selected_checks s JOIN attempts a ON a.id=s.attempt_id
                           WHERE s.attempt_id=?1 AND s.revision=a.selected_checks_revision AND s.required=1
                             AND NOT EXISTS(SELECT 1 FROM check_runs cr WHERE cr.attempt_id=?1 AND cr.candidate_hash=?2
                               AND cr.check_id=s.check_id AND cr.selected_check_revision=s.revision AND cr.status='finished'
                               AND cr.exit_code=0 AND cr.freshness_state='current'))",
                        params![attempt_id,candidate_hash],|row|row.get(0),
                    )?;
                    let conformance_ok:bool=transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN trip_conformance_receipts c
                           ON c.attempt_id=a.id AND c.revision=a.manager_conformance_revision
                           AND c.candidate_hash=a.candidate_hash WHERE a.id=?1)
                         AND NOT EXISTS(SELECT 1 FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state!='yielded')",
                        params![attempt_id],|row|row.get(0)
                    )?;
                    let handoff: bool = transaction.query_row(
                        "SELECT phase='awaiting_human_review' FROM attempts WHERE id=?1",
                        params![attempt_id],
                        |row| row.get(0),
                    )?;
                    if !(snapshot_matches && final_gate && checks_ok && conformance_ok && handoff) {
                        bail!("accept requires the exact immutable snapshot, final review, selected-check matrix, current manager conformance, yielded lanes, and manager handoff evidence")
                    }
                    transaction.execute("UPDATE tasks SET lifecycle='done',attention='none',version=version+1,updated_at=?1 WHERE id=?2", params![now, task_id])?;
                    transaction.execute(
                        "UPDATE attempts SET status='done',human_acceptance_at=?1,updated_at=?1 WHERE id=?2",
                        params![now, attempt_id],
                    )?;
                    transaction.execute("UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id IN (SELECT id FROM role_generations WHERE attempt_id=?2) AND revoked_at IS NULL",params![now,attempt_id])?;
                    transaction.execute("UPDATE role_generations SET status='stopping',updated_at=?1 WHERE attempt_id=?2 AND status IN ('launch_reserved','running')",params![now,attempt_id])?;
                    transaction.execute("UPDATE claims SET state='complete',updated_at=?1 WHERE attempt_id=?2 AND state='running'",params![now,attempt_id])?;
                    result(
                        operation_id,
                        "task",
                        task_id.clone(),
                        Some(expected_version + 1),
                        "accepted",
                    )
                }
                "request_changes" => {
                    if feedback.trim().is_empty() {
                        bail!("request changes feedback cannot be blank")
                    }
                    let (base, config, plan_hash, plan_approved,snapshot,scope_hash,configuration_hash): (String,i64,Option<String>,Option<String>,Option<String>,String,String) = transaction.query_row(
                        "SELECT base_revision,configuration_revision,plan_hash,plan_approved_at,accepted_snapshot_id,scope_hash,configuration_hash FROM attempts WHERE id=?1 AND task_id=?2",
                        params![attempt_id, task_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)))?;
                    let snapshot = snapshot.ok_or_else(|| {
                        anyhow!("rework requires the exact reviewed accepted snapshot")
                    })?;
                    let task_scope: (String, String, String) = transaction.query_row(
                        "SELECT title,description,acceptance_criteria_json FROM tasks WHERE id=?1",
                        params![task_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?;
                    let current_scope = json_hash(
                        &serde_json::json!({"title":task_scope.0,"description":task_scope.1,"acceptance_criteria":serde_json::from_str::<serde_json::Value>(&task_scope.2)?,"base_revision":base}),
                    )?;
                    let configurations = {
                        let mut statement=transaction.prepare("SELECT role,revision,config_json FROM role_settings r WHERE task_id=?1 AND revision=(SELECT MAX(revision) FROM role_settings WHERE task_id=r.task_id AND role=r.role) ORDER BY role")?;
                        let rows = statement
                            .query_map(params![task_id], |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, i64>(1)?,
                                    row.get::<_, String>(2)?,
                                ))
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows
                    };
                    let current_configuration = json_hash(&configurations)?;
                    let current_configuration_revision = configurations
                        .iter()
                        .map(|(_, revision, _)| *revision)
                        .max()
                        .unwrap_or(config);
                    let carry = *carry_plan_approval
                        && plan_hash.is_some()
                        && plan_approved.is_some()
                        && scope_hash == current_scope
                        && configuration_hash == current_configuration;
                    if *carry_plan_approval && !carry {
                        bail!("plan approval cannot carry because task scope or role configuration changed")
                    }
                    let new_attempt = uuid::Uuid::new_v4().to_string();
                    let phase = if carry { "implementation" } else { "planning" };
                    let carried_plan_hash = if carry { plan_hash.as_deref() } else { None };
                    transaction.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at,parent_attempt_id,plan_hash,plan_approved_at,accepted_snapshot_id,scope_hash,configuration_hash,workflow_version,workflow_hash,upstream_source_hash,overlay_hash,legacy_migration_required)
                        VALUES(?1,?2,?3,?4,?5,?6,'materialization_pending',?7,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,0)",
                        params![new_attempt, task_id, uuid::Uuid::new_v4().to_string(), phase, base, current_configuration_revision, now, attempt_id,
                            carried_plan_hash, carry.then_some(now.as_str()), snapshot,current_scope,current_configuration,crate::workflow_resources::WORKFLOW_VERSION,crate::workflow_resources::workflow_hash(),crate::trip::source_hash(),crate::trip::overlay_hash()])?;
                    transaction.execute("INSERT INTO trip_attempt_profiles(attempt_id,role,settings_revision,activation_id,source,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_id,capability_key,capability_proof_hash,bound_at)
                        SELECT ?1,role,settings_revision,activation_id,source,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_id,capability_key,capability_proof_hash,?2 FROM trip_attempt_profiles WHERE attempt_id=?3",
                        params![new_attempt,now,attempt_id])?;
                    for (kind, allowance) in [("plan", 2), ("code", 2), ("final", 1)] {
                        transaction.execute("INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance) VALUES(?1,?2,?3,?4)",
                            params![uuid::Uuid::new_v4().to_string(), new_attempt, kind, allowance])?;
                    }
                    transaction.execute("UPDATE tasks SET lifecycle='in_progress',attention='none',version=version+1,description=description,updated_at=?1 WHERE id=?2", params![now, task_id])?;
                    transaction.execute(
                        "UPDATE attempts SET status='rework_staging',updated_at=?1 WHERE id=?2",
                        params![now, attempt_id],
                    )?;
                    transaction.execute("UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id IN (SELECT id FROM role_generations WHERE attempt_id=?2) AND revoked_at IS NULL",params![now,attempt_id])?;
                    transaction.execute("UPDATE role_generations SET status='stopping',updated_at=?1 WHERE attempt_id=?2 AND status IN ('launch_reserved','running')",params![now,attempt_id])?;
                    transaction.execute("INSERT INTO rework_intents(id,operation_id,parent_attempt_id,new_attempt_id,snapshot_id,feedback,carry_plan_approval,scope_hash,configuration_hash,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'reserved',?10,?10)",params![uuid::Uuid::new_v4().to_string(),operation_id,attempt_id,new_attempt,snapshot,feedback,carry,current_scope,current_configuration,now])?;
                    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                        VALUES(?1,?2,'human','rework.lineage.created','attempt',?3,?4,?5)", params![uuid::Uuid::new_v4().to_string(), operation_id, new_attempt,
                            serde_json::json!({"parent_attempt_id":attempt_id,"feedback":feedback,"carry_plan_approval":carry}).to_string(), now])?;
                    result(
                        operation_id,
                        "attempt",
                        new_attempt,
                        Some(expected_version + 1),
                        "rework_created",
                    )
                }
                _ => bail!("human review decision must be accept or request_changes"),
            }
        }
        HumanCommand::Guidance {
            task_id,
            role_generation_id,
            expected_version,
            body,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            if body.trim().is_empty() {
                bail!("guidance cannot be blank")
            }
            let attempt: String = transaction.query_row("SELECT rg.attempt_id FROM role_generations rg JOIN attempts a ON a.id=rg.attempt_id WHERE rg.id=?1 AND a.task_id=?2 AND rg.role='manager' AND rg.status='running'", params![role_generation_id,task_id], |row| row.get(0))
                .optional()?.ok_or_else(||anyhow!("guidance targets only the current running manager generation"))?;
            transaction.execute("INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,reason,created_at) VALUES(?1,?2,?3,?4,'queued','awaiting_supported_idle_boundary',?5)",
                params![uuid::Uuid::new_v4().to_string(), attempt, role_generation_id, body, now])?;
            bump_task(&transaction, task_id, *expected_version, &now)?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "guidance_queued",
            )
        }
        HumanCommand::ApplyTransition {
            task_id,
            proposal_id,
            expected_version,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            let (attempt,target,payload,manager_safe): (String,String,String,bool) = transaction.query_row(
                "SELECT c.attempt_id,json_extract(c.payload_json,'$.phase'),c.payload_json,
                   EXISTS(SELECT 1 FROM sessions s WHERE s.role_generation_id=c.role_generation_id AND (s.status='exited' OR (s.status='running' AND s.readiness_state='idle_candidate')))
                 FROM controls c JOIN attempts a ON a.id=c.attempt_id JOIN tasks t ON t.id=a.task_id
                 JOIN role_generations rg ON rg.id=c.role_generation_id
                 JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager'
                 WHERE c.id=?1 AND a.task_id=?2 AND c.kind='transition_proposal' AND c.state='proposed'
                   AND a.status='running'
                   AND c.expected_version=t.version AND json_extract(c.payload_json,'$.expected_task_version')=t.version",
                params![proposal_id,task_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional()?.ok_or_else(||anyhow!("transition proposal is stale"))?;
            crate::trip::require_attempt_ready(&transaction, &attempt, None)?;
            if !manager_safe {
                bail!("transition waits for a proven manager turn boundary or process quiescence")
            }
            let evidence: serde_json::Value = serde_json::from_str(&payload)?;
            if evidence
                .get("evidence")
                .and_then(|value| value.as_array())
                .is_none_or(Vec::is_empty)
            {
                bail!("transition proposal has no structured evidence")
            }
            let current: (String, Option<String>, Option<String>) = transaction.query_row(
                "SELECT phase,plan_hash,candidate_hash FROM attempts WHERE id=?1",
                params![attempt],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if evidence
                .get("source_phase")
                .and_then(|value| value.as_str())
                != Some(&current.0)
                || evidence.get("plan_hash")
                    != Some(
                        &current
                            .1
                            .clone()
                            .map_or(serde_json::Value::Null, serde_json::Value::String),
                    )
                || evidence.get("candidate_hash")
                    != Some(
                        &current
                            .2
                            .clone()
                            .map_or(serde_json::Value::Null, serde_json::Value::String),
                    )
            {
                bail!("transition proposal no longer matches the exact attempt phase and candidate")
            }
            match target.as_str() {
                "plan_review" if current.0 == "planning" && current.1.is_some() => {}
                "code_review" if current.0 == "implementation" && current.2.is_some() => {}
                "final_review" if current.0 == "checks" && current.2.is_some() => {
                    let checks_ok:bool=transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM trip_selected_checks s JOIN attempts a ON a.id=s.attempt_id
                           WHERE s.attempt_id=?1 AND s.revision=a.selected_checks_revision AND s.required=1)
                         AND NOT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND candidate_hash=?2 AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))
                         AND NOT EXISTS(SELECT 1 FROM trip_selected_checks s JOIN attempts a ON a.id=s.attempt_id
                           WHERE s.attempt_id=?1 AND s.revision=a.selected_checks_revision AND s.required=1
                             AND NOT EXISTS(SELECT 1 FROM check_runs cr WHERE cr.attempt_id=?1 AND cr.candidate_hash=?2
                               AND cr.check_id=s.check_id AND cr.selected_check_revision=s.revision AND cr.status='finished'
                               AND cr.exit_code=0 AND cr.freshness_state='current'))
                         AND EXISTS(SELECT 1 FROM attempts a JOIN trip_conformance_receipts c ON c.attempt_id=a.id
                           AND c.revision=a.manager_conformance_revision AND c.candidate_hash=a.candidate_hash WHERE a.id=?1)
                         AND EXISTS(SELECT 1 FROM trip_explorer_decisions d WHERE d.attempt_id=?1 AND d.stage='final'
                           AND d.candidate_hash=?2 AND (d.activated=0 OR d.outcome_json IS NOT NULL))
                         AND NOT EXISTS(SELECT 1 FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state!='yielded')",
                        params![attempt,current.2],|row|row.get(0))?;
                    if !checks_ok {
                        bail!("final review requires successful checks for the exact candidate")
                    }
                }
                "human_review" if current.0 == "manager_handoff" && current.2.is_some() => {
                    let ready:bool=transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM review_requests WHERE attempt_id=?1 AND review_kind='final' AND candidate_hash=?2 AND verdict='approved' AND delivery_state='finished')
                         AND EXISTS(SELECT 1 FROM snapshots s JOIN attempts a ON a.accepted_snapshot_id=s.id WHERE a.id=?1 AND s.manifest_hash=?2 AND s.complete=1)",
                        params![attempt,current.2],|row|row.get(0))?;
                    if !ready {
                        bail!("human review requires final approval and exact accepted snapshot")
                    }
                }
                "implementation" => {
                    bail!("implementation begins only through explicit human plan approval")
                }
                "checks" => bail!("checks begin only through an authenticated code-review verdict"),
                _ => bail!("transition is not valid from the current attempt phase"),
            }
            let persisted_phase = if target == "human_review" {
                "awaiting_human_review"
            } else {
                target.as_str()
            };
            transaction.execute(
                "UPDATE attempts SET phase=?1,updated_at=?2 WHERE id=?3",
                params![persisted_phase, now, attempt],
            )?;
            transaction.execute("UPDATE controls SET state='finished',updated_at=?1 WHERE id=?2 AND state='proposed'",params![now,proposal_id])?;
            if target == "human_review" {
                transaction.execute("UPDATE tasks SET lifecycle='awaiting_review',attention='needs_human_review' WHERE id=?1",params![task_id])?;
            }
            bump_task(&transaction, task_id, *expected_version, &now)?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                &format!("transitioned_{target}"),
            )
        }
        HumanCommand::ResolveRecovery {
            task_id,
            attempt_id,
            recovery_id,
            session_id,
            expected_version,
            decision,
            evidence,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            validate_recovery_identity(
                &transaction,
                recovery_id,
                task_id,
                attempt_id,
                session_id.as_deref(),
            )?;
            if evidence.trim().is_empty() || evidence.len() > 64 * 1024 {
                bail!("bounded recovery evidence is required")
            }
            let belongs:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND task_id=?2 AND status IN ('needs_recovery','running'))",params![attempt_id,task_id],|row|row.get(0))?;
            if !belongs {
                bail!("attempt is not awaiting recovery")
            }
            let rework = rework_recovery(&transaction, attempt_id)?;
            if rework.as_ref().is_some_and(|(_, pending)| *pending) && decision != "cancel" {
                bail!("a requested rework cancellation must be completed with the cancel recovery decision")
            }
            if rework.is_some() && decision == "confirm_quiescent" {
                bail!("failed rework recovery must retry materialization or cancel the rework lineage")
            }
            match decision.as_str() {
                "confirm_quiescent" => {
                    let verified = recovery_evidence
                        .as_ref()
                        .ok_or_else(|| anyhow!("recorded process verification is required"))?;
                    if let Some(session) = session_id.as_deref() {
                        let (record,resume_authority):(String,Option<String>)=transaction.query_row(
                            "SELECT recovery.id,
                                    CASE WHEN permit.purpose IN ('setup_discovery','profile_probe','runtime_probe')
                                           AND permit.state='issued'
                                           AND ((permit.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
                                             OR (permit.purpose='profile_probe' AND s.validation_cell='trip_setup_probe')
                                             OR (permit.purpose='runtime_probe' AND s.validation_cell='trip_runtime_probe'))
                                         THEN json_object('recovery_record_id',recovery.id,'session_id',s.id,
                                           'attempt_id',rg.attempt_id,'role_generation_id',rg.id,
                                           'setup_permit_id',permit.id,'setup_operation_id',permit.setup_operation_id,
                                           'transcript_epoch',s.transcript_epoch,'resume_count',s.resume_count,
                                           'validation_cell',s.validation_cell,'purpose',permit.purpose)
                                         ELSE NULL END
                             FROM recovery_records recovery
                             JOIN sessions s ON s.id=recovery.session_id
                             JOIN role_generations rg ON rg.id=s.role_generation_id
                             LEFT JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
                             WHERE recovery.id=?1 AND recovery.session_id=?2 AND recovery.attempt_id=?3
                               AND recovery.state='attention_required'
                               AND s.status IN ('recovery_required','exited')
                             ",
                            params![recovery_id,session,attempt_id],|row|Ok((row.get(0)?,row.get(1)?))
                        ).optional()?.ok_or_else(||anyhow!("no unresolved recovery record matches the session"))?;
                        let recovery_patch = serde_json::json!({
                            "human_annotation":evidence,
                            "verification":verified,
                            "resolved_resume_authority":resume_authority
                                .and_then(|value|serde_json::from_str::<serde_json::Value>(&value).ok())
                        });
                        transaction.execute("UPDATE recovery_records SET state='resolved_quiescent',detail_json=json_patch(detail_json,?1),resolved_at=?2,updated_at=?2 WHERE id=?3",params![recovery_patch.to_string(),now,record])?;
                        transaction.execute("UPDATE sessions SET status='exited',readiness_state='unknown',exit_json=?1,updated_at=?2 WHERE id=?3 AND status IN ('recovery_required','exited')",params![serde_json::json!({"process_group_quiescent":true,"source":"recorded_process_inventory_reconciled","verification":verified,"human_annotation":evidence}).to_string(),now,session])?;
                        park_resolved_admission(
                            &transaction,
                            session,
                            attempt_id,
                            recovery_id,
                            &now,
                        )?;
                        transaction.execute("UPDATE role_generations SET status='exited',updated_at=?1 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",params![now,session])?;
                        transaction.execute("UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id=(SELECT role_generation_id FROM sessions WHERE id=?2) AND revoked_at IS NULL",params![now,session])?;
                        transaction.execute("UPDATE review_requests SET delivery_state='abandoned',ambiguity_state='human_abandoned_after_verified_quiescence',updated_at=?1 WHERE session_id=?2 AND attempt_id=?3 AND delivery_state='ambiguous'",params![now,session,attempt_id])?;
                    } else {
                        let (record,kind,subject):(String,String,String)=transaction.query_row(
                            "SELECT id,COALESCE(json_extract(detail_json,'$.kind'),'check'),
                                    COALESCE(json_extract(detail_json,'$.check_id'),
                                             json_extract(detail_json,'$.claim_id'),
                                             json_extract(detail_json,'$.freeze_id'))
                             FROM recovery_records
                             WHERE id=?1 AND session_id IS NULL AND attempt_id=?2 AND state='attention_required'
                               AND (json_extract(detail_json,'$.check_id') IS NOT NULL
                                 OR json_extract(detail_json,'$.kind') IN ('database_restore_claim','database_restore_freeze'))
                             ",
                            params![recovery_id,attempt_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))
                        ).optional()?.ok_or_else(||anyhow!("no unresolved check, claim, or freeze recovery record matches the attempt"))?;
                        transaction.execute("UPDATE recovery_records SET state='resolved_quiescent',detail_json=json_patch(detail_json,?1),resolved_at=?2,updated_at=?2 WHERE id=?3",params![serde_json::json!({"human_annotation":evidence,"verification":verified,"reconciled_id":&subject}).to_string(),now,record])?;
                        match kind.as_str() {
                            "database_restore_claim" => {
                                transaction.execute("UPDATE claims SET state='cancelled',updated_at=?1 WHERE id=?2 AND state IN ('reserved','launching','running','unknown','stopping')",params![now,subject])?;
                            }
                            "database_restore_freeze" => {
                                transaction.execute("UPDATE freeze_intents SET state='abandoned',error='database restore recovery verified quiescence',updated_at=?1 WHERE id=?2 AND state='recovery_required'",params![now,subject])?;
                            }
                            _ => {
                                transaction.execute("UPDATE check_runs SET status='reconciled_interrupted',finished_at=?1,evidence_json=?2 WHERE id=?3 AND status='recovery_required'",params![now,serde_json::json!({"verification":verified,"human_annotation":evidence}).to_string(),subject])?;
                            }
                        }
                    }
                    let restore_hold_active: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id='database-restore-hold' AND state='attention_required')",
                        [],
                        |row| row.get(0),
                    )?;
                    if restore_hold_active {
                        transaction.execute("UPDATE claims SET state='cancelled',updated_at=?1 WHERE attempt_id=?2 AND state IN ('reserved','launching','running','unknown','stopping')",params![now,attempt_id])?;
                    }
                    let remaining:i64=transaction.query_row("SELECT COUNT(*) FROM recovery_records WHERE attempt_id=?1 AND state='attention_required'",params![attempt_id],|row|row.get(0))?;
                    let return_to_validation_hold: bool = transaction.query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM sessions s
                           JOIN role_generations rg ON rg.id=s.role_generation_id
                           JOIN attempts a ON a.id=rg.attempt_id
                           JOIN tasks t ON t.id=a.task_id
                           WHERE a.id=?1 AND s.validation_cell IS NOT NULL
                             AND t.lifecycle='validation'
                         )",
                        params![attempt_id],
                        |row| row.get(0),
                    )?;
                    let explicit_restart_hold: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM restart_candidates rc
                           JOIN sessions s ON s.id=rc.session_id
                           LEFT JOIN recovery_records r ON r.id=json_extract(rc.result_json,'$.startup_admission_reconciliation.recovery_id')
                           WHERE rc.attempt_id=?1 AND rc.state NOT IN ('resumed','released_fresh_dispatch','cancelled')
                             AND ((rc.state='parked' AND COALESCE(json_extract(rc.result_json,'$.native_resume_forbidden'),0)=1
                                   AND json_extract(rc.result_json,'$.startup_admission_reconciliation.recovery_id') IS NULL)
                               OR (r.session_id=rc.session_id AND r.attempt_id=rc.attempt_id
                                   AND r.state='resolved_quiescent'
                                   AND json_extract(rc.result_json,'$.startup_admission_reconciliation.role_generation_id')=s.role_generation_id
                                   AND NOT EXISTS(SELECT 1 FROM recovery_records newer WHERE newer.session_id=r.session_id
                                     AND newer.id!=r.id AND (newer.created_at>r.created_at OR
                                       (newer.created_at=r.created_at AND newer.rowid>r.rowid))))))",
                        params![attempt_id],|row|row.get(0)
                    )?;
                    let active_restored_lane: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
                         WHERE rc.attempt_id=?1 AND rc.state='resumed' AND s.status='running')",
                        params![attempt_id],|row|row.get(0)
                    )?;
                    let park_restart = explicit_restart_hold
                        && !active_restored_lane
                        && !return_to_validation_hold
                        && !restore_hold_active;
                    if remaining == 0 {
                        transaction.execute(
                            "UPDATE attempts SET status=CASE WHEN ?1 OR ?2 THEN 'held' WHEN ?3 THEN 'restart_parked' ELSE 'running' END,updated_at=?4 WHERE id=?5",
                            params![return_to_validation_hold, restore_hold_active, park_restart, now, attempt_id],
                        )?;
                        if !restore_hold_active {
                            transaction.execute("UPDATE claims SET state='running',updated_at=?1 WHERE attempt_id=?2 AND state='unknown'",params![now,attempt_id])?;
                        }
                    }
                    transaction.execute("UPDATE tasks SET attention=CASE WHEN ?1!=0 THEN 'needs_recovery' WHEN ?2 OR ?3 THEN 'paused' WHEN ?4 THEN 'restart_parked' ELSE 'none' END,version=version+1,updated_at=?5 WHERE id=?6",params![remaining,return_to_validation_hold,restore_hold_active,park_restart,now,task_id])?;
                    if remaining == 0 && !restore_hold_active && !explicit_restart_hold {
                        Store::release_restart_hold_for_fresh_dispatch_in(
                            &transaction,
                            attempt_id,
                            "human_recovery",
                            &now,
                        )?;
                    }
                    result(
                        operation_id,
                        "attempt",
                        attempt_id.clone(),
                        Some(expected_version + 1),
                        if remaining == 0 {
                            "recovery_quiescence_confirmed"
                        } else {
                            "recovery_identity_confirmed_more_pending"
                        },
                    )
                }
                "cancel" => {
                    if let Some((parent, _)) = rework {
                        let verified = recovery_evidence.as_ref().ok_or_else(|| {
                            anyhow!(
                                "rework cancellation requires verified parent and child ownership"
                            )
                        })?;
                        if verified["kind"] != "rework_cancellation" {
                            bail!("rework cancellation verification has the wrong scope")
                        }
                        let verified_attempts =
                            serde_json::from_value::<Vec<String>>(verified["attempt_ids"].clone())?;
                        let verified_ownership =
                            serde_json::from_value::<Vec<String>>(verified["ownership"].clone())?;
                        let (current_ownership, current_attempts) =
                            rework_recovery_ownership(&transaction, attempt_id, &parent)?;
                        if verified_attempts != current_attempts
                            || verified_ownership != current_ownership
                        {
                            bail!("rework cancellation ownership changed during verification")
                        }
                        transaction.execute("UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id IN (SELECT role_generation_id FROM sessions WHERE id IN (SELECT session_id FROM recovery_records WHERE attempt_id IN (?2,?3) AND session_id IS NOT NULL)) AND revoked_at IS NULL",params![now,attempt_id,parent])?;
                        transaction.execute("UPDATE role_generations SET status='exited',updated_at=?1 WHERE id IN (SELECT role_generation_id FROM sessions WHERE id IN (SELECT session_id FROM recovery_records WHERE attempt_id IN (?2,?3) AND session_id IS NOT NULL))",params![now,attempt_id,parent])?;
                        transaction.execute("UPDATE sessions SET status='exited',readiness_state='unknown',exit_json=?1,updated_at=?2 WHERE id IN (SELECT session_id FROM recovery_records WHERE attempt_id IN (?3,?4) AND session_id IS NOT NULL) AND status='recovery_required'",params![serde_json::json!({"process_group_quiescent":true,"source":"rework_recovery_cancel_after_recorded_inventory","verification":verified,"human_evidence":evidence}).to_string(),now,attempt_id,parent])?;
                        transaction.execute("UPDATE review_requests SET delivery_state='abandoned',ambiguity_state='human_abandoned_after_verified_quiescence',updated_at=?1 WHERE session_id IN (SELECT session_id FROM recovery_records WHERE attempt_id IN (?2,?3) AND session_id IS NOT NULL) AND delivery_state='ambiguous'",params![now,attempt_id,parent])?;
                        transaction.execute("UPDATE check_runs SET status='reconciled_cancelled',finished_at=?1 WHERE id IN (SELECT json_extract(detail_json,'$.check_id') FROM recovery_records WHERE attempt_id IN (?2,?3) AND session_id IS NULL)",params![now,attempt_id,parent])?;
                        transaction.execute("UPDATE recovery_records SET state='resolved_cancelled',detail_json=json_patch(detail_json,?1),resolved_at=?2,updated_at=?2 WHERE attempt_id IN (?3,?4) AND state='attention_required'",params![serde_json::json!({"human_evidence":evidence,"verification":verified}).to_string(),now,attempt_id,parent])?;
                        let changed = transaction.execute(
                            "UPDATE rework_intents SET state='cancelled',result_json=json_patch(result_json,?1),updated_at=?2 WHERE new_attempt_id=?3 AND state='recovery_required'",
                            params![serde_json::json!({"cancellation_pending":false,"cancelled_after_verified_quiescence":true,"verification":verified}).to_string(),now,attempt_id],
                        )?;
                        if changed != 1 {
                            bail!("rework recovery changed before cancellation completed")
                        }
                        transaction.execute(
                            "UPDATE workspaces SET state='cancelled',updated_at=?1 WHERE attempt_id IN (?2,?3)",
                            params![now, attempt_id, parent],
                        )?;
                        transaction.execute(
                            "UPDATE attempts SET status='cancelled',updated_at=?1 WHERE id IN (?2,?3)",
                            params![now, attempt_id, parent],
                        )?;
                        transaction.execute(
                            "UPDATE claims SET state='cancelled',updated_at=?1 WHERE attempt_id IN (?2,?3)",
                            params![now, attempt_id, parent],
                        )?;
                        transaction.execute(
                            "UPDATE controls SET state='cancelled',updated_at=?1 WHERE attempt_id IN (?2,?3) AND state IN ('requested','draining','proposed','recovery_required')",
                            params![now, attempt_id, parent],
                        )?;
                    } else {
                        let process_recovery: bool = transaction.query_row(
                            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1 AND state='attention_required' AND (session_id IS NOT NULL OR json_extract(detail_json,'$.check_id') IS NOT NULL))",
                            params![attempt_id], |row| row.get(0),
                        )?;
                        if process_recovery && recovery_evidence.is_none() {
                            bail!("process recovery cancellation requires verified quiescence")
                        }
                        if process_recovery {
                            let verified = recovery_evidence.as_ref().ok_or_else(|| {
                                anyhow!(
                                    "process recovery cancellation requires verified quiescence"
                                )
                            })?;
                            if verified["kind"] != "attempt_cancellation" {
                                bail!("attempt recovery cancellation verification has the wrong scope")
                            }
                            let verified_attempts = serde_json::from_value::<Vec<String>>(
                                verified["attempt_ids"].clone(),
                            )?;
                            let verified_ownership = serde_json::from_value::<Vec<String>>(
                                verified["ownership"].clone(),
                            )?;
                            let (current_ownership, current_attempts) =
                                rework_recovery_ownership(&transaction, attempt_id, attempt_id)?;
                            if verified_attempts != current_attempts
                                || verified_ownership != current_ownership
                            {
                                bail!("attempt recovery ownership changed during verification")
                            }
                        }
                        let verified_recovery = recovery_evidence.as_ref();
                        transaction.execute("UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id IN (SELECT role_generation_id FROM sessions WHERE id IN (SELECT session_id FROM recovery_records WHERE attempt_id=?2 AND session_id IS NOT NULL)) AND revoked_at IS NULL",params![now,attempt_id])?;
                        transaction.execute("UPDATE role_generations SET status='exited',updated_at=?1 WHERE id IN (SELECT role_generation_id FROM sessions WHERE id IN (SELECT session_id FROM recovery_records WHERE attempt_id=?2 AND session_id IS NOT NULL))",params![now,attempt_id])?;
                        transaction.execute("UPDATE sessions SET status='exited',readiness_state='unknown',exit_json=?1,updated_at=?2 WHERE id IN (SELECT session_id FROM recovery_records WHERE attempt_id=?3 AND session_id IS NOT NULL) AND status='recovery_required'",params![serde_json::json!({"process_group_quiescent":true,"source":"attempt_recovery_cancel_after_recorded_inventory","verification":verified_recovery}).to_string(),now,attempt_id])?;
                        transaction.execute("UPDATE check_runs SET status='reconciled_cancelled',finished_at=?1 WHERE id IN (SELECT json_extract(detail_json,'$.check_id') FROM recovery_records WHERE attempt_id=?2 AND session_id IS NULL)",params![now,attempt_id])?;
                        transaction.execute("UPDATE recovery_records SET state='resolved_cancelled',detail_json=json_patch(detail_json,?1),resolved_at=?2,updated_at=?2 WHERE attempt_id=?3 AND state='attention_required'",params![serde_json::json!({"human_evidence":evidence,"verification":verified_recovery}).to_string(),now,attempt_id])?;
                        transaction.execute(
                            "UPDATE attempts SET status='cancelled',updated_at=?1 WHERE id=?2",
                            params![now, attempt_id],
                        )?;
                        transaction.execute(
                            "UPDATE claims SET state='cancelled',updated_at=?1 WHERE attempt_id=?2",
                            params![now, attempt_id],
                        )?;
                        transaction.execute(
                            "UPDATE controls SET state='cancelled',updated_at=?1 WHERE attempt_id=?2 AND state IN ('requested','draining','proposed','recovery_required')",
                            params![now, attempt_id],
                        )?;
                    }
                    transaction.execute("UPDATE tasks SET lifecycle='cancelled',attention='none',version=version+1,updated_at=?1 WHERE id=?2",params![now,task_id])?;
                    result(
                        operation_id,
                        "attempt",
                        attempt_id.clone(),
                        Some(expected_version + 1),
                        "recovery_cancelled",
                    )
                }
                "retry_materialization" => {
                    let changed=transaction.execute("UPDATE rework_intents SET state='reserved',result_json=?1,updated_at=?2 WHERE new_attempt_id=?3 AND state='recovery_required' AND COALESCE(json_extract(result_json,'$.cancellation_pending'),0)=0 AND EXISTS(SELECT 1 FROM tasks WHERE id=?4 AND lifecycle='in_progress' AND attention='needs_recovery')",params![serde_json::json!({"human_evidence":evidence}).to_string(),now,attempt_id,task_id])?;
                    if changed != 1 {
                        bail!("attempt has no failed rework materialization")
                    }
                    transaction.execute("UPDATE attempts SET status='materialization_pending',updated_at=?1 WHERE id=?2",params![now,attempt_id])?;
                    transaction.execute("UPDATE tasks SET attention='none',version=version+1,updated_at=?1 WHERE id=?2",params![now,task_id])?;
                    result(
                        operation_id,
                        "attempt",
                        attempt_id.clone(),
                        Some(expected_version + 1),
                        "materialization_retry_reserved",
                    )
                }
                _ => bail!(
                    "recovery decision must be confirm_quiescent, retry_materialization, or cancel"
                ),
            }
        }
        HumanCommand::NormalizeLegacyTask {
            task_id,
            expected_version,
            ..
        } => {
            let normalized: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM tasks t WHERE t.id=?1 AND t.version=?2
                     AND t.lifecycle IN ('backlog','in_progress','awaiting_review')
                     AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=t.id)
                     AND COALESCE(json_extract(t.legacy_json,'$.source_status'),
                         json_extract(t.legacy_json,'$.frontmatter.status')) IN ('in_progress','implemented')
                     AND json_extract(t.legacy_json,'$.llmrelay_normalization.state') IS NULL
                 )",
                params![task_id, expected_version],
                |row| row.get(0),
            )?;
            if !normalized {
                bail!("legacy task is stale, has attempt lineage, or has no supported nonterminal source status")
            }
            transaction.execute(
                "UPDATE tasks SET lifecycle='backlog',attention='needs_input',ready_at=NULL,
                        legacy_json=json_set(COALESCE(legacy_json,'{}'),
                          '$.llmrelay_normalization',
                          json_object('state','normalized','normalized_at',?1)),
                        version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
                params![now, task_id, expected_version],
            )?;
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "legacy_task_normalized",
            )
        }
        HumanCommand::RetryWorkspaceReservation { .. }
        | HumanCommand::CancelWorkspaceReservation { .. }
        | HumanCommand::RetryGracefulStop { .. }
        | HumanCommand::ForceStopExactProcess { .. } => {
            bail!("this exact recovery command requires the application runtime authority")
        }
        HumanCommand::Archive {
            task_id,
            expected_version,
            ..
        } => {
            let changed = transaction.execute(
                "UPDATE tasks SET archived_at=?1,version=version+1,updated_at=?1
                 WHERE id=?2 AND version=?3 AND archived_at IS NULL
                   AND (lifecycle='done' OR (lifecycle='backlog' AND ready_at IS NULL
                     AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=?2)))",
                params![now, task_id, expected_version],
            )?;
            if changed != 1 {
                bail!("only a completed task or a never attempted backlog draft can be archived at its current version")
            }
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "archived",
            )
        }
        HumanCommand::Restore {
            task_id,
            expected_version,
            ..
        } => {
            task_version(&transaction, task_id, *expected_version)?;
            let changed = transaction.execute("UPDATE tasks SET archived_at=NULL,version=version+1,updated_at=?1 WHERE id=?2 AND archived_at IS NOT NULL", params![now, task_id])?;
            if changed != 1 {
                bail!("task is not archived")
            }
            result(
                operation_id,
                "task",
                task_id.clone(),
                Some(expected_version + 1),
                "restored",
            )
        }
        HumanCommand::SetAutoResume {
            expected_version,
            enabled,
            ..
        } => {
            let changed = transaction.execute(
                "UPDATE instance_settings SET auto_resume_eligible=?1,version=version+1,updated_at=?2
                 WHERE singleton=1 AND version=?3",
                params![enabled, now, expected_version],
            )?;
            if changed != 1 {
                bail!("instance settings version is stale")
            }
            result(
                operation_id,
                "instance_settings",
                "local".to_owned(),
                Some(expected_version + 1),
                if *enabled {
                    "auto_resume_enabled"
                } else {
                    "auto_resume_disabled"
                },
            )
        }
        HumanCommand::DecidePermission {
            request_id,
            expected_revision,
            decision,
            lifetime,
            reason,
            ..
        } => crate::permissions::apply_human_decision(
            &transaction,
            operation_id,
            request_id,
            *expected_revision,
            *decision,
            *lifetime,
            reason,
        )?,
        HumanCommand::RevokePermissionRule {
            rule_id,
            expected_revision,
            reason,
            ..
        } => crate::permissions::revoke_rule(
            &transaction,
            operation_id,
            rule_id,
            *expected_revision,
            reason,
        )?,
    };
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,new_version,detail_json,created_at)
        VALUES(?1,?2,'human','human.command.applied',?3,?4,?5,?6,?7)", params![uuid::Uuid::new_v4().to_string(), operation_id,
            result.entity_kind, result.entity_id, result.version, serde_json::to_string(&result)?, now])?;
    transaction.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
        VALUES(?1,'human_control','human_command',?2,?3,?4)", params![operation_id, request_hash, serde_json::to_string(&result)?, now])?;
    transaction.commit()?;
    Ok(result)
}

pub fn role_preparations(
    store: &Store,
    task_id: &str,
    hooks: &crate::providers::HookAssets,
    role_socket: &std::path::Path,
    executable: &std::path::Path,
) -> Result<Vec<serde_json::Value>> {
    let requested = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT rs.role,rs.revision,rs.config_json,
                    COALESCE((SELECT w.path FROM attempts a JOIN workspaces w ON w.attempt_id=a.id
                      WHERE a.task_id=t.id AND w.state='ready' ORDER BY a.created_at DESC LIMIT 1),
                      p.repository_path)
             FROM tasks t JOIN projects p ON p.id=t.project_id
             JOIN role_settings rs ON rs.task_id=t.id
             WHERE t.id=?1 AND rs.revision=(SELECT MAX(current.revision)
               FROM role_settings current WHERE current.task_id=rs.task_id AND current.role=rs.role)
             ORDER BY rs.role",
        )?;
        let rows = statement
            .query_map(params![task_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if requested.is_empty() {
        let connection = store.lock()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
            params![task_id],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("task does not exist")
        }
    }
    let mut preparations = Vec::with_capacity(requested.len());
    for (role_text, revision, config_json, cwd) in requested {
        let role = role_text
            .parse::<crate::domain::RoleKind>()
            .map_err(|error| anyhow!(error))?;
        let config: crate::domain::RoleOverride = serde_json::from_str(&config_json)?;
        let provider = config.provider;
        let prepared = crate::providers::prepare_role_launch_with_bundles(
            provider,
            role,
            &config.model,
            &config.effort,
            std::path::Path::new(&cwd),
            "current role capability preparation",
            role_socket,
            "normalized-preparation-token",
            "normalized-preparation-generation",
            "normalized-preparation-session",
            None,
            hooks,
            executable,
            &store.compatibility_bundles,
        );
        match prepared {
            Ok(prepared) => {
                let capability_key = match crate::providers::capability_key(&prepared.config) {
                    Ok(key) => key,
                    Err(error) => {
                        preparations.push(serde_json::json!({
                            "task_id":task_id,
                            "role":role,
                            "requested_revision":revision,
                            "config":config,
                            "status":"unsupported",
                            "reason":format!("Current preparation identity failed: {error:#}"),
                        }));
                        continue;
                    }
                };
                let supported = {
                    let connection = store.lock()?;
                    connection.query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM capabilities current_capability
                            WHERE current_capability.rowid=(
                                SELECT latest_capability.rowid FROM capabilities latest_capability
                                WHERE latest_capability.provider=?1
                                  AND latest_capability.executable_version=?2
                                  AND latest_capability.role=?3
                                  AND latest_capability.mode='interactive_pty'
                                ORDER BY latest_capability.checked_at DESC,
                                         latest_capability.rowid DESC LIMIT 1)
                              AND current_capability.config_hash=?4
                              AND current_capability.status='supported'
                              AND current_capability.proof_json!='{}')",
                        params![
                            provider.to_string(),
                            prepared.config.executable_version,
                            role.to_string(),
                            capability_key,
                        ],
                        |row| row.get::<_, bool>(0),
                    )?
                };
                let authority = {
                    let connection = store.lock()?;
                    crate::trip::task_profile_preparation_authority_with_bundles(
                        &connection,
                        task_id,
                        role,
                        revision,
                        &prepared.config,
                        &store.compatibility_bundles,
                    )
                };
                let runtime_admission = {
                    let connection = store.lock()?;
                    let source = authority
                        .as_ref()
                        .ok()
                        .map(|value| value.descriptor.source.as_str())
                        .unwrap_or("task_override");
                    connection.query_row(
                        "SELECT json_object('id',a.id,'scope_hash',a.scope_hash,'state',a.state,'fresh_call_count',a.fresh_call_count,'failure_reason',a.failure_reason,'failure_category',(SELECT json_extract(rr.metadata_json,'$.validation_observation.failure_category') FROM role_results rr WHERE rr.session_id=p.session_id AND rr.outcome='capability_observed' ORDER BY rr.created_at DESC LIMIT 1),'probe_state',p.state,'session_id',p.session_id,'session_status',s.status,'readiness',s.readiness_state,'hook_trust',s.hook_trust_state,'has_native_session',s.native_session_id IS NOT NULL)
                         FROM trip_runtime_probes p JOIN trip_runtime_admissions a ON a.id=p.admission_id
                         LEFT JOIN sessions s ON s.id=p.session_id
                         WHERE a.project_id=(SELECT project_id FROM tasks WHERE id=?1)
                           AND ((?4='task_override' AND a.task_id=?1) OR (?4='project_default' AND a.task_id IS NULL))
                           AND p.role=?2 AND p.capability_key=?3 ORDER BY a.created_at DESC LIMIT 1",
                        params![task_id,role.to_string(),capability_key,source],
                        |row|row.get::<_,String>(0),
                    ).optional()?.and_then(|value|serde_json::from_str::<serde_json::Value>(&value).ok())
                };
                let generic_supported = supported;
                let exact_runtime_authority = authority
                    .as_ref()
                    .is_ok_and(|value| value.exact_runtime_authority);
                let exact_reason = authority
                    .as_ref()
                    .err()
                    .map(|error| format!("{error:#}"))
                    .or_else(|| {
                        authority
                            .as_ref()
                            .ok()
                            .and_then(|value| value.exact_runtime_reason.clone())
                    });
                preparations.push(serde_json::json!({
                    "task_id":task_id,
                    "role":role,
                    "requested_revision":revision,
                    "config":config,
                    "capability_key":capability_key,
                    "generic_capability_supported":generic_supported,
                    "exact_runtime_authority":exact_runtime_authority,
                    "exact_runtime_reason":exact_reason,
                    "task_profile_activated":authority.as_ref().ok().is_some_and(|value|value.task_profile_activated),
                    "task_profile_reason":authority.as_ref().ok().and_then(|value|value.task_profile_reason.as_deref()),
                    "task_profile_source":authority.as_ref().ok().map(|value|value.descriptor.source.as_str()),
                    "adapter":authority.as_ref().ok().map(|value|value.descriptor.adapter_name.as_str()),
                    "runtime_admission":runtime_admission,
                    "compatibility":prepared.config.compatibility.as_ref().map(|binding|
                        crate::provider_compatibility::matched_explanation(binding, generic_supported)),
                    "status":if exact_runtime_authority { "supported" } else { "unverified" },
                    "reason":if exact_runtime_authority {
                        "Exact current runtime-scoped authority is supported"
                    } else if generic_supported {
                        "The generic prepared tuple is Supported, but exact current scoped authority is missing; it is not a usable replacement until corrected"
                    } else if provider == crate::domain::Provider::Codex && role == crate::domain::RoleKind::Implementer {
                        crate::providers::codex::IMPLEMENTER_NATIVE_POLICY_REASON
                    } else {
                        "Exact current prepared capability requires validation"
                    },
                }));
            }
            Err(error) => preparations.push(serde_json::json!({
                "task_id":task_id,
                "role":role,
                "requested_revision":revision,
                "config":config,
                "status":"unsupported",
                "compatibility":error.chain().find_map(|cause| cause.downcast_ref::<crate::provider_compatibility::CompatibilityError>())
                    .map(crate::provider_compatibility::CompatibilityError::explanation),
                "reason":format!("Current preparation failed: {error:#}"),
            })),
        }
    }
    Ok(preparations)
}

fn selected_setup_capability<'a>(
    selection: &serde_json::Value,
    receipts: &[serde_json::Value],
    capabilities: &'a [serde_json::Value],
) -> Option<&'a serde_json::Value> {
    let exact_receipt = receipts
        .iter()
        .filter(|receipt| {
            receipt.get("role") == selection.get("role")
                && receipt.get("provider") == selection.pointer("/profile/provider")
                && receipt.get("profile_hash") == selection.get("profile_hash")
        })
        .max_by_key(|receipt| {
            receipt
                .get("created_at")
                .and_then(serde_json::Value::as_str)
        })?;
    if exact_receipt
        .get("result")
        .and_then(serde_json::Value::as_str)
        != Some("success")
    {
        return None;
    }
    let key = exact_receipt.get("capability_key")?;
    capabilities.iter().find(|capability| {
        capability.get("config_hash") == Some(key)
            && capability.get("provider") == selection.pointer("/profile/provider")
            && capability.get("role") == selection.get("role")
            && capabilities
                .iter()
                .filter(|other| {
                    other.get("provider") == capability.get("provider")
                        && other.get("version") == capability.get("version")
                        && other.get("role") == capability.get("role")
                        && other.get("mode") == capability.get("mode")
                })
                .all(|other| {
                    (
                        other.get("checked_at").and_then(serde_json::Value::as_str),
                        other.get("rowid").and_then(serde_json::Value::as_i64),
                    ) <= (
                        capability
                            .get("checked_at")
                            .and_then(serde_json::Value::as_str),
                        capability.get("rowid").and_then(serde_json::Value::as_i64),
                    )
                })
    })
}

#[cfg(test)]
mod m7_setup_projection_tests {
    use super::{
        continuation_actions, permanent_resume_rejection_is_current, selected_setup_capability,
    };
    use crate::domain::TaskDto;
    use serde_json::{json, Value};

    #[test]
    fn exact_selected_profile_ignores_unrelated_and_newer_failed_evidence() {
        let selected =
            json!({"role":"explorer","profile":{"provider":"codex"},"profile_hash":"selected"});
        let other =
            json!({"role":"plan_reviewer","profile":{"provider":"codex"},"profile_hash":"other"});
        let receipts = vec![
            json!({"role":"explorer","provider":"codex","profile_hash":"selected","result":"success","capability_key":"selected-key","created_at":"2026-01-01"}),
            json!({"role":"plan_reviewer","provider":"codex","profile_hash":"other","result":"success","capability_key":"other-key","created_at":"2026-01-02"}),
        ];
        let capabilities = vec![
            json!({"rowid":1,"provider":"codex","version":"v1","role":"explorer","mode":"interactive_pty","config_hash":"selected-key","checked_at":"2026-01-01","compatibility":{"status":"matched"}}),
            json!({"rowid":2,"provider":"codex","version":"v2","role":"explorer","mode":"interactive_pty","config_hash":"unrelated-key","checked_at":"2026-01-03","compatibility":{"status":"unknown_version"}}),
            json!({"rowid":3,"provider":"codex","version":"v1","role":"plan_reviewer","mode":"interactive_pty","config_hash":"other-key","checked_at":"2026-01-02","compatibility":{"status":"matched"}}),
        ];
        assert_eq!(
            selected_setup_capability(&selected, &receipts, &capabilities).unwrap()["config_hash"],
            "selected-key"
        );
        assert_eq!(
            selected_setup_capability(&other, &receipts, &capabilities).unwrap()["config_hash"],
            "other-key"
        );
        let newer_same_scope = json!({"rowid":4,"provider":"codex","version":"v1","role":"explorer","mode":"interactive_pty","config_hash":"new-key","checked_at":"2026-01-04","compatibility":{"status":"evidence_stale"}});
        let mut superseded = capabilities.clone();
        superseded.push(newer_same_scope);
        assert!(selected_setup_capability(&selected, &receipts, &superseded).is_none());
        assert!(selected_setup_capability(&other, &receipts, &superseded).is_some());
        let mut failed = receipts.clone();
        failed.push(json!({"role":"explorer","provider":"codex","profile_hash":"selected","result":"failure","capability_key":"selected-key","created_at":"2026-01-05"}));
        assert!(selected_setup_capability(&selected, &failed, &capabilities).is_none());
        assert!(selected_setup_capability(&other, &failed, &capabilities).is_some());
        let wrong_profile: Value =
            json!({"role":"explorer","profile":{"provider":"codex"},"profile_hash":"new-profile"});
        assert!(selected_setup_capability(&wrong_profile, &receipts, &capabilities).is_none());
    }

    #[test]
    fn invalid_manifest_rejection_retires_only_for_newer_exact_proof() {
        let task = TaskDto {
            id: "task".into(),
            project_id: "project".into(),
            title: String::new(),
            description: String::new(),
            acceptance_criteria: vec![],
            priority: 0,
            manual_order: 0,
            lifecycle: "active".into(),
            attention: "active".into(),
            version: 1,
            archived: false,
            can_archive: false,
            recipe_provenance: None,
            permission_waiting: false,
            role_overrides: json!({}),
            dependencies: vec![],
            active_attempt: Some(json!({"id":"attempt"})),
            role_settings: vec![
                json!({"role":"manager","revision":1,"effective_generation_id":"generation","config":{"provider":"codex","model":"model","effort":"high"}}),
            ],
            reviews: vec![],
            snapshots: vec![],
            review_budgets: vec![],
            legacy: json!({}),
        };
        let session = json!({"id":"session","task_id":"task","attempt_id":"attempt","role":"manager","role_generation_id":"generation","generation_status":"exited","config_revision":1,"provider":"codex","status":"exited","resume_count":0,"transcript_epoch":"epoch","capability_current":false});
        let rejection = json!({"category":"provider_compatibility_invalid_manifest"});
        let binding = crate::provider_compatibility::BundleSet::embedded()
            .resolve(
                crate::domain::Provider::Codex,
                crate::provider_compatibility::CODEX_EXACT_VERSION,
                crate::domain::RoleKind::Manager,
            )
            .unwrap();
        let proof = json!({"compatibility":crate::provider_compatibility::AuthorityBinding::from(&binding),"model":"model","effort":"high"});
        let exact = json!({"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,"role":"manager","mode":"interactive_pty","config_hash":"new-key","status":"supported","checked_at":"2026-01-03","proof":proof});
        let unrelated = json!({"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,"role":"explorer","mode":"interactive_pty","config_hash":"other-key","status":"supported","checked_at":"2026-01-04","proof":proof});
        assert!(permanent_resume_rejection_is_current(
            &[task.clone()],
            &[],
            &[unrelated.clone()],
            Some("2026-01-02"),
            &rejection,
            &session
        ));
        assert!(permanent_resume_rejection_is_current(
            &[task.clone()],
            &[],
            &[exact.clone()],
            Some("2026-01-04"),
            &rejection,
            &session
        ));
        assert!(!permanent_resume_rejection_is_current(
            &[task.clone()],
            &[],
            &[unrelated.clone(), exact.clone()],
            Some("2026-01-02"),
            &rejection,
            &session
        ));
        let newer_unverified = json!({"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,"role":"manager","mode":"interactive_pty","config_hash":"newest-key","status":"unverified","checked_at":"2026-01-05","proof":{}});
        assert!(permanent_resume_rejection_is_current(
            &[task.clone()],
            &[],
            &[exact.clone(), newer_unverified],
            Some("2026-01-02"),
            &rejection,
            &session
        ));
        let detail = json!({"category":"provider_compatibility_invalid_manifest","role_generation_id":"generation","transcript_epoch":"epoch","resume_count":0,"attempt_id":"attempt","role":"manager","config_revision":1});
        let history = vec![
            json!({"id":"rejection","event_code":"session.resume.rejected","entity_id":"session","created_at":"2026-01-02","detail":detail}),
        ];
        let decision_actions = |evidence: &[Value]| {
            continuation_actions(
                &[task.clone()],
                evidence,
                &[session.clone()],
                &[],
                &[],
                &[],
                &history,
                &[],
                &[],
                &[],
                &[],
                &[],
            )
        };
        let held = decision_actions(&[unrelated.clone()]);
        assert!(held
            .iter()
            .any(|action| action.binding["session_id"] == "session"
                && action.operation == "terminal_incomplete"
                && !action.enabled));
        let released = decision_actions(&[unrelated, exact]);
        assert!(!released
            .iter()
            .any(|action| action.binding["session_id"] == "session"
                && action.operation == "terminal_incomplete"));
    }
}

pub fn state(store: &Store) -> Result<AppStateDto> {
    let connection = store.lock()?;
    // Read before any projected row: without a read transaction, an external
    // writer can only make the rows newer than this cursor, never older.
    let revision = crate::store::read_state_revision(&connection)
        .context("read state revision for the dashboard projection")?;
    let projects = {
        let mut statement = connection.prepare("SELECT p.id,p.display_name,p.repository_path,p.repository_identity,p.base_revision,p.queue_paused,p.version,p.settings_json,
            COALESCE((SELECT json_object('readiness',s.readiness,'reason',s.reason,'detected_installation',s.detected_installation,'detected',json(s.detected_json),'setup_operation_id',s.setup_operation_id,'active_config_revision_id',s.active_config_revision_id,'workflow_id',s.workflow_id,'package_version',s.package_version,'upstream_source_hash',s.upstream_source_hash,'overlay_hash',s.overlay_hash,'manifest_hash',s.manifest_hash,'activated_at',s.activated_at,'updated_at',s.updated_at,
                'configuration',(SELECT json_object('id',r.id,'revision',r.revision,'state',r.state,'configuration_hash',r.configuration_hash,'config',json(r.config_json),'adapters',json(r.adapters_json),'preflight',json_object('status','recorded','receipt_count',json_array_length(r.preflight_json)),'verification',json(r.verification_json),'created_at',r.created_at,'activated_at',r.activated_at) FROM trip_config_revisions r WHERE r.id=s.active_config_revision_id AND r.project_id=p.id)) FROM trip_project_state s WHERE s.project_id=p.id),'{}')
            FROM projects p WHERE p.internal_purpose IS NULL ORDER BY p.display_name")?;
        let rows = statement
            .query_map([], |row| {
                Ok(ProjectDto {
                    id: row.get(0)?,
                    display_name: row.get(1)?,
                    repository_path: row.get::<_, String>(2)?.into(),
                    repository_identity: row.get::<_, String>(3)?.into(),
                    base_revision: row.get(4)?,
                    queue_paused: row.get(5)?,
                    version: row.get(6)?,
                    settings: parse(row.get(7)?),
                    trip: parse(row.get(8)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let task_ids = {
        let mut statement = connection
            .prepare("SELECT t.id FROM tasks t JOIN projects p ON p.id=t.project_id WHERE p.internal_purpose IS NULL ORDER BY t.priority DESC,t.manual_order,t.created_at")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut tasks = Vec::new();
    for id in task_ids {
        let mut task = connection.query_row("SELECT id,project_id,title,description,acceptance_criteria_json,priority,manual_order,lifecycle,attention,version,archived_at,role_overrides_json,legacy_json,EXISTS(SELECT 1 FROM permission_requests pr WHERE pr.task_id=tasks.id AND pr.state='pending'),(archived_at IS NULL AND (lifecycle='done' OR (lifecycle='backlog' AND ready_at IS NULL AND NOT EXISTS(SELECT 1 FROM attempts WHERE task_id=tasks.id)))) FROM tasks WHERE id=?1",
            params![id], |row| Ok(TaskDto { id:row.get(0)?,project_id:row.get(1)?,title:row.get(2)?,description:row.get(3)?,acceptance_criteria:serde_json::from_str(&row.get::<_,String>(4)?).unwrap_or_default(),priority:row.get(5)?,manual_order:row.get(6)?,lifecycle:row.get(7)?,attention:row.get(8)?,version:row.get(9)?,archived:row.get::<_,Option<String>>(10)?.is_some(),can_archive:row.get(14)?,recipe_provenance:None,role_overrides:parse(row.get(11)?),legacy:parse(row.get(12)?),permission_waiting:row.get(13)?,dependencies:vec![],active_attempt:None,role_settings:vec![],reviews:vec![],snapshots:vec![],review_budgets:vec![] }))?;
        task.recipe_provenance = crate::recipes::task_provenance(&connection, &id)?;
        task.dependencies = json_rows(&connection, "SELECT json_object('task_id',depends_on_task_id,'integration_ref',integration_ref,'verified_at',verified_at) FROM task_dependencies WHERE task_id=?1", &id)?;
        task.active_attempt = connection
            .query_row(
                "SELECT json_object(
            'id',a.id,'phase',a.phase,'status',a.status,'base_revision',a.base_revision,
            'plan_hash',a.plan_hash,'plan',(
                SELECT json_extract(rr.metadata_json,'$.plan') FROM snapshots s
                JOIN role_results rr ON rr.id=json_extract(s.manifest_json,'$.role_result_id')
                WHERE s.attempt_id=a.id AND s.kind='plan' AND s.complete=1
                  AND s.manifest_hash=a.plan_hash AND json_type(rr.metadata_json,'$.plan')='text'
                ORDER BY s.created_at DESC LIMIT 1
            ),
            'candidate_hash',a.candidate_hash,'accepted_snapshot_id',a.accepted_snapshot_id,
            'parent_attempt_id',a.parent_attempt_id,'workflow_version',a.workflow_version,
            'workflow_hash',a.workflow_hash,'structured_plan_id',a.structured_plan_id,
            'config_revision_id',(SELECT active_config_revision_id FROM trip_project_state
                WHERE project_id=t.project_id),
            'selected_checks_revision',a.selected_checks_revision,
            'manager_conformance_revision',a.manager_conformance_revision,
            'final_repair_round',a.final_repair_round,
            'legacy_migration_required',a.legacy_migration_required,
            'human_acceptance_at',a.human_acceptance_at
        ) FROM attempts a JOIN tasks t ON t.id=a.task_id
          WHERE a.task_id=?1 ORDER BY a.created_at DESC LIMIT 1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(parse);
        task.role_settings = json_rows(&connection, "SELECT json_object('id',rs.id,'role',rs.role,'revision',rs.revision,'config',json(rs.config_json),'effective_generation_id',rs.effective_generation_id,'activation',(SELECT json_object('id',a.id,'source',CASE WHEN a.profile_json=json(rs.config_json) THEN 'task_override' ELSE 'task_override' END,'profile_hash',a.profile_hash,'project_config_revision_id',a.project_config_revision_id,'project_configuration_hash',a.project_configuration_hash,'adapter',a.adapter_name,'adapter_hash',a.adapter_hash,'capability_id',a.capability_id,'capability_key',a.capability_key,'capability_proof_hash',a.capability_proof_hash,'activated_at',a.activated_at) FROM trip_task_profile_activations a WHERE a.settings_id=rs.id ORDER BY a.activated_at DESC LIMIT 1)) FROM role_settings rs WHERE rs.task_id=?1 ORDER BY rs.role,rs.revision", &id)?;
        task.reviews = json_rows(&connection, "SELECT json_object('id',r.id,'kind',r.review_kind,'candidate_hash',r.candidate_hash,'delivery_state',r.delivery_state,'verdict',r.verdict,'feedback',r.feedback,'session_id',r.session_id,'role_generation_id',r.role_generation_id,'settings_revision',r.settings_revision,'ambiguity_state',r.ambiguity_state) FROM review_requests r JOIN attempts a ON a.id=r.attempt_id WHERE a.task_id=?1 ORDER BY r.created_at", &id)?;
        task.snapshots = json_rows(&connection, "SELECT json_object('id',s.id,'attempt_id',s.attempt_id,'kind',s.kind,'manifest_hash',s.manifest_hash,'manifest',json(s.manifest_json),'complete',s.complete,'created_at',s.created_at,'original_base',s.original_base,'candidate_head',s.candidate_head,'source_role_generation_id',s.source_role_generation_id,'source_settings_revision',s.source_settings_revision,'workspace_id',s.workspace_id,'workspace_hash',s.workspace_hash) FROM snapshots s JOIN attempts a ON a.id=s.attempt_id WHERE a.task_id=?1 ORDER BY s.created_at", &id)?;
        for snapshot in &mut task.snapshots {
            let Some(manifest) = snapshot
                .get_mut("manifest")
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            let total = manifest
                .get_mut("entries")
                .and_then(serde_json::Value::as_array_mut)
                .map(|entries| {
                    let total = entries.len();
                    entries.truncate(100);
                    total
                });
            let Some(total) = total else { continue };
            manifest.insert("total_entries".into(), total.into());
            manifest.insert("entries_truncated".into(), (total > 100).into());
        }
        task.review_budgets = json_rows(&connection, "SELECT json_object('id',b.id,'attempt_id',b.attempt_id,'kind',b.review_kind,'initial_allowance',b.initial_allowance,'extension_allowance',b.extension_allowance,'spent',b.spent,'remaining',b.initial_allowance+b.extension_allowance-b.spent,'version',b.version) FROM review_budgets b JOIN attempts a ON a.id=b.attempt_id WHERE a.task_id=?1 ORDER BY b.review_kind", &id)?;
        tasks.push(task);
    }
    let production_role_restrictions = vec![serde_json::json!({
        "provider": "codex",
        "role": "implementer",
        "status": "unverified",
        "reason": crate::providers::codex::IMPLEMENTER_NATIVE_POLICY_REASON,
    })];
    let mut capabilities = json_rows_no_param(&connection, "SELECT json_object('rowid',rowid,'provider',provider,'version',executable_version,'role',role,'mode',mode,'config_hash',config_hash,'hook_hash',hook_hash,'status',status,'proof',json(proof_json),'gaps',json(gaps_json),'checked_at',checked_at) FROM capabilities ORDER BY provider,role")?;
    for capability in &mut capabilities {
        let Some(provider) = capability
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<crate::domain::Provider>().ok())
        else {
            continue;
        };
        let Some(role) = capability
            .get("role")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<crate::domain::RoleKind>().ok())
        else {
            continue;
        };
        let version = capability
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let current = store.compatibility_bundles.resolve(provider, version, role);
        let recorded = capability
            .pointer("/proof/compatibility")
            .and_then(|value| {
                serde_json::from_value::<crate::provider_compatibility::AuthorityBinding>(
                    value.clone(),
                )
                .ok()
            });
        let explanation = match current {
            Ok(binding) if recorded.as_ref() == Some(&(&binding).into()) => {
                crate::provider_compatibility::matched_explanation(
                    &binding,
                    capability.get("status").and_then(serde_json::Value::as_str)
                        == Some("supported"),
                )
            }
            Ok(binding) => crate::provider_compatibility::CompatibilityExplanation {
                status: crate::provider_compatibility::CompatibilityStatus::EvidenceStale,
                observed_version: None,
                pack_id: Some(binding.pack_id),
                pack_revision: Some(binding.pack_revision),
                contract_id: Some(binding.contract_id),
                contract_revision: Some(binding.contract_revision),
                short_hash: Some(binding.effective_hash.chars().take(12).collect()),
                predicate_id: Some(binding.predicate_id),
                missing_evidence: vec!["current_contract_proof".into()],
                action: crate::provider_compatibility::SafeAction::RequalifyExactProfile,
                message:
                    "Stored evidence predates or differs from this exact compatibility contract."
                        .into(),
            },
            Err(error) => error.explanation().clone(),
        };
        if explanation.status != crate::provider_compatibility::CompatibilityStatus::Matched {
            capability["status"] = serde_json::json!("unverified");
            let gap = serde_json::json!("provider_compatibility_requalification_required");
            if let Some(gaps) = capability["gaps"].as_array_mut() {
                if !gaps.contains(&gap) {
                    gaps.push(gap);
                }
            } else {
                capability["gaps"] = serde_json::json!([gap]);
            }
        }
        capability["compatibility"] = serde_json::to_value(explanation)?;
    }
    let active_sessions = json_rows_no_param(
        &connection,
        r#"
        SELECT json_object(
            'id',s.id,'role_generation_id',s.role_generation_id,'provider',s.provider,
            'status',s.status,'launch_state',s.launch_state,'launch_error',s.launch_error,
            'exit_code',json_extract(s.exit_json,'$.code'),
            'exit_success',json_extract(s.exit_json,'$.success'),
            'exit_status',substr(json_extract(s.exit_json,'$.status'),1,256),
            'exit_reason',substr(COALESCE(json_extract(s.exit_json,'$.reason'),json_extract(s.exit_json,'$.output_tail')),1,2048),
            'process_group_quiescent',COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0),
            'native_session_id',s.native_session_id,'validation_cell',s.validation_cell,
            'runtime_admission_id',(SELECT probe.admission_id FROM trip_runtime_probes probe
              WHERE probe.session_id=s.id ORDER BY probe.updated_at DESC LIMIT 1),
            'readiness',s.readiness_state,
            'capture_state',s.capture_state,'updated_at',s.updated_at,
            'interrupt_requested_at',s.interrupt_requested_at,
            'transcript_epoch',s.transcript_epoch,'resume_count',s.resume_count,
            'process_identity',json(s.process_identity_json),'task_id',t.id,
            'attempt_id',a.id,'role',rg.role,'generation',rg.generation,
            'config_revision',rg.config_revision,'lane_id',rg.lane_id,
            'generation_status',rg.status,
            'has_newer_generation',EXISTS(
                SELECT 1 FROM role_generations newer
                 WHERE newer.attempt_id=rg.attempt_id AND newer.role=rg.role
                   AND newer.lane_id=rg.lane_id AND newer.generation>rg.generation
                   AND newer.status IN ('launch_reserved','running','exited','stopping')
                   AND (
                     EXISTS(SELECT 1 FROM role_settings setting
                             WHERE setting.task_id=t.id AND setting.role=newer.role
                               AND setting.revision=newer.config_revision
                               AND setting.effective_generation_id=newer.id)
                     OR (newer.role='implementer' AND newer.lane_id!='default'
                         AND EXISTS(SELECT 1 FROM lane_generations lane
                                     WHERE lane.lane_id=newer.lane_id
                                       AND lane.effective_generation_id=newer.id))
                   )
            ),
            'attempt_phase',a.phase,'attempt_candidate_hash',a.candidate_hash,
            'attempt_plan_hash',a.plan_hash,'setup_permit_id',s.setup_permit_id,
            'setup_operation_id',a.setup_operation_id,'workflow_version',s.workflow_version,
            'workflow_hash',s.workflow_hash,'role_prompt_hash',s.prompt_hash,
            'launch',json(s.launch_config_json),
            'capability_key',s.capability_key,
            'current_capability_key',(SELECT c.config_hash FROM capabilities c
              WHERE c.provider=s.provider AND c.executable_version=s.executable_version
                AND c.role=rg.role AND c.mode='interactive_pty'
              ORDER BY c.checked_at DESC,c.rowid DESC LIMIT 1),
            'current_capability_rowid',(SELECT c.rowid FROM capabilities c
              WHERE c.provider=s.provider AND c.executable_version=s.executable_version
                AND c.role=rg.role AND c.mode='interactive_pty'
              ORDER BY c.checked_at DESC,c.rowid DESC LIMIT 1),
            'capability_current',EXISTS(SELECT 1 FROM capabilities current_capability
              WHERE current_capability.rowid=(
                SELECT latest_capability.rowid FROM capabilities latest_capability
                WHERE latest_capability.provider=s.provider
                  AND latest_capability.executable_version=s.executable_version
                  AND latest_capability.role=rg.role
                  AND latest_capability.mode='interactive_pty'
                ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1)
                AND current_capability.config_hash=s.capability_key
                AND current_capability.status='supported'
                AND current_capability.proof_json!='{}'),
            'cmux_surface',json((
                SELECT json_object(
                    'id',r.id,'task_workspace_id',r.task_workspace_id,
                    'workspace_id',r.workspace_id,'surface_id',r.surface_id,
                    'binding_revision',r.binding_revision,'surface_state',r.surface_state,
                    'attachment_state',r.attachment_state,
                    'desired_input_state',r.desired_input_state,
                    'actual_input_state',r.actual_input_state,
                    'control_revision',r.control_revision,
                    'applied_revision',r.applied_revision,
                    'last_error',r.last_error,'updated_at',r.updated_at
                )
                FROM cmux_session_surfaces r
                WHERE r.session_id=s.id AND r.role_generation_id=s.role_generation_id
                  AND r.transcript_epoch=s.transcript_epoch
                  AND r.process_identity_json=s.process_identity_json
                -- A current service boot may begin again at binding revision
                -- one. Keep a live current route ahead of retired prior-boot
                -- history whose revision is numerically higher.
                ORDER BY CASE
                    WHEN r.surface_state IN ('opening','open','unknown')
                     AND r.attachment_state IN ('pending','live') THEN 0
                    ELSE 1
                END,r.created_at DESC,r.binding_revision DESC LIMIT 1
            )),
            'input_control',json((
                SELECT json_object('owner_kind',lease.owner_kind,'expires_at',lease.expires_at)
                FROM input_leases lease
                WHERE lease.session_id=s.id AND lease.revoked_at IS NULL
                  AND julianday(lease.expires_at)>julianday('now') LIMIT 1
            ))
        )
        FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
        JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
        ORDER BY s.created_at DESC LIMIT 200
    "#,
    )?;
    let mut active_sessions = active_sessions;
    for session in &mut active_sessions {
        let current = capabilities.iter().find(|capability| {
            capability.get("rowid") == session.get("current_capability_rowid")
                && capability.get("config_hash") == session.get("capability_key")
                && capability.get("status").and_then(serde_json::Value::as_str) == Some("supported")
        });
        session["capability_current"] = serde_json::json!(current.is_some());
    }
    let controls = json_rows_no_param(&connection, "SELECT json_object('id',id,'attempt_id',attempt_id,'role_generation_id',role_generation_id,'kind',kind,'state',state,'payload',json(payload_json),'updated_at',updated_at) FROM controls WHERE state NOT IN ('finished','cancelled') ORDER BY created_at")?;
    let guidance=json_rows_no_param(&connection,"SELECT json_object('id',id,'attempt_id',attempt_id,'role_generation_id',role_generation_id,'body',body,'state',state,'reason',reason,'created_at',created_at,'acknowledged_at',acknowledged_at) FROM guidance_messages ORDER BY created_at DESC LIMIT 200")?;
    let check_suites=json_rows_no_param(&connection,"SELECT json_object('id',id,'project_id',project_id,'name',name,'position',position,'executable',executable,'arguments',json(arguments_json),'timeout_seconds',timeout_seconds,'enabled',enabled,'version',version) FROM check_suites ORDER BY project_id,position,name")?;
    let checks=json_rows_no_param(&connection,"SELECT json_object('id',id,'attempt_id',attempt_id,'candidate_hash',candidate_hash,'check_id',check_id,'selected_check_revision',selected_check_revision,'suite_name',suite_name,'suite_version',check_suite_version,'executable',executable,'arguments',json(arguments_json),'status',status,'launch_state',launch_state,'launch_error',launch_error,'failure_category',json_extract(evidence_json,'$.failure_category'),'exit_code',exit_code,'inputs_hash',inputs_hash,'acceptance_coverage',json(acceptance_coverage_json),'elapsed_millis',elapsed_millis,'freshness_state',freshness_state,'evidence',json(evidence_json),'created_at',created_at,'finished_at',finished_at) FROM check_runs ORDER BY created_at DESC LIMIT 200")?;
    let switches=json_rows_no_param(&connection,"SELECT json_object('id',si.id,'attempt_id',si.attempt_id,'role',si.role,'lane_id',COALESCE((SELECT lane_id FROM role_generations WHERE id=si.old_generation_id),(SELECT lane_id FROM role_generations WHERE id=si.new_generation_id),'default'),'old_generation_id',si.old_generation_id,'new_generation_id',si.new_generation_id,'requested_settings_revision',si.requested_settings_revision,'checkpoint_snapshot_id',si.checkpoint_snapshot_id,'handoff',CASE WHEN json_valid(si.handoff_json) THEN json(si.handoff_json) ELSE json_object('malformed_handoff',1) END,'state',si.state,'updated_at',si.updated_at) FROM switch_intents si ORDER BY si.created_at DESC LIMIT 200")?;
    let mut recovery = json_rows_no_param(
        &connection,
        r#"
        SELECT json_object(
            'id',r.id,'session_id',r.session_id,'attempt_id',r.attempt_id,
            'state',r.state,'detail',json(r.detail_json),'resolved_at',r.resolved_at,
            'updated_at',r.updated_at,
            'actionable',CASE
                WHEN r.state!='attention_required' THEN 0
                WHEN json_extract(r.detail_json,'$.kind')='workspace_reservation' THEN EXISTS(
                    SELECT 1 FROM attempts a
                    JOIN tasks t ON t.id=a.task_id
                    JOIN workspaces w ON w.id=json_extract(r.detail_json,'$.workspace_id')
                      AND w.attempt_id=a.id
                    JOIN claims c ON c.attempt_id=a.id
                     WHERE a.id=r.attempt_id AND a.status='needs_recovery'
                       AND t.lifecycle NOT IN ('done','cancelled')
                       AND a.id=(SELECT latest.id FROM attempts latest
                                 WHERE latest.task_id=t.id
                                 ORDER BY latest.created_at DESC LIMIT 1)
                       AND w.state IN ('reserved','unknown','recovery_required')
                       AND c.state='unknown'
                )
                WHEN json_extract(r.detail_json,'$.kind')='graceful_stop_deadline' THEN EXISTS(
                    SELECT 1 FROM sessions s
                    JOIN role_generations rg ON rg.id=s.role_generation_id
                    JOIN attempts a ON a.id=rg.attempt_id
                    JOIN tasks t ON t.id=a.task_id
                     WHERE s.id=r.session_id AND s.process_identity_json=r.process_identity_json
                       AND s.status IN ('interrupt_requested','recovery_required')
                       AND s.interrupt_requested_at IS NOT NULL
                       AND rg.id=json_extract(r.detail_json,'$.role_generation_id')
                       AND s.transcript_epoch=json_extract(r.detail_json,'$.transcript_epoch')
                       AND a.id=r.attempt_id AND a.status='needs_recovery'
                       AND t.lifecycle NOT IN ('done','cancelled')
                )
                ELSE 1
            END
        ) FROM recovery_records r ORDER BY r.created_at DESC LIMIT 200
    "#,
    )?;
    recovery.extend(json_rows_no_param(
        &connection,
        "SELECT json_object('id',ri.id,'session_id',NULL,'attempt_id',ri.new_attempt_id,
          'state','attention_required','detail',json_object('kind','rework_materialization',
          'rework_intent_id',ri.id,'parent_attempt_id',ri.parent_attempt_id),
          'updated_at',ri.updated_at,'actionable',1)
         FROM rework_intents ri JOIN attempts a ON a.id=ri.new_attempt_id
         JOIN tasks t ON t.id=a.task_id WHERE ri.state='recovery_required'
           AND a.status='needs_recovery' AND t.attention='needs_recovery'
           AND t.lifecycle='in_progress' ORDER BY ri.updated_at DESC",
    )?);
    let history=json_rows_no_param(&connection,"SELECT json_object('id',id,'operation_id',operation_id,'actor_kind',actor_kind,'actor_id',actor_id,'event_code',event_code,'entity_kind',entity_kind,'entity_id',entity_id,'old_version',old_version,'new_version',new_version,'detail',json(detail_json),'created_at',created_at) FROM audit_events WHERE event_code IN ('session.resume.rejected','session.resume.fresh_route.reserved') OR rowid IN (SELECT rowid FROM audit_events ORDER BY created_at DESC LIMIT 500) ORDER BY created_at DESC")?;
    let process_observations = resource_observations(&connection)?;
    let capacity = capacity_status(&connection)?;
    let instance_settings: (i64, bool, String) = connection.query_row(
        "SELECT version,auto_resume_eligible,updated_at FROM instance_settings WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let restart_candidates=json_rows_no_param(&connection,"SELECT json_object('session_id',session_id,'attempt_id',attempt_id,'task_id',task_id,'source',source,'state',state,'reason',reason,'requested_by',requested_by,'result',json(result_json),'created_at',created_at,'updated_at',updated_at) FROM restart_candidates ORDER BY updated_at DESC")?;
    let (permission_requests, permission_rules) = crate::permissions::state_rows(&connection)?;
    let (mut trip_setups, trip_explorer, trip_lanes, trip_checks, trip_task_verification) =
        crate::trip::state_rows(&connection)?;
    let mut decisions = crate::coordinator::read_only_decisions(&connection)?;
    decisions.extend(crate::scheduler::read_only_decisions(&connection)?);
    for task in &tasks {
        if decisions
            .iter()
            .any(|decision| decision.subject.task_id.as_deref() == Some(task.id.as_str()))
        {
            continue;
        }
        if task.lifecycle == "backlog" {
            decisions.push(backlog_readiness_decision(&connection, task)?);
            continue;
        }
        let terminal = matches!(task.lifecycle.as_str(), "done" | "cancelled");
        let project_ready =
            crate::trip::require_project_ready(&connection, &task.project_id).is_ok();
        let prerequisite = DecisionPrerequisite {
            code: if terminal {
                "task.terminal"
            } else {
                "task.owner_evaluation"
            }
            .into(),
            state: if terminal {
                DecisionEvidenceState::Satisfied
            } else if !project_ready {
                DecisionEvidenceState::Missing
            } else {
                DecisionEvidenceState::Unknown
            },
            owner: DecisionOwner::Human,
            evidence: serde_json::json!({"lifecycle":task.lifecycle,"project_id":task.project_id}),
            message: Some(if terminal {
                format!("Task is {}", task.lifecycle)
            } else if !project_ready {
                "Project readiness must be restored before task admission".into()
            } else {
                "Task admission needs current profile and owner verification".into()
            }),
        };
        decisions.push(DecisionExplanation {
            decision_schema: 1,
            reason_code: if terminal {
                "task.terminal"
            } else {
                "task.owner_evaluation_unknown"
            }
            .into(),
            disposition: if terminal {
                DecisionDisposition::Terminal
            } else {
                DecisionDisposition::Waiting
            },
            subject: DecisionSubject {
                project_id: Some(task.project_id.clone()),
                task_id: Some(task.id.clone()),
                attempt_id: task
                    .active_attempt
                    .as_ref()
                    .and_then(|attempt| attempt.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                ..DecisionSubject::default()
            },
            observed_revision: DecisionObservedRevision {
                task_version: Some(task.version),
                ..DecisionObservedRevision::default()
            },
            primary_blocker: (!terminal).then_some(prerequisite.clone()),
            prerequisites: vec![prerequisite],
            ownership: DecisionOwnership {
                owner: DecisionOwner::Human,
                state: "recorded_task_status".into(),
                binding: DecisionActionBinding {
                    task_id: Some(task.id.clone()),
                    expected_task_version: Some(task.version),
                    ..DecisionActionBinding::default()
                },
            },
            next_action: None,
            control_policy: DecisionControlPolicy {
                allowed_controls: Vec::new(),
                disabled_reason_code: (!terminal)
                    .then_some("task.current_admission_unknown".into()),
            },
        });
    }
    let restore_hold_active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id='database-restore-hold' AND state='attention_required')",
        [], |row| row.get(0),
    )?;
    if restore_hold_active {
        let prerequisites = crate::database::restore_prerequisites(&connection, false)?;
        decisions.push(DecisionExplanation {
            decision_schema: 1,
            reason_code: "restore.hold_active".into(),
            disposition: DecisionDisposition::Held,
            subject: DecisionSubject {
                recovery_id: Some("database-restore-hold".into()),
                ..DecisionSubject::default()
            },
            observed_revision: DecisionObservedRevision::default(),
            primary_blocker: prerequisites
                .iter()
                .find(|item| item.state != crate::domain::DecisionEvidenceState::Satisfied)
                .cloned(),
            prerequisites,
            ownership: DecisionOwnership {
                owner: DecisionOwner::External,
                state: "offline_restore_release".into(),
                binding: DecisionActionBinding {
                    recovery_id: Some("database-restore-hold".into()),
                    ..DecisionActionBinding::default()
                },
            },
            next_action: None,
            control_policy: DecisionControlPolicy {
                allowed_controls: Vec::new(),
                disabled_reason_code: Some("restore.release_offline_only".into()),
            },
        });
    }
    let continuation_actions = continuation_actions(
        &tasks,
        &capabilities,
        &active_sessions,
        &controls,
        &switches,
        &recovery,
        &history,
        &restart_candidates,
        &trip_setups,
        &trip_explorer,
        &trip_lanes,
        &decisions,
    );
    for setup in &mut trip_setups {
        let setup_receipts = setup
            .get("probe_receipts")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(selections) = setup
            .get_mut("selected_profiles")
            .and_then(serde_json::Value::as_array_mut)
        {
            for selection in selections {
                let Some(provider) = selection
                    .pointer("/profile/provider")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<crate::domain::Provider>().ok())
                else {
                    continue;
                };
                let Some(role) = selection
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<crate::domain::RoleKind>().ok())
                else {
                    continue;
                };
                let observed = selected_setup_capability(selection, &setup_receipts, &capabilities);
                let explanation = if let Some(observed) = observed {
                    observed.get("compatibility").cloned()
                } else if provider == crate::domain::Provider::Claude {
                    store
                        .compatibility_bundles
                        .resolve(provider, "", role)
                        .err()
                        .and_then(|error| serde_json::to_value(error.explanation()).ok())
                } else {
                    Some(serde_json::to_value(crate::provider_compatibility::CompatibilityExplanation {
                        status: crate::provider_compatibility::CompatibilityStatus::EvidenceStale,
                        observed_version: None,
                        pack_id: None,
                        pack_revision: None,
                        contract_id: None,
                        contract_revision: None,
                        short_hash: None,
                        predicate_id: None,
                        missing_evidence: vec!["exact_selected_profile_observation".into()],
                        action: crate::provider_compatibility::SafeAction::RequalifyExactProfile,
                        message: "No exact compatibility observation is recorded for this selected profile.".into(),
                    })?)
                };
                let explanation = explanation.map(|mut value| {
                    if value["status"] == "matched" {
                        value["message"] = serde_json::json!(
                            "A reviewed contract candidate exists. Selected profile authority is checked separately."
                        );
                    }
                    value
                });
                if let (Some(explanation), Some(object)) = (explanation, selection.as_object_mut())
                {
                    object.insert("compatibility".into(), explanation);
                }
            }
        }
        let setup_id = setup.get("setup_operation_id");
        let project_id = setup.get("project_id");
        let discovery_attempt = setup.get("discovery_attempt_id");
        let probe_attempt = setup.get("probe_attempt_id");
        let setup_actions = continuation_actions
            .iter()
            .filter(|action| {
                action.binding.get("setup_operation_id") == setup_id
                    || action.binding.get("attempt_id") == discovery_attempt
                    || action.binding.get("attempt_id") == probe_attempt
                    || (action.binding.get("project_id") == project_id
                        && matches!(
                            action.operation.as_str(),
                            "prepare_runtime_admission" | "recover_installation"
                        ))
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(object) = setup.as_object_mut() {
            object.insert(
                "continuation_actions".to_owned(),
                serde_json::to_value(setup_actions)?,
            );
        }
    }
    let attention = AttentionSources {
        projects: &projects,
        tasks: &tasks,
        sessions: &active_sessions,
        recovery: &recovery,
        permission_requests: &permission_requests,
        trip_setups: &trip_setups,
        decisions: &decisions,
        continuation_actions: &continuation_actions,
    }
    .items();
    let resources = serde_json::json!({
        "active_sessions":active_sessions.iter().filter(|value|!matches!(value.get("status").and_then(|state|state.as_str()),Some("exited"|"launch_failed"))).count(),
        "active_controls":controls.len(),
        "queued_guidance":guidance.iter().filter(|value|value.get("state").and_then(|state|state.as_str())==Some("queued")).count(),
        "running_checks":checks.iter().filter(|value|matches!(value.get("status").and_then(|state|state.as_str()),Some("launch_reserved"|"running"|"recovery_required"))).count(),
        "capacity":capacity,
        "observed_at":Utc::now().to_rfc3339(),
        "processes":process_observations,
    });
    let (profile_sets, task_recipes, recipe_schedules) = crate::recipes::projection(&connection)?;
    Ok(AppStateDto {
        schema: 8,
        generated_at: Utc::now().to_rfc3339(),
        revision: revision.to_string(),
        projects,
        tasks,
        profile_sets,
        task_recipes,
        recipe_schedules,
        production_role_restrictions,
        capabilities,
        active_sessions,
        controls,
        guidance,
        check_suites,
        checks,
        switches,
        recovery,
        history,
        resources,
        instance_settings: serde_json::json!({"version":instance_settings.0,"auto_resume_eligible":instance_settings.1,"updated_at":instance_settings.2}),
        restart_candidates,
        permission_requests,
        permission_rules,
        trip_setups,
        trip_explorer,
        trip_lanes,
        trip_checks,
        trip_task_verification,
        decisions,
        continuation_actions,
        attention,
    })
}

fn backlog_readiness_decision(
    connection: &Connection,
    task: &TaskDto,
) -> Result<DecisionExplanation> {
    let project = crate::trip::require_project_ready(connection, &task.project_id);
    let scope = validate_task(&task.title, &task.acceptance_criteria);
    let configured: i64 = connection.query_row(
        "SELECT COUNT(DISTINCT role)
         FROM role_settings WHERE task_id=?1 AND role IN
         ('manager','explorer','plan_reviewer','implementer','code_reviewer','final_verifier')
         AND revision=(SELECT MAX(latest.revision) FROM role_settings latest
                       WHERE latest.task_id=role_settings.task_id AND latest.role=role_settings.role)",
        params![task.id], |row| row.get(0),
    )?;
    let prerequisite =
        |code: &str, state: DecisionEvidenceState, message: Option<String>| DecisionPrerequisite {
            code: code.into(),
            state,
            owner: DecisionOwner::Human,
            evidence: serde_json::json!({"task_id":task.id,"project_id":task.project_id}),
            message,
        };
    let prerequisites = vec![
        prerequisite("task.project_ready", if project.is_ok() { DecisionEvidenceState::Satisfied } else { DecisionEvidenceState::Missing }, project.as_ref().err().map(|error| format!("{error:#}"))),
        prerequisite("task.valid_scope", if scope.is_ok() { DecisionEvidenceState::Satisfied } else { DecisionEvidenceState::Missing }, scope.as_ref().err().map(|error| format!("{error:#}"))),
        prerequisite("task.six_role_settings", if configured == 6 { DecisionEvidenceState::Satisfied } else { DecisionEvidenceState::Missing }, (configured != 6).then(|| format!("{configured} of six app roles have current settings"))),
        prerequisite("task.profile_activation_and_runtime", DecisionEvidenceState::Unknown, Some("Exact project-default or task-override activation and current runtime capability are checked by Make Ready".into())),
    ];
    let blocker = prerequisites
        .iter()
        .find(|item| item.state == DecisionEvidenceState::Missing)
        .cloned();
    let enabled = blocker.is_none();
    let binding = DecisionActionBinding {
        project_id: Some(task.project_id.clone()),
        task_id: Some(task.id.clone()),
        expected_task_version: Some(task.version),
        ..DecisionActionBinding::default()
    };
    Ok(DecisionExplanation {
        decision_schema: 1, reason_code: "task.backlog_readiness".into(),
        disposition: DecisionDisposition::Waiting,
        subject: DecisionSubject { project_id: Some(task.project_id.clone()), task_id: Some(task.id.clone()), ..DecisionSubject::default() },
        observed_revision: DecisionObservedRevision { task_version: Some(task.version), ..DecisionObservedRevision::default() },
        primary_blocker: blocker.clone(), prerequisites,
        ownership: DecisionOwnership { owner: DecisionOwner::Human, state: "recorded_task_status".into(), binding: binding.clone() },
        next_action: Some(DecisionNextAction {
            operation: if project.is_ok() { "make_ready" } else { "inspect_project" }.into(),
            enabled: enabled || project.is_err(), owner: DecisionOwner::Human, binding,
            accounting_note: Some("Make Ready rechecks project, scope, six settings, activations, and current runtime authority".into()),
        }),
        control_policy: DecisionControlPolicy {
            allowed_controls: if enabled { vec!["make_ready".into()] } else if project.is_err() { vec!["inspect_project".into()] } else { Vec::new() },
            disabled_reason_code: blocker.map(|item| item.code),
        },
    })
}

fn continuation(
    kind: ContinuationActionKind,
    enabled: bool,
    reason: impl Into<String>,
    owner: &str,
    waiting_for: Option<&str>,
    since: Option<String>,
    deadline_at: Option<String>,
    operation: &str,
    binding: serde_json::Value,
    accounting_note: Option<&str>,
) -> ContinuationAction {
    ContinuationAction {
        kind,
        enabled,
        reason: reason.into(),
        owner: owner.to_owned(),
        waiting_for: waiting_for.map(str::to_owned),
        since,
        deadline_at,
        operation: operation.to_owned(),
        binding,
        accounting_note: accounting_note.map(str::to_owned),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReviewerFreshRoute {
    NotReviewer,
    Eligible,
    AllowanceExhausted,
    Stale,
    FinalFreshOnly,
}

fn reviewer_fresh_route(
    tasks: &[TaskDto],
    session: &serde_json::Value,
    detail: &serde_json::Value,
) -> ReviewerFreshRoute {
    let role = session.get("role").and_then(serde_json::Value::as_str);
    if role == Some("final_verifier") {
        return ReviewerFreshRoute::FinalFreshOnly;
    }
    let Some((review_kind, candidate_key)) = (match role {
        Some("plan_reviewer") => Some(("plan", "plan_hash")),
        Some("code_reviewer") => Some(("code", "candidate_hash")),
        _ => None,
    }) else {
        return ReviewerFreshRoute::NotReviewer;
    };
    let task_id = session.get("task_id").and_then(serde_json::Value::as_str);
    let attempt_id = session
        .get("attempt_id")
        .and_then(serde_json::Value::as_str);
    let session_id = session.get("id").and_then(serde_json::Value::as_str);
    let generation_id = session
        .get("role_generation_id")
        .and_then(serde_json::Value::as_str);
    let review_id = detail
        .get("review_request_id")
        .and_then(serde_json::Value::as_str);
    let candidate = detail.get(candidate_key);
    let Some(task) = tasks.iter().find(|task| Some(task.id.as_str()) == task_id) else {
        return ReviewerFreshRoute::Stale;
    };
    if task
        .active_attempt
        .as_ref()
        .and_then(|attempt| attempt.get("id"))
        .and_then(serde_json::Value::as_str)
        != attempt_id
    {
        return ReviewerFreshRoute::Stale;
    }
    let current_review = task.reviews.iter().find(|review| {
        review.get("id").and_then(serde_json::Value::as_str) == review_id
            && review.get("kind").and_then(serde_json::Value::as_str) == Some(review_kind)
            && review.get("session_id").and_then(serde_json::Value::as_str) == session_id
            && review
                .get("role_generation_id")
                .and_then(serde_json::Value::as_str)
                == generation_id
            && review
                .get("delivery_state")
                .and_then(serde_json::Value::as_str)
                == Some("delivered")
            && review.get("candidate_hash") == candidate
            && review.get("settings_revision") == detail.get("config_revision")
    });
    if current_review.is_none() {
        return ReviewerFreshRoute::Stale;
    }
    if task.review_budgets.iter().any(|budget| {
        budget.get("attempt_id").and_then(serde_json::Value::as_str) == attempt_id
            && budget.get("kind").and_then(serde_json::Value::as_str) == Some(review_kind)
            && budget
                .get("remaining")
                .and_then(serde_json::Value::as_i64)
                .is_some_and(|remaining| remaining > 0)
    }) {
        ReviewerFreshRoute::Eligible
    } else {
        ReviewerFreshRoute::AllowanceExhausted
    }
}

fn session_generation_is_current(
    tasks: &[TaskDto],
    trip_lanes: &[serde_json::Value],
    session: &serde_json::Value,
) -> bool {
    let (Some(task_id), Some(attempt_id), Some(role), Some(generation_id)) = (
        session.get("task_id").and_then(serde_json::Value::as_str),
        session
            .get("attempt_id")
            .and_then(serde_json::Value::as_str),
        session.get("role").and_then(serde_json::Value::as_str),
        session
            .get("role_generation_id")
            .and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    let Some(task) = tasks.iter().find(|task| task.id == task_id) else {
        return false;
    };
    if task
        .active_attempt
        .as_ref()
        .and_then(|attempt| attempt.get("id"))
        .and_then(serde_json::Value::as_str)
        != Some(attempt_id)
    {
        return false;
    }
    if !matches!(
        session
            .get("generation_status")
            .and_then(serde_json::Value::as_str),
        Some("running" | "exited" | "launch_reserved" | "stopping")
    ) {
        return false;
    }
    if session
        .get("has_newer_generation")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || session
            .get("has_newer_generation")
            .and_then(serde_json::Value::as_i64)
            == Some(1)
    {
        return false;
    }
    let lane_id = session
        .get("lane_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default");
    if role == "implementer" && lane_id != "default" {
        return trip_lanes.iter().any(|lane| {
            lane.get("id").and_then(serde_json::Value::as_str) == Some(lane_id)
                && lane.get("attempt_id").and_then(serde_json::Value::as_str) == Some(attempt_id)
                && lane
                    .get("effective_generation_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(generation_id)
        });
    }
    task.role_settings.iter().any(|setting| {
        setting.get("role").and_then(serde_json::Value::as_str) == Some(role)
            && setting.get("revision") == session.get("config_revision")
            && setting
                .get("effective_generation_id")
                .and_then(serde_json::Value::as_str)
                == Some(generation_id)
    })
}

fn native_resume_is_fenced(
    controls: &[serde_json::Value],
    switches: &[serde_json::Value],
    session: &serde_json::Value,
) -> bool {
    let (Some(session_id), Some(attempt_id), Some(generation_id)) = (
        session.get("id").and_then(serde_json::Value::as_str),
        session
            .get("attempt_id")
            .and_then(serde_json::Value::as_str),
        session
            .get("role_generation_id")
            .and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    let manager_stop_fenced = controls.iter().any(|control| {
        control
            .get("attempt_id")
            .and_then(serde_json::Value::as_str)
            == Some(attempt_id)
            && control.get("kind").and_then(serde_json::Value::as_str) == Some("manager_stop")
            && control.get("state").and_then(serde_json::Value::as_str) == Some("held")
            && control
                .get("payload")
                .and_then(|payload| payload.get("manager_session_id"))
                .and_then(serde_json::Value::as_str)
                == Some(session_id)
    });
    let manager_change_fenced = controls.iter().any(|control| {
        control
            .get("attempt_id")
            .and_then(serde_json::Value::as_str)
            == Some(attempt_id)
            && control.get("kind").and_then(serde_json::Value::as_str) == Some("manager_change")
            && !matches!(
                control.get("state").and_then(serde_json::Value::as_str),
                Some("finished" | "cancelled" | "superseded" | "rejected" | "failed")
            )
            && control
                .get("payload")
                .and_then(|payload| payload.get("old_generation_id"))
                .and_then(serde_json::Value::as_str)
                == Some(generation_id)
    });
    manager_stop_fenced
        || manager_change_fenced
        || switches.iter().any(|switch| {
            switch.get("attempt_id").and_then(serde_json::Value::as_str) == Some(attempt_id)
                && switch
                    .get("old_generation_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(generation_id)
                && !matches!(
                    switch.get("state").and_then(serde_json::Value::as_str),
                    Some("completed" | "cancelled" | "superseded")
                )
        })
}

fn observed_resume_identity_is_complete(detail: &serde_json::Value) -> bool {
    let Some(identity) = detail
        .get("observed_identity")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    ["provider", "executable_version", "role", "model", "effort"]
        .iter()
        .all(|field| {
            identity
                .get(*field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
        && identity.get("mode").and_then(serde_json::Value::as_str) == Some("interactive_pty")
        && detail
            .get("observed_capability_key")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty())
}

fn observed_resume_capability_is_current_supported(
    capabilities: &[serde_json::Value],
    detail: &serde_json::Value,
) -> bool {
    if !observed_resume_identity_is_complete(detail) {
        return false;
    }
    let Some(identity) = detail
        .get("observed_identity")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let latest = capabilities
        .iter()
        .filter(|capability| {
            capability.get("provider") == identity.get("provider")
                && capability.get("version") == identity.get("executable_version")
                && capability.get("role") == identity.get("role")
                && capability.get("mode") == identity.get("mode")
        })
        .max_by_key(|capability| {
            (
                capability
                    .get("checked_at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                capability
                    .get("rowid")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default(),
            )
        });
    latest.is_some_and(|capability| {
        capability.get("status").and_then(serde_json::Value::as_str) == Some("supported")
            && capability
                .get("proof")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|proof| !proof.is_empty())
            && capability.get("config_hash") == detail.get("observed_capability_key")
    })
}

fn observed_resume_profile_is_current(
    tasks: &[TaskDto],
    session: &serde_json::Value,
    detail: &serde_json::Value,
) -> bool {
    if !observed_resume_identity_is_complete(detail) {
        return false;
    }
    let (Some(identity), Some(task_id), Some(role)) = (
        detail
            .get("observed_identity")
            .and_then(serde_json::Value::as_object),
        session.get("task_id").and_then(serde_json::Value::as_str),
        session.get("role").and_then(serde_json::Value::as_str),
    ) else {
        return false;
    };
    if identity.get("role").and_then(serde_json::Value::as_str) != Some(role) {
        return false;
    }
    let Some(task) = tasks.iter().find(|task| task.id == task_id) else {
        return false;
    };
    task.role_settings.iter().any(|setting| {
        setting.get("role").and_then(serde_json::Value::as_str) == Some(role)
            && setting.get("revision") == session.get("config_revision")
            && setting
                .get("config")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|profile| {
                    profile.get("provider") == identity.get("provider")
                        && profile.get("model") == identity.get("model")
                        && profile.get("effort") == identity.get("effort")
                })
    })
}

fn permanent_resume_rejection_is_current(
    tasks: &[TaskDto],
    trip_lanes: &[serde_json::Value],
    capabilities: &[serde_json::Value],
    rejection_created_at: Option<&str>,
    detail: &serde_json::Value,
    session: &serde_json::Value,
) -> bool {
    if !session_generation_is_current(tasks, trip_lanes, session) {
        return false;
    }
    match detail.get("category").and_then(serde_json::Value::as_str) {
        Some(
            "provider_compatibility_unsupported"
            | "provider_compatibility_contract_changed"
            | "provider_compatibility_invalid_manifest",
        ) => {
            // A proved replacement does not make an unsupported or changed frozen session resumable.
            if detail.get("category").and_then(serde_json::Value::as_str)
                == Some("provider_compatibility_invalid_manifest")
            {
                let replacement_proved = capabilities.iter().any(|capability| {
                    let (Some(rejected_at), Some(proof)) =
                        (rejection_created_at, capability.get("proof"))
                    else {
                        return false;
                    };
                    if capability.get("status").and_then(serde_json::Value::as_str)
                        != Some("supported")
                        || capability.get("provider") != session.get("provider")
                        || capability.get("role") != session.get("role")
                        || capability
                            .get("checked_at")
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(|checked| checked <= rejected_at)
                        || proof.get("compatibility").is_none()
                    {
                        return false;
                    }
                    let candidate = serde_json::json!({
                        "observed_capability_key":capability.get("config_hash"),
                        "observed_identity":{
                            "provider":capability.get("provider"),
                            "executable_version":capability.get("version"),
                            "role":capability.get("role"),
                            "model":proof.get("model"),
                            "effort":proof.get("effort"),
                            "mode":"interactive_pty"
                        }
                    });
                    observed_resume_profile_is_current(tasks, session, &candidate)
                        && observed_resume_capability_is_current_supported(capabilities, &candidate)
                });
                return !replacement_proved;
            }
            let Some(observed_key) = detail
                .get("observed_capability_key")
                .and_then(serde_json::Value::as_str)
            else {
                return true;
            };
            let capability_current = session
                .get("capability_current")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            !capability_current
                || session
                    .get("current_capability_key")
                    .and_then(serde_json::Value::as_str)
                    != Some(observed_key)
        }
        Some("frozen_runtime_identity_changed") => detail
            .get("frozen_capability_key")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        Some(_) | None => true,
    }
}

fn permanent_resume_rejection_route(
    category: &str,
) -> (
    ContinuationActionKind,
    bool,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
) {
    match category {
        "provider_compatibility_invalid_manifest" => (
            ContinuationActionKind::TerminalIncomplete, false,
            "The embedded provider compatibility contract is invalid; update LLMRelay.",
            "external", "terminal_incomplete", Some("No retry is available from this release."),
        ),
        "provider_compatibility_unsupported" | "provider_compatibility_contract_changed" => (
            ContinuationActionKind::ReplaceStaleAuthority, false,
            "The provider compatibility contract needs a reviewed exact version and fresh proof.",
            "human", "replace_stale_authority", Some("Fresh dispatch requires a current matching contract and proof."),
        ),
        "resume_spent" | "authority_consumed" => (
            ContinuationActionKind::AuthorizationRequired,
            false,
            "This retained session's authority is spent and cannot be resumed or silently replaced.",
            "external",
            "authorization_required",
            Some("A new authorization is required; this action never reuses spent retained-session authority."),
        ),
        "profile_authority_changed"
        | "native_history_unavailable"
        | "hook_trust_unavailable"
        | "invocation_provenance_missing"
        | "stale_generation"
        | "stale_review_or_scope" => (
            ContinuationActionKind::ReplaceStaleAuthority,
            true,
            "The retained session is permanently ineligible for exact resume; review and authorize a corrected replacement.",
            "human",
            "replace_stale_authority",
            Some("Replacement authority is separately reviewed and never converts this rejected exact resume into a fresh launch."),
        ),
        _ => (
            ContinuationActionKind::TerminalIncomplete,
            false,
            "The retained session has an unrecognized permanent rejection category and cannot resume automatically.",
            "external",
            "terminal_incomplete",
            Some("Preserve the durable rejection record and obtain an explicit disposition before replacing this session."),
        ),
    }
}

fn continuation_actions(
    tasks: &[TaskDto],
    capabilities: &[serde_json::Value],
    sessions: &[serde_json::Value],
    controls: &[serde_json::Value],
    switches: &[serde_json::Value],
    recovery: &[serde_json::Value],
    history: &[serde_json::Value],
    restart_candidates: &[serde_json::Value],
    trip_setups: &[serde_json::Value],
    trip_explorer: &[serde_json::Value],
    trip_lanes: &[serde_json::Value],
    decisions: &[DecisionExplanation],
) -> Vec<ContinuationAction> {
    let mut actions = Vec::new();
    for task in tasks {
        let active = task.active_attempt.as_ref();
        if let Some(attempt) = active {
            let attempt_id = attempt.get("id").and_then(serde_json::Value::as_str);
            if attempt.get("phase").and_then(serde_json::Value::as_str)
                == Some("awaiting_implementation_authorization")
            {
                actions.push(continuation(
                    ContinuationActionKind::AuthorizeImplementation,
                    attempt_id.is_some() && attempt.get("plan_hash").and_then(serde_json::Value::as_str).is_some(),
                    "The approved plan needs a distinct implementation authorization.",
                    "human",
                    None,
                    None,
                    None,
                    "authorize_implementation",
                    serde_json::json!({"task_id":task.id,"attempt_id":attempt_id,"expected_task_version":task.version,"plan_hash":attempt.get("plan_hash"),"config_revision_id":attempt.get("config_revision_id")}),
                    Some("Does not consume a provider call; it authorizes implementation of this exact plan."),
                ));
            }
            if attempt
                .get("legacy_migration_required")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                actions.push(continuation(
                    ContinuationActionKind::MigrateAttempt,
                    attempt_id.is_some(),
                    "This active attempt requires explicit migration before ordinary controls can continue.",
                    "human",
                    None,
                    None,
                    None,
                    "migrate_attempt",
                    serde_json::json!({"task_id":task.id,"attempt_id":attempt_id,"expected_task_version":task.version,"plan_hash":attempt.get("plan_hash"),"config_revision_id":attempt.get("config_revision_id")}),
                    Some("Migration rechecks quiescence and current workflow configuration."),
                ));
            }
        }
        let source_status = task
            .legacy
            .get("source_status")
            .or_else(|| {
                task.legacy
                    .get("frontmatter")
                    .and_then(|value| value.get("status"))
            })
            .and_then(serde_json::Value::as_str);
        let legacy_normalized = task
            .legacy
            .get("llmrelay_normalization")
            .and_then(|value| value.get("state"))
            .and_then(serde_json::Value::as_str)
            == Some("normalized");
        if matches!(source_status, Some("in_progress" | "implemented"))
            && active.is_none()
            && !task.archived
            && !matches!(task.lifecycle.as_str(), "done" | "cancelled")
            && !legacy_normalized
        {
            actions.push(continuation(
                ContinuationActionKind::StartManagedLegacyAttempt,
                true,
                "Imported legacy work has no LLMRelay attempt lineage and must begin through normal current admission.",
                "human",
                None,
                None,
                None,
                "normalize_legacy_task",
                serde_json::json!({"task_id":task.id,"expected_task_version":task.version,"source_status":source_status}),
                Some("Starts no provider work itself; normal Ready and admission checks remain required."),
            ));
        }
    }
    for setup in trip_setups {
        let state = setup.get("state").and_then(serde_json::Value::as_str);
        if matches!(state, Some("applying" | "recovery_required")) {
            actions.push(continuation(
                ContinuationActionKind::RecoverSetupApply,
                state == Some("recovery_required"),
                "The authorized installation journal requires byte-for-byte recovery before it can proceed.",
                "human",
                (state == Some("applying")).then_some("startup reconciliation"),
                setup.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                None,
                "recover_installation",
                serde_json::json!({"setup_operation_id":setup.get("setup_operation_id"),"project_id":setup.get("project_id")}),
                Some("Recovery verifies existing journal bytes and never blindly overwrites destination drift."),
            ));
        }
        if let Some(admissions) = setup
            .get("runtime_admissions")
            .and_then(serde_json::Value::as_array)
        {
            for admission in admissions {
                let admission_state = admission
                    .get("probe_state")
                    .or_else(|| admission.get("state"))
                    .and_then(serde_json::Value::as_str);
                if matches!(admission_state, Some("failed" | "stale")) {
                    actions.push(continuation(
                        ContinuationActionKind::PrepareCorrectedRuntime,
                        true,
                        admission
                            .get("failure_reason")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("The retained runtime proof is no longer usable and requires a corrected scoped verification.")
                            .to_owned(),
                        "human",
                        None,
                        admission.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                        None,
                        "prepare_runtime_admission",
                        serde_json::json!({"project_id":setup.get("project_id"),"task_id":admission.get("task_id"),"admission_id":admission.get("id"),"role":admission.get("role")}),
                        Some("A corrected runtime verification is a fresh, separately scoped call and does not repair the rejected retained session."),
                    ));
                }
            }
        }
    }
    for record in recovery {
        if record.get("state").and_then(serde_json::Value::as_str) != Some("attention_required") {
            continue;
        }
        let detail = record.get("detail").cloned().unwrap_or_default();
        let kind = detail.get("kind").and_then(serde_json::Value::as_str);
        let actionable = record
            .get("actionable")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            || record.get("actionable").and_then(serde_json::Value::as_i64) == Some(1);
        let binding = serde_json::json!({
            "recovery_id":record.get("id"),"attempt_id":record.get("attempt_id"),
            "session_id":record.get("session_id"),"workspace_id":detail.get("workspace_id"),
            "detail":detail
        });
        if kind == Some("workspace_reservation") {
            if !actionable {
                actions.push(continuation(
                    ContinuationActionKind::RefreshAndReconcile,
                    true,
                    "The recorded workspace recovery no longer owns a current unresolved reservation tuple. Refresh before submitting another exact workspace operation.",
                    "human",
                    None,
                    record.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                    None,
                    "refresh_and_reconcile",
                    binding,
                    Some("Historical or superseded workspace records never authorize a retry or cancellation."),
                ));
                continue;
            }
            actions.push(continuation(
                ContinuationActionKind::RecoverWorkspaceReservation,
                true,
                "Repository workspace ownership is uncertain; inspect the exact recorded path before retrying or cancelling it.",
                "human",
                None,
                record.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                None,
                "recover_workspace_reservation",
                binding,
                Some("Retry and cancel are versioned exact-tuple operations; neither removes worktree bytes."),
            ));
        } else if kind == Some("graceful_stop_deadline") {
            if !actionable {
                actions.push(continuation(
                    ContinuationActionKind::RefreshAndReconcile,
                    true,
                    "The recorded graceful-stop tuple is no longer current. Refresh the durable state; retry and force-stop are unavailable without its exact live binding.",
                    "human",
                    None,
                    record.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                    None,
                    "refresh_and_reconcile",
                    binding,
                    Some("A settled or superseded stop record cannot signal a process again."),
                ));
                continue;
            }
            let session = record
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|session_id| {
                    sessions.iter().find(|session| {
                        session.get("id").and_then(serde_json::Value::as_str) == Some(session_id)
                    })
                });
            if let Some(session) = session {
                let binding = serde_json::json!({
                    "task_id":session.get("task_id"),
                    "attempt_id":session.get("attempt_id"),
                    "session_id":session.get("id"),
                    "role_generation_id":session.get("role_generation_id"),
                    "transcript_epoch":session.get("transcript_epoch"),
                    "process_identity":session.get("process_identity"),
                    "interrupt_requested_at":detail.get("interrupt_requested_at"),
                    "recovery_id":record.get("id"),
                });
                let exact_binding = binding
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
                    && binding
                        .get("role_generation_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some()
                    && binding
                        .get("transcript_epoch")
                        .and_then(serde_json::Value::as_str)
                        .is_some()
                    && binding
                        .get("process_identity")
                        .is_some_and(serde_json::Value::is_object);
                actions.push(continuation(
                    ContinuationActionKind::RetryGracefulStop,
                    exact_binding,
                    "The durable graceful-stop deadline elapsed without exact quiescence.",
                    "human",
                    None,
                    record
                        .get("updated_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    None,
                    "retry_graceful_stop",
                    binding.clone(),
                    Some("Rechecks the same managed identity and preserves the original deadline."),
                ));
                actions.push(continuation(
                    ContinuationActionKind::ForceStopExactProcess,
                    exact_binding,
                    "Force stop remains a human-only escalation for the exact managed process identity.",
                    "human",
                    None,
                    record.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                    None,
                    "force_stop_exact_process",
                    binding,
                    Some("Immediately rechecks identity and requires later quiescence proof; it never runs automatically."),
                ));
            } else {
                actions.push(continuation(
                    ContinuationActionKind::RefreshAndReconcile,
                    true,
                    "The graceful-stop recovery no longer has its exact session projection. Refresh and inspect the durable record; generic recovery cannot safely act without that identity.",
                    "human",
                    None,
                    record.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                    None,
                    "refresh_and_reconcile",
                    binding,
                    None,
                ));
            }
        } else {
            let exact_process_or_check = record
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|session_id| !session_id.is_empty())
                || detail
                    .get("check_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|check_id| !check_id.is_empty())
                || matches!(
                    kind,
                    Some(
                        "database_restore_claim"
                            | "database_restore_freeze"
                            | "rework_materialization"
                    )
                );
            actions.push(continuation(
                if exact_process_or_check {
                    ContinuationActionKind::RecoverOwnership
                } else {
                    ContinuationActionKind::RefreshAndReconcile
                },
                true,
                if exact_process_or_check {
                    "Durable process or ownership recovery must be resolved before automatic work can continue."
                } else {
                    "This historical control record has no exact process, check, workspace, or claim binding. Refresh and submit a corrected control instead of using generic recovery."
                },
                "human",
                None,
                record.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                None,
                if exact_process_or_check {
                    "resolve_recovery"
                } else {
                    "refresh_and_reconcile"
                },
                binding,
                None,
            ));
        }
    }
    for session in sessions {
        let status = session.get("status").and_then(serde_json::Value::as_str);
        let id = session.get("id").and_then(serde_json::Value::as_str);
        let restart_candidate_managed = id.is_some_and(|session_id| {
            session
                .get("validation_cell")
                .is_some_and(serde_json::Value::is_null)
                && restart_candidates.iter().any(|candidate| {
                    candidate
                        .get("session_id")
                        .and_then(serde_json::Value::as_str)
                        == Some(session_id)
                        && !matches!(
                            candidate.get("state").and_then(serde_json::Value::as_str),
                            Some("resumed" | "released_fresh_dispatch" | "cancelled")
                        )
                })
        });
        let session_generation_current = session_generation_is_current(tasks, trip_lanes, session);
        let native_resume_fenced = native_resume_is_fenced(controls, switches, session);
        if status == Some("interrupt_requested") {
            let since = session
                .get("interrupt_requested_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let deadline_at = since.as_deref().and_then(|value| {
                chrono::DateTime::parse_from_rfc3339(value)
                    .ok()
                    .map(|time| {
                        (time + chrono::Duration::seconds(GRACEFUL_STOP_SECONDS)).to_rfc3339()
                    })
            });
            actions.push(continuation(
                ContinuationActionKind::WaitForExit,
                false,
                "A verified exact process-group exit is required before replacement or resume.",
                "service",
                Some("verified process-group exit"),
                since.clone(),
                deadline_at.clone(),
                "wait_for_exit",
                serde_json::json!({"task_id":session.get("task_id"),"attempt_id":session.get("attempt_id"),"session_id":id,"role_generation_id":session.get("role_generation_id"),"transcript_epoch":session.get("transcript_epoch"),"process_identity":session.get("process_identity")}),
                None,
            ));
        }
        if status == Some("exited") {
            if native_resume_fenced {
                continue;
            }
            let matching_rejection = history.iter().find(|event| {
                event.get("event_code").and_then(serde_json::Value::as_str)
                    == Some("session.resume.rejected")
                    && event.get("entity_id") == session.get("id")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("role_generation_id"))
                        == session.get("role_generation_id")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("transcript_epoch"))
                        == session.get("transcript_epoch")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("resume_count"))
                        == session.get("resume_count")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("attempt_id"))
                        == session.get("attempt_id")
                    && event.get("detail").and_then(|detail| detail.get("role"))
                        == session.get("role")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("config_revision"))
                        == session.get("config_revision")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("attempt_phase"))
                        == session.get("attempt_phase")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("candidate_hash"))
                        == session.get("attempt_candidate_hash")
                    && event
                        .get("detail")
                        .and_then(|detail| detail.get("plan_hash"))
                        == session.get("attempt_plan_hash")
            });
            let current_rejection = matching_rejection.filter(|event| {
                permanent_resume_rejection_is_current(
                    tasks,
                    trip_lanes,
                    capabilities,
                    event.get("created_at").and_then(serde_json::Value::as_str),
                    event.get("detail").unwrap_or(&serde_json::Value::Null),
                    session,
                ) && !history.iter().any(|consumed| {
                    consumed
                        .get("event_code")
                        .and_then(serde_json::Value::as_str)
                        == Some("session.resume.fresh_route.reserved")
                        && consumed
                            .get("detail")
                            .and_then(|detail| detail.get("rejection_event_id"))
                            == event.get("id")
                })
            });
            let rejection_authority_replaced =
                matching_rejection.is_some() && !session_generation_current;
            if let Some(event) = current_rejection {
                if session
                    .get("validation_cell")
                    .and_then(serde_json::Value::as_str)
                    == Some("trip_runtime_probe")
                {
                    continue;
                }
                let detail = event.get("detail").cloned().unwrap_or_default();
                let same_profile_authority = detail
                    .get("same_profile_authority")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                let current_capability_supported =
                    observed_resume_capability_is_current_supported(capabilities, &detail)
                        && observed_resume_profile_is_current(tasks, session, &detail);
                let typed_setup = matches!(
                    session
                        .get("validation_cell")
                        .and_then(serde_json::Value::as_str),
                    Some("trip_setup_discovery" | "trip_setup_probe")
                );
                let category = detail
                    .get("category")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                let reviewer_route = reviewer_fresh_route(tasks, session, &detail);
                let (kind, enabled, reason, owner, operation, accounting_note) = if matches!(
                    category,
                    "provider_compatibility_unsupported"
                        | "provider_compatibility_contract_changed"
                ) {
                    (
                        ContinuationActionKind::ReplaceStaleAuthority,
                        same_profile_authority && current_capability_supported,
                        "The provider compatibility contract needs a reviewed exact version and fresh proof.",
                        "human", "replace_stale_authority",
                        Some("Fresh dispatch requires a current matching contract and proof."),
                    )
                } else if category == "frozen_runtime_identity_changed" {
                    match reviewer_route {
                            ReviewerFreshRoute::FinalFreshOnly => (
                                ContinuationActionKind::AuthorizationRequired,
                                false,
                                "Final verification is fresh-only. Its observed delivery remains spent, so this rejected resume cannot create another final-review call.",
                                "external",
                                "authorization_required",
                                Some("No dashboard action extends final-review accounting or reuses a delivered final verification."),
                            ),
                            ReviewerFreshRoute::AllowanceExhausted if same_profile_authority => (
                                ContinuationActionKind::AuthorizationRequired,
                                false,
                                "The exact delivered review has no remaining allowance for a fresh request.",
                                "external",
                                "authorization_required",
                                Some("No dashboard action extends review allowance; preserve the delivered review and obtain separate authorization before changing it."),
                            ),
                            ReviewerFreshRoute::Stale if same_profile_authority => (
                                ContinuationActionKind::ReplaceStaleAuthority,
                                true,
                                "The review request or its frozen evidence no longer matches this rejected session.",
                                "human",
                                "replace_stale_authority",
                                None,
                            ),
                            ReviewerFreshRoute::Eligible | ReviewerFreshRoute::NotReviewer
                                if same_profile_authority && current_capability_supported => (
                                ContinuationActionKind::FreshAccountedRetry,
                                true,
                                detail
                                    .get("reason")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("The exact native resume is no longer authorized."),
                                "human",
                                if typed_setup {
                                    "trip_setup_dispatch"
                                } else {
                                    "fresh_accounted_retry"
                                },
                                Some("A fresh route consumes the existing role-specific provider or review accounting; it never converts an exact resume silently."),
                            ),
                            _ => (
                                ContinuationActionKind::ReplaceStaleAuthority,
                                true,
                                detail
                                    .get("reason")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("The rejected session no longer has current replacement authority."),
                                "human",
                                "replace_stale_authority",
                                None,
                            ),
                        }
                } else {
                    permanent_resume_rejection_route(category)
                };
                actions.push(continuation(
                    kind,
                    enabled,
                    reason,
                    owner,
                    None,
                    event
                        .get("created_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    None,
                    operation,
                    serde_json::json!({
                        "task_id":session.get("task_id"),
                        "attempt_id":session.get("attempt_id"),
                        "session_id":id,
                        "role_generation_id":session.get("role_generation_id"),
                        "transcript_epoch":session.get("transcript_epoch"),
                        "resume_count":detail.get("resume_count"),
                        "category":detail.get("category"),
                        "rejection_event_id":event.get("id"),
                        "role":session.get("role"),
                        "config_revision":session.get("config_revision"),
                        "attempt_phase":session.get("attempt_phase"),
                        "candidate_hash":session.get("attempt_candidate_hash"),
                        "plan_hash":session.get("attempt_plan_hash"),
                        "setup_permit_id":detail.get("setup_permit_id"),
                        "review_request_id":detail.get("review_request_id"),
                    }),
                    accounting_note,
                ));
            } else if !rejection_authority_replaced
                && session.get("role").and_then(serde_json::Value::as_str) != Some("final_verifier")
                && !restart_candidate_managed
            {
                let quiescent = session
                    .get("process_group_quiescent")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                    || session
                        .get("process_group_quiescent")
                        .and_then(serde_json::Value::as_i64)
                        == Some(1);
                let native_session = session
                    .get("native_session_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty());
                let runtime_probe = session
                    .get("validation_cell")
                    .and_then(serde_json::Value::as_str)
                    == Some("trip_runtime_probe");
                let runtime_admission = session
                    .get("runtime_admission_id")
                    .and_then(serde_json::Value::as_str);
                let production_capability_current = session
                    .get("validation_cell")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|_| {
                        session
                            .get("capability_current")
                            .and_then(serde_json::Value::as_bool)
                            == Some(true)
                    });
                let resumable = quiescent
                    && native_session
                    && production_capability_current
                    && (!runtime_probe || runtime_admission.is_some());
                actions.push(continuation(
                    if resumable {
                        ContinuationActionKind::ExactResume
                    } else {
                        ContinuationActionKind::WaitForExit
                    },
                    resumable,
                    if resumable && runtime_probe {
                        "The retained native runtime probe is eligible only for its exact scoped runtime resume."
                    } else if resumable {
                        "The retained native session is eligible for an exact resume with its frozen identity."
                    } else {
                        "Exact resume remains unavailable until the native identity, process-group quiescence, and current frozen authority are proven."
                    },
                    if resumable { "human" } else { "service" },
                    (!resumable).then_some("verified process-group exit and current scoped authority"),
                    session.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned),
                    None,
                    if resumable {
                        if runtime_probe { "runtime_probe_resume" } else { "role_resume" }
                    } else {
                        "wait_for_exit"
                    },
                    serde_json::json!({"task_id":session.get("task_id"),"attempt_id":session.get("attempt_id"),"session_id":id,"role_generation_id":session.get("role_generation_id"),"transcript_epoch":session.get("transcript_epoch"),"runtime_admission_id":runtime_admission,"role":session.get("role")}),
                    Some(if runtime_probe {
                        "Exact runtime resume reuses only the retained scoped native probe and never creates a fresh provider call."
                    } else {
                        "Exact resume reuses only the retained native conversation and never creates a fresh provider call."
                    }),
                ));
            }
        }
    }
    for decision in decisions.iter().filter(|decision| {
        decision.reason_code.starts_with("restart.")
            && decision.reason_code != "restart.hold_active"
            && decision.subject.session_id.is_some()
    }) {
        let Some(next_action) = decision.next_action.as_ref() else {
            continue;
        };
        let kind = match next_action.operation.as_str() {
            "restart_resume" => ContinuationActionKind::ExactResume,
            "continue" => ContinuationActionKind::ContinueFreshDispatch,
            "wait_for_capacity" => ContinuationActionKind::WaitForCapacity,
            "wait_for_service" => ContinuationActionKind::WaitForService,
            "resolve_recovery" => ContinuationActionKind::RecoverOwnership,
            "refresh_and_reconcile" => ContinuationActionKind::RefreshAndReconcile,
            _ => ContinuationActionKind::TerminalIncomplete,
        };
        let reason = decision
            .primary_blocker
            .as_ref()
            .and_then(|blocker| blocker.message.as_deref())
            .or_else(|| {
                decision
                    .prerequisites
                    .first()
                    .and_then(|prerequisite| prerequisite.message.as_deref())
            })
            .unwrap_or("Recorded restart authority requires revalidation.");
        let owner = match next_action.owner {
            DecisionOwner::Service => "service",
            DecisionOwner::Human => "human",
            DecisionOwner::Provider => "provider",
            DecisionOwner::External => "external",
        };
        actions.push(continuation(
            kind,
            next_action.enabled,
            reason,
            owner,
            (!next_action.enabled).then_some(decision.reason_code.as_str()),
            None,
            None,
            &next_action.operation,
            serde_json::to_value(&next_action.binding).unwrap_or_default(),
            next_action.accounting_note.as_deref(),
        ));
    }
    for control in controls {
        if matches!(
            control.get("state").and_then(serde_json::Value::as_str),
            Some("recovery_required" | "rejected")
        ) && control
            .get("payload")
            .and_then(|payload| payload.get("failure"))
            .is_some()
        {
            actions.push(continuation(ContinuationActionKind::RefreshAndReconcile, true, "The selected control was definitively rejected. Refresh, review its failure, and submit a corrected versioned control; generic ownership recovery is unavailable because this control has no independent exact ownership tuple.", "human", None, control.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned), None, "refresh_and_reconcile", serde_json::json!({"control_id":control.get("id"),"attempt_id":control.get("attempt_id")}), None));
        }
    }
    for switch in switches {
        if matches!(
            switch.get("state").and_then(serde_json::Value::as_str),
            Some("recovery_required" | "rejected")
        ) {
            actions.push(continuation(ContinuationActionKind::RefreshAndReconcile, true, "The role switch was rejected before dispatch. Refresh and submit a corrected, versioned replacement; any separately recorded exact recovery remains on its owning process, check, or workspace action.", "human", None, switch.get("updated_at").and_then(serde_json::Value::as_str).map(str::to_owned), None, "refresh_and_reconcile", serde_json::json!({"switch_intent_id":switch.get("id"),"attempt_id":switch.get("attempt_id")}), None));
        }
    }
    for decision in trip_explorer {
        let extension = decision
            .get("additional_authorization")
            .cloned()
            .unwrap_or_default();
        let attempt_id = decision
            .get("attempt_id")
            .and_then(serde_json::Value::as_str);
        let task = attempt_id.and_then(|attempt_id| {
            tasks.iter().find(|task| {
                task.active_attempt
                    .as_ref()
                    .and_then(|attempt| attempt.get("id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(attempt_id)
            })
        });
        if decision.get("stage").and_then(serde_json::Value::as_str) == Some("rescue")
            && decision
                .get("activated")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            && extension.get("authorized_at").is_none()
            && task.is_some()
        {
            let task = task.expect("checked above");
            actions.push(continuation(ContinuationActionKind::AuthorizeAdditionalExplorer, true, "A bounded rescue Explorer call requires a separate human justification.", "human", None, decision.get("created_at").and_then(serde_json::Value::as_str).map(str::to_owned), None, "authorize_additional_explorer", serde_json::json!({"task_id":task.id,"attempt_id":attempt_id,"expected_task_version":task.version,"stage":"rescue"}), Some("Consumes only the explicitly authorized additional Explorer allowance.")));
        }
    }
    actions
}

/// Rows already read for one snapshot, so attention derives from exactly what
/// the dashboard shows under the same cursor.
struct AttentionSources<'a> {
    projects: &'a [ProjectDto],
    tasks: &'a [TaskDto],
    sessions: &'a [serde_json::Value],
    recovery: &'a [serde_json::Value],
    permission_requests: &'a [PermissionRequestDto],
    trip_setups: &'a [serde_json::Value],
    decisions: &'a [DecisionExplanation],
    continuation_actions: &'a [ContinuationAction],
}

impl AttentionSources<'_> {
    /// Precedence: every pending permission request and open recovery record
    /// is its own item, as is the restore hold. A continuation action bound to
    /// an open recovery record or to an unready project's setup folds into
    /// that item; implementation authorization folds into its attempt's
    /// decision. A task item comes last and is omitted when listed permission
    /// or recovery items already explain its attention.
    fn items(&self) -> Vec<AttentionItem> {
        let mut items: Vec<AttentionItem> = self.permission_items().collect();
        items.extend(self.restore_hold_item());
        items.extend(self.recovery_items());
        items.extend(self.setup_items());
        items.extend(self.continuation_items());
        let task_items = self
            .tasks
            .iter()
            .filter_map(|task| self.task_item(task, &items))
            .collect::<Vec<_>>();
        items.extend(task_items);
        let mut seen = HashSet::new();
        items.retain(|item| seen.insert(item.id.clone()));
        items.sort_by_key(|item| item.category);
        items
    }

    fn permission_items(&self) -> impl Iterator<Item = AttentionItem> + '_ {
        self.permission_requests
            .iter()
            .filter(|request| request.state == "pending")
            .map(|request| AttentionItem {
                id: format!("permission_request:{}", request.id),
                category: AttentionCategory::Permission,
                title: format!("{} · {}", request.task_id, request.tool_name),
                reason: format!(
                    "The {} role ({}) is waiting for a permission decision.",
                    request.role.to_string().replace('_', " "),
                    request.provider
                ),
                target: Some(AttentionTarget::PermissionRequest {
                    project_id: request.project_id.clone(),
                    task_id: request.task_id.clone(),
                    attempt_id: request.attempt_id.clone(),
                    session_id: request.session_id.clone(),
                    request_id: request.id.clone(),
                    request_revision: request.revision,
                }),
                held_tasks: Vec::new(),
            })
    }

    fn restore_hold_item(&self) -> Option<AttentionItem> {
        let hold = self.decisions.iter().find(|decision| {
            decision.subject.recovery_id.as_deref() == Some("database-restore-hold")
        })?;
        let held = |task: &&TaskDto| {
            hold.prerequisites
                .iter()
                .any(|item| json_text(&item.evidence, "task_id") == Some(task.id.as_str()))
        };
        Some(AttentionItem {
            id: "restore_hold".into(),
            category: AttentionCategory::Recovery,
            title: "Instance restore hold · offline release required".into(),
            reason: hold
                .prerequisites
                .iter()
                .filter(|item| item.state != DecisionEvidenceState::Satisfied)
                .map(|item| item.message.as_deref().unwrap_or(&item.code))
                .collect::<Vec<_>>()
                .join("; "),
            target: None,
            held_tasks: self.tasks.iter().filter(held).map(task_target).collect(),
        })
    }

    fn recovery_items(&self) -> impl Iterator<Item = AttentionItem> + '_ {
        self.recovery
            .iter()
            .filter(|record| json_text(record, "state") == Some("attention_required"))
            .filter_map(|record| {
                let recovery_id =
                    json_text(record, "id").filter(|id| *id != "database-restore-hold")?;
                let attempt_id = json_text(record, "attempt_id");
                let (subject, target) = match self.setup_owning_recovery(recovery_id) {
                    Some(setup) => {
                        let project_id = json_text(setup, "project_id").unwrap_or_default();
                        let project = self.project(project_id);
                        (
                            format!(
                                "{} setup",
                                project.map_or(project_id, |item| item.display_name.as_str())
                            ),
                            json_text(setup, "setup_operation_id")
                                .and_then(|id| self.setup_target(id)),
                        )
                    }
                    None => match attempt_id
                        .and_then(|id| Some((id, self.task_with_active_attempt(id)?)))
                    {
                        Some((attempt_id, task)) => (
                            task.id.clone(),
                            Some(AttentionTarget::RecoveryRecord {
                                project_id: task.project_id.clone(),
                                task_id: task.id.clone(),
                                attempt_id: attempt_id.to_owned(),
                                recovery_id: recovery_id.to_owned(),
                            }),
                        ),
                        None => (
                            attempt_id.map_or("Unbound".into(), |id| format!("Attempt {id}")),
                            None,
                        ),
                    },
                };
                let kind = record
                    .get("detail")
                    .and_then(|detail| json_text(detail, "kind"))
                    .unwrap_or("process_ownership");
                let reason = self
                    .continuation_actions
                    .iter()
                    .find(|action| json_text(&action.binding, "recovery_id") == Some(recovery_id))
                    .map_or_else(
                        || {
                            "Durable recovery must be resolved before automatic work can continue."
                                .to_owned()
                        },
                        |action| action.reason.clone(),
                    );
                Some(AttentionItem {
                    id: format!("recovery_record:{recovery_id}"),
                    category: AttentionCategory::Recovery,
                    title: format!("{subject} · {} recovery", kind.replace('_', " ")),
                    reason,
                    target,
                    held_tasks: Vec::new(),
                })
            })
    }

    fn setup_items(&self) -> impl Iterator<Item = AttentionItem> + '_ {
        self.projects
            .iter()
            .filter(|project| json_text(&project.trip, "readiness") != Some("ready"))
            .map(|project| AttentionItem {
                id: format!("project_setup:{}", project.id),
                category: if json_text(&project.trip, "readiness") == Some("recovery_required") {
                    AttentionCategory::Recovery
                } else {
                    AttentionCategory::Compatibility
                },
                title: format!("{} · project setup", project.display_name),
                reason: json_text(&project.trip, "reason")
                    .unwrap_or("Project setup has not been inspected.")
                    .to_owned(),
                target: Some(AttentionTarget::ProjectSetup {
                    project_id: project.id.clone(),
                    setup_operation_id: json_text(&project.trip, "setup_operation_id")
                        .map(str::to_owned),
                }),
                held_tasks: Vec::new(),
            })
    }

    fn continuation_items(&self) -> Vec<AttentionItem> {
        let open_recoveries = self
            .recovery
            .iter()
            .filter(|record| json_text(record, "state") == Some("attention_required"))
            .filter_map(|record| json_text(record, "id"))
            .collect::<HashSet<_>>();
        let unready_projects = self
            .projects
            .iter()
            .filter(|project| json_text(&project.trip, "readiness") != Some("ready"))
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();
        self.continuation_actions
            .iter()
            .filter(|action| {
                action.owner != "service" || action.waiting_for.is_some() || action.enabled
            })
            // Projected only for an attempt awaiting this authorization, which
            // already has its own decision item.
            .filter(|action| {
                !matches!(action.kind, ContinuationActionKind::AuthorizeImplementation)
            })
            .filter(|action| {
                !json_text(&action.binding, "recovery_id")
                    .is_some_and(|id| open_recoveries.contains(id))
            })
            .filter_map(|action| {
                let target = self.continuation_target(action);
                match &target {
                    Some(AttentionTarget::ProjectSetup { project_id, .. })
                        if unready_projects.contains(project_id.as_str()) =>
                    {
                        None
                    }
                    _ => Some(continuation_item(action, target)),
                }
            })
            .collect()
    }

    fn continuation_target(&self, action: &ContinuationAction) -> Option<AttentionTarget> {
        let binding = &action.binding;
        if let Some(setup_operation_id) = json_text(binding, "setup_operation_id") {
            return self.setup_target(setup_operation_id);
        }
        if let Some(session_id) = json_text(binding, "session_id") {
            let session = self
                .sessions
                .iter()
                .find(|session| json_text(session, "id") == Some(session_id))?;
            let Some(task) = json_text(session, "task_id").and_then(|id| self.task(id)) else {
                return json_text(session, "setup_operation_id")
                    .and_then(|id| self.setup_target(id));
            };
            return Some(AttentionTarget::Session {
                project_id: task.project_id.clone(),
                task_id: task.id.clone(),
                attempt_id: json_text(session, "attempt_id")?.to_owned(),
                session_id: session_id.to_owned(),
                role_generation_id: json_text(session, "role_generation_id")?.to_owned(),
            });
        }
        let attempt_id = json_text(binding, "attempt_id");
        let task = json_text(binding, "task_id")
            .and_then(|id| self.task(id))
            .or_else(|| attempt_id.and_then(|id| self.task_with_active_attempt(id)));
        if let Some(task) = task {
            return match attempt_id {
                Some(attempt_id) => attempt_target(task, attempt_id),
                None => Some(AttentionTarget::Task(task_target(task))),
            };
        }
        let project = json_text(binding, "project_id").and_then(|id| self.project(id))?;
        Some(AttentionTarget::ProjectSetup {
            project_id: project.id.clone(),
            setup_operation_id: json_text(&project.trip, "setup_operation_id").map(str::to_owned),
        })
    }

    fn task_item(&self, task: &TaskDto, listed: &[AttentionItem]) -> Option<AttentionItem> {
        let attempt_id = active_attempt_id(task);
        // Acceptance and cancellation end the task but keep the attempt's last
        // phase, which is then history rather than a pending decision.
        let terminal = task.archived || matches!(task.lifecycle.as_str(), "done" | "cancelled");
        let phase = task
            .active_attempt
            .as_ref()
            .and_then(|attempt| json_text(attempt, "phase"))
            .filter(|_| !terminal);
        let task_scope = || Some(AttentionTarget::Task(task_target(task)));
        let (id, category, reason, target) = match (phase, task.attention.as_str()) {
            (Some(phase @ "awaiting_plan_approval"), _) => (
                format!("attempt:{}:{phase}", attempt_id?),
                AttentionCategory::Decision,
                "The reviewed plan is waiting for your approval.".to_owned(),
                attempt_target(task, attempt_id?),
            ),
            (Some(phase @ "awaiting_implementation_authorization"), _) => (
                format!("attempt:{}:{phase}", attempt_id?),
                AttentionCategory::Decision,
                "The approved plan needs a separate implementation authorization.".to_owned(),
                attempt_target(task, attempt_id?),
            ),
            (Some(phase @ "awaiting_human_review"), _) => (
                format!("attempt:{}:{phase}", attempt_id?),
                AttentionCategory::AwaitingAcceptance,
                "The candidate passed its checks and final review and awaits your acceptance."
                    .to_owned(),
                attempt_target(task, attempt_id?),
            ),
            (_, "none") => {
                let decision = self.decisions.iter().find(|decision| {
                    decision.subject.task_id.as_deref() == Some(task.id.as_str())
                        && (task.lifecycle == "ready"
                            || decision.disposition == DecisionDisposition::RetryDeferred)
                        && decision.disposition != DecisionDisposition::Ready
                })?;
                let reason = decision
                    .primary_blocker
                    .as_ref()
                    .and_then(|blocker| blocker.message.clone())
                    .unwrap_or_else(|| decision.reason_code.replace(['.', '_'], " "));
                (
                    format!("task:{}:decision", task.id),
                    AttentionCategory::Blocked,
                    reason,
                    task_scope(),
                )
            }
            (_, attention @ ("needs_recovery" | "restart_parked")) => {
                let explained = listed.iter().any(|item| {
                    item.category == AttentionCategory::Recovery
                        && item.target.as_ref().and_then(attention_task_id)
                            == Some(task.id.as_str())
                });
                if explained {
                    return None;
                }
                (
                    format!("task:{}:{attention}", task.id),
                    AttentionCategory::Recovery,
                    self.task_reason(task),
                    task_scope(),
                )
            }
            (_, attention) => (
                format!("task:{}:{attention}", task.id),
                match attention {
                    "needs_human_review" => AttentionCategory::AwaitingAcceptance,
                    "needs_input" | "needs_review_budget" => AttentionCategory::Decision,
                    _ => AttentionCategory::Blocked,
                },
                self.task_reason(task),
                task_scope(),
            ),
        };
        Some(AttentionItem {
            id,
            category,
            title: format!("{} · {}", task.id, task.title),
            reason,
            target,
            held_tasks: Vec::new(),
        })
    }

    fn task_reason(&self, task: &TaskDto) -> String {
        self.decisions
            .iter()
            .find(|decision| decision.subject.task_id.as_deref() == Some(task.id.as_str()))
            .and_then(|decision| decision.primary_blocker.as_ref()?.message.clone())
            .unwrap_or_else(|| match task.attention.as_str() {
                "needs_input" => "The task needs your input before it can continue.".into(),
                "needs_review_budget" => {
                    "The review allowance is exhausted and needs a human decision.".into()
                }
                "needs_recovery" => {
                    "The task needs recovery before automatic work can continue.".into()
                }
                "restart_parked" => "The task is parked until its restart is resolved.".into(),
                "paused" => "Automatic progress is paused for this task.".into(),
                other => other.replace('_', " "),
            })
    }

    fn task(&self, task_id: &str) -> Option<&TaskDto> {
        self.tasks.iter().find(|task| task.id == task_id)
    }

    fn task_with_active_attempt(&self, attempt_id: &str) -> Option<&TaskDto> {
        self.tasks
            .iter()
            .find(|task| active_attempt_id(task) == Some(attempt_id))
    }

    fn project(&self, project_id: &str) -> Option<&ProjectDto> {
        self.projects
            .iter()
            .find(|project| project.id == project_id)
    }

    fn setup_owning_recovery(&self, recovery_id: &str) -> Option<&serde_json::Value> {
        self.trip_setups.iter().find(|setup| {
            setup
                .get("recoveries")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|row| json_text(row, "record_id") == Some(recovery_id))
                })
        })
    }

    /// The dashboard shows only a project's current setup operation, so an
    /// older operation's binding has no target rather than the newer setup.
    fn setup_target(&self, setup_operation_id: &str) -> Option<AttentionTarget> {
        let setup = self
            .trip_setups
            .iter()
            .find(|setup| json_text(setup, "setup_operation_id") == Some(setup_operation_id))?;
        let project = self.project(json_text(setup, "project_id")?)?;
        (json_text(&project.trip, "setup_operation_id") == Some(setup_operation_id)).then(|| {
            AttentionTarget::ProjectSetup {
                project_id: project.id.clone(),
                setup_operation_id: Some(setup_operation_id.to_owned()),
            }
        })
    }
}

fn continuation_item(
    action: &ContinuationAction,
    target: Option<AttentionTarget>,
) -> AttentionItem {
    let (category, label) = continuation_attention(&action.kind);
    let identity = [
        "session_id",
        "recovery_id",
        "control_id",
        "switch_intent_id",
        "admission_id",
        "setup_operation_id",
        "attempt_id",
        "task_id",
        "project_id",
    ]
    .into_iter()
    .find_map(|key| json_text(&action.binding, key))
    .unwrap_or("instance");
    let subject = match &target {
        Some(AttentionTarget::ProjectSetup { .. }) => Some("project setup"),
        Some(target) => attention_task_id(target),
        None => json_text(&action.binding, "task_id"),
    };
    AttentionItem {
        id: format!("continuation:{}:{identity}", action.operation),
        category,
        title: subject.map_or_else(
            || label.to_owned(),
            |subject| format!("{label} · {subject}"),
        ),
        reason: match &action.waiting_for {
            Some(waiting_for) => format!("{} Waiting for {waiting_for}.", action.reason),
            None => action.reason.clone(),
        },
        target,
        held_tasks: Vec::new(),
    }
}

fn continuation_attention(kind: &ContinuationActionKind) -> (AttentionCategory, &'static str) {
    use AttentionCategory::{Blocked, Compatibility, Decision, Recovery};
    match kind {
        ContinuationActionKind::ExactResume => (Recovery, "Exact resume"),
        ContinuationActionKind::FreshAccountedRetry => (Recovery, "Fresh accounted retry"),
        ContinuationActionKind::ReplaceStaleAuthority => (Recovery, "Replace stale authority"),
        ContinuationActionKind::WaitForExit => (Recovery, "Waiting for exit"),
        ContinuationActionKind::WaitForCapacity => (Blocked, "Waiting for capacity"),
        ContinuationActionKind::WaitForService => (Blocked, "Waiting for service"),
        ContinuationActionKind::RecoverOwnership => (Recovery, "Recover ownership"),
        ContinuationActionKind::RetryGracefulStop => (Recovery, "Retry graceful stop"),
        ContinuationActionKind::ForceStopExactProcess => (Recovery, "Force stop exact process"),
        ContinuationActionKind::PrepareCorrectedRuntime => {
            (Compatibility, "Prepare corrected runtime")
        }
        ContinuationActionKind::AuthorizeImplementation => (Decision, "Authorize implementation"),
        ContinuationActionKind::MigrateAttempt => (Compatibility, "Migrate attempt"),
        ContinuationActionKind::AuthorizeAdditionalExplorer => {
            (Decision, "Authorize additional Explorer")
        }
        ContinuationActionKind::RecoverSetupApply => (Recovery, "Recover setup installation"),
        ContinuationActionKind::RecoverWorkspaceReservation => {
            (Recovery, "Recover workspace reservation")
        }
        ContinuationActionKind::ContinueFreshDispatch => (Recovery, "Continue with fresh dispatch"),
        ContinuationActionKind::StartManagedLegacyAttempt => {
            (Compatibility, "Start managed legacy attempt")
        }
        ContinuationActionKind::RefreshAndReconcile => (Recovery, "Refresh and reconcile"),
        ContinuationActionKind::AuthorizationRequired => (Decision, "Authorization required"),
        ContinuationActionKind::TerminalIncomplete => (Recovery, "Terminal incomplete"),
    }
}

/// Only the task's active attempt has a dashboard view; any other attempt
/// yields no target rather than the newer one.
fn attempt_target(task: &TaskDto, attempt_id: &str) -> Option<AttentionTarget> {
    let attempt = task
        .active_attempt
        .as_ref()
        .filter(|attempt| json_text(attempt, "id") == Some(attempt_id))?;
    Some(AttentionTarget::Attempt {
        project_id: task.project_id.clone(),
        task_id: task.id.clone(),
        attempt_id: attempt_id.to_owned(),
        phase: json_text(attempt, "phase")?.to_owned(),
        plan_hash: json_text(attempt, "plan_hash").map(str::to_owned),
        candidate_hash: json_text(attempt, "candidate_hash").map(str::to_owned),
    })
}

fn task_target(task: &TaskDto) -> TaskAttentionTarget {
    TaskAttentionTarget {
        project_id: task.project_id.clone(),
        task_id: task.id.clone(),
        task_version: task.version,
    }
}

fn attention_task_id(target: &AttentionTarget) -> Option<&str> {
    match target {
        AttentionTarget::Task(target) => Some(&target.task_id),
        AttentionTarget::Attempt { task_id, .. }
        | AttentionTarget::Session { task_id, .. }
        | AttentionTarget::PermissionRequest { task_id, .. }
        | AttentionTarget::RecoveryRecord { task_id, .. } => Some(task_id),
        AttentionTarget::ProjectSetup { .. } => None,
    }
}

fn active_attempt_id(task: &TaskDto) -> Option<&str> {
    task.active_attempt
        .as_ref()
        .and_then(|attempt| json_text(attempt, "id"))
}

fn json_text<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

fn capacity_status(connection: &rusqlite::Connection) -> Result<serde_json::Value> {
    let (occupied, codex, claude, reservations, managers, codex_managers, claude_managers): (i64, i64, i64, i64, i64, i64, i64) =
        connection.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN provider='codex' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN provider='claude' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(is_reservation),0),
                    COALESCE(SUM(CASE WHEN role='manager' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN provider='codex' AND role='manager' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN provider='claude' AND role='manager' THEN 1 ELSE 0 END),0)
             FROM (
               SELECT s.provider AS provider,rg.role AS role,0 AS is_reservation
               FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
               WHERE s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
               UNION ALL
               SELECT CASE WHEN lp.switch_intent_id IS NULL
                         THEN json_extract(rs.config_json,'$.provider')
                         ELSE json_extract(si.config_json,'$.provider') END AS provider,
                      lp.role AS role,1 AS is_reservation
               FROM launch_permits lp
               JOIN attempts a ON a.id=lp.attempt_id
               LEFT JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=lp.role
                 AND rs.revision=lp.settings_revision
               LEFT JOIN switch_intents si ON si.id=lp.switch_intent_id
               WHERE lp.state='issued'
             ) occupied",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
    Ok(serde_json::json!({
        "global_processes":4,
        "active_invocations_per_provider":2,
        "managers_per_provider":1,
        "idle_persistent_managers_count_as_invocations":true,
        "occupied_global":occupied,
        "occupied_by_provider":{"codex":codex,"claude":claude},
        "occupied_managers":managers,
        "occupied_managers_by_provider":{"codex":codex_managers,"claude":claude_managers},
        "issued_reservations":reservations,
        "policy":"up to four occupied invocations; each provider admits two, with at most one manager. Live idle managers, pending reservations, stopping sessions, and recovery-required sessions continue to occupy capacity"
    }))
}

fn resource_observations(connection: &rusqlite::Connection) -> Result<Vec<serde_json::Value>> {
    let recorded = {
        let mut statement=connection.prepare("SELECT sp.session_id,sp.pid,sp.native_start_marker,sp.last_seen_at FROM session_processes sp JOIN sessions s ON s.id=sp.session_id WHERE s.status NOT IN ('exited','launch_failed') ORDER BY sp.session_id,sp.last_seen_at DESC")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as u32,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if recorded.is_empty() {
        return Ok(Vec::new());
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,rss=,%cpu=,lstart="])
        .output();
    let Ok(output) = output else {
        return Ok(recorded.into_iter().map(|(session,pid,_,last)|serde_json::json!({"session_id":session,"pid":pid,"state":"unavailable","last_identity_observed_at":last})).collect());
    };
    if !output.status.success() {
        return Ok(recorded.into_iter().map(|(session,pid,_,last)|serde_json::json!({"session_id":session,"pid":pid,"state":"unavailable","last_identity_observed_at":last})).collect());
    }
    let mut live = std::collections::HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 8 {
            continue;
        }
        if let (Ok(pid), Ok(rss), Ok(cpu)) = (
            fields[0].parse::<u32>(),
            fields[1].parse::<u64>(),
            fields[2].parse::<f64>(),
        ) {
            live.insert(pid, (rss, cpu, fields[3..8].join(" ")));
        }
    }
    Ok(recorded.into_iter().map(|(session,pid,start,last)|match live.get(&pid){Some((rss,cpu,observed))if observed==&start=>serde_json::json!({"session_id":session,"pid":pid,"native_start_marker":start,"state":"observed","rss_bytes":rss*1024,"cpu_percent":cpu,"observed_at":Utc::now().to_rfc3339()}),Some(_)=>serde_json::json!({"session_id":session,"pid":pid,"native_start_marker":start,"state":"stale_pid_reused","last_identity_observed_at":last}),None=>serde_json::json!({"session_id":session,"pid":pid,"native_start_marker":start,"state":"stale_absent","last_identity_observed_at":last})}).collect())
}

pub(crate) fn insert_task_rows(
    transaction: &Transaction<'_>,
    project_id: &str,
    title: &str,
    description: &str,
    criteria: &[String],
    priority: i64,
    role_overrides: &serde_json::Value,
    defaults: Option<&serde_json::Value>,
    ready: bool,
    now: &str,
) -> Result<String> {
    validate_task(title, criteria)?;
    if !role_overrides.is_null() && !role_overrides.is_object() {
        bail!("role_overrides must be a role-keyed JSON object")
    }
    let id = format!(
        "AJ-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8].to_ascii_uppercase()
    );
    transaction.execute(
        "INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,priority,manual_order,lifecycle,attention,version,created_at,updated_at,role_overrides_json,ready_at)
         VALUES(?1,?2,?3,?4,?5,?6,0,?7,'none',1,?8,?8,?9,?10)",
        params![id,project_id,title.trim(),description,serde_json::to_string(criteria)?,priority,
            if ready { "ready" } else { "backlog" },now,serde_json::to_string(role_overrides)?,ready.then_some(now)],
    )?;
    for role in [
        "manager",
        "explorer",
        "plan_reviewer",
        "implementer",
        "code_reviewer",
        "final_verifier",
    ] {
        let selected = role_overrides.get(role).or_else(|| {
            defaults
                .and_then(|value| value.get("roles"))
                .and_then(|roles| roles.get(role))
        });
        let Some(selected) = selected else {
            if ready {
                bail!("Ready tasks require provider, model, and effort settings for role {role}")
            }
            continue;
        };
        let config: crate::domain::RoleOverride = serde_json::from_value(selected.clone())?;
        if config.model.trim().is_empty() || config.effort.trim().is_empty() {
            bail!("role {role} model and effort are required")
        }
        transaction.execute("INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at) VALUES(?1,?2,?3,1,?4,?5)",
            params![uuid::Uuid::new_v4().to_string(),id,role,serde_json::to_string(&config)?,now])?;
    }
    Ok(id)
}

fn validate_task(title: &str, criteria: &[String]) -> Result<()> {
    if title.trim().is_empty() {
        bail!("task title is required")
    }
    if title.chars().count() > 200 {
        bail!("task title is limited to 200 characters")
    }
    if criteria.iter().any(|criterion| criterion.trim().is_empty()) {
        bail!("acceptance criteria cannot contain blank items")
    }
    Ok(())
}

fn task_version(transaction: &Transaction<'_>, task_id: &str, expected: i64) -> Result<()> {
    transaction
        .query_row(
            "SELECT id FROM tasks WHERE id=?1 AND version=?2",
            params![task_id, expected],
            |_| Ok(()),
        )
        .optional()?
        .ok_or_else(|| anyhow!("task version is stale"))
}

fn require_task_state(
    transaction: &Transaction<'_>,
    task_id: &str,
    expected: i64,
    allowed: &[&str],
) -> Result<()> {
    let lifecycle: String = transaction
        .query_row(
            "SELECT lifecycle FROM tasks WHERE id=?1 AND version=?2",
            params![task_id, expected],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("task version is stale"))?;
    if !allowed.contains(&lifecycle.as_str()) {
        bail!("task state {lifecycle} does not allow this action")
    }
    Ok(())
}

fn bump_task(transaction: &Transaction<'_>, task_id: &str, expected: i64, now: &str) -> Result<()> {
    if transaction.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
        params![now, task_id, expected],
    )? != 1
    {
        bail!("task version is stale")
    }
    Ok(())
}

fn apply_review_transition(
    transaction: &Transaction<'_>,
    attempt: &str,
    kind: &str,
    verdict: &str,
    now: &str,
) -> Result<()> {
    let phase = review_transition_phase(kind, verdict)?;
    let clearing = verdict != "approved";
    transaction.execute(
        "UPDATE attempts SET phase=?1,
         plan_hash=CASE WHEN ?2 AND ?3='plan' THEN NULL ELSE plan_hash END,
         plan_approved_at=CASE WHEN ?2 AND ?3='plan' THEN NULL ELSE plan_approved_at END,
         candidate_hash=CASE WHEN ?2 THEN NULL ELSE candidate_hash END,
         accepted_snapshot_id=CASE WHEN ?2 THEN NULL ELSE accepted_snapshot_id END,
         updated_at=?4 WHERE id=?5",
        params![phase, clearing, kind, now, attempt],
    )?;
    if clearing {
        transaction.execute(
            "UPDATE controls SET state='superseded',updated_at=?1 WHERE attempt_id=?2 AND kind='transition_proposal' AND state='proposed'",
            params![now, attempt],
        )?;
    }
    Ok(())
}

pub(crate) fn review_transition_phase(kind: &str, verdict: &str) -> Result<&'static str> {
    Ok(match (kind, verdict) {
        ("plan", "approved") => "awaiting_plan_approval",
        ("plan", "request_changes" | "needs_rework") => "planning",
        ("code", "approved") => "checks",
        ("code", "request_changes") => "implementation",
        ("final", "approved") => "manager_handoff",
        ("final", "request_changes") => "implementation",
        (_, "needs_rework") => "needs_input",
        _ => bail!("invalid review transition"),
    })
}

fn result(
    operation_id: &str,
    entity_kind: &str,
    entity_id: String,
    version: Option<i64>,
    state: &str,
) -> OperationResult {
    OperationResult {
        operation_id: operation_id.to_owned(),
        entity_kind: entity_kind.to_owned(),
        entity_id,
        version,
        state: state.to_owned(),
        detail: serde_json::json!({}),
    }
}

fn parse(value: String) -> serde_json::Value {
    serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value))
}

fn json_rows(
    connection: &rusqlite::Connection,
    sql: &str,
    parameter: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map(params![parameter], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(parse).collect())
}

fn json_rows_no_param(
    connection: &rusqlite::Connection,
    sql: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(parse).collect())
}
