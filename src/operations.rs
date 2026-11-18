use crate::auth;
use crate::checks::CheckService;
use crate::config::InstancePaths;
use crate::coordinator::StepEffect;
use crate::diagnostics::DiagnosticSink;
use crate::domain::{
    HumanCommand, LaunchConfig, OperationResult, ProcessIdentity, RestartAdmissionV1,
    RestartBatchMembership, RestartBatchV1, RestartCandidateResult, RoleContext, SessionOwner,
    TripHumanAction, ValidationLaunchRequest, ValidationLaunchResult, WorkflowValidationRequest,
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

mod context_delivery;
mod conversation;

#[derive(Debug)]
pub(crate) struct CheckedOperationRefused {
    message: String,
    terminal_operation_id: Option<String>,
}

impl CheckedOperationRefused {
    pub(crate) fn new(error: anyhow::Error) -> Self {
        Self {
            message: format!("{error:#}"),
            terminal_operation_id: None,
        }
    }

    fn from_terminal_receipt(operation_id: &str, error: anyhow::Error) -> Self {
        Self {
            message: format!("{error:#}"),
            terminal_operation_id: Some(operation_id.to_owned()),
        }
    }

    pub(crate) fn confirms_terminal_refusal(&self, operation_id: &str) -> bool {
        self.terminal_operation_id.as_deref() == Some(operation_id)
    }
}

impl std::fmt::Display for CheckedOperationRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CheckedOperationRefused {}

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
    drain_progress: Arc<Mutex<DrainProgress>>,
    pub(crate) bootstrap: auth::BootstrapIssuer,
    paired_cmux: Arc<Mutex<Option<crate::supervisor::PairedCmuxProof>>>,
    coordinator_lock: Arc<Mutex<()>>,
    intake_lock: Arc<Mutex<()>>,
    rework_lock: Arc<Mutex<()>>,
    permission_boot_id: Arc<String>,
    permission_notify: Arc<tokio::sync::Notify>,
    pub(crate) blocking_operations: Arc<tokio::sync::Semaphore>,
    cmux_host_ancestry: Option<Arc<crate::supervisor::CmuxHostAncestry>>,
    synthetic_dispatch_for_tests: bool,
    /// Set once this boot's first observation pass has discarded earlier candidates.
    attention_baseline_reset: Arc<AtomicBool>,
    attention_failure_reported: Arc<Mutex<Option<String>>>,
    schedule_clocks: Arc<Mutex<HashMap<String, ScheduleClock>>>,
}

pub(crate) struct ScheduleClock {
    pub(crate) origin: std::time::Instant,
    pub(crate) allowance: std::time::Duration,
}

fn schedule_timing(
    clock: Option<&ScheduleClock>,
    now: chrono::DateTime<Utc>,
    deadline: chrono::DateTime<Utc>,
    last_wall: Option<chrono::DateTime<Utc>>,
    stop_at: Option<chrono::DateTime<Utc>>,
) -> (bool, bool, bool) {
    let rollback = last_wall.is_some_and(|last| now < last);
    let remaining = clock.map(|clock| clock.allowance.saturating_sub(clock.origin.elapsed()));
    let expired = now >= deadline || remaining.is_some_and(|left| left.is_zero());
    let grace_ended = stop_at.is_some_and(|started| now >= started + ChronoDuration::seconds(30));
    let stopping = stop_at.is_some()
        || expired
        || rollback
        || clock.is_none()
        || now >= deadline - ChronoDuration::seconds(30)
        || remaining.is_some_and(|left| left <= std::time::Duration::from_secs(30));
    (
        stopping,
        expired || rollback || clock.is_none() || grace_ended,
        rollback,
    )
}

#[derive(Default)]
struct DrainProgress {
    captured: Option<Vec<String>>,
    checks: crate::checks::CheckInterruptProgress,
}

enum BrowserLaunchDispatch {
    Launched(ValidationLaunchResult),
    Existing(serde_json::Value),
}

const MAX_RESTART_SELECTION: usize = 200;
const MAX_RESTART_BATCH: usize = 4;

