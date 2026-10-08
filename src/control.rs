use crate::domain::{
    AttachmentBinding, CapabilityProofInput, CmuxAttachmentControlDisposition, CmuxAttachmentMode,
    HumanCommand, PeerProcessIdentity, RoleKind, ValidationLaunchRequest,
    WorkflowValidationRequest,
};
use crate::operations::Application;
use crate::protocol::{self, ClientKind, Declaration, Hello, HelloKind};
use crate::transcript;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

const MAX_CONTROL_MESSAGE_BYTES: usize = 1024 * 1024;
// Healthy attachment polling occurs every 50 ms. This deadline leaves ample
// scheduling room while ensuring a silent authenticated peer cannot retain a
// live route or input lease indefinitely.
const CONTROL_IDLE_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlRequest {
    Status,
    RestartPreview,
    Stop {
        drain: bool,
    },
    CapabilityLaunch {
        request: ValidationLaunchRequest,
    },
    CapabilityWorkflowLaunch {
        request: WorkflowValidationRequest,
    },
    CapabilityList,
    CapabilityInspect {
        session_id: String,
    },
    CapabilityTranscript {
        session_id: String,
        after_epoch: Option<String>,
        after_sequence: u64,
        limit_bytes: usize,
    },
    CapabilityAcquireInput {
        session_id: String,
        owner_id: String,
        seconds: i64,
    },
    CapabilityRenewInput {
        session_id: String,
        lease: String,
        seconds: i64,
    },
    CapabilityTakeoverInput {
        session_id: String,
        owner_id: String,
        seconds: i64,
    },
    CapabilitySendInput {
        session_id: String,
        lease: String,
        data_base64: String,
    },
    CapabilityReleaseInput {
        session_id: String,
        lease: String,
    },
    CapabilityResize {
        session_id: String,
        lease: String,
        rows: u16,
        cols: u16,
    },
    Attach {
        session_id: String,
        owner_id: String,
        seconds: i64,
        #[serde(default)]
        takeover: bool,
        /// Watch attachments read the exact transcript binding but never ask
        /// for input ownership. The default preserves manual CLI attach
        /// compatibility, where an operator is intentionally requesting
        /// control unless another lease is already active.
        #[serde(default)]
        view_only: bool,
        /// A dashboard-created cmux attachment supplies the immutable binding
        /// it was routed for. The service compares it at connection time;
        /// route lookup alone is never sufficient.
        #[serde(default)]
        expected_binding: Option<AttachmentBinding>,
        #[serde(default)]
        cmux_route_id: Option<String>,
        /// The persistent mode-neutral protocol never accepts a migration-024
        /// route ID.  It starts view-only and learns later keyboard intent over
        /// this same authenticated connection.
        #[serde(default)]
        cmux_surface_route_id: Option<String>,
        #[serde(default)]
        cmux_binding_revision: Option<i64>,
    },
    /// The dashboard-created attach executable could not prove its immutable
    /// binding at connect time. Record only that route failure; this neither
    /// restarts the provider nor makes a surface an authority.
    CmuxAttachmentFailed {
        route_id: String,
        binding: AttachmentBinding,
    },
    /// New-protocol launch failure.  This is deliberately distinct from the
    /// migration-024 route report so a legacy route can never be adopted.
    CmuxSessionSurfaceFailed {
        surface_route_id: String,
        binding: AttachmentBinding,
        binding_revision: i64,
    },
    AttachmentTranscript {
        after_epoch: Option<String>,
        after_sequence: u64,
        limit_bytes: usize,
    },
    AttachmentRenew {
        seconds: i64,
    },
    /// Applies a browser-authored desired revision on this connection only.
    /// No lease material is accepted or returned.
    AttachmentControlSync,
    AttachmentSendInput {
        data_base64: String,
    },
    AttachmentResize {
        rows: u16,
        cols: u16,
    },
    AttachmentDetach,
    CapabilityInterrupt {
        session_id: String,
    },
    CapabilityResume {
        session_id: String,
        prompt: String,
    },
    CapabilityRecordProof {
        proof: CapabilityProofInput,
    },
    HumanCommand {
        command: HumanCommand,
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
        attempt_id: String,
        role: RoleKind,
    },
    RoleSwitchFinish {
        intent_id: String,
    },
    RoleResume {
        session_id: String,
        prompt: String,
    },
    RestartResume {
        operation_id: String,
        session_ids: Option<Vec<String>>,
    },
    RoleSwitchRequest {
        operation_id: String,
        attempt_id: String,
        role: String,
        old_generation_id: String,
        settings_revision: i64,
        snapshot_id: String,
        handoff: serde_json::Value,
        expected_task_version: i64,
    },
    GuidanceDeliver {
        guidance_id: String,
    },
    State,
    TaskRecoveryBindings {
        task_id: String,
    },
    Diagnostics {
        limit: usize,
    },
    CheckRun {
        attempt_id: String,
        #[serde(default)]
        suite_name: Option<String>,
        #[serde(default)]
        check_id: Option<String>,
    },
    LegacyPreview {
        source: std::path::PathBuf,
    },
    LegacyImport {
        operation_id: String,
        project_id: String,
        expected_project_version: i64,
        source: std::path::PathBuf,
        expected_source_hash: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    pub result: serde_json::Value,
    pub error: Option<String>,
}

impl ControlResponse {
    fn success<T: Serialize>(value: T) -> Result<Self> {
        Ok(Self {
            ok: true,
            result: serde_json::to_value(value)?,
            error: None,
        })
    }
    fn failure(error: anyhow::Error) -> Self {
        Self {
            ok: false,
            result: serde_json::Value::Null,
            error: Some(format!("{error:#}")),
        }
    }
}

pub struct ControlConnection {
    reader: Option<BufReader<OwnedReadHalf>>,
    writer: Option<OwnedWriteHalf>,
}

impl ControlConnection {
    pub async fn connect(socket: &Path) -> Result<Self> {
        Self::connect_for_kind(socket, ClientKind::HumanCli).await
    }

    pub async fn connect_for_kind(socket: &Path, kind: ClientKind) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect to {}", socket.display()))?;
        set_close_on_exec(stream.as_raw_fd())?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let hello = Hello {
            kind: HelloKind::Hello,
            declaration: Declaration {
                generation: protocol::GENERATION,
                client_kind: kind,
                required_features: kind
                    .features()
                    .iter()
                    .map(|feature| (*feature).to_owned())
                    .collect(),
            },
        };
        let mut frame = serde_json::to_vec(&hello)?;
        frame.push(b'\n');
        writer.write_all(&frame).await?;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            read_control_message(&mut reader),
        )
        .await
        .map_err(|_| anyhow!("Protocol negotiation timed out. {}", protocol::GUIDANCE))??;
        let response = response.ok_or_else(|| {
            anyhow!(
                "The service may be older or may have closed the connection. {}",
                protocol::GUIDANCE
            )
        })?;
        let response: serde_json::Value = serde_json::from_slice(&response).map_err(|_| {
            anyhow!(
                "The service returned an invalid protocol response. {}",
                protocol::GUIDANCE
            )
        })?;
        if let Some(error) = response.get("protocol_error") {
            let reason = error
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("malformed");
            bail!(
                "Protocol negotiation refused ({reason}). {}",
                protocol::GUIDANCE
            );
        }
        let descriptor = response.get("descriptor").ok_or_else(|| {
            anyhow!(
                "The service may be older or may have closed the connection. {}",
                protocol::GUIDANCE
            )
        })?;
        let generation = descriptor
            .get("generation")
            .and_then(serde_json::Value::as_u64);
        let instance = descriptor
            .get("instance_id")
            .and_then(serde_json::Value::as_str);
        let version = descriptor
            .get("server_version")
            .and_then(serde_json::Value::as_str);
        let features = descriptor
            .get("supported_features")
            .and_then(serde_json::Value::as_array);
        if response.get("ok") != Some(&serde_json::Value::Bool(true))
            || generation != Some(protocol::GENERATION as u64)
            || !instance.is_some_and(|value| !value.is_empty())
            || !version.is_some_and(|value| !value.is_empty())
            || !features.is_some_and(|values| {
                values.iter().all(serde_json::Value::is_string)
                    && kind
                        .features()
                        .iter()
                        .all(|required| values.iter().any(|value| value.as_str() == Some(required)))
            })
        {
            bail!(
                "The service returned an incompatible protocol descriptor. {}",
                protocol::GUIDANCE
            );
        }
        Ok(Self {
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    pub async fn request(&mut self, request: &ControlRequest) -> Result<ControlResponse> {
        Self::require_success(self.request_raw(request).await?)
    }

    /// Uses a deadline only for callers whose interactive terminal state must
    /// remain recoverable. A timed-out request may have left either side in
    /// the middle of a frame, so both socket halves are dropped before the
    /// error is returned and this connection can never be reused.
    pub async fn request_with_timeout(
        &mut self,
        request: &ControlRequest,
        timeout: std::time::Duration,
    ) -> Result<ControlResponse> {
        let response = match tokio::time::timeout(timeout, self.request_raw(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                self.poison();
                return Err(error);
            }
            Err(_) => {
                self.poison();
                bail!(
                    "control attachment request timed out after {} ms",
                    timeout.as_millis()
                )
            }
        };
        Self::require_success(response)
    }

    fn poison(&mut self) {
        self.reader.take();
        self.writer.take();
    }

    pub fn is_poisoned(&self) -> bool {
        self.reader.is_none() || self.writer.is_none()
    }

    async fn request_raw(&mut self, request: &ControlRequest) -> Result<ControlResponse> {
        let encoded = serde_json::to_vec(request)?;
        if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
            bail!("control request exceeds the 1 MiB message limit")
        }
        self.writer
            .as_mut()
            .ok_or_else(|| anyhow!("control connection is unavailable after a request failure"))?
            .write_all(&encoded)
            .await?;
        self.writer
            .as_mut()
            .ok_or_else(|| anyhow!("control connection is unavailable after a request failure"))?
            .write_all(b"\n")
            .await?;
        let Some(line) =
            read_control_message(self.reader.as_mut().ok_or_else(|| {
                anyhow!("control connection is unavailable after a request failure")
            })?)
            .await?
        else {
            bail!("control connection closed before its response")
        };
        Ok(serde_json::from_slice(&line)?)
    }

    fn require_success(response: ControlResponse) -> Result<ControlResponse> {
        if !response.ok {
            bail!(
                "{}",
                response
                    .error
                    .unwrap_or_else(|| "control operation failed".to_owned())
            )
        }
        Ok(response)
    }
}

struct ConnectionAttachment {
    binding: AttachmentBinding,
    owner_id: String,
    lease: Option<String>,
    cmux_route_id: Option<String>,
    cmux_surface_route_id: Option<String>,
    cmux_binding_revision: Option<i64>,
}

struct ConnectionState {
    peer_identity: PeerProcessIdentity,
    attachment: Option<ConnectionAttachment>,
}

fn release_claimed_cmux_route(app: &Application, route_id: &str, binding: &AttachmentBinding) {
    let _ = app
        .store
        .mark_cmux_attachment_ended(route_id, app.service_boot_id(), binding);
}

fn release_attachment_lease(app: &Application, binding: &AttachmentBinding, lease: &str) {
    if app
        .supervisor
        .release_attachment_input(binding, lease)
        .is_err()
    {
        // The secret remains in this connection only. The fallback is scoped
        // to that exact session/secret pair and cannot release another owner.
        let _ = app.store.release_input_lease(&binding.session_id, lease);
    }
}

impl ConnectionState {
    fn attachment(&self) -> Result<&ConnectionAttachment> {
        self.attachment
            .as_ref()
            .ok_or_else(|| anyhow!("attach to a session before using attachment operations"))
    }

    fn lease(&self) -> Result<(&AttachmentBinding, &str)> {
        let attachment = self.attachment()?;
        let lease = attachment.lease.as_deref().ok_or_else(|| {
            anyhow!("this attachment is view-only; reconnect with --takeover to request input ownership")
        })?;
        Ok((&attachment.binding, lease))
    }

    fn release_attachment(&mut self, app: &Application) {
        let Some(attachment) = self.attachment.take() else {
            return;
        };
        if let Some(lease) = attachment.lease {
            release_attachment_lease(app, &attachment.binding, &lease);
        }
        if let (Some(surface_route_id), Some(binding_revision)) = (
            attachment.cmux_surface_route_id.as_deref(),
            attachment.cmux_binding_revision,
        ) {
            let _ = app.store.mark_cmux_session_surface_ended(
                surface_route_id,
                app.service_boot_id(),
                &attachment.binding,
                binding_revision,
            );
        }
        if let Some(route_id) = attachment.cmux_route_id {
            let _ = app.store.mark_cmux_attachment_ended(
                &route_id,
                app.service_boot_id(),
                &attachment.binding,
            );
        }
    }
}

fn persistent_attachment_parts(
    connection: &ConnectionState,
) -> Result<(AttachmentBinding, String, String, i64)> {
    let attachment = connection.attachment()?;
    let surface_route_id = attachment.cmux_surface_route_id.clone().ok_or_else(|| {
        anyhow!("attachment control sync is available only to a persistent cmux surface")
    })?;
    let binding_revision = attachment
        .cmux_binding_revision
        .ok_or_else(|| anyhow!("persistent cmux attachment has no binding revision"))?;
    Ok((
        attachment.binding.clone(),
        attachment.owner_id.clone(),
        surface_route_id,
        binding_revision,
    ))
}

fn clear_connection_lease(app: &Application, connection: &mut ConnectionState) {
    let released = connection.attachment.as_mut().and_then(|attachment| {
        attachment
            .lease
            .take()
            .map(|lease| (attachment.binding.clone(), lease))
    });
    if let Some((binding, lease)) = released {
        release_attachment_lease(app, &binding, &lease);
    }
}

/// A rejected persistent input operation must not leave a stale secret in the
/// authenticated socket. The durable row remains the source of truth: when it
/// is still a live open/unknown route, record that this connection reconciled
/// to view-only at the current desired revision. Lost and retired rows retain
/// their stronger historical evidence instead.
fn clear_and_reconcile_persistent_lease(
    app: &Application,
    connection: &mut ConnectionState,
    reason: &str,
) {
    let parts = persistent_attachment_parts(connection).ok();
    clear_connection_lease(app, connection);
    let Some((binding, _, surface_route_id, binding_revision)) = parts else {
        return;
    };
    let Ok(surface) = current_persistent_surface(app, &surface_route_id) else {
        return;
    };
    if surface.attachment_state == "live"
        && matches!(surface.surface_state.as_str(), "open" | "unknown")
    {
        let _ = reconcile_persistent_control(
            app,
            &surface_route_id,
            &binding,
            binding_revision,
            surface.control_revision,
            "view_only",
            reason,
        );
    }
}

fn persistent_sync_payload(
    surface: &crate::domain::CmuxSessionSurface,
    message: &str,
) -> serde_json::Value {
    let state = if matches!(surface.surface_state.as_str(), "opening" | "unknown")
        && surface.attachment_state == "live"
    {
        "unknown_live"
    } else if surface.surface_state != "open" || surface.attachment_state != "live" {
        "retired"
    } else if surface.actual_input_state == "control" {
        "control"
    } else if surface.actual_input_state == "blocked" {
        "blocked"
    } else {
        "view_only"
    };
    serde_json::json!({
        "state": state,
        "message": message,
        "surface_route_id": surface.id,
        "binding_revision": surface.binding_revision,
        "desired_input_state": surface.desired_input_state,
        "actual_input_state": surface.actual_input_state,
        "control_revision": surface.control_revision,
        "applied_revision": surface.applied_revision,
    })
}

fn current_persistent_surface(
    app: &Application,
    surface_route_id: &str,
) -> Result<crate::domain::CmuxSessionSurface> {
    app.store.cmux_session_surface(surface_route_id)
}

fn acknowledge_persistent_control(
    app: &Application,
    surface_route_id: &str,
    binding: &AttachmentBinding,
    binding_revision: i64,
    desired_revision: i64,
    actual_input_state: &str,
    error: Option<&str>,
) -> Result<()> {
    let _ = app.store.acknowledge_cmux_attachment_control(
        surface_route_id,
        app.service_boot_id(),
        binding,
        binding_revision,
        desired_revision,
        actual_input_state,
        error,
    )?;
    Ok(())
}

/// Cleanup follows a rejected operation rather than a new control decision.
/// It must reconcile the applied state while retaining the causal observation
/// recorded when a live route became unknown or lost.
fn reconcile_persistent_control(
    app: &Application,
    surface_route_id: &str,
    binding: &AttachmentBinding,
    binding_revision: i64,
    desired_revision: i64,
    actual_input_state: &str,
    reason: &str,
) -> Result<()> {
    let _ = app.store.reconcile_cmux_attachment_control(
        surface_route_id,
        app.service_boot_id(),
        binding,
        binding_revision,
        desired_revision,
        actual_input_state,
        Some(reason),
    )?;
    Ok(())
}

fn require_persistent_actual_control(
    app: &Application,
    connection: &ConnectionState,
) -> Result<()> {
    let (binding, _, surface_route_id, binding_revision) = persistent_attachment_parts(connection)?;
    let current = app.supervisor.establish_attachment(&binding.session_id)?;
    if current != binding {
        bail!("persistent cmux attachment binding is stale")
    }
    let surface = app.store.cmux_session_surface(&surface_route_id)?;
    if surface.binding != binding
        || surface.binding_revision != binding_revision
        || surface.surface_state != "open"
        || surface.attachment_state != "live"
        || surface.actual_input_state != "control"
    {
        bail!("persistent cmux attachment is no longer the exact current keyboard-control surface")
    }
    Ok(())
}

pub fn bind(app: &Application) -> Result<UnixListener> {
    remove_stale_socket(&app.paths.control_socket)?;
    let listener = UnixListener::bind(&app.paths.control_socket)
        .with_context(|| format!("bind control socket {}", app.paths.control_socket.display()))?;
    set_close_on_exec(listener.as_raw_fd())?;
    std::fs::set_permissions(
        &app.paths.control_socket,
        std::fs::Permissions::from_mode(0o600),
    )?;
    Ok(listener)
}

async fn accept_control_stream(listener: &UnixListener) -> Result<UnixStream> {
    let (stream, _) = listener.accept().await?;
    set_close_on_exec(stream.as_raw_fd())?;
    Ok(stream)
}

pub async fn serve_bound(
    app: Application,
    shutdown: watch::Sender<bool>,
    instance: serde_json::Value,
    listener: UnixListener,
) -> Result<()> {
    let mut stop = shutdown.subscribe();
    loop {
        let stream = tokio::select! {
            accepted = accept_control_stream(&listener) => accepted?,
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { return Ok(()) }
                continue
            }
        };
        let app = app.clone();
        let diagnostic_app = app.clone();
        let shutdown = shutdown.clone();
        let instance = instance.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(stream, app, shutdown, instance).await {
                tracing::warn!(error = %error, "control request failed");
                if let Err(sink_error) = diagnostic_app.diagnostics.record(
                    "warn",
                    "control.ipc.rejected",
                    "control",
                    "rejected",
                    None,
                    serde_json::json!({"cause": format!("{error:#}")}),
                ) {
                    let _ = crate::diagnostics::write_degraded_marker(
                        diagnostic_app.diagnostics.root(),
                        &format!("control IPC rejection log failed: {sink_error:#}"),
                    );
                }
            }
        });
    }
}

