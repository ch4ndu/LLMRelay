use crate::domain::{HumanCommand, OperationResult, RoleOverride};
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[cfg(test)]
thread_local! {
    static TEST_BEFORE_FIRE: std::cell::RefCell<Option<Box<dyn FnMut(usize)>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_test_before_fire(hook: Option<Box<dyn FnMut(usize)>>) {
    TEST_BEFORE_FIRE.with(|slot| *slot.borrow_mut() = hook);
}

const ROLES: [&str; 6] = [
    "manager",
    "explorer",
    "plan_reviewer",
    "implementer",
    "code_reviewer",
    "final_verifier",
];
const STALE_REMEDY: &str = "Recipe pins are stale. Re-save the profile set and recipe under the active configuration, archive this draft, then create a draft from the new revision.";

fn outcome(
    operation_id: &str,
    kind: &str,
    id: String,
    version: i64,
    state: &str,
) -> OperationResult {
    OperationResult {
        operation_id: operation_id.into(),
        entity_kind: kind.into(),
        entity_id: id,
        version: Some(version),
        state: state.into(),
        detail: Value::Null,
    }
}

fn bounded(value: &str, field: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > max {
        bail!("{field} must contain 1 to {max} characters")
    }
    Ok(())
}

fn visible_project(connection: &Connection, project_id: &str) -> Result<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND internal_purpose IS NULL)",
        [project_id],
        |row| row.get(0),
    )?;
    if !exists {
        bail!("project does not exist or is reserved for internal setup")
    }
    Ok(())
}

fn active_pin(connection: &Connection, project_id: &str) -> Result<(String, String)> {
    visible_project(connection, project_id)?;
    crate::trip::require_project_ready(connection, project_id)?;
    connection
        .query_row(
            "SELECT r.id,r.configuration_hash FROM trip_project_state s
         JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
         WHERE s.project_id=?1 AND r.project_id=?1 AND r.state='activated'",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("project has no active configuration"))
}

fn validate_roles(roles: &Value) -> Result<()> {
    let object = roles
        .as_object()
        .ok_or_else(|| anyhow!("roles must be an object"))?;
    if object.len() != ROLES.len() || ROLES.iter().any(|role| !object.contains_key(*role)) {
        bail!("profile set needs exactly the six app roles")
    }
    for role in ROLES {
        let config: RoleOverride = serde_json::from_value(object[role].clone())?;
        if config.model.trim().is_empty()
            || config.effort.trim().is_empty()
            || config.model.len() > 200
            || config.effort.len() > 100
        {
            bail!("role {role} needs a nonempty model and effort")
        }
    }
    Ok(())
}

fn validate_checks(
    connection: &Connection,
    project_id: &str,
    config_id: &str,
    ids: &[String],
) -> Result<()> {
    if ids.len() > 32 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        bail!("required check IDs must be unique and limited to 32")
    }
    for id in ids {
        let eligible: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM trip_verification_checks
             WHERE id=?1 AND project_id=?2 AND config_revision_id=?3 AND enabled=1)",
            params![id, project_id, config_id],
            |row| row.get(0),
        )?;
        if !eligible {
            bail!("required check {id} is not enabled in the active matrix")
        }
    }
    Ok(())
}

fn current_version(
    connection: &Connection,
    table: &str,
    id: &str,
    project_id: &str,
    expected: i64,
) -> Result<i64> {
    let sql = format!("SELECT current_revision FROM {table} WHERE id=?1 AND project_id=?2 AND version=?3 AND archived_at IS NULL");
    connection
        .query_row(&sql, params![id, project_id, expected], |row| row.get(0))
        .optional()?
        .ok_or_else(|| anyhow!("{table} version is stale or archived"))
}

