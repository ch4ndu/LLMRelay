CREATE TABLE project_profile_sets (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    current_revision INTEGER NOT NULL CHECK(current_revision >= 1),
    version INTEGER NOT NULL CHECK(version >= 1),
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, name)
);

CREATE TABLE project_profile_set_revisions (
    id TEXT PRIMARY KEY,
    profile_set_id TEXT NOT NULL REFERENCES project_profile_sets(id),
    revision INTEGER NOT NULL CHECK(revision >= 1),
    roles_json TEXT NOT NULL CHECK(json_valid(roles_json) AND json_type(roles_json)='object'),
    config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    configuration_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(profile_set_id, revision)
);

CREATE TABLE task_recipes (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    current_revision INTEGER NOT NULL CHECK(current_revision >= 1),
    version INTEGER NOT NULL CHECK(version >= 1),
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, name)
);

CREATE TABLE task_recipe_revisions (
    id TEXT PRIMARY KEY,
    recipe_id TEXT NOT NULL REFERENCES task_recipes(id),
    revision INTEGER NOT NULL CHECK(revision >= 1),
    title TEXT NOT NULL,
    description TEXT NOT NULL,
    acceptance_criteria_json TEXT NOT NULL CHECK(json_valid(acceptance_criteria_json) AND json_type(acceptance_criteria_json)='array'),
    priority INTEGER NOT NULL,
    profile_revision_id TEXT NOT NULL REFERENCES project_profile_set_revisions(id),
    required_check_ids_json TEXT NOT NULL CHECK(json_valid(required_check_ids_json) AND json_type(required_check_ids_json)='array'),
    config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    configuration_hash TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    workflow_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(recipe_id, revision)
);

CREATE TABLE task_recipe_bindings (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    profile_revision_id TEXT NOT NULL REFERENCES project_profile_set_revisions(id),
    required_check_ids_json TEXT NOT NULL CHECK(json_valid(required_check_ids_json) AND json_type(required_check_ids_json)='array'),
    config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
    configuration_hash TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    workflow_hash TEXT NOT NULL,
    schedule_id TEXT REFERENCES recipe_schedules(id),
    scheduled_for_utc TEXT,
    created_at TEXT NOT NULL,
    CHECK((schedule_id IS NULL) = (scheduled_for_utc IS NULL))
);

CREATE TABLE recipe_schedules (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    cadence TEXT NOT NULL CHECK(cadence IN ('daily','weekly')),
    anchor_utc TEXT NOT NULL,
    next_fire_utc TEXT,
    paused INTEGER NOT NULL CHECK(paused IN (0,1)),
    version INTEGER NOT NULL CHECK(version >= 1),
    archived_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, name),
    CHECK(paused=0 OR next_fire_utc IS NULL)
);

CREATE TABLE recipe_schedule_fires (
    schedule_id TEXT NOT NULL REFERENCES recipe_schedules(id),
    scheduled_for_utc TEXT NOT NULL,
    recipe_revision_id TEXT NOT NULL REFERENCES task_recipe_revisions(id),
    outcome TEXT NOT NULL CHECK(outcome IN ('task_created','skipped_ineligible','missed')),
    task_id TEXT REFERENCES tasks(id),
    reason TEXT,
    missed_first_utc TEXT,
    missed_last_utc TEXT,
    missed_count INTEGER NOT NULL DEFAULT 0 CHECK(missed_count >= 0),
    created_at TEXT NOT NULL,
    PRIMARY KEY(schedule_id, scheduled_for_utc),
    CHECK((outcome='task_created') = (task_id IS NOT NULL))
);


