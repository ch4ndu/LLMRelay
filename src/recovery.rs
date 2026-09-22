use crate::store::Store;
use anyhow::{bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension};

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
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT s.id,rg.attempt_id,a.task_id,s.status,s.desired_running,s.validation_cell,
                    s.native_session_id,EXISTS(
                      SELECT 1 FROM capabilities current_capability
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
                        AND current_capability.proof_json!='{}'
                    ) AND s.capability_identity_json IS NOT NULL,rg.role,rg.status,a.status,
                    a.phase,t.lifecycle,t.attention,t.archived_at,
                    a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id ORDER BY latest.created_at DESC LIMIT 1),
                    NOT EXISTS(SELECT 1 FROM role_generations newer WHERE newer.attempt_id=a.id AND newer.role=rg.role AND newer.lane_id=rg.lane_id AND newer.generation>rg.generation),
                    EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=t.id AND rs.role=rg.role AND rs.revision=rg.config_revision AND rs.effective_generation_id=rg.id)
                      OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                        SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id AND lg.effective_generation_id=rg.id)),
                    CASE WHEN rg.role IN ('explorer','plan_reviewer','code_reviewer','final_verifier') THEN EXISTS(
                      SELECT 1 FROM review_requests r WHERE r.session_id=s.id AND r.role_generation_id=rg.id AND r.delivery_state='delivered') ELSE 1 END
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE (s.desired_running=1 OR s.status IN ('launch_reserved','running'))
               AND s.status!='interrupt_requested'",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, bool>(15)?,
                    row.get::<_, bool>(16)?,
                    row.get::<_, bool>(17)?,
                    row.get::<_, bool>(18)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut results = Vec::new();
    for (
        session,
        attempt,
        task,
        status,
        desired,
        validation,
        native,
        native_supported,
        role,
        generation_status,
        attempt_status,
        phase,
        lifecycle,
        attention,
        archived,
        latest,
        current_generation,
        current_config,
        current_review,
    ) in rows
    {
        let source = if desired {
            "planned_shutdown"
        } else {
            "unclean_shutdown"
        };
        let reason = if validation.is_some() && attempt_status == "capability_validation" {
            Some("isolated validation sessions are never restored")
        } else if native.as_deref().is_none_or(str::is_empty) || !native_supported {
            Some("same-generation native history or current Supported evidence for the complete frozen capability identity is unavailable")
        } else if !latest {
            Some("session does not belong to the latest task attempt")
        } else if archived.is_some()
            || matches!(lifecycle.as_str(), "awaiting_review" | "done" | "cancelled")
        {
            Some("task is awaiting human review, terminal, cancelled, or archived")
        } else if !matches!(attention.as_str(), "none" | "restart_parked") {
            Some("paused, input-required, recovery, or otherwise held work is never restarted")
        } else if !matches!(attempt_status.as_str(), "running" | "restart_parked") {
            Some("attempt is not previously running")
        } else if !matches!(
            generation_status.as_str(),
            "running" | "exited" | "launch_reserved" | "stopping"
        ) || !current_generation
        {
            Some("role generation was replaced or is stale")
        } else if !current_config {
            Some("saved role configuration or authorization is stale")
        } else if !current_review {
            Some("review is complete, replaced, stale, or not durably delivered")
        } else if !matches!(
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
        ) {
            Some("workflow phase is not resumable")
        } else {
            None
        };
        let state = if reason.is_some() {
            "skipped"
        } else if status == "exited" {
            "parked"
        } else {
            "pending_reconciliation"
        };
        let previous_state: Option<String> = transaction
            .query_row(
                "SELECT state FROM restart_candidates WHERE session_id=?1",
                params![session],
                |row| row.get(0),
            )
            .optional()?;
        let state = match (previous_state.as_deref(), state) {
            (Some("failed"), _) => "failed",
            (Some("queued_capacity"), "parked") => "queued_capacity",
            (_, state) => state,
        };
        let persisted_reason = if matches!(state, "failed" | "queued_capacity") {
            transaction.query_row(
                "SELECT reason FROM restart_candidates WHERE session_id=?1",
                params![session],
                |row| row.get::<_, String>(0),
            )?
        } else {
            reason
                .unwrap_or("eligible exact native binding is parked")
                .to_owned()
        };
        transaction.execute(
            "INSERT INTO restart_candidates(session_id,attempt_id,task_id,source,state,reason,result_json,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)
             ON CONFLICT(session_id) DO UPDATE SET source=excluded.source,state=excluded.state,reason=excluded.reason,
               result_json=CASE WHEN restart_candidates.state=excluded.state AND excluded.state IN ('failed','queued_capacity')
                                THEN restart_candidates.result_json ELSE excluded.result_json END,
               updated_at=excluded.updated_at",
            params![session,attempt,task,source,state,persisted_reason,serde_json::json!({"role":role,"prior_status":status}).to_string(),now],
        )?;
        if validation.is_none()
            && attempt_status == "running"
            && attention == "none"
            && archived.is_none()
            && !matches!(lifecycle.as_str(), "awaiting_review" | "done" | "cancelled")
        {
            transaction.execute(
                "UPDATE attempts SET status='restart_parked',updated_at=?1 WHERE id=?2",
                params![now, attempt],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='restart_parked',updated_at=?1 WHERE id=?2",
                params![now, task],
            )?;
        }
        if state == "skipped" {
            transaction.execute(
                "UPDATE sessions SET desired_running=0 WHERE id=?1",
                params![session],
            )?;
        } else if state != "parked" {
            transaction.execute(
                "UPDATE sessions SET desired_running=1 WHERE id=?1",
                params![session],
            )?;
        }
        results.push(serde_json::json!({"session_id":session,"attempt_id":attempt,"task_id":task,"source":source,"state":state}));
    }
    transaction.commit()?;
    Ok(results)
}

