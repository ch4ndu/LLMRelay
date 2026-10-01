use crate::auth;
use crate::checks::CheckService;
use crate::config::InstancePaths;
use crate::coordinator::StepEffect;
use crate::diagnostics::DiagnosticSink;
use crate::domain::{
    HumanCommand, LaunchConfig, OperationResult, ProcessIdentity, RestartAdmissionV1,
    RestartBatchMembership, RestartBatchV1, RestartCandidateResult, RoleContext, TripHumanAction,
    ValidationLaunchRequest, ValidationLaunchResult, WorkflowValidationRequest,
    ROLE_RESULT_REPORT_CONTRACT,
};
use crate::providers::{self, HookAssets};
use crate::review::ReviewService;
use crate::roles::RoleService;
use crate::scheduler::Scheduler;
use crate::store::{
    json_hash, BrowserLaunchReceipt, BrowserLaunchReservation, PreparedResumeIdentity,
    RestartAdmissionBinding, RoleResumeCapacityError, Store,
};
use crate::supervisor::{ProcessInventory, SessionInventory, SpawnFailure, Supervisor};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{OptionalExtension, Transaction};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[cfg(test)]
thread_local! {
    static TEST_INTERRUPT_DRAIN_AFTER_CAPTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone)]
pub struct Application {
    pub paths: InstancePaths,
    pub store: Store,
    pub supervisor: Supervisor,
    pub hooks: HookAssets,
    pub diagnostics: DiagnosticSink,
    pub scheduler: Scheduler,
    pub reviews: ReviewService,
    pub roles: RoleService,
    pub checks: CheckService,
    executable: PathBuf,
    role_tokens: Arc<RwLock<HashMap<String, String>>>,
    dispatch_enabled: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    coordinator_lock: Arc<Mutex<()>>,
    intake_lock: Arc<Mutex<()>>,
    rework_lock: Arc<Mutex<()>>,
    permission_boot_id: Arc<String>,
    permission_notify: Arc<tokio::sync::Notify>,
    pub(crate) blocking_operations: Arc<tokio::sync::Semaphore>,
    cmux_host_ancestry: Option<Arc<crate::supervisor::CmuxHostAncestry>>,
    synthetic_dispatch_for_tests: bool,
}

enum BrowserLaunchDispatch {
    Launched(ValidationLaunchResult),
    Existing(serde_json::Value),
}

const MAX_RESTART_SELECTION: usize = 200;
const MAX_RESTART_BATCH: usize = 4;
const MAX_RESTART_REPLACEMENT_FAILURES: u8 = 3;
const MAX_BLOCKING_OPERATIONS: usize = 16;
const HOST_RESTART_RESUME_NOTICE: &str = "LLMRelay restarted while this session was running and interrupted its previous turn. Before repeating any command, edit or report that may already have been in flight, check its actual effect; do not assume it did or did not happen. The task instructions below are unchanged.";

#[derive(Clone, Debug)]
struct RestartAdmissionTicket {
    session_id: String,
    attempt_id: String,
    task_id: String,
    role_generation_id: String,
    expected_task_version: i64,
    admission_id: String,
    expected_resume_ordinal: u32,
    prior_transcript_epoch: String,
    automatic: bool,
    receipt: Option<(String, String)>,
}

impl RestartAdmissionTicket {
    fn binding(&self) -> RestartAdmissionBinding<'_> {
        RestartAdmissionBinding {
            admission_id: &self.admission_id,
            attempt_id: &self.attempt_id,
            task_id: &self.task_id,
            role_generation_id: &self.role_generation_id,
            expected_task_version: self.expected_task_version,
            prior_transcript_epoch: &self.prior_transcript_epoch,
            expected_resume_ordinal: self.expected_resume_ordinal,
        }
    }
}

enum RestartAdmissionStart {
    Started(RestartAdmissionTicket),
    Finished(serde_json::Value),
}

fn lane_source_binding_schema() -> serde_json::Value {
    serde_json::json!({
        "anyOf":[
            {"type":"string","pattern":"^[0-9A-Fa-f]{64}$","description":"legacy regular-file SHA-256 identity"},
            {"type":"object","additionalProperties":false,"required":["kind"],"properties":{"kind":{"const":"missing"}}},
            {"type":"object","additionalProperties":false,"required":["kind","sha256"],"properties":{"kind":{"const":"file"},"sha256":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"}}},
            {"type":"object","additionalProperties":false,"required":["kind","tree_sha256","entry_count"],"properties":{"kind":{"const":"directory"},"tree_sha256":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"},"entry_count":{"type":"integer","minimum":0,"maximum":4096}}}
        ]
    })
}

fn manager_command_schemas() -> serde_json::Value {
    serde_json::json!({
        "record_explorer_decision":{
            "type":"object",
            "required":["stage","trigger","activated","census","limits"],
            "properties":{
                "stage":{"type":"string","enum":["planning","rescue","final"]},
                "trigger":{"type":"string"},
                "activated":{"type":"boolean"},
                "census":{"type":"object"},
                "limits":{"type":"object","required":["max_words"],"properties":{
                    "max_words":{"type":"integer","minimum":1,"maximum":800},
                    "question":{"type":"string","minLength":1}
                }}
            },
            "stage_trigger_catalog":{
                "planning":["multi_module","multi_platform","contract_change","repository_wide","edit_set_over_eight","large_file_refactor","ownership_unresolved","not_invoked"],
                "rescue":["repeated_gate_failure","unplanned_scope","outside_edit_set","conflicting_review_evidence","owner_unresolved","not_invoked"],
                "final":["repository_wide","three_or_more_modules","source_evidence_conflict","not_invoked"]
            },
            "constraints":[
                "the serialized Explorer decision input must be at most 131072 bytes",
                "trigger must come from the selected stage catalog",
                "activated must equal whether trigger is not not_invoked",
                "an activated decision requires a nonblank limits.question",
                "the stage must match the current workflow phase"
            ]
        },
        "structured_plan":{
            "location":"role report metadata.structured_plan when outcome is plan_ready",
            "type":"object",
            "required":["outcomes_scope","classification","ownership","acceptance_criteria","test_policy","verification_matrix","documentation","restrictions","explorer_disposition","unresolved_decisions","config_revision_id","conformance"],
            "properties":{
                "outcomes_scope":{"type":"object"},
                "classification":{"type":"string","enum":["bounded","broad","program_sized"]},
                "ownership":{"type":"object","properties":{"lanes":{"type":"array","minItems":2,"items":{"type":"object",
                    "required":["lane_key","owned_paths","shared_paths","dependencies"],
                    "properties":{
                        "lane_key":{"type":"string","pattern":"^[a-z0-9_]+$"},
                        "owned_paths":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"string"}},
                        "shared_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                        "protected_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                        "dependencies":{"type":"array","uniqueItems":true,"items":{"type":"string"}}
                    }
                }}}},
                "acceptance_criteria":{"type":"array"},
                "test_policy":{"type":"object","required":["coverage"],"properties":{"coverage":{"type":"string"}}},
                "verification_matrix":{"type":"array","items":{"type":"object","required":["check_id"],"properties":{"check_id":{"type":"string","minLength":1}}}},
                "documentation":{"type":"object"},
                "restrictions":{"type":"array"},
                "explorer_disposition":{"type":"object","required":["decision_id","activated"],"properties":{"decision_id":{"type":"string","minLength":1},"activated":{"type":"boolean"}}},
                "unresolved_decisions":{"type":"array","maxItems":0},
                "config_revision_id":{"type":"string","minLength":1},
                "conformance":{"type":"object"}
            },
            "report_siblings":{
                "plan":{"type":"string","minLength":1,"maxUtf8Bytes":262144}
            },
            "constraints":[
                "metadata.plan must be nonblank after trimming and at most 262144 UTF-8 bytes",
                "metadata.structured_plan serialized JSON must be at most 262144 bytes",
                "metadata.structured_plan object keys must not recursively contain the case-insensitive substrings credential, secret, token, hidden_reasoning, or chain_of_thought",
                "acceptance_criteria must exactly equal task.acceptance_criteria in order",
                "test_policy.coverage must equal project_policy.testing.coverage",
                "config_revision_id must equal project_policy.config_revision_id",
                "verification_matrix check IDs as a set must exactly equal selected_checks.check_ids at selected_checks.revision",
                "explorer_disposition must bind the current planning decision and its actual activation/evidence state",
                "ownership.lanes is optional for the default single-lane flow; when present it requires at least two uniquely named explicit lanes",
                "each ownership lane requires unique normalized owned_paths, shared_paths, and dependencies; protected_paths is optional but must contain unique normalized paths when present",
                "each owned path must be non-overlapping with every other owned, shared, or protected path in the same or another lane; shared and protected scopes may overlap one another",
                "dependencies must uniquely name other reviewed lanes, must not name the same lane, and must be acyclic",
                "no unresolved decision may remain"
            ]
        },
        "configure_lanes":{
            "type":"object","required":["lanes"],
            "properties":{"lanes":{"type":"array","minItems":2,"items":{"type":"object",
                "required":["lane_key","owned_paths","shared_paths","protected_paths","dependencies"],
                "properties":{
                    "lane_key":{"type":"string","pattern":"^[a-z0-9_]+$"},
                    "owned_paths":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"string"}},
                    "shared_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                    "protected_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                    "dependencies":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                    "source_hashes":{"type":"object","minProperties":1,"maxProperties":4096,"additionalProperties":lane_source_binding_schema()},
                    "frozen_seams_hash":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"}
                }
            }}},
            "constraints":["the serialized lane configuration input must be at most 131072 bytes","requires the exact approved structured plan and human implementation authorization","lane_key, owned_paths, shared_paths, protected_paths, and dependencies must exactly match approved structured plan ownership; omitted reviewed protected_paths is admitted as an empty array","all paths must be normalized relative paths; each owned path must be non-overlapping with every other owned, shared, or protected path in the same or another lane; shared and protected scopes may overlap one another","normally omit both source_hashes and frozen_seams_hash; the authenticated service computes and freezes typed current bindings for every exact reviewed owned, shared, and protected scope","if either dynamic field is supplied then both are required; explicit source bindings must exactly cover the reviewed scopes and each must match current missing, regular-file, or bounded directory state","missing, regular-file, and directory identities are distinct; directory identities bind contained membership, entry types, and file hashes without projecting target contents","frozen_seams_hash is SHA-256 of the lane shared_paths and their current shared source bindings","initial lane dispatch rechecks every admitted source binding and exact scope coverage before creating authority; retained resume and yield use the already-admitted lane authority","dependencies must uniquely name other reviewed lanes, must not name the same lane, and must be acyclic","lane admission is immutable after the first successful configuration"]
        },
        "request_integration":{
            "type":"object","required":["capsule"],
            "properties":{"capsule":{"type":"object","required":["ordered_lanes","merge_strategy","verification_boundary"],"properties":{
                "ordered_lanes":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                "merge_strategy":{"anyOf":[{"type":"string","minLength":1},{"type":"object","minProperties":1}]},
                "verification_boundary":{"anyOf":[{"type":"string","minLength":1},{"type":"object","minProperties":1}]}
            }}},
            "constraints":["the capsule serialized JSON must be at most 131072 bytes","capsule object keys must not recursively contain the case-insensitive substrings credential, secret, token, hidden_reasoning, or chain_of_thought","merge_strategy and verification_boundary must each be either a string that is nonblank after trimming or a nonempty object","every configured required lane must already be yielded","ordered_lanes must name every configured required lane exactly once","all lane writers must be quiescent","the service verifies the actual worktree scope before recording the request"]
        },
        "select_checks":{
            "type":"object","required":["check_ids"],
            "properties":{"check_ids":{"type":"array","uniqueItems":true,"items":{"type":"string"}}},
            "constraints":["every ID must be present in verification_catalog.checks for its exact activated config revision","selection is planning-only and each accepted call creates the next immutable revision","an empty selection remains an explicit revision and blocks verified completion"]
        },
        "submit_conformance":{
            "type":"object",
            "required":["candidate_hash","config_hash","acceptance","ownership","documentation","test_policy","readability"],
            "properties":{
                "candidate_hash":{"type":"string","minLength":1},
                "config_hash":{"type":"string","minLength":1},
                "acceptance":{"type":"array","items":{"type":"object","required":["criterion","evidence"],"properties":{"criterion":{"type":"string"},"evidence":{"type":"array","minItems":1,"maxItems":64,"items":{"anyOf":[{"type":"string","minLength":1,"maxLength":4096,"pattern":"\\S"},{"type":"object","minProperties":1}]}}}}},
                "ownership":{"type":"object","minProperties":1,"maxProperties":32},
                "documentation":{"type":"object","minProperties":1,"maxProperties":32},
                "test_policy":{"type":"object","minProperties":1,"maxProperties":32},
                "readability":{"type":"object","minProperties":1,"maxProperties":32}
            },
            "constraints":["the serialized conformance input must be at most 262144 bytes","ownership, documentation, test_policy, and readability must each serialize to at most 16384 bytes and contain only nonblank keys with meaningful nonempty values","candidate_hash and config_hash must match the current attempt and activated project configuration","acceptance rows must exactly cover task.acceptance_criteria in order; each row requires 1 to 64 evidence items, each either a nonblank string of at most 4096 characters or a meaningful nonempty object of at most 16384 bytes","all required lanes must have yielded and all implementer writers must be quiescent"]
        }
    })
}

fn lane_yield_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "required":["source_hashes","changed_paths","agent_claimed_output_hash"],
        "properties":{
            "source_hashes":{"type":"object","maxProperties":4096,"additionalProperties":lane_source_binding_schema()},
            "changed_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
            "agent_claimed_output_hash":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"}
        },
        "constraints":["the serialized lane-yield input must be at most 131072 bytes","the source_hashes object must exactly equal the admitted lane source hashes","changed paths must remain within owned paths and outside protected paths","only the current active generation for the exact lane may yield"]
    })
}

impl Application {
    pub fn new(paths: InstancePaths, store: Store, executable: PathBuf) -> Result<Self> {
        Self::new_inner(paths, store, executable, false, None)
    }

    #[doc(hidden)]
    pub fn new_with_synthetic_dispatch_for_tests(
        paths: InstancePaths,
        store: Store,
        executable: PathBuf,
        hooks: HookAssets,
    ) -> Result<Self> {
        if store.compatibility_bundles.is_synthetic() {
            bail!("use the synthetic compatibility constructor for a synthetic Store")
        }
        Self::new_inner(paths, store, executable, true, Some(hooks))
    }

    #[doc(hidden)]
    pub fn new_with_synthetic_compatibility_for_tests(
        paths: InstancePaths,
        store: Store,
        executable: PathBuf,
        hooks: HookAssets,
    ) -> Result<Self> {
        if !store.compatibility_bundles.is_synthetic() {
            bail!("synthetic compatibility constructor requires a synthetic Store")
        }
        Self::new_inner(paths, store, executable, true, Some(hooks))
    }

    #[cfg(test)]
    pub(crate) fn set_automatic_cmux_routing_for_tests(&mut self, permitted: bool) -> Result<()> {
        self.cmux_host_ancestry = if permitted {
            Some(Arc::new(
                crate::supervisor::current_process_cmux_ancestry_for_tests()?,
            ))
        } else {
            None
        };
        Ok(())
    }

    fn new_inner(
        paths: InstancePaths,
        store: Store,
        executable: PathBuf,
        synthetic_dispatch_for_tests: bool,
        synthetic_hooks: Option<HookAssets>,
    ) -> Result<Self> {
        if store.compatibility_bundles.is_synthetic() && !synthetic_dispatch_for_tests {
            bail!("production application cannot use a synthetic compatibility Store")
        }
        let permission_boot_id = Arc::new(uuid::Uuid::new_v4().to_string());
        crate::permissions::expire_prior_boot(&store, permission_boot_id.as_str())?;
        let hooks = match synthetic_hooks {
            Some(hooks) => hooks,
            None => providers::install_hook_assets(&paths, &executable)?,
        };
        let diagnostics = DiagnosticSink::new(paths.logs.clone())?;
        let supervisor = Supervisor::new(store.clone(), paths.transcripts.clone());
        let cmux_host_ancestry = crate::supervisor::capture_cmux_host_ancestry().map(Arc::new);
        let scheduler = Scheduler::new(store.clone(), paths.artifacts.clone()).with_runtime(
            hooks.clone(),
            paths.role_socket.clone(),
            executable.clone(),
        );
        let reviews = ReviewService::new(store.clone(), paths.artifacts.clone());
        let roles = RoleService::new(store.clone(), supervisor.clone()).with_runtime(
            hooks.clone(),
            paths.role_socket.clone(),
            executable.clone(),
        );
        let checks = CheckService::new(store.clone(), supervisor.clone(), paths.artifacts.clone());
        let dispatch_enabled = !crate::database::hold_active(&store)?;
        Ok(Self {
            paths,
            store,
            supervisor,
            hooks,
            diagnostics,
            scheduler,
            reviews,
            roles,
            checks,
            executable,
            role_tokens: Arc::new(RwLock::new(HashMap::new())),
            dispatch_enabled: Arc::new(AtomicBool::new(dispatch_enabled)),
            draining: Arc::new(AtomicBool::new(false)),
            coordinator_lock: Arc::new(Mutex::new(())),
            intake_lock: Arc::new(Mutex::new(())),
            rework_lock: Arc::new(Mutex::new(())),
            permission_boot_id,
            permission_notify: Arc::new(tokio::sync::Notify::new()),
            blocking_operations: Arc::new(tokio::sync::Semaphore::new(MAX_BLOCKING_OPERATIONS)),
            cmux_host_ancestry,
            synthetic_dispatch_for_tests,
        })
    }

