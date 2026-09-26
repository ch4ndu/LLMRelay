use crate::domain::{
    DecisionActionBinding, DecisionControlPolicy, DecisionDisposition, DecisionEvidenceState,
    DecisionExplanation, DecisionNextAction, DecisionObservedRevision, DecisionOwner,
    DecisionOwnership, DecisionPrerequisite, DecisionSubject, OperationResult, DECISION_SCHEMA_V1,
};
use crate::store::Store;
use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
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
    compatibility_bundles: std::sync::Arc<crate::provider_compatibility::BundleSet>,
}

#[derive(Clone)]
struct ReadyTask {
    task_id: String,
    project_id: String,
    repository_identity: String,
    repository_path: String,
    base_revision: String,
    single_step: bool,
    task_version: i64,
    project_version: i64,
    queue_paused: bool,
}

#[derive(Clone, Copy)]
enum SchedulerMutation {
    None,
    NeedsInput,
    BacklogNeedsInput,
    QueuedCapacity,
    Blocked,
}

struct SchedulerEvaluation {
    task: ReadyTask,
    decision: DecisionExplanation,
    mutation: SchedulerMutation,
    eligible: bool,
}

pub(crate) struct SchedulerClaimOutcome {
    pub plan: Option<DispatchPlan>,
    pub decision: Option<DecisionExplanation>,
}

pub(crate) fn read_only_decisions(connection: &Connection) -> Result<Vec<DecisionExplanation>> {
    let (global_active, active_claims) = scheduler_capacity(connection)?;
    load_ready_tasks(connection)?
        .into_iter()
        .map(|task| {
            Ok(
                evaluate_ready_task(connection, None, global_active, &active_claims, task)?
                    .decision,
            )
        })
        .collect()
}

