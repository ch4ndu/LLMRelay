use crate::auth;
use crate::config::{atomic_write, InstancePaths};
use crate::control;
use crate::domain::{CapabilityProofInput, CmuxKeyboardControlAction, HumanCommand, RoleKind};
use crate::operations::Application;
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::watch;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));
}

#[derive(Clone)]
struct WebState {
    app: Application,
    instance_id: String,
    boot_id: String,
    allowed_hosts: Vec<String>,
    bootstrap_hash: Arc<Mutex<Option<String>>>,
    browser_sessions: Arc<Mutex<HashSet<String>>>,
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
    let lock = acquire_instance_lock(&paths)?;
    let store = Store::open(&paths.database)?;
    let executable = std::env::current_exe().context("resolve LLMRelay executable")?;
    let app = Application::new(paths.clone(), store, executable)?;
    let retired_cmux_presentations = app
        .store
        .retire_prior_cmux_presentation_boots(app.service_boot_id())?;
    let recovery = crate::recovery::reconcile_prior_boot(&app.store)?;
    app.supervisor.reconcile()?;
    let restart_candidates = crate::recovery::prepare_restart_candidates(&app.store)?;
    let restart_reconciliation = crate::recovery::reconcile_restart_candidates(&app.store)?;
    let interrupted_applies = crate::trip::reconcile_interrupted_applies(&app.store)?;
    if !recovery.is_empty()
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
            serde_json::json!({"sessions":recovery,"restart_candidates":restart_candidates,"restart_reconciliation":restart_reconciliation,"interrupted_applies":interrupted_applies,"retired_cmux_presentations":retired_cmux_presentations}),
        )?;
    }
    let _ = app.scheduler.reconcile_unknown()?;
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
    };
    let router = Router::new()
        .route("/", get(index))
        .route("/api/bootstrap", post(bootstrap_session))
        .route("/api/health", get(health))
        .route("/api/state", get(api_state))
        .route("/api/model-catalog", get(api_model_catalog))
        .route(
            "/api/tasks/{task_id}/role-preparations",
            get(api_role_preparations),
        )
        .route("/api/command", post(api_command))
        .route("/api/operation", post(api_operation))
        .route("/api/diagnostics", get(api_diagnostics))
        .route("/{*asset}", get(asset))
        .fallback(not_found)
        .with_state(web_state);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
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
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let tick_app=coordinator_app.clone();
                    match tokio::task::spawn_blocking(move||tick_app.coordinator_tick()).await {
                        Ok(Ok(result))=>{
                            if result.get("action").and_then(|value|value.as_str())!=Some("idle"){
                                let _=coordinator_app.diagnostics.record("info","coordinator.tick","coordinator","success",None,result);
                            }
                            match coordinator_app.drain_status(){
                                Ok(status) if status.get("draining")==Some(&serde_json::Value::Bool(true))&&status.get("quiescent")==Some(&serde_json::Value::Bool(true))=>{let _=coordinator_shutdown.send(true);break},
                                Ok(_)=>{},
                                Err(error)=>tracing::warn!(error=%error,"drain reconciliation failed"),
                            }
                        }
                        Ok(Err(error))=>{tracing::warn!(error=%error,"coordinator action deferred");let _=coordinator_app.diagnostics.record("warn","coordinator.tick","coordinator","deferred",None,serde_json::json!({"cause":format!("{error:#}")}));}
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

fn acquire_instance_lock(paths: &InstancePaths) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&paths.lock_file)?;
    file.try_lock_exclusive().map_err(|error| {
        anyhow!(
            "another LLMRelay instance owns {}: {error}",
            paths.root.display()
        )
    })?;
    Ok(file)
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
    match crate::workflow::state(&state.app.store) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":format!("{error:#}")})),
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
    let result = (|| -> Result<serde_json::Value> {
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
            } => Ok(serde_json::to_value(crate::transcript::read_frames(
                &state.app.paths.transcripts,
                &session_id,
                after_epoch.as_deref(),
                after_sequence,
                limit_bytes.min(1024 * 1024),
            )?)?),
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
    })();
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

    fn browser_state(app: Application) -> (WebState, HeaderMap) {
        let browser_secret = "server-boundary-browser-session";
        let browser_hash = hex::encode(Sha256::digest(browser_secret.as_bytes()));
        let state = WebState {
            boot_id: app.service_boot_id().to_owned(),
            app,
            instance_id: "server-boundary-instance".to_owned(),
            allowed_hosts: vec!["server-boundary.test".to_owned()],
            bootstrap_hash: Arc::new(Mutex::new(None)),
            browser_sessions: Arc::new(Mutex::new(HashSet::from([browser_hash]))),
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
        (state, headers)
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
        let (state, headers) = browser_state(app.clone());
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
}