    #[doc(hidden)]
    pub fn synthetic_role_context_for_tests(&self, session_id: &str) -> Result<RoleContext> {
        if !self.synthetic_dispatch_for_tests {
            bail!("synthetic role context is unavailable on a production application")
        }
        let token = self
            .role_tokens
            .read()
            .map_err(|_| anyhow!("role token map poisoned"))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("synthetic dispatched session has no role token"))?;
        self.store.role_context(&token)
    }

    pub(crate) fn interrupt_completed_role(&self, session_id: &str) -> Result<()> {
        if self.synthetic_dispatch_for_tests {
            self.store.mark_interrupt_requested(session_id)
        } else {
            self.supervisor.interrupt(session_id)
        }
    }

    pub(crate) fn manager_control_native_idle_ready(&self, session_id: &str) -> Result<bool> {
        // Synthetic dispatch has no provider process to inventory. Its tests still
        // require the durable hook/authority boundary before reaching this seam.
        if self.synthetic_dispatch_for_tests {
            return Ok(true);
        }
        self.supervisor.native_idle_ready(session_id)
    }

    pub(crate) fn retained_session_native_idle_ready(&self, session_id: &str) -> Result<bool> {
        self.manager_control_native_idle_ready(session_id)
    }

    pub(crate) fn request_setup_retained_first_turn_stop(
        &self,
        receipt: &crate::store::SetupRetainedFirstTurnStopReceipt,
    ) -> Result<crate::supervisor::InterruptOutcome> {
        if self.synthetic_dispatch_for_tests {
            return Ok(
                match self.store.claim_setup_retained_first_turn_stop(receipt)? {
                    crate::store::ManagerServiceStopClaim::Claimed => {
                        crate::supervisor::InterruptOutcome::Requested
                    }
                    crate::store::ManagerServiceStopClaim::AlreadyRequested => {
                        crate::supervisor::InterruptOutcome::AlreadyRequested
                    }
                    crate::store::ManagerServiceStopClaim::Stale => {
                        crate::supervisor::InterruptOutcome::Stale
                    }
                },
            );
        }
        self.supervisor
            .request_setup_retained_first_turn_stop(receipt)
    }

    #[doc(hidden)]
    pub fn synthetic_codex_stop_idle_interleaving_for_tests<F>(
        &self,
        between_selection_and_claim: F,
    ) -> Result<bool>
    where
        F: FnOnce(),
    {
        if !self.synthetic_dispatch_for_tests {
            bail!("synthetic Codex Stop reconciliation seam is unavailable in production")
        }
        let receipt = {
            let connection = self.store.lock()?;
            crate::store::eligible_codex_stop_idle_reconciliation(&connection, None)?
                .ok_or_else(|| anyhow!("no exact Codex Stop reconciliation receipt is eligible"))?
        };
        between_selection_and_claim();
        self.store.mark_codex_stop_idle_candidate(&receipt)
    }

    #[doc(hidden)]
    pub fn synthetic_codex_stop_idle_native_readiness_for_tests(
        &self,
        native_idle_ready: bool,
    ) -> Result<bool> {
        if !self.synthetic_dispatch_for_tests {
            bail!("synthetic Codex Stop reconciliation seam is unavailable in production")
        }
        let receipt = {
            let connection = self.store.lock()?;
            crate::store::eligible_codex_stop_idle_reconciliation(&connection, None)?
                .ok_or_else(|| anyhow!("no exact Codex Stop reconciliation receipt is eligible"))?
        };
        if !native_idle_ready {
            return Ok(false);
        }
        self.store.mark_codex_stop_idle_candidate(&receipt)
    }

    pub(crate) fn request_manager_service_stop(
        &self,
        attempt_id: &str,
        phase: &str,
        receipt: &crate::store::ManagerServiceStopReceipt,
    ) -> Result<crate::supervisor::InterruptOutcome> {
        if self.synthetic_dispatch_for_tests {
            return Ok(
                match self
                    .store
                    .claim_manager_service_stop(attempt_id, phase, receipt)?
                {
                    crate::store::ManagerServiceStopClaim::Claimed => {
                        crate::supervisor::InterruptOutcome::Requested
                    }
                    crate::store::ManagerServiceStopClaim::AlreadyRequested => {
                        crate::supervisor::InterruptOutcome::AlreadyRequested
                    }
                    crate::store::ManagerServiceStopClaim::Stale => {
                        crate::supervisor::InterruptOutcome::Stale
                    }
                },
            );
        }
        self.supervisor
            .request_manager_service_stop(attempt_id, phase, receipt)
    }

    #[doc(hidden)]
    pub fn synthetic_manager_service_stop_interleaving_for_tests<F>(
        &self,
        attempt_id: &str,
        phase: &str,
        between_selection_and_claim: F,
    ) -> Result<serde_json::Value>
    where
        F: FnOnce(),
    {
        if !self.synthetic_dispatch_for_tests {
            bail!("synthetic manager service-stop seam is unavailable in production")
        }
        let receipt = {
            let connection = self.store.lock()?;
            crate::store::eligible_manager_service_stop(&connection, attempt_id, phase)?
                .ok_or_else(|| anyhow!("no exact manager service-stop receipt is eligible"))?
        };
        between_selection_and_claim();
        let outcome = self.request_manager_service_stop(attempt_id, phase, &receipt)?;
        Ok(serde_json::json!({
            "outcome":match outcome {
                crate::supervisor::InterruptOutcome::Requested => "requested",
                crate::supervisor::InterruptOutcome::AlreadyRequested => "already_requested",
                crate::supervisor::InterruptOutcome::Stale => "stale",
            },
            "session_id":receipt.session_id,
            "role_generation_id":receipt.generation_id,
            "credential_id":receipt.credential_id,
            "transcript_epoch":receipt.transcript_epoch,
            "stop_event_rowid":receipt.stop_rowid
        }))
    }

    pub async fn permission_request(
        &self,
        context: &crate::domain::RoleContext,
        credential: &str,
        payload: &serde_json::Value,
        connection_nonce: &str,
    ) -> Result<serde_json::Value> {
        match crate::permissions::begin_request(
            &self.store,
            context,
            payload,
            self.permission_boot_id.as_str(),
            connection_nonce,
        )? {
            crate::permissions::BridgeStart::Immediate(response) => Ok(response),
            crate::permissions::BridgeStart::Pending { request_id } => loop {
                if let Some(response) =
                    crate::permissions::native_resolution_bridge_response(&self.store, &request_id)?
                {
                    return Ok(response);
                }
                let current_context = match self.store.role_context(credential) {
                    Ok(current)
                        if current.project_id == context.project_id
                            && current.task_id == context.task_id
                            && current.attempt_id == context.attempt_id
                            && current.role_generation_id == context.role_generation_id
                            && current.session_id == context.session_id
                            && current.credential_id == context.credential_id
                            && current.transcript_epoch == context.transcript_epoch
                            && current.role == context.role
                            && current.provider == context.provider
                            && current.configuration_revision == context.configuration_revision
                            && current.lane_id == context.lane_id
                            && current.permissions == context.permissions =>
                    {
                        current
                    }
                    Ok(_) => {
                        crate::permissions::reserve_connection_denial(
                            &self.store,
                            connection_nonce,
                            "role credential context changed before permission response consumption",
                        )?;
                        return Ok(crate::permissions::provider_response(
                            false,
                            Some("Role authority changed before the permission decision was delivered"),
                        ));
                    }
                    Err(_) => {
                        crate::permissions::reserve_connection_denial(
                            &self.store,
                            connection_nonce,
                            "role credential was revoked or became stale before permission response consumption",
                        )?;
                        return Ok(crate::permissions::provider_response(
                            false,
                            Some("Role authority is no longer current; no action was authorized"),
                        ));
                    }
                };
                if let Some(response) = crate::permissions::consume_ready_response(
                    &self.store,
                    &request_id,
                    self.permission_boot_id.as_str(),
                    connection_nonce,
                    credential,
                    &current_context,
                )? {
                    return Ok(response);
                }
                tokio::select! {
                    _ = self.permission_notify.notified() => {}
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
                }
            },
        }
    }

    pub fn dispatch_enabled(&self) -> bool {
        self.dispatch_enabled.load(Ordering::SeqCst)
    }

    pub fn service_boot_id(&self) -> &str {
        self.permission_boot_id.as_str()
    }

    /// Automatic cmux routing is a host-start property.  Socket availability
    /// and inherited environment are deliberately not considered here.
    pub(crate) fn automatic_cmux_routing_permitted(&self) -> bool {
        self.cmux_executable_path().is_some()
    }

    pub(crate) fn cmux_executable_path(&self) -> Option<PathBuf> {
        self.cmux_host_ancestry
            .as_ref()
            .and_then(|identity| identity.verified_executable())
    }

    pub fn executable_path(&self) -> &Path {
        &self.executable
    }

    pub fn begin_drain(&self) -> Result<serde_json::Value> {
        self.dispatch_enabled.store(false, Ordering::SeqCst);
        // A fire holding intake_lock commits before drain activation; later fires see the disabled gate.
        {
            let _intake = self
                .intake_lock
                .lock()
                .map_err(|_| anyhow!("intake mutex is poisoned"))?;
            self.draining.store(true, Ordering::SeqCst);
        }
        let _guard = self
            .coordinator_lock
            .lock()
            .map_err(|_| anyhow!("coordinator mutex is poisoned"))?;
        let reconciled = self.supervisor.reconcile_inventory()?;
        let desired_running = crate::recovery::capture_desired_running_before_drain(&self.store)?;
        #[cfg(test)]
        if TEST_INTERRUPT_DRAIN_AFTER_CAPTURE.with(|armed| armed.replace(false)) {
            bail!(
                "injected interruption after desired-running capture: {}",
                desired_running.len()
            )
        }
        let inventory = match reconciled {
            ProcessInventory::Unavailable(error) => SessionInventory::Unknown {
                attached: self.supervisor.attached_session_ids()?,
                reason: format!("{error:#}"),
            },
            ProcessInventory::Observed => self.supervisor.session_inventory()?,
        };
        let mut interrupted = Vec::new();
        let mut already_interrupt_requested = Vec::new();
        let mut unknown = Vec::new();
        let (active, interrupted_checks, process_inventory) = match inventory {
            SessionInventory::Observed(active) => {
                for session in &active {
                    match self.supervisor.interrupt_once(session) {
                        Ok(crate::supervisor::InterruptOutcome::Requested) => {
                            interrupted.push(session.clone())
                        }
                        Ok(crate::supervisor::InterruptOutcome::AlreadyRequested) => {
                            already_interrupt_requested.push(session.clone())
                        }
                        Ok(crate::supervisor::InterruptOutcome::Stale) => unknown.push(
                            serde_json::json!({"session_id":session,"error":"interrupt receipt became stale"}),
                        ),
                        Err(error) => unknown.push(
                            serde_json::json!({"session_id":session,"error":format!("{error:#}")}),
                        ),
                    }
                }
                let interrupted_checks = self.checks.interrupt_all()?;
                (
                    active,
                    interrupted_checks,
                    serde_json::json!({"state":"observed"}),
                )
            }
            // No signal is sent without an exact current inventory, and the
            // unknown entry keeps every caller from reading this as quiescent.
            SessionInventory::Unknown { attached, reason } => {
                unknown.extend(
                    attached
                        .iter()
                        .map(|session| serde_json::json!({"session_id":session,"error":reason})),
                );
                unknown.push(serde_json::json!({"process_inventory":"unknown","error":reason}));
                (
                    attached,
                    Vec::new(),
                    serde_json::json!({"state":"unknown","reason":reason}),
                )
            }
        };
        let active_checks = self.checks.active_status()?;
        Ok(
            serde_json::json!({"draining":true,"new_dispatch":false,"desired_running_captured":desired_running,"active":active,"active_checks":active_checks,"interrupt_requested":interrupted,"already_interrupt_requested":already_interrupt_requested,"check_interrupt_requested":interrupted_checks,"unknown":unknown,"process_inventory":process_inventory}),
        )
    }

    pub fn drain_status(&self) -> Result<serde_json::Value> {
        let (observed, active, inventory) = match self.supervisor.session_inventory()? {
            SessionInventory::Observed(active) => {
                (true, active, serde_json::json!({"state":"observed"}))
            }
            SessionInventory::Unknown { attached, reason } => (
                false,
                attached,
                serde_json::json!({"state":"unknown","reason":reason}),
            ),
        };
        let checks = self.checks.active_status()?;
        let quiescent = observed && active.is_empty() && checks.is_empty();
        Ok(
            serde_json::json!({"draining":self.draining.load(Ordering::SeqCst),"new_dispatch":self.dispatch_enabled(),"active":active,"active_checks":checks,"process_inventory":inventory,"quiescent":quiescent}),
        )
    }

    pub fn restart_preview(&self) -> Result<crate::domain::RestartPreview> {
        crate::recovery::restart_preview(
            &self.store,
            self.dispatch_enabled.load(Ordering::SeqCst),
            self.draining.load(Ordering::SeqCst),
        )
    }

    pub fn coordinator_tick(&self) -> Result<serde_json::Value> {
        self.store
            .require_execution_unheld("coordinator execution")?;
        let _guard = self
            .coordinator_lock
            .lock()
            .map_err(|_| anyhow!("coordinator mutex is poisoned"))?;
        crate::coordinator::tick(self)
    }

    pub fn scheduled_intake_tick(
        &self,
        now: chrono::DateTime<Utc>,
        service_started_at: chrono::DateTime<Utc>,
    ) -> Result<usize> {
        if !self.dispatch_enabled() || self.draining.load(Ordering::SeqCst) {
            return Ok(0);
        }
        if self.store.restore_hold()?.is_some() {
            return Ok(0);
        }
        crate::recipes::scheduled_intake_tick_gated(
            &self.store,
            now,
            service_started_at,
            &self.intake_lock,
            &self.dispatch_enabled,
            &self.draining,
        )
    }

    pub fn resume_restart_sessions(
        &self,
        operation_id: &str,
        selected: Option<&[String]>,
    ) -> Result<serde_json::Value> {
        let _guard = self
            .coordinator_lock
            .lock()
            .map_err(|_| anyhow!("coordinator mutex is poisoned"))?;
        self.resume_restart_sessions_inner(operation_id, selected)
    }

    pub(crate) fn auto_resume_one(&self) -> Result<Option<serde_json::Value>> {
        let enabled: bool = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT auto_resume_eligible FROM instance_settings WHERE singleton=1",
                [],
                |row| row.get(0),
            )?
        };
        let due = Utc::now();
        // A held attempt gets no automatic restart, while a person's queued
        // restart stops only on its own unresolved failure or provider hold.
        let candidates = {
            let connection = self.store.lock()?;
            let mut statement = connection.prepare(&format!(
                "SELECT rc.session_id,rc.requested_by,rc.state,rc.result_json,rc.attempt_id,
                        NOT {},
                        EXISTS(SELECT 1 FROM recovery_records failure
                          WHERE failure.attempt_id=rc.attempt_id
                            AND failure.state='attention_required'
                            AND json_extract(failure.detail_json,'$.kind')='coordinator_failure'
                            AND json_extract(failure.detail_json,'$.operation')='auto_resume'
                            AND json_extract(failure.detail_json,'$.causal_identity.session_id')=rc.session_id)
                 FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 WHERE rc.state IN ('parked','queued_capacity') AND NOT {}
                 ORDER BY CASE rg.role WHEN 'manager' THEN 0 ELSE 1 END,rc.created_at,rc.session_id",
                crate::coordinator::coordinator_hold_absent("rc.attempt_id"),
                crate::store::provider_failure_hold_restricts("rc.attempt_id", "rg.role", "rg.lane_id")
            ))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, bool>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut due_human = None;
        let mut due_automatic = None;
        for (session, requested_by, state, result_json, attempt, held, own_failure) in candidates {
            if own_failure {
                continue;
            }
            let result = match RestartCandidateResult::parse(&result_json).and_then(|result| {
                result.validate_candidate_state(&state, &session)?;
                Ok(result)
            }) {
                Ok(result) => result,
                Err(_) => continue,
            };
            let human_member =
                requested_by.as_deref() == Some("human") && result.restart.active_batch().is_some();
            if (held && !human_member)
                || result.restart.replacement_failures >= MAX_RESTART_REPLACEMENT_FAILURES
                || !restart_due(&result.restart.next_due_at, &due).map_err(|error| {
                    auto_resume_failure(error, &session, &attempt, StepEffect::None)
                })?
            {
                continue;
            }
            if human_member && due_human.is_none() {
                due_human = Some((session, attempt));
            } else if !human_member && due_automatic.is_none() {
                due_automatic = Some((session, attempt));
            }
        }
        let selected = due_human.map(|candidate| (candidate, false)).or_else(|| {
            enabled
                .then_some(due_automatic)
                .flatten()
                .map(|candidate| (candidate, true))
        });
        let Some(((session, attempt), automatic)) = selected else {
            return Ok(None);
        };
        // Admission is one transaction; finishing it may already have resumed the session.
        let start = self
            .start_restart_admission(&session, automatic, false, None)
            .map_err(|error| auto_resume_failure(error, &session, &attempt, StepEffect::None))?;
        let result = self.finish_restart_admission(start).map_err(|error| {
            auto_resume_failure(error, &session, &attempt, StepEffect::Possible)
        })?;
        Ok(Some(
            serde_json::json!({"action":"auto_resume","result":result}),
        ))
    }

    fn resume_restart_sessions_inner(
        &self,
        operation_id: &str,
        selected: Option<&[String]>,
    ) -> Result<serde_json::Value> {
        self.store
            .require_execution_unheld("restart session resume")?;
        if !self.dispatch_enabled() {
            bail!("service is draining; restart resume is disabled")
        }
        if selected.is_some_and(|sessions| sessions.is_empty()) {
            bail!("Resume selected requires at least one session")
        }
        if operation_id.trim().is_empty() || operation_id.len() > 512 {
            bail!("operation_id is required and must be at most 512 bytes")
        }
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        if let Some(selected) = selected {
            for session in selected {
                if session.trim().is_empty() || session.len() > 512 {
                    bail!("Resume selected contains a blank or oversized session ID")
                }
                if seen.insert(session.clone()) {
                    ids.push(session.clone());
                    if ids.len() > MAX_RESTART_SELECTION {
                        bail!("Resume selected accepts at most {MAX_RESTART_SELECTION} distinct session IDs")
                    }
                }
            }
        }
        let request = if selected.is_some() {
            serde_json::json!({"mode":"selected","selected":ids})
        } else {
            serde_json::json!({"mode":"eligible"})
        };
        let request_hash = json_hash(&request)?;
        if let Some(receipt) = self.store.operation_receipt(
            operation_id,
            "human_control",
            "restart_resume",
            &request_hash,
        )? {
            return Ok(receipt);
        }
        if selected.is_some() && ids.len() == 1 {
            let start = self.start_restart_admission(
                &ids[0],
                false,
                true,
                Some((operation_id, &request_hash)),
            )?;
            return self.finish_restart_admission(start);
        }
        self.queue_restart_batch(
            operation_id,
            &request_hash,
            selected.map(|_| ids.as_slice()),
        )
    }

    fn queue_restart_batch(
        &self,
        operation_id: &str,
        request_hash: &str,
        selected: Option<&[String]>,
    ) -> Result<serde_json::Value> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(receipt) = restart_receipt_in(&tx, operation_id, request_hash)? {
            return Ok(receipt);
        }
        let selected_set = selected.map(|ids| ids.iter().cloned().collect::<HashSet<_>>());
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT rc.session_id,rg.role,rc.state,rc.reason,rc.result_json,rc.created_at
                 FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 ORDER BY CASE rg.role WHEN 'manager' THEN 0 ELSE 1 END,rc.created_at,rc.session_id",
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
            rows
        };
        let known = candidates
            .iter()
            .map(|candidate| candidate.0.clone())
            .collect::<HashSet<_>>();
        let mut queued = Vec::new();
        let mut omitted = Vec::new();
        let mut outcomes = Vec::new();
        let mut accepted = Vec::new();
        for (session, _role, state, reason, raw_result, _created_at) in candidates {
            if selected.is_none()
                && !matches!(state.as_str(), "parked" | "queued_capacity" | "failed")
            {
                continue;
            }
            if selected_set
                .as_ref()
                .is_some_and(|selection| !selection.contains(&session))
            {
                continue;
            }
            if !matches!(state.as_str(), "parked" | "queued_capacity" | "failed") {
                omitted.push(session.clone());
                outcomes.push(serde_json::json!({
                    "session_id":session,
                    "state":"omitted",
                    "reason":if state=="resumed"{"session was already resumed"}else{reason.as_str()},
                    "candidate_state":state,
                }));
                continue;
            }
            let result = RestartCandidateResult::parse(&raw_result).with_context(|| {
                format!("restart candidate {session} has invalid durable accounting")
            })?;
            result
                .validate_candidate_state(&state, &session)
                .with_context(|| {
                    format!("restart candidate {session} has invalid durable accounting")
                })?;
            if let Some(batch) = result.restart.active_batch() {
                omitted.push(session.clone());
                outcomes.push(serde_json::json!({
                    "session_id":session,
                    "state":"omitted",
                    "reason":"candidate already belongs to an active restart batch",
                    "batch_operation_id":batch.operation_id,
                }));
                continue;
            }
            if let Some(held) = crate::store::provider_failure_hold_for_session(&tx, &session)? {
                omitted.push(session.clone());
                outcomes.push(held_restart_outcome(&session, &held));
                continue;
            }
            let mut facts = crate::recovery::restart_session_facts(&tx, Some(&session))?;
            let Some(facts) = facts.pop() else {
                omitted.push(session.clone());
                outcomes.push(serde_json::json!({"session_id":session,"state":"omitted","reason":"session is not a durable restart candidate"}));
                continue;
            };
            if let Some(gate) = facts.admission_gate(false) {
                omitted.push(session.clone());
                outcomes.push(serde_json::json!({
                    "session_id":session,"state":"omitted","reason":gate.message(),"reason_code":gate.reason_code()
                }));
                continue;
            }
            if let Err(error) = crate::trip::require_attempt_ready(&tx, &facts.attempt_id, None) {
                omitted.push(session.clone());
                outcomes.push(serde_json::json!({
                    "session_id":session,"state":"omitted","reason":format!("{error:#}"),"reason_code":"restart.trip_not_ready"
                }));
                continue;
            }
            if accepted.len() == MAX_RESTART_BATCH {
                omitted.push(session.clone());
                outcomes.push(serde_json::json!({
                    "session_id":session,"state":"omitted","reason":"bounded restart batch capacity is four sessions"
                }));
                continue;
            }
            accepted.push((session, state, raw_result, result));
        }
        if let Some(selected) = selected {
            for session in selected {
                if !known.contains(session) {
                    omitted.push(session.clone());
                    outcomes.push(serde_json::json!({
                        "session_id":session,"state":"omitted","reason":"session is not a durable restart candidate"
                    }));
                }
            }
        }
        let members = accepted
            .iter()
            .map(|candidate| candidate.0.clone())
            .collect::<Vec<_>>();
        for (index, (session, state, raw_result, mut result)) in accepted.into_iter().enumerate() {
            result.restart.begin_batch(RestartBatchV1 {
                operation_id: operation_id.to_owned(),
                ordinal: u8::try_from(index + 1).expect("restart batch is bounded to four"),
                members: members.clone(),
                membership: RestartBatchMembership::Queued,
            })?;
            result.restart.next_due_at = Some(now.clone());
            result.restart.admission = None;
            result.set(
                "queue",
                serde_json::json!({"operation_id":operation_id,"ordinal":index + 1}),
            );
            let changed = tx.execute(
                "UPDATE restart_candidates SET state='queued_capacity',requested_by='human',
                        reason='queued for serialized exact-resume admission',result_json=?1,updated_at=?2
                 WHERE session_id=?3 AND state=?4 AND result_json=?5",
                rusqlite::params![result.encode()?, now, session, state, raw_result],
            )?;
            if changed != 1 {
                bail!("restart candidate {session} changed while its batch was being recorded")
            }
            queued.push(session.clone());
            outcomes.push(serde_json::json!({
                "session_id":session,"state":"queued","reason":"queued for serialized exact-resume admission",
                "batch_operation_id":operation_id,"ordinal":index + 1
            }));
        }
        let result = restart_operation_result(
            Some(operation_id),
            if selected.is_some() {
                "selected"
            } else {
                "eligible"
            },
            selected.unwrap_or_default(),
            &queued,
            &omitted,
            outcomes,
        );
        insert_restart_receipt_in(&tx, operation_id, request_hash, &result, &now)?;
        tx.commit()?;
        Ok(result)
    }

    fn start_restart_admission(
        &self,
        session: &str,
        automatic: bool,
        bypass_capacity_delay: bool,
        receipt: Option<(&str, &str)>,
    ) -> Result<RestartAdmissionStart> {
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((operation_id, request_hash)) = receipt {
            if let Some(existing) = restart_receipt_in(&tx, operation_id, request_hash)? {
                return Ok(RestartAdmissionStart::Finished(existing));
            }
        }
        let candidate: Option<(String, String, String)> = tx
            .query_row(
                "SELECT state,reason,result_json FROM restart_candidates WHERE session_id=?1",
                rusqlite::params![session],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((candidate_state, candidate_reason, raw_result)) = candidate else {
            let outcome = serde_json::json!({
                "session_id":session,"state":"omitted","reason":"session is not a durable restart candidate"
            });
            let result = restart_operation_result(
                receipt.map(|value| value.0),
                if automatic { "auto" } else { "selected" },
                &[session.to_owned()],
                &[],
                &[session.to_owned()],
                vec![outcome],
            );
            if let Some((operation_id, request_hash)) = receipt {
                insert_restart_receipt_in(&tx, operation_id, request_hash, &result, &now_text)?;
            }
            tx.commit()?;
            return Ok(RestartAdmissionStart::Finished(result));
        };
        let mut candidate_result =
            RestartCandidateResult::parse(&raw_result).with_context(|| {
                format!("restart candidate {session} has invalid durable accounting")
            })?;
        candidate_result
            .validate_candidate_state(&candidate_state, session)
            .with_context(|| {
                format!("restart candidate {session} has invalid durable accounting")
            })?;
        if candidate_state == "admitting" {
            let outcome = serde_json::json!({
                "session_id":session,
                "state":"omitted",
                "reason":"an existing serialized restart admission still owns this candidate"
            });
            let result = restart_operation_result(
                receipt.map(|value| value.0),
                if automatic { "auto" } else { "selected" },
                &[session.to_owned()],
                &[],
                &[session.to_owned()],
                vec![outcome],
            );
            if let Some((operation_id, request_hash)) = receipt {
                insert_restart_receipt_in(&tx, operation_id, request_hash, &result, &now_text)?;
            }
            tx.commit()?;
            return Ok(RestartAdmissionStart::Finished(result));
        }
        if !matches!(
            candidate_state.as_str(),
            "parked" | "queued_capacity" | "failed"
        ) {
            let outcome = serde_json::json!({
                "session_id":session,
                "state":"omitted",
                "reason":candidate_reason,
                "candidate_state":candidate_state
            });
            let result = restart_operation_result(
                receipt.map(|value| value.0),
                if automatic { "auto" } else { "selected" },
                &[session.to_owned()],
                &[],
                &[session.to_owned()],
                vec![outcome],
            );
            if let Some((operation_id, request_hash)) = receipt {
                insert_restart_receipt_in(&tx, operation_id, request_hash, &result, &now_text)?;
            }
            tx.commit()?;
            return Ok(RestartAdmissionStart::Finished(result));
        }
        if !bypass_capacity_delay && !restart_due(&candidate_result.restart.next_due_at, &now)? {
            let outcome = serde_json::json!({
                "session_id":session,"state":"queued_capacity","reason":"restart candidate is not due yet",
                "next_due_at":candidate_result.restart.next_due_at
            });
            tx.commit()?;
            return Ok(RestartAdmissionStart::Finished(restart_operation_result(
                receipt.map(|value| value.0),
                if automatic { "auto" } else { "selected" },
                &[session.to_owned()],
                &[],
                &[session.to_owned()],
                vec![outcome],
            )));
        }
        if let Some((operation_id, _)) = receipt {
            if candidate_result.restart.active_batch().is_none() {
                candidate_result.restart.begin_batch(RestartBatchV1 {
                    operation_id: operation_id.to_owned(),
                    ordinal: 1,
                    members: vec![session.to_owned()],
                    membership: RestartBatchMembership::Queued,
                })?;
            }
        }
        if let Some(held) = crate::store::provider_failure_hold_for_session(&tx, session)? {
            return Ok(RestartAdmissionStart::Finished(restart_operation_result(
                receipt.map(|value| value.0),
                if automatic { "auto" } else { "selected" },
                &[session.to_owned()],
                &[],
                &[session.to_owned()],
                vec![held_restart_outcome(session, &held)],
            )));
        }
        let mut facts = crate::recovery::restart_session_facts(&tx, Some(session))?;
        let facts = facts
            .pop()
            .ok_or_else(|| anyhow!("session is not a durable restart candidate"))?;
        let refusal = facts
            .admission_gate(automatic)
            .map(|gate| (gate.reason_code().to_owned(), gate.message().to_owned()))
            .or_else(|| {
                crate::trip::require_attempt_ready(&tx, &facts.attempt_id, None)
                    .err()
                    .map(|error| ("restart.trip_not_ready".to_owned(), format!("{error:#}")))
            });
        if let Some((reason_code, reason)) = refusal {
            candidate_result.restart.terminalize_batch();
            candidate_result.set(
                "admission_rejection",
                serde_json::json!({"reason_code":reason_code,"reason":reason}),
            );
            let changed = tx.execute(
                "UPDATE restart_candidates SET state='blocked',reason=?1,result_json=?2,updated_at=?3
                 WHERE session_id=?4 AND state=?5 AND result_json=?6",
                rusqlite::params![
                    reason,
                    candidate_result.encode()?,
                    now_text,
                    session,
                    candidate_state,
                    raw_result
                ],
            )?;
            if changed != 1 {
                bail!("restart candidate changed while admission refusal was recorded")
            }
            let outcome = serde_json::json!({
                "session_id":session,"state":"blocked","reason":reason,"reason_code":reason_code,
                "prior_reason":candidate_reason
            });
            let result = restart_operation_result(
                receipt.map(|value| value.0),
                if automatic { "auto" } else { "selected" },
                &[session.to_owned()],
                &[],
                &[session.to_owned()],
                vec![outcome],
            );
            if let Some((operation_id, request_hash)) = receipt {
                insert_restart_receipt_in(&tx, operation_id, request_hash, &result, &now_text)?;
            }
            tx.commit()?;
            return Ok(RestartAdmissionStart::Finished(result));
        }
        let (prior_transcript_epoch, resume_count): (String, i64) = tx.query_row(
            "SELECT transcript_epoch,resume_count FROM sessions WHERE id=?1",
            rusqlite::params![session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let expected_resume_ordinal = u32::try_from(
            resume_count
                .checked_add(1)
                .ok_or_else(|| anyhow!("resume ordinal overflow"))?,
        )
        .map_err(|_| anyhow!("resume ordinal is outside the supported range"))?;
        let admission_id = uuid::Uuid::new_v4().to_string();
        candidate_result.restart.admission = Some(RestartAdmissionV1 {
            id: admission_id.clone(),
            attempt_id: facts.attempt_id.clone(),
            role_generation_id: facts.role_generation_id.clone(),
            expected_task_version: facts.task_version,
            prior_candidate_state: candidate_state.clone(),
            prior_transcript_epoch: prior_transcript_epoch.clone(),
            expected_resume_ordinal,
            requested_by: if automatic { "auto" } else { "human" }.to_owned(),
        });
        if let Some(batch) = candidate_result.restart.batch.as_mut() {
            if batch.membership.active() {
                batch.membership = RestartBatchMembership::Admitting;
            }
        }
        candidate_result.set(
            "last_admission",
            serde_json::json!({"id":admission_id,"expected_resume_ordinal":expected_resume_ordinal}),
        );
        let changed = tx.execute(
            "UPDATE restart_candidates SET state='admitting',requested_by=?1,
                    reason='serialized exact-resume admission granted',result_json=?2,updated_at=?3
             WHERE session_id=?4 AND state=?5 AND result_json=?6",
            rusqlite::params![
                if automatic { "auto" } else { "human" },
                candidate_result.encode()?,
                now_text,
                session,
                candidate_state,
                raw_result
            ],
        )?;
        if changed != 1 {
            bail!("restart candidate changed while admission authority was reserved")
        }
        tx.execute(
            "UPDATE attempts SET status='running',updated_at=?1 WHERE id=?2",
            rusqlite::params![now_text, facts.attempt_id],
        )?;
        tx.execute(
            "UPDATE tasks SET attention='none',updated_at=?1 WHERE id=?2",
            rusqlite::params![now_text, facts.task_id],
        )?;
        tx.execute(
            "UPDATE claims SET state='running',updated_at=?1 WHERE attempt_id=?2 AND state='unknown'",
            rusqlite::params![now_text, facts.attempt_id],
        )?;
        if let Some((operation_id, request_hash)) = receipt {
            let reserved = restart_operation_result(
                Some(operation_id),
                "selected",
                &[session.to_owned()],
                &[],
                &[],
                vec![serde_json::json!({
                    "session_id":session,"state":"admitting","admission_id":admission_id
                })],
            );
            insert_restart_receipt_in(&tx, operation_id, request_hash, &reserved, &now_text)?;
        }
        tx.commit()?;
        Ok(RestartAdmissionStart::Started(RestartAdmissionTicket {
            session_id: session.to_owned(),
            attempt_id: facts.attempt_id,
            task_id: facts.task_id,
            role_generation_id: facts.role_generation_id,
            expected_task_version: facts.task_version,
            admission_id,
            expected_resume_ordinal,
            prior_transcript_epoch,
            automatic,
            receipt: receipt.map(|(operation_id, request_hash)| {
                (operation_id.to_owned(), request_hash.to_owned())
            }),
        }))
    }

    fn finish_restart_admission(&self, start: RestartAdmissionStart) -> Result<serde_json::Value> {
        let ticket = match start {
            RestartAdmissionStart::Started(ticket) => ticket,
            RestartAdmissionStart::Finished(result) => return Ok(result),
        };
        let resume = self
            .resume_role_session_inner(&ticket.session_id, "", None, false, Some(ticket.binding()))
            .and_then(browser_launch_dispatch_result);
        self.record_restart_admission_outcome(ticket, resume)
    }

    fn record_restart_admission_outcome(
        &self,
        ticket: RestartAdmissionTicket,
        resume: Result<ValidationLaunchResult>,
    ) -> Result<serde_json::Value> {
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<(String, String)> = tx
            .query_row(
                "SELECT state,result_json FROM restart_candidates WHERE session_id=?1",
                rusqlite::params![ticket.session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((candidate_state, raw_result)) = current else {
            let result = finish_stale_restart_outcome(
                &tx,
                &ticket,
                "restart candidate was removed",
                &now_text,
            )?;
            tx.commit()?;
            return Ok(result);
        };
        let mut candidate_result = RestartCandidateResult::parse(&raw_result)?;
        candidate_result.validate_candidate_state(&candidate_state, &ticket.session_id)?;
        let admission_matches = candidate_state == "admitting"
            && candidate_result
                .restart
                .admission
                .as_ref()
                .is_some_and(|admission| {
                    admission.id == ticket.admission_id
                        && admission.attempt_id == ticket.attempt_id
                        && admission.role_generation_id == ticket.role_generation_id
                        && admission.expected_task_version == ticket.expected_task_version
                        && admission.expected_resume_ordinal == ticket.expected_resume_ordinal
                        && admission.prior_transcript_epoch == ticket.prior_transcript_epoch
                });
        if !admission_matches {
            let result = finish_stale_restart_outcome(
                &tx,
                &ticket,
                "newer restart authority replaced this admission",
                &now_text,
            )?;
            tx.commit()?;
            return Ok(result);
        }
        let (
            session_status,
            resume_count,
            transcript_epoch,
            generation,
            attempt_status,
            task_attention,
            task_version,
            current_configuration,
            controls_clear,
        ): (String, i64, String, String, String, String, i64, bool, bool) = tx.query_row(
            "SELECT s.status,s.resume_count,s.transcript_epoch,s.role_generation_id,
                    a.status,t.attention,t.version,
                    EXISTS(SELECT 1 FROM role_settings setting
                      WHERE setting.task_id=t.id AND setting.role=rg.role
                        AND setting.revision=rg.config_revision
                        AND setting.effective_generation_id=rg.id)
                      OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                        SELECT 1 FROM lane_generations lane WHERE lane.lane_id=rg.lane_id
                          AND lane.effective_generation_id=rg.id)),
                    NOT EXISTS(SELECT 1 FROM controls control WHERE control.attempt_id=a.id
                      AND ((control.kind IN ('pause_now','pause_after_role','cancel')
                            AND control.state IN ('requested','draining','recovery_required'))
                        OR (control.kind IN ('manager_stop','manager_change')
                            AND control.state NOT IN ('finished','cancelled','superseded','rejected'))))
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE s.id=?1 AND rg.attempt_id=?2 AND t.id=?3",
            rusqlite::params![ticket.session_id, ticket.attempt_id, ticket.task_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )?;
        let invocation: Option<(String, String)> = tx
            .query_row(
                "SELECT state,transcript_epoch FROM resume_invocations
                 WHERE session_id=?1 AND resume_ordinal=?2",
                rusqlite::params![ticket.session_id, ticket.expected_resume_ordinal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let authority_current = attempt_status == "running"
            && task_attention == "none"
            && task_version == ticket.expected_task_version
            && current_configuration
            && controls_clear;
        let unchanged_preflight = invocation.is_none()
            && session_status == "exited"
            && resume_count + 1 == i64::from(ticket.expected_resume_ordinal)
            && transcript_epoch == ticket.prior_transcript_epoch
            && generation == ticket.role_generation_id;
        if !authority_current {
            let result = if resume.is_err() && unchanged_preflight {
                block_unreserved_stale_admission(
                    &tx,
                    &ticket,
                    &raw_result,
                    candidate_result,
                    current_configuration,
                    &now_text,
                )?
            } else {
                finish_stale_restart_outcome(
                    &tx,
                    &ticket,
                    "task, attempt, control, or role-generation authority changed during exact resume",
                    &now_text,
                )?
            };
            tx.commit()?;
            return Ok(result);
        }
        let exact_success = resume.as_ref().ok().is_some_and(|result| {
            result.session_id == ticket.session_id
                && result.role_generation_id == ticket.role_generation_id
                && generation == ticket.role_generation_id
                && session_status == "running"
                && resume_count == i64::from(ticket.expected_resume_ordinal)
                && invocation
                    .as_ref()
                    .is_some_and(|(state, epoch)| state == "running" && epoch == &transcript_epoch)
        });
        let proven_nondelivery = invocation.as_ref().is_some_and(|(state, _)| {
            state == "proven_nondelivery"
                && session_status == "exited"
                && resume_count == i64::from(ticket.expected_resume_ordinal)
                && transcript_epoch == ticket.prior_transcript_epoch
                && generation == ticket.role_generation_id
        });
        let typed_capacity = resume
            .as_ref()
            .err()
            .is_some_and(|error| error.downcast_ref::<RoleResumeCapacityError>().is_some());
        let (state, reason, delivery) = if exact_success {
            (
                "resumed",
                "exact native resume is running".to_owned(),
                "running",
            )
        } else if typed_capacity && unchanged_preflight {
            candidate_result.restart.capacity_deferrals = candidate_result
                .restart
                .capacity_deferrals
                .saturating_add(1);
            let delay = restart_capacity_delay(candidate_result.restart.capacity_deferrals);
            let due = now + ChronoDuration::seconds(delay);
            candidate_result.restart.next_due_at = Some(due.to_rfc3339());
            candidate_result.restart.admission = None;
            if let Some(batch) = candidate_result.restart.batch.as_mut() {
                if batch.membership.active() {
                    batch.membership = RestartBatchMembership::Queued;
                }
            }
            (
                "queued_capacity",
                format!(
                    "role capacity is full for exact resume; retry deferred until {}",
                    due.to_rfc3339()
                ),
                "not_attempted_capacity",
            )
        } else {
            let counted = resume.is_err() && (unchanged_preflight || proven_nondelivery);
            if counted {
                candidate_result.restart.replacement_failures = candidate_result
                    .restart
                    .replacement_failures
                    .saturating_add(1)
                    .min(MAX_RESTART_REPLACEMENT_FAILURES);
            }
            candidate_result.restart.terminalize_batch();
            let uncertain = !counted;
            let capped =
                candidate_result.restart.replacement_failures >= MAX_RESTART_REPLACEMENT_FAILURES;
            let error = resume
                .as_ref()
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| {
                    "resume returned success without matching durable invocation evidence"
                        .to_owned()
                });
            (
                if uncertain || capped {
                    "blocked"
                } else {
                    "failed"
                },
                if capped {
                    format!(
                        "three proven preflight or nondelivery failures exhausted exact retry authority; last failure: {error}"
                    )
                } else if uncertain {
                    format!("resume delivery is uncertain and cannot retry automatically: {error}")
                } else {
                    error
                },
                if uncertain {
                    "uncertain_or_delivered"
                } else if proven_nondelivery {
                    "proven_nondelivery"
                } else {
                    "preflight_failure"
                },
            )
        };
        if exact_success {
            candidate_result.restart.admission = None;
            candidate_result.restart.next_due_at = None;
            if let Some(batch) = candidate_result.restart.batch.as_mut() {
                if batch.membership.active() {
                    batch.membership = RestartBatchMembership::Completed;
                }
            }
        }
        candidate_result.set(
            "last_resume_outcome",
            serde_json::json!({
                "admission_id":ticket.admission_id,"state":state,"delivery":delivery,
                "expected_resume_ordinal":ticket.expected_resume_ordinal,
                "invocation_state":invocation.as_ref().map(|value| value.0.as_str())
            }),
        );
        let replacement_failures = candidate_result.restart.replacement_failures;
        let encoded = candidate_result.encode()?;
        let changed = tx.execute(
            "UPDATE restart_candidates SET state=?1,reason=?2,result_json=?3,updated_at=?4
             WHERE session_id=?5 AND state='admitting' AND result_json=?6",
            rusqlite::params![
                state,
                reason,
                encoded,
                now_text,
                ticket.session_id,
                raw_result
            ],
        )?;
        if changed != 1 {
            let result = finish_stale_restart_outcome(
                &tx,
                &ticket,
                "restart candidate changed before its outcome committed",
                &now_text,
            )?;
            tx.commit()?;
            return Ok(result);
        }
        let active_restored: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
             WHERE rc.attempt_id=?1 AND rc.session_id!=?2 AND rc.state='resumed' AND s.status='running')",
            rusqlite::params![ticket.attempt_id, ticket.session_id],
            |row| row.get(0),
        )?;
        match state {
            "resumed" => {
                tx.execute(
                    "UPDATE sessions SET desired_running=0 WHERE id=?1 AND role_generation_id=?2",
                    rusqlite::params![ticket.session_id, ticket.role_generation_id],
                )?;
                tx.execute(
                    "UPDATE attempts SET status='running',updated_at=?1 WHERE id=?2",
                    rusqlite::params![now_text, ticket.attempt_id],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention='none',updated_at=?1 WHERE id=?2",
                    rusqlite::params![now_text, ticket.task_id],
                )?;
            }
            "queued_capacity" => {
                tx.execute(
                    "UPDATE attempts SET status=CASE WHEN ?1 THEN 'running' ELSE 'restart_parked' END,updated_at=?2 WHERE id=?3",
                    rusqlite::params![active_restored, now_text, ticket.attempt_id],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention=CASE WHEN ?1 THEN 'none' ELSE 'restart_parked' END,updated_at=?2 WHERE id=?3",
                    rusqlite::params![active_restored, now_text, ticket.task_id],
                )?;
            }
            "failed" => {
                tx.execute(
                    "UPDATE attempts SET status=CASE WHEN ?1 THEN 'running' ELSE 'needs_input' END,updated_at=?2 WHERE id=?3",
                    rusqlite::params![active_restored, now_text, ticket.attempt_id],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention=CASE WHEN ?1 THEN 'none' ELSE 'resume_failed' END,updated_at=?2 WHERE id=?3",
                    rusqlite::params![active_restored, now_text, ticket.task_id],
                )?;
            }
            _ => {
                tx.execute(
                    "UPDATE attempts SET status=CASE WHEN ?1 THEN 'running' ELSE 'needs_recovery' END,updated_at=?2 WHERE id=?3",
                    rusqlite::params![active_restored, now_text, ticket.attempt_id],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention=CASE WHEN ?1 THEN 'none' ELSE 'needs_recovery' END,updated_at=?2 WHERE id=?3",
                    rusqlite::params![active_restored, now_text, ticket.task_id],
                )?;
                if !active_restored {
                    tx.execute(
                        "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
                        rusqlite::params![now_text, ticket.attempt_id],
                    )?;
                }
            }
        }
        let outcome = serde_json::json!({
            "session_id":ticket.session_id,"state":state,"reason":reason,"delivery":delivery,
            "replacement_failures":replacement_failures,
        });
        let queued = (state == "queued_capacity").then(|| vec![ticket.session_id.clone()]);
        let omitted = (!matches!(state, "resumed" | "queued_capacity"))
            .then(|| vec![ticket.session_id.clone()]);
        let result = restart_operation_result(
            ticket.receipt.as_ref().map(|value| value.0.as_str()),
            if ticket.automatic { "auto" } else { "selected" },
            &[ticket.session_id.clone()],
            queued.as_deref().unwrap_or_default(),
            omitted.as_deref().unwrap_or_default(),
            vec![outcome],
        );
        update_restart_receipt_in(&tx, &ticket, &result)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn role_context_payload(
        &self,
        context: &crate::domain::RoleContext,
    ) -> Result<serde_json::Value> {
        use rusqlite::params;
        let connection = self.store.lock()?;
        // Runtime recall contexts fail closed before any ordinary task policy or
        // catalog data is assembled. The resumed turn must rely only on retained
        // native-conversation recall.
        let setup_contract = crate::trip::setup_context(&connection, context)?;
        let isolated_validation: bool = connection.query_row(
            "SELECT a.status='capability_validation' FROM attempts a
             JOIN tasks t ON t.id=a.task_id
             WHERE a.id=?1 AND t.id=?2 AND t.project_id=?3",
            params![context.attempt_id, context.task_id, context.project_id],
            |row| row.get(0),
        )?;
        let ordinary_task = setup_contract.is_none() && !isolated_validation;
        let task:String=connection.query_row("SELECT json_object('id',id,'project_id',project_id,'title',title,'description',description,'acceptance_criteria',json(acceptance_criteria_json),'priority',priority,'lifecycle',lifecycle,'attention',attention,'version',version) FROM tasks WHERE id=?1",params![context.task_id],|row|row.get(0))?;
        let attempt:String=connection.query_row("SELECT json_object('id',id,'phase',phase,'status',status,'base_revision',base_revision,'plan_hash',plan_hash,'candidate_hash',candidate_hash,'accepted_snapshot_id',accepted_snapshot_id,'parent_attempt_id',parent_attempt_id,'scope_hash',scope_hash,'configuration_hash',configuration_hash,'workflow_version',workflow_version,'workflow_hash',workflow_hash) FROM attempts WHERE id=?1",params![context.attempt_id],|row|row.get(0))?;
        let plan_record: Option<(String, String)> = connection
            .query_row(
                "SELECT s.id,s.manifest_hash FROM snapshots s
             JOIN attempts a ON a.id=s.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND s.kind='plan'
               AND s.complete=1 AND s.manifest_hash=a.plan_hash",
                params![context.project_id, context.task_id, context.attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let current_review:Option<(String,String,String,Option<String>,String,String)>=connection.query_row(
            "SELECT r.id,r.review_kind,r.candidate_hash,a.plan_hash,a.phase,
             json_object('id',r.id,'review_kind',r.review_kind,'candidate_hash',r.candidate_hash,
               'prompt',r.prompt_text,'handoff',json(COALESCE(r.handoff_json,'{}')),
               'delivery_state',r.delivery_state,'resume_count',r.resume_count)
             FROM review_requests r JOIN attempts a ON a.id=r.attempt_id
             JOIN tasks t ON t.id=a.task_id
             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND r.role_generation_id=?4
               AND r.review_kind=CASE ?5 WHEN 'plan_reviewer' THEN 'plan'
                 WHEN 'code_reviewer' THEN 'code' WHEN 'final_verifier' THEN 'final' ELSE '' END
               AND r.delivery_state IN ('launching','delivered','ambiguous')
               AND r.candidate_hash=CASE r.review_kind WHEN 'plan' THEN a.plan_hash ELSE a.candidate_hash END
             ORDER BY r.created_at DESC LIMIT 1",
            params![context.project_id,context.task_id,context.attempt_id,context.role_generation_id,context.role.to_string()],
            |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))
        ).optional()?;
        let completed_final_review: Option<String> = if context.role
            == crate::domain::RoleKind::Manager
        {
            connection.query_row("SELECT json_object('id',r.id,'candidate_hash',r.candidate_hash,'reviewer_generation_id',r.role_generation_id,'verdict',r.verdict,'delivery_state',r.delivery_state,'updated_at',r.updated_at) FROM review_requests r JOIN attempts a ON a.id=r.attempt_id WHERE r.attempt_id=?1 AND r.review_kind='final' AND r.candidate_hash=a.candidate_hash AND r.verdict='approved' AND r.delivery_state='finished' ORDER BY r.updated_at DESC LIMIT 1",params![context.attempt_id],|row|row.get(0)).optional()?
        } else {
            None
        };
        let feedback = {
            let mut statement=connection.prepare("SELECT json_object('request_id',id,'kind',review_kind,'verdict',verdict,'feedback',feedback,'candidate_hash',candidate_hash) FROM review_requests WHERE attempt_id=?1 AND delivery_state='finished' AND verdict!='approved' ORDER BY updated_at DESC LIMIT 10")?;
            let rows = statement
                .query_map(params![context.attempt_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let checks = {
            let mut statement=connection.prepare("SELECT json_object('id',c.id,'check_id',c.check_id,'candidate_hash',c.candidate_hash,'selected_check_revision',c.selected_check_revision,'suite_name',c.suite_name,'suite_version',c.check_suite_version,'executable',c.executable,'arguments',json(c.arguments_json),'status',c.status,'launch_state',c.launch_state,'launch_error',c.launch_error,'exit_code',c.exit_code,'inputs_hash',c.inputs_hash,'acceptance_coverage',json(c.acceptance_coverage_json),'elapsed_millis',c.elapsed_millis,'freshness_state',c.freshness_state,'evidence',json(c.evidence_json),'created_at',c.created_at,'finished_at',c.finished_at) FROM check_runs c JOIN attempts a ON a.id=c.attempt_id JOIN tasks t ON t.id=a.task_id WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND c.candidate_hash=a.candidate_hash ORDER BY c.created_at DESC LIMIT 20")?;
            let rows = statement
                .query_map(
                    params![context.project_id, context.task_id, context.attempt_id],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let budgets = {
            let mut statement=connection.prepare("SELECT json_object('kind',review_kind,'initial',initial_allowance,'extension',extension_allowance,'spent',spent) FROM review_budgets WHERE attempt_id=?1 ORDER BY review_kind")?;
            let rows = statement
                .query_map(params![context.attempt_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let final_repair_recheck: Option<String> = connection
            .query_row(
                "SELECT json_object('accounting_lane','final_repair_recheck','allowance',1,
                   'provenance_kind',provenance_kind,
                   'spent',CASE WHEN spent_at IS NULL THEN 0 ELSE 1 END,'state',state,
                   'candidate_hash',candidate_hash,'review_request_id',review_request_id,
                   'verdict',verdict,'closed_reason',closed_reason,
                   'invalid_ordinary_review_request_id',rejected_code_request_id,
                   'ordinary_review_budget','separate and never spent by this lane')
                 FROM final_repair_rechecks WHERE attempt_id=?1",
                params![context.attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let guidance = if context.role == crate::domain::RoleKind::Manager {
            let mut statement=connection.prepare("SELECT json_object(
                'id',g.id,'body',g.body,'state',g.state,'reason',g.reason,
                'created_at',g.created_at,'written_at',g.written_at,'submitted_at',g.submitted_at,
                'acknowledgement_eligible',json(CASE WHEN g.state='submitted'
                  AND g.delivery_session_id=?2
                  AND g.delivery_transcript_epoch=(SELECT s.transcript_epoch FROM sessions s WHERE s.id=?2)
                  AND g.delivery_resume_invocation_id IS (
                    SELECT ri.id FROM resume_invocations ri JOIN sessions s ON s.id=?2
                    WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                    ORDER BY ri.resume_ordinal DESC LIMIT 1)
                  THEN 'true' ELSE 'false' END))
                FROM guidance_messages g WHERE g.role_generation_id=?1 AND g.state NOT IN ('acknowledged')
                ORDER BY g.created_at LIMIT 50")?;
            let rows = statement
                .query_map(
                    params![context.role_generation_id, context.session_id],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        } else {
            Vec::new()
        };
        let task_profiles = {
            let mut statement=connection.prepare("SELECT json_object('role',ap.role,'settings_revision',ap.settings_revision,'source',ap.source,'profile',json(ap.profile_json),'profile_hash',ap.profile_hash,'base_project_config_revision_id',ap.project_config_revision_id,'base_project_configuration_hash',ap.project_configuration_hash,'adapter',ap.adapter_name,'adapter_hash',ap.adapter_hash,'capability_id',ap.capability_id,'capability_key',ap.capability_key,'capability_proof_hash',ap.capability_proof_hash,'effective_for_this_generation',ap.role=?2 AND ap.settings_revision=(SELECT config_revision FROM role_generations WHERE id=?3)) FROM trip_attempt_profiles ap WHERE ap.attempt_id=?1 ORDER BY ap.role")?;
            let rows = statement
                .query_map(
                    params![
                        context.attempt_id,
                        context.role.to_string(),
                        context.role_generation_id
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let (project_policy, verification_catalog, selected_checks, explorer_decisions) =
            if ordinary_task {
                let (config_revision_id, config_revision, configuration_hash, config_json):(
                    String,i64,String,String
                )=connection.query_row(
                    "SELECT s.active_config_revision_id,r.revision,r.configuration_hash,r.config_json
                     FROM attempts a JOIN tasks t ON t.id=a.task_id
                     JOIN trip_project_state s ON s.project_id=t.project_id
                     JOIN trip_config_revisions r ON r.id=s.active_config_revision_id AND r.project_id=t.project_id
                     WHERE a.id=?1 AND t.id=?2 AND t.project_id=?3
                       AND s.readiness='ready' AND r.state='activated'",
                    params![context.attempt_id,context.task_id,context.project_id],
                    |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
                )?;
                let config: serde_json::Value = serde_json::from_str(&config_json)?;
                let testing = config
                    .get("testing")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        anyhow!("activated project configuration lacks structured testing policy")
                    })?;
                testing
                    .get("coverage")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow!("activated project configuration lacks testing coverage")
                    })?;
                let documentation = config
                    .get("documentation")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        anyhow!(
                            "activated project configuration lacks structured documentation policy"
                        )
                    })?;
                let guidance_paths = config
                    .get("guidance")
                    .filter(|value| value.is_array())
                    .ok_or_else(|| {
                        anyhow!("activated project configuration lacks guidance paths")
                    })?;
                let policy = serde_json::json!({
                    "config_revision_id":config_revision_id,
                    "config_revision":config_revision,
                    "configuration_hash":configuration_hash,
                    "testing":testing,
                    "documentation":documentation,
                    "guidance":{
                        "configured_paths":guidance_paths,
                        "content_available_in_role_context":false,
                        "boundary":"These are reviewed project-relative guidance paths. Ordinary role context does not perform target-file reads; use content already assigned in the workspace and report unavailable content honestly. setup-read is setup-discovery-only."
                    }
                });
                let catalog = {
                    let mut statement=connection.prepare(
                        "SELECT json_object('id',c.id,'config_revision_id',c.config_revision_id,
                         'check_key',c.check_key,'category',c.category,'command_kind',c.command_kind,
                         'executable',c.executable,'arguments',json(c.arguments_json),
                         'shell_command',c.shell_command,'cwd',c.cwd,'timeout_seconds',c.timeout_seconds,
                         'acceptance_criteria',json(c.acceptance_rows_json),
                         'relevant_inputs',json(c.relevant_inputs_json),'invalidation',json(c.invalidation_json),
                         'original_text',c.original_text)
                         FROM trip_verification_checks c
                         JOIN trip_project_state s ON s.project_id=c.project_id
                         JOIN attempts a ON a.id=?1 JOIN tasks t ON t.id=a.task_id
                         WHERE t.id=?2 AND t.project_id=?3 AND c.project_id=t.project_id
                           AND c.config_revision_id=s.active_config_revision_id AND c.enabled=1
                         ORDER BY c.category,c.check_key,c.id")?;
                    let rows = statement
                        .query_map(
                            params![context.attempt_id, context.task_id, context.project_id],
                            |row| row.get::<_, String>(0),
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let selected_revision: i64 = connection.query_row(
                    "SELECT selected_checks_revision FROM attempts WHERE id=?1 AND task_id=?2",
                    params![context.attempt_id, context.task_id],
                    |row| row.get(0),
                )?;
                let selected_ids = if selected_revision == 0 {
                    Vec::new()
                } else {
                    let mut statement = connection.prepare(
                        "SELECT check_id FROM trip_selected_checks
                         WHERE attempt_id=?1 AND revision=?2 AND required=1 ORDER BY check_id",
                    )?;
                    let rows = statement
                        .query_map(params![context.attempt_id, selected_revision], |row| {
                            row.get::<_, String>(0)
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let decisions = {
                    let mut statement=connection.prepare(
                        "SELECT json_object('decision_id',id,'stage',stage,'trigger',trigger,
                         'activated',json(CASE activated WHEN 1 THEN 'true' ELSE 'false' END),
                         'census',json(census_json),'limits',json(limits_json),'candidate_hash',candidate_hash,
                         'evidence_submitted',json(CASE WHEN outcome_json IS NULL THEN 'false' ELSE 'true' END))
                         FROM trip_explorer_decisions WHERE attempt_id=?1
                         ORDER BY created_at,id LIMIT 10")?;
                    let rows = statement
                        .query_map(params![context.attempt_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                (
                    Some(policy),
                    Some(serde_json::json!({
                        "scope":{
                            "project_id":context.project_id,
                            "task_id":context.task_id,
                            "attempt_id":context.attempt_id,
                            "config_revision_id":config_revision_id
                        },
                        "checks":catalog.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?
                    })),
                    Some(serde_json::json!({
                        "revision":selected_revision,
                        "check_ids":selected_ids,
                        "config_revision_id":config_revision_id
                    })),
                    decisions
                        .into_iter()
                        .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                        .collect::<serde_json::Result<Vec<_>>>()?,
                )
            } else {
                (None, None, None, Vec::new())
            };
        let ordinary_review_evidence = if ordinary_task && context.role.is_reviewer() {
            if let Some((request_id, review_kind, candidate_hash, plan_hash, phase, _)) =
                current_review.as_ref()
            {
                let candidate:Option<String>=connection.query_row(
                    "SELECT json_object('snapshot_id',s.id,'kind',s.kind,
                       'manifest_hash',s.manifest_hash,'manifest',json(s.manifest_json),
                       'snapshot_base',s.snapshot_base,'original_base',s.original_base,
                       'candidate_head',s.candidate_head,'source_role_generation_id',s.source_role_generation_id,
                       'source_settings_revision',s.source_settings_revision,'created_at',s.created_at)
                     FROM projects p JOIN tasks t ON t.project_id=p.id
                     JOIN attempts a ON a.task_id=t.id
                     JOIN review_requests r ON r.attempt_id=a.id
                     JOIN snapshots s ON s.attempt_id=a.id AND s.manifest_hash=r.candidate_hash
                     WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                       AND r.role_generation_id=?5 AND r.candidate_hash=?6 AND r.review_kind=?7
                       AND r.candidate_hash=CASE r.review_kind WHEN 'plan' THEN a.plan_hash ELSE a.candidate_hash END
                       AND s.kind=CASE r.review_kind WHEN 'plan' THEN 'plan' ELSE 'candidate' END
                       AND s.complete=1",
                    params![context.project_id,context.task_id,context.attempt_id,request_id,
                        context.role_generation_id,candidate_hash,review_kind],
                    |row|row.get(0)
                ).optional()?;
                if let Some(candidate) = candidate {
                    let structured_plan: Option<String> = if let Some(plan_hash) = plan_hash {
                        connection.query_row(
                            "SELECT json_object('id',sp.id,'plan_hash',sp.plan_hash,'plan',json(sp.plan_json),
                               'workflow_id',sp.workflow_id,'profile_revision_id',sp.profile_revision_id,
                               'criteria_hash',sp.criteria_hash,'verification_hash',sp.verification_hash,
                               'ownership_hash',sp.ownership_hash,'conformance_hash',sp.conformance_hash,
                               'explorer_decision_id',sp.explorer_decision_id,
                               'plan_review_receipt',json_object('availability',CASE WHEN sp.reviewed_at IS NULL THEN 'unavailable' ELSE 'recorded' END,'review_request_id',sp.review_request_id,'at',sp.reviewed_at),
                               'human_plan_approval_receipt',json_object('availability',CASE WHEN sp.approved_at IS NULL THEN 'unavailable' ELSE 'recorded' END,'at',sp.approved_at),
                               'implementation_authorization_receipt',json_object('availability',CASE WHEN sp.implementation_authorized_at IS NULL THEN 'unavailable' ELSE 'recorded' END,'at',sp.implementation_authorized_at),
                               'created_at',sp.created_at)
                             FROM projects p JOIN tasks t ON t.project_id=p.id
                             JOIN attempts a ON a.task_id=t.id
                             JOIN trip_structured_plans sp ON sp.id=a.structured_plan_id AND sp.plan_hash=a.plan_hash
                             JOIN review_requests r ON r.attempt_id=a.id
                             WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                               AND r.role_generation_id=?5 AND r.candidate_hash=?6
                               AND a.plan_hash=?7",
                            params![context.project_id,context.task_id,context.attempt_id,request_id,
                                context.role_generation_id,candidate_hash,plan_hash],
                            |row|row.get(0)
                        ).optional()?
                    } else {
                        None
                    };
                    let lanes = if let Some(plan_hash) = plan_hash {
                        let mut statement=connection.prepare(
                            "SELECT json_object('id',l.id,'lane_key',l.lane_key,
                               'owned_paths',json(l.owned_paths_json),'shared_paths',json(l.shared_paths_json),
                               'protected_paths',json(l.protected_paths_json),'dependencies',json(l.dependencies_json),
                               'source_hashes',json(l.source_hashes_json),'frozen_seams_hash',l.frozen_seams_hash,
                               'required',json(CASE l.required WHEN 1 THEN 'true' ELSE 'false' END),
                               'state',l.state,'effective_generation_id',lg.effective_generation_id,
                               'pending_settings_revision',lg.pending_settings_revision,
                               'yielded_at',l.yielded_at,'yield_receipt',json(l.receipt_json))
                             FROM projects p JOIN tasks t ON t.project_id=p.id
                             JOIN attempts a ON a.task_id=t.id
                             JOIN trip_structured_plans sp ON sp.id=a.structured_plan_id AND sp.plan_hash=a.plan_hash
                             JOIN review_requests r ON r.attempt_id=a.id
                             JOIN implementation_lanes l ON l.attempt_id=a.id
                             LEFT JOIN lane_generations lg ON lg.lane_id=l.id
                             WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                               AND r.role_generation_id=?5 AND r.candidate_hash=?6 AND a.plan_hash=?7
                             ORDER BY l.lane_key")?;
                        let rows = statement
                            .query_map(
                                params![
                                    context.project_id,
                                    context.task_id,
                                    context.attempt_id,
                                    request_id,
                                    context.role_generation_id,
                                    candidate_hash,
                                    plan_hash
                                ],
                                |row| row.get::<_, String>(0),
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows
                    } else {
                        Vec::new()
                    };
                    let integration: Option<String> = if let Some(plan_hash) = plan_hash {
                        connection.query_row(
                            "SELECT json_object('id',i.id,'state',i.state,'capsule',json(i.capsule_json),
                               'requested_by_generation_id',i.requested_by_generation_id,
                               'created_at',i.created_at,'dispatched_at',i.dispatched_at)
                             FROM projects p JOIN tasks t ON t.project_id=p.id
                             JOIN attempts a ON a.task_id=t.id
                             JOIN trip_structured_plans sp ON sp.id=a.structured_plan_id AND sp.plan_hash=a.plan_hash
                             JOIN review_requests r ON r.attempt_id=a.id
                             JOIN trip_integration_requests i ON i.attempt_id=a.id
                             WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                               AND r.role_generation_id=?5 AND r.candidate_hash=?6 AND a.plan_hash=?7",
                            params![context.project_id,context.task_id,context.attempt_id,request_id,
                                context.role_generation_id,candidate_hash,plan_hash],
                            |row|row.get(0)
                        ).optional()?
                    } else {
                        None
                    };
                    let approved_code_review: Option<String> = connection.query_row(
                        "SELECT json_object('availability','recorded','id',code.id,
                           'candidate_hash',code.candidate_hash,'reviewer_generation_id',code.role_generation_id,
                           'verdict',code.verdict,'delivery_state',code.delivery_state,'updated_at',code.updated_at)
                         FROM projects p JOIN tasks t ON t.project_id=p.id
                         JOIN attempts a ON a.task_id=t.id
                         JOIN review_requests code ON code.attempt_id=a.id
                         WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND a.candidate_hash=?4
                           AND code.review_kind='code' AND code.candidate_hash=a.candidate_hash
                           AND code.verdict='approved' AND code.delivery_state='finished'
                         ORDER BY code.updated_at DESC LIMIT 1",
                        params![context.project_id,context.task_id,context.attempt_id,candidate_hash],
                        |row|row.get(0)
                    ).optional()?;
                    let manager_conformance: Option<String> = connection.query_row(
                        "SELECT json_object('availability','recorded','id',c.id,'revision',c.revision,
                           'candidate_hash',c.candidate_hash,'config_hash',c.config_hash,
                           'active_config_revision_id',s.active_config_revision_id,
                           'active_configuration_hash',r.configuration_hash,
                           'acceptance',json(c.acceptance_json),'ownership',json(c.ownership_json),
                           'documentation',json(c.documentation_json),'test_policy',json(c.test_policy_json),
                           'readability',json(c.readability_json),
                           'submitted_by_generation_id',c.submitted_by_generation_id,'created_at',c.created_at)
                         FROM projects p JOIN tasks t ON t.project_id=p.id
                         JOIN attempts a ON a.task_id=t.id
                         JOIN trip_project_state s ON s.project_id=p.id AND s.readiness='ready'
                         JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
                           AND r.project_id=p.id AND r.state='activated'
                         JOIN trip_conformance_receipts c ON c.attempt_id=a.id
                           AND c.revision=a.manager_conformance_revision
                           AND c.candidate_hash=a.candidate_hash AND c.config_hash=r.configuration_hash
                         WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND a.candidate_hash=?4",
                        params![context.project_id,context.task_id,context.attempt_id,candidate_hash],
                        |row|row.get(0)
                    ).optional()?;
                    let current_selected_runs = {
                        let mut statement=connection.prepare(
                            "SELECT json_object('id',c.id,'check_id',c.check_id,
                               'candidate_hash',c.candidate_hash,'selected_check_revision',c.selected_check_revision,
                               'suite_name',c.suite_name,'suite_version',c.check_suite_version,
                               'executable',c.executable,'arguments',json(c.arguments_json),
                               'status',c.status,'launch_state',c.launch_state,'launch_error',c.launch_error,
                               'exit_code',c.exit_code,'inputs_hash',c.inputs_hash,
                               'acceptance_coverage',json(c.acceptance_coverage_json),
                               'elapsed_millis',c.elapsed_millis,'freshness_state',c.freshness_state,
                               'evidence',json(c.evidence_json),
                               'created_at',c.created_at,'finished_at',c.finished_at)
                             FROM trip_selected_checks selected
                             JOIN attempts a ON a.id=selected.attempt_id
                             JOIN tasks t ON t.id=a.task_id
                             JOIN check_runs c ON c.id=(SELECT latest.id FROM check_runs latest
                               WHERE latest.attempt_id=a.id AND latest.candidate_hash=a.candidate_hash
                                 AND latest.check_id=selected.check_id
                                 AND latest.selected_check_revision=selected.revision
                                 AND latest.status!='retry_superseded'
                               ORDER BY latest.created_at DESC,latest.id DESC LIMIT 1)
                             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3
                               AND selected.revision=a.selected_checks_revision AND selected.required=1
                             ORDER BY selected.check_id")?;
                        let rows = statement
                            .query_map(
                                params![context.project_id, context.task_id, context.attempt_id],
                                |row| row.get::<_, String>(0),
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows.into_iter()
                            .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                            .collect::<serde_json::Result<Vec<_>>>()?
                    };
                    let selected_ids = selected_checks
                        .as_ref()
                        .and_then(|value| value["check_ids"].as_array())
                        .cloned()
                        .unwrap_or_default();
                    let selected_revision = selected_checks
                        .as_ref()
                        .and_then(|value| value["revision"].as_i64())
                        .unwrap_or(0);
                    let failed = current_selected_runs.iter().any(|run| {
                        matches!(
                            run["status"].as_str(),
                            Some(
                                "failed"
                                    | "launch_ambiguous"
                                    | "precondition_failed"
                                    | "launch_failed"
                                    | "recovery_required"
                            )
                        ) || run["launch_state"] == "capture_failed"
                            || (run["status"] == "finished" && run["exit_code"].as_i64() != Some(0))
                    });
                    let passed = !selected_ids.is_empty()
                        && selected_ids.iter().all(|id| {
                            current_selected_runs.iter().any(|run| {
                                run["check_id"] == *id
                                    && run["status"] == "finished"
                                    && run["exit_code"] == 0
                                    && run["freshness_state"] == "current"
                            })
                        });
                    let check_state = if failed {
                        "failed"
                    } else if passed {
                        "passed"
                    } else if selected_ids.is_empty() {
                        "not_selected"
                    } else if phase == "code_review" {
                        "pending_after_code_approval"
                    } else {
                        "pending"
                    };
                    Some(serde_json::json!({
                        "availability":"available","review_request_id":request_id,"review_kind":review_kind,
                        "candidate_hash":candidate_hash,"candidate_snapshot":serde_json::from_str::<serde_json::Value>(&candidate)?,
                        "structured_plan":structured_plan.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?
                            .unwrap_or_else(||serde_json::json!({"availability":"unavailable","reason":"exact current structured plan receipt is missing or stale"})),
                        "lanes":{"availability":if lanes.is_empty(){"not_recorded"}else{"available"},
                            "records":lanes.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?},
                        "integration":integration.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?
                            .unwrap_or_else(||serde_json::json!({"availability":"not_recorded"})),
                        "approved_code_review_receipt":approved_code_review.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?
                            .unwrap_or_else(||serde_json::json!({"availability":"unavailable","reason":"exact current-candidate finished approved code-review receipt is missing or stale"})),
                        "manager_conformance_receipt":manager_conformance.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?
                            .unwrap_or_else(||serde_json::json!({"availability":"unavailable","reason":"exact current-candidate conformance receipt for the activated project configuration is missing or stale"})),
                        "check_gate":{"state":check_state,"selected_check_ids":selected_ids,
                            "selected_check_revision":selected_revision,"current_selected_runs":current_selected_runs,
                            "guidance":"A missing selected-check result before code approval is pending, not failed. It never bypasses the checks gate; only the latest non-superseded exact current-candidate run for each current selection row, at the current selection revision and with current freshness, establishes pass. Historical, unselected, wrong-revision, stale, and superseded runs do not satisfy or fail the current gate."},
                        "workspace_inspection":{"authority":"read_only",
                            "guidance":"Inspect the assigned workspace bytes with native read-only commands against this frozen manifest. Manifest hashes identify scope but do not substitute for byte inspection. Do not modify files."}
                    }))
                } else {
                    Some(serde_json::json!({"availability":"unavailable",
                        "reason":"exact current review candidate snapshot is missing or stale"}))
                }
            } else {
                Some(serde_json::json!({"availability":"unavailable",
                    "reason":"exact current delivered review request is missing or stale"}))
            }
        } else {
            None
        };
        let manager_feedback: Option<String> = if context.role
            == crate::domain::RoleKind::Implementer
        {
            connection.query_row(
                "SELECT json_object('result_id',r.id,'outcome',r.outcome,'summary',r.summary,
                   'metadata',json(r.metadata_json),'created_at',r.created_at)
                 FROM role_results r JOIN role_generations g ON g.id=r.role_generation_id
                 JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
                 JOIN role_settings settings ON settings.task_id=t.id AND settings.role='manager'
                   AND settings.effective_generation_id=g.id
                 WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND a.phase='implementation'
                   AND g.role='manager' AND r.outcome='needs_input'
                   AND json_extract(r.metadata_json,'$.approved_plan_hash')=a.plan_hash
                   AND json_extract(r.metadata_json,'$.candidate_hash') IS a.candidate_hash
                   AND EXISTS(SELECT 1 FROM sessions own WHERE own.id=?4 AND own.validation_cell IS NULL)
                 ORDER BY r.created_at DESC,r.id DESC LIMIT 1",
                params![context.project_id,context.task_id,context.attempt_id,context.session_id],
                |row| row.get(0),
            ).optional()?
        } else {
            None
        };
        drop(connection);
        let plan = if let Some((id, hash)) = plan_record {
            let path = self
                .paths
                .artifacts
                .join("snapshots")
                .join(id)
                .join("plan.md");
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read approved plan {}", path.display()))?;
            if bytes.len() > 256 * 1024 || hex::encode(sha2::Sha256::digest(&bytes)) != hash {
                bail!("approved plan artifact is missing, oversized, or changed")
            };
            Some(String::from_utf8(bytes)?)
        } else {
            None
        };
        let executable = providers::shell_quote(self.executable.to_string_lossy().as_ref());
        let context_command = format!("{executable} role context");
        let report = format!("{executable} role report --json '<literal JSON>'");
        let propose_transition = format!("{executable} role propose-transition --operation-id <UUID> --phase <PHASE> --evidence <TEXT>");
        let acknowledge_guidance =
            format!("{executable} role acknowledge-guidance --guidance <ID>");
        let json_command = |name: &str| format!("{executable} role {name} --json '<literal JSON>'");
        let setup_discovery = setup_contract
            .as_ref()
            .and_then(|value| value.get("purpose"))
            .and_then(|value| value.as_str())
            == Some("setup_discovery");
        let commands = match context.role {
            crate::domain::RoleKind::Manager => {
                let mut commands = serde_json::json!({"context":context_command,"report":report,"reporting_contract":ROLE_RESULT_REPORT_CONTRACT,
                    "propose_transition":propose_transition,"acknowledge_guidance":acknowledge_guidance,
                    "record_explorer_decision":json_command("record-explorer-decision"),
                    "configure_lanes":json_command("configure-lanes"),"request_integration":json_command("request-integration"),"select_checks":json_command("select-checks"),
                    "submit_conformance":json_command("submit-conformance")});
                if setup_discovery {
                    commands["setup_read"] = serde_json::Value::String(format!(
                        "{executable} role setup-read --relative-path <CONTAINED_PATH>"
                    ));
                }
                if ordinary_task {
                    commands["schemas"] = manager_command_schemas();
                }
                commands
            }
            crate::domain::RoleKind::Implementer if context.lane_id != "default" => {
                serde_json::json!({"context":context_command,"report":report,"reporting_contract":ROLE_RESULT_REPORT_CONTRACT,
                    "yield_lane":json_command("yield-lane"),"schemas":{"yield_lane":lane_yield_schema()}})
            }
            _ => {
                serde_json::json!({"context":context_command,"report":report,"reporting_contract":ROLE_RESULT_REPORT_CONTRACT})
            }
        };
        let guidance = guidance
            .into_iter()
            .map(|value| serde_json::from_str::<serde_json::Value>(&value))
            .collect::<serde_json::Result<Vec<_>>>()?;
        let guidance_contract=(context.role==crate::domain::RoleKind::Manager).then(||serde_json::json!({
            "content_available":!guidance.is_empty(),
            "acknowledgement_rule":"Only a row with acknowledgement_eligible=true may be acknowledged. queued, delivery_reserved, written_awaiting_submit, and delivery_unknown do not prove native submission.",
            "server_guard":"The service still requires the exact current manager generation and correlated submitted state."
        }));
        let mut value = serde_json::json!({"identity":context,"task":serde_json::from_str::<serde_json::Value>(&task)?,"attempt":serde_json::from_str::<serde_json::Value>(&attempt)?,"task_profiles":task_profiles.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"project_policy":project_policy,"verification_catalog":verification_catalog,"selected_checks":selected_checks,"explorer_decisions":explorer_decisions,"approved_plan":plan,"active_review":current_review.map(|value|serde_json::from_str::<serde_json::Value>(&value.5)).transpose()?,"completed_final_review":completed_final_review.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?,"review_feedback":feedback.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"review_budgets":budgets.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"check_results":checks.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"guidance":guidance,"guidance_contract":guidance_contract,"setup_contract":setup_contract,"commands":commands});
        if let Some(evidence) = ordinary_review_evidence {
            value["ordinary_review_evidence"] = evidence;
        }
        if let Some(recheck) = final_repair_recheck {
            value["final_repair_recheck"] = serde_json::from_str(&recheck)?;
        }
        if let Some(feedback) = manager_feedback {
            value["manager_feedback"] = serde_json::json!({
                "result":serde_json::from_str::<serde_json::Value>(&feedback)?,
                "authority":"Source repair guidance only. Preserve approved ownership and human approval boundaries. This is not a new approval, a selected-check receipt, or permission to report unverified completion. A source candidate may list pending checks for later service verification."
            });
        }
        if serde_json::to_vec(&value)?.len() > 1024 * 1024 {
            bail!("task-scoped role context exceeds the 1 MiB response bound")
        }
        Ok(value)
    }

    pub fn launch_validation(
        &self,
        request: ValidationLaunchRequest,
    ) -> Result<ValidationLaunchResult> {
        self.store.require_execution_unheld("validation launches")?;
        if !self.dispatch_enabled() {
            bail!("service is draining; new provider dispatch is disabled")
        }
        validate_request(&request)?;
        let request_hash = json_hash(&request)?;
        if let Some(receipt) = self.store.operation_receipt(
            &request.operation_id,
            "human_control",
            "capability_launch",
            &request_hash,
        )? {
            if receipt.get("state").and_then(|value| value.as_str()) == Some("launch_reserved") {
                bail!("this operation already reserved session {}; inspect it instead of launching another process",
                    receipt.get("session_id").and_then(|value| value.as_str()).unwrap_or("unknown"));
            }
            if matches!(
                receipt.get("state").and_then(|value| value.as_str()),
                Some("launch_failed" | "delivery_unknown")
            ) {
                bail!(
                    "prior validation launch did not establish a safe running session: {}",
                    receipt
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or("inspect the recorded session")
                )
            }
            return Ok(serde_json::from_value(receipt)?);
        }
        let repository = resolve_repository(&request.project_path)?;
        let project_id = uuid::Uuid::new_v4().to_string();
        let task_id = format!(
            "FEAS-{}-{}",
            request.cell.to_ascii_uppercase(),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let attempt_id = uuid::Uuid::new_v4().to_string();
        let context_id = uuid::Uuid::new_v4().to_string();
        let config_id = uuid::Uuid::new_v4().to_string();
        let role_generation_id = uuid::Uuid::new_v4().to_string();
        let credential_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let transcript_epoch = uuid::Uuid::new_v4().to_string();
        let role_token = auth::issue_secret();
        let launch = providers::prepare_role_launch_with_bundles(
            request.provider,
            request.role,
            &request.model,
            &request.effort,
            &repository.root,
            &request.prompt,
            &self.paths.role_socket,
            &role_token,
            &role_generation_id,
            &session_id,
            None,
            &self.hooks,
            &self.executable,
            &self.store.compatibility_bundles,
        )?;
        self.store.reserve_validation(
            &request,
            &request_hash,
            &project_id,
            &repository.root.to_string_lossy(),
            &repository.identity.to_string_lossy(),
            &repository.base_revision,
            &task_id,
            &attempt_id,
            &context_id,
            &config_id,
            &role_generation_id,
            &credential_id,
            &auth::hash_secret(&role_token),
            &session_id,
            &transcript_epoch,
            &launch.config,
        )?;
        self.store
            .bind_invocation_input(&session_id, &request.prompt, None)?;
        let process = match self.supervisor.spawn(
            &session_id,
            &role_generation_id,
            &transcript_epoch,
            &launch,
        ) {
            Ok(process) => process,
            Err(error) => {
                let (delivery_unknown, retryable) =
                    self.record_spawn_failure(&session_id, &error, false)?;
                self.store.finalize_validation_receipt(
                    &request.operation_id,
                    &serde_json::json!({"state":if delivery_unknown{"delivery_unknown"}else if retryable{"launch_failed"}else{"provider_nondelivery_cleanup_unknown"},"session_id":session_id,"reason":error.reason()}),
                )?;
                return Err(error.into());
            }
        };
        if let Err(error) = self.store.update_session_running(
            &session_id,
            &transcript_epoch,
            &serde_json::to_string(&process)?,
        ) {
            let _ = self.supervisor.interrupt(&session_id);
            self.store
                .mark_session_delivery_ambiguous(&session_id, &format!("{error:#}"))?;
            self.store.finalize_validation_receipt(
                &request.operation_id,
                &serde_json::json!({"state":"delivery_unknown","session_id":session_id,"reason":format!("{error:#}")}),
            )?;
            return Err(error.context("provider spawned but durable running state is ambiguous"));
        }
        self.role_tokens
            .write()
            .map_err(|_| anyhow!("role token map poisoned"))?
            .insert(session_id.clone(), role_token);
        let result = ValidationLaunchResult {
            session_id: session_id.clone(),
            task_id,
            attempt_id,
            role_generation_id,
            launch: launch.config,
            process,
            status: "running_unverified".to_owned(),
        };
        self.store
            .finalize_validation_receipt(&request.operation_id, &serde_json::to_value(&result)?)?;
        Ok(result)
    }

    pub fn resume_validation(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<ValidationLaunchResult> {
        self.resume_validation_with_instruction(session_id, prompt, None, false)
    }

    fn resume_validation_with_instruction(
        &self,
        session_id: &str,
        prompt: &str,
        service_instruction: Option<&str>,
        runtime_probe: bool,
    ) -> Result<ValidationLaunchResult> {
        self.store
            .require_execution_unheld("validation session resume")?;
        let result = self
            .resume_validation_with_instruction_inner(
                session_id,
                prompt,
                service_instruction,
                runtime_probe,
                None,
            )
            .and_then(browser_launch_dispatch_result);
        if let Err(error) = &result {
            self.record_permanent_resume_rejection(session_id, error, None)?;
        }
        result
    }

    fn resume_validation_with_instruction_inner(
        &self,
        session_id: &str,
        prompt: &str,
        service_instruction: Option<&str>,
        runtime_probe: bool,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<BrowserLaunchDispatch> {
        if !self.dispatch_enabled() {
            bail!("service is draining; validation resume is disabled")
        }
        let record = self.store.session_json(session_id)?;
        let is_runtime_probe = record["validation_cell"].as_str() == Some("trip_runtime_probe");
        if is_runtime_probe && !runtime_probe {
            bail!("ordinary runtime probe sessions resume only through resume_runtime_probe")
        }
        if runtime_probe && !is_runtime_probe {
            bail!("runtime probe resume requires a trip_runtime_probe session")
        }
        if !self.synthetic_dispatch_for_tests {
            self.supervisor.reconcile()?;
            if self
                .supervisor
                .active_session_ids()?
                .iter()
                .any(|active| active == session_id)
            {
                bail!("session process group is still active or has observed descendants; resume is blocked until quiescence is proven")
            }
        }
        let native_id = self.store.native_session_id(session_id)?.ok_or_else(|| {
            anyhow!(
                "native session identity is unavailable; a fresh session cannot be called resume"
            )
        })?;
        if record["hook_trust_state"].as_str() != Some("observed_unverified") {
            bail!("native resume requires an observed managed-descendant hook candidate; identity remains Unverified until L01/L02 evidence is reviewed")
        }
        let persisted_prompt = {
            let invocation = self.store.invocation_input(session_id)?;
            let persisted = invocation
                .get("prompt")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow!("validation session has no persisted prompt"))?;
            if !self.store.is_isolated_validation_session(session_id)? {
                let original = persisted
                    .split("\n\nReview request:")
                    .next()
                    .unwrap_or(persisted)
                    .split("\n\nPersisted human-approved checkpoint switch handoff:")
                    .next()
                    .unwrap_or(persisted);
                if prompt != persisted && prompt != original {
                    bail!("workflow validation resume input differs from its immutable invocation")
                }
            } else if prompt != persisted {
                bail!("isolated validation resume input differs from its immutable invocation")
            }
            self.store.validate_resume_input(session_id, &invocation)?;
            persisted.to_owned()
        };
        let resume_prompt = service_instruction.unwrap_or(&persisted_prompt);
        let prior: LaunchConfig = serde_json::from_value(record["launch_config"].clone())?;
        if prior.role == crate::domain::RoleKind::FinalReviewer {
            bail!("final verifier sessions are always fresh and cannot resume")
        }
        let token = auth::issue_secret();
        let generation = record["role_generation_id"]
            .as_str()
            .ok_or_else(|| anyhow!("session role generation is missing"))?;
        let epoch = uuid::Uuid::new_v4().to_string();
        let read_denials = crate::trip::setup_target_read_denials(
            &self.store,
            record["attempt_id"].as_str().unwrap_or_default(),
        )?;
        let runtime_policy = runtime_probe
            .then(|| {
                crate::trip::runtime_probe_launch_policy(
                    &self.store,
                    record["attempt_id"].as_str().unwrap_or_default(),
                    prior.role,
                    Some(session_id),
                )
            })
            .transpose()?
            .flatten();
        if runtime_policy.is_some() && !read_denials.is_empty() {
            bail!("runtime probe resume cannot inherit setup target read denials")
        }
        let launch = if let Some(policy) = runtime_policy.as_ref() {
            providers::prepare_runtime_probe_role_launch_with_bundles(
                prior.provider,
                prior.role,
                &prior.model,
                &prior.effort,
                &prior.cwd,
                &resume_prompt,
                &self.paths.role_socket,
                &token,
                generation,
                session_id,
                Some(&native_id),
                &self.hooks,
                &self.executable,
                policy,
                &self.store.compatibility_bundles,
            )?
        } else {
            providers::prepare_role_launch_with_read_denials_and_bundles(
                prior.provider,
                prior.role,
                &prior.model,
                &prior.effort,
                &prior.cwd,
                &resume_prompt,
                &self.paths.role_socket,
                &token,
                generation,
                session_id,
                Some(&native_id),
                &self.hooks,
                &self.executable,
                &read_denials,
                &self.store.compatibility_bundles,
            )?
        };
        let attempted = PreparedResumeIdentity::from_launch(&launch.config)?;
        let runtime = crate::trip::CapabilityRuntime::from_store(
            &self.store,
            self.hooks.clone(),
            self.paths.role_socket.clone(),
            self.executable.clone(),
        );
        let reservation = if let Some(receipt) = browser_receipt {
            self.store
                .reserve_session_resume_with_runtime_and_browser_receipt(
                    session_id,
                    &epoch,
                    &launch.config,
                    &token,
                    &runtime,
                    receipt,
                )
        } else {
            self.store
                .reserve_session_resume_with_runtime(
                    session_id,
                    &epoch,
                    &launch.config,
                    &token,
                    &runtime,
                )
                .map(|()| BrowserLaunchReservation::Reserved)
        };
        let reservation = match reservation {
            Ok(reservation) => reservation,
            Err(error) => {
                self.record_permanent_resume_rejection(session_id, &error, Some(&attempted))?;
                return Err(error);
            }
        };
        if let BrowserLaunchReservation::Existing(existing) = reservation {
            return Ok(BrowserLaunchDispatch::Existing(existing));
        }
        let process = if self.synthetic_dispatch_for_tests {
            self.store
                .mark_session_spawning(session_id, &epoch, "synthetic-resume-no-process")?;
            ProcessIdentity {
                pid: std::process::id(),
                process_group_id: 0,
                native_start_marker: "synthetic-resume-no-process".into(),
                observed_started_at: Utc::now().to_rfc3339(),
            }
        } else {
            match self
                .supervisor
                .spawn(session_id, generation, &epoch, &launch)
            {
                Ok(process) => process,
                Err(error) => {
                    self.record_spawn_failure(session_id, &error, true)?;
                    return Err(error.into());
                }
            }
        };
        if let Err(error) =
            self.store
                .update_session_running(session_id, &epoch, &serde_json::to_string(&process)?)
        {
            let _ = self.supervisor.interrupt(session_id);
            self.store
                .mark_session_delivery_ambiguous(session_id, &format!("{error:#}"))?;
            return Err(
                error.context("resumed provider spawned but durable running state is ambiguous")
            );
        }
        self.role_tokens
            .write()
            .map_err(|_| anyhow!("role token map poisoned"))?
            .insert(session_id.to_owned(), token);
        Ok(BrowserLaunchDispatch::Launched(ValidationLaunchResult {
            session_id: session_id.to_owned(),
            task_id: record["task_id"].as_str().unwrap_or_default().to_owned(),
            attempt_id: record["attempt_id"].as_str().unwrap_or_default().to_owned(),
            role_generation_id: generation.to_owned(),
            launch: launch.config,
            process,
            status: "running_resumed_unverified".to_owned(),
        }))
    }

    pub fn launch_workflow_validation(
        &self,
        request: WorkflowValidationRequest,
    ) -> Result<ValidationLaunchResult> {
        self.store
            .require_execution_unheld("workflow validation launches")?;
        if !matches!(
            request.launch.cell.as_str(),
            "L03" | "L04" | "L05" | "L06" | "L07" | "L08" | "L09" | "L10"
        ) {
            bail!("workflow validation cell must be L03 through L10")
        }
        validate_request(&request.launch)?;
        let expected = match request.launch.cell.as_str() {
            "L03" => (
                crate::domain::Provider::Claude,
                crate::domain::RoleKind::Manager,
            ),
            "L04" => (
                crate::domain::Provider::Codex,
                crate::domain::RoleKind::Manager,
            ),
            "L05" => (
                crate::domain::Provider::Codex,
                crate::domain::RoleKind::Implementer,
            ),
            "L06" => (
                crate::domain::Provider::Claude,
                crate::domain::RoleKind::Implementer,
            ),
            "L07" => (
                crate::domain::Provider::Codex,
                crate::domain::RoleKind::CodeReviewer,
            ),
            "L08" => (
                crate::domain::Provider::Claude,
                crate::domain::RoleKind::CodeReviewer,
            ),
            "L09" => (
                crate::domain::Provider::Codex,
                crate::domain::RoleKind::FinalReviewer,
            ),
            "L10" => (
                crate::domain::Provider::Claude,
                crate::domain::RoleKind::FinalReviewer,
            ),
            _ => unreachable!(),
        };
        if (request.launch.provider, request.launch.role) != expected {
            bail!("workflow validation cell has a fixed provider and role tuple")
        }
        let request_hash = json_hash(&request)?;
        if let Some(receipt) = self.store.operation_receipt(
            &request.launch.operation_id,
            "human_control",
            "workflow_capability_launch",
            &request_hash,
        )? {
            if receipt.get("state").and_then(|v| v.as_str()) == Some("launch_reserved") {
                bail!("workflow capability launch is already reserved; inspect its session")
            }
            if matches!(
                receipt.get("state").and_then(|value| value.as_str()),
                Some("launch_failed" | "delivery_unknown")
            ) {
                bail!(
                    "prior workflow validation launch did not establish a safe running session: {}",
                    receipt
                        .get("reason")
                        .and_then(|value| value.as_str())
                        .unwrap_or("inspect the recorded session")
                )
            }
            return Ok(serde_json::from_value(receipt)?);
        }
        self.reject_internal_setup_validation_target(&request.task_id)?;
        let attempt = match request.attempt_id.as_deref() {
            Some(id) => id.to_owned(),
            None => {
                self.prepare_validation_attempt(&request.task_id, &request.launch.project_path)?
            }
        };
        self.validate_workflow_validation_target(
            &attempt,
            &request.task_id,
            &request.launch.project_path,
        )?;
        let role = request.launch.role;
        let mut prompt = request.launch.prompt.clone();
        let mut review_request = None;
        if role.is_reviewer() {
            let kind = match role {
                crate::domain::RoleKind::PlanReviewer => "plan",
                crate::domain::RoleKind::CodeReviewer => "code",
                crate::domain::RoleKind::FinalReviewer => "final",
                _ => unreachable!(),
            };
            let handoff = serde_json::json!({"validation_cell":request.launch.cell,"task_id":request.task_id,"attempt_id":attempt,"purpose":"same_context_capability_validation"});
            let reserved =
                self.reviews
                    .reserve_request(&attempt, kind, &prompt, handoff.clone())?;
            if !matches!(reserved.state.as_str(), "reserved" | "nondelivered") {
                bail!(
                    "workflow validation review request is not safely dispatchable: {}",
                    reserved.state
                )
            }
            prompt = format!(
                "{}\n\nReview request: {}\nCandidate hash: {}\nStructured handoff: {}",
                prompt,
                reserved.request_id,
                reserved.candidate_hash,
                serde_json::to_string(&reserved.handoff)?
            );
            review_request = Some(reserved.request_id);
        }
        let switch = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT id FROM switch_intents WHERE attempt_id=?1 AND role=?2 AND state='ready_for_dispatch' ORDER BY created_at LIMIT 1",rusqlite::params![attempt,role.to_string()],|row|row.get::<_,String>(0)).optional()?
        };
        if let Some(intent) = switch.as_deref() {
            let handoff: String = {
                let connection = self.store.lock()?;
                connection.query_row(
                    "SELECT handoff_json FROM switch_intents WHERE id=?1 AND state='ready_for_dispatch'",
                    rusqlite::params![intent],
                    |row| row.get(0),
                )?
            };
            prompt.push_str("\n\nPersisted human-approved checkpoint switch handoff: ");
            prompt.push_str(&handoff);
        }
        let context = if let Some(intent) = switch.as_deref() {
            self.store.switched_validation_role_launch_context(intent)?
        } else {
            self.store.validation_role_launch_context(&attempt, role)?
        };
        if context.config.provider != request.launch.provider
            || context.config.model != request.launch.model
            || context.config.effort != request.launch.effort
        {
            let error =
                anyhow!("validation launch tuple differs from the task's current role settings");
            return Err(
                match self.store.release_unconsumed_launch_permits(
                    Some(&context.permit_id),
                    "validation launch tuple mismatch",
                ) {
                    Ok(_) => error,
                    Err(cleanup) => {
                        let original = format!("{error:#}");
                        error.context(format!(
                            "launch permit cleanup also failed after `{original}`: {cleanup:#}"
                        ))
                    }
                },
            );
        }
        let result = self.dispatch_context(
            &attempt,
            role,
            &prompt,
            review_request.as_deref(),
            context,
            Some((
                &request.launch.cell,
                &request.launch.operation_id,
                &request_hash,
            )),
            switch.as_deref(),
            None,
        )?;
        Ok(result)
    }

    fn validate_workflow_validation_target(
        &self,
        attempt: &str,
        task: &str,
        supplied_project: &Path,
    ) -> Result<()> {
        let supplied = crate::workspace::inspect(supplied_project)?;
        let (actual_task, identity, workspace, lifecycle, internal_purpose): (
            String,
            String,
            String,
            String,
            Option<String>,
        ) = {
            let connection = self.store.lock()?;
            connection
                .query_row(
                    "SELECT t.id,p.repository_identity,w.path,t.lifecycle,p.internal_purpose
                 FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
                 JOIN workspaces w ON w.attempt_id=a.id
                 WHERE a.id=?1 AND w.state='ready'",
                    rusqlite::params![attempt],
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
                .ok_or_else(|| {
                    anyhow!("workflow validation attempt has no ready owned workspace")
                })?
        };
        let expected_workspace = self.paths.artifacts.join("worktrees").join(attempt);
        if actual_task != task
            || identity != supplied.identity
            || lifecycle != "validation"
            || internal_purpose.as_deref() == Some("trip_setup_fixture")
            || std::path::Path::new(&workspace) != expected_workspace
        {
            bail!("workflow validation task, repository identity, or owned workspace does not match the requested attempt")
        }
        Ok(())
    }

    fn reject_internal_setup_validation_target(&self, task: &str) -> Result<()> {
        let connection = self.store.lock()?;
        let internal: Option<String> = connection
            .query_row(
                "SELECT p.internal_purpose FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?1",
                rusqlite::params![task],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if internal.as_deref() == Some("trip_setup_fixture") {
            bail!(
                "internal TRIP setup fixtures launch only through typed setup or runtime dispatch"
            )
        }
        Ok(())
    }

    fn prepare_validation_attempt(&self, task: &str, project_path: &Path) -> Result<String> {
        use rusqlite::{params, TransactionBehavior};
        let repository = crate::workspace::inspect(project_path)?;
        let attempt = uuid::Uuid::new_v4().to_string();
        let workspace_id = uuid::Uuid::new_v4().to_string();
        let claim = uuid::Uuid::new_v4().to_string();
        let workspace = self.paths.artifacts.join("worktrees").join(&attempt);
        let now = chrono::Utc::now().to_rfc3339();
        let (mut connection, config_hash, revision) = {
            let connection = self.store.lock()?;
            let configurations = {
                let mut statement=connection.prepare("SELECT role,revision,config_json FROM role_settings r WHERE task_id=?1 AND revision=(SELECT MAX(revision) FROM role_settings WHERE task_id=r.task_id AND role=r.role) ORDER BY role")?;
                let rows = statement
                    .query_map(params![task], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            if configurations.len() != 6 {
                bail!("workflow validation task requires all six app role settings")
            }
            let revision = configurations.iter().map(|v| v.1).max().unwrap_or(1);
            let hash = json_hash(&configurations)?;
            (connection, hash, revision)
        };
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let(project,identity,base,title,description,criteria):(String,String,String,String,String,String)=tx.query_row("SELECT p.id,p.repository_identity,p.base_revision,t.title,t.description,t.acceptance_criteria_json FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?1 AND t.lifecycle IN ('ready','validation')",params![task],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)))?;
        if identity != repository.identity || base != repository.head {
            bail!("validation task repository identity or base revision drifted")
        }
        let scope = json_hash(
            &serde_json::json!({"title":title,"description":description,"acceptance_criteria":serde_json::from_str::<serde_json::Value>(&criteria)?,"base_revision":base}),
        )?;
        crate::trip::require_project_ready(&tx, &project)?;
        tx.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,scope_hash,configuration_hash,created_at,updated_at,workflow_version,workflow_hash,upstream_source_hash,overlay_hash,legacy_migration_required) VALUES(?1,?2,?3,'planning',?4,?5,'workspace_reserved',?6,?7,?8,?8,?9,?10,?11,?12,0)",params![attempt,task,uuid::Uuid::new_v4().to_string(),base,revision,scope,config_hash,now,crate::workflow_resources::WORKFLOW_VERSION,crate::workflow_resources::workflow_hash(),crate::trip::source_hash(),crate::trip::overlay_hash()])?;
        crate::trip::bind_attempt_profiles(
            &tx,
            task,
            &attempt,
            &repository.root,
            &crate::trip::CapabilityRuntime::from_store(
                &self.store,
                self.hooks.clone(),
                self.paths.role_socket.clone(),
                self.executable.clone(),
            ),
            &now,
        )?;
        for (kind, allowance) in [("plan", 2), ("code", 2), ("final", 1)] {
            tx.execute("INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance) VALUES(?1,?2,?3,?4)",params![uuid::Uuid::new_v4().to_string(),attempt,kind,allowance])?;
        }
        tx.execute("INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at) VALUES(?1,?2,?3,?4,'reserved',?5,?5)",params![claim,task,attempt,identity,now])?;
        tx.execute("INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5,?6,'reserved',?7,?7)",params![workspace_id,attempt,repository.identity,workspace.to_string_lossy(),base,serde_json::json!({"validation":true,"no_auto_commit":true,"no_auto_merge":true}).to_string(),now])?;
        tx.execute("UPDATE tasks SET lifecycle='validation',attention='paused',version=version+1,updated_at=?1 WHERE id=?2",params![now,task])?;
        tx.commit()?;
        drop(connection);
        if let Err(error) =
            crate::workspace::create_detached_worktree(&repository, &workspace, &base)
        {
            crate::scheduler::record_workspace_reservation_recovery(
                &self.store,
                &attempt,
                &workspace_id,
                &format!("workflow validation worktree outcome requires recovery: {error:#}"),
                serde_json::json!({
                    "stage":"validation_worktree_create",
                    "path_exists":workspace.exists(),
                    "error":format!("{error:#}"),
                }),
            )?;
            return Err(error.context("workflow validation worktree outcome requires recovery"));
        }
        if let Err(error) =
            crate::trip::materialize_project_policy(&self.store, &attempt, &workspace)
        {
            crate::scheduler::record_workspace_reservation_recovery(
                &self.store,
                &attempt,
                &workspace_id,
                &format!("workflow validation policy materialization requires recovery: {error:#}"),
                serde_json::json!({
                    "stage":"validation_policy_materialization",
                    "path_exists":workspace.exists(),
                    "error":format!("{error:#}"),
                }),
            )?;
            return Err(
                error.context("workflow validation policy materialization requires recovery")
            );
        }
        crate::scheduler::promote_workspace_reservation(&self.store, &workspace_id, &attempt)?;
        let _ = project;
        Ok(attempt)
    }

    pub fn resume_role_session(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<ValidationLaunchResult> {
        self.store.require_execution_unheld("role session resume")?;
        let result = self
            .resume_role_session_inner(session_id, prompt, None, true, None)
            .and_then(browser_launch_dispatch_result);
        if let Err(error) = &result {
            self.record_permanent_resume_rejection(session_id, error, None)?;
        }
        result
    }

    pub fn resume_role_session_with_operation(
        &self,
        operation_id: &str,
        session_id: &str,
        prompt: &str,
    ) -> Result<ValidationLaunchResult> {
        self.store.require_execution_unheld("role session resume")?;
        let request_hash =
            json_hash(&serde_json::json!({"session_id":session_id,"prompt":prompt}))?;
        if let Some(existing) =
            self.store
                .browser_launch_receipt(operation_id, "role_resume", &request_hash)?
        {
            return browser_launch_receipt(existing);
        }
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "role_resume",
            input_hash: &request_hash,
            entity_id: session_id,
        };
        let result = self.resume_role_session_inner(session_id, prompt, Some(receipt), true, None);
        if let Err(error) = &result {
            self.record_permanent_resume_rejection(session_id, error, None)?;
        }
        self.finish_browser_launch(receipt, result)
    }

    fn resume_role_session_inner(
        &self,
        session_id: &str,
        prompt: &str,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
        record_permanent_rejection: bool,
        restart_admission: Option<RestartAdmissionBinding<'_>>,
    ) -> Result<BrowserLaunchDispatch> {
        if !self.dispatch_enabled() {
            bail!("service is draining; role resume is disabled")
        }
        let record = self.store.session_json(session_id)?;
        if record["validation_cell"].as_str() == Some("trip_runtime_probe") {
            bail!("ordinary runtime probe sessions resume only through resume_runtime_probe")
        }
        if matches!(
            record["validation_cell"].as_str(),
            Some("trip_setup_discovery" | "trip_setup_probe")
        ) {
            if restart_admission.is_some() {
                bail!("a host-restart admission cannot resume a setup session")
            }
            let invocation = self.store.invocation_input(session_id)?;
            let persisted_prompt = invocation
                .get("prompt")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    anyhow!(
                        "setup session has no persisted invocation input; exact resume is blocked"
                    )
                })?;
            if !prompt.trim().is_empty() && prompt != persisted_prompt {
                bail!("resume input differs from the persisted setup invocation")
            }
            return self.resume_validation_with_instruction_inner(
                session_id,
                persisted_prompt,
                None,
                false,
                browser_receipt,
            );
        }
        if !self.synthetic_dispatch_for_tests {
            self.supervisor.reconcile()?;
            if self
                .supervisor
                .active_session_ids()?
                .iter()
                .any(|id| id == session_id)
            {
                bail!("session or an observed descendant remains active")
            }
        }
        if record["validation_cell"].is_string()
            && self.store.is_isolated_validation_session(session_id)?
        {
            bail!("use capability resume for isolated validation sessions")
        }
        let native_id = self.store.native_session_id(session_id)?;
        let prior: LaunchConfig = serde_json::from_value(record["launch_config"].clone())?;
        if native_id.is_none()
            && !matches!(
                prior.role,
                crate::domain::RoleKind::PlanReviewer | crate::domain::RoleKind::CodeReviewer
            )
        {
            bail!("native session identity is unavailable")
        }
        if prior.role == crate::domain::RoleKind::FinalReviewer {
            bail!("final verifier sessions are always fresh and cannot resume")
        }
        let invocation = self.store.invocation_input(session_id)?;
        let persisted_prompt = invocation
            .get("prompt")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                anyhow!("session has no persisted invocation input; exact resume is blocked")
            })?;
        if !prompt.trim().is_empty() && prompt != persisted_prompt {
            bail!("resume input differs from the persisted invocation; create a fresh role or review request")
        }
        self.store.validate_resume_input(session_id, &invocation)?;
        // Only this delivered turn carries the notice; the saved invocation stays the original prompt.
        let delivered_prompt = match restart_admission {
            Some(_) => format!("{HOST_RESTART_RESUME_NOTICE}\n\n{persisted_prompt}"),
            None => persisted_prompt.to_owned(),
        };
        let generation = record["role_generation_id"]
            .as_str()
            .ok_or_else(|| anyhow!("session generation is missing"))?
            .to_owned();
        let token = auth::issue_secret();
        let epoch = uuid::Uuid::new_v4().to_string();
        let read_denials = crate::trip::setup_target_read_denials(
            &self.store,
            record["attempt_id"].as_str().unwrap_or_default(),
        )?;
        let launch = providers::prepare_role_launch_with_read_denials_and_bundles(
            prior.provider,
            prior.role,
            &prior.model,
            &prior.effort,
            &prior.cwd,
            &delivered_prompt,
            &self.paths.role_socket,
            &token,
            &generation,
            session_id,
            native_id.as_deref(),
            &self.hooks,
            &self.executable,
            &read_denials,
            &self.store.compatibility_bundles,
        )?;
        let attempted = PreparedResumeIdentity::from_launch(&launch.config)?;
        if native_id.is_none() {
            let error = anyhow!("native session identity is unavailable");
            if record_permanent_rejection {
                self.record_permanent_resume_rejection(session_id, &error, Some(&attempted))?;
            }
            return Err(error);
        }
        let reservation = if let Some(receipt) = browser_receipt {
            self.store.reserve_role_resume_with_browser_receipt(
                session_id,
                &epoch,
                &launch.config,
                &token,
                receipt,
            )
        } else if let Some(admission) = restart_admission {
            self.store
                .reserve_role_resume_for_restart_admission(
                    session_id,
                    &epoch,
                    &launch.config,
                    &token,
                    admission,
                )
                .map(|()| BrowserLaunchReservation::Reserved)
        } else {
            self.store
                .reserve_role_resume(session_id, &epoch, &launch.config, &token)
                .map(|()| BrowserLaunchReservation::Reserved)
        };
        let reservation = match reservation {
            Ok(reservation) => reservation,
            Err(error) => {
                if record_permanent_rejection {
                    self.record_permanent_resume_rejection(session_id, &error, Some(&attempted))?;
                }
                return Err(error);
            }
        };
        if let BrowserLaunchReservation::Existing(existing) = reservation {
            return Ok(BrowserLaunchDispatch::Existing(existing));
        }
        let process = if self.synthetic_dispatch_for_tests {
            self.store
                .mark_session_spawning(session_id, &epoch, "synthetic-resume-no-process")?;
            ProcessIdentity {
                pid: std::process::id(),
                process_group_id: 0,
                native_start_marker: "synthetic-resume-no-process".into(),
                observed_started_at: Utc::now().to_rfc3339(),
            }
        } else {
            match self
                .supervisor
                .spawn(session_id, &generation, &epoch, &launch)
            {
                Ok(process) => process,
                Err(error) => {
                    self.record_spawn_failure(session_id, &error, true)?;
                    return Err(error.into());
                }
            }
        };
        if let Err(error) =
            self.store
                .update_session_running(session_id, &epoch, &serde_json::to_string(&process)?)
        {
            let _ = self.supervisor.interrupt(session_id);
            self.store
                .mark_session_delivery_ambiguous(session_id, &format!("{error:#}"))?;
            return Err(
                error.context("resumed role spawned but durable running state is ambiguous")
            );
        }
        self.role_tokens
            .write()
            .map_err(|_| anyhow!("role token map poisoned"))?
            .insert(session_id.to_owned(), token);
        Ok(BrowserLaunchDispatch::Launched(ValidationLaunchResult {
            session_id: session_id.into(),
            task_id: record["task_id"].as_str().unwrap_or_default().into(),
            attempt_id: record["attempt_id"].as_str().unwrap_or_default().into(),
            role_generation_id: generation,
            launch: launch.config,
            process,
            status: "running_resumed".into(),
        }))
    }

    pub fn dispatch_attempt_role(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        prompt: &str,
    ) -> Result<ValidationLaunchResult> {
        if matches!(
            role,
            crate::domain::RoleKind::PlanReviewer
                | crate::domain::RoleKind::CodeReviewer
                | crate::domain::RoleKind::FinalReviewer
        ) {
            bail!("reviewers launch only from a stable reserved review request")
        }
        self.dispatch_role(attempt_id, role, prompt, None)
    }

    pub fn dispatch_implementation_lane(
        &self,
        attempt_id: &str,
        lane_key: &str,
        prompt: &str,
    ) -> Result<ValidationLaunchResult> {
        let role = crate::domain::RoleKind::Implementer;
        let context = self
            .store
            .lane_role_launch_context(attempt_id, role, lane_key)?;
        self.dispatch_context(attempt_id, role, prompt, None, context, None, None, None)
    }

    pub fn dispatch_trip_setup_role(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
    ) -> Result<ValidationLaunchResult> {
        self.dispatch_trip_setup_role_inner(attempt_id, role, false, None)
    }

    pub fn dispatch_trip_setup_role_with_operation(
        &self,
        operation_id: &str,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<ValidationLaunchResult> {
        let request_hash = json_hash(&serde_json::json!({
            "attempt_id":attempt_id,
            "role":role,
            "fresh_resume_rejection":fresh_resume_rejection,
        }))?;
        if let Some(existing) =
            self.store
                .browser_launch_receipt(operation_id, "trip_setup_dispatch", &request_hash)?
        {
            return browser_launch_receipt(existing);
        }
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "trip_setup_dispatch",
            input_hash: &request_hash,
            entity_id: attempt_id,
        };
        let result = self.dispatch_trip_setup_role_inner_with_browser(
            attempt_id,
            role,
            false,
            Some(receipt),
            fresh_resume_rejection,
        );
        self.finish_browser_launch(receipt, result)
    }

    fn dispatch_trip_setup_role_inner(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        runtime_probe: bool,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<ValidationLaunchResult> {
        self.dispatch_trip_setup_role_inner_with_browser(
            attempt_id,
            role,
            runtime_probe,
            None,
            fresh_resume_rejection,
        )
        .and_then(browser_launch_dispatch_result)
    }

    fn dispatch_trip_setup_role_inner_with_browser(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        runtime_probe: bool,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<BrowserLaunchDispatch> {
        let prompt = crate::trip::setup_launch_prompt(&self.store, attempt_id, role)?;
        let context = self
            .store
            .trip_setup_role_launch_context(attempt_id, role, runtime_probe)?;
        let runtime_policy = runtime_probe
            .then(|| crate::trip::runtime_probe_launch_policy(&self.store, attempt_id, role, None))
            .transpose()?
            .flatten();
        if let Some(receipt) = browser_receipt {
            self.dispatch_context_with_browser_receipt(
                attempt_id,
                role,
                &prompt,
                None,
                context,
                None,
                None,
                runtime_policy.as_ref(),
                receipt,
                fresh_resume_rejection,
            )
        } else {
            self.dispatch_context_inner(
                attempt_id,
                role,
                &prompt,
                None,
                context,
                None,
                None,
                runtime_policy.as_ref(),
                None,
                fresh_resume_rejection,
            )
        }
    }

    pub fn dispatch_runtime_probe(
        &self,
        operation_id: &str,
        admission_id: &str,
        role: crate::domain::RoleKind,
    ) -> Result<ValidationLaunchResult> {
        let attempt: String = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT p.attempt_id FROM trip_runtime_probes p JOIN trip_runtime_admissions a ON a.id=p.admission_id WHERE p.admission_id=?1 AND p.role=?2 AND p.state='authorized' AND a.state IN ('authorized','running') AND NOT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE rg.attempt_id=p.attempt_id AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required'))",rusqlite::params![admission_id,role.to_string()],|row|row.get(0)).optional()?.ok_or_else(||anyhow!("ordinary runtime probe is not explicitly authorized or already has active, consumed, or recorded evidence; prepare corrected runtime verification for a fresh scope"))?
        };
        match self.dispatch_trip_setup_role_inner(&attempt, role, true, None) {
            Ok(result) => {
                let now = Utc::now().to_rfc3339();
                let connection = self.store.lock()?;
                connection.execute("UPDATE trip_runtime_probes SET state='running',session_id=?1,updated_at=?2 WHERE admission_id=?3 AND role=?4",rusqlite::params![result.session_id,now,admission_id,role.to_string()])?;
                crate::trip::refresh_runtime_admission_state(&connection, admission_id, &now)?;
                connection.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'human','runtime_probe.launch','runtime_admission',?3,?4,?5)",rusqlite::params![uuid::Uuid::new_v4().to_string(),operation_id,admission_id,serde_json::json!({"role":role,"session_id":result.session_id,"attempt_id":attempt}).to_string(),now])?;
                Ok(result)
            }
            Err(error) if error.is::<crate::store::RoleLaunchCapacityError>() => Err(error),
            Err(error) => {
                let reason = format!("{error:#}");
                let now = Utc::now().to_rfc3339();
                let connection = self.store.lock()?;
                connection.execute("UPDATE trip_runtime_probes SET state='failed',failure_reason=?1,updated_at=?2 WHERE admission_id=?3 AND role=?4",rusqlite::params![reason,now,admission_id,role.to_string()])?;
                crate::trip::refresh_runtime_admission_state(&connection, admission_id, &now)?;
                Err(error)
            }
        }
    }

    pub fn resume_runtime_probe(
        &self,
        admission_id: &str,
        role: crate::domain::RoleKind,
    ) -> Result<ValidationLaunchResult> {
        self.resume_runtime_probe_inner(admission_id, role, None)
            .and_then(browser_launch_dispatch_result)
    }

    fn resume_runtime_probe_inner(
        &self,
        admission_id: &str,
        role: crate::domain::RoleKind,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<BrowserLaunchDispatch> {
        if role == crate::domain::RoleKind::FinalReviewer {
            bail!("final verifier runtime probes are fresh-only and cannot resume")
        }
        let session: String = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT p.session_id FROM trip_runtime_probes p JOIN trip_runtime_admissions a ON a.id=p.admission_id JOIN sessions s ON s.id=p.session_id WHERE p.admission_id=?1 AND p.role=?2 AND p.state IN ('running','awaiting_resume') AND a.state IN ('running','awaiting_publication') AND s.status='exited' AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1",rusqlite::params![admission_id,role.to_string()],|row|row.get(0)).optional()?.ok_or_else(||anyhow!("ordinary runtime probe has no quiescent retained session eligible for exact resume"))?
        };
        let prompt = self
            .store
            .invocation_input(&session)?
            .get("prompt")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("ordinary runtime probe lost its immutable prompt"))?
            .to_owned();
        let instruction =
            crate::trip::runtime_probe_resume_prompt(&self.store, admission_id, role, &session)?;
        let result = self.resume_validation_with_instruction_inner(
            &session,
            &prompt,
            Some(&instruction),
            true,
            browser_receipt,
        );
        if let Err(error) = &result {
            self.record_permanent_resume_rejection(&session, error, None)?;
        }
        let result = result?;
        if let BrowserLaunchDispatch::Existing(existing) = result {
            return Ok(BrowserLaunchDispatch::Existing(existing));
        }
        let BrowserLaunchDispatch::Launched(result) = result else {
            unreachable!();
        };
        let now = Utc::now().to_rfc3339();
        let connection = self.store.lock()?;
        connection.execute("UPDATE trip_runtime_probes SET state='running',updated_at=?1 WHERE admission_id=?2 AND role=?3",rusqlite::params![now,admission_id,role.to_string()])?;
        crate::trip::refresh_runtime_admission_state(&connection, admission_id, &now)?;
        Ok(BrowserLaunchDispatch::Launched(result))
    }

    pub fn resume_runtime_probe_with_operation(
        &self,
        operation_id: &str,
        admission_id: &str,
        role: crate::domain::RoleKind,
    ) -> Result<ValidationLaunchResult> {
        let request_hash =
            json_hash(&serde_json::json!({"admission_id":admission_id,"role":role}))?;
        if let Some(existing) = self.store.browser_launch_receipt(
            operation_id,
            "runtime_probe_resume",
            &request_hash,
        )? {
            return browser_launch_receipt(existing);
        }
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "runtime_probe_resume",
            input_hash: &request_hash,
            entity_id: admission_id,
        };
        self.finish_browser_launch(
            receipt,
            self.resume_runtime_probe_inner(admission_id, role, Some(receipt)),
        )
    }

    fn finish_browser_launch(
        &self,
        receipt: BrowserLaunchReceipt<'_>,
        launch: Result<BrowserLaunchDispatch>,
    ) -> Result<ValidationLaunchResult> {
        match launch {
            Ok(BrowserLaunchDispatch::Launched(result)) => {
                self.store.finalize_browser_launch_receipt(
                    receipt.operation_id,
                    receipt.operation_kind,
                    receipt.input_hash,
                    Some(serde_json::to_value(&result)?),
                    None,
                )?;
                Ok(result)
            }
            Ok(BrowserLaunchDispatch::Existing(existing)) => browser_launch_receipt(existing),
            Err(error) => {
                match self.store.browser_launch_receipt(
                    receipt.operation_id,
                    receipt.operation_kind,
                    receipt.input_hash,
                )? {
                    Some(existing)
                        if existing.get("state").and_then(serde_json::Value::as_str)
                            == Some("reserved") =>
                    {
                        self.store.finalize_browser_launch_receipt(
                            receipt.operation_id,
                            receipt.operation_kind,
                            receipt.input_hash,
                            None,
                            Some(format!("{error:#}")),
                        )?;
                        Err(error)
                    }
                    Some(existing) => browser_launch_receipt(existing),
                    None => {
                        self.store.reject_browser_launch_without_reservation(
                            receipt.operation_id,
                            receipt.operation_kind,
                            receipt.input_hash,
                            receipt.entity_id,
                            &format!("{error:#}"),
                        )?;
                        Err(error)
                    }
                }
            }
        }
    }

    fn record_permanent_resume_rejection(
        &self,
        session_id: &str,
        error: &anyhow::Error,
        attempted: Option<&PreparedResumeIdentity>,
    ) -> Result<()> {
        let Some(category) = permanent_resume_rejection_category(error) else {
            return Ok(());
        };
        let reason = format!("{error:#}");
        let compatibility = error
            .chain()
            .find_map(|cause| {
                cause.downcast_ref::<crate::provider_compatibility::CompatibilityError>()
            })
            .map(crate::provider_compatibility::CompatibilityError::explanation);
        self.store.record_permanent_resume_rejection(
            session_id,
            category,
            &reason,
            attempted,
            compatibility,
        )?;
        Ok(())
    }

    pub fn dispatch_reserved_review(
        &self,
        request: &crate::review::ReviewDispatch,
    ) -> Result<ValidationLaunchResult> {
        let prompt=format!("{}\n\nReview request: {}\nCandidate hash: {}\nStructured handoff: {}\nInclude review_request_id, review_kind, and candidate_hash in report metadata.",
            request.prompt,request.request_id,request.candidate_hash,serde_json::to_string(&request.handoff)?);
        self.dispatch_role(
            &request.attempt_id,
            request.role,
            &prompt,
            Some(&request.request_id),
        )
    }

    fn dispatch_role(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        prompt: &str,
        review_request: Option<&str>,
    ) -> Result<ValidationLaunchResult> {
        let context = self.store.role_launch_context(attempt_id, role)?;
        self.dispatch_context(
            attempt_id,
            role,
            prompt,
            review_request,
            context,
            None,
            None,
            None,
        )
    }

    pub fn dispatch_switch(&self, intent_id: &str, prompt: &str) -> Result<ValidationLaunchResult> {
        let (attempt, role_kind, handoff) = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT attempt_id,role,handoff_json FROM switch_intents WHERE id=?1 AND state='ready_for_dispatch'",
                rusqlite::params![intent_id],
                |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?)),
            )?
        };
        let role_kind: crate::domain::RoleKind =
            role_kind.parse().map_err(|error: String| anyhow!(error))?;
        let mut invocation_prompt = prompt.to_owned();
        let review_request = if role_kind.is_reviewer() {
            let kind = match role_kind {
                crate::domain::RoleKind::PlanReviewer => "plan",
                crate::domain::RoleKind::CodeReviewer => "code",
                crate::domain::RoleKind::FinalReviewer => "final",
                _ => unreachable!(),
            };
            let dispatch = self.reviews.reserve_request(
                &attempt,
                kind,
                prompt,
                serde_json::from_str(&handoff)?,
            )?;
            invocation_prompt = format!("{}\n\nReview request: {}\nCandidate hash: {}\nStructured handoff: {}\nInclude review_request_id, review_kind, and candidate_hash in report metadata.",dispatch.prompt,dispatch.request_id,dispatch.candidate_hash,serde_json::to_string(&dispatch.handoff)?);
            Some(dispatch.request_id)
        } else {
            None
        };
        let context = self.store.switched_role_launch_context(intent_id)?;
        self.dispatch_context(
            &attempt,
            role_kind,
            &invocation_prompt,
            review_request.as_deref(),
            context,
            None,
            Some(intent_id),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_role_switch(
        &self,
        operation_id: &str,
        attempt_id: &str,
        role: &str,
        old_generation_id: &str,
        settings_revision: i64,
        snapshot_id: &str,
        handoff: serde_json::Value,
        expected_task_version: i64,
    ) -> Result<String> {
        self.roles.request_switch(
            operation_id,
            attempt_id,
            role,
            old_generation_id,
            settings_revision,
            snapshot_id,
            handoff,
            expected_task_version,
        )
    }

    fn dispatch_context(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        prompt: &str,
        review_request: Option<&str>,
        context: crate::store::RoleLaunchContext,
        validation: Option<(&str, &str, &str)>,
        switch_intent: Option<&str>,
        runtime_probe_policy: Option<&providers::RuntimeProbeCommandPolicy>,
    ) -> Result<ValidationLaunchResult> {
        self.dispatch_context_inner(
            attempt_id,
            role,
            prompt,
            review_request,
            context,
            validation,
            switch_intent,
            runtime_probe_policy,
            None,
            None,
        )
        .and_then(browser_launch_dispatch_result)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_context_with_browser_receipt(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        prompt: &str,
        review_request: Option<&str>,
        context: crate::store::RoleLaunchContext,
        validation: Option<(&str, &str, &str)>,
        switch_intent: Option<&str>,
        runtime_probe_policy: Option<&providers::RuntimeProbeCommandPolicy>,
        browser_receipt: BrowserLaunchReceipt<'_>,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<BrowserLaunchDispatch> {
        self.dispatch_context_inner(
            attempt_id,
            role,
            prompt,
            review_request,
            context,
            validation,
            switch_intent,
            runtime_probe_policy,
            Some(browser_receipt),
            fresh_resume_rejection,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_context_inner(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        prompt: &str,
        review_request: Option<&str>,
        context: crate::store::RoleLaunchContext,
        validation: Option<(&str, &str, &str)>,
        switch_intent: Option<&str>,
        runtime_probe_policy: Option<&providers::RuntimeProbeCommandPolicy>,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<BrowserLaunchDispatch> {
        self.store.require_execution_unheld("role launches")?;
        let release_permit = |error: anyhow::Error, reason: &str| match self
            .store
            .release_unconsumed_launch_permits(Some(&context.permit_id), reason)
        {
            Ok(_) => error,
            Err(cleanup) => {
                let original = format!("{error:#}");
                error.context(format!(
                    "launch permit cleanup also failed after `{original}`: {cleanup:#}"
                ))
            }
        };
        if !self.dispatch_enabled() {
            return Err(release_permit(
                anyhow!("service is draining; new role dispatch is disabled"),
                "dispatch rejected while service draining",
            ));
        }
        let read_denials = match crate::trip::setup_target_read_denials(&self.store, attempt_id) {
            Ok(paths) => paths,
            Err(error) => {
                return Err(release_permit(
                    error,
                    "setup target read confinement could not be established",
                ))
            }
        };
        if runtime_probe_policy.is_some() && !read_denials.is_empty() {
            return Err(release_permit(
                anyhow!("runtime probe launch cannot inherit setup target read denials"),
                "runtime probe policy was mixed with setup confinement",
            ));
        }
        let prepared_launch = if let Some(policy) = runtime_probe_policy {
            providers::prepare_runtime_probe_role_launch_with_bundles(
                context.config.provider,
                role,
                &context.config.model,
                &context.config.effort,
                &context.workspace,
                prompt,
                &self.paths.role_socket,
                &context.token,
                &context.role_generation_id,
                &context.session_id,
                None,
                &self.hooks,
                &self.executable,
                policy,
                &self.store.compatibility_bundles,
            )
        } else {
            providers::prepare_role_launch_with_read_denials_and_bundles(
                context.config.provider,
                role,
                &context.config.model,
                &context.config.effort,
                &context.workspace,
                prompt,
                &self.paths.role_socket,
                &context.token,
                &context.role_generation_id,
                &context.session_id,
                None,
                &self.hooks,
                &self.executable,
                &read_denials,
                &self.store.compatibility_bundles,
            )
        };
        let launch = match prepared_launch {
            Ok(launch) => launch,
            Err(error) => return Err(release_permit(error, "provider launch preparation failed")),
        };
        if runtime_probe_policy.is_some() {
            if let Err(error) = crate::trip::require_runtime_probe_launch_policy(
                &self.store,
                attempt_id,
                role,
                &launch.config,
            ) {
                return Err(release_permit(
                    error,
                    "runtime probe policy differs from the authoritative frozen record",
                ));
            }
        }
        let reservation = if let Some(receipt) = browser_receipt {
            if let Some(fresh_resume_rejection) = fresh_resume_rejection {
                self.store
                    .reserve_role_invocation_with_browser_receipt_and_fresh_rejection(
                        &context,
                        &launch.config,
                        receipt,
                        fresh_resume_rejection,
                    )
            } else {
                self.store.reserve_role_invocation_with_browser_receipt(
                    &context,
                    &launch.config,
                    receipt,
                )
            }
        } else {
            self.store
                .reserve_role_invocation(&context, &launch.config)
                .map(|()| BrowserLaunchReservation::Reserved)
        };
        match reservation {
            Ok(BrowserLaunchReservation::Reserved) => {}
            Ok(BrowserLaunchReservation::Existing(existing)) => {
                return Ok(BrowserLaunchDispatch::Existing(existing));
            }
            Err(error) => {
                return Err(release_permit(
                    error,
                    "authoritative role reservation rejected",
                ));
            }
        }
        if let Some((cell, operation, hash)) = validation {
            self.store
                .reserve_workflow_validation(&context.session_id, cell, operation, hash)?;
        }
        self.store
            .bind_invocation_input(&context.session_id, prompt, review_request)?;
        if let Some(request) = review_request {
            self.reviews.bind_launch_intent(
                request,
                &context.session_id,
                &context.role_generation_id,
                context.settings_revision,
            )?;
        }
        if let Some(intent) = switch_intent {
            self.store
                .mark_switch_dispatched(intent, &context.role_generation_id)?;
        }
        let process = if self.synthetic_dispatch_for_tests {
            self.store.mark_session_spawning(
                &context.session_id,
                &context.transcript_epoch,
                "synthetic-dispatch-no-process",
            )?;
            ProcessIdentity {
                pid: std::process::id(),
                process_group_id: 0,
                native_start_marker: "synthetic-dispatch-no-process".into(),
                observed_started_at: Utc::now().to_rfc3339(),
            }
        } else {
            match self.supervisor.spawn(
                &context.session_id,
                &context.role_generation_id,
                &context.transcript_epoch,
                &launch,
            ) {
                Ok(process) => process,
                Err(error) => {
                    let (delivery_unknown, retryable) =
                        self.record_spawn_failure(&context.session_id, &error, false)?;
                    if let Some(request) = review_request {
                        self.reviews.mark_delivery_failure(
                            request,
                            Some(&context.session_id),
                            delivery_unknown,
                            error.reason(),
                        )?;
                    }
                    if retryable {
                        if let Some(intent) = switch_intent {
                            self.store.restore_switch_after_proven_nondelivery(
                                intent,
                                &context.role_generation_id,
                            )?;
                        }
                    }
                    if let Some((_, operation, _)) = validation {
                        self.store.finalize_workflow_validation(
                            operation,
                            &serde_json::json!({"state":if delivery_unknown{"delivery_unknown"}else if retryable{"launch_failed"}else{"provider_nondelivery_cleanup_unknown"},"session_id":context.session_id,"reason":error.reason()}),
                        )?;
                    }
                    return Err(error.into());
                }
            }
        };
        if let Err(error) = self.store.update_session_running(
            &context.session_id,
            &context.transcript_epoch,
            &serde_json::to_string(&process)?,
        ) {
            let _ = self.supervisor.interrupt(&context.session_id);
            if let Some(request) = review_request {
                self.reviews.mark_delivery_failure(
                    request,
                    Some(&context.session_id),
                    true,
                    &format!("{error:#}"),
                )?;
            }
            self.store
                .mark_session_delivery_ambiguous(&context.session_id, &format!("{error:#}"))?;
            if let Some((_, operation, _)) = validation {
                self.store.finalize_workflow_validation(
                    operation,
                    &serde_json::json!({"state":"delivery_unknown","session_id":context.session_id,"reason":format!("{error:#}")}),
                )?;
            }
            return Err(error.context("provider spawned but durable running state is ambiguous"));
        }
        if let Some(request) = review_request {
            if let Err(error) = self.reviews.bind_delivery(
                request,
                &context.session_id,
                &context.role_generation_id,
                context.settings_revision,
            ) {
                let _ = self.supervisor.interrupt(&context.session_id);
                self.reviews.mark_delivery_failure(
                    request,
                    Some(&context.session_id),
                    true,
                    &format!("review delivery binding failed after native spawn: {error:#}"),
                )?;
                self.store.mark_session_delivery_ambiguous(
                    &context.session_id,
                    &format!("review delivery binding failed after native spawn: {error:#}"),
                )?;
                if let Some((_, operation, _)) = validation {
                    self.store.finalize_workflow_validation(
                        operation,
                        &serde_json::json!({"state":"delivery_unknown","session_id":context.session_id,"reason":format!("{error:#}")}),
                    )?;
                }
                return Err(error.context("review delivery state is ambiguous"));
            }
        }
        self.role_tokens
            .write()
            .map_err(|_| anyhow!("role token map poisoned"))?
            .insert(context.session_id.clone(), context.token);
        let result = ValidationLaunchResult {
            session_id: context.session_id,
            task_id: context.task_id,
            attempt_id: attempt_id.to_owned(),
            role_generation_id: context.role_generation_id,
            launch: launch.config,
            process,
            status: if validation.is_some() {
                "running_workflow_validation_unverified".into()
            } else {
                "running".into()
            },
        };
        if let Some((_, operation, _)) = validation {
            self.store
                .finalize_workflow_validation(operation, &serde_json::to_value(&result)?)?;
        }
        Ok(BrowserLaunchDispatch::Launched(result))
    }

    fn record_spawn_failure(
        &self,
        session_id: &str,
        failure: &SpawnFailure,
        resumed: bool,
    ) -> Result<(bool, bool)> {
        match failure {
            SpawnFailure::ProvenNondelivery {
                reason,
                owned_process_state,
                ..
            } => {
                let (root_pid, process_group_id, members) = owned_process_state.evidence();
                self.store.record_session_spawn_uncertainty(
                    session_id,
                    root_pid,
                    process_group_id,
                    members,
                )?;
                if owned_process_state.is_quiescent() {
                    if resumed {
                        self.store
                            .restore_resume_after_proven_nondelivery(session_id, reason)?;
                    } else {
                        self.store
                            .update_session_launch_failed(session_id, reason)?;
                    }
                    Ok((false, true))
                } else {
                    self.store
                        .hold_session_after_proven_nondelivery(session_id, reason, resumed)?;
                    Ok((false, false))
                }
            }
            SpawnFailure::DeliveryUnknown {
                reason,
                root_pid,
                process_group_id,
                members,
            } => {
                self.store.record_session_spawn_uncertainty(
                    session_id,
                    *root_pid,
                    *process_group_id,
                    members,
                )?;
                self.store
                    .mark_session_delivery_ambiguous(session_id, reason)?;
                Ok((true, false))
            }
        }
    }

    pub fn execute_human_command(&self, command: &HumanCommand) -> Result<OperationResult> {
        if let HumanCommand::ResolveRecovery {
            operation_id,
            recovery_id,
            task_id,
            attempt_id,
            session_id,
            ..
        } = command
        {
            let connection = self.store.lock()?;
            let receipt: Option<(String, String)> = connection.query_row(
                "SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='human_command'",
                rusqlite::params![operation_id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((receipt_hash, result_json)) = receipt {
                if receipt_hash != json_hash(command)? {
                    bail!("operation_id was already used for another request")
                }
                return Ok(serde_json::from_str(&result_json)?);
            } else {
                crate::workflow::validate_recovery_identity(
                    &connection,
                    recovery_id,
                    task_id,
                    attempt_id,
                    session_id.as_deref(),
                )?;
            }
        }
        // Recorded-state commands stay available while inventory is unknown;
        // quiescence proofs re-inventory on their own and fail closed.
        if let ProcessInventory::Unavailable(error) = self.supervisor.reconcile_inventory()? {
            if matches!(
                command,
                HumanCommand::RetryGracefulStop { .. }
                    | HumanCommand::ForceStopExactProcess { .. }
                    | HumanCommand::Trip { .. }
            ) {
                return Err(error.context("LLMRelay cannot act on agent processes yet"));
            }
        }
        match command {
            HumanCommand::RetryWorkspaceReservation {
                operation_id,
                task_id,
                attempt_id,
                workspace_id,
                expected_version,
            } => {
                return self.scheduler.retry_workspace_reservation(
                    operation_id,
                    task_id,
                    attempt_id,
                    workspace_id,
                    *expected_version,
                );
            }
            HumanCommand::CancelWorkspaceReservation {
                operation_id,
                task_id,
                attempt_id,
                workspace_id,
                expected_version,
            } => {
                return self.scheduler.cancel_workspace_reservation(
                    operation_id,
                    task_id,
                    attempt_id,
                    workspace_id,
                    *expected_version,
                );
            }
            HumanCommand::RetryGracefulStop { .. } | HumanCommand::ForceStopExactProcess { .. } => {
                return self.execute_exact_stop_recovery(command);
            }
            _ => {}
        }
        if let HumanCommand::Trip {
            operation_id,
            action,
        } = command
        {
            if let TripHumanAction::StopSetupManager {
                setup_operation_id,
                expected_project_version,
            } = action
            {
                let preparation = crate::trip::prepare_setup_manager_stop(
                    &self.store,
                    operation_id,
                    setup_operation_id,
                    *expected_project_version,
                )?;
                if let Some(result) = preparation.completed {
                    return Ok(result);
                }
                let signal_delivery = match preparation.session_id.as_deref() {
                    Some(session_id) if self.synthetic_dispatch_for_tests => {
                        self.store.mark_interrupt_requested(session_id)?;
                        crate::trip::SetupManagerSignalDelivery::Requested
                    }
                    Some(session_id) => {
                        let outcome = if preparation.retry_signal_delivery {
                            self.supervisor
                                .retry_setup_manager_interrupt_once(session_id)
                        } else {
                            self.supervisor.interrupt_once(session_id)
                        };
                        match outcome {
                            Ok(crate::supervisor::InterruptOutcome::Requested) => {
                                crate::trip::SetupManagerSignalDelivery::Requested
                            }
                            Ok(crate::supervisor::InterruptOutcome::AlreadyRequested) => {
                                crate::trip::SetupManagerSignalDelivery::AlreadyRequested
                            }
                            Ok(crate::supervisor::InterruptOutcome::Stale) => {
                                crate::trip::SetupManagerSignalDelivery::AlreadyQuiescent
                            }
                            Err(error) => crate::trip::SetupManagerSignalDelivery::Failed(format!(
                                "manager-only signal delivery failed: {error:#}"
                            )),
                        }
                    }
                    None => crate::trip::SetupManagerSignalDelivery::NotRunning,
                };
                return crate::trip::finish_setup_manager_stop(
                    &self.store,
                    &preparation,
                    signal_delivery,
                );
            }
            let runtime = crate::trip::CapabilityRuntime::from_store(
                &self.store,
                self.hooks.clone(),
                self.paths.role_socket.clone(),
                self.executable.clone(),
            );
            return crate::trip::execute_human_with_runtime(
                &self.store,
                &self.paths,
                &runtime,
                operation_id,
                action,
            );
        }
        if let HumanCommand::ActivateTaskProfile {
            operation_id,
            task_id,
            role,
            settings_revision,
            expected_version,
        } = command
        {
            let runtime = crate::trip::CapabilityRuntime::from_store(
                &self.store,
                self.hooks.clone(),
                self.paths.role_socket.clone(),
                self.executable.clone(),
            );
            return crate::trip::activate_task_profile(
                &self.store,
                &runtime,
                operation_id,
                task_id,
                *role,
                *settings_revision,
                *expected_version,
            );
        }
        if let HumanCommand::DecidePermission { request_id, .. } = command {
            crate::permissions::validate_request_boot(
                &self.store,
                request_id,
                self.permission_boot_id.as_str(),
            )?;
        }
        let recovery_prior_receipt = if let HumanCommand::ResolveRecovery {
            operation_id,
            recovery_id,
            task_id,
            attempt_id,
            session_id,
            decision,
            ..
        } = command
        {
            let prior_receipt: bool = self.store.lock()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='human_command')",
                rusqlite::params![operation_id], |row| row.get(0),
            )?;
            if !prior_receipt && matches!(decision.as_str(), "confirm_quiescent" | "cancel") {
                let (rework_recovery, coordinator_failure) = {
                    let connection = self.store.lock()?;
                    crate::workflow::validate_recovery_identity(
                        &connection,
                        recovery_id,
                        task_id,
                        attempt_id,
                        session_id.as_deref(),
                    )?;
                    let (workspace_recovery, coordinator_failure): (bool, bool) = connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE id=?1 AND attempt_id=?2
                               AND state='attention_required'
                               AND json_extract(detail_json,'$.kind')='workspace_reservation'),
                                    EXISTS(SELECT 1 FROM recovery_records WHERE id=?1 AND attempt_id=?2
                               AND state='attention_required'
                               AND json_extract(detail_json,'$.kind')='coordinator_failure')",
                            rusqlite::params![recovery_id, attempt_id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )?;
                    if workspace_recovery {
                        bail!("workspace reservation recovery must use retry_workspace_reservation or cancel_workspace_reservation")
                    }
                    let rework_recovery = connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM rework_intents WHERE new_attempt_id=?1 AND state='recovery_required')",
                        rusqlite::params![attempt_id],
                        |row| row.get::<_, bool>(0),
                    )?;
                    (rework_recovery, coordinator_failure)
                };
                if rework_recovery || coordinator_failure {
                    // Checked inside the workflow transaction; no process inventory applies here.
                } else if decision == "confirm_quiescent" {
                    if let Some(session) = session_id.as_deref() {
                        crate::recovery::verify_session_quiescent(&self.store, session)?;
                    } else {
                        crate::recovery::verify_attempt_quiescent(&self.store, attempt_id)?;
                    }
                } else if !rework_recovery {
                    crate::recovery::verify_attempt_quiescent(&self.store, attempt_id)?;
                }
            }
            prior_receipt
        } else {
            false
        };
        let runtime = crate::trip::CapabilityRuntime::from_store(
            &self.store,
            self.hooks.clone(),
            self.paths.role_socket.clone(),
            self.executable.clone(),
        );
        let result = crate::workflow::execute_with_runtime(&self.store, command, Some(&runtime))?;
        if !recovery_prior_receipt {
            if let HumanCommand::ResolveRecovery {
                recovery_id,
                session_id,
                decision,
                ..
            } = command
            {
                if decision == "confirm_quiescent" {
                    if let Some(session_id) = session_id.as_deref() {
                        self.store
                            .preserve_proven_nondelivery_recovery_detail(session_id, recovery_id)?;
                    }
                }
            }
        }
        if matches!(
            result.entity_kind.as_str(),
            "permission_request" | "permission_rule"
        ) {
            self.permission_notify.notify_waiters();
        }
        if result.entity_kind == "attempt" && result.state == "rework_created" {
            self.prepare_rework(&result.entity_id)?;
        }
        Ok(result)
    }

    fn execute_exact_stop_recovery(&self, command: &HumanCommand) -> Result<OperationResult> {
        let (
            operation_id,
            task_id,
            session_id,
            generation_id,
            transcript_epoch,
            process_identity,
            expected_version,
            action,
        ) = match command {
            HumanCommand::RetryGracefulStop {
                operation_id,
                task_id,
                session_id,
                role_generation_id,
                transcript_epoch,
                process_identity,
                expected_version,
            } => (
                operation_id,
                task_id,
                session_id,
                role_generation_id,
                transcript_epoch,
                process_identity,
                *expected_version,
                "retry_graceful_stop",
            ),
            HumanCommand::ForceStopExactProcess {
                operation_id,
                task_id,
                session_id,
                role_generation_id,
                transcript_epoch,
                process_identity,
                expected_version,
            } => (
                operation_id,
                task_id,
                session_id,
                role_generation_id,
                transcript_epoch,
                process_identity,
                *expected_version,
                "force_stop_exact_process",
            ),
            _ => bail!("exact stop recovery command is required"),
        };
        let request_hash = json_hash(command)?;
        if let Some(receipt) = self.store.operation_receipt(
            operation_id,
            "human_control",
            "session_stop_recovery",
            &request_hash,
        )? {
            return Ok(serde_json::from_value(receipt)?);
        }
        let now = Utc::now().to_rfc3339();
        {
            let mut connection = self.store.lock()?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current: Option<String> = transaction
                .query_row(
                    "SELECT s.process_identity_json FROM sessions s
                     JOIN role_generations rg ON rg.id=s.role_generation_id
                     JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                     WHERE t.id=?1 AND t.version=?2 AND s.id=?3 AND rg.id=?4
                       AND s.transcript_epoch=?5 AND s.status IN ('interrupt_requested','recovery_required')
                       AND s.interrupt_requested_at IS NOT NULL
                       AND EXISTS(SELECT 1 FROM recovery_records r WHERE r.session_id=s.id
                         AND r.state='attention_required'
                         AND json_extract(r.detail_json,'$.kind')='graceful_stop_deadline')",
                    rusqlite::params![task_id, expected_version, session_id, generation_id, transcript_epoch],
                    |row| row.get(0),
                )
                .optional()?;
            let current =
                current.ok_or_else(|| anyhow!("exact graceful-stop recovery binding is stale"))?;
            let stored_identity: serde_json::Value = serde_json::from_str(&current)?;
            if stored_identity != *process_identity {
                bail!("exact graceful-stop process identity changed")
            }
            let pending = OperationResult {
                operation_id: operation_id.to_owned(),
                entity_kind: "session".to_owned(),
                entity_id: session_id.to_owned(),
                version: Some(expected_version),
                state: "stop_recovery_reserved".to_owned(),
                detail: serde_json::json!({"action":action,"automatic":false}),
            };
            transaction.execute(
                "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
                 VALUES(?1,'human_control','session_stop_recovery',?2,?3,?4)",
                rusqlite::params![operation_id, request_hash, serde_json::to_string(&pending)?, now],
            )?;
            transaction.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?2,'human','session.graceful_stop.recovery_reserved','session',?3,?4,?5)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    operation_id,
                    session_id,
                    serde_json::json!({
                        "action":action,"role_generation_id":generation_id,
                        "transcript_epoch":transcript_epoch,"process_identity":process_identity,
                    }).to_string(),
                    now,
                ],
            )?;
            transaction.commit()?;
        }
        let delivery = if action == "retry_graceful_stop" {
            self.supervisor.retry_graceful_stop_exact(session_id)
        } else {
            self.supervisor.force_stop_exact_managed_process(session_id)
        };
        let result = match delivery {
            Ok(()) => OperationResult {
                operation_id: operation_id.to_owned(),
                entity_kind: "session".to_owned(),
                entity_id: session_id.to_owned(),
                version: Some(expected_version),
                state: if action == "retry_graceful_stop" {
                    "graceful_stop_retried".to_owned()
                } else {
                    "force_stop_requested".to_owned()
                },
                detail: serde_json::json!({
                    "action":action,"automatic":false,"quiescence_required":true,
                    "replacement_or_resume":false,
                }),
            },
            Err(error) => OperationResult {
                operation_id: operation_id.to_owned(),
                entity_kind: "session".to_owned(),
                entity_id: session_id.to_owned(),
                version: Some(expected_version),
                state: "stop_recovery_required".to_owned(),
                detail: serde_json::json!({
                    "action":action,"automatic":false,"reason":format!("{error:#}"),
                }),
            },
        };
        let mut connection = self.store.lock()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2
             AND actor_key='human_control' AND operation_kind='session_stop_recovery' AND request_hash=?3",
            rusqlite::params![serde_json::to_string(&result)?, operation_id, request_hash],
        )?;
        if changed != 1 {
            bail!("exact stop recovery receipt is missing or changed")
        }
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human','session.graceful_stop.recovery_finalized','session',?3,?4,?5)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(), operation_id, session_id,
                serde_json::to_string(&result)?, Utc::now().to_rfc3339(),
            ],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub(crate) fn rework_control_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.rework_lock
            .lock()
            .map_err(|_| anyhow!("rework mutex is poisoned"))
    }

    pub(crate) fn prepare_rework(&self, attempt_id: &str) -> Result<bool> {
        use rusqlite::params;
        let _rework_guard = self.rework_control_guard()?;
        let (intent, parent, task, identity, snapshot, intent_state, prior_result): (
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
        ) = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT ri.id,ri.parent_attempt_id,a.task_id,p.repository_identity,ri.snapshot_id,ri.state,ri.result_json
                 FROM rework_intents ri JOIN attempts a ON a.id=ri.new_attempt_id
                 JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
                 WHERE a.id=?1",
                params![attempt_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?
        };
        match intent_state.as_str() {
            "completed" => return Ok(true),
            "cancelled" | "paused" => return Ok(false),
            "recovery_required" => {
                let reason = prior_result
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                    .and_then(|value| value.get("reason")?.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "rework materialization requires recovery".to_owned());
                bail!("rework workspace outcome requires recovery: {reason}")
            }
            "reserved" | "materializing" => {}
            state => bail!("rework intent has unsupported state {state}"),
        }
        let workspace_id = uuid::Uuid::new_v4().to_string();
        let destination = self.paths.artifacts.join("worktrees").join(attempt_id);
        let base: String = {
            let connection = self.store.lock()?;
            connection.query_row(
                "SELECT snapshot_base FROM snapshots WHERE id=?1",
                params![snapshot],
                |row| row.get(0),
            )?
        };
        let now = chrono::Utc::now().to_rfc3339();
        {
            let mut connection = self.store.lock()?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Nothing is created on disk and no claim moves until every parent
            // role is proven quiescent; until then the lineage stays reserved.
            if rework_parent_blocker(&transaction, &parent)?.is_some() {
                return Ok(false);
            }
            let reserved = transaction.execute(
                "UPDATE rework_intents SET state='materializing',updated_at=?1
                 WHERE id=?2 AND state IN ('reserved','materializing')
                   AND EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
                     WHERE a.id=?3 AND a.status='materialization_pending'
                       AND t.lifecycle='in_progress' AND t.attention='none')
                   AND NOT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?3
                     AND c.state IN ('requested','draining')
                     AND c.kind IN ('pause_now','pause_after_role','cancel'))
                   AND NOT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?3
                     AND c.kind IN ('manager_stop','manager_change')
                     AND c.state NOT IN ('finished','cancelled','superseded','rejected'))",
                params![now, intent, attempt_id],
            )?;
            if reserved != 1 {
                return Ok(false);
            }
            transaction.execute("INSERT OR IGNORE INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5,?6,'reserved',?7,?7)",params![workspace_id,attempt_id,identity,destination.to_string_lossy(),base,serde_json::json!({"lineage_snapshot_id":snapshot,"no_auto_commit":true,"no_auto_merge":true}).to_string(),now])?;
            transaction.commit()?;
        }
        let materialized = if destination.exists() {
            self.reviews.verify_rework_source(&intent, &destination)
        } else {
            self.reviews
                .materialize_rework(&snapshot, &destination)
                .and_then(|_| self.reviews.verify_rework_source(&intent, &destination))
        };
        if !matches!(&materialized, Ok(true)) {
            let reason = materialized
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "materialized bytes did not verify".into());
            let mut connection = self.store.lock()?;
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let failed = chrono::Utc::now().to_rfc3339();
            let recorded = transaction.execute(
                "UPDATE rework_intents SET state='recovery_required',result_json=?1,updated_at=?2
                 WHERE id=?3 AND state='materializing'
                   AND EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
                     WHERE a.id=?4 AND a.status='materialization_pending'
                       AND t.lifecycle='in_progress' AND t.attention='none')
                   AND NOT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?4
                     AND c.state IN ('requested','draining')
                     AND c.kind IN ('pause_now','pause_after_role','cancel'))
                   AND NOT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?4
                     AND c.kind IN ('manager_stop','manager_change')
                     AND c.state NOT IN ('finished','cancelled','superseded','rejected'))",
                params![
                    serde_json::json!({"reason":reason.clone()}).to_string(),
                    failed,
                    intent,
                    attempt_id
                ],
            )?;
            if recorded == 1 {
                transaction.execute(
                    "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2 AND status='materialization_pending'",
                    params![failed, attempt_id],
                )?;
                transaction.execute(
                    "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2 AND lifecycle='in_progress' AND attention='none'",
                    params![failed, task],
                )?;
            } else {
                transaction.execute(
                    "UPDATE rework_intents SET result_json=?1,updated_at=?2 WHERE id=?3 AND state='materializing'",
                    params![serde_json::json!({"reason":reason.clone(),"deferred_to_pending_control":true}).to_string(),failed,intent],
                )?;
            }
            transaction.commit()?;
            bail!("rework workspace outcome requires recovery: {reason}")
        }
        if let Err(error) =
            crate::trip::materialize_project_policy(&self.store, attempt_id, &destination)
        {
            let reason = format!("TRIP policy materialization failed: {error:#}");
            let now = chrono::Utc::now().to_rfc3339();
            let connection = self.store.lock()?;
            connection.execute(
                "UPDATE rework_intents SET state='recovery_required',result_json=?1,updated_at=?2 WHERE id=?3",
                params![serde_json::json!({"reason":reason}).to_string(),now,intent],
            )?;
            connection.execute(
                "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
                params![now, attempt_id],
            )?;
            connection.execute(
                "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2",
                params![now, task],
            )?;
            bail!("{reason}")
        }
        let mut connection = self.store.lock()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let finished = chrono::Utc::now().to_rfc3339();
        let eligible: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM rework_intents ri
             JOIN attempts a ON a.id=ri.new_attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE ri.id=?1 AND ri.state='materializing' AND a.id=?2
               AND a.status='materialization_pending' AND t.lifecycle='in_progress'
               AND t.attention='none' AND NOT EXISTS(SELECT 1 FROM controls c
                 WHERE c.attempt_id=a.id AND c.state IN ('requested','draining')
                   AND c.kind IN ('pause_now','pause_after_role','cancel'))
               AND NOT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=a.id
                 AND c.kind IN ('manager_stop','manager_change')
                 AND c.state NOT IN ('finished','cancelled','superseded','rejected')))",
            params![intent, attempt_id],
            |row| row.get(0),
        )?;
        if !eligible {
            return Ok(false);
        }
        if let Some(blocker) = rework_parent_blocker(&transaction, &parent)? {
            let reason = format!("parent ownership changed during materialization: {blocker}");
            transaction.execute(
                "UPDATE rework_intents SET state='recovery_required',result_json=?1,updated_at=?2 WHERE id=?3 AND state='materializing'",
                params![serde_json::json!({"reason":reason,"claim_retained":true}).to_string(),finished,intent],
            )?;
            transaction.execute(
                "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2 AND status='materialization_pending'",
                params![finished, attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2 AND lifecycle='in_progress' AND attention='none'",
                params![finished, task],
            )?;
            transaction.commit()?;
            bail!("{reason}")
        }
        let policy_json: String = transaction.query_row(
            "SELECT policy_json FROM workspaces WHERE attempt_id=?1 AND state='reserved'",
            params![attempt_id],
            |row| row.get(0),
        )?;
        crate::trip::validate_materialized_policy(&policy_json)?;
        let workspace_changed = transaction.execute(
            "UPDATE workspaces SET state='ready',updated_at=?1 WHERE attempt_id=?2 AND state='reserved'",
            params![finished, attempt_id],
        )?;
        if workspace_changed != 1 {
            bail!("rework workspace changed before atomic publication")
        }
        let moved=transaction.execute("UPDATE claims SET attempt_id=?1,task_id=?2,updated_at=?3 WHERE attempt_id=?4 AND state='running'",params![attempt_id,task,finished,parent])?;
        if moved != 1 {
            let reason = "parent repository claim is not safely transferable";
            transaction.execute(
                "UPDATE rework_intents SET state='recovery_required',result_json=?1,updated_at=?2 WHERE id=?3 AND state='materializing'",
                params![serde_json::json!({"reason":reason,"claim_retained":true}).to_string(),finished,intent],
            )?;
            transaction.execute(
                "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2 AND status='materialization_pending'",
                params![finished, attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2 AND lifecycle='in_progress' AND attention='none'",
                params![finished, task],
            )?;
            transaction.commit()?;
            bail!("{reason}")
        }
        transaction.execute(
            "UPDATE attempts SET status='reworked',updated_at=?1 WHERE id=?2",
            params![finished, parent],
        )?;
        transaction.execute(
            "UPDATE attempts SET status='running',updated_at=?1 WHERE id=?2",
            params![finished, attempt_id],
        )?;
        transaction.execute(
            "UPDATE rework_intents SET state='completed',result_json=?1,updated_at=?2 WHERE id=?3",
            params![
                serde_json::json!({"workspace":destination,"claim_transferred":true}).to_string(),
                finished,
                intent
            ],
        )?;
        transaction.execute(
            "UPDATE tasks SET attention='none',updated_at=?1 WHERE id=?2",
            params![finished, task],
        )?;
        transaction.commit()?;
        Ok(true)
    }
}

