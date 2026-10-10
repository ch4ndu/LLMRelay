use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

// Source directions survive a retained resume. Native invocation authority is
// checked separately; an epoch or task version must not pin the next turn.
pub(crate) fn bindings(connection: &Connection, attempt: &str) -> Result<Option<Value>> {
    let text: Option<String> = connection.query_row(
        "SELECT json_object('task_id',t.id,'attempt_id',a.id,'source_phase',a.phase,
          'approved_plan_hash',a.plan_hash,'approved_plan_record_id',p.id,
          'candidate_hash',a.candidate_hash,'base_revision',a.base_revision,
          'scope_hash',a.scope_hash,'attempt_configuration_hash',a.configuration_hash,
          'config_revision_id',config.id,'project_configuration_hash',config.configuration_hash,
          'lane_id','default','implementer_role_generation_id',g.id,'session_id',s.id)
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN trip_structured_plans p ON p.id=a.structured_plan_id AND p.attempt_id=a.id AND p.plan_hash=a.plan_hash
         JOIN trip_config_revisions config ON config.id=p.profile_revision_id
         JOIN trip_project_state state ON state.project_id=t.project_id AND state.active_config_revision_id=config.id
         JOIN role_settings settings ON settings.task_id=t.id AND settings.role='implementer'
         JOIN role_generations g ON g.id=settings.effective_generation_id AND g.attempt_id=a.id
           AND g.role='implementer' AND g.lane_id='default' AND g.status NOT IN ('replaced','revoked')
         JOIN sessions s ON s.role_generation_id=g.id AND s.validation_cell IS NULL
         WHERE a.id=?1 AND a.phase='implementation' AND t.lifecycle!='validation'
           AND p.approved_at IS NOT NULL AND p.implementation_authorized_at IS NOT NULL
         ORDER BY s.created_at DESC,s.rowid DESC LIMIT 1",
        params![attempt], |row| row.get(0),
    ).optional()?;
    let Some(text) = text else { return Ok(None) };
    let mut value: Value = serde_json::from_str(&text)?;
    if let Some(review) = latest_repair_review(connection, attempt)? {
        value["review_request_id"] = review["request_id"].clone();
        value["review_report_id"] = review["result_id"].clone();
        value["rejected_candidate_hash"] = review["candidate_hash"].clone();
    }
    Ok(Some(value))
}

fn latest_repair_review(connection: &Connection, attempt: &str) -> Result<Option<Value>> {
    let text: Option<String> = connection
        .query_row(
            "SELECT json_object('request_id',r.id,'candidate_hash',r.candidate_hash,
          'result_id',(SELECT rr.id FROM role_results rr
            WHERE rr.session_id=r.session_id AND rr.role_generation_id=r.role_generation_id
              AND rr.outcome=r.verdict AND rr.consumed_at IS NOT NULL
              AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
              AND json_extract(rr.metadata_json,'$.candidate_hash')=r.candidate_hash
              AND json_extract(rr.metadata_json,'$.review_kind')=r.review_kind
            ORDER BY rr.created_at DESC,rr.rowid DESC LIMIT 1),
          'updated_at',r.updated_at)
         FROM review_requests r WHERE r.attempt_id=?1 AND r.review_kind IN ('code','final')
           AND r.delivery_state='finished' AND r.verdict='request_changes'
         ORDER BY r.updated_at DESC,r.rowid DESC LIMIT 1",
            params![attempt],
            |row| row.get(0),
        )
        .optional()?;
    text.map(|text| serde_json::from_str(&text).map_err(Into::into))
        .transpose()
}

