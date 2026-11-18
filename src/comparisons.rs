use crate::domain::{
    ComparisonInvocationTiming, ComparisonRunTiming, ComparisonTaskTiming, ComparisonTimingState,
    TaskLifecycle,
};
use crate::store::{read_state_revision, Store};
use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use rusqlite::{params, Connection, TransactionBehavior};
use std::collections::BTreeSet;

struct TimingSession {
    id: String,
    generation: String,
    epoch: String,
    status: String,
    launch_state: String,
    resume_count: i64,
    process_json: Option<String>,
}

struct TimingResume {
    ordinal: i64,
    epoch: String,
    prior_epoch: Option<String>,
    state: String,
    process_json: Option<String>,
}

struct Boundary {
    id: String,
    epoch: String,
    process_hash: String,
    kind: String,
    observed_at: String,
    conflicted: bool,
}

struct TaskTimingSummary {
    state: ComparisonTimingState,
    first_covered: bool,
    first_start: Option<DateTime<FixedOffset>>,
    elapsed: Option<i64>,
    reason: Option<&'static str>,
}

pub fn task_timing(store: &Store, task_id: &str) -> Result<ComparisonTaskTiming> {
    crate::providers::codex_app_server::bounded(task_id, 128, "task ID")?;
    let mut connection = store.lock()?;
    let snapshot = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let timing = task_timing_in(&snapshot, task_id)?;
    snapshot.commit()?;
    Ok(timing)
}