/// Parent state that must be settled before a rework child's workspace is
/// created or the repository claim moves. `?1` is the parent attempt. A
/// generation must be terminal and its sessions must have recorded process-group
/// quiescence; an exited session alone cannot release role capacity.
const REWORK_PARENT_FENCES: &[(&str, &str)] = &[
    ("a parent role is still live",
     "SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1
        AND status NOT IN ('exited','launch_failed','replaced','revoked'))"),
    ("a parent session has not exited with proven process-group quiescence",
     "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
        WHERE g.attempt_id=?1
          AND NOT (s.status='launch_failed'
            OR (s.status='exited'
              AND COALESCE(CASE WHEN json_valid(s.exit_json)
                THEN json_extract(s.exit_json,'$.process_group_quiescent') END,0)=1)))"),
    ("a parent review is in flight or its delivery is uncertain",
     "SELECT EXISTS(SELECT 1 FROM review_requests WHERE attempt_id=?1
        AND delivery_state IN ('launching','delivered','ambiguous'))"),
    ("a parent check is running or needs recovery",
     "SELECT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1
        AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))"),
    ("a parent capture is in progress",
     "SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1
        AND state IN ('reserved','capturing','recovery_required'))"),
    ("a parent permission request is pending",
     "SELECT EXISTS(SELECT 1 FROM permission_requests WHERE attempt_id=?1
        AND consumed_at IS NULL AND delivery_state NOT IN ('expired','not_delivered')
        AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
          WHERE native.permission_request_id=permission_requests.id))"),
    ("someone has keyboard control of a parent agent",
     "SELECT EXISTS(SELECT 1 FROM input_leases lease JOIN sessions s ON s.id=lease.session_id
        JOIN role_generations g ON g.id=s.role_generation_id
        WHERE g.attempt_id=?1 AND lease.revoked_at IS NULL
          AND julianday(lease.expires_at)>julianday('now'))"),
    ("parent guidance is submitted or its delivery is unconfirmed",
     "SELECT EXISTS(SELECT 1 FROM guidance_messages WHERE attempt_id=?1
        AND state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown','submitted'))"),
    ("a parent recovery is open",
     "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1
        AND state='attention_required')"),
    ("a parent restart hold is open",
     "SELECT EXISTS(SELECT 1 FROM restart_candidates WHERE attempt_id=?1
        AND state NOT IN ('resumed','released_fresh_dispatch','cancelled'))"),
    ("a parent agent switch is pending",
     "SELECT EXISTS(SELECT 1 FROM switch_intents WHERE attempt_id=?1
        AND state NOT IN ('dispatched','completed','cancelled','rejected','superseded'))"),
    ("a parent manager stop or change is unresolved",
     "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
        AND kind IN ('manager_stop','manager_change')
        AND state NOT IN ('finished','cancelled','superseded','rejected'))"),
    ("a parent control is pending",
     "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
        AND kind NOT IN ('transition_proposal','manager_stop','manager_change')
        AND state IN ('requested','draining','held','recovery_required'))"),
    ("the parent repository claim is not exactly one running claim",
     "SELECT NOT ((SELECT COUNT(*) FROM claims WHERE attempt_id=?1 AND state='running')=1
        AND NOT EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1
          AND state IN ('reserved','launching','unknown','stopping')))"),
];