async fn handle(
    stream: UnixStream,
    app: Application,
    shutdown: watch::Sender<bool>,
    instance: serde_json::Value,
) -> Result<()> {
    // The audit-token proof is retained through the supervisor's one
    // admission-time inventory, then revalidated before this socket is read.
    let mut peer_admission = peer_admission(&stream)?;
    let (mut reader, mut writer, peer_identity) = admit_human_control(
        peer_admission.identity.clone(),
        |peer| app.supervisor.peer_is_managed_or_descendant(peer),
        || peer_admission.revalidate_after_inventory(),
        |peer_identity| {
            let (reader, writer) = stream.into_split();
            Ok((BufReader::new(reader), writer, peer_identity))
        },
    )?;
    let connection = ConnectionState {
        peer_identity,
        attachment: None,
    };
    serve_connection(
        &mut reader,
        &mut writer,
        app,
        shutdown,
        instance,
        connection,
        CONTROL_IDLE_READ_DEADLINE,
    )
    .await
}

fn admit_human_control<T>(
    peer_identity: PeerProcessIdentity,
    classify_ancestry: impl FnOnce(&PeerProcessIdentity) -> Result<bool>,
    revalidate_kernel_generation: impl FnOnce() -> Result<()>,
    after_admission: impl FnOnce(PeerProcessIdentity) -> Result<T>,
) -> Result<T> {
    if classify_ancestry(&peer_identity)? {
        bail!("managed role process ancestry cannot use human control authority")
    }
    revalidate_kernel_generation()
        .context("control peer kernel generation changed after ancestry validation")?;
    after_admission(peer_identity)
}