fn task_timing_in(connection: &Connection, task_id: &str) -> Result<ComparisonTaskTiming> {
    let revision = read_state_revision(connection)?;
    let lifecycle: String = connection
        .query_row(
            "SELECT t.lifecycle FROM tasks t JOIN projects p ON p.id=t.project_id
         WHERE t.id=?1 AND p.internal_purpose IS NULL",
            [task_id],
            |row| row.get(0),
        )
        .context("read comparison timing task")?;
    let lifecycle: TaskLifecycle = serde_json::from_value(serde_json::Value::String(lifecycle))
        .context("parse comparison task lifecycle")?;
    let attempts = {
        let mut statement = connection.prepare(
            "SELECT id,human_acceptance_at FROM attempts WHERE task_id=?1 ORDER BY created_at,id",
        )?;
        let rows = statement
            .query_map([task_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let runs = attempts
        .iter()
        .map(|(id, _)| run_timing_in(connection, id))
        .collect::<Result<Vec<_>>>()?;
    let acceptance = exact_acceptance(attempts.iter().filter_map(|(_, at)| at.as_deref()));
    let summary = summarize_task_timing(lifecycle, acceptance, runs.iter());
    Ok(ComparisonTaskTiming {
        task_id: task_id.into(),
        state_revision: revision,
        state: summary.state,
        first_start_coverage_complete: summary.first_covered,
        earliest_known_observed_start: summary.first_start.map(|stamp| stamp.to_rfc3339()),
        human_acceptance_at: acceptance.map(str::to_owned),
        elapsed_to_acceptance_millis: summary.elapsed,
        unavailable_reason: summary.reason.map(str::to_owned),
        runs,
    })
}

fn exact_acceptance<'a>(mut accepted: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let first = accepted.next();
    if accepted.next().is_none() {
        first
    } else {
        None
    }
}

fn summarize_task_timing<'a>(
    lifecycle: TaskLifecycle,
    acceptance: Option<&str>,
    runs: impl ExactSizeIterator<Item = &'a ComparisonRunTiming> + Clone,
) -> TaskTimingSummary {
    let first_start = runs
        .clone()
        .flat_map(|run| &run.invocations)
        .filter_map(|invocation| invocation.observed_start.as_deref().and_then(timestamp))
        .min();
    let has_runs = runs.len() != 0;
    let first_covered = has_runs && runs.clone().all(|run| run.first_start_coverage_complete);
    let lifecycle_timing = match lifecycle {
        TaskLifecycle::Backlog
        | TaskLifecycle::Ready
        | TaskLifecycle::InProgress
        | TaskLifecycle::Validation
        | TaskLifecycle::AwaitingReview => Some((
            if has_runs {
                ComparisonTimingState::Ongoing
            } else {
                ComparisonTimingState::NeverExecuted
            },
            None,
            Some("no_human_acceptance"),
        )),
        TaskLifecycle::Cancelled => Some((
            ComparisonTimingState::Unavailable,
            None,
            Some("no_human_acceptance"),
        )),
        TaskLifecycle::Done => None,
    };
    let (state, elapsed, reason) = if let Some(lifecycle_state) = lifecycle_timing {
        lifecycle_state
    } else if !first_covered {
        (
            ComparisonTimingState::Unavailable,
            None,
            Some("incomplete_first_execution_coverage"),
        )
    } else if acceptance.is_none() {
        (
            ComparisonTimingState::Unavailable,
            None,
            Some("missing_or_ambiguous_human_acceptance"),
        )
    } else {
        match first_start.zip(acceptance.and_then(timestamp)) {
            Some((start, end))
                if end >= start
                    && runs
                        .flat_map(|run| &run.invocations)
                        .filter_map(|invocation| {
                            invocation.observed_start.as_deref().and_then(timestamp)
                        })
                        .all(|observed| observed <= end) =>
            {
                (
                    ComparisonTimingState::Complete,
                    Some((end - start).num_milliseconds()),
                    None,
                )
            }
            Some(_) | None => (
                ComparisonTimingState::Unavailable,
                None,
                Some("invalid_acceptance_interval"),
            ),
        }
    };
    TaskTimingSummary {
        state,
        first_covered,
        first_start,
        elapsed,
        reason,
    }
}

fn run_timing_in(connection: &Connection, attempt: &str) -> Result<ComparisonRunTiming> {
    let sessions = {
        let mut statement = connection.prepare(
            "SELECT s.id,g.id,s.transcript_epoch,s.status,s.launch_state,s.resume_count,
                    s.process_identity_json
             FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
             WHERE g.attempt_id=?1 ORDER BY s.created_at,s.id",
        )?;
        let rows = statement
            .query_map([attempt], |row| {
                Ok(TimingSession {
                    id: row.get(0)?,
                    generation: row.get(1)?,
                    epoch: row.get(2)?,
                    status: row.get(3)?,
                    launch_state: row.get(4)?,
                    resume_count: row.get(5)?,
                    process_json: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut invocations = Vec::new();
    for session in sessions {
        session_timing_in(connection, &session, &mut invocations)?;
    }
    let mut known_millis = 0i64;
    let mut complete_epochs = 0;
    let mut missing_epochs = usize::from(invocations.is_empty());
    let mut ongoing_epochs = 0;
    let mut never_executed_epochs = 0;
    let mut first_start_coverage_complete = !invocations.is_empty();
    for invocation in &invocations {
        match invocation.state {
            ComparisonTimingState::Complete => {
                complete_epochs += 1;
                known_millis = known_millis
                    .checked_add(
                        invocation
                            .elapsed_millis
                            .context("complete invocation timing has no interval")?,
                    )
                    .context("observed execution interval sum overflow")?;
            }
            ComparisonTimingState::Ongoing => ongoing_epochs += 1,
            ComparisonTimingState::Unavailable => missing_epochs += 1,
            ComparisonTimingState::NeverExecuted => never_executed_epochs += 1,
        }
        if invocation.state != ComparisonTimingState::NeverExecuted {
            first_start_coverage_complete &= invocation
                .observed_start
                .as_deref()
                .and_then(timestamp)
                .is_some()
                && (invocation.state != ComparisonTimingState::Unavailable
                    || invocation.unavailable_reason.as_deref() == Some("missing_execution_end"));
        }
    }
    Ok(ComparisonRunTiming {
        attempt_id: attempt.into(),
        complete_observed_interval_millis: (complete_epochs > 0
            && missing_epochs == 0
            && ongoing_epochs == 0)
            .then_some(known_millis),
        known_partial_interval_millis: (complete_epochs > 0).then_some(known_millis),
        complete_epochs,
        missing_epochs,
        ongoing_epochs,
        never_executed_epochs,
        first_start_coverage_complete,
        invocations,
    })
}

fn session_timing_in(
    connection: &Connection,
    session: &TimingSession,
    invocations: &mut Vec<ComparisonInvocationTiming>,
) -> Result<()> {
    let resumes = {
        let mut statement = connection.prepare(
            "SELECT resume_ordinal,transcript_epoch,prior_transcript_epoch,state,process_identity_json
             FROM resume_invocations WHERE session_id=?1 ORDER BY resume_ordinal",
        )?;
        let rows = statement
            .query_map([&session.id], |row| {
                Ok(TimingResume {
                    ordinal: row.get(0)?,
                    epoch: row.get(1)?,
                    prior_epoch: row.get(2)?,
                    state: row.get(3)?,
                    process_json: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let boundaries = {
        let mut statement = connection.prepare(
            "SELECT b.id,b.transcript_epoch,b.process_identity_hash,b.kind,b.observed_at,
                EXISTS(SELECT 1 FROM workflow_execution_boundary_conflicts c WHERE c.boundary_id=b.id)
             FROM workflow_execution_boundaries b
             WHERE b.session_id=?1 AND b.role_generation_id=?2 ORDER BY b.transcript_epoch,b.id",
        )?;
        let rows = statement
            .query_map(params![session.id, session.generation], |row| {
                Ok(Boundary {
                    id: row.get(0)?,
                    epoch: row.get(1)?,
                    process_hash: row.get(2)?,
                    kind: row.get(3)?,
                    observed_at: row.get(4)?,
                    conflicted: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut epochs = BTreeSet::from([session.epoch.as_str()]);
    for resume in &resumes {
        epochs.insert(&resume.epoch);
        if let Some(prior) = &resume.prior_epoch {
            epochs.insert(prior);
        }
    }
    for boundary in &boundaries {
        epochs.insert(&boundary.epoch);
    }
    let history_complete = session.resume_count >= 0
        && resumes.len() as i64 == session.resume_count
        && resumes.iter().enumerate().all(|(index, resume)| {
            resume.ordinal == index as i64 + 1 && resume.prior_epoch.is_some()
        });
    if !history_complete {
        let mut missing = empty_invocation(session, None);
        missing.unavailable_reason = Some("incomplete_invocation_history".into());
        invocations.push(missing);
    }
    for epoch in epochs {
        let facts: Vec<_> = boundaries
            .iter()
            .filter(|fact| fact.epoch == epoch)
            .collect();
        let resume = resumes.iter().find(|resume| resume.epoch == epoch);
        let never_executed = facts.is_empty()
            && match resume {
                Some(resume) => {
                    resume.state == "proven_nondelivery" && resume.process_json.is_none()
                }
                None => {
                    epoch == session.epoch
                        && session.resume_count == 0
                        && session.status == "launch_reserved"
                        && session.launch_state == "reserved"
                        && session.process_json.is_none()
                }
            };
        let active = epoch == session.epoch
            && matches!(session.status.as_str(), "running" | "interrupt_requested");
        let mut invocation = invocation_timing(session, epoch, &facts, active);
        if facts.is_empty() && history_complete && session.resume_count == 0 {
            if let Some(legacy) = legacy_current_invocation(connection, session, epoch)? {
                invocation = legacy;
            }
        }
        if never_executed {
            invocation.state = ComparisonTimingState::NeverExecuted;
            invocation.unavailable_reason = None;
        }
        invocations.push(invocation);
    }
    Ok(())
}

fn empty_invocation(session: &TimingSession, epoch: Option<&str>) -> ComparisonInvocationTiming {
    ComparisonInvocationTiming {
        evidence_basis: None,
        session_id: session.id.clone(),
        role_generation_id: session.generation.clone(),
        transcript_epoch: epoch.map(str::to_owned),
        process_identity_hash: None,
        start_boundary_id: None,
        end_boundary_id: None,
        observed_start: None,
        observed_end: None,
        elapsed_millis: None,
        state: ComparisonTimingState::Unavailable,
        unavailable_reason: None,
    }
}

fn invocation_timing(
    session: &TimingSession,
    epoch: &str,
    facts: &[&Boundary],
    active: bool,
) -> ComparisonInvocationTiming {
    let mut timing = empty_invocation(session, Some(epoch));
    timing.evidence_basis = (!facts.is_empty()).then(|| "persisted_execution_boundaries".into());
    let identities: BTreeSet<_> = facts
        .iter()
        .map(|fact| fact.process_hash.as_str())
        .collect();
    if identities.len() > 1 || facts.iter().any(|fact| fact.conflicted) {
        timing.unavailable_reason = Some("conflicting_execution_boundaries".into());
        return timing;
    }
    timing.process_identity_hash = identities.first().map(|hash| (*hash).to_owned());
    let start = facts.iter().find(|fact| fact.kind == "started");
    let end = facts.iter().find(|fact| fact.kind == "ended");
    timing.start_boundary_id = start.map(|fact| fact.id.clone());
    timing.end_boundary_id = end.map(|fact| fact.id.clone());
    timing.observed_start = start.map(|fact| fact.observed_at.clone());
    timing.observed_end = end.map(|fact| fact.observed_at.clone());
    let reason = match (start, end) {
        (Some(start), Some(end)) => {
            match timestamp(&start.observed_at).zip(timestamp(&end.observed_at)) {
                Some((start, end)) if end >= start => {
                    timing.elapsed_millis = Some((end - start).num_milliseconds());
                    timing.state = ComparisonTimingState::Complete;
                    None
                }
                Some(_) | None => Some("invalid_execution_interval"),
            }
        }
        (Some(start), None) if timestamp(&start.observed_at).is_some() => {
            if active {
                timing.state = ComparisonTimingState::Ongoing;
            }
            Some("missing_execution_end")
        }
        (Some(_), None) => Some("invalid_execution_start"),
        (None, Some(_)) | (None, None) => Some("missing_execution_start"),
    };
    timing.unavailable_reason = reason.map(str::to_owned);
    timing
}

fn timestamp(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

use crate::domain::{
    ComparisonAttribution, ComparisonAttributionState, ComparisonCheckState, ComparisonCohort,
    ComparisonCounts, ComparisonDimension, ComparisonDurationSummary, ComparisonEvidenceFact,
    ComparisonEvidenceMetric, ComparisonEvidencePage, ComparisonEvidenceQuery,
    ComparisonEvidenceRow, ComparisonExecutionIdentity, HistoricalSourceKey, HistorySourceKind,
    ObservedUsageStatus, WorkflowComparisonGroup, WorkflowComparisonPage, WorkflowComparisonQuery,
};
use anyhow::{anyhow, bail};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;

const COMPARISON_RESPONSE_BYTES: usize = 256 * 1024;
const COMPARISON_PAGE_ROWS: usize = 50;

struct ComparisonTask {
    id: String,
    project: String,
    created_at: String,
    lifecycle: String,
    archived: bool,
    runs: Vec<ComparisonRun>,
    recipe_attribution: Option<crate::domain::ComparisonRecipeAttribution>,
}

struct ComparisonRun {
    id: String,
    created_at: String,
    status: String,
    accepted_at: Option<String>,
    workflow: Option<String>,
    preset: Option<String>,
    candidate: Option<String>,
    selection_revision: i64,
    executions: Vec<ComparisonExecution>,
    recipe_attribution: Option<crate::domain::ComparisonRecipeAttribution>,
}

struct ComparisonExecution {
    generation: String,
    session: String,
    epoch: String,
    identity: ComparisonExecutionIdentity,
}

struct ComparisonUnit<'a> {
    task: &'a ComparisonTask,
    runs: Vec<&'a ComparisonRun>,
    id: &'a str,
    attribution: ComparisonAttribution,
    group: String,
}

struct ComparisonReads<'a> {
    connection: &'a Connection,
    timings: BTreeMap<String, ComparisonRunTiming>,
    usage_ownership_checked: bool,
}

impl<'a> ComparisonReads<'a> {
    fn new(transaction: &'a rusqlite::Transaction<'_>) -> Self {
        Self {
            connection: transaction,
            timings: BTreeMap::new(),
            usage_ownership_checked: false,
        }
    }

    fn timing(&mut self, attempt: &str) -> Result<&ComparisonRunTiming> {
        use std::collections::btree_map::Entry;
        Ok(match self.timings.entry(attempt.to_owned()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(run_timing_in(self.connection, attempt)?),
        })
    }

    fn usage(&mut self, scopes: &[(String, String)]) -> Result<crate::domain::ObservedUsage> {
        crate::workflow::comparison_observed_usage(
            self.connection,
            scopes,
            &mut self.usage_ownership_checked,
        )
    }
}

fn normalize_query(query: &mut WorkflowComparisonQuery) -> Result<()> {
    for value in [
        &mut query.project_id,
        &mut query.workflow,
        &mut query.preset,
        &mut query.provider,
        &mut query.model,
        &mut query.effort,
        &mut query.profile_revision,
        &mut query.adapter_id,
        &mut query.adapter_hash,
    ] {
        if let Some(text) = value {
            *text = text.trim().to_owned();
            if text.is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
                bail!(
                    "Choose comparison filters between 1 and 256 bytes without control characters."
                )
            }
        }
    }
    if query
        .workflow
        .as_deref()
        .is_some_and(|v| !["trip", "single_agent"].contains(&v))
        || query
            .provider
            .as_deref()
            .is_some_and(|v| !["codex", "claude"].contains(&v))
        || query.preset.as_deref().is_some_and(|v| {
            !["single_agent", "repository_research", "build_and_review"].contains(&v)
        })
    {
        bail!("Choose a supported workflow, provider or preset.")
    }
    let start = crate::workflow::usage_date(query.start_date.as_deref())?;
    let end = crate::workflow::usage_date(query.end_date.as_deref())?;
    if start.zip(end).is_some_and(|(start, end)| start > end) {
        bail!("The end date must be on or after the start date.")
    }
    Ok(())
}

fn selection_filters(query: &WorkflowComparisonQuery) -> Result<serde_json::Value> {
    Ok(serde_json::json!({"operation":"workflow_comparisons_v1", "selection":query}))
}

fn bounded_cursor(
    incarnation: &str,
    revision: i64,
    filters: &serde_json::Value,
    last: Vec<String>,
) -> Result<String> {
    let cursor = crate::workflow::history_cursor(incarnation, revision, filters, last)?;
    if cursor.len() > 8192 {
        bail!("The comparison selection is too large. Reduce the filters and Apply again.")
    }
    Ok(cursor)
}

fn read_position(
    cursor: Option<&str>,
    incarnation: &str,
    revision: i64,
    filters: &serde_json::Value,
) -> Result<Vec<String>> {
    crate::workflow::history_read_position(cursor, incarnation, revision, filters)
        .map_err(|error| anyhow!("{error} Apply again to refresh the comparison."))
}

fn read_tasks(connection: &Connection) -> Result<Vec<ComparisonTask>> {
    let mut statement = connection.prepare(
        "SELECT t.id,t.project_id,t.created_at,t.lifecycle,t.archived_at IS NOT NULL,
                t.id IN (SELECT task_id FROM attempts),
                EXISTS(SELECT 1 FROM task_recipe_bindings b WHERE b.task_id=t.id)
         FROM tasks t JOIN projects p ON p.id=t.project_id
         WHERE p.internal_purpose IS NULL ORDER BY t.id",
    )?;
    let rows = statement
        .query_map([], |row| {
            let task = ComparisonTask {
                id: row.get(0)?,
                project: row.get(1)?,
                created_at: row.get(2)?,
                lifecycle: row.get(3)?,
                archived: row.get(4)?,
                runs: Vec::new(),
                recipe_attribution: None,
            };
            Ok((task, row.get::<_, bool>(5)?, row.get::<_, bool>(6)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut tasks = Vec::with_capacity(rows.len());
    let mut attempts = connection.prepare(
        "SELECT a.id,a.created_at,a.status,a.human_acceptance_at,b.family,b.definition_id,
                a.candidate_hash,a.selected_checks_revision,a.workflow_version
         FROM attempts a LEFT JOIN workflow_run_bindings b ON b.attempt_id=a.id
         WHERE a.task_id=?1 ORDER BY a.created_at,a.id",
    )?;
    for (mut task, has_attempts, has_recipe) in rows {
        if has_recipe {
            task.recipe_attribution = read_recipe_attribution(connection, &task.id, None)?;
        }
        if !has_attempts {
            tasks.push(task);
            continue;
        }
        task.runs = attempts
            .query_map([&task.id], |row| {
                Ok(ComparisonRun {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    status: row.get(2)?,
                    accepted_at: row.get(3)?,
                    workflow: {
                        let family = crate::workflow::usage_workflow_family(
                            row.get(4)?,
                            &row.get::<_, String>(8)?,
                        );
                        (family != "unknown").then_some(family)
                    },
                    preset: None,
                    candidate: row.get(6)?,
                    selection_revision: row.get(7)?,
                    executions: Vec::new(),
                    recipe_attribution: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for run in &mut task.runs {
            if has_recipe {
                run.recipe_attribution =
                    read_recipe_attribution(connection, &task.id, Some(&run.id))?;
            }
            if run.workflow.as_deref() == Some("single_agent") {
                run.preset = crate::workflow::run_preset(connection, &run.id)
                    .ok()
                    .map(|preset| serde_json::to_value(preset))
                    .transpose()?
                    .and_then(|v| v.as_str().map(str::to_owned));
            }
            run.executions =
                read_executions(connection, &run.id, run.workflow.as_deref() == Some("trip"))?;
        }
        tasks.push(task);
    }
    Ok(tasks)
}

fn read_recipe_attribution(
    connection: &Connection,
    task: &str,
    attempt: Option<&str>,
) -> Result<Option<crate::domain::ComparisonRecipeAttribution>> {
    // A task's schedule is origin evidence, not authority for unrelated attempts.
    let attribution = connection
        .query_row(
            "WITH RECURSIVE lineage(id) AS (
            SELECT id FROM attempts WHERE id=?2 AND task_id=?1
            UNION
            SELECT a.parent_attempt_id FROM attempts a JOIN lineage l ON l.id=a.id
             WHERE a.task_id=?1 AND a.parent_attempt_id IS NOT NULL
         )
         SELECT b.recipe_revision_id,r.title,b.schedule_id,b.scheduled_for_utc,
                f.id,f.authorization_id,f.attempt_id,f.outcome,f.reason
         FROM task_recipe_bindings b JOIN task_recipe_revisions r ON r.id=b.recipe_revision_id
         LEFT JOIN recipe_schedule_fires f ON f.task_id=b.task_id AND f.schedule_id=b.schedule_id
           AND f.scheduled_for_utc=b.scheduled_for_utc AND f.recipe_revision_id=b.recipe_revision_id
         WHERE b.task_id=?1 AND (?2 IS NULL OR (f.attempt_id IS NOT NULL
             AND f.attempt_id IN (SELECT id FROM lineage)))",
            params![task, attempt],
            |row| {
                Ok(crate::domain::ComparisonRecipeAttribution {
                    recipe_revision_id: row.get(0)?,
                    recipe_title: row.get(1)?,
                    schedule_id: row.get(2)?,
                    scheduled_for_utc: row.get(3)?,
                    occurrence_id: row.get(4)?,
                    authorization_id: row.get(5)?,
                    root_attempt_id: row.get(6)?,
                    occurrence_outcome: row.get(7)?,
                    occurrence_reason: row.get(8)?,
                })
            },
        )
        .optional()?;
    Ok(attribution)
}

fn read_executions(
    connection: &Connection,
    attempt: &str,
    trip: bool,
) -> Result<Vec<ComparisonExecution>> {
    let mut statement = connection.prepare(
        "SELECT g.id,s.id,s.transcript_epoch,s.provider,s.launch_config_json,
                s.capability_identity_json,r.profile_revision_id,p.profile_hash,
                p.adapter_id,p.adapter_hash,tp.profile_hash,tp.adapter_name,tp.adapter_hash,
                tp.activation_id,s.executable_version,tp.settings_revision
         FROM role_generations g JOIN sessions s ON s.role_generation_id=g.id
         LEFT JOIN execution_bindings e ON e.role_generation_id=g.id
         LEFT JOIN workflow_run_profiles r ON r.attempt_id=e.attempt_id AND r.slot_key=e.slot_key
         LEFT JOIN agent_profile_revisions p ON p.id=r.profile_revision_id
         LEFT JOIN trip_attempt_profiles tp ON tp.attempt_id=g.attempt_id AND tp.role=g.role
           AND tp.settings_revision=g.config_revision
         WHERE g.attempt_id=?1 ORDER BY s.id",
    )?;
    let rows = statement
        .query_map([attempt], |row| {
            let settings_revision: Option<i64> = row.get(15)?;
            let trip_profile_hash: Option<String> = row.get(10)?;
            let profile_revision =
                row.get::<_, Option<String>>(6)?
                    .or(row.get(13)?)
                    .or_else(|| {
                        settings_revision
                            .zip(trip_profile_hash.as_deref())
                            .map(|(revision, hash)| format!("trip:{revision}:{hash}"))
                    });
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                ComparisonExecutionIdentity {
                    profile_revision,
                    settings_revision,
                    profile_hash: row.get::<_, Option<String>>(7)?.or(row.get(10)?),
                    adapter_id: row.get::<_, Option<String>>(8)?.or(row.get(11)?),
                    adapter_hash: row.get::<_, Option<String>>(9)?.or(row.get(12)?),
                    executable_version: row.get(14)?,
                    ..Default::default()
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut executions: Vec<ComparisonExecution> = Vec::new();
    for (generation, session, epoch, provider, launch, capability, identity) in rows {
        let mut frozen_identity = identity.clone();
        fill_launch_identity(
            &mut frozen_identity,
            &provider,
            &launch,
            capability.as_deref(),
        );
        let mut resumes = connection.prepare(
            "SELECT transcript_epoch,launch_config_json,capability_identity_json,prior_transcript_epoch,resume_ordinal
             FROM resume_invocations WHERE session_id=?1 ORDER BY resume_ordinal",
        )?;
        let resumed = resumes
            .query_map([&session], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let prior_epochs: Vec<_> = resumed
            .iter()
            .filter_map(|(_, _, _, prior, _)| prior.as_ref())
            .cloned()
            .collect();
        let initial_epoch = resumed
            .iter()
            .find(|(_, _, _, _, ordinal)| *ordinal == 1)
            .and_then(|(_, _, _, prior, _)| prior.clone());
        for (epoch, launch, capability, _, _) in resumed {
            if executions
                .iter()
                .any(|e| e.session == session && e.epoch == epoch)
            {
                continue;
            }
            let mut identity = identity.clone();
            identity.executable_version = None;
            fill_launch_identity(&mut identity, &provider, &launch, Some(&capability));
            executions.push(ComparisonExecution {
                generation: generation.clone(),
                session: session.clone(),
                epoch,
                identity,
            });
        }
        if !executions
            .iter()
            .any(|e| e.session == session && e.epoch == epoch)
        {
            executions.push(ComparisonExecution {
                generation: generation.clone(),
                session: session.clone(),
                epoch,
                identity: frozen_identity.clone(),
            });
        }
        for epoch in prior_epochs {
            if !executions
                .iter()
                .any(|e| e.session == session && e.epoch == epoch)
            {
                executions.push(ComparisonExecution {
                    generation: generation.clone(),
                    session: session.clone(),
                    // TRIP preserves its initial launch; Single agent resumes overwrite it.
                    identity: if trip && initial_epoch.as_ref() == Some(&epoch) {
                        frozen_identity.clone()
                    } else {
                        ComparisonExecutionIdentity {
                            provider: Some(provider.clone()),
                            ..Default::default()
                        }
                    },
                    epoch,
                });
            }
        }
    }
    Ok(executions)
}

fn fill_launch_identity(
    identity: &mut ComparisonExecutionIdentity,
    provider: &str,
    launch: &str,
    capability: Option<&str>,
) {
    let launch = serde_json::from_str::<serde_json::Value>(launch).ok();
    let capability = capability.and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok());
    let string = |key: &str| {
        launch
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_str())
            .or_else(|| {
                capability
                    .as_ref()
                    .and_then(|v| v.get(key))
                    .and_then(|v| v.as_str())
            })
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    identity.provider = Some(provider.into());
    identity.model = string("model");
    identity.effort = string("effort");
    identity.executable_version =
        string("executable_version").or(identity.executable_version.take());
    identity.compatibility_revision = launch
        .as_ref()
        .and_then(|v| v.pointer("/compatibility/contract_revision"))
        .or_else(|| {
            launch
                .as_ref()
                .and_then(|v| v.pointer("/policy_binding/contract_revision"))
        })
        .or_else(|| {
            capability
                .as_ref()
                .and_then(|v| v.pointer("/compatibility/contract_revision"))
        })
        .and_then(|v| v.as_str())
        .map(str::to_owned);
}

fn execution_matches(
    query: &WorkflowComparisonQuery,
    identity: &ComparisonExecutionIdentity,
) -> bool {
    [
        (&query.provider, &identity.provider),
        (&query.model, &identity.model),
        (&query.effort, &identity.effort),
        (&query.profile_revision, &identity.profile_revision),
        (&query.adapter_id, &identity.adapter_id),
        (&query.adapter_hash, &identity.adapter_hash),
    ]
    .iter()
    .all(|(filter, value)| filter.is_none() || filter == value)
}

fn has_execution_filter(query: &WorkflowComparisonQuery) -> bool {
    [
        &query.provider,
        &query.model,
        &query.effort,
        &query.profile_revision,
        &query.adapter_id,
        &query.adapter_hash,
    ]
    .iter()
    .any(|value| value.is_some())
}

fn run_matches(query: &WorkflowComparisonQuery, run: &ComparisonRun) -> bool {
    query
        .workflow
        .as_ref()
        .is_none_or(|v| Some(v) == run.workflow.as_ref())
        && query
            .preset
            .as_ref()
            .is_none_or(|v| Some(v) == run.preset.as_ref())
        && (!has_execution_filter(query)
            || run
                .executions
                .iter()
                .any(|e| execution_matches(query, &e.identity)))
}

fn in_dates(query: &WorkflowComparisonQuery, created: &str) -> Result<bool> {
    if query.start_date.is_none() && query.end_date.is_none() {
        return Ok(true);
    }
    let date = timestamp(created)
        .context("comparison cohort has an invalid recorded creation date")?
        .with_timezone(&chrono::Utc)
        .date_naive();
    let start = crate::workflow::usage_date(query.start_date.as_deref())?;
    let end = crate::workflow::usage_date(query.end_date.as_deref())?;
    Ok(start.is_none_or(|v| date >= v) && end.is_none_or(|v| date <= v))
}

fn attribution(values: impl IntoIterator<Item = Option<String>>) -> ComparisonAttribution {
    let mut known = BTreeSet::new();
    let mut missing = false;
    for value in values {
        match value {
            Some(value) => {
                known.insert(value);
            }
            None => missing = true,
        }
    }
    let state = if known.is_empty() {
        ComparisonAttributionState::Unavailable
    } else if missing {
        ComparisonAttributionState::Incomplete
    } else if known.len() > 1 {
        ComparisonAttributionState::Mixed
    } else {
        ComparisonAttributionState::Known
    };
    ComparisonAttribution {
        state,
        values: known.into_iter().collect(),
    }
}

fn unit_attribution(
    task: &ComparisonTask,
    runs: &[&ComparisonRun],
    dimension: ComparisonDimension,
) -> ComparisonAttribution {
    if dimension == ComparisonDimension::Project {
        return attribution([Some(task.project.clone())]);
    }
    let mut values = Vec::new();
    for run in runs {
        match dimension {
            ComparisonDimension::Project => values.push(Some(task.project.clone())),
            ComparisonDimension::Workflow => values.push(run.workflow.clone()),
            ComparisonDimension::Preset => values.push(run.preset.clone()),
            _ => {
                if run.executions.is_empty() {
                    values.push(None);
                }
                for execution in &run.executions {
                    let identity = &execution.identity;
                    values.push(match dimension {
                        ComparisonDimension::Provider => identity.provider.clone(),
                        ComparisonDimension::Model => identity.model.clone(),
                        ComparisonDimension::Effort => identity.effort.clone(),
                        ComparisonDimension::ProfileRevision => identity.profile_revision.clone(),
                        ComparisonDimension::Adapter => identity
                            .adapter_id
                            .as_ref()
                            .zip(identity.adapter_hash.as_ref())
                            .map(|(id, hash)| format!("{id}@{hash}")),
                        ComparisonDimension::Project
                        | ComparisonDimension::Workflow
                        | ComparisonDimension::Preset => None,
                    });
                }
            }
        }
    }
    attribution(values)
}

fn select_units<'a>(
    tasks: &'a [ComparisonTask],
    query: &WorkflowComparisonQuery,
) -> Result<Vec<ComparisonUnit<'a>>> {
    let mut units = Vec::new();
    for task in tasks {
        if query
            .project_id
            .as_ref()
            .is_some_and(|v| v != &task.project)
        {
            continue;
        }
        match query.cohort {
            ComparisonCohort::Task => {
                let run_filter = query.workflow.is_some()
                    || query.preset.is_some()
                    || has_execution_filter(query);
                if !in_dates(query, &task.created_at)?
                    || (run_filter && !task.runs.iter().any(|run| run_matches(query, run)))
                {
                    continue;
                }
                let runs: Vec<_> = task.runs.iter().collect();
                let attribution = unit_attribution(task, &runs, query.group_by);
                let group = attribution_key(&attribution)?;
                units.push(ComparisonUnit {
                    task,
                    runs,
                    id: &task.id,
                    attribution,
                    group,
                });
            }
            ComparisonCohort::Run => {
                for run in &task.runs {
                    if !in_dates(query, &run.created_at)? || !run_matches(query, run) {
                        continue;
                    }
                    let runs = vec![run];
                    let attribution = unit_attribution(task, &runs, query.group_by);
                    let group = attribution_key(&attribution)?;
                    units.push(ComparisonUnit {
                        task,
                        runs,
                        id: &run.id,
                        attribution,
                        group,
                    });
                }
            }
        }
    }
    Ok(units)
}

fn attribution_key(attribution: &ComparisonAttribution) -> Result<String> {
    // Mixed cohorts stay one group without embedding an unbounded identity list in cursors.
    Ok(match attribution.state {
        ComparisonAttributionState::Known => format!(
            "known:{}",
            crate::store::json_hash(&serde_json::to_value(&attribution.values)?)?
        ),
        ComparisonAttributionState::Mixed => "mixed".into(),
        ComparisonAttributionState::Incomplete => "incomplete".into(),
        ComparisonAttributionState::Unavailable => "unavailable".into(),
    })
}

fn evidence_row(
    unit: &ComparisonUnit<'_>,
    run: Option<&ComparisonRun>,
    key: String,
    source: Option<(HistorySourceKind, String)>,
    fact: ComparisonEvidenceFact,
) -> ComparisonEvidenceRow {
    ComparisonEvidenceRow {
        key: format!("{}:{key}", unit.id),
        unit_id: unit.id.into(),
        task_id: unit.task.id.clone(),
        attempt_id: run.map(|run| run.id.clone()),
        recipe_attribution: match run {
            Some(run) => run.recipe_attribution.clone(),
            None if unit.id == unit.task.id => unit.task.recipe_attribution.clone(),
            None => unit
                .runs
                .first()
                .and_then(|run| run.recipe_attribution.clone()),
        },
        source: source.map(|(kind, id)| HistoricalSourceKey {
            task_id: unit.task.id.clone(),
            attempt_id: run.map(|run| run.id.clone()),
            kind,
            id,
        }),
        fact,
        truncated: false,
    }
}

fn unit_outcome(unit: &ComparisonUnit<'_>, cohort: ComparisonCohort) -> &'static str {
    let accepted = unit.runs.iter().any(|run| run.accepted_at.is_some());
    if accepted {
        return "accepted";
    }
    match cohort {
        ComparisonCohort::Task if unit.task.lifecycle == "cancelled" => "cancelled",
        ComparisonCohort::Task => "incomplete",
        ComparisonCohort::Run => match unit.runs.first().map(|run| run.status.as_str()) {
            Some("cancelled") => "cancelled",
            Some("failed" | "materialization_failed") => "failed",
            _ => "incomplete",
        },
    }
}

fn unit_facts(
    reads: &mut ComparisonReads<'_>,
    unit: &ComparisonUnit<'_>,
    query: &WorkflowComparisonQuery,
) -> Result<Vec<ComparisonEvidenceRow>> {
    let connection = reads.connection;
    let mut facts = vec![evidence_row(
        unit,
        None,
        "outcome".into(),
        None,
        ComparisonEvidenceFact::Outcome {
            lifecycle: unit.task.lifecycle.clone(),
            run_status: (query.cohort == ComparisonCohort::Run)
                .then(|| unit.runs[0].status.clone()),
            archived: unit.task.archived,
            human_acceptance_at: unit.runs.iter().find_map(|run| run.accepted_at.clone()),
            outcome: unit_outcome(unit, query.cohort).into(),
        },
    )];
    if query.cohort == ComparisonCohort::Task {
        let lifecycle =
            serde_json::from_value(serde_json::Value::String(unit.task.lifecycle.clone()))
                .context("parse comparison task lifecycle")?;
        let acceptance = exact_acceptance(
            unit.runs
                .iter()
                .filter_map(|run| run.accepted_at.as_deref()),
        );
        for run in &unit.runs {
            reads.timing(&run.id)?;
        }
        let runs = unit
            .runs
            .iter()
            .map(|run| {
                reads
                    .timings
                    .get(&run.id)
                    .context("comparison timing was not loaded")
            })
            .collect::<Result<Vec<_>>>()?;
        let timing = summarize_task_timing(lifecycle, acceptance, runs.into_iter());
        facts.push(evidence_row(
            unit,
            None,
            "task_timing".into(),
            None,
            ComparisonEvidenceFact::TaskTiming {
                state: timing.state,
                elapsed_millis: timing.elapsed,
                first_start_coverage_complete: timing.first_covered,
                reason: timing.reason.map(str::to_owned),
            },
        ));
    }
    for run in &unit.runs {
        if run.workflow.is_none() {
            for metric in [
                ComparisonEvidenceMetric::HumanChange,
                ComparisonEvidenceMetric::ProviderVerdict,
                ComparisonEvidenceMetric::Repair,
            ] {
                facts.push(evidence_row(unit,Some(run),format!("classification:{}:{metric:?}",run.id),None,
                    ComparisonEvidenceFact::ClassificationUnavailable {metric,
                        reason:"Historical workflow classification is not recorded; recorded source counts do not prove complete coverage.".into()}));
            }
        }
        usage_facts(connection, unit, run, query, &mut facts)?;
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("run:{}", run.id),
            None,
            ComparisonEvidenceFact::Run {
                status: run.status.clone(),
                workflow: run.workflow.clone(),
                preset: run.preset.clone(),
                created_at: run.created_at.clone(),
                human_acceptance_at: run.accepted_at.clone(),
                candidate_hash: run.candidate.clone(),
                selection_revision: run.selection_revision,
            },
        ));
        boundary_facts(connection, unit, run, &mut facts)?;
        for execution in &run.executions {
            let usage = reads.usage(&[(execution.session.clone(), execution.epoch.clone())])?;
            facts.push(evidence_row(
                unit,
                Some(run),
                format!("execution:{}:{}", execution.session, execution.epoch),
                None,
                ComparisonEvidenceFact::Execution {
                    role_generation_id: execution.generation.clone(),
                    session_id: execution.session.clone(),
                    transcript_epoch: execution.epoch.clone(),
                    identity: execution.identity.clone(),
                    matching: run_matches(query, run)
                        && execution_matches(query, &execution.identity),
                    usage,
                },
            ));
        }
        reads.timing(&run.id)?;
        decision_facts(reads, unit, run, &mut facts)?;
        recovery_facts(connection, unit, run, &mut facts)?;
        check_facts(connection, unit, run, &mut facts)?;
    }
    facts.sort_by(|a, b| a.key.cmp(&b.key));
    facts.dedup_by(|a, b| a.key == b.key);
    Ok(facts)
}

fn decision_facts(
    reads: &mut ComparisonReads<'_>,
    unit: &ComparisonUnit<'_>,
    run: &ComparisonRun,
    facts: &mut Vec<ComparisonEvidenceRow>,
) -> Result<()> {
    let connection = reads.connection;
    let mut statement = connection.prepare(
        "SELECT ac.id,ac.child_attempt_id,ac.id FROM workflow_result_acceptances ac
         JOIN workflow_result_bindings b ON b.id=ac.result_binding_id
         WHERE b.attempt_id=?1 AND ac.decision='changes_requested'
         UNION ALL
         SELECT (SELECT e.id FROM audit_events e WHERE e.operation_id=r.operation_id
             AND e.actor_kind='human' AND e.event_code='human.command.applied' ORDER BY e.created_at,e.rowid LIMIT 1),
             r.new_attempt_id,r.id FROM rework_intents r
         WHERE r.parent_attempt_id=?1
           AND NOT EXISTS(SELECT 1 FROM single_agent_repair_receipts p WHERE p.rework_intent_id=r.id)
           AND NOT EXISTS(SELECT 1 FROM workflow_result_acceptances ac WHERE ac.operation_id=r.operation_id)
           AND EXISTS(SELECT 1 FROM audit_events e WHERE e.operation_id=r.operation_id
               AND e.actor_kind='human' AND e.event_code='human.command.applied')",
    )?;
    let human = statement
        .query_map([&run.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, child, source_id) in human {
        let kind = if run.workflow.as_deref() == Some("single_agent") {
            HistorySourceKind::ResultAcceptance
        } else {
            HistorySourceKind::Rework
        };
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("human:{id}"),
            Some((kind, source_id)),
            ComparisonEvidenceFact::HumanChange {
                decision_id: id,
                child_attempt_id: child,
            },
        ));
    }
    let mut statement = connection.prepare(
        "SELECT r.id,
            (SELECT rr.id FROM role_results rr WHERE rr.role_generation_id=r.role_generation_id
               AND rr.session_id=r.session_id AND rr.consumed_at IS NOT NULL
               AND json_extract(rr.metadata_json,'$.review_request_id')=r.id ORDER BY rr.created_at,rr.rowid LIMIT 1),
            r.verdict,r.review_kind,r.created_at
         FROM review_requests r WHERE r.attempt_id=?1 AND r.delivery_state='finished' AND r.verdict IS NOT NULL
         UNION ALL
         SELECT d.id,r.role_result_id,r.verdict,'single_agent',r.created_at
         FROM single_agent_review_deliveries d JOIN single_agent_review_results r ON r.delivery_id=d.id
         WHERE d.attempt_id=?1 AND d.state='finished'",
    )?;
    let verdicts = statement
        .query_map([&run.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut decisions = BTreeSet::new();
    for (id, result_id, verdict, kind, created) in verdicts {
        if !decisions.insert(id.clone()) {
            continue;
        }
        let consumed_at: Option<String> = if kind != "single_agent" {
            connection
                .query_row(
                    "SELECT rr.consumed_at FROM role_results rr WHERE rr.id=?1",
                    [result_id.as_deref()],
                    |row| row.get(0),
                )
                .optional()?
                .flatten()
        } else {
            None
        };
        let dedicated_recheck: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM final_repair_rechecks WHERE review_request_id=?1)",
            [&id],
            |row| row.get(0),
        )?;
        let source = (kind != "single_agent").then(|| (HistorySourceKind::Review, id.clone()));
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("verdict:{id}"),
            source,
            ComparisonEvidenceFact::ProviderVerdict {
                decision_id: id.clone(),
                result_id,
                verdict: verdict.clone(),
            },
        ));
        if matches!(kind.as_str(), "code" | "final")
            && verdict == "request_changes"
            && !dedicated_recheck
        {
            let applied: Option<String> = connection.query_row(
                "SELECT created_at FROM audit_events WHERE entity_id=?1 AND event_code='review.result.applied'
                 ORDER BY created_at,rowid LIMIT 1", [&id], |row| row.get(0),
            ).optional()?;
            let final_receipt: Option<String> = connection.query_row(
                "SELECT created_at FROM final_repair_rechecks WHERE attempt_id=?1 AND final_request_id=?2",
                params![run.id, id], |row| row.get(0),
            ).optional()?;
            if let Some(authorized_at) = applied.or(final_receipt).or(consumed_at) {
                let started = repair_start(reads, &run.id, &authorized_at, Some(&id))?;
                facts.push(evidence_row(unit, Some(run), format!("repair:{id}"), None,
                    ComparisonEvidenceFact::Repair {
                        authorization_id: id.clone(), originating_decision_id: id,
                        classification_reason: started.is_none().then(|| "No exact subsequent implementer execution is linked to this transition.".into()),
                        started_session_id: started,
                    }));
            } else {
                facts.push(evidence_row(
                    unit,
                    Some(run),
                    format!("repair_unknown:{id}"),
                    None,
                    ComparisonEvidenceFact::ClassificationUnavailable {
                        metric: ComparisonEvidenceMetric::Repair,
                        reason: "The verdict is recorded but its repair transition is not.".into(),
                    },
                ));
            }
        }
        let _ = created;
    }
    let mut statement = connection.prepare(
        "SELECT delivery_id,child_attempt_id,created_at FROM single_agent_repair_receipts
         WHERE root_attempt_id=?1",
    )?;
    let repairs = statement
        .query_map([&run.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (decision, child, created) in repairs {
        let started = repair_start(reads, &child, &created, None)?;
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("repair:{decision}"),
            None,
            ComparisonEvidenceFact::Repair {
                authorization_id: format!("single_agent:{}", run.id),
                originating_decision_id: decision,
                classification_reason: started
                    .is_none()
                    .then(|| "No bound repair execution start was recorded.".into()),
                started_session_id: started,
            },
        ));
    }
    Ok(())
}

fn repair_start(
    reads: &mut ComparisonReads<'_>,
    attempt: &str,
    authorized_at: &str,
    decision: Option<&str>,
) -> Result<Option<String>> {
    let Some(authorized) = timestamp(authorized_at) else {
        return Ok(None);
    };
    let connection = reads.connection;
    let timing = reads.timing(attempt)?;
    let mut invocations: Vec<_> = timing.invocations.iter().collect();
    invocations.sort_by(|a, b| a.observed_start.cmp(&b.observed_start));
    for invocation in invocations {
        let Some(start) = invocation.observed_start.as_deref().and_then(timestamp) else {
            continue;
        };
        if start < authorized
            || invocation.unavailable_reason.as_deref() == Some("conflicting_execution_boundaries")
        {
            continue;
        }
        let linked:bool=connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
             WHERE s.id=?1 AND g.attempt_id=?2 AND g.role IN ('implementer','single_agent')
               AND (?3 IS NULL OR EXISTS(SELECT 1 FROM review_requests r JOIN role_results rr
                   ON rr.role_generation_id=r.role_generation_id AND rr.session_id=r.session_id
                   AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
                   WHERE r.id=?3 AND rr.consumed_at IS NOT NULL AND rr.consumed_at<=?4))
               AND NOT EXISTS(SELECT 1 FROM review_requests next WHERE next.attempt_id=?2
                   AND next.review_kind IN ('code','final') AND next.id IS NOT ?3
                   AND next.created_at>?5 AND next.created_at<?4))",
            params![invocation.session_id,attempt,decision,invocation.observed_start,authorized_at],|row|row.get(0),
        )?;
        if linked {
            return Ok(Some(invocation.session_id.clone()));
        }
    }
    Ok(None)
}

fn recovery_facts(
    connection: &Connection,
    unit: &ComparisonUnit<'_>,
    run: &ComparisonRun,
    facts: &mut Vec<ComparisonEvidenceRow>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT r.id,r.session_id,r.state,r.detail_json,r.created_at,r.resolved_at
         FROM recovery_records r WHERE r.attempt_id=?1 OR (r.attempt_id IS NULL AND r.session_id IN
             (SELECT s.id FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id WHERE g.attempt_id=?1))",
    )?;
    let rows = statement
        .query_map([&run.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, session, state, detail, created, resolved) in rows {
        let reason = serde_json::from_str::<serde_json::Value>(&detail)
            .ok()
            .and_then(|v| {
                v.get("reason")
                    .or_else(|| v.get("message"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            });
        let elapsed = timestamp(&created)
            .zip(resolved.as_deref().and_then(timestamp))
            .filter(|(start, end)| end >= start)
            .map(|(start, end)| (end - start).num_milliseconds());
        let ongoing =
            state == "attention_required" && resolved.is_none() && timestamp(&created).is_some();
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("recovery:{id}"),
            None,
            ComparisonEvidenceFact::Recovery {
                recovery_id: id,
                session_id: session,
                state,
                reason,
                created_at: created,
                resolved_at: resolved,
                elapsed_millis: elapsed,
                ongoing,
            },
        ));
    }
    Ok(())
}

struct CheckReceipt {
    id: String,
    check: Option<String>,
    revision: Option<i64>,
    candidate: String,
    inputs: Option<String>,
    status: String,
    freshness: String,
    launch: String,
    exit: Option<i64>,
    elapsed: Option<i64>,
    executable: String,
    arguments: String,
    cwd: String,
}

fn receipt_state(receipt: &CheckReceipt) -> ComparisonCheckState {
    if receipt.launch == "capture_failed"
        || matches!(
            receipt.status.as_str(),
            "precondition_failed" | "launch_failed" | "launch_ambiguous" | "recovery_required"
        )
    {
        ComparisonCheckState::Unavailable
    } else if matches!(receipt.status.as_str(), "running" | "launch_reserved") {
        ComparisonCheckState::Running
    } else if receipt.inputs.is_none() || receipt.freshness == "unknown" {
        ComparisonCheckState::Unavailable
    } else if !matches!(receipt.freshness.as_str(), "current" | "failed") {
        ComparisonCheckState::Stale
    } else if receipt.status == "finished" {
        match receipt.exit {
            Some(0) => ComparisonCheckState::Passed,
            Some(_) => ComparisonCheckState::Failed,
            None => ComparisonCheckState::Unavailable,
        }
    } else {
        ComparisonCheckState::Unavailable
    }
}

fn check_facts(
    connection: &Connection,
    unit: &ComparisonUnit<'_>,
    run: &ComparisonRun,
    facts: &mut Vec<ComparisonEvidenceRow>,
) -> Result<()> {
    let single = run.workflow.as_deref() == Some("single_agent");
    let result = if single {
        connection
            .query_row(
                "SELECT id,state,snapshot_id,candidate_hash,selected_check_revision,created_at
             FROM workflow_result_bindings WHERE attempt_id=?1
             ORDER BY (state='accepted') DESC,created_at DESC,rowid DESC LIMIT 1",
                [&run.id],
                |row| {
                    Ok(crate::domain::SingleAgentResultDto {
                        id: row.get(0)?,
                        state: row.get(1)?,
                        summary: String::new(),
                        acceptance_rows: Vec::new(),
                        snapshot_id: row.get(2)?,
                        candidate_hash: row.get(3)?,
                        selected_check_revision: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .optional()?
    } else {
        None
    };
    let revision = match &result {
        Some(result) => result.selected_check_revision,
        None if single => connection
            .query_row(
                "SELECT MAX(revision) FROM workflow_check_selections WHERE attempt_id=?1",
                [&run.id],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .unwrap_or(0),
        None => run.selection_revision,
    };
    let candidate = result
        .as_ref()
        .and_then(|r| r.candidate_hash.as_ref())
        .or(run.candidate.as_ref());
    let table = if single {
        "workflow_check_selections"
    } else {
        "trip_selected_checks"
    };
    let mut statement = connection.prepare(&format!(
        "SELECT check_id FROM {table} WHERE attempt_id=?1 AND revision=?2 ORDER BY check_id"
    ))?;
    let selected = statement
        .query_map(params![run.id, revision], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let accepted = run.accepted_at.is_some();
    let accepted_checks = result
        .as_ref()
        .filter(|r| accepted && r.state == "accepted")
        .map(|r| crate::workflow::accepted_single_agent_checks(connection, &run.id, r))
        .transpose()?;
    let matrix_known = if single && accepted {
        let valid_acceptance: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_result_acceptances ac
             JOIN workflow_result_bindings r ON r.id=ac.result_binding_id AND r.state='accepted'
             JOIN workflow_run_bindings b ON b.attempt_id=r.attempt_id
             JOIN attempts a ON a.id=r.attempt_id AND a.status='done'
               AND a.candidate_hash=r.candidate_hash AND a.accepted_snapshot_id=r.snapshot_id
             WHERE r.attempt_id=?1 AND ac.decision='accepted'
               AND ac.candidate_hash=r.candidate_hash AND ac.definition_hash=b.definition_hash
               AND ac.profile_binding_hash=r.profile_binding_hash AND ac.criteria_hash=r.criteria_hash
               AND json_extract(ac.check_receipts_json,'$.execution_config_hash')=b.execution_config_hash
               AND json_type(ac.check_receipts_json,'$.check_runs')='array')",
            [&run.id], |row|row.get(0),
        )?;
        valid_acceptance
            && accepted_checks
                .as_ref()
                .is_some_and(|checks| checks.len() == selected.len())
    } else if !single && accepted {
        revision > 0 && connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts a JOIN snapshots s ON s.id=a.accepted_snapshot_id
             AND s.attempt_id=a.id AND s.complete=1 AND s.manifest_hash=a.candidate_hash
             WHERE a.id=?1)", [&run.id], |row|row.get::<_,bool>(0),
        )?
    } else {
        revision > 0 || (single && result.is_some())
    };
    if !matrix_known {
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("check_matrix:{}", run.id),
            None,
            ComparisonEvidenceFact::UnknownCheckMatrix {
                reason:
                    "The exact run selection or accepted candidate/receipt binding is unavailable."
                        .into(),
            },
        ));
    }
    let mut statement = connection.prepare(
        "SELECT id,check_id,selected_check_revision,candidate_hash,inputs_hash,status,freshness_state,
                launch_state,exit_code,elapsed_millis,executable,arguments_json,cwd FROM check_runs WHERE attempt_id=?1
         ORDER BY created_at DESC,rowid DESC",
    )?;
    let receipts = statement
        .query_map([&run.id], |row| {
            Ok(CheckReceipt {
                id: row.get(0)?,
                check: row.get(1)?,
                revision: row.get(2)?,
                candidate: row.get(3)?,
                inputs: row.get(4)?,
                status: row.get(5)?,
                freshness: row.get(6)?,
                launch: row.get(7)?,
                exit: row.get(8)?,
                elapsed: row.get(9)?,
                executable: row.get(10)?,
                arguments: row.get(11)?,
                cwd: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counted = BTreeSet::new();
    if matrix_known {
        for check in &selected {
            let pinned = accepted_checks
                .as_ref()
                .and_then(|checks| checks.iter().find(|c| &c.check_id == check));
            let eligible = |receipt: &&CheckReceipt| {
                receipt.check.as_ref() == Some(check)
                    && receipt.revision == Some(revision)
                    && Some(&receipt.candidate) == candidate
            };
            let receipt = if single && accepted {
                pinned
                    .and_then(|pin| pin.latest_run_id.as_ref())
                    .and_then(|id| receipts.iter().find(|r| &r.id == id))
            } else {
                let mut matched = None;
                for receipt in receipts.iter().filter(eligible) {
                    if check_command_matches(connection, run, check, single, receipt)? {
                        matched = Some(receipt);
                        break;
                    }
                }
                matched
            };
            let any_earlier = receipts.iter().any(|r| r.check.as_ref() == Some(check));
            let state = match receipt {
                Some(receipt) => receipt_state(receipt),
                None if single && accepted => ComparisonCheckState::Unavailable,
                None if any_earlier => ComparisonCheckState::Stale,
                None => ComparisonCheckState::NotRun,
            };
            if let Some(receipt) = receipt {
                counted.insert(receipt.id.as_str());
            }
            facts.push(evidence_row(
                unit,
                Some(run),
                format!("selected:{}:{revision}:{check}", run.id),
                None,
                ComparisonEvidenceFact::Check {
                    check_id: check.clone(),
                    selection_revision: revision,
                    receipt_id: receipt.map(|r| r.id.clone()),
                    state,
                    candidate_hash: candidate.cloned(),
                    inputs_hash: receipt.and_then(|r| r.inputs.clone()),
                    elapsed_millis: receipt.and_then(|r| r.elapsed).filter(|v| *v >= 0),
                    selected: true,
                    recorded_status: receipt.map(|r| r.status.clone()),
                    freshness_state: receipt.map(|r| r.freshness.clone()),
                    launch_state: receipt.map(|r| r.launch.clone()),
                    exit_code: receipt.and_then(|r| r.exit),
                    reason: receipt.is_none().then(|| {
                        if single && accepted {
                            "The acceptance-pinned receipt is missing or invalid."
                        } else if any_earlier {
                            "Only receipts for other candidate or selection bindings exist."
                        } else {
                            "No receipt was recorded for this selected check."
                        }
                        .into()
                    }),
                },
            ));
        }
    }
    for receipt in &receipts {
        if counted.contains(receipt.id.as_str()) {
            continue;
        }
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("receipt:{}", receipt.id),
            None,
            ComparisonEvidenceFact::Check {
                check_id: receipt
                    .check
                    .clone()
                    .unwrap_or_else(|| "unavailable".into()),
                selection_revision: receipt.revision.unwrap_or(0),
                receipt_id: Some(receipt.id.clone()),
                state: receipt_state(receipt),
                candidate_hash: Some(receipt.candidate.clone()),
                inputs_hash: receipt.inputs.clone(),
                elapsed_millis: receipt.elapsed.filter(|v| *v >= 0),
                selected: false,
                recorded_status: Some(receipt.status.clone()),
                freshness_state: Some(receipt.freshness.clone()),
                launch_state: Some(receipt.launch.clone()),
                exit_code: receipt.exit,
                reason: Some("Historical receipt; not part of the reduced selected matrix.".into()),
            },
        ));
    }
    Ok(())
}

fn add_duration(
    summary: &mut ComparisonDurationSummary,
    millis: Option<i64>,
    ongoing: bool,
) -> Result<()> {
    if let Some(millis) = millis.filter(|v| *v >= 0) {
        summary.sum_millis = Some(
            summary
                .sum_millis
                .unwrap_or(0)
                .checked_add(millis)
                .context("comparison duration sum overflow")?,
        );
        summary.complete_samples += 1;
        summary.mean_millis = summary
            .sum_millis
            .map(|sum| sum as f64 / summary.complete_samples as f64);
    } else if ongoing {
        summary.ongoing_samples += 1;
    } else {
        summary.unavailable_samples += 1;
    }
    Ok(())
}

fn group_projection(
    reads: &mut ComparisonReads<'_>,
    units: &[&ComparisonUnit<'_>],
    query: &WorkflowComparisonQuery,
) -> Result<WorkflowComparisonGroup> {
    let first = units.first().context("comparison group has no units")?;
    let mut group = WorkflowComparisonGroup {
        key: first.group.clone(),
        attribution: ComparisonAttribution {
            state: first.attribution.state.clone(),
            values: if first.attribution.state == ComparisonAttributionState::Known {
                first.attribution.values.clone()
            } else {
                Vec::new()
            },
        },
        counts: ComparisonCounts::default(),
        elapsed_to_acceptance: Default::default(),
        observed_execution_intervals: Default::default(),
        known_partial_execution_intervals: Default::default(),
        recorded_recovery_waits: Default::default(),
        check_execution: Default::default(),
        usage: crate::domain::ObservedUsage::unavailable(
            "No matching execution usage was recorded.",
        ),
    };
    let mut sessions = BTreeSet::new();
    for unit in units {
        group.counts.units += 1;
        match unit_outcome(unit, query.cohort) {
            "accepted" => group.counts.accepted += 1,
            "cancelled" => group.counts.cancelled += 1,
            "failed" => group.counts.failed += 1,
            _ => group.counts.incomplete += 1,
        }
        for run in &unit.runs {
            let timing = reads.timing(&run.id)?;
            add_duration(
                &mut group.observed_execution_intervals,
                timing.complete_observed_interval_millis,
                timing.ongoing_epochs > 0,
            )?;
            add_duration(
                &mut group.known_partial_execution_intervals,
                timing.known_partial_interval_millis,
                false,
            )?;
            group.counts.complete_execution_epochs += timing.complete_epochs;
            group.counts.missing_execution_epochs += timing.missing_epochs;
            group.counts.ongoing_execution_epochs += timing.ongoing_epochs;
            group.counts.never_executed_epochs += timing.never_executed_epochs;
        }
        for row in unit_facts(reads, unit, query)? {
            match row.fact {
                ComparisonEvidenceFact::TaskTiming {
                    state,
                    elapsed_millis,
                    ..
                } => {
                    add_duration(
                        &mut group.elapsed_to_acceptance,
                        elapsed_millis,
                        state == ComparisonTimingState::Ongoing,
                    )?;
                }
                ComparisonEvidenceFact::ClassificationUnavailable { metric, .. } => match metric {
                    ComparisonEvidenceMetric::HumanChange => {
                        group.counts.human_classification_unavailable += 1
                    }
                    ComparisonEvidenceMetric::ProviderVerdict => {
                        group.counts.provider_classification_unavailable += 1
                    }
                    ComparisonEvidenceMetric::Repair => {
                        group.counts.repair_classification_unavailable += 1
                    }
                    _ => {}
                },
                ComparisonEvidenceFact::HumanChange { .. } => group.counts.human_changes += 1,
                ComparisonEvidenceFact::ProviderVerdict { verdict, .. } => {
                    group.counts.provider_verdicts += 1;
                    if verdict != "approved" {
                        group.counts.provider_negative_verdicts += 1;
                    }
                }
                ComparisonEvidenceFact::Repair {
                    started_session_id,
                    classification_reason,
                    ..
                } => {
                    group.counts.repairs_authorized += 1;
                    if started_session_id.is_some() {
                        group.counts.repairs_started += 1;
                    }
                    if classification_reason.is_some() {
                        group.counts.repair_classification_unavailable += 1;
                    }
                }
                ComparisonEvidenceFact::Recovery {
                    elapsed_millis,
                    ongoing,
                    ..
                } => {
                    group.counts.infrastructure_recoveries += 1;
                    add_duration(&mut group.recorded_recovery_waits, elapsed_millis, ongoing)?;
                }
                ComparisonEvidenceFact::UnknownCheckMatrix { .. } => {
                    group.counts.unknown_check_matrices += 1
                }
                ComparisonEvidenceFact::Check {
                    selected: true,
                    state,
                    elapsed_millis,
                    ..
                } => {
                    group.counts.selected_checks += 1;
                    match state {
                        ComparisonCheckState::Passed => group.counts.checks_passed += 1,
                        ComparisonCheckState::Failed => group.counts.checks_failed += 1,
                        ComparisonCheckState::Running => group.counts.checks_running += 1,
                        ComparisonCheckState::NotRun => group.counts.checks_not_run += 1,
                        ComparisonCheckState::Stale => group.counts.checks_stale += 1,
                        ComparisonCheckState::Unavailable => group.counts.checks_unavailable += 1,
                    }
                    add_duration(
                        &mut group.check_execution,
                        elapsed_millis,
                        state == ComparisonCheckState::Running,
                    )?;
                }
                ComparisonEvidenceFact::Execution {
                    matching: true,
                    session_id,
                    transcript_epoch,
                    ..
                } => {
                    sessions.insert((session_id, transcript_epoch));
                }
                _ => {}
            }
        }
    }
    let sessions: Vec<_> = sessions.into_iter().collect();
    group.counts.matching_sessions = sessions
        .iter()
        .map(|(session, _)| session)
        .collect::<BTreeSet<_>>()
        .len();
    for session in sessions
        .iter()
        .map(|(session, _)| session)
        .collect::<BTreeSet<_>>()
    {
        let scopes: Vec<_> = sessions
            .iter()
            .filter(|(id, _)| id == session)
            .cloned()
            .collect();
        let usage = reads.usage(&scopes)?;
        if usage.observed_responses.is_some_and(|count| count > 0) {
            group.counts.sessions_with_usage += 1;
        } else {
            group.counts.sessions_without_usage += 1;
        }
    }
    group.usage = reads.usage(&sessions)?;
    if group.usage.status == ObservedUsageStatus::Observed
        && group.counts.sessions_without_usage > 0
    {
        group.usage.reason = Some(
            "Partial observations: some matching sessions have no supported response records."
                .into(),
        );
    }
    Ok(group)
}

pub fn workflow_comparisons(
    store: &Store,
    mut query: WorkflowComparisonQuery,
    incarnation: &str,
) -> Result<WorkflowComparisonPage> {
    normalize_query(&mut query)?;
    let cursor = query.cursor.take();
    let filters = selection_filters(&query)?;
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let revision = read_state_revision(&transaction)?;
    let last = read_position(cursor.as_deref(), incarnation, revision, &filters)?;
    if cursor.is_some() && last.len() != 1 {
        bail!("Invalid comparison page. Apply again.");
    }
    let snapshot = bounded_cursor(incarnation, revision, &filters, Vec::new())?;
    let tasks = read_tasks(&transaction)?;
    let units = select_units(&tasks, &query)?;
    let mut buckets = BTreeMap::<String, Vec<&ComparisonUnit<'_>>>::new();
    for unit in &units {
        buckets.entry(unit.group.clone()).or_default().push(unit);
    }
    let mut page=WorkflowComparisonPage {
        groups:Vec::new(),selection:query.clone(),total_selected_units:units.len(),displayed_group_units:0,
        cohort:query.cohort,
        date_basis:match query.cohort {ComparisonCohort::Task=>"tasks.created_at UTC",ComparisonCohort::Run=>"attempts.created_at UTC"}.into(),
        outcome_scope:"Each selected task or run counts once; execution filters retain the whole unit and its mixed attribution. Acceptance requires persisted human evidence.".into(),
        usage_scope:"Lifetime response observations for matching sessions only; session creation is the date basis of the separate existing session-usage view. Cached input and reasoning are subsets.".into(),
        timing_scope:"Observed process intervals, including overlapping processes, are distinct from task elapsed time and CPU time. Start/end observation latency is unmeasured. Task elapsed requires first-start coverage across all runs and exact human acceptance. Run interval samples include every contributing run; known partial sums have separate epoch coverage.".into(),
        wait_scope:"Recorded recovery intervals only; this does not account for all human, queue or other wait time.".into(),
        snapshot,next_cursor:None,
    };
    let mut reads = ComparisonReads::new(&transaction);
    for (key, bucket) in buckets {
        if last.first().is_some_and(|last| &key <= last) {
            continue;
        }
        if page.groups.len() == COMPARISON_PAGE_ROWS {
            break;
        }
        let group = group_projection(&mut reads, &bucket, &query)?;
        let count = group.counts.units;
        let previous = page.next_cursor.clone();
        page.next_cursor = Some(bounded_cursor(incarnation, revision, &filters, vec![key])?);
        page.groups.push(group);
        page.displayed_group_units += count;
        if serde_json::to_vec(&page)?.len() > COMPARISON_RESPONSE_BYTES {
            page.groups.pop();
            page.displayed_group_units -= count;
            page.next_cursor = previous;
            if page.groups.is_empty() {
                bail!("A saved comparison group exceeds the response limit. Inspect its recorded identities.");
            }
            transaction.commit()?;
            return Ok(page);
        }
    }
    let displayed_end = page.groups.last().map(|g| g.key.as_str());
    if !units
        .iter()
        .any(|unit| displayed_end.is_some_and(|key| unit.group.as_str() > key))
    {
        page.next_cursor = None;
    }
    transaction.commit()?;
    Ok(page)
}

fn fact_metric(fact: &ComparisonEvidenceFact) -> ComparisonEvidenceMetric {
    match fact {
        ComparisonEvidenceFact::Outcome { .. } | ComparisonEvidenceFact::Run { .. } => {
            ComparisonEvidenceMetric::Outcome
        }
        ComparisonEvidenceFact::Execution { .. } => ComparisonEvidenceMetric::Execution,
        ComparisonEvidenceFact::UsageObservation { .. } => ComparisonEvidenceMetric::Usage,
        ComparisonEvidenceFact::ClassificationUnavailable { metric, .. } => *metric,
        ComparisonEvidenceFact::HumanChange { .. } => ComparisonEvidenceMetric::HumanChange,
        ComparisonEvidenceFact::ProviderVerdict { .. } => ComparisonEvidenceMetric::ProviderVerdict,
        ComparisonEvidenceFact::Repair { .. } => ComparisonEvidenceMetric::Repair,
        ComparisonEvidenceFact::Recovery { .. } => ComparisonEvidenceMetric::Recovery,
        ComparisonEvidenceFact::Check { .. }
        | ComparisonEvidenceFact::UnknownCheckMatrix { .. } => ComparisonEvidenceMetric::Check,
        ComparisonEvidenceFact::Timing { .. }
        | ComparisonEvidenceFact::TaskTiming { .. }
        | ComparisonEvidenceFact::Boundary { .. }
        | ComparisonEvidenceFact::BoundaryConflict { .. } => ComparisonEvidenceMetric::Timing,
    }
}

fn bound_evidence_text(row: &mut ComparisonEvidenceRow) {
    let reason = match &mut row.fact {
        ComparisonEvidenceFact::Recovery { reason, .. }
        | ComparisonEvidenceFact::Check { reason, .. }
        | ComparisonEvidenceFact::TaskTiming { reason, .. } => reason.as_mut(),
        ComparisonEvidenceFact::Repair {
            classification_reason,
            ..
        } => classification_reason.as_mut(),
        ComparisonEvidenceFact::UnknownCheckMatrix { reason }
        | ComparisonEvidenceFact::ClassificationUnavailable { reason, .. } => Some(reason),
        ComparisonEvidenceFact::Timing { invocation } => invocation.unavailable_reason.as_mut(),
        _ => None,
    };
    let mut texts = Vec::new();
    if let Some(reason) = reason {
        texts.push(reason);
    }
    if let Some(attribution) = &mut row.recipe_attribution {
        texts.push(&mut attribution.recipe_title);
        if let Some(reason) = &mut attribution.occurrence_reason {
            texts.push(reason);
        }
    }
    for text in texts {
        if text.len() > 4096 {
            let mut end = 4096;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            row.truncated = true;
        }
    }
}

pub fn comparison_evidence(
    store: &Store,
    request: ComparisonEvidenceQuery,
    incarnation: &str,
) -> Result<ComparisonEvidencePage> {
    if request.selection.len() > 4096 || request.group.is_empty() || request.group.len() > 256 {
        bail!("Invalid evidence selection. Apply again.")
    }
    let mut query: WorkflowComparisonQuery = serde_json::from_str(&request.selection)
        .context("Invalid evidence filters. Apply again.")?;
    if query.cursor.is_some() {
        bail!("Evidence selection cannot contain a group cursor. Apply again.");
    }
    normalize_query(&mut query)?;
    let filters = selection_filters(&query)?;
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let revision = read_state_revision(&transaction)?;
    if !read_position(Some(&request.snapshot), incarnation, revision, &filters)?.is_empty() {
        bail!("Invalid applied comparison snapshot. Apply again.")
    }
    let evidence_filters = serde_json::json!({
        "operation":"workflow_comparison_evidence_v1","selection":query,
        "group":request.group,"metric":request.metric,
    });
    let last = read_position(
        request.cursor.as_deref(),
        incarnation,
        revision,
        &evidence_filters,
    )?;
    if request.cursor.is_some() && last.len() != 1 {
        bail!("Invalid evidence page. Apply again.");
    }
    let tasks = read_tasks(&transaction)?;
    let units = select_units(&tasks, &query)?;
    let mut selected = units
        .iter()
        .filter(|unit| unit.group == request.group)
        .peekable();
    if selected.peek().is_none() {
        bail!("This group is no longer part of the applied comparison. Apply again.");
    }
    let mut reads = ComparisonReads::new(&transaction);
    let mut rows = Vec::new();
    for unit in selected.clone() {
        rows.extend(
            unit_facts(&mut reads, unit, &query)?
                .into_iter()
                .filter(|row| {
                    request.metric == ComparisonEvidenceMetric::All
                        || fact_metric(&row.fact) == request.metric
                }),
        );
    }
    // Repair facts can use another run's timing, so move invocations into
    // response rows only after projecting repairs for every selected unit.
    if matches!(
        request.metric,
        ComparisonEvidenceMetric::All | ComparisonEvidenceMetric::Timing
    ) {
        for unit in selected {
            let mut invocation_rows = Vec::new();
            for run in &unit.runs {
                let timing = reads
                    .timings
                    .remove(&run.id)
                    .context("comparison timing was not loaded")?;
                for invocation in timing.invocations {
                    invocation_rows.push(evidence_row(
                        unit,
                        Some(run),
                        format!(
                            "timing:{}:{}",
                            invocation.session_id,
                            invocation.transcript_epoch.as_deref().unwrap_or("unknown")
                        ),
                        None,
                        ComparisonEvidenceFact::Timing { invocation },
                    ));
                }
            }
            invocation_rows.sort_by(|a, b| a.key.cmp(&b.key));
            invocation_rows.dedup_by(|a, b| a.key == b.key);
            rows.extend(invocation_rows);
        }
    }
    drop(reads);
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    let total_rows = rows.len();
    let mut page = ComparisonEvidencePage {
        rows: Vec::new(),
        total_rows,
        snapshot: request.snapshot,
        next_cursor: None,
    };
    let mut remaining = false;
    for mut row in rows {
        if last.first().is_some_and(|last| &row.key <= last) {
            continue;
        }
        if page.rows.len() == COMPARISON_PAGE_ROWS {
            remaining = true;
            break;
        }
        bound_evidence_text(&mut row);
        let previous = page.next_cursor.clone();
        page.next_cursor = Some(bounded_cursor(
            incarnation,
            revision,
            &evidence_filters,
            vec![row.key.clone()],
        )?);
        page.rows.push(row);
        if serde_json::to_vec(&page)?.len() > COMPARISON_RESPONSE_BYTES {
            page.rows.pop();
            page.next_cursor = previous;
            remaining = true;
            if page.rows.is_empty() {
                bail!("A saved evidence fact exceeds the response limit. Inspect its recorded identifiers.");
            }
            break;
        }
    }
    if !remaining {
        page.next_cursor = None;
    }
    transaction.commit()?;
    Ok(page)
}

fn boundary_facts(
    connection: &Connection,
    unit: &ComparisonUnit<'_>,
    run: &ComparisonRun,
    facts: &mut Vec<ComparisonEvidenceRow>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT b.id,b.session_id,b.transcript_epoch,b.process_identity_hash,b.kind,b.observed_at
         FROM workflow_execution_boundaries b JOIN role_generations g ON g.id=b.role_generation_id
         WHERE g.attempt_id=?1 ORDER BY b.id",
    )?;
    let boundaries = statement
        .query_map([&run.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, session, epoch, hash, kind, observed) in boundaries {
        let mut conflicts=connection.prepare(
            "SELECT fact_hash,observed_at FROM workflow_execution_boundary_conflicts WHERE boundary_id=?1 ORDER BY fact_hash",
        )?;
        for conflict in conflicts.query_map([&id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (hash, observed) = conflict?;
            facts.push(evidence_row(
                unit,
                Some(run),
                format!("conflict:{id}:{hash}"),
                None,
                ComparisonEvidenceFact::BoundaryConflict {
                    boundary_id: id.clone(),
                    fact_hash: hash,
                    observed_at: observed,
                },
            ));
        }
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("boundary:{id}"),
            None,
            ComparisonEvidenceFact::Boundary {
                boundary_id: id,
                session_id: session,
                transcript_epoch: epoch,
                process_identity_hash: hash,
                boundary_kind: kind,
                observed_at: observed,
            },
        ));
    }
    Ok(())
}

fn legacy_current_invocation(
    connection: &Connection,
    session: &TimingSession,
    epoch: &str,
) -> Result<Option<ComparisonInvocationTiming>> {
    if epoch != session.epoch || session.resume_count != 0 {
        return Ok(None);
    }
    let Some(process_json) = session.process_json.as_deref() else {
        return Ok(None);
    };
    let Ok(process) = serde_json::from_str::<crate::domain::ProcessIdentity>(process_json) else {
        return Ok(None);
    };
    let Some(start) = timestamp(&process.observed_started_at) else {
        return Ok(None);
    };
    let mut timing = empty_invocation(session, Some(epoch));
    timing.evidence_basis = Some("legacy_current_invocation_observations".into());
    timing.process_identity_hash = Some(crate::store::json_hash(&process)?);
    timing.observed_start = Some(process.observed_started_at.clone());
    let exit:Option<String>=connection.query_row(
        "SELECT exit_json FROM sessions WHERE id=?1 AND transcript_epoch=?2 AND process_identity_json=?3",
        params![session.id,epoch,process_json],|row|row.get(0),
    )?;
    let exit = exit
        .as_deref()
        .and_then(|exit| serde_json::from_str::<serde_json::Value>(exit).ok());
    let completion = exit.as_ref().filter(|exit| {
        exit.get("status").and_then(|v| v.as_str()).is_some()
            && exit
                .get("structured_result_required")
                .and_then(|v| v.as_bool())
                == Some(true)
            && exit
                .get("process_group_quiescent")
                .and_then(|v| v.as_bool())
                == Some(true)
            && exit
                .get("source")
                .and_then(|v| v.as_str())
                .is_none_or(|source| source == "attachment_transcript_reconciliation")
    });
    if let Some(end_text) = completion
        .and_then(|exit| exit.get("observed_at"))
        .and_then(|v| v.as_str())
    {
        timing.observed_end = Some(end_text.into());
        if let Some(end) = timestamp(end_text).filter(|end| *end >= start) {
            timing.elapsed_millis = Some((end - start).num_milliseconds());
            timing.state = ComparisonTimingState::Complete;
        } else {
            timing.unavailable_reason = Some("invalid_legacy_execution_interval".into());
        }
    } else {
        timing.state = if matches!(session.status.as_str(), "running" | "interrupt_requested") {
            ComparisonTimingState::Ongoing
        } else {
            ComparisonTimingState::Unavailable
        };
        timing.unavailable_reason = Some("missing_execution_end".into());
    }
    Ok(Some(timing))
}

fn usage_facts(
    connection: &Connection,
    unit: &ComparisonUnit<'_>,
    run: &ComparisonRun,
    query: &WorkflowComparisonQuery,
    facts: &mut Vec<ComparisonEvidenceRow>,
) -> Result<()> {
    if !run_matches(query, run) {
        return Ok(());
    }
    let executions: Vec<_> = run
        .executions
        .iter()
        .filter(|execution| execution_matches(query, &execution.identity))
        .map(|execution| (&execution.session, &execution.epoch))
        .collect();
    let scopes = serde_json::to_string(&executions)?;
    let mut statement=connection.prepare(
        "SELECT o.provider,o.native_thread_id,o.native_turn_id,o.response_id,o.session_id,
                o.transcript_epoch,o.role_generation_id,o.accepted_hook_event_id,o.invalid,o.received_at,
                o.input_tokens,o.cached_input_tokens,o.cache_write_input_tokens,o.output_tokens,
                o.reasoning_output_tokens,o.total_tokens,o.task_id,o.attempt_id,o.source_version,o.contract_revision
         FROM codex_usage_observations o WHERE EXISTS(SELECT 1 FROM json_each(?1) selected
             WHERE json_extract(selected.value,'$[0]')=o.session_id
               AND json_extract(selected.value,'$[1]')=o.transcript_epoch)
          OR EXISTS(SELECT 1 FROM json_each(o.conflict_owners_json) owner JOIN json_each(?1) selected
             ON json_extract(owner.value,'$.session_id')=json_extract(selected.value,'$[0]')
             WHERE json_extract(owner.value,'$.transcript_epoch') IS NULL
                OR json_extract(owner.value,'$.transcript_epoch')=json_extract(selected.value,'$[1]'))
         ORDER BY o.rowid",
    )?;
    let mut rows = statement.query([scopes])?;
    while let Some(row) = rows.next()? {
        let provider: String = row.get(0)?;
        let thread: String = row.get(1)?;
        let turn: String = row.get(2)?;
        let response: String = row.get(3)?;
        let key = crate::store::json_hash(&serde_json::json!([provider, thread, turn, response]))?;
        facts.push(evidence_row(
            unit,
            Some(run),
            format!("usage:{key}"),
            None,
            ComparisonEvidenceFact::UsageObservation {
                provider,
                native_thread_id: thread,
                native_turn_id: turn,
                response_id: response,
                session_id: row.get(4)?,
                transcript_epoch: row.get(5)?,
                role_generation_id: row.get(6)?,
                accepted_hook_event_id: row.get(7)?,
                invalid: row.get(8)?,
                received_at: row.get(9)?,
                owner_task_id: row.get(16)?,
                owner_attempt_id: row.get(17)?,
                source_version: row.get(18)?,
                contract_revision: row.get(19)?,
                counters: crate::domain::ObservedUsageCounters {
                    input_tokens: row.get(10)?,
                    cached_input_tokens: row.get(11)?,
                    cache_write_input_tokens: row.get(12)?,
                    output_tokens: row.get(13)?,
                    reasoning_output_tokens: row.get(14)?,
                    total_tokens: row.get(15)?,
                },
            },
        ));
    }
    Ok(())
}

fn check_command_matches(
    connection: &Connection,
    run: &ComparisonRun,
    check: &str,
    single: bool,
    receipt: &CheckReceipt,
) -> Result<bool> {
    let sql = if single {
        "SELECT c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd,w.path
         FROM execution_verification_checks c JOIN workflow_run_bindings b
           ON b.execution_config_revision_id=c.execution_config_revision_id
         LEFT JOIN workspaces w ON w.attempt_id=b.attempt_id
         WHERE b.attempt_id=?1 AND c.id=?2"
    } else {
        "SELECT c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd,w.path
         FROM trip_verification_checks c JOIN trip_selected_checks s ON s.check_id=c.id
         LEFT JOIN workspaces w ON w.attempt_id=s.attempt_id
         WHERE s.attempt_id=?1 AND c.id=?2 ORDER BY s.revision DESC LIMIT 1"
    };
    let spec = connection
        .query_row(sql, params![run.id, check], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .optional()?;
    let Some((kind, executable, arguments, shell, cwd, workspace)) = spec else {
        return Ok(false);
    };
    let Some(workspace) = workspace else {
        return Ok(false);
    };
    let (executable, arguments) = match kind.as_str() {
        "structured_argv" | "exact_argv" => {
            let Some(executable) = executable else {
                return Ok(false);
            };
            let Ok(arguments) =
                serde_json::from_str::<Vec<String>>(arguments.as_deref().unwrap_or("[]"))
            else {
                return Ok(false);
            };
            (executable, arguments)
        }
        "exact_shell" => {
            let Some(shell) = shell else {
                return Ok(false);
            };
            ("/bin/sh".into(), vec!["-lc".into(), shell])
        }
        _ => return Ok(false),
    };
    let recorded = serde_json::from_str::<Vec<String>>(&receipt.arguments).ok();
    Ok(receipt.executable == executable
        && recorded.as_ref() == Some(&arguments)
        && std::path::Path::new(&workspace).join(cwd).as_path()
            == std::path::Path::new(&receipt.cwd))
}

#[cfg(test)]
mod comparison_query_tests {
    use super::*;

    fn dataset() -> Result<Store> {
        let store = Store::open(std::path::Path::new(":memory:"))?;
        store.lock()?.execute_batch(r#"
            INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
              VALUES('p','Project','/fixture','fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO tasks(id,project_id,title,lifecycle,archived_at,created_at,updated_at) VALUES
              ('t','p','Accepted','done','2026-02-01','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
              ('never','p','Never run','backlog',NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
              ('cancelled','p','Cancelled','cancelled',NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
              ('failure','p','Failed run','in_progress',NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,
              human_acceptance_at,candidate_hash,selected_checks_revision,created_at,updated_at) VALUES
              ('a1','t','ctx1','implementation','base',1,'held',NULL,'candidate',2,'2026-01-02T00:00:00Z','2026-01-02T00:00:00Z'),
              ('a2','t','ctx2','done','base',1,'done','2026-01-03T01:00:00Z','accepted',0,'2026-01-03T00:00:00Z','2026-01-03T01:00:00Z'),
              ('a3','failure','ctx3','implementation','base',1,'failed',NULL,NULL,0,'2026-01-04T00:00:00Z','2026-01-04T00:00:00Z');
            INSERT INTO workflow_run_bindings(attempt_id,family,definition_state,evaluator_revision,selection_hash,scope_hash,created_at) VALUES
              ('a1','trip','unavailable',1,'selection','scope','2026-01-02T00:00:00Z'),
              ('a2','single_agent','unavailable',1,'selection','scope','2026-01-03T00:00:00Z');
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at) VALUES
              ('g1','a1','implementer','codex',1,1,'running','authority','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z'),
              ('g2','a2','single_agent','claude',1,1,'exited','authority','2026-01-03T00:00:00Z','2026-01-03T00:00:00Z'),
              ('gr','a1','code_reviewer','codex',1,1,'exited','authority','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at) VALUES
              ('s1','g1','codex','running','{"model":"exact","effort":"medium"}','fixture','e1','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z'),
              ('s2','g2','claude','exited','{"model":"other","effort":"high"}','fixture','e2','2026-01-03T00:00:00Z','2026-01-03T00:00:00Z'),
              ('sr','gr','codex','exited','{"model":"exact","effort":"medium"}','fixture','er','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z');
            INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,session_id,prompt_hash,handoff_hash,delivery_state,verdict,created_at,updated_at)
              VALUES('review','a1','code','candidate','gr','sr','prompt','handoff','finished','request_changes','2026-01-02T00:00:00Z','2026-01-02T00:01:00Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
              VALUES('result','review-op','sr','gr','request_changes','Repair','{}','{"review_request_id":"review"}','2026-01-02T00:00:30Z','2026-01-02T00:01:00Z');
            INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at,resolved_at)
              VALUES('recovery','s1','a1','resolved_quiescent','{"reason":"interrupted"}','2026-01-02T00:00:00Z','2026-01-02T00:00:10Z','2026-01-02T00:00:10Z');
            INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
              VALUES('workspace','a1','fixture','/fixture','base','head','{}','ready','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z');
            INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,preflight_json,verification_json,source_hash,overlay_hash,configuration_hash,created_at)
              VALUES('config','p',1,'active','{}','{}','{}','{}','source','overlay','config-hash','2026-01-01T00:00:00Z');
            INSERT INTO trip_verification_checks(id,project_id,config_revision_id,check_key,category,command_kind,executable,arguments_json,cwd,timeout_seconds,
              acceptance_rows_json,relevant_inputs_json,invalidation_json,original_text) VALUES
              ('check','p','config','check','test','exact_argv','check','[]','.',60,'[]','[]','[]','Check'),
              ('missing','p','config','missing','test','exact_argv','check','[]','.',60,'[]','[]','[]','Missing');
            INSERT INTO trip_selected_checks(attempt_id,revision,check_id,selected_by_generation_id,created_at) VALUES
              ('a1',1,'check','g1','2026-01-02T00:00:00Z'),
              ('a1',2,'check','g1','2026-01-02T00:00:01Z'),
              ('a1',2,'missing','g1','2026-01-02T00:00:01Z');
            INSERT INTO check_runs(id,attempt_id,candidate_hash,executable,arguments_json,cwd,status,exit_code,check_id,selected_check_revision,inputs_hash,freshness_state,elapsed_millis,created_at) VALUES
              ('old','a1','candidate','check','[]','/fixture/','finished',1,'check',1,'inputs','current',30,'2026-01-02T00:00:02Z'),
              ('failed','a1','candidate','check','[]','/fixture/','finished',1,'check',2,'inputs','current',40,'2026-01-02T00:00:03Z'),
              ('passed','a1','candidate','check','[]','/fixture/','finished',0,'check',2,'inputs','current',50,'2026-01-02T00:00:04Z');
            INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at)
              VALUES('snapshot','a1','candidate','base','candidate','{}',1,'2026-01-02T00:00:00Z');
            INSERT INTO rework_intents(id,operation_id,parent_attempt_id,new_attempt_id,snapshot_id,feedback,carry_plan_approval,scope_hash,configuration_hash,state,created_at,updated_at)
              VALUES('rework','human-op','a1','a2','snapshot','Human changes',0,'scope','config','finished','2026-01-02T00:04:00Z','2026-01-02T00:04:00Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
              VALUES('decision','human-op','human','human.command.applied','attempt','a2','{}','2026-01-02T00:04:00Z');
            INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
              VALUES('hook','s1','g1','codex','Stop','{}',1,1,'peer','managed_process_group_untrusted_payload','2026-01-02T00:03:00Z');
        "#)?;
        for (id, input, output) in [("zero", 0, 0), ("observed", 10, 5)] {
            store.lock()?.execute(
                "INSERT INTO codex_usage_observations(provider,native_thread_id,native_turn_id,response_id,
                 task_id,attempt_id,role_generation_id,session_id,transcript_epoch,invocation_start,accepted_hook_event_id,
                 input_tokens,cached_input_tokens,output_tokens,reasoning_output_tokens,total_tokens,
                 source_version,contract_revision,source_identity_json,received_at)
                 VALUES('codex','native','turn',?1,'t','a1','g1','s1','e1',0,'hook',?2,0,?3,0,?4,?5,?6,'{}','2026-01-02T00:03:00Z')",
                params![id,input,output,input+output,crate::providers::codex::SOURCE_VERSION,crate::providers::codex::USAGE_CONTRACT_REVISION],
            )?;
        }
        Ok(store)
    }

    #[test]
    fn comparison_dataset_keeps_whole_units_and_reduces_selected_reruns() -> Result<()> {
        let store = dataset()?;
        let query = WorkflowComparisonQuery {
            group_by: ComparisonDimension::Provider,
            ..Default::default()
        };
        let page = workflow_comparisons(&store, query.clone(), "instance")?;
        assert_eq!(page.total_selected_units, 4);
        let mixed = page
            .groups
            .iter()
            .find(|group| group.key == "mixed")
            .unwrap();
        assert_eq!(mixed.counts.units, 1);
        assert_eq!(mixed.counts.accepted, 1);
        assert_eq!(mixed.counts.human_changes, 1);
        assert_eq!(mixed.counts.provider_verdicts, 1);
        assert_eq!(mixed.counts.provider_negative_verdicts, 1);
        assert_eq!(mixed.counts.repairs_authorized, 1);
        assert_eq!(mixed.counts.repairs_started, 0);
        assert_eq!(mixed.counts.repair_classification_unavailable, 1);
        assert_eq!(mixed.counts.infrastructure_recoveries, 1);
        assert_eq!(mixed.recorded_recovery_waits.sum_millis, Some(10_000));
        assert_eq!(mixed.counts.selected_checks, 2);
        assert_eq!(mixed.counts.checks_passed, 1);
        assert_eq!(mixed.counts.checks_failed, 0);
        assert_eq!(mixed.counts.checks_not_run, 1);
        assert_eq!(mixed.counts.unknown_check_matrices, 1);
        assert_eq!(mixed.check_execution.sum_millis, Some(50));
        assert_eq!(mixed.elapsed_to_acceptance.complete_samples, 0);
        assert_eq!(mixed.usage.status, ObservedUsageStatus::Observed);
        assert_eq!(mixed.usage.observed_responses, Some(2));
        assert_eq!(mixed.usage.counters.as_ref().unwrap().total_tokens, 15);
        assert!(mixed.usage.reason.as_ref().unwrap().contains("Partial"));
        let evidence = comparison_evidence(
            &store,
            ComparisonEvidenceQuery {
                selection: serde_json::to_string(&query)?,
                snapshot: page.snapshot,
                group: mixed.key.clone(),
                metric: ComparisonEvidenceMetric::All,
                cursor: None,
            },
            "instance",
        )?;
        assert!(evidence
            .rows
            .iter()
            .any(|row| matches!(&row.fact,ComparisonEvidenceFact::Check {
            receipt_id:Some(id),selected:false,..} if id=="failed")));
        assert!(evidence.rows.iter().any(|row| matches!(
            &row.fact,
            ComparisonEvidenceFact::Outcome { archived: true, .. }
        )));
        let filtered = workflow_comparisons(
            &store,
            WorkflowComparisonQuery {
                provider: Some("claude".into()),
                ..query.clone()
            },
            "instance",
        )?;
        assert_eq!(filtered.total_selected_units, 1);
        assert_eq!(filtered.groups[0].key, "mixed");
        assert_eq!(filtered.groups[0].counts.matching_sessions, 1);
        assert_eq!(
            filtered.groups[0].usage.status,
            ObservedUsageStatus::Unavailable
        );
        let old_usage = crate::workflow::usage_comparisons(&store, Default::default(), "instance")?;
        assert_eq!(
            old_usage.groups[0]
                .usage
                .counters
                .as_ref()
                .unwrap()
                .total_tokens,
            15
        );
        let wrong_tuple = workflow_comparisons(
            &store,
            WorkflowComparisonQuery {
                provider: Some("claude".into()),
                model: Some("exact".into()),
                ..query.clone()
            },
            "instance",
        )?;
        assert_eq!(wrong_tuple.total_selected_units, 0);
        let unknown = page
            .groups
            .iter()
            .find(|group| group.key == "unavailable")
            .unwrap();
        assert_eq!(unknown.counts.units, 3);
        assert_eq!(unknown.counts.cancelled, 1);
        assert_eq!(unknown.counts.incomplete, 2);
        let cross_run = workflow_comparisons(
            &store,
            WorkflowComparisonQuery {
                workflow: Some("trip".into()),
                provider: Some("claude".into()),
                ..query.clone()
            },
            "instance",
        )?;
        assert_eq!(cross_run.total_selected_units, 0);
        let runs = workflow_comparisons(
            &store,
            WorkflowComparisonQuery {
                cohort: ComparisonCohort::Run,
                start_date: Some("2026-01-03".into()),
                end_date: Some("2026-01-03".into()),
                ..query
            },
            "instance",
        )?;
        assert_eq!(runs.total_selected_units, 1);
        assert_eq!(runs.groups[0].counts.accepted, 1);
        let failed = workflow_comparisons(
            &store,
            WorkflowComparisonQuery {
                cohort: ComparisonCohort::Run,
                start_date: Some("2026-01-04".into()),
                ..Default::default()
            },
            "instance",
        )?;
        assert_eq!(failed.total_selected_units, 1);
        assert_eq!(failed.groups[0].counts.failed, 1);
        for (freshness, exit, expected) in [
            ("failed", Some(1), ComparisonCheckState::Failed),
            ("failed", None, ComparisonCheckState::Unavailable),
            ("stale", Some(1), ComparisonCheckState::Stale),
        ] {
            store.lock()?.execute(
                "UPDATE check_runs SET freshness_state=?1,exit_code=?2,launch_state='finished' WHERE id='passed'",
                params![freshness, exit],
            )?;
            let selection = WorkflowComparisonQuery {
                cohort: ComparisonCohort::Run,
                workflow: Some("trip".into()),
                ..Default::default()
            };
            let page = workflow_comparisons(&store, selection.clone(), "instance")?;
            let counts = &page.groups[0].counts;
            assert_eq!(counts.selected_checks, 2);
            assert_eq!(
                counts.checks_failed,
                usize::from(expected == ComparisonCheckState::Failed)
            );
            assert_eq!(
                counts.checks_unavailable,
                usize::from(expected == ComparisonCheckState::Unavailable)
            );
            assert_eq!(
                counts.checks_stale,
                usize::from(expected == ComparisonCheckState::Stale)
            );
            let evidence = comparison_evidence(
                &store,
                ComparisonEvidenceQuery {
                    selection: serde_json::to_string(&selection)?,
                    snapshot: page.snapshot,
                    group: page.groups[0].key.clone(),
                    metric: ComparisonEvidenceMetric::Check,
                    cursor: None,
                },
                "instance",
            )?;
            assert!(evidence.rows.iter().any(|row| matches!(&row.fact,
                ComparisonEvidenceFact::Check { receipt_id: Some(id), state, selected: true, .. }
                    if id == "passed" && state == &expected)));
        }
        store.lock()?.execute_batch(r#"
            UPDATE sessions SET transcript_epoch='resumed',resume_count=1,
                capability_identity_json='{"model":"exact","effort":"medium"}' WHERE id='s1';
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,prior_transcript_epoch,
                launch_config_json,capability_key,capability_identity_json,state,created_at,updated_at)
              VALUES('resume','s1',1,'resumed','e1','{}','capability',
                '{"model":"exact","effort":"medium"}','finished','2026-01-02T00:04:00Z','2026-01-02T00:04:00Z');
            UPDATE codex_usage_observations SET transcript_epoch='resumed',input_tokens=2,output_tokens=1,total_tokens=3 WHERE response_id='zero';
        "#)?;
        let resumed_query = WorkflowComparisonQuery {
            cohort: ComparisonCohort::Run,
            group_by: ComparisonDimension::Model,
            workflow: Some("trip".into()),
            provider: Some("codex".into()),
            model: Some("exact".into()),
            effort: Some("medium".into()),
            ..Default::default()
        };
        let resumed = workflow_comparisons(&store, resumed_query.clone(), "instance")?;
        assert_eq!(resumed.total_selected_units, 1);
        assert_eq!(
            resumed.groups[0].attribution.state,
            ComparisonAttributionState::Known
        );
        assert_eq!(resumed.groups[0].attribution.values, vec!["exact"]);
        assert_eq!(resumed.groups[0].usage.observed_responses, Some(2));
        assert_eq!(
            resumed.groups[0]
                .usage
                .counters
                .as_ref()
                .unwrap()
                .total_tokens,
            18
        );
        let evidence = comparison_evidence(
            &store,
            ComparisonEvidenceQuery {
                selection: serde_json::to_string(&resumed_query)?,
                snapshot: resumed.snapshot,
                group: resumed.groups[0].key.clone(),
                metric: ComparisonEvidenceMetric::Usage,
                cursor: None,
            },
            "instance",
        )?;
        for epoch in ["e1", "resumed"] {
            assert!(evidence.rows.iter().any(|row| matches!(&row.fact,
                ComparisonEvidenceFact::UsageObservation { transcript_epoch, .. } if transcript_epoch == epoch)));
        }
        store.lock()?.execute(
            r#"UPDATE resume_invocations SET capability_identity_json='{"model":"changed","effort":"high"}' WHERE id='resume'"#,
            [],
        )?;
        let changed = workflow_comparisons(&store, resumed_query.clone(), "instance")?;
        assert_eq!(
            changed.groups[0].attribution.state,
            ComparisonAttributionState::Mixed
        );
        assert_eq!(changed.groups[0].usage.observed_responses, Some(1));
        assert_eq!(
            changed.groups[0]
                .usage
                .counters
                .as_ref()
                .unwrap()
                .total_tokens,
            15
        );
        store.lock()?.execute(
            "UPDATE workflow_run_bindings SET family='single_agent' WHERE attempt_id='a1'",
            [],
        )?;
        let unknown_initial = read_tasks(&*store.lock()?)?;
        let initial = unknown_initial
            .iter()
            .flat_map(|t| &t.runs)
            .flat_map(|r| &r.executions)
            .find(|e| e.session == "s1" && e.epoch == "e1")
            .unwrap();
        assert!(initial.identity.model.is_none());
        assert!(initial.identity.effort.is_none());
        Ok(())
    }

    #[test]
    fn comparison_first_evidence_refuses_boundary_only_change_and_wrong_selection() -> Result<()> {
        let store = dataset()?;
        let query = WorkflowComparisonQuery::default();
        let page = workflow_comparisons(&store, query.clone(), "instance")?;
        let group = page.groups[0].key.clone();
        let request = |snapshot: String,
                       selection: &WorkflowComparisonQuery|
         -> Result<ComparisonEvidenceQuery> {
            Ok(ComparisonEvidenceQuery {
                selection: serde_json::to_string(selection)?,
                snapshot,
                group: group.clone(),
                metric: ComparisonEvidenceMetric::All,
                cursor: None,
            })
        };
        assert!(comparison_evidence(
            &store,
            request(page.snapshot.clone(), &query)?,
            "other-instance"
        )
        .is_err());
        let other = WorkflowComparisonQuery {
            provider: Some("claude".into()),
            ..query.clone()
        };
        assert!(
            comparison_evidence(&store, request(page.snapshot.clone(), &other)?, "instance")
                .is_err()
        );
        store.lock()?.execute_batch(r#"
            INSERT INTO workflow_execution_boundaries(id,session_id,role_generation_id,transcript_epoch,
                process_identity_hash,process_identity_json,kind,observed_at)
              VALUES('start','s1','g1','e1','identity','{}','started','2026-01-02T00:02:00Z');
        "#)?;
        let error =
            comparison_evidence(&store, request(page.snapshot, &query)?, "instance").unwrap_err();
        assert!(error.to_string().contains("Apply again"));
        let fresh = workflow_comparisons(&store, query.clone(), "instance")?;
        let fresh_request = request(fresh.snapshot, &query)?;
        let evidence = comparison_evidence(&store, fresh_request.clone(), "instance")?;
        assert!(evidence.rows.iter().any(|row|matches!(&row.fact,ComparisonEvidenceFact::Boundary {boundary_id,..} if boundary_id=="start")));
        assert!(evidence.rows.iter().any(|row| matches!(
            &row.fact,
            ComparisonEvidenceFact::Repair { started_session_id, .. }
                if started_session_id.as_deref() == Some("s1")
        )));
        assert_eq!(fresh.groups[0].counts.repairs_started, 1);
        for metric in [
            ComparisonEvidenceMetric::Outcome,
            ComparisonEvidenceMetric::Repair,
            ComparisonEvidenceMetric::Timing,
        ] {
            let filtered = comparison_evidence(
                &store,
                ComparisonEvidenceQuery {
                    metric,
                    ..fresh_request.clone()
                },
                "instance",
            )?;
            let expected: Vec<_> = evidence
                .rows
                .iter()
                .filter(|row| fact_metric(&row.fact) == metric)
                .collect();
            assert!(!expected.is_empty());
            assert_eq!(filtered.total_rows, expected.len());
            assert_eq!(
                serde_json::to_value(&filtered.rows)?,
                serde_json::to_value(expected)?
            );
        }
        let cursor = bounded_cursor(
            "instance",
            read_state_revision(&*store.lock()?)?,
            &selection_filters(&query)?,
            vec![group.clone()],
        )?;
        let changed = WorkflowComparisonQuery {
            cursor: Some(cursor),
            group_by: ComparisonDimension::Provider,
            ..query
        };
        assert!(workflow_comparisons(&store, changed, "instance").is_err());
        for index in 0..55 {
            store.lock()?.execute(
                "INSERT INTO recovery_records(id,attempt_id,state,detail_json,created_at,updated_at)
                 VALUES(?1,'a1','attention_required',?2,'2026-01-02T00:00:00Z','2026-01-02T00:00:00Z')",
                params![format!("page-{index:03}"),serde_json::json!({"reason":"x".repeat(5000)}).to_string()],
            )?;
        }
        let query = WorkflowComparisonQuery::default();
        let applied = workflow_comparisons(&store, query.clone(), "instance")?;
        let mut first = request(applied.snapshot, &query)?;
        let page = comparison_evidence(&store, first.clone(), "instance")?;
        assert_eq!(page.rows.len(), 50);
        assert!(page.total_rows > 50);
        assert!(page.rows.iter().any(|row| row.truncated));
        assert!(serde_json::to_vec(&page)?.len() <= COMPARISON_RESPONSE_BYTES);
        first.cursor = page.next_cursor;
        let next = comparison_evidence(&store, first.clone(), "instance")?;
        assert!(!next.rows.is_empty());
        assert!(page
            .rows
            .iter()
            .all(|a| next.rows.iter().all(|b| a.key != b.key)));
        store.lock()?.execute_batch(
            "INSERT INTO workflow_execution_boundary_conflicts(boundary_id,fact_hash,observed_at)
             VALUES('start','conflict','2026-01-02T00:02:01Z')",
        )?;
        assert!(comparison_evidence(&store, first, "instance")
            .unwrap_err()
            .to_string()
            .contains("Apply again"));
        Ok(())
    }
}