fn rework_parent_blocker(
    connection: &rusqlite::Connection,
    parent: &str,
) -> Result<Option<&'static str>> {
    for (blocker, sql) in REWORK_PARENT_FENCES {
        let blocked: bool =
            connection.query_row(sql, rusqlite::params![parent], |row| row.get(0))?;
        if blocked {
            return Ok(Some(*blocker));
        }
    }
    if crate::store::provider_failure_hold_in_attempt(connection, parent)?.is_some() {
        return Ok(Some("a parent role is on hold after a provider failure"));
    }
    Ok(None)
}

fn auto_resume_failure(
    error: anyhow::Error,
    session: &str,
    attempt: &str,
    effect: StepEffect,
) -> anyhow::Error {
    crate::coordinator::subject_failure(
        error,
        attempt,
        crate::coordinator::SubjectStep::AutoResume,
        effect,
        serde_json::json!({"session_id":session}),
    )
}

fn restart_due(next_due_at: &Option<String>, now: &chrono::DateTime<Utc>) -> Result<bool> {
    let Some(next_due_at) = next_due_at.as_deref() else {
        return Ok(true);
    };
    let due = chrono::DateTime::parse_from_rfc3339(next_due_at)
        .map_err(|_| anyhow!("restart candidate next_due_at is malformed"))?;
    Ok(due.with_timezone(&Utc) <= now.clone())
}