async fn serve_connection(
    reader: &mut BufReader<OwnedReadHalf>,
    writer: &mut OwnedWriteHalf,
    app: Application,
    shutdown: watch::Sender<bool>,
    instance: serde_json::Value,
    mut connection: ConnectionState,
    idle_read_deadline: std::time::Duration,
) -> Result<()> {
    let result = async {
        let first = tokio::time::timeout(idle_read_deadline, read_control_message(reader))
            .await
            .map_err(|_| anyhow!("control connection idle read deadline expired"))?;
        let first = match first {
            Ok(first) => first,
            Err(_) => {
                let error = protocol::ProtocolError::new("malformed", None);
                let response = serde_json::json!({"ok":false,"error":error.guidance,"protocol_error":error});
                write_protocol_response(writer, &response).await?;
                return Ok(());
            }
        };
        let Some(first) = first else {
            return Ok(());
        };
        let hello: std::result::Result<Hello, protocol::ProtocolError> = if first.len() > protocol::MAX_DECLARATION_BYTES {
            Err(protocol::ProtocolError::new("malformed", None))
        } else {
            serde_json::from_slice(&first).map_err(|_| {
                let kind = serde_json::from_slice::<serde_json::Value>(&first).ok()
                    .and_then(|value| value.get("kind").and_then(|kind| kind.as_str()).map(str::to_owned));
                protocol::ProtocolError::new(if kind.as_deref().is_some_and(|kind| kind != "hello") { "missing" } else { "malformed" }, None)
            })
        };
        let negotiation = hello.and_then(|hello| {
            if !matches!(hello.declaration.client_kind, ClientKind::HumanCli | ClientKind::Attachment) {
                return Err(protocol::ProtocolError::new("wrong_client_kind", Some(hello.declaration.generation)));
            }
            protocol::validate(&hello.declaration, hello.declaration.client_kind)?;
            Ok(hello.declaration.client_kind)
        });
        let _negotiated = match negotiation {
            Ok(kind) => {
                let instance_id = instance.get("instance_id").and_then(serde_json::Value::as_str).unwrap_or_default();
                let response = serde_json::json!({"ok":true,"descriptor":protocol::Descriptor::new(instance_id.to_owned(), kind)});
                write_protocol_response(writer, &response).await?;
                (protocol::GENERATION, kind)
            }
            Err(error) => {
                let response = serde_json::json!({"ok":false,"error":error.guidance,"protocol_error":error});
                write_protocol_response(writer, &response).await?;
                return Ok(());
            }
        };
        loop {
            let line = tokio::time::timeout(idle_read_deadline, read_control_message(reader))
                .await
                .map_err(|_| anyhow!("control connection idle read deadline expired"))??;
            let Some(line) = line else {
                break;
            };
            debug_assert!(connection.peer_identity.pid > 0);
            let request: ControlRequest = match serde_json::from_slice(&line) {
                Ok(request) => request,
                Err(_) => {
                    let reason = if serde_json::from_slice::<serde_json::Value>(&line).ok()
                        .and_then(|value| value.get("kind").and_then(|kind| kind.as_str()).map(str::to_owned))
                        .as_deref() == Some("hello") { "duplicate hello" } else { "malformed control request" };
                    write_control_response(writer, &ControlResponse::failure(anyhow!(reason))).await?;
                    break;
                }
            };
            let response = if blocking_control_request(&request) {
                let permit = Arc::clone(&app.blocking_operations).acquire_owned().await;
                let blocking_app = app.clone();
                let blocking_shutdown = shutdown.clone();
                let blocking_instance = instance.clone();
                let peer_identity = connection.peer_identity.clone();
                match permit {
                    Ok(permit) => match tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        let mut stateless_connection = ConnectionState {
                            peer_identity,
                            attachment: None,
                        };
                        dispatch(
                            request,
                            &blocking_app,
                            &blocking_shutdown,
                            &blocking_instance,
                            &mut stateless_connection,
                        )
                    })
                    .await
                    {
                        Ok(Ok(response)) => response,
                        Ok(Err(error)) => ControlResponse::failure(error),
                        Err(error) => ControlResponse::failure(anyhow!(
                            "control blocking task failed: {error}"
                        )),
                    },
                    Err(_) => ControlResponse::failure(anyhow!(
                        "blocking operation admission is unavailable"
                    )),
                }
            } else {
                match dispatch(request, &app, &shutdown, &instance, &mut connection) {
                    Ok(response) => response,
                    Err(error) => ControlResponse::failure(error),
                }
            };
            write_control_response(writer, &response).await?;
        }
        Ok(())
    }
    .await;
    connection.release_attachment(&app);
    result
}

async fn write_protocol_response(
    writer: &mut OwnedWriteHalf,
    response: &serde_json::Value,
) -> Result<()> {
    let mut frame = serde_json::to_vec(response)?;
    frame.push(b'\n');
    writer.write_all(&frame).await?;
    Ok(())
}

fn set_close_on_exec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error())
            .context("read control socket close-on-exec flags");
    }
    if flags & libc::FD_CLOEXEC != 0 {
        return Ok(());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("set control socket close-on-exec");
    }
    Ok(())
}

