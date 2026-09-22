use crate::domain::RoleKind;
use crate::operations::Application;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

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
    let mut visible_wait = None;
    for attempt in active_attempts(app)? {
        if attempt.attention != "none" || has_restart_hold(app, &attempt.id)? {
            continue;
        }
        if let Some(control) = manager_control_hold(app, &attempt.id)? {
            if visible_wait.is_none() {
                visible_wait = Some(control);
            }
            continue;
        }
        let readiness = {
            let connection = app.store.lock()?;
            crate::trip::require_attempt_ready(&connection, &attempt.id, None)
        };
        if let Err(error) = readiness {
            return Ok(
                serde_json::json!({"action":"held","attempt_id":attempt.id,"for":"project_trip_readiness","reason":format!("{error:#}")}),
            );
        }
        if let Some(value) = consume_blocked_result(app, &attempt.id)? {
            return Ok(value);
        }
        let advanced = match attempt.phase.as_str() {
            "planning" => advance_planning(app, &attempt),
            "plan_review" => advance_review(app, &attempt, "plan"),
            "awaiting_plan_approval" => Ok(waiting(&attempt, "human_plan_approval")),
            "awaiting_implementation_authorization" => {
                Ok(waiting(&attempt, "human_implementation_authorization"))
            }
            "implementation" => advance_implementation(app, &attempt),
            "code_review" => advance_review(app, &attempt, "code"),
            "checks" => advance_checks(app, &attempt),
            "final_review" => advance_review(app, &attempt, "final"),
            "manager_handoff" => advance_handoff(app, &attempt),
            "awaiting_human_review" => Ok(waiting(&attempt, "human_accept_or_rework")),
            other => Ok(serde_json::json!({"action":"held","attempt_id":attempt.id,"phase":other})),
        };
        let value = match advanced {
            Ok(value) => value,
            Err(error) if format!("{error:#}").contains("capacity") => continue,
            Err(error) if format!("{error:#}").contains("capability") => {
                set_attention(app, &attempt.task_id, &attempt.id, "blocked")?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if is_committed_action(&value) {
            mark_attempt_served(app, &attempt.id)?;
            return Ok(value);
        }
        if visible_wait.is_none()
            && matches!(
                value.get("for").and_then(|value| value.as_str()),
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
        {
            visible_wait = Some(value);
        }
    }
    Ok(match app.scheduler.claim_next()? {
        Some(plan) => {
            serde_json::json!({"action":"workspace_created","task_id":plan.task_id,"attempt_id":plan.attempt_id})
        }
        None => visible_wait.unwrap_or_else(|| serde_json::json!({"action":"idle"})),
    })
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

fn manager_control_hold(app: &Application, attempt: &str) -> Result<Option<serde_json::Value>> {
    let connection = app.store.lock()?;
    let hold: Option<(String, String, String)> = connection
        .query_row(
            "SELECT kind,state,payload_json FROM controls WHERE attempt_id=?1
           AND kind IN ('manager_stop','manager_change')
           AND state NOT IN ('finished','cancelled','superseded','rejected')
         ORDER BY created_at LIMIT 1",
            params![attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(hold.map(|(kind, state, payload)| {
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
        serde_json::json!({
            "action":"held",
            "attempt_id":attempt,
            "for":"manager_control",
            "kind":kind,
            "state":state,
            "next_action":payload.get("next_action").cloned().unwrap_or(serde_json::Value::Null),
        })
    }))
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
    phase: String,
    attention: String,
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

fn active_attempts(app: &Application) -> Result<Vec<Attempt>> {
    let connection = app.store.lock()?;
    let mut statement = connection.prepare(
        "SELECT a.id,a.task_id,a.phase,t.attention,t.title,t.description,t.acceptance_criteria_json,a.plan_hash,a.candidate_hash
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         WHERE a.status IN ('running','workspace_reserved','needs_input') AND t.lifecycle IN ('in_progress','validation','awaiting_review')
         ORDER BY COALESCE(a.last_coordinator_at,''),a.created_at")?;
    let rows = statement
        .query_map([], |row| {
            Ok(Attempt {
                id: row.get(0)?,
                task_id: row.get(1)?,
                phase: row.get(2)?,
                attention: row.get(3)?,
                title: row.get(4)?,
                description: row.get(5)?,
                criteria: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
                plan_hash: row.get(7)?,
                candidate_hash: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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

fn advance_planning(app: &Application, attempt: &Attempt) -> Result<serde_json::Value> {
    if attempt.plan_hash.is_none() {
        if has_result(app, &attempt.id, "manager", "plan_ready")? {
            let snapshot = app.reviews.freeze(&attempt.id, "plan")?;
            return Ok(
                serde_json::json!({"action":"plan_frozen","attempt_id":attempt.id,"snapshot":snapshot}),
            );
        }
        let accepted_busy_plan = {
            let connection = app.store.lock()?;
            crate::store::eligible_manager_plan(&connection, &attempt.id, false)?.and_then(
                |candidate| {
                    (candidate.session_status == "running"
                        && candidate.readiness_state != "idle_candidate")
                        .then_some(candidate.session_id)
                },
            )
        };
        if let Some(session) = accepted_busy_plan {
            app.interrupt_completed_role(&session)?;
            return Ok(serde_json::json!({
                "action":"quiescing_completed_role",
                "role":"manager",
                "session_id":session,
                "completion":"current_plan_ready"
            }));
        }
        if active_role(app, &attempt.id, "manager")? {
            return Ok(waiting(attempt, "current_manager_plan"));
        }
        let prompt = crate::workflow_resources::render(
            RoleKind::Manager,
            &serde_json::json!({"task_id":attempt.task_id,"attempt_id":attempt.id,"phase":attempt.phase,"title":attempt.title,"description":attempt.description,"acceptance_criteria":attempt.criteria,"required_outcome":"plan_ready"}),
        )?;
        let (launch, resumed) = dispatch_or_resume_manager(app, &attempt.id, &prompt)?;
        return Ok(serde_json::json!({
            "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
            "attempt_id":attempt.id,
            "session_id":launch.session_id
        }));
    }
    if let Some(value) = apply_manager_proposal(app, attempt, "plan_review")? {
        return Ok(value);
    }
    Ok(waiting(attempt, "manager_plan_and_transition"))
}

fn advance_implementation(app: &Application, attempt: &Attempt) -> Result<serde_json::Value> {
    if let Some(value) = apply_manager_proposal(app, attempt, "code_review")? {
        return Ok(value);
    }
    if let Some(session) = manager_interrupt_pending(app, &attempt.id)? {
        return Ok(serde_json::json!({
            "action":"waiting",
            "for":"manager_service_stop_quiescence",
            "attempt_id":attempt.id,
            "session_id":session
        }));
    }
    if !active_role(app, &attempt.id, "manager")? {
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
        return Ok(serde_json::json!({
            "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
            "attempt_id":attempt.id,
            "session_id":launch.session_id,
            "context":"fresh_implementation_attempt"
        }));
    }
    if attempt.candidate_hash.is_none() {
        let reviewed_lane_count = {
            let connection = app.store.lock()?;
            crate::trip::reviewed_parallel_lane_count(&connection, &attempt.id)?
        };
        let lane_state: Option<(i64, i64, Option<String>, Option<String>)> = {
            let connection = app.store.lock()?;
            let configured: i64 = connection.query_row(
                "SELECT COUNT(*) FROM implementation_lanes WHERE attempt_id=?1",
                params![attempt.id],
                |row| row.get(0),
            )?;
            if configured == 0 {
                None
            } else {
                crate::trip::require_configured_lanes_match_reviewed(&connection, &attempt.id)?;
                let unyielded:i64=connection.query_row("SELECT COUNT(*) FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state!='yielded'",params![attempt.id],|row|row.get(0))?;
                let request:Option<(String,String)>=connection.query_row("SELECT id,capsule_json FROM trip_integration_requests WHERE attempt_id=?1 AND state IN ('requested','dispatched')",params![attempt.id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
                Some((
                    configured,
                    unyielded,
                    request.as_ref().map(|value| value.0.clone()),
                    request.map(|value| value.1),
                ))
            }
        };
        if lane_state.is_none() {
            if let Some(reviewed_lanes) = reviewed_lane_count {
                return Ok(
                    serde_json::json!({"action":"waiting","for":"manager_lane_admission","attempt_id":attempt.id,"reviewed_lanes":reviewed_lanes,"configured_lanes":0}),
                );
            }
        }
        if let Some((configured, unyielded, request_id, capsule)) = &lane_state {
            if let Some(value) = quiesce_yielded_lane_writer(app, attempt)? {
                return Ok(value);
            }
            if *unyielded != 0 {
                if let Some(dispatch) = next_implementation_lane(app, attempt)? {
                    let prompt = crate::workflow_resources::render(
                        RoleKind::Implementer,
                        &dispatch.prompt_context,
                    )?;
                    match app.dispatch_implementation_lane(&attempt.id, &dispatch.lane_key, &prompt)
                    {
                        Ok(launch) => {
                            return Ok(serde_json::json!({
                                "action":"implementation_lane_dispatched",
                                "attempt_id":attempt.id,
                                "lane_key":dispatch.lane_key,
                                "session_id":launch.session_id,
                                "invocation":"initial",
                                "profile_session":dispatch.profile_session
                            }));
                        }
                        Err(error) if format!("{error:#}").contains("capacity") => {
                            let waiting = implementation_lane_waiting(app, &attempt.id)?;
                            return Ok(serde_json::json!({
                                "action":"waiting",
                                "for":"implementation_lane_capacity",
                                "attempt_id":attempt.id,
                                "configured_lanes":configured,
                                "unyielded_lanes":unyielded,
                                "lane_status":waiting
                            }));
                        }
                        Err(error) => return Err(error),
                    }
                }
                let waiting = implementation_lane_waiting(app, &attempt.id)?;
                return Ok(
                    serde_json::json!({"action":"waiting","for":"required_lane_yields","attempt_id":attempt.id,"configured_lanes":configured,"unyielded_lanes":unyielded,"lane_status":waiting}),
                );
            }
            let notice = "All required implementation lanes have accepted persisted yield receipts and their exact current writers are positively quiescent. The exact ordered integration request can now proceed through request-integration; do not fabricate a request, report, or approval.";
            if queue_manager_notice(app, attempt, notice, true)? {
                return Ok(serde_json::json!({
                    "action":"manager_integration_ready_notified",
                    "attempt_id":attempt.id
                }));
            }
            if request_id.is_none() {
                return Ok(
                    serde_json::json!({"action":"waiting","for":"manager_integration_request","attempt_id":attempt.id,"configured_lanes":configured}),
                );
            }
            if !has_result(app, &attempt.id, "implementer", "candidate_ready")?
                && !active_role(app, &attempt.id, "implementer")?
            {
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
                    &serde_json::json!({"task_id":attempt.task_id,"attempt_id":attempt.id,"phase":attempt.phase,"title":attempt.title,"description":attempt.description,"acceptance_criteria":attempt.criteria,"plan_hash":attempt.plan_hash,"integration_capsule":serde_json::from_str::<serde_json::Value>(capsule.as_deref().ok_or_else(||anyhow!("integration capsule is missing"))?)?,"lane_receipts":lane_receipts,"required_outcome":"candidate_ready","integration_only":true}),
                )?;
                let launch =
                    app.dispatch_attempt_role(&attempt.id, RoleKind::Implementer, &prompt)?;
                let connection = app.store.lock()?;
                let exact_request_id = request_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("integration request is missing"))?;
                let changed=connection.execute("UPDATE trip_integration_requests SET state='dispatched',dispatched_at=?1 WHERE id=?2 AND state='requested'",params![Utc::now().to_rfc3339(),exact_request_id])?;
                if changed != 1 {
                    bail!("manager integration request changed during dispatch")
                }
                return Ok(
                    serde_json::json!({"action":"integration_implementer_dispatched","attempt_id":attempt.id,"session_id":launch.session_id,"integration_request_id":exact_request_id}),
                );
            }
        }
        if has_result(app, &attempt.id, "implementer", "candidate_ready")? {
            if let Some(session) = running_result_session(app, &attempt.id, "implementer")? {
                app.supervisor.interrupt(&session)?;
                return Ok(
                    serde_json::json!({"action":"quiescing_completed_role","role":"implementer","session_id":session}),
                );
            }
            let snapshot = app.reviews.freeze(&attempt.id, "candidate")?;
            queue_manager_notice(app,attempt,"Implementation candidate is frozen. Inspect the evidence and propose phase code_review.",false)?;
            return Ok(
                serde_json::json!({"action":"candidate_frozen","attempt_id":attempt.id,"snapshot":snapshot}),
            );
        }
        if lane_state.is_none() && !active_role(app, &attempt.id, "implementer")? {
            let feedback = latest_rework_feedback(app, &attempt.id)?;
            let prompt = crate::workflow_resources::render(
                RoleKind::Implementer,
                &serde_json::json!({"task_id":attempt.task_id,"attempt_id":attempt.id,"phase":attempt.phase,"title":attempt.title,"description":attempt.description,"acceptance_criteria":attempt.criteria,"rework_feedback":feedback,"required_outcome":"candidate_ready"}),
            )?;
            let launch = app.dispatch_attempt_role(&attempt.id, RoleKind::Implementer, &prompt)?;
            return Ok(
                serde_json::json!({"action":"implementer_dispatched","attempt_id":attempt.id,"session_id":launch.session_id}),
            );
        }
    }
    if attempt.candidate_hash.is_some() {
        queue_manager_notice(
            app,
            attempt,
            "Implementation candidate is frozen. Inspect the evidence and propose phase code_review.",
            false,
        )?;
        if let Some(value) =
            quiesce_manager_for_obligation(app, attempt, "current_frozen_candidate_code_review")?
        {
            return Ok(value);
        }
    }
    Ok(waiting(attempt, "implementation_or_manager_transition"))
}

fn quiesce_yielded_lane_writer(
    app: &Application,
    attempt: &Attempt,
) -> Result<Option<serde_json::Value>> {
    let row = {
        let connection = app.store.lock()?;
        let invalid: Option<String> = connection
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
        if let Some(lane) = invalid {
            return Ok(Some(serde_json::json!({
                "action":"waiting",
                "for":"accepted_lane_yield_receipts",
                "attempt_id":attempt.id,
                "lane_key":lane
            })));
        }
        connection
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
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
    };
    let Some((lane, session, generation_status, session_status)) = row else {
        return Ok(None);
    };
    if generation_status == "running" && session_status == "running" {
        app.interrupt_completed_role(&session)?;
        return Ok(Some(serde_json::json!({
            "action":"yielded_lane_interrupt",
            "attempt_id":attempt.id,
            "lane_key":lane,
            "session_id":session
        })));
    }
    Ok(Some(serde_json::json!({
        "action":"waiting",
        "for":"yielded_lane_quiescence",
        "attempt_id":attempt.id,
        "lane_key":lane,
        "session_id":session,
        "generation_status":generation_status,
        "session_status":session_status
    })))
}

fn next_implementation_lane(app: &Application, attempt: &Attempt) -> Result<Option<LaneDispatch>> {
    let connection = app.store.lock()?;
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

fn advance_review(app: &Application, attempt: &Attempt, kind: &str) -> Result<serde_json::Value> {
    let role = match kind {
        "plan" => "plan_reviewer",
        "code" => "code_reviewer",
        "final" => "final_verifier",
        _ => bail!("unknown review kind"),
    };
    if let Some(session) = running_result_session(app, &attempt.id, role)? {
        app.supervisor.interrupt(&session)?;
        return Ok(
            serde_json::json!({"action":"quiescing_completed_role","role":role,"session_id":session}),
        );
    }
    if let Some(value) = consume_review_result(app, attempt, kind)? {
        return Ok(value);
    }
    let prompt = crate::workflow_resources::render(
        role.parse().map_err(|error: String| anyhow!(error))?,
        &serde_json::json!({"task_id":attempt.task_id,"attempt_id":attempt.id,"phase":attempt.phase,"review_kind":kind,"plan_hash":attempt.plan_hash,"candidate_hash":attempt.candidate_hash,"title":attempt.title,"description":attempt.description,"acceptance_criteria":attempt.criteria}),
    )?;
    let handoff = serde_json::json!({"task_id":attempt.task_id,"attempt_id":attempt.id,"phase":attempt.phase,"plan_hash":attempt.plan_hash,"candidate_hash":attempt.candidate_hash});
    let request = app
        .reviews
        .reserve_request(&attempt.id, kind, &prompt, handoff)?;
    match request.state.as_str() {
        "reserved" | "nondelivered" => {
            let launch = app.dispatch_reserved_review(&request)?;
            Ok(
                serde_json::json!({"action":"review_dispatched","kind":kind,"request_id":request.request_id,"session_id":launch.session_id}),
            )
        }
        "delivered" | "launching" => Ok(
            serde_json::json!({"action":"waiting","for":"review_result","request_id":request.request_id}),
        ),
        "ambiguous" => Ok(
            serde_json::json!({"action":"held","reason":"review_delivery_ambiguous","request_id":request.request_id}),
        ),
        state => Ok(
            serde_json::json!({"action":"held","reason":format!("review_request_{state}"),"request_id":request.request_id}),
        ),
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
    let row:Option<(String,String,String,String,String)>=transaction.query_row(
        "SELECT r.id,rr.id,rr.outcome,rr.summary,rr.metadata_json FROM review_requests r
         JOIN role_results rr ON rr.role_generation_id=r.role_generation_id AND rr.session_id=r.session_id
         JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN sessions s ON s.id=rr.session_id
         WHERE r.attempt_id=?1 AND r.review_kind=?2 AND r.delivery_state='delivered' AND rg.role=?3
           AND s.status='exited'
           AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
           AND json_extract(rr.metadata_json,'$.review_kind')=r.review_kind
           AND json_extract(rr.metadata_json,'$.candidate_hash')=r.candidate_hash
         ORDER BY rr.created_at LIMIT 1",params![attempt.id,kind,expected_role],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
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

fn advance_checks(app: &Application, attempt: &Attempt) -> Result<serde_json::Value> {
    let candidate = attempt
        .candidate_hash
        .as_deref()
        .ok_or_else(|| anyhow!("checks require a frozen candidate"))?;
    let blocked_check: Option<(String, String)> = {
        let connection = app.store.lock()?;
        connection.query_row(
            "SELECT COALESCE(check_id,suite_name),status FROM check_runs WHERE attempt_id=?1 AND candidate_hash=?2
             AND selected_check_revision=(SELECT selected_checks_revision FROM attempts WHERE id=?1)
             AND (status IN ('launch_ambiguous','launch_failed','precondition_failed') OR launch_state='capture_failed'
               OR (status='finished' AND COALESCE(exit_code,-1)!=0))
             ORDER BY created_at DESC LIMIT 1",
            params![attempt.id,candidate],
            |row| Ok((row.get(0)?,row.get(1)?)),
        ).optional()?
    };
    if let Some((suite, status)) = blocked_check {
        set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
        return Ok(
            serde_json::json!({"action":"held","reason":"check_failure","suite":suite,"status":status}),
        );
    }
    let next: Option<String> = {
        let connection = app.store.lock()?;
        connection.query_row(
        "SELECT s.check_id FROM trip_selected_checks s JOIN attempts a ON a.id=s.attempt_id
         WHERE s.attempt_id=?1 AND s.revision=a.selected_checks_revision AND s.required=1
           AND NOT EXISTS(SELECT 1 FROM check_runs cr WHERE cr.attempt_id=s.attempt_id AND cr.candidate_hash=?2
             AND cr.check_id=s.check_id AND cr.selected_check_revision=s.revision AND cr.status='finished'
             AND cr.exit_code=0 AND cr.freshness_state='current')
         ORDER BY s.check_id LIMIT 1",params![attempt.id,candidate],|row|row.get(0)).optional()?
    };
    if let Some(check_id) = next {
        let authorized = app.checks.selected_authorized(&attempt.id, &check_id)?;
        if !authorized {
            set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
            return Ok(
                serde_json::json!({"action":"held","reason":"selected_check_requires_service_permission","check_id":check_id}),
            );
        }
        let result = app.checks.run_selected(&attempt.id, &check_id)?;
        if result.get("passed").and_then(|v| v.as_bool()) != Some(true) {
            set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
        }
        return Ok(
            serde_json::json!({"action":"check_finished","check_id":check_id,"result":result}),
        );
    }
    let (configured, failed): (i64, bool) = {
        let connection = app.store.lock()?;
        connection.query_row(
        "SELECT (SELECT COUNT(*) FROM trip_selected_checks s JOIN attempts selected ON selected.id=s.attempt_id
                   WHERE s.attempt_id=?1 AND s.revision=selected.selected_checks_revision AND s.required=1),
          EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND candidate_hash=?2 AND status IN ('running','recovery_required'))",
        params![attempt.id,candidate],|row|Ok((row.get(0)?,row.get(1)?)))?
    };
    if configured == 0 {
        set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
        return Ok(serde_json::json!({"action":"held","reason":"no_plan_selected_trip_checks"}));
    }
    if failed {
        return Ok(serde_json::json!({"action":"held","reason":"check_failure"}));
    }
    let conformance_ready: bool = {
        let connection = app.store.lock()?;
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts a JOIN trip_conformance_receipts c
               ON c.attempt_id=a.id AND c.revision=a.manager_conformance_revision
               AND c.candidate_hash=a.candidate_hash WHERE a.id=?1)
             AND NOT EXISTS(SELECT 1 FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state!='yielded')
             AND NOT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role='implementer'
               AND status IN ('launch_reserved','running','stopping'))",
            params![attempt.id],|row|row.get(0)
        )?
    };
    if !conformance_ready {
        if let Some(session) = manager_interrupt_pending(app, &attempt.id)? {
            return Ok(serde_json::json!({
                "action":"waiting",
                "for":"manager_service_stop_quiescence",
                "attempt_id":attempt.id,
                "session_id":session
            }));
        }
        if !active_role(app, &attempt.id, "manager")? {
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
            return Ok(serde_json::json!({
                "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                "attempt_id":attempt.id,
                "session_id":launch.session_id,
                "context":"checks_conformance"
            }));
        }
        queue_manager_notice(
            app,
            attempt,
            "Required checks are current. Load current role context and submit exact candidate-bound manager conformance after confirming every required lane is yielded and writer-quiescent. Do not fabricate evidence or clear a human hold.",
            false,
        )?;
        if let Some(value) =
            quiesce_manager_for_obligation(app, attempt, "current_checks_conformance")?
        {
            return Ok(value);
        }
        set_attention(app, &attempt.task_id, &attempt.id, "needs_input")?;
        return Ok(
            serde_json::json!({"action":"held","reason":"manager_conformance_or_lane_yield_missing"}),
        );
    }
    let final_explorer_ready = {
        let connection = app.store.lock()?;
        connection.query_row(
            "SELECT activated=0 OR outcome_json IS NOT NULL FROM trip_explorer_decisions
             WHERE attempt_id=?1 AND stage='final' AND candidate_hash=?2 ORDER BY created_at DESC LIMIT 1",
            params![attempt.id,candidate],|row|row.get::<_,bool>(0)
        ).optional()?
    };
    match final_explorer_ready {
        None => {
            if let Some(session) = manager_interrupt_pending(app, &attempt.id)? {
                return Ok(serde_json::json!({
                    "action":"waiting",
                    "for":"manager_service_stop_quiescence",
                    "attempt_id":attempt.id,
                    "session_id":session
                }));
            }
            if !active_role(app, &attempt.id, "manager")? {
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
                return Ok(serde_json::json!({
                    "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
                    "attempt_id":attempt.id,
                    "session_id":launch.session_id,
                    "context":"checks_final_explorer_decision"
                }));
            }
            queue_manager_notice(
                app,
                attempt,
                "Manager conformance is current. Load current role context and record the exact candidate-bound final Explorer activation decision. Do not bypass an activated Explorer or any human authorization gate.",
                false,
            )?;
            if let Some(value) =
                quiesce_manager_for_obligation(app, attempt, "current_final_explorer_decision")?
            {
                return Ok(value);
            }
            return Ok(
                serde_json::json!({"action":"waiting","for":"manager_final_explorer_decision","attempt_id":attempt.id,"candidate_hash":candidate}),
            );
        }
        Some(false) => {
            return Ok(
                serde_json::json!({"action":"waiting","for":"activated_final_explorer_evidence","attempt_id":attempt.id,"candidate_hash":candidate}),
            )
        }
        Some(true) => {}
    }
    if let Some(wait) =
        wait_for_completed_manager_boundary(app, attempt, "completed_checks_and_final_explorer")?
    {
        return Ok(wait);
    }
    if !consume_completed_checks(app, attempt, candidate)? {
        return Ok(serde_json::json!({
            "action":"waiting",
            "for":"completed_checks_manager_boundary_recheck",
            "attempt_id":attempt.id,
            "candidate_hash":candidate
        }));
    }
    Ok(
        serde_json::json!({"action":"checks_approved","attempt_id":attempt.id,"phase":"final_review"}),
    )
}

fn advance_handoff(app: &Application, attempt: &Attempt) -> Result<serde_json::Value> {
    if exact_handoff_ready(app, attempt)? {
        if let Some(wait) =
            wait_for_completed_manager_boundary(app, attempt, "completed_final_review_handoff")?
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
                "snapshot_reusable":true
            }));
        }
        return Ok(
            serde_json::json!({"action":"human_review_ready","attempt_id":attempt.id,"snapshot":snapshot}),
        );
    }
    if let Some(session) = manager_interrupt_pending(app, &attempt.id)? {
        return Ok(serde_json::json!({
            "action":"waiting",
            "for":"manager_service_stop_quiescence",
            "attempt_id":attempt.id,
            "session_id":session
        }));
    }
    if !active_role(app, &attempt.id, "manager")? {
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
        return Ok(serde_json::json!({
            "action":if resumed {"manager_resumed"} else {"manager_dispatched"},
            "attempt_id":attempt.id,
            "session_id":launch.session_id,
            "context":"final_handoff"
        }));
    }
    let final_review: String = {
        let connection = app.store.lock()?;
        connection.query_row("SELECT json_object('final_review_request_id',id,'candidate_hash',candidate_hash,'reviewer_generation_id',role_generation_id,'verdict',verdict,'delivery_state',delivery_state) FROM review_requests WHERE attempt_id=?1 AND review_kind='final' AND candidate_hash=?2 AND verdict='approved' AND delivery_state='finished' ORDER BY updated_at DESC LIMIT 1",params![attempt.id,attempt.candidate_hash],|row|row.get(0))?
    };
    queue_manager_notice(app,attempt,&format!("Final review approved. Read context and report handoff_ready using this exact approved tuple: {final_review}. Inspect the exact candidate and checks before proposing phase human_review."),false)?;
    if let Some(value) =
        quiesce_manager_for_obligation(app, attempt, "current_final_review_handoff")?
    {
        return Ok(value);
    }
    Ok(waiting(attempt, "manager_handoff"))
}

fn apply_manager_proposal(
    app: &Application,
    attempt: &Attempt,
    target: &str,
) -> Result<Option<serde_json::Value>> {
    let now = Utc::now().to_rfc3339();
    let mut connection = app.store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let proposal:Option<(String,String)>=transaction.query_row(
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
         ORDER BY c.created_at LIMIT 1",params![attempt.id,target],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
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
            if let Err(error) = finish_continue_with_restart_hold(app, &id, &task, &attempt) {
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
fn has_restart_hold(app: &Application, attempt: &str) -> Result<bool> {
    let connection = app.store.lock()?;
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM restart_candidates
         WHERE attempt_id=?1 AND state NOT IN ('resumed','released_fresh_dispatch','cancelled'))",
        params![attempt],
        |row| row.get(0),
    )?)
}
fn active_role(app: &Application, attempt: &str, role: &str) -> Result<bool> {
    let connection = app.store.lock()?;
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role=?2 AND status IN ('launch_reserved','running','stopping'))",params![attempt,role],|row|row.get(0))?)
}

fn manager_interrupt_pending(app: &Application, attempt: &str) -> Result<Option<String>> {
    let connection = app.store.lock()?;
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

fn wait_for_completed_manager_boundary(
    app: &Application,
    attempt: &Attempt,
    obligation: &str,
) -> Result<Option<serde_json::Value>> {
    let current: Option<(String, String, String, String, bool)> = {
        let connection = app.store.lock()?;
        connection
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
            .optional()?
    };
    let Some((session, generation_status, session_status, readiness, quiescent)) = current else {
        return Ok(Some(serde_json::json!({
            "action":"waiting",
            "for":"current_manager_completion_boundary",
            "attempt_id":attempt.id,
            "obligation":obligation
        })));
    };
    if generation_status == "running"
        && session_status == "running"
        && readiness == "idle_candidate"
    {
        let credential_current: bool = {
            let connection = app.store.lock()?;
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_credentials credential
                   ON credential.role_generation_id=s.role_generation_id
                   WHERE s.id=?1 AND credential.revoked_at IS NULL)",
                params![session],
                |row| row.get(0),
            )?
        };
        if credential_current {
            return Ok(None);
        }
    }
    if generation_status == "exited" && session_status == "exited" && quiescent {
        return Ok(None);
    }
    if session_status == "interrupt_requested" {
        return Ok(Some(serde_json::json!({
            "action":"waiting",
            "for":"manager_service_stop_quiescence",
            "attempt_id":attempt.id,
            "session_id":session,
            "obligation":obligation
        })));
    }
    if generation_status == "running"
        && session_status == "running"
        && readiness == "busy_unresolved_hook_work"
    {
        if let Some(value) = quiesce_manager_for_obligation(app, attempt, obligation)? {
            return Ok(Some(value));
        }
    }
    Ok(Some(serde_json::json!({
        "action":"waiting",
        "for":if session_status == "exited" && !quiescent {
            "manager_quiescence_or_recovery"
        } else {
            "manager_safe_completion_boundary"
        },
        "attempt_id":attempt.id,
        "session_id":session,
        "obligation":obligation,
        "manager_generation_status":generation_status,
        "manager_session_status":session_status,
        "manager_readiness":readiness,
        "process_group_quiescent":quiescent
    })))
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
fn has_result(app: &Application, attempt: &str, role: &str, outcome: &str) -> Result<bool> {
    let connection = app.store.lock()?;
    if role == "manager" && outcome == "plan_ready" {
        return Ok(crate::store::eligible_manager_plan(&connection, attempt, true)?.is_some());
    }
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role=rg.role WHERE rg.attempt_id=?1 AND rg.role=?2 AND rr.outcome=?3 AND rr.consumed_at IS NULL AND rr.created_at>=a.updated_at)",params![attempt,role,outcome],|row|row.get(0))?)
}
fn running_result_session(app: &Application, attempt: &str, role: &str) -> Result<Option<String>> {
    let connection = app.store.lock()?;
    Ok(connection.query_row("SELECT s.id FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id JOIN sessions s ON s.id=rr.session_id JOIN attempts a ON a.id=rg.attempt_id JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role=rg.role WHERE rg.attempt_id=?1 AND rg.role=?2 AND s.status='running' AND rr.consumed_at IS NULL AND rr.created_at>=a.updated_at ORDER BY rr.created_at DESC LIMIT 1",params![attempt,role],|row|row.get(0)).optional()?)
}
fn exact_handoff_ready(app: &Application, attempt: &Attempt) -> Result<bool> {
    let Some(candidate) = attempt.candidate_hash.as_deref() else {
        return Ok(false);
    };
    let connection = app.store.lock()?;
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

fn finish_continue_with_restart_hold(
    app: &Application,
    id: &str,
    task: &str,
    attempt: &str,
) -> Result<()> {
    let mut connection = app.store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
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
    Ok(())
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
    let row:Option<(String,String,String)>=transaction.query_row("SELECT rr.id,rr.outcome,rr.summary FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id WHERE rg.attempt_id=?1 AND rr.outcome IN ('blocked','needs_input') AND rr.consumed_at IS NULL ORDER BY rr.created_at DESC LIMIT 1",params![attempt],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
    if let Some((result_id, outcome, summary)) = row {
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE attempts SET status='needs_input',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        transaction.execute("UPDATE tasks SET attention='needs_input',updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
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
fn waiting(attempt: &Attempt, for_what: &str) -> serde_json::Value {
    serde_json::json!({"action":"waiting","attempt_id":attempt.id,"phase":attempt.phase,"for":for_what})
}