fn restart_capacity_delay(deferrals: u32) -> i64 {
    match deferrals {
        0 | 1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        5 => 30,
        _ => 60,
    }
}

fn restart_operation_result(
    operation_id: Option<&str>,
    mode: &str,
    selected: &[String],
    queued: &[String],
    omitted: &[String],
    outcomes: Vec<serde_json::Value>,
) -> serde_json::Value {
    let state = if outcomes.iter().any(|outcome| {
        outcome.get("state").and_then(serde_json::Value::as_str) == Some("admitting")
    }) {
        "admitting"
    } else if outcomes
        .iter()
        .any(|outcome| outcome.get("state").and_then(serde_json::Value::as_str) == Some("resumed"))
    {
        "resumed"
    } else if !queued.is_empty() {
        "queued"
    } else {
        "completed"
    };
    serde_json::json!({
        "operation_id":operation_id,
        "mode":mode,
        "state":state,
        "selected_ids":selected,
        "queued_ids":queued,
        "omitted_ids":omitted,
        "omitted_count":omitted.len(),
        "outcomes":outcomes,
    })
}

/// The candidate stays as it was; the restart waits for its hold to end.
fn held_restart_outcome(
    session: &str,
    held: &crate::store::ProviderFailureHeld,
) -> serde_json::Value {
    serde_json::json!({
        "session_id":session,"state":"held","reason":held.to_string(),
        "reason_code":"restart.provider_failure_hold","hold_id":held.hold_id,
    })
}

