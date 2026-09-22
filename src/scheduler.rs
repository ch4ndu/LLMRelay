use crate::domain::OperationResult;
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DispatchPlan {
    pub task_id: String,
    pub attempt_id: String,
    pub project_id: String,
    pub repository_identity: String,
    pub repository_path: PathBuf,
    pub base_revision: String,
    pub workspace_id: String,
    pub workspace_path: PathBuf,
    pub state: String,
    pub single_step: bool,
}

#[derive(Clone)]
pub struct Scheduler {
    store: Store,
    worktrees: PathBuf,
    runtime: Option<RuntimeAdmission>,
}

#[derive(Clone)]
struct RuntimeAdmission {
    hooks: crate::providers::HookAssets,
    role_socket: PathBuf,
    executable: PathBuf,
}

impl Scheduler {
    pub fn new(store: Store, artifacts: PathBuf) -> Self {
        Self {
            store,
            worktrees: artifacts.join("worktrees"),
            runtime: None,
        }
    }

    pub fn with_runtime(
        mut self,
        hooks: crate::providers::HookAssets,
        role_socket: PathBuf,
        executable: PathBuf,
    ) -> Self {
        self.runtime = Some(RuntimeAdmission {
            hooks,
            role_socket,
            executable,
        });
        self
    }

    pub fn claim_next(&self) -> Result<Option<DispatchPlan>> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            anyhow!("scheduler admission requires the effective runtime capability policy")
        })?;
        let task_runtime = crate::trip::CapabilityRuntime {
            hooks: runtime.hooks.clone(),
            role_socket: runtime.role_socket.clone(),
            executable: runtime.executable.clone(),
        };
        let global_active: i64 = transaction.query_row("SELECT COUNT(*) FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping')", [], |row| row.get(0))?;
        if global_active >= 2 {
            return Ok(None);
        }
        let candidates = {
            let mut statement=transaction.prepare(
            "SELECT t.id,p.id,p.repository_identity,p.repository_path,p.base_revision,t.attention='run_next_requested' FROM tasks t JOIN projects p ON p.id=t.project_id
             LEFT JOIN scheduler_projects sp ON sp.project_id=p.id
             WHERE t.lifecycle='ready' AND t.archived_at IS NULL AND (p.queue_paused=0 OR t.attention='run_next_requested')
               AND NOT EXISTS(SELECT 1 FROM claims c WHERE c.repository_identity=p.repository_identity AND c.state IN ('reserved','launching','running','unknown','stopping'))
               AND NOT EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks parent ON parent.id=d.depends_on_task_id
                 WHERE d.task_id=t.id AND parent.lifecycle!='done')
             ORDER BY COALESCE(sp.last_claimed_at,''),t.priority DESC,t.manual_order,t.created_at")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut candidate = None;
        for item in candidates {
            if let Err(error) = crate::trip::require_project_ready(&transaction, &item.1) {
                transaction.execute(
                    "UPDATE tasks SET attention='needs_input',updated_at=?1 WHERE id=?2",
                    params![now, item.0],
                )?;
                tracing::info!(task_id = %item.0, reason = %error, "TRIP readiness held scheduler claim");
                continue;
            }
            let configured_roles: i64 = transaction.query_row(
                "SELECT COUNT(DISTINCT role) FROM role_settings WHERE task_id=?1",
                params![item.0],
                |row| row.get(0),
            )?;
            if configured_roles < 6 {
                transaction.execute(
                    "UPDATE tasks SET attention='needs_input',updated_at=?1 WHERE id=?2",
                    params![now, item.0],
                )?;
                continue;
            }
            if let Err(error) =
                crate::trip::require_task_profiles_activated(&transaction, &item.0, &task_runtime)
            {
                transaction.execute(
                    "UPDATE tasks SET lifecycle='backlog',attention='needs_input',ready_at=NULL,updated_at=?1 WHERE id=?2",
                    params![now, item.0],
                )?;
                tracing::info!(task_id = %item.0, reason = %error, "task profile held before scheduler claim");
                continue;
            }
            let manager_json: String = transaction.query_row(
                "SELECT config_json FROM role_settings WHERE task_id=?1 AND role='manager' ORDER BY revision DESC LIMIT 1",
                params![item.0], |row| row.get(0),
            )?;
            let manager_config: crate::domain::RoleOverride = serde_json::from_str(&manager_json)?;
            let manager_provider = manager_config.provider.to_string();
            if !crate::store::role_capacity_available(
                &transaction,
                &manager_provider,
                crate::domain::RoleKind::Manager,
                None,
                None,
            )? {
                transaction.execute(
                    "UPDATE tasks SET attention='queued_capacity',updated_at=?1 WHERE id=?2",
                    params![now, item.0],
                )?;
                continue;
            }
            let latest = {
                let mut statement = transaction.prepare(
                    "SELECT role,config_json FROM role_settings r WHERE task_id=?1
                     AND revision=(SELECT MAX(revision) FROM role_settings
                       WHERE task_id=r.task_id AND role=r.role) ORDER BY role",
                )?;
                let settings = statement
                    .query_map(params![item.0], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                settings
            };
            let mut all_supported = latest.len() == 6;
            for (role, config_json) in latest {
                let role = role
                    .parse::<crate::domain::RoleKind>()
                    .map_err(|error| anyhow!(error))?;
                let config: crate::domain::RoleOverride = serde_json::from_str(&config_json)?;
                let launch = crate::providers::prepare_role_launch(
                    config.provider,
                    role,
                    &config.model,
                    &config.effort,
                    std::path::Path::new(&item.3),
                    "capability admission only",
                    &task_runtime.role_socket,
                    "normalized-admission-token",
                    "normalized-admission-generation",
                    "normalized-admission-session",
                    None,
                    &task_runtime.hooks,
                    &task_runtime.executable,
                )?;
                all_supported &= crate::trip::current_task_profile_authority(
                    &transaction,
                    &item.0,
                    role,
                    transaction.query_row(
                        "SELECT MAX(revision) FROM role_settings WHERE task_id=?1 AND role=?2",
                        params![item.0, role.to_string()],
                        |row| row.get(0),
                    )?,
                    &launch.config,
                )
                .is_ok();
            }
            if !all_supported {
                transaction.execute(
                    "UPDATE tasks SET attention='blocked',updated_at=?1 WHERE id=?2",
                    params![now, item.0],
                )?;
                continue;
            }
            candidate = Some(item);
            break;
        }
        let Some((
            task_id,
            project_id,
            repository_identity,
            repository_path,
            base_revision,
            single_step,
        )) = candidate
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let workspace_id = uuid::Uuid::new_v4().to_string();
        let claim_id = uuid::Uuid::new_v4().to_string();
        let workspace_path = self.worktrees.join(&attempt_id);
        let scope: (String, String, String) = transaction.query_row(
            "SELECT title,description,acceptance_criteria_json FROM tasks WHERE id=?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let scope_hash = crate::store::json_hash(
            &serde_json::json!({"title":scope.0,"description":scope.1,"acceptance_criteria":serde_json::from_str::<serde_json::Value>(&scope.2)?,"base_revision":base_revision}),
        )?;
        let configurations = {
            let mut statement=transaction.prepare("SELECT role,revision,config_json FROM role_settings r WHERE task_id=?1 AND revision=(SELECT MAX(revision) FROM role_settings WHERE task_id=r.task_id AND role=r.role) ORDER BY role")?;
            let rows = statement
                .query_map(params![task_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let configuration_hash = crate::store::json_hash(&configurations)?;
        let configuration_revision = configurations.iter().map(|item| item.1).max().unwrap_or(1);
        transaction.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at,scope_hash,configuration_hash,workflow_version,workflow_hash,upstream_source_hash,overlay_hash,legacy_migration_required)
            VALUES(?1,?2,?3,'planning',?4,?5,'workspace_reserved',?6,?6,?7,?8,?9,?10,?11,?12,0)", params![attempt_id,task_id,uuid::Uuid::new_v4().to_string(),base_revision,configuration_revision,now,scope_hash,configuration_hash,crate::workflow_resources::WORKFLOW_VERSION,crate::workflow_resources::workflow_hash(),crate::trip::source_hash(),crate::trip::overlay_hash()])?;
        crate::trip::bind_attempt_profiles(
            &transaction,
            &task_id,
            &attempt_id,
            std::path::Path::new(&repository_path),
            &task_runtime,
            &now,
        )?;
        for (kind, allowance) in [("plan", 2), ("code", 2), ("final", 1)] {
            transaction.execute("INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance) VALUES(?1,?2,?3,?4)",
                params![uuid::Uuid::new_v4().to_string(),attempt_id,kind,allowance])?;
        }
        transaction.execute("INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at) VALUES(?1,?2,?3,?4,'reserved',?5,?5)",
            params![claim_id,task_id,attempt_id,repository_identity,now])?;
        transaction.execute("INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
            VALUES(?1,?2,?3,?4,?5,?5,?6,'reserved',?7,?7)", params![workspace_id,attempt_id,repository_identity,workspace_path.to_string_lossy(),base_revision,
                serde_json::json!({"no_auto_commit":true,"no_auto_merge":true,"eligible_untracked":true}).to_string(),now])?;
        transaction.execute("INSERT INTO scheduler_projects(project_id,last_claimed_at) VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET last_claimed_at=excluded.last_claimed_at", params![project_id,now])?;
        transaction.execute("UPDATE tasks SET lifecycle='in_progress',attention='none',version=version+1,updated_at=?1 WHERE id=?2", params![now,task_id])?;
        transaction.commit()?;
        drop(connection);
        let plan = DispatchPlan {
            task_id,
            attempt_id,
            project_id,
            repository_identity,
            repository_path: repository_path.into(),
            base_revision,
            workspace_id,
            workspace_path,
            state: "workspace_reserved".into(),
            single_step,
        };
        self.create_workspace(plan)
    }

    fn create_workspace(&self, mut plan: DispatchPlan) -> Result<Option<DispatchPlan>> {
        let repository = match crate::workspace::inspect(&plan.repository_path) {
            Ok(repository) => repository,
            Err(error) => {
                return self.fail_claim(
                    &plan,
                    &format!(
                        "registered repository inspection failed after reservation: {error:#}"
                    ),
                    serde_json::json!({
                        "stage":"repository_inspection",
                        "repository_path":plan.repository_path,
                        "worktree_path":plan.workspace_path,
                        "worktree_path_exists":plan.workspace_path.exists(),
                        "inspection_error":format!("{error:#}"),
                    }),
                );
            }
        };
        if repository.identity != plan.repository_identity {
            return self.fail_claim(
                &plan,
                "registered repository identity changed after reservation",
                serde_json::json!({
                    "stage":"repository_identity",
                    "repository_path":repository.root,
                    "expected_repository_identity":plan.repository_identity,
                    "observed_repository_identity":repository.identity,
                    "expected_base_revision":plan.base_revision,
                    "observed_base_revision":repository.head,
                    "worktree_path":plan.workspace_path,
                    "worktree_path_exists":plan.workspace_path.exists(),
                }),
            );
        }
        if repository.head != plan.base_revision {
            return self.fail_claim(
                &plan,
                "registered base revision drifted before dispatch",
                serde_json::json!({
                    "stage":"repository_base_revision",
                    "repository_path":repository.root,
                    "repository_identity":repository.identity,
                    "expected_base_revision":plan.base_revision,
                    "observed_base_revision":repository.head,
                    "worktree_path":plan.workspace_path,
                    "worktree_path_exists":plan.workspace_path.exists(),
                }),
            );
        }
        if let Err(error) =
            self.validate_dependencies(&plan.task_id, &repository, &plan.base_revision)
        {
            return self.release_claim(
                &plan,
                &format!("dependency integration became stale: {error:#}"),
            );
        }
        if let Err(error) = crate::workspace::create_detached_worktree(
            &repository,
            &plan.workspace_path,
            &plan.base_revision,
        ) {
            record_workspace_reservation_recovery(
                &self.store,
                &plan.attempt_id,
                &plan.workspace_id,
                &format!("worktree creation outcome requires recovery: {error:#}"),
                serde_json::json!({
                    "stage":"worktree_create",
                    "path_exists":plan.workspace_path.exists(),
                    "error":format!("{error:#}"),
                }),
            )?;
            bail!("worktree creation outcome requires recovery: {error:#}")
        }
        if let Err(error) = crate::trip::materialize_project_policy(
            &self.store,
            &plan.attempt_id,
            &plan.workspace_path,
        ) {
            return self.fail_claim(
                &plan,
                &format!("TRIP policy materialization requires recovery: {error:#}"),
                serde_json::json!({
                    "stage":"policy_materialization",
                    "repository_path":repository.root,
                    "repository_identity":repository.identity,
                    "base_revision":repository.head,
                    "worktree_path":plan.workspace_path,
                    "worktree_path_exists":plan.workspace_path.exists(),
                    "error":format!("{error:#}"),
                }),
            );
        }
        let now = Utc::now().to_rfc3339();
        let connection = self.store.lock()?;
        connection.execute(
            "UPDATE workspaces SET state='ready',updated_at=?1 WHERE id=?2",
            params![now, plan.workspace_id],
        )?;
        connection.execute(
            "UPDATE claims SET state='running',updated_at=?1 WHERE attempt_id=?2",
            params![now, plan.attempt_id],
        )?;
        connection.execute(
            "UPDATE attempts SET status=?1,updated_at=?2 WHERE id=?3",
            params![
                if plan.single_step { "held" } else { "running" },
                now,
                plan.attempt_id
            ],
        )?;
        if plan.single_step {
            connection.execute(
                "UPDATE tasks SET attention='paused',version=version+1,updated_at=?1 WHERE id=?2",
                params![now, plan.task_id],
            )?;
        }
        plan.state = if plan.single_step {
            "ready_held"
        } else {
            "ready"
        }
        .into();
        Ok(Some(plan))
    }

    fn validate_dependencies(
        &self,
        task_id: &str,
        repository: &crate::workspace::RepositoryInfo,
        base: &str,
    ) -> Result<()> {
        let connection = self.store.lock()?;
        let mut statement=connection.prepare("SELECT d.depends_on_task_id,d.integration_commit,d.integration_manifest_hash,s.manifest_json FROM task_dependencies d JOIN attempts a ON a.id=(SELECT accepted.id FROM attempts accepted WHERE accepted.task_id=d.depends_on_task_id AND accepted.status='done' AND accepted.accepted_snapshot_id IS NOT NULL ORDER BY accepted.created_at DESC LIMIT 1) JOIN snapshots s ON s.id=a.accepted_snapshot_id WHERE d.task_id=?1")?;
        let rows = statement
            .query_map(params![task_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        drop(connection);
        for (depends_on, recorded_commit, expected_hash, manifest_json) in rows {
            let manifest: crate::snapshot::SnapshotManifest = serde_json::from_str(&manifest_json)?;
            let observed_hash = crate::store::json_hash(&manifest)?;
            let commit = match recorded_commit {
                Some(commit) => commit,
                None => crate::snapshot::clean_candidate_commit(repository, &manifest)?
                    .ok_or_else(|| {
                        anyhow!("dirty dependency requires an explicit integration commit")
                    })?,
            };
            if !crate::workspace::is_ancestor(repository, &commit, base)? {
                bail!("integration commit {commit} is not in current base")
            }
            if expected_hash
                .as_deref()
                .is_some_and(|value| value != observed_hash)
            {
                bail!("accepted dependency snapshot changed")
            }
            crate::snapshot::verify_integration(repository, &manifest, &commit)?;
            if expected_hash.is_none() {
                let connection = self.store.lock()?;
                connection.execute("UPDATE task_dependencies SET integration_commit=?1,integration_manifest_hash=?2,verified_at=?3,evidence_json=?4 WHERE task_id=?5 AND depends_on_task_id=?6 AND integration_commit IS NULL",
                    params![commit,observed_hash,Utc::now().to_rfc3339(),serde_json::json!({"automatic":"clean_candidate_ancestry","candidate_head":commit,"repository_identity":repository.identity}).to_string(),task_id,depends_on])?;
            }
        }
        Ok(())
    }

    fn release_claim(&self, plan: &DispatchPlan, reason: &str) -> Result<Option<DispatchPlan>> {
        let connection = self.store.lock()?;
        let now = Utc::now().to_rfc3339();
        connection.execute(
            "UPDATE claims SET state='failed',updated_at=?1 WHERE attempt_id=?2",
            params![now, plan.attempt_id],
        )?;
        connection.execute(
            "UPDATE workspaces SET state='failed',updated_at=?1 WHERE attempt_id=?2",
            params![now, plan.attempt_id],
        )?;
        connection.execute(
            "UPDATE attempts SET status='failed',updated_at=?1 WHERE id=?2",
            params![now, plan.attempt_id],
        )?;
        connection.execute("UPDATE tasks SET lifecycle='ready',attention='blocked',version=version+1,updated_at=?1 WHERE id=?2", params![now,plan.task_id])?;
        bail!("{reason}")
    }

    fn fail_claim(
        &self,
        plan: &DispatchPlan,
        reason: &str,
        observation: serde_json::Value,
    ) -> Result<Option<DispatchPlan>> {
        record_workspace_reservation_recovery(
            &self.store,
            &plan.attempt_id,
            &plan.workspace_id,
            reason,
            observation,
        )?;
        bail!("{reason}")
    }

    pub fn reconcile_unknown(&self) -> Result<Vec<String>> {
        let connection = self.store.lock()?;
        let mut statement = connection.prepare(
            "SELECT w.id,w.path,w.base_revision,w.repository_identity,w.attempt_id
             FROM workspaces w JOIN attempts a ON a.id=w.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE w.state IN ('reserved','unknown','recovery_required')
               AND a.status NOT IN ('cancelled','done','failed')
               AND t.lifecycle NOT IN ('done','cancelled')",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        drop(connection);
        let mut recovered = Vec::new();
        for (workspace, path, base, identity, attempt) in rows {
            let observed = if std::path::Path::new(&path).exists() {
                crate::workspace::inspect(std::path::Path::new(&path))
                    .map(|repo| {
                        serde_json::json!({
                            "path_exists":true,
                            "repository_identity":repo.identity,
                            "head":repo.head,
                        })
                    })
                    .unwrap_or_else(|error| {
                        serde_json::json!({
                            "path_exists":true,"inspection_error":format!("{error:#}"),
                        })
                    })
            } else {
                serde_json::json!({"path_exists":false})
            };
            let valid = observed
                .get("repository_identity")
                .and_then(serde_json::Value::as_str)
                == Some(identity.as_str())
                && observed.get("head").and_then(serde_json::Value::as_str) == Some(base.as_str());
            if valid {
                promote_workspace_reservation(&self.store, &workspace, &attempt)?;
                recovered.push(attempt);
            } else {
                record_workspace_reservation_recovery(
                    &self.store,
                    &attempt,
                    &workspace,
                    "startup reconciliation could not prove the recorded workspace tuple",
                    observed,
                )?;
            }
        }
        Ok(recovered)
    }

    pub(crate) fn retry_workspace_reservation(
        &self,
        operation_id: &str,
        task_id: &str,
        attempt_id: &str,
        workspace_id: &str,
        expected_version: i64,
    ) -> Result<OperationResult> {
        let request = serde_json::json!({
            "operation":"retry_workspace_reservation","task_id":task_id,
            "attempt_id":attempt_id,"workspace_id":workspace_id,
            "expected_version":expected_version,
        });
        let request_hash = crate::store::json_hash(&request)?;
        let Some(tuple) = reserve_workspace_recovery_operation(
            &self.store,
            operation_id,
            "retry_workspace_reservation",
            &request_hash,
            task_id,
            attempt_id,
            workspace_id,
            expected_version,
        )?
        else {
            return self
                .store
                .operation_receipt(
                    operation_id,
                    "human_control",
                    "workspace_recovery",
                    &request_hash,
                )?
                .map(serde_json::from_value)
                .transpose()?
                .ok_or_else(|| anyhow!("workspace recovery receipt disappeared"));
        };
        let source = crate::workspace::inspect(&tuple.repository_path);
        let source_ok = source.as_ref().is_ok_and(|repository| {
            repository.identity == tuple.repository_identity
                && repository.head == tuple.base_revision
        });
        if !source_ok {
            let reason = source
                .err()
                .map(|error| format!("registered repository inspection failed: {error:#}"))
                .unwrap_or_else(|| {
                    "registered repository identity or base revision changed".to_owned()
                });
            return retain_workspace_recovery(
                &self.store,
                operation_id,
                &request_hash,
                &tuple,
                &reason,
                serde_json::json!({"stage":"retry_source_reinspection","reason":reason}),
            );
        }
        let repository = source.expect("source inspection was checked");
        let observed = inspect_workspace_tuple(&tuple);
        let valid_existing = observed
            .get("repository_identity")
            .and_then(serde_json::Value::as_str)
            == Some(tuple.repository_identity.as_str())
            && observed.get("head").and_then(serde_json::Value::as_str)
                == Some(tuple.base_revision.as_str());
        if !tuple.workspace_path.exists() {
            if let Err(error) = crate::workspace::create_detached_worktree(
                &repository,
                &tuple.workspace_path,
                &tuple.base_revision,
            ) {
                return retain_workspace_recovery(
                    &self.store,
                    operation_id,
                    &request_hash,
                    &tuple,
                    &format!(
                        "explicit worktree retry could not establish an exact outcome: {error:#}"
                    ),
                    serde_json::json!({"stage":"retry_worktree_create","error":format!("{error:#}"),"before":observed}),
                );
            }
        } else if !valid_existing {
            return retain_workspace_recovery(
                &self.store,
                operation_id,
                &request_hash,
                &tuple,
                "the recorded workspace path exists but is not the exact expected worktree",
                serde_json::json!({"stage":"retry_existing_worktree_reinspection","observed":observed}),
            );
        }
        if !workspace_path_matches_tuple(&tuple) {
            return retain_workspace_recovery(
                &self.store,
                operation_id,
                &request_hash,
                &tuple,
                "the worktree created by the explicit retry did not match the immutable repository identity and base revision",
                serde_json::json!({"stage":"retry_postcreate_reinspection","observed":inspect_workspace_tuple(&tuple)}),
            );
        }
        if let Err(error) = crate::trip::materialize_project_policy(
            &self.store,
            &tuple.attempt_id,
            &tuple.workspace_path,
        ) {
            return retain_workspace_recovery(
                &self.store,
                operation_id,
                &request_hash,
                &tuple,
                &format!(
                    "explicit worktree retry policy materialization requires recovery: {error:#}"
                ),
                serde_json::json!({"stage":"retry_policy_materialization","error":format!("{error:#}"),"after":inspect_workspace_tuple(&tuple)}),
            );
        }
        let promoted = promote_workspace_reservation(&self.store, workspace_id, attempt_id)?;
        let result = OperationResult {
            operation_id: operation_id.to_owned(),
            entity_kind: "workspace".to_owned(),
            entity_id: workspace_id.to_owned(),
            version: Some(expected_version + i64::from(promoted)),
            state: if promoted {
                "workspace_reservation_recovered".to_owned()
            } else {
                "workspace_reservation_already_reconciled".to_owned()
            },
            detail: serde_json::json!({
                "attempt_id":attempt_id,"task_id":task_id,"workspace_path":tuple.workspace_path.to_string_lossy(),
                "worktree_deleted":false,"fresh_provider_call":false,
            }),
        };
        finalize_workspace_recovery_operation(&self.store, operation_id, &request_hash, &result)?;
        Ok(result)
    }

    pub(crate) fn cancel_workspace_reservation(
        &self,
        operation_id: &str,
        task_id: &str,
        attempt_id: &str,
        workspace_id: &str,
        expected_version: i64,
    ) -> Result<OperationResult> {
        let request = serde_json::json!({
            "operation":"cancel_workspace_reservation","task_id":task_id,
            "attempt_id":attempt_id,"workspace_id":workspace_id,
            "expected_version":expected_version,
        });
        let request_hash = crate::store::json_hash(&request)?;
        let Some(tuple) = reserve_workspace_recovery_operation(
            &self.store,
            operation_id,
            "cancel_workspace_reservation",
            &request_hash,
            task_id,
            attempt_id,
            workspace_id,
            expected_version,
        )?
        else {
            return self
                .store
                .operation_receipt(
                    operation_id,
                    "human_control",
                    "workspace_recovery",
                    &request_hash,
                )?
                .map(serde_json::from_value)
                .transpose()?
                .ok_or_else(|| anyhow!("workspace recovery receipt disappeared"));
        };
        let observation = inspect_workspace_tuple(&tuple);
        let active_ownership = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                     WHERE rg.attempt_id=?1 AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
                    UNION ALL
                    SELECT 1 FROM check_runs WHERE attempt_id=?1
                     AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous')
                 )",
                params![attempt_id],
                |row| row.get::<_, bool>(0),
            )?
        };
        if active_ownership {
            return retain_workspace_recovery(
                &self.store,
                operation_id,
                &request_hash,
                &tuple,
                "workspace cancellation is blocked while exact provider or check ownership remains active or uncertain",
                serde_json::json!({"stage":"cancel_ownership_inventory","workspace":observation}),
            );
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = workspace_recovery_tuple_in(
            &transaction,
            task_id,
            attempt_id,
            workspace_id,
            expected_version,
        )?;
        if current.is_none() {
            let result = OperationResult {
                operation_id: operation_id.to_owned(),
                entity_kind: "workspace".to_owned(),
                entity_id: workspace_id.to_owned(),
                version: Some(expected_version),
                state: "workspace_recovery_stale".to_owned(),
                detail: serde_json::json!({
                    "task_id":task_id,
                    "attempt_id":attempt_id,
                    "workspace_path":tuple.workspace_path.to_string_lossy(),
                    "reason":"workspace recovery was reconciled or superseded before cancellation could commit",
                    "worktree_deleted":false,
                }),
            };
            finalize_workspace_recovery_operation_in(
                &transaction,
                operation_id,
                &request_hash,
                &result,
                &now,
            )?;
            transaction.commit()?;
            return Ok(result);
        }
        transaction.execute(
            "UPDATE workspaces SET state='abandoned',updated_at=?1 WHERE id=?2 AND attempt_id=?3",
            params![now, workspace_id, attempt_id],
        )?;
        transaction.execute(
            "UPDATE claims SET state='cancelled',updated_at=?1 WHERE id=?2 AND state='unknown'",
            params![now, tuple.claim_id],
        )?;
        transaction.execute(
            "UPDATE attempts SET status='cancelled',updated_at=?1 WHERE id=?2 AND status='needs_recovery'",
            params![now, attempt_id],
        )?;
        transaction.execute(
            "UPDATE tasks SET lifecycle='cancelled',attention='none',version=version+1,updated_at=?1
             WHERE id=?2 AND version=?3",
            params![now, task_id, expected_version],
        )?;
        transaction.execute(
            "UPDATE recovery_records SET state='resolved_workspace_cancelled',resolved_at=?1,updated_at=?1,
                    detail_json=json_set(detail_json,'$.cancellation_observation',json(?2),'$.worktree_deleted',false)
             WHERE attempt_id=?3 AND state='attention_required'
               AND json_extract(detail_json,'$.kind')='workspace_reservation'
               AND json_extract(detail_json,'$.workspace_id')=?4",
            params![now, observation.to_string(), attempt_id, workspace_id],
        )?;
        let result = OperationResult {
            operation_id: operation_id.to_owned(),
            entity_kind: "workspace".to_owned(),
            entity_id: workspace_id.to_owned(),
            version: Some(expected_version + 1),
            state: "workspace_reservation_cancelled".to_owned(),
            detail: serde_json::json!({
                "task_id":task_id,"attempt_id":attempt_id,"workspace_path":tuple.workspace_path.to_string_lossy(),
                "worktree_deleted":false,"surviving_path_requires_explicit_cleanup":tuple.workspace_path.exists(),
            }),
        };
        finalize_workspace_recovery_operation_in(
            &transaction,
            operation_id,
            &request_hash,
            &result,
            &now,
        )?;
        transaction.commit()?;
        Ok(result)
    }
}

#[derive(Clone, Debug)]
struct WorkspaceRecoveryTuple {
    task_id: String,
    attempt_id: String,
    workspace_id: String,
    claim_id: String,
    repository_identity: String,
    repository_path: PathBuf,
    base_revision: String,
    workspace_path: PathBuf,
}

fn workspace_recovery_tuple_in(
    transaction: &rusqlite::Transaction<'_>,
    task_id: &str,
    attempt_id: &str,
    workspace_id: &str,
    expected_version: i64,
) -> Result<Option<WorkspaceRecoveryTuple>> {
    transaction
        .query_row(
            "SELECT a.task_id,a.id,w.id,c.id,w.repository_identity,p.repository_path,w.base_revision,w.path
             FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
             JOIN workspaces w ON w.attempt_id=a.id JOIN claims c ON c.attempt_id=a.id
             WHERE t.id=?1 AND t.version=?2 AND a.id=?3 AND a.status='needs_recovery'
               AND w.id=?4 AND w.state IN ('unknown','recovery_required') AND c.state='unknown'
               AND EXISTS(SELECT 1 FROM recovery_records r WHERE r.attempt_id=a.id
                   AND r.state='attention_required'
                   AND json_extract(r.detail_json,'$.kind')='workspace_reservation'
                   AND json_extract(r.detail_json,'$.workspace_id')=w.id)",
            params![task_id, expected_version, attempt_id, workspace_id],
            |row| {
                Ok(WorkspaceRecoveryTuple {
                    task_id: row.get(0)?,
                    attempt_id: row.get(1)?,
                    workspace_id: row.get(2)?,
                    claim_id: row.get(3)?,
                    repository_identity: row.get(4)?,
                    repository_path: row.get::<_, String>(5)?.into(),
                    base_revision: row.get(6)?,
                    workspace_path: row.get::<_, String>(7)?.into(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn reserve_workspace_recovery_operation(
    store: &Store,
    operation_id: &str,
    operation: &str,
    request_hash: &str,
    task_id: &str,
    attempt_id: &str,
    workspace_id: &str,
    expected_version: i64,
) -> Result<Option<WorkspaceRecoveryTuple>> {
    if operation_id.trim().is_empty() {
        bail!("operation_id is required")
    }
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1
             AND actor_key='human_control' AND operation_kind='workspace_recovery'",
            params![operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((stored_hash, _)) = existing {
        if stored_hash != request_hash {
            bail!("operation ID was already used with different input")
        }
        transaction.commit()?;
        return Ok(None);
    }
    let tuple = workspace_recovery_tuple_in(
        &transaction,
        task_id,
        attempt_id,
        workspace_id,
        expected_version,
    )?
    .ok_or_else(|| {
        anyhow!("workspace recovery tuple is stale, terminal, or lacks its exact recovery record")
    })?;
    let now = Utc::now().to_rfc3339();
    let pending = OperationResult {
        operation_id: operation_id.to_owned(),
        entity_kind: "workspace".to_owned(),
        entity_id: workspace_id.to_owned(),
        version: Some(expected_version),
        state: "workspace_recovery_reserved".to_owned(),
        detail: serde_json::json!({"operation":operation,"attempt_id":attempt_id}),
    };
    transaction.execute(
        "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
         VALUES(?1,'human_control','workspace_recovery',?2,?3,?4)",
        params![operation_id, request_hash, serde_json::to_string(&pending)?, now],
    )?;
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','workspace.recovery.reserved','workspace',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            operation_id,
            workspace_id,
            serde_json::json!({"operation":operation,"attempt_id":attempt_id,"task_id":task_id}).to_string(),
            now
        ],
    )?;
    transaction.commit()?;
    Ok(Some(tuple))
}

fn finalize_workspace_recovery_operation(
    store: &Store,
    operation_id: &str,
    request_hash: &str,
    result: &OperationResult,
) -> Result<()> {
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    finalize_workspace_recovery_operation_in(
        &transaction,
        operation_id,
        request_hash,
        result,
        &Utc::now().to_rfc3339(),
    )?;
    transaction.commit()?;
    Ok(())
}

fn finalize_workspace_recovery_operation_in(
    transaction: &rusqlite::Transaction<'_>,
    operation_id: &str,
    request_hash: &str,
    result: &OperationResult,
    now: &str,
) -> Result<()> {
    let changed = transaction.execute(
        "UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2
         AND actor_key='human_control' AND operation_kind='workspace_recovery' AND request_hash=?3",
        params![serde_json::to_string(result)?, operation_id, request_hash],
    )?;
    if changed != 1 {
        bail!("workspace recovery receipt is missing or changed")
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','workspace.recovery.finalized','workspace',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            operation_id,
            &result.entity_id,
            serde_json::to_string(result)?,
            now
        ],
    )?;
    Ok(())
}

fn retain_workspace_recovery(
    store: &Store,
    operation_id: &str,
    request_hash: &str,
    tuple: &WorkspaceRecoveryTuple,
    reason: &str,
    observation: serde_json::Value,
) -> Result<OperationResult> {
    record_workspace_reservation_recovery(
        store,
        &tuple.attempt_id,
        &tuple.workspace_id,
        reason,
        observation,
    )?;
    let result = OperationResult {
        operation_id: operation_id.to_owned(),
        entity_kind: "workspace".to_owned(),
        entity_id: tuple.workspace_id.clone(),
        version: None,
        state: "workspace_recovery_required".to_owned(),
        detail: serde_json::json!({
            "task_id":tuple.task_id,"attempt_id":tuple.attempt_id,"reason":reason,
            "worktree_deleted":false,
        }),
    };
    finalize_workspace_recovery_operation(store, operation_id, request_hash, &result)?;
    Ok(result)
}

fn inspect_workspace_tuple(tuple: &WorkspaceRecoveryTuple) -> serde_json::Value {
    if !tuple.workspace_path.exists() {
        return serde_json::json!({"path_exists":false,"path":tuple.workspace_path});
    }
    crate::workspace::inspect(&tuple.workspace_path)
        .map(|repository| {
            serde_json::json!({
                "path_exists":true,"path":tuple.workspace_path,
                "repository_identity":repository.identity,"head":repository.head,
            })
        })
        .unwrap_or_else(|error| {
            serde_json::json!({
                "path_exists":true,"path":tuple.workspace_path,
                "inspection_error":format!("{error:#}"),
            })
        })
}

fn workspace_path_matches_tuple(tuple: &WorkspaceRecoveryTuple) -> bool {
    let observed = inspect_workspace_tuple(tuple);
    observed
        .get("repository_identity")
        .and_then(serde_json::Value::as_str)
        == Some(tuple.repository_identity.as_str())
        && observed.get("head").and_then(serde_json::Value::as_str)
            == Some(tuple.base_revision.as_str())
}

pub(crate) fn record_workspace_reservation_recovery(
    store: &Store,
    attempt_id: &str,
    workspace_id: &str,
    reason: &str,
    observation: serde_json::Value,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (task_id, claim_id, identity, path, base, repository_path): (
        String,
        String,
        String,
        String,
        String,
        String,
    ) = transaction.query_row(
        "SELECT a.task_id,c.id,w.repository_identity,w.path,w.base_revision,p.repository_path
             FROM attempts a JOIN tasks t ON t.id=a.task_id
             JOIN claims c ON c.attempt_id=a.id JOIN workspaces w ON w.attempt_id=a.id
             JOIN projects p ON p.id=t.project_id
             WHERE a.id=?1 AND w.id=?2",
        params![attempt_id, workspace_id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    transaction.execute(
        "UPDATE workspaces SET state='recovery_required',updated_at=?1
         WHERE id=?2 AND attempt_id=?3",
        params![now, workspace_id, attempt_id],
    )?;
    transaction.execute(
        "UPDATE claims SET state='unknown',updated_at=?1 WHERE id=?2 AND attempt_id=?3",
        params![now, claim_id, attempt_id],
    )?;
    transaction.execute(
        "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
        params![now, attempt_id],
    )?;
    transaction.execute(
        "UPDATE tasks SET attention='needs_recovery',version=version+CASE WHEN attention='needs_recovery' THEN 0 ELSE 1 END,updated_at=?1 WHERE id=?2",
        params![now, task_id],
    )?;
    let detail = serde_json::json!({
        "kind":"workspace_reservation",
        "workspace_id":workspace_id,
        "claim_id":claim_id,
        "attempt_id":attempt_id,
        "task_id":task_id,
        "repository_identity":identity,
        "repository_path":repository_path,
        "worktree_path":path,
        "base_revision":base,
        "reason":reason,
        "observed_filesystem":observation,
        "observed_at":now,
    });
    let existing: Option<String> = transaction
        .query_row(
            "SELECT id FROM recovery_records WHERE attempt_id=?1 AND state='attention_required'
               AND json_extract(detail_json,'$.kind')='workspace_reservation'
               AND json_extract(detail_json,'$.workspace_id')=?2 ORDER BY created_at DESC LIMIT 1",
            params![attempt_id, workspace_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        transaction.execute(
            "UPDATE recovery_records SET detail_json=?1,updated_at=?2 WHERE id=?3",
            params![detail.to_string(), now, id],
        )?;
    } else {
        transaction.execute(
            "INSERT INTO recovery_records(id,attempt_id,state,detail_json,created_at,updated_at)
             VALUES(?1,?2,'attention_required',?3,?4,?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                attempt_id,
                detail.to_string(),
                now
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','workspace.reservation.recovery_required','workspace',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            workspace_id,
            detail.to_string(),
            now
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn promote_workspace_reservation(
    store: &Store,
    workspace_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (task_id, lifecycle, policy_json): (String, String, String) = transaction
        .query_row(
            "SELECT a.task_id,t.lifecycle,w.policy_json FROM attempts a
             JOIN tasks t ON t.id=a.task_id JOIN workspaces w ON w.attempt_id=a.id
             WHERE a.id=?1 AND w.id=?2 AND a.status IN ('needs_recovery','workspace_reserved')
               AND t.lifecycle NOT IN ('done','cancelled')",
            params![attempt_id, workspace_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("workspace recovery tuple is stale or terminal"))?;
    let validation = serde_json::from_str::<serde_json::Value>(&policy_json)
        .ok()
        .and_then(|value| value.get("validation").and_then(serde_json::Value::as_bool))
        == Some(true)
        || lifecycle == "validation";
    let workspace_changed = transaction.execute(
        "UPDATE workspaces SET state='ready',updated_at=?1 WHERE id=?2 AND attempt_id=?3
           AND state IN ('reserved','unknown','recovery_required')",
        params![now, workspace_id, attempt_id],
    )?;
    if workspace_changed != 1 {
        transaction.commit()?;
        return Ok(false);
    }
    transaction.execute(
        "UPDATE claims SET state='running',updated_at=?1 WHERE attempt_id=?2 AND state IN ('unknown','reserved')",
        params![now, attempt_id],
    )?;
    transaction.execute(
        "UPDATE attempts SET status=?1,updated_at=?2 WHERE id=?3 AND status IN ('needs_recovery','workspace_reserved')",
        params![if validation { "held" } else { "running" }, now, attempt_id],
    )?;
    transaction.execute(
        "UPDATE tasks SET attention=?1,version=version+CASE WHEN attention=?1 THEN 0 ELSE 1 END,updated_at=?2 WHERE id=?3",
        params![if validation { "paused" } else { "none" }, now, task_id],
    )?;
    transaction.execute(
        "UPDATE recovery_records SET state='resolved_workspace_verified',resolved_at=?1,updated_at=?1
         WHERE attempt_id=?2 AND state='attention_required'
           AND json_extract(detail_json,'$.kind')='workspace_reservation'
           AND json_extract(detail_json,'$.workspace_id')=?3",
        params![now, attempt_id, workspace_id],
    )?;
    transaction.commit()?;
    Ok(true)
}
