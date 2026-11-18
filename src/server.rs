use crate::auth;
use crate::config::{atomic_write, InstancePaths};
use crate::control;
use crate::database::{
    BackupOptions, BackupPublication, InstanceLock, OnlineBackupControl, PublishedBackup,
};
use crate::domain::{
    AppStateDto, CapabilityProofInput, CmuxKeyboardControlAction, FunctionalCapabilityProofInput,
    HumanCommand, KnowledgeObservation, ProviderValidationSelection, RoleKind, WorkflowCommand,
};
use crate::operations::Application;
use crate::protocol::{self, ClientKind};
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering as AtomicOrdering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));
}

const MAX_STATE_WAITERS: usize = 64;
const MAX_INCARNATION_BYTES: usize = 128;
const DEFAULT_STATE_WAIT_MILLIS: u64 = 25_000;
const STATE_WAIT_MILLIS: RangeInclusive<u64> = 1_000..=25_000;
const MAX_SESSION_COOKIE_VALUE_BYTES: usize = 256;
const MAX_RUNTIME_BINDINGS_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
struct WebState {
    app: Application,
    instance_id: String,
    boot_id: String,
    allowed_hosts: Vec<String>,
    session_cookie: String,
    browser_sessions: Arc<Mutex<HashSet<String>>>,
    shutdown: watch::Receiver<bool>,
    state_waiters: Arc<Semaphore>,
}

/// Browsers scope cookies by host but not port, so each data directory names
/// its own session cookie; the canonical root is stable and not secret.
fn browser_session_cookie_name(data_dir: &std::path::Path) -> String {
    let digest = hex::encode(Sha256::digest(data_dir.as_os_str().as_encoded_bytes()));
    format!("agenticjira_session_{}", &digest[..20])
}

/// Snapshot served by both `/api/state` and state waits. `incarnation` is the
/// serving process identity: a restart or offline restore can lower the
/// revision, so cursors are only comparable within one incarnation.
#[derive(Serialize)]
struct StateSnapshot {
    incarnation: String,
    #[serde(flatten)]
    state: AppStateDto,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum StateWaitResponse {
    StateChanged {
        incarnation: String,
        revision: String,
        state: StateSnapshot,
    },
    Reset {
        incarnation: String,
        revision: String,
        state: StateSnapshot,
    },
    Unchanged {
        incarnation: String,
        revision: String,
    },
}

impl StateWaitResponse {
    fn state_changed(state: StateSnapshot) -> Self {
        Self::StateChanged {
            incarnation: state.incarnation.clone(),
            revision: state.state.revision.clone(),
            state,
        }
    }

    fn reset(state: StateSnapshot) -> Self {
        Self::Reset {
            incarnation: state.incarnation.clone(),
            revision: state.state.revision.clone(),
            state,
        }
    }
}

enum StateWait {
    Respond(StateWaitResponse),
    ShuttingDown,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateWaitQuery {
    incarnation: String,
    revision: String,
    timeout_ms: Option<String>,
}

struct StateWaitRequest {
    incarnation: String,
    revision: i64,
    timeout: Duration,
}

impl TryFrom<StateWaitQuery> for StateWaitRequest {
    type Error = &'static str;

    fn try_from(query: StateWaitQuery) -> std::result::Result<Self, Self::Error> {
        if query.incarnation.is_empty() || query.incarnation.len() > MAX_INCARNATION_BYTES {
            return Err("incarnation must be 1 to 128 bytes");
        }
        let revision = canonical_decimal(&query.revision)
            .and_then(|revision| i64::try_from(revision).ok())
            .ok_or(
                "revision must be a canonical unsigned decimal within the signed 64-bit range",
            )?;
        let timeout_millis = match query.timeout_ms.as_deref() {
            None => DEFAULT_STATE_WAIT_MILLIS,
            Some(timeout) => canonical_decimal(timeout)
                .filter(|millis| STATE_WAIT_MILLIS.contains(millis))
                .ok_or("timeout_ms must be a canonical decimal from 1000 to 25000")?,
        };
        Ok(Self {
            incarnation: query.incarnation,
            revision,
            timeout: Duration::from_millis(timeout_millis),
        })
    }
}

/// Accepts `0` or digits without a leading zero, so every value has one
/// spelling and clients may order revisions by length, then lexicographically.
fn canonical_decimal(value: &str) -> Option<u64> {
    let canonical = !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'));
    canonical.then(|| value.parse().ok()).flatten()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub instance_id: String,
    pub boot_id: String,
    pub pid: u32,
    pub address: String,
    pub data_dir: PathBuf,
    pub control_socket: PathBuf,
    pub role_socket: PathBuf,
    pub started_at: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct BootstrapRequest {
    token: String,
}

#[derive(Debug, Deserialize)]
struct ModelCatalogQuery {
    provider: String,
}

fn deserialize_legacy_capability_proof<'de, D>(
    deserializer: D,
) -> Result<CapabilityProofInput, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if crate::domain::QualificationContract::from_record(&value)
        .map_err(serde::de::Error::custom)?
        != crate::domain::QualificationContract::Legacy
    {
        return Err(serde::de::Error::custom(
            "functional proof requires its explicit web operation",
        ));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
enum WebOperation {
    RecipeScheduleExecutionPreview {
        request: crate::domain::RecipeSchedulePreviewRequest,
    },
    Conversation {
        operation: crate::providers::codex_app_server::ConversationOperation,
    },
    SchedulerRunOnce,
    SnapshotFreeze {
        attempt_id: String,
        snapshot_kind: String,
    },
    SnapshotVerify {
        snapshot_id: String,
    },
    RoleDispatch {
        attempt_id: String,
        role: RoleKind,
        #[serde(default)]
        lane: Option<String>,
        prompt: String,
    },
    TripSetupDispatch {
        operation_id: String,
        attempt_id: String,
        role: RoleKind,
        #[serde(default)]
        fresh_resume_rejection: Option<serde_json::Value>,
    },
    RuntimeProbeLaunch {
        operation_id: String,
        admission_id: String,
        role: RoleKind,
    },
    RuntimeProbeResume {
        operation_id: String,
        admission_id: String,
        role: RoleKind,
    },
    GuidanceDeliver {
        guidance_id: String,
    },
    SwitchRoleRequest {
        operation_id: String,
        attempt_id: String,
        role: String,
        old_generation_id: String,
        settings_revision: i64,
        snapshot_id: String,
        handoff: serde_json::Value,
        expected_task_version: i64,
    },
    SwitchRoleFinish {
        intent_id: String,
    },
    RoleResume {
        operation_id: String,
        session_id: String,
        prompt: String,
    },
    RestartResume {
        operation_id: String,
        #[serde(default)]
        session_ids: Option<Vec<String>>,
    },
    CheckRun {
        operation_id: String,
        attempt_id: String,
        #[serde(default)]
        suite_name: Option<String>,
        #[serde(default)]
        check_id: Option<String>,
    },
    LegacyPreview {
        source: PathBuf,
    },
    LegacyImport {
        operation_id: String,
        project_id: String,
        expected_project_version: i64,
        source: PathBuf,
        expected_source_hash: String,
    },
    Transcript {
        session_id: String,
        after_epoch: Option<String>,
        after_sequence: u64,
        limit_bytes: usize,
    },
    CmuxView {
        operation_id: String,
        session_id: String,
    },
    CmuxSetKeyboardControl {
        operation_id: String,
        session_id: String,
        surface_route_id: String,
        expected_binding_revision: i64,
        expected_control_revision: i64,
        action: CmuxKeyboardControlAction,
    },
    CmuxDiscardUnknown {
        operation_id: String,
        surface_route_id: String,
        session_id: String,
    },
    Interrupt {
        session_id: String,
    },
    CapabilityRecordProof {
        #[serde(deserialize_with = "deserialize_legacy_capability_proof")]
        proof: CapabilityProofInput,
    },
    CapabilityRecordFunctionalProof {
        proof: FunctionalCapabilityProofInput,
    },
    HistoryPage {
        session_id: String,
        transcript_epoch: String,
        after_sequence: u64,
        limit_bytes: usize,
    },
    HistoryExportText {
        session_id: String,
        transcript_epoch: String,
    },
    HistoryPaneOpen {
        operation_id: String,
        session_id: String,
        transcript_epoch: String,
        #[serde(default)]
        acknowledged_unknown_view_id: Option<String>,
    },
    HistoryPaneViews {
        session_id: String,
        transcript_epoch: String,
    },
    ProviderValidationPreview {
        selections: Vec<ProviderValidationSelection>,
    },
    ProviderValidationStart {
        operation_id: String,
        preview_hash: String,
        selections: Vec<ProviderValidationSelection>,
        authorize_model_calls: bool,
        max_model_calls: u32,
    },
    /// The exact start request whose outcome is unknown; never starts checks.
    ProviderValidationReconcile {
        operation_id: String,
        preview_hash: String,
        selections: Vec<ProviderValidationSelection>,
        authorize_model_calls: bool,
        max_model_calls: u32,
    },
    ProviderValidationInspect {
        batch_id: String,
    },
    ProviderValidationCancel {
        batch_id: String,
    },
    Workflow {
        command: WorkflowCommand,
    },
}

impl WebOperation {
    fn checked_operation_id(&self) -> Option<&str> {
        match self {
            Self::TripSetupDispatch { operation_id, .. }
            | Self::RuntimeProbeLaunch { operation_id, .. }
            | Self::RuntimeProbeResume { operation_id, .. }
            | Self::RoleResume { operation_id, .. }
            | Self::LegacyImport { operation_id, .. } => Some(operation_id),
            _ => None,
        }
    }

    fn required_feature(&self) -> Option<&'static str> {
        match self {
            Self::RecipeScheduleExecutionPreview { .. } => {
                Some(protocol::SCHEDULE_EXECUTION_AUTHORIZATION_FEATURE)
            }
            Self::Conversation { .. } => Some(protocol::CONVERSATION_FEATURE),
            Self::Workflow { command } => Some(if command.uses_single_agent_presets() {
                protocol::SINGLE_AGENT_PRESETS_FEATURE
            } else {
                protocol::MULTI_WORKFLOW_FEATURE
            }),
            Self::HistoryPage { .. }
            | Self::HistoryExportText { .. }
            | Self::HistoryPaneOpen { .. }
            | Self::HistoryPaneViews { .. } => Some(protocol::HISTORY_VIEW_FEATURE),
            Self::ProviderValidationPreview { .. }
            | Self::ProviderValidationStart { .. }
            | Self::ProviderValidationReconcile { .. }
            | Self::ProviderValidationInspect { .. }
            | Self::ProviderValidationCancel { .. } => Some(protocol::PROVIDER_VALIDATION_FEATURE),
            Self::SchedulerRunOnce
            | Self::SnapshotFreeze { .. }
            | Self::SnapshotVerify { .. }
            | Self::RoleDispatch { .. }
            | Self::TripSetupDispatch { .. }
            | Self::RuntimeProbeLaunch { .. }
            | Self::RuntimeProbeResume { .. }
            | Self::GuidanceDeliver { .. }
            | Self::SwitchRoleRequest { .. }
            | Self::SwitchRoleFinish { .. }
            | Self::RoleResume { .. }
            | Self::RestartResume { .. }
            | Self::CheckRun { .. }
            | Self::LegacyPreview { .. }
            | Self::LegacyImport { .. }
            | Self::Transcript { .. }
            | Self::CmuxView { .. }
            | Self::CmuxSetKeyboardControl { .. }
            | Self::CmuxDiscardUnknown { .. }
            | Self::Interrupt { .. }
            | Self::CapabilityRecordProof { .. }
            | Self::CapabilityRecordFunctionalProof { .. } => None,
        }
    }
}

fn declared_features(headers: &HeaderMap) -> Vec<String> {
    headers
        .get(protocol::HTTP_HEADER)
        .and_then(|value| protocol::parse_declaration(value.as_bytes(), ClientKind::Browser).ok())
        .map(|declaration| declaration.required_features)
        .unwrap_or_default()
}

struct ScheduledBackup {
    interval: Duration,
    destination: PathBuf,
    next_due: Instant,
    worker: Option<tokio::task::JoinHandle<(Instant, Result<PublishedBackup>)>>,
    cancelled: Arc<AtomicBool>,
}

fn backup_due(
    now: Instant,
    next_due: Instant,
    in_flight: bool,
    draining: bool,
    held: bool,
) -> bool {
    now >= next_due && !in_flight && !draining && !held
}

impl ScheduledBackup {
    async fn tick(&mut self, app: &Application, lock: &Arc<InstanceLock>, now: Instant) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            self.reap(app).await;
        }
        if self.cancelled.load(AtomicOrdering::SeqCst)
            || !backup_due(
                now,
                self.next_due,
                self.worker.is_some(),
                app.is_draining(),
                false,
            )
        {
            return;
        }
        match crate::database::hold_active(&app.store) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(error=%error, "scheduled backup admission could not read restore hold");
                return;
            }
        }
        let worker_app = app.clone();
        let worker_lock = Arc::clone(lock);
        let destination = self.destination.clone();
        let control = OnlineBackupControl::new(Arc::clone(&self.cancelled));
        self.worker = Some(tokio::task::spawn_blocking(move || {
            let result = run_scheduled_backup(&worker_app, worker_lock, &destination, &control);
            (Instant::now(), result)
        }));
    }

    async fn reap(&mut self, app: &Application) {
        if let Some(worker) = self.worker.take() {
            let (finished, result) = match worker.await {
                Ok(result) => result,
                Err(error) => (
                    Instant::now(),
                    Err(anyhow!("scheduled backup worker failed: {error}")),
                ),
            };
            self.next_due = finished + self.interval;
            record_backup_outcome(app, result);
        }
    }

    async fn finish(&mut self, app: &Application) {
        self.cancelled.store(true, AtomicOrdering::SeqCst);
        self.reap(app).await;
    }
}

fn run_scheduled_backup(
    app: &Application,
    lock: Arc<InstanceLock>,
    destination: &std::path::Path,
    control: &OnlineBackupControl,
) -> Result<PublishedBackup> {
    record_backup_event(
        app,
        "info",
        "backup.started",
        "started",
        None,
        serde_json::json!({"destination":destination}),
    );
    crate::database::validate_online_backup_destination(&app.paths, &lock, destination)?;
    if app.is_draining() || crate::database::hold_active(&app.store)? {
        bail!("scheduled backup suppressed before copying because service is draining or a restore hold is active")
    }
    crate::database::backup_online(&app.paths, lock, destination, control)
}

fn record_backup_outcome(app: &Application, result: Result<PublishedBackup>) {
    let (severity, outcome, operation_id, detail) = match result {
        Err(error) => (
            "warn",
            "unpublished",
            None,
            serde_json::json!({"cause":format!("{error:#}"),"action":"Inspect backup storage and diagnostics; the next attempt waits one interval."}),
        ),
        Ok(backup) => {
            let operation_id = backup.snapshot["operation_id"].as_str().map(str::to_owned);
            let (severity, outcome, warning, action) = match backup.publication {
                BackupPublication::Complete => ("info", "verified", None, None),
                BackupPublication::DurabilityUnconfirmed(cause) => (
                    "warn", "publication_durability_unconfirmed", Some(cause),
                    Some("Keep the snapshot and inspect storage errors before relying on it for recovery."),
                ),
                BackupPublication::RetentionWarning(cause) => (
                    "warn", "retention_incomplete", Some(cause),
                    Some("The snapshot is durable; inspect destination permissions, space and the retention warning."),
                ),
            };
            (
                severity,
                outcome,
                operation_id,
                serde_json::json!({"snapshot":backup.snapshot,"warning":warning,"action":action}),
            )
        }
    };
    record_backup_event(
        app,
        severity,
        "backup.finished",
        outcome,
        operation_id.as_deref(),
        detail,
    );
}

fn record_backup_event(
    app: &Application,
    severity: &str,
    event: &str,
    outcome: &str,
    operation_id: Option<&str>,
    detail: serde_json::Value,
) {
    if let Err(error) = app.diagnostics.record(
        severity,
        event,
        "database_backup",
        outcome,
        operation_id,
        detail.clone(),
    ) {
        tracing::warn!(error=%error, event, outcome, detail=%crate::diagnostics::sanitize_value(detail), "backup diagnostic could not be persisted");
    }
}

pub(crate) async fn serve(
    paths: InstancePaths,
    requested_port: u16,
    open_browser: bool,
    backup_options: BackupOptions,
) -> Result<()> {
    let owned = paths.clone();
    let (lock, backup_options) = tokio::task::spawn_blocking(move || -> Result<_> {
        backup_options.resolve(&owned)?;
        owned.create()?;
        Ok((Arc::new(InstanceLock::acquire(&owned)?), backup_options))
    })
    .await??;
    serve_locked(
        paths,
        requested_port,
        open_browser,
        backup_options,
        lock,
        false,
    )
    .await
}

pub(crate) async fn serve_locked(
    paths: InstancePaths,
    requested_port: u16,
    open_browser: bool,
    backup_options: BackupOptions,
    lock: Arc<InstanceLock>,
    background: bool,
) -> Result<()> {
    let startup_paths = paths.clone();
    let startup_lock = lock.clone();
    let (app, backup_configuration) = tokio::task::spawn_blocking(move || -> Result<_> {
    let paths = startup_paths;
    let lock = startup_lock;
    let backup_configuration = backup_options.resolve(&paths)?;
    crate::database::recover_interrupted_restore_locked(&paths)?;
    crate::database::validate_live_database_family(&paths)?;
    let store = crate::database::open_service_locked(&paths, &lock)?;
    let executable = std::env::current_exe().context("resolve LLMRelay executable")?;
    let app = Application::new(paths.clone(), store, executable)?;
    let restore_held = crate::database::hold_active(&app.store)?;
    let retired_cmux_presentations = app
        .store
        .retire_prior_cmux_presentation_boots(app.service_boot_id())?
        + app
            .store
            .retire_prior_boot_history_views(app.service_boot_id())?;
    if let Err(error) = crate::transcript::reclaim_history_exports(&paths.artifacts) {
        tracing::warn!(error = %format!("{error:#}"), "disposable history exports were not reclaimed");
    }
    let (recovery, restart_candidates, restart_reconciliation, interrupted_applies) =
        if restore_held {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        } else {
            let recovery = crate::recovery::reconcile_prior_boot(&app.store)?;
            app.supervisor.reconcile()?;
            let restart_candidates = crate::recovery::prepare_restart_candidates(&app.store)?;
            let restart_reconciliation = crate::recovery::reconcile_restart_candidates(&app.store)?;
            let interrupted_applies = crate::trip::reconcile_interrupted_applies(&app.store)?;
            (
                recovery,
                restart_candidates,
                restart_reconciliation,
                interrupted_applies,
            )
        };
    if restore_held
        || !recovery.is_empty()
        || !restart_candidates.is_empty()
        || !interrupted_applies.is_empty()
        || retired_cmux_presentations > 0
    {
        app.diagnostics.record(
            "warn",
            "recovery.prior_boot",
            "recovery",
            "attention_required",
            None,
            serde_json::json!({"restore_hold":restore_held,"sessions":recovery,"restart_candidates":restart_candidates,"restart_reconciliation":restart_reconciliation,"interrupted_applies":interrupted_applies,"retired_cmux_presentations":retired_cmux_presentations}),
        )?;
    }
    if !restore_held {
        let _ = app.scheduler.reconcile_unknown()?;
    }
    if let Some((_, destination)) = &backup_configuration {
        crate::database::validate_online_backup_destination(&paths, &lock, destination)
            .context("scheduled backup destination is unsafe; choose a private destination outside the instance and repositories, or omit the backup flags")?;
    }
    record_backup_event(
        &app,
        "info",
        "backup.configuration",
        "configured",
        None,
        serde_json::json!({"enabled":backup_configuration.is_some(),"interval_hours":backup_options.every_hours,"destination":backup_configuration.as_ref().map(|(_, path)|path)}),
    );
    Ok((app, backup_configuration))
    }).await??;
    let listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        requested_port,
    ))
    .await
    .with_context(|| format!("bind loopback port {requested_port}"))?;
    let bound = listener.local_addr()?;
    let endpoint_app = app.clone();
    let (control_listener, role_listener) = tokio::task::spawn_blocking(move || -> Result<_> {
        Ok((
            control::bind(&endpoint_app)?,
            crate::task_cli::bind(&endpoint_app)?,
        ))
    })
    .await??;
    let instance_id = uuid::Uuid::new_v4().to_string();
    let boot_id = app.service_boot_id().to_owned();
    let bootstrap = if background {
        None
    } else {
        Some(app.bootstrap.issue()?)
    };
    let record = InstanceRecord {
        instance_id: instance_id.clone(),
        boot_id: boot_id.clone(),
        pid: std::process::id(),
        address: format!("http://127.0.0.1:{}", bound.port()),
        data_dir: paths.root.clone(),
        control_socket: paths.control_socket.clone(),
        role_socket: paths.role_socket.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        status: "running".to_owned(),
    };
    let startup_paths = paths.clone();
    let startup_record = record.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        atomic_write(
            &startup_paths.instance_file,
            &serde_json::to_vec_pretty(&startup_record)?,
        )?;
        append_startup_event(&startup_paths, &startup_record)
    })
    .await??;
    let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    println!("LLMRelay {}", crate::VERSION);
    println!("Instance: {} (boot {})", record.instance_id, record.boot_id);
    println!("Data: {}", paths.root.display());
    println!("Logs: {}", paths.logs.display());
    println!("Role socket: {}", paths.role_socket.display());

    let clean_dashboard_url = record.address.clone();
    let bootstrap_url = bootstrap.map(|token| format!("{}/#bootstrap={}", record.address, token));

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let web_state = WebState {
        app: app.clone(),
        instance_id: instance_id.clone(),
        boot_id: boot_id.clone(),
        allowed_hosts: vec![
            format!("127.0.0.1:{}", bound.port()),
            format!("localhost:{}", bound.port()),
        ],
        session_cookie: browser_session_cookie_name(&paths.root),
        browser_sessions: Arc::new(Mutex::new(HashSet::new())),
        shutdown: shutdown_rx.clone(),
        state_waiters: Arc::new(Semaphore::new(MAX_STATE_WAITERS)),
    };
    let router = web_router(web_state);
    let (failure_tx, mut failure_rx) = watch::channel(None::<String>);
    let control_app = app.clone();
    let control_instance = serde_json::to_value(&record)?;
    let control_tx = shutdown_tx.clone();
    let control_failure = failure_tx.clone();
    let control_task = tokio::spawn(async move {
        if let Err(error) = control::serve_bound(
            control_app,
            control_tx.clone(),
            control_instance,
            control_listener,
        )
        .await
        {
            tracing::error!(error = %error, "control socket stopped");
            let _ = control_failure.send(Some(format!("control listener failed: {error:#}")));
            let _ = control_tx.send(true);
        }
    });
    let role_app = app.clone();
    let role_tx = shutdown_tx.clone();
    let role_failure = failure_tx.clone();
    tokio::spawn(async move {
        if let Err(error) =
            crate::task_cli::serve_bound(role_app, role_listener, role_tx.subscribe()).await
        {
            tracing::error!(error = %error, "role socket stopped");
            let _ = role_failure.send(Some(format!("role listener failed: {error:#}")));
            let _ = role_tx.send(true);
        }
    });
    let coordinator_app = app.clone();
    let drain_observation = Arc::new(tokio::sync::Notify::new());
    let coordinator_drain_observation = drain_observation.clone();
    let mut reconcile_shutdown = shutdown_rx.clone();
    let coordinator_lock = Arc::clone(&lock);
    let backup_cancelled = Arc::new(AtomicBool::new(false));
    let coordinator_cancelled = Arc::clone(&backup_cancelled);
    let mut scheduled_backup =
        backup_configuration.map(|(interval, destination)| ScheduledBackup {
            interval,
            destination,
            next_due: Instant::now() + interval,
            worker: None,
            cancelled: coordinator_cancelled.clone(),
        });
    let coordinator_task = tokio::spawn(async move {
        let service_started_at = chrono::Utc::now();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut intake_error_reported = false;
        // Unknown at startup: a deferral recorded before a restart is closed by
        // the first tick that completes.
        let mut tick_deferral_open = true;
        loop {
            if *reconcile_shutdown.borrow() {
                break;
            }
            tokio::select! {
                _ = interval.tick() => {
                    if *reconcile_shutdown.borrow() { break; }
                    if let Some(backup) = &mut scheduled_backup {
                        backup.tick(&coordinator_app, &coordinator_lock, Instant::now()).await;
                    }
                    let tick_app = coordinator_app.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        if let Err(error) = tick_app.checks.reap_pending_children() {
                            tracing::warn!(error=%error, "retained check cleanup deferred");
                        }
                        let coordinator = tick_app.coordinator_tick();
                        let intake = tick_app.scheduled_intake_tick(chrono::Utc::now(), service_started_at);
                        if let Err(error) = intake {
                            if !intake_error_reported {
                                tracing::warn!(error=%error, "scheduled intake deferred");
                                let _ = tick_app.diagnostics.record("warn", "recipe.intake", "recipe_schedule", "deferred", None, serde_json::json!({"cause":format!("{error:#}")}));
                            }
                            intake_error_reported = true;
                        } else { intake_error_reported = false; }
                        match coordinator {
                            Ok(result) => {
                                if tick_deferral_open {
                                    match crate::coordinator::record_tick_recovered(&tick_app.store) {
                                        Ok(_) => tick_deferral_open = false,
                                        Err(error) => tracing::warn!(error=%error, "coordinator recovery record deferred"),
                                    }
                                }
                                if result.get("action").and_then(serde_json::Value::as_str) != Some("idle") {
                                    let _ = tick_app.diagnostics.record("info", "coordinator.tick", "coordinator", "success", None, result);
                                }
                            }
                            Err(error) => {
                                let cause = format!("{error:#}");
                                tick_deferral_open = true;
                                let subject = error.downcast_ref::<crate::coordinator::TickSubject>().map(|subject| subject.attempt_id.clone());
                                match crate::coordinator::record_tick_deferred(&tick_app.store, &cause, subject.as_deref()) {
                                    Ok(false) => {},
                                    Ok(true) | Err(_) => {
                                        tracing::warn!(error=%error, "coordinator action deferred");
                                        let _ = tick_app.diagnostics.record("warn", "coordinator.tick", "coordinator", "deferred", None, serde_json::json!({"cause":cause}));
                                    }
                                }
                            }
                        }
                        (intake_error_reported, tick_deferral_open)
                    }).await;
                    match result {
                        Ok((intake_error, tick_deferral)) => {
                            intake_error_reported = intake_error;
                            tick_deferral_open = tick_deferral;
                            coordinator_drain_observation.notify_one();
                        }
                        Err(error) => {
                            tracing::error!(error=%error, "coordinator worker failed; retaining ownership while drain reconciles");
                            coordinator_app.request_drain();
                        }
                    }
                }
                changed = reconcile_shutdown.changed() => { if changed.is_err() || *reconcile_shutdown.borrow() { break } }
            }
        }
        coordinator_cancelled.store(true, AtomicOrdering::SeqCst);
        if let Some(backup) = &mut scheduled_backup {
            backup.finish(&coordinator_app).await;
        }
    });
    let mut opener_shutdown = shutdown_rx.clone();
    let opener_task = if let Some(bootstrap_url) = bootstrap_url.filter(|_| open_browser) {
        Some(tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
                changed = opener_shutdown.changed() => {
                    if changed.is_err() || *opener_shutdown.borrow() {
                        return
                    }
                }
            }
            match open_default_browser(&bootstrap_url).await {
                Ok(()) => {
                    println!("Dashboard: {clean_dashboard_url}");
                    println!("The default browser accepted the dashboard launch request; authentication completes when it exchanges the one-use link.");
                }
                Err(error) => {
                    eprintln!("Could not open the default browser: {error:#}");
                    println!("Manual dashboard login: {bootstrap_url}");
                }
            }
        }))
    } else {
        if !background {
            let token = app.bootstrap.issue()?;
            println!("Automatic browser opening disabled by --no-open.");
            println!(
                "Manual dashboard login: {}/#bootstrap={token}",
                record.address
            );
        } else {
            println!(
                "Dashboard: {} (use Open dashboard to authenticate)",
                record.address
            );
        }
        None
    };
    let signal_shutdown = shutdown_tx.clone();
    let signal_app = app.clone();
    let shutdown_owner = tokio::spawn(async move {
        shutdown_owner(
            signal_app,
            signal_shutdown,
            interrupt,
            terminate,
            drain_observation,
        )
        .await
    });
    let server_result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() { break }
                    }
                    changed = failure_rx.changed() => {
                        if changed.is_err() || failure_rx.borrow().is_some() { break }
                    }
                }
            }
        })
        .await;

    backup_cancelled.store(true, AtomicOrdering::SeqCst);
    let _ = shutdown_tx.send(true);
    let shutdown_error = match shutdown_owner.await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("shutdown owner failed: {error:#}")),
        Err(error) => Some(format!("shutdown owner task failed: {error}")),
    };
    let coordinator_error = coordinator_task
        .await
        .err()
        .map(|error| format!("coordinator loop failed: {error}"));
    let control_error = control_task
        .await
        .err()
        .map(|error| format!("control listener task failed: {error}"));

    if let Some(task) = opener_task {
        let _ = task.await;
    }

    let listener_failure = failure_tx
        .borrow()
        .clone()
        .or(shutdown_error)
        .or(coordinator_error)
        .or(control_error);
    let final_record = InstanceRecord {
        status: if listener_failure.is_some() || server_result.is_err() {
            "unhealthy".to_owned()
        } else {
            "stopped".to_owned()
        },
        ..record
    };
    let final_path = paths.instance_file.clone();
    tokio::task::spawn_blocking(move || {
        atomic_write(&final_path, &serde_json::to_vec_pretty(&final_record)?)
    })
    .await??;
    drop(lock);
    server_result.context("dashboard listener failed")?;
    if let Some(error) = listener_failure {
        Err(anyhow!(error))
    } else {
        Ok(())
    }
}