pub(crate) fn apply_command(
    tx: &Transaction<'_>,
    command: &HumanCommand,
    now: &str,
) -> Result<OperationResult> {
    let operation = command.operation_id();
    match command {
        HumanCommand::UpsertProfileSet {
            project_id,
            profile_set_id,
            expected_version,
            name,
            roles,
            ..
        } => {
            let (config_id, hash) = active_pin(tx, project_id)?;
            bounded(name, "profile set name", 100)?;
            validate_roles(roles)?;
            let (id, revision, version) = match (profile_set_id, expected_version) {
                (None, None) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute("INSERT INTO project_profile_sets(id,project_id,name,current_revision,version,created_at,updated_at) VALUES(?1,?2,?3,1,1,?4,?4)", params![id, project_id, name.trim(), now])?;
                    (id, 1, 1)
                }
                (Some(id), Some(expected)) => {
                    let revision =
                        current_version(tx, "project_profile_sets", id, project_id, *expected)? + 1;
                    tx.execute("UPDATE project_profile_sets SET name=?1,current_revision=?2,version=version+1,updated_at=?3 WHERE id=?4", params![name.trim(), revision, now, id])?;
                    (id.clone(), revision, expected + 1)
                }
                _ => bail!(
                    "profile creation and update require matching identity and expected version"
                ),
            };
            let revision_id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO project_profile_set_revisions(id,profile_set_id,revision,roles_json,config_revision_id,configuration_hash,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![revision_id,id,revision,serde_json::to_string(roles)?,config_id,hash,now])?;
            let mut result = outcome(operation, "profile_set", id, version, "saved");
            result.detail = json!({"revision_id": revision_id, "revision": revision});
            Ok(result)
        }
        HumanCommand::ArchiveProfileSet {
            project_id,
            profile_set_id,
            expected_version,
            ..
        } => {
            visible_project(tx, project_id)?;
            current_version(
                tx,
                "project_profile_sets",
                profile_set_id,
                project_id,
                *expected_version,
            )?;
            let active_refs: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_recipes r JOIN task_recipe_revisions rr ON rr.recipe_id=r.id AND rr.revision=r.current_revision JOIN project_profile_set_revisions pr ON pr.id=rr.profile_revision_id WHERE pr.profile_set_id=?1 AND r.archived_at IS NULL)
                  OR EXISTS(SELECT 1 FROM recipe_schedules s JOIN task_recipe_revisions rr ON rr.id=s.recipe_revision_id JOIN project_profile_set_revisions pr ON pr.id=rr.profile_revision_id WHERE pr.profile_set_id=?1 AND s.archived_at IS NULL)",
                [profile_set_id], |row| row.get(0),
            )?;
            if active_refs {
                bail!("archive or edit active recipes and schedules before archiving this profile set")
            }
            tx.execute("UPDATE project_profile_sets SET archived_at=?1,version=version+1,updated_at=?1 WHERE id=?2", params![now, profile_set_id])?;
            Ok(outcome(
                operation,
                "profile_set",
                profile_set_id.clone(),
                expected_version + 1,
                "archived",
            ))
        }
        HumanCommand::UpsertTaskRecipe {
            project_id,
            recipe_id,
            expected_version,
            name,
            title,
            description,
            acceptance_criteria,
            priority,
            profile_revision_id,
            required_check_ids,
            ..
        } => {
            let (config_id, hash) = active_pin(tx, project_id)?;
            bounded(name, "recipe name", 100)?;
            bounded(title, "task title", 200)?;
            if description.len() > 16_384
                || acceptance_criteria.len() > 32
                || acceptance_criteria
                    .iter()
                    .any(|item| item.trim().is_empty() || item.len() > 2_000)
            {
                bail!("recipe task content exceeds limits or has blank acceptance criteria")
            }
            let profile_pin: Option<(String, String)> = tx.query_row(
                "SELECT pr.config_revision_id,pr.configuration_hash FROM project_profile_set_revisions pr JOIN project_profile_sets p ON p.id=pr.profile_set_id WHERE pr.id=?1 AND p.project_id=?2 AND p.archived_at IS NULL",
                params![profile_revision_id, project_id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if profile_pin.as_ref() != Some(&(config_id.clone(), hash.clone())) {
                bail!(
                    "profile revision is foreign, archived, or pinned to an earlier configuration"
                )
            }
            validate_checks(tx, project_id, &config_id, required_check_ids)?;
            let (id, revision, version) = match (recipe_id, expected_version) {
                (None, None) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute("INSERT INTO task_recipes(id,project_id,name,current_revision,version,created_at,updated_at) VALUES(?1,?2,?3,1,1,?4,?4)", params![id, project_id, name.trim(), now])?;
                    (id, 1, 1)
                }
                (Some(id), Some(expected)) => {
                    let revision =
                        current_version(tx, "task_recipes", id, project_id, *expected)? + 1;
                    tx.execute("UPDATE task_recipes SET name=?1,current_revision=?2,version=version+1,updated_at=?3 WHERE id=?4", params![name.trim(), revision, now, id])?;
                    (id.clone(), revision, expected + 1)
                }
                _ => bail!(
                    "recipe creation and update require matching identity and expected version"
                ),
            };
            let revision_id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO task_recipe_revisions(id,recipe_id,revision,title,description,acceptance_criteria_json,priority,profile_revision_id,required_check_ids_json,config_revision_id,configuration_hash,workflow_version,workflow_hash,created_at)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![revision_id,id,revision,title.trim(),description,serde_json::to_string(acceptance_criteria)?,priority,profile_revision_id,serde_json::to_string(required_check_ids)?,config_id,hash,crate::workflow_resources::WORKFLOW_VERSION,crate::workflow_resources::workflow_hash(),now])?;
            let mut result = outcome(operation, "recipe", id, version, "saved");
            result.detail = json!({"revision_id": revision_id, "revision": revision});
            Ok(result)
        }
        HumanCommand::ArchiveTaskRecipe {
            project_id,
            recipe_id,
            expected_version,
            ..
        } => {
            visible_project(tx, project_id)?;
            current_version(tx, "task_recipes", recipe_id, project_id, *expected_version)?;
            tx.execute("UPDATE task_recipes SET archived_at=?1,version=version+1,updated_at=?1 WHERE id=?2", params![now, recipe_id])?;
            tx.execute("UPDATE recipe_schedules SET paused=1,next_fire_utc=NULL,version=version+1,updated_at=?1 WHERE archived_at IS NULL AND paused=0 AND recipe_revision_id IN (SELECT id FROM task_recipe_revisions WHERE recipe_id=?2)", params![now, recipe_id])?;
            Ok(outcome(
                operation,
                "recipe",
                recipe_id.clone(),
                expected_version + 1,
                "archived",
            ))
        }
        HumanCommand::CreateDraftFromRecipe {
            project_id,
            recipe_id,
            recipe_revision_id,
            expected_recipe_version,
            ..
        } => {
            current_version(
                tx,
                "task_recipes",
                recipe_id,
                project_id,
                *expected_recipe_version,
            )?;
            let belongs: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_recipe_revisions WHERE id=?1 AND recipe_id=?2)",
                params![recipe_revision_id, recipe_id],
                |row| row.get(0),
            )?;
            if !belongs {
                bail!("exact recipe revision does not belong to the requested recipe")
            }
            let task_id = materialize(tx, project_id, recipe_revision_id, None, now)?;
            Ok(outcome(operation, "task", task_id, 1, "backlog"))
        }
        HumanCommand::UpsertRecipeSchedule {
            project_id,
            schedule_id,
            expected_version,
            name,
            recipe_revision_id,
            cadence,
            anchor_utc,
            ..
        } => {
            visible_project(tx, project_id)?;
            bounded(name, "schedule name", 100)?;
            let anchor = parse_utc(anchor_utc)?;
            period(cadence)?;
            require_recipe_revision(tx, project_id, recipe_revision_id)?;
            if let Some(reason) = fire_ineligible(tx, project_id, recipe_revision_id)? {
                bail!("recipe revision cannot be scheduled under current pins: {reason}")
            }
            let (id, version, paused, next_fire) = match (schedule_id, expected_version) {
                (None, None) => (uuid::Uuid::new_v4().to_string(), 1, true, None),
                (Some(id), Some(expected)) => {
                    let stored: Option<(bool, Option<String>)> = tx.query_row("SELECT paused,next_fire_utc FROM recipe_schedules WHERE id=?1 AND project_id=?2 AND version=?3 AND archived_at IS NULL", params![id,project_id,expected], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
                    let (paused, persisted_next) =
                        stored.ok_or_else(|| anyhow!("schedule version is stale or archived"))?;
                    let next = if paused {
                        None
                    } else {
                        let command_time = DateTime::parse_from_rfc3339(now)?.with_timezone(&Utc);
                        let latest = latest_fire(tx, id)?;
                        let lower = latest.map_or(command_time, |fire| fire.max(command_time));
                        let persisted = persisted_next
                            .as_deref()
                            .ok_or_else(|| anyhow!("enabled schedule has no next fire"))
                            .and_then(parse_utc)?;
                        let at_or_after = persisted
                            .checked_sub_signed(Duration::seconds(1))
                            .ok_or_else(|| anyhow!("schedule time overflow"))?;
                        Some(next_after(anchor, cadence, lower.max(at_or_after))?)
                    };
                    (id.clone(), expected + 1, paused, next)
                }
                _ => bail!(
                    "schedule creation and update require matching identity and expected version"
                ),
            };
            if schedule_id.is_none() {
                tx.execute("INSERT INTO recipe_schedules(id,project_id,name,recipe_revision_id,cadence,anchor_utc,next_fire_utc,paused,version,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,1,1,?7,?7)", params![id,project_id,name.trim(),recipe_revision_id,cadence,anchor_utc,now])?;
            } else {
                tx.execute("UPDATE recipe_schedules SET name=?1,recipe_revision_id=?2,cadence=?3,anchor_utc=?4,next_fire_utc=?5,paused=?6,version=version+1,updated_at=?7 WHERE id=?8", params![name.trim(),recipe_revision_id,cadence,anchor_utc,next_fire,paused,now,id])?;
            }
            Ok(outcome(
                operation,
                "recipe_schedule",
                id,
                version,
                if paused { "paused" } else { "enabled" },
            ))
        }
        HumanCommand::PauseRecipeSchedule {
            project_id,
            schedule_id,
            expected_version,
            ..
        }
        | HumanCommand::ArchiveRecipeSchedule {
            project_id,
            schedule_id,
            expected_version,
            ..
        } => {
            visible_project(tx, project_id)?;
            let archive = matches!(command, HumanCommand::ArchiveRecipeSchedule { .. });
            let changed = tx.execute(if archive {
                "UPDATE recipe_schedules SET paused=1,next_fire_utc=NULL,archived_at=?1,version=version+1,updated_at=?1 WHERE id=?2 AND project_id=?3 AND version=?4 AND archived_at IS NULL"
            } else {
                "UPDATE recipe_schedules SET paused=1,next_fire_utc=NULL,version=version+1,updated_at=?1 WHERE id=?2 AND project_id=?3 AND version=?4 AND archived_at IS NULL AND paused=0"
            }, params![now,schedule_id,project_id,expected_version])?;
            if changed != 1 {
                bail!("schedule version is stale or already paused or archived")
            }
            Ok(outcome(
                operation,
                "recipe_schedule",
                schedule_id.clone(),
                expected_version + 1,
                if archive { "archived" } else { "paused" },
            ))
        }
        HumanCommand::ResumeRecipeSchedule {
            project_id,
            schedule_id,
            expected_version,
            ..
        } => {
            visible_project(tx, project_id)?;
            let (anchor, cadence, revision_id): (String,String,String) = tx.query_row("SELECT anchor_utc,cadence,recipe_revision_id FROM recipe_schedules WHERE id=?1 AND project_id=?2 AND version=?3 AND archived_at IS NULL AND paused=1", params![schedule_id,project_id,expected_version], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?.ok_or_else(||anyhow!("schedule version is stale or not paused"))?;
            require_recipe_revision(tx, project_id, &revision_id)?;
            if let Some(reason) = fire_ineligible(tx, project_id, &revision_id)? {
                bail!("recipe revision cannot be enabled under current pins: {reason}")
            }
            let command_time = DateTime::parse_from_rfc3339(now)?.with_timezone(&Utc);
            let lower =
                latest_fire(tx, schedule_id)?.map_or(command_time, |fire| fire.max(command_time));
            let next = next_after(parse_utc(&anchor)?, &cadence, lower)?;
            tx.execute("UPDATE recipe_schedules SET paused=0,next_fire_utc=?1,version=version+1,updated_at=?2 WHERE id=?3", params![next,now,schedule_id])?;
            Ok(outcome(
                operation,
                "recipe_schedule",
                schedule_id.clone(),
                expected_version + 1,
                "enabled",
            ))
        }
        _ => bail!("command is not a recipe command"),
    }
}

