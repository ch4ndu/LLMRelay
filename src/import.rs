use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const REQUIRED_FIELDS: [&str; 10] = [
    "id",
    "title",
    "status",
    "owner",
    "priority",
    "attention",
    "phase",
    "validation_status",
    "created",
    "updated",
];
const HEADINGS: [&str; 8] = [
    "## Context",
    "## Acceptance Criteria",
    "## Progress",
    "## Validation",
    "### Runs",
    "### Bugs",
    "## Questions / Decisions",
    "## Activity",
];
const RUN_FIELDS: [&str; 7] = [
    "Status",
    "Platform",
    "Command",
    "Started",
    "Completed",
    "Evidence",
    "Summary",
];
const BUG_FIELDS: [&str; 10] = [
    "Status",
    "Severity",
    "Blocking",
    "Expected",
    "Actual",
    "Reproduction",
    "Owner",
    "Resolution",
    "Evidence",
    "Retest",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacyPreview {
    pub source: PathBuf,
    pub source_hash: String,
    pub frontmatter: BTreeMap<String, String>,
    pub sections: BTreeMap<String, String>,
    pub unknown_frontmatter: BTreeMap<String, String>,
    pub validation_runs: Vec<BTreeMap<String, String>>,
    pub validation_bugs: Vec<BTreeMap<String, String>>,
    pub source_text: String,
}

pub fn preview(path: &Path) -> Result<LegacyPreview> {
    let source = path
        .canonicalize()
        .with_context(|| format!("resolve import source {}", path.display()))?;
    if std::fs::metadata(&source)?.len() > 2 * 1024 * 1024 {
        bail!("legacy task source exceeds the 2 MiB preview/import limit")
    }
    let source_text = std::fs::read_to_string(&source)?;
    let normalized = source_text.replace("\r\n", "\n");
    let lines = normalized.lines().collect::<Vec<_>>();
    if lines.first() != Some(&"---") {
        bail!("legacy task must begin with --- frontmatter")
    }
    let boundary = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (*line == "---").then_some(index))
        .ok_or_else(|| anyhow!("legacy frontmatter is not closed"))?;
    let mut frontmatter = BTreeMap::new();
    for line in &lines[1..boundary] {
        if line.trim().is_empty() {
            continue;
        }
        let (key, raw) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("malformed scalar frontmatter line {line:?}"))?;
        let key = key.trim();
        if key.is_empty()
            || !key.chars().enumerate().all(|(index, value)| {
                value.is_ascii_alphabetic()
                    || (index > 0 && (value.is_ascii_digit() || value == '_'))
            })
        {
            bail!("invalid legacy frontmatter key {key:?}")
        }
        let value =
            unquote(raw.trim()).ok_or_else(|| anyhow!("malformed quoted scalar for {key}"))?;
        if frontmatter.insert(key.to_owned(), value).is_some() {
            bail!("duplicate frontmatter key {key}")
        }
    }
    for field in REQUIRED_FIELDS {
        if !frontmatter.contains_key(field) {
            bail!("missing required legacy frontmatter field {field}")
        }
    }
    validate_frontmatter(&frontmatter)?;
    if source.file_stem().and_then(|value| value.to_str()) != Some(frontmatter["id"].as_str()) {
        bail!("legacy filename must match its frontmatter id")
    }

    let body = lines[boundary + 1..]
        .iter()
        .copied()
        .skip_while(|line| line.trim().is_empty())
        .collect::<Vec<_>>();
    if body.first().is_none_or(|line| !line.starts_with("# "))
        || body.iter().filter(|line| line.starts_with("# ")).count() != 1
    {
        bail!("legacy task body must begin with exactly one title heading")
    }
    let mut positions = Vec::new();
    for heading in HEADINGS {
        let found = body
            .iter()
            .enumerate()
            .filter_map(|(index, line)| (*line == heading).then_some(index))
            .collect::<Vec<_>>();
        if found.len() != 1 {
            bail!("required legacy heading {heading:?} must appear exactly once")
        }
        positions.push(found[0]);
    }
    if positions.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("legacy sections are out of order")
    }
    let mut sections = BTreeMap::new();
    for (name, start, end) in [
        ("Context", positions[0], positions[1]),
        ("Acceptance Criteria", positions[1], positions[2]),
        ("Progress", positions[2], positions[3]),
        ("Validation", positions[3], positions[6]),
        ("Questions / Decisions", positions[6], positions[7]),
        ("Activity", positions[7], body.len()),
    ] {
        sections.insert(
            name.to_owned(),
            body[start + 1..end].join("\n").trim().to_owned(),
        );
    }
    let validation_runs = parse_blocks(
        &body[positions[4] + 1..positions[5]],
        "#### Run: ",
        &RUN_FIELDS,
        true,
    )?;
    let validation_bugs = parse_blocks(
        &body[positions[5] + 1..positions[6]],
        "#### Bug: ",
        &BUG_FIELDS,
        false,
    )?;
    let unknown_frontmatter = frontmatter
        .iter()
        .filter(|(key, _)| !REQUIRED_FIELDS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(LegacyPreview {
        source,
        source_hash: hex::encode(Sha256::digest(source_text.as_bytes())),
        frontmatter,
        sections,
        unknown_frontmatter,
        validation_runs,
        validation_bugs,
        source_text,
    })
}

