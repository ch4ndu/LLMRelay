use crate::auth;
use crate::config::{atomic_write, InstancePaths};
use crate::control;
use crate::domain::{
    AppStateDto, CapabilityProofInput, CmuxKeyboardControlAction, HumanCommand, RoleKind,
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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));
}

const MAX_STATE_WAITERS: usize = 64;
const MAX_INCARNATION_BYTES: usize = 128;
const DEFAULT_STATE_WAIT_MILLIS: u64 = 25_000;
const STATE_WAIT_MILLIS: RangeInclusive<u64> = 1_000..=25_000;

#[derive(Clone)]
struct WebState {
    app: Application,
    instance_id: String,
    boot_id: String,
    allowed_hosts: Vec<String>,
    bootstrap_hash: Arc<Mutex<Option<String>>>,
    browser_sessions: Arc<Mutex<HashSet<String>>>,
    shutdown: watch::Receiver<bool>,
    state_waiters: Arc<Semaphore>,
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

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
enum WebOperation {
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
        proof: CapabilityProofInput,
    },
}

pub async fn serve(paths: InstancePaths, requested_port: u16, open_browser: bool) -> Result<()> {
    paths.create()?;
    let lock = crate::database::InstanceLock::acquire(&paths)?;
    crate::database::recover_interrupted_restore_locked(&paths)?;
    crate::database::validate_live_database_family(&paths)?;
    let store = Store::open_service(&paths.database)?;
    let executable = std::env::current_exe().context("resolve LLMRelay executable")?;
    let app = Application::new(paths.clone(), store, executable)?;
    let restore_held = crate::database::hold_active(&app.store)?;
    let retired_cmux_presentations = app
        .store
        .retire_prior_cmux_presentation_boots(app.service_boot_id())?;
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
    let listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        requested_port,
    ))
    .await
    .with_context(|| format!("bind loopback port {requested_port}"))?;
    let bound = listener.local_addr()?;
    let control_listener = control::bind(&app)?;
    let role_listener = crate::task_cli::bind(&app)?;
    let instance_id = uuid::Uuid::new_v4().to_string();
    let boot_id = app.service_boot_id().to_owned();
    let bootstrap = auth::issue_secret();
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
    atomic_write(&paths.instance_file, &serde_json::to_vec_pretty(&record)?)?;
    append_startup_event(&paths, &record)?;

    println!("LLMRelay {}", crate::VERSION);
    println!("Instance: {} (boot {})", record.instance_id, record.boot_id);
    println!("Data: {}", paths.root.display());
    println!("Logs: {}", paths.logs.display());
    println!("Role socket: {}", paths.role_socket.display());

    let clean_dashboard_url = record.address.clone();
    let bootstrap_url = format!("{}/#bootstrap={}", record.address, bootstrap);

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let web_state = WebState {
        app: app.clone(),
        instance_id: instance_id.clone(),
        boot_id: boot_id.clone(),
        allowed_hosts: vec![
            format!("127.0.0.1:{}", bound.port()),
            format!("localhost:{}", bound.port()),
        ],
        bootstrap_hash: Arc::new(Mutex::new(Some(crate::auth::hash_secret(&bootstrap)))),
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
    tokio::spawn(async move {
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
    let coordinator_shutdown = shutdown_tx.clone();
    let coordinator_failure = failure_tx.clone();
    let mut reconcile_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let service_started_at = chrono::Utc::now();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut intake_error_reported = false;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let tick_app=coordinator_app.clone();
                    match tokio::task::spawn_blocking(move||{
                        let coordinator = tick_app.coordinator_tick();
                        let intake = tick_app.scheduled_intake_tick(chrono::Utc::now(), service_started_at);
                        (coordinator, intake)
                    }).await {
                        Ok((coordinator, intake))=>{
                            if let Err(error) = intake {
                                if !intake_error_reported {
                                    tracing::warn!(error=%error,"scheduled intake deferred");
                                    let _=coordinator_app.diagnostics.record("warn","recipe.intake","recipe_schedule","deferred",None,serde_json::json!({"cause":format!("{error:#}")}));
                                    intake_error_reported = true;
                                }
                            } else {
                                intake_error_reported = false;
                            }
                            match coordinator {
                        Ok(result)=>{
                            if result.get("action").and_then(|value|value.as_str())!=Some("idle"){
                                let _=coordinator_app.diagnostics.record("info","coordinator.tick","coordinator","success",None,result);
                            }
                            match coordinator_app.drain_status(){
                                Ok(status) if status.get("draining")==Some(&serde_json::Value::Bool(true))&&status.get("quiescent")==Some(&serde_json::Value::Bool(true))=>{let _=coordinator_shutdown.send(true);break},
                                Ok(_)=>{},
                                Err(error)=>tracing::warn!(error=%error,"drain reconciliation failed"),
                            }
                        }
                        Err(error)=>{tracing::warn!(error=%error,"coordinator action deferred");let _=coordinator_app.diagnostics.record("warn","coordinator.tick","coordinator","deferred",None,serde_json::json!({"cause":format!("{error:#}")}));}
                            }
                        }
                        Err(error)=>{let _=coordinator_failure.send(Some(format!("coordinator task failed: {error}")));let _=coordinator_shutdown.send(true);break}
                    }
                }
                changed = reconcile_shutdown.changed() => { if changed.is_err() || *reconcile_shutdown.borrow() { break } }
            }
        }
    });
    let mut opener_shutdown = shutdown_rx.clone();
    let opener_task = if open_browser {
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
        println!("Automatic browser opening disabled by --no-open.");
        println!("Manual dashboard login: {bootstrap_url}");
        None
    };
    let signal_shutdown = shutdown_tx.clone();
    let signal_app = app.clone();
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
            match signal_app.begin_drain() {
                Ok(status) if status.get("active").and_then(|value|value.as_array()).is_some_and(Vec::is_empty)
                    && status.get("active_checks").and_then(|value|value.as_array()).is_some_and(Vec::is_empty)
                    && status.get("unknown").and_then(|value|value.as_array()).is_some_and(Vec::is_empty)=>{let _=signal_shutdown.send(true);break}
                Ok(status)=>eprintln!("LLMRelay is draining managed work before shutdown: {status}"),
                Err(error)=>eprintln!("LLMRelay kept running because process ownership could not be reconciled: {error:#}"),
            }
        }
    });
    axum::serve(listener, router)
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
        .await?;

    if let Some(task) = opener_task {
        let _ = task.await;
    }

    let listener_failure = failure_tx.borrow().clone();
    let final_record = InstanceRecord {
        status: if listener_failure.is_some() {
            "unhealthy".to_owned()
        } else {
            "stopped".to_owned()
        },
        ..record
    };
    atomic_write(
        &paths.instance_file,
        &serde_json::to_vec_pretty(&final_record)?,
    )?;
    drop(lock);
    if let Some(error) = listener_failure {
        Err(anyhow!(error))
    } else {
        Ok(())
    }
}

