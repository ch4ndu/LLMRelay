use crate::domain::{
    AttachmentBinding, CmuxAttachmentMode, CmuxAttachmentRoute, CmuxKeyboardControlAction,
    CmuxKeyboardControlOutcome, CmuxSessionSurface, CmuxTaskWorkspace, CmuxViewOutcome,
    TranscriptFrame, TranscriptPage,
};
use crate::operations::Application;
use crate::transcript;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::{cell::RefCell, collections::VecDeque};

const CMUX_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const CMUX_COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;
const RECORDED_OUTPUT_LIMIT: usize = 64 * 1024;
const MAX_HEALTH_SURFACES: usize = 256;
const MAX_CAPABILITY_METHODS: usize = 2_048;
const MAX_TREE_WINDOWS: usize = 64;
const MAX_TREE_WORKSPACES: usize = 512;
const MAX_TREE_PANES: usize = 2_048;
const MAX_TREE_SURFACES: usize = 4_096;

/// The browser never speaks to cmux directly.  This is deliberately a small,
/// one-shot bridge which creates a fresh owned surface or focuses an already
/// live attachment.  It never sends text to a pre-existing terminal.
#[derive(Clone, Debug, Serialize)]
pub struct CmuxRouteOutcome {
    pub state: String,
    pub mode: CmuxAttachmentMode,
    pub message: String,
    pub retry_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<CmuxAttachmentRoute>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_output: Option<TranscriptPage>,
}

enum CmuxCommandError {
    Unavailable(String),
    AccessDenied(String),
    TimedOut,
    Failed(String),
}

impl CmuxCommandError {
    fn message(&self) -> String {
        match self {
            Self::Unavailable(detail) => format!("cmux is unavailable: {detail}"),
            Self::AccessDenied(detail) => format!("cmux access was denied: {detail}"),
            Self::TimedOut => "cmux did not respond before the bounded timeout".to_owned(),
            Self::Failed(detail) => format!("cmux did not accept the attachment request: {detail}"),
        }
    }
}

pub fn open_attachment(
    app: &Application,
    service_boot_id: &str,
    session_id: &str,
    mode: CmuxAttachmentMode,
) -> Result<CmuxRouteOutcome> {
    uuid::Uuid::parse_str(session_id).context("cmux attachment session must be a UUID")?;
    let session = app.store.session_json(session_id)?;

    // Never manufacture an interactive surface for a dead, revoked, or failed
    // provider.  Its retained transcript is the explicit, bounded read-only
    // fallback and does not acquire an input lease or resume anything.
    if session["status"].as_str() != Some("running") {
        return recorded_output(
            app,
            session_id,
            session["transcript_epoch"]
                .as_str()
                .ok_or_else(|| anyhow!("session has no transcript epoch"))?,
            None,
            mode,
        );
    }

    let binding = match app.supervisor.establish_attachment(session_id) {
        Ok(binding) => binding,
        Err(_) => {
            return recorded_output(
                app,
                session_id,
                session["transcript_epoch"]
                    .as_str()
                    .ok_or_else(|| anyhow!("session has no transcript epoch"))?,
                None,
                mode,
            )
        }
    };
    open_attachment_with_binding(app, service_boot_id, &binding, mode)
}

pub fn discard_unknown_route(
    app: &Application,
    service_boot_id: &str,
    operation_id: &str,
    route_id: &str,
    session_id: &str,
) -> Result<CmuxRouteOutcome> {
    uuid::Uuid::parse_str(operation_id).context("cmux discard operation must be a UUID")?;
    uuid::Uuid::parse_str(route_id).context("cmux attachment route must be a UUID")?;
    uuid::Uuid::parse_str(session_id).context("cmux attachment session must be a UUID")?;
    let route = app.store.cmux_attachment_route(route_id)?;
    if route.service_boot_id != service_boot_id || route.binding.session_id != session_id {
        return Err(anyhow!(
            "unknown cmux route does not belong to this service and session"
        ));
    }
    if let Some(route) = app.store.cmux_unknown_discard_receipt(
        operation_id,
        service_boot_id,
        route_id,
        &route.binding,
    )? {
        return Ok(discarded_unknown_route_outcome(route));
    }
    let binding = app.supervisor.establish_attachment(session_id)?;
    if binding != route.binding {
        return Err(anyhow!(
            "unknown cmux route no longer matches the exact live process binding"
        ));
    }
    let route = app.store.discard_unknown_cmux_attachment_route(
        operation_id,
        service_boot_id,
        route_id,
        &binding,
    )?;
    Ok(discarded_unknown_route_outcome(route))
}

fn discarded_unknown_route_outcome(route: CmuxAttachmentRoute) -> CmuxRouteOutcome {
    CmuxRouteOutcome {
        state: "failed".to_owned(),
        mode: route.mode,
        message: "The unknown cmux reservation was discarded. No possible surface was addressed or closed, and no create was retried. Choose an attachment action again to make a new explicit create request.".to_owned(),
        retry_available: true,
        attachment_command: None,
        route: Some(route),
        recorded_output: None,
    }
}

fn open_attachment_with_binding(
    app: &Application,
    service_boot_id: &str,
    binding: &AttachmentBinding,
    mode: CmuxAttachmentMode,
) -> Result<CmuxRouteOutcome> {
    let fallback_command = attachment_command(app, binding, None, mode)?;
    if !app.automatic_cmux_routing_permitted() {
        return Ok(CmuxRouteOutcome {
            state: "failed".to_owned(),
            mode,
            message: format!(
                "Automatic cmux create, health, and focus are unavailable because this foreground LLMRelay host was not booted from a live cmux ancestor executable with the captured start identity. No route was reserved. Start the foreground `llmrelay serve` host from a cmux terminal, then retry. Manual attachment command: {fallback_command}"
            ),
            retry_available: false,
            attachment_command: Some(fallback_command),
            route: None,
            recorded_output: None,
        });
    }
    let (route, created) =
        match app
            .store
            .reserve_cmux_attachment_route(service_boot_id, binding, mode)
        {
            Ok(route) => route,
            Err(_) => {
                return recorded_output(
                    app,
                    &binding.session_id,
                    &binding.transcript_epoch,
                    Some(fallback_command),
                    mode,
                )
            }
        };
    let route_command = attachment_command(app, binding, Some(&route.id), route.mode)?;

    if !created {
        return open_existing_route(app, route, route_command, fallback_command);
    }

    let title = format!("LLMRelay attachment {}", short_id(&binding.session_id));
    let cwd = app
        .paths
        .root
        .to_str()
        .ok_or_else(|| anyhow!("LLMRelay data directory is not valid UTF-8"))?;
    let create = cmux_rpc(
        app,
        "surface.create",
        json!({
            "type": "terminal",
            "cwd": cwd,
            "title": title,
            "initial_command": route_command.clone(),
            "focus": true,
        }),
    );
    let (workspace_id, surface_id) = match create.and_then(|value| extract_surface_ids(&value)) {
        Ok(ids) => ids,
        Err(error) => {
            let surface_state = match &error {
                CmuxCommandError::Unavailable(_) | CmuxCommandError::AccessDenied(_) => "failed",
                CmuxCommandError::TimedOut | CmuxCommandError::Failed(_) => "unknown",
            };
            let message = unavailable_message(&error, &fallback_command);
            let route = app.store.fail_cmux_attachment_route(
                &route.id,
                service_boot_id,
                surface_state,
                &message,
            )?;
            if route.surface_state == "unknown" && route.attachment_state == "live" {
                return Ok(unknown_live_route_outcome(route));
            }
            // A timed-out or malformed create response has no trustworthy
            // surface identity.  A copied command may still attach manually,
            // but it must not claim the unknown managed route.
            let attachment_command = fallback_command;
            return Ok(CmuxRouteOutcome {
                state: "failed".to_owned(),
                mode: route.mode,
                message,
                retry_available: surface_state == "failed",
                attachment_command: Some(attachment_command),
                route: Some(route),
                recorded_output: None,
            });
        }
    };
    let route =
        app.store
            .mark_cmux_surface_open(&route.id, service_boot_id, &workspace_id, &surface_id)?;
    if matches!(route.attachment_state.as_str(), "failed" | "ended") {
        return Ok(CmuxRouteOutcome {
            state: "failed".to_owned(),
            mode: route.mode,
            message: "The new cmux surface was created, but its exact attachment ended before routing completed. It will not be reused or injected into; retry creates a fresh owned surface.".to_owned(),
            retry_available: true,
            attachment_command: Some(fallback_command),
            route: Some(route),
            recorded_output: None,
        });
    }

    // This command contains an exact, short-lived process binding and route
    // token. Registering it as cmux resume metadata both prompts the user on
    // every new surface and risks restoring a stale capability after the
    // provider changes. LLMRelay owns restoration from its durable session and
    // transcript state, so the cmux surface deliberately has no resume command.
    let route = app
        .store
        .set_cmux_resume_state(&route.id, service_boot_id, "unavailable", None)?;

    Ok(CmuxRouteOutcome {
        state: "opened".to_owned(),
        mode: route.mode,
        message: format!(
            "{} Dashboard approvals and workflow gates remain dashboard-owned. The ephemeral attachment command is not registered for cmux auto-restore; LLMRelay recreates terminal access from the current session binding.",
            attachment_opened_message(route.mode),
        ),
        retry_available: true,
        attachment_command: Some(route_command),
        route: Some(route),
        recorded_output: None,
    })
}

fn open_existing_route(
    app: &Application,
    route: CmuxAttachmentRoute,
    attachment_command: String,
    manual_attachment_command: String,
) -> Result<CmuxRouteOutcome> {
    if route.surface_state == "unknown" && route.attachment_state == "live" {
        return Ok(unknown_live_route_outcome(route));
    }
    if route.surface_state == "unknown" {
        let attachment_detail = match route.attachment_state.as_str() {
            "failed" => "The exact attachment client reported failure.",
            "ended" => "The exact attachment client connected and has ended.",
            _ => "The exact attachment client has not connected.",
        };
        return Ok(CmuxRouteOutcome {
            state: "failed".to_owned(),
            mode: route.mode,
            message: format!(
                "The previous cmux create request has an unknown result and will not be retried or reused. {attachment_detail} {} Discard the unknown route explicitly before requesting another automatic create. Manual attachment command: {manual_attachment_command}",
                route.last_error.as_deref().unwrap_or("No safe surface identity was recorded."),
            ),
            retry_available: false,
            attachment_command: Some(manual_attachment_command),
            route: Some(route),
            recorded_output: None,
        });
    }
    if route.attachment_state == "live" {
        if let (Some(workspace_id), Some(surface_id)) =
            (route.workspace_id.as_deref(), route.surface_id.as_deref())
        {
            // A stored UUID is not enough. Probe the exact surface, require a
            // matching ID in cmux's response, then focus it. No shell input or
            // respawn path is used for an existing surface.
            match verify_and_focus_live_surface(app, workspace_id, surface_id) {
                Ok(()) => {
                    return Ok(CmuxRouteOutcome {
                        state: "open".to_owned(),
                        mode: route.mode,
                        message: format!(
                            "Focused the verified live {} cmux attachment. {}",
                            route.mode.as_str(),
                            attachment_detach_message(route.mode),
                        ),
                        retry_available: true,
                        attachment_command: Some(attachment_command),
                        route: Some(route),
                        recorded_output: None,
                    });
                }
                Err(error) => {
                    return Ok(CmuxRouteOutcome {
                        state: "failed".to_owned(),
                        mode: route.mode,
                        message: unavailable_message(&error, &attachment_command),
                        retry_available: true,
                        attachment_command: Some(attachment_command),
                        route: Some(route),
                        recorded_output: None,
                    });
                }
            }
        }
    }

    let pending = route.attachment_state == "pending";
    Ok(CmuxRouteOutcome {
        state: if pending { "pending" } else { "failed" }.to_owned(),
        mode: route.mode,
        message: if pending {
            "The exact cmux attachment is still connecting. Retry checks its state only; it never types into that surface. Use the recorded attachment command only after starting the foreground host from cmux.".to_owned()
        } else {
            "The previous cmux attachment ended or failed. A new attempt will use a fresh owned surface and will not reuse the old terminal.".to_owned()
        },
        retry_available: true,
        attachment_command: Some(attachment_command),
        route: Some(route),
        recorded_output: None,
    })
}

fn unknown_live_route_outcome(route: CmuxAttachmentRoute) -> CmuxRouteOutcome {
    CmuxRouteOutcome {
        state: "open".to_owned(),
        mode: route.mode,
        message: "The exact cmux attachment client is live even though the create response did not provide a validated surface identity. Its output and control connection remain active, but LLMRelay cannot focus, address, discard, or reuse the unidentified surface. No create request was retried.".to_owned(),
        retry_available: false,
        attachment_command: None,
        route: Some(route),
        recorded_output: None,
    }
}

fn recorded_output(
    app: &Application,
    session_id: &str,
    transcript_epoch: &str,
    attachment_command: Option<String>,
    mode: CmuxAttachmentMode,
) -> Result<CmuxRouteOutcome> {
    uuid::Uuid::parse_str(transcript_epoch).context("recorded output epoch must be a UUID")?;
    let output = match transcript::read_attachment_frames(
        &app.paths.transcripts,
        session_id,
        transcript_epoch,
        None,
        0,
        RECORDED_OUTPUT_LIMIT,
    ) {
        Ok(page) => page,
        Err(error) => TranscriptPage {
            frames: vec![TranscriptFrame {
                epoch: transcript_epoch.to_owned(),
                sequence: 0,
                captured_at: chrono::Utc::now().to_rfc3339(),
                encoding: "utf8".to_owned(),
                data: format!(
                    "No retained recorded output is available for this exact session epoch: {}",
                    error
                ),
                gap: true,
            }],
            next_epoch: Some(transcript_epoch.to_owned()),
            next_sequence: 0,
            has_more: false,
        },
    };
    Ok(CmuxRouteOutcome {
        state: "recorded_output".to_owned(),
        mode,
        message: "This session is no longer attachable. Showing only bounded recorded output for its exact prior generation; no provider restart or input lease was requested.".to_owned(),
        retry_available: false,
        attachment_command,
        route: None,
        recorded_output: Some(output),
    })
}

