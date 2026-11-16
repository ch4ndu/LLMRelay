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
    _bundles: &crate::provider_compatibility::BundleSet,
    command: &HumanCommand,
    now: &str,
) -> Result<OperationResult> {
    let operation = command.operation_id();
    match command {
        HumanCommand::RequestRecipeScheduleOccurrenceStop { intent, .. } => {
            visible_project(tx, &intent.project_id)?;
            if schedule_occurrence_stop_intent_in(tx, &intent.occurrence_id)?.as_ref()
                != Some(intent)
            {
                bail!("The occurrence changed; refresh before stopping the current run")
            }
            persist_schedule_stop_intent_in(
                tx,
                &intent.occurrence_id,
                "Stop current occurrence requested",
                now,
            )?;
            snapshot_schedule_stop_targets_in(tx, &intent.occurrence_id, now)?;
            let mut result = outcome(
                operation,
                "recipe_schedule_occurrence",
                intent.occurrence_id.clone(),
                intent.expected_version,
                "stop_requested",
            );
            result.detail = json!({"intent":intent});
            Ok(result)
        }
        HumanCommand::StopRecipeScheduleOccurrence {
            project_id,
            schedule_id,
            expected_version,
            binding,
            ..
        } => {
            let current: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM recipe_schedules s JOIN recipe_schedule_fires f ON f.schedule_id=s.id
                 WHERE s.id=?1 AND s.project_id=?2 AND s.version=?3 AND f.id=?4)",
                params![schedule_id,project_id,expected_version,binding.occurrence_id], |row| row.get(0),
            )?;
            if !current
                || schedule_stop_binding_in(tx, &binding.occurrence_id)?.as_ref() != Some(binding)
            {
                bail!("The occurrence changed; refresh before stopping the current run")
            }
            persist_schedule_stop_in(tx, binding, "Stop current occurrence requested", now)?;
            Ok(OperationResult {
                operation_id: operation.into(),
                entity_kind: "recipe_schedule_occurrence".into(),
                entity_id: binding.occurrence_id.clone(),
                version: Some(*expected_version),
                state: "stop_requested".into(),
                detail: json!({"binding":binding}),
            })
        }
        HumanCommand::AuthorizeRecipeScheduleExecution { .. } => {
            bail!("Automatic recipe execution has been retired. Create a draft schedule instead.")
        }
        HumanCommand::UpsertSingleAgentRecipe {
            project_id,
            recipe_id,
            expected_version,
            recipe,
            ..
        } => {
            visible_project(tx, project_id)?;
            bounded(&recipe.name, "recipe name", 100)?;
            bounded(&recipe.title, "task title", 200)?;
            if recipe.description.len() > 16_384
                || recipe.acceptance_criteria.is_empty()
                || recipe.acceptance_criteria.len() > 32
                || recipe
                    .acceptance_criteria
                    .iter()
                    .any(|item| item.trim().is_empty() || item.len() > 2_000)
            {
                bail!("Recipe needs bounded task content and acceptance criteria")
            }
            let selection = &recipe.selection;
            let (config_hash, primary_hash, reviewer_hash) =
                crate::workflow::single_agent_recipe_pins(tx, project_id, selection)?;
            let definition = crate::workflow::ensure_preset_definition(tx, selection.preset, now)?;
            let (id, revision, version) = match (recipe_id, expected_version) {
                (None, None) => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute("INSERT INTO task_recipes(id,project_id,name,current_revision,version,created_at,updated_at) VALUES(?1,?2,?3,1,1,?4,?4)",params![id,project_id,recipe.name.trim(),now])?;
                    (id, 1, 1)
                }
                (Some(id), Some(expected)) => {
                    let revision =
                        current_version(tx, "task_recipes", id, project_id, *expected)? + 1;
                    tx.execute("UPDATE task_recipes SET name=?1,current_revision=?2,version=version+1,updated_at=?3 WHERE id=?4",params![recipe.name.trim(),revision,now,id])?;
                    (id.clone(), revision, expected + 1)
                }
                _ => bail!("Recipe creation and update require matching identity and version"),
            };
            let revision_id = uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO task_recipe_revisions(id,recipe_id,revision,title,description,acceptance_criteria_json,priority,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash,reviewer_profile_revision_id,reviewer_profile_hash)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'single_agent',?13,?14,?15,?16,?17,?18,?19)",
                params![revision_id,id,revision,recipe.title.trim(),recipe.description,serde_json::to_string(&recipe.acceptance_criteria)?,recipe.priority,serde_json::to_string(&selection.check_ids)?,config_hash,crate::workflow_resources::single_agent_definition(selection.preset)?.version,definition.1,now,selection.preset.as_str(),definition.0,selection.execution_config_revision_id,selection.profile_revision_id,primary_hash,selection.reviewer_profile_revision_id,reviewer_hash])?;
            let mut result = outcome(operation, "recipe", id, version, "saved");
            result.detail = json!({"revision_id":revision_id,"revision":revision});
            Ok(result)
        }
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
            let version =
                tx.query_row("SELECT version FROM tasks WHERE id=?1", [&task_id], |row| {
                    row.get(0)
                })?;
            Ok(outcome(operation, "task", task_id, version, "backlog"))
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
            if let Some(id) = schedule_id {
                let mode: String = tx.query_row(
                    "SELECT mode FROM recipe_schedules WHERE id=?1 AND project_id=?2",
                    params![id, project_id],
                    |row| row.get(0),
                )?;
                if mode != "create_draft" {
                    bail!("This automatic schedule is retired. Create a new draft schedule.")
                }
            }
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
                let executing: bool = tx.query_row(
                    "SELECT mode IN ('run_authorized_research','run_authorized_recipe') FROM recipe_schedules WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )?;
                if executing {
                    hold_waiting_occurrences(
                        tx,
                        &id,
                        "Schedule edited; authorize future runs again",
                    )?;
                    tx.execute("UPDATE recipe_schedules SET mode='create_draft',active_authorization_id=NULL,paused=1,next_fire_utc=NULL WHERE id=?1", [&id])?;
                    tx.execute("UPDATE recipe_schedules SET name=?1,recipe_revision_id=?2,cadence=?3,anchor_utc=?4,version=version+1,updated_at=?5 WHERE id=?6", params![name.trim(),recipe_revision_id,cadence,anchor_utc,now,id])?;
                    return Ok(outcome(operation, "recipe_schedule", id, version, "paused"));
                }
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
            hold_waiting_occurrences(
                tx,
                schedule_id,
                "Future runs paused; this occurrence requires fresh future authorization",
            )?;
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
            let execution_mode: bool = tx.query_row("SELECT mode IN ('run_authorized_research','run_authorized_recipe') FROM recipe_schedules WHERE id=?1 AND project_id=?2", params![schedule_id,project_id], |row| row.get(0))?;
            if execution_mode {
                bail!(
                    "Automatic recipe execution has been retired. Create a draft schedule instead."
                )
            }
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

fn hold_waiting_occurrences(connection: &Connection, schedule: &str, reason: &str) -> Result<()> {
    let occurrences = {
        let mut statement = connection.prepare(
            "SELECT id FROM recipe_schedule_fires WHERE schedule_id=?1 AND authorization_id IS NOT NULL",
        )?;
        let rows = statement
            .query_map([schedule], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for occurrence in occurrences {
        crate::permissions::invalidate_schedule_in(
            connection,
            &occurrence,
            reason,
            &Utc::now().to_rfc3339(),
        )?;
    }
    connection.execute("UPDATE recipe_schedule_fires SET outcome='held',reason=?1
        WHERE schedule_id=?2 AND authorization_id IS NOT NULL AND outcome='waiting' AND invocations_spent=0",
        params![reason,schedule])?;
    Ok(())
}

impl Store {
    pub fn recipe_schedule_execution_preview(
        &self,
        request: &crate::domain::RecipeSchedulePreviewRequest,
    ) -> Result<crate::domain::RecipeScheduleExecutionPreview> {
        let connection = self.lock()?;
        schedule_execution_preview_in(&connection, &self.compatibility_bundles, request)
    }
}

fn schedule_execution_preview_in(
    _connection: &Connection,
    _bundles: &crate::provider_compatibility::BundleSet,
    _request: &crate::domain::RecipeSchedulePreviewRequest,
) -> Result<crate::domain::RecipeScheduleExecutionPreview> {
    bail!("Automatic recipe execution has been retired. Create a draft schedule instead.")
}

// Ancestry inventories uncertainty only. Every returned member must independently
// satisfy the immutable root/receipt predicate before it can authorize an effect.
fn schedule_member_candidates_in(connection: &Connection, occurrence: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "WITH RECURSIVE lineage(id) AS (
           SELECT attempt_id FROM recipe_schedule_fires WHERE id=?1 AND attempt_id IS NOT NULL
           UNION SELECT a.id FROM attempts a JOIN lineage p ON a.parent_attempt_id=p.id)
         SELECT id FROM lineage UNION SELECT g.attempt_id FROM sessions s
           JOIN role_generations g ON g.id=s.role_generation_id WHERE s.schedule_occurrence_id=?1",
    )?;
    let members = statement
        .query_map([occurrence], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(members)
}

pub(crate) fn schedule_members_in(
    connection: &Connection,
    occurrence: &str,
) -> Result<Vec<String>> {
    let members = schedule_member_candidates_in(connection, occurrence)?;
    for member in &members {
        if !schedule_attempt_member_in(connection, occurrence, member)? {
            bail!("An unrecognized scheduled descendant requires recovery; no process authority can be inferred")
        }
    }
    Ok(members)
}

fn schedule_occurrence_stop_intent_in(
    connection: &Connection,
    occurrence: &str,
) -> Result<Option<crate::domain::ScheduleOccurrenceStopIntent>> {
    connection
        .query_row(
            "SELECT s.project_id,s.id,s.version,f.id,a.id
         FROM recipe_schedule_fires f JOIN recipe_schedules s ON s.id=f.schedule_id
         JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
           AND a.schedule_id=s.id AND a.project_id=s.project_id
           AND a.recipe_revision_id=f.recipe_revision_id
         JOIN projects p ON p.id=s.project_id AND p.internal_purpose IS NULL
         WHERE f.id=?1 AND f.allowance_reserved=1 AND f.stop_requested_at IS NULL
           AND f.outcome IN ('waiting','reserved','running','recovery_required')
           AND a.restore_invalidated_at IS NULL
           AND NOT EXISTS(SELECT 1 FROM recovery_records
             WHERE id='database-restore-hold' AND state='attention_required')",
            [occurrence],
            |row| {
                Ok(crate::domain::ScheduleOccurrenceStopIntent {
                    project_id: row.get(0)?,
                    schedule_id: row.get(1)?,
                    expected_version: row.get(2)?,
                    occurrence_id: row.get(3)?,
                    authorization_id: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn persist_schedule_stop_intent_in(
    tx: &Transaction<'_>,
    occurrence: &str,
    reason: &str,
    now: &str,
) -> Result<()> {
    let changed = tx.execute(
        "UPDATE recipe_schedule_fires SET outcome='stop_requested',
           stop_requested_at=COALESCE(stop_requested_at,?1),stop_reason=COALESCE(stop_reason,?2)
         WHERE id=?3 AND authorization_id IS NOT NULL AND allowance_reserved=1
           AND outcome NOT IN ('held','missed','finished')
           AND EXISTS(SELECT 1 FROM recipe_schedule_authorizations a WHERE a.id=authorization_id
             AND a.restore_invalidated_at IS NULL)",
        params![now, reason, occurrence],
    )?;
    if changed != 1 {
        bail!("The occurrence cannot acquire stop authority; retain its recovery fence")
    }
    crate::permissions::invalidate_schedule_in(tx, occurrence, reason, now)?;
    // Readiness is fenced in the same transaction as intent, including null-root waiting work.
    tx.execute(
        "UPDATE tasks SET lifecycle='cancelled',version=version+1,updated_at=?1
        WHERE id=(SELECT task_id FROM recipe_schedule_fires WHERE id=?2 AND attempt_id IS NULL)
          AND lifecycle IN ('backlog','draft','ready')",
        params![now, occurrence],
    )?;
    Ok(())
}

pub(crate) fn snapshot_schedule_stop_targets_in(
    tx: &Transaction<'_>,
    occurrence: &str,
    now: &str,
) -> Result<Vec<crate::domain::ScheduleNativeStopTarget>> {
    let members = schedule_member_candidates_in(tx, occurrence)?;
    for attempt in members {
        match schedule_attempt_member_in(tx, occurrence, &attempt) {
            Ok(true) => {}
            evidence => {
                tx.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason=?1 WHERE id=?2",
                    params![format!("Unrecognized occurrence member cannot authorize a stop target: {evidence:?}"),occurrence])?;
                continue;
            }
        }
        let mut statement = tx.prepare(
            "SELECT s.id,s.role_generation_id,s.transcript_epoch,r.id,
                CASE WHEN s.launch_state='reserved' THEN NULL WHEN s.resume_count>0 THEN r.process_identity_json ELSE s.process_identity_json END,
                s.schedule_authorization_id
             FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
             LEFT JOIN resume_invocations r ON r.session_id=s.id AND r.transcript_epoch=s.transcript_epoch
             WHERE g.attempt_id=?1 AND s.schedule_occurrence_id=?2
               AND NOT (s.status='launch_failed' OR (s.status='exited'
                 AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1))",
        )?;
        let sessions = statement
            .query_map(params![attempt, occurrence], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for (session, generation, epoch, resume, process, grant) in sessions {
            tx.execute("INSERT INTO recipe_schedule_stop_targets(id,occurrence_id,authorization_id,attempt_id,
                target_kind,session_id,role_generation_id,resume_invocation_id,transcript_epoch,process_identity_json,created_at,observed_at)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11) ON CONFLICT DO NOTHING",
                params![uuid::Uuid::new_v4().to_string(),occurrence,grant,attempt,
                    if resume.is_some() { "resume" } else { "session" },session,generation,resume,epoch,process,now])?;
        }
        tx.execute("INSERT INTO recipe_schedule_stop_targets(id,occurrence_id,authorization_id,attempt_id,
            target_kind,session_id,role_generation_id,resume_invocation_id,transcript_epoch,process_identity_json,created_at,observed_at)
            SELECT lower(hex(randomblob(16))),?2,r.schedule_authorization_id,?1,'resume',s.id,s.role_generation_id,r.id,
              r.transcript_epoch,r.process_identity_json,?3,?3 FROM resume_invocations r
            JOIN sessions s ON s.id=r.session_id JOIN role_generations g ON g.id=s.role_generation_id
            WHERE g.attempt_id=?1 AND r.schedule_occurrence_id=?2 AND r.transcript_epoch!=s.transcript_epoch
              AND r.state NOT IN ('exited','proven_nondelivery') ON CONFLICT DO NOTHING",params![attempt,occurrence,now])?;
        tx.execute("INSERT INTO recipe_schedule_stop_targets(id,occurrence_id,authorization_id,attempt_id,
            target_kind,check_run_id,process_identity_json,created_at,observed_at)
            SELECT lower(hex(randomblob(16))),f.id,f.authorization_id,c.attempt_id,'check',c.id,c.recovery_anchor_json,?3,?3
            FROM check_runs c JOIN recipe_schedule_fires f ON f.id=?2 WHERE c.attempt_id=?1
              AND c.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')
            ON CONFLICT DO NOTHING", params![attempt,occurrence,now])?;
    }
    // A null target owns an invocation, but grants no signal authority. Bind only
    // its exact durable publication, including one that exited before this pass.
    let pending = {
        let mut statement = tx.prepare("SELECT target.id,target.attempt_id,
            CASE target.target_kind WHEN 'check' THEN c.recovery_anchor_json
              WHEN 'resume' THEN r.process_identity_json ELSE s.process_identity_json END
            FROM recipe_schedule_stop_targets target
            JOIN recipe_schedule_fires f ON f.id=target.occurrence_id
            JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
            LEFT JOIN sessions s ON s.id=target.session_id
            LEFT JOIN role_generations g ON g.id=s.role_generation_id
            LEFT JOIN resume_invocations r ON r.id=target.resume_invocation_id
            LEFT JOIN check_runs c ON c.id=target.check_run_id
            WHERE target.occurrence_id=?1 AND target.process_identity_json IS NULL
              AND target.status!='quiescent' AND target.restore_invalidated_at IS NULL
              AND a.restore_invalidated_at IS NULL AND f.authorization_id=target.authorization_id
              AND f.stop_requested_at IS NOT NULL AND f.outcome IN ('stop_requested','recovery_required')
              AND ((target.target_kind='check' AND c.attempt_id=target.attempt_id
                    AND c.recovery_anchor_json IS NOT NULL)
                OR (s.role_generation_id=target.role_generation_id AND g.attempt_id=target.attempt_id
                  AND s.schedule_occurrence_id=f.id AND s.schedule_authorization_id=a.id
                  AND s.transcript_epoch=target.transcript_epoch
                  AND ((target.target_kind='session' AND s.resume_count=0
                        AND s.launch_state!='reserved' AND s.process_identity_json IS NOT NULL)
                    OR (target.target_kind='resume' AND r.session_id=s.id
                      AND r.transcript_epoch=s.transcript_epoch AND r.state!='reserved'
                      AND r.schedule_occurrence_id=f.id AND r.schedule_authorization_id=a.id
                      AND r.process_identity_json IS NOT NULL))))")?;
        let rows = statement
            .query_map([occurrence], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (target, attempt, identity) in pending {
        if schedule_attempt_member_in(tx, occurrence, &attempt)? {
            tx.execute(
                "UPDATE recipe_schedule_stop_targets SET process_identity_json=?1,observed_at=?2
                WHERE id=?3 AND process_identity_json IS NULL",
                params![identity, now, target],
            )?;
        }
    }
    tx.execute("UPDATE recipe_schedule_stop_targets SET status='quiescent',observed_at=?2
        WHERE occurrence_id=?1 AND target_kind='check' AND restore_invalidated_at IS NULL
          AND EXISTS(SELECT 1 FROM check_runs c WHERE c.id=check_run_id AND c.attempt_id=recipe_schedule_stop_targets.attempt_id
            AND c.status NOT IN ('launch_reserved','running','recovery_required','launch_ambiguous')
            AND COALESCE(json_extract(c.evidence_json,'$.capture_incomplete'),0)=0
            AND c.recovery_anchor_json IS recipe_schedule_stop_targets.process_identity_json)",params![occurrence,now])?;
    let mut statement = tx.prepare("SELECT id,authorization_id,attempt_id,session_id,role_generation_id,
        transcript_epoch,resume_invocation_id,process_identity_json FROM recipe_schedule_stop_targets
        WHERE occurrence_id=?1 AND target_kind IN ('session','resume') AND status!='quiescent'
          AND restore_invalidated_at IS NULL ORDER BY created_at,id")?;
    let rows = statement
        .query_map([occurrence], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(
                id,
                authorization_id,
                attempt_id,
                session_id,
                role_generation_id,
                transcript_epoch,
                resume_invocation_id,
                process,
            )| {
                Ok(crate::domain::ScheduleNativeStopTarget {
                    id,
                    binding: crate::domain::ScheduleStopBinding {
                        occurrence_id: occurrence.into(),
                        authorization_id,
                        attempt_id,
                        session_id,
                        role_generation_id,
                        transcript_epoch,
                        resume_invocation_id,
                        process: process
                            .map(|value| serde_json::from_str(&value))
                            .transpose()?,
                    },
                })
            },
        )
        .collect()
}

pub(crate) fn schedule_native_target_quiescent_in(
    connection: &Connection,
    binding: &crate::domain::ScheduleStopBinding,
) -> Result<bool> {
    if !schedule_stop_is_current_in(connection, binding)? {
        return Ok(false);
    }
    Ok(connection.query_row(
        "SELECT status='launch_failed' OR (status='exited'
        AND COALESCE(json_extract(exit_json,'$.process_group_quiescent'),0)=1)
        FROM sessions WHERE id=?1 AND transcript_epoch=?2",
        params![binding.session_id, binding.transcript_epoch],
        |row| row.get(0),
    )?)
}

pub(crate) fn schedule_occurrence_quiescence_in(
    connection: &Connection,
    occurrence: &str,
    stopping: bool,
) -> Result<()> {
    for attempt in schedule_members_in(connection, occurrence)? {
        crate::workflow::single_agent_quiescence(connection, &attempt)?;
        crate::operations::require_schedule_member_effects_quiescent_in(connection, &attempt)?;
        let pending: bool = connection.query_row("SELECT
            EXISTS(SELECT 1 FROM rework_intents WHERE (parent_attempt_id=?1 OR new_attempt_id=?1)
              AND state NOT IN ('completed','cancelled'))
            OR EXISTS(SELECT 1 FROM single_agent_review_deliveries WHERE attempt_id=?1
              AND state IN ('reserved','materializing','ready','launched'))
            OR EXISTS(SELECT 1 FROM workspaces WHERE attempt_id=?1
              AND state IN ('reserved','unknown','recovery_required'))
            OR EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND status IN ('workspace_reserved','materialization_pending','needs_recovery'))
            OR (?2=0 AND EXISTS(SELECT 1 FROM workflow_result_bindings WHERE attempt_id=?1
              AND state IN ('awaiting_checks','awaiting_review','review_running','review_changes_requested')))
            OR (?2=1 AND EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1
              AND state IN ('reserved','launching','running','unknown','stopping')))",
            params![attempt,stopping], |row| row.get(0))?;
        if pending {
            bail!("Occurrence preparation, transition, or workspace ownership has not settled")
        }
    }
    let unbound: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM recipe_schedule_fires f
        JOIN attempts a ON a.task_id=f.task_id WHERE f.id=?1 AND f.attempt_id IS NULL)",
        [occurrence],
        |row| row.get(0),
    )?;
    if unbound {
        bail!("Waiting occurrence has unexpected attempt ownership; recovery is required")
    }
    Ok(())
}

pub(crate) fn schedule_stop_binding_in(
    connection: &Connection,
    occurrence: &str,
) -> Result<Option<crate::domain::ScheduleStopBinding>> {
    let frozen: Option<String> = connection
        .query_row(
            "SELECT stop_process_identity_json FROM recipe_schedule_fires WHERE id=?1",
            [occurrence],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(frozen) = frozen {
        return Ok(Some(serde_json::from_str(&frozen)?));
    }
    let binding = connection.query_row(
        "SELECT f.authorization_id,f.attempt_id,s.id,s.role_generation_id,s.transcript_epoch,
                CASE WHEN s.launch_state='reserved' THEN NULL WHEN s.resume_count>0 THEN r.process_identity_json ELSE s.process_identity_json END,
                r.id FROM recipe_schedule_fires f JOIN role_generations g ON g.attempt_id=f.attempt_id
         JOIN sessions s ON s.role_generation_id=g.id AND s.schedule_occurrence_id=f.id
         LEFT JOIN resume_invocations r ON r.session_id=s.id AND r.transcript_epoch=s.transcript_epoch
         WHERE f.id=?1 AND f.invocations_spent>0
           AND (f.stop_session_id IS NULL OR (f.stop_session_id=s.id
             AND f.stop_transcript_epoch=s.transcript_epoch AND f.stop_resume_invocation_id IS r.id))
         ORDER BY s.created_at DESC LIMIT 1", [occurrence],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,
            row.get::<_,String>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?)),
    ).optional()?;
    binding
        .map(
            |(
                authorization_id,
                attempt_id,
                session_id,
                role_generation_id,
                transcript_epoch,
                process,
                resume_invocation_id,
            )| {
                Ok(crate::domain::ScheduleStopBinding {
                    occurrence_id: occurrence.into(),
                    authorization_id,
                    attempt_id,
                    session_id,
                    resume_invocation_id,
                    role_generation_id,
                    transcript_epoch,
                    process: process
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                })
            },
        )
        .transpose()
}

pub(crate) fn persist_schedule_stop_in(
    tx: &Transaction<'_>,
    binding: &crate::domain::ScheduleStopBinding,
    reason: &str,
    now: &str,
) -> Result<()> {
    let valid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM recipe_schedule_fires f JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
         WHERE f.id=?1 AND a.id=?2 AND f.attempt_id=?3 AND a.restore_invalidated_at IS NULL)",
        params![binding.occurrence_id,binding.authorization_id,binding.attempt_id], |row| row.get(0),
    )?;
    if !valid {
        bail!("Restored or changed schedule authority cannot authorize process signals")
    }
    let stopping: bool = tx.query_row(
        "SELECT stop_session_id IS NOT NULL FROM recipe_schedule_fires WHERE id=?1",
        [&binding.occurrence_id],
        |row| row.get(0),
    )?;
    if stopping && !schedule_stop_is_current_in(tx, binding)? {
        bail!("The immutable scheduled stop belongs to an earlier invocation identity")
    }
    tx.execute(
        "UPDATE recipe_schedule_fires SET outcome='stop_requested',
           stop_requested_at=COALESCE(stop_requested_at,?1),stop_reason=COALESCE(stop_reason,?2),
           stop_session_id=COALESCE(stop_session_id,?3),stop_resume_invocation_id=COALESCE(stop_resume_invocation_id,?4),
           stop_transcript_epoch=COALESCE(stop_transcript_epoch,?5),
           stop_process_identity_json=COALESCE(stop_process_identity_json,?6)
         WHERE id=?7 AND outcome NOT IN ('held','missed','finished')",
        params![now,reason,binding.session_id,binding.resume_invocation_id,binding.transcript_epoch,
            if binding.process.is_some() { Some(serde_json::to_string(binding)?) } else { None },binding.occurrence_id],
    )?;
    crate::permissions::invalidate_schedule_in(tx, &binding.occurrence_id, reason, now)?;
    Ok(())
}

pub(crate) fn schedule_stop_is_current_in(
    connection: &Connection,
    binding: &crate::domain::ScheduleStopBinding,
) -> Result<bool> {
    let target: Option<(Option<String>,Option<String>)> = connection.query_row(
        "SELECT target.process_identity_json,
           CASE WHEN target.target_kind='resume' THEN r.process_identity_json
             WHEN s.launch_state='reserved' THEN NULL ELSE s.process_identity_json END
         FROM recipe_schedule_stop_targets target
         JOIN recipe_schedule_fires f ON f.id=target.occurrence_id
         JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
         JOIN sessions s ON s.id=target.session_id JOIN role_generations g ON g.id=s.role_generation_id
         LEFT JOIN resume_invocations r ON r.id=target.resume_invocation_id
         WHERE target.occurrence_id=?1 AND target.authorization_id=?2 AND target.attempt_id=?3
           AND target.session_id=?4 AND target.role_generation_id=?5 AND target.transcript_epoch=?6
           AND target.resume_invocation_id IS ?7
           AND target.restore_invalidated_at IS NULL AND a.restore_invalidated_at IS NULL
           AND f.authorization_id=target.authorization_id AND f.stop_requested_at IS NOT NULL
           AND f.outcome IN ('stop_requested','recovery_required') AND g.attempt_id=target.attempt_id
           AND s.schedule_occurrence_id=f.id AND s.schedule_authorization_id=a.id
           AND s.transcript_epoch=target.transcript_epoch AND s.role_generation_id=target.role_generation_id
           AND ((target.target_kind='session' AND s.resume_count=0)
             OR (target.target_kind='resume' AND r.session_id=s.id AND r.transcript_epoch=s.transcript_epoch
                 AND r.schedule_occurrence_id=f.id AND r.schedule_authorization_id=a.id
                 ))",
        params![binding.occurrence_id,binding.authorization_id,binding.attempt_id,binding.session_id,
            binding.role_generation_id,binding.transcript_epoch,binding.resume_invocation_id],
        |row|Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    if let Some((frozen, current)) = target {
        let parse = |value: Option<String>| -> Result<Option<crate::domain::ProcessIdentity>> {
            Ok(value.map(|text| serde_json::from_str(&text)).transpose()?)
        };
        return Ok(parse(frozen)? == binding.process
            && parse(current)? == binding.process
            && schedule_attempt_member_in(
                connection,
                &binding.occurrence_id,
                &binding.attempt_id,
            )?);
    }
    // A known expanded target that failed exact validation cannot fall back to
    // the legacy root binding, including after target-only restore invalidation.
    let known: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM recipe_schedule_stop_targets
        WHERE occurrence_id=?1 AND session_id=?2 AND transcript_epoch=?3)",
        params![
            binding.occurrence_id,
            binding.session_id,
            binding.transcript_epoch
        ],
        |row| row.get(0),
    )?;
    if known {
        return Ok(false);
    }
    let current: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM recipe_schedule_fires f JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
         JOIN sessions s ON s.id=f.stop_session_id JOIN role_generations g ON g.id=s.role_generation_id
         WHERE f.id=?1 AND f.authorization_id=?2 AND f.attempt_id=?3 AND s.id=?4
           AND g.id=?5 AND s.transcript_epoch=?6 AND s.schedule_occurrence_id=f.id
           AND s.schedule_authorization_id=a.id AND g.attempt_id=f.attempt_id
           AND f.stop_transcript_epoch=s.transcript_epoch AND f.stop_resume_invocation_id IS ?7
           AND f.stop_requested_at IS NOT NULL AND a.restore_invalidated_at IS NULL
           AND f.outcome IN ('stop_requested','recovery_required')
           AND (?7 IS NULL OR EXISTS(SELECT 1 FROM resume_invocations r WHERE r.id=?7
             AND r.session_id=s.id AND r.transcript_epoch=s.transcript_epoch AND r.schedule_occurrence_id=f.id)))",
        params![binding.occurrence_id,binding.authorization_id,binding.attempt_id,binding.session_id,
            binding.role_generation_id,binding.transcript_epoch,binding.resume_invocation_id], |row| row.get(0),
    )?;
    if !current {
        return Ok(false);
    }
    let process: Option<String> = connection.query_row(
        "SELECT CASE WHEN s.launch_state='reserved' THEN NULL WHEN s.resume_count>0 THEN r.process_identity_json ELSE s.process_identity_json END
         FROM sessions s LEFT JOIN resume_invocations r ON r.session_id=s.id AND r.transcript_epoch=s.transcript_epoch WHERE s.id=?1",
        [&binding.session_id], |row| row.get(0))?;
    Ok(process
        .map(|value| serde_json::from_str::<crate::domain::ProcessIdentity>(&value))
        .transpose()?
        == binding.process)
}