fn scheduler_capacity(connection: &Connection) -> Result<(i64, Vec<serde_json::Value>)> {
    let global_active = connection.query_row(
        "SELECT COUNT(*) FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping')",
        [],
        |row| row.get(0),
    )?;
    let active_claims = {
        let mut statement = connection.prepare(
            "SELECT id,task_id,attempt_id,repository_identity,state FROM claims
             WHERE state IN ('reserved','launching','running','unknown','stopping')
             ORDER BY created_at,id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(serde_json::json!({
                    "claim_id": row.get::<_, String>(0)?,
                    "task_id": row.get::<_, String>(1)?,
                    "attempt_id": row.get::<_, String>(2)?,
                    "repository_identity": row.get::<_, String>(3)?,
                    "state": row.get::<_, String>(4)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    Ok((global_active, active_claims))
}

fn load_ready_tasks(connection: &Connection) -> Result<Vec<ReadyTask>> {
    let mut statement = connection.prepare(
        "SELECT t.id,p.id,p.repository_identity,p.repository_path,p.base_revision,
                t.attention='run_next_requested',t.version,p.version,p.queue_paused
         FROM tasks t JOIN projects p ON p.id=t.project_id
         LEFT JOIN scheduler_projects sp ON sp.project_id=p.id
         WHERE t.lifecycle='ready' AND t.archived_at IS NULL
         ORDER BY COALESCE(sp.last_claimed_at,''),t.priority DESC,t.manual_order,t.created_at",
    )?;
    let tasks = statement
        .query_map([], |row| {
            Ok(ReadyTask {
                task_id: row.get(0)?,
                project_id: row.get(1)?,
                repository_identity: row.get(2)?,
                repository_path: row.get(3)?,
                base_revision: row.get(4)?,
                single_step: row.get(5)?,
                task_version: row.get(6)?,
                project_version: row.get(7)?,
                queue_paused: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tasks)
}

fn evaluate_ready_task(
    connection: &Connection,
    runtime: Option<&RuntimeAdmission>,
    global_active: i64,
    active_claims: &[serde_json::Value],
    task: ReadyTask,
) -> Result<SchedulerEvaluation> {
    let task_runtime = runtime.map(|runtime| crate::trip::CapabilityRuntime {
        hooks: runtime.hooks.clone(),
        role_socket: runtime.role_socket.clone(),
        executable: runtime.executable.clone(),
        compatibility_bundles: runtime.compatibility_bundles.clone(),
    });
    let mut prerequisites = Vec::new();
    if global_active >= 2 {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.global_capacity_full",
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"active":global_active,"limit":2,"claims":active_claims}),
            None,
            prerequisites,
            SchedulerMutation::None,
            None,
            Vec::new(),
        ));
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.global_capacity_available",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Service,
        serde_json::json!({"active":global_active,"limit":2}),
        None,
    ));
    if task.queue_paused && !task.single_step {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.queue_paused",
            DecisionEvidenceState::Pending,
            DecisionOwner::Human,
            serde_json::json!({"queue_paused":true,"run_next_requested":false}),
            None,
            prerequisites,
            SchedulerMutation::None,
            Some(DecisionNextAction {
                operation: "set_queue_paused".into(),
                enabled: true,
                owner: DecisionOwner::Human,
                binding: DecisionActionBinding {
                    project_id: Some(task.project_id.clone()),
                    expected_project_version: Some(task.project_version),
                    desired_queue_paused: Some(false),
                    ..DecisionActionBinding::default()
                },
                accounting_note: None,
            }),
            vec!["set_queue_paused".into(), "run_next".into()],
        ));
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.queue_admission",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({
            "queue_paused":task.queue_paused,
            "run_next_requested":task.single_step,
        }),
        None,
    ));
    let repository_claim: Option<(String, String, String, String, String, i64)> = connection
        .query_row(
            "SELECT claim.id,claim.task_id,claim.attempt_id,claim.state,owner.project_id,owner.version
             FROM claims claim JOIN tasks owner ON owner.id=claim.task_id
             WHERE claim.repository_identity=?1
               AND claim.state IN ('reserved','launching','running','unknown','stopping')
             ORDER BY claim.created_at,claim.id LIMIT 1",
            params![task.repository_identity],
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
        )
        .optional()?;
    if let Some((claim_id, owner_task, owner_attempt, state, owner_project, owner_version)) =
        repository_claim
    {
        let mut evaluation = blocked_scheduler_evaluation(
            &task,
            "scheduler.repository_claim_active",
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({
                "claim_id":claim_id,
                "task_id":owner_task,
                "attempt_id":owner_attempt,
                "project_id":owner_project,
                "state":state,
            }),
            None,
            prerequisites,
            SchedulerMutation::None,
            None,
            Vec::new(),
        );
        evaluation.decision.ownership.binding.claim_id = Some(claim_id);
        evaluation.decision.ownership.binding.project_id = Some(owner_project);
        evaluation.decision.ownership.binding.task_id = Some(owner_task);
        evaluation.decision.ownership.binding.attempt_id = Some(owner_attempt);
        evaluation.decision.ownership.binding.expected_task_version = Some(owner_version);
        return Ok(evaluation);
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.repository_available",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Service,
        serde_json::json!({"repository_identity":task.repository_identity.clone()}),
        None,
    ));
    let incomplete_dependency: Option<(String, String, String, i64)> = connection
        .query_row(
            "SELECT parent.id,parent.lifecycle,parent.project_id,parent.version
             FROM task_dependencies dependency
             JOIN tasks parent ON parent.id=dependency.depends_on_task_id
             WHERE dependency.task_id=?1 AND parent.lifecycle!='done'
             ORDER BY parent.id LIMIT 1",
            params![task.task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((dependency_id, lifecycle, dependency_project, dependency_version)) =
        incomplete_dependency
    {
        let mut evaluation = blocked_scheduler_evaluation(
            &task,
            "scheduler.dependency_incomplete",
            DecisionEvidenceState::Pending,
            DecisionOwner::External,
            serde_json::json!({
                "project_id":dependency_project,
                "task_id":dependency_id,
                "lifecycle":lifecycle,
            }),
            None,
            prerequisites,
            SchedulerMutation::None,
            None,
            Vec::new(),
        );
        evaluation.decision.ownership.binding.project_id = Some(dependency_project);
        evaluation.decision.ownership.binding.task_id = Some(dependency_id);
        evaluation.decision.ownership.binding.expected_task_version = Some(dependency_version);
        return Ok(evaluation);
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.dependencies_complete",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::External,
        serde_json::json!({"task_id":task.task_id.clone()}),
        None,
    ));
    if let Err(error) = crate::trip::require_project_ready(connection, &task.project_id) {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.project_readiness_stale",
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            serde_json::json!({"project_id":task.project_id.clone()}),
            Some(format!("{error:#}")),
            prerequisites,
            SchedulerMutation::NeedsInput,
            Some(DecisionNextAction {
                operation: "inspect_project".into(),
                enabled: true,
                owner: DecisionOwner::Human,
                binding: DecisionActionBinding {
                    project_id: Some(task.project_id.clone()),
                    ..DecisionActionBinding::default()
                },
                accounting_note: None,
            }),
            vec!["inspect_project".into()],
        ));
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.project_ready",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({"project_id":task.project_id.clone()}),
        None,
    ));
    if let Err(error) = crate::recipes::require_binding_current(connection, &task.task_id) {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.recipe_pin_stale",
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            serde_json::json!({"task_id":task.task_id.clone()}),
            Some(error.to_string()),
            prerequisites,
            SchedulerMutation::NeedsInput,
            Some(DecisionNextAction {
                operation: "inspect_project".into(),
                enabled: true,
                owner: DecisionOwner::Human,
                binding: DecisionActionBinding {
                    project_id: Some(task.project_id.clone()),
                    ..DecisionActionBinding::default()
                },
                accounting_note: None,
            }),
            vec!["inspect_project".into()],
        ));
    }
    let role_names = {
        let mut statement = connection
            .prepare("SELECT DISTINCT role FROM role_settings WHERE task_id=?1 ORDER BY role")?;
        let rows = statement
            .query_map(params![task.task_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if role_names.len() != 6 {
        let (reason, state, mutation) = if role_names.len() < 6 {
            (
                "scheduler.role_settings_missing",
                DecisionEvidenceState::Missing,
                SchedulerMutation::NeedsInput,
            )
        } else {
            (
                "scheduler.capability_authority_stale",
                DecisionEvidenceState::Stale,
                SchedulerMutation::Blocked,
            )
        };
        return Ok(blocked_scheduler_evaluation(
            &task,
            reason,
            state,
            DecisionOwner::Human,
            serde_json::json!({"configured":role_names.len(),"required":6}),
            None,
            prerequisites,
            mutation,
            None,
            Vec::new(),
        ));
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.role_settings_complete",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({"configured":role_names.len(),"required":6}),
        None,
    ));
    let Some(task_runtime) = task_runtime.as_ref() else {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.runtime_authority_unobserved",
            DecisionEvidenceState::Unknown,
            DecisionOwner::Service,
            serde_json::json!({"runtime_available":false}),
            None,
            prerequisites,
            SchedulerMutation::None,
            None,
            Vec::new(),
        ));
    };
    for role in &role_names {
        role.parse::<crate::domain::RoleKind>()
            .map_err(|error| anyhow!("task {} has invalid role setting: {error}", task.task_id))?;
    }
    if let Err(error) =
        crate::trip::require_task_profiles_activated(connection, &task.task_id, task_runtime)
    {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.task_profile_authority_stale",
            DecisionEvidenceState::Stale,
            DecisionOwner::Human,
            serde_json::json!({"task_id":task.task_id.clone()}),
            Some(format!("{error:#}")),
            prerequisites,
            SchedulerMutation::BacklogNeedsInput,
            None,
            Vec::new(),
        ));
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.task_profiles_current",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Human,
        serde_json::json!({"task_id":task.task_id.clone()}),
        None,
    ));
    let manager_json: String = connection.query_row(
        "SELECT config_json FROM role_settings WHERE task_id=?1 AND role='manager' ORDER BY revision DESC LIMIT 1",
        params![task.task_id],
        |row| row.get(0),
    )?;
    let manager_config: crate::domain::RoleOverride = serde_json::from_str(&manager_json)?;
    let manager_provider = manager_config.provider.to_string();
    if !crate::store::role_capacity_available(
        connection,
        &manager_provider,
        crate::domain::RoleKind::Manager,
        None,
        None,
    )? {
        return Ok(blocked_scheduler_evaluation(
            &task,
            "scheduler.manager_capacity_full",
            DecisionEvidenceState::Pending,
            DecisionOwner::Service,
            serde_json::json!({"provider":manager_provider.clone(),"role":"manager"}),
            None,
            prerequisites,
            SchedulerMutation::QueuedCapacity,
            None,
            Vec::new(),
        ));
    }
    prerequisites.push(scheduler_prerequisite(
        "scheduler.manager_capacity_available",
        DecisionEvidenceState::Satisfied,
        DecisionOwner::Service,
        serde_json::json!({"provider":manager_provider,"role":"manager"}),
        None,
    ));
    Ok(SchedulerEvaluation {
        decision: scheduler_decision(
            &task,
            "scheduler.ready_for_admission",
            DecisionDisposition::Ready,
            None,
            prerequisites,
            DecisionOwnership {
                owner: DecisionOwner::Service,
                state: "ready".into(),
                binding: scheduler_binding(&task),
            },
            Some(DecisionNextAction {
                operation: "scheduler_claim".into(),
                enabled: true,
                owner: DecisionOwner::Service,
                binding: scheduler_action_binding(&task),
                accounting_note: Some(
                    "Admission still rechecks every current predicate in its transaction.".into(),
                ),
            }),
            DecisionControlPolicy::default(),
        ),
        task,
        mutation: SchedulerMutation::None,
        eligible: true,
    })
}

