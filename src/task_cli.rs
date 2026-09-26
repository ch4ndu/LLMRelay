use crate::domain::{HookEnvelope, RoleResultReport};
use crate::operations::Application;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Semaphore};

const MAX_FIRST_FRAME_CONNECTIONS: usize = 64;
const FIRST_FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleRequest {
    pub credential: String,
    #[serde(flatten)]
    pub operation: RoleOperation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoleOperation {
    Context,
    Report {
        report: RoleResultReport,
    },
    Hook {
        envelope: HookEnvelope,
    },
    PermissionRequest {
        envelope: HookEnvelope,
    },
    ProposeTransition {
        operation_id: String,
        phase: String,
        evidence: Vec<String>,
    },
    AcknowledgeGuidance {
        guidance_id: String,
    },
    SetupRead {
        relative_path: String,
    },
    RecordExplorerDecision {
        input: serde_json::Value,
    },
    ConfigureLanes {
        input: serde_json::Value,
    },
    RequestIntegration {
        input: serde_json::Value,
    },
    YieldLane {
        input: serde_json::Value,
    },
    SelectChecks {
        input: serde_json::Value,
    },
    SubmitConformance {
        input: serde_json::Value,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoleResponse {
    pub ok: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

pub fn bind(app: &Application) -> Result<UnixListener> {
    let socket = &app.paths.role_socket;
    remove_stale_socket(socket)?;
    let listener = UnixListener::bind(socket)
        .with_context(|| format!("bind role socket {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve_bound(
    app: Application,
    listener: UnixListener,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    serve_bound_with_limits(
        app,
        listener,
        stop,
        MAX_FIRST_FRAME_CONNECTIONS,
        FIRST_FRAME_DEADLINE,
    )
    .await
}

async fn serve_bound_with_limits(
    app: Application,
    listener: UnixListener,
    mut stop: watch::Receiver<bool>,
    max_first_frame_connections: usize,
    first_frame_deadline: std::time::Duration,
) -> Result<()> {
    let first_frame_admission = Arc::new(Semaphore::new(max_first_frame_connections));
    loop {
        let permit = tokio::select! {
            permit = Arc::clone(&first_frame_admission).acquire_owned() => {
                permit.map_err(|_| anyhow::anyhow!("role first-frame admission closed"))?
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { return Ok(()) }
                continue
            }
        };
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { return Ok(()) }
                continue
            }
        };
        let app = app.clone();
        let connection_stop = stop.clone();
        tokio::spawn(async move {
            if let Err(error) =
                handle(stream, app, connection_stop, first_frame_deadline, permit).await
            {
                tracing::warn!(error = %error, "role request failed");
            }
        });
    }
}

async fn handle(
    stream: UnixStream,
    app: Application,
    mut stop: watch::Receiver<bool>,
    first_frame_deadline: std::time::Duration,
    first_frame_permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<()> {
    let peer_pid = peer_pid(&stream)?;
    let connection_nonce = uuid::Uuid::new_v4().to_string();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let first_frame_result = {
        let mut bounded_reader = (&mut reader).take(1024 * 1024);
        let first_frame =
            tokio::time::timeout(first_frame_deadline, bounded_reader.read_line(&mut line));
        tokio::pin!(first_frame);
        loop {
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return Ok(())
                    }
                }
                result = &mut first_frame => break result,
            }
        }
    };
    first_frame_result.map_err(|_| anyhow::anyhow!("role first-frame deadline expired"))??;
    drop(first_frame_permit);
    let request: RoleRequest = serde_json::from_str(&line)?;
    let operation_kind = match &request.operation {
        RoleOperation::Context => "context",
        RoleOperation::Report { .. } => "report",
        RoleOperation::Hook { .. } => "hook",
        RoleOperation::PermissionRequest { .. } => "permission_request",
        RoleOperation::ProposeTransition { .. } => "transition_proposal",
        RoleOperation::AcknowledgeGuidance { .. } => "guidance_acknowledgment",
        RoleOperation::SetupRead { .. } => "trip_setup_read",
        RoleOperation::RecordExplorerDecision { .. } => "trip_explorer_decision",
        RoleOperation::ConfigureLanes { .. } => "trip_lane_configuration",
        RoleOperation::RequestIntegration { .. } => "trip_integration_request",
        RoleOperation::YieldLane { .. } => "trip_lane_yield",
        RoleOperation::SelectChecks { .. } => "trip_check_selection",
        RoleOperation::SubmitConformance { .. } => "trip_conformance",
    };
    if !reader.buffer().is_empty() {
        crate::permissions::expire_connection(
            &app.store,
            &connection_nonce,
            "permission IPC request contained trailing bytes",
        )?;
        bail!("role IPC accepts exactly one newline-framed request per connection")
    }
    let dispatch = dispatch(request, &app, peer_pid, &connection_nonce);
    tokio::pin!(dispatch);
    let mut trailing = [0_u8; 1];
    let dispatched = tokio::select! {
        biased;
        read = reader.read(&mut trailing) => {
            let (reason, message) = match read {
                Ok(0) => (
                    "permission IPC connection disconnected before response delivery",
                    "role IPC client disconnected before response delivery".to_owned(),
                ),
                Ok(_) => (
                    "permission IPC request contained trailing bytes",
                    "role IPC accepts exactly one newline-framed request per connection".to_owned(),
                ),
                Err(error) => (
                    "permission IPC connection failed before response delivery",
                    format!("role IPC connection failed before response delivery: {error}"),
                ),
            };
            crate::permissions::expire_connection(&app.store, &connection_nonce, reason)?;
            bail!("{message}")
        }
        result = &mut dispatch => result,
    };
    let response = match dispatched {
        Ok(result) => RoleResponse {
            ok: true,
            result,
            error: None,
        },
        Err(error) => {
            if let Err(sink_error) = app.diagnostics.record(
                "warn", "role.ipc.rejected", "task_cli", "rejected", None,
                serde_json::json!({"peer_pid": peer_pid, "operation_kind": operation_kind, "cause": format!("{error:#}")}),
            ) {
                let _ = crate::diagnostics::write_degraded_marker(
                    app.diagnostics.root(), &format!("role IPC rejection log failed: {sink_error:#}"),
                );
            }
            RoleResponse {
                ok: false,
                result: serde_json::Value::Null,
                error: Some(format!("{error:#}")),
            }
        }
    };
    let encoded = match serde_json::to_vec(&response) {
        Ok(encoded) => encoded,
        Err(error) => {
            crate::permissions::mark_response_delivery_unknown(
                &app.store,
                &connection_nonce,
                "permission response serialization failed before local delivery completed",
            )?;
            return Err(error.into());
        }
    };
    let write_result = async {
        writer.write_all(&encoded).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    }
    .await;
    if let Err(error) = write_result {
        crate::permissions::mark_response_delivery_unknown(
            &app.store,
            &connection_nonce,
            "permission response socket write or flush failed; delivery is unknown and replay is prohibited",
        )?;
        return Err(error.into());
    }
    crate::permissions::mark_response_delivered(&app.store, &connection_nonce)?;
    Ok(())
}

async fn dispatch(
    request: RoleRequest,
    app: &Application,
    peer_pid: u32,
    connection_nonce: &str,
) -> Result<serde_json::Value> {
    let initial_context = app.store.role_context(&request.credential)?;
    app.supervisor
        .await_role_attachment(
            &initial_context.session_id,
            &initial_context.role_generation_id,
        )
        .await?;
    let context = app.store.role_context(&request.credential)?;
    if context.session_id != initial_context.session_id
        || context.role_generation_id != initial_context.role_generation_id
        || context.credential_id != initial_context.credential_id
        || context.transcript_epoch != initial_context.transcript_epoch
    {
        bail!("role credential changed session, generation, or invocation during supervisor attachment")
    }
    let provenance = app.supervisor.validate_role_peer(
        &context.session_id,
        &context.role_generation_id,
        peer_pid,
    )?;
    match request.operation {
        RoleOperation::Context => app.role_context_payload(&context),
        RoleOperation::Report { report } => app.store.save_role_result(&context, &report),
        RoleOperation::Hook { envelope } => {
            app.store.save_hook_event(&context, &envelope, &provenance)
        }
        RoleOperation::PermissionRequest { envelope } => {
            if envelope
                .payload
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str)
                != Some("PermissionRequest")
            {
                bail!("permission bridge accepts only PermissionRequest hook events")
            }
            app.store
                .save_hook_event(&context, &envelope, &provenance)?;
            app.permission_request(
                &context,
                &request.credential,
                &envelope.payload,
                connection_nonce,
            )
            .await
        }
        RoleOperation::ProposeTransition {
            operation_id,
            phase,
            evidence,
        } => app
            .store
            .save_transition_proposal(&context, &operation_id, &phase, &evidence),
        RoleOperation::AcknowledgeGuidance { guidance_id } => {
            app.store.acknowledge_guidance(&context, &guidance_id)
        }
        RoleOperation::SetupRead { relative_path } => {
            crate::trip::setup_read(&app.store, &context, &relative_path)
        }
        RoleOperation::RecordExplorerDecision { input } => {
            crate::trip::record_explorer_decision(&app.store, &context, &input)
        }
        RoleOperation::ConfigureLanes { input } => {
            crate::trip::configure_lanes(&app.store, &context, &input)
        }
        RoleOperation::RequestIntegration { input } => {
            crate::trip::request_integration(&app.store, &context, &input)
        }
        RoleOperation::YieldLane { input } => crate::trip::yield_lane(&app.store, &context, &input),
        RoleOperation::SelectChecks { input } => {
            crate::trip::select_checks(&app.store, &context, &input)
        }
        RoleOperation::SubmitConformance { input } => {
            crate::trip::submit_conformance(&app.store, &context, &input)
        }
    }
}

pub async fn request(socket: &Path, request: &RoleRequest) -> Result<RoleResponse> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect role socket {}", socket.display()))?;
    stream.write_all(&serde_json::to_vec(request)?).await?;
    stream.write_all(b"\n").await?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(1024 * 1024)
        .read_line(&mut line)
        .await?;
    let response: RoleResponse = serde_json::from_str(&line)?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .clone()
                .unwrap_or_else(|| "role operation failed".to_owned())
        )
    }
    Ok(response)
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)
            .with_context(|| format!("remove stale socket {}", path.display())),
        Ok(_) => bail!("refusing to replace non-socket path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("inspect stale socket {}", path.display()))
        }
    }
}