pub fn apply(
    store: &Store,
    operation_id: &str,
    project_id: &str,
    expected_project_version: i64,
    path: &Path,
    expected_source_hash: &str,
) -> Result<serde_json::Value> {
    let preview = preview(path)?;
    if expected_source_hash.trim().is_empty() || preview.source_hash != expected_source_hash {
        bail!("legacy source changed after preview; preview it again before import")
    }
    let visible_source = preview.source.to_string_lossy().into_owned();
    let source_identity = format!("{project_id}::{visible_source}");
    let preview_json = serde_json::to_value(&preview)?;
    let request_hash = crate::store::json_hash(&serde_json::json!({
        "project_id":project_id,"expected_project_version":expected_project_version,
        "source":visible_source,"source_hash":expected_source_hash
    }))?;
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some((stored,result)) = transaction.query_row(
        "SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='legacy_import'",
        params![operation_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)),
    ).optional()? {
        if stored != request_hash { bail!("legacy import operation ID was reused with different input") }
        return Ok(serde_json::from_str(&result)?)
    }
    let current_version: i64 = transaction
        .query_row(
            "SELECT version FROM projects WHERE id=?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown project"))?;
    if current_version != expected_project_version {
        bail!("project version is stale; preview again against current project state")
    }
    if let Some((hash, result)) = transaction
        .query_row(
            "SELECT source_hash,result_json FROM import_records WHERE source_identity=?1",
            params![source_identity],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if hash != preview.source_hash {
            bail!("import source changed after its prior import; preview and choose a new source copy")
        }
        let result: serde_json::Value = serde_json::from_str(&result)?;
        transaction.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','legacy_import',?2,?3,?4)",params![operation_id,request_hash,result.to_string(),Utc::now().to_rfc3339()])?;
        transaction.commit()?;
        return Ok(result);
    }
    let task_id = preview.frontmatter["id"].clone();
    if transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1)",
        params![task_id],
        |row| row.get::<_, bool>(0),
    )? {
        bail!("legacy task id {task_id} already exists")
    }
    let lifecycle = match preview.frontmatter["status"].as_str() {
        "todo" => "backlog",
        "in_progress" | "implemented" => "backlog",
        "verified" => "done",
        _ => unreachable!(),
    };
    let priority = match preview.frontmatter["priority"].as_str() {
        "low" => -1,
        "normal" => 0,
        "high" => 1,
        _ => unreachable!(),
    };
    let source_requires_managed_attempt = matches!(
        preview.frontmatter["status"].as_str(),
        "in_progress" | "implemented"
    );
    let attention = if source_requires_managed_attempt
        || preview.frontmatter["attention"] == "waiting_for_input"
    {
        "needs_input"
    } else {
        "none"
    };
    let criteria = preview.sections["Acceptance Criteria"]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.strip_prefix("- [ ] ")
                .or_else(|| line.strip_prefix("- [x] "))
                .or_else(|| line.strip_prefix("- [X] "))
                .or_else(|| line.strip_prefix("- "))
                .unwrap_or(line)
                .to_owned()
        })
        .collect::<Vec<_>>();
    let legacy = serde_json::json!({
        "source":visible_source,"source_hash":preview.source_hash,"frontmatter":preview.frontmatter,
        "source_status":preview.frontmatter["status"],
        "source_attention":preview.frontmatter["attention"],
        "source_phase":preview.frontmatter["phase"],
        "source_validation_status":preview.frontmatter["validation_status"],
        "unknown_frontmatter":preview.unknown_frontmatter,"sections":preview.sections,
        "validation_runs":preview.validation_runs,"validation_bugs":preview.validation_bugs,"source_text":preview.source_text
    });
    transaction.execute(
        "INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,priority,manual_order,lifecycle,attention,version,created_at,updated_at,role_overrides_json,legacy_json)
         VALUES(?1,?2,?3,?4,?5,?6,0,?7,?8,1,?9,?10,'{}',?11)",
        params![task_id,project_id,preview.frontmatter["title"],preview.sections["Context"],serde_json::to_string(&criteria)?,priority,lifecycle,attention,preview.frontmatter["created"],preview.frontmatter["updated"],legacy.to_string()],
    )?;
    let result = serde_json::json!({"imported":true,"task_id":task_id,"project_id":project_id,"source":visible_source,"source_hash":preview.source_hash});
    transaction.execute("INSERT INTO import_records(source_identity,source_hash,result_json,imported_at,preview_json) VALUES(?1,?2,?3,?4,?5)", params![source_identity,preview.source_hash,result.to_string(),Utc::now().to_rfc3339(),preview_json.to_string()])?;
    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "UPDATE projects SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
        params![now, project_id, expected_project_version],
    )?;
    transaction.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,old_version,new_version,detail_json,created_at) VALUES(?1,?2,'human','legacy.imported','project',?3,?4,?5,?6,?7)",params![uuid::Uuid::new_v4().to_string(),operation_id,project_id,expected_project_version,expected_project_version+1,serde_json::json!({"imported_task_id":task_id,"source":visible_source,"source_hash":preview.source_hash,"task_created_version":1}).to_string(),now])?;
    transaction.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','legacy_import',?2,?3,?4)",params![operation_id,request_hash,result.to_string(),now])?;
    transaction.commit()?;
    Ok(result)
}