fn blocked_scheduler_evaluation(
    task: &ReadyTask,
    reason_code: &str,
    state: DecisionEvidenceState,
    owner: DecisionOwner,
    evidence: serde_json::Value,
    message: Option<String>,
    mut prerequisites: Vec<DecisionPrerequisite>,
    mutation: SchedulerMutation,
    next_action: Option<DecisionNextAction>,
    allowed_controls: Vec<String>,
) -> SchedulerEvaluation {
    let blocker = scheduler_prerequisite(reason_code, state, owner, evidence, message);
    let disposition = if matches!(
        reason_code,
        "scheduler.global_capacity_full" | "scheduler.manager_capacity_full"
    ) {
        DecisionDisposition::RetryDeferred
    } else if state == DecisionEvidenceState::Pending {
        DecisionDisposition::Waiting
    } else {
        DecisionDisposition::Held
    };
    let disabled_reason_code = next_action
        .as_ref()
        .is_none_or(|action| !action.enabled)
        .then(|| reason_code.into());
    prerequisites.push(blocker.clone());
    prerequisites.push(scheduler_prerequisite(
        "scheduler.remaining_admission_prerequisites",
        DecisionEvidenceState::Unknown,
        DecisionOwner::Service,
        serde_json::json!({"evaluated":false,"short_circuit":reason_code}),
        None,
    ));
    let binding = scheduler_binding(task);
    SchedulerEvaluation {
        decision: scheduler_decision(
            task,
            reason_code,
            disposition,
            Some(blocker),
            prerequisites,
            DecisionOwnership {
                owner,
                state: "ready".into(),
                binding,
            },
            next_action,
            DecisionControlPolicy {
                allowed_controls,
                disabled_reason_code,
            },
        ),
        task: task.clone(),
        mutation,
        eligible: false,
    }
}