fn attachment_command(
    app: &Application,
    binding: &AttachmentBinding,
    route_id: Option<&str>,
    mode: CmuxAttachmentMode,
) -> Result<String> {
    let executable = app
        .executable_path()
        .to_str()
        .ok_or_else(|| anyhow!("LLMRelay executable path is not valid UTF-8"))?;
    let data_dir = app
        .paths
        .root
        .to_str()
        .ok_or_else(|| anyhow!("LLMRelay data directory is not valid UTF-8"))?;
    let mut argv = vec![
        executable.to_owned(),
        "attach".to_owned(),
        "--data-dir".to_owned(),
        data_dir.to_owned(),
        "--session".to_owned(),
        binding.session_id.clone(),
        "--expected-generation".to_owned(),
        binding.role_generation_id.clone(),
        "--expected-epoch".to_owned(),
        binding.transcript_epoch.clone(),
        "--expected-pid".to_owned(),
        binding.process.pid.to_string(),
        "--expected-process-group".to_owned(),
        binding.process.process_group_id.to_string(),
        "--expected-start-marker".to_owned(),
        binding.process.native_start_marker.clone(),
        "--expected-observed-at".to_owned(),
        binding.process.observed_started_at.clone(),
    ];
    if let Some(route_id) = route_id {
        uuid::Uuid::parse_str(route_id).context("cmux route must be a UUID")?;
        argv.push("--cmux-route-id".to_owned());
        argv.push(route_id.to_owned());
    }
    if mode == CmuxAttachmentMode::Watch {
        argv.push("--view-only".to_owned());
    }
    Ok(argv
        .iter()
        .map(|value| shell_quote(value))
        .collect::<Vec<_>>()
        .join(" "))
}

fn verify_and_focus_live_surface(
    app: &Application,
    workspace_id: &str,
    surface_id: &str,
) -> std::result::Result<(), CmuxCommandError> {
    let health = cmux_rpc(
        app,
        "surface.health",
        json!({"workspace_id": workspace_id, "surface_id": surface_id}),
    )?;
    verify_health_surface(&health, workspace_id, surface_id)?;
    cmux_rpc(
        app,
        "surface.focus",
        json!({"workspace_id": workspace_id, "surface_id": surface_id}),
    )?;
    Ok(())
}

fn verify_health_surface(
    value: &Value,
    expected_workspace_id: &str,
    expected_surface_id: &str,
) -> std::result::Result<(), CmuxCommandError> {
    valid_tree_uuid(expected_workspace_id, "expected cmux health workspace")?;
    valid_tree_uuid(expected_surface_id, "expected cmux health surface")?;

    // A scalar `surface_id` is descriptive only.  It cannot prove that a
    // terminal remains present because it carries neither the complete bounded
    // inventory nor the terminal type.  Preserve it as an agreement check when
    // cmux returns it alongside the recorded `surfaces[]` contract.
    let candidates = [Some(value), value.get("result"), value.get("data")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    for candidate in &candidates {
        if let Some(workspace) = candidate.get("workspace_id") {
            if workspace.as_str() != Some(expected_workspace_id) {
                return health_contract_error("workspace_id did not match the exact request");
            }
        }
        if let Some(workspace) = candidate.get("workspace") {
            if workspace.get("id").and_then(Value::as_str) != Some(expected_workspace_id) {
                return health_contract_error(
                    "workspace object did not agree with the exact request",
                );
            }
        }
        if let Some(surface_id) = candidate.get("surface_id") {
            if surface_id.as_str() != Some(expected_surface_id) {
                return health_contract_error("surface_id did not agree with surfaces[]");
            }
        }
        if let Some(surface) = candidate.get("surface") {
            if surface.get("id").and_then(Value::as_str) != Some(expected_surface_id)
                || surface.get("type").and_then(Value::as_str) != Some("terminal")
            {
                return health_contract_error(
                    "surface object did not agree with the exact terminal",
                );
            }
        }
    }

    let inventories = candidates
        .into_iter()
        .filter(|candidate| candidate.get("surfaces").is_some())
        .collect::<Vec<_>>();
    if inventories.is_empty() {
        return health_contract_error("the bounded surfaces[] inventory was absent");
    }
    for candidate in inventories {
        if candidate.get("workspace_id").and_then(Value::as_str) != Some(expected_workspace_id) {
            return health_contract_error("surfaces[] did not carry the exact workspace_id");
        }
        let surfaces = candidate
            .get("surfaces")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CmuxCommandError::Failed(
                    "cmux health response did not provide a bounded surfaces[] array".to_owned(),
                )
            })?;
        if surfaces.len() > MAX_HEALTH_SURFACES {
            return Err(CmuxCommandError::Failed(
                "cmux health response exceeded the supported surface bound".to_owned(),
            ));
        }
        let mut ids = HashSet::with_capacity(surfaces.len());
        let mut requested_terminal_count = 0_usize;
        for surface in surfaces {
            let surface = surface.as_object().ok_or_else(|| {
                CmuxCommandError::Failed(
                    "cmux health surfaces[] contained a non-object entry".to_owned(),
                )
            })?;
            let id = surface.get("id").and_then(Value::as_str).ok_or_else(|| {
                CmuxCommandError::Failed("cmux health surfaces[] entry omitted its UUID".to_owned())
            })?;
            valid_tree_uuid(id, "cmux health surface")?;
            if !ids.insert(id) {
                return health_contract_error("surfaces[] contained a duplicate surface UUID");
            }
            let surface_type = surface.get("type").and_then(Value::as_str).ok_or_else(|| {
                CmuxCommandError::Failed("cmux health surfaces[] entry omitted its type".to_owned())
            })?;
            if id == expected_surface_id && surface_type == "terminal" {
                requested_terminal_count += 1;
            }
        }
        if requested_terminal_count != 1 {
            return health_contract_error("surfaces[] did not contain one exact terminal surface");
        }
    }
    Ok(())
}

fn health_contract_error<T>(detail: &str) -> std::result::Result<T, CmuxCommandError> {
    Err(CmuxCommandError::Failed(format!(
        "cmux health response did not prove the exact bounded terminal inventory: {detail}"
    )))
}

fn attachment_opened_message(mode: CmuxAttachmentMode) -> &'static str {
    match mode {
        CmuxAttachmentMode::Watch => {
            "Opened a read-only cmux watch attachment. It stays connected to output but does not acquire, renew, or resize an input lease; automatic manager guidance remains eligible. Use Take keyboard control only for an explicit human input action; Ctrl-] detaches."
        }
        CmuxAttachmentMode::Control => {
            "Opened an explicit cmux keyboard-control attachment. It requests the existing exclusive human input lease without taking over another owner; if another human owns it, this attachment remains view-only. Rerun the copied control command with --takeover only to explicitly revoke that lease. Ctrl-] detaches and releases any lease without stopping the provider."
        }
    }
}

fn attachment_detach_message(mode: CmuxAttachmentMode) -> &'static str {
    match mode {
        CmuxAttachmentMode::Watch => {
            "It remains read-only, so it does not hold a keyboard lease; Ctrl-] detaches without stopping the provider."
        }
        CmuxAttachmentMode::Control => {
            "Ctrl-] detaches and releases its human input lease without stopping the provider; --takeover remains an explicit copied-command action when another human owns input."
        }
    }
}

/// The persistent dashboard protocol is intentionally separate from the
/// migration-024 route API above.  A View operation reserves durable
/// presentation state before any cmux RPC and always starts a newly-created
/// physical terminal as view-only.  It never obtains a human input lease.
pub fn view(
    app: &Application,
    service_boot_id: &str,
    operation_id: &str,
    session_id: &str,
) -> Result<CmuxViewOutcome> {
    uuid::Uuid::parse_str(operation_id).context("cmux view operation must be a UUID")?;
    uuid::Uuid::parse_str(session_id).context("cmux view session must be a UUID")?;
    if app.service_boot_id() != service_boot_id {
        return Err(anyhow!(
            "cmux view service boot is not the current authenticated service"
        ));
    }
    if let Some(outcome) =
        app.store
            .reserve_cmux_view_operation(operation_id, service_boot_id, session_id)?
    {
        return Ok(outcome);
    }

    let outcome = match view_fresh(app, service_boot_id, session_id, operation_id) {
        Ok(outcome) => outcome,
        Err(error) => CmuxViewOutcome {
            state: "failed".to_owned(),
            message: format!(
                "The cmux presentation request stopped before it could safely create or focus a terminal: {}. No provider, workflow, approval, or native-resume action was requested.",
                bounded_detail(&error.to_string())
            ),
            retry_available: true,
            surface: None,
            recorded_output: None,
        },
    };
    app.store
        .finish_cmux_view_operation(operation_id, &outcome)?;
    cmux_diagnostic(
        app,
        if outcome.state == "failed" {
            "warn"
        } else {
            "info"
        },
        "cmux.view",
        &outcome.state,
        Some(operation_id),
        json!({
            "session_id": session_id,
            "surface_route_id": outcome.surface.as_ref().map(|surface| surface.id.as_str()),
            "binding_revision": outcome.surface.as_ref().map(|surface| surface.binding_revision),
        }),
    );
    Ok(outcome)
}

/// Changes only the desired keyboard state.  The existing authenticated
/// attachment connection later applies the exact revision; browser JSON never
/// carries a lease secret or a takeover capability.
pub fn set_keyboard_control(
    app: &Application,
    service_boot_id: &str,
    operation_id: &str,
    session_id: &str,
    surface_route_id: &str,
    expected_binding_revision: i64,
    expected_control_revision: i64,
    action: CmuxKeyboardControlAction,
) -> Result<CmuxKeyboardControlOutcome> {
    if app.service_boot_id() != service_boot_id {
        return Err(anyhow!(
            "cmux keyboard-control service boot is not the current authenticated service"
        ));
    }
    if let Some(outcome) = app.store.cmux_keyboard_control_receipt(
        operation_id,
        service_boot_id,
        session_id,
        surface_route_id,
        expected_binding_revision,
        expected_control_revision,
        action,
    )? {
        return Ok(outcome);
    }
    let surface = app.store.cmux_session_surface(surface_route_id)?;
    if surface.service_boot_id != service_boot_id
        || surface.binding.session_id != session_id
        || surface.binding_revision != expected_binding_revision
    {
        return Err(anyhow!(
            "cmux keyboard-control route does not belong to this exact current session binding"
        ));
    }
    // This browser operation records only a desired revision; it neither
    // obtains nor returns a lease. It still requires Supervisor's current
    // OS-backed attachment binding before it can advance that desired state.
    // Actual input acquisition remains confined to the authenticated
    // attachment connection, where the same fence guards the secret.
    let current_binding = app
        .supervisor
        .establish_attachment(session_id)
        .context("cmux keyboard-control route is stale for the current provider binding")?;
    if current_binding != surface.binding {
        return Err(anyhow!(
            "cmux keyboard-control route no longer matches the exact live provider binding"
        ));
    }
    let outcome = app.store.set_cmux_keyboard_control(
        operation_id,
        service_boot_id,
        session_id,
        surface_route_id,
        expected_binding_revision,
        expected_control_revision,
        action,
    )?;
    cmux_diagnostic(
        app,
        "info",
        "cmux.control.desired",
        "pending",
        Some(operation_id),
        json!({
            "session_id": session_id,
            "surface_route_id": surface_route_id,
            "binding_revision": expected_binding_revision,
            "control_revision": outcome.surface.control_revision,
            "desired_input_state": outcome.surface.desired_input_state,
        }),
    );
    Ok(outcome)
}

/// An explicit authenticated discard can retire only an uncertain new-protocol
/// row.  Looking up migration-024 IDs through this path fails before any
/// cmux surface is addressed, which keeps those historical rows audit-only.
pub fn discard_unknown_surface(
    app: &Application,
    service_boot_id: &str,
    operation_id: &str,
    surface_route_id: &str,
    session_id: &str,
) -> Result<CmuxViewOutcome> {
    if app.service_boot_id() != service_boot_id {
        return Err(anyhow!(
            "cmux unknown-discard service boot is not the current authenticated service"
        ));
    }
    uuid::Uuid::parse_str(operation_id).context("cmux discard operation must be a UUID")?;
    uuid::Uuid::parse_str(session_id).context("cmux discard session must be a UUID")?;
    let existing = app.store.cmux_session_surface(surface_route_id)?;
    if existing.service_boot_id != service_boot_id || existing.binding.session_id != session_id {
        return Err(anyhow!(
            "cmux unknown-discard route does not belong to this service and session"
        ));
    }
    let surface = app.store.discard_unknown_cmux_session_surface(
        operation_id,
        service_boot_id,
        surface_route_id,
        &existing.binding,
    )?;
    let outcome = CmuxViewOutcome {
        state: "failed".to_owned(),
        message: "The unknown cmux reservation was discarded without addressing, closing, focusing, or retrying any possible terminal. A later explicit View may reserve a fresh view-only surface.".to_owned(),
        retry_available: true,
        surface: Some(surface.clone()),
        recorded_output: None,
    };
    cmux_diagnostic(
        app,
        "info",
        "cmux.surface.unknown_discarded",
        "retired",
        Some(operation_id),
        json!({
            "session_id": session_id,
            "surface_route_id": surface.id,
            "binding_revision": surface.binding_revision,
        }),
    );
    Ok(outcome)
}