fn restart_receipt_in(
    tx: &Transaction<'_>,
    operation_id: &str,
    request_hash: &str,
) -> Result<Option<serde_json::Value>> {
    let receipt: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash,result_json FROM operation_receipts
             WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='restart_resume'",
            rusqlite::params![operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((stored_hash, result_json)) = receipt else {
        return Ok(None);
    };
    if stored_hash != request_hash {
        bail!("operation ID was already used with different input")
    }
    Ok(Some(serde_json::from_str(&result_json)?))
}

fn insert_restart_receipt_in(
    tx: &Transaction<'_>,
    operation_id: &str,
    request_hash: &str,
    result: &serde_json::Value,
    now: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
         VALUES(?1,'human_control','restart_resume',?2,?3,?4)",
        rusqlite::params![operation_id, request_hash, result.to_string(), now],
    )?;
    Ok(())
}

fn update_restart_receipt_in(
    tx: &Transaction<'_>,
    ticket: &RestartAdmissionTicket,
    result: &serde_json::Value,
) -> Result<()> {
    let Some((operation_id, request_hash)) = ticket.receipt.as_ref() else {
        return Ok(());
    };
    let changed = tx.execute(
        "UPDATE operation_receipts SET result_json=?1
         WHERE operation_id=?2 AND actor_key='human_control' AND operation_kind='restart_resume'
           AND request_hash=?3",
        rusqlite::params![result.to_string(), operation_id, request_hash],
    )?;
    if changed != 1 {
        bail!("durable restart operation receipt changed before outcome publication")
    }
    Ok(())
}