pub(crate) const ROLE_CONTEXT_OUTPUT_BYTES: usize = 4096;
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
        mut store: Store,
        executable: PathBuf,
        synthetic_dispatch_for_tests: bool,
        synthetic_hooks: Option<HookAssets>,
    ) -> Result<Self> {
        if store.compatibility_bundles.is_synthetic() && !synthetic_dispatch_for_tests {
            bail!("production application cannot use a synthetic compatibility Store")
        }
        let schedule_clocks = Arc::new(Mutex::new(HashMap::new()));
        store.schedule_preparation_clocks = Some(schedule_clocks.clone());
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
            drain_progress: Arc::new(Mutex::new(DrainProgress::default())),
            bootstrap: auth::BootstrapIssuer::default(),
            paired_cmux: Arc::new(Mutex::new(None)),
            coordinator_lock: Arc::new(Mutex::new(())),
            intake_lock: Arc::new(Mutex::new(())),
            rework_lock: Arc::new(Mutex::new(())),
            permission_boot_id,
            permission_notify: Arc::new(tokio::sync::Notify::new()),
            blocking_operations: Arc::new(tokio::sync::Semaphore::new(MAX_BLOCKING_OPERATIONS)),
            cmux_host_ancestry,
            synthetic_dispatch_for_tests,
            attention_baseline_reset: Arc::new(AtomicBool::new(false)),
            attention_failure_reported: Arc::new(Mutex::new(None)),
            schedule_clocks,
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

    pub(crate) fn interrupt_resolved_implementer(
        &self,
        receipt: &crate::store::StoppedTurnReceipt,
    ) -> Result<bool> {
        if self.synthetic_dispatch_for_tests {
            self.store.reserve_resolved_implementer_stop(receipt)
        } else {
            self.supervisor.interrupt_resolved_implementer(receipt)
        }
    }

    pub(crate) fn interrupt_completed_probe_report(
        &self,
        receipt: &crate::store::StoppedTurnReceipt,
    ) -> Result<bool> {
        if self.synthetic_dispatch_for_tests {
            self.store.reserve_completed_probe_report_stop(receipt)
        } else {
            self.supervisor.interrupt_completed_probe_report(receipt)
        }
    }

    pub(crate) fn interrupt_single_agent_report(
        &self,
        receipt: &crate::store::SingleAgentStopReceipt,
    ) -> Result<bool> {
        if self.synthetic_dispatch_for_tests {
            self.store.reserve_single_agent_stop(receipt)
        } else {
            self.supervisor.interrupt_single_agent_report(receipt)
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

    /// The permission actor a credential currently resolves to, from either
    /// execution branch.
    pub(crate) fn permission_actor(
        &self,
        credential: &str,
    ) -> Result<crate::permissions::PermissionActor> {
        match self.store.execution_context(credential)? {
            crate::domain::ExecutionContext::Trip(context) => {
                Ok(crate::permissions::PermissionActor::from(&context))
            }
            crate::domain::ExecutionContext::SingleAgent(context) => {
                crate::permissions::PermissionActor::single_agent(&self.store, &context)
            }
        }
    }

    pub async fn permission_request(
        &self,
        context: &crate::permissions::PermissionActor,
        credential: &str,
        payload: &serde_json::Value,
        connection_nonce: &str,
    ) -> Result<serde_json::Value> {
        match crate::permissions::begin_actor_request(
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
                let current_context = match self.permission_actor(credential) {
                    Ok(current) if current == *context => current,
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
                if let Some(response) = crate::permissions::consume_actor_response(
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

    pub(crate) fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub(crate) fn request_drain(&self) {
        self.dispatch_enabled.store(false, Ordering::SeqCst);
        self.draining.store(true, Ordering::SeqCst);
    }

    pub(crate) fn request_idle_shutdown(&self) -> Result<()> {
        let _coordinator = self
            .coordinator_lock
            .lock()
            .map_err(|_| anyhow!("coordinator ownership is unavailable; use stop --drain"))?;
        let _intake = self
            .intake_lock
            .lock()
            .map_err(|_| anyhow!("intake ownership is unavailable; use stop --drain"))?;
        if self.drain_status()?["quiescent"] != true {
            bail!("service still owns active or unknown sessions or checks; use stop --drain to interrupt and visibly reconcile them")
        }
        self.request_drain();
        Ok(())
    }

    pub fn service_boot_id(&self) -> &str {
        self.permission_boot_id.as_str()
    }

    pub(crate) fn desktop_snapshot(&self) -> Result<crate::domain::DesktopSnapshot> {
        use crate::domain::{DesktopSnapshot, DesktopSnapshotState};
        let instance_key = self.paths.root_key();
        let projected = crate::workflow::desktop_attention(&self.store, &instance_key);
        let (revision, mut entries, state) = match projected {
            Ok((revision, entries)) => (revision, entries, DesktopSnapshotState::Complete),
            Err(_) => ("0".into(), Vec::new(), DesktopSnapshotState::Unavailable),
        };
        let count = entries.len();
        entries.retain(|entry| !entry.attention_id.is_empty() && entry.attention_id.len() <= 512);
        for entry in &mut entries {
            if serde_json::to_vec(&entry.target)?.len() > 4096 {
                entry.target = None;
                entry.episode_key = None;
            }
        }
        entries.truncate(100);
        let mut snapshot = DesktopSnapshot {
            instance_key,
            incarnation: self.service_boot_id().into(),
            revision,
            count,
            state: if count > entries.len() {
                DesktopSnapshotState::Truncated
            } else {
                state
            },
            entries,
        };
        // Account for the entire envelope, including bounded typed targets.
        while serde_json::to_vec(&snapshot)?.len() > 128 * 1024 {
            snapshot.entries.pop();
            snapshot.state = DesktopSnapshotState::Truncated;
        }
        Ok(snapshot)
    }

    pub(crate) fn desktop_target(
        &self,
        activation: &crate::domain::DesktopActivation,
    ) -> Result<crate::domain::DesktopTarget> {
        activation.validate()?;
        let (revision, entries) =
            crate::workflow::desktop_attention(&self.store, &self.paths.root_key())?;
        let target = activation.resolve(
            &self.paths.root_key(),
            &revision,
            self.service_boot_id(),
            entries,
        );
        if serde_json::to_vec(&target)?.len() > 4096 {
            return Ok(crate::domain::DesktopTarget::Stale { message: "This attention binding exceeds the desktop limit. Choose an item from the dashboard.".into() });
        }
        Ok(target)
    }

    /// Foreground ancestry and explicit pairing are separate local proofs.
    /// Each cmux operation still checks the independent endpoint contract.
    pub(crate) fn automatic_cmux_routing_permitted(&self) -> bool {
        self.cmux_executable_path().is_some()
    }

    pub(crate) fn cmux_executable_path(&self) -> Option<PathBuf> {
        self.cmux_host_ancestry
            .as_ref()
            .and_then(|identity| identity.verified_executable())
            .or_else(|| {
                self.paired_cmux
                    .lock()
                    .ok()?
                    .as_ref()?
                    .verified_executable(self.service_boot_id())
            })
    }

    pub(crate) fn pair_cmux(
        &self,
        proof: crate::supervisor::PairedCmuxProof,
    ) -> Result<serde_json::Value> {
        if proof.verified_executable(self.service_boot_id()).is_none() {
            bail!("cmux pairing generation is no longer current")
        }
        *self
            .paired_cmux
            .lock()
            .map_err(|_| anyhow!("cmux pairing state unavailable"))? = Some(proof);
        let endpoint = crate::cmux::require_persistent_cmux_contract(self);
        Ok(
            serde_json::json!({"instance_key":self.paths.root_key(),"paired":true,
            "automatic_routing":endpoint.is_ok(),"message":match endpoint {
                Ok(()) => "Paired with the current cmux generation and endpoint.".to_owned(),
                Err(_) => "The cmux process is paired, but its independent endpoint policy refused or could not verify access. Use the session's manual llmrelay attach command; pairing does not change cmux settings.".to_owned()
            }}),
        )
    }

    pub fn executable_path(&self) -> &Path {
        &self.executable
    }

    pub fn begin_drain(&self) -> Result<serde_json::Value> {
        self.request_drain();
        // Close dispatch first, then capture only after any in-flight intake commits.
        {
            let _intake = self
                .intake_lock
                .try_lock()
                .map_err(|_| anyhow!("drain is waiting for the current intake operation"))?;
            self.draining.store(true, Ordering::SeqCst);
        }
        let mut progress = self
            .drain_progress
            .lock()
            .map_err(|_| anyhow!("drain progress is unavailable"))?;
        // A synchronous selected check owns the scheduling mutex until it exits.
        // Interrupt its exact recorded generation first, retaining partial receipts
        // even while capture must wait for the in-flight coordinator action.
        let check_interrupts = self.checks.interrupt_pending(&mut progress.checks);
        let _guard = match self.coordinator_lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                bail!("drain is waiting for the current coordinator operation")
            }
        };
        // This mutex protects scheduling, not recoverable data. Exclusive drain
        // may recover a poisoned guard; capture and positive process checks still
        // have to succeed, and ordinary coordinator dispatch remains refused.
        if progress.captured.is_none() {
            progress.captured = Some(crate::recovery::capture_desired_running_before_drain(
                &self.store,
            )?);
        }
        let desired_running = progress.captured.clone().unwrap_or_default();
        let reconciled = self.supervisor.reconcile_inventory()?;
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
        let interrupted_checks = match check_interrupts {
            Ok(ids) => ids,
            Err(error) => {
                unknown.push(
                    serde_json::json!({"check_inventory":"unknown","error":format!("{error:#}")}),
                );
                Vec::new()
            }
        };
        if progress.checks.pending() {
            unknown.push(serde_json::json!({"check_interrupts":progress.checks.snapshot()}));
        }
        let (active, process_inventory) = match inventory {
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
                (active, serde_json::json!({"state":"observed"}))
            }
            // No session signal is sent without an exact current inventory; the
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
        let _guard = self
            .coordinator_lock
            .lock()
            .map_err(|_| anyhow!("coordinator mutex is poisoned"))?;
        self.reconcile_schedule_execution_limits()?;
        self.store
            .require_execution_unheld("coordinator execution")?;
        let ticked = if let Some(action) = self.settle_one_provider_check()? {
            Ok(action)
        } else {
            match self.settle_main_checkout_verification()? {
                Some(action) if action["action"] == "waiting" => crate::coordinator::tick(self)
                    .map(|other| {
                        if matches!(other["action"].as_str(), Some("waiting" | "idle")) {
                            action
                        } else {
                            other
                        }
                    }),
                Some(action) => Ok(action),
                None => crate::coordinator::tick(self),
            }
        };
        self.observe_attention();
        ticked
    }

    fn reconcile_schedule_execution_limits(&self) -> Result<()> {
        if crate::database::hold_active(&self.store)? {
            return Ok(());
        }
        let now = Utc::now();
        let mut clocks = self
            .schedule_clocks
            .lock()
            .map_err(|_| anyhow!("schedule clock state poisoned"))?;
        let mut signals = Vec::new();
        let mut settlements = Vec::new();
        {
            let mut connection = self.store.lock()?;
            let tx =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE recipe_schedule_fires SET outcome='held',reason='Initial scheduled start window ended without a reservation'
                 WHERE authorization_id IS NOT NULL AND outcome='waiting' AND invocations_spent=0
                   AND julianday(?1)>=julianday(scheduled_for_utc)+(SELECT start_window_seconds/86400.0
                     FROM recipe_schedule_authorizations WHERE id=authorization_id)",
                [now.to_rfc3339()],
            )?;
            let rows = {
                let mut statement = tx.prepare(
                    "SELECT f.id,f.attempt_id,f.runtime_deadline_utc,f.last_observed_wall_utc,f.outcome,
                            a.restore_invalidated_at,f.stop_requested_at,
                            a.revoked_at IS NOT NULL OR s.active_authorization_id IS NOT a.id
                              OR NOT ((s.mode='run_authorized_research' AND json_extract(a.scope_json,'$.schema')=1)
                                OR (s.mode='run_authorized_recipe' AND json_extract(a.scope_json,'$.schema')=2))
                              OR s.archived_at IS NOT NULL
                     FROM recipe_schedule_fires f JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id
                     JOIN recipe_schedules s ON s.id=f.schedule_id
                     WHERE f.allowance_reserved=1 AND f.outcome IN ('waiting','reserved','running','stop_requested','recovery_required')",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, bool>(7)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let active = rows
                .iter()
                .map(|row| row.0.as_str())
                .collect::<HashSet<_>>();
            clocks.retain(|id, clock| {
                active.contains(id.as_str()) || clock.origin.elapsed() < clock.allowance
            });
            drop(active);
            for (
                occurrence,
                attempt,
                deadline,
                last_wall,
                _outcome,
                restored,
                stop_at,
                authority_invalidated,
            ) in rows
            {
                if let Some(restored_at) = restored.as_deref() {
                    clocks.remove(&occurrence);
                    let writer: bool = tx.query_row("SELECT json_extract(scope_json,'$.schema')=2 FROM recipe_schedule_authorizations
                        WHERE id=(SELECT authorization_id FROM recipe_schedule_fires WHERE id=?1)", [&occurrence], |row|row.get(0))?;
                    if attempt.is_none() {
                        continue;
                    }
                    let attempt = attempt
                        .as_deref()
                        .ok_or_else(|| anyhow!("Restored occurrence has no root"))?;
                    // Recovery may release the historical fence, never the restored grant or stop target.
                    let recovered: bool = if writer {
                        let hold: Option<(String, String)> = tx.query_row(
                            "SELECT json_extract(detail_json,'$.operation_id'),created_at FROM recovery_records
                             WHERE id='database-restore-hold' AND state='resolved_quiescent'
                               AND julianday(resolved_at)>=julianday(created_at)
                               AND julianday(created_at)>=julianday(?1)",
                            [restored_at], |row| Ok((row.get(0)?, row.get(1)?)),
                        ).optional()?;
                        let members = crate::recipes::schedule_members_in(&tx, &occurrence);
                        let mut complete = false;
                        if let (Some((operation, created)), Ok(members)) = (hold, members) {
                            complete = !members.is_empty();
                            for member in members {
                                let missing_session: bool = tx.query_row(
                                    "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
                                     WHERE g.attempt_id=?1 AND s.schedule_occurrence_id=?2
                                       AND NOT EXISTS(SELECT 1 FROM recovery_records r WHERE r.session_id=s.id
                                         AND r.attempt_id=?1 AND json_extract(r.detail_json,'$.kind')='database_restore_session'
                                         AND json_extract(r.detail_json,'$.operation_id')=?3))",
                                    rusqlite::params![member, occurrence, operation], |row| row.get(0),
                                )?;
                                let mut statement = tx.prepare(
                                    "SELECT session_id,state,detail_json,COALESCE(
                                         julianday(resolved_at)>=julianday(?3)
                                         AND julianday(resolved_at)>=julianday(created_at),0)
                                     FROM recovery_records WHERE attempt_id=?1
                                       AND json_extract(detail_json,'$.operation_id')=?2",
                                )?;
                                let records = statement
                                    .query_map(
                                        rusqlite::params![member, operation, created],
                                        |row| {
                                            Ok((
                                                row.get::<_, Option<String>>(0)?,
                                                row.get::<_, String>(1)?,
                                                row.get::<_, String>(2)?,
                                                row.get::<_, bool>(3)?,
                                            ))
                                        },
                                    )?
                                    .collect::<rusqlite::Result<Vec<_>>>()?;
                                complete &= !missing_session && !records.is_empty();
                                for (session, state, detail, fresh) in records {
                                    let detail: serde_json::Value = serde_json::from_str(&detail)?;
                                    let verification = &detail["verification"];
                                    let annotated = detail["human_annotation"]
                                        .as_str()
                                        .is_some_and(|value| !value.trim().is_empty());
                                    let confirmed = if let Some(session) = session.as_deref() {
                                        detail["kind"] == "database_restore_session"
                                            && verification["session_id"].as_str() == Some(session)
                                    } else {
                                        let resource = match detail["kind"].as_str() {
                                            Some("database_restore_check") => {
                                                Some(("checks", "check_id"))
                                            }
                                            Some("database_restore_claim") => {
                                                Some(("claims", "claim_id"))
                                            }
                                            Some("database_restore_freeze") => {
                                                Some(("freeze_intents", "freeze_id"))
                                            }
                                            _ => None,
                                        };
                                        verification["attempt_id"].as_str() == Some(member.as_str())
                                            && resource.is_some_and(|(collection, key)| {
                                                detail[key].as_str().is_some_and(|subject| {
                                                    verification[collection].as_array().is_some_and(
                                                        |rows| {
                                                            rows.iter().any(|row| {
                                                                row[key].as_str() == Some(subject)
                                                            })
                                                        },
                                                    )
                                                })
                                            })
                                    };
                                    let preparation =
                                        detail["kind"] == "database_restore_preparation";
                                    let preparation_cancelled = if preparation {
                                        let identity = &detail["preparation"];
                                        identity == &verification["preparation"]
                                            && identity["operation_id"].as_str() == Some(operation.as_str())
                                            && identity["occurrence_id"].as_str() == Some(occurrence.as_str())
                                            && identity["child_attempt_id"].as_str() == Some(member.as_str())
                                            && verification["kind"] == "rework_cancellation"
                                            && tx.query_row(
                                                "SELECT EXISTS(SELECT 1 FROM rework_intents i
                                                 JOIN single_agent_repair_receipts r ON r.rework_intent_id=i.id
                                                 WHERE i.id=?1 AND i.new_attempt_id=?2 AND i.state='cancelled'
                                                   AND i.parent_attempt_id=?3 AND r.delivery_id=?4 AND r.snapshot_id=?5
                                                   AND json_extract(i.result_json,'$.restore_preparation')=?6)",
                                                rusqlite::params![identity["intent_id"].as_str(),member,
                                                    identity["parent_attempt_id"].as_str(),identity["delivery_id"].as_str(),
                                                    identity["snapshot_id"].as_str(),identity.to_string()], |row|row.get::<_,bool>(0),
                                            )?
                                    } else {
                                        false
                                    };
                                    let known_resource = matches!(
                                        detail["kind"].as_str(),
                                        Some(
                                            "database_restore_session"
                                                | "database_restore_check"
                                                | "database_restore_claim"
                                                | "database_restore_freeze"
                                        )
                                    );
                                    let cancelled_scope = if preparation {
                                        preparation_cancelled
                                    } else {
                                        known_resource
                                            && verification["attempt_ids"].as_array().is_some_and(
                                                |attempts| {
                                                    attempts.iter().any(|id| {
                                                        id.as_str() == Some(member.as_str())
                                                    })
                                                },
                                            )
                                    };
                                    let cancelled = detail["human_evidence"]
                                        .as_str()
                                        .is_some_and(|value| !value.trim().is_empty())
                                        && matches!(
                                            verification["kind"].as_str(),
                                            Some("attempt_cancellation" | "rework_cancellation")
                                        )
                                        && cancelled_scope;
                                    complete &= fresh
                                        && ((state == "resolved_quiescent"
                                            && annotated
                                            && confirmed)
                                            || (state == "resolved_cancelled" && cancelled));
                                }
                            }
                        }
                        complete
                    } else {
                        tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM recovery_records hold
                         WHERE hold.id='database-restore-hold' AND hold.state='resolved_quiescent'
                           AND julianday(hold.resolved_at)>=julianday(hold.created_at)
                           AND julianday(hold.created_at)>=julianday(?2)
                           AND EXISTS(SELECT 1 FROM recovery_records r JOIN sessions s ON s.id=r.session_id
                             WHERE r.attempt_id=?1 AND s.schedule_occurrence_id=?3
                               AND json_extract(r.detail_json,'$.kind')='database_restore_session'
                               AND json_extract(r.detail_json,'$.operation_id')=json_extract(hold.detail_json,'$.operation_id'))
                           AND NOT EXISTS(SELECT 1 FROM recovery_records r
                             WHERE r.attempt_id=?1
                               AND json_extract(r.detail_json,'$.kind')='database_restore_session'
                               AND json_extract(r.detail_json,'$.operation_id')=json_extract(hold.detail_json,'$.operation_id')
                               AND NOT COALESCE(
                                 julianday(r.resolved_at)>=julianday(hold.created_at)
                                 AND julianday(r.resolved_at)>=julianday(r.created_at)
                                 AND ((r.state='resolved_quiescent'
                                   AND length(trim(json_extract(r.detail_json,'$.human_annotation')))>0
                                   AND json_extract(r.detail_json,'$.verification.session_id')=r.session_id)
                                   OR (r.state='resolved_cancelled'
                                     AND length(trim(json_extract(r.detail_json,'$.human_evidence')))>0
                                     AND json_extract(r.detail_json,'$.verification.kind')='attempt_cancellation'
                                     AND EXISTS(SELECT 1 FROM json_each(r.detail_json,'$.verification.attempt_ids')
                                       WHERE value=?1))),0)))",
                        rusqlite::params![attempt, restored_at, occurrence],
                        |row| row.get(0),
                    )?
                    };
                    if recovered
                        && crate::recipes::schedule_occurrence_quiescence_in(
                            &tx,
                            &occurrence,
                            writer,
                        )
                        .is_ok()
                    {
                        tx.execute(
                            "UPDATE recipe_schedule_fires SET outcome='finished',
                             reason='Explicit restore recovery established quiescence; historical authority remains invalid'
                             WHERE id=?1",
                            [&occurrence],
                        )?;
                    }
                    continue;
                }
                let clock = clocks.get(&occurrence);
                let last = last_wall
                    .as_deref()
                    .map(chrono::DateTime::parse_from_rfc3339)
                    .transpose()?
                    .map(|value| value.with_timezone(&Utc));
                let stop_time = stop_at
                    .as_deref()
                    .map(chrono::DateTime::parse_from_rfc3339)
                    .transpose()?
                    .map(|value| value.with_timezone(&Utc));
                let (timed_stop, force, rollback) = if let Some(deadline) = deadline.as_deref() {
                    schedule_timing(
                        clock,
                        now,
                        chrono::DateTime::parse_from_rfc3339(deadline)?.with_timezone(&Utc),
                        last,
                        stop_time,
                    )
                } else {
                    (stop_time.is_some(), true, false)
                };
                let stopping = timed_stop || authority_invalidated;
                if stopping {
                    let reason = if rollback {
                        "Wall clock moved backward; recovery required"
                    } else if authority_invalidated {
                        "The original execution grant was invalidated; stop and reconcile this occurrence"
                    } else if deadline.is_some() && clock.is_none() {
                        "Runtime origin unavailable after restart; reconcile without replay"
                    } else if force {
                        "Scheduled runtime allowance ended or stop requested before submission"
                    } else {
                        "Scheduled runtime is ending; bounded interrupt grace"
                    };
                    crate::recipes::persist_schedule_stop_intent_in(
                        &tx,
                        &occurrence,
                        reason,
                        &now.to_rfc3339(),
                    )?;
                    // Preserve schema-1 request/history compatibility. Expanded targets below
                    // never replace that historical single-invocation identity.
                    let legacy: bool = tx.query_row("SELECT json_extract(scope_json,'$.schema')=1 FROM recipe_schedule_authorizations
                        WHERE id=(SELECT authorization_id FROM recipe_schedule_fires WHERE id=?1)", [&occurrence], |row|row.get(0))?;
                    if legacy {
                        if let Some(binding) =
                            crate::recipes::schedule_stop_binding_in(&tx, &occurrence)?
                        {
                            if crate::recipes::persist_schedule_stop_in(
                                &tx,
                                &binding,
                                reason,
                                &now.to_rfc3339(),
                            )
                            .is_err()
                            {
                                tx.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason='Historical stop identity requires recovery' WHERE id=?1", [&occurrence])?;
                            }
                        }
                    }
                    match crate::recipes::snapshot_schedule_stop_targets_in(
                        &tx,
                        &occurrence,
                        &now.to_rfc3339(),
                    ) {
                        Ok(targets) => {
                            signals.extend(targets.into_iter().map(|target| (target, force)))
                        }
                        Err(error) => {
                            tx.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason=?1 WHERE id=?2",
                                rusqlite::params![format!("Occurrence identity requires recovery: {error:#}"),occurrence])?;
                        }
                    }
                    settlements.push(occurrence);
                } else {
                    let members = crate::recipes::schedule_members_in(&tx, &occurrence);
                    if let Err(error) = &members {
                        crate::recipes::persist_schedule_stop_intent_in(
                            &tx,
                            &occurrence,
                            &format!("{error:#}"),
                            &now.to_rfc3339(),
                        )?;
                        tx.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason=?1 WHERE id=?2",
                            rusqlite::params![format!("{error:#}"),occurrence])?;
                        // Unknown lineage retains the fence, but cannot prevent stopping
                        // other independently authenticated exact invocations.
                        match crate::recipes::snapshot_schedule_stop_targets_in(
                            &tx,
                            &occurrence,
                            &now.to_rfc3339(),
                        ) {
                            Ok(targets) => {
                                signals.extend(targets.into_iter().map(|target| (target, force)))
                            }
                            Err(snapshot_error) => {
                                tx.execute("UPDATE recipe_schedule_fires SET reason=?1 WHERE id=?2",
                                    rusqlite::params![format!("Occurrence target identity requires recovery: {snapshot_error:#}"),occurrence])?;
                            }
                        }
                        continue;
                    }
                    let members = members?;
                    // A parent's rejection is intermediate. Only the final member's reviewable
                    // outcome can release overlap, after every member and transition settles.
                    let mut report = false;
                    let mut terminal = !members.is_empty();
                    for member in &members {
                        report |= tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM workflow_result_bindings r WHERE r.attempt_id=?1 AND r.state='awaiting_human'
                             AND NOT EXISTS(SELECT 1 FROM single_agent_repair_receipts WHERE root_attempt_id=?1))",
                            [member], |row|row.get::<_,bool>(0))?;
                        terminal &= tx.query_row("SELECT status IN ('done','failed','cancelled') FROM attempts WHERE id=?1",
                            [member], |row|row.get::<_,bool>(0))?;
                    }
                    if (report || terminal)
                        && crate::recipes::schedule_occurrence_quiescence_in(
                            &tx,
                            &occurrence,
                            false,
                        )
                        .is_ok()
                    {
                        tx.execute(
                            "UPDATE recipe_schedule_fires SET outcome=?1 WHERE id=?2",
                            rusqlite::params![
                                if report { "review_ready" } else { "finished" },
                                occurrence
                            ],
                        )?;
                        if !report {
                            clocks.remove(&occurrence);
                        }
                        continue;
                    }
                    tx.execute("UPDATE recipe_schedule_fires SET last_observed_wall_utc=?1,
                        outcome=CASE WHEN EXISTS(SELECT 1 FROM sessions WHERE schedule_occurrence_id=?2
                          AND status='running' AND launch_state='started') THEN 'running' ELSE outcome END WHERE id=?2",
                        rusqlite::params![now.to_rfc3339(),occurrence])?;
                }
            }
            tx.commit()?;
        }
        drop(clocks);
        for (target, force) in signals {
            let quiescent = {
                let connection = self.store.lock()?;
                crate::recipes::schedule_native_target_quiescent_in(&connection, &target.binding)?
            };
            if quiescent {
                self.store.lock()?.execute("UPDATE recipe_schedule_stop_targets SET status='quiescent',observed_at=?1 WHERE id=?2",
                    rusqlite::params![now.to_rfc3339(),target.id])?;
                continue;
            }
            let result = self.supervisor.signal_schedule_stop(&target.binding, force);
            let connection = self.store.lock()?;
            let status = if crate::recipes::schedule_native_target_quiescent_in(
                &connection,
                &target.binding,
            )? {
                "quiescent"
            } else if result.is_ok() {
                "stop_requested"
            } else {
                "recovery_required"
            };
            connection.execute(
                "UPDATE recipe_schedule_stop_targets SET status=?1,observed_at=?2 WHERE id=?3",
                rusqlite::params![status, now.to_rfc3339(), target.id],
            )?;
            if let Err(error) = result {
                connection.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason=?1 WHERE id=?2 AND outcome='stop_requested'",
                    rusqlite::params![format!("Exact scheduled process could not be stopped: {error:#}"),target.binding.occurrence_id])?;
            }
        }
        for occurrence in settlements {
            self.settle_stopped_schedule_preparation(&occurrence)?;
        }
        Ok(())
    }

    fn settle_stopped_schedule_preparation(&self, occurrence: &str) -> Result<()> {
        use rusqlite::params;
        // Intent has already committed. Never wait on a materializer while holding
        // the Store or clock locks; contention and poison retain all ownership.
        let Ok(_preparation) = self.store.try_root_preparation_guard() else {
            return Ok(());
        };
        let Ok(_rework) = self.rework_lock.try_lock() else {
            return Ok(());
        };
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM recipe_schedule_fires f
            JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id WHERE f.id=?1
              AND f.stop_requested_at IS NOT NULL AND f.outcome IN ('stop_requested','recovery_required')
              AND a.restore_invalidated_at IS NULL)", [occurrence], |row|row.get(0))?;
        if !current {
            return Ok(());
        }
        let members = match crate::recipes::schedule_members_in(&tx, occurrence) {
            Ok(members) => members,
            Err(_) => return Ok(()),
        };
        for attempt in &members {
            // Reserved/ready review delivery has no active materializer. A materializing
            // delivery remains owned until its fenced callback abandons it or recovery verifies it.
            tx.execute("UPDATE single_agent_review_deliveries SET state='abandoned',reason=COALESCE(reason,'Occurrence stopped before review delivery'),updated_at=?1
                WHERE attempt_id=?2 AND state IN ('reserved','ready')", params![now,attempt])?;
            if crate::workflow::single_agent_quiescence(&tx, attempt).is_ok() {
                tx.execute("UPDATE single_agent_review_deliveries SET state='abandoned',reason=COALESCE(reason,'Occurrence stopped after exact review quiescence'),updated_at=?1
                    WHERE attempt_id=?2 AND state='launched'", params![now,attempt])?;
            }
            tx.execute("UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)
                WHERE id IN (SELECT credential_id FROM single_agent_review_deliveries WHERE attempt_id=?2 AND state='abandoned')",
                params![now,attempt])?;
            let staged: Option<(String, String)> = tx
                .query_row(
                    "SELECT id,state FROM rework_intents
                WHERE new_attempt_id=?1 AND state IN ('reserved','materializing')",
                    [attempt],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((intent, state)) = staged {
                let workspace: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE attempt_id=?1)",
                    [attempt],
                    |row| row.get(0),
                )?;
                let destination = self.paths.artifacts.join("worktrees").join(attempt);
                let uncertain = state == "materializing"
                    || workspace
                    || destination.try_exists()?
                    || crate::workflow::single_agent_quiescence(&tx, attempt).is_err();
                tx.execute("UPDATE rework_intents SET state=?1,result_json=?2,updated_at=?3 WHERE id=?4 AND state IN ('reserved','materializing')",
                    params![if uncertain { "recovery_required" } else { "cancelled" },
                        serde_json::json!({"reason":"Occurrence stopped during repair preparation","claim_retained":true,"cancellation_pending":uncertain}).to_string(),now,intent])?;
                tx.execute("UPDATE attempts SET status=?1,updated_at=?2 WHERE id=?3 AND status='materialization_pending'",
                    params![if uncertain { "needs_recovery" } else { "cancelled" },now,attempt])?;
                if uncertain {
                    tx.execute("UPDATE workspaces SET state='recovery_required',updated_at=?1 WHERE attempt_id=?2 AND state='reserved'",params![now,attempt])?;
                    tx.execute("UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
                }
            }
        }
        // Do not release any lineage claim until all native/check/effect ownership
        // and every preparation is settled. Unresolved permission requests and
        // response deliveries retain ownership until reconciled.
        for attempt in &members {
            if crate::workflow::single_agent_quiescence(&tx, attempt).is_err() {
                tx.commit()?;
                return Ok(());
            }
            if require_schedule_member_effects_quiescent_in(&tx, attempt).is_err() {
                tx.commit()?;
                return Ok(());
            }
            let pending: bool = tx.query_row("SELECT
                EXISTS(SELECT 1 FROM rework_intents WHERE (parent_attempt_id=?1 OR new_attempt_id=?1) AND state NOT IN ('completed','cancelled'))
                OR EXISTS(SELECT 1 FROM single_agent_review_deliveries WHERE attempt_id=?1 AND state IN ('reserved','materializing','ready','launched'))
                OR EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND status IN ('materialization_pending','needs_recovery'))",
                [attempt], |row|row.get(0))?;
            if pending {
                tx.commit()?;
                return Ok(());
            }
        }
        // Filesystem evidence is checked under both preparation guards, before any
        // claim changes. Unknown files/tuples retain recovery instead of being erased.
        for attempt in &members {
            let workspace: Option<(String,String,String,String,String)> = tx.query_row(
                "SELECT w.id,w.path,w.repository_identity,w.base_revision,w.state FROM workspaces w
                 WHERE w.attempt_id=?1", [attempt], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
            if let Some((workspace, path, identity, base, state)) = workspace {
                if matches!(state.as_str(), "abandoned" | "cancelled") {
                    continue;
                }
                let path = PathBuf::from(&path);
                let evidence = if !path.try_exists()? {
                    state == "reserved"
                } else {
                    crate::workspace::inspect(&path).is_ok_and(|observed| {
                        observed.identity == identity && observed.head == base
                    }) && matches!(state.as_str(), "reserved" | "ready")
                };
                if !evidence {
                    let root: bool = tx.query_row(
                        "SELECT attempt_id=?1 FROM recipe_schedule_fires WHERE id=?2",
                        params![attempt, occurrence],
                        |row| row.get(0),
                    )?;
                    if root {
                        crate::scheduler::record_workspace_reservation_recovery_in(
                            &tx,
                            attempt,
                            None,
                            &workspace,
                            "Stopped occurrence workspace ownership needs explicit recovery",
                            serde_json::json!({"stage":"schedule_stop_workspace","path_exists":path.try_exists()?,"files_preserved":true}),
                        )?;
                    }
                    tx.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason='Workspace ownership needs explicit recovery before stop can settle' WHERE id=?1",[occurrence])?;
                    tx.commit()?;
                    return Ok(());
                }
                // Validate every member before applying even the first disposition.
            } else {
                let claimed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1 AND state IN ('reserved','launching','running','unknown','stopping'))",[attempt],|row|row.get(0))?;
                if claimed {
                    tx.commit()?;
                    return Ok(());
                }
            }
        }
        for attempt in &members {
            let uncertain_claim: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM claims c
                WHERE c.attempt_id=?1 AND c.state IN ('reserved','launching','running','unknown','stopping')
                  AND (c.state NOT IN ('reserved','running') OR NOT EXISTS(
                    SELECT 1 FROM workspaces w JOIN attempts a ON a.id=w.attempt_id
                    WHERE w.attempt_id=c.attempt_id AND c.task_id=a.task_id
                      AND c.repository_identity=w.repository_identity)))
                OR (SELECT count(*) FROM claims WHERE attempt_id=?1
                  AND state IN ('reserved','launching','running','unknown','stopping'))>1",
                [attempt],|row|row.get(0))?;
            if uncertain_claim {
                tx.execute("UPDATE recipe_schedule_fires SET outcome='recovery_required',reason='Exact claim ownership requires recovery before stop can settle' WHERE id=?1",[occurrence])?;
                tx.commit()?;
                return Ok(());
            }
        }
        let unknown_target: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recipe_schedule_stop_targets
            WHERE occurrence_id=?1 AND status!='quiescent')",
            [occurrence],
            |row| row.get(0),
        )?;
        if unknown_target {
            tx.commit()?;
            return Ok(());
        }
        for attempt in &members {
            tx.execute("UPDATE workspaces SET state='abandoned',updated_at=?1 WHERE attempt_id=?2 AND state IN ('reserved','ready')",params![now,attempt])?;
            tx.execute("UPDATE claims SET state='cancelled',updated_at=?1 WHERE attempt_id=?2 AND state IN ('reserved','running')",params![now,attempt])?;
            tx.execute("UPDATE attempts SET status='cancelled',updated_at=?1 WHERE id=?2 AND status NOT IN ('done','failed','cancelled')",params![now,attempt])?;
        }
        if crate::recipes::schedule_occurrence_quiescence_in(&tx, occurrence, true).is_ok() {
            tx.execute("UPDATE recipe_schedule_fires SET outcome=CASE WHEN task_id IS NULL THEN 'held' ELSE 'finished' END,reason='Occurrence stop settled after exact preparation, workspace and process quiescence' WHERE id=?1",[occurrence])?;
            tx.execute("UPDATE tasks SET lifecycle='cancelled',attention='none',version=version+1,updated_at=?1
                WHERE id=(SELECT task_id FROM recipe_schedule_fires WHERE id=?2) AND lifecycle NOT IN ('done','cancelled')",params![now,occurrence])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Classification bookkeeping outside the tick's one workflow action; its
    /// failure is reported once per cause and never alters the tick's outcome.
    fn observe_attention(&self) {
        let fresh_boot = !self.attention_baseline_reset.load(Ordering::SeqCst);
        let observed = self
            .store
            .require_execution_unheld("attention observation")
            .and_then(|()| self.supervisor.process_snapshot())
            .and_then(|processes| {
                let attention = self
                    .store
                    .observe_attention(Utc::now(), &processes, fresh_boot);
                if attention.is_ok() {
                    self.attention_baseline_reset.store(true, Ordering::SeqCst);
                }
                let capacity = (|| {
                    let Some(root) = crate::providers::codex::capacity_history_root() else {
                        return Ok(());
                    };
                    let observations = self.store.read_codex_capacity(&root, &processes)?;
                    if !observations.is_empty() {
                        let current = self.supervisor.process_snapshot()?;
                        for (candidate, source) in observations {
                            self.store
                                .record_codex_capacity(&candidate, &source, &current)?;
                        }
                    }
                    Ok(())
                })();
                let usage = (|| {
                    let Some(root) = crate::providers::codex::capacity_history_root() else {
                        return Ok(());
                    };
                    let observations = self.store.read_codex_usage(&root, &processes)?;
                    if !observations.is_empty() {
                        let current = self.supervisor.process_snapshot()?;
                        for (candidate, source) in observations {
                            self.store
                                .record_codex_usage(&root, &candidate, &source, &current)?;
                        }
                    }
                    Ok(())
                })();
                attention.and(capacity).and(usage)
            });
        let Ok(mut reported) = self.attention_failure_reported.lock() else {
            tracing::warn!("attention observation failure state is poisoned");
            return;
        };
        match observed {
            Ok(()) => {
                *reported = None;
            }
            Err(error) => {
                let cause = format!("{error:#}");
                if reported.as_deref() != Some(cause.as_str()) {
                    tracing::warn!(error = %error, "attention observation deferred");
                    let _ = self.diagnostics.record(
                        "warn",
                        "attention.observation",
                        "attention_observation",
                        "deferred",
                        None,
                        serde_json::json!({"cause":cause}),
                    );
                    *reported = Some(cause);
                }
            }
        }
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
            {
                let connection = self.store.lock()?;
                let task: String = connection.query_row(
                    "SELECT task_id FROM attempts WHERE id=?1",
                    [&attempt],
                    |row| row.get(0),
                )?;
                if crate::workflow::task_execution_retirement_problem(&connection, &task)?.is_some()
                {
                    continue;
                }
                if crate::store::failure_stop_fence(&connection, &attempt)?.is_some() {
                    continue;
                }
            }
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
        crate::store::require_failure_stop_released(&tx, &facts.attempt_id)?;
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
            &SessionOwner::RoleGeneration(role_generation_id.clone()),
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
        {
            let connection = self.store.lock()?;
            crate::store::require_session_failure_stop_released(&connection, session_id)?;
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
        let mut launch = if let Some(policy) = runtime_policy.as_ref() {
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
        providers::retain_qualification_contract(&mut launch.config, &prior.security_policy)?;
        let prompt_hash = launch.request_prompt_hash(&resume_prompt)?;
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
                    &prompt_hash,
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
                    &prompt_hash,
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
            match self.supervisor.spawn(
                session_id,
                &SessionOwner::RoleGeneration(generation.to_owned()),
                &epoch,
                &launch,
            ) {
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

    pub(crate) fn reconcile_unanswered_review(
        &self,
        receipt: &crate::review::UnansweredReview,
    ) -> Result<serde_json::Value> {
        let recorded = if receipt.missing_native_history {
            let record = self.store.session_json(&receipt.session_id)?;
            let prior: LaunchConfig = serde_json::from_value(record["launch_config"].clone())?;
            let invocation = self.store.invocation_input(&receipt.session_id)?;
            self.store
                .validate_resume_input(&receipt.session_id, &invocation)?;
            let prompt = invocation["prompt"]
                .as_str()
                .context("review session is missing its persisted input")?;
            let read_denials =
                crate::trip::setup_target_read_denials(&self.store, &receipt.attempt_id)?;
            // Prepare the exact profile to measure its current identity. No
            // reservation, provider spawn, or review allowance is spent here.
            let mut prepared = providers::prepare_role_launch_with_read_denials_and_bundles(
                prior.provider,
                prior.role,
                &prior.model,
                &prior.effort,
                &prior.cwd,
                prompt,
                &self.paths.role_socket,
                &auth::issue_secret(),
                &receipt.generation_id,
                &receipt.session_id,
                None,
                &self.hooks,
                &self.executable,
                &read_denials,
                &self.store.compatibility_bundles,
            )?;
            providers::retain_qualification_contract(&mut prepared.config, &prior.security_policy)?;
            self.store.record_unanswered_review_rejection(
                receipt,
                &PreparedResumeIdentity::from_launch(&prepared.config)?,
            )?
        } else {
            return Ok(
                serde_json::json!({"action":"waiting","for":"review_session_resume",
                "request_id":receipt.request_id,"session_id":receipt.session_id}),
            );
        };
        Ok(
            serde_json::json!({"action":if recorded {"review_session_recovery_required"} else {"review_recovery_superseded"},
            "request_id":receipt.request_id,"session_id":receipt.session_id,"new_provider_call":false}),
        )
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
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "role_resume",
            input_hash: &request_hash,
            entity_id: session_id,
        };
        if let Some(existing) = self.read_browser_launch_receipt(receipt)? {
            return self.checked_browser_launch_receipt(receipt, existing);
        }
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
        {
            let connection = self.store.lock()?;
            crate::store::require_session_failure_stop_released(&connection, session_id)?;
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
        let verification_resume: bool=self.store.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM controls WHERE kind='main_checkout_verify'
            AND state='native_running' AND json_extract(payload_json,'$.session_id')=?1 AND json_extract(payload_json,'$.prompt_hash')=?2
            AND json_extract(payload_json,'$.turns_reserved')=1)",rusqlite::params![session_id,json_hash(&prompt)?], |row|row.get(0))?;
        let ordinary_binding: Option<Option<String>> = self.store.lock()?.query_row(
            "SELECT json_extract(payload_json,'$.ordinary_input_hash') FROM controls WHERE kind='main_checkout_verify'
             AND state='finished' AND json_extract(payload_json,'$.session_id')=?1",
            rusqlite::params![session_id], |row| row.get(0),
        ).optional()?;
        if let Some(binding) = ordinary_binding {
            if binding.as_deref() != Some(json_hash(&invocation)?.as_str()) {
                bail!("The verified session lost its ordinary task input binding; reconcile its recorded verification before resuming")
            }
        }
        let persisted_prompt = invocation
            .get("prompt")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                anyhow!("session has no persisted invocation input; exact resume is blocked")
            })?;
        if !verification_resume && !prompt.trim().is_empty() && prompt != persisted_prompt {
            bail!("resume input differs from the persisted invocation; create a fresh role or review request")
        }
        if !verification_resume {
            self.store.validate_resume_input(session_id, &invocation)?;
        }
        let mut delivered_prompt = if verification_resume {
            prompt.to_owned()
        } else {
            persisted_prompt.to_owned()
        };
        if prior.role == crate::domain::RoleKind::Manager
            && record["validation_cell"].is_null()
            && !verification_resume
        {
            let approvals = {
                let connection = self.store.lock()?;
                crate::trip::human_approval_context(
                    &connection,
                    record["attempt_id"]
                        .as_str()
                        .ok_or_else(|| anyhow!("session attempt is missing"))?,
                )?
            };
            delivered_prompt = format!(
                "Current workflow checkpoint: {approvals}\nThe saved invocation below is historical context. Continue the current phase shown above using authenticated role context, preserving its current scope and feedback.\n\n{delivered_prompt}"
            );
        }
        // The restart notice leads only its admitted turn; the saved invocation stays unchanged.
        if restart_admission.is_some() {
            delivered_prompt = format!("{HOST_RESTART_RESUME_NOTICE}\n\n{delivered_prompt}");
        }
        let generation = record["role_generation_id"]
            .as_str()
            .ok_or_else(|| anyhow!("session generation is missing"))?
            .to_owned();
        let token = auth::issue_secret();
        let epoch = uuid::Uuid::new_v4().to_string();
        let checkout_scope =
            self.checkout_scope(record["attempt_id"].as_str().unwrap_or_default())?;
        let mut read_denials = crate::trip::setup_target_read_denials(
            &self.store,
            record["attempt_id"].as_str().unwrap_or_default(),
        )?;
        if let Some(scope) = &checkout_scope {
            read_denials = serde_json::from_value(scope["protected_roots"].clone())?;
        }
        let mut launch = providers::prepare_role_launch_with_read_denials_and_bundles(
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
        providers::retain_qualification_contract(&mut launch.config, &prior.security_policy)?;
        if let Some(scope) = &checkout_scope {
            let policy = self.main_readonly_policy(
                record["attempt_id"]
                    .as_str()
                    .context("Session attempt is missing")?,
                prior.role,
                scope,
            )?;
            providers::bind_checkout_launch(&mut launch, scope, policy.as_ref())?;
        }
        let prompt_hash = launch.request_prompt_hash(&delivered_prompt)?;
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
                &prompt_hash,
                &token,
                receipt,
            )
        } else if let Some(admission) = restart_admission {
            self.store
                .reserve_role_resume_for_restart_admission(
                    session_id,
                    &epoch,
                    &launch.config,
                    &prompt_hash,
                    &token,
                    admission,
                )
                .map(|()| BrowserLaunchReservation::Reserved)
        } else {
            self.store
                .reserve_role_resume_with_prompt_hash(
                    session_id,
                    &epoch,
                    &launch.config,
                    &prompt_hash,
                    &token,
                )
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
        if self.checkout_scope(record["attempt_id"].as_str().unwrap_or_default())? != checkout_scope
        {
            let error=anyhow!("Checkout protections changed after resume reservation; the native conversation was not started");
            self.store
                .restore_resume_after_proven_nondelivery(session_id, &error.to_string())?;
            return Err(if browser_receipt.is_some() {
                CheckedOperationRefused::new(error).into()
            } else {
                error
            });
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
            match self.supervisor.spawn(
                session_id,
                &SessionOwner::RoleGeneration(generation.clone()),
                &epoch,
                &launch,
            ) {
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
        {
            let connection = self.store.lock()?;
            crate::store::require_failure_stop_released(&connection, attempt_id)?;
        }
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
        self.dispatch_trip_setup_role_inner_with_browser(attempt_id, role, None, None)
            .and_then(browser_launch_dispatch_result)
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
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "trip_setup_dispatch",
            input_hash: &request_hash,
            entity_id: attempt_id,
        };
        if let Some(existing) = self.read_browser_launch_receipt(receipt)? {
            return self.checked_browser_launch_receipt(receipt, existing);
        }
        let result = self.dispatch_trip_setup_role_inner_with_browser(
            attempt_id,
            role,
            Some(receipt),
            fresh_resume_rejection,
        );
        self.finish_browser_launch(receipt, result)
    }

    fn dispatch_trip_setup_role_inner_with_browser(
        &self,
        attempt_id: &str,
        role: crate::domain::RoleKind,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<BrowserLaunchDispatch> {
        let prompt = crate::trip::setup_launch_prompt(&self.store, attempt_id, role)?;
        let context = self
            .store
            .trip_setup_role_launch_context(attempt_id, role, false)?;
        if let Some(receipt) = browser_receipt {
            self.dispatch_context_with_browser_receipt(
                attempt_id,
                role,
                &prompt,
                None,
                context,
                None,
                None,
                None,
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
                None,
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
        if operation_id.trim().is_empty() {
            bail!(CheckedOperationRefused::new(anyhow!(
                "operation_id is required"
            )))
        }
        let request_hash =
            json_hash(&serde_json::json!({"admission_id":admission_id,"role":role}))?;
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "runtime_probe_launch",
            input_hash: &request_hash,
            entity_id: admission_id,
        };
        if let Some(existing) = self.read_browser_launch_receipt(receipt)? {
            return self.checked_browser_launch_receipt(receipt, existing);
        }

        let role_text = role.to_string();
        {
            let connection = self.store.lock()?;
            let legacy_target: Option<(String, Option<String>)> = connection.query_row(
                "SELECT entity_id,json_extract(detail_json,'$.role') FROM audit_events
                 WHERE operation_id=?1 AND event_code='runtime_probe.launch' AND entity_kind='runtime_admission'
                 ORDER BY rowid LIMIT 1",
                [operation_id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((prior_admission, prior_role)) = legacy_target {
                if prior_admission != admission_id
                    || prior_role
                        .as_deref()
                        .is_some_and(|prior| prior != role_text.as_str())
                {
                    bail!(CheckedOperationRefused::new(anyhow!(
                        "operation ID was already used with different input"
                    )))
                }
                return Err(crate::trip::outcome_unknown(anyhow!(
                    "The earlier runtime probe launch audit has no exact request receipt"
                )));
            }
            let incomplete: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM trip_runtime_probes p
                   WHERE p.admission_id=?1 AND p.role=?2 AND p.session_id IS NOT NULL
                   AND p.state IN ('authorized','running','awaiting_resume','evidence_recorded','published')
                   AND NOT EXISTS(SELECT 1 FROM audit_events e WHERE e.event_code='runtime_probe.launch'
                       AND e.entity_kind='runtime_admission' AND e.entity_id=p.admission_id
                       AND json_extract(e.detail_json,'$.session_id')=p.session_id))",
                rusqlite::params![admission_id, role_text], |row| row.get(0),
            )?;
            if incomplete {
                return Err(crate::trip::outcome_unknown(anyhow!(
                    "The runtime probe has a reserved session without a confirmed launch receipt"
                )));
            }
        }
        let runtime = crate::trip::CapabilityRuntime::from_store(
            &self.store,
            self.hooks.clone(),
            self.paths.role_socket.clone(),
            self.executable.clone(),
        );
        let mut attempt = None;
        let mut session = None;
        let launch = (|| {
            let attempt_id = {
                let connection = self.store.lock()?;
                let candidate: String = connection.query_row(
                    "SELECT p.attempt_id FROM trip_runtime_probes p
                     JOIN trip_runtime_admissions a ON a.id=p.admission_id
                     WHERE p.admission_id=?1 AND p.role=?2 AND p.state='authorized'
                       AND p.session_id IS NULL AND a.state IN ('authorized','running')
                       AND NOT EXISTS(SELECT 1 FROM sessions s
                           JOIN role_generations rg ON rg.id=s.role_generation_id
                           WHERE rg.attempt_id=p.attempt_id AND s.status IN
                             ('launch_reserved','running','interrupt_requested','recovery_required'))",
                    rusqlite::params![admission_id, role_text], |row| row.get(0),
                ).optional()?.ok_or_else(||anyhow!("ordinary runtime probe is not explicitly authorized or already has active, consumed, or recorded evidence; prepare corrected runtime verification for a fresh scope"))?;
                let attempt_id = attempt.insert(candidate);
                crate::trip::require_current_runtime_admission_scope(
                    &connection,
                    &runtime,
                    admission_id,
                )?;
                attempt_id.as_str()
            };
            let prompt = crate::trip::setup_launch_prompt(&self.store, attempt_id, role)?;
            let context = self
                .store
                .trip_setup_role_launch_context(attempt_id, role, true)?;
            session = Some(context.session_id.clone());
            let permit_id = context.permit_id.clone();
            let dispatch = (|| {
                let policy =
                    crate::trip::runtime_probe_launch_policy(&self.store, attempt_id, role, None)?;
                self.dispatch_context_with_browser_receipt(
                    attempt_id,
                    role,
                    &prompt,
                    None,
                    context,
                    None,
                    None,
                    policy.as_ref(),
                    receipt,
                    None,
                )
            })();
            dispatch.map_err(|error| {
                match self.store.release_unconsumed_launch_permits(
                    Some(&permit_id),
                    "runtime probe launch preparation did not complete",
                ) {
                    Ok(_) => error,
                    Err(cleanup) => crate::trip::outcome_unknown(error.context(format!(
                        "runtime probe permit cleanup also failed: {cleanup:#}"
                    ))),
                }
            })
        })();
        match launch {
            Ok(BrowserLaunchDispatch::Existing(existing)) => {
                self.checked_browser_launch_receipt(receipt, existing)
            }
            Ok(BrowserLaunchDispatch::Launched(result)) => {
                self.record_runtime_probe_launch(receipt, role, &result)
                    .map_err(crate::trip::outcome_unknown)?;
                self.finish_browser_launch(receipt, Ok(BrowserLaunchDispatch::Launched(result)))
                    .map_err(crate::trip::outcome_unknown)
            }
            Err(error) => self.finish_runtime_probe_launch_error(
                receipt,
                &role_text,
                attempt.as_deref(),
                session.as_deref(),
                error,
            ),
        }
    }

    fn record_runtime_probe_launch(
        &self,
        receipt: BrowserLaunchReceipt<'_>,
        role: crate::domain::RoleKind,
        result: &ValidationLaunchResult,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE trip_runtime_probes SET state=CASE WHEN state='authorized' THEN 'running' ELSE state END,
               updated_at=CASE WHEN state='authorized' THEN ?2 ELSE updated_at END
             WHERE admission_id=?3 AND role=?4 AND session_id=?1 AND attempt_id=?5
               AND state IN ('authorized','running','awaiting_resume','evidence_recorded','published','failed','stale')",
            rusqlite::params![result.session_id, now, receipt.entity_id, role.to_string(), result.attempt_id],
        )?;
        if changed != 1 {
            bail!("runtime probe launch lost its exact reserved session binding")
        }
        crate::trip::refresh_runtime_admission_state(&tx, receipt.entity_id, &now)?;
        let detail = serde_json::json!({"role":role,"session_id":result.session_id,"attempt_id":result.attempt_id});
        tx.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human','runtime_probe.launch','runtime_admission',?3,?4,?5)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), receipt.operation_id, receipt.entity_id, detail.to_string(), now],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn finish_runtime_probe_launch_error(
        &self,
        receipt: BrowserLaunchReceipt<'_>,
        role: &str,
        attempt: Option<&str>,
        session: Option<&str>,
        error: anyhow::Error,
    ) -> Result<ValidationLaunchResult> {
        let now = Utc::now().to_rfc3339();
        let reason = format!("{error:#}");
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = crate::store::browser_launch_receipt_in(
            &tx,
            receipt.operation_id,
            receipt.operation_kind,
            receipt.input_hash,
        )?;
        let owned_status: Option<String> = tx
            .query_row(
                "SELECT s.status FROM trip_runtime_probes p JOIN sessions s ON s.id=p.session_id
             WHERE p.admission_id=?1 AND p.role=?2 AND p.attempt_id=?3 AND s.id=?4",
                rusqlite::params![receipt.entity_id, role, attempt, session],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if owned_status.is_none() || existing["state"].as_str() != Some("reserved") {
                drop(tx);
                drop(connection);
                return self.checked_browser_launch_receipt(receipt, existing);
            }
        } else if owned_status.is_none() {
            if !can_confirm_launch_preflight_refusal(&error) {
                return Err(error);
            }
            let preparing: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM launch_permits
                 WHERE attempt_id=?1 AND role=?2 AND state='issued')",
                rusqlite::params![attempt, role],
                |row| row.get(0),
            )?;
            if preparing {
                return Err(crate::trip::outcome_unknown(error.context(
                    "A runtime probe launch is already being prepared for this request target",
                )));
            }
            // Bind the refusal before changing the unlaunched probe. A concurrent
            // request with the same ID must see the same hash or lose reservation.
            crate::store::reserve_browser_launch_receipt_in(
                &tx,
                receipt,
                serde_json::json!({"preflight_rejected":true}),
                &now,
            )?;
            let changed = tx.execute(
                "UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2
                 AND actor_key='human_control' AND operation_kind=?3",
                rusqlite::params![
                    serde_json::json!({"state":"rejected","reason":reason}).to_string(),
                    receipt.operation_id,
                    receipt.operation_kind
                ],
            )?;
            if changed != 1 {
                bail!("browser launch refusal receipt is missing or changed")
            }
        } else {
            return Err(crate::trip::outcome_unknown(anyhow!(
                "The reserved runtime probe session is missing its launch receipt"
            )));
        }
        let spawn_failure = error.downcast_ref::<SpawnFailure>();
        let proven_nondelivery = match spawn_failure {
            Some(SpawnFailure::ProvenNondelivery { .. }) => true,
            Some(SpawnFailure::DeliveryUnknown { .. }) => false,
            None => owned_status.as_deref() == Some("launch_failed"),
        };
        if !error.is::<crate::store::RoleLaunchCapacityError>()
            && (owned_status.is_none() || proven_nondelivery || spawn_failure.is_some())
        {
            let changed = tx.execute(
                "UPDATE trip_runtime_probes SET state='failed',failure_reason=?1,updated_at=?2
                 WHERE admission_id=?3 AND role=?4 AND attempt_id=?5 AND session_id IS ?6
                   AND state='authorized'",
                rusqlite::params![
                    reason,
                    now,
                    receipt.entity_id,
                    role,
                    attempt,
                    session.filter(|_| owned_status.is_some())
                ],
            )?;
            if changed == 1 {
                crate::trip::refresh_runtime_admission_state(&tx, receipt.entity_id, &now)?;
            }
        }
        tx.commit()?;
        drop(connection);
        if owned_status.is_none() {
            return Err(CheckedOperationRefused::from_terminal_receipt(
                receipt.operation_id,
                error,
            )
            .into());
        }
        if proven_nondelivery && can_confirm_launch_preflight_refusal(&error) {
            return self
                .finish_browser_launch(receipt, Err(CheckedOperationRefused::new(error).into()));
        }
        // Keep the receipt reserved when the process may have started. HTTP
        // already classifies the native delivery-unknown error as uncertain.
        if matches!(spawn_failure, Some(SpawnFailure::DeliveryUnknown { .. })) {
            return Err(error);
        }
        Err(crate::trip::outcome_unknown(error))
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
        let changed = connection.execute(
            "UPDATE trip_runtime_probes SET state=CASE WHEN state='awaiting_resume' THEN 'running' ELSE state END,
               updated_at=CASE WHEN state IN ('running','awaiting_resume') THEN ?1 ELSE updated_at END
             WHERE admission_id=?2 AND role=?3 AND session_id=?4
               AND state IN ('running','awaiting_resume','evidence_recorded','published','failed','stale')",
            rusqlite::params![now,admission_id,role.to_string(),session],
        )?;
        if changed != 1 {
            bail!("runtime probe resume lost its exact retained session binding")
        }
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
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "runtime_probe_resume",
            input_hash: &request_hash,
            entity_id: admission_id,
        };
        if let Some(existing) = self.read_browser_launch_receipt(receipt)? {
            return self.checked_browser_launch_receipt(receipt, existing);
        }
        self.finish_browser_launch(
            receipt,
            self.resume_runtime_probe_inner(admission_id, role, Some(receipt)),
        )
    }

    fn read_browser_launch_receipt(
        &self,
        receipt: BrowserLaunchReceipt<'_>,
    ) -> Result<Option<serde_json::Value>> {
        self.store.browser_launch_receipt(
            receipt.operation_id,
            receipt.operation_kind,
            receipt.input_hash,
        )
    }

    fn checked_browser_launch_receipt(
        &self,
        request: BrowserLaunchReceipt<'_>,
        receipt: serde_json::Value,
    ) -> Result<ValidationLaunchResult> {
        let state = receipt["state"].as_str();
        if state == Some("reserved") {
            return browser_launch_receipt(receipt).map_err(crate::trip::outcome_unknown);
        }
        if state != Some("rejected") {
            return browser_launch_receipt(receipt);
        }
        // Older writers recorded errors after reservation as rejected even when
        // delivery was unknown. Only an explicit proof can settle their replay.
        let refused = receipt["proven_nondelivery"].as_bool() == Some(true)
            || self.store.lock()?.query_row(
                "SELECT COALESCE(json_extract(detail_json,'$.authority.preflight_rejected'),0)
                 FROM audit_events WHERE operation_id=?1 AND event_code='browser.launch.reserved'
                   AND json_extract(detail_json,'$.operation_kind')=?2
                   AND json_extract(detail_json,'$.input_hash')=?3
                 ORDER BY rowid DESC LIMIT 1",
                rusqlite::params![
                    request.operation_id,
                    request.operation_kind,
                    request.input_hash
                ],
                |row| row.get::<_, bool>(0),
            )?;
        browser_launch_receipt(receipt).map_err(|error| {
            if refused {
                CheckedOperationRefused::from_terminal_receipt(request.operation_id, error).into()
            } else {
                crate::trip::outcome_unknown(error)
            }
        })
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
            Ok(BrowserLaunchDispatch::Existing(existing)) => {
                self.checked_browser_launch_receipt(receipt, existing)
            }
            Err(error) => match self.read_browser_launch_receipt(receipt)? {
                Some(existing) if existing["state"].as_str() == Some("reserved") => {
                    let refused = error.is::<CheckedOperationRefused>()
                        || matches!(
                            error.downcast_ref::<SpawnFailure>(),
                            Some(SpawnFailure::ProvenNondelivery { .. })
                        );
                    if !refused {
                        return Err(crate::trip::outcome_unknown(error));
                    }
                    // Persist the proof so replay can distinguish this outcome
                    // from historical rejections after an ambiguous dispatch.
                    self.store.finalize_browser_launch_receipt(
                        receipt.operation_id,
                        receipt.operation_kind,
                        receipt.input_hash,
                        Some(serde_json::json!({
                            "state":"rejected", "reason":format!("{error:#}"),
                            "proven_nondelivery":true,
                        })),
                        None,
                    )?;
                    Err(
                        CheckedOperationRefused::from_terminal_receipt(receipt.operation_id, error)
                            .into(),
                    )
                }
                Some(existing) => self.checked_browser_launch_receipt(receipt, existing),
                None => {
                    if !can_confirm_launch_preflight_refusal(&error) {
                        return Err(error);
                    }
                    self.store.reject_browser_launch_without_reservation(
                        receipt.operation_id,
                        receipt.operation_kind,
                        receipt.input_hash,
                        receipt.entity_id,
                        &format!("{error:#}"),
                    )?;
                    // A concurrent request can reserve the ID before this
                    // write, so settle against the receipt that actually won.
                    let existing = self
                        .read_browser_launch_receipt(receipt)?
                        .context("browser launch preflight refusal receipt is missing")?;
                    self.checked_browser_launch_receipt(receipt, existing)
                }
            },
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
        {
            let connection = self.store.lock()?;
            crate::store::require_failure_stop_released(&connection, attempt_id)?;
        }
        let release_permit = |error: anyhow::Error, reason: &str| match self
            .store
            .release_unconsumed_launch_permits(Some(&context.permit_id), reason)
        {
            Ok(_) => error,
            Err(cleanup) => {
                let original = format!("{error:#}");
                let error = error.context(format!(
                    "launch permit cleanup also failed after `{original}`: {cleanup:#}"
                ));
                if browser_receipt.is_some() {
                    crate::trip::outcome_unknown(error)
                } else {
                    error
                }
            }
        };
        if !self.dispatch_enabled() {
            return Err(release_permit(
                anyhow!("service is draining; new role dispatch is disabled"),
                "dispatch rejected while service draining",
            ));
        }
        let checkout_scope = self.checkout_scope(attempt_id).map_err(|error| {
            release_permit(error, "checkout authority could not be established")
        })?;
        let mut read_denials = match crate::trip::setup_target_read_denials(&self.store, attempt_id)
        {
            Ok(paths) => paths,
            Err(error) => {
                return Err(release_permit(
                    error,
                    "setup target read confinement could not be established",
                ))
            }
        };
        if let Some(scope) = &checkout_scope {
            read_denials = serde_json::from_value(scope["protected_roots"].clone())?;
        }
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
        let mut launch = match prepared_launch {
            Ok(launch) => launch,
            Err(error) => return Err(release_permit(error, "provider launch preparation failed")),
        };
        let runtime_probe: bool = self.store.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM launch_permits launch
               JOIN trip_setup_permits setup ON setup.id=launch.setup_permit_id
               WHERE launch.id=?1 AND launch.attempt_id=?2 AND launch.role=?3 AND setup.purpose='runtime_probe')",
            rusqlite::params![context.permit_id, attempt_id, role.to_string()], |row| row.get(0),
        )?;
        if runtime_probe {
            let policy = crate::trip::runtime_probe_security_policy(&*self.store.lock()?, attempt_id, role)?;
            providers::retain_qualification_contract(&mut launch.config, &policy)?;
        } else if let Some(payload) = self.store.lock()?.query_row(
            "SELECT payload_json FROM controls WHERE attempt_id=?1 AND kind='main_checkout_verify'
               AND state='native_requested' AND json_extract(payload_json,'$.actor')=?2",
            rusqlite::params![attempt_id, role.to_string()], |row| row.get::<_, String>(0),
        ).optional()? {
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            providers::retain_qualification_contract(&mut launch.config, &payload)?;
        }
        if let Some(scope) = &checkout_scope {
            let policy = self
                .main_readonly_policy(attempt_id, role, scope)
                .map_err(|error| {
                    release_permit(error, "Main read-only command preparation failed")
                })?;
            providers::bind_checkout_launch(&mut launch, scope, policy.as_ref())
                .map_err(|error| release_permit(error, "Main checkout launch binding failed"))?;
        }
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
        let prompt_hash = launch.request_prompt_hash(prompt).map_err(|error| {
            release_permit(error, "prepared provider request differs from launch input")
        })?;
        let reservation = if let Some(receipt) = browser_receipt {
            if let Some(fresh_resume_rejection) = fresh_resume_rejection {
                self.store
                    .reserve_role_invocation_with_browser_receipt_and_fresh_rejection(
                        &context,
                        &launch.config,
                        &prompt_hash,
                        receipt,
                        fresh_resume_rejection,
                    )
            } else {
                self.store.reserve_role_invocation_with_browser_receipt(
                    &context,
                    &launch.config,
                    &prompt_hash,
                    receipt,
                )
            }
        } else {
            self.store
                .reserve_role_invocation_with_prompt_hash(&context, &launch.config, &prompt_hash)
                .map(|()| BrowserLaunchReservation::Reserved)
        };
        match reservation {
            Ok(BrowserLaunchReservation::Reserved) => {}
            Ok(BrowserLaunchReservation::Existing(existing)) => {
                if runtime_probe {
                    self.store.release_unconsumed_launch_permits(
                        Some(&context.permit_id),
                        "runtime probe request already has a durable receipt",
                    )?;
                }
                return Ok(BrowserLaunchDispatch::Existing(existing));
            }
            Err(error) => {
                return Err(release_permit(
                    error,
                    "authoritative role reservation rejected",
                ));
            }
        }
        if self.checkout_scope(attempt_id)? != checkout_scope {
            let error = anyhow!(
                "Checkout protections changed after reservation; no role process was started"
            );
            self.store
                .update_session_launch_failed(&context.session_id, &error.to_string())?;
            return Err(if browser_receipt.is_some() {
                CheckedOperationRefused::new(error).into()
            } else {
                error
            });
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
                &SessionOwner::RoleGeneration(context.role_generation_id.clone()),
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
        let prior_trip_progress = if let HumanCommand::Trip {
            operation_id,
            action,
        } = command
        {
            if let Some(result) =
                crate::trip::replay_human_command(&self.store, operation_id, action)?
            {
                return Ok(result);
            }
            crate::trip::has_human_command_progress(&self.store, operation_id, action)?
        } else {
            false
        };
        self.execute_human_command_inner(command).map_err(|error| {
            if prior_trip_progress {
                return crate::trip::outcome_unknown(error);
            }
            if let HumanCommand::Trip {
                operation_id,
                action,
            } = command
            {
                crate::trip::classify_trip_error(&self.store, operation_id, action, error)
            } else {
                error
            }
        })
    }

    fn execute_human_command_inner(&self, command: &HumanCommand) -> Result<OperationResult> {
        if matches!(
            command,
            HumanCommand::AuthorizeRecipeScheduleExecution { .. }
        ) {
            return crate::workflow::execute(&self.store, command);
        }
        if matches!(command, HumanCommand::StopRecipeScheduleOccurrence { .. }) {
            let result = crate::workflow::execute(&self.store, command)?;
            self.reconcile_schedule_execution_limits()?;
            return Ok(result);
        }
        if matches!(command, HumanCommand::StopFailedSession { .. }) {
            return self.supervisor.stop_failed_session(command);
        }
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
                return (|| -> Result<OperationResult> {
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
                                Err(error) => crate::trip::SetupManagerSignalDelivery::Failed(
                                    format!("manager-only signal delivery failed: {error:#}"),
                                ),
                            }
                        }
                        None => crate::trip::SetupManagerSignalDelivery::NotRunning,
                    };
                    crate::trip::finish_setup_manager_stop(
                        &self.store,
                        &preparation,
                        signal_delivery,
                    )
                })()
                .map_err(crate::trip::outcome_unknown);
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
        {
            let connection = self.store.lock()?;
            let frozen = crate::knowledge::attempt_knowledge_manifest_in(&connection, attempt_id)?;
            if frozen.is_none()
                && crate::knowledge::attempt_knowledge_manifest_in(&connection, &parent)?.is_some()
            {
                bail!("The continuation is missing its frozen knowledge manifest; restore the attempt record before materializing it")
            }
        }
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
        let (parent_path, parent_policy): (String, String) = self.store.lock()?.query_row(
            "SELECT path,policy_json FROM workspaces WHERE attempt_id=?1",
            params![parent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut child_policy: serde_json::Value = serde_json::from_str(&parent_policy)?;
        let borrowed = crate::workspace::policy_mode(&child_policy)?
            == crate::domain::WorkspaceMode::MainCheckout;
        let destination = if borrowed {
            std::path::PathBuf::from(parent_path).canonicalize()?
        } else {
            self.paths.artifacts.join("worktrees").join(attempt_id)
        };
        if borrowed {
            crate::workflow::validate_workspace_checkout(
                &*self.store.lock()?,
                &task,
                &child_policy,
            )?;
            if child_policy["baseline_snapshot_id"].as_str().is_none() {
                bail!("Main rework has no original checkout baseline")
            }
            child_policy
                .as_object_mut()
                .ok_or_else(|| anyhow!("Main workspace policy must be an object"))?
                .remove("main_verification");
            child_policy["lineage_snapshot_id"] = snapshot.clone().into();
            child_policy["rework_start_snapshot_id"] = snapshot.clone().into();
        } else {
            child_policy = serde_json::json!({"lineage_snapshot_id":snapshot,"no_auto_commit":true,"no_auto_merge":true});
        }
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
            if let Err(error) = self
                .store
                .require_schedule_preparation_in(&transaction, attempt_id)
            {
                if error.is::<crate::review::SingleAgentReviewDeferred>() {
                    return Ok(false);
                }
                // A reserved intent has not touched the filesystem. Interrupted
                // materialization retains its files and the parent's claim for recovery.
                let workspace_recorded: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM workspaces WHERE attempt_id=?1)",
                    [attempt_id],
                    |row| row.get(0),
                )?;
                let uncertain = intent_state == "materializing"
                    || workspace_recorded
                    || destination.try_exists()?
                    || crate::workflow::single_agent_quiescence(&transaction, attempt_id).is_err();
                transaction.execute(
                    "UPDATE rework_intents SET state=?1,result_json=?2,updated_at=?3 WHERE id=?4 AND state IN ('reserved','materializing')",
                    params![if uncertain { "recovery_required" } else { "cancelled" },
                        serde_json::json!({"reason":format!("{error:#}"),"claim_retained":true,"cancellation_pending":uncertain}).to_string(),now,intent],
                )?;
                transaction.execute("UPDATE attempts SET status=?1,updated_at=?2 WHERE id=?3 AND status='materialization_pending'",
                    params![if uncertain { "needs_recovery" } else { "cancelled" },now,attempt_id])?;
                if uncertain {
                    transaction.execute("UPDATE workspaces SET state='recovery_required',updated_at=?1 WHERE attempt_id=?2 AND state='reserved'",params![now,attempt_id])?;
                    transaction.execute(
                        "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2",
                        params![now, task],
                    )?;
                }
                transaction.commit()?;
                return Err(error.context(
                    "Scheduled repair preparation was fenced; the parent claim was retained",
                ));
            }
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
            if borrowed {
                crate::workflow::validate_workspace_checkout(&transaction, &task, &child_policy)?;
            }
            transaction.execute("INSERT OR IGNORE INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5,?6,'reserved',?7,?7)",params![workspace_id,attempt_id,identity,destination.to_string_lossy(),base,child_policy.to_string(),now])?;
            transaction.commit()?;
        }
        let materialized = if borrowed || destination.exists() {
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
        let trip_policy = {
            let connection = self.store.lock()?;
            crate::workflow::attempt_family(&connection, attempt_id)?
                == crate::domain::WorkflowFamily::Trip
        };
        let materialized = if trip_policy {
            crate::trip::materialize_project_policy(&self.store, attempt_id, &destination)
                .map(|_| ())
        } else {
            Ok(())
        };
        if let Err(error) = materialized {
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
        if borrowed {
            crate::workflow::validate_workspace_checkout(&transaction, &task, &child_policy)?;
            let policy: String = transaction.query_row(
                "SELECT policy_json FROM workspaces WHERE attempt_id=?1",
                params![attempt_id],
                |row| row.get(0),
            )?;
            let policy: serde_json::Value = serde_json::from_str(&policy)?;
            if policy["checkout_authorization"] != child_policy["checkout_authorization"]
                || policy["baseline_snapshot_id"] != child_policy["baseline_snapshot_id"]
            {
                bail!("Main rework authority or original baseline changed before claim transfer")
            }
        }
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
        if trip_policy {
            crate::trip::validate_materialized_policy(&policy_json)?;
        }
        if let Err(error) = self
            .store
            .require_schedule_preparation_in(&transaction, attempt_id)
        {
            if error.is::<crate::review::SingleAgentReviewDeferred>() {
                return Ok(false);
            }
            transaction.execute(
                "UPDATE rework_intents SET state='recovery_required',result_json=?1,updated_at=?2 WHERE id=?3 AND state='materializing'",
                params![serde_json::json!({"reason":format!("{error:#}"),"claim_retained":true,"cancellation_pending":true}).to_string(),finished,intent],
            )?;
            transaction.execute("UPDATE workspaces SET state='recovery_required',updated_at=?1 WHERE attempt_id=?2 AND state='reserved'",params![finished,attempt_id])?;
            transaction.execute("UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2 AND status='materialization_pending'",params![finished,attempt_id])?;
            transaction.execute(
                "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2",
                params![finished, task],
            )?;
            transaction.commit()?;
            return Err(error.context("Scheduled repair publication was fenced; materialized files and the parent claim were retained"));
        }
        if !eligible {
            return Ok(false);
        }
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

pub(crate) fn require_schedule_member_effects_quiescent_in(
    connection: &rusqlite::Connection,
    attempt: &str,
) -> Result<()> {
    // The final fence checks a running parent claim for transfer. Stop settlement
    // instead verifies the exact current workspace/claim tuple under preparation guards.
    for (blocker, sql) in REWORK_PARENT_FENCES
        .iter()
        .take(REWORK_PARENT_FENCES.len() - 1)
    {
        if connection.query_row(sql, [attempt], |row| row.get::<_, bool>(0))? {
            bail!("Scheduled occurrence still owns unresolved work: {blocker}")
        }
    }
    Ok(())
}

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

fn can_confirm_launch_preflight_refusal(error: &anyhow::Error) -> bool {
    !error.is::<crate::trip::TripOutcomeUnknown>()
        && !error.is::<serde_json::Error>()
        && !matches!(error.downcast_ref::<rusqlite::Error>(), Some(sqlite)
            if !matches!(sqlite, rusqlite::Error::QueryReturnedNoRows))
        && !matches!(
            error.downcast_ref::<SpawnFailure>(),
            Some(SpawnFailure::DeliveryUnknown { .. })
        )
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

impl Application {
    pub fn execute_workflow_command(
        &self,
        command: &crate::domain::WorkflowCommand,
    ) -> Result<OperationResult> {
        if let crate::domain::WorkflowCommand::ResumeRun { .. } = command {
            return self.resume_single_agent_command(command);
        }
        crate::workflow::execute_workflow_command(&self.store, command)
    }

    /// A person's resume of a held writer, bound to the exact task version,
    /// session and transcript epoch they reviewed. Replaying the operation
    /// returns its first outcome.
    fn resume_single_agent_command(
        &self,
        command: &crate::domain::WorkflowCommand,
    ) -> Result<OperationResult> {
        let crate::domain::WorkflowCommand::ResumeRun {
            operation_id,
            task_id,
            attempt_id,
            expected_version,
            session_id,
            transcript_epoch,
            message,
        } = command
        else {
            bail!("not a resume command")
        };
        if operation_id.trim().is_empty() || operation_id.len() > 128 {
            bail!("operation_id is required")
        }
        if message.as_ref().is_some_and(|text| text.len() > 16 * 1024) {
            bail!("The resume message is limited to 16 KiB")
        }
        let request_hash = json_hash(command)?;
        if let Some(existing) = self.store.operation_receipt(
            operation_id,
            "human_control",
            "workflow_command",
            &request_hash,
        )? {
            return Ok(serde_json::from_value(existing)?);
        }
        {
            let connection = self.store.lock()?;
            let current: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM tasks t JOIN attempts a ON a.task_id=t.id
                   JOIN sessions s ON s.id=?3 AND s.transcript_epoch=?4
                   JOIN execution_bindings eb ON eb.role_generation_id=s.role_generation_id AND eb.attempt_id=a.id
                   WHERE t.id=?1 AND a.id=?2 AND t.version=?5)",
                rusqlite::params![task_id, attempt_id, session_id, transcript_epoch, expected_version],
                |row| row.get(0),
            )?;
            if !current {
                bail!("The task, its writer session or transcript changed since you reviewed it; reload before resuming")
            }
        }
        self.launch_single_agent_turn(
            attempt_id,
            Some(crate::workflow::SingleAgentResumeMode::Explicit),
            message.as_deref(),
            Some(&crate::store::SingleAgentResumeBinding {
                operation_id,
                request_hash: &request_hash,
                task_id,
                attempt_id,
                expected_version: *expected_version,
                session_id,
                transcript_epoch,
            }),
            false,
        )?;
        let stored = self
            .store
            .operation_receipt(
                operation_id,
                "human_control",
                "workflow_command",
                &request_hash,
            )?
            .ok_or_else(|| anyhow!("the resume receipt was not recorded"))?;
        Ok(serde_json::from_value(stored)?)
    }

    /// Continues the run's retained writer conversation for the coordinator
    /// after a planned shutdown.
    pub fn resume_single_agent(
        &self,
        attempt_id: &str,
        message: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.launch_single_agent_turn(
            attempt_id,
            Some(crate::workflow::SingleAgentResumeMode::Automatic),
            message,
            None,
            false,
        )
    }

    /// Launches a claimed Single agent run's one writer in a fresh native
    /// conversation. The reservation rechecks every launch condition; a later
    /// launch of the same run is refused because its conversation is retained.
    pub fn dispatch_single_agent(&self, attempt_id: &str) -> Result<serde_json::Value> {
        self.launch_single_agent_turn(attempt_id, None, None, None, false)
    }

    fn checkout_scope(&self, attempt_id: &str) -> Result<Option<serde_json::Value>> {
        let policy = {
            let connection = self.store.lock()?;
            let (task, policy): (String, Option<String>) = connection.query_row(
                "SELECT a.task_id,w.policy_json FROM attempts a
                LEFT JOIN workspaces w ON w.attempt_id=a.id WHERE a.id=?1",
                rusqlite::params![attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(policy) = policy else {
                if crate::workflow::task_workspace_mode(&connection, &task)?
                    == crate::domain::WorkspaceMode::MainCheckout
                {
                    bail!("Main execution requires its prepared borrowed checkout before launch or resume")
                }
                return Ok(None);
            };
            let policy: serde_json::Value = serde_json::from_str(&policy)?;
            crate::workflow::validate_workspace_checkout(&connection, &task, &policy)?;
            policy
        };
        if crate::workspace::policy_mode(&policy)? != crate::domain::WorkspaceMode::MainCheckout {
            return Ok(None);
        }
        let scope = providers::checkout_launch_scope(
            &policy,
            &self.paths.root,
            &self.paths.role_socket,
            &self.store.repository_inventory()?,
            &self.executable,
        )?;
        self.store.check_checkout_preparation(Some(&scope))?;
        Ok(Some(scope))
    }

    fn main_readonly_policy(
        &self,
        attempt: &str,
        role: crate::domain::RoleKind,
        scope: &serde_json::Value,
    ) -> Result<Option<providers::RuntimeProbeCommandPolicy>> {
        if role == crate::domain::RoleKind::Implementer {
            return Ok(None);
        }
        let payload =
            crate::store::main_readonly_control_payload_in(&*self.store.lock()?, attempt, role)?
                .context("Verify this Main read-only role before preparing its native commands")?;
        providers::require_main_protection_partition(&payload["scope"])?;
        let request = match crate::domain::QualificationContract::from_record(
            &payload["observation_request"],
        )
        .map_err(anyhow::Error::msg)?
        {
            crate::domain::QualificationContract::Legacy => {
                providers::main_checkout_observation_request(scope, &self.paths.database)?
            }
            crate::domain::QualificationContract::FunctionalV2 => {
                providers::validate_main_functional_request(
                    scope,
                    &payload["observation_request"],
                )?;
                payload["observation_request"].clone()
            }
        };
        let policy = providers::main_readonly_command_policy(
            scope,
            &request,
            &payload["readonly_probe_paths"],
        )?;
        if payload["scope"] != *scope
            || payload["observation_request"] != request
            || payload["scope_hash"].as_str() != Some(json_hash(scope)?.as_str())
            || payload["observation_request_hash"].as_str() != Some(json_hash(&request)?.as_str())
            || payload["readonly_command_policy"] != serde_json::to_value(&policy)?
        {
            bail!("The frozen Main read-only commands or protections changed; verify the current scope again")
        }
        Ok(Some(policy))
    }

    pub(crate) fn record_functional_main_checkout_proof(
        &self,
        proof: &crate::domain::FunctionalCapabilityProofInput,
    ) -> Result<serde_json::Value> {
        let mut result = self.store.record_functional_main_checkout_proof(proof)?;
        let payload = &result["retained_observations"];
        let control: String = self.store.lock()?.query_row(
            "SELECT id FROM controls WHERE kind='main_checkout_verify' AND state='finished'
               AND json_extract(payload_json,'$.session_id')=?1 AND json_extract(payload_json,'$.probe_id')=?2",
            rusqlite::params![proof.session_id,payload["probe_id"].as_str()], |row| row.get(0),
        )?;
        if let Some(error) = self.cleanup_terminal_main_artifacts(&control, payload)? {
            result["artifact_cleanup_error"] = error.into();
        }
        Ok(result)
    }

    fn cleanup_terminal_main_artifacts(
        &self,
        control: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<String>> {
        let error = cleanup_functional_main_artifacts(payload)
            .err()
            .map(|error| format!("{error:#}"));
        if let Some(error) = &error {
            let changed = self.store.lock()?.execute(
                "UPDATE controls SET payload_json=json_set(payload_json,'$.artifact_cleanup_error',?1),updated_at=?2
                 WHERE id=?3 AND kind='main_checkout_verify' AND state IN ('cancelled','failed','finished')
                   AND json_extract(payload_json,'$.probe_id')=?4",
                rusqlite::params![error,Utc::now().to_rfc3339(),control,payload["probe_id"].as_str()],
            )?;
            if changed != 1 {
                bail!("Main artifact cleanup lost its terminal control ownership")
            }
        }
        Ok(error)
    }

    fn settle_main_verification_launch_failure(
        &self,
        control: &str,
        attempt: &str,
        task: &str,
        prepared: &serde_json::Value,
        error: &anyhow::Error,
    ) -> Result<serde_json::Value> {
        use rusqlite::{params, OptionalExtension};
        let nonce = prepared["nonce"]
            .as_str()
            .context("Main preparation lost its nonce binding")?;
        let reason: String = format!("{error:#}")
            .replace(nonce, "[redacted]")
            .chars()
            .take(4096)
            .collect();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (json, state): (String, String) = tx.query_row(
            "SELECT payload_json,state FROM controls WHERE id=?1 AND attempt_id=?2 AND kind='main_checkout_verify'
             AND state IN ('native_requested','native_running','native_stopping','cancelled','failed')",
            params![control, attempt], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut payload: serde_json::Value = serde_json::from_str(&json)?;
        if payload["probe_id"] != prepared["probe_id"] {
            bail!("Main verification lost its exact preparation ownership")
        }
        if prepared["functional_challenge_created"] == true {
            payload["functional_challenge_created"] = true.into();
            payload["scope"] = prepared["scope"].clone();
            if prepared.get("observation_request").is_some() {
                payload["observation_request"] = prepared["observation_request"].clone();
            }
        }
        if payload.get("launch_reservation").is_none() {
            if let Some(reservation) = prepared.get("launch_reservation") {
                payload["launch_reservation"] = reservation.clone();
            }
        }
        let reservation = &payload["launch_reservation"];
        let session = payload["session_id"]
            .as_str()
            .or(reservation["session_id"].as_str());
        let generation = payload["role_generation_id"]
            .as_str()
            .or(reservation["role_generation_id"].as_str());
        let epoch = payload["transcript_epoch"]
            .as_str()
            .or(reservation["transcript_epoch"].as_str());
        let owned: Option<(String, String, bool)> = tx
            .query_row(
                "SELECT s.id,s.launch_state,s.status='launch_failed' AND s.launch_state='failed'
               AND s.process_identity_json IS NULL
             FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
             WHERE s.id=?1 AND g.id=?2 AND s.transcript_epoch=?3 AND g.attempt_id=?4 AND g.role=?5",
                params![
                    session,
                    generation,
                    epoch,
                    attempt,
                    payload["actor"].as_str()
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let any_reserved_owner: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 OR role_generation_id=?2 OR transcript_epoch=?3)
             OR EXISTS(SELECT 1 FROM role_generations WHERE id=?2)",
            params![session, generation, epoch], |row| row.get(0),
        )?;
        // A reservation or spawn marker is ownership evidence, never proof of nondelivery.
        let not_started = payload["turns_reserved"].as_u64().unwrap_or(0) == 0
            && !any_reserved_owner
            && payload.get("session_id").is_none();
        let nondelivery = owned.as_ref().is_some_and(|(_, _, failed)| *failed);
        let terminal = not_started || nondelivery;
        let recovery_session = owned
            .as_ref()
            .filter(|(_, state, _)| state != "delivery_unknown")
            .map(|(id, _, _)| id.clone());
        let owner = owned.as_ref().map(|(id, _, _)| {
            serde_json::json!({"session_id":id,
            "role_generation_id":generation,"transcript_epoch":epoch})
        });
        if let Some(owner) = owner {
            for field in ["session_id", "role_generation_id", "transcript_epoch"] {
                payload[field] = owner[field].clone();
            }
        }
        payload["failure_reason"] = reason.clone().into();
        payload["launch_disposition"] = if not_started {
            "not_started"
        } else if nondelivery {
            "proven_nondelivery"
        } else {
            "recovery_required"
        }
        .into();
        tx.execute(
            "UPDATE controls SET state=?1,payload_json=?2,updated_at=?3 WHERE id=?4",
            params![
                if !terminal {
                    "recovery_required"
                } else if state == "cancelled" {
                    "cancelled"
                } else {
                    "failed"
                },
                payload.to_string(),
                Utc::now().to_rfc3339(),
                control
            ],
        )?;
        tx.execute(
            "UPDATE attempts SET status='needs_input' WHERE id=?1 AND status IN ('running','held','needs_input')",
            params![attempt],
        )?;
        tx.execute(
            "UPDATE tasks SET attention='needs_input' WHERE id=?1 AND lifecycle='in_progress'
             AND attention IN ('none','paused','needs_input')",
            params![task],
        )?;
        tx.commit()?;
        drop(connection);
        if terminal {
            if let Some(permit) = payload["launch_reservation"]["permit_id"].as_str() {
                self.store.release_unconsumed_launch_permits(
                    Some(permit),
                    "Main verification preparation failed",
                )?;
            }
            let _ = self.cleanup_terminal_main_artifacts(control, &payload)?;
        } else if let Some(session) = recovery_session {
            self.store
                .mark_session_delivery_ambiguous(&session, &reason)?;
        }
        Ok(
            serde_json::json!({"action":if terminal {"main_verification_failed"} else {"main_verification_recovery_required"},
            "control_id":control,"retry_started":false,"success_proof":false,"not_started":not_started,
            "reason":reason,"message":if terminal {"Main verification could not start. Review this failure before requesting a new verification."}
                else {"Main verification may have started. Reconcile its exact session before requesting another verification."}}),
        )
    }

    fn settle_main_checkout_verification(&self) -> Result<Option<serde_json::Value>> {
        use rusqlite::{params, OptionalExtension};
        let pending: Option<(String,String,String)> = self.store.lock()?.query_row("SELECT c.id,c.attempt_id,c.payload_json FROM controls c
            JOIN workspaces w ON w.attempt_id=c.attempt_id AND w.state='ready' WHERE c.kind='main_checkout_verify'
            AND (c.state IN ('native_requested','native_running','native_stopping')
              OR (c.state='awaiting_native_evidence' AND (EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
                  WHERE a.id=c.attempt_id AND t.lifecycle='cancelled')
                OR EXISTS(SELECT 1 FROM controls stop WHERE stop.attempt_id=c.attempt_id
                  AND stop.kind IN ('cancel','pause_now','pause_after_role') AND stop.state IN ('requested','draining')))))
            ORDER BY c.created_at,c.rowid LIMIT 1",
            [], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        let Some((control, attempt, json)) = pending else {
            return Ok(None);
        };
        let mut payload: serde_json::Value = serde_json::from_str(&json)?;
        let functional = crate::domain::QualificationContract::from_record(&payload)
            .map_err(anyhow::Error::msg)?
            == crate::domain::QualificationContract::FunctionalV2;
        let task = payload["task_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Verification has no task"))?
            .to_owned();
        let actor = payload["actor"]
            .as_str()
            .ok_or_else(|| anyhow!("Verification has no actor"))?
            .to_owned();
        let nonce = payload["nonce"]
            .as_str()
            .ok_or_else(|| anyhow!("Verification has no nonce"))?
            .to_owned();
        let probe = payload["probe_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Verification has no owned output identity"))?
            .to_owned();
        if [nonce.as_str(), probe.as_str()]
            .iter()
            .any(|value| value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            bail!("Malformed Main verification output binding")
        }
        let expired = payload["deadline_at"]
            .as_str()
            .is_none_or(|deadline| deadline <= Utc::now().to_rfc3339().as_str());
        let cancelled: bool=self.store.lock()?.query_row("SELECT t.lifecycle='cancelled' OR EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
            AND kind IN ('cancel','pause_now','pause_after_role') AND state IN ('requested','draining')) FROM tasks t WHERE t.id=?2",
            params![attempt,task], |row|row.get(0))?;
        let turns = payload["turns_reserved"].as_u64().unwrap_or(0);
        let evidence_pending: bool = self.store.lock()?.query_row(
            "SELECT state='awaiting_native_evidence' FROM controls WHERE id=?1",
            params![control],
            |row| row.get(0),
        )?;
        if evidence_pending && !cancelled {
            return Ok(None);
        }
        if turns == 0 && !expired && !cancelled {
            let started = (|| -> Result<serde_json::Value> {
                if functional {
                    if payload["functional_challenge_created"] == true
                        || payload.get("launch_reservation").is_some()
                    {
                        bail!("Main verification already attempted preparation; inspect this verification before starting a new one")
                    }
                    self.store.preflight_main_verification(
                        &attempt,
                        actor.parse().map_err(|error: String| anyhow!(error))?,
                    )?;
                }
                let root = self
                    .checkout_scope(&attempt)?
                    .ok_or_else(|| anyhow!("Verification lost Main scope"))?;
                if functional {
                    payload["scope"] = root.clone();
                    payload["scope_hash"] = json_hash(&root)?.into();
                }
                let path = std::path::Path::new(root["root"].as_str().unwrap())
                    .join(format!(".llmrelay-main-{probe}-first"));
                if !functional || actor == "implementer" {
                    match std::fs::symlink_metadata(&path) {
                        Ok(_) => bail!(
                            "Verification output {} already exists; cancel without overwriting it",
                            path.display()
                        ),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!("Cannot inspect verification output {}", path.display())
                            })
                        }
                    }
                }
                let request = if functional {
                    if actor == "single_agent" {
                        bail!("Single agent Main verification has no functional role contract")
                    }
                    use std::io::Write;
                    let challenge = std::path::Path::new(root["root"].as_str().unwrap())
                        .join(format!(".llmrelay-main-{probe}-challenge"));
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&challenge)
                        .context(
                            "Create the exact owned Main challenge without replacing a file",
                        )?;
                    payload["functional_challenge_created"] = true.into();
                    if self.store.lock()?.execute(
                    "UPDATE controls SET payload_json=?1 WHERE id=?2 AND state='native_requested'",
                    params![payload.to_string(), control],
                )? != 1 {
                    bail!("Main challenge preparation lost its current control ownership")
                }
                    file.write_all(nonce.as_bytes())?;
                    providers::main_checkout_functional_observation_request(
                        &root,
                        &challenge,
                        (actor == "implementer").then_some(path.as_path()),
                    )?
                } else {
                    providers::main_checkout_observation_request(&root, &self.paths.database)?
                };
                payload["observation_request_hash"] = json_hash(&request)?.into();
                payload["observation_request"] = request.clone();
                if !functional {
                    payload["scope"] = root.clone();
                    payload["scope_hash"] = json_hash(&root)?.into();
                }
                let writable = actor == "single_agent" || actor == "implementer";
                let readonly_probes = if functional {
                    if !writable {
                        payload["readonly_command_policy"] =
                            serde_json::to_value(providers::main_readonly_command_policy(
                                &root,
                                &request,
                                &serde_json::Value::Null,
                            )?)?;
                    }
                    String::new()
                } else if writable {
                    String::new()
                } else {
                    let compound = format!(".llmrelay-main-{probe}-compound");
                    let redirect = format!(".llmrelay-main-{probe}-redirect");
                    for name in [&compound, &redirect] {
                        let artifact =
                            std::path::Path::new(root["root"].as_str().unwrap()).join(name);
                        match std::fs::symlink_metadata(&artifact) {
                        Ok(_) => bail!("Read-only verification output {} already exists; cancel without overwriting it",artifact.display()),
                        Err(error) if error.kind()==std::io::ErrorKind::NotFound => {},
                        Err(error) => return Err(error).with_context(||format!("Cannot inspect read-only verification output {}",artifact.display())),
                    }
                    }
                    payload["readonly_probe_paths"] = serde_json::json!([
                        format!(".llmrelay-main-{probe}-first"),
                        compound,
                        redirect
                    ]);
                    let policy = providers::main_readonly_command_policy(
                        &root,
                        &request,
                        &payload["readonly_probe_paths"],
                    )?;
                    payload["readonly_command_policy"] = serde_json::to_value(&policy)?;
                    format!("Remain read-only: writing tools are disabled. Run only these four exact commands once each and preserve their native tool results:\n{}\nThe three write probes use fixed harmless bytes, not the conversation nonce. If a probe leaves an artifact, report its exact path without deleting or resetting it.",
                    policy.commands.iter().map(|entry|entry.command.as_str()).collect::<Vec<_>>().join("\n"))
                };
                let command = request["command"].as_str().unwrap();
                let prompt = if functional {
                    crate::trip::functional_main_launch_prompt(
                        actor.parse().map_err(|error: String| anyhow!(error))?,
                        &request,
                    )?
                } else {
                    let action = if writable {
                        format!("Retain the nonce in this native conversation. Use the native file tool to write exactly {nonce} followed by a single LF newline, with no other bytes, to .llmrelay-main-{probe}-first. Run this exact harmless boundary command once:\n{command}")
                    } else {
                        format!(
                            "Remember {nonce} only in this native conversation. {readonly_probes}"
                        )
                    };
                    format!("LLMRelay Main verification first. This is only the authorized permission check; do not implement the task or submit a completion report. {action}\nThe boundary command only opens and closes paths without reading, creating, truncating or writing bytes, and connects then closes without an application protocol request. Preserve actual native commands and tool results. Do not print contents, enumerate directories, read credentials, send app-control requests, install runtimes, bypass a refusal or retry. Missing paths/runtime, tool refusal, connection failure, narration and unchanged files do not prove OS denial; report them as unavailable. Finish this turn and wait.")
                };
                payload["prompt_hash"] = json_hash(&prompt)?.into();
                let connection = self.store.lock()?;
                if connection.execute(
                    "UPDATE controls SET payload_json=?1 WHERE id=?2 AND state='native_requested'",
                    params![payload.to_string(), control],
                )? != 1
                {
                    bail!("Main verification preparation lost its current control ownership")
                }
                connection.execute("UPDATE attempts SET status='running' WHERE id=?1 AND status IN ('held','needs_input')",params![attempt])?;
                connection.execute("UPDATE tasks SET attention='none' WHERE id=?1 AND attention IN ('paused','needs_input')",params![task])?;
                drop(connection);
                let launch = if actor == "single_agent" {
                    self.launch_single_agent_turn(&attempt, None, Some(&prompt), None, true)?
                } else {
                    let role = actor.parse().map_err(|error: String| anyhow!(error))?;
                    let launched = if functional {
                        let context = self.store.role_launch_context(&attempt, role)?;
                        payload["launch_reservation"] = serde_json::json!({"session_id":context.session_id,
                        "role_generation_id":context.role_generation_id,"transcript_epoch":context.transcript_epoch,
                        "permit_id":context.permit_id});
                        if self.store.lock()?.execute(
                        "UPDATE controls SET payload_json=?1 WHERE id=?2 AND state='native_requested'",
                        params![payload.to_string(), control],
                    )? != 1 {
                        bail!("Main launch preparation lost its current control ownership")
                    }
                        self.dispatch_context(
                            &attempt, role, &prompt, None, context, None, None, None,
                        )?
                    } else {
                        self.dispatch_role(&attempt, role, &prompt, None)?
                    };
                    serde_json::to_value(launched)?
                };
                self.hold_main_verification(&control, &attempt, &task)?;
                Ok(
                    serde_json::json!({"action":"main_verification_started","control_id":control,"launch":launch}),
                )
            })();
            return match started {
                Ok(result) => Ok(Some(result)),
                Err(error) if functional => self
                    .settle_main_verification_launch_failure(
                        &control, &attempt, &task, &payload, &error,
                    )
                    .map(Some),
                Err(error) => Err(error),
            };
        }
        if turns == 0 {
            self.store.lock()?.execute("UPDATE controls SET state='cancelled',updated_at=?1 WHERE id=?2 AND state='native_requested'",params![Utc::now().to_rfc3339(),control])?;
            if functional {
                let _ = self.cleanup_terminal_main_artifacts(&control, &payload)?;
            }
            return Ok(Some(
                serde_json::json!({"action":"main_verification_not_started","expired":expired,"control_id":control}),
            ));
        }
        let session = payload["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Verification reservation lost its session"))?
            .to_owned();
        self.supervisor.reconcile()?;
        let record = self.store.session_json(&session)?;
        if record["transcript_epoch"] != payload["transcript_epoch"]
            || record["role_generation_id"] != payload["role_generation_id"]
        {
            let nondelivery: bool = self.store.lock()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM resume_invocations WHERE session_id=?1
                AND transcript_epoch=?2 AND state='proven_nondelivery')",
                params![session, payload["transcript_epoch"].as_str()],
                |row| row.get(0),
            )?;
            if nondelivery
                && record["role_generation_id"] == payload["role_generation_id"]
                && record["status"] == "exited"
                && record["exit"]["process_group_quiescent"] == true
            {
                self.store.lock()?.execute(
                    "UPDATE controls SET state='failed',updated_at=?1 WHERE id=?2",
                    params![Utc::now().to_rfc3339(), control],
                )?;
                if functional {
                    let _ = self.cleanup_terminal_main_artifacts(&control, &payload)?;
                }
                return Ok(Some(
                    serde_json::json!({"action":"main_verification_failed","session_id":session,"proven_nondelivery":true,"retry_started":false}),
                ));
            }
            bail!("This Main verification no longer owns the recorded turn. Reconcile its exact reservation before any stop or additional provider call.")
        }
        if record["status"] == "launch_failed" {
            let mut connection = self.store.lock()?;
            let tx =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE controls SET state='failed',updated_at=?1 WHERE id=?2",
                params![Utc::now().to_rfc3339(), control],
            )?;
            tx.execute("UPDATE attempts SET status='needs_input' WHERE id=?1 AND status IN ('running','held')",params![attempt])?;
            tx.execute(
                "UPDATE tasks SET attention='needs_input' WHERE id=?1 AND lifecycle='in_progress' AND attention!='resume_failed'",
                params![task],
            )?;
            tx.commit()?;
            drop(connection);
            if functional {
                let _ = self.cleanup_terminal_main_artifacts(&control, &payload)?;
            }
            return Ok(Some(
                serde_json::json!({"action":"main_verification_failed","session_id":session,"retry_started":false,"reason":record["launch_error"]}),
            ));
        }
        if record["status"] != "exited" || record["exit"]["process_group_quiescent"] != true {
            let idle: bool=self.store.lock()?.query_row("SELECT s.readiness_state='idle_candidate' AND s.native_session_id IS NOT NULL
                AND EXISTS(SELECT 1 FROM hook_events h JOIN audit_events a ON a.entity_id=s.id AND json_extract(a.detail_json,'$.hook_event_id')=h.id
                WHERE h.session_id=s.id AND h.event_name='Stop' AND h.native_session_id=s.native_session_id
                  AND a.event_code='provider.hook.received' AND json_extract(a.detail_json,'$.safe_idle_boundary')=1
                  AND h.rowid>COALESCE((SELECT hook_event_boundary_rowid FROM resume_invocations WHERE session_id=s.id
                    AND transcript_epoch=s.transcript_epoch),0)) FROM sessions s WHERE s.id=?1",params![session], |row|row.get(0))?;
            if (expired || cancelled || idle)
                && payload["stop_requested_turn"].as_u64() != Some(turns)
            {
                payload["stop_requested_turn"] = turns.into();
                let changed = self.store.lock()?.execute(
                    "UPDATE controls SET state='native_stopping',payload_json=?1,updated_at=?2
                    WHERE id=?3 AND payload_json=?4",
                    params![payload.to_string(), Utc::now().to_rfc3339(), control, json],
                )?;
                if changed == 1 {
                    let stop = self.store.session_owner(&session).and_then(|owner| {
                        if owner
                            != SessionOwner::RoleGeneration(
                                payload["role_generation_id"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .into(),
                            )
                        {
                            bail!("Verification owner changed before stop")
                        }
                        self.supervisor.interrupt(&session)
                    });
                    if let Err(error) = stop {
                        payload["stop_error"] = format!("{error:#}").into();
                        self.store.lock()?.execute(
                            "UPDATE controls SET payload_json=?1 WHERE id=?2",
                            params![payload.to_string(), control],
                        )?;
                    }
                }
            }
            return Ok(Some(
                serde_json::json!({"action":"waiting","for":"main_verification_quiescence","session_id":session,"expired":expired}),
            ));
        }
        if expired || cancelled {
            self.store.lock()?.execute(
                "UPDATE controls SET state='cancelled',updated_at=?1 WHERE id=?2",
                params![Utc::now().to_rfc3339(), control],
            )?;
            if functional {
                let _ = self.cleanup_terminal_main_artifacts(&control, &payload)?;
            }
            return Ok(Some(
                serde_json::json!({"action":"main_verification_stopped","expired":expired,"session_id":session,"success_proof":false}),
            ));
        }
        let completed_turn: bool=self.store.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM hook_events h JOIN audit_events a
            ON a.entity_id=s.id AND json_extract(a.detail_json,'$.hook_event_id')=h.id WHERE h.session_id=s.id
            AND h.event_name='Stop' AND h.native_session_id=s.native_session_id AND a.event_code='provider.hook.received'
            AND json_extract(a.detail_json,'$.safe_idle_boundary')=1 AND h.rowid>COALESCE((SELECT hook_event_boundary_rowid
                FROM resume_invocations WHERE session_id=s.id AND transcript_epoch=s.transcript_epoch),0)) FROM sessions s WHERE s.id=?1",
            params![session], |row|row.get(0))?;
        if !completed_turn {
            bail!("Main verification stopped without an authenticated complete turn; no retained call or success proof was started")
        }
        if functional {
            let validation = (|| -> Result<()> {
                let connection = self.store.lock()?;
                let generation = payload["role_generation_id"]
                    .as_str()
                    .context("Main verification lost its generation")?;
                let failed: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM role_results WHERE session_id=?1 AND role_generation_id=?2
                       AND json_extract(metadata_json,'$.runtime_report_format')='functional-v2'
                       AND json_extract(metadata_json,'$.validation_observation.functional.status') IN ('failed','missing_context'))",
                    params![session,generation], |row| row.get(0),
                )?;
                if failed {
                    bail!("Main functional report recorded failed or missing context")
                }
                match (turns, payload["max_turns"].as_u64()) {
                    (1, Some(2)) => return Ok(()),
                    (1, Some(1)) | (2, Some(2)) => {}
                    _ => bail!("Main functional verification has an invalid turn reservation"),
                }
                let mut statement = connection.prepare(
                    "SELECT json_object('operation_id',operation_id,'outcome',outcome,'summary',summary,
                       'evidence',json(evidence_json),'metadata',json(metadata_json)) FROM role_results
                     WHERE session_id=?1 AND role_generation_id=?2 ORDER BY rowid LIMIT 2",
                )?;
                let reports = statement
                    .query_map(params![session, generation], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if reports.len() != 1 {
                    let rejection: Option<String> = connection.query_row(
                        "SELECT json_extract(result_json,'$.reason') FROM operation_receipts
                         WHERE actor_key=?1 AND operation_kind='role_report'
                           AND json_extract(result_json,'$.accepted')=0
                           AND json_extract(result_json,'$.session_id')=?2
                           AND json_extract(result_json,'$.transcript_epoch')=?3 ORDER BY rowid DESC LIMIT 1",
                        params![generation,session,payload["transcript_epoch"].as_str()], |row| row.get(0),
                    ).optional()?;
                    bail!(
                        "{}",
                        rejection.as_deref().unwrap_or("Main functional verification requires exactly one accepted current report")
                    )
                }
                let report: crate::domain::RoleResultReport = serde_json::from_str(&reports[0])?;
                if report.outcome != "capability_observed"
                    || report.metadata["runtime_report_format"] != "functional-v2"
                    || report.metadata["validation_observation"]["functional"]["status"] != "passed"
                {
                    bail!("Main functional verification requires a passed current functional-v2 report")
                }
                crate::store::validate_functional_main_report_in(
                    &connection,
                    &session,
                    generation,
                    &report,
                )?;
                Ok(())
            })();
            if let Err(error) = validation {
                let reason = format!("{error:#}");
                payload["failure_reason"] = reason.clone().into();
                let mut connection = self.store.lock()?;
                let tx = connection
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                tx.execute(
                    "UPDATE controls SET state='failed',payload_json=?1,updated_at=?2 WHERE id=?3",
                    params![payload.to_string(), Utc::now().to_rfc3339(), control],
                )?;
                tx.execute(
                    "UPDATE attempts SET status='needs_input' WHERE id=?1",
                    params![attempt],
                )?;
                tx.execute(
                    "UPDATE tasks SET attention='needs_input' WHERE id=?1 AND attention!='resume_failed'",
                    params![task],
                )?;
                tx.commit()?;
                drop(connection);
                let _ = self.cleanup_terminal_main_artifacts(&control, &payload)?;
                return Ok(Some(
                    serde_json::json!({"action":"main_verification_failed","session_id":session,"retry_started":false,"success_proof":false,
                        "reason":reason,"message":"Prepare fresh Main verification after correcting the evidence; no retry or publication was started."}),
                ));
            }
        }
        let root = self
            .checkout_scope(&attempt)?
            .ok_or_else(|| anyhow!("Verification lost Main checkout authority"))?;
        providers::require_main_protection_partition(&payload["scope"])?;
        if payload["scope"] != root {
            bail!("Checkout protections changed during Main verification; no proof was published")
        }
        let writable = actor == "single_agent" || actor == "implementer";
        let filename = format!(
            ".llmrelay-main-{probe}-{}",
            if turns == 1 { "first" } else { "recall" }
        );
        let path = std::path::Path::new(root["root"].as_str().unwrap()).join(&filename);
        let observed = if functional {
            let request = &payload["observation_request"];
            let read = std::path::Path::new(
                request["permitted_read"]["path"]
                    .as_str()
                    .context("Main functional verification lost its challenge")?,
            );
            let writer = request["owned_writer"]["path"]
                .as_str()
                .map(std::path::Path::new);
            *request
                == providers::main_checkout_functional_observation_request(&root, read, writer)?
                && std::fs::read(read)? == nonce.as_bytes()
                && match writer {
                    Some(path) => {
                        std::fs::symlink_metadata(path)?.is_file()
                            && std::fs::read(path)? == nonce.as_bytes()
                    }
                    None => !writable,
                }
        } else if writable {
            remove_owned_verification_output(&path, &nonce)?
        } else {
            for name in payload["readonly_probe_paths"]
                .as_array()
                .context("Read-only verification lost its probe paths")?
            {
                let artifact = std::path::Path::new(root["root"].as_str().unwrap())
                    .join(name.as_str().context("Invalid readonly probe path")?);
                match std::fs::symlink_metadata(&artifact) {
                    Ok(_) => bail!("Read-only probe left artifact {}; preserve it for review, no file was deleted",artifact.display()),
                    Err(error) if error.kind()==std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(error).with_context(||format!("Cannot inspect read-only probe {}",artifact.display())),
                }
            }
            true
        };
        if !observed {
            bail!("Main verification did not produce the exact owned nonce result; no success proof was published")
        }
        let first = turns == 1;
        payload[if first {
            "first_turn_complete"
        } else {
            "retained_turn_complete"
        }] = true.into();
        payload[if first {
            "first_file_observed"
        } else {
            "retained_recall_observed"
        }] = writable.into();
        if first && payload["max_turns"].as_u64() == Some(2) {
            let recall_path = std::path::Path::new(root["root"].as_str().unwrap())
                .join(format!(".llmrelay-main-{probe}-recall"));
            if !functional && writable && std::fs::symlink_metadata(recall_path).is_ok() {
                bail!("The retained verification output already exists; no replacement or overwrite was authorized")
            }
            let recall = if writable {
                format!("Recall the earlier nonce from conversation memory into .llmrelay-main-{probe}-recall using the native file tool; write the remembered nonce followed by a single LF newline, with no other bytes.")
            } else {
                "Remain read-only. State the remembered nonce in your final answer for independent native UI review. Do not run tools or attempt any file write.".into()
            };
            let prompt = if functional {
                crate::trip::functional_main_resume_prompt()
            } else {
                format!("LLMRelay Main verification recall. Continue only this diagnostic in the same native conversation. {recall} Do not read prior files, transcripts or fixture records. Finish and wait; do not implement, submit a result, bypass a denial or retry.")
            };
            payload["prompt_hash"] = json_hash(&prompt)?.into();
            payload["phase"] = "recall".into();
            self.store.lock()?.execute(
                "UPDATE controls SET state='native_running',payload_json=?1 WHERE id=?2",
                params![payload.to_string(), control],
            )?;
            if actor == "single_agent" {
                let connection = self.store.lock()?;
                connection.execute(
                    "UPDATE attempts SET status='needs_input' WHERE id=?1",
                    params![attempt],
                )?;
                connection.execute(
                    "UPDATE tasks SET attention='needs_input' WHERE id=?1 AND attention!='resume_failed'",
                    params![task],
                )?;
                drop(connection);
                self.launch_single_agent_turn(
                    &attempt,
                    Some(crate::workflow::SingleAgentResumeMode::Explicit),
                    Some(&prompt),
                    None,
                    true,
                )?;
            } else {
                let connection = self.store.lock()?;
                connection.execute(
                    "UPDATE attempts SET status='running' WHERE id=?1 AND status='held'",
                    params![attempt],
                )?;
                connection.execute(
                    "UPDATE tasks SET attention='none' WHERE id=?1 AND attention='paused'",
                    params![task],
                )?;
                drop(connection);
                self.resume_role_session_inner(&session, &prompt, None, false, None)?;
            }
            self.hold_main_verification(&control, &attempt, &task)?;
            return Ok(Some(
                serde_json::json!({"action":"main_verification_resume_started","session_id":session}),
            ));
        }
        if payload["max_turns"] == 1 {
            payload["retained_recall_observed"] = false.into();
        }
        payload["completed_at"] = Utc::now().to_rfc3339().into();
        let mut connection = self.store.lock()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute("UPDATE controls SET state='awaiting_native_evidence',payload_json=?1,updated_at=?2 WHERE id=?3",params![payload.to_string(),Utc::now().to_rfc3339(),control])?;
        transaction.execute(
            "UPDATE attempts SET status='needs_input' WHERE id=?1 AND status IN ('running','held')",
            params![attempt],
        )?;
        transaction.execute(
            "UPDATE tasks SET attention='needs_input' WHERE id=?1 AND attention!='resume_failed'",
            params![task],
        )?;
        transaction.commit()?;
        Ok(Some(
            serde_json::json!({"action":"main_verification_requires_native_evidence","session_id":session,
            "message":if functional {"Publish the exact typed functional Main evidence with capability record-proof; functional evidence does not attest OS confinement."} else {"Inspect exact native protected-root denial evidence and record it with capability record-proof. File/recall observations alone do not qualify Main permissions."}}),
        ))
    }

    fn hold_main_verification(&self, control: &str, attempt: &str, task: &str) -> Result<()> {
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM controls WHERE id=?1 AND attempt_id=?2 AND state='native_running')",rusqlite::params![control,attempt], |row|row.get(0))?;
        if !current {
            bail!("Main verification ownership changed after dispatch")
        }
        tx.execute(
            "UPDATE attempts SET status='held' WHERE id=?1 AND status='running'",
            rusqlite::params![attempt],
        )?;
        tx.execute(
            "UPDATE tasks SET attention='paused' WHERE id=?1 AND attention='none'",
            rusqlite::params![task],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn launch_single_agent_turn(
        &self,
        attempt_id: &str,
        resume: Option<crate::workflow::SingleAgentResumeMode>,
        message: Option<&str>,
        human: Option<&crate::store::SingleAgentResumeBinding<'_>>,
        verification: bool,
    ) -> Result<serde_json::Value> {
        self.store
            .require_execution_unheld("Single agent launches")?;
        if !self.dispatch_enabled() {
            bail!("service is draining; new agent launches are disabled")
        }
        {
            let connection = self.store.lock()?;
            crate::store::require_failure_stop_released(&connection, attempt_id)?;
            if let Some(mode) = resume {
                // Preparation requires a live workspace; reject stopped runs before
                // touching it, while retaining reservation and final-spawn checks.
                crate::workflow::single_agent_resume_target_for_verification(
                    &connection,
                    attempt_id,
                    Some(mode),
                    verification,
                )?
                .map_err(|reason| anyhow!(reason))?;
            }
        }
        {
            let connection = self.store.lock()?;
            crate::workflow::require_supported_attempt_execution(&connection, attempt_id)?;
        }
        let context = if verification {
            self.store
                .single_agent_verification_context(attempt_id, resume.is_some())?
        } else {
            match resume {
                Some(mode) => self.store.single_agent_resume_context(attempt_id, mode)?,
                None => self.store.single_agent_launch_context(attempt_id)?,
            }
        };
        let verified_main_resume = if let Some(target) = context.resume.as_ref() {
            !verification && self.store.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM controls WHERE kind='main_checkout_verify'
                AND state='finished' AND json_extract(payload_json,'$.session_id')=?1 AND COALESCE(json_extract(payload_json,'$.coding_started'),0)=0)",rusqlite::params![target.session_id], |row|row.get::<_,bool>(0))?
        } else {
            false
        };
        let prompt = if verification {
            message
                .ok_or_else(|| anyhow!("Main verification omitted its exact prompt"))?
                .to_owned()
        } else {
            match (
            &context.resume,
            message.map(str::trim).filter(|text| !text.is_empty()),
        ) {
            (Some(_), guidance) if verified_main_resume => format!("{}\nRun binding: {}\nProfile revision: {}\nThe project permission diagnostic is complete. Begin the actual task from authenticated role context. Profile instructions: {}\n{}",
                crate::workflow_resources::SINGLE_AGENT_PROMPT.trim(),context.attempt_id,context.profile_revision_id,context.instructions_text,
                guidance.map(|text|format!("A person added: {text}")).unwrap_or_default()),
            (Some(_), Some(text)) => format!("A person answered while you were stopped:\n{text}"),
            (Some(_), None) => HOST_RESTART_RESUME_NOTICE.to_owned(),
            (None, _) => {
                let mut prompt = format!(
                    "{}\n\nRun binding: {}\nProfile revision: {}\nRead your full task context with the role context command before starting.",
                    crate::workflow_resources::single_agent_definition(context.preset)?.prompt.trim(),
                    context.attempt_id,
                    context.profile_revision_id
                );
                if !context.instructions_text.trim().is_empty() {
                    prompt.push_str("\n\nProfile instructions:\n");
                    prompt.push_str(context.instructions_text.trim());
                }
                prompt
            }
        }
        };
        let checkout_scope = self.checkout_scope(attempt_id)?;
        let executable = self.executable.canonicalize()?;
        let mut prepared =
            providers::prepare_single_agent_launch(&providers::SingleAgentLaunchRequest {
                purpose: context.purpose,
                provider: context.provider,
                model: &context.model,
                effort: &context.effort,
                cwd: &context.workspace,
                read_denials: &context.read_denials,
                service_root: &self.paths.root,
                prompt: &prompt,
                role_socket: &self.paths.role_socket,
                token: &context.token,
                role_generation_id: &context.role_generation_id,
                session_id: &context.session_id,
                native_session_id: context
                    .resume
                    .as_ref()
                    .map(|target| target.native_session_id.as_str()),
                assets: &self.hooks,
                executable_path: &executable,
                bundles: &self.store.compatibility_bundles,
                checkout_scope: checkout_scope.as_ref(),
            })?;
        prepared.record["prompt_hash"] = serde_json::json!(json_hash(&prompt)?);
        let origin = std::time::Instant::now();
        let mut clocks = self
            .schedule_clocks
            .lock()
            .map_err(|_| anyhow!("schedule clock state poisoned"))?;
        let occurrence: Option<(String, i64, i64)> = self
            .store
            .lock()?
            .query_row(
                "SELECT f.id,f.invocations_spent,a.max_runtime_seconds FROM recipe_schedule_fires f
             JOIN recipe_schedule_authorizations a ON a.id=f.authorization_id WHERE f.task_id=?1",
                [&context.task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((id, spent, _)) = &occurrence {
            let connection = self.store.lock()?;
            if !crate::recipes::schedule_attempt_member_in(&connection, id, attempt_id)? {
                bail!("The attempt is not a trusted member of this scheduled occurrence")
            }
            drop(connection);
            if *spent > 0
                && clocks
                    .get(id)
                    .is_none_or(|clock| clock.origin.elapsed() >= clock.allowance)
            {
                bail!("The original scheduled runtime is unavailable or exhausted; reconcile without resuming")
            }
        }
        match resume {
            Some(mode) => self
                .store
                .reserve_single_agent_resume(&context, mode, &prepared, human)?,
            None => self
                .store
                .reserve_single_agent_invocation(&context, &prepared)?,
        }
        if let Some((id, spent, seconds)) = &occurrence {
            if *spent == 0 {
                clocks.entry(id.clone()).or_insert(ScheduleClock {
                    origin,
                    allowance: std::time::Duration::from_secs(*seconds as u64),
                });
            }
        }
        let monotonic_deadline = occurrence
            .as_ref()
            .and_then(|(id, _, _)| clocks.get(id))
            .and_then(|clock| clock.origin.checked_add(clock.allowance));
        drop(clocks);
        let checkout_check = (|| -> Result<()> {
            if occurrence.is_some() && !self.dispatch_enabled() {
                bail!("The service is draining; this scheduled reservation will remain spent without spawning")
            }
            if self.checkout_scope(attempt_id)? != checkout_scope {
                bail!("The checkout protections changed after reservation; no process was started")
            }
            Ok(())
        })();
        if let Err(error) = checkout_check {
            if resume.is_some() {
                self.store.restore_resume_after_proven_nondelivery(
                    &context.session_id,
                    &error.to_string(),
                )?;
            } else {
                self.store
                    .update_session_launch_failed(&context.session_id, &error.to_string())?;
            }
            return Err(error);
        }
        let process = if self.synthetic_dispatch_for_tests {
            if let Err(error) = self.store.mark_session_spawning_with_schedule_deadline(
                &context.session_id,
                &context.transcript_epoch,
                "synthetic-dispatch-no-process",
                monotonic_deadline,
                Some(&self.dispatch_enabled),
            ) {
                if context.review_delivery_id.is_some()
                    && !error.is::<crate::review::SingleAgentReviewDeferred>()
                {
                    self.store
                        .update_session_launch_failed(&context.session_id, &error.to_string())?;
                }
                return Err(error);
            }
            ProcessIdentity {
                pid: std::process::id(),
                process_group_id: 0,
                native_start_marker: "synthetic-dispatch-no-process".into(),
                observed_started_at: Utc::now().to_rfc3339(),
            }
        } else {
            match self.supervisor.spawn_process_with_schedule_deadline(
                &context.session_id,
                &SessionOwner::RoleGeneration(context.role_generation_id.clone()),
                &context.transcript_epoch,
                &prepared.process(),
                monotonic_deadline,
                Some(&self.dispatch_enabled),
            ) {
                Ok(process) => process,
                Err(error) => {
                    self.record_spawn_failure(&context.session_id, &error, resume.is_some())?;
                    return Err(error.into());
                }
            }
        };
        if let Err(error) = self.store.update_session_running(
            &context.session_id,
            &context.transcript_epoch,
            &serde_json::to_string(&process)?,
        ) {
            let stop = match self.supervisor.interrupt(&context.session_id) {
                Ok(()) => String::new(),
                Err(stop) => format!("; the stop signal was not confirmed ({stop:#})"),
            };
            self.store.mark_session_delivery_ambiguous(
                &context.session_id,
                &format!("{error:#}{stop}"),
            )?;
            return Err(
                error.context("the agent started but its durable running state is ambiguous")
            );
        }
        self.role_tokens
            .write()
            .map_err(|_| anyhow!("role token map poisoned"))?
            .insert(context.session_id.clone(), context.token);
        Ok(serde_json::json!({
            "action": if resume.is_some() { "single_agent_resumed" } else { "single_agent_dispatched" },
            "attempt_id": attempt_id,
            "task_id": context.task_id,
            "session_id": context.session_id,
            "role_generation_id": context.role_generation_id,
            "transcript_epoch": context.transcript_epoch,
        }))
    }
}

const PROVIDER_CHECK_PREFIX: &str = "LLMRELAY-CHECK-";

fn remove_owned_verification_output(path: &std::path::Path, nonce: &str) -> Result<bool> {
    remove_owned_verification_bytes(path, format!("{nonce}\n").as_bytes())
}

fn cleanup_functional_main_artifacts(payload: &serde_json::Value) -> Result<()> {
    if payload["functional_challenge_created"] != true {
        return Ok(());
    }
    let root = std::path::Path::new(
        payload["scope"]["root"]
            .as_str()
            .context("Owned Main cleanup lost its root")?,
    );
    let probe = payload["probe_id"]
        .as_str()
        .context("Owned Main cleanup lost its identity")?;
    let nonce = payload["nonce"]
        .as_str()
        .context("Owned Main cleanup lost its nonce")?;
    if [probe, nonce]
        .iter()
        .any(|value| value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        bail!("Owned Main cleanup has an invalid identity")
    }
    let request = &payload["observation_request"];
    providers::validate_main_functional_request(&payload["scope"], request)?;
    let challenge = root.join(format!(".llmrelay-main-{probe}-challenge"));
    let writer = root.join(format!(".llmrelay-main-{probe}-first"));
    if request["permitted_read"]["path"].as_str() != challenge.to_str()
        || (request["owned_writer"] != serde_json::Value::Null
            && request["owned_writer"]["path"].as_str() != writer.to_str())
    {
        bail!("Owned Main cleanup paths differ from the reserved artifacts")
    }
    for path in [&challenge, &writer] {
        if path == &writer && request["owned_writer"] == serde_json::Value::Null {
            continue;
        }
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect owned Main artifact {}", path.display()))
            }
            Ok(_) => {}
        }
        if !remove_owned_verification_bytes(path, nonce.as_bytes())
            .with_context(|| format!("remove owned Main artifact {}", path.display()))?
        {
            bail!(
                "Main artifact {} no longer has the exact owned bytes; preserve it for review",
                path.display()
            )
        }
    }
    Ok(())
}