fn view_fresh(
    app: &Application,
    service_boot_id: &str,
    session_id: &str,
    operation_id: &str,
) -> Result<CmuxViewOutcome> {
    let session = app.store.session_json(session_id)?;
    if session["status"].as_str() != Some("running") {
        return persistent_recorded_output(
            app,
            session_id,
            session["transcript_epoch"]
                .as_str()
                .ok_or_else(|| anyhow!("session has no transcript epoch"))?,
        );
    }
    let binding = match app.supervisor.establish_attachment(session_id) {
        Ok(binding) => binding,
        Err(_) => {
            return persistent_recorded_output(
                app,
                session_id,
                session["transcript_epoch"]
                    .as_str()
                    .ok_or_else(|| anyhow!("session has no transcript epoch"))?,
            )
        }
    };
    if !app.automatic_cmux_routing_permitted() {
        return Ok(CmuxViewOutcome {
            state: "failed".to_owned(),
            message: "Automatic cmux presentation is unavailable because this foreground LLMRelay host was not booted from a proven live cmux ancestor executable. No task workspace or terminal surface was created. Start the foreground service from cmux, then choose View output again.".to_owned(),
            retry_available: false,
            surface: None,
            recorded_output: None,
        });
    }
    let task_id = session["task_id"]
        .as_str()
        .ok_or_else(|| anyhow!("running session has no task identity"))?;
    let (workspace, workspace_created) = app
        .store
        .reserve_cmux_task_workspace(service_boot_id, task_id)?;
    let (surface, surface_created) =
        app.store
            .reserve_cmux_session_surface(service_boot_id, &workspace.id, &binding)?;

    if workspace.state == "unknown" || surface.surface_state == "unknown" {
        return Ok(uncertain_surface_outcome(surface));
    }

    if let Err(error) = require_persistent_cmux_contract(app) {
        if workspace_created {
            let _ = app.store.fail_cmux_task_workspace_reservation(
                &workspace.id,
                service_boot_id,
                &format!("cmux contract is unavailable: {error}"),
            );
        } else if surface_created {
            let _ = app.store.fail_cmux_session_surface_reservation(
                &surface.id,
                service_boot_id,
                &format!("cmux contract is unavailable: {error}"),
            );
        }
        return Ok(CmuxViewOutcome {
            state: "failed".to_owned(),
            message: format!(
                "cmux did not satisfy the required persistent-routing contract: {}. No fallback workspace, shell command, title lookup, first surface, provider action, or keyboard lease was used.",
                bounded_detail(&error)
            ),
            retry_available: true,
            surface: Some(surface),
            recorded_output: None,
        });
    }

    if surface_created {
        return create_reserved_surface(app, service_boot_id, workspace, surface, operation_id);
    }
    view_existing_surface(app, service_boot_id, workspace, surface, operation_id)
}

fn create_reserved_surface(
    app: &Application,
    service_boot_id: &str,
    workspace: CmuxTaskWorkspace,
    surface: CmuxSessionSurface,
    operation_id: &str,
) -> Result<CmuxViewOutcome> {
    match workspace.state.as_str() {
        "opening" => {
            if !app.store.claim_cmux_task_workspace_create(
                &workspace.id,
                &surface.id,
                service_boot_id,
            )? {
                if workspace.opening_surface_id.as_deref() == Some(surface.id.as_str()) {
                    // A prior request already became the durable creator. It
                    // may have crossed the external boundary, so a later view
                    // must not issue a second workspace.create.
                    let surface = app.store.mark_cmux_workspace_create_unknown(
                        &workspace.id,
                        &surface.id,
                        service_boot_id,
                        "a previous durable workspace-create reservation may have reached cmux; no automatic retry is safe",
                    )?;
                    return Ok(uncertain_surface_outcome(surface));
                }
                return Ok(pending_surface_outcome(
                    surface,
                    "Another exact session surface is creating this task workspace. This action did not select, focus, or create any fallback terminal.",
                ));
            }
            let command = persistent_attachment_command(app, &surface.binding, &surface)?;
            let cwd = app
                .paths
                .root
                .to_str()
                .ok_or_else(|| anyhow!("LLMRelay data directory is not valid UTF-8"))?;
            let result = cmux_rpc(
                app,
                "workspace.create",
                json!({
                    "cwd": cwd,
                    "initial_command": command,
                    "focus": false,
                }),
            );
            let (workspace_id, surface_id) = match result.and_then(|value| extract_surface_ids(&value)) {
                Ok(ids) => ids,
                Err(error) => {
                    return workspace_create_error_outcome(
                        app,
                        service_boot_id,
                        &workspace,
                        &surface,
                        error,
                    )
                }
            };
            if let Err(error) = verify_new_surface_contract(app, &workspace_id, &surface_id) {
                let surface = app.store.mark_cmux_workspace_create_unknown(
                    &workspace.id,
                    &surface.id,
                    service_boot_id,
                    &format!("workspace create returned IDs but exact health or empty-resume validation was uncertain: {}", error.message()),
                )?;
                return Ok(uncertain_surface_outcome(surface));
            }
            let surface = app.store.mark_cmux_workspace_and_initial_surface_open(
                &workspace.id,
                &surface.id,
                service_boot_id,
                &workspace_id,
                &surface_id,
            )?;
            focus_persistent_surface(app, service_boot_id, surface, operation_id, true)
        }
        "open" => create_targeted_surface(app, service_boot_id, workspace, surface, operation_id),
        "unknown" => Ok(uncertain_surface_outcome(surface)),
        _ => Ok(CmuxViewOutcome {
            state: "failed".to_owned(),
            message: "The durable cmux task-workspace reservation is no longer eligible for an automatic create. No terminal was addressed; choose View output again to make a fresh explicit request.".to_owned(),
            retry_available: true,
            surface: Some(surface),
            recorded_output: None,
        }),
    }
}

fn create_targeted_surface(
    app: &Application,
    service_boot_id: &str,
    workspace: CmuxTaskWorkspace,
    surface: CmuxSessionSurface,
    operation_id: &str,
) -> Result<CmuxViewOutcome> {
    let Some(workspace_id) = workspace.workspace_id.as_deref() else {
        let surface = app.store.mark_cmux_session_surface_observation_unknown(
            &surface.id,
            service_boot_id,
            "the durable open task workspace has no exact cmux workspace identity",
        )?;
        return Ok(uncertain_surface_outcome(surface));
    };
    if !app
        .store
        .claim_cmux_targeted_surface_create(&workspace.id, &surface.id, service_boot_id)?
    {
        return Ok(pending_surface_outcome(
            surface,
            "Another exact action is already crossing the durable targeted-surface create boundary. This action did not issue a second create, select another terminal, or change keyboard control.",
        ));
    }
    let command = persistent_attachment_command(app, &surface.binding, &surface)?;
    let cwd = app
        .paths
        .root
        .to_str()
        .ok_or_else(|| anyhow!("LLMRelay data directory is not valid UTF-8"))?;
    let result = cmux_rpc(
        app,
        "surface.create",
        json!({
            "workspace_id": workspace_id,
            "type": "terminal",
            "cwd": cwd,
            "initial_command": command,
            "focus": false,
        }),
    );
    let (returned_workspace_id, surface_id) = match result
        .and_then(|value| extract_surface_ids(&value))
    {
        Ok(ids) => ids,
        Err(error) => return targeted_create_error_outcome(app, service_boot_id, &surface, error),
    };
    if returned_workspace_id != workspace_id {
        return targeted_create_error_outcome(
            app,
            service_boot_id,
            &surface,
            CmuxCommandError::Failed(
                "cmux targeted surface.create returned a different workspace identity".to_owned(),
            ),
        );
    }
    if let Err(error) = verify_new_surface_contract(app, workspace_id, &surface_id) {
        let surface = app.store.mark_cmux_targeted_surface_unknown(
            &surface.id,
            service_boot_id,
            &format!(
                "targeted surface create returned IDs but exact health or empty-resume validation was uncertain: {}",
                error.message()
            ),
        )?;
        return Ok(uncertain_surface_outcome(surface));
    }
    let surface = app.store.mark_cmux_targeted_surface_open(
        &surface.id,
        service_boot_id,
        workspace_id,
        &surface_id,
    )?;
    focus_persistent_surface(app, service_boot_id, surface, operation_id, true)
}

fn workspace_create_error_outcome(
    app: &Application,
    service_boot_id: &str,
    workspace: &CmuxTaskWorkspace,
    surface: &CmuxSessionSurface,
    error: CmuxCommandError,
) -> Result<CmuxViewOutcome> {
    match error {
        CmuxCommandError::Unavailable(_) | CmuxCommandError::AccessDenied(_) => {
            app.store.fail_cmux_task_workspace_reservation(
                &workspace.id,
                service_boot_id,
                &format!(
                    "workspace.create was not dispatched successfully: {}",
                    error.message()
                ),
            )?;
            let surface = app.store.cmux_session_surface(&surface.id)?;
            Ok(CmuxViewOutcome {
                state: "failed".to_owned(),
                message: format!(
                    "cmux could not create the task workspace: {}. No current workspace, title, shell, or first-surface fallback was used.",
                    bounded_detail(&error.message())
                ),
                retry_available: true,
                surface: Some(surface),
                recorded_output: None,
            })
        }
        _ => {
            let surface = app.store.mark_cmux_workspace_create_unknown(
                &workspace.id,
                &surface.id,
                service_boot_id,
                &format!(
                    "workspace.create has an uncertain result: {}",
                    error.message()
                ),
            )?;
            Ok(uncertain_surface_outcome(surface))
        }
    }
}

fn targeted_create_error_outcome(
    app: &Application,
    service_boot_id: &str,
    surface: &CmuxSessionSurface,
    error: CmuxCommandError,
) -> Result<CmuxViewOutcome> {
    match error {
        CmuxCommandError::Unavailable(_) | CmuxCommandError::AccessDenied(_) => {
            let surface = app.store.fail_cmux_session_surface_reservation(
                &surface.id,
                service_boot_id,
                &format!(
                    "targeted surface.create was not dispatched successfully: {}",
                    error.message()
                ),
            )?;
            Ok(CmuxViewOutcome {
                state: "failed".to_owned(),
                message: format!(
                    "cmux could not create the targeted task terminal: {}. No unrelated surface was selected or addressed.",
                    bounded_detail(&error.message())
                ),
                retry_available: true,
                surface: Some(surface),
                recorded_output: None,
            })
        }
        _ => {
            let surface = app.store.mark_cmux_targeted_surface_unknown(
                &surface.id,
                service_boot_id,
                &format!(
                    "targeted surface.create has an uncertain result: {}",
                    error.message()
                ),
            )?;
            Ok(uncertain_surface_outcome(surface))
        }
    }
}

fn verify_new_surface_contract(
    app: &Application,
    workspace_id: &str,
    surface_id: &str,
) -> std::result::Result<(), CmuxCommandError> {
    let health = cmux_rpc(
        app,
        "surface.health",
        json!({"workspace_id": workspace_id, "surface_id": surface_id}),
    )?;
    verify_health_surface(&health, workspace_id, surface_id)?;
    let resume = cmux_rpc(
        app,
        "surface.resume.get",
        json!({"workspace_id": workspace_id, "surface_id": surface_id}),
    )?;
    verify_empty_resume_metadata(&resume, workspace_id, surface_id)
}

fn view_existing_surface(
    app: &Application,
    service_boot_id: &str,
    workspace: CmuxTaskWorkspace,
    surface: CmuxSessionSurface,
    operation_id: &str,
) -> Result<CmuxViewOutcome> {
    if surface.surface_state == "unknown" {
        return Ok(uncertain_surface_outcome(surface));
    }
    if surface.surface_state == "opening" {
        if workspace.state == "open" && workspace.workspace_id.is_some() {
            return create_targeted_surface(app, service_boot_id, workspace, surface, operation_id);
        }
        return Ok(pending_surface_outcome(
            surface,
            "The exact cmux terminal reservation is still settling. This action did not issue a second create, select another surface, or request keyboard control.",
        ));
    }
    if surface.surface_state != "open" {
        return Ok(CmuxViewOutcome {
            state: "failed".to_owned(),
            message: "The prior cmux terminal is historical and will not be focused, respawned, injected into, or closed. Choose View output again to request a new view-only surface.".to_owned(),
            retry_available: true,
            surface: Some(surface),
            recorded_output: None,
        });
    }
    if !matches!(surface.attachment_state.as_str(), "pending" | "live") {
        return Ok(CmuxViewOutcome {
            state: "failed".to_owned(),
            message: "The prior cmux attachment ended or failed. Its terminal remains historical; the next explicit View creates a fresh view-only surface in the same task workspace.".to_owned(),
            retry_available: true,
            surface: Some(surface),
            recorded_output: None,
        });
    }
    let (Some(workspace_id), Some(surface_id)) = (
        workspace.workspace_id.as_deref(),
        surface.surface_id.as_deref(),
    ) else {
        let surface = app.store.mark_cmux_session_surface_observation_unknown(
            &surface.id,
            service_boot_id,
            "the durable open route has no exact workspace and terminal identity",
        )?;
        return Ok(uncertain_surface_outcome(surface));
    };
    if surface.workspace_id.as_deref() != Some(workspace_id) {
        let surface = app.store.mark_cmux_session_surface_observation_unknown(
            &surface.id,
            service_boot_id,
            "the session surface does not match its durable task-workspace identity",
        )?;
        return Ok(uncertain_surface_outcome(surface));
    }

    match inspect_global_tree(app, workspace_id, surface_id) {
        Ok(TreePresence::Present) => {}
        Ok(TreePresence::WorkspaceMissing) => {
            app.store.mark_cmux_task_workspace_lost(
                &workspace.id,
                service_boot_id,
                workspace_id,
            )?;
            cmux_diagnostic(
                app,
                "warn",
                "cmux.workspace.loss_classified",
                "lost",
                Some(operation_id),
                json!({
                    "task_workspace_id": workspace.id,
                    "workspace_id": workspace_id,
                    "surface_route_id": surface.id,
                    "source": "validated_system_tree",
                }),
            );
            return Ok(loss_retirement_outcome(
                app.store.cmux_session_surface(&surface.id)?,
                "The validated workspace loss is recorded, but its exact live attachment remains in the retirement interval. This View did not reserve, create, or focus a replacement. The authenticated connection must receive Retire, clear its in-memory lease, and record ended; an observed bounded exact lease expiry is the only alternate retirement proof.",
            ));
        }
        Ok(TreePresence::SurfaceMissing) => {
            app.store.mark_cmux_session_surface_lost(
                &surface.id,
                service_boot_id,
                workspace_id,
                surface_id,
            )?;
            cmux_diagnostic(
                app,
                "warn",
                "cmux.surface.loss_classified",
                "lost",
                Some(operation_id),
                json!({
                    "task_workspace_id": workspace.id,
                    "workspace_id": workspace_id,
                    "surface_route_id": surface.id,
                    "surface_id": surface_id,
                    "source": "validated_system_tree",
                }),
            );
            return Ok(loss_retirement_outcome(
                app.store.cmux_session_surface(&surface.id)?,
                "The validated terminal loss is recorded, but its exact live attachment remains in the retirement interval. This View did not reserve, create, or focus a replacement. The authenticated connection must receive Retire, clear its in-memory lease, and record ended; an observed bounded exact lease expiry is the only alternate retirement proof.",
            ));
        }
        Err(error) => {
            let surface = app.store.mark_cmux_session_surface_observation_unknown(
                &surface.id,
                service_boot_id,
                &format!(
                    "a global cmux tree did not meet the bounded exact loss schema: {}",
                    error.message()
                ),
            )?;
            return Ok(uncertain_surface_outcome(surface));
        }
    }

    let health = cmux_rpc(
        app,
        "surface.health",
        json!({"workspace_id": workspace_id, "surface_id": surface_id}),
    )
    .and_then(|value| verify_health_surface(&value, workspace_id, surface_id));
    if let Err(error) = health {
        let surface = app.store.mark_cmux_session_surface_observation_unknown(
            &surface.id,
            service_boot_id,
            &format!(
                "the exact owned surface could not be health-validated: {}",
                error.message()
            ),
        )?;
        return Ok(uncertain_surface_outcome(surface));
    }
    focus_persistent_surface(app, service_boot_id, surface, operation_id, false)
}