fn require_recipe_revision(
    connection: &Connection,
    project_id: &str,
    revision_id: &str,
) -> Result<()> {
    let eligible: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id WHERE rr.id=?1 AND r.project_id=?2 AND r.archived_at IS NULL)", params![revision_id,project_id], |row| row.get(0))?;
    if !eligible {
        bail!("recipe revision is foreign, missing, or archived")
    }
    Ok(())
}

pub(crate) fn require_binding_current(connection: &Connection, task_id: &str) -> Result<()> {
    let binding: Option<(String,String,String,String,String,String,String,String)> = connection.query_row(
        "SELECT b.project_id,b.recipe_revision_id,b.profile_revision_id,b.required_check_ids_json,b.config_revision_id,b.configuration_hash,b.workflow_version,b.workflow_hash FROM task_recipe_bindings b WHERE b.task_id=?1",
        [task_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
    ).optional()?;
    let Some((
        project_id,
        recipe_revision_id,
        profile_revision_id,
        checks,
        config_id,
        hash,
        workflow_version,
        workflow_hash,
    )) = binding
    else {
        return Ok(());
    };
    let current = active_pin(connection, &project_id).map_err(|_| anyhow!(STALE_REMEDY))?;
    if current != (config_id.clone(), hash.clone())
        || workflow_version != crate::workflow_resources::WORKFLOW_VERSION
        || workflow_hash != crate::workflow_resources::workflow_hash()
    {
        bail!("{STALE_REMEDY}")
    }
    let links: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id JOIN project_profile_set_revisions pr ON pr.id=rr.profile_revision_id JOIN project_profile_sets p ON p.id=pr.profile_set_id WHERE rr.id=?1 AND pr.id=?2 AND r.project_id=?3 AND p.project_id=?3 AND rr.config_revision_id=?4 AND rr.configuration_hash=?5 AND pr.config_revision_id=?4 AND pr.configuration_hash=?5)",
        params![recipe_revision_id,profile_revision_id,project_id,config_id,hash], |row| row.get(0),
    )?;
    if !links {
        bail!("{STALE_REMEDY}")
    }
    let checks: Vec<String> = serde_json::from_str(&checks)?;
    validate_checks(connection, &project_id, &config_id, &checks).map_err(|_| anyhow!(STALE_REMEDY))
}