async fn shutdown_owner(
    app: Application,
    shutdown: watch::Sender<bool>,
    mut interrupt: tokio::signal::unix::Signal,
    mut terminate: tokio::signal::unix::Signal,
    observation: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let mut stopped = shutdown.subscribe();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interrupt.recv() => app.request_drain(),
            _ = terminate.recv() => app.request_drain(),
            _ = interval.tick() => {},
            _ = observation.notified() => {},
            _ = stopped.wait_for(|value| *value) => return Ok(()),
        }
        if !app.is_draining() {
            continue;
        }
        let drain_app = app.clone();
        let status = tokio::task::spawn_blocking(move || {
            let initiation = drain_app.begin_drain();
            // Observation is independent of a held/failed ordinary tick and of
            // failed initiation. Unknown ownership can never become a timeout.
            let status = drain_app.drain_status();
            (initiation, status)
        })
        .await;
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(error=%error, "shutdown observation failed; retaining instance ownership for retry");
                continue;
            }
        };
        if status.0.is_ok()
            && status
                .1
                .as_ref()
                .is_ok_and(|value| value["quiescent"] == true)
        {
            control::signal_shutdown(&shutdown)?;
            return Ok(());
        }
    }
}

async fn api_desktop_target(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(activation): Query<crate::domain::DesktopActivation>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    if !declared_features(&headers)
        .iter()
        .any(|feature| feature == protocol::DESKTOP_TARGET_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            "Reload the dashboard to open desktop attention",
        )
            .into_response();
    }
    let incarnation = state.instance_id.clone();
    match tokio::task::spawn_blocking(move || state.app.desktop_target(&activation)).await {
        Ok(Ok(mut target)) => {
            if let crate::domain::DesktopTarget::Current {
                incarnation: current,
                ..
            } = &mut target
            {
                *current = incarnation;
            }
            Json(target).into_response()
        }
        Ok(Err(_)) => (
            StatusCode::CONFLICT,
            "Attention could not be revalidated. Refresh the dashboard.",
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Desktop attention is unavailable",
        )
            .into_response(),
    }
}

fn web_router(web_state: WebState) -> Router {
    let operational = Router::new()
        .route("/api/health", get(health))
        .route("/api/state", get(api_state))
        .route("/api/desktop-target", get(api_desktop_target))
        .route("/api/state/wait", get(api_state_wait))
        .route("/api/restart-preview", get(api_restart_preview))
        .route("/api/model-catalog", get(api_model_catalog))
        .route(
            "/api/tasks/{task_id}/role-preparations",
            get(api_role_preparations),
        )
        .route("/api/tasks/{task_id}/content", get(api_task_content))
        .route(
            "/api/tasks/{task_id}/guidance-reauthorization",
            get(api_guidance_reauthorization),
        )
        .route("/api/tasks/{task_id}/activity", get(api_task_activity))
        .route("/api/history/search", get(api_history_search))
        .route("/api/history/source", get(api_historical_source))
        .route("/api/history/usage", get(api_usage_comparisons))
        .route("/api/history/comparisons", get(api_workflow_comparisons))
        .route(
            "/api/history/comparisons/evidence",
            get(api_comparison_evidence),
        )
        .route("/api/permission-coverage", post(api_permission_coverage))
        .route("/api/knowledge/preview", post(api_knowledge_preview))
        .route("/api/knowledge/read", post(api_knowledge_read))
        .route("/api/knowledge/command", post(api_knowledge_command))
        .route("/api/command", post(api_command))
        .route("/api/operation", post(api_operation))
        .route("/api/diagnostics", get(api_diagnostics))
        .route(
            "/api/diagnostics/runtime-bindings",
            get(api_diagnostics_runtime_bindings),
        )
        .route_layer(middleware::from_fn_with_state(
            web_state.clone(),
            guard_operational_protocol,
        ));
    Router::new()
        .route("/", get(index))
        .route("/api/bootstrap", post(bootstrap_session))
        .route("/api/protocol", get(api_protocol))
        .merge(operational)
        .route("/{*asset}", get(asset))
        .fallback(not_found)
        .with_state(web_state)
}

async fn open_default_browser(url: &str) -> Result<()> {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || open_browser_blocking(&url)).await?
}

#[cfg(target_os = "macos")]
pub(crate) fn open_browser_blocking(url: &str) -> Result<()> {
    let mut child = Command::new("/usr/bin/open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("launch /usr/bin/open")?;
    let deadline = Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                bail!("/usr/bin/open exited with {status}")
            }
            Ok(None) => {}
            Err(error) => {
                if let Err(cleanup_error) = terminate_and_reap_browser_opener(&mut child) {
                    bail!(
                        "wait for /usr/bin/open failed ({error}); cleanup also failed: {cleanup_error:#}"
                    )
                }
                return Err(error).context("wait for /usr/bin/open");
            }
        }
        if Instant::now() >= deadline {
            if let Err(cleanup_error) = terminate_and_reap_browser_opener(&mut child) {
                bail!("/usr/bin/open timed out and cleanup also failed: {cleanup_error:#}")
            }
            bail!("/usr/bin/open did not finish within 3 seconds")
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[cfg(target_os = "macos")]
fn terminate_and_reap_browser_opener(child: &mut Child) -> Result<()> {
    let kill_error = child.kill().err();
    match child.wait() {
        Ok(_) => Ok(()),
        Err(wait_error) => match kill_error {
            Some(kill_error) => Err(anyhow!(
                "could not terminate /usr/bin/open ({kill_error}) or reap it ({wait_error})"
            )),
            None => Err(wait_error).context("reap /usr/bin/open"),
        },
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn open_browser_blocking(_url: &str) -> Result<()> {
    bail!("automatic browser opening is supported only on macOS")
}

async fn index(State(state): State<WebState>, headers: HeaderMap) -> Response {
    match check_origin(&state, &headers, false) {
        Ok(()) => embedded_response("/"),
        Err(response) => response,
    }
}

async fn asset(
    State(state): State<WebState>,
    headers: HeaderMap,
    AxumPath(asset): AxumPath<String>,
) -> Response {
    match check_origin(&state, &headers, false) {
        Ok(()) => embedded_response(&format!("/{asset}")),
        Err(response) => response,
    }
}

fn embedded_response(path: &str) -> Response {
    let Some((_, bytes, content_type)) =
        embedded::ASSETS.iter().find(|(route, _, _)| *route == path)
    else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mut response = Response::new(Body::from(*bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        content_type.parse().expect("generated asset content type"),
    );
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert(
        header::HeaderName::from_static("x-agenticjira-asset-identity"),
        embedded::ASSET_IDENTITY.parse().unwrap(),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; font-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
    response
}

async fn bootstrap_session(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<BootstrapRequest>,
) -> Response {
    if let Err(response) = check_origin(&state, &headers, true) {
        return response;
    }
    if state.app.bootstrap.consume(&request.token).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            "bootstrap credential is invalid, expired, or consumed",
        )
            .into_response();
    }
    let session = auth::issue_secret();
    let session_hash = crate::auth::hash_secret(&session);
    if let Ok(mut sessions) = state.browser_sessions.lock() {
        sessions.insert(session_hash);
    } else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "browser authentication state unavailable",
        )
            .into_response();
    }
    let mut response = Json(serde_json::json!({"authenticated": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "{}={session}; HttpOnly; SameSite=Strict; Path=/",
            state.session_cookie
        )
        .parse()
        .unwrap(),
    );
    response
}

async fn health(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(response) = check_origin(&state, &headers, true) {
        return response;
    }
    if !has_browser_session(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "browser session required").into_response();
    }
    Json(serde_json::json!({"status": "running", "instance_id": state.instance_id, "boot_id": state.boot_id, "dashboard": "embedded", "asset_identity":embedded::ASSET_IDENTITY})).into_response()
}

async fn api_state(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    match blocking_snapshot(&state).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}

async fn api_state_wait(
    State(state): State<WebState>,
    headers: HeaderMap,
    query: std::result::Result<Query<StateWaitQuery>, QueryRejection>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let request = match query {
        Ok(Query(query)) => StateWaitRequest::try_from(query).map_err(str::to_owned),
        Err(rejection) => Err(rejection.body_text()),
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":error})),
            )
                .into_response()
        }
    };
    let mut shutdown = state.shutdown.clone();
    if *shutdown.borrow_and_update() {
        return state_wait_unavailable("service is shutting down");
    }
    let Ok(permit) = Arc::clone(&state.state_waiters)
        .try_acquire_owned()
        .map(Arc::new)
    else {
        return state_wait_unavailable("too many dashboard state waits are active");
    };
    let deadline = tokio::time::sleep(request.timeout);
    match wait_for_state(&state, &request, &permit, &mut shutdown, deadline).await {
        Ok(StateWait::Respond(response)) => Json(response).into_response(),
        Ok(StateWait::ShuttingDown) => state_wait_unavailable("service is shutting down"),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}

/// Once shutdown begins the wait closes, even while one of its reads is still
/// running, and a response completed as shutdown began is withheld.
async fn wait_for_state(
    state: &WebState,
    request: &StateWaitRequest,
    permit: &Arc<OwnedSemaphorePermit>,
    shutdown: &mut watch::Receiver<bool>,
    deadline: impl Future<Output = ()>,
) -> Result<StateWait> {
    let response = tokio::select! {
        biased;
        // A dropped sender also means the service is stopping.
        _ = shutdown.wait_for(|stopping| *stopping) => return Ok(StateWait::ShuttingDown),
        response = respond_to_cursor(state, request, permit, deadline) => response,
    };
    if *shutdown.borrow() {
        return Ok(StateWait::ShuttingDown);
    }
    response.map(StateWait::Respond)
}

/// Waits until the committed revision differs from the client cursor. Each
/// iteration registers for wakes before reading the revision, so a commit that
/// lands between the read and the wait still wakes this waiter.
async fn respond_to_cursor(
    state: &WebState,
    request: &StateWaitRequest,
    permit: &Arc<OwnedSemaphorePermit>,
    deadline: impl Future<Output = ()>,
) -> Result<StateWaitResponse> {
    if request.incarnation != state.instance_id {
        let snapshot = waiter_snapshot(state, permit).await?;
        return Ok(StateWaitResponse::reset(snapshot));
    }
    tokio::pin!(deadline);
    loop {
        let changed = state.app.store.state_changes().notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let revision = blocking_wait_read(state, permit, Store::state_revision).await?;
        match revision.cmp(&request.revision) {
            Ordering::Greater => {
                let snapshot = waiter_snapshot(state, permit).await?;
                return Ok(StateWaitResponse::state_changed(snapshot));
            }
            Ordering::Less => {
                let snapshot = waiter_snapshot(state, permit).await?;
                return Ok(StateWaitResponse::reset(snapshot));
            }
            Ordering::Equal => {}
        }
        tokio::select! {
            biased;
            () = &mut changed => {}
            () = &mut deadline => {
                return Ok(StateWaitResponse::Unchanged {
                    incarnation: state.instance_id.clone(),
                    revision: request.revision.to_string(),
                })
            }
        }
    }
}

/// Builds the projection on the blocking pool: it holds the store lock and
/// samples processes, neither of which may run on an executor thread.
async fn blocking_snapshot(state: &WebState) -> Result<StateSnapshot> {
    let store = state.app.store.clone();
    let projection = tokio::task::spawn_blocking(move || crate::workflow::state(&store))
        .await
        .context("state snapshot task failed")??;
    Ok(StateSnapshot {
        incarnation: state.instance_id.clone(),
        state: projection,
    })
}

async fn waiter_snapshot(
    state: &WebState,
    permit: &Arc<OwnedSemaphorePermit>,
) -> Result<StateSnapshot> {
    Ok(StateSnapshot {
        incarnation: state.instance_id.clone(),
        state: blocking_wait_read(state, permit, crate::workflow::state).await?,
    })
}

/// Runs a waiter's store read on the blocking pool. The read owns a share of
/// the waiter's permit until it returns, so a waiter that is cancelled or
/// closed by shutdown keeps its capacity while that read is queued or running.
async fn blocking_wait_read<T: Send + 'static>(
    state: &WebState,
    permit: &Arc<OwnedSemaphorePermit>,
    read: fn(&Store) -> Result<T>,
) -> Result<T> {
    let store = state.app.store.clone();
    let permit = Arc::clone(permit);
    tokio::task::spawn_blocking(move || {
        let output = read(&store);
        drop(permit);
        output
    })
    .await
    .context("state wait read task failed")?
}

fn state_wait_unavailable(reason: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1")],
        Json(serde_json::json!({"error":reason})),
    )
        .into_response()
}

async fn api_restart_preview(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let app = state.app.clone();
    match tokio::task::spawn_blocking(move || app.restart_preview()).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("restart preview task failed: {error}")})),
        )
            .into_response(),
    }
}

async fn api_model_catalog(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<ModelCatalogQuery>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    if !matches!(query.provider.as_str(), "codex" | "claude") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":"provider must be codex or claude"})),
        )
            .into_response();
    }
    let permits = Arc::clone(&state.app.blocking_operations);
    match admitted_blocking_work(permits, move || {
        Json(crate::model_catalog::read(&query.provider)).into_response()
    })
    .await
    {
        Ok(response) => response,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"The model catalog could not be read. Try again."})),
        )
            .into_response(),
    }
}

#[derive(Debug)]
enum BlockingWorkError {
    Admission,
    Interrupted(tokio::task::JoinError),
}

impl std::fmt::Display for BlockingWorkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admission => formatter.write_str("blocking operation admission unavailable"),
            Self::Interrupted(error) => {
                write!(formatter, "blocking operation interrupted: {error}")
            }
        }
    }
}

// The closure owns admission until the blocking work ends, even if its HTTP
// caller disconnects or is cancelled while awaiting the result.
async fn admitted_blocking_work<T: Send + 'static>(
    permits: Arc<Semaphore>,
    work: impl FnOnce() -> T + Send + 'static,
) -> std::result::Result<T, BlockingWorkError> {
    let permit = permits
        .acquire_owned()
        .await
        .map_err(|_| BlockingWorkError::Admission)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(BlockingWorkError::Interrupted)
}

async fn api_role_preparations(
    State(state): State<WebState>,
    AxumPath(task_id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let permits = Arc::clone(&state.app.blocking_operations);
    match admitted_blocking_work(permits, move || -> Result<_> {
        let executable = std::env::current_exe().context("resolve current LLMRelay executable")?;
        crate::workflow::role_preparations(
            &state.app.store,
            &task_id,
            &state.app.hooks,
            &state.app.paths.role_socket,
            &executable,
        )
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":format!("role preparation could not finish: {error:#}")})),
        )
            .into_response(),
    }
}

async fn api_guidance_reauthorization(
    State(state): State<WebState>,
    AxumPath(task_id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || {
        crate::trip::guidance_reauthorization_preview(&store, &task_id)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("guidance preview read failed: {error}")})),
        )
            .into_response(),
    }
}

async fn api_task_content(
    State(state): State<WebState>,
    AxumPath(task_id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || crate::workflow::task_content(&store, &task_id)).await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("task content read failed: {error}")})),
        )
            .into_response(),
    }
}

async fn api_command(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(command): Json<HumanCommand>,
) -> Response {
    api_command_with_executor(state, headers, command, |app, command| {
        match app.execute_human_command(command) {
            Ok(value) => Json(serde_json::json!({"result":value})).into_response(),
            Err(error) => {
                let _ = app.diagnostics.record(
                    "warn",
                    "human.command.rejected",
                    "server",
                    "rejected",
                    Some(command.operation_id()),
                    serde_json::json!({"cause":format!("{error:#}")}),
                );
                if matches!(
                    command,
                    HumanCommand::SetTaskKnowledge { .. }
                        | HumanCommand::PrepareNextRunKnowledge { .. }
                ) {
                    // Rework preparation can fail after the workflow transaction commits.
                    let receipt = (|| -> Result<bool> {
                        let hash = crate::store::json_hash(command)?;
                        Ok(app.store.lock()?.query_row(
                            "SELECT EXISTS(SELECT 1 FROM operation_receipts WHERE operation_id=?1
                             AND actor_key='human_control' AND operation_kind='human_command'
                             AND request_hash=?2)",
                            rusqlite::params![command.operation_id(), hash],
                            |row| row.get(0),
                        )?)
                    })();
                    return knowledge_mutation_error(
                        StatusCode::CONFLICT,
                        command.operation_id(),
                        !matches!(receipt, Ok(false)),
                    );
                }
                human_command_error_response(command.operation_id(), &error)
            }
        }
    })
    .await
}

fn human_command_error_response(operation_id: &str, error: &anyhow::Error) -> Response {
    let refusal = error.downcast_ref::<crate::trip::SetupProbeAuthorizationRefused>();
    let terminal_refusal = refusal.is_some_and(|refusal| refusal.matches_operation(operation_id));
    checked_operation_error_response(
        StatusCode::CONFLICT,
        operation_id,
        error.is::<crate::trip::TripOutcomeUnknown>() || (refusal.is_some() && !terminal_refusal),
        terminal_refusal,
        &format!("{error:#}"),
    )
}

async fn api_command_with_executor(
    state: WebState,
    headers: HeaderMap,
    command: HumanCommand,
    execute: impl FnOnce(&Application, &HumanCommand) -> Response + Send + 'static,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    if matches!(
        command,
        HumanCommand::RequestRecipeScheduleOccurrenceStop { .. }
    ) && !declared_features(&headers)
        .iter()
        .any(|feature| feature == protocol::SCHEDULE_OCCURRENCE_STOP_INTENT_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"Reload the dashboard to stop this scheduled occurrence."
            })),
        )
            .into_response();
    }
    if matches!(
        command,
        HumanCommand::AuthorizeRecipeScheduleExecution { .. }
    ) && !declared_features(&headers)
        .iter()
        .any(|feature| feature == protocol::SCHEDULE_EXECUTION_AUTHORIZATION_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"Reload the dashboard to review and authorize scheduled execution."
            })),
        )
            .into_response();
    }
    let task_knowledge_command = matches!(
        command,
        HumanCommand::SetTaskKnowledge { .. } | HumanCommand::PrepareNextRunKnowledge { .. }
    );
    if task_knowledge_command
        && !declared_features(&headers)
            .iter()
            .any(|feature| feature == crate::knowledge::TASK_KNOWLEDGE_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"Reload the dashboard to prepare task knowledge.",
                "operation_id":command.operation_id(),
                "outcome_unknown":false
            })),
        )
            .into_response();
    }
    if matches!(command, HumanCommand::UpsertSingleAgentRecipe { .. })
        && !declared_features(&headers)
            .iter()
            .any(|feature| feature == protocol::SINGLE_AGENT_PRESETS_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"Reload the dashboard to use Single agent recipes."})),
        )
            .into_response();
    }
    if matches!(
        command,
        HumanCommand::AddTaskComment { .. }
            | HumanCommand::EditGuidance { .. }
            | HumanCommand::CancelGuidance { .. }
    ) && !declared_features(&headers)
        .iter()
        .any(|f| f == protocol::TASK_INTERACTIONS_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"Reload the dashboard to add comments."})),
        )
            .into_response();
    }
    let permits = Arc::clone(&state.app.blocking_operations);
    let operation_id = command.operation_id().to_owned();
    match admitted_blocking_work(permits, move || execute(&state.app, &command)).await {
        Ok(response) => response,
        Err(error) => {
            let outcome_unknown = matches!(error, BlockingWorkError::Interrupted(_));
            if task_knowledge_command {
                return knowledge_mutation_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &operation_id,
                    outcome_unknown,
                );
            }
            let message = if outcome_unknown {
                "The request outcome is unknown. Refresh and check before retrying."
            } else {
                "The request was not started because the service cannot accept more work."
            };
            (StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":format!("{message} {error}"),"operation_id":operation_id,"outcome_unknown":outcome_unknown})),
            ).into_response()
        }
    }
}

async fn api_diagnostics(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let permits = Arc::clone(&state.app.blocking_operations);
    match admitted_blocking_work(permits, move || {
        state
            .app
            .diagnostics
            .read_sanitized(500)
            .map(|events| Json(serde_json::json!({"events":events})).into_response())
    })
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Diagnostics could not be read. Try again."})),
        )
            .into_response(),
    }
}

async fn api_diagnostics_runtime_bindings(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let permits = Arc::clone(&state.app.blocking_operations);
    match admitted_blocking_work(permits, move || -> Result<_> {
        let app = state.app;
        let runtime = crate::trip::CapabilityRuntime::from_store(
            &app.store,
            app.hooks.clone(),
            app.paths.role_socket.clone(),
            app.executable_path().to_owned(),
        );
        #[derive(Serialize)]
        struct RuntimeBindings {
            bindings: Vec<crate::trip::DiagnosticsRuntimeBinding>,
        }
        let bindings = crate::trip::diagnostics_runtime_bindings(&app.store, &runtime)?;
        let body = serde_json::to_vec(&RuntimeBindings { bindings })?;
        if body.len() > MAX_RUNTIME_BINDINGS_RESPONSE_BYTES {
            bail!("Runtime bindings exceed the diagnostics response size limit");
        }
        Ok(body)
    })
    .await
    {
        Ok(Ok(body)) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Ok(Err(error)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"Runtime bindings could not be read. Try again."})),
        )
            .into_response(),
    }
}