fn loss_retirement_outcome(surface: CmuxSessionSurface, message: &str) -> CmuxViewOutcome {
    let retirement_pending = surface.attachment_state == "live";
    CmuxViewOutcome {
        state: if retirement_pending {
            "pending".to_owned()
        } else {
            "failed".to_owned()
        },
        message: if retirement_pending {
            message.to_owned()
        } else {
            "The validated cmux loss is historical and no live attachment remains. This operation intentionally did not create a replacement; submit a new explicit View after the durable retirement record is complete."
                .to_owned()
        },
        retry_available: !retirement_pending,
        surface: Some(surface),
        recorded_output: None,
    }
}

fn focus_persistent_surface(
    app: &Application,
    service_boot_id: &str,
    surface: CmuxSessionSurface,
    operation_id: &str,
    created: bool,
) -> Result<CmuxViewOutcome> {
    let (Some(workspace_id), Some(surface_id)) = (
        surface.workspace_id.as_deref(),
        surface.surface_id.as_deref(),
    ) else {
        let surface = app.store.mark_cmux_session_surface_observation_unknown(
            &surface.id,
            service_boot_id,
            "a route saved as open has no exact physical terminal identity",
        )?;
        return Ok(uncertain_surface_outcome(surface));
    };
    if let Err(error) = cmux_rpc(
        app,
        "surface.focus",
        json!({"workspace_id": workspace_id, "surface_id": surface_id}),
    ) {
        let surface = app.store.mark_cmux_session_surface_observation_unknown(
            &surface.id,
            service_boot_id,
            &format!(
                "cmux could not focus the exact owned terminal: {}",
                error.message()
            ),
        )?;
        return Ok(uncertain_surface_outcome(surface));
    }
    cmux_diagnostic(
        app,
        "info",
        if created {
            "cmux.surface.created_and_focused"
        } else {
            "cmux.surface.focused"
        },
        "ok",
        Some(operation_id),
        json!({
            "surface_route_id": surface.id,
            "binding_revision": surface.binding_revision,
            "workspace_id": workspace_id,
            "surface_id": surface_id,
        }),
    );
    Ok(live_surface_outcome(surface, created))
}

fn pending_surface_outcome(surface: CmuxSessionSurface, message: &str) -> CmuxViewOutcome {
    CmuxViewOutcome {
        state: "pending".to_owned(),
        message: message.to_owned(),
        retry_available: false,
        surface: Some(surface),
        recorded_output: None,
    }
}

fn uncertain_surface_outcome(surface: CmuxSessionSurface) -> CmuxViewOutcome {
    let live = surface.attachment_state == "live";
    CmuxViewOutcome {
        state: if live { "unknown_live" } else { "unknown" }.to_owned(),
        message: if live {
            "The exact attachment remains connected, but cmux presentation state is uncertain. This View did not change its existing input lease or control revision; it cannot be focused, discarded, recreated, or used for a new keyboard-control action until that live attachment ends. No loss was inferred and no create was retried.".to_owned()
        } else {
            "The cmux create or observation result is uncertain. No loss was inferred, no terminal was selected by fallback, and no automatic retry will occur. Detach any exact live child first, then explicitly discard this reservation before requesting another View.".to_owned()
        },
        retry_available: false,
        surface: Some(surface),
        recorded_output: None,
    }
}

fn live_surface_outcome(surface: CmuxSessionSurface, created: bool) -> CmuxViewOutcome {
    let state = if surface.attachment_state == "pending" {
        "pending"
    } else if surface.actual_input_state == "control" {
        "control"
    } else if surface.actual_input_state == "blocked" {
        "blocked"
    } else {
        "view_only"
    };
    let message = if surface.attachment_state == "pending" {
        if created {
            "Created and focused one exact task terminal. Its mode-neutral attachment is connecting view-only; Take keyboard control remains pending until that authenticated connection is live."
        } else {
            "Focused the exact task terminal. Its mode-neutral attachment is still connecting view-only; this action did not request or change keyboard control."
        }
    } else if surface.actual_input_state == "control" {
        "Focused the exact task terminal while its existing authenticated attachment retains keyboard control. View output did not acquire, release, renew, resize, or take over that lease."
    } else if surface.actual_input_state == "blocked" {
        "Focused the exact task terminal. Keyboard control remains blocked by another authenticated human lease; View output did not take it over."
    } else if created {
        "Created and focused one exact task terminal in view-only mode. It replays retained output for this exact epoch and does not restart the provider, resume a workflow, or restore prior keyboard control."
    } else {
        "Focused the exact task terminal without changing its existing keyboard-control state. Provider lifecycle, workflow gates, approvals, and native resume remain outside cmux."
    };
    CmuxViewOutcome {
        state: state.to_owned(),
        message: message.to_owned(),
        retry_available: true,
        surface: Some(surface),
        recorded_output: None,
    }
}

fn persistent_recorded_output(
    app: &Application,
    session_id: &str,
    transcript_epoch: &str,
) -> Result<CmuxViewOutcome> {
    uuid::Uuid::parse_str(transcript_epoch).context("recorded output epoch must be a UUID")?;
    let recorded_output = match transcript::read_attachment_frames(
        &app.paths.transcripts,
        session_id,
        transcript_epoch,
        None,
        0,
        RECORDED_OUTPUT_LIMIT,
    ) {
        Ok(page) => page,
        Err(error) => TranscriptPage {
            frames: vec![TranscriptFrame {
                epoch: transcript_epoch.to_owned(),
                sequence: 0,
                captured_at: chrono::Utc::now().to_rfc3339(),
                encoding: "utf8".to_owned(),
                data: format!(
                    "No retained recorded output is available for this exact session epoch: {}",
                    bounded_detail(&error.to_string())
                ),
                gap: true,
            }],
            next_epoch: Some(transcript_epoch.to_owned()),
            next_sequence: 0,
            has_more: false,
        },
    };
    Ok(CmuxViewOutcome {
        state: "recorded_output".to_owned(),
        message: "This session is no longer attachable. Showing only bounded retained output for its exact prior epoch; no provider restart, workflow resume, native resume, or input lease was requested.".to_owned(),
        retry_available: false,
        surface: None,
        recorded_output: Some(recorded_output),
    })
}

fn persistent_attachment_command(
    app: &Application,
    binding: &AttachmentBinding,
    surface: &CmuxSessionSurface,
) -> Result<String> {
    binding
        .validate_route_identifiers()
        .map_err(|error| anyhow!(error))?;
    uuid::Uuid::parse_str(&surface.id).context("cmux session surface must be a UUID")?;
    if surface.binding != *binding || surface.binding_revision <= 0 {
        return Err(anyhow!(
            "persistent attachment command does not match the exact reserved surface binding"
        ));
    }
    let executable = app
        .executable_path()
        .to_str()
        .ok_or_else(|| anyhow!("LLMRelay executable path is not valid UTF-8"))?;
    let data_dir = app
        .paths
        .root
        .to_str()
        .ok_or_else(|| anyhow!("LLMRelay data directory is not valid UTF-8"))?;
    let argv = vec![
        executable.to_owned(),
        "attach".to_owned(),
        "--data-dir".to_owned(),
        data_dir.to_owned(),
        "--session".to_owned(),
        binding.session_id.clone(),
        "--expected-generation".to_owned(),
        binding.role_generation_id.clone(),
        "--expected-epoch".to_owned(),
        binding.transcript_epoch.clone(),
        "--expected-pid".to_owned(),
        binding.process.pid.to_string(),
        "--expected-process-group".to_owned(),
        binding.process.process_group_id.to_string(),
        "--expected-start-marker".to_owned(),
        binding.process.native_start_marker.clone(),
        "--expected-observed-at".to_owned(),
        binding.process.observed_started_at.clone(),
        "--cmux-surface-route-id".to_owned(),
        surface.id.clone(),
        "--cmux-binding-revision".to_owned(),
        surface.binding_revision.to_string(),
        "--view-only".to_owned(),
    ];
    Ok(argv
        .iter()
        .map(|value| shell_quote(value))
        .collect::<Vec<_>>()
        .join(" "))
}

const REQUIRED_PERSISTENT_CMUX_METHODS: &[&str] = &[
    "workspace.create",
    "workspace.close",
    "system.tree",
    "surface.create",
    "surface.close",
    "surface.focus",
    "surface.health",
    "surface.read_text",
    "surface.resume.get",
];

fn require_persistent_cmux_contract(app: &Application) -> std::result::Result<(), String> {
    let response = cmux_rpc(app, "system.capabilities", json!({}))
        .map_err(|error| bounded_detail(&error.message()))?;
    let capability = structured_payload(&response)
        .ok_or_else(|| "cmux capabilities did not contain a structured payload".to_owned())?;
    if capability.get("protocol").and_then(Value::as_str) != Some("cmux-socket") {
        return Err("cmux capabilities did not report protocol cmux-socket".to_owned());
    }
    if capability.get("version").and_then(Value::as_u64) != Some(2) {
        return Err("cmux capabilities did not report protocol version 2".to_owned());
    }
    let methods = capability
        .get("methods")
        .and_then(Value::as_array)
        .ok_or_else(|| "cmux capabilities did not provide a methods array".to_owned())?;
    if methods.len() > MAX_CAPABILITY_METHODS {
        return Err("cmux capabilities methods array exceeded the bounded contract".to_owned());
    }
    let mut observed = HashSet::with_capacity(methods.len());
    for method in methods {
        let method = method
            .as_str()
            .filter(|method| !method.is_empty() && method.len() <= 128)
            .ok_or_else(|| "cmux capabilities methods contained a malformed value".to_owned())?;
        if !observed.insert(method) {
            return Err("cmux capabilities methods contained a duplicate value".to_owned());
        }
    }
    let missing = REQUIRED_PERSISTENT_CMUX_METHODS
        .iter()
        .copied()
        .filter(|method| !observed.contains(method))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "cmux capabilities are missing required methods: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn verify_empty_resume_metadata(
    value: &Value,
    expected_workspace_id: &str,
    expected_surface_id: &str,
) -> std::result::Result<(), CmuxCommandError> {
    for candidate in [Some(value), value.get("result"), value.get("data")]
        .into_iter()
        .flatten()
    {
        if candidate.get("workspace_id").and_then(Value::as_str) != Some(expected_workspace_id)
            || candidate.get("surface_id").and_then(Value::as_str) != Some(expected_surface_id)
        {
            continue;
        }
        if candidate.get("restore_record").is_some_and(Value::is_null)
            && candidate.get("resume_binding").is_some_and(Value::is_null)
        {
            return Ok(());
        }
    }
    Err(CmuxCommandError::Failed(
        "cmux did not prove that the new exact surface has null resume metadata".to_owned(),
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TreePresence {
    Present,
    WorkspaceMissing,
    SurfaceMissing,
}

fn inspect_global_tree(
    app: &Application,
    expected_workspace_id: &str,
    expected_surface_id: &str,
) -> std::result::Result<TreePresence, CmuxCommandError> {
    let response = cmux_rpc(app, "system.tree", json!({}))?;
    let inventory = parse_global_tree(&response)?;
    let Some(surfaces) = inventory.get(expected_workspace_id) else {
        return Ok(TreePresence::WorkspaceMissing);
    };
    if !surfaces.contains(expected_surface_id) {
        return Ok(TreePresence::SurfaceMissing);
    }
    Ok(TreePresence::Present)
}

fn parse_global_tree(
    value: &Value,
) -> std::result::Result<HashMap<String, HashSet<String>>, CmuxCommandError> {
    let root = [Some(value), value.get("result"), value.get("data")]
        .into_iter()
        .flatten()
        .find(|candidate| candidate.get("windows").is_some())
        .ok_or_else(|| {
            CmuxCommandError::Failed(
                "system.tree did not provide the required global windows inventory".to_owned(),
            )
        })?;
    let root = required_object(root, "system.tree")?;
    let windows = bounded_array(root, "windows", MAX_TREE_WINDOWS, "system.tree")?;
    let mut window_ids = HashSet::new();
    let mut workspace_ids = HashSet::new();
    let mut pane_ids = HashSet::new();
    let mut surface_ids = HashSet::new();
    let mut workspace_total = 0_usize;
    let mut pane_total = 0_usize;
    let mut surface_total = 0_usize;
    let mut inventory = HashMap::<String, HashSet<String>>::new();

    for window in windows {
        let window = required_object(window, "system.tree window")?;
        let window_id = required_uuid(window, "id", "system.tree window")?;
        if !window_ids.insert(window_id.to_owned()) {
            return malformed_tree("duplicate window UUID");
        }
        let workspaces = bounded_array(
            window,
            "workspaces",
            MAX_TREE_WORKSPACES,
            "system.tree window",
        )?;
        let declared_count = required_count(window, "workspace_count", "system.tree window")?;
        if declared_count != workspaces.len() {
            return malformed_tree("workspace_count did not match the workspace inventory");
        }
        workspace_total = workspace_total.saturating_add(workspaces.len());
        if workspace_total > MAX_TREE_WORKSPACES {
            return malformed_tree("workspace inventory exceeded the bounded contract");
        }
        for workspace in workspaces {
            let workspace = required_object(workspace, "system.tree workspace")?;
            let workspace_id = required_uuid(workspace, "id", "system.tree workspace")?;
            if !workspace_ids.insert(workspace_id.to_owned()) {
                return malformed_tree("duplicate workspace UUID");
            }
            let panes = bounded_array(workspace, "panes", MAX_TREE_PANES, "system.tree workspace")?;
            pane_total = pane_total.saturating_add(panes.len());
            if pane_total > MAX_TREE_PANES {
                return malformed_tree("pane inventory exceeded the bounded contract");
            }
            let workspace_surfaces = inventory.entry(workspace_id.to_owned()).or_default();
            for pane in panes {
                let pane = required_object(pane, "system.tree pane")?;
                let pane_id = required_uuid(pane, "id", "system.tree pane")?;
                if !pane_ids.insert(pane_id.to_owned()) {
                    return malformed_tree("duplicate pane UUID");
                }
                let listed_surface_ids =
                    bounded_array(pane, "surface_ids", MAX_TREE_SURFACES, "system.tree pane")?;
                let surfaces =
                    bounded_array(pane, "surfaces", MAX_TREE_SURFACES, "system.tree pane")?;
                let declared_count = required_count(pane, "surface_count", "system.tree pane")?;
                if declared_count != listed_surface_ids.len() || declared_count != surfaces.len() {
                    return malformed_tree(
                        "surface_count did not match the complete pane surface inventory",
                    );
                }
                surface_total = surface_total.saturating_add(surfaces.len());
                if surface_total > MAX_TREE_SURFACES {
                    return malformed_tree("surface inventory exceeded the bounded contract");
                }
                let mut listed = HashSet::with_capacity(listed_surface_ids.len());
                for surface_id in listed_surface_ids {
                    let surface_id = surface_id.as_str().ok_or_else(|| {
                        CmuxCommandError::Failed(
                            "system.tree pane surface_ids contained a non-string value".to_owned(),
                        )
                    })?;
                    valid_tree_uuid(surface_id, "system.tree pane surface_ids")?;
                    if !listed.insert(surface_id.to_owned()) {
                        return malformed_tree("duplicate surface UUID in pane surface_ids");
                    }
                }
                let mut observed = HashSet::with_capacity(surfaces.len());
                for surface in surfaces {
                    let surface = required_object(surface, "system.tree surface")?;
                    let surface_id = required_uuid(surface, "id", "system.tree surface")?;
                    let terminal =
                        surface.get("type").and_then(Value::as_str).ok_or_else(|| {
                            CmuxCommandError::Failed(
                                "system.tree surface did not provide string type".to_owned(),
                            )
                        })? == "terminal";
                    if surface.get("pane_id").and_then(Value::as_str) != Some(pane_id) {
                        return malformed_tree("surface pane_id did not match its containing pane");
                    }
                    if !surface_ids.insert(surface_id.to_owned()) {
                        return malformed_tree("duplicate surface UUID");
                    }
                    observed.insert(surface_id.to_owned());
                    if terminal {
                        workspace_surfaces.insert(surface_id.to_owned());
                    }
                }
                if observed != listed {
                    return malformed_tree("surface_ids did not match complete surface objects");
                }
            }
        }
    }
    Ok(inventory)
}

fn structured_payload(value: &Value) -> Option<&Value> {
    [Some(value), value.get("result"), value.get("data")]
        .into_iter()
        .flatten()
        .find(|candidate| candidate.is_object())
}

fn required_object<'a>(
    value: &'a Value,
    label: &str,
) -> std::result::Result<&'a serde_json::Map<String, Value>, CmuxCommandError> {
    value
        .as_object()
        .ok_or_else(|| CmuxCommandError::Failed(format!("{label} was not an object")))
}

fn bounded_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    maximum: usize,
    label: &str,
) -> std::result::Result<&'a Vec<Value>, CmuxCommandError> {
    let array = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| CmuxCommandError::Failed(format!("{label} did not provide array {key}")))?;
    if array.len() > maximum {
        return Err(CmuxCommandError::Failed(format!(
            "{label} array {key} exceeded its bounded contract"
        )));
    }
    Ok(array)
}