pub(crate) fn validate(connection: &Connection, attempt: &str, metadata: &Value) -> Result<()> {
    let capsule = metadata.get("implementation_capsule").context(
        "repair handoff requires implementation_capsule; copy the current repair_handoff_template from role context",
    )?;
    let supplied = capsule.get("bindings").and_then(Value::as_object).context(
        "repair handoff requires implementation_capsule.bindings (not durable_bindings); copy repair_handoff_template.bindings from current role context",
    )?;
    let expected = bindings(connection, attempt)?.context(
        "repair handoff has no current approved plan and default writer; refresh role context before submitting source directions",
    )?;
    for field in ["task_version", "transcript_epoch", "resume_count"] {
        if supplied.contains_key(field) {
            bail!("repair handoff bindings.{field} is invocation-specific; keep historical observations outside bindings and use current authenticated context on resume")
        }
    }
    for (field, value) in expected
        .as_object()
        .context("repair binding template is not an object")?
    {
        if supplied.get(field) != Some(value) {
            bail!("repair handoff implementation_capsule.bindings.{field} is missing or stale; copy the current repair_handoff_template.bindings before resubmitting")
        }
    }
    for field in ["approved_plan_hash", "candidate_hash"] {
        if metadata.get(field) != expected.get(field) {
            bail!("repair handoff metadata.{field} is missing or stale; refresh role context")
        }
    }
    if expected.get("review_request_id").is_some() && expected["review_report_id"].is_null() {
        bail!("repair handoff is waiting for the exact accepted review result")
    }
    Ok(())
}

pub(crate) struct Feedback {
    pub result: Option<Value>,
    pub blocker: Option<Value>,
}

pub(crate) fn feedback(connection: &Connection, attempt: &str) -> Result<Feedback> {
    // Choose the latest intended handoff before validating it. Filtering invalid
    // bindings in SQL would silently resurrect an older review's directions.
    let text: Option<String> = connection
        .query_row(
            "SELECT json_object('result_id',r.id,'outcome',r.outcome,'summary',r.summary,
          'metadata',json(r.metadata_json),'created_at',r.created_at)
         FROM role_results r JOIN role_generations g ON g.id=r.role_generation_id
         JOIN attempts a ON a.id=g.attempt_id
         JOIN role_settings settings ON settings.task_id=a.task_id AND settings.role='manager'
           AND settings.effective_generation_id=g.id
         WHERE a.id=?1 AND a.phase='implementation' AND g.role='manager' AND r.outcome='needs_input'
           AND (json_type(r.metadata_json,'$.approved_plan_hash') IS NOT NULL
             OR json_type(r.metadata_json,'$.implementation_capsule') IS NOT NULL)
         ORDER BY r.created_at DESC,r.rowid DESC LIMIT 1",
            params![attempt],
            |row| row.get(0),
        )
        .optional()?;
    let Some(text) = text else {
        return Ok(Feedback {
            result: None,
            blocker: None,
        });
    };
    let result: Value = serde_json::from_str(&text)?;
    let metadata = &result["metadata"];
    let (plan, candidate): (Option<String>, Option<String>) = connection.query_row(
        "SELECT plan_hash,candidate_hash FROM attempts WHERE id=?1",
        params![attempt],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if metadata.get("approved_plan_hash") != Some(&json!(plan))
        || metadata.get("candidate_hash") != Some(&json!(candidate))
    {
        return Ok(Feedback {
            result: None,
            blocker: None,
        });
    }
    // A previous repair allocation is superseded by a newer accepted review.
    // Ordinary repairs receive that complete review and the full approved plan.
    if let Some(review) = latest_repair_review(connection, attempt)? {
        if result["created_at"].as_str() < review["updated_at"].as_str() {
            return Ok(Feedback {
                result: None,
                blocker: None,
            });
        }
    }
    if metadata.get("implementation_capsule").is_some() {
        if let Err(error) = validate(connection, attempt, metadata) {
            return Ok(Feedback {
                result: None,
                blocker: Some(json!({
                    "code":"invalid_repair_handoff","result_id":result["result_id"],
                    "reason":format!("{error:#}"),"owner":"manager",
                    "action":"Correct the current handoff using repair_handoff_template; no new plan approval is needed."
                })),
            });
        }
    }
    Ok(Feedback {
        result: Some(result),
        blocker: None,
    })
}