fn require_knowledge(state: &WebState, headers: &HeaderMap) -> Result<(), Response> {
    require_browser(state, headers)?;
    if !declared_features(headers)
        .iter()
        .any(|feature| feature == crate::knowledge::TASK_KNOWLEDGE_FEATURE)
    {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"Reload the dashboard to use project knowledge.",
                "outcome_unknown":false
            })),
        )
            .into_response());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgeReadRequest {
    project_id: String,
    read: KnowledgeRead,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum KnowledgeRead {
    Search {
        search: crate::domain::KnowledgeSearch,
    },
    Entry {
        entry_id: String,
    },
    Revision {
        entry_id: String,
        revision_id: String,
    },
    Source {
        entry_id: String,
        revision_id: String,
        index: usize,
    },
    History {
        entry_id: String,
        after_version: i64,
    },
    Freshness {
        entry_id: String,
        revision_id: String,
    },
    Manifest {
        manifest_id: String,
    },
    ConversationConsumer {
        conversation_id: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum KnowledgeCommand {
    Mutate {
        request: crate::domain::KnowledgeMutationRequest,
    },
    Revalidate {
        project_id: String,
        operation_id: String,
        entry_id: String,
        revision_id: String,
        expected_version: i64,
        expected_observation: String,
    },
}

#[derive(Serialize)]
struct KnowledgeCommandResult<'a, T> {
    operation_id: &'a str,
    #[serde(flatten)]
    result: T,
}

async fn knowledge_body<T: serde::de::DeserializeOwned>(
    state: &WebState,
    request: Request,
) -> Result<T, Response> {
    if let Err(response) = require_knowledge(state, request.headers()) {
        return Err(response);
    }
    let body = axum::body::to_bytes(request.into_body(), crate::knowledge::PREVIEW_REQUEST_BYTES).await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, Json(serde_json::json!({
            "error":"Shorten the input or select fewer entries; knowledge requests allow at most 32 KiB."
        }))).into_response())?;
    serde_json::from_slice(&body).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error":"Review the knowledge request and try again."
            })),
        )
            .into_response()
    })
}

fn knowledge_mutation_error(
    status: StatusCode,
    operation_id: &str,
    outcome_unknown: bool,
) -> Response {
    let error = if outcome_unknown {
        "The knowledge request outcome is unavailable. Refresh and check its outcome before retrying."
    } else if status == StatusCode::SERVICE_UNAVAILABLE {
        "The knowledge request was not started. Try again when the service can accept work."
    } else {
        "The knowledge request was refused. Refresh and review the exact knowledge selection before saving again."
    };
    (
        status,
        Json(serde_json::json!({
            "error":error, "operation_id":operation_id, "outcome_unknown":outcome_unknown
        })),
    )
        .into_response()
}

async fn knowledge_work<T: Serialize>(
    state: WebState,
    limit: usize,
    operation_id: Option<String>,
    work: impl FnOnce(&Store) -> Result<T> + Send + 'static,
) -> Response {
    knowledge_response_work(state, limit, operation_id, move |store| {
        let value = work(store).map_err(|error| (error, false))?;
        serde_json::to_vec(&value).map_err(|error| (error.into(), true))
    })
    .await
}

async fn knowledge_response_work(
    state: WebState,
    limit: usize,
    operation_id: Option<String>,
    work: impl FnOnce(&Store) -> Result<Vec<u8>, (anyhow::Error, bool)> + Send + 'static,
) -> Response {
    let permits = Arc::clone(&state.app.blocking_operations);
    let result = admitted_blocking_work(permits, move || {
        let bytes = work(&state.app.store)?;
        if bytes.len() > limit {
            return Err((
                anyhow::anyhow!(
                "The knowledge response is too large; narrow the search or open one exact revision"
            ),
                true,
            ));
        }
        Ok(bytes)
    })
    .await;
    match (result, operation_id.as_deref()) {
        (Ok(Ok(bytes)), _) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        (Ok(Err((error, after_work))), Some(id)) => {
            // Validation and ownership errors precede commit. A failed commit or
            // unreadable receipt can leave the exact operation unconfirmed.
            let outcome_unknown = after_work || error.is::<serde_json::Error>()
                || matches!(error.downcast_ref::<rusqlite::Error>(), Some(sqlite)
                    if !matches!(sqlite, rusqlite::Error::QueryReturnedNoRows));
            knowledge_mutation_error(StatusCode::CONFLICT, id, outcome_unknown)
        }
        (Ok(Err((error, _))), None) => (StatusCode::CONFLICT, Json(serde_json::json!({
            "error":format!("Review the project knowledge selection, refresh its preview and try again. {error:#}")
        }))).into_response(),
        (Err(BlockingWorkError::Admission), Some(id)) => {
            knowledge_mutation_error(StatusCode::SERVICE_UNAVAILABLE, id, false)
        }
        (Err(BlockingWorkError::Interrupted(_)), Some(id)) => {
            knowledge_mutation_error(StatusCode::INTERNAL_SERVER_ERROR, id, true)
        }
        (Err(BlockingWorkError::Admission), None) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Refresh and try again when the service is available",
        ).into_response(),
        (Err(BlockingWorkError::Interrupted(_)), None) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"The knowledge read was interrupted. Refresh and try again."})),
        ).into_response(),
    }
}

async fn api_knowledge_preview(State(state): State<WebState>, request: Request) -> Response {
    let request =
        match knowledge_body::<crate::domain::KnowledgePreviewRequest>(&state, request).await {
            Ok(request) => request,
            Err(response) => return response,
        };
    knowledge_work(
        state,
        crate::knowledge::PREVIEW_RESPONSE_BYTES,
        None,
        move |store| Ok(serde_json::to_value(store.knowledge_preview(&request)?)?),
    )
    .await
}

async fn api_knowledge_read(State(state): State<WebState>, request: Request) -> Response {
    let request = match knowledge_body::<KnowledgeReadRequest>(&state, request).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let limit = match &request.read {
        KnowledgeRead::Manifest { .. } => crate::knowledge::MANIFEST_BYTES,
        KnowledgeRead::Revision { .. } | KnowledgeRead::Source { .. } => {
            crate::knowledge::REVISION_DETAIL_BYTES
        }
        _ => crate::knowledge::PREVIEW_RESPONSE_BYTES,
    };
    knowledge_response_work(state, limit, None, move |store| {
        knowledge_read_bytes(store, &request.project_id, request.read, limit)
            .map_err(|error| (error, false))
    })
    .await
}

fn knowledge_read_bytes(
    store: &Store,
    project: &str,
    read: KnowledgeRead,
    limit: usize,
) -> Result<Vec<u8>> {
    Ok(match read {
        KnowledgeRead::Search { search } => {
            serde_json::to_vec(&store.knowledge_search(project, &search)?)?
        }
        KnowledgeRead::Entry { entry_id } => {
            serde_json::to_vec(&store.knowledge_entry(project, &entry_id)?)?
        }
        KnowledgeRead::Revision {
            entry_id,
            revision_id,
        } => serde_json::to_vec(&store.knowledge_revision(project, &entry_id, &revision_id)?)?,
        KnowledgeRead::Source {
            entry_id,
            revision_id,
            index,
        } => {
            serde_json::to_vec(&store.knowledge_source(project, &entry_id, &revision_id, index)?)?
        }
        KnowledgeRead::History {
            entry_id,
            after_version,
        } => serde_json::to_vec(&store.knowledge_history(project, &entry_id, after_version)?)?,
        KnowledgeRead::Freshness {
            entry_id,
            revision_id,
        } => bounded_freshness(
            store.knowledge_freshness(project, &entry_id, &revision_id)?,
            limit,
        )?,
        KnowledgeRead::Manifest { manifest_id } => {
            serde_json::to_vec(&store.knowledge_manifest(project, &manifest_id)?)?
        }
        KnowledgeRead::ConversationConsumer { conversation_id } => {
            serde_json::to_vec(&store.knowledge_conversation_consumer(project, &conversation_id)?)?
        }
    })
}

fn bounded_freshness(
    mut observation: KnowledgeObservation,
    limit: usize,
) -> serde_json::Result<Vec<u8>> {
    let full = serde_json::to_vec(&observation)?;
    if full.len() <= limit || observation.sources.is_empty() {
        return Ok(full);
    }
    drop(full);
    let sources_omitted = observation.sources.len();
    // The fingerprint still identifies the complete observation.
    observation.sources.clear();
    #[derive(Serialize)]
    struct CompactFreshness {
        #[serde(flatten)]
        observation: KnowledgeObservation,
        sources_omitted: usize,
    }
    serde_json::to_vec(&CompactFreshness {
        observation,
        sources_omitted,
    })
}

async fn api_knowledge_command(State(state): State<WebState>, request: Request) -> Response {
    let request = match knowledge_body::<KnowledgeCommand>(&state, request).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    // Only the authenticated browser control path creates this actor; the wire has no actor field.
    let actor = crate::domain::KnowledgeUserActor {
        user_id: "human_control".into(),
    };
    let operation_id = match &request {
        KnowledgeCommand::Mutate { request } => request.operation_id.clone(),
        KnowledgeCommand::Revalidate { operation_id, .. } => operation_id.clone(),
    };
    knowledge_response_work(
        state,
        crate::knowledge::PREVIEW_RESPONSE_BYTES,
        Some(operation_id),
        move |store| match request {
            KnowledgeCommand::Mutate { request } => {
                let entry = store
                    .knowledge_mutate(&actor, &request)
                    .map_err(|error| (error, false))?;
                serde_json::to_vec(&KnowledgeCommandResult {
                    operation_id: &request.operation_id,
                    result: entry,
                })
                .map_err(|error| (error.into(), true))
            }
            KnowledgeCommand::Revalidate {
                project_id,
                operation_id,
                entry_id,
                revision_id,
                expected_version,
                expected_observation,
            } => {
                let observation = store
                    .knowledge_revalidate(
                        &actor,
                        &project_id,
                        &operation_id,
                        &entry_id,
                        &revision_id,
                        expected_version,
                        &expected_observation,
                    )
                    .map_err(|error| (error, false))?;
                if observation.revision_id != revision_id {
                    return Err((
                        anyhow!("The committed observation does not match the requested revision"),
                        true,
                    ));
                }
                let full = serde_json::to_vec(&KnowledgeCommandResult {
                    operation_id: &operation_id,
                    result: &observation,
                })
                .map_err(|error| (error.into(), true))?;
                if full.len() <= crate::knowledge::PREVIEW_RESPONSE_BYTES {
                    return Ok(full);
                }
                serde_json::to_vec(&serde_json::json!({
                    "kind":"revalidation_confirmed",
                    "operation_id":operation_id,
                    "project_id":project_id,
                    "entry_id":entry_id,
                    "revision_id":observation.revision_id,
                    "fingerprint":observation.fingerprint,
                }))
                .map_err(|error| (error.into(), true))
            }
        },
    )
    .await
}

async fn api_operation(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(operation): Json<WebOperation>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    if let Some(feature) = operation.required_feature() {
        if !declared_features(&headers)
            .iter()
            .any(|declared| declared == feature)
        {
            let error = protocol::ProtocolError::new(
                "missing_required_feature",
                Some(protocol::GENERATION),
            );
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":error.guidance,"protocol_error":error})),
            )
                .into_response();
        }
    }
    if matches!(
        &operation,
        WebOperation::Conversation {
            operation: crate::providers::codex_app_server::ConversationOperation::WithKnowledge { .. }
        }
    ) {
        if let Err(response) = require_knowledge(&state, &headers) {
            return response;
        }
    }
    let checked_operation_id = operation.checked_operation_id().map(str::to_owned);
    let permit = match Arc::clone(&state.app.blocking_operations)
        .acquire_owned()
        .await
    {
        Ok(permit) => permit,
        Err(_) => {
            if let Some(id) = checked_operation_id.as_deref() {
                return checked_operation_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    id,
                    false,
                    false,
                    "The request was not started. Try again after refreshing.",
                );
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"blocking operation admission is unavailable"})),
            )
                .into_response();
        }
    };
    let blocking_state = state.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
        let _permit = permit;
        let state = blocking_state;
        match operation {
            WebOperation::RecipeScheduleExecutionPreview { request } =>
                Ok(serde_json::to_value(state.app.store.recipe_schedule_execution_preview(&request)?)?),
            WebOperation::Conversation { operation } => state.app.conversation_operation(operation),
            WebOperation::SchedulerRunOnce => state.app.coordinator_tick(),
            WebOperation::SnapshotFreeze {
                attempt_id,
                snapshot_kind,
            } => {
                state.app.supervisor.reconcile()?;
                state.app.reviews.freeze(&attempt_id, &snapshot_kind)
            }
            WebOperation::SnapshotVerify { snapshot_id } => Ok(
                serde_json::json!({"snapshot_id":snapshot_id,"verified":state.app.reviews.verify(&snapshot_id)?}),
            ),
            WebOperation::RoleDispatch {
                attempt_id,
                role,
                lane,
                prompt,
            } => Ok(serde_json::to_value(match lane {
                Some(lane) => {
                    if role != RoleKind::Implementer {
                        bail!("only implementer dispatch accepts a lane")
                    }
                    state
                        .app
                        .dispatch_implementation_lane(&attempt_id, &lane, &prompt)?
                }
                None => state
                    .app
                    .dispatch_attempt_role(&attempt_id, role, &prompt)?,
            })?),
            WebOperation::TripSetupDispatch {
                operation_id,
                attempt_id,
                role,
                fresh_resume_rejection,
            } => {
                let result = state
                    .app
                    .dispatch_trip_setup_role_with_operation(
                        &operation_id,
                        &attempt_id,
                        role,
                        fresh_resume_rejection.as_ref(),
                    )?;
                checked_launch_result(
                    &result,
                    &operation_id,
                    None,
                    result.attempt_id == attempt_id && result.launch.role == role,
                )
            }
            WebOperation::RuntimeProbeLaunch {
                operation_id,
                admission_id,
                role,
            } => {
                let result = state
                    .app
                    .dispatch_runtime_probe(&operation_id, &admission_id, role)?;
                checked_launch_result(
                    &result,
                    &operation_id,
                    Some(&admission_id),
                    result.launch.role == role,
                )
            }
            WebOperation::RuntimeProbeResume {
                operation_id,
                admission_id,
                role,
            } => {
                let result = state
                    .app
                    .resume_runtime_probe_with_operation(&operation_id, &admission_id, role)?;
                checked_launch_result(
                    &result,
                    &operation_id,
                    Some(&admission_id),
                    result.launch.role == role,
                )
            }
            WebOperation::GuidanceDeliver { guidance_id } => {
                state.app.roles.deliver_guidance(&guidance_id)
            }
            WebOperation::SwitchRoleRequest {
                operation_id,
                attempt_id,
                role,
                old_generation_id,
                settings_revision,
                snapshot_id,
                handoff,
                expected_task_version,
            } => Ok(serde_json::json!({
                "intent_id":state.app.request_role_switch(&operation_id,&attempt_id,&role,&old_generation_id,settings_revision,&snapshot_id,handoff,expected_task_version)?,"state":"stopping_old"})),
            WebOperation::SwitchRoleFinish { intent_id } => {
                state.app.roles.finish_switch(&intent_id)
            }
            WebOperation::RoleResume {
                operation_id,
                session_id,
                prompt,
            } => {
                let result = state
                    .app
                    .resume_role_session_with_operation(&operation_id, &session_id, &prompt)?;
                checked_launch_result(
                    &result,
                    &operation_id,
                    None,
                    result.session_id == session_id,
                )
            }
            WebOperation::RestartResume {
                operation_id,
                session_ids,
            } => state
                .app
                .resume_restart_sessions(&operation_id, session_ids.as_deref()),
            WebOperation::CheckRun {
                operation_id,
                attempt_id,
                suite_name,
                check_id,
            } => match (check_id, suite_name) {
                (Some(check), None) => {
                    state
                        .app
                        .checks
                        .run_selected_browser(&operation_id, &attempt_id, &check)
                }
                (None, Some(suite)) => state.app.checks.run_configured(&attempt_id, &suite),
                _ => bail!(
                    "check run requires exactly one selected check_id; suite_name is history-only"
                ),
            },
            WebOperation::LegacyPreview { source } => {
                Ok(serde_json::to_value(crate::import::preview(&source)?)?)
            }
            WebOperation::LegacyImport {
                operation_id,
                project_id,
                expected_project_version,
                source,
                expected_source_hash,
            } => crate::import::apply(
                &state.app.store,
                &operation_id,
                &project_id,
                expected_project_version,
                &source,
                &expected_source_hash,
            ),
            WebOperation::Transcript {
                session_id,
                after_epoch,
                after_sequence,
                limit_bytes,
            } => {
                state.app.store.session_json(&session_id)?;
                Ok(serde_json::to_value(crate::transcript::read_frames(
                    &state.app.paths.transcripts,
                    &session_id,
                    after_epoch.as_deref(),
                    after_sequence,
                    limit_bytes.min(1024 * 1024),
                )?)?)
            }
            WebOperation::CmuxView {
                operation_id,
                session_id,
            } => Ok(serde_json::to_value(crate::cmux::view(
                &state.app,
                &state.boot_id,
                &operation_id,
                &session_id,
            )?)?),
            WebOperation::CmuxSetKeyboardControl {
                operation_id,
                session_id,
                surface_route_id,
                expected_binding_revision,
                expected_control_revision,
                action,
            } => Ok(serde_json::to_value(crate::cmux::set_keyboard_control(
                &state.app,
                &state.boot_id,
                &operation_id,
                &session_id,
                &surface_route_id,
                expected_binding_revision,
                expected_control_revision,
                action,
            )?)?),
            WebOperation::CmuxDiscardUnknown {
                operation_id,
                surface_route_id,
                session_id,
            } => Ok(serde_json::to_value(crate::cmux::discard_unknown_surface(
                &state.app,
                &state.boot_id,
                &operation_id,
                &surface_route_id,
                &session_id,
            )?)?),
            WebOperation::Interrupt { session_id } => {
                state.app.supervisor.interrupt(&session_id)?;
                Ok(serde_json::json!({"interrupt_requested":true}))
            }
            WebOperation::CapabilityRecordProof { proof } => {
                state.app.store.record_capability_proof(&proof)
            }
            WebOperation::CapabilityRecordFunctionalProof { proof } => {
                state.app.record_functional_main_checkout_proof(&proof)
            }
            WebOperation::HistoryPage {
                session_id,
                transcript_epoch,
                after_sequence,
                limit_bytes,
            } => {
                state.app.store.session_json(&session_id)?;
                Ok(serde_json::to_value(crate::transcript::read_history_page(
                    &state.app.paths.transcripts,
                    &session_id,
                    &transcript_epoch,
                    after_sequence,
                    limit_bytes,
                )?)?)
            }
            WebOperation::HistoryExportText {
                session_id,
                transcript_epoch,
            } => {
                state.app.store.session_json(&session_id)?;
                let export = crate::transcript::write_history_export(
                    &state.app.paths.transcripts,
                    &state.app.paths.artifacts,
                    &session_id,
                    &transcript_epoch,
                )?;
                Ok(serde_json::json!({
                    "session_id": session_id,
                    "transcript_epoch": transcript_epoch,
                    "export_hash": export.export_hash,
                    "high_water_sequence": export.high_water_sequence,
                    "retention": export.retention,
                    "text": export.text,
                }))
            }
            WebOperation::HistoryPaneOpen {
                operation_id,
                session_id,
                transcript_epoch,
                acknowledged_unknown_view_id,
            } => Ok(serde_json::to_value(crate::cmux::open_history_pane(
                &state.app,
                &state.boot_id,
                &crate::store::HistoryPaneRequest {
                    operation_id: &operation_id,
                    session_id: &session_id,
                    transcript_epoch: &transcript_epoch,
                    acknowledged_unknown_view_id: acknowledged_unknown_view_id.as_deref(),
                },
            )?)?),
            WebOperation::HistoryPaneViews {
                session_id,
                transcript_epoch,
            } => Ok(serde_json::to_value(
                state
                    .app
                    .store
                    .history_pane_views(&session_id, &transcript_epoch)?,
            )?),
            WebOperation::ProviderValidationPreview { selections } => Ok(serde_json::to_value(
                state.app.preview_provider_validation(
                    &crate::domain::ProviderValidationPreviewRequest { selections },
                )?,
            )?),
            WebOperation::ProviderValidationStart {
                operation_id,
                preview_hash,
                selections,
                authorize_model_calls,
                max_model_calls,
            } => Ok(serde_json::to_value(state.app.start_provider_validation(
                &crate::domain::ProviderValidationStartRequest {
                    operation_id,
                    preview_hash,
                    selections,
                    authorize_model_calls,
                    max_model_calls,
                },
            )?)?),
            WebOperation::ProviderValidationReconcile {
                operation_id,
                preview_hash,
                selections,
                authorize_model_calls,
                max_model_calls,
            } => Ok(serde_json::to_value(
                state.app.reconcile_provider_validation_start(
                    &crate::domain::ProviderValidationStartRequest {
                        operation_id,
                        preview_hash,
                        selections,
                        authorize_model_calls,
                        max_model_calls,
                    },
                )?,
            )?),
            WebOperation::ProviderValidationInspect { batch_id } => Ok(serde_json::to_value(
                state.app.inspect_provider_validation(&batch_id)?,
            )?),
            WebOperation::ProviderValidationCancel { batch_id } => Ok(serde_json::to_value(
                state.app.cancel_provider_validation(&batch_id)?,
            )?),
            WebOperation::Workflow { command } => Ok(serde_json::to_value(
                state.app.execute_workflow_command(&command)?,
            )?),
        }
    })
    .await;
    web_operation_response(&state, checked_operation_id.as_deref(), result)
}

fn checked_launch_result(
    result: &crate::domain::ValidationLaunchResult,
    operation_id: &str,
    admission_id: Option<&str>,
    target_matches: bool,
) -> Result<serde_json::Value> {
    if !target_matches {
        return Err(crate::trip::outcome_unknown(anyhow!(
            "The launch result does not match the requested target"
        )));
    }
    // The operation owner verifies the request before returning a successful
    // launch or replay. These fields belong to HTTP, outside the stored result.
    #[derive(Serialize)]
    struct BoundLaunch<'a> {
        #[serde(flatten)]
        launch: &'a crate::domain::ValidationLaunchResult,
        operation_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        admission_id: Option<&'a str>,
    }
    serde_json::to_value(BoundLaunch {
        launch: result,
        operation_id,
        admission_id,
    })
    .map_err(|error| crate::trip::outcome_unknown(error.into()))
}

fn web_operation_response(
    state: &WebState,
    checked_operation_id: Option<&str>,
    result: std::result::Result<Result<serde_json::Value>, tokio::task::JoinError>,
) -> Response {
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let message = format!("The request may already have been applied. Refresh and check before retrying. Operation worker interrupted: {error}");
            if let Some(id) = checked_operation_id {
                return checked_operation_error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    id,
                    true,
                    false,
                    &message,
                );
            }
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": message,
                    "outcome_unknown": true,
                })),
            )
                .into_response();
        }
    };
    match result {
        Ok(value) => Json(serde_json::json!({"result":value})).into_response(),
        Err(error) => {
            let _ = state.app.diagnostics.record(
                "warn",
                "web.operation.rejected",
                "server",
                "rejected",
                None,
                serde_json::json!({"cause":format!("{error:#}")}),
            );
            if let Some(id) = checked_operation_id {
                let refusal = error.downcast_ref::<crate::operations::CheckedOperationRefused>();
                let refused = refusal.is_some();
                let message = if refused || error.is::<crate::trip::TripOutcomeUnknown>() {
                    format!("{error:#}")
                } else {
                    format!("The request outcome could not be confirmed. Refresh and check before retrying. Technical details: {error:#}")
                };
                return checked_operation_error_response(
                    StatusCode::CONFLICT,
                    id,
                    !refused,
                    refusal.is_some_and(|refusal| refusal.confirms_terminal_refusal(id)),
                    &message,
                );
            }
            let mut body = serde_json::json!({"error":format!("{error:#}")});
            if error.is::<crate::trip::TripOutcomeUnknown>() {
                body["outcome_unknown"] = serde_json::Value::Bool(true);
            }
            (StatusCode::CONFLICT, Json(body)).into_response()
        }
    }
}

fn checked_operation_error_response(
    status: StatusCode,
    operation_id: &str,
    outcome_unknown: bool,
    terminal_refusal: bool,
    message: &str,
) -> Response {
    let mut body = serde_json::json!({
        "error":message, "operation_id":operation_id, "outcome_unknown":outcome_unknown
    });
    if terminal_refusal {
        body["terminal_refusal"] = serde_json::Value::Bool(true);
    }
    (status, Json(body)).into_response()
}

fn require_browser(state: &WebState, headers: &HeaderMap) -> std::result::Result<(), Response> {
    check_origin(state, headers, true)?;
    if !has_browser_session(state, headers) {
        return Err((StatusCode::UNAUTHORIZED, "browser session required").into_response());
    }
    Ok(())
}

async fn api_protocol(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    Json(protocol::Descriptor::new(
        state.instance_id,
        ClientKind::Browser,
    ))
    .into_response()
}

async fn guard_operational_protocol(
    State(state): State<WebState>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(response) = require_operational_protocol(&state, request.headers()) {
        return response;
    }
    next.run(request).await
}

fn require_operational_protocol(
    state: &WebState,
    headers: &HeaderMap,
) -> std::result::Result<(), Response> {
    require_browser(state, headers)?;
    let declarations = headers.get_all(protocol::HTTP_HEADER);
    let mut values = declarations.iter();
    let result = match (values.next(), values.next()) {
        (None, _) => Err(protocol::ProtocolError::new("missing", None)),
        (Some(_), Some(_)) => Err(protocol::ProtocolError::new("malformed", None)),
        (Some(value), None) => {
            protocol::parse_declaration(value.as_bytes(), ClientKind::Browser).map(|_| ())
        }
    };
    if let Err(error) = result {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.guidance,"protocol_error":error})),
        )
            .into_response());
    }
    Ok(())
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}

fn check_origin(
    state: &WebState,
    headers: &HeaderMap,
    require_origin_if_present: bool,
) -> std::result::Result<(), Response> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !host.is_some_and(|host| state.allowed_hosts.iter().any(|allowed| allowed == host)) {
        return Err((StatusCode::FORBIDDEN, "invalid Host header").into_response());
    }
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        let valid = state
            .allowed_hosts
            .iter()
            .any(|host| origin == format!("http://{host}"));
        if !valid {
            return Err((StatusCode::FORBIDDEN, "invalid Origin header").into_response());
        }
    } else if require_origin_if_present
        && headers.contains_key(HeaderName::from_static("sec-fetch-site"))
    {
        let site = headers
            .get(HeaderName::from_static("sec-fetch-site"))
            .and_then(|value| value.to_str().ok());
        if !matches!(site, Some("same-origin") | Some("none")) {
            return Err((StatusCode::FORBIDDEN, "cross-site request rejected").into_response());
        }
    }
    Ok(())
}