fn dispatch(
    request: ControlRequest,
    app: &Application,
    shutdown: &watch::Sender<bool>,
    instance: &serde_json::Value,
    connection: &mut ConnectionState,
) -> Result<ControlResponse> {
    match request {
        ControlRequest::Status => ControlResponse::success(
            serde_json::json!({"instance": instance, "sessions": app.store.list_sessions()?,"coordinator":app.drain_status()?}),
        ),
        ControlRequest::RestartPreview => ControlResponse::success(app.restart_preview()?),
        ControlRequest::Stop { drain } => {
            if drain {
                let status = app.begin_drain()?;
                let quiescent = status
                    .get("active")
                    .and_then(|value| value.as_array())
                    .is_some_and(Vec::is_empty)
                    && status
                        .get("active_checks")
                        .and_then(|value| value.as_array())
                        .is_some_and(Vec::is_empty)
                    && status
                        .get("unknown")
                        .and_then(|value| value.as_array())
                        .is_some_and(Vec::is_empty)
                    && status["process_inventory"]["state"] == "observed";
                if quiescent {
                    signal_shutdown(shutdown)?;
                }
                return ControlResponse::success(status);
            }
            let status = app.drain_status()?;
            if status.get("quiescent") != Some(&serde_json::Value::Bool(true)) {
                bail!("service still owns active sessions or checks {}; use stop --drain to interrupt and visibly reconcile them", status)
            }
            signal_shutdown(shutdown)?;
            ControlResponse::success(
                serde_json::json!({"shutdown_requested": true, "drain": false, "active_dispatch": "quiescent"}),
            )
        }
        ControlRequest::CapabilityLaunch { request } => {
            ControlResponse::success(app.launch_validation(request)?)
        }
        ControlRequest::CapabilityWorkflowLaunch { request } => {
            ControlResponse::success(app.launch_workflow_validation(request)?)
        }
        ControlRequest::CapabilityList => {
            app.supervisor.reconcile()?;
            ControlResponse::success(app.store.list_sessions()?)
        }
        ControlRequest::CapabilityInspect { session_id } => {
            app.supervisor.reconcile()?;
            let mut record = app.store.session_json(&session_id)?;
            record["process_group_members"] =
                serde_json::to_value(app.supervisor.process_group_members(&session_id)?)?;
            ControlResponse::success(record)
        }
        ControlRequest::CapabilityTranscript {
            session_id,
            after_epoch,
            after_sequence,
            limit_bytes,
        } => {
            app.store.session_json(&session_id)?;
            ControlResponse::success(transcript::read_frames(
                &app.paths.transcripts,
                &session_id,
                after_epoch.as_deref(),
                after_sequence,
                limit_bytes,
            )?)
        }
        ControlRequest::CapabilityAcquireInput {
            session_id,
            owner_id,
            seconds,
        } => {
            let (lease, expires_at) =
                app.supervisor
                    .acquire_input(&session_id, &owner_id, seconds)?;
            ControlResponse::success(serde_json::json!({"lease": lease, "expires_at": expires_at}))
        }
        ControlRequest::CapabilityRenewInput {
            session_id,
            lease,
            seconds,
        } => {
            let binding = app.supervisor.establish_attachment(&session_id)?;
            let expires_at = app.supervisor.renew_input(&binding, &lease, seconds)?;
            ControlResponse::success(serde_json::json!({"expires_at":expires_at}))
        }
        ControlRequest::CapabilityTakeoverInput {
            session_id,
            owner_id,
            seconds,
        } => {
            let binding = app.supervisor.establish_attachment(&session_id)?;
            let (lease, expires_at) = app
                .supervisor
                .takeover_input(&binding, &owner_id, seconds)?;
            ControlResponse::success(
                serde_json::json!({"lease":lease,"expires_at":expires_at,"takeover":true}),
            )
        }
        ControlRequest::CapabilitySendInput {
            session_id,
            lease,
            data_base64,
        } => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(data_base64)?;
            app.supervisor.write_input(&session_id, &lease, &bytes)?;
            ControlResponse::success(serde_json::json!({"written": bytes.len()}))
        }
        ControlRequest::CapabilityReleaseInput { session_id, lease } => {
            app.supervisor.release_input(&session_id, &lease)?;
            ControlResponse::success(serde_json::json!({"released": true}))
        }
        ControlRequest::CapabilityResize {
            session_id,
            lease,
            rows,
            cols,
        } => {
            app.supervisor.resize(&session_id, &lease, rows, cols)?;
            ControlResponse::success(
                serde_json::json!({"session_id":session_id,"rows":rows,"cols":cols,"authority":"authenticated_human_control"}),
            )
        }
        ControlRequest::Attach {
            session_id,
            owner_id,
            seconds,
            takeover,
            view_only,
            expected_binding,
            cmux_route_id,
            cmux_surface_route_id,
            cmux_binding_revision,
        } => {
            if connection.attachment.is_some() {
                bail!("this control connection already has an attachment; detach before attaching another session")
            }
            let binding = app.supervisor.establish_attachment(&session_id)?;
            if let Some(expected) = expected_binding.as_ref() {
                expected
                    .validate_route_identifiers()
                    .map_err(|error| anyhow!(error))?;
                if expected.session_id != session_id {
                    bail!("expected attachment session does not match the requested session")
                }
                if expected != &binding {
                    bail!("expected attachment binding no longer matches the live service session")
                }
            }
            if cmux_route_id.is_some() && expected_binding.is_none() {
                bail!("a cmux attachment route requires its exact expected binding")
            }
            if view_only && takeover {
                bail!("a read-only watch attachment cannot take over keyboard control")
            }
            if let Some(surface_route_id) = cmux_surface_route_id.as_deref() {
                if cmux_route_id.is_some() {
                    bail!("a persistent cmux surface route cannot be combined with a migration-024 route")
                }
                if !view_only || takeover {
                    bail!("a persistent cmux surface must begin view-only without takeover")
                }
                let binding_revision = cmux_binding_revision
                    .filter(|revision| *revision > 0)
                    .ok_or_else(|| {
                        anyhow!(
                            "a persistent cmux surface route requires a positive binding revision"
                        )
                    })?;
                let expected = expected_binding.as_ref().ok_or_else(|| {
                    anyhow!("a persistent cmux surface route requires its exact expected binding")
                })?;
                let surface = app.store.cmux_session_surface(surface_route_id)?;
                if surface.service_boot_id != app.service_boot_id()
                    || surface.binding != binding
                    || expected != &binding
                    || surface.binding_revision != binding_revision
                {
                    bail!("persistent cmux surface route does not match this exact current binding and revision")
                }
                let surface = app.store.mark_cmux_session_surface_connected(
                    surface_route_id,
                    app.service_boot_id(),
                    &binding,
                    binding_revision,
                )?;
                connection.attachment = Some(ConnectionAttachment {
                    binding: binding.clone(),
                    owner_id,
                    lease: None,
                    cmux_route_id: None,
                    cmux_surface_route_id: Some(surface_route_id.to_owned()),
                    cmux_binding_revision: Some(binding_revision),
                });
                return ControlResponse::success(serde_json::json!({
                    "mode":"view_only",
                    "actual_input_state":"view_only",
                    "desired_input_state":surface.desired_input_state,
                    "binding_revision":surface.binding_revision,
                    "control_revision":surface.control_revision,
                    "control_available":true,
                }));
            }
            if cmux_binding_revision.is_some() {
                bail!("a cmux binding revision requires the persistent cmux surface protocol")
            }
            let requested_mode = if view_only {
                CmuxAttachmentMode::Watch
            } else {
                CmuxAttachmentMode::Control
            };
            if let Some(route_id) = cmux_route_id.as_deref() {
                let route = app.store.cmux_attachment_route(route_id)?;
                if route.binding != binding || route.mode != requested_mode {
                    bail!("cmux attachment route does not match this exact binding and requested mode")
                }
            }
            let active_owner = app.store.active_input_lease_owner(&session_id)?;
            let claimed_route_id = if let Some(route_id) = cmux_route_id.as_deref() {
                // This atomic transition checks the service boot, exact binding,
                // mode, pending state, and live surface before any input authority
                // can be acquired or revoked. A copied or already-live route fails
                // here without disturbing its current input owner.
                app.store.mark_cmux_attachment_connected(
                    route_id,
                    app.service_boot_id(),
                    &binding,
                    requested_mode,
                )?;
                Some(route_id)
            } else {
                None
            };
            let ownership = if view_only {
                connection.attachment = Some(ConnectionAttachment {
                    binding: binding.clone(),
                    owner_id: owner_id.clone(),
                    lease: None,
                    cmux_route_id: cmux_route_id.clone(),
                    cmux_surface_route_id: None,
                    cmux_binding_revision: None,
                });
                serde_json::json!({"mode":"watch","control_available":true})
            } else if takeover && active_owner.is_some() {
                let (lease, expires_at) =
                    match app.supervisor.takeover_input(&binding, &owner_id, seconds) {
                        Ok(lease) => lease,
                        Err(error) => {
                            if let Some(route_id) = claimed_route_id {
                                release_claimed_cmux_route(app, route_id, &binding);
                            }
                            return Err(error);
                        }
                    };
                connection.attachment = Some(ConnectionAttachment {
                    binding: binding.clone(),
                    owner_id: owner_id.clone(),
                    lease: Some(lease),
                    cmux_route_id: cmux_route_id.clone(),
                    cmux_surface_route_id: None,
                    cmux_binding_revision: None,
                });
                serde_json::json!({"mode":"owner","expires_at":expires_at,"takeover":true})
            } else {
                match app
                    .supervisor
                    .acquire_input(&session_id, &owner_id, seconds)
                {
                    Ok((lease, expires_at)) => {
                        connection.attachment = Some(ConnectionAttachment {
                            binding: binding.clone(),
                            owner_id: owner_id.clone(),
                            lease: Some(lease),
                            cmux_route_id: cmux_route_id.clone(),
                            cmux_surface_route_id: None,
                            cmux_binding_revision: None,
                        });
                        serde_json::json!({"mode":"owner","expires_at":expires_at,"takeover":false})
                    }
                    Err(error) => match app.store.active_input_lease_owner(&session_id) {
                        Ok(Some((active_owner, active_expires_at))) => {
                            connection.attachment = Some(ConnectionAttachment {
                                binding: binding.clone(),
                                owner_id: owner_id.clone(),
                                lease: None,
                                cmux_route_id: cmux_route_id.clone(),
                                cmux_surface_route_id: None,
                                cmux_binding_revision: None,
                            });
                            serde_json::json!({"mode":"view_only","reason":"another authenticated human control lease is active","active_owner_id":active_owner,"active_expires_at":active_expires_at,"takeover_available":true,"acquire_error":format!("{error:#}")})
                        }
                        Ok(None) => {
                            if let Some(route_id) = claimed_route_id {
                                release_claimed_cmux_route(app, route_id, &binding);
                            }
                            return Err(error);
                        }
                        Err(owner_error) => {
                            if let Some(route_id) = claimed_route_id {
                                release_claimed_cmux_route(app, route_id, &binding);
                            }
                            return Err(owner_error);
                        }
                    },
                }
            };
            ControlResponse::success(ownership)
        }
        ControlRequest::CmuxAttachmentFailed { route_id, binding } => {
            app.store
                .mark_cmux_attachment_failed(&route_id, app.service_boot_id(), &binding)?;
            ControlResponse::success(serde_json::json!({"recorded":true}))
        }
        ControlRequest::CmuxSessionSurfaceFailed {
            surface_route_id,
            binding,
            binding_revision,
        } => {
            app.store.mark_cmux_session_surface_failed(
                &surface_route_id,
                app.service_boot_id(),
                &binding,
                binding_revision,
            )?;
            ControlResponse::success(
                serde_json::json!({"recorded":true,"protocol":"persistent_cmux"}),
            )
        }
        ControlRequest::AttachmentTranscript {
            after_epoch,
            after_sequence,
            limit_bytes,
        } => {
            let binding = &connection.attachment()?.binding;
            ControlResponse::success(app.supervisor.attachment_transcript(
                binding,
                after_epoch.as_deref(),
                after_sequence,
                limit_bytes.min(512 * 1024),
            )?)
        }
        ControlRequest::AttachmentControlSync => {
            let (binding, owner_id, surface_route_id, binding_revision) =
                persistent_attachment_parts(connection)?;
            let directive = app.store.cmux_attachment_control_directive(
                &surface_route_id,
                app.service_boot_id(),
                &binding,
                binding_revision,
            )?;
            match directive.disposition {
                CmuxAttachmentControlDisposition::Retire => {
                    clear_connection_lease(app, connection);
                    let surface = current_persistent_surface(app, &surface_route_id)?;
                    return ControlResponse::success(persistent_sync_payload(
                        &surface,
                        "This cmux attachment is retired. It released any in-memory lease and will not focus, respawn, inject into, or close its historical terminal. The client must now detach so this exact binding records ended before any replacement can be reserved.",
                    ));
                }
                CmuxAttachmentControlDisposition::Hold => {
                    let surface = current_persistent_surface(app, &surface_route_id)?;
                    return ControlResponse::success(persistent_sync_payload(
                        &surface,
                        "cmux presentation is uncertain. This sync preserved the existing lease and control revision; it will not acquire, release, renew, resize, or take over input until the exact presentation state is resolved.",
                    ));
                }
                CmuxAttachmentControlDisposition::Apply => {}
            }
            let current_binding = app.supervisor.establish_attachment(&binding.session_id);
            if current_binding.as_ref().ok() != Some(&binding) {
                clear_connection_lease(app, connection);
                let _ = app.store.mark_cmux_session_surface_ended(
                    &surface_route_id,
                    app.service_boot_id(),
                    &binding,
                    binding_revision,
                );
                let surface = current_persistent_surface(app, &surface_route_id)?;
                return ControlResponse::success(persistent_sync_payload(
                    &surface,
                    "The exact provider binding is no longer current. This historical terminal will not be reused; the connection released any in-memory lease.",
                ));
            }
            if directive.desired_input_state == "view_only" {
                clear_connection_lease(app, connection);
                if directive.applied_revision != directive.desired_revision
                    || directive.actual_input_state != "view_only"
                {
                    acknowledge_persistent_control(
                        app,
                        &surface_route_id,
                        &binding,
                        binding_revision,
                        directive.desired_revision,
                        "view_only",
                        None,
                    )?;
                }
                let surface = current_persistent_surface(app, &surface_route_id)?;
                return ControlResponse::success(persistent_sync_payload(
                            &surface,
                            "Keyboard control is released on this authenticated attachment; the terminal remains view-only.",
                        ));
            }
            if directive.applied_revision == directive.desired_revision {
                let surface = current_persistent_surface(app, &surface_route_id)?;
                return ControlResponse::success(persistent_sync_payload(
                            &surface,
                            "The current keyboard-control revision was already applied; no automatic reacquire or takeover was attempted.",
                        ));
            }
            let already_has_lease = connection
                .attachment
                .as_ref()
                .and_then(|attachment| attachment.lease.as_ref())
                .is_some();
            if already_has_lease {
                acknowledge_persistent_control(
                    app,
                    &surface_route_id,
                    &binding,
                    binding_revision,
                    directive.desired_revision,
                    "control",
                    None,
                )?;
                let surface = current_persistent_surface(app, &surface_route_id)?;
                return ControlResponse::success(persistent_sync_payload(
                    &surface,
                    "Keyboard control is active on this authenticated attachment.",
                ));
            }
            match app
                .supervisor
                .acquire_input(&binding.session_id, &owner_id, 30)
            {
                Ok((lease, _)) => {
                    if let Some(attachment) = connection.attachment.as_mut() {
                        attachment.lease = Some(lease);
                    }
                    acknowledge_persistent_control(
                        app,
                        &surface_route_id,
                        &binding,
                        binding_revision,
                        directive.desired_revision,
                        "control",
                        None,
                    )?;
                    let surface = current_persistent_surface(app, &surface_route_id)?;
                    ControlResponse::success(persistent_sync_payload(
                                &surface,
                                "Keyboard control is active on this authenticated attachment. No secret was returned outside this connection.",
                            ))
                }
                Err(error) => {
                    let active = app.store.active_input_lease_owner(&binding.session_id)?;
                    let actual = if active.is_some() {
                        "blocked"
                    } else {
                        "view_only"
                    };
                    acknowledge_persistent_control(
                        app,
                        &surface_route_id,
                        &binding,
                        binding_revision,
                        directive.desired_revision,
                        actual,
                        Some(&format!(
                            "keyboard-control acquire was not applied: {error:#}"
                        )),
                    )?;
                    let surface = current_persistent_surface(app, &surface_route_id)?;
                    let message = if actual == "blocked" {
                        "Keyboard control is blocked by another authenticated human lease. Release or detach from that exact terminal, or wait for bounded lease expiry before submitting a new explicit Take request. No takeover was attempted."
                    } else {
                        "Keyboard control could not be acquired. This revision is applied view-only and will not retry automatically; submit a new explicit Take request after resolving the condition."
                    };
                    ControlResponse::success(persistent_sync_payload(&surface, message))
                }
            }
        }
        ControlRequest::AttachmentRenew { seconds } => {
            let persistent = connection
                .attachment
                .as_ref()
                .is_some_and(|attachment| attachment.cmux_surface_route_id.is_some());
            if persistent {
                if let Err(error) = require_persistent_actual_control(app, connection) {
                    clear_and_reconcile_persistent_lease(
                        app,
                        connection,
                        "persistent renewal was rejected because this connection no longer has exact keyboard authority",
                    );
                    return Err(error);
                }
            }
            let binding = connection.attachment()?.binding.clone();
            let lease = connection
                .attachment()?
                .lease
                .clone()
                .ok_or_else(|| anyhow!("this attachment is view-only; reconnect with --takeover to request input ownership"))?;
            match app.supervisor.renew_input(&binding, &lease, seconds) {
                Ok(expires_at) => ControlResponse::success(
                    serde_json::json!({"mode":"owner","expires_at":expires_at}),
                ),
                Err(error) => {
                    if persistent {
                        clear_and_reconcile_persistent_lease(
                            app,
                            connection,
                            "the exact human input lease could not be renewed",
                        );
                    } else if let Some(attachment) = connection.attachment.as_mut() {
                        attachment.lease = None;
                    }
                    return Err(error);
                }
            }
        }
        ControlRequest::AttachmentSendInput { data_base64 } => {
            if connection
                .attachment
                .as_ref()
                .is_some_and(|attachment| attachment.cmux_surface_route_id.is_some())
            {
                if let Err(error) = require_persistent_actual_control(app, connection) {
                    clear_and_reconcile_persistent_lease(
                        app,
                        connection,
                        "persistent input was rejected because this connection no longer has exact keyboard authority",
                    );
                    return Err(error);
                }
            }
            let (binding, lease) = connection.lease()?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(data_base64)?;
            app.supervisor
                .write_attachment_input(binding, lease, &bytes)?;
            ControlResponse::success(serde_json::json!({"written":bytes.len()}))
        }
        ControlRequest::AttachmentResize { rows, cols } => {
            if connection
                .attachment
                .as_ref()
                .is_some_and(|attachment| attachment.cmux_surface_route_id.is_some())
            {
                if let Err(error) = require_persistent_actual_control(app, connection) {
                    clear_and_reconcile_persistent_lease(
                        app,
                        connection,
                        "persistent resize was rejected because this connection no longer has exact keyboard authority",
                    );
                    return Err(error);
                }
            }
            let (binding, lease) = connection.lease()?;
            app.supervisor
                .resize_attachment(binding, lease, rows, cols)?;
            ControlResponse::success(
                serde_json::json!({"rows":rows,"cols":cols,"authority":"authenticated_human_control"}),
            )
        }
        ControlRequest::AttachmentDetach => {
            connection.release_attachment(app);
            ControlResponse::success(serde_json::json!({"detached":true,"provider_stopped":false}))
        }
        ControlRequest::CapabilityInterrupt { session_id } => {
            app.supervisor.interrupt(&session_id)?;
            ControlResponse::success(
                serde_json::json!({"interrupt_requested": true, "quiescent": false}),
            )
        }
        ControlRequest::CapabilityResume { session_id, prompt } => {
            ControlResponse::success(app.resume_validation(&session_id, &prompt)?)
        }
        ControlRequest::CapabilityRecordProof { proof } => {
            ControlResponse::success(app.store.record_capability_proof(&proof)?)
        }
        ControlRequest::HumanCommand { command } => {
            ControlResponse::success(app.execute_human_command(&command)?)
        }
        ControlRequest::SchedulerRunOnce => ControlResponse::success(app.coordinator_tick()?),
        ControlRequest::SnapshotFreeze {
            attempt_id,
            snapshot_kind,
        } => {
            app.supervisor.reconcile()?;
            ControlResponse::success(app.reviews.freeze(&attempt_id, &snapshot_kind)?)
        }
        ControlRequest::SnapshotVerify { snapshot_id } => {
            ControlResponse::success(serde_json::json!({
                "snapshot_id": snapshot_id,
                "verified": app.reviews.verify(&snapshot_id)?,
            }))
        }
        ControlRequest::RoleDispatch {
            attempt_id,
            role,
            lane,
            prompt,
        } => ControlResponse::success(match lane {
            Some(lane) => {
                if role != RoleKind::Implementer {
                    bail!("only implementer dispatch accepts a lane")
                }
                app.dispatch_implementation_lane(&attempt_id, &lane, &prompt)?
            }
            None => app.dispatch_attempt_role(&attempt_id, role, &prompt)?,
        }),
        ControlRequest::TripSetupDispatch { attempt_id, role } => {
            ControlResponse::success(app.dispatch_trip_setup_role(&attempt_id, role)?)
        }
        ControlRequest::RoleSwitchFinish { intent_id } => {
            ControlResponse::success(app.roles.finish_switch(&intent_id)?)
        }
        ControlRequest::RoleResume { session_id, prompt } => {
            ControlResponse::success(app.resume_role_session(&session_id, &prompt)?)
        }
        ControlRequest::RestartResume {
            operation_id,
            session_ids,
        } => ControlResponse::success(
            app.resume_restart_sessions(&operation_id, session_ids.as_deref())?,
        ),
        ControlRequest::RoleSwitchRequest {
            operation_id,
            attempt_id,
            role,
            old_generation_id,
            settings_revision,
            snapshot_id,
            handoff,
            expected_task_version,
        } => ControlResponse::success(serde_json::json!({"intent_id":app.roles.request_switch(
                &operation_id,&attempt_id,&role,&old_generation_id,settings_revision,&snapshot_id,handoff,expected_task_version
            )?,"state":"stopping_old"})),
        ControlRequest::GuidanceDeliver { guidance_id } => {
            ControlResponse::success(app.roles.deliver_guidance(&guidance_id)?)
        }
        ControlRequest::State => ControlResponse::success(crate::workflow::state(&app.store)?),
        ControlRequest::TaskRecoveryBindings { task_id } => ControlResponse::success(
            crate::workflow::task_recovery_bindings(&app.store, &task_id)?,
        ),
        ControlRequest::Diagnostics { limit } => {
            ControlResponse::success(app.diagnostics.read_sanitized(limit.min(2_000))?)
        }
        ControlRequest::CheckRun {
            attempt_id,
            suite_name,
            check_id,
        } => ControlResponse::success(match (check_id, suite_name) {
            (Some(check), None) => app.checks.run_selected(&attempt_id, &check)?,
            (None, Some(suite)) => app.checks.run_configured(&attempt_id, &suite)?,
            _ => bail!("check run requires exactly one selected check_id or legacy suite_name"),
        }),
        ControlRequest::LegacyPreview { source } => {
            ControlResponse::success(serde_json::to_value(crate::import::preview(&source)?)?)
        }
        ControlRequest::LegacyImport {
            operation_id,
            project_id,
            expected_project_version,
            source,
            expected_source_hash,
        } => ControlResponse::success(crate::import::apply(
            &app.store,
            &operation_id,
            &project_id,
            expected_project_version,
            &source,
            &expected_source_hash,
        )?),
    }
}