fn web_router(web_state: WebState) -> Router {
    let operational = Router::new()
        .route("/api/health", get(health))
        .route("/api/state", get(api_state))
        .route("/api/state/wait", get(api_state_wait))
        .route("/api/restart-preview", get(api_restart_preview))
        .route("/api/model-catalog", get(api_model_catalog))
        .route(
            "/api/tasks/{task_id}/role-preparations",
            get(api_role_preparations),
        )
        .route("/api/command", post(api_command))
        .route("/api/operation", post(api_operation))
        .route("/api/diagnostics", get(api_diagnostics))
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

#[cfg(target_os = "macos")]
async fn open_default_browser(url: &str) -> Result<()> {
    let mut child = Command::new("/usr/bin/open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("launch /usr/bin/open")?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
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
        if tokio::time::Instant::now() >= deadline {
            if let Err(cleanup_error) = terminate_and_reap_browser_opener(&mut child) {
                bail!("/usr/bin/open timed out and cleanup also failed: {cleanup_error:#}")
            }
            bail!("/usr/bin/open did not finish within 3 seconds")
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
async fn open_default_browser(_url: &str) -> Result<()> {
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
    let supplied_hash = crate::auth::hash_secret(&request.token);
    let mut bootstrap = match state.bootstrap_hash.lock() {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "browser authentication state unavailable",
            )
                .into_response()
        }
    };
    let Some(expected) = bootstrap.as_ref() else {
        return (
            StatusCode::UNAUTHORIZED,
            "bootstrap credential was already consumed",
        )
            .into_response();
    };
    if crate::auth::verify_secret(&request.token, expected).is_err() || supplied_hash != *expected {
        return (StatusCode::UNAUTHORIZED, "invalid bootstrap credential").into_response();
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
    *bootstrap = None;
    let mut response = Json(serde_json::json!({"authenticated": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!("agenticjira_session={session}; HttpOnly; SameSite=Strict; Path=/")
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
    Json(crate::model_catalog::read(&query.provider)).into_response()
}

async fn api_role_preparations(
    State(state): State<WebState>,
    AxumPath(task_id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":format!("resolve current LLMRelay executable: {error}")})),
            )
                .into_response()
        }
    };
    match crate::workflow::role_preparations(
        &state.app.store,
        &task_id,
        &state.app.hooks,
        &state.app.paths.role_socket,
        &executable,
    ) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}

async fn api_command(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(command): Json<HumanCommand>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    match state.app.execute_human_command(&command) {
        Ok(value) => Json(serde_json::json!({"result":value})).into_response(),
        Err(error) => {
            let _ = state.app.diagnostics.record(
                "warn",
                "human.command.rejected",
                "server",
                "rejected",
                Some(command.operation_id()),
                serde_json::json!({"cause":format!("{error:#}")}),
            );
            (StatusCode::CONFLICT, Json(serde_json::json!({"error":format!("{error:#}"),"operation_id":command.operation_id()}))).into_response()
        }
    }
}

async fn api_diagnostics(State(state): State<WebState>, headers: HeaderMap) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    match state.app.diagnostics.read_sanitized(500) {
        Ok(events) => Json(serde_json::json!({"events":events})).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
        )
            .into_response(),
    }
}

async fn api_operation(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(operation): Json<WebOperation>,
) -> Response {
    if let Err(response) = require_browser(&state, &headers) {
        return response;
    }
    let permit = match Arc::clone(&state.app.blocking_operations)
        .acquire_owned()
        .await
    {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"blocking operation admission is unavailable"})),
            )
                .into_response()
        }
    };
    let blocking_state = state.clone();
    let result = match tokio::task::spawn_blocking(move || -> Result<serde_json::Value> {
        let _permit = permit;
        let state = blocking_state;
        match operation {
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
            } => Ok(serde_json::to_value(
                state.app.dispatch_trip_setup_role_with_operation(
                    &operation_id,
                    &attempt_id,
                    role,
                    fresh_resume_rejection.as_ref(),
                )?,
            )?),
            WebOperation::RuntimeProbeLaunch {
                operation_id,
                admission_id,
                role,
            } => Ok(serde_json::to_value(state.app.dispatch_runtime_probe(
                &operation_id,
                &admission_id,
                role,
            )?)?),
            WebOperation::RuntimeProbeResume {
                operation_id,
                admission_id,
                role,
            } => Ok(serde_json::to_value(
                state.app.resume_runtime_probe_with_operation(
                    &operation_id,
                    &admission_id,
                    role,
                )?,
            )?),
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
            } => Ok(serde_json::to_value(
                state.app.resume_role_session_with_operation(
                    &operation_id,
                    &session_id,
                    &prompt,
                )?,
            )?),
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
        }
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(anyhow!("web operation blocking task failed: {error}")),
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
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":format!("{error:#}")})),
            )
                .into_response()
        }
    }
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
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let Some(value) = cookie
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("agenticjira_session="))
    else {
        return false;
    };
    let hash = hex::encode(Sha256::digest(value.as_bytes()));
    state
        .browser_sessions
        .lock()
        .map(|sessions| sessions.contains(&hash))
        .unwrap_or(false)
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
            app,
            instance_id: "server-boundary-instance".to_owned(),
            allowed_hosts: vec!["server-boundary.test".to_owned()],
            bootstrap_hash: Arc::new(Mutex::new(None)),
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
            HeaderValue::from_static("agenticjira_session=server-boundary-browser-session"),
        );
        (state, headers, shutdown_tx)
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
            api_protocol(State(state.clone()), unauthenticated)
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

        let (root, app, _, _) = server_test_application();
        let (state, _, _) = browser_state(app);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            tokio::spawn(async move { axum::serve(listener, web_router(state)).await.unwrap() });
        for (method, path) in [
            ("GET", "/api/health"),
            ("GET", "/api/state"),
            ("GET", "/api/state/wait?revision=invalid"),
            ("GET", "/api/restart-preview"),
            ("GET", "/api/model-catalog?provider=invalid"),
            ("GET", "/api/tasks/fixture/role-preparations"),
            ("GET", "/api/diagnostics"),
            ("POST", "/api/command"),
            ("POST", "/api/operation"),
        ] {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: server-boundary.test\r\nCookie: agenticjira_session=server-boundary-browser-session\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{{"
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            let text = String::from_utf8(response).unwrap();
            assert!(text.starts_with("HTTP/1.1 409"), "{path}: {text}");
            assert!(text.contains("\"reason\":\"missing\""), "{path}: {text}");
        }
        server.abort();
        let _ = std::fs::remove_dir_all(root);
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