fn block_unreserved_stale_admission(
    tx: &Transaction<'_>,
    ticket: &RestartAdmissionTicket,
    raw_result: &str,
    mut candidate_result: RestartCandidateResult,
    generation_current: bool,
    now: &str,
) -> Result<serde_json::Value> {
    let reason = "exact restart authority changed before the resume was reserved; nothing was delivered and a human decision is required";
    candidate_result.restart.terminalize_batch();
    candidate_result.set(
        "startup_admission_reconciliation",
        serde_json::json!({
            "delivery":"proven_nondelivery_or_preflight",
            "authority":"stale",
            "invocation_state":"not_reserved",
            "expected_resume_ordinal":ticket.expected_resume_ordinal,
            "prior_transcript_epoch":ticket.prior_transcript_epoch,
            "admission_id":ticket.admission_id,
            "role_generation_id":ticket.role_generation_id,
            "recovery_id":serde_json::Value::Null,
        }),
    );
    candidate_result.set(
        "last_resume_outcome",
        serde_json::json!({
            "admission_id":ticket.admission_id,"state":"blocked","delivery":"proven_nondelivery",
            "expected_resume_ordinal":ticket.expected_resume_ordinal,"invocation_state":serde_json::Value::Null
        }),
    );
    let replacement_failures = candidate_result.restart.replacement_failures;
    let changed = tx.execute(
        "UPDATE restart_candidates SET state='blocked',reason=?1,result_json=?2,updated_at=?3
         WHERE session_id=?4 AND state='admitting' AND result_json=?5",
        rusqlite::params![
            reason,
            candidate_result.encode()?,
            now,
            ticket.session_id,
            raw_result
        ],
    )?;
    if changed != 1 {
        return finish_stale_restart_outcome(
            tx,
            ticket,
            "restart candidate changed before its outcome committed",
            now,
        );
    }
    // Only this admission's own running hand-off is undone; a newer control, generation,
    // task decision or still-running restored peer keeps the attempt and task as they are.
    let reparked = generation_current
        && tx.execute(
            "UPDATE attempts SET status='restart_parked',updated_at=?1
             WHERE id=?2 AND task_id=?3 AND status='running'
               AND id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=?3
                       ORDER BY latest.created_at DESC LIMIT 1)
               AND EXISTS(SELECT 1 FROM tasks t WHERE t.id=?3 AND t.attention='none'
                 AND t.archived_at IS NULL AND t.lifecycle IN ('in_progress','validation'))
               AND NOT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?2
                 AND c.kind!='transition_proposal'
                 AND c.state NOT IN ('finished','cancelled','superseded','rejected','failed','abandoned'))
               AND NOT EXISTS(SELECT 1 FROM restart_candidates peer JOIN sessions s ON s.id=peer.session_id
                 WHERE peer.attempt_id=?2 AND peer.session_id!=?4 AND peer.state='resumed'
                   AND s.status='running')",
            rusqlite::params![now, ticket.attempt_id, ticket.task_id, ticket.session_id],
        )? == 1;
    if reparked {
        tx.execute(
            "UPDATE tasks SET attention='restart_parked',updated_at=?1 WHERE id=?2 AND attention='none'",
            rusqlite::params![now, ticket.task_id],
        )?;
    }
    let result = restart_operation_result(
        ticket.receipt.as_ref().map(|value| value.0.as_str()),
        if ticket.automatic { "auto" } else { "selected" },
        &[ticket.session_id.clone()],
        &[],
        &[ticket.session_id.clone()],
        vec![serde_json::json!({
            "session_id":ticket.session_id,"state":"blocked","reason":reason,
            "delivery":"proven_nondelivery","replacement_failures":replacement_failures,
            "admission_id":ticket.admission_id,
        })],
    );
    update_restart_receipt_in(tx, ticket, &result)?;
    Ok(result)
}