pub fn reconcile_restart_candidates(store: &Store) -> Result<Vec<serde_json::Value>> {
    let pending = {
        let connection = store.lock()?;
        let mut statement = connection.prepare("SELECT session_id FROM restart_candidates WHERE state='pending_reconciliation' ORDER BY created_at")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut results = Vec::new();
    for session in pending {
        match verify_session_quiescent(store, &session) {
            Ok(verified) => {
                let now = Utc::now().to_rfc3339();
                let mut connection = store.lock()?;
                let tx = connection.transaction()?;
                let (attempt, task): (String, String) = tx.query_row(
                    "SELECT attempt_id,task_id FROM restart_candidates WHERE session_id=?1",
                    params![session],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
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
                tx.execute("UPDATE restart_candidates SET state='parked',reason='machine-verified quiescent; exact native resume is available',result_json=?1,updated_at=?2 WHERE session_id=?3",params![verified.to_string(),now,session])?;
                tx.commit()?;
                results.push(serde_json::json!({"session_id":session,"state":"parked","verification":verified}));
            }
            Err(error) => {
                let reason = format!("{error:#}");
                let connection = store.lock()?;
                connection.execute("UPDATE restart_candidates SET state='blocked',reason=?1,result_json=?2,updated_at=?3 WHERE session_id=?4",params![reason,serde_json::json!({"verification":"uncertain_or_occupied"}).to_string(),Utc::now().to_rfc3339(),session])?;
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
    let connection = store.lock()?;
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
        connection.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at) SELECT ?1,?2,?3,'attention_required',?4,?5,?6,?6 WHERE NOT EXISTS(SELECT 1 FROM recovery_records WHERE session_id=?2 AND state='attention_required')",params![uuid::Uuid::new_v4().to_string(),id,attempt,identity,serde_json::json!({"observation":state,"launch_state":launch_state,"replacement_allowed":false,"descendant_state":"unknown_until_explicit_reconciliation"}).to_string(),now])?;
        results.push(serde_json::json!({"session_id":id,"attempt_id":attempt,"state":state,"replacement_allowed":false}));
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

pub fn verify_session_quiescent(store: &Store, session: &str) -> Result<serde_json::Value> {
    let (recorded, group_anchor, anchor, launch_boot) = {
        let connection = store.lock()?;
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
        (rows, group, anchor, boot)
    };
    let inventory = process_inventory()?;
    let evidence = verify_generation_absent(
        "session",
        session,
        &recorded,
        group_anchor,
        anchor.as_deref(),
        launch_boot.as_deref(),
        &inventory,
    )?;
    Ok(serde_json::json!({"session_id":session,"verification":evidence}))
}

#[derive(Clone, Debug)]
pub(crate) enum SessionProcessObservation {
    Quiescent(serde_json::Value),
    Live(serde_json::Value),
    Uncertain(String),
}

/// Classifies one durable session generation from fresh operating-system
/// evidence. This is used after a service restart, when no in-memory child
/// handle exists and database status alone cannot decide whether a stop timed
/// out or the process exited at the deadline.
pub(crate) fn observe_session_processes(
    store: &Store,
    session: &str,
) -> Result<SessionProcessObservation> {
    let (mut recorded, group_anchor, anchor_json, launch_boot, process_json) = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT pid,native_start_marker,process_group_id FROM session_processes
             WHERE session_id=?1",
        )?;
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
        let state = connection.query_row(
            "SELECT recovery_process_group_id,recovery_anchor_json,launch_boot_identity,process_identity_json
             FROM sessions WHERE id=?1",
            params![session],
            |row| {
                Ok((
                    row.get::<_, Option<i32>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )?;
        (rows, state.0, state.1, state.2, state.3)
    };
    if let Some(process_json) = process_json {
        match serde_json::from_str::<crate::domain::ProcessIdentity>(&process_json) {
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
                return Ok(SessionProcessObservation::Uncertain(format!(
                    "session {session} has malformed exact process identity: {error}"
                )))
            }
        }
    }
    let anchor = match anchor_json
        .as_deref()
        .map(serde_json::from_str::<crate::domain::ProcessGenerationAnchor>)
        .transpose()
    {
        Ok(anchor) => anchor,
        Err(error) => {
            return Ok(SessionProcessObservation::Uncertain(format!(
                "session {session} has malformed generation anchor: {error}"
            )))
        }
    };
    let inventory = match process_inventory() {
        Ok(inventory) => inventory,
        Err(error) => {
            return Ok(SessionProcessObservation::Uncertain(format!(
                "operating-system process inventory was unavailable: {error:#}"
            )))
        }
    };
    let current_boot = match crate::supervisor::system_boot_identity() {
        Ok(boot) => boot,
        Err(error) => {
            return Ok(SessionProcessObservation::Uncertain(format!(
                "operating-system boot identity was unavailable: {error:#}"
            )))
        }
    };
    let recorded_boot = anchor
        .as_ref()
        .map(|value| value.boot_identity.as_str())
        .or(launch_boot.as_deref());
    if recorded_boot
        .is_some_and(|boot| crate::supervisor::boot_identity_proves_reboot(boot, &current_boot))
    {
        return Ok(SessionProcessObservation::Quiescent(serde_json::json!({
            "session_id":session,
            "verification":{
                "source":"operating_system_boot_changed",
                "recorded_boot":recorded_boot,
                "current_boot":current_boot,
                "recorded_identities":recorded.len()
            }
        })));
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
        return Ok(SessionProcessObservation::Live(serde_json::json!({
            "source":"operating_system_exact_process_inventory",
            "session_id":session,
            "live_processes":live
        })));
    }
    match verify_generation_absent_evidence(
        "session",
        session,
        &recorded,
        group_anchor,
        anchor.as_ref(),
        launch_boot.as_deref(),
        &current_boot,
        &inventory,
    ) {
        Ok(verification) => Ok(SessionProcessObservation::Quiescent(
            serde_json::json!({"session_id":session,"verification":verification}),
        )),
        Err(error) => Ok(SessionProcessObservation::Uncertain(format!("{error:#}"))),
    }
}

pub fn verify_attempt_quiescent(store: &Store, attempt: &str) -> Result<serde_json::Value> {
    let (sessions, checks) = {
        let connection = store.lock()?;
        let mut statement=connection.prepare("SELECT DISTINCT session_id FROM recovery_records WHERE attempt_id=?1 AND session_id IS NOT NULL AND state='attention_required'")?;
        let rows = statement
            .query_map(params![attempt], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut check_statement=connection.prepare("SELECT DISTINCT json_extract(detail_json,'$.check_id') FROM recovery_records WHERE attempt_id=?1 AND session_id IS NULL AND state='attention_required' AND json_extract(detail_json,'$.check_id') IS NOT NULL")?;
        let checks = check_statement
            .query_map(params![attempt], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        (rows, checks)
    };
    if sessions.is_empty() && checks.is_empty() {
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
    Ok(
        serde_json::json!({"attempt_id":attempt,"sessions":session_evidence,"checks":check_evidence}),
    )
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

fn process_inventory() -> Result<Vec<(u32, i32, String)>> {
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