fn required_count(
    object: &serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> std::result::Result<usize, CmuxCommandError> {
    let count = object.get(key).and_then(Value::as_u64).ok_or_else(|| {
        CmuxCommandError::Failed(format!("{label} did not provide integer {key}"))
    })?;
    usize::try_from(count).map_err(|_| {
        CmuxCommandError::Failed(format!("{label} integer {key} exceeded platform bounds"))
    })
}

fn required_uuid<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    label: &str,
) -> std::result::Result<&'a str, CmuxCommandError> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CmuxCommandError::Failed(format!("{label} did not provide string {key}")))?;
    valid_tree_uuid(value, label)?;
    Ok(value)
}

fn valid_tree_uuid(value: &str, label: &str) -> std::result::Result<(), CmuxCommandError> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| CmuxCommandError::Failed(format!("{label} contained a malformed UUID")))
}

fn malformed_tree<T>(detail: &str) -> std::result::Result<T, CmuxCommandError> {
    Err(CmuxCommandError::Failed(format!(
        "system.tree was internally inconsistent: {detail}"
    )))
}

fn bounded_detail(detail: &str) -> String {
    detail.chars().take(512).collect()
}

fn cmux_diagnostic(
    app: &Application,
    severity: &str,
    event_code: &str,
    outcome: &str,
    operation_id: Option<&str>,
    detail: Value,
) {
    let _ = app
        .diagnostics
        .record(severity, event_code, "cmux", outcome, operation_id, detail);
}

fn cmux_rpc(
    app: &Application,
    method: &str,
    params: Value,
) -> std::result::Result<Value, CmuxCommandError> {
    #[cfg(test)]
    if let Some(hook) = take_test_cmux_rpc_hook() {
        hook();
    }
    #[cfg(test)]
    if let Some(result) = take_test_cmux_rpc_result() {
        return result;
    }
    let output = run_cmux(
        app,
        vec![
            "rpc".to_owned(),
            method.to_owned(),
            serde_json::to_string(&params)
                .map_err(|error| CmuxCommandError::Failed(error.to_string()))?,
        ],
    )?;
    serde_json::from_str(&output).map_err(|_| {
        CmuxCommandError::Failed(
            "cmux did not return the required structured JSON response".to_owned(),
        )
    })
}

#[cfg(test)]
thread_local! {
    static TEST_CMUX_RPC_RESULTS: RefCell<VecDeque<std::result::Result<Value, CmuxCommandError>>> = RefCell::new(VecDeque::new());
    static TEST_CMUX_RPC_HOOKS: RefCell<VecDeque<Box<dyn FnOnce()>>> = RefCell::new(VecDeque::new());
    static TEST_CMUX_COMMAND_RESULTS: RefCell<VecDeque<std::result::Result<String, CmuxCommandError>>> = RefCell::new(VecDeque::new());
}

#[cfg(test)]
fn take_test_cmux_rpc_hook() -> Option<Box<dyn FnOnce()>> {
    TEST_CMUX_RPC_HOOKS.with(|hooks| hooks.borrow_mut().pop_front())
}

#[cfg(test)]
fn push_test_cmux_rpc_hook(hook: impl FnOnce() + 'static) {
    TEST_CMUX_RPC_HOOKS.with(|hooks| hooks.borrow_mut().push_back(Box::new(hook)));
}

#[cfg(test)]
fn take_test_cmux_rpc_result() -> Option<std::result::Result<Value, CmuxCommandError>> {
    TEST_CMUX_RPC_RESULTS.with(|results| results.borrow_mut().pop_front())
}

#[cfg(test)]
fn push_test_cmux_rpc_result(result: std::result::Result<Value, CmuxCommandError>) {
    TEST_CMUX_RPC_RESULTS.with(|results| results.borrow_mut().push_back(result));
}

#[cfg(test)]
fn test_cmux_rpc_result_count() -> usize {
    TEST_CMUX_RPC_RESULTS.with(|results| results.borrow().len())
}

#[cfg(test)]
fn clear_test_cmux_rpc_results() {
    TEST_CMUX_RPC_RESULTS.with(|results| results.borrow_mut().clear());
    TEST_CMUX_RPC_HOOKS.with(|hooks| hooks.borrow_mut().clear());
}

fn extract_surface_ids(value: &Value) -> std::result::Result<(String, String), CmuxCommandError> {
    for candidate in [Some(value), value.get("result"), value.get("data")]
        .into_iter()
        .flatten()
    {
        let workspace = candidate
            .get("workspace_id")
            .and_then(Value::as_str)
            .or_else(|| {
                candidate
                    .get("workspace")
                    .and_then(|item| item.get("id"))
                    .and_then(Value::as_str)
            });
        let surface = candidate
            .get("surface_id")
            .and_then(Value::as_str)
            .or_else(|| {
                candidate
                    .get("surface")
                    .and_then(|item| item.get("id"))
                    .and_then(Value::as_str)
            });
        if let (Some(workspace), Some(surface)) = (workspace, surface) {
            if uuid::Uuid::parse_str(workspace).is_ok() && uuid::Uuid::parse_str(surface).is_ok() {
                return Ok((workspace.to_owned(), surface.to_owned()));
            }
        }
    }
    Err(CmuxCommandError::Failed(
        "cmux did not return validated workspace and surface UUIDs".to_owned(),
    ))
}

fn run_cmux(app: &Application, args: Vec<String>) -> std::result::Result<String, CmuxCommandError> {
    #[cfg(test)]
    if let Some(result) = take_test_cmux_command_result() {
        return result;
    }
    let executable = app.cmux_executable_path().ok_or_else(|| {
        CmuxCommandError::Unavailable(
            "the proven cmux ancestor executable is no longer current".to_owned(),
        )
    })?;
    let mut child = Command::new(executable)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                CmuxCommandError::Unavailable("the cmux executable was not found".to_owned())
            }
            std::io::ErrorKind::PermissionDenied => {
                CmuxCommandError::AccessDenied("the cmux executable is not permitted".to_owned())
            }
            _ => CmuxCommandError::Unavailable(error.to_string()),
        })?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout = thread::spawn(move || drain_limited(stdout));
    let stderr = thread::spawn(move || drain_limited(stderr));
    let deadline = Instant::now() + CMUX_COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_limited_reader(stdout, "stdout");
                let _ = join_limited_reader(stderr, "stderr");
                return Err(CmuxCommandError::TimedOut);
            }
            Err(error) => return Err(CmuxCommandError::Failed(error.to_string())),
        }
    };
    let stdout = join_limited_reader(stdout, "stdout")?;
    let stderr = join_limited_reader(stderr, "stderr")?;
    reject_limited_reader("stdout", &stdout)?;
    reject_limited_reader("stderr", &stderr)?;
    if status.success() {
        return decode_cmux_stdout(stdout);
    }
    let detail = strict_cmux_detail(stderr.bytes);
    let normalized = detail.to_ascii_lowercase();
    if normalized.contains("permission denied")
        || normalized.contains("access denied")
        || normalized.contains("operation not permitted")
    {
        Err(CmuxCommandError::AccessDenied(detail))
    } else if normalized.contains("socket")
        || normalized.contains("connection refused")
        || normalized.contains("not running")
    {
        Err(CmuxCommandError::Unavailable(detail))
    } else {
        Err(CmuxCommandError::Failed(if detail.is_empty() {
            "cmux exited without a structured response".to_owned()
        } else {
            detail
        }))
    }
}

#[derive(Debug)]
struct LimitedRead {
    bytes: Vec<u8>,
    overflowed: bool,
    read_error: Option<String>,
}

fn join_limited_reader(
    reader: thread::JoinHandle<LimitedRead>,
    stream: &str,
) -> std::result::Result<LimitedRead, CmuxCommandError> {
    reader
        .join()
        .map_err(|_| CmuxCommandError::Failed(format!("cmux {stream} reader did not complete")))
}

fn reject_limited_reader(
    stream: &str,
    read: &LimitedRead,
) -> std::result::Result<(), CmuxCommandError> {
    if read.overflowed {
        return Err(CmuxCommandError::Failed(format!(
            "cmux {stream} exceeded the {}-byte response bound",
            CMUX_COMMAND_OUTPUT_LIMIT
        )));
    }
    if let Some(error) = &read.read_error {
        return Err(CmuxCommandError::Failed(format!(
            "cmux {stream} could not be read completely: {}",
            bounded_detail(error)
        )));
    }
    Ok(())
}

fn decode_cmux_stdout(read: LimitedRead) -> std::result::Result<String, CmuxCommandError> {
    reject_limited_reader("stdout", &read)?;
    String::from_utf8(read.bytes)
        .map(|output| output.trim().to_owned())
        .map_err(|_| {
            CmuxCommandError::Failed(
                "cmux stdout was not valid UTF-8 structured JSON output".to_owned(),
            )
        })
}

fn strict_cmux_detail(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(detail) => detail.trim().chars().take(1024).collect(),
        Err(_) => "cmux stderr was not valid UTF-8".to_owned(),
    }
}

#[cfg(test)]
fn take_test_cmux_command_result() -> Option<std::result::Result<String, CmuxCommandError>> {
    TEST_CMUX_COMMAND_RESULTS.with(|results| results.borrow_mut().pop_front())
}

#[cfg(test)]
fn push_test_cmux_command_result(result: std::result::Result<String, CmuxCommandError>) {
    TEST_CMUX_COMMAND_RESULTS.with(|results| results.borrow_mut().push_back(result));
}

#[cfg(test)]
fn test_cmux_command_result_count() -> usize {
    TEST_CMUX_COMMAND_RESULTS.with(|results| results.borrow().len())
}

#[cfg(test)]
fn clear_test_cmux_command_results() {
    TEST_CMUX_COMMAND_RESULTS.with(|results| results.borrow_mut().clear());
}

fn drain_limited(mut reader: impl Read) -> LimitedRead {
    let mut output = Vec::with_capacity(CMUX_COMMAND_OUTPUT_LIMIT);
    let mut buffer = [0_u8; 4096];
    let mut overflowed = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                return LimitedRead {
                    bytes: output,
                    overflowed,
                    read_error: None,
                }
            }
            Ok(count) => {
                let remaining = CMUX_COMMAND_OUTPUT_LIMIT.saturating_sub(output.len());
                let retained = remaining.min(count);
                output.extend_from_slice(&buffer[..retained]);
                overflowed |= retained != count;
            }
            Err(error) => {
                return LimitedRead {
                    bytes: output,
                    overflowed,
                    read_error: Some(error.to_string()),
                }
            }
        }
    }
}