fn has_browser_session(state: &WebState, headers: &HeaderMap) -> bool {
    let Some(value) = instance_session_cookie(&state.session_cookie, headers) else {
        return false;
    };
    let hash = hex::encode(Sha256::digest(value.as_bytes()));
    state
        .browser_sessions
        .lock()
        .map(|sessions| sessions.contains(&hash))
        .unwrap_or(false)
}

/// The one value of this instance's cookie. An unreadable Cookie header or a
/// repeated or malformed instance cookie authenticates nothing, because the
/// browser's choice among them cannot be known.
fn instance_session_cookie<'a>(name: &str, headers: &'a HeaderMap) -> Option<&'a str> {
    let mut found = None;
    for header in headers.get_all(header::COOKIE) {
        for pair in header.to_str().ok()?.split(';') {
            let Some((cookie, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if cookie != name {
                continue;
            }
            if found.is_some() || !is_cookie_value(value) {
                return None;
            }
            found = Some(value);
        }
    }
    found
}

/// RFC 6265 cookie-octets without quoting, within a fixed bound.
fn is_cookie_value(value: &str) -> bool {
    let cookie_octet =
        |byte: u8| matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e);
    (1..=MAX_SESSION_COOKIE_VALUE_BYTES).contains(&value.len()) && value.bytes().all(cookie_octet)
}