fn blocking_control_request(request: &ControlRequest) -> bool {
    match request {
        ControlRequest::Attach { .. }
        | ControlRequest::AttachmentTranscript { .. }
        | ControlRequest::AttachmentRenew { .. }
        | ControlRequest::AttachmentControlSync
        | ControlRequest::AttachmentSendInput { .. }
        | ControlRequest::AttachmentResize { .. }
        | ControlRequest::AttachmentDetach => false,
        ControlRequest::Status
        | ControlRequest::RestartPreview
        | ControlRequest::Stop { .. }
        | ControlRequest::CapabilityLaunch { .. }
        | ControlRequest::CapabilityWorkflowLaunch { .. }
        | ControlRequest::CapabilityList
        | ControlRequest::CapabilityInspect { .. }
        | ControlRequest::CapabilityTranscript { .. }
        | ControlRequest::CapabilityAcquireInput { .. }
        | ControlRequest::CapabilityRenewInput { .. }
        | ControlRequest::CapabilityTakeoverInput { .. }
        | ControlRequest::CapabilitySendInput { .. }
        | ControlRequest::CapabilityReleaseInput { .. }
        | ControlRequest::CapabilityResize { .. }
        | ControlRequest::CmuxAttachmentFailed { .. }
        | ControlRequest::CmuxSessionSurfaceFailed { .. }
        | ControlRequest::CapabilityInterrupt { .. }
        | ControlRequest::CapabilityResume { .. }
        | ControlRequest::CapabilityRecordProof { .. }
        | ControlRequest::HumanCommand { .. }
        | ControlRequest::SchedulerRunOnce
        | ControlRequest::SnapshotFreeze { .. }
        | ControlRequest::SnapshotVerify { .. }
        | ControlRequest::RoleDispatch { .. }
        | ControlRequest::TripSetupDispatch { .. }
        | ControlRequest::RoleSwitchFinish { .. }
        | ControlRequest::RoleResume { .. }
        | ControlRequest::RestartResume { .. }
        | ControlRequest::RoleSwitchRequest { .. }
        | ControlRequest::GuidanceDeliver { .. }
        | ControlRequest::State
        | ControlRequest::TaskRecoveryBindings { .. }
        | ControlRequest::Diagnostics { .. }
        | ControlRequest::CheckRun { .. }
        | ControlRequest::LegacyPreview { .. }
        | ControlRequest::LegacyImport { .. } => true,
    }
}