-- Projected rows advance the committed dashboard cursor only on real changes.
CREATE TRIGGER state_revision_project_profile_sets_insert AFTER INSERT ON project_profile_sets
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_project_profile_sets_update AFTER UPDATE ON project_profile_sets
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.name IS NOT NEW.name OR OLD.current_revision IS NOT NEW.current_revision OR OLD.version IS NOT NEW.version OR OLD.archived_at IS NOT NEW.archived_at OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_project_profile_sets_delete AFTER DELETE ON project_profile_sets
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_project_profile_set_revisions_insert AFTER INSERT ON project_profile_set_revisions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_project_profile_set_revisions_update AFTER UPDATE ON project_profile_set_revisions
WHEN OLD.id IS NOT NEW.id OR OLD.profile_set_id IS NOT NEW.profile_set_id OR OLD.revision IS NOT NEW.revision OR OLD.roles_json IS NOT NEW.roles_json OR OLD.config_revision_id IS NOT NEW.config_revision_id OR OLD.configuration_hash IS NOT NEW.configuration_hash OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_project_profile_set_revisions_delete AFTER DELETE ON project_profile_set_revisions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipes_insert AFTER INSERT ON task_recipes
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipes_update AFTER UPDATE ON task_recipes
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.name IS NOT NEW.name OR OLD.current_revision IS NOT NEW.current_revision OR OLD.version IS NOT NEW.version OR OLD.archived_at IS NOT NEW.archived_at OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipes_delete AFTER DELETE ON task_recipes
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_revisions_insert AFTER INSERT ON task_recipe_revisions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_revisions_update AFTER UPDATE ON task_recipe_revisions
WHEN OLD.id IS NOT NEW.id OR OLD.recipe_id IS NOT NEW.recipe_id OR OLD.revision IS NOT NEW.revision OR OLD.title IS NOT NEW.title OR OLD.description IS NOT NEW.description OR OLD.acceptance_criteria_json IS NOT NEW.acceptance_criteria_json OR OLD.priority IS NOT NEW.priority OR OLD.profile_revision_id IS NOT NEW.profile_revision_id OR OLD.required_check_ids_json IS NOT NEW.required_check_ids_json OR OLD.config_revision_id IS NOT NEW.config_revision_id OR OLD.configuration_hash IS NOT NEW.configuration_hash OR OLD.workflow_version IS NOT NEW.workflow_version OR OLD.workflow_hash IS NOT NEW.workflow_hash OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_revisions_delete AFTER DELETE ON task_recipe_revisions
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_bindings_insert AFTER INSERT ON task_recipe_bindings
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_bindings_update AFTER UPDATE ON task_recipe_bindings
WHEN OLD.task_id IS NOT NEW.task_id OR OLD.project_id IS NOT NEW.project_id OR OLD.recipe_revision_id IS NOT NEW.recipe_revision_id OR OLD.profile_revision_id IS NOT NEW.profile_revision_id OR OLD.required_check_ids_json IS NOT NEW.required_check_ids_json OR OLD.config_revision_id IS NOT NEW.config_revision_id OR OLD.configuration_hash IS NOT NEW.configuration_hash OR OLD.workflow_version IS NOT NEW.workflow_version OR OLD.workflow_hash IS NOT NEW.workflow_hash OR OLD.schedule_id IS NOT NEW.schedule_id OR OLD.scheduled_for_utc IS NOT NEW.scheduled_for_utc OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_task_recipe_bindings_delete AFTER DELETE ON task_recipe_bindings
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedules_insert AFTER INSERT ON recipe_schedules
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedules_update AFTER UPDATE ON recipe_schedules
WHEN OLD.id IS NOT NEW.id OR OLD.project_id IS NOT NEW.project_id OR OLD.name IS NOT NEW.name OR OLD.recipe_revision_id IS NOT NEW.recipe_revision_id OR OLD.cadence IS NOT NEW.cadence OR OLD.anchor_utc IS NOT NEW.anchor_utc OR OLD.next_fire_utc IS NOT NEW.next_fire_utc OR OLD.paused IS NOT NEW.paused OR OLD.version IS NOT NEW.version OR OLD.archived_at IS NOT NEW.archived_at OR OLD.created_at IS NOT NEW.created_at OR OLD.updated_at IS NOT NEW.updated_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedules_delete AFTER DELETE ON recipe_schedules
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_fires_insert AFTER INSERT ON recipe_schedule_fires
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_fires_update AFTER UPDATE ON recipe_schedule_fires
WHEN OLD.schedule_id IS NOT NEW.schedule_id OR OLD.scheduled_for_utc IS NOT NEW.scheduled_for_utc OR OLD.recipe_revision_id IS NOT NEW.recipe_revision_id OR OLD.outcome IS NOT NEW.outcome OR OLD.task_id IS NOT NEW.task_id OR OLD.reason IS NOT NEW.reason OR OLD.missed_first_utc IS NOT NEW.missed_first_utc OR OLD.missed_last_utc IS NOT NEW.missed_last_utc OR OLD.missed_count IS NOT NEW.missed_count OR OLD.created_at IS NOT NEW.created_at
BEGIN UPDATE state_revision SET revision=revision+1; END;
CREATE TRIGGER state_revision_recipe_schedule_fires_delete AFTER DELETE ON recipe_schedule_fires
BEGIN UPDATE state_revision SET revision=revision+1; END;