#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("read role peer PID");
    }
    Ok(pid as u32)
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("read role peer credentials");
    }
    Ok(credentials.pid as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InstancePaths;
    use crate::store::Store;

    fn test_application() -> (std::path::PathBuf, Application) {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-role-admission-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        let app = Application::new(paths, store, std::env::current_exe().unwrap()).unwrap();
        (root, app)
    }

    #[tokio::test]
    async fn first_frame_timeout_releases_admission_for_the_next_connection() {
        let (root, app) = test_application();
        let socket = app.paths.role_socket.clone();
        let listener = bind(&app).unwrap();
        let (stop_tx, stop) = watch::channel(false);
        let server = tokio::spawn(serve_bound_with_limits(
            app,
            listener,
            stop,
            1,
            std::time::Duration::from_millis(50),
        ));
        let _parked = UnixStream::connect(&socket).await.unwrap();
        let mut next = UnixStream::connect(&socket).await.unwrap();
        next.write_all(b"{\"credential\":\"unknown\",\"kind\":\"context\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(next);
        let mut line = String::new();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            reader.read_line(&mut line),
        )
        .await
        .is_err());
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let response: RoleResponse = serde_json::from_str(&line).unwrap();
        assert!(!response.ok);
        stop_tx.send(true).unwrap();
        server.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn first_frame_disconnect_releases_admission_for_the_next_connection() {
        let (root, app) = test_application();
        let socket = app.paths.role_socket.clone();
        let listener = bind(&app).unwrap();
        let (stop_tx, stop) = watch::channel(false);
        let server = tokio::spawn(serve_bound_with_limits(
            app,
            listener,
            stop,
            1,
            std::time::Duration::from_secs(60),
        ));
        let disconnected = UnixStream::connect(&socket).await.unwrap();
        drop(disconnected);
        let mut next = UnixStream::connect(&socket).await.unwrap();
        next.write_all(b"{\"credential\":\"unknown\",\"kind\":\"context\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(next);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let response: RoleResponse = serde_json::from_str(&line).unwrap();
        assert!(!response.ok);
        stop_tx.send(true).unwrap();
        server.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn shutdown_closes_a_parked_first_frame_reader() {
        let (root, app) = test_application();
        let socket = app.paths.role_socket.clone();
        let listener = bind(&app).unwrap();
        let (stop_tx, stop) = watch::channel(false);
        let server = tokio::spawn(serve_bound_with_limits(
            app,
            listener,
            stop,
            1,
            std::time::Duration::from_secs(60),
        ));
        let client = UnixStream::connect(&socket).await.unwrap();
        tokio::task::yield_now().await;
        stop_tx.send(true).unwrap();
        server.await.unwrap().unwrap();
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(read, 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