fn signal_shutdown(shutdown: &watch::Sender<bool>) -> Result<()> {
    match shutdown.send(true) {
        Ok(()) => Ok(()),
        Err(_) if *shutdown.borrow() => Ok(()),
        Err(_) => Err(anyhow!("service shutdown channel is closed")),
    }
}

async fn read_control_message<R>(reader: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    let read = reader
        .take((MAX_CONTROL_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if read > MAX_CONTROL_MESSAGE_BYTES || line.last() != Some(&b'\n') {
        bail!("control messages must be newline-delimited and no larger than 1 MiB")
    }
    line.pop();
    Ok(Some(line))
}

async fn write_control_response(
    writer: &mut OwnedWriteHalf,
    response: &ControlResponse,
) -> Result<()> {
    let mut encoded = serde_json::to_vec(response)?;
    if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
        encoded = serde_json::to_vec(&ControlResponse::failure(anyhow!(
            "control response exceeds the 1 MiB message limit"
        )))?;
    }
    writer.write_all(&encoded).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

pub async fn request(socket: &Path, request: &ControlRequest) -> Result<ControlResponse> {
    ControlConnection::connect(socket)
        .await?
        .request(request)
        .await
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
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    values: [u32; 8],
}

struct PeerAdmissionProof {
    identity: PeerProcessIdentity,
    #[cfg(target_os = "macos")]
    audit_token: AuditToken,
}

#[cfg(target_os = "macos")]
impl PeerAdmissionProof {
    fn revalidate_after_inventory(&mut self) -> Result<()> {
        validate_audit_token_live(&mut self.audit_token)
    }
}

#[cfg(not(target_os = "macos"))]
impl PeerAdmissionProof {
    fn revalidate_after_inventory(&mut self) -> Result<()> {
        bail!("human control peer identity requires the supported macOS audit-token connector")
    }
}

#[cfg(target_os = "macos")]
#[link(name = "bsm")]
extern "C" {
    fn audit_token_to_pid(token: AuditToken) -> libc::pid_t;
    fn audit_token_to_pidversion(token: AuditToken) -> libc::c_int;
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
extern "C" {
    fn proc_pidpath_audittoken(
        token: *mut AuditToken,
        buffer: *mut libc::c_void,
        buffer_size: u32,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
fn peer_admission(stream: &UnixStream) -> Result<PeerAdmissionProof> {
    let mut token = AuditToken { values: [0; 8] };
    let mut length = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            &mut token as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("read control peer audit token");
    }
    if length as usize != std::mem::size_of::<AuditToken>() {
        bail!("control peer audit token has an unexpected size")
    }
    let pid = unsafe { audit_token_to_pid(token) };
    let pid_version = unsafe { audit_token_to_pidversion(token) };
    if pid <= 0 || pid_version <= 0 {
        bail!("control peer audit token has no usable PID/version identity")
    }
    validate_audit_token_live(&mut token)?;
    let native_start_marker = crate::supervisor::native_start_marker(pid as u32)?;
    // The second kernel validation rejects an exit/reuse race during the
    // inventory-start capture; two PID reads alone would not bind the peer.
    validate_audit_token_live(&mut token)?;
    let identity = PeerProcessIdentity::new(pid as u32, native_start_marker)
        .map_err(|error| anyhow!(error))?;
    Ok(PeerAdmissionProof {
        identity,
        audit_token: token,
    })
}

#[cfg(target_os = "macos")]
fn validate_audit_token_live(token: &mut AuditToken) -> Result<()> {
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe {
        proc_pidpath_audittoken(
            token as *mut AuditToken,
            path.as_mut_ptr().cast(),
            path.len() as u32,
        )
    };
    if length <= 0 {
        return Err(std::io::Error::last_os_error())
            .context("kernel validation of control peer audit token");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn peer_admission(_stream: &UnixStream) -> Result<PeerAdmissionProof> {
    bail!("human control peer identity requires the supported macOS audit-token connector")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InstancePaths;
    use crate::domain::{
        AttachmentBinding, CmuxAttachmentMode, CmuxKeyboardControlAction, ProcessIdentity,
    };
    use crate::store::Store;
    use rusqlite::params;
    use std::cell::Cell;
    use std::os::fd::AsRawFd;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    fn control_test_application() -> (std::path::PathBuf, Application, AttachmentBinding) {
        let root =
            std::env::temp_dir().join(format!("agenticjira-control-g14-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        let binding = AttachmentBinding {
            session_id: uuid::Uuid::new_v4().to_string(),
            role_generation_id: uuid::Uuid::new_v4().to_string(),
            transcript_epoch: uuid::Uuid::new_v4().to_string(),
            process: ProcessIdentity {
                pid: 8181,
                process_group_id: 8181,
                native_start_marker: "control-g14-live-process".into(),
                observed_started_at: "2026-01-01T00:00:00Z".into(),
            },
        };
        let connection = store.lock().unwrap();
        connection.execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('project','project','/tmp/project','control-g14-project','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('task','project','task','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('attempt','task','context','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        ).unwrap();
        connection.execute(
            "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
             VALUES(?1,'attempt','manager','codex',1,1,'running','control-g14-authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            params![binding.role_generation_id],
        ).unwrap();
        connection.execute(
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
        ).unwrap();
        drop(connection);
        let app = Application::new(paths, store, std::env::current_exe().unwrap()).unwrap();
        (root, app, binding)
    }

    #[test]
    fn transcript_request_rejects_outside_path_without_mutating_sentinel() {
        let (root, app, _) = control_test_application();
        let outside = root.join("outside.jsonl");
        let sentinel = b"malformed control transcript sentinel";
        std::fs::write(&outside, sentinel).unwrap();
        let (shutdown, _) = watch::channel(false);
        let mut connection = ConnectionState {
            peer_identity: PeerProcessIdentity::new(1, "control-transcript-test".into()).unwrap(),
            attachment: None,
        };
        let _error = dispatch(
            ControlRequest::CapabilityTranscript {
                session_id: "../outside".to_owned(),
                after_epoch: None,
                after_sequence: 0,
                limit_bytes: 1024,
            },
            &app,
            &shutdown,
            &serde_json::json!({}),
            &mut connection,
        )
        .unwrap_err();
        assert_eq!(std::fs::read(&outside).unwrap(), sentinel);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn attachment_stateful_requests_remain_on_the_ordered_connection_path() {
        let requests = [
            ControlRequest::Attach {
                session_id: "session".into(),
                owner_id: "owner".into(),
                seconds: 30,
                takeover: false,
                view_only: true,
                expected_binding: None,
                cmux_route_id: None,
                cmux_surface_route_id: None,
                cmux_binding_revision: None,
            },
            ControlRequest::AttachmentTranscript {
                after_epoch: None,
                after_sequence: 0,
                limit_bytes: 1024,
            },
            ControlRequest::AttachmentRenew { seconds: 30 },
            ControlRequest::AttachmentControlSync,
            ControlRequest::AttachmentSendInput {
                data_base64: "eA==".into(),
            },
            ControlRequest::AttachmentResize { rows: 24, cols: 80 },
            ControlRequest::AttachmentDetach,
        ];
        assert!(requests
            .iter()
            .all(|request| !blocking_control_request(request)));
        assert!(blocking_control_request(&ControlRequest::Status));
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

    #[test]
    fn persistent_attachment_sync_during_create_holds_until_surface_is_bound() {
        let (root, app, binding) = control_test_application();
        let boot = app.service_boot_id();
        let (workspace, _) = app.store.reserve_cmux_task_workspace(boot, "task").unwrap();
        let (surface, _) = app
            .store
            .reserve_cmux_session_surface(boot, &workspace.id, &binding)
            .unwrap();
        assert!(app
            .store
            .claim_cmux_task_workspace_create(&workspace.id, &surface.id, boot)
            .unwrap());
        let connected = app
            .store
            .mark_cmux_session_surface_connected(
                &surface.id,
                boot,
                &binding,
                surface.binding_revision,
            )
            .unwrap();
        let mut connection = persistent_connection(binding.clone(), &connected, String::new());
        connection.attachment.as_mut().unwrap().lease = None;
        let (shutdown, _) = watch::channel(false);
        let sync = dispatch(
            ControlRequest::AttachmentControlSync,
            &app,
            &shutdown,
            &serde_json::json!({}),
            &mut connection,
        )
        .unwrap();
        assert_eq!(sync.result["state"], "unknown_live");
        assert_eq!(sync.result["actual_input_state"], "view_only");
        assert!(connection.attachment.as_ref().unwrap().lease.is_none());
        let opened = app
            .store
            .mark_cmux_workspace_and_initial_surface_open(
                &workspace.id,
                &surface.id,
                boot,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        assert_eq!(opened.attachment_state, "live");
        assert_eq!(
            persistent_sync_payload(&opened, "bound")["state"],
            "view_only"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn grant_persistent_controller(
        app: &Application,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        surface: &crate::domain::CmuxSessionSurface,
    ) -> (crate::domain::CmuxSessionSurface, String) {
        let pending = app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                service_boot_id,
                &binding.session_id,
                &surface.id,
                surface.binding_revision,
                surface.control_revision,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap();
        assert!(app
            .store
            .acknowledge_cmux_attachment_control(
                &surface.id,
                service_boot_id,
                binding,
                surface.binding_revision,
                pending.surface.control_revision,
                "control",
                None,
            )
            .unwrap());
        let lease = format!("persistent-control-{}", uuid::Uuid::new_v4());
        app.store
            .acquire_input_lease(
                &binding.session_id,
                &lease,
                "persistent-control-test-owner",
                &serde_json::to_string(&binding.process).unwrap(),
                &binding.role_generation_id,
                "2999-01-01T00:00:00Z",
            )
            .unwrap();
        (app.store.cmux_session_surface(&surface.id).unwrap(), lease)
    }

    fn persistent_connection(
        binding: AttachmentBinding,
        surface: &crate::domain::CmuxSessionSurface,
        lease: String,
    ) -> ConnectionState {
        ConnectionState {
            peer_identity: PeerProcessIdentity::new(1, "persistent-control-peer".into()).unwrap(),
            attachment: Some(ConnectionAttachment {
                binding,
                owner_id: "persistent-control-test-owner".to_owned(),
                lease: Some(lease),
                cmux_route_id: None,
                cmux_surface_route_id: Some(surface.id.clone()),
                cmux_binding_revision: Some(surface.binding_revision),
            }),
        }
    }

    fn close_on_exec(fd: RawFd) -> bool {
        unsafe { libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC != 0 }
    }

    fn late_persistent_request(operation: &str) -> ControlRequest {
        match operation {
            "renewal" => ControlRequest::AttachmentRenew { seconds: 30 },
            "input" => ControlRequest::AttachmentSendInput {
                data_base64: "eA==".to_owned(),
            },
            "resize" => ControlRequest::AttachmentResize { rows: 24, cols: 80 },
            _ => unreachable!("unknown late persistent operation {operation}"),
        }
    }

    fn assert_late_persistent_cleanup(lifecycle: &str, operation: &str) {
        let (root, app, binding) = control_test_application();
        let service_boot_id = app.service_boot_id().to_owned();
        let surface = open_persistent_surface(&app, &service_boot_id, &binding);
        let (surface, lease) =
            grant_persistent_controller(&app, &service_boot_id, &binding, &surface);
        let surface = match lifecycle {
            "unknown" => app
                .store
                .mark_cmux_session_surface_observation_unknown(
                    &surface.id,
                    &service_boot_id,
                    "fixture malformed cmux observation",
                )
                .unwrap(),
            "lost" => {
                app.store
                    .mark_cmux_session_surface_lost(
                        &surface.id,
                        &service_boot_id,
                        surface.workspace_id.as_deref().unwrap(),
                        surface.surface_id.as_deref().unwrap(),
                    )
                    .unwrap();
                app.store.cmux_session_surface(&surface.id).unwrap()
            }
            _ => unreachable!("unknown persistent lifecycle {lifecycle}"),
        };
        let causal_error = surface.last_error.clone().expect("fixture causal error");
        let mut connection = persistent_connection(binding.clone(), &surface, lease.clone());
        assert_eq!(
            connection
                .attachment
                .as_ref()
                .and_then(|attachment| attachment.lease.as_deref()),
            Some(lease.as_str()),
            "{lifecycle}/{operation} must begin with a fresh connection secret"
        );
        assert_eq!(
            app.store
                .active_input_lease_owner(&binding.session_id)
                .unwrap()
                .as_ref()
                .map(|(owner, _)| owner.as_str()),
            Some("persistent-control-test-owner"),
            "{lifecycle}/{operation} must begin with durable input authority"
        );
        let (shutdown, _) = watch::channel(false);
        assert!(
            dispatch(
                late_persistent_request(operation),
                &app,
                &shutdown,
                &serde_json::json!({}),
                &mut connection,
            )
            .is_err(),
            "late {operation} unexpectedly retained {lifecycle}/live authority"
        );
        assert!(connection
            .attachment
            .as_ref()
            .and_then(|attachment| attachment.lease.as_deref())
            .is_none());
        assert_eq!(
            app.store
                .active_input_lease_owner(&binding.session_id)
                .unwrap(),
            None
        );
        let reconciled = app.store.cmux_session_surface(&surface.id).unwrap();
        assert_eq!(reconciled.surface_state, lifecycle);
        assert_eq!(reconciled.attachment_state, "live");
        assert_eq!(
            reconciled.actual_input_state,
            if lifecycle == "unknown" {
                "view_only"
            } else {
                "lost"
            }
        );
        assert_eq!(
            reconciled.last_error.as_deref().map(str::as_bytes),
            Some(causal_error.as_bytes()),
            "{lifecycle}/{operation} replaced the original causal error bytes"
        );
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn persistent_late_operations_clear_each_unknown_or_lost_live_authority() {
        for lifecycle in ["unknown", "lost"] {
            for operation in ["renewal", "input", "resize"] {
                assert_late_persistent_cleanup(lifecycle, operation);
            }
        }
    }

    #[test]
    fn persistent_retire_clears_the_secret_then_detach_records_the_exact_binding_ended() {
        let (root, app, binding) = control_test_application();
        let service_boot_id = app.service_boot_id().to_owned();
        let surface = open_persistent_surface(&app, &service_boot_id, &binding);
        let (surface, lease) =
            grant_persistent_controller(&app, &service_boot_id, &binding, &surface);
        app.store
            .mark_cmux_session_surface_lost(
                &surface.id,
                &service_boot_id,
                surface.workspace_id.as_deref().unwrap(),
                surface.surface_id.as_deref().unwrap(),
            )
            .unwrap();

        let mut connection = persistent_connection(binding.clone(), &surface, lease.clone());
        let (shutdown, _) = watch::channel(false);
        let retired = dispatch(
            ControlRequest::AttachmentControlSync,
            &app,
            &shutdown,
            &serde_json::json!({}),
            &mut connection,
        )
        .unwrap();
        assert_eq!(retired.result["state"], "retired");
        assert!(!serde_json::to_string(&retired).unwrap().contains(&lease));
        assert!(connection
            .attachment
            .as_ref()
            .and_then(|attachment| attachment.lease.as_deref())
            .is_none());
        assert_eq!(
            app.store
                .active_input_lease_owner(&binding.session_id)
                .unwrap(),
            None
        );
        assert_eq!(
            app.store
                .cmux_session_surface(&surface.id)
                .unwrap()
                .attachment_state,
            "live",
            "Retire clears secret first; only detach marks the exact connection ended"
        );

        let detached = dispatch(
            ControlRequest::AttachmentDetach,
            &app,
            &shutdown,
            &serde_json::json!({}),
            &mut connection,
        )
        .unwrap();
        assert_eq!(detached.result["detached"], true);
        assert_eq!(
            app.store
                .cmux_session_surface(&surface.id)
                .unwrap()
                .attachment_state,
            "ended"
        );

        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shutdown_signal_accepts_only_an_already_requested_closed_channel() {
        let (shutdown, receiver) = watch::channel(false);
        signal_shutdown(&shutdown).unwrap();
        assert!(*receiver.borrow());

        let (shutdown, receiver) = watch::channel(false);
        shutdown.send(true).unwrap();
        drop(receiver);
        signal_shutdown(&shutdown).unwrap();

        let (shutdown, receiver) = watch::channel(false);
        drop(receiver);
        assert_eq!(
            signal_shutdown(&shutdown).unwrap_err().to_string(),
            "service shutdown channel is closed"
        );
        assert!(!*shutdown.borrow());
    }

    #[tokio::test]
    async fn control_socket_client_listener_and_accepted_descriptors_are_close_on_exec() {
        let (root, app, _) = control_test_application();
        let listener = bind(&app).unwrap();
        assert!(close_on_exec(listener.as_raw_fd()));
        let accepted = async {
            let mut accepted = accept_control_stream(&listener).await.unwrap();
            let accepted_close_on_exec = close_on_exec(accepted.as_raw_fd());
            let mut frame = Vec::new();
            BufReader::new(&mut accepted)
                .read_until(b'\n', &mut frame)
                .await
                .unwrap();
            assert!(serde_json::from_slice::<Hello>(&frame).is_ok());
            let response = serde_json::json!({"ok":true,"descriptor":protocol::Descriptor::new("fixture".into(), ClientKind::HumanCli)});
            let mut encoded = serde_json::to_vec(&response).unwrap();
            encoded.push(b'\n');
            accepted.write_all(&encoded).await.unwrap();
            accepted_close_on_exec
        };
        let (client, accepted_close_on_exec) = tokio::join!(
            ControlConnection::connect(&app.paths.control_socket),
            accepted
        );
        let client = client.unwrap();
        assert!(close_on_exec(
            client.writer.as_ref().unwrap().as_ref().as_raw_fd()
        ));
        assert!(accepted_close_on_exec);
        drop(client);
        drop(listener);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn hello_refuses_business_first_and_duplicate_without_dispatch() {
        let (root, app, _) = control_test_application();
        let (shutdown, _) = watch::channel(false);
        for (first, reason) in [
            (br#"{"kind":"status"}"#.as_slice(), "missing"),
            (br#"{"kind":"hello","generation":2,"client_kind":"human_cli","required_features":[]}"#.as_slice(), "incompatible_generation"),
            (br#"{"kind":"hello","generation":0,"client_kind":"human_cli","required_features":[]}"#.as_slice(), "incompatible_generation"),
            (br#"{"kind":"hello","generation":1,"client_kind":"browser","required_features":[]}"#.as_slice(), "wrong_client_kind"),
            (br#"{"kind":"hello","generation":1,"client_kind":"human_cli","required_features":["native_provider_supported"]}"#.as_slice(), "missing_required_feature"),
            (br#"{"kind":"hello","generation":1,"client_kind":"human_cli","required_features":[],"extra":true}"#.as_slice(), "malformed"),
        ] {
            let (server, client) = UnixStream::pair().unwrap();
            let (server_reader, mut server_writer) = server.into_split();
            let app = app.clone();
            let shutdown = shutdown.clone();
            let task = tokio::spawn(async move {
                serve_connection(
                    &mut BufReader::new(server_reader), &mut server_writer,
                    app, shutdown, serde_json::json!({"instance_id":"fixture"}),
                    ConnectionState {
                        peer_identity: PeerProcessIdentity::new(1, "fixture".into()).unwrap(),
                        attachment: None,
                    },
                    std::time::Duration::from_secs(1),
                ).await
            });
            let (client_reader, mut client_writer) = client.into_split();
            let mut client_reader = BufReader::new(client_reader);
            client_writer.write_all(first).await.unwrap();
            client_writer.write_all(b"\n").await.unwrap();
            let mut response = Vec::new();
            client_reader.read_until(b'\n', &mut response).await.unwrap();
            let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
            assert_eq!(response["protocol_error"]["reason"], reason);
            assert_eq!(client_reader.read_u8().await.unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
            task.await.unwrap().unwrap();
        }

        let (server, client) = UnixStream::pair().unwrap();
        let (server_reader, mut server_writer) = server.into_split();
        let server_app = app.clone();
        let task = tokio::spawn(async move {
            serve_connection(
                &mut BufReader::new(server_reader),
                &mut server_writer,
                server_app,
                shutdown,
                serde_json::json!({"instance_id":"fixture"}),
                ConnectionState {
                    peer_identity: PeerProcessIdentity::new(1, "fixture".into()).unwrap(),
                    attachment: None,
                },
                std::time::Duration::from_secs(1),
            )
            .await
        });
        let (client_reader, mut client_writer) = client.into_split();
        let mut client_reader = BufReader::new(client_reader);
        let hello = b"{\"kind\":\"hello\",\"generation\":1,\"client_kind\":\"human_cli\",\"required_features\":[\"control_requests_v1\"]}\n";
        client_writer.write_all(hello).await.unwrap();
        let mut response = Vec::new();
        client_reader
            .read_until(b'\n', &mut response)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response).unwrap()["ok"],
            true
        );
        client_writer
            .write_all(b"{\"kind\":\"status\"}\n")
            .await
            .unwrap();
        response.clear();
        client_reader
            .read_until(b'\n', &mut response)
            .await
            .unwrap();
        assert!(
            serde_json::from_slice::<ControlResponse>(&response)
                .unwrap()
                .ok
        );
        client_writer.write_all(hello).await.unwrap();
        response.clear();
        client_reader
            .read_until(b'\n', &mut response)
            .await
            .unwrap();
        assert!(
            !serde_json::from_slice::<ControlResponse>(&response)
                .unwrap()
                .ok
        );
        assert_eq!(
            client_reader.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        task.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn closed_older_server_does_not_receive_a_business_request() {
        let (root, app, _) = control_test_application();
        let listener = bind(&app).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut first = Vec::new();
            reader.read_until(b'\n', &mut first).await.unwrap();
            assert!(serde_json::from_slice::<Hello>(&first).is_ok());
            let mut second = Vec::new();
            assert!(tokio::time::timeout(
                std::time::Duration::from_millis(50),
                reader.read_until(b'\n', &mut second),
            )
            .await
            .is_err());
        });
        let error = ControlConnection::connect(&app.paths.control_socket)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("may be older or may have closed"));
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn g14_idle_connection_releases_exact_lease_and_route_after_healthy_polling() {
        let (root, app, binding) = control_test_application();
        let service_boot_id = app.service_boot_id().to_owned();
        let (route, created) = app
            .store
            .reserve_cmux_attachment_route(&service_boot_id, &binding, CmuxAttachmentMode::Control)
            .unwrap();
        assert!(created);
        app.store
            .mark_cmux_surface_open(
                &route.id,
                &service_boot_id,
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        app.store
            .mark_cmux_attachment_connected(
                &route.id,
                &service_boot_id,
                &binding,
                CmuxAttachmentMode::Control,
            )
            .unwrap();
        let lease = "g14-idle-lease".to_owned();
        app.store
            .acquire_input_lease(
                &binding.session_id,
                &lease,
                "g14-human",
                &serde_json::to_string(&binding.process).unwrap(),
                &binding.role_generation_id,
                "2999-01-01T00:00:00Z",
            )
            .unwrap();

        let (server, client) = UnixStream::pair().unwrap();
        let (server_reader, server_writer) = server.into_split();
        let (shutdown, _) = watch::channel(false);
        let server_app = app.clone();
        let server_binding = binding.clone();
        let server_lease = lease.clone();
        let server_route_id = route.id.clone();
        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            let mut writer = server_writer;
            serve_connection(
                &mut reader,
                &mut writer,
                server_app,
                shutdown,
                serde_json::json!({}),
                ConnectionState {
                    peer_identity: PeerProcessIdentity::new(1, "g14-peer".into()).unwrap(),
                    attachment: Some(ConnectionAttachment {
                        binding: server_binding,
                        owner_id: "g14-human".to_owned(),
                        lease: Some(server_lease),
                        cmux_route_id: Some(server_route_id),
                        cmux_surface_route_id: None,
                        cmux_binding_revision: None,
                    }),
                },
                std::time::Duration::from_millis(100),
            )
            .await
        });
        let (client_reader, mut client_writer) = client.into_split();
        let mut client_reader = BufReader::new(client_reader);
        client_writer.write_all(b"{\"kind\":\"hello\",\"generation\":1,\"client_kind\":\"attachment\",\"required_features\":[\"attachment_v1\"]}\n").await.unwrap();
        let mut descriptor = Vec::new();
        client_reader
            .read_until(b'\n', &mut descriptor)
            .await
            .unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&descriptor).unwrap()["ok"] == true);
        for _ in 0..3 {
            client_writer
                .write_all(b"{\"kind\":\"status\"}\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            client_reader
                .read_until(b'\n', &mut response)
                .await
                .unwrap();
            assert!(
                serde_json::from_slice::<ControlResponse>(&response)
                    .unwrap()
                    .ok
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(
            app.store
                .active_input_lease_owner(&binding.session_id)
                .unwrap()
                .as_ref()
                .map(|(owner, _)| owner.as_str()),
            Some("g14-human"),
        );
        assert_eq!(
            app.store
                .cmux_attachment_route(&route.id)
                .unwrap()
                .attachment_state,
            "live"
        );

        let idle = tokio::time::timeout(std::time::Duration::from_millis(400), server_task)
            .await
            .expect("injected idle deadline did not end the control connection")
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(idle.contains("idle read deadline expired"));
        assert_eq!(
            app.store
                .active_input_lease_owner(&binding.session_id)
                .unwrap(),
            None
        );
        assert_eq!(
            app.store
                .cmux_attachment_route(&route.id)
                .unwrap()
                .attachment_state,
            "ended"
        );
        drop(client_writer);
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn control_admission_rejects_a_post_inventory_same_pid_start_replacement_before_dispatch() {
        let peer = PeerProcessIdentity::new(4242, "Thu Sep 18 12:00:00 2026".into()).unwrap();
        let same_pid_start_replacement =
            PeerProcessIdentity::new(4242, "Thu Sep 18 12:00:00 2026".into()).unwrap();
        let inventory_calls = Cell::new(0);
        let kernel_generation_is_current = Cell::new(true);
        let dispatches = Cell::new(0);

        let error = admit_human_control(
            peer.clone(),
            |observed| {
                inventory_calls.set(inventory_calls.get() + 1);
                assert_eq!(observed, &same_pid_start_replacement);
                // The replacement has the same PID and coarse lstart marker,
                // but the retained audit-token generation is no longer live.
                kernel_generation_is_current.set(false);
                Ok(false)
            },
            || {
                if kernel_generation_is_current.get() {
                    Ok(())
                } else {
                    Err(anyhow!("original audit-token generation is no longer live"))
                }
            },
            |_| {
                dispatches.set(dispatches.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(inventory_calls.get(), 1);
        assert_eq!(dispatches.get(), 0);
        assert!(format!("{error:#}").contains("original audit-token generation is no longer live"));
    }

    #[test]
    fn control_admission_dispatches_once_when_the_original_generation_is_current() {
        let peer = PeerProcessIdentity::new(4242, "Thu Sep 18 12:00:00 2026".into()).unwrap();
        let inventory_calls = Cell::new(0);
        let dispatches = Cell::new(0);

        let admitted = admit_human_control(
            peer.clone(),
            |observed| {
                inventory_calls.set(inventory_calls.get() + 1);
                assert_eq!(observed, &peer);
                Ok(false)
            },
            || Ok(()),
            |identity| {
                dispatches.set(dispatches.get() + 1);
                Ok(identity)
            },
        )
        .unwrap();

        assert_eq!(admitted, peer);
        assert_eq!(inventory_calls.get(), 1);
        assert_eq!(dispatches.get(), 1);
    }
}