fn materialize(
    tx: &Transaction<'_>,
    project_id: &str,
    revision_id: &str,
    schedule: Option<(&str, &str)>,
    now: &str,
) -> Result<String> {
    let (config_id, hash) = active_pin(tx, project_id)?;
    let row: Option<(String,String,String,i64,String,String,String,String,String,String,String)> = tx.query_row(
        "SELECT rr.title,rr.description,rr.acceptance_criteria_json,rr.priority,rr.profile_revision_id,rr.required_check_ids_json,rr.config_revision_id,rr.configuration_hash,rr.workflow_version,rr.workflow_hash,pr.roles_json
         FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id
         JOIN project_profile_set_revisions pr ON pr.id=rr.profile_revision_id
         JOIN project_profile_sets p ON p.id=pr.profile_set_id
         WHERE rr.id=?1 AND r.project_id=?2 AND p.project_id=?2 AND r.archived_at IS NULL AND p.archived_at IS NULL",
        params![revision_id,project_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?)),
    ).optional()?;
    let Some((
        title,
        description,
        criteria,
        priority,
        profile_id,
        checks,
        pin_id,
        pin_hash,
        workflow_version,
        workflow_hash,
        roles,
    )) = row
    else {
        bail!("recipe or profile revision is unavailable")
    };
    let profile_pin: (String,String) = tx.query_row("SELECT config_revision_id,configuration_hash FROM project_profile_set_revisions WHERE id=?1",[&profile_id],|row|Ok((row.get(0)?,row.get(1)?)))?;
    if (pin_id.clone(), pin_hash.clone()) != (config_id.clone(), hash.clone())
        || profile_pin != (config_id.clone(), hash.clone())
        || workflow_version != crate::workflow_resources::WORKFLOW_VERSION
        || workflow_hash != crate::workflow_resources::workflow_hash()
    {
        bail!("{STALE_REMEDY}")
    }
    let check_ids: Vec<String> = serde_json::from_str(&checks)?;
    validate_checks(tx, project_id, &config_id, &check_ids)?;
    let roles_value: Value = serde_json::from_str(&roles)?;
    validate_roles(&roles_value)?;
    let criteria: Vec<String> = serde_json::from_str(&criteria)?;
    let task_id = crate::workflow::insert_task_rows(
        tx,
        project_id,
        &title,
        &description,
        &criteria,
        priority,
        &roles_value,
        None,
        false,
        now,
    )?;
    let (schedule_id, scheduled_for) = schedule
        .map(|(id, time)| (Some(id), Some(time)))
        .unwrap_or((None, None));
    tx.execute("INSERT INTO task_recipe_bindings(task_id,project_id,recipe_revision_id,profile_revision_id,required_check_ids_json,config_revision_id,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![task_id,project_id,revision_id,profile_id,checks,config_id,hash,workflow_version,workflow_hash,schedule_id,scheduled_for,now])?;
    Ok(task_id)
}