fn append_startup_event(paths: &InstancePaths, record: &InstanceRecord) -> Result<()> {
    use std::io::Write;
    let path = paths.logs.join("agenticjira.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let event = serde_json::json!({
        "schema": 1, "timestamp": chrono::Utc::now().to_rfc3339(), "severity": "info",
        "event_code": "service.started", "component": "server", "outcome": "success",
        "instance_id": record.instance_id.clone(), "boot_id": record.boot_id.clone(), "version": crate::VERSION,
        "data_dir": paths.root.to_string_lossy(), "address": record.address.clone()
    });
    writeln!(file, "{}", event)?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod web_operation_tests {
    use super::*;
    use crate::config::InstancePaths;
    use crate::domain::{AttachmentBinding, ProcessIdentity};
    use crate::store::Store;
    use axum::http::HeaderValue;
    use rusqlite::params;

    #[test]
    fn functional_main_proof_web_transport_keeps_the_contract_explicit() {
        let request = serde_json::json!({
            "kind": "capability_record_functional_proof",
            "proof": {
                "qualification_contract": "functional-v2",
                "operation_id": "publish",
                "session_id": "session",
                "evidence_reference": "native-reference",
                "main": {
                    "observation_request_hash": "request",
                    "native_command": "/bin/cat -- '/owned/challenge'",
                    "native_session_id": "native",
                    "role_generation_id": "generation",
                    "observation": {
                        "status": "passed",
                        "binding_hash": "binding",
                        "challenge": "nonce",
                        "session_mode": "retained"
                    }
                }
            }
        });
        let parsed: WebOperation = serde_json::from_value(request.clone()).unwrap();
        let WebOperation::CapabilityRecordFunctionalProof { proof } = parsed else {
            panic!("functional proof must retain its dedicated web operation");
        };
        assert_eq!(proof.operation_id, "publish");
        assert_eq!(serde_json::to_value(proof).unwrap(), request["proof"]);

        let mut wrong = request.clone();
        wrong["kind"] = "capability_record_proof".into();
        assert!(serde_json::from_value::<WebOperation>(wrong).is_err());
        let mut unknown = request;
        unknown["proof"]["qualification_contract"] = "functional-v3".into();
        assert!(serde_json::from_value::<WebOperation>(unknown).is_err());

        let mut legacy = serde_json::json!({"kind":"capability_record_proof","proof":{
            "operation_id":"legacy","session_id":"session","evidence_reference":"native-reference",
            "direct_write_denied":true,"compound_denied":true,"redirect_denied":true,
            "sentinel_relative_path":"sentinel","native_resume_session_id":"native","history_nonce":"nonce"}});
        let parsed: WebOperation = serde_json::from_value(legacy.clone()).unwrap();
        let WebOperation::CapabilityRecordProof { proof } = parsed else {
            panic!("legacy proof must retain its original web operation");
        };
        assert_eq!(proof.operation_id, "legacy");
        legacy["proof"]["qualification_contract"] = "functional-v2".into();
        assert!(serde_json::from_value::<WebOperation>(legacy.clone()).is_err());
        let mut wrong = legacy.clone();
        wrong["kind"] = "capability_record_functional_proof".into();
        assert!(serde_json::from_value::<WebOperation>(wrong).is_err());
        legacy["proof"]["qualification_contract"] = "functional-v3".into();
        assert!(serde_json::from_value::<WebOperation>(legacy).is_err());
    }

    #[test]
    fn scheduled_backup_admission_is_single_flight_and_restart_waits_a_future_interval() {
        let boot = Instant::now();
        let interval = Duration::from_secs(3600);
        let due = boot + interval;
        assert!(!backup_due(boot, due, false, false, false));
        assert!(!backup_due(
            due - Duration::from_nanos(1),
            due,
            false,
            false,
            false
        ));
        let late = boot + interval * 12;
        assert!(backup_due(late, due, false, false, false));
        for (in_flight, draining, held) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(!backup_due(late, due, in_flight, draining, held));
        }
        assert!(
            !backup_due(late, late + interval, false, false, false),
            "restart recreates a future-only schedule"
        );
    }

    #[tokio::test]
    async fn scheduled_backup_failed_attempt_waits_from_completion_without_catch_up() {
        let (root, app, _, _) = server_test_application();
        let boot = Instant::now();
        let interval = Duration::from_secs(3600);
        let finished = boot + interval * 12;
        let mut backup = ScheduledBackup {
            interval,
            destination: root.with_extension("backups"),
            next_due: boot + interval,
            worker: Some(tokio::spawn(async move {
                (finished, Err(anyhow!("storage unavailable")))
            })),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        backup.reap(&app).await;
        assert!(!backup_due(
            finished,
            backup.next_due,
            backup.worker.is_some(),
            false,
            false
        ));
        assert!(!backup_due(
            finished + interval - Duration::from_nanos(1),
            backup.next_due,
            false,
            false,
            false
        ));
        assert!(backup_due(
            finished + interval,
            backup.next_due,
            false,
            false,
            false
        ));
        let events = app.diagnostics.read_sanitized(100).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["event_code"] == "backup.finished")
                .count(),
            1
        );
        assert!(!backup.destination.exists());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn scheduled_backup_shutdown_joins_actual_worker_before_releasing_offline_ownership() {
        let (root, app, _, _) = server_test_application();
        let destination = root.with_extension("backups");
        let lock = Arc::new(InstanceLock::acquire(&app.paths).unwrap());
        let worker_lock = lock.clone();
        let worker_app = app.clone();
        let worker_destination = destination.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut control = OnlineBackupControl::new(cancelled.clone());
        let pinned = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let hook_pinned = pinned.clone();
        let hook_release = release.clone();
        let first = AtomicBool::new(true);
        control.copy_hook = Some(Arc::new(move || {
            if first.swap(false, AtomicOrdering::SeqCst) {
                hook_pinned.wait();
                hook_release.wait();
            }
        }));
        let worker = tokio::task::spawn_blocking(move || {
            let result =
                run_scheduled_backup(&worker_app, worker_lock, &worker_destination, &control);
            (Instant::now(), result)
        });
        pinned.wait();
        let mut backup = ScheduledBackup {
            interval: Duration::from_secs(3600),
            destination: destination.clone(),
            next_due: Instant::now(),
            worker: Some(worker),
            cancelled: cancelled.clone(),
        };
        backup
            .tick(&app, &lock, Instant::now() + Duration::from_secs(24 * 3600))
            .await;
        assert_eq!(
            app.diagnostics
                .read_sanitized(100)
                .unwrap()
                .iter()
                .filter(|event| event["event_code"] == "backup.started")
                .count(),
            1,
            "late ticks must not launch a second actual worker"
        );
        cancelled.store(true, AtomicOrdering::SeqCst);
        backup
            .tick(&app, &lock, Instant::now() + Duration::from_secs(24 * 3600))
            .await;
        drop(lock);
        assert!(crate::database::backup(&app.paths, Some(&destination))
            .unwrap_err()
            .to_string()
            .contains("another LLMRelay instance"));
        assert!(crate::database::restore(&app.paths, &destination)
            .unwrap_err()
            .to_string()
            .contains("another LLMRelay instance"));
        let finish_app = app.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let finish = tokio::spawn(async move {
            entered_tx.send(()).unwrap();
            backup.finish(&finish_app).await;
        });
        entered_rx.await.unwrap();
        assert!(
            !finish.is_finished(),
            "shutdown must await the paused blocking worker"
        );
        assert!(!destination.exists());
        release.wait();
        finish.await.unwrap();
        drop(InstanceLock::acquire(&app.paths).unwrap());
        let events: Vec<_> = app
            .diagnostics
            .read_sanitized(100)
            .unwrap()
            .into_iter()
            .filter(|event| event["component"] == "database_backup")
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event_code"], "backup.started");
        assert_eq!(events[1]["outcome"], "unpublished");
        assert!(events[1]["detail"]["cause"]
            .as_str()
            .unwrap()
            .contains("cancelled"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn scheduled_backup_restore_hold_and_draining_have_no_copy_effects() {
        let (root, app, _, _) = server_test_application();
        let lock = Arc::new(InstanceLock::acquire(&app.paths).unwrap());
        let destination = root.with_extension("backups");
        let mut backup = ScheduledBackup {
            interval: Duration::from_secs(3600),
            destination: destination.clone(),
            next_due: Instant::now(),
            worker: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        app.store.lock().unwrap().execute_batch(
            "INSERT INTO recovery_records(id,state,detail_json,created_at,updated_at)
             VALUES('database-restore-hold','attention_required','{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
        backup.tick(&app, &lock, Instant::now()).await;
        assert!(backup.worker.is_none());
        assert!(!destination.exists());
        let control = OnlineBackupControl::new(backup.cancelled.clone());
        assert!(
            run_scheduled_backup(&app, lock.clone(), &destination, &control)
                .err()
                .unwrap()
                .to_string()
                .contains("suppressed")
        );
        assert!(!destination.exists());
        app.store.lock().unwrap().execute_batch(
            "DELETE FROM recovery_records WHERE id='database-restore-hold'; DELETE FROM sessions;"
        ).unwrap();
        app.begin_drain().unwrap();
        assert!(app.is_draining());
        backup.tick(&app, &lock, Instant::now()).await;
        assert!(backup.worker.is_none());
        assert!(run_scheduled_backup(&app, lock.clone(), &destination, &control).is_err());
        assert!(!destination.exists());
        drop(lock);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn serve_rejects_backup_overlap_before_creating_instance_or_opening_database() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-startup-backup-{}", uuid::Uuid::new_v4()));
        let paths = InstancePaths::resolve(Some(root)).unwrap();
        let options = BackupOptions {
            every_hours: Some(1),
            directory: Some(paths.root.join("backups")),
        };
        let error = serve(paths.clone(), 0, false, options).await.unwrap_err();
        assert!(error.to_string().contains("overlap"));
        assert!(!paths.root.exists());
    }

    #[test]
    fn online_backup_rechecks_repository_privacy_and_instance_lock_identity() {
        use std::os::unix::fs::PermissionsExt;
        let (root, app, _, _) = server_test_application();
        let lock = Arc::new(InstanceLock::acquire(&app.paths).unwrap());
        let destination = root.with_extension("backups");
        crate::database::validate_online_backup_destination(&app.paths, &lock, &destination)
            .unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(crate::database::backup_online(
            &app.paths,
            lock.clone(),
            &destination,
            &OnlineBackupControl::new(Arc::new(AtomicBool::new(false)))
        )
        .is_err());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700)).unwrap();
        app.store
            .lock()
            .unwrap()
            .execute(
                "UPDATE projects SET repository_path=?1 WHERE id='project'",
                [destination.to_str().unwrap()],
            )
            .unwrap();
        let error =
            crate::database::validate_online_backup_destination(&app.paths, &lock, &destination)
                .unwrap_err();
        assert!(error.to_string().contains("registered repository"));
        let other_paths = InstancePaths::resolve(Some(root.with_extension("other"))).unwrap();
        other_paths.create().unwrap();
        let other_lock = Arc::new(InstanceLock::acquire(&other_paths).unwrap());
        let error = crate::database::backup_online(
            &app.paths,
            other_lock.clone(),
            &destination,
            &OnlineBackupControl::new(Arc::new(AtomicBool::new(false))),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("instance lock"));
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        drop(other_lock);
        drop(lock);
        drop(app);
        std::fs::remove_dir_all(other_paths.root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(destination).unwrap();
    }

    fn server_test_application() -> (
        std::path::PathBuf,
        Application,
        AttachmentBinding,
        AttachmentBinding,
    ) {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-server-cmux-boundary-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        let connection = store.lock().unwrap();
        connection.execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('project','project','/tmp/project','server-boundary-project','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('task','project','task','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('attempt','task','context','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        )
        .unwrap();
        drop(connection);
        let app = Application::new(paths, store, std::env::current_exe().unwrap()).unwrap();
        let target = seed_binding(&app, "manager", 1, "target");
        let other = seed_binding(&app, "explorer", 2, "other");
        (root, app, target, other)
    }

    fn server_test_repository(
        root: &std::path::Path,
        files: usize,
    ) -> crate::workspace::RepositoryInfo {
        let path = root.join("fixture-repository");
        std::fs::create_dir(&path).unwrap();
        for index in 0..files {
            std::fs::write(
                path.join(format!("file-{index}")),
                "Exact source evidence\n",
            )
            .unwrap();
        }
        for arguments in [
            "init -q",
            "add -- .",
            "-c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm fixture",
        ] {
            let mut command = std::process::Command::new("git");
            command.current_dir(&path).args(arguments.split_whitespace());
            for variable in [
                "GIT_AUTHOR_NAME",
                "GIT_AUTHOR_EMAIL",
                "GIT_COMMITTER_NAME",
                "GIT_COMMITTER_EMAIL",
            ] {
                command.env_remove(variable);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        crate::workspace::inspect(&path).unwrap()
    }

    fn seed_binding(
        app: &Application,
        role: &str,
        generation: i64,
        suffix: &str,
    ) -> AttachmentBinding {
        let binding = AttachmentBinding {
            session_id: uuid::Uuid::new_v4().to_string(),
            role_generation_id: uuid::Uuid::new_v4().to_string(),
            transcript_epoch: uuid::Uuid::new_v4().to_string(),
            process: ProcessIdentity {
                pid: 9000 + generation as u32,
                process_group_id: 9000 + generation as i32,
                native_start_marker: format!("server-boundary-{suffix}"),
                observed_started_at: "2026-01-01T00:00:00Z".to_owned(),
            },
        };
        let connection = app.store.lock().unwrap();
        connection
            .execute(
                "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                 VALUES(?1,'attempt',?2,'codex',?3,1,'running',?4,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![
                    binding.role_generation_id,
                    role,
                    generation,
                    format!("server-boundary-authority-{suffix}"),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,
                                      process_identity_json,transcript_epoch,created_at,updated_at)
                 VALUES(?1,?2,'codex','running','{}','fixture',?3,?4,
                        '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![
                    binding.session_id,
                    binding.role_generation_id,
                    serde_json::to_string(&binding.process).unwrap(),
                    binding.transcript_epoch,
                ],
            )
            .unwrap();
        binding
    }

    fn open_persistent_surface(
        app: &Application,
        service_boot_id: &str,
        binding: &AttachmentBinding,
    ) -> crate::domain::CmuxSessionSurface {
        let (workspace, created) = app
            .store
            .reserve_cmux_task_workspace(service_boot_id, "task")
            .unwrap();
        assert!(created);
        let (reserved, created) = app
            .store
            .reserve_cmux_session_surface(service_boot_id, &workspace.id, binding)
            .unwrap();
        assert!(created);
        assert!(app
            .store
            .claim_cmux_task_workspace_create(&workspace.id, &reserved.id, service_boot_id)
            .unwrap());
        let opened = app
            .store
            .mark_cmux_workspace_and_initial_surface_open(
                &workspace.id,
                &reserved.id,
                service_boot_id,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        app.store
            .mark_cmux_session_surface_connected(
                &opened.id,
                service_boot_id,
                binding,
                opened.binding_revision,
            )
            .unwrap()
    }

    fn browser_state(app: Application) -> (WebState, HeaderMap, watch::Sender<bool>) {
        let browser_secret = "server-boundary-browser-session";
        let browser_hash = hex::encode(Sha256::digest(browser_secret.as_bytes()));
        let (shutdown_tx, shutdown) = watch::channel(false);
        let state = WebState {
            boot_id: app.service_boot_id().to_owned(),
            session_cookie: browser_session_cookie_name(&app.paths.root),
            app,
            instance_id: "server-boundary-instance".to_owned(),
            allowed_hosts: vec!["server-boundary.test".to_owned()],
            browser_sessions: Arc::new(Mutex::new(HashSet::from([browser_hash]))),
            shutdown,
            state_waiters: Arc::new(Semaphore::new(MAX_STATE_WAITERS)),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static("server-boundary.test"),
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{}={browser_secret}", state.session_cookie)).unwrap(),
        );
        (state, headers, shutdown_tx)
    }

    async fn knowledge_response_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(
            response.into_body(),
            crate::knowledge::PREVIEW_RESPONSE_BYTES,
        )
        .await
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn checked_project_setup_launch_replays_historical_receipts_with_http_bindings() {
        use crate::domain::{CapabilityStatus, LaunchConfig, Provider, ValidationLaunchResult};
        use crate::store::{json_hash, reserve_browser_launch_receipt_in, BrowserLaunchReceipt};
        use serde_json::json;

        let (root, app, setup_session, _) = server_test_application();
        let ordinary_session = seed_binding(&app, "manager", 3, "ordinary-resume");
        let probe_a = seed_binding(&app, "manager", 4, "runtime-probe-a");
        let probe_b = seed_binding(&app, "manager", 5, "runtime-probe-b");
        let pending_probe = seed_binding(&app, "manager", 6, "runtime-pending");
        let legacy_probe = seed_binding(&app, "manager", 7, "runtime-legacy");
        let (state, headers, _shutdown) = browser_state(app);
        let launch = LaunchConfig {
            provider: Provider::Codex,
            role: RoleKind::Manager,
            executable: "/usr/bin/true".into(),
            executable_version: "fixture".into(),
            model: "fixture".into(),
            effort: "high".into(),
            cwd: root.clone(),
            argv: vec![],
            environment_keys: vec![],
            permission_policy: "fixture".into(),
            security_policy: json!({}),
            hook_revision: "fixture".into(),
            capability_status: CapabilityStatus::Supported,
            compatibility: None,
        };
        let send = |request: serde_json::Value, headers: HeaderMap| {
            api_operation(
                State(state.clone()),
                headers,
                Json(serde_json::from_value(request).unwrap()),
            )
        };
        let reserve = |operation_id: &str, kind: &str, target: &str, hash: &str, authority| {
            let mut connection = state.app.store.lock().unwrap();
            let tx = connection.transaction().unwrap();
            reserve_browser_launch_receipt_in(
                &tx,
                BrowserLaunchReceipt {
                    operation_id,
                    operation_kind: kind,
                    input_hash: hash,
                    entity_id: target,
                },
                authority,
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
            tx.commit().unwrap();
        };
        let runtime_authority = |session: &AttachmentBinding| {
            json!({
                "attempt_id":"attempt", "task_id":"task", "role":"manager",
                "settings_revision":1, "permit_id":format!("permit-{}", session.session_id),
                "setup_permit_id":format!("setup-permit-{}", session.session_id),
                "validation_dispatch":true, "validation_cell":"trip_runtime_probe",
                "switch_intent_id":null, "lane_id":"default", "phase":"planning",
                "attempt_status":"running", "lifecycle":"validation", "attention":"none",
                "capability_key":"fixture", "launch":launch,
            })
        };
        // Use the production receipt writers to exercise replay with no provider.
        for (kind, operation_id, target, admission, mut input, status, session) in [
            (
                "trip_setup_dispatch",
                "dispatch-op",
                "attempt",
                None,
                json!({"attempt_id":"attempt","role":"manager","fresh_resume_rejection":null}),
                "running",
                &setup_session,
            ),
            (
                "role_resume",
                "setup-resume-op",
                setup_session.session_id.as_str(),
                None,
                json!({"session_id":setup_session.session_id,"prompt":""}),
                "running_resumed_unverified",
                &setup_session,
            ),
            (
                "role_resume",
                "ordinary-resume-op",
                ordinary_session.session_id.as_str(),
                None,
                json!({"session_id":ordinary_session.session_id,"prompt":"retained prompt"}),
                "running_resumed",
                &ordinary_session,
            ),
            (
                "runtime_probe_launch",
                "runtime-launch-a",
                "admission-a",
                Some("admission-a"),
                json!({"admission_id":"admission-a","role":"manager"}),
                "running",
                &probe_a,
            ),
            (
                "runtime_probe_launch",
                "runtime-launch-b",
                "admission-b",
                Some("admission-b"),
                json!({"admission_id":"admission-b","role":"manager"}),
                "running",
                &probe_b,
            ),
            (
                "runtime_probe_resume",
                "runtime-resume-a",
                "admission-a",
                Some("admission-a"),
                json!({"admission_id":"admission-a","role":"manager"}),
                "running_resumed_unverified",
                &probe_a,
            ),
            (
                "runtime_probe_resume",
                "runtime-resume-b",
                "admission-b",
                Some("admission-b"),
                json!({"admission_id":"admission-b","role":"manager"}),
                "running_resumed_unverified",
                &probe_b,
            ),
        ] {
            let hash = json_hash(&input).unwrap();
            let historical = serde_json::to_value(ValidationLaunchResult {
                session_id: session.session_id.clone(),
                task_id: "task".into(),
                attempt_id: "attempt".into(),
                role_generation_id: session.role_generation_id.clone(),
                launch: launch.clone(),
                process: session.process.clone(),
                status: status.into(),
            })
            .unwrap();
            let authority = if kind == "runtime_probe_launch" {
                runtime_authority(session)
            } else {
                json!({"session_id":session.session_id,"role_generation_id":session.role_generation_id})
            };
            reserve(operation_id, kind, target, &hash, authority);
            state
                .app
                .store
                .finalize_browser_launch_receipt(
                    operation_id,
                    kind,
                    &hash,
                    Some(historical.clone()),
                    None,
                )
                .unwrap();
            input["kind"] = kind.into();
            input["operation_id"] = operation_id.into();
            let mut anonymous = headers.clone();
            anonymous.remove(header::COOKIE);
            assert_eq!(
                send(input.clone(), anonymous).await.status(),
                StatusCode::UNAUTHORIZED
            );
            for _ in 0..2 {
                let response = send(input.clone(), headers.clone()).await;
                assert_eq!(response.status(), StatusCode::OK);
                let mut result = knowledge_response_json(response).await["result"].clone();
                assert_eq!(
                    result.as_object_mut().unwrap().remove("operation_id"),
                    Some(json!(operation_id))
                );
                assert_eq!(
                    result.as_object_mut().unwrap().remove("admission_id"),
                    admission.map(|id| json!(id))
                );
                assert_eq!(result, historical);
            }
            if kind == "runtime_probe_launch" {
                let mut changed_role = input.clone();
                changed_role["role"] = "explorer".into();
                let response = send(changed_role, headers.clone()).await;
                assert_eq!(response.status(), StatusCode::CONFLICT);
                let refusal = knowledge_response_json(response).await;
                assert!(refusal["error"]
                    .as_str()
                    .unwrap()
                    .contains("different input"));
                assert_eq!(refusal["operation_id"], operation_id);
                assert_eq!(refusal["outcome_unknown"], true);
            }
            let field = match kind {
                "trip_setup_dispatch" => "attempt_id",
                "role_resume" => "session_id",
                _ => "admission_id",
            };
            input[field] = match (kind, target) {
                ("runtime_probe_launch", "admission-a") => "admission-b",
                ("runtime_probe_launch", "admission-b") => "admission-a",
                _ => "different-target",
            }
            .into();
            let response = send(input, headers.clone()).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let refusal = knowledge_response_json(response).await;
            assert!(refusal["error"]
                .as_str()
                .unwrap()
                .contains("different input"));
            assert!(refusal.get("result").is_none());
            assert_eq!(refusal["operation_id"], operation_id);
            assert_eq!(
                state
                    .app
                    .store
                    .browser_launch_receipt(operation_id, kind, &hash)
                    .unwrap(),
                Some(historical)
            );
        }

        let pending_input = json!({"admission_id":"admission-pending","role":"manager"});
        let pending_hash = json_hash(&pending_input).unwrap();
        reserve(
            "runtime-pending",
            "runtime_probe_launch",
            "admission-pending",
            &pending_hash,
            runtime_authority(&pending_probe),
        );
        let mut pending = pending_input;
        pending["kind"] = "runtime_probe_launch".into();
        pending["operation_id"] = "runtime-pending".into();
        let (first, second) = tokio::join!(
            send(pending.clone(), headers.clone()),
            send(pending.clone(), headers.clone()),
        );
        for response in [first, second] {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = knowledge_response_json(response).await;
            assert_eq!(body["operation_id"], "runtime-pending");
            assert_eq!(body["outcome_unknown"], true);
            assert!(body.get("terminal_refusal").is_none());
            assert!(body.get("result").is_none());
        }
        pending["role"] = "explorer".into();
        let response = send(pending, headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let refusal = knowledge_response_json(response).await;
        assert!(refusal["error"]
            .as_str()
            .unwrap()
            .contains("different input"));
        assert_eq!(refusal["operation_id"], "runtime-pending");
        assert_eq!(refusal["outcome_unknown"], true);
        let reopened = Store::open(&state.app.paths.database).unwrap();
        let pending_receipt = reopened
            .browser_launch_receipt("runtime-pending", "runtime_probe_launch", &pending_hash)
            .unwrap()
            .unwrap();
        assert_eq!(pending_receipt["state"], "reserved");
        let restarted = Application::new(
            state.app.paths.clone(),
            reopened,
            std::env::current_exe().unwrap(),
        )
        .unwrap();
        assert!(restarted
            .dispatch_runtime_probe("runtime-pending", "admission-pending", RoleKind::Manager)
            .unwrap_err()
            .is::<crate::trip::TripOutcomeUnknown>());
        let replayed = restarted
            .dispatch_runtime_probe("runtime-launch-a", "admission-a", RoleKind::Manager)
            .unwrap();
        assert_eq!(replayed.session_id, probe_a.session_id);

        {
            let connection = state.app.store.lock().unwrap();
            connection.execute(
                "UPDATE sessions SET launch_state='started', validation_cell='trip_runtime_probe'
                 WHERE id=?1",
                [&legacy_probe.session_id],
            ).unwrap();
            connection.execute_batch(
                "INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,
                   preflight_json,verification_json,source_hash,overlay_hash,configuration_hash,created_at)
                 VALUES('legacy-config','project',1,'active','{}','{}','{}','{}','source','overlay','config','now');
                 INSERT INTO trip_runtime_admissions(id,project_id,task_id,scope_hash,state,fresh_call_count,created_at,updated_at)
                 VALUES('admission-legacy','project','task','scope','authorized',1,'now','now');",
            ).unwrap();
            connection.execute(
                "INSERT INTO trip_runtime_probes(admission_id,role,settings_revision,launch_config_json,
                   profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,
                   adapter_hash,capability_key,capability_identity_json,fixture_project_id,fixture_repository_identity,
                   fixture_root,attempt_id,nonce,state,session_id,created_at,updated_at)
                 VALUES('admission-legacy','manager',1,?1,'{}','profile','legacy-config','config','codex',
                   'adapter','fixture','{}','project','server-boundary-project',?2,'attempt','nonce','authorized',?3,'now','now')",
                params![serde_json::to_string(&launch).unwrap(), root.to_string_lossy(), legacy_probe.session_id],
            ).unwrap();
        }
        let legacy = json!({
            "kind":"runtime_probe_launch","operation_id":"legacy-launch",
            "admission_id":"admission-legacy","role":"manager"
        });
        let response = send(legacy.clone(), headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            knowledge_response_json(response).await["outcome_unknown"],
            true
        );
        {
            let connection = state.app.store.lock().unwrap();
            connection.execute_batch(
                "UPDATE trip_runtime_probes SET state='running' WHERE admission_id='admission-legacy';
                 UPDATE trip_runtime_admissions SET state='running' WHERE id='admission-legacy';",
            ).unwrap();
            connection.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES('legacy-launch-audit','legacy-launch','human','runtime_probe.launch','runtime_admission','admission-legacy',?1,'2026-01-01T00:00:00Z')",
                [json!({"role":"manager","session_id":legacy_probe.session_id,"attempt_id":"attempt"}).to_string()],
            ).unwrap();
        }
        let mut wrong_legacy = legacy.clone();
        wrong_legacy["admission_id"] = "admission-b".into();
        let response = send(wrong_legacy, headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(knowledge_response_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("different input"));
        let response = send(legacy, headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = knowledge_response_json(response).await;
        assert_eq!(body["outcome_unknown"], true);
        assert!(body.get("result").is_none());

        let unadmitted = json!({
            "kind":"runtime_probe_launch","operation_id":"unadmitted-op",
            "admission_id":"missing-admission","role":"manager"
        });
        let hash =
            json_hash(&json!({"admission_id":"missing-admission","role":"manager"})).unwrap();
        let load_refusal = || {
            state
                .app
                .store
                .browser_launch_receipt("unadmitted-op", "runtime_probe_launch", &hash)
                .unwrap()
                .unwrap()
        };
        let mut saved = None;
        for _ in 0..2 {
            let response = send(unadmitted.clone(), headers.clone()).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let refusal = knowledge_response_json(response).await;
            assert!(refusal["error"]
                .as_str()
                .unwrap()
                .contains("not explicitly authorized"));
            assert_eq!(refusal["operation_id"], "unadmitted-op");
            assert_eq!(refusal["outcome_unknown"], false);
            assert_eq!(refusal["terminal_refusal"], true);
            let receipt = load_refusal();
            assert_eq!(receipt["state"], "rejected");
            assert_eq!(*saved.get_or_insert_with(|| receipt.clone()), receipt);
        }
        let mut anonymous = headers.clone();
        anonymous.remove(header::COOKIE);
        assert_eq!(
            send(unadmitted.clone(), anonymous).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let mut changed = unadmitted.clone();
        changed["admission_id"] = "admission-a".into();
        let response = send(changed, headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let mismatch = knowledge_response_json(response).await;
        assert_eq!(mismatch["operation_id"], "unadmitted-op");
        assert_eq!(mismatch["outcome_unknown"], true);
        assert!(mismatch.get("terminal_refusal").is_none());
        assert!(mismatch["error"]
            .as_str()
            .unwrap()
            .contains("different input"));

        let setup_input = json!({
            "attempt_id":"missing-attempt","role":"manager","fresh_resume_rejection":null
        });
        let mut setup = setup_input.clone();
        setup["kind"] = "trip_setup_dispatch".into();
        setup["operation_id"] = "setup-refused".into();
        for _ in 0..2 {
            let response = send(setup.clone(), headers.clone()).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = knowledge_response_json(response).await;
            assert_eq!(body["operation_id"], "setup-refused");
            assert_eq!(body["outcome_unknown"], false);
            assert_eq!(body["terminal_refusal"], true);
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("no issued typed permit"));
        }
        state
            .app
            .store
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_setup_receipt BEFORE INSERT ON operation_receipts
             WHEN NEW.operation_id='setup-store-error'
             BEGIN SELECT RAISE(ABORT,'fixture receipt write failed'); END;",
            )
            .unwrap();
        setup["operation_id"] = "setup-store-error".into();
        let response = send(setup.clone(), headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = knowledge_response_json(response).await;
        assert_eq!(body["operation_id"], "setup-store-error");
        assert_eq!(body["outcome_unknown"], true);
        assert!(body.get("terminal_refusal").is_none());

        // A historical rejection written after reservation does not prove
        // nondelivery. It must keep the same request recoverable.
        let hash = json_hash(&setup_input).unwrap();
        reserve(
            "setup-uncertain",
            "trip_setup_dispatch",
            "missing-attempt",
            &hash,
            json!({"session_id":setup_session.session_id}),
        );
        state
            .app
            .store
            .finalize_browser_launch_receipt(
                "setup-uncertain",
                "trip_setup_dispatch",
                &hash,
                None,
                Some("fixture post-reservation write failed".into()),
            )
            .unwrap();
        setup["operation_id"] = "setup-uncertain".into();
        let response = send(setup, headers.clone()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = knowledge_response_json(response).await;
        assert_eq!(body["operation_id"], "setup-uncertain");
        assert_eq!(body["outcome_unknown"], true);
        assert!(body.get("terminal_refusal").is_none());

        // The same exact request cannot settle from a refused HTTP attempt.
        state.app.blocking_operations.close();
        let response = send(unadmitted, headers).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = knowledge_response_json(response).await;
        assert_eq!(body["operation_id"], "unadmitted-op");
        assert_eq!(body["outcome_unknown"], false);
        assert!(body.get("terminal_refusal").is_none());
        assert_eq!(Some(load_refusal()), saved);

        let connection = state.app.store.lock().unwrap();
        let inventory: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM launch_permits)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(inventory, (7, 0));
        drop(connection);
        drop(restarted);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn checked_legacy_import_distinguishes_stale_preview_and_unreadable_committed_receipt() {
        use serde_json::json;

        let (root, app, _, _) = server_test_application();
        let source = root.join("legacy-task.md");
        std::fs::write(
            &source,
            r#"---
id: legacy-task
title: Legacy task
status: todo
owner: nobody
priority: normal
attention: none
phase: backlog
validation_status: not_started
created: 2026-01-01T00:00:00Z
updated: 2026-01-01T00:00:00Z
---
# Legacy task
## Context
## Acceptance Criteria
## Progress
## Validation
### Runs
### Bugs
## Questions / Decisions
## Activity
"#,
        )
        .unwrap();
        let preview = crate::import::preview(&source).unwrap();
        let version: i64 = app
            .store
            .lock()
            .unwrap()
            .query_row(
                "SELECT version FROM projects WHERE id='project'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let (state, headers, _shutdown) = browser_state(app);
        let send = |request: serde_json::Value| {
            api_operation(
                State(state.clone()),
                headers.clone(),
                Json(serde_json::from_value(request).unwrap()),
            )
        };
        let request = json!({
            "kind":"legacy_import", "operation_id":"legacy-op", "project_id":"project",
            "expected_project_version":version, "source":source,
            "expected_source_hash":preview.source_hash,
        });
        let mut stale = request.clone();
        stale["operation_id"] = "legacy-stale".into();
        stale["expected_source_hash"] = "stale".into();
        let response = send(stale).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = knowledge_response_json(response).await;
        assert_eq!(body["operation_id"], "legacy-stale");
        assert_eq!(body["outcome_unknown"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("source changed after preview"));

        let response = send(request.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            knowledge_response_json(response).await["result"]["operation_id"],
            "legacy-op"
        );
        state
            .app
            .store
            .lock()
            .unwrap()
            .execute(
                "UPDATE operation_receipts SET result_json='[]' WHERE operation_id='legacy-op'",
                [],
            )
            .unwrap();
        let response = send(request).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = knowledge_response_json(response).await;
        assert_eq!(body["operation_id"], "legacy-op");
        assert_eq!(body["outcome_unknown"], true);
        let (project_version, imported_tasks, refusal_receipts): (i64, i64, i64) = state
            .app
            .store
            .lock()
            .unwrap()
            .query_row(
                "SELECT version, (SELECT COUNT(*) FROM tasks WHERE id='legacy-task'),
                    (SELECT COUNT(*) FROM operation_receipts WHERE operation_id='legacy-stale')
                 FROM projects WHERE id='project'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (project_version, imported_tasks, refusal_receipts),
            (version + 1, 1, 0)
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn knowledge_manifest_api_reads_large_persisted_provenance_and_rejects_overbound() {
        use crate::domain::*;
        let (root, app, _, _) = server_test_application();
        let (state, mut headers, _shutdown) = browser_state(app);
        headers.insert(HeaderName::from_static(protocol::HTTP_HEADER), HeaderValue::from_static(
            r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1","project_knowledge_v1"]}"#));
        let actor = KnowledgeUserActor {
            user_id: "human_control".into(),
        };
        let mut sources = Vec::new();
        for index in 0..16 {
            let id = format!("comment-{index}-{}", "c".repeat(100));
            state.app.store.lock().unwrap().execute(
                "INSERT INTO task_comments(id,task_id,body,actor_kind,created_at) VALUES(?1,'task','Exact evidence','human','now')",
                [&id],
            ).unwrap();
            sources.push(KnowledgeSourceSelector::Historical {
                source: HistoricalSourceKey {
                    task_id: "task".into(),
                    attempt_id: None,
                    kind: HistorySourceKind::Comment,
                    id,
                },
            });
        }
        let mut selection = Vec::new();
        for index in 0..16 {
            let draft = state
                .app
                .store
                .knowledge_mutate(
                    &actor,
                    &KnowledgeMutationRequest {
                        operation_id: format!("manifest-draft-{index}"),
                        project_id: "project".into(),
                        mutation: KnowledgeMutation::Draft {
                            entry_id: None,
                            expected_version: None,
                            body: KnowledgeDraftBody {
                                title: format!("Evidence {index}"),
                                statement: "Bound reference".into(),
                                category: "design".into(),
                                tags: vec![],
                                sources: sources.clone(),
                            },
                        },
                    },
                )
                .unwrap();
            let entry = state
                .app
                .store
                .knowledge_mutate(
                    &actor,
                    &KnowledgeMutationRequest {
                        operation_id: format!("manifest-approve-{index}"),
                        project_id: "project".into(),
                        mutation: KnowledgeMutation::Approve {
                            entry_id: draft.id.clone(),
                            expected_version: draft.version,
                            revision_id: draft.latest_draft_id.unwrap(),
                            replaces_revision_id: None,
                        },
                    },
                )
                .unwrap();
            let revision = entry.approved_revision_id.unwrap();
            let observation = state
                .app
                .store
                .knowledge_freshness("project", &entry.id, &revision)
                .unwrap();
            selection.push(KnowledgeSelection {
                entry_id: entry.id,
                revision_id: revision,
                expected_entry_version: entry.version,
                observation_fingerprint: observation.fingerprint,
                acknowledged_warning: false,
            });
        }
        let request = KnowledgePreviewRequest {
            project_id: "project".into(),
            consumer: KnowledgeConsumer::Task {
                task_id: "task".into(),
                parent_attempt_id: None,
                continuation: None,
                parent_binding_id: None,
            },
            expected_consumer_version: 1,
            user_text: "Exact task input".into(),
            selection,
        };
        let preview = state.app.store.knowledge_preview(&request).unwrap();
        let manifest = {
            let mut connection = state.app.store.lock().unwrap();
            let tx = connection.transaction().unwrap();
            let manifest = crate::knowledge::prepare_knowledge_manifest_in(
                &tx,
                &request,
                &preview.fingerprint,
                KnowledgeManifestBinding::TaskAttempt {
                    attempt_id: "attempt".into(),
                },
            )
            .unwrap();
            let bytes = serde_json::to_vec(&manifest).unwrap();
            assert!(bytes.len() > crate::knowledge::PREVIEW_RESPONSE_BYTES);
            assert!(bytes.len() <= crate::knowledge::MANIFEST_BYTES);
            crate::knowledge::insert_knowledge_manifest_in(&tx, &manifest).unwrap();
            tx.commit().unwrap();
            manifest
        };
        let read = serde_json::json!({"project_id":"project","read":{"kind":"manifest","manifest_id":manifest.id}});
        let mut request = Request::new(Body::from(read.to_string()));
        *request.headers_mut() = headers;
        let response = api_knowledge_read(State(state.clone()), request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), crate::knowledge::MANIFEST_BYTES)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::to_value(&manifest).unwrap()
        );
        let mut overbound = manifest.clone();
        overbound.id = "overbound-manifest".into();
        overbound.created_at = "x".repeat(crate::knowledge::MANIFEST_BYTES);
        let oversized_response = serde_json::to_value(&overbound).unwrap();
        let response = knowledge_work(
            state.clone(),
            crate::knowledge::MANIFEST_BYTES,
            None,
            move |_| Ok(oversized_response),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = knowledge_response_json(response).await;
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("response is too large"));
        assert!(body.get("outcome_unknown").is_none());
        {
            let mut connection = state.app.store.lock().unwrap();
            let tx = connection.transaction().unwrap();
            let error =
                crate::knowledge::insert_knowledge_manifest_in(&tx, &overbound).unwrap_err();
            assert!(
                error.to_string().contains("serialized data exceeds"),
                "{error:#}"
            );
        }
        assert_eq!(
            state
                .app
                .store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM knowledge_manifests", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn knowledge_api_auth_bounds_project_isolation_and_human_actor() {
        let (root, app, _, _) = server_test_application();
        let (state, headers, _shutdown) = browser_state(app);
        let body = serde_json::json!({"kind":"mutate","request":{
            "operation_id":"knowledge-draft","project_id":"project","mutation":{"kind":"draft",
            "entry_id":null,"expected_version":null,"body":{"title":"Decision","statement":"Human assertion",
            "category":"design","tags":[],"sources":[{"kind":"user_assertion"}]}}}});
        let make_request = |headers: HeaderMap, body: String| {
            let mut request = Request::new(Body::from(body));
            *request.headers_mut() = headers;
            request
        };
        let mut anonymous = headers.clone();
        anonymous.remove(header::COOKIE);
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(anonymous, body.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers.clone(), body.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            knowledge_response_json(response).await["outcome_unknown"],
            false
        );
        let mut headers = headers;
        headers.insert(HeaderName::from_static(protocol::HTTP_HEADER), HeaderValue::from_static(
            r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1","project_knowledge_v1"]}"#));
        let response = api_knowledge_preview(
            State(state.clone()),
            make_request(headers.clone(), " ".repeat(32 * 1024 + 1)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            state
                .app
                .store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM knowledge_entries", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let mut forged = body.clone();
        forged["actor"] = serde_json::json!({"user_id":"provider"});
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers.clone(), forged.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers.clone(), body.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(entry["operation_id"], "knowledge-draft");
        let (author, events): (String,i64) = state.app.store.lock().unwrap().query_row(
            "SELECT json_extract(r.body_json,'$.author'),(SELECT COUNT(*) FROM knowledge_events WHERE actor='human_control')
             FROM knowledge_revisions r WHERE r.entry_id=?1", [entry["id"].as_str().unwrap()], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(author, "human_control");
        assert_eq!(events, 1);
        let foreign = serde_json::json!({"project_id":"missing","read":{"kind":"entry","entry_id":entry["id"]}});
        let response = api_knowledge_read(
            State(state.clone()),
            make_request(headers.clone(), foreign.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let mut wrong_owner = body.clone();
        wrong_owner["request"]["project_id"] = serde_json::json!("missing");
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers.clone(), wrong_owner.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let refusal = knowledge_response_json(response).await;
        assert_eq!(refusal["operation_id"], "knowledge-draft");
        assert_eq!(refusal["outcome_unknown"], false);
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers.clone(), body.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(knowledge_response_json(response).await, entry);

        let mut second_draft = body.clone();
        second_draft["request"]["operation_id"] = "second-knowledge-draft".into();
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers.clone(), second_draft.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let second_entry = knowledge_response_json(response).await;
        assert_eq!(second_entry["operation_id"], "second-knowledge-draft");
        assert_ne!(second_entry["id"], entry["id"]);

        let entry_id = entry["id"].as_str().unwrap();
        let revision_id = entry["latest_draft_id"].as_str().unwrap();
        let prior = state
            .app
            .store
            .knowledge_freshness("project", entry_id, revision_id)
            .unwrap();
        let revalidate = serde_json::json!({
            "kind":"revalidate", "operation_id":"first-check", "project_id":"project",
            "entry_id":entry_id, "revision_id":revision_id,
            "expected_version":1, "expected_observation":prior.fingerprint
        });
        let send = |command: &serde_json::Value| {
            api_knowledge_command(
                State(state.clone()),
                make_request(headers.clone(), command.to_string()),
            )
        };
        let response = send(&revalidate).await;
        assert_eq!(response.status(), StatusCode::OK);
        let first = knowledge_response_json(response).await;
        assert_eq!(first["operation_id"], "first-check");
        assert_eq!(first["revision_id"], revision_id);
        assert!(first["sources"].is_array());
        let mut second_check = revalidate.clone();
        second_check["operation_id"] = "second-check".into();
        second_check["expected_observation"] = first["fingerprint"].clone();
        let response = send(&second_check).await;
        assert_eq!(response.status(), StatusCode::OK);
        let second = knowledge_response_json(response).await;
        assert_eq!(second["operation_id"], "second-check");
        assert_eq!(second["revision_id"], revision_id);
        assert_ne!(first["fingerprint"], second["fingerprint"]);
        let response = send(&revalidate).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(knowledge_response_json(response).await, first);
        let count: i64 = state
            .app
            .store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM knowledge_observations WHERE revision_id=?1",
                [revision_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);

        let response = knowledge_work(
            state.clone(),
            crate::knowledge::PREVIEW_RESPONSE_BYTES,
            Some("interrupted-knowledge".into()),
            |_| -> Result<serde_json::Value> { panic!("private worker detail") },
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let interrupted = knowledge_response_json(response).await;
        assert_eq!(interrupted["operation_id"], "interrupted-knowledge");
        assert_eq!(interrupted["outcome_unknown"], true);
        assert!(!interrupted.to_string().contains("private worker detail"));
        state.app.blocking_operations.close();
        let response = api_knowledge_command(
            State(state.clone()),
            make_request(headers, body.to_string()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let refusal = knowledge_response_json(response).await;
        assert_eq!(refusal["outcome_unknown"], false);
        assert!(refusal["error"]
            .as_str()
            .unwrap()
            .contains("was not started"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn knowledge_revalidation_commits_oversized_observation_and_replay_does_not_write_again()
    {
        use crate::domain::*;
        let (root, app, _, _) = server_test_application();
        let repository = server_test_repository(&root, 16);
        let project = crate::workflow::execute(
            &app.store,
            &HumanCommand::AddProject {
                operation_id: "register-observation-repository".into(),
                path: repository.root.clone(),
                display_name: "Observations".into(),
            },
        )
        .unwrap()
        .entity_id;
        let (state, mut headers, _shutdown) = browser_state(app);
        headers.insert(HeaderName::from_static(protocol::HTTP_HEADER), HeaderValue::from_static(
            r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1","project_knowledge_v1"]}"#));
        let store = &state.app.store;
        let operation_id = "revalidate-oversized";
        let draft = serde_json::json!({"kind":"mutate","request":{
            "operation_id":"draft-oversized-source","project_id":project,"mutation":{"kind":"draft",
            "entry_id":null,"expected_version":null,"body":{"title":"Large observation",
            "statement":"Exact source evidence","category":"note","tags":[],
            "sources":(0..16).map(|index| KnowledgeSourceSelector::Repository {
                commit:repository.head.clone(), path:format!("file-{index}"),
            }).collect::<Vec<_>>()}}}});
        let send = |command: &serde_json::Value| {
            let mut request = Request::new(Body::from(command.to_string()));
            *request.headers_mut() = headers.clone();
            api_knowledge_command(State(state.clone()), request)
        };
        let response = send(&draft).await;
        assert_eq!(response.status(), StatusCode::OK);
        let entry = knowledge_response_json(response).await;
        let entry_id = entry["id"].as_str().unwrap();
        let revision_id = entry["latest_draft_id"].as_str().unwrap();
        let read = |read: serde_json::Value| {
            let body = serde_json::json!({"project_id":project,"read":read});
            let mut request = Request::new(Body::from(body.to_string()));
            *request.headers_mut() = headers.clone();
            api_knowledge_read(State(state.clone()), request)
        };
        let freshness = serde_json::json!({
            "kind":"freshness","entry_id":entry_id,"revision_id":revision_id
        });

        // The linked worktree shares the physical repository. Escaping its
        // longer path once per source contributes over 72 KiB to the response.
        let component = "\u{1}".repeat(150);
        let relinked = root
            .join(&component)
            .join(&component)
            .join(&component)
            .join(&component)
            .join(&component)
            .join("worktree");
        crate::workspace::create_detached_worktree(&repository, &relinked, &repository.head)
            .unwrap();
        let relink = crate::workflow::execute(
            store,
            &HumanCommand::RelinkProject {
                operation_id: "relink-observation-repository".into(),
                project_id: project.clone(),
                path: relinked,
                expected_version: 1,
            },
        )
        .unwrap();
        assert_eq!(relink.state, "relinked");
        assert_eq!(relink.version, Some(2));
        let prior = store
            .knowledge_freshness(&project, entry_id, revision_id)
            .unwrap();
        assert_eq!(prior.state, KnowledgeFreshness::NeedsRevalidation);
        assert!(
            serde_json::to_vec(&prior).unwrap().len() < crate::knowledge::PREVIEW_RESPONSE_BYTES
        );
        let response = read(freshness.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            knowledge_response_json(response).await,
            serde_json::to_value(&prior).unwrap()
        );
        let command = serde_json::json!({
            "kind":"revalidate", "operation_id":operation_id, "project_id":project,
            "entry_id":entry_id, "revision_id":revision_id,
            "expected_version":1, "expected_observation":prior.fingerprint
        });
        let persisted = || {
            store.lock().unwrap().query_row(
                "SELECT (SELECT COUNT(*) FROM knowledge_observations WHERE revision_id=?1), result_json
                 FROM knowledge_operation_receipts WHERE operation_id=?2 AND project_id=?3",
                params![revision_id, operation_id, project],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            ).unwrap()
        };
        let response = send(&command).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = knowledge_response_json(response).await;
        let (count, receipt) = persisted();
        assert_eq!(count, 2);
        assert!(receipt.len() > crate::knowledge::PREVIEW_RESPONSE_BYTES);
        let committed: KnowledgeObservation = serde_json::from_str(&receipt).unwrap();
        assert_eq!(committed.state, KnowledgeFreshness::SourceUnavailable);
        assert_eq!(committed.revision_id, revision_id);
        assert_ne!(committed.fingerprint, prior.fingerprint);
        assert_eq!(
            body,
            serde_json::json!({
                "kind":"revalidation_confirmed", "operation_id":operation_id, "project_id":project,
                "entry_id":entry_id, "revision_id":revision_id, "fingerprint":committed.fingerprint
            })
        );

        let response = send(&command).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(knowledge_response_json(response).await, body);
        let mut conflicting = command;
        conflicting["expected_version"] = 2.into();
        let response = send(&conflicting).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            knowledge_response_json(response).await["outcome_unknown"],
            false
        );
        assert_eq!(persisted(), (count, receipt));

        let response = read(serde_json::json!({"kind":"entry","entry_id":entry_id})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let reopened_entry = knowledge_response_json(response).await;
        assert_eq!(reopened_entry["latest_draft_id"], revision_id);
        let response = read(serde_json::json!({
            "kind":"revision","entry_id":entry_id,"revision_id":revision_id
        }))
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(knowledge_response_json(response).await["id"], revision_id);
        let response = read(freshness.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let reopened_observation = knowledge_response_json(response).await;
        let mut expected_view = serde_json::to_value(&committed).unwrap();
        expected_view["sources_omitted"] = committed.sources.len().into();
        expected_view["sources"] = serde_json::json!([]);
        assert_eq!(reopened_observation, expected_view);
        assert_eq!(
            store
                .knowledge_freshness(&project, entry_id, revision_id)
                .unwrap()
                .sources
                .len(),
            16
        );

        let next = serde_json::json!({
            "kind":"revalidate", "operation_id":"revalidate-after-reopen", "project_id":project,
            "entry_id":entry_id, "revision_id":revision_id,
            "expected_version":reopened_entry["version"],
            "expected_observation":reopened_observation["fingerprint"]
        });
        let response = send(&next).await;
        assert_eq!(response.status(), StatusCode::OK);
        let next_confirmation = knowledge_response_json(response).await;
        assert_eq!(next_confirmation["kind"], "revalidation_confirmed");
        assert_eq!(next_confirmation["operation_id"], "revalidate-after-reopen");
        assert_eq!(persisted().0, count + 1);
        let response = read(freshness).await;
        assert_eq!(response.status(), StatusCode::OK);
        let latest = knowledge_response_json(response).await;
        assert_eq!(latest["fingerprint"], next_confirmation["fingerprint"]);
        assert_eq!(latest["sources"], serde_json::json!([]));
        assert_eq!(latest["sources_omitted"], 16);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn task_knowledge_command_distinguishes_ownership_refusal_from_post_receipt_failure() {
        use crate::domain::{
            KnowledgeConsumer, KnowledgePreparationAction, KnowledgePreviewRequest,
            PrepareNextRunKnowledge,
        };

        let (root, app, _, _) = server_test_application();
        let repository = server_test_repository(&root, 1);
        let project = crate::workflow::execute(
            &app.store,
            &HumanCommand::AddProject {
                operation_id: "register-rework-repository".into(),
                path: repository.root.clone(),
                display_name: "Rework".into(),
            },
        )
        .unwrap()
        .entity_id;
        let task = "rework-task";
        let parent = "rework-parent";
        let snapshot = "rework-accepted";
        let created_at = "2026-01-01T00:00:00Z";
        let parent_path = root.join("parent-worktree");
        crate::workspace::create_detached_worktree(&repository, &parent_path, &repository.head)
            .unwrap();
        let (manifest, manifest_hash) = crate::snapshot::capture(
            &repository,
            &parent_path,
            &app.paths.artifacts.join("snapshots").join(snapshot),
        )
        .unwrap();
        // Seed the reviewed parent boundary. The continuation command owns
        // staging the child, intent, frozen Knowledge, and durable receipt.
        let policy_hash = hex::encode(Sha256::digest(
            std::fs::read(parent_path.join("file-0")).unwrap(),
        ));
        let task_profiles = ["manager", "explorer", "plan_reviewer", "implementer", "code_reviewer", "final_verifier"]
            .map(|role| serde_json::json!({
                "role":role,"settings_revision":1,"profile":{},"source":"project_default",
                "profile_hash":policy_hash,"project_config_revision_id":"fixture-config",
                "project_configuration_hash":policy_hash,"adapter":"codex","adapter_hash":policy_hash,
                "capability_id":role,"capability_key":policy_hash,"capability_proof_hash":policy_hash
            }));
        let policy = serde_json::json!({
            "kind":"activated_project","workflow_id":crate::trip::WORKFLOW_ID,
            "upstream_source_hash":crate::trip::source_hash(),"overlay_hash":crate::trip::overlay_hash(),
            "manifest_hash":policy_hash,"files":{"file-0":policy_hash},"task_profiles":task_profiles,
            "base_project_configuration_distinct":true
        });
        let user_text = {
            let connection = app.store.lock().unwrap();
            connection.execute(
                "UPDATE trip_project_state SET readiness='ready',workflow_id=?1,package_version=?2,
                 upstream_source_hash=?3,overlay_hash=?4 WHERE project_id=?5",
                params![crate::trip::WORKFLOW_ID, crate::trip::PACKAGE_VERSION, crate::trip::source_hash(),
                    crate::trip::overlay_hash(), project],
            ).unwrap();
            connection.execute(
                "INSERT INTO tasks(id,project_id,title,lifecycle,attention,created_at,updated_at)
                 VALUES(?1,?2,'Reviewed parent','awaiting_review','needs_human_review',?3,?3)",
                params![task, project, created_at],
            ).unwrap();
            connection.execute(
                "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,
                 accepted_snapshot_id,candidate_hash,workflow_version,workflow_hash,scope_hash,configuration_hash,
                 legacy_migration_required,created_at,updated_at)
                 VALUES(?1,?2,'rework-context','awaiting_human_review',?3,1,'running',?4,?5,?6,?7,?8,?9,0,?10,?10)",
                params![parent, task, repository.head, snapshot, manifest_hash,
                    crate::trip::WORKFLOW_ID, crate::workflow_resources::workflow_hash(),
                    crate::store::json_hash(&serde_json::json!({
                        "title":"Reviewed parent", "description":"", "acceptance_criteria":[],
                        "base_revision":repository.head,
                    })).unwrap(),
                    crate::store::json_hash(&serde_json::json!([])).unwrap(), created_at],
            ).unwrap();
            connection.execute(
                "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,
                 policy_json,state,created_at,updated_at)
                 VALUES('rework-workspace',?1,?2,?3,?4,?4,?5,'ready',?6,?6)",
                params![parent, repository.identity, parent_path.to_str().unwrap(), repository.head,
                    policy.to_string(), created_at],
            ).unwrap();
            connection.execute(
                "INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
                 VALUES('rework-claim',?1,?2,?3,'running',?4,?4)",
                params![task, parent, repository.identity, created_at],
            ).unwrap();
            connection.execute(
                "INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at)
                 VALUES(?1,?2,'accepted',?3,?4,?5,1,?6)",
                params![snapshot, parent, manifest.snapshot_base, manifest_hash,
                    serde_json::to_string(&manifest).unwrap(), created_at],
            ).unwrap();
            crate::knowledge::task_knowledge_input_in(&connection, task).unwrap()
        };
        let preview = app
            .store
            .knowledge_preview(&KnowledgePreviewRequest {
                project_id: project.clone(),
                consumer: KnowledgeConsumer::Task {
                    task_id: task.into(),
                    parent_attempt_id: Some(parent.into()),
                    continuation: Some("trip_rework".into()),
                    parent_binding_id: Some(snapshot.into()),
                },
                expected_consumer_version: 1,
                user_text,
                selection: vec![],
            })
            .unwrap();
        let request = PrepareNextRunKnowledge {
            operation_id: "task-knowledge-operation".into(),
            project_id: project,
            task_id: task.into(),
            parent_attempt_id: parent.into(),
            continuation_kind: "trip_rework".into(),
            expected_task_version: 1,
            expected_preparation_version: None,
            parent_result_binding_id: snapshot.into(),
            parent_review_binding_id: None,
            selection: vec![],
            preview_fingerprint: preview.fingerprint,
            action: KnowledgePreparationAction::SaveAndContinueRework,
        };
        let command = HumanCommand::PrepareNextRunKnowledge {
            request: request.clone(),
        };
        let staged = crate::workflow::execute(&app.store, &command).unwrap();
        assert_eq!(staged.state, "rework_created");
        assert_eq!(staged.detail["child_attempt_id"], staged.entity_id);
        assert_eq!(staged.detail["parent_attempt_id"], parent);
        assert_eq!(staged.detail["preparation_version"], 1);
        let blocker = app.paths.artifacts.join("worktrees");
        std::fs::write(&blocker, "A file blocks the worktree parent directory").unwrap();

        let (state, mut headers, _shutdown) = browser_state(app);
        headers.insert(HeaderName::from_static(protocol::HTTP_HEADER), HeaderValue::from_static(
            r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1","project_knowledge_v1"]}"#));
        let persisted = || {
            state
                .app
                .store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM attempts WHERE parent_attempt_id=?1),
                        (SELECT state FROM rework_intents WHERE new_attempt_id=?2), result_json
                 FROM operation_receipts WHERE operation_id=?3 AND actor_key='human_control'
                   AND operation_kind='human_command'",
                    params![parent, staged.entity_id, request.operation_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .unwrap()
        };
        let (count, intent_state, receipt) = persisted();
        assert_eq!(count, 1);
        assert_eq!(intent_state, "reserved");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&receipt).unwrap(),
            serde_json::to_value(&staged).unwrap()
        );

        for _ in 0..2 {
            let response =
                api_command(State(state.clone()), headers.clone(), Json(command.clone())).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let unknown = knowledge_response_json(response).await;
            assert_eq!(unknown["operation_id"], request.operation_id);
            assert_eq!(unknown["outcome_unknown"], true);
            assert!(!unknown.to_string().contains(staged.entity_id.as_str()));
            assert_eq!(
                persisted(),
                (1, "recovery_required".into(), receipt.clone())
            );
        }
        assert!(blocker.is_file());
        let expected_failure = std::fs::create_dir_all(&blocker).unwrap_err().to_string();
        let failure: String = state.app.store.lock().unwrap().query_row(
            "SELECT json_extract(result_json,'$.reason') FROM rework_intents WHERE new_attempt_id=?1",
            [&staged.entity_id],
            |row| row.get(0),
        ).unwrap();
        assert!(failure.contains(&expected_failure), "{failure}");

        let conflicting = HumanCommand::PrepareNextRunKnowledge {
            request: PrepareNextRunKnowledge {
                project_id: "different-project".into(),
                ..request
            },
        };
        let response = api_command(State(state.clone()), headers, Json(conflicting)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let refused = knowledge_response_json(response).await;
        assert_eq!(refused["operation_id"], "task-knowledge-operation");
        assert_eq!(refused["outcome_unknown"], false);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stage_a_permission_preview_handler_is_read_only_and_bound_to_revision() {
        let (root, app, binding, _) = server_test_application();
        let repository_path = root.join("preview-repository");
        std::fs::create_dir(&repository_path).unwrap();
        for arguments in ["init -q", "-c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c core.hooksPath=/dev/null commit --allow-empty -qm fixture"] {
            let mut git = std::process::Command::new("git");
            git.current_dir(&repository_path).args(arguments.split_whitespace());
            for variable in ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"] {
                git.env_remove(variable);
            }
            assert!(git.output().unwrap().status.success());
        }
        let repository = crate::workspace::inspect(&repository_path).unwrap();
        let worktree = root.join("preview-worktree");
        crate::workspace::create_detached_worktree(&repository, &worktree, &repository.head)
            .unwrap();
        let worktree = worktree.canonicalize().unwrap();
        let executable = std::fs::canonicalize("/usr/bin/true").unwrap();
        let family = serde_json::json!({
            "executable_kind":"absolute", "executable_value":executable,
            "display_family":"true with all arguments", "canonical_executable":executable,
            "registered_root":repository.root, "repository_identity":repository.identity,
            "worktree_path":worktree, "session_preview":{}, "project_preview":{}
        });
        {
            let connection = app.store.lock().unwrap();
            let (rp, wp) = (
                repository.root.to_string_lossy(),
                worktree.to_string_lossy(),
            );
            let (sid, gid) = (&binding.session_id, &binding.role_generation_id);
            let (identity, head) = (&repository.identity, &repository.head);
            let executable = executable.to_string_lossy();
            connection.execute(
                "UPDATE projects SET repository_path=?1,repository_identity=?2,base_revision=?3 WHERE id='project'",
                params![rp, identity, head],
            ).unwrap();
            connection.execute(
                "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
                 VALUES('preview-workspace','attempt',?1,?2,?3,?3,'{}','ready','2026-01-01','2026-01-01')",
                params![identity, wp, head],
            ).unwrap();
            connection.execute(
                "UPDATE sessions SET native_session_id='preview-native',capability_key='preview-policy' WHERE id=?1",
                params![sid],
            ).unwrap();
            connection.execute(
                "INSERT INTO permission_requests(id,hook_invocation_nonce,connection_nonce,provider,project_id,task_id,attempt_id,session_id,role_generation_id,role,service_boot_id,native_session_id,cwd,policy_fingerprint,tool_name,input_digest,input_json,family_json,created_at,deadline_at,state,updated_at)
                 VALUES('preview-request','preview-hook','preview-connection','codex','project','task','attempt',?1,?2,'manager','preview-boot','preview-native',?3,'preview-policy','shell','preview-digest','{}',?4,'2026-01-01','9999-01-01T00:00:00Z','pending','2026-01-01')",
                params![sid, gid, wp, family.to_string()],
            ).unwrap();
            connection.execute(
                "INSERT INTO permission_rules(id,provider,project_id,role,lifetime,session_id,role_generation_id,native_session_id,registered_root,repository_identity,worktree_path,executable_kind,executable_value,display_family,policy_fingerprint,created_by,created_at,use_count)
                 VALUES('preview-rule','codex','project','manager','session',?1,?2,'preview-native',?3,?4,?5,'absolute',?6,'true','preview-policy','human','2026-01-01',7)",
                params![sid, gid, rp, identity, wp, executable],
            ).unwrap();
        }
        let snapshot = || {
            let connection = app.store.lock().unwrap();
            connection.query_row(
                "SELECT json_object('rules',(SELECT json_group_array(json_object('id',id,'uses',use_count,'revision',revision,'revoked',revoked_at,'last_used',last_used_at,'provider',provider,'role',role,'scope',lifetime,'session',session_id,'generation',role_generation_id,'native',native_session_id,'root',registered_root,'repository',repository_identity,'worktree',worktree_path,'kind',executable_kind,'value',executable_value,'policy',policy_fingerprint)) FROM permission_rules),
                 'receipts',(SELECT COUNT(*) FROM operation_receipts),'resolutions',(SELECT COUNT(*) FROM permission_native_resolutions),
                 'request',(SELECT json_object('input',input_json,'state',state,'revision',revision,'kind',decision_kind,'actor',decision_actor,'reason',decision_reason,'decided',decided_at,'rule',matching_rule_id,
                 'delivery',delivery_state,'reserved',delivery_reserved_at,'behavior',reserved_behavior,'delivered',delivered_at,'unknown',delivery_unknown_at,'delivery_reason',delivery_reason,'consumed',consumed_at,'updated',updated_at) FROM permission_requests WHERE id='preview-request'))",
                [], |row| row.get::<_, String>(0),
            ).unwrap()
        };
        let before = snapshot();
        let (state, mut headers, _) = browser_state(app.clone());
        headers.insert(protocol::HTTP_HEADER, HeaderValue::from_static(
            r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1","task_interactions_v1"]}"#,
        ));
        let mut sample = crate::domain::PermissionCoverageSample {
            request_id: "preview-request".into(),
            expected_revision: 1,
            command: "/usr/bin/true --help".into(),
            lifetime: crate::domain::PermissionLifetime::Session,
        };
        let mut unauthenticated = headers.clone();
        unauthenticated.remove(header::COOKIE);
        assert_eq!(
            api_permission_coverage(State(state.clone()), unauthenticated, Json(sample.clone()))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        for (command, status, reason) in [
            ("/usr/bin/true --help", "eligible", "family_compatible"),
            ("/usr/bin/false", "ineligible", "executable_family_mismatch"),
            ("sh -c true", "ineligible", "command_policy"),
            ("/usr/bin/true > output", "ineligible", "command_policy"),
        ] {
            sample.command = command.into();
            let response = api_permission_coverage(
                State(state.clone()),
                headers.clone(),
                Json(sample.clone()),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let preview: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(preview["status"], status);
            assert_eq!(preview["reason_code"], reason);
        }
        sample.expected_revision += 1;
        let response = api_permission_coverage(State(state), headers, Json(sample)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(snapshot(), before);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn operational_protocol_guard_is_auth_first_and_descriptor_is_authenticated() {
        let (root, app, _, _) = server_test_application();
        let (state, mut headers, _) = browser_state(app);
        let valid = r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1"]}"#;
        let mut unauthenticated = headers.clone();
        unauthenticated.remove(header::COOKIE);
        assert_eq!(
            require_operational_protocol(&state, &unauthenticated)
                .unwrap_err()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            api_protocol(State(state.clone()), unauthenticated.clone())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let descriptor = api_protocol(State(state.clone()), headers.clone()).await;
        assert_eq!(descriptor.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(descriptor.into_body(), 4096)
            .await
            .unwrap();
        let descriptor: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(descriptor["instance_id"], state.instance_id);
        assert!(descriptor.get("boot_id").is_none());
        let refused = require_operational_protocol(&state, &headers).unwrap_err();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(refused.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["protocol_error"]
                ["reason"],
            "missing"
        );
        for (declaration, reason) in [
            (
                r#"{"generation":0,"client_kind":"browser","required_features":[]}"#,
                "incompatible_generation",
            ),
            (
                r#"{"generation":2,"client_kind":"browser","required_features":[]}"#,
                "incompatible_generation",
            ),
            (
                r#"{"generation":1,"client_kind":"browser","required_features":["provider_support"]}"#,
                "missing_required_feature",
            ),
            (
                r#"{"generation":1,"client_kind":"human_cli","required_features":[]}"#,
                "wrong_client_kind",
            ),
            (
                r#"{"generation":1,"client_kind":"browser","required_features":[],"extra":true}"#,
                "malformed",
            ),
        ] {
            headers.insert(
                protocol::HTTP_HEADER,
                HeaderValue::from_str(declaration).unwrap(),
            );
            let refused = require_operational_protocol(&state, &headers).unwrap_err();
            let bytes = axum::body::to_bytes(refused.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["protocol_error"]
                    ["reason"],
                reason
            );
        }
        headers.insert(protocol::HTTP_HEADER, HeaderValue::from_str(valid).unwrap());
        assert!(require_operational_protocol(&state, &headers).is_ok());
        headers.append(protocol::HTTP_HEADER, HeaderValue::from_str(valid).unwrap());
        assert_eq!(
            require_operational_protocol(&state, &headers)
                .unwrap_err()
                .status(),
            StatusCode::CONFLICT
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn every_operational_route_rejects_before_body_or_query_extraction() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (root, app, target, other) = server_test_application();
        let hold_id = uuid::Uuid::new_v4().to_string();
        let hook_id = uuid::Uuid::new_v4().to_string();
        // Synthetic exited rows exercise HTTP recovery, not real-process stop evidence.
        {
            let connection = app.store.lock().unwrap();
            let exit = serde_json::json!({"success":true,"process_group_quiescent":true,"source":"synthetic_http_fixture"}).to_string();
            for binding in [&target, &other] {
                connection
                    .execute(
                        "UPDATE sessions SET status='exited',readiness_state='busy',exit_json=?1 WHERE id=?2",
                        params![exit, binding.session_id],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE role_generations SET status='exited' WHERE id=?1",
                        params![binding.role_generation_id],
                    )
                    .unwrap();
            }
            connection
                .execute(
                    "INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,payload_json,
                   peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                 VALUES(?1,?2,?3,'codex','StopFailure','{}',?4,?5,?6,
                   'managed_process_group_untrusted_payload','2026-01-01T00:00:00Z')",
                    params![
                        hook_id,
                        target.session_id,
                        target.role_generation_id,
                        target.process.pid,
                        target.process.process_group_id,
                        target.process.native_start_marker,
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
                   failure_hook_event_id,failure_kind,attribution,created_at,state)
                 VALUES(?1,'attempt',?2,?3,?4,?5,'billing_error','startup_invocation','2026-01-01T00:00:00Z','active')",
                    params![
                        hold_id,
                        target.role_generation_id,
                        target.session_id,
                        target.transcript_epoch,
                        hook_id,
                    ],
                )
                .unwrap();
        }
        let inventory = |app: &Application| -> String {
            app.store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT json_array((SELECT COUNT(*) FROM sessions),(SELECT COUNT(*) FROM role_generations),
                   (SELECT COUNT(*) FROM launch_permits),(SELECT COUNT(*) FROM resume_invocations),
                   (SELECT COALESCE(SUM(resume_count),0) FROM sessions),(SELECT COUNT(*) FROM guidance_messages),
                   (SELECT COUNT(*) FROM guidance_messages WHERE state='queued'),(SELECT COUNT(*) FROM role_results),
                   (SELECT COUNT(*) FROM snapshots),(SELECT COUNT(*) FROM controls))",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let preserved = inventory(&app);
        let version: i64 = app
            .store
            .lock()
            .unwrap()
            .query_row(
                "SELECT version FROM tasks WHERE id=?1",
                params!["task"],
                |row| row.get(0),
            )
            .unwrap();
        let (state, headers, _shutdown) = browser_state(app.clone());
        let cookie = headers[header::COOKIE].to_str().unwrap().to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            tokio::spawn(async move { axum::serve(listener, web_router(state)).await.unwrap() });
        let checked_app = app.clone();
        // Assertion panics must not skip joining the listener and deleting its fixture.
        let checks = tokio::spawn(async move {
            let request = |method: &str, path: &str, declaration: &str, body: &str, cookie: &str| {
                let request = format!(
                    "{method} {path} HTTP/1.1\r\nHost: server-boundary.test\r\nCookie: {cookie}\r\n{declaration}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
                );
                async move {
                    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
                    stream.write_all(request.as_bytes()).await.unwrap();
                    let mut response = Vec::new();
                    stream.read_to_end(&mut response).await.unwrap();
                    String::from_utf8(response).unwrap()
                }
            };
            let exchange = |method: &str, path: &str, declaration: &str, body: &str| {
                let response = request(method, path, declaration, body, &cookie);
                async move {
                    let text = response.await;
                    let body: serde_json::Value =
                        serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap();
                    (text, body)
                }
            };
            for (method, path) in [
                ("GET", "/api/health"),
                ("GET", "/api/state"),
                ("GET", "/api/state/wait?revision=invalid"),
                ("GET", "/api/restart-preview"),
                ("GET", "/api/model-catalog?provider=invalid"),
                ("GET", "/api/tasks/fixture/role-preparations"),
                ("GET", "/api/tasks/fixture/guidance-reauthorization"),
                ("GET", "/api/diagnostics"),
                ("GET", "/api/diagnostics/runtime-bindings"),
                ("POST", "/api/command"),
                ("POST", "/api/operation"),
            ] {
                let (text, _) = exchange(method, path, "", "{").await;
                assert!(text.starts_with("HTTP/1.1 409"), "{path}: {text}");
                assert!(text.contains("\"reason\":\"missing\""), "{path}: {text}");
            }
            let declaration = format!(
                "{}: {}\r\n",
                protocol::HTTP_HEADER,
                serde_json::json!({"generation":1,"client_kind":"browser","required_features":["http_operational_v1"]}),
            );
            let path = "/api/diagnostics/runtime-bindings";
            let text = request("GET", path, "", "", "").await;
            assert!(text.starts_with("HTTP/1.1 401"), "{text}");
            let text = request("GET", path, &declaration, "", "").await;
            assert!(text.starts_with("HTTP/1.1 401"), "{text}");
            let (text, bindings) = exchange("GET", path, &declaration, "").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert_eq!(bindings, serde_json::json!({"bindings":[]}));
            let (text, before) = exchange("GET", "/api/state", &declaration, "").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            let session = before["active_sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|session| session["id"] == target.session_id)
                .unwrap();
            assert_eq!(session["status"], "exited");
            assert_eq!(session["readiness"], "busy");
            assert_eq!(session["provider_failure_holds"][0]["id"], hold_id);
            let attention_id = format!("provider_failure_hold:{hold_id}");
            assert!(before["attention"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == attention_id));
            let operation_id = uuid::Uuid::new_v4().to_string();
            let mut release = serde_json::json!({"kind":"release_provider_failure_hold","operation_id":operation_id,
                "task_id":"task","attempt_id":"attempt","session_id":target.session_id,"hold_id":hold_id,"expected_task_version":version});
            let stale_id = uuid::Uuid::new_v4().to_string();
            let mut stale = release.clone();
            stale["operation_id"] = serde_json::json!(stale_id);
            stale["expected_task_version"] = serde_json::json!(version - 1);
            let (text, refused) =
                exchange("POST", "/api/command", &declaration, &stale.to_string()).await;
            assert!(text.starts_with("HTTP/1.1 409"), "{text}");
            assert_eq!(refused["operation_id"], stale_id);
            assert_eq!(refused["error"], "task version is stale");
            assert!(refused.get("protocol_error").is_none());
            let (text, unchanged) = exchange("GET", "/api/state", &declaration, "").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert_eq!(unchanged["tasks"][0]["version"], version);
            assert_eq!(unchanged["active_sessions"], before["active_sessions"]);
            assert_eq!(inventory(&checked_app), preserved);
            let receipt_count = |id: &str| -> i64 {
                checked_app.store.lock().unwrap()
                    .query_row(
                        "SELECT COUNT(*) FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='human_command'",
                        params![id],
                        |row| row.get(0),
                    )
                    .unwrap()
            };
            assert_eq!(receipt_count(&stale_id), 0);
            let stored_request = release.to_string();
            let (text, released) =
                exchange("POST", "/api/command", &declaration, &stored_request).await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert_eq!(released["result"]["operation_id"], operation_id);
            assert_eq!(released["result"]["entity_kind"], "provider_failure_hold");
            assert_eq!(released["result"]["entity_id"], hold_id);
            assert_eq!(released["result"]["state"], "released");
            assert_eq!(released["result"]["version"], version + 1);
            let (text, after) = exchange("GET", "/api/state", &declaration, "").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            let mut preserved_sessions = before["active_sessions"].clone();
            preserved_sessions
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|session| session["id"] == target.session_id)
                .unwrap()["provider_failure_holds"] = serde_json::json!([]);
            assert_eq!(after["active_sessions"], preserved_sessions);
            assert!(!after["attention"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == attention_id));
            assert_eq!(after["tasks"][0]["version"], version + 1);
            assert_eq!(after["guidance"], before["guidance"]);
            assert_eq!(
                after["tasks"][0]["snapshots"],
                before["tasks"][0]["snapshots"]
            );
            assert_eq!(inventory(&checked_app), preserved);
            let (text, replayed) =
                exchange("POST", "/api/command", &declaration, &stored_request).await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            assert_eq!(replayed, released);
            assert_eq!(receipt_count(&operation_id), 1);
            release["expected_task_version"] = serde_json::json!(version + 1);
            let (text, conflict) =
                exchange("POST", "/api/command", &declaration, &release.to_string()).await;
            assert!(text.starts_with("HTTP/1.1 409"), "{text}");
            assert_eq!(conflict["operation_id"], operation_id);
            assert_eq!(
                conflict["error"],
                "operation ID was already used with different input"
            );
            assert!(conflict.get("protocol_error").is_none());
            let (text, final_state) = exchange("GET", "/api/state", &declaration, "").await;
            assert!(text.starts_with("HTTP/1.1 200"), "{text}");
            for field in ["active_sessions", "guidance", "tasks"] {
                assert_eq!(final_state[field], after[field], "{field}");
            }
            assert_eq!(receipt_count(&operation_id), 1);
            assert_eq!(receipt_count(&stale_id), 0);
            assert_eq!(inventory(&checked_app), preserved);
        })
        .await;
        server.abort();
        let _ = server.await;
        drop((headers, _shutdown, app));
        std::fs::remove_dir_all(root).unwrap();
        checks.unwrap();
    }

    #[tokio::test]
    async fn browser_sessions_authenticate_only_their_own_instance_cookie() {
        let (first_root, first_app, _, _) = server_test_application();
        let (second_root, second_app, _, _) = server_test_application();
        let (first, mut anonymous, _) = browser_state(first_app);
        let (second, _, _) = browser_state(second_app);
        anonymous.remove(header::COOKIE);
        assert_ne!(first.session_cookie, second.session_cookie);
        for state in [&first, &second] {
            assert!(state.session_cookie.starts_with("agenticjira_session_"));
            assert_eq!(
                state.session_cookie.len(),
                "agenticjira_session_".len() + 20
            );
        }
        let bootstrap = |state: WebState| {
            let anonymous = anonymous.clone();
            async move {
                let token = state.app.bootstrap.issue().unwrap();
                let response =
                    bootstrap_session(State(state), anonymous, Json(BootstrapRequest { token }))
                        .await;
                assert_eq!(response.status(), StatusCode::OK);
                response.headers()[header::SET_COOKIE]
                    .to_str()
                    .unwrap()
                    .to_owned()
            }
        };
        let first_set = bootstrap(first.clone()).await;
        let second_set = bootstrap(second.clone()).await;
        let (first_cookie, attributes) = first_set.split_once("; ").unwrap();
        assert_eq!(attributes, "HttpOnly; SameSite=Strict; Path=/");
        let (name, secret) = first_cookie.split_once('=').unwrap();
        assert_eq!(name, first.session_cookie);
        let (second_cookie, _) = second_set.split_once("; ").unwrap();
        assert!(second_cookie.starts_with(&format!("{}=", second.session_cookie)));
        let presenting = |cookies: &[String]| {
            let mut headers = anonymous.clone();
            for cookie in cookies {
                headers.append(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
            }
            headers
        };
        let both = presenting(&[format!("{first_cookie}; {second_cookie}")]);
        assert!(has_browser_session(&first, &both));
        assert!(has_browser_session(&second, &both));
        assert!(has_browser_session(
            &first,
            &presenting(&[format!("theme=dark; {first_cookie}")])
        ));
        for refused in [
            vec![first_cookie.to_owned()],
            vec![format!("{}={secret}", second.session_cookie)],
            vec![format!("agenticjira_session={secret}")],
        ] {
            assert!(
                !has_browser_session(&second, &presenting(&refused)),
                "{refused:?}"
            );
        }
        assert!(!has_browser_session(
            &first,
            &presenting(&[format!("agenticjira_session={secret}")])
        ));
        for refused in [
            vec![format!("{first_cookie}; {first_cookie}")],
            vec![first_cookie.to_owned(), first_cookie.to_owned()],
            vec![format!("{first_cookie}; {name}=other")],
            vec![format!("{name}=")],
            vec![format!("{name}=\"{secret}\"")],
            vec![format!(
                "{name}={}",
                "a".repeat(MAX_SESSION_COOKIE_VALUE_BYTES + 1)
            )],
        ] {
            assert!(
                !has_browser_session(&first, &presenting(&refused)),
                "{refused:?}"
            );
        }
        let mut unreadable = presenting(&[first_cookie.to_owned()]);
        unreadable.append(
            header::COOKIE,
            HeaderValue::from_bytes(b"theme=\xff").unwrap(),
        );
        assert!(!has_browser_session(&first, &unreadable));
        let _ = std::fs::remove_dir_all(first_root);
        let _ = std::fs::remove_dir_all(second_root);
    }

    async fn invoke_operation(
        state: WebState,
        headers: HeaderMap,
        operation: WebOperation,
    ) -> (StatusCode, serde_json::Value) {
        let response = api_operation(State(state), headers, Json(operation)).await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn command_and_role_preparation_keep_admission_until_cancelled_work_finishes() {
        for preparation in [false, true] {
            let (root, app, _, _) = server_test_application();
            let (state, headers, _) = browser_state(app);
            let store = state.app.store.clone();
            let (locked_tx, locked_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let locker = std::thread::spawn(move || {
                let _guard = store.lock().unwrap();
                locked_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            });
            locked_rx.recv().unwrap();
            let initial = state.app.blocking_operations.available_permits();
            let request_state = state.clone();
            let request_headers = headers.clone();
            let handler = tokio::spawn(async move {
                if preparation {
                    api_role_preparations(
                        State(request_state),
                        AxumPath("missing-task".to_owned()),
                        request_headers,
                    )
                    .await
                } else {
                    api_command(
                        State(request_state),
                        request_headers,
                        Json(HumanCommand::SetQueuePaused {
                            operation_id: uuid::Uuid::new_v4().to_string(),
                            project_id: "missing-project".into(),
                            expected_version: 1,
                            paused: true,
                        }),
                    )
                    .await
                }
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                while state.app.blocking_operations.available_permits() == initial {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let diagnostics = tokio::time::timeout(
                Duration::from_millis(250),
                api_diagnostics(State(state.clone()), headers),
            )
            .await
            .unwrap();
            assert_eq!(diagnostics.status(), StatusCode::OK);
            handler.abort();
            assert!(handler.await.unwrap_err().is_cancelled());
            assert_eq!(
                state.app.blocking_operations.available_permits(),
                initial - 1
            );
            release_tx.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while state.app.blocking_operations.available_permits() != initial {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            locker.join().unwrap();
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn blocking_admission_refuses_before_dispatch_and_worker_failure_is_distinct() {
        let permits = Arc::new(Semaphore::new(1));
        permits.close();
        let called = Arc::new(AtomicBool::new(false));
        let called_inside = Arc::clone(&called);
        let result = admitted_blocking_work(permits, move || {
            called_inside.store(true, AtomicOrdering::SeqCst)
        })
        .await;
        assert!(matches!(result, Err(BlockingWorkError::Admission)));
        assert!(!called.load(AtomicOrdering::SeqCst));
        let result = admitted_blocking_work(Arc::new(Semaphore::new(1)), || {
            panic!("injected worker failure")
        })
        .await;
        assert!(matches!(result, Err(BlockingWorkError::Interrupted(_))));
    }

    #[tokio::test]
    async fn saturated_handlers_cancelled_before_admission_never_run() {
        use std::{future::Future, task::Poll};
        for preparation in [false, true] {
            let (root, app, _, _) = server_test_application();
            let (state, headers, _) = browser_state(app);
            let capacity = state.app.blocking_operations.available_permits();
            let held = state
                .app
                .blocking_operations
                .clone()
                .acquire_many_owned(capacity as u32)
                .await
                .unwrap();
            let operation_id = uuid::Uuid::new_v4().to_string();
            let command = HumanCommand::SetQueuePaused {
                operation_id: operation_id.clone(),
                project_id: "project".into(),
                expected_version: 1,
                paused: false,
            };
            let request_state = state.clone();
            let mut handler = Box::pin(async move {
                if preparation {
                    api_role_preparations(State(request_state), AxumPath("task".into()), headers)
                        .await
                } else {
                    api_command(State(request_state), headers, Json(command)).await
                }
            });
            std::future::poll_fn(|context| {
                assert!(handler.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;
            assert_eq!(state.app.blocking_operations.available_permits(), 0);
            drop(handler);
            drop(held);
            tokio::task::yield_now().await;
            assert_eq!(state.app.blocking_operations.available_permits(), capacity);
            let connection = state.app.store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM operation_receipts WHERE operation_id=?1",
                        params![operation_id],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT version FROM projects WHERE id='project'",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            drop(connection);
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn trip_application_replays_exact_receipts_before_unavailable_inventory() {
        let (root, app, _, _) = server_test_application();
        app.store.lock().unwrap().execute_batch(
            "INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance,spent) VALUES('trip-budget','attempt','plan',2,0);"
        ).unwrap();
        let action = crate::domain::TripHumanAction::ExtendReviewBudget {
            task_id: "task".into(),
            attempt_id: "attempt".into(),
            expected_task_version: 1,
            review_kind: "plan".into(),
            additional: 1,
        };
        let command = HumanCommand::Trip {
            operation_id: "trip-saved".into(),
            action: action.clone(),
        };
        let saved = app.execute_human_command(&command).unwrap();
        app.supervisor
            .fail_process_inventory_for_tests(Some("deterministic inventory failure"));
        assert_eq!(
            serde_json::to_value(app.execute_human_command(&command).unwrap()).unwrap(),
            serde_json::to_value(saved).unwrap()
        );
        let conflicting = HumanCommand::Trip {
            operation_id: "trip-saved".into(),
            action: crate::domain::TripHumanAction::ExtendReviewBudget {
                task_id: "task".into(),
                attempt_id: "attempt".into(),
                expected_task_version: 1,
                review_kind: "plan".into(),
                additional: 2,
            },
        };
        assert!(app
            .execute_human_command(&conflicting)
            .unwrap_err()
            .to_string()
            .contains("another request"));
        let missing = HumanCommand::Trip {
            operation_id: "trip-missing".into(),
            action,
        };
        let error = app.execute_human_command(&missing).unwrap_err();
        assert!(format!("{error:#}").contains("deterministic inventory failure"));
        assert!(!error.is::<crate::trip::TripOutcomeUnknown>());
        let connection = app.store.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT extension_allowance FROM review_budgets WHERE id='trip-budget'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT version FROM tasks WHERE id='task'", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM operation_receipts WHERE operation_id='trip-missing'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(connection);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn trip_production_mapper_distinguishes_rollback_current_and_persisted_progress() {
        let (root, app, _, _) = server_test_application();
        app.store.lock().unwrap().execute_batch(
            "INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance,spent) VALUES('trip-budget','attempt','plan',2,0);
             CREATE TRIGGER trip_mapper_receipt BEFORE INSERT ON operation_receipts
             WHEN NEW.operation_id='mapper-rollback' BEGIN SELECT RAISE(ABORT,'mapper receipt failure'); END;"
        ).unwrap();
        let runtime = crate::trip::CapabilityRuntime::from_store(
            &app.store,
            app.hooks.clone(),
            app.paths.role_socket.clone(),
            std::env::current_exe().unwrap(),
        );
        let budget = crate::domain::TripHumanAction::ExtendReviewBudget {
            task_id: "task".into(),
            attempt_id: "attempt".into(),
            expected_task_version: 1,
            review_kind: "plan".into(),
            additional: 1,
        };
        let rollback = crate::trip::execute_human_with_runtime(
            &app.store,
            &app.paths,
            &runtime,
            "mapper-rollback",
            &budget,
        )
        .unwrap_err();
        let refusal = crate::trip::execute_human_with_runtime(
            &app.store,
            &app.paths,
            &runtime,
            "mapper-refusal",
            &crate::domain::TripHumanAction::ExtendReviewBudget {
                task_id: "task".into(),
                attempt_id: "attempt".into(),
                expected_task_version: 1,
                review_kind: "final".into(),
                additional: 1,
            },
        )
        .unwrap_err();
        for (operation, error) in [("mapper-rollback", rollback), ("mapper-refusal", refusal)] {
            let response = human_command_error_response(operation, &error);
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["outcome_unknown"], false);
            assert_eq!(body["operation_id"], operation);
        }
        let repository = root.join("mapper-repository");
        std::fs::create_dir(&repository).unwrap();
        for arguments in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-m", "fixture"],
        ] {
            assert!(std::process::Command::new("git")
                .args(arguments)
                .current_dir(&repository)
                .output()
                .unwrap()
                .status
                .success());
        }
        let project = crate::workflow::execute(
            &app.store,
            &HumanCommand::AddProject {
                operation_id: "mapper-project".into(),
                path: repository,
                display_name: "Mapper project".into(),
            },
        )
        .unwrap()
        .entity_id;
        let begin = crate::domain::TripHumanAction::BeginSetup {
            project_id: project.clone(),
            expected_project_version: 1,
            host_manager: crate::domain::RoleOverride {
                provider: crate::domain::Provider::Codex,
                model: "fixture-model".into(),
                effort: "high".into(),
            },
        };
        app.store
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER trip_mapper_setup_receipt BEFORE INSERT ON operation_receipts
             WHEN NEW.operation_id='mapper-progress' AND NEW.operation_kind='trip_command'
             BEGIN SELECT RAISE(ABORT,'mapper setup receipt failure'); END;",
            )
            .unwrap();
        let current = crate::trip::execute_human_with_runtime(
            &app.store,
            &app.paths,
            &runtime,
            "mapper-progress",
            &begin,
        )
        .unwrap_err();
        assert!(format!("{current:#}").contains("mapper setup receipt failure"));
        app.store.lock().unwrap().execute_batch(
            "CREATE TRIGGER trip_mapper_recovery_failure BEFORE UPDATE ON trip_setup_operations
             WHEN NEW.state='workspace_recovery_required' BEGIN SELECT RAISE(ABORT,'recovery recording failed'); END;"
        ).unwrap();
        let workspace: String = app.store.lock().unwrap().query_row(
            "SELECT w.path FROM workspaces w JOIN trip_setup_operations s ON s.discovery_attempt_id=w.attempt_id WHERE s.project_id=?1",
            params![project], |row| row.get(0),
        ).unwrap();
        std::fs::write(
            std::path::Path::new(&workspace).join(".agents/trip-explorer/llmrelay-overlay.md"),
            b"drift",
        )
        .unwrap();
        let persisted = crate::trip::execute_human_with_runtime(
            &app.store,
            &app.paths,
            &runtime,
            "mapper-progress",
            &begin,
        )
        .unwrap_err();
        assert!(format!("{persisted:#}").contains("recovery recording failed"));
        app.store.lock().unwrap().execute("DELETE FROM audit_events WHERE operation_id='mapper-progress' AND event_code='trip.command.progress'", []).unwrap();
        let historical = crate::trip::execute_human_with_runtime(
            &app.store,
            &app.paths,
            &runtime,
            "mapper-progress",
            &begin,
        )
        .unwrap_err();
        assert!(format!("{historical:#}").contains("recovery recording failed"));
        for error in [current, persisted, historical] {
            let response = human_command_error_response("mapper-progress", &error);
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["outcome_unknown"], true);
            assert_eq!(body["operation_id"], "mapper-progress");
            assert!(body["error"]
                .as_str()
                .unwrap()
                .contains("Refresh and check"));
        }
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn occurrence_stop_intent_requires_its_feature_and_preserves_legacy_command() {
        let (root, app, _, _) = server_test_application();
        let (state, mut headers, _) = browser_state(app);
        headers.insert(protocol::HTTP_HEADER, HeaderValue::from_static(
            r#"{"generation":1,"client_kind":"browser","required_features":["http_operational_v1"]}"#,
        ));
        let command = HumanCommand::RequestRecipeScheduleOccurrenceStop {
            operation_id: "typed-stop".into(),
            intent: crate::domain::ScheduleOccurrenceStopIntent {
                project_id: "project".into(),
                schedule_id: "schedule".into(),
                expected_version: 1,
                occurrence_id: "occurrence".into(),
                authorization_id: "grant".into(),
            },
        };
        let response =
            api_command_with_executor(state.clone(), headers.clone(), command.clone(), |_, _| {
                StatusCode::ACCEPTED.into_response()
            })
            .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let legacy = HumanCommand::StopRecipeScheduleOccurrence {
            operation_id: "legacy-stop".into(),
            project_id: "project".into(),
            schedule_id: "schedule".into(),
            expected_version: 1,
            binding: crate::domain::ScheduleStopBinding {
                occurrence_id: "occurrence".into(),
                authorization_id: "grant".into(),
                attempt_id: "attempt".into(),
                session_id: "session".into(),
                resume_invocation_id: None,
                role_generation_id: "generation".into(),
                transcript_epoch: "epoch".into(),
                process: None,
            },
        };
        assert_eq!(
            api_command_with_executor(state.clone(), headers.clone(), legacy, |_, _| {
                StatusCode::ACCEPTED.into_response()
            })
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        headers.insert(protocol::HTTP_HEADER, HeaderValue::from_str(&serde_json::json!({
            "generation":1,"client_kind":"browser",
            "required_features":["http_operational_v1",protocol::SCHEDULE_OCCURRENCE_STOP_INTENT_FEATURE]
        }).to_string()).unwrap());
        assert_eq!(
            api_command_with_executor(state.clone(), headers.clone(), command.clone(), |_, _| {
                StatusCode::ACCEPTED.into_response()
            })
            .await
            .status(),
            StatusCode::ACCEPTED
        );
        headers.remove(header::COOKIE);
        assert_eq!(
            api_command_with_executor(state.clone(), headers, command, |_, _| {
                StatusCode::ACCEPTED.into_response()
            })
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            state
                .app
                .store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM operation_receipts", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn setup_probe_command_confirms_only_the_exact_durable_refusal() {
        use crate::domain::TripHumanAction;

        let (root, app, _, _) = server_test_application();
        let original = TripHumanAction::AuthorizeSetupProbes {
            setup_operation_id: "setup".into(),
            proposal_hash: "proposal-a".into(),
        };
        let hash = crate::store::json_hash(&original).unwrap();
        let saved_draft = serde_json::json!({
            "operation_id": "save-proposal-a",
            "entity_kind": "trip_setup",
            "entity_id": "setup",
            "state": "draft_saved",
            "detail": {"proposal_hash": "proposal-a"}
        })
        .to_string();
        {
            let connection = app.store.lock().unwrap();
            connection.execute(
                "INSERT INTO trip_setup_operations(id,project_id,state,target_inventory_json,proposal_json,proposal_hash,created_at,updated_at)
                 VALUES('setup','project','draft','{}','{}','proposal-b','2026-01-01','2026-01-01')",
                [],
            ).unwrap();
            connection.execute(
                "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
                 VALUES('save-proposal-a','human_control','trip_command','save-hash',?1,'2026-01-01')",
                params![saved_draft],
            ).unwrap();
            connection.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES('saved-a','save-proposal-a','human','trip.command.applied','trip_setup','setup',?1,'2026-01-01'),
                       ('partial-a','partial','human','trip.command.progress','trip_setup','setup',?2,'2026-01-01'),
                       ('historical-a','historical','human','trip.command.applied','trip_setup','setup',?3,'2026-01-01')",
                params![
                    saved_draft,
                    serde_json::json!({"request_hash":hash,"recovery_identity":"setup"}).to_string(),
                    serde_json::json!({"state":"probes_authorized"}).to_string(),
                ],
            ).unwrap();
        }
        let (state, headers, _) = browser_state(app);
        let authorize = |operation_id: &str, action| {
            api_command(
                State(state.clone()),
                headers.clone(),
                Json(HumanCommand::Trip {
                    operation_id: operation_id.into(),
                    action,
                }),
            )
        };
        let wrong_hash = TripHumanAction::AuthorizeSetupProbes {
            setup_operation_id: "setup".into(),
            proposal_hash: "wrong-proposal".into(),
        };
        for (id, action) in [
            ("saved", wrong_hash.clone()),
            ("partial", original.clone()),
            ("historical", original.clone()),
        ] {
            let response = authorize(id, action).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = knowledge_response_json(response).await;
            assert_eq!(body["operation_id"], id);
            assert_eq!(body["outcome_unknown"], true);
            assert!(body.get("terminal_refusal").is_none());
        }
        for _ in 0..2 {
            let response = authorize("saved", original.clone()).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = knowledge_response_json(response).await;
            assert_eq!(body["operation_id"], "saved");
            assert_eq!(body["outcome_unknown"], false);
            assert_eq!(body["terminal_refusal"], true);
            assert!(body.get("result").is_none());
        }
        for action in [
            wrong_hash,
            TripHumanAction::InspectProject {
                project_id: "project".into(),
            },
        ] {
            let response = authorize("saved", action).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = knowledge_response_json(response).await;
            assert_eq!(body["operation_id"], "saved");
            assert_eq!(body["outcome_unknown"], true);
            assert!(body.get("terminal_refusal").is_none());
        }
        let receipts: (String, String, bool) = state.app.store.lock().unwrap().query_row(
            "SELECT actor_key,request_hash,EXISTS(
                SELECT 1 FROM operation_receipts WHERE operation_id IN ('partial','historical')
             ) FROM operation_receipts WHERE operation_id='saved' AND operation_kind='trip_command'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(receipts, ("human_control".to_owned(), hash, false));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn command_handlers_distinguish_refused_admission_from_unknown_mutation_outcome() {
        for interrupted in [false, true] {
            let (root, app, _, _) = server_test_application();
            let (state, headers, _) = browser_state(app);
            let operation_id = uuid::Uuid::new_v4().to_string();
            let command = HumanCommand::SetQueuePaused {
                operation_id: operation_id.clone(),
                project_id: "project".into(),
                expected_version: 1,
                paused: false,
            };
            let response = if interrupted {
                api_command_with_executor(state.clone(), headers, command, |app, command| {
                    app.execute_human_command(command).unwrap();
                    panic!("injected worker failure after committed command")
                })
                .await
            } else {
                state.app.blocking_operations.close();
                let preparation = api_role_preparations(
                    State(state.clone()),
                    AxumPath("task".into()),
                    headers.clone(),
                )
                .await;
                assert_eq!(preparation.status(), StatusCode::SERVICE_UNAVAILABLE);
                api_command(State(state.clone()), headers, Json(command)).await
            };
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["operation_id"], operation_id);
            assert_eq!(body["outcome_unknown"], interrupted);
            assert!(body["error"].as_str().unwrap().contains(if interrupted {
                "Refresh and check before retrying"
            } else {
                "request was not started"
            }));
            let receipt_count: i64 = state
                .app
                .store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM operation_receipts WHERE operation_id=?1",
                    params![operation_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(receipt_count, i64::from(interrupted));
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn workflow_worker_interruption_preserves_unknown_committed_outcome() {
        let (root, app, _, _) = server_test_application();
        let (state, _, _) = browser_state(app);
        let command = crate::domain::WorkflowCommand::SetProjectWorkflowDefault {
            operation_id: uuid::Uuid::new_v4().to_string(),
            project_id: "project".into(),
            expected_version: 0,
            family: crate::domain::WorkflowFamily::SingleAgent,
            preset: Default::default(),
        };
        let worker_state = state.clone();
        let worker_command = command.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
            worker_state
                .app
                .execute_workflow_command(&worker_command)
                .unwrap();
            panic!("injected interruption after committed workflow operation")
        })
        .await;
        let response = web_operation_response(&state, None, result);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["outcome_unknown"], true);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Refresh and check before retrying"));
        // The original identity replays its committed result without incrementing
        // the workflow default again, despite the interrupted HTTP worker.
        let replay = state.app.execute_workflow_command(&command).unwrap();
        assert_eq!(serde_json::to_value(replay).unwrap()["version"], 1);
        assert_eq!(
            state
                .app
                .store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT version FROM project_workflow_defaults WHERE project_id='project'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        let refusal =
            web_operation_response(&state, None, Ok(Err(anyhow!("refused before mutation"))));
        assert_eq!(refusal.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(refusal.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_ne!(body["outcome_unknown"], true);
        drop(state);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocked_store_operation_does_not_stall_store_free_diagnostics() {
        let (root, app, target, _) = server_test_application();
        let (state, headers, _) = browser_state(app);
        let store = state.app.store.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let locker = std::thread::spawn(move || {
            let _guard = store.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.recv().unwrap();

        let initial_permits = state.app.blocking_operations.available_permits();
        let operation_state = state.clone();
        let operation_headers = headers.clone();
        let operation = tokio::spawn(async move {
            invoke_operation(
                operation_state,
                operation_headers,
                WebOperation::Transcript {
                    session_id: target.session_id,
                    after_epoch: None,
                    after_sequence: 0,
                    limit_bytes: 1024,
                },
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            while state.app.blocking_operations.available_permits() == initial_permits {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let diagnostics = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            api_diagnostics(State(state), headers),
        )
        .await
        .unwrap();
        assert_eq!(diagnostics.status(), StatusCode::OK);

        release_tx.send(()).unwrap();
        operation.await.unwrap();
        locker.join().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn transcript_operation_rejects_outside_paths_and_reads_exited_session_history() {
        let (root, app, target, _) = server_test_application();
        let outside = root.join("outside.jsonl");
        let sentinel = b"malformed outside transcript sentinel";
        std::fs::write(&outside, sentinel).unwrap();
        let (state, headers, _) = browser_state(app.clone());
        let (status, _) = invoke_operation(
            state.clone(),
            headers.clone(),
            WebOperation::Transcript {
                session_id: "../outside".to_owned(),
                after_epoch: None,
                after_sequence: 0,
                limit_bytes: 1024,
            },
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&outside).unwrap(), sentinel);

        let sink = crate::transcript::TranscriptSink::create(
            &app.paths.transcripts,
            &target.session_id,
            &target.transcript_epoch,
        )
        .unwrap();
        sink.append(b"exited session history").unwrap();
        app.store
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET status='exited' WHERE id=?1",
                params![target.session_id],
            )
            .unwrap();
        let (status, body) = invoke_operation(
            state,
            headers,
            WebOperation::Transcript {
                session_id: target.session_id,
                after_epoch: None,
                after_sequence: 0,
                limit_bytes: 1024,
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["result"]["frames"][0]["epoch"],
            target.transcript_epoch
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn history_operations_require_their_declared_feature_and_read_one_exact_epoch() {
        let (root, app, target, other) = server_test_application();
        let (state, mut headers, _) = browser_state(app.clone());
        let sink = crate::transcript::TranscriptSink::create(
            &app.paths.transcripts,
            &target.session_id,
            &target.transcript_epoch,
        )
        .unwrap();
        sink.append(b"exact epoch history\n").unwrap();
        let page = |epoch: &str| WebOperation::HistoryPage {
            session_id: target.session_id.clone(),
            transcript_epoch: epoch.to_owned(),
            after_sequence: 0,
            limit_bytes: 4096,
        };

        let (status, body) = invoke_operation(
            state.clone(),
            headers.clone(),
            page(target.transcript_epoch.as_str()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["protocol_error"]["reason"], "missing_required_feature");

        headers.insert(
            protocol::HTTP_HEADER,
            HeaderValue::from_str(
                &serde_json::json!({
                    "generation": 1,
                    "client_kind": "browser",
                    "required_features": ["http_operational_v1", protocol::HISTORY_VIEW_FEATURE],
                })
                .to_string(),
            )
            .unwrap(),
        );
        let (status, body) = invoke_operation(
            state.clone(),
            headers.clone(),
            page(target.transcript_epoch.as_str()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["result"]["frames"][0]["epoch"],
            target.transcript_epoch
        );
        assert_eq!(body["result"]["has_more"], false);

        // Another session's epoch names no frame in this session's transcript.
        let (status, body) = invoke_operation(
            state.clone(),
            headers.clone(),
            page(other.transcript_epoch.as_str()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let frames = body["result"]["frames"].as_array().unwrap();
        assert!(
            frames.iter().all(|frame| frame["gap"] == true),
            "{frames:?}"
        );

        let (status, body) = invoke_operation(
            state,
            headers,
            WebOperation::HistoryExportText {
                session_id: target.session_id.clone(),
                transcript_epoch: target.transcript_epoch.clone(),
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["result"]["text"]
            .as_str()
            .unwrap()
            .ends_with("exact epoch history\n"));
        assert_eq!(body["result"]["retention"]["complete"], true);
        drop(sink);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn provider_validation_operations_require_their_declared_feature() {
        let (root, app, _, _) = server_test_application();
        let (state, mut headers, _) = browser_state(app);
        let inspect = || WebOperation::ProviderValidationInspect {
            batch_id: "missing".into(),
        };
        let (status, body) = invoke_operation(state.clone(), headers.clone(), inspect()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["protocol_error"]["reason"], "missing_required_feature");

        headers.insert(
            protocol::HTTP_HEADER,
            HeaderValue::from_str(
                &serde_json::json!({
                    "generation": 1,
                    "client_kind": "browser",
                    "required_features": ["http_operational_v1", protocol::PROVIDER_VALIDATION_FEATURE],
                })
                .to_string(),
            )
            .unwrap(),
        );
        let (status, body) = invoke_operation(state, headers, inspect()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.get("protocol_error").is_none(), "{body}");
        assert!(
            body["error"].as_str().unwrap().contains("no longer exists"),
            "{body}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn keyboard_operation(
        operation_id: String,
        session_id: String,
        surface_route_id: String,
        expected_binding_revision: i64,
        expected_control_revision: i64,
    ) -> WebOperation {
        WebOperation::CmuxSetKeyboardControl {
            operation_id,
            session_id,
            surface_route_id,
            expected_binding_revision,
            expected_control_revision,
            action: CmuxKeyboardControlAction::Acquire,
        }
    }

    #[test]
    fn browser_terminal_input_operations_and_lease_bearing_cmux_requests_are_rejected() {
        for kind in [
            "acquire_input",
            "renew_input",
            "takeover_input",
            "send_input",
            "release_input",
            "resize",
        ] {
            assert!(serde_json::from_value::<WebOperation>(serde_json::json!({
                "kind": kind,
                "session_id": "00000000-0000-0000-0000-000000000000",
                "owner_id": "browser",
                "lease": "lease",
                "seconds": 30,
                "data_base64": "YQ==",
                "rows": 24,
                "cols": 80,
            }))
            .is_err());
        }
        assert!(serde_json::from_value::<WebOperation>(serde_json::json!({
            "kind": "cmux_discard_unknown",
            "operation_id": "00000000-0000-0000-0000-000000000001",
            "surface_route_id": "00000000-0000-0000-0000-000000000002",
            "session_id": "00000000-0000-0000-0000-000000000003",
        }))
        .is_ok());
        let keyboard_control = serde_json::json!({
            "kind": "cmux_set_keyboard_control",
            "operation_id": "00000000-0000-0000-0000-000000000004",
            "session_id": "00000000-0000-0000-0000-000000000005",
            "surface_route_id": "00000000-0000-0000-0000-000000000006",
            "expected_binding_revision": 1,
            "expected_control_revision": 0,
            "action": "acquire",
        });
        assert!(serde_json::from_value::<WebOperation>(keyboard_control.clone()).is_ok());
        let mut lease_bearing = keyboard_control.as_object().unwrap().clone();
        lease_bearing.insert("lease".to_owned(), serde_json::json!("browser-secret"));
        assert!(
            serde_json::from_value::<WebOperation>(serde_json::Value::Object(lease_bearing,))
                .is_err()
        );
        let mut legacy_route_field = keyboard_control.as_object().unwrap().clone();
        legacy_route_field.remove("surface_route_id");
        legacy_route_field.insert(
            "route_id".to_owned(),
            serde_json::json!("00000000-0000-0000-0000-000000000006"),
        );
        assert!(
            serde_json::from_value::<WebOperation>(serde_json::Value::Object(legacy_route_field,))
                .is_err()
        );
    }

    #[tokio::test]
    async fn browser_keyboard_control_handler_rejects_foreign_prior_and_stale_routes_without_leases(
    ) {
        let (root, app, target, other) = server_test_application();
        let service_boot_id = app.service_boot_id().to_owned();
        let surface = open_persistent_surface(&app, &service_boot_id, &target);
        app.supervisor
            .install_synthetic_attachment_for_tests(target.clone())
            .unwrap();
        let lease_secret = "server-boundary-never-in-json";
        app.store
            .acquire_input_lease(
                &target.session_id,
                lease_secret,
                "server-boundary-owner",
                &serde_json::to_string(&target.process).unwrap(),
                &target.role_generation_id,
                "2999-01-01T00:00:00Z",
            )
            .unwrap();
        let (state, headers, _shutdown) = browser_state(app.clone());
        let revision = || {
            app.store
                .cmux_session_surface(&surface.id)
                .unwrap()
                .control_revision
        };
        assert_eq!(revision(), 0);

        let (status, body) = invoke_operation(
            state.clone(),
            headers.clone(),
            keyboard_operation(
                uuid::Uuid::new_v4().to_string(),
                other.session_id.clone(),
                surface.id.clone(),
                surface.binding_revision,
                0,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(revision(), 0);
        assert!(!body.to_string().contains(lease_secret));

        let mut prior_boot_state = state.clone();
        prior_boot_state.boot_id = uuid::Uuid::new_v4().to_string();
        let (status, body) = invoke_operation(
            prior_boot_state,
            headers.clone(),
            keyboard_operation(
                uuid::Uuid::new_v4().to_string(),
                target.session_id.clone(),
                surface.id.clone(),
                surface.binding_revision,
                0,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(revision(), 0);
        assert!(!body.to_string().contains(lease_secret));

        let (status, body) = invoke_operation(
            state.clone(),
            headers.clone(),
            keyboard_operation(
                uuid::Uuid::new_v4().to_string(),
                target.session_id.clone(),
                surface.id.clone(),
                surface.binding_revision + 1,
                0,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(revision(), 0);
        assert!(!body.to_string().contains(lease_secret));

        let (status, body) = invoke_operation(
            state,
            headers,
            keyboard_operation(
                uuid::Uuid::new_v4().to_string(),
                target.session_id.clone(),
                surface.id.clone(),
                surface.binding_revision,
                0,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["surface"]["control_revision"], 1);
        assert_eq!(revision(), 1);
        let json = body.to_string();
        assert!(!json.contains(lease_secret));
        assert!(body["result"].get("lease").is_none());
        assert!(body["result"].get("secret").is_none());

        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    async fn invoke_state_wait(
        state: &WebState,
        headers: &HeaderMap,
        query: &str,
    ) -> (StatusCode, HeaderMap, serde_json::Value) {
        let uri: axum::http::Uri = format!("/api/state/wait?{query}").parse().unwrap();
        let response = api_state_wait(
            State(state.clone()),
            headers.clone(),
            Query::try_from_uri(&uri),
        )
        .await;
        let status = response.status();
        let response_headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
        });
        (status, response_headers, body)
    }

    /// Deterministic deadline: runs `action` when the waiter first parks, then
    /// expires on the next poll unless a wake already ended the wait.
    fn expire_after_parking(action: impl FnOnce() + Unpin) -> impl Future<Output = ()> {
        let mut action = Some(action);
        std::future::poll_fn(move |context| match action.take() {
            Some(action) => {
                action();
                context.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            None => std::task::Poll::Ready(()),
        })
    }

    fn insert_project(app: &Application, id: &str) {
        app.store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                 VALUES(?1,?1,'/tmp/state-wait',?1,'base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![id],
            )
            .unwrap();
    }

    fn cursor(state: &WebState, revision: i64) -> StateWaitRequest {
        StateWaitRequest {
            incarnation: state.instance_id.clone(),
            revision,
            timeout: Duration::from_millis(DEFAULT_STATE_WAIT_MILLIS),
        }
    }

    fn wait_json(outcome: StateWait) -> serde_json::Value {
        match outcome {
            StateWait::Respond(response) => serde_json::to_value(response).unwrap(),
            StateWait::ShuttingDown => panic!("state wait unexpectedly reported shutdown"),
        }
    }

    fn waiter_permit(state: &WebState) -> Arc<OwnedSemaphorePermit> {
        Arc::new(
            Arc::clone(&state.state_waiters)
                .try_acquire_owned()
                .unwrap(),
        )
    }

    /// Resolves once every waiter permit is free again, which requires each
    /// blocking read that shares one to have exited.
    async fn waiter_permits_released(state: &WebState) {
        let all = u32::try_from(MAX_STATE_WAITERS).unwrap();
        let released = tokio::time::timeout(
            Duration::from_secs(30),
            state.state_waiters.acquire_many(all),
        );
        drop(released.await.expect("blocked reads exit").unwrap());
    }

    fn poll_once<F: Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    #[tokio::test]
    async fn state_wait_authenticates_before_validating_exact_cursor_bounds() {
        let (root, app, _, _) = server_test_application();
        let (state, headers, _shutdown) = browser_state(app.clone());
        let malformed = "incarnation=&revision=01";
        let mut anonymous = headers.clone();
        anonymous.remove(header::COOKIE);
        let mut foreign_host = headers.clone();
        foreign_host.insert(header::HOST, HeaderValue::from_static("attacker.test"));
        let mut foreign_origin = headers.clone();
        foreign_origin.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://attacker.test"),
        );
        for (rejected, status) in [
            (&anonymous, StatusCode::UNAUTHORIZED),
            (&foreign_host, StatusCode::FORBIDDEN),
            (&foreign_origin, StatusCode::FORBIDDEN),
        ] {
            assert_eq!(
                invoke_state_wait(&state, rejected, malformed).await.0,
                status
            );
        }

        let incarnation = &state.instance_id;
        let oversized = "i".repeat(MAX_INCARNATION_BYTES + 1);
        for query in [
            String::new(),
            format!("incarnation={incarnation}"),
            "incarnation=&revision=0".to_owned(),
            format!("incarnation={oversized}&revision=0"),
            format!("incarnation={incarnation}&revision=01"),
            format!("incarnation={incarnation}&revision=-1"),
            format!("incarnation={incarnation}&revision=%2B1"),
            format!("incarnation={incarnation}&revision=1.0"),
            format!("incarnation={incarnation}&revision=9223372036854775808"),
            format!("incarnation={incarnation}&revision=0&timeout_ms=999"),
            format!("incarnation={incarnation}&revision=0&timeout_ms=25001"),
            format!("incarnation={incarnation}&revision=0&timeout_ms=01000"),
            format!("incarnation={incarnation}&revision=0&revision=1"),
            format!("incarnation={incarnation}&revision=0&since=1"),
        ] {
            let (status, _, body) = invoke_state_wait(&state, &headers, &query).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
            assert!(body["error"].is_string(), "{query}");
        }
        assert_eq!(state.state_waiters.available_permits(), MAX_STATE_WAITERS);

        drop((state, app));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn state_wait_resolves_changed_reset_and_unchanged_cursors_without_writing() {
        let (root, app, _, _) = server_test_application();
        let (state, headers, _shutdown) = browser_state(app.clone());
        let current = app.store.state_revision().unwrap();
        assert!(current > 0, "fixture rows advance the committed revision");
        let incarnation = &state.instance_id;

        let (status, _, changed) = invoke_state_wait(
            &state,
            &headers,
            &format!("incarnation={incarnation}&revision=0"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(changed["outcome"], "state_changed");
        assert_eq!(changed["incarnation"], incarnation.as_str());
        assert_eq!(changed["revision"], current.to_string());
        assert_eq!(changed["state"]["revision"], current.to_string());
        assert_eq!(changed["state"]["incarnation"], incarnation.as_str());

        for query in [
            format!("incarnation={incarnation}&revision={}", current + 1),
            format!("incarnation={incarnation}&revision={}", i64::MAX),
            format!("incarnation=previous-service&revision={current}&timeout_ms=1000"),
        ] {
            let (status, _, reset) = invoke_state_wait(&state, &headers, &query).await;
            assert_eq!(status, StatusCode::OK, "{query}");
            assert_eq!(reset["outcome"], "reset", "{query}");
            assert_eq!(reset["incarnation"], incarnation.as_str(), "{query}");
            assert_eq!(reset["state"]["revision"], current.to_string(), "{query}");
        }

        let mut shutdown = state.shutdown.clone();
        let unchanged = wait_json(
            wait_for_state(
                &state,
                &cursor(&state, current),
                &waiter_permit(&state),
                &mut shutdown,
                std::future::ready(()),
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            unchanged,
            serde_json::json!({"outcome":"unchanged","incarnation":incarnation,"revision":current.to_string()})
        );

        let response = api_state(State(state.clone()), headers.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot["incarnation"], incarnation.as_str());
        assert_eq!(snapshot["revision"], current.to_string());
        assert_eq!(snapshot["schema"], 8);
        let keys = |value: &serde_json::Value| {
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&snapshot), keys(&changed["state"]));
        assert_eq!(
            app.store.state_revision().unwrap(),
            current,
            "reads never write"
        );

        drop((state, app));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn parked_state_wait_wakes_on_coalesced_store_commits_and_closes_on_shutdown() {
        let (root, app, _, _) = server_test_application();
        let (state, headers, shutdown_tx) = browser_state(app.clone());
        let current = app.store.state_revision().unwrap();
        let mut shutdown = state.shutdown.clone();

        let lock_free_while_parked = std::cell::Cell::new(false);
        let timed_out = wait_json(
            wait_for_state(
                &state,
                &cursor(&state, current),
                &waiter_permit(&state),
                &mut shutdown,
                expire_after_parking(|| {
                    lock_free_while_parked.set(app.store.connection.try_lock().is_ok())
                }),
            )
            .await
            .unwrap(),
        );
        assert!(
            lock_free_while_parked.get(),
            "a parked waiter holds no store lock"
        );
        assert_eq!(timed_out["outcome"], "unchanged");

        let woken = wait_json(
            wait_for_state(
                &state,
                &cursor(&state, current),
                &waiter_permit(&state),
                &mut shutdown,
                expire_after_parking(|| {
                    insert_project(&app, "wake-first");
                    insert_project(&app, "wake-second");
                }),
            )
            .await
            .unwrap(),
        );
        assert_eq!(woken["outcome"], "state_changed");
        assert_eq!(woken["revision"], (current + 2).to_string());
        let projects = woken["state"]["projects"].as_array().unwrap();
        for id in ["wake-first", "wake-second"] {
            assert!(projects.iter().any(|project| project["id"] == id), "{id}");
        }

        let closing = wait_for_state(
            &state,
            &cursor(&state, current + 2),
            &waiter_permit(&state),
            &mut shutdown,
            expire_after_parking(|| shutdown_tx.send(true).unwrap()),
        )
        .await
        .unwrap();
        assert!(matches!(closing, StateWait::ShuttingDown));
        let (status, response_headers, _) = invoke_state_wait(
            &state,
            &headers,
            &format!("incarnation={}&revision=0", state.instance_id),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response_headers[header::RETRY_AFTER], "1");
        assert_eq!(state.state_waiters.available_permits(), MAX_STATE_WAITERS);

        drop((state, app));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn state_waiter_limit_rejects_excess_and_cancellation_frees_permits_after_reads_exit() {
        let (root, app, _, _) = server_test_application();
        let (state, headers, _shutdown) = browser_state(app.clone());
        let incarnation = state.instance_id.clone();
        let older = format!("incarnation={incarnation}&revision=0");

        let held = state
            .state_waiters
            .try_acquire_many(MAX_STATE_WAITERS as u32)
            .unwrap();
        let (status, response_headers, body) = invoke_state_wait(&state, &headers, &older).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response_headers[header::RETRY_AFTER], "1");
        assert!(body["error"].is_string());
        drop(held);
        assert_eq!(
            invoke_state_wait(&state, &headers, &older).await.2["outcome"],
            "state_changed"
        );

        // A parked waiter has no read in flight, so cancelling it frees its
        // permit at once.
        let current = app.store.state_revision().unwrap();
        let request = cursor(&state, current);
        let permit = waiter_permit(&state);
        let parked = std::cell::Cell::new(false);
        {
            let mut shutdown = state.shutdown.clone();
            let waiting = wait_for_state(
                &state,
                &request,
                &permit,
                &mut shutdown,
                std::future::poll_fn(|_| {
                    parked.set(true);
                    std::task::Poll::Pending
                }),
            );
            let mut waiting = std::pin::pin!(waiting);
            std::future::poll_fn(|context| {
                assert!(waiting.as_mut().poll(context).is_pending());
                if parked.get() {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;
        }
        drop(permit);
        assert_eq!(state.state_waiters.available_permits(), MAX_STATE_WAITERS);

        // Cancelled while its read waits for the store lock, a waiter keeps its
        // permit until that read exits.
        let equal: axum::http::Uri =
            format!("/api/state/wait?incarnation={incarnation}&revision={current}")
                .parse()
                .unwrap();
        let store_lock = app.store.connection.lock().unwrap();
        {
            let pending = api_state_wait(
                State(state.clone()),
                headers.clone(),
                Query::try_from_uri(&equal),
            );
            assert!(poll_once(std::pin::pin!(pending)).is_pending());
        }
        assert_eq!(
            state.state_waiters.available_permits(),
            MAX_STATE_WAITERS - 1
        );
        drop(store_lock);
        waiter_permits_released(&state).await;

        drop((state, app));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn state_wait_shutdown_during_blocked_reads_answers_503_and_keeps_their_permit() {
        let (root, app, _, _) = server_test_application();
        // A stale cursor blocks in its revision read; another incarnation's
        // cursor blocks while building its reset snapshot.
        for foreign in [false, true] {
            let (state, headers, shutdown_tx) = browser_state(app.clone());
            let incarnation = if foreign {
                "previous-service"
            } else {
                state.instance_id.as_str()
            };
            let stale: axum::http::Uri =
                format!("/api/state/wait?incarnation={incarnation}&revision=0")
                    .parse()
                    .unwrap();
            let store_lock = app.store.connection.lock().unwrap();
            let pending = api_state_wait(
                State(state.clone()),
                headers.clone(),
                Query::try_from_uri(&stale),
            );
            let mut pending = std::pin::pin!(pending);
            assert!(poll_once(pending.as_mut()).is_pending());
            shutdown_tx.send(true).unwrap();
            let std::task::Poll::Ready(response) = poll_once(pending.as_mut()) else {
                panic!("shutdown left the {incarnation} wait blocked on its read");
            };
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{incarnation}"
            );
            assert_eq!(response.headers()[header::RETRY_AFTER], "1");
            assert_eq!(
                state.state_waiters.available_permits(),
                MAX_STATE_WAITERS - 1,
                "{incarnation}"
            );
            drop(store_lock);
            waiter_permits_released(&state).await;
        }

        // A response that completes as shutdown begins is still withheld.
        let (state, _, shutdown_tx) = browser_state(app.clone());
        let mut shutdown = state.shutdown.clone();
        let closing = wait_for_state(
            &state,
            &cursor(&state, app.store.state_revision().unwrap()),
            &waiter_permit(&state),
            &mut shutdown,
            async { shutdown_tx.send(true).unwrap() },
        )
        .await
        .unwrap();
        assert!(matches!(closing, StateWait::ShuttingDown));

        drop((state, app));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn state_snapshot_cursor_matches_its_rows_under_concurrent_commits() {
        let (root, app, _, _) = server_test_application();
        let (state, _, _shutdown) = browser_state(app.clone());
        let baseline = blocking_snapshot(&state).await.unwrap().state;
        let baseline_revision: i64 = baseline.revision.parse().unwrap();
        let committed = 40;
        let writer_app = app.clone();
        let writer = std::thread::spawn(move || {
            for index in 0..committed {
                insert_project(&writer_app, &format!("concurrent-{index}"));
            }
        });
        let mut observed = Vec::new();
        loop {
            let finished = writer.is_finished();
            let snapshot = blocking_snapshot(&state).await.unwrap().state;
            let revision: i64 = snapshot.revision.parse().unwrap();
            // One committed row per revision step: the cursor must describe
            // exactly the rows projected with it.
            assert_eq!(
                snapshot.projects.len() - baseline.projects.len(),
                usize::try_from(revision - baseline_revision).unwrap()
            );
            observed.push(revision);
            if finished {
                break;
            }
        }
        writer.join().unwrap();
        assert_eq!(observed.last(), Some(&(baseline_revision + committed)));
        assert!(observed.windows(2).all(|pair| pair[0] <= pair[1]));

        drop((state, app));
        let _ = std::fs::remove_dir_all(root);
    }
}

async fn api_permission_coverage(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(sample): Json<crate::domain::PermissionCoverageSample>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    if !declared_features(&headers)
        .iter()
        .any(|f| f == protocol::TASK_INTERACTIONS_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(
                serde_json::json!({"error":"Reload the dashboard to preview permission coverage."}),
            ),
        )
            .into_response();
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || crate::permissions::coverage_preview(&store, &sample))
        .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct ActivityQuery {
    cursor: Option<String>,
}

fn require_history_insights(state: &WebState, headers: &HeaderMap) -> Result<(), Response> {
    require_browser(state, headers)?;
    if !declared_features(headers)
        .iter()
        .any(|f| f == protocol::HISTORY_INSIGHTS_FEATURE)
    {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"Reload the dashboard to use History search and usage comparisons."
            })),
        )
            .into_response());
    }
    Ok(())
}

async fn api_history_search(
    State(state): State<WebState>,
    axum::extract::Query(query): axum::extract::Query<crate::domain::HistorySearchQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_history_insights(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || {
        crate::workflow::history_search(&store, query, &state.instance_id)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn api_historical_source(
    State(state): State<WebState>,
    axum::extract::Query(key): axum::extract::Query<crate::domain::HistoricalSourceKey>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_history_insights(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || crate::workflow::historical_source(&store, &key)).await {
        Ok(Ok(Some(value))) => Json(value).into_response(),
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"This exact historical source is unavailable. Refresh the search results."}))).into_response(),
        Ok(Err(error)) => (StatusCode::CONFLICT, Json(serde_json::json!({"error":error.to_string()}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":error.to_string()}))).into_response(),
    }
}

async fn api_usage_comparisons(
    State(state): State<WebState>,
    axum::extract::Query(query): axum::extract::Query<crate::domain::UsageComparisonQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_history_insights(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || {
        crate::workflow::usage_comparisons(&store, query, &state.instance_id)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}
fn require_workflow_comparisons(state: &WebState, headers: &HeaderMap) -> Result<(), Response> {
    require_browser(state, headers)?;
    if !declared_features(headers)
        .iter()
        .any(|f| f == protocol::WORKFLOW_COMPARISONS_FEATURE)
    {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"Reload the dashboard to use workflow comparisons."
            })),
        )
            .into_response());
    }
    Ok(())
}

async fn api_workflow_comparisons(
    State(state): State<WebState>,
    axum::extract::Query(query): axum::extract::Query<crate::domain::WorkflowComparisonQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_workflow_comparisons(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || {
        crate::comparisons::workflow_comparisons(&store, query, &state.instance_id)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn api_comparison_evidence(
    State(state): State<WebState>,
    axum::extract::Query(query): axum::extract::Query<crate::domain::ComparisonEvidenceQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_workflow_comparisons(&state, &headers) {
        return response;
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || {
        crate::comparisons::comparison_evidence(&store, query, &state.instance_id)
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn api_task_activity(
    State(state): State<WebState>,
    AxumPath(task_id): AxumPath<String>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    if !declared_features(&headers)
        .iter()
        .any(|f| f == protocol::TASK_INTERACTIONS_FEATURE)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":"Reload the dashboard to read task activity."})),
        )
            .into_response();
    }
    let store = state.app.store.clone();
    match tokio::task::spawn_blocking(move || {
        crate::workflow::task_activity(&store, &task_id, query.cursor.as_deref())
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}