fn scheduler_prerequisite(
    code: &str,
    state: DecisionEvidenceState,
    owner: DecisionOwner,
    evidence: serde_json::Value,
    message: Option<String>,
) -> DecisionPrerequisite {
    DecisionPrerequisite {
        code: code.into(),
        state,
        owner,
        evidence,
        message,
    }
}

fn scheduler_binding(task: &ReadyTask) -> DecisionActionBinding {
    DecisionActionBinding {
        project_id: Some(task.project_id.clone()),
        task_id: Some(task.task_id.clone()),
        expected_task_version: Some(task.task_version),
        ..DecisionActionBinding::default()
    }
}

fn scheduler_action_binding(task: &ReadyTask) -> DecisionActionBinding {
    DecisionActionBinding {
        expected_project_version: Some(task.project_version),
        ..scheduler_binding(task)
    }
}

fn scheduler_decision(
    task: &ReadyTask,
    reason_code: &str,
    disposition: DecisionDisposition,
    primary_blocker: Option<DecisionPrerequisite>,
    prerequisites: Vec<DecisionPrerequisite>,
    ownership: DecisionOwnership,
    next_action: Option<DecisionNextAction>,
    mut control_policy: DecisionControlPolicy,
) -> DecisionExplanation {
    if crate::workflow::ordinary_control_allowed(
        "ready", "none", None, None, None, false, "run_next",
    ) && !control_policy
        .allowed_controls
        .iter()
        .any(|control| control == "run_next")
    {
        control_policy.allowed_controls.push("run_next".into());
    }
    DecisionExplanation {
        decision_schema: DECISION_SCHEMA_V1,
        reason_code: reason_code.into(),
        disposition,
        subject: DecisionSubject {
            project_id: Some(task.project_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..DecisionSubject::default()
        },
        observed_revision: DecisionObservedRevision {
            task_version: Some(task.task_version),
            project_version: matches!(
                reason_code,
                "scheduler.queue_paused" | "scheduler.ready_for_admission"
            )
            .then_some(task.project_version),
            ..DecisionObservedRevision::default()
        },
        primary_blocker,
        prerequisites,
        ownership,
        next_action,
        control_policy,
    }
}

fn audit_scheduler_decision(
    connection: &Connection,
    decision: &DecisionExplanation,
    now: &str,
) -> Result<()> {
    let task_id = decision
        .subject
        .task_id
        .as_deref()
        .ok_or_else(|| anyhow!("scheduler decision omitted its task identity"))?;
    let canonical = decision.canonical_value();
    let previous: Option<String> = connection
        .query_row(
            "SELECT detail_json FROM audit_events
             WHERE event_code='decision.explanation.changed'
               AND entity_kind='task' AND entity_id=?1
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    if previous
        .as_deref()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|value| value.get("canonical").cloned())
        .as_ref()
        == Some(&canonical)
    {
        return Ok(());
    }
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','decision.explanation.changed','task',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            task_id,
            serde_json::json!({"canonical":canonical,"explanation":decision}).to_string(),
            now,
        ],
    )?;
    Ok(())
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
            compatibility_bundles: self.store.compatibility_bundles.clone(),
        });
        self
    }

    pub fn claim_next(&self) -> Result<Option<DispatchPlan>> {
        Ok(self.claim_next_with_decision()?.plan)
    }

    pub(crate) fn claim_next_with_decision(&self) -> Result<SchedulerClaimOutcome> {
        self.store.require_execution_unheld("scheduler claims")?;
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
            compatibility_bundles: runtime.compatibility_bundles.clone(),
        };
        let (global_active, active_claims) = scheduler_capacity(&transaction)?;
        let tasks = load_ready_tasks(&transaction)?;
        let mut first_decision = None;
        let mut candidate = None;
        for task in tasks {
            let evaluation = evaluate_ready_task(
                &transaction,
                Some(runtime),
                global_active,
                &active_claims,
                task,
            )?;
            if evaluation.eligible {
                candidate = Some(evaluation.task);
                break;
            }
            if first_decision.is_none() {
                first_decision = Some(evaluation.decision.clone());
            }
            let task_id = &evaluation.task.task_id;
            match evaluation.mutation {
                SchedulerMutation::None => {}
                SchedulerMutation::NeedsInput => {
                    transaction.execute(
                        "UPDATE tasks SET attention='needs_input',updated_at=?1 WHERE id=?2",
                        params![now, task_id],
                    )?;
                }
                SchedulerMutation::BacklogNeedsInput => {
                    transaction.execute(
                        "UPDATE tasks SET lifecycle='backlog',attention='needs_input',ready_at=NULL,updated_at=?1 WHERE id=?2",
                        params![now, task_id],
                    )?;
                }
                SchedulerMutation::QueuedCapacity => {
                    transaction.execute(
                        "UPDATE tasks SET attention='queued_capacity',updated_at=?1 WHERE id=?2",
                        params![now, task_id],
                    )?;
                }
                SchedulerMutation::Blocked => {
                    transaction.execute(
                        "UPDATE tasks SET attention='blocked',updated_at=?1 WHERE id=?2",
                        params![now, task_id],
                    )?;
                }
            }
            audit_scheduler_decision(&transaction, &evaluation.decision, &now)?;
            if let Some(reason) = evaluation
                .decision
                .primary_blocker
                .as_ref()
                .and_then(|blocker| blocker.message.as_deref())
            {
                tracing::info!(task_id = %task_id, reason = %reason, "scheduler admission held");
            }
        }
        let Some(candidate) = candidate else {
            transaction.commit()?;
            return Ok(SchedulerClaimOutcome {
                plan: None,
                decision: first_decision,
            });
        };
        let ReadyTask {
            task_id,
            project_id,
            repository_identity,
            repository_path,
            base_revision,
            single_step,
            ..
        } = candidate;
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
        Ok(SchedulerClaimOutcome {
            plan: self.create_workspace(plan)?,
            decision: None,
        })
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
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let policy_json: String = transaction.query_row(
            "SELECT policy_json FROM workspaces WHERE id=?1 AND attempt_id=?2 AND state='reserved'",
            params![plan.workspace_id, plan.attempt_id],
            |row| row.get(0),
        )?;
        crate::trip::validate_materialized_policy(&policy_json)?;
        let workspace_changed = transaction.execute(
            "UPDATE workspaces SET state='ready',updated_at=?1 WHERE id=?2 AND attempt_id=?3 AND state='reserved'",
            params![now, plan.workspace_id, plan.attempt_id],
        )?;
        let claim_changed = transaction.execute(
            "UPDATE claims SET state='running',updated_at=?1 WHERE attempt_id=?2 AND state='reserved'",
            params![now, plan.attempt_id],
        )?;
        let attempt_changed = transaction.execute(
            "UPDATE attempts SET status=?1,updated_at=?2 WHERE id=?3 AND status='workspace_reserved'",
            params![
                if plan.single_step { "held" } else { "running" },
                now,
                plan.attempt_id
            ],
        )?;
        if workspace_changed != 1 || claim_changed != 1 || attempt_changed != 1 {
            bail!("workspace claim tuple changed before atomic publication")
        }
        if plan.single_step {
            let task_changed = transaction.execute(
                "UPDATE tasks SET attention='paused',version=version+1,updated_at=?1 WHERE id=?2 AND lifecycle NOT IN ('done','cancelled')",
                params![now, plan.task_id],
            )?;
            if task_changed != 1 {
                bail!("single-step task changed before atomic workspace publication")
            }
        }
        transaction.commit()?;
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
            "SELECT w.id,w.path,w.base_revision,w.repository_identity,w.attempt_id,w.policy_json
             FROM workspaces w JOIN attempts a ON a.id=w.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE (w.state IN ('reserved','unknown','recovery_required')
                    OR (w.state='ready' AND a.status='workspace_reserved'))
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
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        drop(connection);
        let mut recovered = Vec::new();
        for (workspace, path, base, identity, attempt, policy_json) in rows {
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
            let policy_validation = crate::trip::validate_materialized_policy(&policy_json);
            if valid && policy_validation.is_ok() {
                promote_workspace_reservation(&self.store, &workspace, &attempt)?;
                recovered.push(attempt);
            } else {
                let policy_error = policy_validation.err().map(|error| format!("{error:#}"));
                record_workspace_reservation_recovery(
                    &self.store,
                    &attempt,
                    &workspace,
                    "startup reconciliation could not prove the recorded workspace tuple",
                    serde_json::json!({"filesystem":observed,"policy_error":policy_error}),
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
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    record_workspace_reservation_recovery_in(
        &transaction,
        attempt_id,
        None,
        workspace_id,
        reason,
        observation,
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn record_workspace_reservation_recovery_in(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    expected_task_id: Option<&str>,
    workspace_id: &str,
    reason: &str,
    observation: serde_json::Value,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
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
             WHERE a.id=?1 AND w.id=?2 AND (?3 IS NULL OR a.task_id=?3)",
        params![attempt_id, workspace_id, expected_task_id],
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
    let (task_id, lifecycle, policy_json, workspace_state, attempt_status): (
        String,
        String,
        String,
        String,
        String,
    ) = transaction
        .query_row(
            "SELECT a.task_id,t.lifecycle,w.policy_json,w.state,a.status FROM attempts a
             JOIN tasks t ON t.id=a.task_id JOIN workspaces w ON w.attempt_id=a.id
             WHERE a.id=?1 AND w.id=?2
               AND ((a.status IN ('needs_recovery','workspace_reserved')
                       AND w.state IN ('reserved','unknown','recovery_required'))
                    OR (a.status='workspace_reserved' AND w.state='ready'))
               AND t.lifecycle NOT IN ('done','cancelled')",
            params![attempt_id, workspace_id],
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
        .optional()?
        .ok_or_else(|| anyhow!("workspace recovery tuple is stale or terminal"))?;
    crate::trip::validate_materialized_policy(&policy_json)?;
    let validation = serde_json::from_str::<serde_json::Value>(&policy_json)
        .ok()
        .and_then(|value| value.get("validation").and_then(serde_json::Value::as_bool))
        == Some(true)
        || lifecycle == "validation";
    let workspace_changed = transaction.execute(
        "UPDATE workspaces SET state='ready',updated_at=?1 WHERE id=?2 AND attempt_id=?3
           AND (state IN ('reserved','unknown','recovery_required')
                OR (state='ready' AND ?4='ready' AND ?5='workspace_reserved'))",
        params![
            now,
            workspace_id,
            attempt_id,
            workspace_state,
            attempt_status
        ],
    )?;
    if workspace_changed != 1 {
        transaction.commit()?;
        return Ok(false);
    }
    let claim_changed = transaction.execute(
        "UPDATE claims SET state='running',updated_at=?1 WHERE attempt_id=?2
           AND (state IN ('unknown','reserved')
                OR (state='running' AND ?3='ready' AND ?4='workspace_reserved'))",
        params![now, attempt_id, workspace_state, attempt_status],
    )?;
    let attempt_changed = transaction.execute(
        "UPDATE attempts SET status=?1,updated_at=?2 WHERE id=?3 AND status IN ('needs_recovery','workspace_reserved')",
        params![if validation { "held" } else { "running" }, now, attempt_id],
    )?;
    let task_changed = transaction.execute(
        "UPDATE tasks SET attention=?1,version=version+CASE WHEN attention=?1 THEN 0 ELSE 1 END,updated_at=?2 WHERE id=?3",
        params![if validation { "paused" } else { "none" }, now, task_id],
    )?;
    if claim_changed != 1 || attempt_changed != 1 || task_changed != 1 {
        bail!("workspace recovery tuple changed before atomic promotion")
    }
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
