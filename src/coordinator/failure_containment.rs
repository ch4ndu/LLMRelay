use crate::operations::Application;
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

/// Names the attempt whose automatic step failed, attached as error context.
/// The tick holds that attempt; if the hold cannot be recorded, the durable
/// global deferral still points at its task.
#[derive(Debug)]
pub struct TickSubject {
    pub attempt_id: String,
    step: SubjectStep,
    effect: StepEffect,
    causal_identity: serde_json::Value,
}

impl std::fmt::Display for TickSubject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "while advancing attempt {}", self.attempt_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubjectStep {
    SetupStopTimeout,
    CodexStopReconciliation,
    SetupFirstTurnStop,
    ManagerControl,
    AutoResume,
    RetiredRoleInterrupt,
    RoleSwitch,
    GuidanceDelivery,
    ReworkMaterialization,
    StaleReportRetirement,
    SupersededHoldRelease,
    AttemptClassification,
    AttemptAdvance,
    ActionAccounting,
}

impl SubjectStep {
    fn as_str(self) -> &'static str {
        match self {
            Self::SetupStopTimeout => "setup_stop_timeout",
            Self::CodexStopReconciliation => "codex_stop_reconciliation",
            Self::SetupFirstTurnStop => "setup_first_turn_stop",
            Self::ManagerControl => "manager_control",
            Self::AutoResume => "auto_resume",
            Self::RetiredRoleInterrupt => "retired_role_interrupt",
            Self::RoleSwitch => "role_switch",
            Self::GuidanceDelivery => "guidance_delivery",
            Self::ReworkMaterialization => "rework_materialization",
            Self::StaleReportRetirement => "stale_report_retirement",
            Self::SupersededHoldRelease => "superseded_hold_release",
            Self::AttemptClassification => "attempt_classification",
            Self::AttemptAdvance => "attempt_advance",
            Self::ActionAccounting => "action_accounting",
        }
    }
}

/// Whether the failed step may have started an external effect that a repeat
/// could duplicate: a process, signal, terminal input or file change. Database
/// writes alone are re-read by the next evaluation and are not such effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepEffect {
    None,
    Possible,
}

impl StepEffect {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Possible => "possible",
        }
    }
}

/// Attaches the failed step's exact subject. An inner attribution is kept, so
/// the narrowest step that knew the subject names it.
pub(crate) fn subject_failure(
    error: anyhow::Error,
    attempt_id: &str,
    step: SubjectStep,
    effect: StepEffect,
    causal_identity: serde_json::Value,
) -> anyhow::Error {
    if error.downcast_ref::<TickSubject>().is_some() {
        return error;
    }
    error.context(TickSubject {
        attempt_id: attempt_id.to_owned(),
        step,
        effect,
        causal_identity,
    })
}

/// Shared storage or process-table failures say nothing about the selected
/// attempt, so they keep the global deferral instead of holding it.
pub(super) fn shared_infrastructure_failure(error: &anyhow::Error) -> bool {
    use rusqlite::ErrorCode;
    error.chain().any(|cause| {
        cause.is::<crate::supervisor::ProcessInventoryUnavailable>()
            || cause
                .downcast_ref::<rusqlite::Error>()
                .and_then(rusqlite::Error::sqlite_error_code)
                .is_some_and(|code| {
                    matches!(
                        code,
                        ErrorCode::DatabaseBusy
                            | ErrorCode::DatabaseLocked
                            | ErrorCode::OutOfMemory
                            | ErrorCode::PermissionDenied
                            | ErrorCode::ReadOnly
                            | ErrorCode::SystemIoFailure
                            | ErrorCode::DatabaseCorrupt
                            | ErrorCode::DiskFull
                            | ErrorCode::CannotOpen
                            | ErrorCode::FileLockingProtocolFailed
                            | ErrorCode::NotADatabase
                    )
                })
    })
}