fn remove_owned_verification_bytes(path: &std::path::Path, expected: &[u8]) -> Result<bool> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() != expected.len() as u64
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    file.by_ref().take(129).read_to_end(&mut bytes)?;
    let current = std::fs::symlink_metadata(path)?;
    if bytes != expected
        || current.ino() != metadata.ino()
        || current.dev() != metadata.dev()
        || current.nlink() != 1
    {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}
const MAX_PROVIDER_CHECK_PAGES: usize = 32;

fn provider_name(provider: crate::domain::Provider) -> &'static str {
    match provider {
        crate::domain::Provider::Codex => "Codex",
        crate::domain::Provider::Claude => "Claude Code",
    }
}

/// An unsupported Codex version is reported as such before any account read;
/// no other failure is presented as something signing in would fix.
fn codex_observation_account(
    scope: &Path,
    installed_version: &str,
) -> std::result::Result<providers::ProviderAccountObservation, String> {
    let supported = providers::codex::EXACT_CODEX_VERSION;
    if installed_version != supported {
        return Err(format!(
            "This LLMRelay release checks only {supported}, but {installed_version} is installed. Install {supported}, or use an LLMRelay release that supports {installed_version}, then preview again."
        ));
    }
    providers::codex::inspect_observation_account(scope, installed_version)
        .map_err(|error| codex_setup_unconfirmed(&error))
}