fn validate_frontmatter(fields: &BTreeMap<String, String>) -> Result<()> {
    if !lower_kebab(&fields["id"]) || fields["title"].trim().is_empty() {
        bail!("legacy id or title is invalid")
    }
    if !["todo", "in_progress", "implemented", "verified"].contains(&fields["status"].as_str()) {
        bail!("unsupported legacy status {}", fields["status"])
    }
    if !["low", "normal", "high"].contains(&fields["priority"].as_str()) {
        bail!("unsupported legacy priority {}", fields["priority"])
    }
    if !["none", "waiting_for_input"].contains(&fields["attention"].as_str()) {
        bail!("unsupported legacy attention {}", fields["attention"])
    }
    if ![
        "backlog",
        "planning",
        "plan_review",
        "implementation",
        "review_readiness",
        "code_review",
        "validation",
        "validation_remediation",
        "final_verification",
        "release",
    ]
    .contains(&fields["phase"].as_str())
    {
        bail!("unsupported legacy phase {}", fields["phase"])
    }
    if fields["status"] == "todo" && fields["phase"] != "backlog" {
        bail!("todo legacy tasks must use backlog phase")
    }
    if !["not_started", "in_progress", "failed", "passed", "blocked"]
        .contains(&fields["validation_status"].as_str())
    {
        bail!(
            "unsupported legacy validation status {}",
            fields["validation_status"]
        )
    }
    for key in ["created", "updated"] {
        if !fields[key].ends_with('Z') {
            bail!("legacy {key} must use an RFC3339 UTC timestamp ending in Z")
        }
        DateTime::parse_from_rfc3339(&fields[key])
            .with_context(|| format!("legacy {key} is not RFC3339 UTC"))?;
    }
    Ok(())
}

fn parse_blocks(
    lines: &[&str],
    prefix: &str,
    required: &[&str],
    run: bool,
) -> Result<Vec<BTreeMap<String, String>>> {
    let starts = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("#### ").then_some(index))
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    for (position, start) in starts.iter().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(lines.len());
        let heading = lines[*start];
        if !heading.starts_with(prefix) {
            continue;
        }
        let identity = heading.strip_prefix(prefix).unwrap().trim();
        let (id, title) = if run {
            (identity, None)
        } else {
            identity
                .split_once(" — ")
                .map(|(id, title)| (id.trim(), Some(title.trim())))
                .ok_or_else(|| anyhow!("legacy bug heading requires id and title"))?
        };
        if !lower_kebab(id) || title.is_some_and(str::is_empty) {
            bail!("invalid legacy validation block identity")
        }
        let mut fields = BTreeMap::new();
        fields.insert("id".to_owned(), id.to_owned());
        if let Some(title) = title {
            fields.insert("title".to_owned(), title.to_owned());
        }
        for line in lines[*start + 1..end]
            .iter()
            .copied()
            .skip_while(|line| line.trim().is_empty())
            .take_while(|line| !line.trim().is_empty())
        {
            if let Some((key, value)) = line.split_once(':') {
                fields.insert(key.trim().to_owned(), value.trim().to_owned());
            }
        }
        for field in required {
            if !fields.contains_key(*field) {
                bail!("legacy validation block {id} is missing {field}")
            }
        }
        if run
            && !["pending", "running", "failed", "passed", "cancelled"]
                .contains(&fields["Status"].to_ascii_lowercase().as_str())
        {
            bail!("legacy run {id} has invalid status")
        }
        if !run {
            if ![
                "open",
                "in_progress",
                "fixed",
                "retest",
                "verified",
                "wont_fix",
                "reopened",
            ]
            .contains(&fields["Status"].to_ascii_lowercase().as_str())
                || !["low", "normal", "high", "critical"]
                    .contains(&fields["Severity"].to_ascii_lowercase().as_str())
                || !["true", "false"].contains(&fields["Blocking"].to_ascii_lowercase().as_str())
            {
                bail!("legacy bug {id} has invalid metadata")
            }
        }
        values.push(fields);
    }
    Ok(values)
}

fn unquote(value: &str) -> Option<String> {
    if value.starts_with('\'') {
        (value.len() >= 2 && value.ends_with('\''))
            .then(|| value[1..value.len() - 1].replace("''", "'"))
    } else if value.starts_with('"') {
        (value.len() >= 2 && value.ends_with('"')).then(|| value[1..value.len() - 1].to_owned())
    } else {
        Some(value.to_owned())
    }
}

fn lower_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}