fn parse_utc(input: &str) -> Result<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(input)
        .map_err(|_| anyhow!("UTC time must be a valid whole-second RFC3339 instant"))?
        .with_timezone(&Utc);
    if parsed.to_rfc3339_opts(SecondsFormat::Secs, true) != input {
        bail!("UTC time must use canonical YYYY-MM-DDTHH:MM:SSZ form")
    }
    Ok(parsed)
}

fn period(cadence: &str) -> Result<Duration> {
    match cadence {
        "daily" => Ok(Duration::days(1)),
        "weekly" => Ok(Duration::weeks(1)),
        _ => bail!("cadence must be daily or weekly"),
    }
}

fn next_after(anchor: DateTime<Utc>, cadence: &str, now: DateTime<Utc>) -> Result<String> {
    let step = period(cadence)?.num_seconds();
    let delta = now.signed_duration_since(anchor).num_seconds();
    let intervals = if now < anchor {
        0
    } else {
        delta
            .div_euclid(step)
            .checked_add(1)
            .ok_or_else(|| anyhow!("schedule time overflow"))?
    };
    let seconds = intervals
        .checked_mul(step)
        .ok_or_else(|| anyhow!("schedule time overflow"))?;
    let next = anchor
        .checked_add_signed(Duration::seconds(seconds))
        .ok_or_else(|| anyhow!("schedule time overflow"))?;
    Ok(next.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn latest_fire(connection: &Connection, schedule_id: &str) -> Result<Option<DateTime<Utc>>> {
    connection
        .query_row(
            "SELECT MAX(scheduled_for_utc) FROM recipe_schedule_fires WHERE schedule_id=?1",
            [schedule_id],
            |row| row.get::<_, Option<String>>(0),
        )?
        .as_deref()
        .map(parse_utc)
        .transpose()
}

fn at_offset(first: DateTime<Utc>, step_seconds: i64, intervals: i64) -> Result<DateTime<Utc>> {
    let seconds = step_seconds
        .checked_mul(intervals)
        .ok_or_else(|| anyhow!("schedule time overflow"))?;
    first
        .checked_add_signed(Duration::seconds(seconds))
        .ok_or_else(|| anyhow!("schedule time overflow"))
}

fn expected_ineligibility(
    error: anyhow::Error,
    reason: &'static str,
) -> Result<Option<&'static str>> {
    if error.chain().any(|cause| cause.is::<rusqlite::Error>()) {
        Err(error)
    } else {
        Ok(Some(reason))
    }
}

fn fire_ineligible(
    connection: &Connection,
    project_id: &str,
    revision_id: &str,
) -> Result<Option<&'static str>> {
    if let Err(error) = visible_project(connection, project_id) {
        return expected_ineligibility(error, "project_unavailable");
    }
    let current = match active_pin(connection, project_id) {
        Ok(pin) => pin,
        Err(error) => return expected_ineligibility(error, "project_configuration_inactive"),
    };
    let pin: Option<(String,String,String,String,String,String,String,String)> = connection.query_row(
        "SELECT rr.config_revision_id,rr.configuration_hash,rr.workflow_version,rr.workflow_hash,pr.config_revision_id,pr.configuration_hash,rr.required_check_ids_json,pr.roles_json
         FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id
         JOIN project_profile_set_revisions pr ON pr.id=rr.profile_revision_id
         JOIN project_profile_sets p ON p.id=pr.profile_set_id
         WHERE rr.id=?1 AND r.project_id=?2 AND p.project_id=?2 AND r.archived_at IS NULL AND p.archived_at IS NULL",
        params![revision_id,project_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
    ).optional()?;
    let Some((
        recipe_config,
        recipe_hash,
        version,
        workflow_hash,
        profile_config,
        profile_hash,
        checks,
        roles,
    )) = pin
    else {
        return Ok(Some("recipe_or_profile_unavailable"));
    };
    if current != (recipe_config.clone(), recipe_hash.clone())
        || current != (profile_config, profile_hash)
        || version != crate::workflow_resources::WORKFLOW_VERSION
        || workflow_hash != crate::workflow_resources::workflow_hash()
    {
        return Ok(Some("configuration_or_workflow_stale"));
    }
    let check_ids: Vec<String> = serde_json::from_str(&checks)?;
    if let Err(error) = validate_checks(connection, project_id, &recipe_config, &check_ids) {
        return expected_ineligibility(error, "required_check_unavailable");
    }
    let roles: Value = serde_json::from_str(&roles)?;
    if validate_roles(&roles).is_err() {
        return Ok(Some("profile_invalid"));
    }
    Ok(None)
}

pub fn scheduled_intake_tick(
    store: &Store,
    now: DateTime<Utc>,
    started_at: DateTime<Utc>,
) -> Result<usize> {
    scheduled_intake_tick_inner(store, now, started_at, None)
}

pub(crate) fn scheduled_intake_tick_gated(
    store: &Store,
    now: DateTime<Utc>,
    started_at: DateTime<Utc>,
    intake_lock: &Mutex<()>,
    dispatch_enabled: &AtomicBool,
    draining: &AtomicBool,
) -> Result<usize> {
    scheduled_intake_tick_inner(
        store,
        now,
        started_at,
        Some((intake_lock, dispatch_enabled, draining)),
    )
}

fn scheduled_intake_tick_inner(
    store: &Store,
    now: DateTime<Utc>,
    started_at: DateTime<Utc>,
    gate: Option<(&Mutex<()>, &AtomicBool, &AtomicBool)>,
) -> Result<usize> {
    store.require_execution_unheld("scheduled intake")?;
    let now_text = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let schedule_ids = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT s.id FROM recipe_schedules s JOIN projects p ON p.id=s.project_id
             WHERE p.internal_purpose IS NULL AND s.archived_at IS NULL AND s.paused=0
               AND s.next_fire_utc IS NOT NULL AND s.next_fire_utc<=?1
             ORDER BY s.next_fire_utc,s.id LIMIT 32",
        )?;
        let ids = statement
            .query_map([&now_text], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    let mut processed = 0;
    for (_index, schedule_id) in schedule_ids.into_iter().enumerate() {
        #[cfg(test)]
        TEST_BEFORE_FIRE.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(_index);
            }
        });
        let _intake = gate
            .map(|(lock, _, _)| lock.lock().map_err(|_| anyhow!("intake mutex is poisoned")))
            .transpose()?;
        if let Some((_, enabled, draining)) = gate {
            if !enabled.load(Ordering::SeqCst) || draining.load(Ordering::SeqCst) {
                break;
            }
        }
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // The hold and fire now share SQLite's writer order, including holds committed after discovery.
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id='database-restore-hold' AND state='attention_required')",
            [],
            |row| row.get(0),
        )?;
        if held {
            break;
        }
        let schedule: Option<(String,String,String,String)> = tx.query_row(
            "SELECT s.project_id,s.recipe_revision_id,s.cadence,s.next_fire_utc FROM recipe_schedules s JOIN projects p ON p.id=s.project_id
             WHERE s.id=?1 AND p.internal_purpose IS NULL AND s.paused=0 AND s.archived_at IS NULL AND s.next_fire_utc<=?2",
            params![schedule_id,now_text],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional()?;
        let Some((project_id, revision_id, cadence, next_text)) = schedule else {
            continue;
        };
        let first = parse_utc(&next_text)?;
        let step = period(&cadence)?.num_seconds();
        let due_count = now
            .signed_duration_since(first)
            .num_seconds()
            .div_euclid(step)
            .checked_add(1)
            .ok_or_else(|| anyhow!("schedule occurrence overflow"))?;
        let newest = at_offset(first, step, due_count - 1)?;
        let future = at_offset(first, step, due_count)?.to_rfc3339_opts(SecondsFormat::Secs, true);
        let eligible = newest >= started_at;
        let missed_count = if eligible { due_count - 1 } else { due_count };
        let missed_last = if missed_count > 0 {
            Some(
                at_offset(first, step, missed_count - 1)?
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            )
        } else {
            None
        };
        let fire_time = newest.to_rfc3339_opts(SecondsFormat::Secs, true);
        let missed_first = (missed_count > 0).then_some(next_text.as_str());
        let reason = if eligible {
            fire_ineligible(&tx, &project_id, &revision_id)?
        } else {
            Some("before_service_start")
        };
        let provisional = tx.execute(
            "INSERT INTO recipe_schedule_fires(schedule_id,scheduled_for_utc,recipe_revision_id,outcome,reason,missed_first_utc,missed_last_utc,missed_count,created_at)
             VALUES(?1,?2,?3,'missed',?4,?5,?6,?7,?8) ON CONFLICT(schedule_id,scheduled_for_utc) DO NOTHING",
            params![schedule_id,fire_time,revision_id,reason,missed_first,missed_last,missed_count,now_text],
        )?;
        if provisional == 1 {
            let (outcome, task_id) = if !eligible {
                ("missed", None)
            } else if reason.is_some() {
                ("skipped_ineligible", None)
            } else {
                (
                    "task_created",
                    Some(materialize(
                        &tx,
                        &project_id,
                        &revision_id,
                        Some((&schedule_id, &fire_time)),
                        &now_text,
                    )?),
                )
            };
            tx.execute("UPDATE recipe_schedule_fires SET outcome=?1,task_id=?2 WHERE schedule_id=?3 AND scheduled_for_utc=?4",params![outcome,task_id,schedule_id,fire_time])?;
            let operation_id = format!("recipe-fire:{schedule_id}:{fire_time}");
            tx.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                VALUES(?1,?2,'service','recipe.schedule.fire','recipe_schedule',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),operation_id,schedule_id,json!({"outcome":outcome,"task_id":task_id,"scheduled_for_utc":fire_time,"missed_count":missed_count}).to_string(),now_text])?;
        }
        tx.execute("UPDATE recipe_schedules SET next_fire_utc=?1,version=version+1,updated_at=?2 WHERE id=?3 AND next_fire_utc=?4",params![future,now_text,schedule_id,next_text])?;
        tx.commit()?;
        processed += 1;
    }
    Ok(processed)
}