fn codex_setup_unconfirmed(error: &anyhow::Error) -> String {
    format!("Codex's local setup could not be confirmed for checks: {error:#}. Fix what this reports, then preview again.")
}

impl Application {
    fn provider_validation_root(&self) -> Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let root = self.paths.artifacts.join("provider-validation");
        std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        Ok(root)
    }

    fn inspect_provider(
        &self,
        provider: crate::domain::Provider,
        scope: &Path,
    ) -> crate::store::ProviderLocalInspection {
        let name = provider_name(provider);
        let installation = providers::inspect_installation(provider).map_err(|error| {
            format!("{name} was not found or its version could not be read ({error:#}). Install it or fix PATH, then preview again.")
        });
        let account = match (&installation, provider) {
            (Err(reason), _) => Err(reason.clone()),
            (Ok(installation), crate::domain::Provider::Codex) => {
                codex_observation_account(scope, &installation.executable_version)
            }
            (Ok(_), crate::domain::Provider::Claude) => {
                Ok(providers::claude::inspect_observation_account())
            }
        };
        #[cfg(test)]
        let policy = installation
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|installation| {
                self.store
                    .compatibility_bundles
                    .resolve_policy(
                        provider,
                        &installation.executable_version,
                        crate::provider_compatibility::PolicyMode::ProviderObservation,
                    )
                    .map_err(|error| error.explanation().message.clone())
            });
        let catalog = crate::model_catalog::read(&provider.to_string());
        crate::store::ProviderLocalInspection {
            provider,
            installation,
            account,
            #[cfg(test)]
            policy,
            catalog: (catalog.state != "unavailable").then(|| {
                catalog
                    .models
                    .into_iter()
                    .map(|model| (model.slug, model.efforts))
                    .collect()
            }),
        }
    }

    fn inspect_selected_providers(
        &self,
        selections: &[crate::domain::ProviderValidationSelection],
    ) -> Result<Vec<crate::store::ProviderLocalInspection>> {
        let scope = self.provider_validation_root()?;
        let mut inspections: Vec<crate::store::ProviderLocalInspection> = Vec::new();
        for selection in selections {
            if !inspections
                .iter()
                .any(|inspection| inspection.provider == selection.provider)
            {
                inspections.push(self.inspect_provider(selection.provider, &scope));
            }
        }
        Ok(inspections)
    }

    /// Inspects locally and plans each check; it never contacts a model.
    pub fn preview_provider_validation(
        &self,
        request: &crate::domain::ProviderValidationPreviewRequest,
    ) -> Result<crate::domain::ProviderValidationPreviewDto> {
        let inspections = self.inspect_selected_providers(&request.selections)?;
        self.store
            .preview_provider_validation(&request.selections, &inspections)
    }

    pub fn start_provider_validation(
        &self,
        request: &crate::domain::ProviderValidationStartRequest,
    ) -> Result<crate::domain::ProviderValidationBatchDto> {
        self.store.start_provider_validation(request)
    }

    /// Returns the exact request's batch, or closes a request that never
    /// started so it never can. It inspects nothing and contacts no model.
    pub fn reconcile_provider_validation_start(
        &self,
        request: &crate::domain::ProviderValidationStartRequest,
    ) -> Result<crate::domain::ProviderValidationStartOutcome> {
        self.store.reconcile_provider_validation_start(request)
    }

    pub fn inspect_provider_validation(
        &self,
        batch_id: &str,
    ) -> Result<crate::domain::ProviderValidationBatchDto> {
        self.store.provider_validation_batch(batch_id)
    }

    pub fn cancel_provider_validation(
        &self,
        batch_id: &str,
    ) -> Result<crate::domain::ProviderValidationBatchDto> {
        let (_, running) = self.store.cancel_provider_validation(batch_id)?;
        for session_id in running {
            if let Err(error) = self.supervisor.interrupt(&session_id) {
                self.store.record_provider_check_stop_failure(
                    &session_id,
                    &format!(
                        "Cancel was requested, but LLMRelay could not confirm the stop reached this check ({error:#}). It stays cancel-requested until its processes are confirmed stopped; nothing is retried automatically."
                    ),
                )?;
            }
        }
        self.store.provider_validation_batch(batch_id)
    }

    pub(crate) fn dispatch_one_provider_check(&self) -> Result<Option<serde_json::Value>> {
        for check in self.store.pending_provider_checks()? {
            if self.store.skip_provider_check(
                &check.invocation_id,
                crate::store::PROVIDER_CHECK_PROJECT_REQUIRED,
            )? {
                return Ok(Some(serde_json::json!({
                    "action": "provider_check_skipped",
                    "provider_validation_invocation_id": check.invocation_id,
                    "reason": crate::store::PROVIDER_CHECK_PROJECT_REQUIRED,
                })));
            }
        }
        Ok(None)
    }

    /// An expired check requests its stop once. Uncertain ownership settles
    /// only after fresh proof that its processes are gone.
    pub(crate) fn settle_one_provider_check(&self) -> Result<Option<serde_json::Value>> {
        use crate::store::{ProviderCheckOutcome, PROVIDER_CHECK_TIMEOUT};
        if let Some((session_id, owner)) = self.store.request_expired_provider_check_stop()? {
            let stop = self.store.session_owner(&session_id).and_then(|current| {
                if current != owner {
                    bail!("provider-check stop owner changed")
                }
                self.supervisor.interrupt(&session_id)
            });
            if let Err(error) = stop {
                self.store.record_provider_check_stop_failure(
                    &session_id,
                    &format!("{PROVIDER_CHECK_TIMEOUT} LLMRelay could not confirm the stop reached this check ({error:#}). Its result stays unknown until its processes are confirmed stopped."),
                )?;
            }
            return Ok(Some(serde_json::json!({
                "action": "provider_check_timeout_requested",
                "provider_validation_invocation_id": owner.id(),
                "session_id": session_id,
            })));
        }
        for check in self.store.unsettled_provider_checks()? {
            let mut exit_success = check.exit_success;
            if !check.quiescent {
                let Ok(verification) =
                    crate::recovery::verify_session_quiescent(&self.store, &check.session_id)
                else {
                    continue;
                };
                if !self.store.record_provider_check_quiescent(
                    &check.session_id,
                    &check.invocation_id,
                    &verification,
                )? {
                    continue;
                }
                exit_success &= check.session_exited;
            }
            let outcome = if check.deadline_expired {
                ProviderCheckOutcome::Cancelled(PROVIDER_CHECK_TIMEOUT)
            } else if check.cancel_requested {
                ProviderCheckOutcome::Cancelled("Cancelled. No result was recorded for this check.")
            } else if check.challenge == crate::store::ProviderCheckChallenge::Invalid {
                ProviderCheckOutcome::Failed(crate::store::PROVIDER_CHECK_PROJECT_REQUIRED)
            } else if !exit_success && !check.session_exited {
                ProviderCheckOutcome::Failed("LLMRelay could not confirm this historical check finished normally. It was not retried. Use Make Ready in a project for a new coding session.")
            } else if !exit_success {
                ProviderCheckOutcome::Failed("The provider exited with an error before answering. Check its sign-in and model name, then use Make Ready in a project for a new coding session.")
            } else {
                match self.provider_check_answered(&check) {
                    Ok(true) => ProviderCheckOutcome::Succeeded,
                    Ok(false) => ProviderCheckOutcome::Failed("The provider finished but did not return the expected answer. This model may not be available to this account."),
                    Err(_) => ProviderCheckOutcome::Failed("LLMRelay could not read this historical check's output. Use Make Ready in a project for a new coding session."),
                }
            };
            if self
                .store
                .settle_provider_check(&check.invocation_id, &check.session_id, outcome)?
            {
                return Ok(Some(
                    serde_json::json!({"action":"provider_check_settled","provider_validation_invocation_id":check.invocation_id,"session_id":check.session_id}),
                ));
            }
        }
        if self.store.finish_settled_provider_batches()? > 0 {
            return Ok(Some(
                serde_json::json!({"action":"provider_validation_finished"}),
            ));
        }
        Ok(None)
    }

    fn provider_check_answered(
        &self,
        check: &crate::store::UnsettledProviderCheck,
    ) -> Result<bool> {
        let Some(expected) = check.nonce_hash.as_deref() else {
            return Ok(false);
        };
        if check.challenge != crate::store::ProviderCheckChallenge::Answer {
            return Ok(false);
        }
        let mut frames = Vec::new();
        let mut after = 0;
        for _ in 0..MAX_PROVIDER_CHECK_PAGES {
            let page = crate::transcript::read_history_page(
                &self.paths.transcripts,
                &check.session_id,
                &check.transcript_epoch,
                after,
                crate::transcript::MAX_HISTORY_PAGE_BYTES,
            )?;
            frames.extend(page.frames);
            if !page.has_more {
                break;
            }
            after = page.next_sequence;
        }
        let text = crate::transcript::recorded_history_text(&frames);
        let length = PROVIDER_CHECK_PREFIX.len() + 32;
        Ok(text.match_indices(PROVIDER_CHECK_PREFIX).any(|(index, _)| {
            text.get(index..index + length).is_some_and(|candidate| {
                hex::encode(sha2::Sha256::digest(candidate.as_bytes())) == expected
            })
        }))
    }
}

#[cfg(test)]
mod provider_validation_tests;

#[cfg(test)]
mod compatibility_error_tests;

#[cfg(test)]
mod drain_interruption_tests;

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
mod scheduled_intake_gate_tests;