fn unavailable_message(error: &CmuxCommandError, attachment_command: &str) -> String {
    format!(
        "{}. Start the foreground `llmrelay serve` host from a cmux terminal, then retry. Manual attachment command: {}",
        error.message(),
        attachment_command
    )
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'=')
        })
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod g14_tests {
    use super::*;
    use crate::config::InstancePaths;
    use crate::domain::{
        CmuxAttachmentControlDisposition, CmuxKeyboardControlAction, ProcessIdentity,
    };
    use crate::operations::Application;
    use crate::store::Store;
    use rusqlite::params;

    fn seed_attachment(app: &Application, suffix: &str) -> AttachmentBinding {
        let binding = AttachmentBinding {
            session_id: uuid::Uuid::new_v4().to_string(),
            role_generation_id: uuid::Uuid::new_v4().to_string(),
            transcript_epoch: uuid::Uuid::new_v4().to_string(),
            process: ProcessIdentity {
                pid: 4242,
                process_group_id: 4242,
                native_start_marker: format!("cmux-test-start-{suffix}"),
                observed_started_at: "2026-01-01T00:00:00Z".into(),
            },
        };
        let connection = app.store.lock().unwrap();
        let generation = match suffix {
            "first" => 1,
            "second" => 2,
            "failed-after-unknown" => 3,
            "live-before-unknown" => 4,
            _ => 5,
        };
        connection.execute(
            "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
             VALUES(?1,'attempt','manager','codex',?2,1,'running',?3,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            params![binding.role_generation_id, generation, format!("authority-{suffix}")],
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
        binding
    }

    fn cmux_test_application() -> (std::path::PathBuf, Application) {
        let root =
            std::env::temp_dir().join(format!("agenticjira-cmux-g14-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        let connection = store.lock().unwrap();
        connection.execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('project','project','/tmp/project','cmux-g14-project','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('task','project','task','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('attempt','task','context','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        ).unwrap();
        drop(connection);
        (
            root,
            Application::new(paths, store, std::env::current_exe().unwrap()).unwrap(),
        )
    }

    fn open_persistent_surface(
        app: &Application,
        service_boot_id: &str,
        binding: &AttachmentBinding,
    ) -> (CmuxTaskWorkspace, CmuxSessionSurface) {
        let (workspace, workspace_created) = app
            .store
            .reserve_cmux_task_workspace(service_boot_id, "task")
            .unwrap();
        assert!(workspace_created);
        let (reserved, surface_created) = app
            .store
            .reserve_cmux_session_surface(service_boot_id, &workspace.id, binding)
            .unwrap();
        assert!(surface_created);
        assert!(app
            .store
            .claim_cmux_task_workspace_create(&workspace.id, &reserved.id, service_boot_id)
            .unwrap());
        let physical_workspace_id = uuid::Uuid::new_v4().to_string();
        let physical_surface_id = uuid::Uuid::new_v4().to_string();
        let opened = app
            .store
            .mark_cmux_workspace_and_initial_surface_open(
                &workspace.id,
                &reserved.id,
                service_boot_id,
                &physical_workspace_id,
                &physical_surface_id,
            )
            .unwrap();
        let connected = app
            .store
            .mark_cmux_session_surface_connected(
                &opened.id,
                service_boot_id,
                binding,
                opened.binding_revision,
            )
            .unwrap();
        (
            app.store.cmux_task_workspace(&workspace.id).unwrap(),
            connected,
        )
    }

    fn make_live_controller(
        app: &Application,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        surface: &CmuxSessionSurface,
    ) -> (CmuxSessionSurface, String) {
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
        let lease_secret = format!("cmux-loss-fixture-{}", uuid::Uuid::new_v4());
        app.store
            .acquire_input_lease(
                &binding.session_id,
                &lease_secret,
                "cmux-loss-fixture-owner",
                &serde_json::to_string(&binding.process).unwrap(),
                &binding.role_generation_id,
                &(chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339(),
            )
            .unwrap();
        (
            app.store.cmux_session_surface(&surface.id).unwrap(),
            lease_secret,
        )
    }

    fn retire_live_controller(
        app: &Application,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        surface: &CmuxSessionSurface,
        lease_secret: &str,
    ) {
        app.store
            .release_input_lease(&binding.session_id, lease_secret)
            .unwrap();
        app.store
            .mark_cmux_session_surface_ended(
                &surface.id,
                service_boot_id,
                binding,
                surface.binding_revision,
            )
            .unwrap();
    }

    #[test]
    fn validated_loss_waits_for_the_exact_live_controller_before_any_replacement() {
        let (workspace_root, workspace_app) = cmux_test_application();
        let workspace_binding = seed_attachment(&workspace_app, "workspace-loss");
        let workspace_boot = uuid::Uuid::new_v4().to_string();
        let (workspace, surface) =
            open_persistent_surface(&workspace_app, &workspace_boot, &workspace_binding);
        let (surface, _lease_secret) = make_live_controller(
            &workspace_app,
            &workspace_boot,
            &workspace_binding,
            &surface,
        );

        workspace_app
            .store
            .mark_cmux_task_workspace_lost(
                &workspace.id,
                &workspace_boot,
                workspace.workspace_id.as_deref().unwrap(),
            )
            .unwrap();
        let lost = workspace_app
            .store
            .cmux_session_surface(&surface.id)
            .unwrap();
        assert_eq!(lost.surface_state, "lost");
        assert_eq!(lost.attachment_state, "live");
        assert!(matches!(
            workspace_app
                .store
                .cmux_attachment_control_directive(
                    &lost.id,
                    &workspace_boot,
                    &workspace_binding,
                    lost.binding_revision,
                )
                .unwrap()
                .disposition,
            crate::domain::CmuxAttachmentControlDisposition::Retire
        ));
        assert!(workspace_app
            .store
            .reserve_cmux_task_workspace(&workspace_boot, "task")
            .unwrap_err()
            .to_string()
            .contains("loss retirement is still pending"));
        assert!(workspace_app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &workspace_boot,
                &workspace_binding.session_id,
                &lost.id,
                lost.binding_revision,
                lost.control_revision,
                CmuxKeyboardControlAction::Acquire,
            )
            .is_err());
        assert_eq!(
            workspace_app
                .store
                .cmux_session_surface(&lost.id)
                .unwrap()
                .control_revision,
            lost.control_revision
        );

        // The other permissible retirement proof is a positively observed
        // expiry for this exact session/generation/process lease. The
        // reservation path must not treat a merely absent lease as proof.
        workspace_app
            .store
            .lock()
            .unwrap()
            .execute(
                "UPDATE input_leases SET expires_at='2000-01-01T00:00:00Z'
                  WHERE session_id=?1",
                rusqlite::params![&workspace_binding.session_id],
            )
            .unwrap();
        let (replacement_workspace, created) = workspace_app
            .store
            .reserve_cmux_task_workspace(&workspace_boot, "task")
            .unwrap();
        assert!(created);
        assert_ne!(replacement_workspace.id, workspace.id);
        assert_eq!(
            workspace_app
                .store
                .cmux_session_surface(&lost.id)
                .unwrap()
                .attachment_state,
            "ended",
            "only the exact observed expired lease released this loss-retirement fence"
        );

        drop(workspace_app);
        let _ = std::fs::remove_dir_all(workspace_root);

        let (surface_root, surface_app) = cmux_test_application();
        let surface_binding = seed_attachment(&surface_app, "surface-loss");
        let surface_boot = uuid::Uuid::new_v4().to_string();
        let (workspace, surface) =
            open_persistent_surface(&surface_app, &surface_boot, &surface_binding);
        let (surface, lease_secret) =
            make_live_controller(&surface_app, &surface_boot, &surface_binding, &surface);

        surface_app
            .store
            .mark_cmux_session_surface_lost(
                &surface.id,
                &surface_boot,
                workspace.workspace_id.as_deref().unwrap(),
                surface.surface_id.as_deref().unwrap(),
            )
            .unwrap();
        let lost = surface_app.store.cmux_session_surface(&surface.id).unwrap();
        assert_eq!(lost.surface_state, "lost");
        assert_eq!(lost.attachment_state, "live");
        assert!(surface_app
            .store
            .reserve_cmux_session_surface(&surface_boot, &workspace.id, &surface_binding)
            .unwrap_err()
            .to_string()
            .contains("loss retirement is still pending"));
        assert!(surface_app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &surface_boot,
                &surface_binding.session_id,
                &lost.id,
                lost.binding_revision,
                lost.control_revision,
                CmuxKeyboardControlAction::Acquire,
            )
            .is_err());
        assert_eq!(
            surface_app
                .store
                .cmux_session_surface(&lost.id)
                .unwrap()
                .control_revision,
            lost.control_revision
        );

        retire_live_controller(
            &surface_app,
            &surface_boot,
            &surface_binding,
            &lost,
            &lease_secret,
        );
        let (replacement_surface, created) = surface_app
            .store
            .reserve_cmux_session_surface(&surface_boot, &workspace.id, &surface_binding)
            .unwrap();
        assert!(created);
        assert_ne!(replacement_surface.id, surface.id);

        drop(surface_app);
        let _ = std::fs::remove_dir_all(surface_root);
    }

    #[test]
    fn persistent_surface_reservation_control_and_unknown_lifecycle_are_revision_bound() {
        let (root, app) = cmux_test_application();
        let first = seed_attachment(&app, "first");
        let second = seed_attachment(&app, "second");
        let service_boot_id = uuid::Uuid::new_v4().to_string();
        let (workspace, first_surface) = open_persistent_surface(&app, &service_boot_id, &first);

        // Repeated View/Take preparation for one binding converges on the same
        // task workspace and mode-neutral physical-surface reservation.
        let (same_workspace, workspace_created) = app
            .store
            .reserve_cmux_task_workspace(&service_boot_id, "task")
            .unwrap();
        assert!(!workspace_created);
        assert_eq!(same_workspace.id, workspace.id);
        let (same_surface, surface_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &first)
            .unwrap();
        assert!(!surface_created);
        assert_eq!(same_surface.id, first_surface.id);

        let view_operation = uuid::Uuid::new_v4().to_string();
        assert!(app
            .store
            .reserve_cmux_view_operation(&view_operation, &service_boot_id, &first.session_id)
            .unwrap()
            .is_none());
        assert_eq!(
            app.store
                .reserve_cmux_view_operation(&view_operation, &service_boot_id, &first.session_id)
                .unwrap()
                .unwrap()
                .state,
            "pending"
        );
        assert!(app
            .store
            .reserve_cmux_view_operation(&view_operation, &service_boot_id, &second.session_id)
            .unwrap_err()
            .to_string()
            .contains("reused with different input"));

        // A second current session in the task receives a distinct surface in
        // the one owned workspace, never a second task workspace.
        let (second_surface, second_surface_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &second)
            .unwrap();
        assert!(second_surface_created);
        assert_eq!(second_surface.task_workspace_id, workspace.id);
        assert!(app
            .store
            .claim_cmux_targeted_surface_create(&workspace.id, &second_surface.id, &service_boot_id)
            .unwrap());
        let second_open = app
            .store
            .mark_cmux_targeted_surface_open(
                &second_surface.id,
                &service_boot_id,
                workspace.workspace_id.as_deref().unwrap(),
                &uuid::Uuid::new_v4().to_string(),
            )
            .unwrap();
        let second_connected = app
            .store
            .mark_cmux_session_surface_connected(
                &second_open.id,
                &service_boot_id,
                &second,
                second_open.binding_revision,
            )
            .unwrap();

        // A detached client makes its physical pane historical. Its next
        // route is a new view-only binding revision in the same workspace,
        // not a focus, respawn, injection, or close of the old surface.
        app.store
            .mark_cmux_session_surface_ended(
                &second_connected.id,
                &service_boot_id,
                &second,
                second_connected.binding_revision,
            )
            .unwrap();
        let (ended_replacement, ended_replacement_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &second)
            .unwrap();
        assert!(ended_replacement_created);
        assert_ne!(ended_replacement.id, second_connected.id);
        assert_eq!(ended_replacement.task_workspace_id, workspace.id);
        assert_eq!(
            ended_replacement.binding_revision,
            second_connected.binding_revision + 1
        );
        assert_eq!(ended_replacement.workspace_id, workspace.workspace_id);
        assert_eq!(ended_replacement.surface_id, None);
        assert_eq!(ended_replacement.surface_state, "opening");
        assert_eq!(ended_replacement.desired_input_state, "view_only");

        let failed = seed_attachment(&app, "failed-after-unknown");
        let (failed_surface, failed_surface_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &failed)
            .unwrap();
        assert!(failed_surface_created);
        app.store
            .mark_cmux_session_surface_failed(
                &failed_surface.id,
                &service_boot_id,
                &failed,
                failed_surface.binding_revision,
            )
            .unwrap();
        let failed_historical = app.store.cmux_session_surface(&failed_surface.id).unwrap();
        assert_eq!(failed_historical.surface_state, "retired");
        assert_eq!(failed_historical.attachment_state, "failed");
        let (failed_replacement, failed_replacement_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &failed)
            .unwrap();
        assert!(failed_replacement_created);
        assert_ne!(failed_replacement.id, failed_surface.id);
        assert_eq!(
            failed_replacement.binding_revision,
            failed_surface.binding_revision + 1
        );
        assert_eq!(failed_replacement.workspace_id, workspace.workspace_id);
        assert_eq!(failed_replacement.surface_id, None);
        assert_eq!(failed_replacement.desired_input_state, "view_only");

        // The browser can advance only desired control revisions. It obtains
        // no lease material, and replay has to preserve its original input.
        let acquire_operation = uuid::Uuid::new_v4().to_string();
        let acquire = app
            .store
            .set_cmux_keyboard_control(
                &acquire_operation,
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                0,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap();
        assert_eq!(acquire.state, "pending");
        assert_eq!(acquire.surface.desired_input_state, "control");
        assert_eq!(acquire.surface.control_revision, 1);
        let replayed = app
            .store
            .set_cmux_keyboard_control(
                &acquire_operation,
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                0,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap();
        assert_eq!(replayed.surface.control_revision, 1);
        assert!(app
            .store
            .set_cmux_keyboard_control(
                &acquire_operation,
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                0,
                CmuxKeyboardControlAction::Release,
            )
            .unwrap_err()
            .to_string()
            .contains("reused with different input"));
        assert!(app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &second.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                1,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap_err()
            .to_string()
            .contains("exact current surface"));
        assert!(app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                1,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap_err()
            .to_string()
            .contains("exact current surface"));

        // A blocked lease, release, stale acknowledgement, renewal loss, and
        // a new acquire all remain one connection's monotonic state machine.
        assert!(app
            .store
            .acknowledge_cmux_attachment_control(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
                1,
                "blocked",
                Some("another exact human lease is active"),
            )
            .unwrap());
        let release = app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                1,
                CmuxKeyboardControlAction::Release,
            )
            .unwrap();
        assert_eq!(release.surface.control_revision, 2);
        assert!(!app
            .store
            .acknowledge_cmux_attachment_control(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
                1,
                "blocked",
                None,
            )
            .unwrap());
        assert!(app
            .store
            .acknowledge_cmux_attachment_control(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
                2,
                "view_only",
                None,
            )
            .unwrap());
        let reacquire = app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                2,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap();
        assert_eq!(reacquire.surface.control_revision, 3);
        assert!(app
            .store
            .acknowledge_cmux_attachment_control(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
                3,
                "lost",
                Some("the exact human input lease could not be renewed"),
            )
            .unwrap());
        let recovered_control = app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                3,
                CmuxKeyboardControlAction::Acquire,
            )
            .unwrap();
        assert!(app
            .store
            .acknowledge_cmux_attachment_control(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
                recovered_control.surface.control_revision,
                "control",
                None,
            )
            .unwrap());

        // An inconclusive observation does not turn View into a hidden lease
        // release. The live child blocks discard and all new control changes.
        let unknown = app
            .store
            .mark_cmux_session_surface_observation_unknown(
                &first_surface.id,
                &service_boot_id,
                "fixture tree response was malformed",
            )
            .unwrap();
        assert_eq!(unknown.surface_state, "unknown");
        assert_eq!(unknown.attachment_state, "live");
        assert_eq!(unknown.desired_input_state, "control");
        assert_eq!(unknown.actual_input_state, "control");
        assert_eq!(unknown.control_revision, 4);
        assert_eq!(unknown.applied_revision, 4);
        let hold = app
            .store
            .cmux_attachment_control_directive(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
            )
            .unwrap();
        assert_eq!(hold.disposition, CmuxAttachmentControlDisposition::Hold);
        assert_eq!(hold.desired_input_state, "control");
        assert!(app
            .store
            .set_cmux_keyboard_control(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &first.session_id,
                &first_surface.id,
                first_surface.binding_revision,
                4,
                CmuxKeyboardControlAction::Release,
            )
            .unwrap_err()
            .to_string()
            .contains("exact current surface"));
        assert!(app
            .store
            .discard_unknown_cmux_session_surface(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &first_surface.id,
                &first,
            )
            .unwrap_err()
            .to_string()
            .contains("child attachment is live"));

        // Ending the child makes the old pane historical. The explicit
        // discard and later reservation create a fresh view-only revision in
        // the same task workspace without targeting the old physical surface.
        app.store
            .mark_cmux_session_surface_ended(
                &first_surface.id,
                &service_boot_id,
                &first,
                first_surface.binding_revision,
            )
            .unwrap();
        let (still_unknown, replacement_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &first)
            .unwrap();
        assert!(!replacement_created);
        assert_eq!(still_unknown.id, first_surface.id);
        assert_eq!(still_unknown.surface_state, "unknown");
        assert_eq!(still_unknown.attachment_state, "ended");
        let discard_operation = uuid::Uuid::new_v4().to_string();
        let discarded = app
            .store
            .discard_unknown_cmux_session_surface(
                &discard_operation,
                &service_boot_id,
                &first_surface.id,
                &first,
            )
            .unwrap();
        assert_eq!(discarded.surface_state, "retired");
        assert_eq!(discarded.attachment_state, "failed");
        assert_eq!(
            app.store
                .discard_unknown_cmux_session_surface(
                    &discard_operation,
                    &service_boot_id,
                    &first_surface.id,
                    &first,
                )
                .unwrap()
                .id,
            first_surface.id
        );
        assert!(app
            .store
            .discard_unknown_cmux_session_surface(
                &discard_operation,
                &service_boot_id,
                &second_connected.id,
                &second,
            )
            .unwrap_err()
            .to_string()
            .contains("reused with different input"));
        let (replacement, replacement_created) = app
            .store
            .reserve_cmux_session_surface(&service_boot_id, &workspace.id, &first)
            .unwrap();
        assert!(replacement_created);
        assert_eq!(replacement.task_workspace_id, workspace.id);
        assert_eq!(
            replacement.binding_revision,
            first_surface.binding_revision + 1
        );
        assert_eq!(replacement.workspace_id, workspace.workspace_id);
        assert_eq!(replacement.surface_id, None);
        assert_eq!(replacement.surface_state, "opening");
        assert_eq!(replacement.desired_input_state, "view_only");
        assert_eq!(replacement.actual_input_state, "view_only");
        assert_eq!(replacement.control_revision, 0);

        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prior_boot_presentation_is_historical_and_does_not_adopt_legacy_routes() {
        let (root, app) = cmux_test_application();
        let binding = seed_attachment(&app, "first");
        let prior_boot_id = uuid::Uuid::new_v4().to_string();
        let (workspace, surface) = open_persistent_surface(&app, &prior_boot_id, &binding);
        let (legacy_route, legacy_created) = app
            .store
            .reserve_cmux_attachment_route(&prior_boot_id, &binding, CmuxAttachmentMode::Watch)
            .unwrap();
        assert!(legacy_created);

        let current_boot_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            app.store
                .retire_prior_cmux_presentation_boots(&current_boot_id)
                .unwrap(),
            2
        );
        let retired_workspace = app.store.cmux_task_workspace(&workspace.id).unwrap();
        let retired_surface = app.store.cmux_session_surface(&surface.id).unwrap();
        assert_eq!(retired_workspace.state, "retired");
        assert_eq!(retired_surface.surface_state, "retired");
        assert_eq!(retired_surface.attachment_state, "ended");
        assert_eq!(retired_surface.desired_input_state, "view_only");
        assert_eq!(retired_surface.actual_input_state, "view_only");
        assert_eq!(
            app.store
                .cmux_attachment_route(&legacy_route.id)
                .unwrap()
                .id,
            legacy_route.id,
            "migration-024 rows remain untouched audit history"
        );
        assert!(app.store.cmux_session_surface(&legacy_route.id).is_err());

        let (new_workspace, new_workspace_created) = app
            .store
            .reserve_cmux_task_workspace(&current_boot_id, "task")
            .unwrap();
        assert!(new_workspace_created);
        assert_ne!(new_workspace.id, workspace.id);
        assert_eq!(new_workspace.state, "opening");

        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn g14_cmux_ancestry_fences_reservation_and_unknown_create_keeps_manual_route_unclaimed() {
        let (root, mut app) = cmux_test_application();
        let first = seed_attachment(&app, "first");
        let second = seed_attachment(&app, "second");
        let service_boot_id = uuid::Uuid::new_v4().to_string();
        let workspace_id = uuid::Uuid::new_v4().to_string();
        let surface_id = uuid::Uuid::new_v4().to_string();
        clear_test_cmux_rpc_results();
        clear_test_cmux_command_results();
        push_test_cmux_rpc_result(Ok(serde_json::json!({
            "workspace_id":workspace_id,
            "surface_id":surface_id,
        })));
        push_test_cmux_command_result(Ok(String::new()));

        app.set_automatic_cmux_routing_for_tests(false).unwrap();
        let rejected = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &first,
            CmuxAttachmentMode::Control,
        )
        .unwrap();
        assert_eq!(rejected.state, "failed");
        assert!(rejected.message.contains("No route was reserved"));
        assert!(!rejected
            .attachment_command
            .unwrap()
            .contains("--cmux-route-id"));
        assert_eq!(
            app.store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM cmux_attachment_routes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            test_cmux_rpc_result_count(),
            1,
            "unproven host must reject before the reachable cmux RPC or route reservation"
        );

        app.set_automatic_cmux_routing_for_tests(true).unwrap();
        let opened = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &first,
            CmuxAttachmentMode::Control,
        )
        .unwrap();
        let old_route = opened.route.unwrap();
        assert_eq!(opened.state, "opened");
        assert_eq!(old_route.surface_state, "open");
        assert_eq!(old_route.resume_state, "unavailable");
        assert_eq!(old_route.last_error, None);
        assert_eq!(
            test_cmux_command_result_count(),
            1,
            "an ephemeral attachment must not register a cmux resume command"
        );
        assert!(opened
            .attachment_command
            .unwrap()
            .contains("--cmux-route-id"));

        push_test_cmux_rpc_result(Err(CmuxCommandError::TimedOut));
        let unknown = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &second,
            CmuxAttachmentMode::Watch,
        )
        .unwrap();
        let unknown_route = unknown.route.clone().unwrap();
        assert_eq!(unknown_route.surface_state, "unknown");
        assert!(!unknown.retry_available);
        assert!(!unknown
            .attachment_command
            .unwrap()
            .contains("--cmux-route-id"));
        assert_eq!(
            app.store
                .cmux_attachment_route(&old_route.id)
                .unwrap()
                .surface_state,
            "open"
        );

        // If the unknown route were retried, this sentinel would be consumed
        // rather than allowing the test to fall through to a real cmux call.
        push_test_cmux_rpc_result(Ok(serde_json::json!({
            "workspace_id":uuid::Uuid::new_v4(),
            "surface_id":uuid::Uuid::new_v4(),
        })));
        let retried = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &second,
            CmuxAttachmentMode::Watch,
        )
        .unwrap();
        assert_eq!(retried.route.unwrap().id, unknown_route.id);
        assert_eq!(
            test_cmux_rpc_result_count(),
            1,
            "unknown create must not dispatch a second create or focus request"
        );
        assert!(!retried
            .attachment_command
            .unwrap()
            .contains("--cmux-route-id"));
        assert_eq!(
            app.store
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM cmux_attachment_routes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );

        let discard_operation = uuid::Uuid::new_v4().to_string();
        let discarded = app
            .store
            .discard_unknown_cmux_attachment_route(
                &discard_operation,
                &service_boot_id,
                &unknown_route.id,
                &second,
            )
            .unwrap();
        assert_eq!(discarded.surface_state, "failed");
        assert_eq!(discarded.attachment_state, "failed");
        assert_eq!(
            app.store
                .discard_unknown_cmux_attachment_route(
                    &discard_operation,
                    &service_boot_id,
                    &unknown_route.id,
                    &second,
                )
                .unwrap()
                .id,
            unknown_route.id,
        );
        app.store
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET status='exited' WHERE id=?1",
                params![second.session_id],
            )
            .unwrap();
        let replayed = discard_unknown_route(
            &app,
            &service_boot_id,
            &discard_operation,
            &unknown_route.id,
            &second.session_id,
        )
        .unwrap();
        assert_eq!(replayed.route.unwrap().id, unknown_route.id);
        let changed_input = discard_unknown_route(
            &app,
            &service_boot_id,
            &discard_operation,
            &old_route.id,
            &first.session_id,
        )
        .unwrap_err()
        .to_string();
        assert!(changed_input.contains("reused with different input"));
        app.store
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET status='running' WHERE id=?1",
                params![second.session_id],
            )
            .unwrap();
        assert_eq!(test_cmux_rpc_result_count(), 1);

        push_test_cmux_command_result(Ok(String::new()));
        let explicitly_reopened = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &second,
            CmuxAttachmentMode::Watch,
        )
        .unwrap();
        assert_ne!(explicitly_reopened.route.unwrap().id, unknown_route.id);
        assert_eq!(test_cmux_rpc_result_count(), 0);

        clear_test_cmux_rpc_results();
        clear_test_cmux_command_results();
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn g14_unknown_route_lifecycle_distinguishes_failed_ended_and_live_clients() {
        let (root, mut app) = cmux_test_application();
        app.set_automatic_cmux_routing_for_tests(true).unwrap();
        let service_boot_id = uuid::Uuid::new_v4().to_string();
        clear_test_cmux_rpc_results();
        clear_test_cmux_command_results();

        // The create result becomes unknown before the launched client reports
        // that it could not establish its exact attachment.
        let failed_binding = seed_attachment(&app, "failed-after-unknown");
        let (failed_route, created) = app
            .store
            .reserve_cmux_attachment_route(
                &service_boot_id,
                &failed_binding,
                CmuxAttachmentMode::Watch,
            )
            .unwrap();
        assert!(created);
        app.store
            .fail_cmux_attachment_route(
                &failed_route.id,
                &service_boot_id,
                "unknown",
                "bounded create timed out",
            )
            .unwrap();
        let connect_error = app
            .store
            .mark_cmux_attachment_connected(
                &failed_route.id,
                &service_boot_id,
                &failed_binding,
                CmuxAttachmentMode::Watch,
            )
            .unwrap_err()
            .to_string();
        assert!(connect_error.contains("not awaiting this exact client binding"));
        let still_unknown = app.store.cmux_attachment_route(&failed_route.id).unwrap();
        assert_eq!(still_unknown.surface_state, "unknown");
        assert_eq!(still_unknown.attachment_state, "pending");
        let (same_failed, duplicate_created) = app
            .store
            .reserve_cmux_attachment_route(
                &service_boot_id,
                &failed_binding,
                CmuxAttachmentMode::Watch,
            )
            .unwrap();
        assert!(!duplicate_created);
        assert_eq!(same_failed.id, failed_route.id);
        app.store
            .mark_cmux_attachment_failed(&failed_route.id, &service_boot_id, &failed_binding)
            .unwrap();
        push_test_cmux_rpc_result(Err(CmuxCommandError::TimedOut));
        let failed_open = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &failed_binding,
            CmuxAttachmentMode::Watch,
        )
        .unwrap();
        assert_eq!(failed_open.state, "failed");
        assert!(failed_open.message.contains("reported failure"));
        assert_eq!(
            failed_open.route.as_ref().unwrap().attachment_state,
            "failed"
        );
        assert_eq!(test_cmux_rpc_result_count(), 1);
        app.store
            .discard_unknown_cmux_attachment_route(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &failed_route.id,
                &failed_binding,
            )
            .unwrap();
        let (failed_replacement, replacement_created) = app
            .store
            .reserve_cmux_attachment_route(
                &service_boot_id,
                &failed_binding,
                CmuxAttachmentMode::Watch,
            )
            .unwrap();
        assert!(replacement_created);
        assert_ne!(failed_replacement.id, failed_route.id);

        // The launched client can connect while surface.create is still in
        // flight. A later timeout makes only the surface identity unknown;
        // the exact attachment remains live and must not be discarded.
        let live_binding = seed_attachment(&app, "live-before-unknown");
        let (live_route, created) = app
            .store
            .reserve_cmux_attachment_route(
                &service_boot_id,
                &live_binding,
                CmuxAttachmentMode::Control,
            )
            .unwrap();
        assert!(created);
        app.store
            .mark_cmux_attachment_connected(
                &live_route.id,
                &service_boot_id,
                &live_binding,
                CmuxAttachmentMode::Control,
            )
            .unwrap();
        app.store
            .fail_cmux_attachment_route(
                &live_route.id,
                &service_boot_id,
                "unknown",
                "create response was malformed",
            )
            .unwrap();
        let live_open = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &live_binding,
            CmuxAttachmentMode::Control,
        )
        .unwrap();
        assert_eq!(live_open.state, "open");
        assert!(!live_open.retry_available);
        assert!(live_open.message.contains("attachment client is live"));
        assert_eq!(live_open.route.as_ref().unwrap().attachment_state, "live");
        assert!(app
            .store
            .discard_unknown_cmux_attachment_route(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &live_route.id,
                &live_binding,
            )
            .unwrap_err()
            .to_string()
            .contains("non-live unknown"));
        let (same_live, created) = app
            .store
            .reserve_cmux_attachment_route(
                &service_boot_id,
                &live_binding,
                CmuxAttachmentMode::Control,
            )
            .unwrap();
        assert!(!created);
        assert_eq!(same_live.id, live_route.id);
        assert_eq!(test_cmux_rpc_result_count(), 1);

        app.store
            .mark_cmux_attachment_ended(&live_route.id, &service_boot_id, &live_binding)
            .unwrap();
        let ended_open = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &live_binding,
            CmuxAttachmentMode::Control,
        )
        .unwrap();
        assert_eq!(ended_open.state, "failed");
        assert!(ended_open.message.contains("connected and has ended"));
        assert_eq!(ended_open.route.as_ref().unwrap().attachment_state, "ended");
        app.store
            .discard_unknown_cmux_attachment_route(
                &uuid::Uuid::new_v4().to_string(),
                &service_boot_id,
                &live_route.id,
                &live_binding,
            )
            .unwrap();
        let (ended_replacement, replacement_created) = app
            .store
            .reserve_cmux_attachment_route(
                &service_boot_id,
                &live_binding,
                CmuxAttachmentMode::Control,
            )
            .unwrap();
        assert!(replacement_created);
        assert_ne!(ended_replacement.id, live_route.id);
        assert_eq!(test_cmux_rpc_result_count(), 1);

        clear_test_cmux_rpc_results();
        clear_test_cmux_command_results();
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn g14_create_error_returns_the_inflight_live_attachment_without_fallback() {
        let (root, mut app) = cmux_test_application();
        app.set_automatic_cmux_routing_for_tests(true).unwrap();
        let binding = seed_attachment(&app, "inflight-live-create-error");
        let service_boot_id = uuid::Uuid::new_v4().to_string();
        clear_test_cmux_rpc_results();
        clear_test_cmux_command_results();

        let hook_store = app.store.clone();
        let hook_binding = binding.clone();
        let hook_boot = service_boot_id.clone();
        push_test_cmux_rpc_hook(move || {
            let route_id: String = hook_store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT id FROM cmux_attachment_routes
                      WHERE service_boot_id=?1 AND session_id=?2
                        AND attachment_mode='watch' AND surface_state='opening'
                      ORDER BY created_at DESC,rowid DESC LIMIT 1",
                    params![hook_boot, hook_binding.session_id],
                    |row| row.get(0),
                )
                .unwrap();
            hook_store
                .mark_cmux_attachment_connected(
                    &route_id,
                    &hook_boot,
                    &hook_binding,
                    CmuxAttachmentMode::Watch,
                )
                .unwrap();
        });
        push_test_cmux_rpc_result(Err(CmuxCommandError::AccessDenied(
            "fixture denied".to_owned(),
        )));
        push_test_cmux_rpc_result(Ok(serde_json::json!({
            "workspace_id":uuid::Uuid::new_v4(),
            "surface_id":uuid::Uuid::new_v4(),
        })));

        let outcome = open_attachment_with_binding(
            &app,
            &service_boot_id,
            &binding,
            CmuxAttachmentMode::Watch,
        )
        .unwrap();
        assert_eq!(outcome.state, "open");
        assert!(!outcome.retry_available);
        assert!(outcome.attachment_command.is_none());
        assert!(outcome.message.contains("attachment client is live"));
        let route = outcome.route.unwrap();
        assert_eq!(route.surface_state, "unknown");
        assert_eq!(route.attachment_state, "live");
        assert!(route.workspace_id.is_none());
        assert!(route.surface_id.is_none());
        let (same_route, duplicate_created) = app
            .store
            .reserve_cmux_attachment_route(&service_boot_id, &binding, CmuxAttachmentMode::Watch)
            .unwrap();
        assert!(!duplicate_created);
        assert_eq!(same_route.id, route.id);
        assert_eq!(
            test_cmux_rpc_result_count(),
            1,
            "the immediate unknown+live return consumed an RPC after the failing create"
        );

        clear_test_cmux_rpc_results();
        clear_test_cmux_command_results();
        drop(app);
        let _ = std::fs::remove_dir_all(root);
    }
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CompletePrefixThenReadError {
        bytes: Vec<u8>,
    }

    impl std::io::Read for CompletePrefixThenReadError {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.bytes.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "fixture read failure after complete JSON prefix",
                ));
            }
            let count = buffer.len().min(self.bytes.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes.drain(..count);
            Ok(count)
        }
    }

    #[test]
    fn cmux_stdout_rejects_invalid_utf8_before_structured_json_parsing() {
        let bytes = b"{\"result\":{\"windows\":[],\"description\":\"\xff\"}}".to_vec();
        assert!(
            serde_json::from_str::<Value>(&String::from_utf8_lossy(&bytes)).is_ok(),
            "the lossy legacy boundary would have repaired this invalid byte into parseable JSON"
        );
        let read = drain_limited(std::io::Cursor::new(bytes));

        let error = decode_cmux_stdout(read).unwrap_err();
        assert!(error.message().contains("not valid UTF-8"));
    }

    #[test]
    fn cmux_stdout_rejects_a_parseable_prefix_when_more_than_the_bound_arrives() {
        let prefix = br#"{"result":{"windows":[]}}"#.to_vec();
        assert!(serde_json::from_slice::<Value>(&prefix).is_ok());
        let mut bytes = prefix;
        bytes.resize(CMUX_COMMAND_OUTPUT_LIMIT + 1, b' ');

        let read = drain_limited(std::io::Cursor::new(bytes));
        assert!(read.overflowed);
        let error = decode_cmux_stdout(read).unwrap_err();
        assert!(error.message().contains("exceeded"));
    }

    #[test]
    fn cmux_stdout_rejects_a_complete_looking_prefix_when_the_reader_fails() {
        let prefix = br#"{"result":{"windows":[]}}"#.to_vec();
        assert!(serde_json::from_slice::<Value>(&prefix).is_ok());
        let read = drain_limited(CompletePrefixThenReadError { bytes: prefix });

        let error = decode_cmux_stdout(read).unwrap_err();
        assert!(error.message().contains("could not be read completely"));
    }

    #[test]
    fn cmux_stdout_accepts_a_normal_complete_below_bound_response() {
        let bytes = b"{\"result\":{\"windows\":[]}}\n".to_vec();
        let output = match decode_cmux_stdout(drain_limited(std::io::Cursor::new(bytes))) {
            Ok(output) => output,
            Err(error) => panic!(
                "normal complete below-bound cmux response was rejected: {}",
                error.message(),
            ),
        };

        assert!(serde_json::from_str::<Value>(&output).is_ok());
    }

    #[test]
    fn health_requires_the_requested_terminal_in_the_requested_workspace() {
        let workspace = "A089CA48-45E5-440E-8D93-829B2F2A3615";
        let requested = "5BC24812-779E-4780-BD56-E7DA491635BE";
        let other = "93E9B50A-F897-45CB-B235-CD64EDE2C6ED";
        let health = serde_json::json!({
            "workspace_id": workspace,
            "window_id": "window-1",
            "surfaces": [
                {"id": requested, "in_window": false, "index": 0, "ref": "surface:90", "type": "terminal"},
                {"id": other, "in_window": false, "index": 1, "ref": "surface:91", "type": "terminal"},
            ],
        });
        assert!(verify_health_surface(&health, workspace, requested).is_ok());
        assert!(
            verify_health_surface(&health, workspace, "00000000-0000-0000-0000-000000000000")
                .is_err()
        );
        assert!(
            verify_health_surface(&health, "00000000-0000-0000-0000-000000000000", requested)
                .is_err()
        );

        let mut missing_type = health.clone();
        missing_type["surfaces"][0]
            .as_object_mut()
            .unwrap()
            .remove("type");
        assert!(verify_health_surface(&missing_type, workspace, requested).is_err());

        let mut singular_disagrees = health.clone();
        singular_disagrees["surface_id"] = json!(other);
        assert!(verify_health_surface(&singular_disagrees, workspace, requested).is_err());

        let mut duplicate = health.clone();
        let repeated = duplicate["surfaces"][0].clone();
        duplicate["surfaces"].as_array_mut().unwrap().push(repeated);
        assert!(verify_health_surface(&duplicate, workspace, requested).is_err());

        let mut non_terminal = health.clone();
        non_terminal["surfaces"][0]["type"] = json!("editor");
        assert!(verify_health_surface(&non_terminal, workspace, requested).is_err());
    }

    #[test]
    fn global_tree_loss_inventory_requires_complete_consistent_terminal_evidence() {
        // IDs are drawn from the saved disposable cmux system.tree response.
        // Keep a full multi-workspace inventory here so the parser cannot
        // silently turn a partial response into proof of terminal loss.
        let tree = json!({
            "windows": [{
                "id": "CD1D680C-950A-481B-99E2-2C0413324BCE",
                "workspace_count": 3,
                "workspaces": [
                    {
                        "id": "0D948FAA-5CC9-495F-8A9F-A2EBC05AE859",
                        "panes": [
                            {
                                "id": "6ECF6D9E-8599-4356-B802-C75CC6D9B960",
                                "surface_count": 1,
                                "surface_ids": ["15E6EA23-D677-46F7-9463-1059B0AD2AD7"],
                                "surfaces": [{
                                    "id": "15E6EA23-D677-46F7-9463-1059B0AD2AD7",
                                    "pane_id": "6ECF6D9E-8599-4356-B802-C75CC6D9B960",
                                    "type": "terminal",
                                }],
                            },
                            {
                                "id": "E701F748-AA94-4AD6-B4DC-0F6EDE3ED0A2",
                                "surface_count": 1,
                                "surface_ids": ["B38556DB-3FCB-406F-951C-2FFBF7968017"],
                                "surfaces": [{
                                    "id": "B38556DB-3FCB-406F-951C-2FFBF7968017",
                                    "pane_id": "E701F748-AA94-4AD6-B4DC-0F6EDE3ED0A2",
                                    "type": "terminal",
                                }],
                            },
                        ],
                    },
                    {
                        "id": "4EDB23C0-FFE9-4CB8-B4D5-79D4567096A0",
                        "panes": [{
                            "id": "B7246EA3-A554-41F8-9563-1BE46E872EA8",
                            "surface_count": 1,
                            "surface_ids": ["3393266E-DA71-49E4-9C66-0A21EA75E1C5"],
                            "surfaces": [{
                                "id": "3393266E-DA71-49E4-9C66-0A21EA75E1C5",
                                "pane_id": "B7246EA3-A554-41F8-9563-1BE46E872EA8",
                                "type": "terminal",
                            }],
                        }],
                    },
                    {
                        "id": "2D302E9C-4D9B-4663-B953-B6C8BFBB8A4B",
                        "panes": [{
                            "id": "1AD29487-DB2D-4B19-9F96-FD370E3CE460",
                            "surface_count": 2,
                            "surface_ids": [
                                "27E13CFC-390E-42FC-A618-155923E51767",
                                "F3938763-0A18-4662-B336-B8C2CFBEB13F",
                            ],
                            "surfaces": [
                                {
                                    "id": "27E13CFC-390E-42FC-A618-155923E51767",
                                    "pane_id": "1AD29487-DB2D-4B19-9F96-FD370E3CE460",
                                    "type": "terminal",
                                },
                                {
                                    "id": "F3938763-0A18-4662-B336-B8C2CFBEB13F",
                                    "pane_id": "1AD29487-DB2D-4B19-9F96-FD370E3CE460",
                                    "type": "browser",
                                    "tty": null,
                                    "url": "",
                                },
                            ],
                        }],
                    },
                ],
            }],
        });
        let inventory = match parse_global_tree(&tree) {
            Ok(inventory) => inventory,
            Err(error) => panic!(
                "saved disposable tree fixture was rejected: {}",
                error.message()
            ),
        };
        let disposable_workspace = inventory
            .get("2D302E9C-4D9B-4663-B953-B6C8BFBB8A4B")
            .unwrap();
        assert!(disposable_workspace.contains("27E13CFC-390E-42FC-A618-155923E51767"));
        assert!(!disposable_workspace.contains("F3938763-0A18-4662-B336-B8C2CFBEB13F"));

        let mut mismatched_count = tree.clone();
        mismatched_count["windows"][0]["workspaces"][2]["panes"][0]["surface_count"] = json!(1);
        assert!(parse_global_tree(&mismatched_count)
            .unwrap_err()
            .message()
            .contains("surface_count did not match"));

        let mut duplicate_surface = tree.clone();
        duplicate_surface["windows"][0]["workspaces"][2]["panes"][0]["surface_ids"] = json!([
            "27E13CFC-390E-42FC-A618-155923E51767",
            "27E13CFC-390E-42FC-A618-155923E51767",
        ]);
        assert!(parse_global_tree(&duplicate_surface)
            .unwrap_err()
            .message()
            .contains("duplicate surface UUID"));
    }
}