pub(crate) fn task_provenance(connection: &Connection, task_id: &str) -> Result<Option<Value>> {
    connection.query_row(
        "SELECT json_object('recipe_id',r.id,'recipe_name',r.name,'recipe_revision_id',rr.id,
             'recipe_revision',rr.revision,'profile_revision_id',b.profile_revision_id,
             'required_check_ids',json(b.required_check_ids_json),'config_revision_id',b.config_revision_id,
             'configuration_hash',b.configuration_hash,'workflow_version',b.workflow_version,
             'workflow_hash',b.workflow_hash,'schedule_id',b.schedule_id,
             'scheduled_for_utc',b.scheduled_for_utc)
         FROM task_recipe_bindings b JOIN task_recipe_revisions rr ON rr.id=b.recipe_revision_id
         JOIN task_recipes r ON r.id=rr.recipe_id WHERE b.task_id=?1",
        [task_id], |row| row.get::<_,String>(0),
    ).optional()?.map(|value|serde_json::from_str(&value).map_err(Into::into)).transpose()
}

fn json_rows(connection: &Connection, sql: &str) -> Result<Vec<Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|row| serde_json::from_str(&row).map_err(Into::into))
        .collect()
}

pub(crate) fn projection(connection: &Connection) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>)> {
    let profiles = json_rows(connection,
        "SELECT json_object('id',s.id,'project_id',s.project_id,'name',s.name,
          'version',s.version,'archived',json(CASE WHEN s.archived_at IS NULL THEN 'false' ELSE 'true' END),
          'revision',r.revision,'revision_id',r.id,'roles',json(r.roles_json),
          'config_revision_id',r.config_revision_id,'configuration_hash',r.configuration_hash)
         FROM project_profile_sets s JOIN projects p ON p.id=s.project_id AND p.internal_purpose IS NULL
         JOIN project_profile_set_revisions r ON r.profile_set_id=s.id AND r.revision=s.current_revision
         ORDER BY s.project_id,s.name")?;
    let recipes = json_rows(connection,
        "SELECT json_object('id',r.id,'project_id',r.project_id,'name',r.name,
          'version',r.version,'archived',json(CASE WHEN r.archived_at IS NULL THEN 'false' ELSE 'true' END),
          'revision',rr.revision,'revision_id',rr.id,'title',rr.title,'description',rr.description,
          'acceptance_criteria',json(rr.acceptance_criteria_json),'priority',rr.priority,
          'profile_revision_id',rr.profile_revision_id,'required_check_ids',json(rr.required_check_ids_json),
          'config_revision_id',rr.config_revision_id,'configuration_hash',rr.configuration_hash,
          'workflow_version',rr.workflow_version,'workflow_hash',rr.workflow_hash)
         FROM task_recipes r JOIN projects p ON p.id=r.project_id AND p.internal_purpose IS NULL
         JOIN task_recipe_revisions rr ON rr.recipe_id=r.id AND rr.revision=r.current_revision
         ORDER BY r.project_id,r.name")?;
    let schedules = json_rows(connection,
        "SELECT json_object('id',s.id,'project_id',s.project_id,'name',s.name,
          'version',s.version,'archived',json(CASE WHEN s.archived_at IS NULL THEN 'false' ELSE 'true' END),'paused',json(CASE WHEN s.paused=1 THEN 'true' ELSE 'false' END),
          'recipe_revision_id',s.recipe_revision_id,'recipe_name',r.name,
          'recipe_config_revision_id',rr.config_revision_id,
          'recipe_archived',json(CASE WHEN r.archived_at IS NULL THEN 'false' ELSE 'true' END),
          'cadence',s.cadence,
          'anchor_utc',s.anchor_utc,'next_fire_utc',s.next_fire_utc,
          'last_fire',(SELECT json_object('scheduled_for_utc',f.scheduled_for_utc,
            'outcome',f.outcome,'task_id',f.task_id,'reason',f.reason,
            'missed_first_utc',f.missed_first_utc,'missed_last_utc',f.missed_last_utc,
            'missed_count',f.missed_count) FROM recipe_schedule_fires f
            WHERE f.schedule_id=s.id ORDER BY f.scheduled_for_utc DESC LIMIT 1))
         FROM recipe_schedules s JOIN projects p ON p.id=s.project_id AND p.internal_purpose IS NULL
         JOIN task_recipe_revisions rr ON rr.id=s.recipe_revision_id JOIN task_recipes r ON r.id=rr.recipe_id
         ORDER BY s.project_id,s.name")?;
    Ok((profiles, recipes, schedules))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recurrence_is_strictly_future_and_utc_canonical() {
        let anchor = parse_utc("2026-09-25T09:00:00Z").unwrap();
        let before = DateTime::parse_from_rfc3339("2026-09-25T08:59:59.500Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            next_after(anchor, "daily", before).unwrap(),
            "2026-09-25T09:00:00Z"
        );
        assert_eq!(
            next_after(anchor, "daily", anchor).unwrap(),
            "2026-09-26T09:00:00Z"
        );
        assert_eq!(
            next_after(anchor, "weekly", anchor).unwrap(),
            "2026-10-02T09:00:00Z"
        );
        assert!(parse_utc("2026-09-25T04:00:00-05:00").is_err());
        assert!(parse_utc("2026-09-25T09:00:00.1Z").is_err());
        assert!(period("monthly").is_err());
    }
}
