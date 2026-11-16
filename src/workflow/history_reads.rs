use super::{fold_observed_usage, history_kind_name, HISTORY_SOURCES_SQL};
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashSet;

/// Durable task history uses exact entity ownership, including prior attempts.
pub fn task_activity(
    store: &Store,
    task_id: &str,
    cursor: Option<&str>,
) -> Result<crate::domain::TaskActivityPage> {
    let connection = store.lock()?;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
        params![task_id],
        |r| r.get(0),
    )?;
    if !exists {
        bail!("task not found")
    }
    let before = if let Some(cursor) = cursor {
        let (owner, sequence) = cursor
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("malformed activity cursor"))?;
        let sequence: i64 = sequence
            .parse()
            .map_err(|_| anyhow!("malformed activity cursor"))?;
        if owner != task_id || sequence <= 0 {
            bail!("activity cursor belongs to another task or is invalid")
        }
        sequence
    } else {
        i64::MAX
    };
    let mut statement = connection.prepare("SELECT e.rowid,e.id,e.actor_kind,e.actor_id,e.event_code,e.entity_kind,e.entity_id,e.created_at,e.detail_json,
        CASE WHEN e.entity_kind='attempt' THEN e.entity_id
        WHEN e.entity_kind='session' THEN (SELECT rg.attempt_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=e.entity_id)
        WHEN e.entity_kind IN ('generation','role_generation') THEN (SELECT attempt_id FROM role_generations WHERE id=e.entity_id)
        WHEN e.entity_kind='role_result' THEN (SELECT rg.attempt_id FROM role_results x JOIN role_generations rg ON rg.id=x.role_generation_id WHERE x.id=e.entity_id)
        WHEN e.entity_kind='guidance' THEN (SELECT attempt_id FROM guidance_messages WHERE id=e.entity_id)
        WHEN e.entity_kind='check_run' THEN (SELECT attempt_id FROM check_runs WHERE id=e.entity_id)
        WHEN e.entity_kind='review_request' THEN (SELECT attempt_id FROM review_requests WHERE id=e.entity_id) END,
        CASE WHEN e.entity_kind='comment' THEN (SELECT body FROM task_comments WHERE id=e.entity_id) END
        FROM audit_events e WHERE e.rowid<=?2 AND (
        (e.entity_kind='task' AND e.entity_id=?1) OR
        (e.entity_kind='attempt' AND EXISTS(SELECT 1 FROM attempts a WHERE a.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='session' AND EXISTS(SELECT 1 FROM sessions x JOIN role_generations rg ON rg.id=x.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind IN ('generation','role_generation') AND EXISTS(SELECT 1 FROM role_generations x JOIN attempts a ON a.id=x.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='permission_request' AND EXISTS(SELECT 1 FROM permission_requests x WHERE x.id=e.entity_id AND x.task_id=?1)) OR
        (e.entity_kind='review_request' AND EXISTS(SELECT 1 FROM review_requests x JOIN attempts a ON a.id=x.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='check_run' AND EXISTS(SELECT 1 FROM check_runs x JOIN attempts a ON a.id=x.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='guidance' AND EXISTS(SELECT 1 FROM guidance_messages x JOIN attempts a ON a.id=x.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='role_result' AND EXISTS(SELECT 1 FROM role_results x JOIN role_generations rg ON rg.id=x.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='explorer_decision' AND EXISTS(SELECT 1 FROM trip_explorer_decisions x JOIN attempts a ON a.id=x.attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='rework_intent' AND EXISTS(SELECT 1 FROM rework_intents x JOIN attempts a ON a.id=x.parent_attempt_id WHERE x.id=e.entity_id AND a.task_id=?1)) OR
        (e.entity_kind='comment' AND EXISTS(SELECT 1 FROM task_comments x WHERE x.id=e.entity_id AND x.task_id=?1))) ORDER BY e.rowid DESC LIMIT 42")?;
    let rows = statement.query_map(params![task_id,before],|r| Ok((r.get::<_,i64>(0)?,serde_json::json!({
        "id":r.get::<_,String>(1)?,"actor_kind":r.get::<_,String>(2)?,"actor_id":r.get::<_,Option<String>>(3)?,
        "event_code":r.get::<_,String>(4)?,"entity_kind":r.get::<_,String>(5)?,"entity_id":r.get::<_,String>(6)?,
        "created_at":r.get::<_,String>(7)?,"detail":r.get::<_,String>(8)?,"attempt_id":r.get::<_,Option<String>>(9)?,"body":r.get::<_,Option<String>>(10)?
    }))))?.collect::<rusqlite::Result<Vec<_>>>()?;
    if cursor.is_some() && rows.first().map(|(sequence, _)| *sequence) != Some(before) {
        bail!("activity cursor does not identify an event belonging to this task")
    }
    let mut events = Vec::new();
    let mut bytes = 0;
    let mut last = None;
    let mut more = false;
    for (sequence, event) in rows {
        if cursor.is_some() && sequence == before {
            continue;
        }
        let size = serde_json::to_vec(&event)?.len();
        if events.len() == 40 || (!events.is_empty() && bytes + size > 128 * 1024) {
            more = true;
            break;
        }
        // Bound oversized legacy details independently from the persisted record.
        let mut event = event;
        if size > 32 * 1024 {
            event["detail"] = serde_json::Value::Null;
            event["detail_truncated"] = serde_json::json!(true);
        }
        bytes += serde_json::to_vec(&event)?.len();
        last = Some(sequence);
        events.push(event);
    }
    Ok(crate::domain::TaskActivityPage {
        events,
        next_cursor: if more {
            last.map(|n| format!("{task_id}:{n}"))
        } else {
            None
        },
        historical_associations_incomplete: true,
    })
}

const HISTORY_RESPONSE_LIMIT: usize = 128 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryReadCursor {
    incarnation: String,
    revision: i64,
    filters: serde_json::Value,
    last: Vec<String>,
}

pub(crate) fn history_read_position(
    cursor: Option<&str>,
    incarnation: &str,
    revision: i64,
    filters: &serde_json::Value,
) -> Result<Vec<String>> {
    let Some(cursor) = cursor else {
        return Ok(Vec::new());
    };
    if cursor.len() > 8192 {
        bail!("Results changed or the page is invalid. Refresh these results.")
    }
    let cursor: HistoryReadCursor = serde_json::from_str(cursor)
        .map_err(|_| anyhow!("This page is invalid. Refresh these results."))?;
    if cursor.incarnation != incarnation
        || cursor.revision != revision
        || &cursor.filters != filters
    {
        bail!("Results changed. Refresh these results before loading another page.")
    }
    Ok(cursor.last)
}

pub(crate) fn history_cursor(
    incarnation: &str,
    revision: i64,
    filters: &serde_json::Value,
    last: Vec<String>,
) -> Result<String> {
    Ok(serde_json::to_string(&HistoryReadCursor {
        incarnation: incarnation.into(),
        revision,
        filters: filters.clone(),
        last,
    })?)
}

fn history_hit(row: &rusqlite::Row<'_>) -> Result<crate::domain::HistorySearchHit> {
    let kind: String = row.get(0)?;
    let kind = match kind.as_str() {
        "task" => crate::domain::HistorySourceKind::Task,
        "comment" => crate::domain::HistorySourceKind::Comment,
        "report" => crate::domain::HistorySourceKind::Report,
        "review" => crate::domain::HistorySourceKind::Review,
        "explorer_decision" => crate::domain::HistorySourceKind::ExplorerDecision,
        "result_acceptance" => crate::domain::HistorySourceKind::ResultAcceptance,
        "rework" => crate::domain::HistorySourceKind::Rework,
        _ => bail!("This saved source has an unrecognized kind and is unavailable."),
    };
    let hit = crate::domain::HistorySearchHit {
        source: crate::domain::HistoricalSourceKey {
            kind,
            id: row.get(1)?,
            task_id: row.get(2)?,
            attempt_id: row.get(3)?,
        },
        created_at: row.get(4)?,
        snippet: row.get(5)?,
        project_id: row.get(6)?,
        project_name: row.get(7)?,
        task_title: row.get(8)?,
        archived: row.get(9)?,
    };
    if [&hit.source.id, &hit.source.task_id, &hit.project_id]
        .into_iter()
        .chain(hit.source.attempt_id.as_ref())
        .any(|id| id.len() > 256)
        || hit.created_at.len() > 128
    {
        bail!("This saved source has oversized identifiers or a date and is unavailable.")
    }
    Ok(hit)
}

pub fn history_search(
    store: &Store,
    mut query: crate::domain::HistorySearchQuery,
    incarnation: &str,
) -> Result<crate::domain::HistorySearchPage> {
    query.query = query.query.trim().to_owned();
    if query.query.is_empty() || query.query.len() > 256 {
        bail!("Enter a search between 1 and 256 UTF-8 bytes.")
    }
    if query.project_id.as_ref().is_some_and(|id| id.len() > 256) {
        bail!("Invalid project filter.")
    }
    let cursor = query.cursor.take();
    let filters = serde_json::to_value(&query)?;
    let connection = store.lock()?;
    let revision = crate::store::read_state_revision(&connection)?;
    let last = history_read_position(cursor.as_deref(), incarnation, revision, &filters)?;
    if cursor.is_some() && last.len() != 3 {
        bail!("Invalid search page. Refresh these results.")
    }
    let mut statement = connection.prepare(&format!(
        "{HISTORY_SOURCES_SQL}
      SELECT kind,id,task_id,attempt_id,created_at,
        substr(text,MAX(1,instr(lower(text),lower(?1))-80),320),project_id,
        substr(display_name,1,256),substr(title,1,256),archived
      FROM visible WHERE instr(lower(text),lower(?1))>0
        AND (?2 IS NULL OR project_id=?2) AND (?3 IS NULL OR kind=?3)
        AND (?4=0 OR archived=0)
        AND (?5 IS NULL OR (created_at,kind,id)>(?5,?6,?7))
      ORDER BY created_at,kind,id LIMIT 41"
    ))?;
    let mut rows = statement.query(params![
        query.query,
        query.project_id,
        query.kind.map(history_kind_name),
        query.exclude_archived,
        last.first(),
        last.get(1),
        last.get(2)
    ])?;
    let mut page = crate::domain::HistorySearchPage {
        results: vec![],
        next_cursor: None,
    };
    while let Some(row) = rows.next()? {
        if page.results.len() == 40 {
            return Ok(page);
        }
        let hit = history_hit(row)?;
        let next = history_cursor(
            incarnation,
            revision,
            &filters,
            vec![
                hit.created_at.clone(),
                history_kind_name(hit.source.kind).into(),
                hit.source.id.clone(),
            ],
        )?;
        page.results.push(hit);
        let previous_cursor = page.next_cursor.replace(next);
        // Include the escaped cursor in the byte cap and resume after the last
        // emitted row if this row needs another page.
        if serde_json::to_vec(&page)?.len() > HISTORY_RESPONSE_LIMIT {
            page.results.pop();
            if page.results.is_empty() {
                bail!("This saved source is too large to display in search results.")
            }
            page.next_cursor = previous_cursor;
            return Ok(page);
        }
    }
    page.next_cursor = None;
    Ok(page)
}

pub fn historical_source(
    store: &Store,
    key: &crate::domain::HistoricalSourceKey,
) -> Result<Option<crate::domain::HistoricalSource>> {
    let connection = store.lock()?;
    // SQLite substr stops at NUL. Keep short sources verbatim and compare the
    // original byte length before declaring that a longer excerpt is complete.
    let mut statement = connection.prepare(&format!(
        "{HISTORY_SOURCES_SQL}
      SELECT kind,id,task_id,attempt_id,created_at,'',project_id,
        substr(display_name,1,256),substr(title,1,256),archived,
        CASE WHEN length(CAST(text AS BLOB))<=131072 THEN text ELSE substr(text,1,131073) END,
        length(CAST(text AS BLOB))
      FROM visible WHERE kind=?1 AND id=?2 AND task_id=?3 AND attempt_id IS ?4"
    ))?;
    let mut rows = statement.query(params![
        history_kind_name(key.kind),
        key.id,
        key.task_id,
        key.attempt_id
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let text: String = row.get(10)?;
    let truncated = row.get::<_, usize>(11)? > text.len();
    let mut source = crate::domain::HistoricalSource {
        hit: history_hit(row)?,
        truncated,
        text,
    };
    // Bound serialized bytes, including JSON escapes and all display fields.
    while serde_json::to_vec(&source)?.len() > HISTORY_RESPONSE_LIMIT {
        if source.text.is_empty() {
            bail!("This source has oversized identifiers and is unavailable.")
        }
        let keep = source.text.chars().count() * 3 / 4;
        source.text = source.text.chars().take(keep).collect();
        source.truncated = true;
    }
    Ok(Some(source))
}

struct UsageScopeSession {
    task_id: String,
    attempt_id: String,
    generation_id: String,
    project_id: String,
    project_name: String,
    workflow: String,
    role: String,
    provider: String,
    created_at: String,
    visible: bool,
}

pub(crate) fn usage_workflow_family(binding: Option<String>, identity: &str) -> String {
    match binding.as_deref() {
        Some("trip") => "trip",
        Some("single_agent") => "single_agent",
        Some(_) => "unknown",
        None => match identity {
            crate::trip::WORKFLOW_ID
            | "trip-explorer-0.11.0-llmrelay-1"
            | "trip-explorer-0.9.0-llmrelay-1"
            | "trip-v1" => "trip",
            _ => "unknown",
        },
    }
    .into()
}

pub(crate) fn usage_date(value: Option<&str>) -> Result<Option<chrono::NaiveDate>> {
    value
        .map(|value| {
            if value.len() != 10 {
                bail!("Choose a valid UTC date in YYYY-MM-DD form.")
            }
            let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| anyhow!("Choose a valid UTC date in YYYY-MM-DD form."))?;
            if date.format("%Y-%m-%d").to_string() != value {
                bail!("Choose a valid UTC date in YYYY-MM-DD form.")
            }
            Ok(date)
        })
        .transpose()
}

struct UsageBucket {
    group: crate::domain::UsageComparisonGroup,
    sum: Option<crate::domain::ObservedUsageCounters>,
    observed_sessions: HashSet<String>,
}

pub fn usage_comparisons(
    store: &Store,
    mut query: crate::domain::UsageComparisonQuery,
    incarnation: &str,
) -> Result<crate::domain::UsageComparisonPage> {
    use crate::domain::{
        ObservedUsage, ObservedUsageStatus, UsageComparisonGroup, UsageComparisonPage, UsageGroupBy,
    };
    use std::collections::{BTreeMap, HashMap};
    if query.project_id.as_ref().is_some_and(|id| id.len() > 256)
        || query.role.as_ref().is_some_and(|role| role.len() > 256)
        || query
            .workflow
            .as_deref()
            .is_some_and(|family| !["trip", "single_agent", "unknown"].contains(&family))
    {
        bail!("Choose valid project, workflow and role filters.")
    }
    let start = usage_date(query.start_date.as_deref())?;
    let end = usage_date(query.end_date.as_deref())?;
    if start.zip(end).is_some_and(|(start, end)| start > end) {
        bail!("The end date must be on or after the start date.")
    }
    let end_exclusive = end
        .map(|date| {
            date.succ_opt()
                .ok_or_else(|| anyhow!("The end date is outside the supported range."))
        })
        .transpose()?;
    let cursor = query.cursor.take();
    let filters = serde_json::to_value(&query)?;
    let connection = store.lock()?;
    let revision = crate::store::read_state_revision(&connection)?;
    let last = history_read_position(cursor.as_deref(), incarnation, revision, &filters)?;
    if cursor.is_some() && last.len() != 1 {
        bail!("Invalid usage page. Apply the filters again.")
    }
    let mut statement = connection.prepare("SELECT s.id,a.task_id,a.id,g.id,t.project_id,p.display_name,b.family,a.workflow_version,g.role,s.provider,s.created_at,p.internal_purpose IS NULL
      FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
      JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
      LEFT JOIN workflow_run_bindings b ON b.attempt_id=a.id")?;
    let sessions = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                UsageScopeSession {
                    task_id: row.get(1)?,
                    attempt_id: row.get(2)?,
                    generation_id: row.get(3)?,
                    project_id: row.get(4)?,
                    project_name: row.get(5)?,
                    workflow: usage_workflow_family(row.get(6)?, &row.get::<_, String>(7)?),
                    role: row.get(8)?,
                    provider: row.get(9)?,
                    created_at: row.get(10)?,
                    visible: row.get(11)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    let mut selected = HashMap::<String, String>::new();
    let mut buckets = BTreeMap::<String, UsageBucket>::new();
    for (id, session) in &sessions {
        if !session.visible
            || query
                .project_id
                .as_ref()
                .is_some_and(|project| project != &session.project_id)
            || query
                .workflow
                .as_ref()
                .is_some_and(|workflow| workflow != &session.workflow)
            || query
                .role
                .as_ref()
                .is_some_and(|role| role != &session.role)
        {
            continue;
        }
        if start.is_some() || end_exclusive.is_some() {
            let date = chrono::DateTime::parse_from_rfc3339(&session.created_at)
                .map_err(|_| anyhow!("Usage comparisons are unavailable: a saved session has an invalid start date."))?
                .with_timezone(&Utc).date_naive();
            if start.is_some_and(|start| date < start)
                || end_exclusive.is_some_and(|end| date >= end)
            {
                continue;
            }
        }
        let (key, label) = match query.group_by {
            UsageGroupBy::Project => (&session.project_id, &session.project_name),
            UsageGroupBy::Workflow => (&session.workflow, &session.workflow),
            UsageGroupBy::Role => (&session.role, &session.role),
        };
        if key.len() > 256 {
            bail!("Usage comparisons are unavailable: a saved group identifier is too long.")
        }
        selected.insert(id.clone(), key.clone());
        let bucket = buckets.entry(key.clone()).or_insert_with(|| UsageBucket {
            group: UsageComparisonGroup {
                key: key.clone(),
                label: label.chars().take(256).collect(),
                sessions: 0,
                codex_sessions: 0,
                claude_sessions: 0,
                unknown_provider_sessions: 0,
                sessions_with_observations: 0,
                sessions_without_observations: 0,
                usage: ObservedUsage::unavailable(
                    "No response usage has been observed for these sessions.",
                ),
            },
            sum: None,
            observed_sessions: HashSet::new(),
        });
        bucket.group.sessions += 1;
        match session.provider.as_str() {
            "codex" => bucket.group.codex_sessions += 1,
            "claude" => bucket.group.claude_sessions += 1,
            _ => bucket.group.unknown_provider_sessions += 1,
        }
    }
    let mut statement = connection.prepare("SELECT input_tokens,cached_input_tokens,cache_write_input_tokens,output_tokens,
      reasoning_output_tokens,total_tokens,invalid,received_at,source_version,contract_revision,
      task_id,attempt_id,role_generation_id,session_id,conflict_owners_json FROM codex_usage_observations ORDER BY rowid")?;
    let mut rows = statement.query([])?;
    let mut unresolved = false;
    while let Some(row) = rows.next()? {
        let original: String = row.get(13)?;
        let task: String = row.get(10)?;
        let attempt: String = row.get(11)?;
        let generation: String = row.get(12)?;
        let mut owners = HashSet::new();
        match sessions.get(&original) {
            Some(session)
                if session.task_id == task
                    && session.attempt_id == attempt
                    && session.generation_id == generation =>
            {
                owners.insert(original);
            }
            _ => unresolved = true,
        }
        let conflicts = serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(14)?).ok();
        let conflicts = conflicts.as_ref().and_then(|value| value.as_array());
        if let Some(conflicts) = conflicts {
            for owner in conflicts {
                let resolved = owner
                    .get("session_id")
                    .and_then(|id| id.as_str())
                    .and_then(|id| sessions.get(id).map(|session| (id, session)))
                    .filter(|(_, session)| {
                        owner.get("task_id").and_then(|id| id.as_str())
                            == Some(session.task_id.as_str())
                    });
                match resolved {
                    Some((id, _)) => {
                        owners.insert(id.to_owned());
                    }
                    None => unresolved = true,
                }
            }
        } else {
            unresolved = true
        }
        // One observation can poison several buckets but is folded once in each.
        let mut affected = HashSet::new();
        for owner in owners {
            if let Some(key) = selected.get(&owner) {
                if let Some(bucket) = buckets.get_mut(key) {
                    bucket.observed_sessions.insert(owner);
                    affected.insert(key.clone());
                }
            }
        }
        for key in affected {
            if let Some(bucket) = buckets.get_mut(&key) {
                fold_observed_usage(&mut bucket.group.usage, &mut bucket.sum, row)?;
                if conflicts.is_some_and(|owners| !owners.is_empty()) {
                    bucket.group.usage.status = ObservedUsageStatus::Invalid;
                    bucket.group.usage.counters = None;
                    bucket.group.usage.reason =
                        Some("Conflicting response ownership; usage is unavailable.".into());
                }
            }
        }
    }
    // An unresolved owner could belong to any selected bucket. Suppress the
    // entire comparison rather than publish guessed coverage or valid totals.
    if unresolved {
        bail!("Usage comparisons are unavailable: a saved response cannot be linked to its sessions. Inspect the saved usage ownership records before comparing usage.")
    }
    let mut page = UsageComparisonPage {
        groups: vec![],
        next_cursor: None,
    };
    for (key, mut bucket) in buckets {
        if last.first().is_some_and(|last| &key <= last) {
            continue;
        }
        if page.groups.len() == 100 {
            return Ok(page);
        }
        let group = &mut bucket.group;
        group.sessions_with_observations = bucket.observed_sessions.len();
        group.sessions_without_observations = group.sessions - group.sessions_with_observations;
        if group.usage.status == ObservedUsageStatus::Observed {
            group.usage.counters = bucket.sum;
            if group.sessions_without_observations > 0
                || group.claude_sessions > 0
                || group.unknown_provider_sessions > 0
            {
                group.usage.reason = Some("Partial observations: some selected sessions have no supported response records.".into());
            }
        }
        let next = history_cursor(incarnation, revision, &filters, vec![key])?;
        page.groups.push(bucket.group);
        let previous_cursor = page.next_cursor.replace(next);
        if serde_json::to_vec(&page)?.len() > HISTORY_RESPONSE_LIMIT {
            page.groups.pop();
            if page.groups.is_empty() {
                bail!("Usage comparisons are unavailable: a saved group is too large to display.")
            }
            page.next_cursor = previous_cursor;
            return Ok(page);
        }
    }
    page.next_cursor = None;
    Ok(page)
}

pub(crate) fn comparison_observed_usage(
    connection: &Connection,
    executions: &[(String, String)],
) -> Result<crate::domain::ObservedUsage> {
    use crate::domain::{ObservedUsage, ObservedUsageStatus};
    let selected = serde_json::to_string(executions)?;
    let unresolved:bool=connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM codex_usage_observations o
         LEFT JOIN sessions s ON s.id=o.session_id
         LEFT JOIN role_generations g ON g.id=s.role_generation_id
         LEFT JOIN attempts a ON a.id=g.attempt_id
         WHERE a.task_id IS NOT o.task_id OR g.attempt_id IS NOT o.attempt_id
           OR g.id IS NOT o.role_generation_id OR json_type(o.conflict_owners_json)!='array'
           OR EXISTS(SELECT 1 FROM json_each(o.conflict_owners_json) owner
               WHERE NOT EXISTS(SELECT 1 FROM sessions cs JOIN role_generations cg ON cg.id=cs.role_generation_id
                   JOIN attempts ca ON ca.id=cg.attempt_id
                   WHERE cs.id=json_extract(owner.value,'$.session_id')
                     AND ca.task_id=json_extract(owner.value,'$.task_id'))))",
        [],|row|row.get(0),
    )?;
    if unresolved {
        bail!("Comparison usage is unavailable because saved response ownership cannot be resolved. Inspect the saved ownership records.");
    }
    let mut statement = connection.prepare(
        "SELECT input_tokens,cached_input_tokens,cache_write_input_tokens,output_tokens,
          reasoning_output_tokens,total_tokens,(invalid OR json_array_length(conflict_owners_json)>0),received_at,source_version,contract_revision
         FROM codex_usage_observations o
         WHERE EXISTS(SELECT 1 FROM json_each(?1) selected
             WHERE json_extract(selected.value,'$[0]')=o.session_id
               AND json_extract(selected.value,'$[1]')=o.transcript_epoch)
            OR EXISTS(SELECT 1 FROM json_each(o.conflict_owners_json) owner JOIN json_each(?1) selected
               ON json_extract(owner.value,'$.session_id')=json_extract(selected.value,'$[0]')
               WHERE json_extract(owner.value,'$.transcript_epoch') IS NULL
                  OR json_extract(owner.value,'$.transcript_epoch')=json_extract(selected.value,'$[1]'))
         ORDER BY o.rowid",
    )?;
    let mut rows = statement.query([selected])?;
    let mut usage = ObservedUsage::unavailable(
        "No supported response usage was recorded for the matching executions.",
    );
    let mut sum = None;
    while let Some(row) = rows.next()? {
        fold_observed_usage(&mut usage, &mut sum, row)?;
    }
    if usage.status == ObservedUsageStatus::Observed {
        usage.counters = sum;
    }
    Ok(usage)
}