pub(crate) fn schedule_attempt_member_in(
    connection: &Connection,
    occurrence: &str,
    attempt: &str,
) -> Result<bool> {
    let frozen: Option<(String, String, String, String, String)> = connection
        .query_row(
            "SELECT a.project_id,f.task_id,f.attempt_id,a.scope_json,a.scope_hash
         FROM recipe_schedule_fires f JOIN recipe_schedule_authorizations a
           ON a.id=f.authorization_id AND a.schedule_id=f.schedule_id
         JOIN tasks t ON t.id=f.task_id AND t.project_id=a.project_id
         WHERE f.id=?1 AND f.attempt_id IS NOT NULL",
            [occurrence],
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
    let Some((project, task, root, scope, hash)) = frozen else {
        return Ok(false);
    };
    let scope: Value = serde_json::from_str(&scope)?;
    if crate::store::json_hash(&scope)? != hash {
        bail!("Stored schedule authority does not match its immutable digest")
    }
    crate::workflow::single_agent_schedule_attempt_member(
        connection, &project, &task, &root, attempt, &scope,
    )
}

pub(crate) fn schedule_authority_in(
    _connection: &Connection,
    _bundles: &crate::provider_compatibility::BundleSet,
    _occurrence: &str,
    _task: &str,
    _attempt: Option<&str>,
    _before_reservation: bool,
    _now: DateTime<Utc>,
) -> Result<crate::domain::RecipeScheduleInvocationBinding> {
    bail!("Automatic recipe execution has been retired. Create a draft schedule instead.")
}

pub(crate) fn require_root_schedule_preparation_in(
    connection: &Connection,
    task: &str,
) -> Result<()> {
    if let Some(reason) = crate::workflow::task_execution_retirement_problem(connection, task)? {
        bail!("{reason}")
    }
    Ok(())
}

pub(crate) fn bind_schedule_claim_in(
    tx: &Transaction<'_>,
    _bundles: &crate::provider_compatibility::BundleSet,
    task: &str,
    _attempt: &str,
) -> Result<()> {
    require_root_schedule_preparation_in(tx, task)
}

pub(crate) fn reserve_schedule_invocation_in(
    tx: &Transaction<'_>,
    _bundles: &crate::provider_compatibility::BundleSet,
    context: &crate::store::SingleAgentLaunchContext,
    _launch: &crate::providers::SingleAgentLaunch,
    _resume: bool,
    _now: DateTime<Utc>,
) -> Result<Option<crate::domain::RecipeScheduleInvocationBinding>> {
    crate::workflow::require_supported_attempt_execution(tx, &context.attempt_id)?;
    Ok(None)
}

pub(crate) fn require_schedule_spawn_in(
    tx: &Transaction<'_>,
    _bundles: &crate::provider_compatibility::BundleSet,
    session: &str,
    epoch: &str,
) -> Result<bool> {
    let attempt: Option<String> = tx.query_row(
        "SELECT g.attempt_id FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
         WHERE s.id=?1 AND s.transcript_epoch=?2", params![session,epoch], |row| row.get(0),
    ).optional()?;
    if let Some(attempt) = attempt {
        crate::workflow::require_supported_attempt_execution(tx, &attempt)?;
    }
    Ok(false)
}

fn require_recipe_revision(
    connection: &Connection,
    project_id: &str,
    revision_id: &str,
) -> Result<()> {
    let preset: Option<String> = connection
        .query_row(
            "SELECT preset FROM task_recipe_revisions WHERE id=?1 AND family='single_agent'",
            [revision_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(preset) = preset {
        preset
            .parse::<crate::domain::SingleAgentPreset>()
            .map_err(|error| anyhow!(error))?
            .require_supported()?;
    }
    let eligible: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM task_recipe_revisions rr JOIN task_recipes r ON r.id=rr.recipe_id WHERE rr.id=?1 AND r.project_id=?2 AND r.archived_at IS NULL)", params![revision_id,project_id], |row| row.get(0))?;
    if !eligible {
        bail!("recipe revision is foreign, missing, or archived")
    }
    Ok(())
}

pub(crate) fn require_binding_current(connection: &Connection, task_id: &str) -> Result<()> {
    let single: Option<(String,String)> = connection.query_row("SELECT project_id,recipe_revision_id FROM task_recipe_bindings WHERE task_id=?1 AND family='single_agent'",[task_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    if let Some((project, revision)) = single {
        let selection = single_agent_selection(connection, &project, &revision)?;
        let matches: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM task_workflow_selections s JOIN task_recipe_bindings b ON b.task_id=s.task_id WHERE s.task_id=?1 AND s.family=b.family AND s.preset=b.preset AND s.definition_id=b.definition_id AND s.execution_config_revision_id=b.execution_config_revision_id AND s.profile_revision_id=b.primary_profile_revision_id AND s.reviewer_profile_revision_id IS b.reviewer_profile_revision_id AND s.check_ids_json=b.required_check_ids_json AND s.workspace_mode='isolated_worktree')",[task_id],|row|row.get(0))?;
        if !matches || selection.family != crate::domain::WorkflowFamily::SingleAgent {
            bail!("Recipe selection changed; create a draft from the current recipe revision")
        }
        return Ok(());
    }
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
    if recipe_family(tx, revision_id)? == "single_agent" {
        return materialize_single_agent(tx, project_id, revision_id, schedule, now);
    }
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

fn recipe_family(connection: &Connection, revision_id: &str) -> Result<String> {
    Ok(connection.query_row(
        "SELECT family FROM task_recipe_revisions WHERE id=?1",
        [revision_id],
        |row| row.get(0),
    )?)
}

fn single_agent_selection(
    connection: &Connection,
    project_id: &str,
    revision_id: &str,
) -> Result<crate::domain::TaskWorkflowSelectionInput> {
    require_recipe_revision(connection, project_id, revision_id)?;
    let (preset, definition_id, definition_hash, version, config, config_hash, primary, primary_hash, reviewer, reviewer_hash, checks):
        (String,String,String,String,String,String,String,String,Option<String>,Option<String>,String) = connection.query_row(
        "SELECT preset,definition_id,workflow_hash,workflow_version,execution_config_revision_id,configuration_hash,primary_profile_revision_id,primary_profile_hash,reviewer_profile_revision_id,reviewer_profile_hash,required_check_ids_json FROM task_recipe_revisions WHERE id=?1 AND family='single_agent'",
        [revision_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?)))?;
    let selection = crate::domain::TaskWorkflowSelectionInput {
        family: crate::domain::WorkflowFamily::SingleAgent,
        preset: preset.parse().map_err(|error: String| anyhow!(error))?,
        workspace_mode: Default::default(),
        execution_config_revision_id: Some(config),
        profile_revision_id: Some(primary),
        reviewer_profile_revision_id: reviewer,
        check_ids: serde_json::from_str(&checks)?,
    };
    let definition = crate::workflow_resources::single_agent_definition(selection.preset)?;
    let stored: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM workflow_definitions WHERE id=?1 AND content_hash=?2 AND definition_text=?3)",params![definition.id,definition.hash(),definition.text],|row|row.get(0))?;
    if !stored
        || definition.id != definition_id
        || definition.hash() != definition_hash
        || definition.version != version
        || crate::workflow::single_agent_recipe_pins(connection, project_id, &selection)?
            != (config_hash, primary_hash, reviewer_hash)
    {
        bail!("The recipe definition, configuration or profile is stale; save a current revision")
    }
    Ok(selection)
}

fn materialize_single_agent(
    tx: &Transaction<'_>,
    project_id: &str,
    revision_id: &str,
    schedule: Option<(&str, &str)>,
    now: &str,
) -> Result<String> {
    visible_project(tx, project_id)?;
    let selection = single_agent_selection(tx, project_id, revision_id)?;
    let (title, description, criteria, priority): (String,String,String,i64) = tx.query_row("SELECT title,description,acceptance_criteria_json,priority FROM task_recipe_revisions WHERE id=?1",[revision_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    let task = crate::workflow::insert_task_rows(
        tx,
        project_id,
        &title,
        &description,
        &serde_json::from_str::<Vec<String>>(&criteria)?,
        priority,
        &Value::Null,
        None,
        false,
        now,
    )?;
    crate::workflow::set_task_workflow(tx, &task, 1, &selection, now)?;
    let (schedule_id, scheduled_for) = schedule
        .map(|(id, time)| (Some(id), Some(time)))
        .unwrap_or((None, None));
    tx.execute("INSERT INTO task_recipe_bindings(task_id,project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash,reviewer_profile_revision_id,reviewer_profile_hash)
        SELECT ?1,?2,id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,?3,?4,?5,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash,reviewer_profile_revision_id,reviewer_profile_hash FROM task_recipe_revisions WHERE id=?6",
        params![task,project_id,schedule_id,scheduled_for,now,revision_id])?;
    Ok(task)
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
    if recipe_family(connection, revision_id)? == "single_agent" {
        return match single_agent_selection(connection, project_id, revision_id) {
            Ok(_) => Ok(None),
            Err(error) => expected_ineligibility(error, "single_agent_recipe_pins_unavailable"),
        };
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
    {
        let _intake = gate
            .map(|(lock, _, _)| lock.lock().map_err(|_| anyhow!("intake mutex is poisoned")))
            .transpose()?;
        if let Some((_, enabled, draining)) = gate {
            if !enabled.load(Ordering::SeqCst) || draining.load(Ordering::SeqCst) {
                return Ok(0);
            }
        }
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id='database-restore-hold' AND state='attention_required')",
            [], |row| row.get(0),
        )?;
        if held {
            return Ok(0);
        }
        tx.commit()?;
    }
    let schedule_ids = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT s.id FROM recipe_schedules s JOIN projects p ON p.id=s.project_id
             WHERE p.internal_purpose IS NULL AND s.archived_at IS NULL AND s.paused=0
               AND s.mode='create_draft' AND s.active_authorization_id IS NULL
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
        let schedule: Option<(String,String,String,String,Option<String>)> = tx.query_row(
            "SELECT s.project_id,s.recipe_revision_id,s.cadence,s.next_fire_utc,s.active_authorization_id FROM recipe_schedules s JOIN projects p ON p.id=s.project_id
             WHERE s.id=?1 AND p.internal_purpose IS NULL AND s.paused=0 AND s.archived_at IS NULL AND s.mode='create_draft' AND s.active_authorization_id IS NULL AND s.next_fire_utc<=?2",
            params![schedule_id,now_text],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        let Some((project_id, revision_id, cadence, next_text, grant)) = schedule else {
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
        let reason = if eligible && grant.is_none() {
            fire_ineligible(&tx, &project_id, &revision_id)?
        } else if eligible {
            None
        } else {
            Some("before_service_start")
        };
        let provisional = tx.execute(
            "INSERT INTO recipe_schedule_fires(schedule_id,scheduled_for_utc,recipe_revision_id,outcome,reason,missed_first_utc,missed_last_utc,missed_count,created_at,authorization_id)
             VALUES(?1,?2,?3,CASE WHEN ?9 IS NOT NULL AND ?10 THEN
               CASE WHEN EXISTS(SELECT 1 FROM recipe_schedule_fires WHERE schedule_id=?1
                 AND outcome IN ('reserved','running','stop_requested','recovery_required'))
                 THEN 'held' ELSE 'recovery_required' END ELSE 'missed' END,
               ?4,?5,?6,?7,?8,?9) ON CONFLICT(schedule_id,scheduled_for_utc) DO NOTHING",
            params![schedule_id,fire_time,revision_id,reason,missed_first,missed_last,missed_count,now_text,grant,eligible],
        )?;
        if provisional == 1 {
            let (outcome, task_id) = if !eligible {
                ("missed".to_owned(), None)
            } else if reason.is_some() {
                ("skipped_ineligible".to_owned(), None)
            } else {
                (
                    "task_created".to_owned(),
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
             'family',b.family,'preset',b.preset,'definition_id',b.definition_id,
             'execution_config_revision_id',b.execution_config_revision_id,'primary_profile_revision_id',b.primary_profile_revision_id,'reviewer_profile_revision_id',b.reviewer_profile_revision_id,
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
          'workflow_version',rr.workflow_version,'workflow_hash',rr.workflow_hash,
          'family',rr.family,'preset',rr.preset,'definition_id',rr.definition_id,
          'execution_config_revision_id',rr.execution_config_revision_id,'primary_profile_revision_id',rr.primary_profile_revision_id,
          'primary_profile_hash',rr.primary_profile_hash,'reviewer_profile_revision_id',rr.reviewer_profile_revision_id,'reviewer_profile_hash',rr.reviewer_profile_hash,
          'revisions',json((SELECT json_group_array(json_object('id',h.id,'revision',h.revision,'family',h.family,'preset',h.preset,'definition_id',h.definition_id,'execution_config_revision_id',h.execution_config_revision_id,'primary_profile_revision_id',h.primary_profile_revision_id,'reviewer_profile_revision_id',h.reviewer_profile_revision_id,'workflow_version',h.workflow_version,'workflow_hash',h.workflow_hash)) FROM (SELECT * FROM task_recipe_revisions WHERE recipe_id=r.id ORDER BY revision DESC) h)))
         FROM task_recipes r JOIN projects p ON p.id=r.project_id AND p.internal_purpose IS NULL
         JOIN task_recipe_revisions rr ON rr.recipe_id=r.id AND rr.revision=r.current_revision
         ORDER BY r.project_id,r.name")?;
    let mut schedules = json_rows(connection,
        "SELECT json_object('id',s.id,'project_id',s.project_id,'name',s.name,
          'version',s.version,'archived',json(CASE WHEN s.archived_at IS NULL THEN 'false' ELSE 'true' END),'paused',json(CASE WHEN s.paused=1 THEN 'true' ELSE 'false' END),
          'recipe_revision_id',s.recipe_revision_id,'recipe_name',r.name,
          'mode',s.mode,'active_authorization_id',s.active_authorization_id,
          'authorization',(SELECT json_object('id',a.id,'scope_hash',a.scope_hash,'scope',json(a.scope_json),
            'created_at',a.created_at,'revoked_at',a.revoked_at,'restore_invalidated_at',a.restore_invalidated_at,
            'max_occurrences',a.max_occurrences,'max_invocations_per_occurrence',a.max_invocations_per_occurrence,
            'max_runtime_seconds',a.max_runtime_seconds,'start_window_seconds',a.start_window_seconds,
            'reserved_occurrences',(SELECT count(*) FROM recipe_schedule_fires f WHERE f.authorization_id=a.id AND f.allowance_reserved=1),
            'remaining_occurrences',a.max_occurrences-(SELECT count(*) FROM recipe_schedule_fires f WHERE f.authorization_id=a.id AND f.allowance_reserved=1))
            FROM recipe_schedule_authorizations a WHERE a.id=s.active_authorization_id),
          'execution_intake_enabled',json('false'),
          'occurrences',json((SELECT json_group_array(json_object('id',f.id,'authorization_id',f.authorization_id,
            'scheduled_for_utc',f.scheduled_for_utc,'task_id',f.task_id,'attempt_id',f.attempt_id,'outcome',f.outcome,
            'reason',f.reason,'allowance_reserved',f.allowance_reserved,'invocations_spent',f.invocations_spent,
            'stop_requested_at',f.stop_requested_at,'stop_reason',f.stop_reason,
            'runtime_deadline_utc',f.runtime_deadline_utc)) FROM
            (SELECT * FROM recipe_schedule_fires WHERE schedule_id=s.id ORDER BY scheduled_for_utc DESC LIMIT 100) f)),
          'recipe_config_revision_id',COALESCE(rr.execution_config_revision_id,rr.config_revision_id),
          'recipe_family',rr.family,'recipe_preset',rr.preset,
          'recipe_archived',json(CASE WHEN r.archived_at IS NULL THEN 'false' ELSE 'true' END),
          'cadence',s.cadence,
          'anchor_utc',s.anchor_utc,'next_fire_utc',s.next_fire_utc,
          'last_fire',(SELECT json_object('scheduled_for_utc',f.scheduled_for_utc,
            'id',f.id,'authorization_id',f.authorization_id,'attempt_id',f.attempt_id,
            'allowance_reserved',json(CASE WHEN f.allowance_reserved=1 THEN 'true' ELSE 'false' END),
            'invocations_spent',f.invocations_spent,'runtime_deadline_utc',f.runtime_deadline_utc,
            'outcome',f.outcome,'task_id',f.task_id,'reason',f.reason,
            'missed_first_utc',f.missed_first_utc,'missed_last_utc',f.missed_last_utc,
            'missed_count',f.missed_count) FROM recipe_schedule_fires f
            WHERE f.schedule_id=s.id ORDER BY f.scheduled_for_utc DESC LIMIT 1))
         FROM recipe_schedules s JOIN projects p ON p.id=s.project_id AND p.internal_purpose IS NULL
         JOIN task_recipe_revisions rr ON rr.id=s.recipe_revision_id JOIN task_recipes r ON r.id=rr.recipe_id
         ORDER BY s.project_id,s.name")?;
    for schedule in &mut schedules {
        schedule["execution_authorization_supported"] = json!(false);
        schedule["execution_authorization_reason"] =
            json!("Automatic recipe execution has been retired. Schedules create drafts only.");
        schedule["execution_qualification_state"] = json!("unsupported");
        if let Some(occurrences) = schedule["occurrences"].as_array_mut() {
            for occurrence in occurrences {
                let id = occurrence["id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Schedule occurrence has no identity"))?;
                let intent = schedule_occurrence_stop_intent_in(connection, id)?;
                let eligible: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM recipe_schedule_fires f
                     JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
                     WHERE f.id=?1 AND a.restore_invalidated_at IS NULL
                     AND f.outcome IN ('reserved','running','stop_requested','recovery_required'))",
                    [id],
                    |row| row.get(0),
                )?;
                let binding = if eligible {
                    schedule_stop_binding_in(connection, id)?
                } else {
                    None
                };
                let exact = if let Some(binding) = &binding {
                    let stopping: bool = connection.query_row(
                        "SELECT stop_requested_at IS NOT NULL FROM recipe_schedule_fires WHERE id=?1", [id], |row| row.get(0),
                    )?;
                    // Frozen stop intent may belong to an earlier resume. Never advertise it as the current target.
                    binding.process.as_ref().is_some_and(|process| {
                        process.pid > 0
                            && process.process_group_id > 0
                            && !process.native_start_marker.trim().is_empty()
                    }) && if stopping {
                        schedule_stop_is_current_in(connection, binding)?
                    } else {
                        connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1
                            AND role_generation_id=?2 AND transcript_epoch=?3 AND launch_state='started'
                            AND status IN ('running','interrupt_requested','recovery_required'))",
                            params![binding.session_id,binding.role_generation_id,binding.transcript_epoch], |row| row.get(0))?
                    }
                } else {
                    false
                };
                occurrence["stop_intent_available"] = json!(intent.is_some());
                occurrence["stop_intent_reason"] = if intent.is_some() {
                    Value::Null
                } else if !occurrence["stop_requested_at"].is_null() {
                    json!(
                        "Stop has been requested; check the occurrence outcome or recovery status"
                    )
                } else {
                    json!("This occurrence cannot accept Stop; refresh to check its outcome or recovery status")
                };
                occurrence["stop_intent"] = serde_json::to_value(intent)?;
                occurrence["stop_binding"] = if exact {
                    serde_json::to_value(binding)?
                } else {
                    Value::Null
                };
            }
        }
    }
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