/// Ends a tick whose step failed. An exactly attributed failure becomes that
/// attempt's durable hold, so later ticks skip it and serve other work; any
/// other failure keeps the global deferral.
pub(super) fn hold_failed_subject(
    app: &Application,
    error: anyhow::Error,
) -> Result<serde_json::Value> {
    if shared_infrastructure_failure(&error) {
        return Err(error);
    }
    let Some(subject) = error.downcast_ref::<TickSubject>() else {
        return Err(error);
    };
    let cause = format!("{error:#}");
    match record_coordinator_failure(&app.store, subject, &cause) {
        Ok((recovery_id, recorded)) => Ok(serde_json::json!({
            "action":"coordinator_failure_held",
            "attempt_id":subject.attempt_id,
            "recovery_id":recovery_id,
            "operation":subject.step.as_str(),
            "effect_certainty":subject.effect.as_str(),
            "recorded":recorded,
        })),
        Err(record_error) => Err(error.context(format!(
            "the failed step could not be held: {record_error:#}"
        ))),
    }
}

/// Opens, or finds the identical open, `coordinator_failure` recovery record
/// for the attempt. Returns its ID and whether it was created now.
pub(super) fn record_coordinator_failure(
    store: &crate::store::Store,
    subject: &TickSubject,
    cause: &str,
) -> Result<(String, bool)> {
    let failure_key = crate::store::json_hash(&serde_json::json!({
        "operation":subject.step.as_str(),
        "causal_identity":subject.causal_identity,
        "cause":cause,
    }))?;
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let open: Option<String> = transaction
        .query_row(
            "SELECT id FROM recovery_records WHERE attempt_id=?1 AND state='attention_required'
               AND json_extract(detail_json,'$.kind')='coordinator_failure'
               AND json_extract(detail_json,'$.failure_key')=?2",
            params![subject.attempt_id, failure_key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(recovery_id) = open {
        return Ok((recovery_id, false));
    }
    let task_id: String = transaction.query_row(
        "SELECT task_id FROM attempts WHERE id=?1",
        params![subject.attempt_id],
        |row| row.get(0),
    )?;
    let recovery_id = uuid::Uuid::new_v4().to_string();
    let detail = serde_json::json!({
        "kind":"coordinator_failure",
        "task_id":task_id,
        "operation":subject.step.as_str(),
        "causal_identity":subject.causal_identity,
        "effect_certainty":subject.effect.as_str(),
        "cause":cause,
        "failure_key":failure_key,
    });
    transaction.execute(
        "INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at)
         VALUES(?1,NULL,?2,'attention_required',?3,?4,?4)",
        params![
            recovery_id,
            subject.attempt_id,
            detail.to_string(),
            Utc::now().to_rfc3339()
        ],
    )?;
    transaction.commit()?;
    Ok((recovery_id, true))
}

/// The terminal outcome recorded for a held step's exact subject, or NULL.
/// Operations not listed record no subject that can reach one, so their holds
/// wait for a person.
const HELD_SUBJECT_OUTCOME: &str = "CASE
      WHEN json_extract(r.detail_json,'$.operation') IN
        ('retired_role_interrupt','setup_first_turn_stop','setup_stop_timeout',
         'codex_stop_reconciliation','guidance_delivery')
      THEN COALESCE(
        (SELECT 'session_' || s.status FROM sessions s
          WHERE s.id=json_extract(r.detail_json,'$.causal_identity.session_id')
            AND s.status IN ('exited','launch_failed')),
        (SELECT 'guidance_settled' FROM sessions s
          WHERE json_extract(r.detail_json,'$.operation')='guidance_delivery'
            AND s.id=json_extract(r.detail_json,'$.causal_identity.session_id')
            AND NOT EXISTS(SELECT 1 FROM guidance_messages g
              WHERE g.role_generation_id=s.role_generation_id
                AND g.state IN ('queued','delivery_reserved','written_awaiting_submit','delivery_unknown'))))
      WHEN json_extract(r.detail_json,'$.operation')='manager_control'
      THEN (SELECT 'control_' || c.state FROM controls c
          WHERE c.id=json_extract(r.detail_json,'$.causal_identity.control_id')
            AND c.state IN ('superseded','cancelled','rejected','finished','failed'))
      WHEN json_extract(r.detail_json,'$.operation')='role_switch'
      THEN (SELECT 'switch_' || si.state FROM switch_intents si
          WHERE si.id=json_extract(r.detail_json,'$.causal_identity.switch_intent_id')
            AND si.state IN ('rejected','superseded','cancelled'))
      WHEN json_extract(r.detail_json,'$.operation')='auto_resume'
      THEN (SELECT 'restart_' || rc.state FROM restart_candidates rc
          WHERE rc.session_id=json_extract(r.detail_json,'$.causal_identity.session_id')
            AND rc.attempt_id=r.attempt_id AND rc.state IN ('cancelled','released_fresh_dispatch'))
      WHEN json_extract(r.detail_json,'$.operation')='rework_materialization'
      THEN (SELECT 'rework_' || ri.state FROM rework_intents ri
          WHERE ri.new_attempt_id=r.attempt_id AND ri.state IN ('completed','cancelled','failed'))
    END";

/// Releases one held step whose exact subject has reached a terminal outcome,
/// once nothing the attempt started is unsettled. Only that record changes:
/// any other coordinator hold on the attempt stays open and keeps it held.
/// Nothing is re-executed.
pub(super) fn release_one_settled_hold(
    store: &crate::store::Store,
) -> Result<Option<serde_json::Value>> {
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let held: Vec<(String, String, String, String, Option<String>)> = {
        let mut statement = transaction.prepare(&format!(
            "SELECT r.id,r.attempt_id,a.task_id,json_extract(r.detail_json,'$.operation'),
                    {HELD_SUBJECT_OUTCOME}
             FROM recovery_records r JOIN attempts a ON a.id=r.attempt_id
             WHERE r.state='attention_required' AND r.session_id IS NULL
               AND json_extract(r.detail_json,'$.kind')='coordinator_failure'
             ORDER BY r.created_at,r.rowid"
        ))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (recovery_id, attempt, task, operation, outcome) in held {
        let Some(outcome) = outcome else {
            continue;
        };
        // A finished recipient does not repair changed instruction files. Keep
        // the hold stable until the prerequisite that failed is ready again.
        if crate::workflow::failed_step_prerequisite(
            &transaction,
            &attempt,
            Some(operation.as_str()),
        )
        .is_some()
        {
            continue;
        }
        if !crate::workflow::unsettled_effects(&transaction, &attempt)?.is_empty() {
            continue;
        }
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE recovery_records SET state='resolved_retry',resolved_at=?1,updated_at=?1,
                    detail_json=json_set(detail_json,'$.resolution',
                      json_object('decision','subject_outcome','outcome',?2))
             WHERE id=?3 AND state='attention_required'",
            params![now, outcome, recovery_id],
        )?;
        transaction.execute(
            "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2",
            params![now, task],
        )?;
        transaction.commit()?;
        return Ok(Some(serde_json::json!({
            "action":"coordinator_failure_released",
            "attempt_id":attempt,
            "recovery_id":recovery_id,
            "operation":operation,
            "outcome":outcome,
        })));
    }
    Ok(None)
}

/// SQL that is true while the attempt in `attempt_column` has no unresolved
/// coordinator failure. Every automatic selector applies it; human controls
/// deliberately do not.
pub(crate) fn coordinator_hold_absent(attempt_column: &str) -> String {
    format!(
        "NOT EXISTS(SELECT 1 FROM recovery_records coordinator_hold
           WHERE coordinator_hold.attempt_id={attempt_column}
             AND coordinator_hold.state='attention_required'
             AND json_extract(coordinator_hold.detail_json,'$.kind')='coordinator_failure')"
    )
}

pub(super) fn coordinator_hold_open(connection: &Connection, attempt_id: &str) -> Result<bool> {
    Ok(connection.query_row(
        &format!("SELECT NOT {}", coordinator_hold_absent("?1")),
        params![attempt_id],
        |row| row.get(0),
    )?)
}

/// The oldest unresolved coordinator failure holding the attempt.
pub(super) fn open_coordinator_failure(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<(String, Option<String>, Option<String>)>> {
    Ok(connection
        .query_row(
            "SELECT id,json_extract(detail_json,'$.operation'),
                    json_extract(detail_json,'$.effect_certainty')
             FROM recovery_records WHERE attempt_id=?1 AND state='attention_required'
               AND json_extract(detail_json,'$.kind')='coordinator_failure'
             ORDER BY created_at,rowid LIMIT 1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

/// Records that the coordinator could not complete a tick. Every tick retries
/// the same action, so one durable record is written per distinct cause rather
/// than one per second; numbers and identifiers in the cause are ignored when
/// deciding whether it is the same cause. Returns whether a record was written.
pub fn record_tick_deferred(
    store: &crate::store::Store,
    cause: &str,
    attempt_id: Option<&str>,
) -> Result<bool> {
    // The restore hold stops ticks on purpose and is already its own item.
    if store.restore_hold()?.is_some() {
        return Ok(false);
    }
    let signature = format!("{}|{}", attempt_id.unwrap_or(""), deferral_signature(cause));
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let open = latest_tick_outcome(&transaction)?;
    if matches!(&open, Some((code, recorded)) if code == "coordinator.tick.deferred" && recorded.as_deref() == Some(signature.as_str()))
    {
        return Ok(false);
    }
    let now = Utc::now().to_rfc3339();
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','coordinator.tick.deferred','coordinator','coordinator',?3,?4)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),serde_json::json!({"cause":cause,"signature":signature,"attempt_id":attempt_id}).to_string(),now])?;
    transaction.commit()?;
    Ok(true)
}

/// Closes an open deferral after a tick completes again. A no-op when the
/// latest recorded tick outcome is not a deferral, so it is safe to call after
/// every successful tick and after a restart.
pub fn record_tick_recovered(store: &crate::store::Store) -> Result<bool> {
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !matches!(latest_tick_outcome(&transaction)?, Some((code, _)) if code == "coordinator.tick.deferred")
    {
        return Ok(false);
    }
    let now = Utc::now().to_rfc3339();
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','coordinator.tick.recovered','coordinator','coordinator','{}',?3)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),now])?;
    transaction.commit()?;
    Ok(true)
}

/// The open coordinator deferral, if the latest recorded tick outcome is one.
pub(crate) struct TickDeferral {
    pub cause: String,
    pub since: String,
    pub attempt_id: Option<String>,
}

pub(crate) fn open_tick_deferral(connection: &Connection) -> Result<Option<TickDeferral>> {
    Ok(connection
        .query_row(
            "SELECT event_code,json_extract(detail_json,'$.cause'),created_at,
                    json_extract(detail_json,'$.attempt_id') FROM audit_events
             WHERE entity_kind='coordinator' AND entity_id='coordinator'
               AND event_code IN ('coordinator.tick.deferred','coordinator.tick.recovered')
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?
        .and_then(|(code, cause, since, attempt_id)| {
            (code == "coordinator.tick.deferred").then(|| TickDeferral {
                cause: cause.unwrap_or_default(),
                since,
                attempt_id,
            })
        }))
}

fn latest_tick_outcome(connection: &Connection) -> Result<Option<(String, Option<String>)>> {
    Ok(connection
        .query_row(
            "SELECT event_code,json_extract(detail_json,'$.signature') FROM audit_events
             WHERE entity_kind='coordinator' AND entity_id='coordinator'
               AND event_code IN ('coordinator.tick.deferred','coordinator.tick.recovered')
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

/// The cause with every run of digits and every long hexadecimal or UUID-like
/// token replaced, so a retried failure that names a new timestamp or
/// identifier still counts as the same cause.
fn deferral_signature(cause: &str) -> String {
    let mut signature = String::with_capacity(cause.len());
    let mut token = String::new();
    let flush = |token: &mut String, signature: &mut String| {
        let identifier = token.len() >= 8 && token.chars().any(|c| c.is_ascii_digit());
        let number = !token.is_empty() && token.chars().all(|c| c.is_ascii_digit());
        signature.push_str(if identifier || number { "#" } else { token });
        token.clear();
    };
    for character in cause.chars() {
        if character.is_ascii_hexdigit() || character == '-' {
            token.push(character);
        } else {
            flush(&mut token, &mut signature);
            signature.push(character);
        }
    }
    flush(&mut token, &mut signature);
    signature
}

/// A cancelled attempt has no step left to retry, so its coordinator failures
/// close with the cancelling control as their resolution.
pub(super) fn dispose_coordinator_failures(
    tx: &rusqlite::Transaction<'_>,
    attempts: &[&str],
    control_id: &str,
    now: &str,
) -> Result<()> {
    for attempt in attempts {
        tx.execute(
            "UPDATE recovery_records SET state='resolved_cancelled',resolved_at=?1,updated_at=?1,
                    detail_json=json_set(detail_json,'$.resolution',
                      json_object('decision','cancel','control_id',?2))
             WHERE attempt_id=?3 AND state='attention_required'
               AND json_extract(detail_json,'$.kind')='coordinator_failure'",
            params![now, control_id, attempt],
        )?;
    }
    Ok(())
}