fn finish_stale_restart_outcome(
    tx: &Transaction<'_>,
    ticket: &RestartAdmissionTicket,
    reason: &str,
    _now: &str,
) -> Result<serde_json::Value> {
    let result = restart_operation_result(
        ticket.receipt.as_ref().map(|value| value.0.as_str()),
        if ticket.automatic { "auto" } else { "selected" },
        &[ticket.session_id.clone()],
        &[],
        &[ticket.session_id.clone()],
        vec![serde_json::json!({
            "session_id":ticket.session_id,
            "state":"stale_outcome_ignored",
            "reason":reason,
            "admission_id":ticket.admission_id,
        })],
    );
    update_restart_receipt_in(tx, ticket, &result)?;
    Ok(result)
}

fn browser_launch_dispatch_result(result: BrowserLaunchDispatch) -> Result<ValidationLaunchResult> {
    match result {
        BrowserLaunchDispatch::Launched(result) => Ok(result),
        BrowserLaunchDispatch::Existing(receipt) => browser_launch_receipt(receipt),
    }
}

fn browser_launch_receipt(receipt: serde_json::Value) -> Result<ValidationLaunchResult> {
    match receipt.get("state").and_then(serde_json::Value::as_str) {
        Some("reserved") => {
            bail!("the operation is already reserved; refresh and reconcile its durable state")
        }
        Some("rejected") => bail!(
            "the prior browser operation was rejected: {}",
            receipt
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("inspect the authoritative state")
        ),
        _ => serde_json::from_value(receipt).context("parse durable browser launch receipt"),
    }
}

fn permanent_resume_rejection_category(error: &anyhow::Error) -> Option<&'static str> {
    if crate::store::ProviderFailureHeld::in_error(error).is_some() {
        return None;
    }
    if let Some(compatibility) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<crate::provider_compatibility::CompatibilityError>())
    {
        return Some(compatibility.category());
    }
    const FROZEN_IDENTITY_MISMATCH: &str = "executable, hook, security policy, arguments, environment, model, or effort changed; exact resume requires a fresh accounted session";
    const MISSING_FROZEN_IDENTITY: &str =
        "session predates frozen capability identity; exact resume requires a fresh accounted session";
    const UNOBSERVED_HOOK_CANDIDATE: &str =
        "native resume requires an observed managed-descendant hook candidate";

    let reason = format!("{error:#}").to_ascii_lowercase();
    // This rejects a wrong local dispatch entrypoint before a runtime admission is attempted.
    if reason.contains("ordinary runtime probe sessions resume only through resume_runtime_probe") {
        return None;
    }
    if reason.contains("resume input differs from") {
        return None;
    }
    if reason.contains(FROZEN_IDENTITY_MISMATCH) {
        return Some("frozen_runtime_identity_changed");
    }
    if reason.contains(MISSING_FROZEN_IDENTITY) {
        return Some("invocation_provenance_missing");
    }
    if reason.contains(UNOBSERVED_HOOK_CANDIDATE) {
        return Some("hook_trust_unavailable");
    }
    if [
        "capacity",
        "still active",
        "descendant",
        "draining",
        "ownership",
        "quiescen",
    ]
    .iter()
    .any(|needle| reason.contains(needle))
    {
        return None;
    }
    if reason.contains("native session identity") || reason.contains("native candidate") {
        Some("native_history_unavailable")
    } else if reason.contains("hook") {
        Some("hook_trust_unavailable")
    } else if reason.contains("capability") || reason.contains("frozen invocation") {
        Some("frozen_runtime_identity_changed")
    } else if reason.contains("invocation")
        || reason.contains("persisted prompt")
        || reason.contains("workflow")
        || reason.contains("provenance")
    {
        Some("invocation_provenance_missing")
    } else if reason.contains("review")
        || reason.contains("runtime probe")
        || reason.contains("scope")
    {
        Some("stale_review_or_scope")
    } else if reason.contains("generation") {
        Some("stale_generation")
    } else if reason.contains("credential")
        || reason.contains("profile")
        || reason.contains("role changed")
        || reason.contains("authority")
    {
        Some("profile_authority_changed")
    } else if reason.contains("final verifier") || reason.contains("resume is blocked") {
        Some("authority_consumed")
    } else {
        None
    }
}

fn validate_request(request: &ValidationLaunchRequest) -> Result<()> {
    let expected = match request.cell.as_str() {
        "L01" | "L02" => crate::domain::RoleKind::PlanReviewer,
        "L03" | "L04" => crate::domain::RoleKind::Manager,
        "L05" | "L06" => crate::domain::RoleKind::Implementer,
        "L07" | "L08" => crate::domain::RoleKind::CodeReviewer,
        "L09" | "L10" => crate::domain::RoleKind::FinalReviewer,
        _ => bail!("capability route accepts only approved cells L01 through L10"),
    };
    if request.role != expected {
        bail!("{} requires role {}", request.cell, expected)
    }
    if request.prompt.trim().is_empty() {
        bail!("capability prompt cannot be empty")
    }
    if request.model.trim().is_empty() || request.effort.trim().is_empty() {
        bail!("model and effort are required")
    }
    if !request.project_path.is_absolute() {
        bail!("capability project must be an absolute path")
    }
    if !request
        .project_path
        .join(".agenticjira-disposable")
        .is_file()
    {
        bail!("capability project must contain a .agenticjira-disposable marker created by the human test driver")
    }
    Ok(())
}

struct RepositoryIdentity {
    root: PathBuf,
    identity: PathBuf,
    base_revision: String,
}

fn resolve_repository(path: &Path) -> Result<RepositoryIdentity> {
    let root = git(path, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root)
        .canonicalize()
        .context("canonicalize Git root")?;
    let common = git(&root, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(common);
    let identity = if common.is_absolute() {
        common
    } else {
        root.join(common)
    }
    .canonicalize()
    .context("canonicalize Git common directory")?;
    let base_revision = git(&root, &["rev-parse", "HEAD"])?;
    Ok(RepositoryIdentity {
        root,
        identity,
        base_revision,
    })
}

fn git(cwd: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .with_context(|| format!("run git in {}", cwd.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            arguments.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod compatibility_error_tests {
    use super::*;

    #[test]
    fn changed_resume_request_does_not_poison_retained_session() {
        assert_eq!(
            permanent_resume_rejection_category(&anyhow!(
                "resume input differs from the persisted invocation; create a fresh role or review request"
            )),
            None
        );
        assert_eq!(
            permanent_resume_rejection_category(&anyhow!(
                "session has no persisted invocation input"
            )),
            Some("invocation_provenance_missing")
        );
    }

    #[test]
    fn typed_compatibility_precedes_frozen_identity_string_fallback() {
        let error = crate::provider_compatibility::BundleSet::embedded()
            .resolve(
                crate::domain::Provider::Claude,
                "unknown",
                crate::domain::RoleKind::Manager,
            )
            .unwrap_err();
        let contextual = anyhow::Error::new(error).context("frozen runtime identity changed");
        assert_eq!(
            permanent_resume_rejection_category(&contextual),
            Some("provider_compatibility_unsupported")
        );
    }
}

#[cfg(test)]
mod drain_interruption_tests {
    use super::*;

    #[test]
    fn interrupted_drain_preserves_capture_without_signalling_or_relaunching() {
        let root = std::env::temp_dir().join(format!("llmrelay-drain-{}", uuid::Uuid::new_v4()));
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-drain-fixture','drain-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','implementation','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','manager','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
               VALUES('settings','t','manager',1,'{}','g','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,desired_running,created_at,updated_at)
               VALUES('s','g','codex','running','{}','fixture','epoch',0,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
        let app = Application::new(paths.clone(), store, std::env::current_exe().unwrap()).unwrap();
        TEST_INTERRUPT_DRAIN_AFTER_CAPTURE.with(|armed| armed.set(true));
        let error = app.begin_drain().unwrap_err();
        assert!(error
            .to_string()
            .contains("injected interruption after desired-running capture: 1"));
        assert!(!app.dispatch_enabled());
        assert!(app.draining.load(Ordering::SeqCst));
        let connection = app.store.lock().unwrap();
        let captured: i64 = connection
            .query_row(
                "SELECT desired_running FROM sessions WHERE id='s'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(captured, 1);
        let capture_events: i64 = connection.query_row("SELECT COUNT(*) FROM audit_events WHERE event_code='restart.desired_running.captured' AND json_extract(detail_json,'$.sessions[0]')='s'", [], |row| row.get(0)).unwrap();
        assert_eq!(capture_events, 1);
        let interrupt_events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM audit_events WHERE event_code LIKE '%interrupt%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(interrupt_events, 0);
        drop(connection);
        drop(app);

        let reopened = Store::open_current_writable(&paths.database).unwrap();
        let candidates = crate::recovery::prepare_restart_candidates(&reopened).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["source"], "planned_shutdown");
        assert_eq!(candidates[0]["state"], "skipped");
        let reason: String = reopened
            .lock()
            .unwrap()
            .query_row(
                "SELECT reason FROM restart_candidates WHERE session_id='s'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "same-generation native history is unavailable");
        let status: String = reopened
            .lock()
            .unwrap()
            .query_row("SELECT status FROM sessions WHERE id='s'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "running");
        let quiescent: i64 = reopened.lock().unwrap().query_row(
            "SELECT COALESCE(json_extract(exit_json,'$.process_group_quiescent'),0) FROM sessions WHERE id='s'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(quiescent, 0);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
fn assert_drain_started_or_inventory_denied(app: &Application, result: Result<serde_json::Value>) {
    assert!(!app.dispatch_enabled());
    assert!(app.draining.load(Ordering::SeqCst));
    if let Err(error) = result {
        assert!(
            error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error)
                == Some(libc::EPERM)
                && error.to_string() == std::io::Error::from_raw_os_error(libc::EPERM).to_string(),
            "unexpected drain error: {error:#}"
        );
    }
}

#[cfg(test)]
mod scheduled_intake_gate_tests {
    use super::*;
    use chrono::DateTime;

    fn time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn application_rechecks_drain_and_hold_after_discovery_and_before_each_fire() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-intake-gate-{}", uuid::Uuid::new_v4()));
        let paths = InstancePaths::resolve(Some(root)).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        store.lock().unwrap().execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-intake','intake','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO recipe_schedules(id,project_id,name,recipe_revision_id,cadence,anchor_utc,next_fire_utc,paused,version,created_at,updated_at)
               VALUES('s1','p','first','revision','daily','2026-09-25T09:00:00Z','2026-09-25T09:00:00Z',0,1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
                     ('s2','p','second','revision','daily','2026-09-25T09:00:00Z','2026-09-25T09:00:00Z',0,1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
        let app = Application::new(
            paths.clone(),
            store.clone(),
            std::env::current_exe().unwrap(),
        )
        .unwrap();
        let drain_app = app.clone();
        crate::recipes::set_test_before_fire(Some(Box::new(move |_| {
            let result = drain_app.begin_drain();
            assert_drain_started_or_inventory_denied(&drain_app, result);
        })));
        assert_eq!(
            app.scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
                .unwrap(),
            0
        );
        crate::recipes::set_test_before_fire(None);
        let counts = || -> (i64, i64, i64, String) {
            store.lock().unwrap().query_row(
                "SELECT (SELECT COUNT(*) FROM tasks),(SELECT COUNT(*) FROM recipe_schedule_fires),
                        (SELECT COUNT(*) FROM audit_events WHERE event_code='recipe.schedule.fire'),
                        (SELECT group_concat(next_fire_utc,',') FROM recipe_schedules ORDER BY id)",
                [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).unwrap()
        };
        assert_eq!(
            counts(),
            (0, 0, 0, "2026-09-25T09:00:00Z,2026-09-25T09:00:00Z".into())
        );

        let app = Application::new(
            paths.clone(),
            store.clone(),
            std::env::current_exe().unwrap(),
        )
        .unwrap();
        let held_store = store.clone();
        crate::recipes::set_test_before_fire(Some(Box::new(move |_| {
            held_store.lock().unwrap().execute(
                "INSERT INTO recovery_records(id,state,detail_json,created_at,updated_at)
                 VALUES('database-restore-hold','attention_required','{}','2026-09-25T09:00:00Z','2026-09-25T09:00:00Z')", []
            ).unwrap();
        })));
        assert_eq!(
            app.scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
                .unwrap(),
            0
        );
        crate::recipes::set_test_before_fire(None);
        assert_eq!(counts().0, 0);
        assert_eq!(counts().1, 0);
        assert_eq!(counts().2, 0);
        store
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM recovery_records WHERE id='database-restore-hold'",
                [],
            )
            .unwrap();

        let close_app = app.clone();
        crate::recipes::set_test_before_fire(Some(Box::new(move |index| {
            if index == 1 {
                let result = close_app.begin_drain();
                assert_drain_started_or_inventory_denied(&close_app, result);
            }
        })));
        assert_eq!(
            app.scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
                .unwrap(),
            1
        );
        crate::recipes::set_test_before_fire(None);
        let after = counts();
        assert_eq!((after.0, after.1, after.2), (0, 1, 1));
        assert_eq!(
            store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT next_fire_utc FROM recipe_schedules WHERE id='s2'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "2026-09-25T09:00:00Z"
        );
        let independent =
            Application::new(paths, store.clone(), std::env::current_exe().unwrap()).unwrap();
        let poison = independent.coordinator_lock.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison.lock().unwrap();
            panic!("injected coordinator mutex failure");
        })
        .join();
        assert!(independent.coordinator_tick().is_err());
        assert_eq!(
            independent
                .scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
                .unwrap(),
            1
        );
        assert_eq!(counts().1, 2);
    }
}
