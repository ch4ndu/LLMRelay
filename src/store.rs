use crate::auth;
use crate::domain::{
    AttachmentBinding, CapabilityIdentity, CapabilityProofInput, CmuxAttachmentControlDirective,
    CmuxAttachmentControlDisposition, CmuxAttachmentMode, CmuxAttachmentRoute,
    CmuxKeyboardControlAction, CmuxKeyboardControlOutcome, CmuxSessionSurface, CmuxTaskWorkspace,
    CmuxViewOutcome, HookEnvelope, LaunchConfig, ObservedProcessIdentity, RestartCandidateResult,
    RoleContext, RoleKind, RolePeerProvenance, RoleResultReport, ValidationLaunchRequest,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

#[cfg(test)]
thread_local! {
    static TEST_INTERRUPT_ROLE_REPORT_AFTER_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Debug)]
pub(crate) struct RoleResumeCapacityError;

impl std::fmt::Display for RoleResumeCapacityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("role capacity is full for exact resume")
    }
}

impl std::error::Error for RoleResumeCapacityError {}

#[derive(Clone, Debug)]
pub struct RoleLaunchContext {
    pub task_id: String,
    pub workspace: std::path::PathBuf,
    pub config: crate::domain::RoleOverride,
    pub settings_revision: i64,
    pub role_generation_id: String,
    pub session_id: String,
    pub transcript_epoch: String,
    pub credential_id: String,
    pub token: String,
    pub permit_id: String,
    pub lane_id: String,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BrowserLaunchReceipt<'a> {
    pub operation_id: &'a str,
    pub operation_kind: &'a str,
    pub input_hash: &'a str,
    pub entity_id: &'a str,
}

#[derive(Clone, Debug)]
pub(crate) enum BrowserLaunchReservation {
    Reserved,
    Existing(serde_json::Value),
}

pub const CAPABILITY_PROOF_REVISION: &str = "llmrelay-capability-proof-v1";

#[derive(Clone, Debug)]
pub(crate) struct PreparedResumeIdentity {
    capability_key: String,
    provider: String,
    executable_version: String,
    role: String,
    model: String,
    effort: String,
    compatibility_hash: Option<String>,
}

impl PreparedResumeIdentity {
    pub(crate) fn from_launch(launch: &LaunchConfig) -> Result<Self> {
        let identity = crate::providers::capability_identity(launch)?;
        Ok(Self {
            capability_key: crate::providers::capability_identity_key(&identity)?,
            provider: identity.provider.to_string(),
            executable_version: identity.executable_version,
            role: identity.role.to_string(),
            model: identity.model,
            effort: identity.effort,
            compatibility_hash: identity.compatibility.map(|binding| binding.effective_hash),
        })
    }

    fn audit_value(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider.as_str(),
            "executable_version": self.executable_version.as_str(),
            "role": self.role.as_str(),
            "model": self.model.as_str(),
            "effort": self.effort.as_str(),
            "mode": "interactive_pty",
            "compatibility_hash": self.compatibility_hash,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentTranscriptAccess {
    Live,
    CaptureDraining,
    CaptureComplete { sequence: u64 },
}

const MIGRATION_001: &str = include_str!("../migrations/001_initial.sql");
const MIGRATION_002: &str = include_str!("../migrations/002_application.sql");
const MIGRATION_003: &str = include_str!("../migrations/003_orchestration.sql");
const MIGRATION_004: &str = include_str!("../migrations/004_backend_completion.sql");
const MIGRATION_005: &str = include_str!("../migrations/005_integration_completion.sql");
const MIGRATION_006: &str = include_str!("../migrations/006_check_provenance.sql");
const MIGRATION_007: &str = include_str!("../migrations/007_review_repairs.sql");
const MIGRATION_008: &str = include_str!("../migrations/008_session_launch_state.sql");
const MIGRATION_009: &str = include_str!("../migrations/009_switch_generation_binding.sql");
const MIGRATION_010: &str = include_str!("../migrations/010_validation_launch_permits.sql");
const MIGRATION_011: &str = include_str!("../migrations/011_invocation_identity.sql");
const MIGRATION_012: &str = include_str!("../migrations/012_restart_restore.sql");
const MIGRATION_013: &str = include_str!("../migrations/013_permissions.sql");
const MIGRATION_014: &str = include_str!("../migrations/014_guidance_invocation_identity.sql");
const MIGRATION_015: &str = include_str!("../migrations/015_codex_native_policy_admission.sql");
const MIGRATION_016: &str = include_str!("../migrations/016_codex_capability_display.sql");
const MIGRATION_017: &str = include_str!("../migrations/017_codex_denied_read_floor.sql");
const MIGRATION_018: &str = include_str!("../migrations/018_trip_explorer.sql");
const MIGRATION_019: &str = include_str!("../migrations/019_trip_finalization.sql");
const MIGRATION_020: &str = include_str!("../migrations/020_trip_capability_binding.sql");
const MIGRATION_021: &str = include_str!("../migrations/021_task_profile_activation.sql");
const MIGRATION_022: &str = include_str!("../migrations/022_runtime_admission.sql");
const MIGRATION_023: &str = include_str!("../migrations/023_check_permissions.sql");
const MIGRATION_024: &str = include_str!("../migrations/024_cmux_attachment_routing.sql");
const MIGRATION_025: &str =
    include_str!("../migrations/025_claude_role_socket_requalification.sql");
const MIGRATION_026: &str = include_str!("../migrations/026_runtime_cmux_socket_observation.sql");
const MIGRATION_027: &str = include_str!("../migrations/027_cmux_task_workspace_routing.sql");
const MIGRATION_028: &str = include_str!("../migrations/028_session_interrupt_deadline.sql");
const MIGRATION_029: &str = include_str!("../migrations/029_state_revision.sql");
const MIGRATION_030: &str = include_str!("../migrations/030_recipes.sql");

type CmuxRouteRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
);

type CmuxTaskWorkspaceRow = (
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
);

type CmuxSessionSurfaceRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    String,
    String,
);

fn valid_cmux_uuid(label: &str, value: &str) -> Result<()> {
    uuid::Uuid::parse_str(value).map_err(|_| anyhow!("{label} must be a UUID"))?;
    Ok(())
}

fn bounded_cmux_error(error: &str) -> String {
    error.chars().take(1024).collect()
}

fn cmux_unknown_discard_request_hash(
    service_boot_id: &str,
    route_id: &str,
    binding: &AttachmentBinding,
) -> Result<String> {
    json_hash(&serde_json::json!({
        "service_boot_id": service_boot_id,
        "route_id": route_id,
        "binding": binding,
    }))
}

fn cmux_route_from_row(row: CmuxRouteRow) -> Result<CmuxAttachmentRoute> {
    let (
        id,
        service_boot_id,
        session_id,
        role_generation_id,
        transcript_epoch,
        process_identity_json,
        attachment_mode,
        workspace_id,
        surface_id,
        surface_state,
        attachment_state,
        resume_state,
        last_error,
        created_at,
        updated_at,
    ) = row;
    let process = serde_json::from_str(&process_identity_json)
        .context("parse persisted cmux attachment process identity")?;
    let binding = AttachmentBinding {
        session_id,
        role_generation_id,
        transcript_epoch,
        process,
    };
    let mode = match attachment_mode.as_str() {
        "watch" => CmuxAttachmentMode::Watch,
        "control" => CmuxAttachmentMode::Control,
        _ => bail!("persisted cmux attachment mode is invalid"),
    };
    binding
        .validate_route_identifiers()
        .map_err(|error| anyhow!(error))?;
    valid_cmux_uuid("cmux attachment route", &id)?;
    valid_cmux_uuid("cmux service boot", &service_boot_id)?;
    if let Some(workspace_id) = workspace_id.as_deref() {
        valid_cmux_uuid("cmux workspace", workspace_id)?;
    }
    if let Some(surface_id) = surface_id.as_deref() {
        valid_cmux_uuid("cmux surface", surface_id)?;
    }
    Ok(CmuxAttachmentRoute {
        id,
        service_boot_id,
        binding,
        mode,
        workspace_id,
        surface_id,
        surface_state,
        attachment_state,
        resume_state,
        last_error,
        created_at,
        updated_at,
    })
}

fn cmux_attachment_route_for_connection(
    connection: &Connection,
    route_id: &str,
) -> Result<CmuxAttachmentRoute> {
    let row: CmuxRouteRow = connection
        .query_row(
            "SELECT id,service_boot_id,session_id,role_generation_id,transcript_epoch,
                    process_identity_json,attachment_mode,workspace_id,surface_id,surface_state,
                    attachment_state,resume_state,last_error,created_at,updated_at
               FROM cmux_attachment_routes WHERE id=?1",
            params![route_id],
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
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown cmux attachment route"))?;
    cmux_route_from_row(row)
}

fn cmux_task_workspace_from_row(row: CmuxTaskWorkspaceRow) -> Result<CmuxTaskWorkspace> {
    let (
        id,
        service_boot_id,
        task_id,
        generation,
        workspace_id,
        opening_surface_id,
        state,
        last_error,
        created_at,
        updated_at,
    ) = row;
    valid_cmux_uuid("cmux task workspace", &id)?;
    valid_cmux_uuid("cmux service boot", &service_boot_id)?;
    if let Some(workspace_id) = workspace_id.as_deref() {
        valid_cmux_uuid("cmux workspace", workspace_id)?;
    }
    if let Some(surface_id) = opening_surface_id.as_deref() {
        valid_cmux_uuid("cmux opening surface", surface_id)?;
    }
    if generation <= 0
        || !matches!(
            state.as_str(),
            "opening" | "open" | "unknown" | "lost" | "retired" | "failed"
        )
    {
        bail!("persisted cmux task workspace state is invalid")
    }
    Ok(CmuxTaskWorkspace {
        id,
        service_boot_id,
        task_id,
        generation,
        workspace_id,
        opening_surface_id,
        state,
        last_error,
        created_at,
        updated_at,
    })
}

fn cmux_session_surface_from_row(row: CmuxSessionSurfaceRow) -> Result<CmuxSessionSurface> {
    let (
        id,
        task_workspace_id,
        service_boot_id,
        session_id,
        role_generation_id,
        transcript_epoch,
        process_identity_json,
        binding_revision,
        workspace_id,
        surface_id,
        surface_state,
        attachment_state,
        desired_input_state,
        actual_input_state,
        control_revision,
        applied_revision,
        last_error,
        created_at,
        updated_at,
    ) = row;
    valid_cmux_uuid("cmux session surface", &id)?;
    valid_cmux_uuid("cmux task workspace", &task_workspace_id)?;
    valid_cmux_uuid("cmux service boot", &service_boot_id)?;
    if let Some(workspace_id) = workspace_id.as_deref() {
        valid_cmux_uuid("cmux workspace", workspace_id)?;
    }
    if let Some(surface_id) = surface_id.as_deref() {
        valid_cmux_uuid("cmux surface", surface_id)?;
    }
    let binding = AttachmentBinding {
        session_id,
        role_generation_id,
        transcript_epoch,
        process: serde_json::from_str(&process_identity_json)
            .context("parse persisted cmux session-surface process identity")?,
    };
    binding
        .validate_route_identifiers()
        .map_err(|error| anyhow!(error))?;
    if binding_revision <= 0
        || control_revision < 0
        || applied_revision < 0
        || applied_revision > control_revision
        || !matches!(
            surface_state.as_str(),
            "opening" | "open" | "unknown" | "lost" | "retired" | "failed"
        )
        || !matches!(
            attachment_state.as_str(),
            "pending" | "live" | "ended" | "failed"
        )
        || !matches!(desired_input_state.as_str(), "view_only" | "control")
        || !matches!(
            actual_input_state.as_str(),
            "view_only" | "control" | "blocked" | "lost"
        )
    {
        bail!("persisted cmux session-surface state is invalid")
    }
    Ok(CmuxSessionSurface {
        id,
        task_workspace_id,
        service_boot_id,
        binding,
        binding_revision,
        workspace_id,
        surface_id,
        surface_state,
        attachment_state,
        desired_input_state,
        actual_input_state,
        control_revision,
        applied_revision,
        last_error,
        created_at,
        updated_at,
    })
}

fn cmux_task_workspace_for_connection(
    connection: &Connection,
    workspace_id: &str,
) -> Result<CmuxTaskWorkspace> {
    let row: CmuxTaskWorkspaceRow = connection
        .query_row(
            "SELECT id,service_boot_id,task_id,generation,workspace_id,opening_surface_id,state,
                    last_error,created_at,updated_at
               FROM cmux_task_workspaces WHERE id=?1",
            params![workspace_id],
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
                    row.get(9)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown cmux task workspace"))?;
    cmux_task_workspace_from_row(row)
}

fn cmux_session_surface_for_connection(
    connection: &Connection,
    surface_id: &str,
) -> Result<CmuxSessionSurface> {
    let row: CmuxSessionSurfaceRow = connection
        .query_row(
            "SELECT id,task_workspace_id,service_boot_id,session_id,role_generation_id,
                    transcript_epoch,process_identity_json,binding_revision,workspace_id,surface_id,
                    surface_state,attachment_state,desired_input_state,actual_input_state,
                    control_revision,applied_revision,last_error,created_at,updated_at
               FROM cmux_session_surfaces WHERE id=?1",
            params![surface_id],
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
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                    row.get(17)?,
                    row.get(18)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow!("unknown cmux session surface"))?;
    cmux_session_surface_from_row(row)
}

/// An expired lease is positive durable evidence that its exact lost
/// attachment can no longer retain keyboard authority.  This is the sole
/// non-socket path that can end a lost live attachment; a merely absent lease
/// is not evidence that a still-connected client released its in-memory
/// secret.
fn observe_expired_lost_cmux_leases_for_task(
    transaction: &Transaction<'_>,
    service_boot_id: &str,
    task_id: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "UPDATE cmux_session_surfaces
            SET attachment_state='ended',desired_input_state='view_only',actual_input_state='lost',
                last_error='Validated cmux loss retirement observed the exact bounded human lease expiry',updated_at=?1
          WHERE service_boot_id=?2 AND surface_state='lost' AND attachment_state='live'
            AND task_workspace_id IN (
                SELECT id FROM cmux_task_workspaces
                 WHERE service_boot_id=?2 AND task_id=?3 AND state='lost'
            )
            AND EXISTS(
                SELECT 1 FROM input_leases lease
                 WHERE lease.session_id=cmux_session_surfaces.session_id
                   AND lease.role_generation_id=cmux_session_surfaces.role_generation_id
                   AND lease.process_identity_json=cmux_session_surfaces.process_identity_json
                   AND lease.revoked_at IS NULL
                   AND julianday(lease.expires_at)<=julianday(?1)
            )",
        params![&now, service_boot_id, task_id],
    )?;
    Ok(())
}

fn observe_expired_lost_cmux_leases_for_binding(
    transaction: &Transaction<'_>,
    service_boot_id: &str,
    binding: &AttachmentBinding,
    process_json: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    transaction.execute(
        "UPDATE cmux_session_surfaces
            SET attachment_state='ended',desired_input_state='view_only',actual_input_state='lost',
                last_error='Validated cmux loss retirement observed the exact bounded human lease expiry',updated_at=?1
          WHERE service_boot_id=?2 AND session_id=?3 AND role_generation_id=?4
            AND transcript_epoch=?5 AND process_identity_json=?6
            AND surface_state='lost' AND attachment_state='live'
            AND EXISTS(
                SELECT 1 FROM input_leases lease
                 WHERE lease.session_id=cmux_session_surfaces.session_id
                   AND lease.role_generation_id=cmux_session_surfaces.role_generation_id
                   AND lease.process_identity_json=cmux_session_surfaces.process_identity_json
                   AND lease.revoked_at IS NULL
                   AND julianday(lease.expires_at)<=julianday(?1)
            )",
        params![
            &now,
            service_boot_id,
            &binding.session_id,
            &binding.role_generation_id,
            &binding.transcript_epoch,
            process_json,
        ],
    )?;
    Ok(())
}

fn pending_lost_cmux_retirement_for_task(
    transaction: &Transaction<'_>,
    service_boot_id: &str,
    task_id: &str,
) -> Result<Option<String>> {
    transaction
        .query_row(
            "SELECT surface.id
               FROM cmux_task_workspaces workspace
               JOIN cmux_session_surfaces surface ON surface.task_workspace_id=workspace.id
              WHERE workspace.service_boot_id=?1 AND workspace.task_id=?2 AND workspace.state='lost'
                AND surface.service_boot_id=?1 AND surface.surface_state='lost'
                AND (
                    surface.attachment_state='live'
                    OR EXISTS(
                        SELECT 1 FROM input_leases lease
                         WHERE lease.session_id=surface.session_id
                           AND lease.role_generation_id=surface.role_generation_id
                           AND lease.process_identity_json=surface.process_identity_json
                           AND lease.revoked_at IS NULL
                           AND julianday(lease.expires_at)>julianday('now')
                    )
                )
              ORDER BY workspace.generation DESC,surface.binding_revision DESC LIMIT 1",
            params![service_boot_id, task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn pending_lost_cmux_retirement_for_binding(
    transaction: &Transaction<'_>,
    service_boot_id: &str,
    binding: &AttachmentBinding,
    process_json: &str,
) -> Result<Option<String>> {
    transaction
        .query_row(
            "SELECT surface.id FROM cmux_session_surfaces surface
              WHERE surface.service_boot_id=?1 AND surface.session_id=?2
                AND surface.role_generation_id=?3 AND surface.transcript_epoch=?4
                AND surface.process_identity_json=?5 AND surface.surface_state='lost'
                AND (
                    surface.attachment_state='live'
                    OR EXISTS(
                        SELECT 1 FROM input_leases lease
                         WHERE lease.session_id=surface.session_id
                           AND lease.role_generation_id=surface.role_generation_id
                           AND lease.process_identity_json=surface.process_identity_json
                           AND lease.revoked_at IS NULL
                           AND julianday(lease.expires_at)>julianday('now')
                    )
                )
              ORDER BY surface.binding_revision DESC LIMIT 1",
            params![
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                process_json,
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn cmux_view_request_hash(service_boot_id: &str, session_id: &str) -> Result<String> {
    json_hash(&serde_json::json!({
        "service_boot_id": service_boot_id,
        "session_id": session_id,
    }))
}

fn cmux_keyboard_control_request_hash(
    service_boot_id: &str,
    session_id: &str,
    surface_id: &str,
    binding_revision: i64,
    control_revision: i64,
    action: CmuxKeyboardControlAction,
) -> Result<String> {
    json_hash(&serde_json::json!({
        "service_boot_id": service_boot_id,
        "session_id": session_id,
        "surface_id": surface_id,
        "binding_revision": binding_revision,
        "control_revision": control_revision,
        "action": action,
    }))
}

fn cmux_session_surface_discard_request_hash(
    service_boot_id: &str,
    surface_id: &str,
    binding: &AttachmentBinding,
) -> Result<String> {
    json_hash(&serde_json::json!({
        "service_boot_id": service_boot_id,
        "surface_id": surface_id,
        "binding": binding,
    }))
}

fn meaningful_validation_observation(
    metadata: &serde_json::Value,
    expected_cell: Option<&str>,
) -> Result<bool> {
    let Some(observation) = metadata.get("validation_observation") else {
        return Ok(false);
    };
    let meaningful = match observation {
        serde_json::Value::String(value) => !value.trim().is_empty() && value.len() <= 64 * 1024,
        serde_json::Value::Object(object) => {
            if object.is_empty() || serde_json::to_vec(object)?.len() > 64 * 1024 {
                false
            } else if let Some(cell) = object.get("cell") {
                cell.as_str()
                    .is_some_and(|cell| Some(cell) == expected_cell)
            } else {
                true
            }
        }
        _ => false,
    };
    Ok(meaningful)
}

fn validate_role_report_contract(
    role: RoleKind,
    isolated_validation: bool,
    workflow_validation: bool,
    validation_cell: Option<&str>,
    report: &RoleResultReport,
) -> Result<()> {
    let capability_observation = report.outcome == "capability_observed";
    if let Some(format) = report.metadata.get("runtime_report_format") {
        if format.as_str() != Some("runtime-v1") {
            bail!("unknown runtime report format")
        }
        if validation_cell != Some("trip_runtime_probe") || !capability_observation {
            bail!("runtime-v1 reports require the server-owned runtime probe cell and capability_observed outcome")
        }
    }
    let meaningful = meaningful_validation_observation(&report.metadata, validation_cell)?;
    if isolated_validation {
        if !capability_observation || validation_cell.is_none() || !meaningful {
            bail!("isolated capability validation accepts only capability_observed with a bounded meaningful validation_observation")
        }
        return Ok(());
    }
    if capability_observation {
        if !workflow_validation || validation_cell.is_none() || !meaningful {
            bail!("workflow capability_observed requires its bounded validation cell and a meaningful validation_observation")
        }
        return Ok(());
    }
    if report.metadata.get("validation_observation").is_some() && !meaningful {
        bail!("validation_observation must be a bounded non-empty string or non-empty object for the server-owned cell")
    }
    match role {
        RoleKind::Manager => {
            if !["plan_ready", "handoff_ready", "blocked", "needs_input"]
                .contains(&report.outcome.as_str())
            {
                bail!("unsupported manager outcome")
            }
            if report.outcome == "plan_ready"
                && !report
                    .metadata
                    .get("plan")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty() && value.len() <= 256 * 1024)
            {
                bail!("plan_ready requires bounded metadata.plan")
            }
            if report.outcome == "handoff_ready"
                && (!report
                    .metadata
                    .get("candidate_hash")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                    || !report
                        .metadata
                        .get("final_review_request_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| !value.is_empty()))
            {
                bail!("handoff_ready requires candidate_hash and final_review_request_id metadata")
            }
        }
        RoleKind::Explorer => {
            if !["evidence_ready", "blocked", "needs_input"].contains(&report.outcome.as_str()) {
                bail!("unsupported Explorer outcome")
            }
        }
        RoleKind::Implementer => {
            if !["candidate_ready", "blocked", "needs_input"].contains(&report.outcome.as_str()) {
                bail!("unsupported implementer outcome")
            }
        }
        RoleKind::PlanReviewer | RoleKind::CodeReviewer | RoleKind::FinalReviewer => {
            if !["approved", "request_changes", "needs_rework"].contains(&report.outcome.as_str()) {
                bail!("unsupported reviewer outcome")
            }
            if !report
                .metadata
                .get("review_kind")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| ["plan", "code", "final"].contains(&kind))
                || !report
                    .metadata
                    .get("candidate_hash")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|hash| !hash.is_empty())
                || !report
                    .metadata
                    .get("review_request_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| !id.is_empty())
            {
                bail!("review result requires review_request_id, review_kind, and candidate_hash metadata")
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct Store {
    pub(crate) connection: Arc<Mutex<Connection>>,
    state_changes: Arc<Notify>,
    pub(crate) compatibility_bundles: Arc<crate::provider_compatibility::BundleSet>,
}

/// Exclusive access to the state connection. Releasing it wakes state waiters
/// only when the committed revision advanced while it was held.
pub(crate) struct StoreGuard<'a> {
    connection: MutexGuard<'a, Connection>,
    state_changes: &'a Notify,
    total_changes_at_acquire: u64,
    revision_at_acquire: Option<i64>,
}

impl<'a> StoreGuard<'a> {
    fn acquire(connection: MutexGuard<'a, Connection>, state_changes: &'a Notify) -> Self {
        Self {
            total_changes_at_acquire: connection.total_changes(),
            revision_at_acquire: read_state_revision(&connection).ok(),
            connection,
            state_changes,
        }
    }
}

impl Deref for StoreGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.connection
    }
}

impl DerefMut for StoreGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

impl Drop for StoreGuard<'_> {
    fn drop(&mut self) {
        // total_changes also counts rolled-back work, so only a re-read committed
        // revision proves there is newer state. Missed wakes are reconciled by the
        // dashboard's watchdog read, never by failing the already committed write.
        if self.connection.total_changes() == self.total_changes_at_acquire
            || !self.connection.is_autocommit()
        {
            return;
        }
        let Some(acquired) = self.revision_at_acquire else {
            tracing::warn!("state revision was unreadable when the store lock was acquired");
            return;
        };
        match read_state_revision(&self.connection) {
            Ok(released) if released > acquired => self.state_changes.notify_waiters(),
            Ok(_) => {}
            Err(error) => tracing::warn!(error = %error, "state revision observation failed"),
        }
    }
}

impl Store {
    fn from_connection(connection: Connection) -> Self {
        Self {
            connection: Arc::new(Mutex::new(connection)),
            state_changes: Arc::new(Notify::new()),
            compatibility_bundles: Arc::new(crate::provider_compatibility::BundleSet::embedded()),
        }
    }

    #[doc(hidden)]
    pub fn with_synthetic_compatibility_for_tests(mut self, codex: &str, claude: &str) -> Self {
        self.compatibility_bundles =
            Arc::new(crate::provider_compatibility::BundleSet::synthetic_for_tests(codex, claude));
        self
    }

    pub fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)
            .with_context(|| format!("open state database {}", path.display()))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        migrate(&mut connection)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self::from_connection(connection))
    }

    pub(crate) fn open_service(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Self::open(path);
        }
        Self::open_current_writable(path).and_then(|store| {
            let connection = store.lock()?;
            connection.pragma_update(None, "journal_mode", "WAL")?;
            drop(connection);
            Ok(store)
        })
    }

    pub fn open_current_readonly(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open state database read-only {}", path.display()))?;
        connection.pragma_update(None, "query_only", "ON")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        require_current_schema(&connection)?;
        Ok(Self::from_connection(connection))
    }

    pub(crate) fn open_current_writable(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open current state database {}", path.display()))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        require_current_schema(&connection)?;
        Ok(Self::from_connection(connection))
    }

    pub(crate) fn restore_hold(&self) -> Result<Option<serde_json::Value>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT detail_json FROM recovery_records
                 WHERE id='database-restore-hold' AND state='attention_required'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| serde_json::from_str(&value).context("parse database restore hold"))
            .transpose()
    }

    pub(crate) fn require_execution_unheld(&self, action: &str) -> Result<()> {
        if self.restore_hold()?.is_some() {
            bail!("{action} is disabled by the database restore hold")
        }
        Ok(())
    }

    pub(crate) fn lock(&self) -> Result<StoreGuard<'_>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("state database lock poisoned"))?;
        Ok(StoreGuard::acquire(connection, &self.state_changes))
    }

    /// Woken after a commit through any clone of this store advances the state
    /// revision. Wakes are lossy hints; the durable revision stays authoritative.
    pub(crate) fn state_changes(&self) -> &Notify {
        &self.state_changes
    }

    pub(crate) fn state_revision(&self) -> Result<i64> {
        let connection = self.lock()?;
        read_state_revision(&connection).context("read committed state revision")
    }

    pub fn role_context(&self, token: &str) -> Result<RoleContext> {
        if self.restore_hold()?.is_some() {
            bail!("role authority is disabled by the database restore hold")
        }
        let hash = auth::hash_secret(token);
        let connection = self.lock()?;
        let row = connection.query_row(
            "SELECT p.id, t.id, a.id, rg.id, s.id, rc.id, s.transcript_epoch, rg.role, rg.provider, a.configuration_revision, rg.lane_id, rc.token_hash, rc.permissions_json
             FROM role_credentials rc
             JOIN role_generations rg ON rg.id = rc.role_generation_id
             JOIN attempts a ON a.id = rg.attempt_id
             JOIN tasks t ON t.id = a.task_id
             JOIN projects p ON p.id = t.project_id
             JOIN sessions s ON s.role_generation_id = rg.id
             WHERE rc.token_hash = ?1 AND rc.revoked_at IS NULL AND rg.status NOT IN ('revoked', 'replaced')",
            params![hash],
            |row| {
                let role: String = row.get(7)?;
                let provider: String = row.get(8)?;
                let permissions: String = row.get(12)?;
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, role, provider, row.get(9)?, row.get(10)?,
                    row.get::<_, String>(11)?, permissions,
                ))
            },
        ).optional()?;
        let Some((
            project_id,
            task_id,
            attempt_id,
            role_generation_id,
            session_id,
            credential_id,
            transcript_epoch,
            role,
            provider,
            configuration_revision,
            lane_id,
            expected_hash,
            permissions_json,
        )) = row
        else {
            bail!("role credential is unknown, revoked, or stale")
        };
        auth::verify_secret(token, &expected_hash)?;
        Ok(RoleContext {
            project_id,
            task_id,
            attempt_id,
            role_generation_id,
            session_id,
            credential_id,
            transcript_epoch,
            role: role.parse().map_err(|error: String| anyhow!(error))?,
            provider: provider.parse().map_err(|error: String| anyhow!(error))?,
            configuration_revision,
            lane_id,
            permissions: serde_json::from_str(&permissions_json)?,
        })
    }

    pub fn save_role_result(
        &self,
        context: &RoleContext,
        report: &RoleResultReport,
    ) -> Result<serde_json::Value> {
        if !context
            .permissions
            .iter()
            .any(|permission| permission == "report_result")
        {
            bail!("this role credential cannot report a result")
        }
        if report.operation_id.trim().is_empty()
            || report.summary.trim().is_empty()
            || report.summary.len() > 64 * 1024
        {
            bail!("role result requires an operation ID and a bounded non-empty summary")
        }
        if report.evidence.len() > 128 || report.evidence.iter().any(|value| value.len() > 8192) {
            bail!("role result evidence exceeds bounded limits")
        }
        if !report.metadata.is_object() {
            bail!("role result metadata must be an object")
        }
        let now = Utc::now().to_rfc3339();
        let request_hash = json_hash(report)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (isolated_validation, workflow_validation, validation_cell): (bool, bool, Option<String>) = transaction.query_row(
            "SELECT a.status='capability_validation',t.lifecycle='validation',s.validation_cell FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id WHERE s.id=?1 AND s.role_generation_id=?2 AND rg.attempt_id=?3",
            params![context.session_id, context.role_generation_id, context.attempt_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        )?;
        validate_role_report_contract(
            context.role,
            isolated_validation,
            workflow_validation,
            validation_cell.as_deref(),
            report,
        )?;
        let stored_receipt: Option<(String, String)> = transaction
            .query_row(
                "SELECT request_hash, result_json FROM operation_receipts
             WHERE operation_id = ?1 AND actor_key = ?2 AND operation_kind = 'role_report'",
                params![report.operation_id, context.role_generation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if context.role.is_reviewer() && report.outcome != "capability_observed" {
            let expected_kind = match context.role {
                RoleKind::PlanReviewer => "plan",
                RoleKind::CodeReviewer => "code",
                RoleKind::FinalReviewer => "final",
                _ => unreachable!(),
            };
            let exact_review: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_requests
                 WHERE id=?1 AND attempt_id=?2 AND review_kind=?3 AND candidate_hash=?4
                   AND role_generation_id=?5 AND session_id=?6
                   AND (?7 OR delivery_state='delivered'))",
                params![
                    report
                        .metadata
                        .get("review_request_id")
                        .and_then(serde_json::Value::as_str),
                    context.attempt_id,
                    expected_kind,
                    report
                        .metadata
                        .get("candidate_hash")
                        .and_then(serde_json::Value::as_str),
                    context.role_generation_id,
                    context.session_id,
                    stored_receipt.is_some(),
                ],
                |row| row.get(0),
            )?;
            if report
                .metadata
                .get("review_kind")
                .and_then(serde_json::Value::as_str)
                != Some(expected_kind)
                || !exact_review
            {
                bail!("review result does not match the exact current delivered review request, kind, candidate, session, and generation")
            }
        }
        if let Some((stored_hash, result)) = stored_receipt {
            if stored_hash != request_hash {
                bail!("operation ID was already used with different input")
            }
            return Ok(serde_json::from_str(&result)?);
        }
        let current_authority: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM role_generations rg
               JOIN sessions s ON s.id=?2 AND s.role_generation_id=rg.id
               JOIN attempts a ON a.id=rg.attempt_id
               WHERE rg.id=?1 AND rg.status='running' AND s.status='running'
                 AND EXISTS(SELECT 1 FROM role_credentials rc
                   WHERE rc.role_generation_id=rg.id AND rc.revoked_at IS NULL)
                 AND (a.status='capability_validation' OR EXISTS(
                   SELECT 1 FROM role_settings rs
                   WHERE rs.task_id=a.task_id AND rs.role=rg.role
                     AND rs.effective_generation_id=rg.id) OR
                   (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                     SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id
                       AND lg.effective_generation_id=rg.id))))",
            params![context.role_generation_id, context.session_id],
            |row| row.get(0),
        )?;
        if !current_authority {
            bail!("role result authority was revoked, replaced, or stopped before persistence")
        }
        crate::trip::record_setup_probe_report(&transaction, context, report)?;
        if context.role == RoleKind::Implementer
            && context.lane_id != "default"
            && report.outcome == "candidate_ready"
        {
            bail!("explicit implementation lanes must yield a scoped receipt; only the manager-directed integration writer can report candidate_ready")
        }
        if context.role == RoleKind::Manager && report.outcome == "plan_ready" {
            let latest_rejection: Option<String> = transaction
                .query_row(
                    "SELECT id FROM review_requests
                     WHERE attempt_id=?1 AND review_kind='plan' AND delivery_state='finished'
                       AND verdict IN ('request_changes','needs_rework')
                     ORDER BY rowid DESC LIMIT 1",
                    params![context.attempt_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(request_id) = latest_rejection {
                if report
                    .metadata
                    .get("rejected_plan_review_request_id")
                    .and_then(|value| value.as_str())
                    != Some(request_id.as_str())
                {
                    bail!("plan_ready requires the exact latest rejected_plan_review_request_id metadata")
                }
                let plan = report
                    .metadata
                    .get("plan")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow!("plan_ready requires bounded metadata.plan"))?;
                let plan_hash = hex::encode(Sha256::digest(plan.as_bytes()));
                let previously_rejected: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_requests
                     WHERE attempt_id=?1 AND review_kind='plan' AND delivery_state='finished'
                       AND verdict IN ('request_changes','needs_rework') AND candidate_hash=?2)",
                    params![context.attempt_id, plan_hash],
                    |row| row.get(0),
                )?;
                if previously_rejected {
                    bail!("plan_ready cannot repeat a previously rejected plan candidate")
                }
            }
        }
        crate::trip::record_structured_plan(&transaction, context, report)?;
        crate::trip::record_explorer_outcome(&transaction, context, report)?;
        let result_id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO role_results(id, operation_id, session_id, role_generation_id, outcome, summary, evidence_json, metadata_json, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                result_id, report.operation_id, context.session_id, context.role_generation_id,
                report.outcome, report.summary, serde_json::to_string(&report.evidence)?,
                serde_json::to_string(&report.metadata)?, now,
            ],
        )?;
        let result = serde_json::json!({
            "accepted": true,
            "result_id": result_id,
            "session_id": context.session_id,
            "authoritative_completion": false,
            "message": "role result recorded; supervisor and workflow gates remain authoritative"
        });
        transaction.execute(
            "INSERT INTO audit_events(id, operation_id, actor_kind, actor_id, event_code, entity_kind, entity_id, detail_json, created_at)
             VALUES(?1, ?2, 'role', ?3, 'role.result.recorded', 'session', ?4, ?5, ?6)",
            params![uuid::Uuid::new_v4().to_string(), report.operation_id, context.role_generation_id, context.session_id, serde_json::to_string(&result)?, now],
        )?;
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id, actor_key, operation_kind, request_hash, result_json, created_at)
             VALUES(?1, ?2, 'role_report', ?3, ?4, ?5)",
            params![report.operation_id, context.role_generation_id, request_hash, serde_json::to_string(&result)?, now],
        )?;
        transaction.commit()?;
        #[cfg(test)]
        if TEST_INTERRUPT_ROLE_REPORT_AFTER_COMMIT.with(|armed| armed.replace(false)) {
            bail!("injected interruption after role report commit")
        }
        Ok(result)
    }

    pub fn save_hook_event(
        &self,
        context: &RoleContext,
        envelope: &HookEnvelope,
        provenance: &RolePeerProvenance,
    ) -> Result<serde_json::Value> {
        if !context
            .permissions
            .iter()
            .any(|permission| permission == "report_hook")
        {
            bail!("this role credential cannot report hook events")
        }
        if context.provider != envelope.provider {
            bail!("hook provider does not match role credential")
        }
        let event_name = envelope
            .payload
            .get("hook_event_name")
            .and_then(|value| value.as_str())
            .unwrap_or("Unknown")
            .to_owned();
        let native_session_id = envelope
            .payload
            .get("session_id")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let native_event = matches!(
            event_name.as_str(),
            "SessionStart"
                | "UserPromptSubmit"
                | "PreToolUse"
                | "PermissionRequest"
                | "PermissionDenied"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "Stop"
                | "StopFailure"
                | "Interrupt"
                | "SubagentStart"
                | "SubagentStop"
                | "SessionEnd"
        );
        let payload_cwd = envelope.payload.get("cwd").and_then(|value| value.as_str());
        let expected_cwd = {
            let connection = self.lock()?;
            connection.query_row(
                "SELECT COALESCE((SELECT path FROM workspaces WHERE attempt_id=?1),repository_path) FROM projects WHERE id=?2",
                params![context.attempt_id, context.project_id], |row| row.get::<_, String>(0),
            )?
        };
        let expected_cwd = std::fs::canonicalize(&expected_cwd).ok();
        let cwd_matches = payload_cwd
            .and_then(|cwd| std::fs::canonicalize(cwd).ok())
            .is_some_and(|cwd| expected_cwd.as_deref() == Some(cwd.as_path()));
        let mut identity_eligible = native_event
            && cwd_matches
            && native_session_id
                .as_deref()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_invocation: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_credentials rc
               JOIN role_generations rg ON rg.id=rc.role_generation_id
               JOIN sessions s ON s.role_generation_id=rg.id
               WHERE rc.id=?1 AND rc.role_generation_id=?2 AND rc.revoked_at IS NULL
                 AND s.id=?3 AND s.transcript_epoch=?4
                 AND rg.status='running' AND s.status='running')",
            params![
                context.credential_id,
                context.role_generation_id,
                context.session_id,
                context.transcript_epoch
            ],
            |row| row.get(0),
        )?;
        if !current_invocation {
            bail!("hook credential or invocation identity is revoked, stale, or no longer running")
        }
        transaction.execute(
            "INSERT INTO hook_events(id, session_id, role_generation_id, provider, event_name, native_session_id, payload_json,
                    peer_pid, peer_process_group_id, peer_start_marker, provenance_state, received_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![uuid::Uuid::new_v4().to_string(), context.session_id, context.role_generation_id, envelope.provider.to_string(), event_name, native_session_id,
                serde_json::to_string(&envelope.payload)?, provenance.peer_pid, provenance.peer_process_group_id,
                provenance.peer_start_marker.as_str(), provenance.state.as_str(), now],
        )?;
        let hook_rowid = transaction.last_insert_rowid();
        if let Some(native_id) = native_session_id.as_deref().filter(|_| identity_eligible) {
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT native_session_id FROM sessions WHERE id = ?1",
                    params![context.session_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if existing
                .as_deref()
                .is_some_and(|current| current != native_id)
            {
                bail!("native session identity changed within one role generation")
            }
            if existing.is_none() && event_name != "SessionStart" {
                identity_eligible = false;
            } else {
                transaction.execute(
                    "UPDATE sessions SET native_session_id = COALESCE(native_session_id, ?1),
                        native_identity_source = 'managed_descendant_hook_unverified',
                        hook_trust_state = 'observed_unverified', updated_at = ?2 WHERE id = ?3",
                    params![native_id, now, context.session_id],
                )?;
            }
        }
        if native_event && !identity_eligible {
            transaction.execute(
                "UPDATE hook_events SET event_name='UntrustedNativeEvent' WHERE rowid=?1",
                params![hook_rowid],
            )?;
        }
        let current_invocation_start_rowid: Option<i64> = if identity_eligible {
            transaction
                .query_row(
                    "SELECT h.rowid
                     FROM hook_events h
                     JOIN sessions s ON s.id=h.session_id
                     LEFT JOIN resume_invocations ri
                       ON ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                     WHERE h.session_id=?1 AND h.event_name='SessionStart' AND h.rowid<?2
                       AND h.native_session_id=s.native_session_id
                       AND h.rowid>CASE
                         WHEN ri.id IS NULL THEN s.initial_hook_event_boundary_rowid
                         ELSE ri.hook_event_boundary_rowid
                       END
                     ORDER BY h.rowid DESC LIMIT 1",
                    params![context.session_id, hook_rowid],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            None
        };
        let safe_idle_boundary = if event_name == "Stop" {
            let submit_rowid: Option<i64> =
                if let Some(session_start) = current_invocation_start_rowid {
                    transaction.query_row(
                        "SELECT MAX(rowid) FROM hook_events
                     WHERE session_id=?1 AND event_name='UserPromptSubmit'
                       AND native_session_id=(SELECT native_session_id FROM sessions WHERE id=?1)
                       AND rowid>?2 AND rowid<=?3",
                        params![context.session_id, session_start, hook_rowid],
                        |row| row.get(0),
                    )?
                } else {
                    None
                };
            if let Some(submit) = submit_rowid {
                let permission_pending: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM permission_requests WHERE session_id=?1 AND state='pending')",
                    params![context.session_id],
                    |row| row.get(0),
                )?;
                if context.provider == crate::domain::Provider::Claude {
                    // Claude's current registries are authoritative because denied tools and
                    // provider-internal child stops do not produce balanced hook pairs.
                    ["background_tasks", "session_crons"].iter().all(|field| {
                        envelope
                            .payload
                            .get(*field)
                            .and_then(|value| value.as_array())
                            .is_some_and(Vec::is_empty)
                    }) && !permission_pending
                } else {
                    'codex: {
                        if permission_pending {
                            break 'codex false;
                        }
                        // A normal event name is persisted only after this function's
                        // canonical native-session and cwd gate has accepted it. Rows
                        // that fail that gate are retained as UntrustedNativeEvent and
                        // must keep the whole current turn busy below.
                        let mut statement = transaction.prepare(
                            r#"
                            WITH current_turn AS (
                                SELECT rowid,event_name,native_session_id,payload_json,
                                       peer_pid,peer_process_group_id,peer_start_marker,provenance_state,
                                       received_at
                                FROM hook_events
                                WHERE session_id=?1 AND role_generation_id=?2 AND provider='codex'
                                  AND rowid BETWEEN ?3 AND ?4
                            ),
                            relevant_events AS (
                                SELECT *,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.hook_event_name') END AS payload_event_name,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.session_id') END AS payload_session_id,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.cwd') END AS payload_cwd,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_type(payload_json,'$.session_id') END AS session_id_type,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_type(payload_json,'$.cwd') END AS cwd_type,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.tool_use_id') END AS tool_use_id,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_type(payload_json,'$.tool_use_id') END AS tool_use_id_type,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.tool_name') END AS tool_name,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_type(payload_json,'$.tool_name') END AS tool_name_type,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.tool_input.command') END AS command,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_type(payload_json,'$.tool_input.command') END AS command_type,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_extract(payload_json,'$.agent_id') END AS agent_id,
                                    CASE WHEN json_valid(payload_json)
                                        THEN json_type(payload_json,'$.agent_id') END AS agent_id_type
                                FROM current_turn
                                WHERE event_name IN (
                                    'PreToolUse','PostToolUse','PostToolUseFailure',
                                    'PermissionRequest','PermissionDenied',
                                    'SubagentStart','SubagentStop','UntrustedNativeEvent'
                                )
                            ),
                            pre AS (
                                SELECT rowid,payload_json,tool_use_id,tool_use_id_type,
                                       tool_name,tool_name_type,command,command_type,payload_cwd,cwd_type
                                FROM relevant_events WHERE event_name='PreToolUse'
                            ),
                            terminal AS (
                                SELECT rowid,tool_use_id,tool_use_id_type
                                FROM relevant_events
                                WHERE event_name IN ('PostToolUse','PostToolUseFailure')
                            ),
                            permission_hooks AS (
                                SELECT rowid,payload_json,tool_name,tool_name_type,
                                       command,command_type,payload_cwd,cwd_type,received_at
                                FROM relevant_events WHERE event_name='PermissionRequest'
                            ),
                            stop_event AS (
                                SELECT received_at FROM current_turn
                                WHERE rowid=?4 AND event_name='Stop'
                            ),
                            child_start AS (
                                SELECT rowid,agent_id,agent_id_type
                                FROM relevant_events WHERE event_name='SubagentStart'
                            ),
                            child_stop AS (
                                SELECT rowid,agent_id,agent_id_type
                                FROM relevant_events WHERE event_name='SubagentStop'
                            ),
                            unmatched AS (
                                SELECT * FROM pre candidate
                                WHERE NOT EXISTS (
                                    SELECT 1 FROM terminal
                                    WHERE terminal.tool_use_id=candidate.tool_use_id
                                      AND terminal.rowid>candidate.rowid
                                )
                            ),
                            turn_integrity AS (
                                SELECT
                                    NOT EXISTS (
                                        SELECT 1 FROM relevant_events event
                                        WHERE event.event_name='UntrustedNativeEvent'
                                           OR COALESCE(json_valid(event.payload_json),0)=0
                                           OR event.payload_event_name IS NOT event.event_name
                                           OR event.session_id_type IS NOT 'text'
                                           OR event.payload_session_id IS NOT (
                                                SELECT native_session_id FROM sessions WHERE id=?1
                                           )
                                           OR event.cwd_type IS NOT 'text'
                                           OR TRIM(COALESCE(event.payload_cwd,''))=''
                                           OR event.native_session_id IS NOT (
                                                SELECT native_session_id FROM sessions WHERE id=?1
                                           )
                                           OR event.provenance_state IS NOT
                                                'managed_process_group_untrusted_payload'
                                           OR COALESCE(event.peer_pid,0)<=0
                                           OR COALESCE(event.peer_process_group_id,0)<=0
                                           OR TRIM(COALESCE(event.peer_start_marker,''))=''
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM pre
                                        WHERE tool_use_id_type IS NOT 'text'
                                           OR TRIM(tool_use_id)=''
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM terminal
                                        WHERE tool_use_id_type IS NOT 'text' OR TRIM(tool_use_id)=''
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM permission_hooks
                                        WHERE tool_name_type IS NOT 'text'
                                           OR command_type IS NOT 'text'
                                           OR cwd_type IS NOT 'text'
                                           OR TRIM(tool_name)=''
                                           OR TRIM(command)=''
                                           OR TRIM(payload_cwd)=''
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM child_start
                                        WHERE agent_id_type IS NOT 'text' OR TRIM(agent_id)=''
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM child_stop
                                        WHERE agent_id_type IS NOT 'text' OR TRIM(agent_id)=''
                                    )
                                    AND NOT EXISTS (
                                        SELECT tool_use_id FROM pre
                                        GROUP BY tool_use_id HAVING COUNT(*)<>1
                                    )
                                    AND NOT EXISTS (
                                        SELECT tool_use_id FROM terminal
                                        GROUP BY tool_use_id HAVING COUNT(*)<>1
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM terminal finished
                                        WHERE NOT EXISTS (
                                            SELECT 1 FROM pre started
                                            WHERE started.tool_use_id=finished.tool_use_id
                                              AND started.rowid<finished.rowid
                                        )
                                    )
                                    AND NOT EXISTS (
                                        SELECT agent_id FROM child_start
                                        GROUP BY agent_id HAVING COUNT(*)<>1
                                    )
                                    AND NOT EXISTS (
                                        SELECT agent_id FROM child_stop
                                        GROUP BY agent_id HAVING COUNT(*)<>1
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM child_stop finished
                                        WHERE NOT EXISTS (
                                            SELECT 1 FROM child_start started
                                            WHERE started.agent_id=finished.agent_id
                                              AND started.rowid<finished.rowid
                                        )
                                    )
                                    AND NOT EXISTS (
                                        SELECT 1 FROM child_start started
                                        WHERE NOT EXISTS (
                                            SELECT 1 FROM child_stop finished
                                            WHERE finished.agent_id=started.agent_id
                                              AND finished.rowid>started.rowid
                                        )
                                    ) AS valid
                            ),
                            candidate_matches AS (
                                SELECT candidate.tool_name,candidate.command,candidate.payload_cwd,
                                       candidate.payload_json AS pre_payload,
                                       permission.payload_json AS permission_payload,
                                       permission.received_at AS permission_received_at,
                                       stop_event.received_at AS stop_received_at
                                FROM unmatched candidate
                                JOIN permission_hooks permission
                                  ON permission.rowid>candidate.rowid
                                  AND permission.tool_name=candidate.tool_name
                                  AND permission.command=candidate.command
                                  AND permission.payload_cwd=candidate.payload_cwd
                                CROSS JOIN stop_event
                                WHERE NOT EXISTS (
                                    SELECT 1 FROM pre other
                                    WHERE other.rowid<>candidate.rowid
                                      AND other.command=candidate.command
                                )
                                  AND candidate.tool_name_type='text'
                                  AND candidate.command_type='text'
                                  AND candidate.cwd_type='text'
                                  AND TRIM(candidate.tool_name)!=''
                                  AND TRIM(candidate.command)!=''
                                  AND TRIM(candidate.payload_cwd)!=''
                                  AND 1=(
                                    SELECT COUNT(*) FROM permission_hooks matching
                                    WHERE matching.rowid>candidate.rowid
                                      AND matching.tool_name=candidate.tool_name
                                      AND matching.command=candidate.command
                                      AND matching.payload_cwd=candidate.payload_cwd
                                  )
                            )
                            SELECT (SELECT COUNT(*) FROM unmatched),
                                   tool_name,command,payload_cwd,pre_payload,permission_payload,
                                   permission_received_at,stop_received_at
                            FROM candidate_matches CROSS JOIN turn_integrity
                            WHERE turn_integrity.valid
                            UNION ALL
                            SELECT 0,NULL,NULL,NULL,NULL,NULL,NULL,NULL
                            FROM turn_integrity
                            WHERE turn_integrity.valid
                              AND NOT EXISTS (SELECT 1 FROM unmatched)
                            "#,
                        )?;
                        let matches = statement
                            .query_map(
                                params![
                                    context.session_id,
                                    context.role_generation_id,
                                    submit,
                                    hook_rowid,
                                ],
                                |row| {
                                    Ok((
                                        row.get::<_, i64>(0)?,
                                        row.get::<_, Option<String>>(1)?,
                                        row.get::<_, Option<String>>(2)?,
                                        row.get::<_, Option<String>>(3)?,
                                        row.get::<_, Option<String>>(4)?,
                                        row.get::<_, Option<String>>(5)?,
                                        row.get::<_, Option<String>>(6)?,
                                        row.get::<_, Option<String>>(7)?,
                                    ))
                                },
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        drop(statement);
                        let Some((unmatched_count, _, _, _, _, _, _, _)) = matches.first() else {
                            break 'codex false;
                        };
                        let unmatched_count = *unmatched_count;
                        if matches
                            .iter()
                            .any(|(count, _, _, _, _, _, _, _)| *count != unmatched_count)
                        {
                            break 'codex false;
                        }
                        if unmatched_count == 0 {
                            break 'codex matches.len() == 1
                                && matches[0].1.is_none()
                                && matches[0].2.is_none()
                                && matches[0].3.is_none()
                                && matches[0].4.is_none()
                                && matches[0].5.is_none()
                                && matches[0].6.is_none()
                                && matches[0].7.is_none();
                        }
                        if unmatched_count != matches.len() as i64 {
                            break 'codex false;
                        }
                        let mut all_denied = true;
                        for (
                            _,
                            tool_name,
                            command,
                            cwd,
                            pre_payload,
                            permission_payload,
                            permission_received_at,
                            stop_received_at,
                        ) in matches
                        {
                            let (
                                Some(tool_name),
                                Some(command),
                                Some(cwd),
                                Some(pre_payload),
                                Some(permission_payload),
                                Some(permission_received_at),
                                Some(stop_received_at),
                            ) = (
                                tool_name,
                                command,
                                cwd,
                                pre_payload,
                                permission_payload,
                                permission_received_at,
                                stop_received_at,
                            )
                            else {
                                all_denied = false;
                                break;
                            };
                            let (Ok(pre_payload), Ok(permission_payload)) = (
                                serde_json::from_str::<serde_json::Value>(&pre_payload),
                                serde_json::from_str::<serde_json::Value>(&permission_payload),
                            ) else {
                                all_denied = false;
                                break;
                            };
                            if crate::permissions::command_text(
                                context.provider,
                                &tool_name,
                                pre_payload
                                    .get("tool_input")
                                    .unwrap_or(&serde_json::Value::Null),
                            )
                            .as_deref()
                                != Some(&command)
                                || crate::permissions::command_text(
                                    context.provider,
                                    &tool_name,
                                    permission_payload
                                        .get("tool_input")
                                        .unwrap_or(&serde_json::Value::Null),
                                )
                                .as_deref()
                                    != Some(&command)
                            {
                                all_denied = false;
                                break;
                            }
                            let Ok(input) = serde_json::to_vec(
                                permission_payload
                                    .get("tool_input")
                                    .unwrap_or(&serde_json::Value::Null),
                            ) else {
                                all_denied = false;
                                break;
                            };
                            let digest = hex::encode(Sha256::digest(input));
                            let precise_rfc3339 = |timestamp: &str| {
                                let (_, fraction_and_offset) = timestamp.rsplit_once('.')?;
                                let fraction = fraction_and_offset
                                    .strip_suffix('Z')
                                    .or_else(|| {
                                        fraction_and_offset
                                            .split_once('+')
                                            .map(|(fraction, _)| fraction)
                                    })
                                    .or_else(|| {
                                        fraction_and_offset
                                            .rsplit_once('-')
                                            .map(|(fraction, _)| fraction)
                                    })?;
                                if fraction.is_empty()
                                    || fraction.len() > 9
                                    || !fraction.bytes().all(|byte| byte.is_ascii_digit())
                                {
                                    return None;
                                }
                                chrono::DateTime::parse_from_rfc3339(timestamp).ok()
                            };
                            let (Some(permission_hook_at), Some(stop_at)) = (
                                precise_rfc3339(&permission_received_at),
                                precise_rfc3339(&stop_received_at),
                            ) else {
                                all_denied = false;
                                break;
                            };
                            if permission_hook_at > stop_at {
                                all_denied = false;
                                break;
                            }
                            let mut statement = transaction.prepare(
                                "SELECT state,decision_kind,decision_actor,delivery_state,
                                        created_at,decided_at,delivered_at,consumed_at
                                 FROM permission_requests WHERE provider='codex' AND session_id=?1 AND role_generation_id=?2
                                   AND native_session_id=(SELECT native_session_id FROM sessions WHERE id=?1)
                                   AND policy_fingerprint=(SELECT capability_key FROM sessions WHERE id=?1)
                                   AND cwd=?3 AND tool_name=?4 AND command_display=?5 AND input_digest=?6",
                            )?;
                            let requests = statement
                                .query_map(
                                    params![
                                        context.session_id,
                                        context.role_generation_id,
                                        cwd,
                                        tool_name,
                                        command,
                                        digest
                                    ],
                                    |row| {
                                        Ok((
                                            row.get::<_, String>(0)?,
                                            row.get::<_, Option<String>>(1)?,
                                            row.get::<_, Option<String>>(2)?,
                                            row.get::<_, Option<String>>(3)?,
                                            row.get::<_, Option<String>>(4)?,
                                            row.get::<_, Option<String>>(5)?,
                                            row.get::<_, Option<String>>(6)?,
                                            row.get::<_, Option<String>>(7)?,
                                        ))
                                    },
                                )?
                                .collect::<rusqlite::Result<Vec<_>>>()?;
                            drop(statement);
                            let mut qualifying_requests = 0;
                            for (
                                state,
                                decision_kind,
                                decision_actor,
                                delivery_state,
                                created_at,
                                decided_at,
                                delivered_at,
                                consumed_at,
                            ) in requests
                            {
                                let Some(created_at) = created_at else {
                                    all_denied = false;
                                    break;
                                };
                                let Some(created_at) = precise_rfc3339(&created_at) else {
                                    all_denied = false;
                                    break;
                                };
                                if created_at < permission_hook_at || created_at > stop_at {
                                    continue;
                                }
                                let (
                                    Some(decision_kind),
                                    Some(decision_actor),
                                    Some(delivery_state),
                                    Some(decided_at),
                                    Some(delivered_at),
                                    Some(consumed_at),
                                ) = (
                                    decision_kind,
                                    decision_actor,
                                    delivery_state,
                                    decided_at,
                                    delivered_at,
                                    consumed_at,
                                )
                                else {
                                    all_denied = false;
                                    break;
                                };
                                let (Some(decided_at), Some(delivered_at), Some(consumed_at)) = (
                                    precise_rfc3339(&decided_at),
                                    precise_rfc3339(&delivered_at),
                                    precise_rfc3339(&consumed_at),
                                ) else {
                                    all_denied = false;
                                    break;
                                };
                                qualifying_requests += 1;
                                if state != "denied"
                                    || decision_kind != "deny"
                                    || decision_actor != "authenticated_human"
                                    || delivery_state != "delivered"
                                    || created_at > decided_at
                                    || decided_at > consumed_at
                                    || consumed_at > delivered_at
                                    || delivered_at > stop_at
                                {
                                    all_denied = false;
                                    break;
                                }
                            }
                            if qualifying_requests != 1 {
                                all_denied = false;
                                break;
                            }
                        }
                        all_denied
                    }
                }
            } else {
                false
            }
        } else {
            false
        };
        let current_invocation_submit =
            event_name == "UserPromptSubmit" && current_invocation_start_rowid.is_some();
        if current_invocation_submit {
            let submitted_text = envelope
                .payload
                .get("prompt")
                .or_else(|| envelope.payload.get("user_prompt"))
                .or_else(|| envelope.payload.get("input"))
                .or_else(|| envelope.payload.get("message"))
                .and_then(|value| value.as_str());
            if let Some(text) = submitted_text {
                let mut statement = transaction.prepare(
                    "SELECT g.id,g.body
                     FROM guidance_messages g JOIN sessions s ON s.id=?2
                     WHERE g.role_generation_id=?1 AND g.state='written_awaiting_submit'
                       AND g.delivery_session_id=s.id
                       AND g.delivery_transcript_epoch=s.transcript_epoch
                       AND g.delivery_resume_invocation_id IS (
                         SELECT ri.id FROM resume_invocations ri
                         WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                         ORDER BY ri.resume_ordinal DESC LIMIT 1)
                     ORDER BY g.rowid DESC LIMIT 101",
                )?;
                let pending = statement
                    .query_map(
                        params![context.role_generation_id, context.session_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                let matching = pending
                    .iter()
                    .filter(|(_, body)| body.trim() == text.trim())
                    .collect::<Vec<_>>();
                // Bound comparison work and fail closed when more than one current
                // delivery has the same complete edge-normalized body.
                if pending.len() <= 100 && matching.len() == 1 {
                    transaction.execute(
                        "UPDATE guidance_messages
                         SET state='submitted',reason='matched_native_user_prompt_submit',submitted_at=?1
                         WHERE id=?2 AND role_generation_id=?3 AND state='written_awaiting_submit'
                           AND delivery_session_id=?4 AND delivery_transcript_epoch=(
                             SELECT transcript_epoch FROM sessions WHERE id=?4)
                           AND delivery_resume_invocation_id IS (
                             SELECT ri.id FROM resume_invocations ri JOIN sessions s ON s.id=?4
                             WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                             ORDER BY ri.resume_ordinal DESC LIMIT 1)",
                        params![
                            now,
                            matching[0].0.as_str(),
                            context.role_generation_id,
                            context.session_id
                        ],
                    )?;
                }
            }
        }
        let readiness = match event_name.as_str() {
            "SessionStart" => Some("unknown"),
            "Stop" if safe_idle_boundary => Some("idle_candidate"),
            "Stop" => Some("busy_unresolved_hook_work"),
            "UserPromptSubmit" | "PreToolUse" | "PermissionRequest" | "PermissionDenied"
            | "PostToolUse" | "PostToolUseFailure" | "StopFailure" | "Interrupt"
            | "SubagentStart" => Some("busy"),
            "SessionEnd" => Some("ended"),
            _ => None,
        };
        if let Some(readiness) = readiness {
            transaction.execute(
                "UPDATE sessions SET readiness_state=?1,updated_at=?2 WHERE id=?3",
                params![readiness, now, context.session_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO audit_events(id, operation_id, actor_kind, actor_id, event_code, entity_kind, entity_id, detail_json, created_at)
             VALUES(?1, ?2, 'hook', ?3, 'provider.hook.received', 'session', ?4, ?5, ?6)",
            params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), context.role_generation_id, context.session_id, serde_json::json!({"event_name": event_name}).to_string(), now],
        )?;
        transaction.commit()?;
        Ok(serde_json::json!({
            "recorded": true, "session_id": context.session_id, "event_name": event_name,
            "native_identity_candidate": identity_eligible,
            "native_identity_authoritative": false,
            "provenance": provenance.state.as_str(), "safe_idle_boundary": safe_idle_boundary
        }))
    }

    pub fn save_transition_proposal(
        &self,
        context: &RoleContext,
        operation_id: &str,
        phase: &str,
        evidence: &[String],
    ) -> Result<serde_json::Value> {
        if context.role != RoleKind::Manager
            || !context
                .permissions
                .iter()
                .any(|value| value == "request_next_role")
        {
            bail!("only the current manager generation can propose workflow transitions")
        }
        if operation_id.trim().is_empty()
            || ![
                "plan_review",
                "implementation",
                "code_review",
                "checks",
                "final_review",
                "human_review",
            ]
            .contains(&phase)
        {
            bail!("transition proposal operation and supported phase are required")
        }
        if evidence.is_empty() || evidence.iter().any(|item| item.trim().is_empty()) {
            bail!("transition proposals require non-empty structured evidence")
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current_phase, plan_hash, candidate_hash, task_version): (String, Option<String>, Option<String>, i64) =
            transaction.query_row(
                "SELECT a.phase,a.plan_hash,a.candidate_hash,t.version FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
                params![context.attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let input = serde_json::json!({"phase": phase, "evidence": evidence,"source_phase":current_phase,"plan_hash":plan_hash,"candidate_hash":candidate_hash,"role_generation_id":context.role_generation_id,"expected_task_version":task_version});
        let request_hash = json_hash(&input)?;
        if let Some((stored_hash, result)) = transaction.query_row(
            "SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key=?2 AND operation_kind='transition_proposal'",
            params![operation_id, context.role_generation_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)),
        ).optional()? {
            if stored_hash != request_hash { bail!("operation ID was already used with different input") }
            return Ok(serde_json::from_str(&result)?)
        }
        let current: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_generations rg JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager' WHERE rg.id=?1 AND rg.attempt_id=?2 AND rg.status='running')",
            params![context.role_generation_id, context.attempt_id], |row| row.get(0),
        )?;
        if !current {
            bail!("manager generation is stale")
        }
        let id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO controls(id,attempt_id,role_generation_id,kind,state,expected_version,payload_json,created_at,updated_at)
             VALUES(?1,?2,?3,'transition_proposal','proposed',?4,?5,?6,?6)",
            params![id, context.attempt_id, context.role_generation_id, task_version, input.to_string(), now],
        )?;
        let result = serde_json::json!({"proposal_id":id,"phase":phase,"state":"proposed","authoritative_transition":false});
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,?2,'transition_proposal',?3,?4,?5)",
            params![operation_id, context.role_generation_id, request_hash, result.to_string(), now],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn acknowledge_guidance(
        &self,
        context: &RoleContext,
        guidance_id: &str,
    ) -> Result<serde_json::Value> {
        if context.role != RoleKind::Manager {
            bail!("only the current manager can acknowledge submitted guidance")
        }
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE guidance_messages SET state='acknowledged',acknowledged_at=?1,reason=NULL
             WHERE id=?2 AND role_generation_id=?3 AND state='submitted'",
            params![now, guidance_id, context.role_generation_id],
        )?;
        if changed != 1 {
            bail!("guidance is unknown, lacks a matching native submit, or belongs to another role generation")
        }
        Ok(serde_json::json!({"guidance_id":guidance_id,"state":"acknowledged"}))
    }

    pub fn mark_session_spawning(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        boot_identity: &str,
    ) -> Result<()> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE sessions SET launch_state='spawning',launch_boot_identity=?1,updated_at=?2
             WHERE id=?3 AND transcript_epoch=?4 AND status='launch_reserved' AND launch_state='reserved'",
            params![boot_identity, Utc::now().to_rfc3339(), session_id, transcript_epoch],
        )?;
        if changed != 1 {
            bail!("session launch reservation is stale before native spawn")
        }
        connection.execute(
            "UPDATE resume_invocations SET state='spawning',updated_at=?1
             WHERE session_id=?2 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?2) AND state='reserved'",
            params![Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    pub fn session_is_spawning_for_attachment(
        &self,
        session_id: &str,
        role_generation_id: &str,
        boot_identity: &str,
    ) -> Result<bool> {
        let connection = self.lock()?;
        Ok(connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s
               JOIN role_generations rg ON rg.id=s.role_generation_id
               WHERE s.id=?1 AND s.role_generation_id=?2
                 AND s.status='launch_reserved' AND s.launch_state='spawning'
                 AND s.launch_boot_identity=?3 AND rg.status='launch_reserved')",
            params![session_id, role_generation_id, boot_identity],
            |row| row.get(0),
        )?)
    }

    pub fn update_session_running(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        process_json: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE sessions SET status='running',launch_state='started',launch_error=NULL,
                    process_identity_json=?1,updated_at=?2
             WHERE id=?3 AND transcript_epoch=?4 AND status='launch_reserved' AND launch_state='spawning'
               AND EXISTS(
                 SELECT 1 FROM role_generations rg
                 WHERE rg.id=(SELECT role_generation_id FROM sessions WHERE id=?3)
                   AND rg.status='launch_reserved'
                   AND NOT (
                     rg.role='manager' AND EXISTS(
                       SELECT 1 FROM controls c
                       WHERE c.attempt_id=rg.attempt_id
                         AND c.kind='setup_manager_change' AND c.state='held'
                     )
                   )
                   AND NOT EXISTS(
                     SELECT 1 FROM controls c
                     WHERE c.attempt_id=rg.attempt_id
                       AND c.kind IN ('manager_stop','manager_change')
                       AND c.state NOT IN ('finished','cancelled','superseded','rejected')
                       AND NOT (
                         c.kind='manager_change' AND c.state='switching'
                         AND json_extract(c.payload_json,'$.switch_intent_id')=(
                           SELECT id FROM switch_intents si WHERE si.new_generation_id=rg.id
                         )
                       )
                   )
               )",
            params![process_json, now, session_id, transcript_epoch],
        )?;
        if changed != 1 {
            bail!("session launch reservation is stale")
        }
        connection.execute(
            "UPDATE resume_invocations SET state='running',process_identity_json=?1,updated_at=?2
             WHERE session_id=?3 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?3) AND state='spawning'",
            params![process_json, now, session_id],
        )?;
        connection.execute(
            "UPDATE role_generations SET status = 'running', updated_at = ?1 WHERE id = (SELECT role_generation_id FROM sessions WHERE id = ?2)",
            params![now, session_id],
        )?;
        let (role, lane_id): (String, String) = connection.query_row(
            "SELECT rg.role,rg.lane_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1",
            params![session_id], |row| Ok((row.get(0)?,row.get(1)?))
        )?;
        if role == "implementer" && lane_id != "default" {
            let changed = connection.execute(
                "UPDATE lane_generations SET effective_generation_id=(SELECT role_generation_id FROM sessions WHERE id=?1),updated_at=?2
                 WHERE lane_id=?3",
                params![session_id,now,lane_id],
            )?;
            if changed != 1 {
                bail!("implementation lane generation mapping is missing")
            }
            let changed = connection.execute(
                "UPDATE implementation_lanes SET state='active',updated_at=?1 WHERE id=?2 AND state IN ('admitted','active')",
                params![now,lane_id],
            )?;
            if changed != 1 {
                bail!("implementation lane is no longer admitted for launch")
            }
        } else {
            connection.execute(
                "UPDATE role_settings SET effective_generation_id=(SELECT role_generation_id FROM sessions WHERE id=?1)
                 WHERE task_id=(SELECT a.task_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE s.id=?1)
                   AND role=(SELECT rg.role FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1)
                   AND revision=(SELECT rg.config_revision FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1)
                   AND NOT EXISTS(SELECT 1 FROM switch_intents si JOIN sessions s ON s.id=?1 JOIN role_generations rg ON rg.id=s.role_generation_id
                                  WHERE si.attempt_id=rg.attempt_id AND si.role=rg.role AND si.requested_settings_revision=rg.config_revision
                                    AND si.state IN ('ready_for_dispatch','stopping_old'))",
                params![session_id],
            )?;
        }
        let switched:Option<(String,String,String,i64,String)>=connection.query_row(
            "SELECT si.id,si.attempt_id,si.role,si.requested_settings_revision,s.launch_config_json FROM switch_intents si JOIN sessions s ON s.id=?1 JOIN role_generations rg ON rg.id=s.role_generation_id WHERE si.new_generation_id=rg.id AND si.state='dispatched'",
            params![session_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))
        ).optional()?;
        if let Some((switch_intent, attempt, role, revision, launch_json)) = switched {
            let task: String = connection.query_row(
                "SELECT task_id FROM attempts WHERE id=?1",
                params![attempt],
                |row| row.get(0),
            )?;
            let role: RoleKind = role.parse().map_err(|error: String| anyhow!(error))?;
            let launch: LaunchConfig = serde_json::from_str(&launch_json)?;
            let authority = crate::trip::current_task_profile_authority_with_bundles(
                &connection,
                &task,
                role,
                revision,
                &launch,
                &self.compatibility_bundles,
            )?;
            crate::trip::replace_attempt_profile(&connection, &attempt, &authority, &now)?;
            if role == RoleKind::Manager {
                connection.execute(
                    "UPDATE controls SET state='finished',updated_at=?1
                     WHERE attempt_id=?2 AND kind='manager_change' AND state='switching'
                       AND json_extract(payload_json,'$.switch_intent_id')=?3",
                    params![now, attempt, switch_intent],
                )?;
            }
        }
        Ok(())
    }

    pub fn record_session_process(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        invocation_process_json: &str,
        pid: u32,
        start: &str,
        pgid: i32,
        ppid: u32,
    ) -> Result<()> {
        let connection = self.lock()?;
        connection.execute("INSERT INTO session_processes(session_id,pid,native_start_marker,process_group_id,parent_pid,last_seen_at)
             SELECT ?1,?2,?3,?4,?5,?6 WHERE EXISTS(
               SELECT 1 FROM sessions WHERE id=?1 AND transcript_epoch=?7
                 AND (status='launch_reserved' OR process_identity_json=?8))
             ON CONFLICT(session_id,pid,native_start_marker) DO UPDATE SET process_group_id=excluded.process_group_id,parent_pid=excluded.parent_pid,last_seen_at=excluded.last_seen_at",params![session_id,pid as i64,start,pgid,ppid as i64,Utc::now().to_rfc3339(),transcript_epoch,invocation_process_json])?;
        Ok(())
    }

    pub fn record_session_anchor(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        anchor: &crate::domain::ProcessGenerationAnchor,
    ) -> Result<()> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE sessions SET recovery_root_pid=?1,recovery_process_group_id=?2,recovery_anchor_json=?3,launch_boot_identity=?4,updated_at=?5 WHERE id=?6 AND transcript_epoch=?7 AND status='launch_reserved'",
            params![i64::from(anchor.pid),anchor.process_group_id,serde_json::to_string(anchor)?,anchor.boot_identity,Utc::now().to_rfc3339(),session_id,transcript_epoch],
        )?;
        if changed != 1 {
            bail!("session disappeared before its generation anchor was durable")
        }
        Ok(())
    }

    pub fn record_session_spawn_uncertainty(
        &self,
        session_id: &str,
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
        members: &[ObservedProcessIdentity],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE sessions SET recovery_root_pid=?1,recovery_process_group_id=?2,updated_at=?3 WHERE id=?4",
            params![root_pid.map(i64::from), process_group_id, now, session_id],
        )?;
        for member in members {
            transaction.execute(
                "INSERT INTO session_processes(session_id,pid,native_start_marker,process_group_id,parent_pid,last_seen_at)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(session_id,pid,native_start_marker) DO UPDATE SET
                   process_group_id=excluded.process_group_id,parent_pid=excluded.parent_pid,last_seen_at=excluded.last_seen_at",
                params![
                    session_id,
                    i64::from(member.pid),
                    member.native_start_marker,
                    member.process_group_id,
                    i64::from(member.parent_pid),
                    now
                ],
            )?;
        }
        let (anchor, boot): (Option<String>, Option<String>) = transaction.query_row(
            "SELECT recovery_anchor_json,launch_boot_identity FROM sessions WHERE id=?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        transaction.execute(
            "UPDATE resume_invocations SET process_identity_json=?1,updated_at=?2
             WHERE session_id=?3 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?3)
               AND state IN ('reserved','spawning')",
            params![
                serde_json::json!({
                    "provider_spawned":false,
                    "wrapper_anchor":anchor.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok()),
                    "launch_boot_identity":boot,
                    "root_pid":root_pid,
                    "process_group_id":process_group_id,
                    "observed_members":members
                }).to_string(),
                now,
                session_id
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn update_session_launch_failed(&self, session_id: &str, reason: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let connection = self.lock()?;
        connection.execute(
            "UPDATE sessions SET status='launch_failed',launch_state='failed',launch_error=?1,
                    exit_json=?2,updated_at=?3 WHERE id=?4",
            params![
                reason,
                serde_json::json!({"reason": reason}).to_string(),
                now,
                session_id
            ],
        )?;
        connection.execute(
            "UPDATE resume_invocations SET state='proven_nondelivery',error=?1,updated_at=?2
             WHERE session_id=?3 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?3)
               AND state IN ('reserved','spawning')",
            params![reason, now, session_id],
        )?;
        connection.execute("UPDATE role_generations SET status='launch_failed',updated_at=?1 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",params![now,session_id])?;
        Ok(())
    }

    pub fn restore_resume_after_proven_nondelivery(
        &self,
        session_id: &str,
        reason: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<(Option<String>,Option<String>,Option<String>,Option<String>,Option<i64>,Option<i32>)> = transaction.query_row(
            "SELECT prior_exit_json,prior_transcript_epoch,prior_launch_boot_identity,prior_recovery_anchor_json,prior_recovery_root_pid,prior_recovery_process_group_id FROM resume_invocations
             WHERE session_id=?1 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?1)
               AND state IN ('reserved','spawning')",
            params![session_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        ).optional()?;
        let (prior_exit, prior_epoch, prior_boot, prior_anchor, prior_root, prior_group) =
            prior.ok_or_else(|| anyhow!("session has no current resumable invocation history"))?;
        let prior_epoch = prior_epoch.ok_or_else(|| {
            anyhow!("resume history predates preservation of its prior transcript epoch")
        })?;
        let changed = transaction.execute(
            "UPDATE resume_invocations SET state='proven_nondelivery',error=?1,updated_at=?2
             WHERE session_id=?3 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?3)
               AND state IN ('reserved','spawning')",
            params![reason, now, session_id],
        )?;
        if changed != 1 {
            bail!("session has no current resumable invocation failure to restore")
        }
        transaction.execute(
            "UPDATE sessions SET status='exited',launch_state='finished',launch_error=?1,
                    exit_json=?2,transcript_epoch=?3,launch_boot_identity=?4,recovery_anchor_json=?5,recovery_root_pid=?6,
                    recovery_process_group_id=?7,readiness_state='unknown',updated_at=?8 WHERE id=?9",
            params![format!("resume proven not delivered: {reason}"),prior_exit,prior_epoch,prior_boot,prior_anchor,prior_root,prior_group,now,session_id],
        )?;
        transaction.execute(
            "UPDATE role_generations SET status='exited',updated_at=?1
             WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",
            params![now, session_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn hold_session_after_proven_nondelivery(
        &self,
        session_id: &str,
        reason: &str,
        resumed: bool,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (attempt, task): (String, String) = transaction.query_row(
            "SELECT rg.attempt_id,a.task_id FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id WHERE s.id=?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        transaction.execute(
            "UPDATE sessions SET status='recovery_required',launch_state='provider_nondelivery_cleanup_unknown',
                    launch_error=?1,readiness_state='unknown',updated_at=?2 WHERE id=?3",
            params![reason, now, session_id],
        )?;
        transaction.execute(
            "UPDATE resume_invocations SET state='proven_nondelivery_cleanup_unknown',error=?1,updated_at=?2
             WHERE session_id=?3 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?3)
               AND state IN ('reserved','spawning')",
            params![reason, now, session_id],
        )?;
        transaction.execute(
            "UPDATE role_generations SET status='recovery_required',updated_at=?1
             WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",
            params![now, session_id],
        )?;
        transaction.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        transaction.execute(
            "UPDATE tasks SET version=version+CASE WHEN attention='needs_recovery' THEN 0 ELSE 1 END,
                    attention='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, task],
        )?;
        transaction.execute(
            "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
            params![now, attempt],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            session_id,
            "owned wrapper cleanup remained uncertain after proven provider nondelivery",
        )?;
        let (anchor, boot, root_pid, process_group_id): (
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i32>,
        ) = transaction.query_row(
            "SELECT recovery_anchor_json,launch_boot_identity,recovery_root_pid,recovery_process_group_id
             FROM sessions WHERE id=?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let members = {
            let mut statement = transaction.prepare(
                "SELECT pid,parent_pid,process_group_id,native_start_marker FROM session_processes
                 WHERE session_id=?1 ORDER BY pid,native_start_marker",
            )?;
            let rows = statement
                .query_map(params![session_id], |row| {
                    Ok(serde_json::json!({
                        "pid":row.get::<_,i64>(0)?,
                        "parent_pid":row.get::<_,i64>(1)?,
                        "process_group_id":row.get::<_,i32>(2)?,
                        "native_start_marker":row.get::<_,String>(3)?
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let fallback_process_evidence = serde_json::json!({
            "provider_spawned":false,
            "wrapper_anchor":anchor.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok()),
            "launch_boot_identity":boot,
            "root_pid":root_pid,
            "process_group_id":process_group_id,
            "observed_members":members
        });
        let failed_process_evidence = transaction
            .query_row(
                "SELECT process_identity_json FROM resume_invocations
                 WHERE session_id=?1 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?1)
                 ORDER BY resume_ordinal DESC LIMIT 1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or(fallback_process_evidence);
        let detail = serde_json::json!({
            "kind":"provider_nondelivery_cleanup_unknown",
            "provider_delivery":"proven_nondelivery",
            "owned_process_state":"uncertain",
            "reason":reason,
            "resume_invocation":resumed,
            "additional_review_round_spent":false,
            "exact_retry_allowed_after_positive_quiescence":resumed,
            "fresh_replacement_allowed_after_positive_quiescence":true
        });
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT id,detail_json FROM recovery_records
                 WHERE session_id=?1 AND state='attention_required' ORDER BY created_at LIMIT 1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((record, prior)) = existing {
            let mut merged = serde_json::from_str::<serde_json::Value>(&prior)
                .unwrap_or_else(|_| serde_json::json!({}));
            if let (Some(target), Some(source)) = (merged.as_object_mut(), detail.as_object()) {
                target.extend(source.clone());
            } else {
                merged = detail;
            }
            transaction.execute(
                "UPDATE recovery_records SET detail_json=?1,process_identity_json=?2,updated_at=?3 WHERE id=?4",
                params![merged.to_string(), failed_process_evidence.to_string(), now, record],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
                 SELECT ?1,s.id,?2,'attention_required',?3,?4,?5,?5
                 FROM sessions s WHERE s.id=?6",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    attempt,
                    failed_process_evidence.to_string(),
                    detail.to_string(),
                    now,
                    session_id
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn release_restart_hold_for_fresh_dispatch(
        &self,
        attempt_id: &str,
        source: &str,
    ) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let released = Self::release_restart_hold_for_fresh_dispatch_in(
            &transaction,
            attempt_id,
            source,
            &now,
        )?;
        transaction.commit()?;
        Ok(released)
    }

    pub(crate) fn release_restart_hold_for_fresh_dispatch_in(
        transaction: &Transaction<'_>,
        attempt_id: &str,
        source: &str,
        now: &str,
    ) -> Result<usize> {
        if !matches!(source, "human_continue" | "human_recovery") {
            bail!("restart holds can be released only by an explicit human continue or recovery action")
        }
        let candidates = {
            let mut statement = transaction.prepare(
                "SELECT rc.session_id,s.status,COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0),rc.state,rc.result_json
                 FROM restart_candidates rc JOIN sessions s ON s.id=rc.session_id
                 WHERE rc.attempt_id=?1 AND rc.state NOT IN ('resumed','released_fresh_dispatch','cancelled')",
            )?;
            let rows = statement
                .query_map(params![attempt_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if candidates.is_empty() {
            return Ok(0);
        }
        if candidates
            .iter()
            .any(|(_, status, quiescent, _, _)| status != "exited" || !quiescent)
        {
            bail!("restart hold release requires every captured prior-running session to have positive quiescence evidence")
        }
        let unresolved_recovery: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1 AND state='attention_required')",
            params![attempt_id],
            |row| row.get(0),
        )?;
        if unresolved_recovery {
            bail!("restart hold release is blocked by unresolved recovery evidence")
        }
        for (session_id, _, _, state, result_json) in &candidates {
            if !matches!(
                state.as_str(),
                "pending_reconciliation"
                    | "parked"
                    | "queued_capacity"
                    | "failed"
                    | "blocked"
                    | "skipped"
                    | "admitting"
            ) {
                bail!("restart hold has an unsupported durable candidate state")
            }
            let mut result = RestartCandidateResult::parse(result_json)?;
            result.validate_candidate_state(state, session_id)?;
            result.restart.terminalize_batch();
            result.set(
                "fresh_dispatch_release",
                serde_json::json!({"source":source,"native_resume_implied":false}),
            );
            transaction.execute(
                "UPDATE restart_candidates SET state='released_fresh_dispatch',requested_by=?1,
                        reason='explicit human action released the restart hold after positive quiescence; no native resume or fresh launch was implied',
                        result_json=?2,updated_at=?3 WHERE session_id=?4",
                params![source, result.encode()?, now, session_id],
            )?;
            transaction.execute(
                "UPDATE sessions SET desired_running=0 WHERE id=?1",
                params![session_id],
            )?;
            transaction.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?2,'human','restart.hold.released','session',?3,?4,?5)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    session_id,
                    serde_json::json!({
                        "attempt_id":attempt_id,
                        "source":source,
                        "positive_quiescence":true,
                        "native_resume_implied":false,
                        "fresh_dispatch_implied":false
                    }).to_string(),
                    now
                ],
            )?;
        }
        Ok(candidates.len())
    }

    pub fn preserve_proven_nondelivery_recovery_detail(
        &self,
        session_id: &str,
        recovery_id: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source: Option<(String, Option<String>, Option<String>, Option<String>)> = transaction
            .query_row(
                "SELECT s.launch_state,s.launch_error,
                        (SELECT ri.state FROM resume_invocations ri WHERE ri.session_id=s.id ORDER BY ri.resume_ordinal DESC LIMIT 1),
                        (SELECT ri.process_identity_json FROM resume_invocations ri WHERE ri.session_id=s.id ORDER BY ri.resume_ordinal DESC LIMIT 1)
                 FROM sessions s WHERE s.id=?1 AND s.launch_state='provider_nondelivery_cleanup_unknown'",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((launch_state, launch_error, resume_state, invocation_evidence)) = source else {
            transaction.commit()?;
            return Ok(());
        };
        let record: Option<(String, String)> = transaction
            .query_row(
                "SELECT id,detail_json FROM recovery_records WHERE id=?1 AND session_id=?2 AND state='resolved_quiescent'",
                params![recovery_id, session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((record_id, detail)) = record {
            let mut merged = serde_json::from_str::<serde_json::Value>(&detail)
                .unwrap_or_else(|_| serde_json::json!({}));
            if let Some(target) = merged.as_object_mut() {
                target.insert(
                    "provider_delivery".to_owned(),
                    serde_json::Value::String("proven_nondelivery".to_owned()),
                );
                target.insert(
                    "owned_process_state_before_resolution".to_owned(),
                    serde_json::Value::String("uncertain".to_owned()),
                );
                target.insert("launch_state".to_owned(), serde_json::json!(launch_state));
                target.insert("launch_error".to_owned(), serde_json::json!(launch_error));
                target.insert(
                    "resume_invocation_state".to_owned(),
                    serde_json::json!(resume_state),
                );
                target.insert(
                    "failed_invocation_process_evidence".to_owned(),
                    invocation_evidence
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or(serde_json::Value::Null),
                );
                target.insert(
                    "additional_review_round_spent".to_owned(),
                    serde_json::Value::Bool(false),
                );
            }
            transaction.execute(
                "UPDATE recovery_records SET detail_json=?1,updated_at=?2 WHERE id=?3",
                params![merged.to_string(), now, record_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_session_delivery_ambiguous(&self, session_id: &str, reason: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempt: String = transaction.query_row(
            "SELECT rg.attempt_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE sessions SET status='recovery_required',launch_state='delivery_unknown',launch_error=?1,readiness_state='unknown',updated_at=?2 WHERE id=?3",
            params![reason,now,session_id],
        )?;
        transaction.execute(
            "UPDATE resume_invocations SET state='delivery_unknown',error=?1,updated_at=?2
             WHERE session_id=?3 AND transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?3)
               AND state IN ('reserved','spawning','running')",
            params![reason, now, session_id],
        )?;
        transaction.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        transaction.execute("UPDATE tasks SET version=version+CASE WHEN attention='needs_recovery' THEN 0 ELSE 1 END,attention='needs_recovery',updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
        transaction.execute(
            "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
            params![now, attempt],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            session_id,
            "provider session delivery became uncertain",
        )?;
        transaction.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at) SELECT ?1,s.id,rg.attempt_id,'attention_required',s.process_identity_json,?2,?3,?3 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?4 AND NOT EXISTS(SELECT 1 FROM recovery_records WHERE session_id=?4 AND state='attention_required')",
            params![uuid::Uuid::new_v4().to_string(),serde_json::json!({"kind":"provider_delivery_unknown","reason":reason,"replacement_allowed_after_quiescence":true}).to_string(),now,session_id])?;
        transaction.commit()?;
        Ok(())
    }

    fn settle_graceful_stop_recovery_after_quiescence(
        &self,
        transaction: &Transaction<'_>,
        session_id: &str,
        transcript_epoch: &str,
        process_json: &str,
        exit_json: &str,
        now: &str,
    ) -> Result<()> {
        let recovery: Option<(String, String, String, String)> = transaction
            .query_row(
                "SELECT r.id,r.attempt_id,t.id,rg.id
                   FROM recovery_records r
                   JOIN sessions s ON s.id=r.session_id
                   JOIN role_generations rg ON rg.id=s.role_generation_id
                   JOIN attempts a ON a.id=r.attempt_id
                   JOIN tasks t ON t.id=a.task_id
                  WHERE r.session_id=?1 AND r.state='attention_required'
                    AND json_extract(r.detail_json,'$.kind')='graceful_stop_deadline'
                    AND r.process_identity_json=?2 AND s.process_identity_json=?2
                    AND s.transcript_epoch=?3
                    AND json_extract(r.detail_json,'$.role_generation_id')=rg.id
                    AND json_extract(r.detail_json,'$.transcript_epoch')=s.transcript_epoch
                    AND s.status='exited'
                    AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
                  ORDER BY r.created_at DESC LIMIT 1",
                params![session_id, process_json, transcript_epoch],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((recovery_id, attempt_id, task_id, generation_id)) = recovery else {
            return Ok(());
        };
        let other_unresolved_recovery: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records
                            WHERE attempt_id=?1 AND state='attention_required' AND id!=?2)",
            params![attempt_id, recovery_id],
            |row| row.get(0),
        )?;
        let other_unresolved_session_ownership: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sessions other
                JOIN role_generations other_generation ON other_generation.id=other.role_generation_id
                 WHERE other_generation.attempt_id=?1 AND other.id!=?2
                   AND other.status='recovery_required'
            )",
            params![attempt_id, session_id],
            |row| row.get(0),
        )?;
        let other_exact_live_ownership: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sessions other
                JOIN role_generations other_generation ON other_generation.id=other.role_generation_id
                 WHERE other_generation.attempt_id=?1 AND other.id!=?2
                   AND other.status IN ('launch_reserved','running','interrupt_requested')
            )",
            params![attempt_id, session_id],
            |row| row.get(0),
        )?;
        let terminal_restart_disposition: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM restart_candidates
                            WHERE session_id=?1
                              AND state IN ('resumed','released_fresh_dispatch','cancelled'))",
            params![session_id],
            |row| row.get(0),
        )?;
        let pending_emergency_control: Option<String> = transaction
            .query_row(
                "SELECT kind FROM controls
                  WHERE attempt_id=?1 AND kind IN ('pause_now','pause_after_role','cancel')
                    AND state IN ('requested','draining','recovery_required')
                  ORDER BY created_at LIMIT 1",
                params![attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let manager_stop_held: Option<String> = transaction
            .query_row(
                "SELECT id FROM controls
                  WHERE attempt_id=?1 AND kind='manager_stop' AND state='held'
                    AND json_extract(payload_json,'$.manager_session_id')=?2
                  ORDER BY created_at DESC LIMIT 1",
                params![attempt_id, session_id],
                |row| row.get(0),
            )
            .optional()?;
        let switch_recovery: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM switch_intents
                 WHERE attempt_id=?1 AND old_generation_id=?2
                   AND state NOT IN ('completed','cancelled','superseded')
            ) OR EXISTS(
                SELECT 1 FROM controls
                 WHERE attempt_id=?1 AND kind='manager_change'
                   AND state NOT IN ('finished','cancelled','superseded','rejected','failed')
                   AND json_extract(payload_json,'$.old_generation_id')=?2
            )",
            params![attempt_id, generation_id],
            |row| row.get(0),
        )?;
        let disposition = if other_unresolved_recovery
            || other_unresolved_session_ownership
            || terminal_restart_disposition
        {
            "remaining_exact_ownership"
        } else if let Some(kind) = pending_emergency_control.as_deref() {
            transaction.execute(
                "UPDATE controls
                    SET state='requested',
                        payload_json=json_set(payload_json,'$.recovered_quiescent',1,
                          '$.next_action',?1),updated_at=?2
                  WHERE attempt_id=?3 AND kind IN ('pause_now','pause_after_role','cancel')
                    AND state IN ('requested','draining','recovery_required')",
                params![
                    "The exact stopped process is quiescent. Apply this already-requested control without launching or resuming a provider.",
                    now,
                    attempt_id
                ],
            )?;
            transaction.execute(
                "UPDATE claims SET state='running',updated_at=?1
                  WHERE attempt_id=?2 AND state='unknown'",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE attempts SET status='running',updated_at=?1 WHERE id=?2",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention=?1,version=version+CASE WHEN attention=?1 THEN 0 ELSE 1 END,
                        updated_at=?2 WHERE id=?3",
                params![
                    if matches!(kind, "pause_now" | "cancel") {
                        "pause_requested"
                    } else {
                        "none"
                    },
                    now,
                    task_id
                ],
            )?;
            match kind {
                "pause_now" => "pause_now_pending",
                "pause_after_role" => "pause_after_role_pending",
                "cancel" => "cancel_pending",
                _ => "control_pending",
            }
        } else if let Some(control_id) = manager_stop_held {
            transaction.execute(
                "UPDATE controls SET payload_json=json_set(payload_json,'$.quiescent',1,
                         '$.recovered_quiescent',1,'$.native_resume_forbidden',1,
                         '$.resume_fence_generation_id',?2,'$.next_action',?1),updated_at=?3
                  WHERE id=?4 AND kind='manager_stop' AND state='held'",
                params![
                    "Manager is verified stopped. Continue manager releases only this hold; Change manager requires a new current authority decision.",
                    generation_id,
                    now,
                    control_id
                ],
            )?;
            transaction.execute(
                "UPDATE claims SET state='running',updated_at=?1
                  WHERE attempt_id=?2 AND state='unknown'",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE attempts SET status='running',updated_at=?1 WHERE id=?2",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='none',version=version+CASE WHEN attention='none' THEN 0 ELSE 1 END,
                        updated_at=?1 WHERE id=?2",
                params![now, task_id],
            )?;
            "manager_stop_quiescent"
        } else if switch_recovery {
            transaction.execute(
                "UPDATE switch_intents SET state='rejected',
                         handoff_json=json_set(handoff_json,'$.recovery_failure',?1,
                           '$.native_resume_forbidden',1,'$.resume_fence_generation_id',?2),updated_at=?3
                  WHERE attempt_id=?4 AND old_generation_id=?5
                    AND state NOT IN ('completed','cancelled','rejected')",
                params![
                    "The old generation stopped after exact recovery; submit a corrected current switch rather than dispatching from this recovered intent.",
                    generation_id,
                    now,
                    attempt_id,
                    generation_id
                ],
            )?;
            transaction.execute(
                "UPDATE controls SET state='failed',
                         payload_json=json_set(payload_json,'$.failure',?1,'$.next_action',?2,
                           '$.native_resume_forbidden',1,'$.resume_fence_generation_id',?3),updated_at=?4
                  WHERE attempt_id=?5 AND kind='manager_change'
                    AND state NOT IN ('finished','cancelled','superseded','rejected','failed')
                    AND json_extract(payload_json,'$.old_generation_id')=?6",
                params![
                    "The old manager stopped after exact recovery.",
                    "Submit a corrected manager change with the current task version; this recovery never dispatches a replacement automatically.",
                    generation_id,
                    now,
                    attempt_id,
                    generation_id
                ],
            )?;
            transaction.execute(
                "UPDATE claims SET state='running',updated_at=?1
                  WHERE attempt_id=?2 AND state='unknown'",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE attempts SET status='running',updated_at=?1 WHERE id=?2",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='none',version=version+CASE WHEN attention='none' THEN 0 ELSE 1 END,
                        updated_at=?1 WHERE id=?2",
                params![now, task_id],
            )?;
            "switch_rejected_after_quiescence"
        } else {
            transaction.execute(
                "UPDATE claims SET state='running',updated_at=?1
                  WHERE attempt_id=?2 AND state='unknown'",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE attempts SET status='restart_parked',updated_at=?1 WHERE id=?2",
                params![now, attempt_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention='restart_parked',
                         version=version+CASE WHEN attention='restart_parked' THEN 0 ELSE 1 END,
                         updated_at=?1 WHERE id=?2",
                params![now, task_id],
            )?;
            transaction.execute(
                "UPDATE sessions SET desired_running=0 WHERE id=?1",
                params![session_id],
            )?;
            let prior_candidate: Option<(String, String)> = transaction
                .query_row(
                    "SELECT state,result_json FROM restart_candidates WHERE session_id=?1",
                    params![session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if prior_candidate.as_ref().is_some_and(|(state, _)| {
                !matches!(
                    state.as_str(),
                    "pending_reconciliation"
                        | "parked"
                        | "queued_capacity"
                        | "failed"
                        | "blocked"
                        | "skipped"
                        | "admitting"
                )
            }) {
                bail!("graceful-stop recovery found an unsupported restart candidate state")
            }
            let mut candidate_result = RestartCandidateResult::parse(
                prior_candidate
                    .as_ref()
                    .map(|(_, result)| result.as_str())
                    .unwrap_or("{}"),
            )?;
            if let Some((state, _)) = prior_candidate.as_ref() {
                candidate_result.validate_candidate_state(state, session_id)?;
            }
            candidate_result.restart.terminalize_batch();
            candidate_result.set(
                "kind",
                serde_json::Value::String("graceful_stop_quiescent".to_owned()),
            );
            candidate_result.set("native_resume_forbidden", serde_json::Value::Bool(true));
            candidate_result.set(
                "fresh_dispatch_requires_explicit_continue",
                serde_json::Value::Bool(true),
            );
            candidate_result.set("automatic_resume", serde_json::Value::Bool(false));
            candidate_result.set("fresh_dispatch_started", serde_json::Value::Bool(false));
            transaction.execute(
                "INSERT INTO restart_candidates(session_id,attempt_id,task_id,source,state,reason,result_json,created_at,updated_at)
                 VALUES(?1,?2,?3,'graceful_stop_recovery','parked',?4,?5,?6,?6)
                 ON CONFLICT(session_id) DO UPDATE SET
                   source=excluded.source,state=excluded.state,reason=excluded.reason,
                   result_json=excluded.result_json,updated_at=excluded.updated_at",
                params![
                    session_id,
                    attempt_id,
                    task_id,
                    "exact graceful-stop recovery reached positive quiescence; native resume remains forbidden until a separate human action releases the fresh-dispatch hold",
                    candidate_result.encode()?,
                    now
                ],
            )?;
            "fresh_dispatch_hold_available"
        };
        let resolution = serde_json::json!({
            "process_group_quiescent":true,
            "session_id":session_id,
            "transcript_epoch":transcript_epoch,
            "process_identity":serde_json::from_str::<serde_json::Value>(process_json)
                .unwrap_or_else(|_| serde_json::json!({"unparseable":process_json})),
            "exit":serde_json::from_str::<serde_json::Value>(exit_json)
                .unwrap_or_else(|_| serde_json::json!({"unparseable":exit_json})),
            "next_disposition":disposition,
            "other_exact_live_ownership":other_exact_live_ownership,
            "other_unresolved_session_ownership":other_unresolved_session_ownership,
            "automatic_resume":false,
            "fresh_dispatch_started":false,
        });
        transaction.execute(
            "UPDATE recovery_records
                SET state='resolved_graceful_stop_quiescent',resolved_at=?1,updated_at=?1,
                    detail_json=json_set(detail_json,'$.quiescence_resolution',json(?2),
                      '$.next_disposition',?3)
              WHERE id=?4 AND state='attention_required'",
            params![now, resolution.to_string(), disposition, recovery_id],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','session.graceful_stop.recovery_quiescent','session',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                session_id,
                serde_json::json!({
                    "recovery_id":recovery_id,
                    "attempt_id":attempt_id,
                    "task_id":task_id,
                    "role_generation_id":generation_id,
                    "positive_quiescence":true,
                    "next_disposition":disposition,
                    "automatic_resume":false,
                    "fresh_dispatch_started":false
                }).to_string(),
                now
            ],
        )?;
        Ok(())
    }

    pub fn update_session_exit(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        process_json: &str,
        exit_json: &str,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE sessions SET status='exited',launch_state='finished',exit_json=?1,
                    interrupt_requested_at=NULL,updated_at=?2
             WHERE id=?3 AND transcript_epoch=?4 AND process_identity_json=?5
               AND status IN ('running','interrupt_requested','recovery_required')",
            params![exit_json, now, session_id, transcript_epoch, process_json],
        )?;
        if changed == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE resume_invocations SET state='exited',updated_at=?1
             WHERE session_id=?2 AND transcript_epoch=?3 AND process_identity_json=?4
               AND state='running'",
            params![now, session_id, transcript_epoch, process_json],
        )?;
        crate::trip::settle_runtime_probe_exit(&transaction, session_id, &now)?;
        transaction.execute(
            "UPDATE role_generations SET status='exited',updated_at=?1 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2) AND status NOT IN ('replaced','revoked')",
            params![now,session_id],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            session_id,
            "native session exited before permission response delivery",
        )?;
        transaction.execute(
            "UPDATE input_leases SET revoked_at=COALESCE(revoked_at,?1),updated_at=?1
             WHERE session_id=?2 AND process_identity_json=?3",
            params![now, session_id, process_json],
        )?;
        self.settle_graceful_stop_recovery_after_quiescence(
            &transaction,
            session_id,
            transcript_epoch,
            process_json,
            exit_json,
            &now,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn reconcile_exited_runtime_probes(&self) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sessions = {
            let mut statement = transaction.prepare(
                "SELECT s.id FROM sessions s
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN trip_runtime_probes probe ON probe.session_id=s.id
                   AND probe.attempt_id=rg.attempt_id AND probe.role=rg.role
                 WHERE s.validation_cell='trip_runtime_probe' AND s.status='exited'
                   AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
                   AND probe.state IN ('running','awaiting_resume')
                 ORDER BY s.updated_at,s.id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut changed = 0;
        for session_id in sessions {
            if crate::trip::settle_runtime_probe_exit(&transaction, &session_id, &now)? {
                changed += 1;
            }
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn session_json(&self, session_id: &str) -> Result<serde_json::Value> {
        let connection = self.lock()?;
        connection.query_row(
            "SELECT s.id, s.status, s.provider, s.validation_cell, s.launch_config_json, s.executable_version,
                    s.native_session_id, s.native_identity_source, s.process_identity_json, s.transcript_epoch,
                    s.transcript_last_sequence, s.capture_state, s.capture_error, s.hook_trust_state,
                    s.exit_json, s.created_at, s.updated_at, s.launch_state, s.launch_error,
                    rg.id, rg.role, rg.generation, rg.config_revision, a.id, t.id, p.id, p.repository_path,
                    s.capability_key,s.capability_identity_json,
                    (SELECT json_object(
                        'id',r.id,'task_workspace_id',r.task_workspace_id,
                        'workspace_id',r.workspace_id,'surface_id',r.surface_id,
                        'binding_revision',r.binding_revision,'surface_state',r.surface_state,
                        'attachment_state',r.attachment_state,
                        'desired_input_state',r.desired_input_state,
                        'actual_input_state',r.actual_input_state,
                        'control_revision',r.control_revision,
                        'applied_revision',r.applied_revision,
                        'last_error',r.last_error,'updated_at',r.updated_at
                     )
                       FROM cmux_session_surfaces r
                      WHERE r.session_id=s.id AND r.role_generation_id=s.role_generation_id
                        AND r.transcript_epoch=s.transcript_epoch
                        AND r.process_identity_json=s.process_identity_json
                      -- A new service boot starts its physical-surface
                      -- generation at one. Prefer the live current
                      -- presentation over any prior-boot audit row whose
                      -- revision happens to be larger, then retain the most
                      -- recent historical row when no live route remains.
                      ORDER BY CASE
                          WHEN r.surface_state IN ('opening','open','unknown')
                           AND r.attachment_state IN ('pending','live') THEN 0
                          ELSE 1
                      END,r.created_at DESC,r.binding_revision DESC LIMIT 1)
             FROM sessions s
             JOIN role_generations rg ON rg.id = s.role_generation_id
             JOIN attempts a ON a.id = rg.attempt_id JOIN tasks t ON t.id = a.task_id JOIN projects p ON p.id = t.project_id
             WHERE s.id = ?1",
            params![session_id],
            |row| Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?, "status": row.get::<_, String>(1)?, "provider": row.get::<_, String>(2)?,
                "validation_cell": row.get::<_, Option<String>>(3)?, "launch_config": parse_value(row.get::<_, String>(4)?),
                "executable_version": row.get::<_, String>(5)?, "native_session_id": row.get::<_, Option<String>>(6)?,
                "native_identity_source": row.get::<_, Option<String>>(7)?, "process_identity": parse_optional_value(row.get::<_, Option<String>>(8)?),
                "transcript_epoch": row.get::<_, String>(9)?, "transcript_last_sequence": row.get::<_, i64>(10)?,
                "capture_state": row.get::<_, String>(11)?, "capture_error": row.get::<_, Option<String>>(12)?,
                "hook_trust_state": row.get::<_, String>(13)?, "exit": parse_optional_value(row.get::<_, Option<String>>(14)?),
                "created_at": row.get::<_, String>(15)?, "updated_at": row.get::<_, String>(16)?,
                "launch_state": row.get::<_, String>(17)?, "launch_error": row.get::<_, Option<String>>(18)?,
                "role_generation_id": row.get::<_, String>(19)?, "role": row.get::<_, String>(20)?, "generation": row.get::<_, i64>(21)?,
                "config_revision": row.get::<_, i64>(22)?, "attempt_id": row.get::<_, String>(23)?, "task_id": row.get::<_, String>(24)?,
                "project_id": row.get::<_, String>(25)?, "project_path": row.get::<_, String>(26)?,
                "capability_key":row.get::<_,Option<String>>(27)?,
                "capability_identity":parse_optional_value(row.get::<_,Option<String>>(28)?),
                "cmux_surface":parse_optional_value(row.get::<_,Option<String>>(29)?)
            })),
        ).optional()?.ok_or_else(|| anyhow!("unknown session {session_id}"))
    }

    pub fn invocation_input(&self, session_id: &str) -> Result<serde_json::Value> {
        let connection = self.lock()?;
        let value: Option<String> = connection
            .query_row(
                "SELECT invocation_input_json FROM sessions WHERE id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        serde_json::from_str(
            value
                .as_deref()
                .ok_or_else(|| anyhow!("session has no persisted invocation input"))?,
        )
        .context("parse persisted invocation input")
    }

    pub fn is_isolated_validation_session(&self, session_id: &str) -> Result<bool> {
        let connection = self.lock()?;
        Ok(connection.query_row("SELECT a.status='capability_validation' FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE s.id=?1",params![session_id],|row|row.get(0))?)
    }

    pub fn validate_resume_input(&self, session_id: &str, input: &serde_json::Value) -> Result<()> {
        let prompt = input
            .get("prompt")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("persisted invocation prompt is missing"))?;
        let expected = input
            .get("prompt_hash")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("persisted invocation prompt hash is missing"))?;
        if json_hash(&prompt)? != expected {
            bail!("persisted invocation prompt hash does not match")
        }
        let (role_prompt_hash, workflow_version, workflow_hash, role_text): (
            String,
            String,
            String,
            String,
        ) = {
            let connection = self.lock()?;
            connection.query_row(
                "SELECT s.prompt_hash,s.workflow_version,s.workflow_hash,rg.role FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?
        };
        if input
            .get("role_prompt_hash")
            .and_then(|value| value.as_str())
            != Some(role_prompt_hash.as_str())
            || input
                .get("workflow_version")
                .and_then(|value| value.as_str())
                != Some(workflow_version.as_str())
            || input.get("workflow_hash").and_then(|value| value.as_str())
                != Some(workflow_hash.as_str())
        {
            bail!("persisted invocation lost its exact workflow or role-prompt provenance")
        }
        let role: RoleKind = role_text.parse().map_err(|error: String| anyhow!(error))?;
        if workflow_version != crate::workflow_resources::WORKFLOW_VERSION
            || workflow_hash != crate::workflow_resources::workflow_hash()
            || role_prompt_hash != crate::workflow_resources::prompt_hash(role)
        {
            bail!("workflow or role-prompt revision changed; exact native resume is blocked")
        }
        if let Some(request_id) = input
            .get("review_request_id")
            .and_then(|value| value.as_str())
        {
            let connection = self.lock()?;
            let valid: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_requests r JOIN sessions s ON s.id=?1
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id
                 WHERE r.id=?2 AND r.session_id=s.id AND r.role_generation_id=rg.id
                   AND r.attempt_id=a.id AND r.delivery_state='delivered'
                   AND r.candidate_hash=CASE r.review_kind WHEN 'plan' THEN a.plan_hash ELSE a.candidate_hash END)",
                params![session_id, request_id],
                |row| row.get(0),
            )?;
            if !valid {
                bail!("review request is no longer the exact current delivered candidate; a fresh request is required")
            }
        }
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<serde_json::Value>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT id FROM sessions ORDER BY created_at")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        drop(connection);
        ids.into_iter().map(|id| self.session_json(&id)).collect()
    }

    pub fn session_evidence_json(&self, session_id: &str) -> Result<serde_json::Value> {
        let connection = self.lock()?;
        let hooks: i64 = connection.query_row(
            "SELECT COUNT(*) FROM hook_events WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let reports: i64 = connection.query_row(
            "SELECT COUNT(*) FROM role_results WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let completion_claims: i64 = connection.query_row(
            "SELECT COUNT(*) FROM role_results WHERE session_id = ?1 AND outcome IN ('accepted', 'approved', 'done')",
            params![session_id], |row| row.get(0),
        )?;
        Ok(serde_json::json!({
            "hook_events": hooks, "role_results": reports,
            "authoritative_completion": false, "reported_completion_words": completion_claims
        }))
    }

    pub fn record_capability_proof(
        &self,
        proof: &CapabilityProofInput,
    ) -> Result<serde_json::Value> {
        self.record_capability_proof_with_guard(proof, |_| Ok(()))
    }

    pub(crate) fn record_capability_proof_with_guard<F>(
        &self,
        proof: &CapabilityProofInput,
        guard: F,
    ) -> Result<serde_json::Value>
    where
        F: FnOnce(&Transaction<'_>) -> Result<()>,
    {
        if proof.history_nonce.trim().is_empty() || proof.evidence_reference.trim().is_empty() {
            bail!("history nonce and evidence reference are required")
        }
        let request_hash = json_hash(proof)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        guard(&transaction)?;
        let (provider,version,role,capability_key,capability_json,native_id,trust,exit_json,project_path,workspace_path): (String,String,String,Option<String>,Option<String>,Option<String>,String,Option<String>,String,Option<String>) = transaction.query_row(
            "SELECT s.provider,s.executable_version,rg.role,s.capability_key,s.capability_identity_json,s.native_session_id,s.hook_trust_state,s.exit_json,p.repository_path,w.path
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id
             JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id LEFT JOIN workspaces w ON w.attempt_id=a.id WHERE s.id=?1",
            params![proof.session_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
        ).optional()?.ok_or_else(|| anyhow!("unknown capability session"))?;
        let role_kind: RoleKind = role.parse().map_err(|error: String| anyhow!(error))?;
        let provider_kind: crate::domain::Provider =
            provider.parse().map_err(|error: String| anyhow!(error))?;
        crate::providers::require_production_role(provider_kind, role_kind)?;
        let session_config_hash = capability_key.clone().ok_or_else(|| {
            anyhow!(
                "capability session predates frozen invocation identity; launch a fresh validation"
            )
        })?;
        let session_identity: CapabilityIdentity = serde_json::from_str(
            capability_json
                .as_deref()
                .ok_or_else(|| anyhow!("capability session has no frozen executable identity"))?,
        )?;
        if crate::providers::capability_identity_key(&session_identity)? != session_config_hash
            || session_identity.provider.to_string() != provider
            || session_identity.executable_version != version
            || session_identity.role != role_kind
        {
            bail!("frozen capability identity is inconsistent with the reserved session")
        }
        let current_cwd = workspace_path.as_deref().unwrap_or(&project_path);
        crate::providers::require_current_capability_identity_with_bundles(
            &session_identity,
            Path::new(current_cwd),
            &self.compatibility_bundles,
        )?;
        if role_kind == RoleKind::Implementer {
            if !(proof.workspace_write_observed
                && proof.original_repo_write_denied
                && proof.service_data_write_denied
                && proof.human_control_denied)
            {
                bail!("implementer proof requires worktree write success plus original repository, service data, and human-control denials")
            }
            let relative = proof
                .workspace_probe_relative_path
                .as_deref()
                .ok_or_else(|| {
                    anyhow!("implementer proof requires workspace_probe_relative_path")
                })?;
            if relative.is_absolute()
                || relative.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                bail!("workspace probe path must be safely relative")
            }
            let workspace_path = workspace_path
                .as_deref()
                .ok_or_else(|| anyhow!("implementer proof has no exact attempt worktree"))?;
            let workspace_probe = std::path::Path::new(workspace_path).join(relative);
            let original_probe = std::path::Path::new(&project_path).join(relative);
            let bytes =
                std::fs::read(&workspace_probe).context("read implementer workspace probe")?;
            let observed = hex::encode(Sha256::digest(bytes));
            if proof.workspace_probe_sha256.as_deref() != Some(observed.as_str()) {
                bail!("implementer workspace probe hash does not match")
            }
            if original_probe.exists() || std::fs::symlink_metadata(&original_probe).is_ok() {
                bail!(
                    "implementer workspace probe also exists in the protected original repository"
                )
            }
        } else if !(proof.direct_write_denied
            && proof.compound_denied
            && proof.redirect_denied
            && proof.human_control_denied)
        {
            bail!("read-only role proof requires all three write denials and human-control denial")
        }
        for path in &proof.denied_sentinel_paths {
            if !path.is_absolute() {
                bail!("denied sentinel paths must be absolute")
            }
            if path.exists() || std::fs::symlink_metadata(path).is_ok() {
                bail!("denied sentinel exists: {}", path.display())
            }
        }
        if native_id.as_deref() != Some(proof.native_resume_session_id.as_str())
            || trust != "observed_unverified"
        {
            bail!("native identity/resume proof does not match observed provider hooks")
        }
        let exit: serde_json::Value = serde_json::from_str(
            exit_json
                .as_deref()
                .ok_or_else(|| anyhow!("session has no verified exit"))?,
        )?;
        if exit
            .get("process_group_quiescent")
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            bail!("provider process group is not proven quiescent")
        }
        let hooks: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM hook_events WHERE session_id=?1",
            params![proof.session_id],
            |row| row.get(0),
        )?;
        let reports: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM role_results WHERE session_id=?1",
            params![proof.session_id],
            |row| row.get(0),
        )?;
        if hooks == 0 || reports == 0 {
            bail!("capability proof requires recorded native hooks and structured role reports")
        }
        let (attempt_status,lifecycle,cell,generation,runtime_probe):(String,String,Option<String>,String,bool)=transaction.query_row("SELECT a.status,t.lifecycle,s.validation_cell,rg.id,EXISTS(SELECT 1 FROM trip_runtime_probes probe WHERE probe.session_id=s.id AND probe.role=rg.role AND probe.state='evidence_recorded') FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id WHERE s.id=?1",params![proof.session_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
        let (config_hash, identity) = if runtime_probe {
            let (base_key, base_identity_json): (String, String) = transaction.query_row(
                "SELECT probe.capability_key,probe.capability_identity_json
                 FROM trip_runtime_probes probe JOIN sessions session ON session.id=probe.session_id
                 JOIN role_generations generation ON generation.id=session.role_generation_id
                 WHERE probe.session_id=?1 AND probe.role=generation.role AND probe.state='evidence_recorded'",
                params![proof.session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let base_identity: CapabilityIdentity = serde_json::from_str(&base_identity_json)?;
            if crate::providers::capability_identity_key(&base_identity)? != base_key
                || base_identity.provider.to_string() != provider
                || base_identity.executable_version != version
                || base_identity.role != role_kind
            {
                bail!("runtime probe base capability identity is inconsistent with its frozen admission")
            }
            crate::providers::require_current_capability_identity_with_bundles(
                &base_identity,
                Path::new(current_cwd),
                &self.compatibility_bundles,
            )?;
            (base_key, base_identity)
        } else {
            (session_config_hash.clone(), session_identity.clone())
        };
        let standalone_capability = attempt_status == "capability_validation" || runtime_probe;
        let legitimate_nonce_report = {
            let mut statement = transaction.prepare(
                "SELECT outcome,metadata_json FROM role_results
                 WHERE session_id=?1 AND role_generation_id=?2",
            )?;
            let rows = statement.query_map(params![proof.session_id, generation], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut found = false;
            for row in rows {
                let (outcome, metadata_json) = row?;
                let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&metadata_json) else {
                    continue;
                };
                let outcome_allowed = if standalone_capability {
                    outcome == "capability_observed"
                } else {
                    (lifecycle == "validation" && outcome == "capability_observed")
                        || match role_kind {
                            RoleKind::Manager => {
                                ["plan_ready", "handoff_ready", "blocked", "needs_input"]
                                    .contains(&outcome.as_str())
                            }
                            RoleKind::Explorer => ["evidence_ready", "blocked", "needs_input"]
                                .contains(&outcome.as_str()),
                            RoleKind::Implementer => ["candidate_ready", "blocked", "needs_input"]
                                .contains(&outcome.as_str()),
                            RoleKind::PlanReviewer
                            | RoleKind::CodeReviewer
                            | RoleKind::FinalReviewer => {
                                ["approved", "request_changes", "needs_rework"]
                                    .contains(&outcome.as_str())
                            }
                        }
                };
                if outcome_allowed
                    && metadata
                        .get("history_nonce")
                        .and_then(serde_json::Value::as_str)
                        == Some(proof.history_nonce.as_str())
                    && meaningful_validation_observation(&metadata, cell.as_deref())?
                {
                    found = true;
                    break;
                }
            }
            found
        };
        let (prompt_submits, native_identities, resume_count): (i64, i64, i64) =
            transaction.query_row(
                "SELECT
                   (SELECT COUNT(*) FROM hook_events WHERE session_id=?1 AND event_name='UserPromptSubmit'),
                   (SELECT COUNT(DISTINCT native_session_id) FROM hook_events WHERE session_id=?1 AND native_session_id IS NOT NULL),
                   (SELECT resume_count FROM sessions WHERE id=?1)",
                params![proof.session_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?;
        let session_semantics = if role_kind == RoleKind::FinalReviewer {
            prompt_submits >= 1 && resume_count == 0
        } else {
            prompt_submits >= 2 && resume_count >= 1
        };
        if !session_semantics || !legitimate_nonce_report || native_identities != 1 {
            if role_kind == RoleKind::FinalReviewer {
                bail!("final-verifier capability proof requires one fresh non-resumed native identity plus a legitimate observation-bearing structured report with the matching nonce")
            }
            bail!("capability proof requires one observed native identity across original and resumed prompts plus a legitimate observation-bearing structured report with the matching history nonce")
        }
        if provider_kind == crate::domain::Provider::Codex && role_kind == RoleKind::Implementer {
            let delivered_decisions: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM permission_requests
                 WHERE provider='codex' AND session_id=?1 AND role_generation_id=?2
                   AND native_session_id=?3 AND policy_fingerprint=?4
                   AND state IN ('approved_once','approved_rule','denied')
                   AND decision_kind IN ('approve_once','always_approve','deny')
                   AND decision_actor='authenticated_human' AND decided_at IS NOT NULL
                   AND delivery_state='delivered' AND delivered_at IS NOT NULL",
                params![
                    proof.session_id.as_str(),
                    generation.as_str(),
                    proof.native_resume_session_id.as_str(),
                    config_hash.as_str()
                ],
                |row| row.get(0),
            )?;
            if delivered_decisions == 0 {
                bail!("Codex Implementer proof requires an actual completed PermissionRequest decision delivered for this exact session and role generation")
            }
        }
        if !standalone_capability {
            let cell = cell.as_deref().ok_or_else(|| {
                anyhow!("workflow capability proof lacks an approved L03-L10 cell")
            })?;
            let contract:bool=match role_kind {
                RoleKind::Manager if cell=="L03"=>transaction.query_row("SELECT EXISTS(SELECT 1 FROM guidance_messages WHERE role_generation_id=?1 AND state='acknowledged') AND EXISTS(SELECT 1 FROM role_results WHERE role_generation_id=?1 AND outcome='plan_ready')",params![generation],|row|row.get(0))?,
                RoleKind::Manager=>transaction.query_row("SELECT EXISTS(SELECT 1 FROM switch_intents si JOIN role_generations rg ON rg.id=si.new_generation_id JOIN role_settings rs ON rs.task_id=(SELECT task_id FROM attempts WHERE id=si.attempt_id) AND rs.role=si.role WHERE rg.id=?1 AND rs.effective_generation_id=rg.id AND si.state='dispatched')",params![generation],|row|row.get(0))?,
                RoleKind::Implementer if cell=="L05"=>transaction.query_row("SELECT EXISTS(SELECT 1 FROM switch_intents si JOIN snapshots s ON s.id=si.checkpoint_snapshot_id WHERE si.old_generation_id=?1 AND si.role='implementer' AND si.state IN ('ready_for_dispatch','validation_reserved','dispatched') AND s.kind='checkpoint' AND s.complete=1 AND s.source_role_generation_id=?1)",params![generation],|row|row.get(0))?,
                RoleKind::Implementer=>transaction.query_row("SELECT EXISTS(SELECT 1 FROM switch_intents si JOIN role_generations rg ON rg.id=si.new_generation_id JOIN role_settings rs ON rs.task_id=(SELECT task_id FROM attempts WHERE id=si.attempt_id) AND rs.role=si.role WHERE rg.id=?1 AND rs.effective_generation_id=rg.id AND si.state='dispatched') AND EXISTS(SELECT 1 FROM role_results WHERE role_generation_id=?1 AND outcome='candidate_ready')",params![generation],|row|row.get(0))?,
                RoleKind::CodeReviewer if cell=="L07"=>transaction.query_row("SELECT EXISTS(SELECT 1 FROM switch_intents si JOIN review_requests r ON r.attempt_id=si.attempt_id AND r.role_generation_id=si.old_generation_id WHERE si.old_generation_id=?1 AND si.role='code_reviewer' AND si.state IN ('ready_for_dispatch','validation_reserved','dispatched') AND r.delivery_state='replaced' AND r.budget_spent_at IS NOT NULL)",params![generation],|row|row.get(0))?,
                _=>transaction.query_row("SELECT EXISTS(SELECT 1 FROM review_requests WHERE role_generation_id=?1 AND session_id=?2 AND delivery_state='finished')",params![generation,proof.session_id],|row|row.get(0))?,
            };
            if !contract {
                bail!("workflow capability cell {cell} has not exercised its required same-context contract")
            }
        }
        let sentinel = std::path::Path::new(&project_path).join(&proof.sentinel_relative_path);
        if sentinel.exists() {
            bail!("confinement sentinel exists: {}", sentinel.display())
        }
        let hook_hash = identity
            .hook_revision
            .split_once(':')
            .map(|(_, hash)| hash)
            .unwrap_or(&identity.hook_revision);
        if let Some((hash,result)) = transaction.query_row(
            "SELECT request_hash,result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='capability_proof'",
            params![proof.operation_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)),
        ).optional()? {
            if hash != request_hash { bail!("capability proof operation was reused with different input") }
            return Ok(serde_json::from_str(&result)?)
        }
        let mut runtime_scopes = Vec::new();
        if let Some(existing) = transaction
            .query_row(
                "SELECT proof_json FROM capabilities WHERE provider=?1 AND executable_version=?2 AND role=?3 AND mode='interactive_pty' AND config_hash=?4 AND status='supported'",
                params![provider, version, role, config_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        {
            runtime_scopes.extend(
                existing
                    .get("runtime_scopes")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            if let Some(scope) = existing
                .get("runtime_scope")
                .filter(|scope| !scope.is_null())
            {
                runtime_scopes.push(scope.clone());
            }
        }
        if let Some(runtime_scope) = proof.runtime_scope.as_ref() {
            if !runtime_scopes.contains(runtime_scope) {
                runtime_scopes.push(runtime_scope.clone());
            }
        }
        let result = serde_json::json!({"status":"supported","provider":provider,"version":version,"role":role,"model":identity.model,"effort":identity.effort,"permission_policy":identity.permission_policy,"denied_read_floor":identity.security_policy.get("denied_read_floor"),
            "local_mcp_coverage_revision":identity.security_policy.pointer("/local_mcp_coverage/revision"),
            "native_approval_ownership_revision":identity.security_policy.pointer("/native_approval_ownership/revision"),
            "config_hash":config_hash,"hook_hash":hook_hash,"session_id":proof.session_id,"native_session_id":native_id,"evidence_reference":proof.evidence_reference,
            "runtime_scope":proof.runtime_scope,"runtime_scopes":runtime_scopes,
            "compatibility":identity.compatibility,
            "compatibility_provenance":serde_json::from_str::<crate::domain::LaunchConfig>(&transaction.query_row(
                "SELECT launch_config_json FROM sessions WHERE id=?1", params![proof.session_id], |row| row.get::<_,String>(0))?)?.compatibility});
        let now = Utc::now().to_rfc3339();
        transaction.execute("UPDATE sessions SET native_identity_verified_at=?1,hook_trust_state='verified_live',updated_at=?1 WHERE id=?2", params![now,proof.session_id])?;
        transaction.execute("INSERT INTO capabilities(id,provider,executable_version,role,mode,config_hash,status,evidence_reference,gaps_json,checked_at,hook_hash,proof_json)
            VALUES(?1,?2,?3,?4,'interactive_pty',?5,'supported',?6,'[]',?7,?8,?9)
            ON CONFLICT(provider,executable_version,role,mode,config_hash) DO UPDATE SET status='supported',evidence_reference=excluded.evidence_reference,
            gaps_json='[]',checked_at=excluded.checked_at,hook_hash=excluded.hook_hash,proof_json=excluded.proof_json",
            params![uuid::Uuid::new_v4().to_string(),provider,version,role,config_hash,proof.evidence_reference,now,hook_hash,result.to_string()])?;
        transaction.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','capability_proof',?2,?3,?4)",
            params![proof.operation_id,request_hash,result.to_string(),now])?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn role_launch_context(
        &self,
        attempt_id: &str,
        role: RoleKind,
    ) -> Result<RoleLaunchContext> {
        self.role_launch_context_inner(attempt_id, role, "default", false, None)
    }

    pub fn lane_role_launch_context(
        &self,
        attempt_id: &str,
        role: RoleKind,
        lane_key: &str,
    ) -> Result<RoleLaunchContext> {
        self.role_launch_context_inner(attempt_id, role, lane_key, false, None)
    }

    pub fn validation_role_launch_context(
        &self,
        attempt_id: &str,
        role: RoleKind,
    ) -> Result<RoleLaunchContext> {
        self.role_launch_context_inner(attempt_id, role, "default", true, None)
    }

    pub fn trip_setup_role_launch_context(
        &self,
        attempt_id: &str,
        role: RoleKind,
        runtime_probe: bool,
    ) -> Result<RoleLaunchContext> {
        self.role_launch_context_inner(attempt_id, role, "default", true, Some(runtime_probe))
    }

    fn role_launch_context_inner(
        &self,
        attempt_id: &str,
        role: RoleKind,
        lane_key: &str,
        validation_dispatch: bool,
        setup_runtime_probe: Option<bool>,
    ) -> Result<RoleLaunchContext> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task_id,workspace,revision,config_json,phase,plan_approved,plan_hash,candidate_hash,attempt_status,lifecycle,attention,workflow_version,workflow_hash):(String,String,i64,String,String,Option<String>,Option<String>,Option<String>,String,String,String,String,String)=transaction.query_row(
            "SELECT a.task_id,w.path,rs.revision,rs.config_json,a.phase,a.plan_approved_at,a.plan_hash,a.candidate_hash,a.status,t.lifecycle,t.attention,a.workflow_version,a.workflow_hash FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN workspaces w ON w.attempt_id=a.id
             JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=?2
             WHERE a.id=?1 AND w.state='ready' AND rs.revision=COALESCE(
               (SELECT settings_revision FROM trip_attempt_profiles WHERE attempt_id=a.id AND role=?2),
               (SELECT settings_revision FROM trip_setup_permits WHERE attempt_id=a.id AND role=?2 AND purpose='runtime_probe' AND state='issued'),
               (SELECT MAX(current.revision) FROM role_settings current WHERE current.task_id=a.task_id AND current.role=?2))
             LIMIT 1",params![attempt_id,role.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?,row.get(12)?))
        ).optional()?.ok_or_else(||anyhow!("attempt workspace or role settings are unavailable"))?;
        let setup_permit: Option<(String, String)> = if setup_runtime_probe.is_some() {
            transaction
                .query_row(
                    "SELECT id,purpose FROM trip_setup_permits WHERE attempt_id=?1 AND role=?2
                     AND settings_revision=?3 AND state='issued' ORDER BY created_at DESC LIMIT 1",
                    params![attempt_id, role.to_string(), revision],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
        } else {
            None
        };
        if let Some(runtime_probe) = setup_runtime_probe {
            let purpose = setup_permit
                .as_ref()
                .map(|(_, purpose)| purpose.as_str())
                .ok_or_else(|| anyhow!("setup role has no issued typed permit"))?;
            if purpose == "runtime_probe" && !runtime_probe {
                bail!("ordinary runtime probes launch only through dispatch_runtime_probe")
            }
            if runtime_probe && purpose != "runtime_probe" {
                bail!("runtime probe launch requires an issued runtime_probe permit")
            }
        }
        let setup_permit_id = setup_permit.map(|(id, _)| id);
        crate::trip::require_attempt_ready(&transaction, attempt_id, setup_permit_id.as_deref())?;
        if workflow_version != crate::workflow_resources::WORKFLOW_VERSION
            || workflow_hash != crate::workflow_resources::workflow_hash()
        {
            bail!("attempt is pinned to a different workflow revision; create explicit fresh lineage before dispatch")
        }
        if validation_dispatch && lifecycle != "validation" {
            bail!("validation role launch requires validation task lifecycle")
        }
        let held_validation = validation_dispatch
            && attempt_status == "held"
            && lifecycle == "validation"
            && attention == "paused";
        if attempt_status != "running" && !held_validation {
            bail!("attempt is not dispatchable")
        }
        let allowed = validation_dispatch
            || match role {
                RoleKind::Manager => matches!(
                    phase.as_str(),
                    "planning"
                        | "plan_review"
                        | "awaiting_plan_approval"
                        | "awaiting_implementation_authorization"
                        | "implementation"
                        | "code_review"
                        | "checks"
                        | "final_review"
                        | "manager_handoff"
                ),
                RoleKind::Explorer => matches!(
                    phase.as_str(),
                    "planning" | "implementation" | "code_review" | "checks" | "final_review"
                ),
                RoleKind::PlanReviewer => phase == "plan_review" && plan_hash.is_some(),
                RoleKind::Implementer => phase == "implementation" && plan_approved.is_some(),
                RoleKind::CodeReviewer => phase == "code_review" && candidate_hash.is_some(),
                RoleKind::FinalReviewer => phase == "final_review" && candidate_hash.is_some(),
            };
        if !allowed {
            bail!("role {role} cannot launch during attempt phase {phase}")
        }
        if role == RoleKind::Explorer && !validation_dispatch {
            let authorized: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM trip_explorer_decisions d JOIN attempts a ON a.id=d.attempt_id
                 WHERE d.attempt_id=?1 AND d.activated=1 AND d.outcome_json IS NULL AND d.candidate_hash IS a.candidate_hash)",
                params![attempt_id], |row| row.get(0),
            )?;
            if !authorized {
                bail!("Explorer launch requires a current activated planning, rescue, or final decision")
            }
        }
        let reviewed_parallel_lanes = if role == RoleKind::Implementer && setup_permit_id.is_none()
        {
            crate::trip::reviewed_parallel_lane_count(&transaction, attempt_id)?
        } else {
            None
        };
        if role == RoleKind::Implementer {
            let freeze_active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1 AND state IN ('reserved','capturing','recovery_required'))",params![attempt_id],|row|row.get(0))?;
            if freeze_active {
                bail!("writer launch is fenced by an active or unresolved snapshot freeze")
            }
            if !validation_dispatch && lane_key == "default" && reviewed_parallel_lanes.is_some() {
                require_default_integration_launch(&transaction, attempt_id)?;
            }
        }
        if role != RoleKind::Implementer && lane_key != "default" {
            bail!("only implementer authority can be scoped to an implementation lane")
        }
        let lane_id = if role == RoleKind::Implementer && lane_key != "default" {
            transaction.query_row(
                "SELECT l.id FROM implementation_lanes l WHERE l.attempt_id=?1 AND l.lane_key=?2 AND l.state='admitted'
                   AND NOT EXISTS(SELECT 1 FROM json_each(l.dependencies_json) dependency
                     JOIN implementation_lanes prerequisite ON prerequisite.attempt_id=l.attempt_id AND prerequisite.lane_key=dependency.value
                     WHERE prerequisite.state!='yielded')",
                params![attempt_id,lane_key], |row| row.get::<_,String>(0)
            ).optional()?.ok_or_else(||anyhow!("implementation lane is unknown, not admitted, or waiting for a required predecessor yield"))?
        } else {
            "default".to_owned()
        };
        if role == RoleKind::Implementer && lane_id != "default" {
            require_initial_lane_launch(
                &transaction,
                attempt_id,
                &lane_id,
                Path::new(&workspace),
                None,
            )?;
        }
        let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role=?2 AND lane_id=?3 AND status IN ('launch_reserved','running','stopping'))",params![attempt_id,role.to_string(),lane_id],|row|row.get(0))?;
        if active {
            bail!("role already has active or stopping authority")
        }
        let prior_context:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations rg JOIN attempts prior ON prior.id=rg.attempt_id WHERE prior.task_id=?1 AND prior.id!=?2 AND rg.role=?3 AND rg.status IN ('launch_reserved','running','stopping'))",params![task_id,attempt_id,role.to_string()],|row|row.get(0))?;
        if prior_context {
            bail!("role capacity is reserved until the prior task context is quiescent")
        }
        let resolved_config = serde_json::from_str::<crate::domain::RoleOverride>(&config_json)?;
        if !validation_dispatch {
            crate::providers::require_production_role(resolved_config.provider, role)?;
            let bound:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM trip_attempt_profiles WHERE attempt_id=?1 AND role=?2 AND settings_revision=?3 AND json_extract(profile_json,'$.provider')=?4 AND json_extract(profile_json,'$.model')=?5 AND json_extract(profile_json,'$.effort')=?6)",params![attempt_id,role.to_string(),revision,resolved_config.provider.to_string(),resolved_config.model,resolved_config.effort],|row|row.get(0))?;
            if !bound {
                bail!("attempt lacks an effective task-profile authority for this role")
            }
        }
        let provider = resolved_config.provider.to_string();
        let conflicts:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND status IN ('launch_reserved','running','stopping') AND ((?2='implementer' AND role IN ('explorer','plan_reviewer','code_reviewer','final_verifier')) OR (?2 IN ('explorer','plan_reviewer','code_reviewer','final_verifier') AND role='implementer')))",params![attempt_id,role.to_string()],|row|row.get(0))?;
        if conflicts {
            bail!("writer and reviewer authorities cannot overlap")
        }
        let check_running: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND status='running')",
            params![attempt_id],
            |row| row.get(0),
        )?;
        if check_running {
            bail!("role launch conflicts with a durably owned running check")
        }
        if !role_capacity_available(&transaction, &provider, role, None, None)? {
            bail!("role capacity is full")
        }
        let held:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1 AND (t.attention IN ('pause_requested','needs_recovery','restart_parked','resume_failed') OR EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=a.id AND c.kind IN ('pause_now','pause_after_role','cancel') AND c.state='requested') OR (?2='manager' AND EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=a.id AND c.kind='setup_manager_change' AND c.state='held')) OR EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=a.id AND c.kind IN ('manager_stop','manager_change') AND c.state NOT IN ('finished','cancelled','superseded','rejected')) OR EXISTS(SELECT 1 FROM restart_candidates rc WHERE rc.attempt_id=a.id AND rc.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))))",params![attempt_id,role.to_string()],|row|row.get(0))?;
        if held {
            bail!("attempt is held by a pending control or recovery decision")
        }
        let permit_id = uuid::Uuid::new_v4().to_string();
        transaction.execute("INSERT INTO launch_permits(id,attempt_id,role,settings_revision,state,created_at,validation_dispatch,setup_permit_id,lane_id) VALUES(?1,?2,?3,?4,'issued',?5,?6,?7,?8)",params![permit_id,attempt_id,role.to_string(),revision,Utc::now().to_rfc3339(),validation_dispatch,setup_permit_id,lane_id])?;
        transaction.commit()?;
        Ok(RoleLaunchContext {
            task_id,
            workspace: workspace.into(),
            config: resolved_config,
            settings_revision: revision,
            role_generation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            transcript_epoch: uuid::Uuid::new_v4().to_string(),
            credential_id: uuid::Uuid::new_v4().to_string(),
            token: auth::issue_secret(),
            permit_id,
            lane_id,
        })
    }

    pub fn require_supported_capability(&self, config: &LaunchConfig) -> Result<()> {
        let connection = self.lock()?;
        require_supported_capability_in(&connection, config, &self.compatibility_bundles)
    }

    pub fn switched_role_launch_context(&self, intent_id: &str) -> Result<RoleLaunchContext> {
        self.switched_role_launch_context_inner(intent_id, false)
    }

    pub fn switched_validation_role_launch_context(
        &self,
        intent_id: &str,
    ) -> Result<RoleLaunchContext> {
        self.switched_role_launch_context_inner(intent_id, true)
    }

    fn switched_role_launch_context_inner(
        &self,
        intent_id: &str,
        validation_dispatch: bool,
    ) -> Result<RoleLaunchContext> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (attempt_id,task_id,role_text,revision,config_json,workspace,phase,attempt_status,lifecycle,attention,workflow_version,workflow_hash):(String,String,String,i64,String,String,String,String,String,String,String,String)=transaction.query_row(
            "SELECT si.attempt_id,a.task_id,si.role,si.requested_settings_revision,si.config_json,w.path,a.phase,a.status,t.lifecycle,t.attention,a.workflow_version,a.workflow_hash
             FROM switch_intents si JOIN attempts a ON a.id=si.attempt_id JOIN tasks t ON t.id=a.task_id
             JOIN workspaces w ON w.attempt_id=si.attempt_id
             WHERE si.id=?1 AND si.state='ready_for_dispatch' AND w.state='ready'",
            params![intent_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?)))?;
        let setup_permit_id: Option<String> = None;
        crate::trip::require_attempt_ready(&transaction, &attempt_id, setup_permit_id.as_deref())?;
        if workflow_version != crate::workflow_resources::WORKFLOW_VERSION
            || workflow_hash != crate::workflow_resources::workflow_hash()
        {
            bail!("switch intent belongs to a different workflow revision")
        }
        if validation_dispatch && lifecycle != "validation" {
            bail!("validation role switch requires validation task lifecycle")
        }
        let held_validation = validation_dispatch
            && attempt_status == "held"
            && lifecycle == "validation"
            && attention == "paused";
        if attempt_status != "running" && !held_validation {
            bail!("switched role attempt is not dispatchable")
        }
        let role: RoleKind = role_text.parse().map_err(|error: String| anyhow!(error))?;
        let config: crate::domain::RoleOverride = serde_json::from_str(&config_json)?;
        if !validation_dispatch {
            crate::providers::require_production_role(config.provider, role)?;
            crate::trip::require_recorded_task_profile_authority(
                &transaction,
                &task_id,
                &role_text,
                revision,
            )?;
        }
        let allowed = match role {
            RoleKind::Manager => matches!(
                phase.as_str(),
                "planning"
                    | "plan_review"
                    | "awaiting_plan_approval"
                    | "awaiting_implementation_authorization"
                    | "implementation"
                    | "code_review"
                    | "checks"
                    | "final_review"
                    | "manager_handoff"
            ),
            RoleKind::Explorer => matches!(
                phase.as_str(),
                "planning" | "implementation" | "code_review" | "checks" | "final_review"
            ),
            RoleKind::PlanReviewer => phase == "plan_review",
            RoleKind::Implementer => phase == "implementation",
            RoleKind::CodeReviewer => phase == "code_review",
            RoleKind::FinalReviewer => phase == "final_review",
        };
        if !allowed {
            bail!("captured switch role is no longer allowed in phase {phase}")
        }
        if role == RoleKind::Implementer {
            let freeze_active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1 AND state IN ('reserved','capturing','recovery_required'))",params![attempt_id],|row|row.get(0))?;
            if freeze_active {
                bail!("writer switch is fenced by an active or unresolved snapshot freeze")
            }
        }
        let old_lane: String = transaction.query_row(
            "SELECT lane_id FROM role_generations WHERE id=(SELECT old_generation_id FROM switch_intents WHERE id=?1)",
            params![intent_id], |row| row.get(0)
        )?;
        let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role=?2 AND lane_id=?3 AND status IN ('launch_reserved','running','stopping'))",params![attempt_id,role_text,old_lane],|row|row.get(0))?;
        if active {
            bail!("switched role still has active authority")
        }
        let conflicts:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND status IN ('launch_reserved','running','stopping') AND ((?2='implementer' AND role IN ('explorer','plan_reviewer','code_reviewer','final_verifier')) OR (?2 IN ('explorer','plan_reviewer','code_reviewer','final_verifier') AND role='implementer')))",params![attempt_id,role_text],|row|row.get(0))?;
        if conflicts {
            bail!("captured switch conflicts with current writer or reviewer authority")
        }
        let check_running: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND status='running')",
            params![attempt_id],
            |row| row.get(0),
        )?;
        if check_running {
            bail!("switched role conflicts with a durably owned running check")
        }
        let provider = config.provider.to_string();
        if !role_capacity_available(&transaction, &provider, role, None, None)? {
            bail!("role capacity is full")
        }
        let held:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND (attention IN ('pause_requested','needs_recovery') OR (attention='paused' AND NOT ?2))) OR EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?3 AND c.kind IN ('manager_stop','manager_change') AND c.state NOT IN ('finished','cancelled','superseded','rejected') AND (c.kind!='manager_change' OR c.state!='switching' OR json_extract(c.payload_json,'$.switch_intent_id') IS NOT ?4))",params![task_id,held_validation,attempt_id,intent_id],|row|row.get(0))?;
        if held {
            bail!("attempt is held by pause or recovery")
        }
        let permit_id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute("INSERT INTO launch_permits(id,attempt_id,role,settings_revision,switch_intent_id,state,created_at,validation_dispatch,setup_permit_id,lane_id) VALUES(?1,?2,?3,?4,?5,'issued',?6,?7,?8,?9)",params![permit_id,attempt_id,role_text,revision,intent_id,now,validation_dispatch,setup_permit_id,old_lane])?;
        if validation_dispatch {
            let changed = transaction.execute(
                "UPDATE switch_intents SET state='validation_reserved',updated_at=?1 WHERE id=?2 AND state='ready_for_dispatch'",
                params![now,intent_id],
            )?;
            if changed != 1 {
                bail!("switch intent was concurrently claimed")
            }
        }
        transaction.commit()?;
        Ok(RoleLaunchContext {
            task_id,
            workspace: workspace.into(),
            config,
            settings_revision: revision,
            role_generation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            transcript_epoch: uuid::Uuid::new_v4().to_string(),
            credential_id: uuid::Uuid::new_v4().to_string(),
            token: auth::issue_secret(),
            permit_id,
            lane_id: old_lane,
        })
    }

    pub fn mark_switch_dispatched(&self, intent_id: &str, generation_id: &str) -> Result<()> {
        let connection = self.lock()?;
        let changed=connection.execute("UPDATE switch_intents SET state='dispatched',new_generation_id=?1,updated_at=?2 WHERE id=?3 AND state IN ('ready_for_dispatch','validation_reserved') AND new_generation_id IS NULL AND EXISTS(SELECT 1 FROM launch_permits WHERE switch_intent_id=?3 AND state='consumed')",params![generation_id,Utc::now().to_rfc3339(),intent_id])?;
        if changed != 1 {
            bail!("switch intent was not ready for its captured dispatch")
        }
        let (role, lane_id): (String, String) = connection.query_row(
            "SELECT role,lane_id FROM role_generations WHERE id=?1",
            params![generation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if role == "implementer" && lane_id != "default" {
            connection.execute("UPDATE lane_generations SET effective_generation_id=?1,pending_settings_revision=NULL,updated_at=?2 WHERE lane_id=?3",params![generation_id,Utc::now().to_rfc3339(),lane_id])?;
        } else {
            connection.execute("UPDATE role_settings SET effective_generation_id=?1 WHERE task_id=(SELECT task_id FROM attempts WHERE id=(SELECT attempt_id FROM switch_intents WHERE id=?2)) AND role=(SELECT role FROM switch_intents WHERE id=?2) AND revision=(SELECT requested_settings_revision FROM switch_intents WHERE id=?2)",params![generation_id,intent_id])?;
        }
        connection.execute("UPDATE guidance_messages SET role_generation_id=?1 WHERE attempt_id=(SELECT attempt_id FROM switch_intents WHERE id=?2) AND role_generation_id=(SELECT old_generation_id FROM switch_intents WHERE id=?2) AND state='queued'",params![generation_id,intent_id])?;
        Ok(())
    }

    /// A manager replacement reserves its old-authority revocation before the
    /// process-group interrupt. If delivery fails, the intent must no longer
    /// be eligible for quiescence advancement or dispatch; only a later
    /// versioned human manager operation may supersede it.
    pub fn fail_manager_switch_after_signal_failure(
        &self,
        intent_id: &str,
        control_id: &str,
        reason: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent_changed = transaction.execute(
            "UPDATE switch_intents SET state='recovery_required',updated_at=?1
             WHERE id=?2 AND role='manager' AND state='stopping_old'",
            params![now, intent_id],
        )?;
        if intent_changed != 1 {
            bail!("manager switch intent was not stopping when signal delivery failed")
        }
        // A missing service-owned handle can fail before interrupt_once has
        // marked the session. Neither a running reservation nor an
        // interrupt-requested marker is a truthful delivery receipt after
        // this exact replacement stop fails.
        transaction.execute(
            "UPDATE sessions SET status='recovery_required',updated_at=?1
             WHERE role_generation_id=(SELECT old_generation_id FROM switch_intents WHERE id=?2)
               AND status IN ('launch_reserved','running','interrupt_requested')",
            params![now, intent_id],
        )?;
        let control_changed = transaction.execute(
            "UPDATE controls SET state='failed',
                    payload_json=json_set(payload_json,
                        '$.switch_intent_id',?1,
                        '$.switch_recovery_state','recovery_required',
                        '$.failure',?2,
                        '$.next_action',?3),updated_at=?4
             WHERE id=?5 AND kind='manager_change'
               AND state IN ('waiting_safe_boundary','switch_requested','switching')",
            params![
                intent_id,
                reason,
                "The replacement intent is non-dispatchable after the failed manager-only signal. Reconcile the captured manager process, then submit a fresh version-checked Change manager operation; no automatic retry is scheduled.",
                now,
                control_id
            ],
        )?;
        if control_changed != 1 {
            bail!("manager change control changed while signal delivery failure was recorded")
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn restore_switch_after_proven_nondelivery(
        &self,
        intent_id: &str,
        generation_id: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE switch_intents SET state='ready_for_dispatch',new_generation_id=NULL,updated_at=?1 WHERE id=?2 AND state='dispatched' AND new_generation_id=?3 AND EXISTS(SELECT 1 FROM sessions WHERE role_generation_id=?3 AND status='launch_failed' AND launch_state='failed')",
            params![now,intent_id,generation_id],
        )?;
        if changed != 1 {
            bail!("switch launch is not proven nondelivered for a safe retry")
        }
        transaction.execute(
            "UPDATE role_settings SET effective_generation_id=NULL WHERE effective_generation_id=?1",
            params![generation_id],
        )?;
        transaction.execute(
            "UPDATE lane_generations SET effective_generation_id=NULL,updated_at=?1 WHERE effective_generation_id=?2",
            params![now,generation_id],
        )?;
        transaction.execute(
            "UPDATE controls SET state='failed',
                    payload_json=json_set(payload_json,'$.failure',?1,'$.next_action',?2),updated_at=?3
             WHERE kind='manager_change' AND state='switching'
               AND json_extract(payload_json,'$.switch_intent_id')=?4",
            params![
                "replacement launch was proven nondelivered; no automatic paid retry was scheduled",
                "Review the failed replacement and submit a fresh Change manager operation when ready.",
                now,
                intent_id
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn release_unconsumed_launch_permits(
        &self,
        permit_id: Option<&str>,
        reason: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let permits = {
            let mut statement = transaction.prepare(
                "SELECT id,attempt_id,role,switch_intent_id,validation_dispatch FROM launch_permits
                 WHERE state='issued' AND (?1 IS NULL OR id=?1) ORDER BY created_at,id",
            )?;
            let rows = statement.query_map(params![permit_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut released = Vec::new();
        for (permit, attempt, role, switch_intent, validation_dispatch) in permits {
            let changed = transaction.execute(
                "UPDATE launch_permits SET state='released_nondelivery' WHERE id=?1 AND state='issued'",
                params![permit],
            )?;
            if changed == 0 {
                continue;
            }
            if validation_dispatch {
                if let Some(intent) = switch_intent.as_deref() {
                    transaction.execute(
                        "UPDATE switch_intents SET state='ready_for_dispatch',updated_at=?1
                         WHERE id=?2 AND state='validation_reserved' AND new_generation_id IS NULL
                           AND EXISTS(SELECT 1 FROM launch_permits WHERE id=?3 AND switch_intent_id=?2
                             AND state='released_nondelivery' AND validation_dispatch=1)",
                        params![now, intent, permit],
                    )?;
                }
            }
            let detail = serde_json::json!({"permit_id":permit,"attempt_id":attempt,"role":role,"reason":reason});
            transaction.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?2,'service','launch.permit.released_nondelivery','launch_permit',?3,?4,?5)",
                params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),permit,detail.to_string(),now],
            )?;
            released.push(serde_json::json!({"permit_id":permit,"attempt_id":attempt,"role":role,"state":"released_nondelivery"}));
        }
        transaction.commit()?;
        Ok(released)
    }

    pub fn reserve_role_invocation(
        &self,
        context: &RoleLaunchContext,
        config: &LaunchConfig,
    ) -> Result<()> {
        match self.reserve_role_invocation_inner(context, config, None, None)? {
            BrowserLaunchReservation::Reserved => Ok(()),
            BrowserLaunchReservation::Existing(_) => {
                bail!("non-browser role invocation unexpectedly found a browser receipt")
            }
        }
    }

    pub(crate) fn reserve_role_invocation_with_browser_receipt(
        &self,
        context: &RoleLaunchContext,
        config: &LaunchConfig,
        receipt: BrowserLaunchReceipt<'_>,
    ) -> Result<BrowserLaunchReservation> {
        self.reserve_role_invocation_inner(context, config, Some(receipt), None)
    }

    pub(crate) fn reserve_role_invocation_with_browser_receipt_and_fresh_rejection(
        &self,
        context: &RoleLaunchContext,
        config: &LaunchConfig,
        receipt: BrowserLaunchReceipt<'_>,
        fresh_resume_rejection: &serde_json::Value,
    ) -> Result<BrowserLaunchReservation> {
        self.reserve_role_invocation_inner(
            context,
            config,
            Some(receipt),
            Some(fresh_resume_rejection),
        )
    }

    fn reserve_role_invocation_inner(
        &self,
        context: &RoleLaunchContext,
        config: &LaunchConfig,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
        fresh_resume_rejection: Option<&serde_json::Value>,
    ) -> Result<BrowserLaunchReservation> {
        self.require_execution_unheld("role launch reservations")?;
        let capability_identity = crate::providers::capability_identity(config)?;
        crate::providers::require_current_capability_identity_with_bundles(
            &capability_identity,
            &config.cwd,
            &self.compatibility_bundles,
        )?;
        let capability_key = crate::providers::capability_identity_key(&capability_identity)?;
        let capability_identity_json = serde_json::to_string(&capability_identity)?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = browser_receipt {
            if let Some(existing) = browser_launch_receipt_in(
                &transaction,
                receipt.operation_id,
                receipt.operation_kind,
                receipt.input_hash,
            )? {
                return Ok(BrowserLaunchReservation::Existing(existing));
            }
        }
        let attempt_id: String = transaction.query_row(
            "SELECT attempt_id FROM workspaces WHERE path=?1 AND state='ready'",
            params![context.workspace.to_string_lossy()],
            |row| row.get(0),
        )?;
        let permit:Option<(bool,Option<String>,String,Option<String>)>=transaction.query_row("SELECT validation_dispatch,setup_permit_id,lane_id,switch_intent_id FROM launch_permits WHERE id=?1 AND attempt_id=?2 AND role=?3 AND settings_revision=?4 AND state='issued'",params![context.permit_id,attempt_id,config.role.to_string(),context.settings_revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
        let (validation_dispatch, setup_permit_id, permit_lane_id, switch_intent) =
            permit.ok_or_else(|| anyhow!("role launch permit is stale or already consumed"))?;
        if permit_lane_id != context.lane_id {
            bail!("role launch lane changed after permit issuance")
        }
        let setup_validation_cell: Option<String> = if let Some(setup_permit) =
            setup_permit_id.as_deref()
        {
            Some(
                transaction
                    .query_row(
                        "SELECT CASE purpose WHEN 'setup_discovery' THEN 'trip_setup_discovery' WHEN 'profile_probe' THEN 'trip_setup_probe' WHEN 'runtime_probe' THEN 'trip_runtime_probe' ELSE NULL END
                         FROM trip_setup_permits WHERE id=?1 AND state='issued'",
                        params![setup_permit],
                        |row| row.get(0),
                    )
                    .optional()?
                    .ok_or_else(|| anyhow!("setup role launch permit is stale or consumed"))?,
            )
        } else {
            None
        };
        if setup_validation_cell.as_deref() == Some("trip_runtime_probe") {
            crate::trip::require_runtime_probe_launch_policy_in_connection(
                &transaction,
                &attempt_id,
                config.role,
                config,
            )?;
        }
        crate::trip::require_attempt_ready(&transaction, &attempt_id, setup_permit_id.as_deref())?;
        if !validation_dispatch {
            crate::providers::require_production_capability(config)?;
        }
        if validation_dispatch {
            let lifecycle: String = transaction.query_row(
                "SELECT t.lifecycle FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
                params![attempt_id],
                |row| row.get(0),
            )?;
            if lifecycle != "validation" {
                bail!("validation role launch permit requires current validation task lifecycle")
            }
        }
        if !validation_dispatch {
            require_supported_capability_in(&transaction, config, &self.compatibility_bundles)?;
            if switch_intent.is_some() {
                crate::trip::current_task_profile_authority_with_bundles(
                    &transaction,
                    &context.task_id,
                    config.role,
                    context.settings_revision,
                    config,
                    &self.compatibility_bundles,
                )?;
            } else {
                crate::trip::require_attempt_profile_launch_with_bundles(
                    &transaction,
                    &attempt_id,
                    config.role,
                    context.settings_revision,
                    config,
                    &self.compatibility_bundles,
                )?;
            }
        }
        let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role=?2 AND lane_id=?3 AND status IN ('launch_reserved','running','stopping'))",params![attempt_id,config.role.to_string(),context.lane_id],|row|row.get(0))?;
        if active {
            bail!("role authority was concurrently reserved")
        }
        let conflicts:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND status IN ('launch_reserved','running','stopping') AND ((?2='implementer' AND role IN ('explorer','plan_reviewer','code_reviewer','final_verifier')) OR (?2 IN ('explorer','plan_reviewer','code_reviewer','final_verifier') AND role='implementer')))",params![attempt_id,config.role.to_string()],|row|row.get(0))?;
        if conflicts {
            bail!("writer and reviewer authority changed before reservation")
        }
        let (phase, status, task_id, lifecycle, attention, workflow_version, workflow_hash): (String, String, String, String, String, String, String) = transaction.query_row(
            "SELECT a.phase,a.status,a.task_id,t.lifecycle,t.attention,a.workflow_version,a.workflow_hash FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )?;
        if workflow_version != crate::workflow_resources::WORKFLOW_VERSION
            || workflow_hash != crate::workflow_resources::workflow_hash()
        {
            bail!("workflow revision changed before authoritative reservation")
        }
        let held_validation = validation_dispatch
            && status == "held"
            && lifecycle == "validation"
            && attention == "paused";
        if status != "running" && !held_validation {
            bail!("attempt is not dispatchable")
        }
        let typed_setup_phase_authority =
            match (setup_permit_id.as_deref(), setup_validation_cell.as_deref()) {
                (Some(setup_permit), Some("trip_setup_discovery" | "trip_setup_probe")) => {
                    validate_typed_setup_launch(
                        &transaction,
                        &attempt_id,
                        context,
                        config,
                        setup_permit,
                    )?
                }
                _ => false,
            };
        let allowed = typed_setup_phase_authority
            || match config.role {
                RoleKind::Manager => matches!(
                    phase.as_str(),
                    "planning"
                        | "plan_review"
                        | "awaiting_plan_approval"
                        | "awaiting_implementation_authorization"
                        | "implementation"
                        | "code_review"
                        | "checks"
                        | "final_review"
                        | "manager_handoff"
                ),
                RoleKind::Explorer => matches!(
                    phase.as_str(),
                    "planning" | "implementation" | "code_review" | "checks" | "final_review"
                ),
                RoleKind::PlanReviewer => phase == "plan_review",
                RoleKind::Implementer => phase == "implementation",
                RoleKind::CodeReviewer => phase == "code_review",
                RoleKind::FinalReviewer => phase == "final_review",
            };
        if !allowed {
            bail!("role phase changed before authoritative reservation")
        }
        let explorer_decision: Option<String> = if config.role == RoleKind::Explorer
            && !validation_dispatch
        {
            transaction.query_row(
                "SELECT d.id FROM trip_explorer_decisions d
                 WHERE d.attempt_id=?1 AND d.activated=1 AND d.outcome_json IS NULL
                   AND d.candidate_hash IS (SELECT candidate_hash FROM attempts WHERE id=?1)
                   AND (d.role_generation_id IS NULL OR d.role_generation_id IN (
                     SELECT si.old_generation_id FROM launch_permits lp JOIN switch_intents si ON si.id=lp.switch_intent_id
                     WHERE lp.id=?2))
                 ORDER BY d.created_at DESC LIMIT 1",
                params![attempt_id,context.permit_id], |row| row.get(0),
            ).optional()?
        } else {
            None
        };
        if config.role == RoleKind::Explorer && !validation_dispatch && explorer_decision.is_none()
        {
            bail!("Explorer reservation no longer matches a current activated decision")
        }
        if config.role == RoleKind::Implementer {
            let freeze_active: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1 AND state IN ('reserved','capturing','recovery_required'))",params![attempt_id],|row|row.get(0))?;
            if freeze_active {
                bail!("writer reservation is fenced by an active or unresolved snapshot freeze")
            }
            let reviewed = if setup_permit_id.is_none() {
                crate::trip::reviewed_parallel_lane_count(&transaction, &attempt_id)?
            } else {
                None
            };
            if !validation_dispatch {
                if switch_intent.is_none() {
                    if context.lane_id == "default" {
                        if reviewed.is_some() {
                            require_default_integration_launch(&transaction, &attempt_id)?;
                        }
                    } else {
                        require_initial_lane_launch(
                            &transaction,
                            &attempt_id,
                            &context.lane_id,
                            &context.workspace,
                            Some(&context.permit_id),
                        )?;
                    }
                }
            }
        }
        let held:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM tasks t WHERE t.id=?1 AND (t.attention IN ('pause_requested','needs_recovery') OR (t.attention='paused' AND NOT ?3) OR EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?2 AND c.kind IN ('pause_now','pause_after_role','cancel') AND c.state IN ('requested','draining')) OR (?4='manager' AND EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?2 AND c.kind='setup_manager_change' AND c.state='held')) OR EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?2 AND c.kind IN ('manager_stop','manager_change') AND c.state NOT IN ('finished','cancelled','superseded','rejected') AND (json_extract(c.payload_json,'$.switch_intent_id') IS NULL OR json_extract(c.payload_json,'$.switch_intent_id') IS NOT ?5))))",params![task_id,attempt_id,held_validation,config.role.to_string(),switch_intent],|row|row.get(0))?;
        if held {
            bail!("attempt became held before authoritative reservation")
        }
        let provider = config.provider.to_string();
        if !role_capacity_available(
            &transaction,
            &provider,
            config.role,
            None,
            Some(&context.permit_id),
        )? {
            bail!("role capacity filled before authoritative reservation")
        }
        let generation:i64=transaction.query_row("SELECT COALESCE(MAX(generation),0)+1 FROM role_generations WHERE attempt_id=?1 AND role=?2",params![attempt_id,config.role.to_string()],|row|row.get(0))?;
        if let Some(fresh_resume_rejection) = fresh_resume_rejection {
            let setup_permit_id = setup_permit_id.as_deref().ok_or_else(|| {
                anyhow!("fresh rejected-session dispatch requires a typed setup permit")
            })?;
            let setup_validation_cell = setup_validation_cell.as_deref().ok_or_else(|| {
                anyhow!("fresh rejected-session dispatch requires a typed setup validation cell")
            })?;
            reserve_typed_setup_fresh_route(
                &transaction,
                fresh_resume_rejection,
                context,
                config,
                &capability_key,
                &attempt_id,
                setup_permit_id,
                setup_validation_cell,
                &now,
            )?;
        }
        if let Some(receipt) = browser_receipt {
            let authority = serde_json::json!({
                "attempt_id":attempt_id,
                "task_id":task_id,
                "role":config.role,
                "settings_revision":context.settings_revision,
                "permit_id":context.permit_id,
                "setup_permit_id":setup_permit_id,
                "validation_dispatch":validation_dispatch,
                "validation_cell":setup_validation_cell,
                "switch_intent_id":switch_intent,
                "lane_id":context.lane_id,
                "phase":phase,
                "attempt_status":status,
                "lifecycle":lifecycle,
                "attention":attention,
                "capability_key":capability_key,
                "launch":config,
            });
            match reserve_browser_launch_receipt_in(&transaction, receipt, authority, &now)? {
                BrowserLaunchReservation::Reserved => {}
                BrowserLaunchReservation::Existing(existing) => {
                    return Ok(BrowserLaunchReservation::Existing(existing));
                }
            }
        }
        transaction.execute("INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at,lane_id) VALUES(?1,?2,?3,?4,?5,?6,'launch_reserved',?7,?8,?8,?9)",params![context.role_generation_id,attempt_id,config.role.to_string(),config.provider.to_string(),generation,context.settings_revision,uuid::Uuid::new_v4().to_string(),now,context.lane_id])?;
        transaction.execute("INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at) VALUES(?1,?2,?3,?4,?5)",params![context.credential_id,context.role_generation_id,auth::hash_secret(&context.token),serde_json::to_string(&role_permissions(config.role))?,now])?;
        transaction.execute("INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,hook_trust_state,created_at,updated_at,workflow_version,workflow_hash,prompt_hash,capability_key,capability_identity_json,setup_permit_id,lane_id,validation_cell) VALUES(?1,?2,?3,'launch_reserved',?4,?5,?6,'pending_observation',?7,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![context.session_id,context.role_generation_id,config.provider.to_string(),serde_json::to_string(config)?,config.executable_version,context.transcript_epoch,now,crate::workflow_resources::WORKFLOW_VERSION,crate::workflow_resources::workflow_hash(),crate::workflow_resources::prompt_hash(config.role),capability_key,capability_identity_json,setup_permit_id,context.lane_id,setup_validation_cell])?;
        if typed_setup_phase_authority {
            crate::trip::require_setup_session_confinement(
                &transaction,
                &context.session_id,
                &context.role_generation_id,
            )?;
        }
        if let Some(decision_id) = explorer_decision {
            transaction.execute(
                "UPDATE trip_explorer_decisions SET role_generation_id=?1 WHERE id=?2 AND outcome_json IS NULL",
                params![context.role_generation_id,decision_id],
            )?;
        }
        transaction.execute("UPDATE launch_permits SET state='consumed',consumed_at=?1 WHERE id=?2 AND state='issued'",params![now,context.permit_id])?;
        transaction.commit()?;
        Ok(BrowserLaunchReservation::Reserved)
    }

    pub fn bind_invocation_input(
        &self,
        session_id: &str,
        prompt: &str,
        review_request: Option<&str>,
    ) -> Result<()> {
        if prompt.trim().is_empty() || prompt.len() > 512 * 1024 {
            bail!("invocation prompt must be non-empty and at most 512 KiB")
        }
        let prompt_hash = json_hash(&prompt)?;
        let connection = self.lock()?;
        let role_prompt_hash: String = connection.query_row(
            "SELECT prompt_hash FROM sessions WHERE id=?1 AND status='launch_reserved'",
            params![session_id],
            |row| row.get(0),
        )?;
        let input = serde_json::json!({"prompt":prompt,"prompt_hash":prompt_hash,"review_request_id":review_request,"workflow_version":crate::workflow_resources::WORKFLOW_VERSION,"workflow_hash":crate::workflow_resources::workflow_hash(),"role_prompt_hash":role_prompt_hash});
        let changed=connection.execute("UPDATE sessions SET invocation_input_json=?1,updated_at=?2 WHERE id=?3 AND status='launch_reserved'",params![input.to_string(),Utc::now().to_rfc3339(),session_id])?;
        if changed != 1 {
            bail!("session launch reservation is stale before prompt binding")
        }
        Ok(())
    }

    pub fn set_transcript_sequence(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        sequence: u64,
    ) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "UPDATE sessions SET transcript_last_sequence = ?1, updated_at = ?2 WHERE id = ?3 AND transcript_epoch=?4",
            params![sequence as i64, Utc::now().to_rfc3339(), session_id, transcript_epoch],
        )?;
        Ok(())
    }

    pub fn verify_attachment_binding(&self, binding: &AttachmentBinding) -> Result<()> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        let connection = self.lock()?;
        let process_json = serde_json::to_string(&binding.process)?;
        let current: bool = connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s
               JOIN role_generations rg ON rg.id=s.role_generation_id
               WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
                 AND s.process_identity_json=?4 AND s.status='running' AND rg.status='running'
             )",
            params![
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                process_json
            ],
            |row| row.get(0),
        )?;
        if !current {
            bail!("terminal attachment is stale, revoked, or no longer running")
        }
        Ok(())
    }

    /// A new service boot never adopts a previous boot's physical terminals or
    /// in-memory leases. This is a durable presentation-only retirement: it
    /// does not address, focus, close, restart, or otherwise mutate cmux.
    pub fn retire_prior_cmux_presentation_boots(&self, service_boot_id: &str) -> Result<usize> {
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let workspaces = transaction.execute(
            "UPDATE cmux_task_workspaces
                SET state='retired',last_error='Presentation belongs to a prior service boot and is audit-only',updated_at=?1
              WHERE service_boot_id<>?2 AND state IN ('opening','open','unknown')",
            params![&now, service_boot_id],
        )?;
        let surfaces = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='retired',
                    attachment_state=CASE WHEN attachment_state='live' THEN 'ended' ELSE attachment_state END,
                    desired_input_state='view_only',
                    actual_input_state=CASE WHEN actual_input_state='control' THEN 'lost' ELSE actual_input_state END,
                    last_error='Presentation belongs to a prior service boot and is audit-only',updated_at=?1
              WHERE service_boot_id<>?2 AND surface_state IN ('opening','open','unknown')",
            params![&now, service_boot_id],
        )?;
        if workspaces + surfaces > 0 {
            transaction.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?2,'service','cmux.prior_boot.retired','cmux_presentation','prior_boots',?3,?4)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    serde_json::json!({"workspaces":workspaces,"surfaces":surfaces}).to_string(),
                    &now,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(workspaces + surfaces)
    }

    /// Reserve the browser operation before any external cmux call.  A replay
    /// returns its recorded outcome; reusing the UUID for a different session
    /// fails rather than creating or focusing another surface.
    pub fn reserve_cmux_view_operation(
        &self,
        operation_id: &str,
        service_boot_id: &str,
        session_id: &str,
    ) -> Result<Option<CmuxViewOutcome>> {
        valid_cmux_uuid("cmux view operation", operation_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux view session", session_id)?;
        let request_hash = cmux_view_request_hash(service_boot_id, session_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_hash, stored_result)) = transaction
            .query_row(
                "SELECT request_hash,result_json FROM operation_receipts
                  WHERE operation_id=?1 AND actor_key='human_control'
                    AND operation_kind='cmux_view'",
                params![operation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if stored_hash != request_hash {
                bail!("cmux view operation ID was reused with different input")
            }
            transaction.commit()?;
            return Ok(Some(serde_json::from_str(&stored_result)?));
        }
        let pending = CmuxViewOutcome {
            state: "pending".to_owned(),
            message:
                "The exact cmux presentation request is reserved; no keyboard lease was requested."
                    .to_owned(),
            retry_available: false,
            surface: None,
            recorded_output: None,
        };
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
             VALUES(?1,'human_control','cmux_view',?2,?3,?4)",
            params![
                operation_id,
                request_hash,
                serde_json::to_string(&pending)?,
                Utc::now().to_rfc3339(),
            ],
        )?;
        transaction.commit()?;
        Ok(None)
    }

    pub fn finish_cmux_view_operation(
        &self,
        operation_id: &str,
        outcome: &CmuxViewOutcome,
    ) -> Result<()> {
        valid_cmux_uuid("cmux view operation", operation_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE operation_receipts SET result_json=?1
              WHERE operation_id=?2 AND actor_key='human_control' AND operation_kind='cmux_view'",
            params![serde_json::to_string(outcome)?, operation_id],
        )?;
        if changed != 1 {
            bail!("missing cmux view operation reservation")
        }
        Ok(())
    }

    /// Read a committed keyboard-control receipt before checking live provider
    /// state.  That makes an identical browser retry replayable even after the
    /// provider has naturally ended, while a changed payload under the same
    /// operation ID remains a hard rejection.
    pub fn cmux_keyboard_control_receipt(
        &self,
        operation_id: &str,
        service_boot_id: &str,
        session_id: &str,
        surface_route_id: &str,
        expected_binding_revision: i64,
        expected_control_revision: i64,
        action: CmuxKeyboardControlAction,
    ) -> Result<Option<CmuxKeyboardControlOutcome>> {
        valid_cmux_uuid("cmux keyboard-control operation", operation_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux keyboard-control session", session_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        if expected_binding_revision <= 0 || expected_control_revision < 0 {
            bail!("cmux keyboard-control revisions are invalid")
        }
        let request_hash = cmux_keyboard_control_request_hash(
            service_boot_id,
            session_id,
            surface_route_id,
            expected_binding_revision,
            expected_control_revision,
            action,
        )?;
        let connection = self.lock()?;
        let stored: Option<(String, String)> = connection
            .query_row(
                "SELECT request_hash,result_json FROM operation_receipts
                  WHERE operation_id=?1 AND actor_key='human_control'
                    AND operation_kind='cmux_set_keyboard_control'",
                params![operation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((stored_hash, stored_result)) = stored else {
            return Ok(None);
        };
        if stored_hash != request_hash {
            bail!("cmux keyboard-control operation ID was reused with different input")
        }
        Ok(Some(serde_json::from_str(&stored_result)?))
    }

    pub fn reserve_cmux_task_workspace(
        &self,
        service_boot_id: &str,
        task_id: &str,
    ) -> Result<(CmuxTaskWorkspace, bool)> {
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        if task_id.trim().is_empty() || task_id.len() > 128 {
            bail!("cmux task workspace task identity is malformed")
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        observe_expired_lost_cmux_leases_for_task(&transaction, service_boot_id, task_id)?;
        if let Some(surface_route_id) =
            pending_lost_cmux_retirement_for_task(&transaction, service_boot_id, task_id)?
        {
            bail!(
                "cmux workspace loss retirement is still pending for exact live surface {surface_route_id}"
            )
        }
        let existing: Option<CmuxTaskWorkspaceRow> = transaction
            .query_row(
                "SELECT id,service_boot_id,task_id,generation,workspace_id,opening_surface_id,state,
                        last_error,created_at,updated_at
                   FROM cmux_task_workspaces
                  WHERE service_boot_id=?1 AND task_id=?2
                    AND state IN ('opening','open','unknown')
                  ORDER BY generation DESC LIMIT 1",
                params![service_boot_id, task_id],
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
                        row.get(9)?,
                    ))
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            transaction.commit()?;
            return Ok((cmux_task_workspace_from_row(existing)?, false));
        }
        let generation: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(generation),0)+1 FROM cmux_task_workspaces
              WHERE service_boot_id=?1 AND task_id=?2",
            params![service_boot_id, task_id],
            |row| row.get(0),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO cmux_task_workspaces(
                 id,service_boot_id,task_id,generation,state,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,'opening',?5,?5)",
            params![&id, service_boot_id, task_id, generation, &now],
        )?;
        transaction.commit()?;
        Ok((
            CmuxTaskWorkspace {
                id,
                service_boot_id: service_boot_id.to_owned(),
                task_id: task_id.to_owned(),
                generation,
                workspace_id: None,
                opening_surface_id: None,
                state: "opening".to_owned(),
                last_error: None,
                created_at: now.clone(),
                updated_at: now,
            },
            true,
        ))
    }

    pub fn cmux_task_workspace(&self, workspace_id: &str) -> Result<CmuxTaskWorkspace> {
        valid_cmux_uuid("cmux task workspace", workspace_id)?;
        let connection = self.lock()?;
        cmux_task_workspace_for_connection(&connection, workspace_id)
    }

    /// Exactly one pending child gets to supply `workspace.create`'s initial
    /// terminal.  Other sessions wait for that durable reservation to settle.
    pub fn claim_cmux_task_workspace_create(
        &self,
        task_workspace_id: &str,
        surface_route_id: &str,
        service_boot_id: &str,
    ) -> Result<bool> {
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_task_workspaces
                SET opening_surface_id=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND state='opening'
                AND opening_surface_id IS NULL",
            params![
                surface_route_id,
                Utc::now().to_rfc3339(),
                task_workspace_id,
                service_boot_id,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Claims the one external `surface.create` boundary for a pending child
    /// of an already-owned workspace.  `opening_surface_id` is deliberately
    /// cleared after each validated create, then reused as a durable in-flight
    /// claim: concurrent View/Take requests can observe the reservation but
    /// cannot issue a second targeted create for the same physical surface.
    pub fn claim_cmux_targeted_surface_create(
        &self,
        task_workspace_id: &str,
        surface_route_id: &str,
        service_boot_id: &str,
    ) -> Result<bool> {
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = cmux_task_workspace_for_connection(&transaction, task_workspace_id)?;
        let surface = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        if workspace.service_boot_id != service_boot_id
            || workspace.state != "open"
            || surface.task_workspace_id != workspace.id
            || surface.service_boot_id != service_boot_id
            || surface.surface_state != "opening"
            || !matches!(surface.attachment_state.as_str(), "pending" | "live")
        {
            bail!("cmux targeted surface is not current for a durable create claim")
        }
        let changed = transaction.execute(
            "UPDATE cmux_task_workspaces
                SET opening_surface_id=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND state='open'
                AND opening_surface_id IS NULL",
            params![
                surface_route_id,
                Utc::now().to_rfc3339(),
                task_workspace_id,
                service_boot_id,
            ],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn reserve_cmux_session_surface(
        &self,
        service_boot_id: &str,
        task_workspace_id: &str,
        binding: &AttachmentBinding,
    ) -> Result<(CmuxSessionSurface, bool)> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = cmux_task_workspace_for_connection(&transaction, task_workspace_id)?;
        if workspace.service_boot_id != service_boot_id
            || !matches!(workspace.state.as_str(), "opening" | "open" | "unknown")
        {
            bail!("cmux task workspace is not current for this service boot")
        }
        observe_expired_lost_cmux_leases_for_binding(
            &transaction,
            service_boot_id,
            binding,
            &process_json,
        )?;
        if let Some(surface_route_id) = pending_lost_cmux_retirement_for_binding(
            &transaction,
            service_boot_id,
            binding,
            &process_json,
        )? {
            bail!(
                "cmux surface loss retirement is still pending for exact live surface {surface_route_id}"
            )
        }
        let existing: Option<CmuxSessionSurfaceRow> = transaction
            .query_row(
                "SELECT id,task_workspace_id,service_boot_id,session_id,role_generation_id,
                        transcript_epoch,process_identity_json,binding_revision,workspace_id,surface_id,
                        surface_state,attachment_state,desired_input_state,actual_input_state,
                        control_revision,applied_revision,last_error,created_at,updated_at
                  FROM cmux_session_surfaces
                  WHERE service_boot_id=?1 AND session_id=?2 AND role_generation_id=?3
                    AND transcript_epoch=?4 AND process_identity_json=?5
                    AND (
                        (surface_state IN ('opening','open')
                         AND attachment_state IN ('pending','live'))
                        OR surface_state='unknown'
                    )
                  ORDER BY binding_revision DESC LIMIT 1",
                params![
                    service_boot_id,
                    &binding.session_id,
                    &binding.role_generation_id,
                    &binding.transcript_epoch,
                    &process_json,
                ],
                |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                        row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                        row.get(10)?, row.get(11)?, row.get(12)?, row.get(13)?, row.get(14)?,
                        row.get(15)?, row.get(16)?, row.get(17)?, row.get(18)?,
                    ))
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            transaction.commit()?;
            return Ok((cmux_session_surface_from_row(existing)?, false));
        }
        let current: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
                  AND s.process_identity_json=?4 AND s.status='running' AND rg.status='running'
             )",
            params![
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
            ],
            |row| row.get(0),
        )?;
        if !current {
            bail!("cmux session surface no longer belongs to the exact live provider binding")
        }
        let binding_revision: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(binding_revision),0)+1 FROM cmux_session_surfaces
              WHERE service_boot_id=?1 AND session_id=?2",
            params![service_boot_id, &binding.session_id],
            |row| row.get(0),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO cmux_session_surfaces(
                 id,task_workspace_id,service_boot_id,session_id,role_generation_id,
                 transcript_epoch,process_identity_json,binding_revision,workspace_id,
                 surface_state,attachment_state,desired_input_state,actual_input_state,
                 control_revision,applied_revision,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'opening','pending','view_only',
                      'view_only',0,0,?10,?10)",
            params![
                &id,
                task_workspace_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                binding_revision,
                workspace.workspace_id,
                &now,
            ],
        )?;
        transaction.commit()?;
        Ok((
            CmuxSessionSurface {
                id,
                task_workspace_id: task_workspace_id.to_owned(),
                service_boot_id: service_boot_id.to_owned(),
                binding: binding.clone(),
                binding_revision,
                workspace_id: workspace.workspace_id,
                surface_id: None,
                surface_state: "opening".to_owned(),
                attachment_state: "pending".to_owned(),
                desired_input_state: "view_only".to_owned(),
                actual_input_state: "view_only".to_owned(),
                control_revision: 0,
                applied_revision: 0,
                last_error: None,
                created_at: now.clone(),
                updated_at: now,
            },
            true,
        ))
    }

    pub fn cmux_session_surface(&self, surface_id: &str) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux session surface", surface_id)?;
        let connection = self.lock()?;
        cmux_session_surface_for_connection(&connection, surface_id)
    }

    pub fn mark_cmux_workspace_and_initial_surface_open(
        &self,
        task_workspace_id: &str,
        surface_route_id: &str,
        service_boot_id: &str,
        workspace_id: &str,
        surface_id: &str,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux workspace", workspace_id)?;
        valid_cmux_uuid("cmux surface", surface_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let workspace_changed = transaction.execute(
            "UPDATE cmux_task_workspaces
                SET workspace_id=?1,opening_surface_id=NULL,state='open',last_error=NULL,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND state='opening'
                AND opening_surface_id=?5",
            params![
                workspace_id,
                &now,
                task_workspace_id,
                service_boot_id,
                surface_route_id,
            ],
        )?;
        if workspace_changed != 1 {
            bail!("cmux task workspace is stale before its exact create result can be recorded")
        }
        let surface_changed = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET workspace_id=?1,surface_id=?2,surface_state='open',last_error=NULL,updated_at=?3
              WHERE id=?4 AND task_workspace_id=?5 AND service_boot_id=?6
                AND surface_state='opening' AND attachment_state IN ('pending','live')",
            params![
                workspace_id,
                surface_id,
                &now,
                surface_route_id,
                task_workspace_id,
                service_boot_id,
            ],
        )?;
        if surface_changed != 1 {
            bail!("cmux initial session surface is stale before its exact create result can be recorded")
        }
        let result = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn mark_cmux_targeted_surface_open(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        workspace_id: &str,
        surface_id: &str,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux workspace", workspace_id)?;
        valid_cmux_uuid("cmux surface", surface_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace_row: Option<CmuxTaskWorkspaceRow> = transaction
            .query_row(
                "SELECT id,service_boot_id,task_id,generation,workspace_id,opening_surface_id,state,
                        last_error,created_at,updated_at
                   FROM cmux_task_workspaces
                  WHERE service_boot_id=?1 AND workspace_id=?2 AND state='open'",
                params![service_boot_id, workspace_id],
                |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                        row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                    ))
                },
            )
            .optional()?;
        let workspace = workspace_row
            .map(cmux_task_workspace_from_row)
            .transpose()?
            .ok_or_else(|| anyhow!("cmux targeted surface has no current owned workspace"))?;
        let changed = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET workspace_id=?1,surface_id=?2,surface_state='open',last_error=NULL,updated_at=?3
              WHERE id=?4 AND task_workspace_id=?5 AND service_boot_id=?6
                AND surface_state='opening' AND attachment_state IN ('pending','live')",
            params![
                workspace_id,
                surface_id,
                Utc::now().to_rfc3339(),
                surface_route_id,
                workspace.id,
                service_boot_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux targeted surface is stale before its exact create result can be recorded")
        }
        let claim_cleared = transaction.execute(
            "UPDATE cmux_task_workspaces
                SET opening_surface_id=NULL,updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND state='open'
                AND opening_surface_id=?4",
            params![
                Utc::now().to_rfc3339(),
                &workspace.id,
                service_boot_id,
                surface_route_id,
            ],
        )?;
        if claim_cleared != 1 {
            bail!("cmux targeted surface lost its durable create claim")
        }
        let result = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        transaction.commit()?;
        Ok(result)
    }

    /// An uncertain create has no safe surface identity.  It stays durable and
    /// blocks automatic retry.  A live child remains connected but cannot be
    /// focused, discarded, recreated, or used for keyboard control.
    pub fn mark_cmux_workspace_create_unknown(
        &self,
        task_workspace_id: &str,
        surface_route_id: &str,
        service_boot_id: &str,
        error: &str,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let changed = transaction.execute(
            "UPDATE cmux_task_workspaces
                SET state='unknown',last_error=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND state='opening'
                AND opening_surface_id=?5",
            params![
                bounded_cmux_error(error),
                &now,
                task_workspace_id,
                service_boot_id,
                surface_route_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux task workspace is stale before its unknown create result can be recorded")
        }
        let changed = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='unknown',actual_input_state=CASE
                        WHEN actual_input_state='control' THEN 'lost' ELSE actual_input_state END,
                    last_error=?1,updated_at=?2
              WHERE id=?3 AND task_workspace_id=?4 AND service_boot_id=?5
                AND surface_state='opening' AND attachment_state IN ('pending','live')",
            params![
                bounded_cmux_error(error),
                &now,
                surface_route_id,
                task_workspace_id,
                service_boot_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux initial session surface is stale before its unknown create result can be recorded")
        }
        let result = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn mark_cmux_targeted_surface_unknown(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        error: &str,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='unknown',actual_input_state=CASE
                        WHEN actual_input_state='control' THEN 'lost' ELSE actual_input_state END,
                    last_error=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND surface_state='opening'
                AND attachment_state IN ('pending','live')",
            params![
                bounded_cmux_error(error),
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux targeted surface is stale before its unknown create result can be recorded")
        }
        transaction.execute(
            "UPDATE cmux_task_workspaces
                SET opening_surface_id=NULL,updated_at=?1
              WHERE id=(SELECT task_workspace_id FROM cmux_session_surfaces WHERE id=?2)
                AND service_boot_id=?3 AND state='open' AND opening_surface_id=?2",
            params![Utc::now().to_rfc3339(), surface_route_id, service_boot_id],
        )?;
        let result = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        transaction.commit()?;
        Ok(result)
    }

    /// A failed or malformed observation never proves that a terminal went
    /// away. Preserve its durable identity and input state: a View must not
    /// silently release a normal human lease merely because cmux observation
    /// became uncertain. The live attachment is fenced from new control
    /// changes, input, resize, and renewal by its exact-route checks until it
    /// ends or an authenticated discard is allowed.
    pub fn mark_cmux_session_surface_observation_unknown(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        error: &str,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='unknown',last_error=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND surface_state IN ('opening','open')
                AND attachment_state IN ('pending','live')",
            params![
                bounded_cmux_error(error),
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux session surface changed before an uncertain observation could be recorded")
        }
        cmux_session_surface_for_connection(&connection, surface_route_id)
    }

    pub fn fail_cmux_task_workspace_reservation(
        &self,
        task_workspace_id: &str,
        service_boot_id: &str,
        error: &str,
    ) -> Result<()> {
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "UPDATE cmux_task_workspaces
                SET state='failed',last_error=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND state IN ('opening','open')",
            params![
                bounded_cmux_error(error),
                &now,
                task_workspace_id,
                service_boot_id
            ],
        )?;
        transaction.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='failed',attachment_state=CASE
                        WHEN attachment_state='live' THEN 'live' ELSE 'failed' END,
                    actual_input_state=CASE WHEN actual_input_state='control' THEN 'lost'
                        ELSE actual_input_state END,
                    last_error=?1,updated_at=?2
              WHERE task_workspace_id=?3 AND service_boot_id=?4
                AND surface_state IN ('opening','open')",
            params![
                bounded_cmux_error(error),
                &now,
                task_workspace_id,
                service_boot_id
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_cmux_session_surface_reservation(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        error: &str,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='failed',attachment_state=CASE
                        WHEN attachment_state='live' THEN 'live' ELSE 'failed' END,
                    actual_input_state=CASE WHEN actual_input_state='control' THEN 'lost'
                        ELSE actual_input_state END,
                    last_error=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND surface_state='opening'",
            params![
                bounded_cmux_error(error),
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux session surface is stale before its failure can be recorded")
        }
        transaction.execute(
            "UPDATE cmux_task_workspaces
                SET opening_surface_id=NULL,updated_at=?1
              WHERE id=(SELECT task_workspace_id FROM cmux_session_surfaces WHERE id=?2)
                AND service_boot_id=?3 AND state='open' AND opening_surface_id=?2",
            params![Utc::now().to_rfc3339(), surface_route_id, service_boot_id],
        )?;
        let result = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        transaction.commit()?;
        Ok(result)
    }

    /// A valid global tree is the only caller allowed to record presentation
    /// loss. This method never talks to cmux or revokes a lease. A live
    /// attachment remains live until its next control sync returns Retire and
    /// connection cleanup records it ended, or the exact bounded lease expiry
    /// is observed while a later reservation checks this durable state.
    pub fn mark_cmux_task_workspace_lost(
        &self,
        task_workspace_id: &str,
        service_boot_id: &str,
        expected_workspace_id: &str,
    ) -> Result<()> {
        valid_cmux_uuid("cmux task workspace", task_workspace_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux workspace", expected_workspace_id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let changed = transaction.execute(
            "UPDATE cmux_task_workspaces
                SET state='lost',last_error='A validated global cmux tree omitted this exact workspace',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND workspace_id=?4
                AND state IN ('open','unknown')",
            params![&now, task_workspace_id, service_boot_id, expected_workspace_id],
        )?;
        if changed != 1 {
            bail!("cmux task workspace changed before validated loss could be recorded")
        }
        transaction.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='lost',attachment_state=CASE
                        WHEN attachment_state='live' THEN 'live'
                        WHEN attachment_state='pending' THEN 'failed'
                        ELSE attachment_state END,
                    desired_input_state='view_only',actual_input_state='lost',
                    last_error='A validated global cmux tree omitted this task workspace',updated_at=?1
              WHERE task_workspace_id=?2 AND service_boot_id=?3
                AND surface_state IN ('opening','open','unknown')",
            params![&now, task_workspace_id, service_boot_id],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','cmux.workspace.lost','cmux_task_workspace',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                task_workspace_id,
                serde_json::json!({"workspace_id": expected_workspace_id, "source": "validated_system_tree"}).to_string(),
                &now,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_cmux_session_surface_lost(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        expected_workspace_id: &str,
        expected_surface_id: &str,
    ) -> Result<()> {
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux workspace", expected_workspace_id)?;
        valid_cmux_uuid("cmux surface", expected_surface_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state='lost',attachment_state=CASE
                        WHEN attachment_state='live' THEN 'live'
                        WHEN attachment_state='pending' THEN 'failed'
                        ELSE attachment_state END,
                    desired_input_state='view_only',actual_input_state='lost',
                    last_error='A validated global cmux tree omitted this exact terminal surface',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND workspace_id=?4 AND surface_id=?5
                AND surface_state IN ('opening','open','unknown')",
            params![
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
                expected_workspace_id,
                expected_surface_id,
            ],
        )?;
        if changed != 1 {
            bail!("cmux session surface changed before validated loss could be recorded")
        }
        Ok(())
    }

    pub fn mark_cmux_session_surface_connected(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
    ) -> Result<CmuxSessionSurface> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        if binding_revision <= 0 {
            bail!("cmux binding revision must be positive")
        }
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_session_surfaces
                SET attachment_state='live',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND session_id=?4
                AND role_generation_id=?5 AND transcript_epoch=?6 AND process_identity_json=?7
                AND binding_revision=?8 AND surface_state IN ('opening','open')
                AND attachment_state='pending'",
            params![
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                binding_revision,
            ],
        )?;
        if changed != 1 {
            bail!("cmux session surface is stale, retired, or not awaiting this exact attachment")
        }
        cmux_session_surface_for_connection(&connection, surface_route_id)
    }

    pub fn mark_cmux_session_surface_failed(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
    ) -> Result<()> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state=CASE WHEN surface_state='unknown' THEN 'unknown' ELSE 'retired' END,
                    attachment_state='failed',desired_input_state='view_only',
                    actual_input_state=CASE WHEN actual_input_state='control' THEN 'lost'
                        ELSE actual_input_state END,
                    last_error='the cmux client could not establish its exact mode-neutral attachment',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND session_id=?4
                AND role_generation_id=?5 AND transcript_epoch=?6 AND process_identity_json=?7
                AND binding_revision=?8 AND attachment_state='pending'",
            params![
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                binding_revision,
            ],
        )?;
        if changed != 1 {
            bail!("cmux session surface is stale or no longer awaiting this exact attachment")
        }
        Ok(())
    }

    /// Ended and failed clients retire their row without addressing the old
    /// physical terminal.  A later explicit View can reserve a new binding
    /// revision in the still-owned task workspace.
    pub fn mark_cmux_session_surface_ended(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
    ) -> Result<()> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        connection.execute(
            "UPDATE cmux_session_surfaces
                SET surface_state=CASE
                        WHEN surface_state IN ('unknown','lost') THEN surface_state ELSE 'retired' END,
                    attachment_state='ended',desired_input_state='view_only',
                    actual_input_state=CASE WHEN actual_input_state='lost' THEN 'lost'
                        ELSE 'view_only' END,
                    updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND session_id=?4
                AND role_generation_id=?5 AND transcript_epoch=?6 AND process_identity_json=?7
                AND binding_revision=?8 AND attachment_state='live'",
            params![
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                binding_revision,
            ],
        )?;
        Ok(())
    }

    pub fn cmux_attachment_control_directive(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
    ) -> Result<CmuxAttachmentControlDirective> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let surface = self.cmux_session_surface(surface_route_id)?;
        if surface.service_boot_id != service_boot_id
            || surface.binding != *binding
            || surface.binding_revision != binding_revision
        {
            bail!("cmux attachment control sync does not match the exact current surface binding")
        }
        if surface.surface_state == "unknown" && surface.attachment_state == "live" {
            return Ok(CmuxAttachmentControlDirective {
                disposition: CmuxAttachmentControlDisposition::Hold,
                desired_input_state: surface.desired_input_state,
                desired_revision: surface.control_revision,
                applied_revision: surface.applied_revision,
                actual_input_state: surface.actual_input_state,
                last_error: surface.last_error,
            });
        }
        if surface.surface_state == "opening" && surface.attachment_state == "live" {
            return Ok(CmuxAttachmentControlDirective {
                disposition: CmuxAttachmentControlDisposition::Hold,
                desired_input_state: "view_only".to_owned(),
                desired_revision: surface.control_revision,
                applied_revision: surface.applied_revision,
                actual_input_state: surface.actual_input_state,
                last_error: surface.last_error,
            });
        }
        if surface.surface_state != "open" || surface.attachment_state != "live" {
            return Ok(CmuxAttachmentControlDirective {
                disposition: CmuxAttachmentControlDisposition::Retire,
                desired_input_state: "view_only".to_owned(),
                desired_revision: surface.control_revision,
                applied_revision: surface.applied_revision,
                actual_input_state: surface.actual_input_state,
                last_error: surface.last_error,
            });
        }
        Ok(CmuxAttachmentControlDirective {
            disposition: CmuxAttachmentControlDisposition::Apply,
            desired_input_state: surface.desired_input_state,
            desired_revision: surface.control_revision,
            applied_revision: surface.applied_revision,
            actual_input_state: surface.actual_input_state,
            last_error: surface.last_error,
        })
    }

    /// Acknowledgements are conditional on the exact desired revision, so an
    /// old control socket cannot overwrite a newer browser decision.
    pub fn acknowledge_cmux_attachment_control(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
        desired_revision: i64,
        actual_input_state: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        self.update_cmux_attachment_control(
            surface_route_id,
            service_boot_id,
            binding,
            binding_revision,
            desired_revision,
            actual_input_state,
            error,
            false,
        )
    }

    /// Rejected late operations reconcile the currently applied revision after
    /// clearing their connection-local lease. If durable observation has
    /// already classified this route as unknown or lost, retain that bounded
    /// causal evidence instead of replacing it with cleanup diagnostics.
    pub fn reconcile_cmux_attachment_control(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
        desired_revision: i64,
        actual_input_state: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        self.update_cmux_attachment_control(
            surface_route_id,
            service_boot_id,
            binding,
            binding_revision,
            desired_revision,
            actual_input_state,
            error,
            true,
        )
    }

    fn update_cmux_attachment_control(
        &self,
        surface_route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        binding_revision: i64,
        desired_revision: i64,
        actual_input_state: &str,
        error: Option<&str>,
        preserve_causal_observation: bool,
    ) -> Result<bool> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        if !matches!(
            actual_input_state,
            "view_only" | "control" | "blocked" | "lost"
        ) {
            bail!("invalid cmux actual input state")
        }
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_session_surfaces
                SET actual_input_state=?1,applied_revision=?2,
                    last_error=CASE
                        WHEN ?3 != 0 AND surface_state IN ('unknown','lost') THEN last_error
                        ELSE ?4
                    END,
                    updated_at=?5
              WHERE id=?6 AND service_boot_id=?7 AND session_id=?8
                AND role_generation_id=?9 AND transcript_epoch=?10 AND process_identity_json=?11
                AND binding_revision=?12 AND control_revision=?2
                AND attachment_state='live'",
            params![
                actual_input_state,
                desired_revision,
                if preserve_causal_observation { 1 } else { 0 },
                error.map(bounded_cmux_error),
                Utc::now().to_rfc3339(),
                surface_route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                binding_revision,
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn set_cmux_keyboard_control(
        &self,
        operation_id: &str,
        service_boot_id: &str,
        session_id: &str,
        surface_route_id: &str,
        expected_binding_revision: i64,
        expected_control_revision: i64,
        action: CmuxKeyboardControlAction,
    ) -> Result<CmuxKeyboardControlOutcome> {
        valid_cmux_uuid("cmux keyboard-control operation", operation_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux keyboard-control session", session_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        if expected_binding_revision <= 0 || expected_control_revision < 0 {
            bail!("cmux keyboard-control revisions are invalid")
        }
        let request_hash = cmux_keyboard_control_request_hash(
            service_boot_id,
            session_id,
            surface_route_id,
            expected_binding_revision,
            expected_control_revision,
            action,
        )?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_hash, stored_result)) = transaction
            .query_row(
                "SELECT request_hash,result_json FROM operation_receipts
                  WHERE operation_id=?1 AND actor_key='human_control'
                    AND operation_kind='cmux_set_keyboard_control'",
                params![operation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if stored_hash != request_hash {
                bail!("cmux keyboard-control operation ID was reused with different input")
            }
            transaction.commit()?;
            return Ok(serde_json::from_str(&stored_result)?);
        }
        let surface = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        if surface.service_boot_id != service_boot_id
            || surface.binding.session_id != session_id
            || surface.binding_revision != expected_binding_revision
            || surface.control_revision != expected_control_revision
            || surface.surface_state != "open"
            || !matches!(surface.attachment_state.as_str(), "pending" | "live")
        {
            bail!("cmux keyboard-control request is stale or does not belong to the exact current surface")
        }
        let desired_input_state = action.desired_input_state();
        let next_revision = expected_control_revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("cmux control revision overflow"))?;
        let now = Utc::now().to_rfc3339();
        let changed = transaction.execute(
            "UPDATE cmux_session_surfaces
                SET desired_input_state=?1,control_revision=?2,last_error=NULL,updated_at=?3
              WHERE id=?4 AND service_boot_id=?5 AND session_id=?6
                AND binding_revision=?7 AND control_revision=?8 AND surface_state='open'
                AND attachment_state IN ('pending','live')",
            params![
                desired_input_state,
                next_revision,
                &now,
                surface_route_id,
                service_boot_id,
                session_id,
                expected_binding_revision,
                expected_control_revision,
            ],
        )?;
        if changed != 1 {
            bail!("cmux keyboard-control request changed before it could be committed")
        }
        let surface = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        let outcome = CmuxKeyboardControlOutcome {
            state: "pending".to_owned(),
            message: match action {
                CmuxKeyboardControlAction::Acquire => {
                    "Keyboard control is pending on the existing authenticated cmux attachment; no lease secret was returned to the browser."
                }
                CmuxKeyboardControlAction::Release => {
                    "Release is pending on the existing authenticated cmux attachment; it will return to view-only without stopping the provider."
                }
            }
            .to_owned(),
            surface,
        };
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
             VALUES(?1,'human_control','cmux_set_keyboard_control',?2,?3,?4)",
            params![
                operation_id,
                request_hash,
                serde_json::to_string(&outcome)?,
                &now,
            ],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human','cmux.control.desired','cmux_session_surface',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                operation_id,
                surface_route_id,
                serde_json::json!({
                    "binding_revision": expected_binding_revision,
                    "control_revision": next_revision,
                    "desired_input_state": desired_input_state,
                })
                .to_string(),
                &now,
            ],
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Discard can retire only an uncertain route and, if necessary, its
    /// uncertain task workspace.  A live child blocks both cases; no hidden
    /// second surface can be created while that connection remains usable.
    pub fn discard_unknown_cmux_session_surface(
        &self,
        operation_id: &str,
        service_boot_id: &str,
        surface_route_id: &str,
        binding: &AttachmentBinding,
    ) -> Result<CmuxSessionSurface> {
        valid_cmux_uuid("cmux discard operation", operation_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux session surface", surface_route_id)?;
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        let request_hash =
            cmux_session_surface_discard_request_hash(service_boot_id, surface_route_id, binding)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_hash, stored_result)) = transaction
            .query_row(
                "SELECT request_hash,result_json FROM operation_receipts
                  WHERE operation_id=?1 AND actor_key='human_control'
                    AND operation_kind='cmux_session_surface_unknown_discard'",
                params![operation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if stored_hash != request_hash {
                bail!("cmux unknown-discard operation ID was reused with different input")
            }
            transaction.commit()?;
            return Ok(serde_json::from_str(&stored_result)?);
        }
        let current: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
                  AND s.process_identity_json=?4 AND s.status='running' AND rg.status='running'
             )",
            params![
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
            ],
            |row| row.get(0),
        )?;
        if !current {
            bail!(
                "unknown cmux session surface no longer belongs to the exact live provider binding"
            )
        }
        let surface = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        if surface.service_boot_id != service_boot_id || surface.binding != *binding {
            bail!("unknown cmux session surface does not match the exact current binding")
        }
        let workspace =
            cmux_task_workspace_for_connection(&transaction, &surface.task_workspace_id)?;
        if surface.attachment_state == "live" {
            bail!("an unknown cmux reservation cannot be discarded while its child attachment is live")
        }
        if surface.surface_state != "unknown" && workspace.state != "unknown" {
            bail!("cmux session surface is not an unknown reservation")
        }
        if workspace.state == "unknown" {
            let live_children: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM cmux_session_surfaces
                  WHERE task_workspace_id=?1 AND attachment_state='live')",
                params![&workspace.id],
                |row| row.get(0),
            )?;
            if live_children {
                bail!("an unknown cmux task workspace cannot be discarded while any child attachment is live")
            }
        }
        let now = Utc::now().to_rfc3339();
        if workspace.state == "unknown" {
            transaction.execute(
                "UPDATE cmux_task_workspaces
                    SET state='retired',last_error='Unknown workspace reservation discarded by an authenticated human',updated_at=?1
                  WHERE id=?2 AND service_boot_id=?3 AND state='unknown'",
                params![&now, &workspace.id, service_boot_id],
            )?;
            transaction.execute(
                "UPDATE cmux_session_surfaces
                    SET surface_state='retired',attachment_state=CASE
                            WHEN attachment_state='live' THEN 'live' ELSE 'failed' END,
                        desired_input_state='view_only',actual_input_state=CASE
                            WHEN actual_input_state='control' THEN 'lost' ELSE actual_input_state END,
                        last_error='Unknown task-workspace reservation discarded by an authenticated human',updated_at=?1
                  WHERE task_workspace_id=?2 AND service_boot_id=?3 AND surface_state='unknown'",
                params![&now, &workspace.id, service_boot_id],
            )?;
        } else {
            let changed = transaction.execute(
                "UPDATE cmux_session_surfaces
                    SET surface_state='retired',attachment_state='failed',desired_input_state='view_only',
                        actual_input_state=CASE WHEN actual_input_state='control' THEN 'lost'
                            ELSE actual_input_state END,
                        last_error='Unknown surface reservation discarded by an authenticated human',updated_at=?1
                  WHERE id=?2 AND service_boot_id=?3 AND surface_state='unknown'
                    AND attachment_state IN ('pending','failed','ended')",
                params![&now, surface_route_id, service_boot_id],
            )?;
            if changed != 1 {
                bail!("cmux unknown surface is no longer discardable")
            }
        }
        let result = cmux_session_surface_for_connection(&transaction, surface_route_id)?;
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
             VALUES(?1,'human_control','cmux_session_surface_unknown_discard',?2,?3,?4)",
            params![
                operation_id,
                request_hash,
                serde_json::to_string(&result)?,
                &now,
            ],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human','cmux.surface.unknown_discarded','cmux_session_surface',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                operation_id,
                surface_route_id,
                serde_json::json!({
                    "task_workspace_id": surface.task_workspace_id,
                    "surface_action": "none",
                    "automatic_retry": false,
                })
                .to_string(),
                &now,
            ],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    /// Reserves one cmux route for one immutable live attachment. Repeated
    /// dashboard opens return the outstanding route instead of creating a
    /// second surface. An ended route is deliberately not reused here: cmux
    /// may have returned that surface to a human shell after detach.
    pub fn reserve_cmux_attachment_route(
        &self,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        mode: CmuxAttachmentMode,
    ) -> Result<(CmuxAttachmentRoute, bool)> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<CmuxRouteRow> = transaction
            .query_row(
                "SELECT id,service_boot_id,session_id,role_generation_id,transcript_epoch,
                        process_identity_json,attachment_mode,workspace_id,surface_id,surface_state,
                        attachment_state,resume_state,last_error,created_at,updated_at
                   FROM cmux_attachment_routes
                  WHERE service_boot_id=?1 AND session_id=?2 AND role_generation_id=?3
                    AND transcript_epoch=?4 AND process_identity_json=?5 AND attachment_mode=?6
                    AND ((surface_state='unknown' AND attachment_state IN ('pending','live','failed','ended'))
                      OR (surface_state IN ('opening','open') AND attachment_state IN ('pending','live')))
                  ORDER BY created_at DESC,rowid DESC LIMIT 1",
                params![
                    service_boot_id,
                    &binding.session_id,
                    &binding.role_generation_id,
                    &binding.transcript_epoch,
                    &process_json,
                    mode.as_str(),
                ],
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
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                        row.get(14)?,
                    ))
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            transaction.commit()?;
            return Ok((cmux_route_from_row(existing)?, false));
        }
        let current: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
                  AND s.process_identity_json=?4 AND s.status='running' AND rg.status='running'
             )",
            params![
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
            ],
            |row| row.get(0),
        )?;
        if !current {
            bail!("terminal attachment is stale, revoked, or no longer running")
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        transaction.execute(
            "INSERT INTO cmux_attachment_routes(
                 id,service_boot_id,session_id,role_generation_id,transcript_epoch,
                 process_identity_json,attachment_mode,surface_state,attachment_state,resume_state,
                 created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,'opening','pending','pending',?8,?8)",
            params![
                &id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                mode.as_str(),
                &now,
            ],
        )?;
        transaction.commit()?;
        Ok((
            CmuxAttachmentRoute {
                id,
                service_boot_id: service_boot_id.to_owned(),
                binding: binding.clone(),
                mode,
                workspace_id: None,
                surface_id: None,
                surface_state: "opening".to_owned(),
                attachment_state: "pending".to_owned(),
                resume_state: "pending".to_owned(),
                last_error: None,
                created_at: now.clone(),
                updated_at: now,
            },
            true,
        ))
    }

    pub fn cmux_attachment_route(&self, route_id: &str) -> Result<CmuxAttachmentRoute> {
        valid_cmux_uuid("cmux attachment route", route_id)?;
        let connection = self.lock()?;
        cmux_attachment_route_for_connection(&connection, route_id)
    }

    pub fn mark_cmux_surface_open(
        &self,
        route_id: &str,
        service_boot_id: &str,
        workspace_id: &str,
        surface_id: &str,
    ) -> Result<CmuxAttachmentRoute> {
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        valid_cmux_uuid("cmux workspace", workspace_id)?;
        valid_cmux_uuid("cmux surface", surface_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_attachment_routes
                SET workspace_id=?1,surface_id=?2,surface_state='open',updated_at=?3
              WHERE id=?4 AND service_boot_id=?5 AND surface_state='opening'",
            params![
                workspace_id,
                surface_id,
                Utc::now().to_rfc3339(),
                route_id,
                service_boot_id
            ],
        )?;
        if changed != 1 {
            bail!("cmux route is stale before its surface association can be recorded")
        }
        cmux_attachment_route_for_connection(&connection, route_id)
    }

    pub fn set_cmux_resume_state(
        &self,
        route_id: &str,
        service_boot_id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<CmuxAttachmentRoute> {
        if !matches!(state, "attachment_only" | "unavailable") {
            bail!("invalid cmux resume state")
        }
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_attachment_routes
                SET resume_state=?1,last_error=?2,updated_at=?3
              WHERE id=?4 AND service_boot_id=?5 AND surface_state='open'",
            params![
                state,
                error.map(bounded_cmux_error),
                Utc::now().to_rfc3339(),
                route_id,
                service_boot_id
            ],
        )?;
        if changed != 1 {
            bail!("cmux route is stale before its resume metadata can be recorded")
        }
        cmux_attachment_route_for_connection(&connection, route_id)
    }

    pub fn fail_cmux_attachment_route(
        &self,
        route_id: &str,
        service_boot_id: &str,
        surface_state: &str,
        error: &str,
    ) -> Result<CmuxAttachmentRoute> {
        if !matches!(surface_state, "failed" | "unknown") {
            bail!("invalid cmux route failure state")
        }
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_attachment_routes
                SET surface_state=CASE
                        WHEN attachment_state='live' THEN 'unknown'
                        ELSE ?1
                    END,
                    attachment_state=CASE
                        WHEN attachment_state='live' THEN 'live'
                        WHEN ?1='failed' THEN 'failed'
                        ELSE attachment_state
                    END,
                    last_error=?2,updated_at=?3
              WHERE id=?4 AND service_boot_id=?5 AND surface_state='opening'",
            params![
                surface_state,
                bounded_cmux_error(error),
                Utc::now().to_rfc3339(),
                route_id,
                service_boot_id
            ],
        )?;
        if changed != 1 {
            bail!("cmux route is stale before its failure can be recorded")
        }
        cmux_attachment_route_for_connection(&connection, route_id)
    }

    /// Retires a create result whose cmux surface identity was never known.
    /// This deliberately does not address or close any cmux surface: the exact
    /// live session/process binding and an unidentified, non-live unknown route
    /// must still match before a human can clear the reservation.
    pub fn discard_unknown_cmux_attachment_route(
        &self,
        operation_id: &str,
        service_boot_id: &str,
        route_id: &str,
        binding: &AttachmentBinding,
    ) -> Result<CmuxAttachmentRoute> {
        valid_cmux_uuid("cmux discard operation", operation_id)?;
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        let process_json = serde_json::to_string(&binding.process)?;
        let request_hash = cmux_unknown_discard_request_hash(service_boot_id, route_id, binding)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((stored_hash, stored_result)) = transaction
            .query_row(
                "SELECT request_hash,result_json FROM operation_receipts
                  WHERE operation_id=?1 AND actor_key='human_control'
                    AND operation_kind='cmux_route_unknown_discard'",
                params![operation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            if stored_hash != request_hash {
                bail!("cmux discard operation ID was reused with different input")
            }
            return Ok(serde_json::from_str(&stored_result)?);
        }
        let current: bool = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
                  AND s.process_identity_json=?4 AND s.status='running' AND rg.status='running'
             )",
            params![
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
            ],
            |row| row.get(0),
        )?;
        if !current {
            bail!("unknown cmux route no longer belongs to the exact live session binding")
        }
        let reason = "Unknown cmux create reservation discarded by an authenticated human; no surface was addressed or closed";
        let now = Utc::now().to_rfc3339();
        let changed = transaction.execute(
            "UPDATE cmux_attachment_routes
                SET surface_state='failed',attachment_state='failed',resume_state='unavailable',
                    last_error=?1,updated_at=?2
              WHERE id=?3 AND service_boot_id=?4 AND session_id=?5
                AND role_generation_id=?6 AND transcript_epoch=?7 AND process_identity_json=?8
                AND surface_state='unknown' AND attachment_state IN ('pending','failed','ended')
                AND workspace_id IS NULL AND surface_id IS NULL",
            params![
                reason,
                &now,
                route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
            ],
        )?;
        if changed != 1 {
            bail!("cmux route is no longer a discardable non-live unknown create reservation")
        }
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human','cmux.route.unknown_discarded','cmux_attachment_route',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                operation_id,
                route_id,
                serde_json::json!({
                    "session_id": binding.session_id,
                    "role_generation_id": binding.role_generation_id,
                    "transcript_epoch": binding.transcript_epoch,
                    "process": binding.process,
                    "surface_action": "none",
                    "automatic_retry": false,
                }).to_string(),
                &now,
            ],
        )?;
        let result = cmux_attachment_route_for_connection(&transaction, route_id)?;
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
             VALUES(?1,'human_control','cmux_route_unknown_discard',?2,?3,?4)",
            params![
                operation_id,
                request_hash,
                serde_json::to_string(&result)?,
                &now,
            ],
        )?;
        transaction.commit()?;
        Ok(result)
    }

    /// Replays an already committed discard without requiring the provider
    /// process to remain live. A missing receipt grants no authority; callers
    /// must then perform the full current-process checks before mutation.
    pub fn cmux_unknown_discard_receipt(
        &self,
        operation_id: &str,
        service_boot_id: &str,
        route_id: &str,
        binding: &AttachmentBinding,
    ) -> Result<Option<CmuxAttachmentRoute>> {
        valid_cmux_uuid("cmux discard operation", operation_id)?;
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        let request_hash = cmux_unknown_discard_request_hash(service_boot_id, route_id, binding)?;
        let connection = self.lock()?;
        let receipt = connection
            .query_row(
                "SELECT request_hash,result_json FROM operation_receipts
                  WHERE operation_id=?1 AND actor_key='human_control'
                    AND operation_kind='cmux_route_unknown_discard'",
                params![operation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((stored_hash, stored_result)) = receipt else {
            return Ok(None);
        };
        if stored_hash != request_hash {
            bail!("cmux discard operation ID was reused with different input")
        }
        Ok(Some(serde_json::from_str(&stored_result)?))
    }

    /// Atomically claims one pending route after the CLI supplied its exact
    /// binding on the same control connection. Callers claim before acquiring
    /// input authority and mark only this exact route ended if attachment
    /// establishment later fails; lookup alone is never authority.
    pub fn mark_cmux_attachment_connected(
        &self,
        route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
        mode: CmuxAttachmentMode,
    ) -> Result<()> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_attachment_routes
                SET attachment_state='live',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND session_id=?4
                AND role_generation_id=?5 AND transcript_epoch=?6
                AND process_identity_json=?7 AND attachment_mode=?8
                AND surface_state IN ('opening','open')
                AND attachment_state='pending'",
            params![
                Utc::now().to_rfc3339(),
                route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
                mode.as_str(),
            ],
        )?;
        if changed != 1 {
            bail!("cmux attachment route is stale, replaced, or not awaiting this exact client binding")
        }
        Ok(())
    }

    /// A cmux-launched client could not establish the exact route binding.
    /// Keep the old surface untouched, but permit a later dashboard action to
    /// create a fresh owned surface rather than injecting into this one.
    pub fn mark_cmux_attachment_failed(
        &self,
        route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
    ) -> Result<()> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE cmux_attachment_routes
                SET attachment_state='failed',last_error='the cmux client could not establish its exact attachment binding',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND session_id=?4
                AND role_generation_id=?5 AND transcript_epoch=?6
                AND process_identity_json=?7 AND attachment_state='pending'",
            params![
                Utc::now().to_rfc3339(), route_id, service_boot_id,
                &binding.session_id, &binding.role_generation_id,
                &binding.transcript_epoch, &process_json,
            ],
        )?;
        if changed != 1 {
            bail!("cmux attachment route is stale or no longer awaiting this exact client binding")
        }
        Ok(())
    }

    pub fn mark_cmux_attachment_ended(
        &self,
        route_id: &str,
        service_boot_id: &str,
        binding: &AttachmentBinding,
    ) -> Result<()> {
        binding
            .validate_route_identifiers()
            .map_err(|error| anyhow!(error))?;
        valid_cmux_uuid("cmux attachment route", route_id)?;
        valid_cmux_uuid("cmux service boot", service_boot_id)?;
        let process_json = serde_json::to_string(&binding.process)?;
        let connection = self.lock()?;
        connection.execute(
            "UPDATE cmux_attachment_routes
                SET attachment_state='ended',updated_at=?1
              WHERE id=?2 AND service_boot_id=?3 AND session_id=?4
                AND role_generation_id=?5 AND transcript_epoch=?6
                AND process_identity_json=?7 AND attachment_state='live'",
            params![
                Utc::now().to_rfc3339(),
                route_id,
                service_boot_id,
                &binding.session_id,
                &binding.role_generation_id,
                &binding.transcript_epoch,
                &process_json,
            ],
        )?;
        Ok(())
    }

    /// A final transcript read is safe only for the original, quiescent
    /// process binding while its PTY capture is still draining or is complete.
    /// This deliberately does not authorize any interactive operation.
    pub fn attachment_transcript_access(
        &self,
        binding: &AttachmentBinding,
    ) -> Result<AttachmentTranscriptAccess> {
        let connection = self.lock()?;
        let process_json = serde_json::to_string(&binding.process)?;
        let state: Option<(String, String, String, bool, i64)> = connection
            .query_row(
                "SELECT s.status,rg.status,s.capture_state,
                        s.transcript_last_sequence,
                        COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
                   FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                  WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
                    AND s.process_identity_json=?4",
                params![
                    &binding.session_id,
                    &binding.role_generation_id,
                    &binding.transcript_epoch,
                    process_json
                ],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(4)?,
                        row.get(3)?,
                    ))
                },
            )
            .optional()?;
        match state {
            Some((session, generation, _, _, _))
                if session == "running" && generation == "running" =>
            {
                Ok(AttachmentTranscriptAccess::Live)
            }
            Some((session, generation, capture, quiescent, _))
                if session == "exited"
                    && generation == "exited"
                    && quiescent
                    && capture == "capturing" =>
            {
                Ok(AttachmentTranscriptAccess::CaptureDraining)
            }
            Some((session, generation, capture, quiescent, sequence))
                if session == "exited"
                    && generation == "exited"
                    && quiescent
                    && capture == "complete" =>
            {
                Ok(AttachmentTranscriptAccess::CaptureComplete {
                    sequence: sequence.max(0) as u64,
                })
            }
            _ => bail!("terminal attachment is stale, revoked, or cannot drain captured output"),
        }
    }

    pub fn record_capture_failure(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        reason: &str,
    ) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "UPDATE sessions SET capture_state = 'failed', capture_error = ?1, updated_at = ?2 WHERE id = ?3 AND transcript_epoch=?4",
            params![reason, Utc::now().to_rfc3339(), session_id, transcript_epoch],
        )?;
        Ok(())
    }

    pub fn mark_capture_complete(
        &self,
        session_id: &str,
        transcript_epoch: &str,
        process_json: &str,
        output_tail: Option<&str>,
    ) -> Result<bool> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE sessions SET capture_state='complete',capture_error=NULL,
                    exit_json=CASE WHEN exit_json IS NULL OR ?1 IS NULL
                                        OR COALESCE(json_extract(exit_json,'$.success'),0)=1
                                   THEN exit_json
                                   ELSE json_set(exit_json,'$.output_tail',?1) END,
                    updated_at=?2
             WHERE id=?3 AND transcript_epoch=?4 AND process_identity_json=?5
               AND capture_state='capturing'",
            params![
                output_tail,
                Utc::now().to_rfc3339(),
                session_id,
                transcript_epoch,
                process_json
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn operation_receipt(
        &self,
        operation_id: &str,
        actor_key: &str,
        operation_kind: &str,
        request_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        let connection = self.lock()?;
        let receipt = connection.query_row(
            "SELECT request_hash, result_json FROM operation_receipts WHERE operation_id = ?1 AND actor_key = ?2 AND operation_kind = ?3",
            params![operation_id, actor_key, operation_kind],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()?;
        let Some((stored_hash, result)) = receipt else {
            return Ok(None);
        };
        if stored_hash != request_hash {
            bail!("operation ID was already used with different input")
        }
        Ok(Some(serde_json::from_str(&result)?))
    }

    pub(crate) fn browser_launch_receipt(
        &self,
        operation_id: &str,
        operation_kind: &str,
        input_hash: &str,
    ) -> Result<Option<serde_json::Value>> {
        let connection = self.lock()?;
        let receipt: Option<String> = connection
            .query_row(
                "SELECT result_json FROM operation_receipts WHERE operation_id=?1
                 AND actor_key='human_control' AND operation_kind=?2",
                params![operation_id, operation_kind],
                |row| row.get(0),
            )
            .optional()?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };
        let audit: String = connection
            .query_row(
                "SELECT detail_json FROM audit_events WHERE operation_id=?1
                 AND event_code='browser.launch.reserved'
                 AND json_extract(detail_json,'$.operation_kind')=?2
                 ORDER BY rowid DESC LIMIT 1",
                params![operation_id, operation_kind],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                anyhow!("browser launch receipt lacks its immutable reservation audit")
            })?;
        let audit: serde_json::Value = serde_json::from_str(&audit)?;
        if audit.get("input_hash").and_then(serde_json::Value::as_str) != Some(input_hash) {
            bail!("operation ID was already used with different input")
        }
        Ok(Some(serde_json::from_str(&receipt)?))
    }

    pub(crate) fn finalize_browser_launch_receipt(
        &self,
        operation_id: &str,
        operation_kind: &str,
        input_hash: &str,
        success: Option<serde_json::Value>,
        error: Option<String>,
    ) -> Result<()> {
        let result = match (success, error) {
            (Some(result), None) => result,
            (None, Some(reason)) => serde_json::json!({"state":"rejected","reason":reason}),
            _ => bail!("browser launch receipt must have exactly one outcome"),
        };
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pending =
            browser_launch_receipt_in(&transaction, operation_id, operation_kind, input_hash)?
                .ok_or_else(|| anyhow!("browser launch receipt is missing"))?;
        if pending.get("state").and_then(serde_json::Value::as_str) != Some("reserved") {
            bail!("browser launch receipt is already finalized")
        }
        let changed = transaction.execute(
            "UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2
             AND actor_key='human_control' AND operation_kind=?3",
            params![result.to_string(), operation_id, operation_kind],
        )?;
        if changed != 1 {
            bail!("browser launch receipt is missing or changed")
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn reject_browser_launch_without_reservation(
        &self,
        operation_id: &str,
        operation_kind: &str,
        input_hash: &str,
        entity_id: &str,
        reason: &str,
    ) -> Result<()> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if browser_launch_receipt_in(&transaction, operation_id, operation_kind, input_hash)?
            .is_some()
        {
            transaction.commit()?;
            return Ok(());
        }
        reserve_browser_launch_receipt_in(
            &transaction,
            BrowserLaunchReceipt {
                operation_id,
                operation_kind,
                input_hash,
                entity_id,
            },
            serde_json::json!({"preflight_rejected":true}),
            &Utc::now().to_rfc3339(),
        )?;
        transaction.execute(
            "UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2
             AND actor_key='human_control' AND operation_kind=?3",
            params![
                serde_json::json!({"state":"rejected","reason":reason}).to_string(),
                operation_id,
                operation_kind,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reserve_validation(
        &self,
        request: &ValidationLaunchRequest,
        request_hash: &str,
        project_id: &str,
        repository_path: &str,
        repository_identity: &str,
        base_revision: &str,
        task_id: &str,
        attempt_id: &str,
        context_id: &str,
        config_id: &str,
        role_generation_id: &str,
        credential_id: &str,
        credential_hash: &str,
        session_id: &str,
        transcript_epoch: &str,
        launch: &LaunchConfig,
    ) -> Result<()> {
        self.require_execution_unheld("validation launch reservations")?;
        let capability_identity = crate::providers::capability_identity(launch)?;
        crate::providers::require_current_capability_identity_with_bundles(
            &capability_identity,
            &launch.cwd,
            &self.compatibility_bundles,
        )?;
        let capability_key = crate::providers::capability_identity_key(&capability_identity)?;
        let capability_identity_json = serde_json::to_string(&capability_identity)?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = transaction.query_row(
            "SELECT request_hash FROM operation_receipts WHERE operation_id = ?1 AND actor_key = 'human_control' AND operation_kind = 'capability_launch'",
            params![request.operation_id], |row| row.get(0),
        ).optional()?;
        if let Some(existing_hash) = existing {
            if existing_hash != request_hash {
                bail!("operation ID was already used with different input")
            }
            bail!("validation launch is already reserved; inspect its recorded session instead of launching again")
        }
        if !role_capacity_available(
            &transaction,
            &request.provider.to_string(),
            request.role,
            None,
            None,
        )? {
            bail!("role capacity is full for capability validation")
        }
        transaction.execute(
            "INSERT INTO projects(id, display_name, repository_path, repository_identity, base_revision, queue_paused, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)
             ON CONFLICT(repository_identity) DO UPDATE SET repository_path = excluded.repository_path, base_revision = excluded.base_revision, updated_at = excluded.updated_at",
            params![project_id, format!("Capability {}", request.cell), repository_path, repository_identity, base_revision, now],
        )?;
        let resolved_project: String = transaction.query_row(
            "SELECT id FROM projects WHERE repository_identity = ?1",
            params![repository_identity],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO tasks(id, project_id, title, description, acceptance_criteria_json, priority, manual_order, lifecycle, attention, version, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, '[]', 0, 0, 'in_progress', 'none', 1, ?5, ?5)",
            params![task_id, resolved_project, format!("Capability validation {}", request.cell), "Disposable provider capability validation; never a production task", now],
        )?;
        transaction.execute(
            "INSERT INTO attempts(id, task_id, context_id, phase, base_revision, configuration_revision, status, created_at, updated_at, workflow_version, workflow_hash)
             VALUES(?1, ?2, ?3, 'plan_review', ?4, 1, 'capability_validation', ?5, ?5, ?6, ?7)",
            params![attempt_id, task_id, context_id, base_revision, now, crate::workflow_resources::WORKFLOW_VERSION, crate::workflow_resources::workflow_hash()],
        )?;
        let launch_json = serde_json::to_string(launch)?;
        transaction.execute(
            "INSERT INTO config_revisions(id, attempt_id, revision, config_json, created_at) VALUES(?1, ?2, 1, ?3, ?4)",
            params![config_id, attempt_id, launch_json, now],
        )?;
        transaction.execute(
            "INSERT INTO role_generations(id, attempt_id, role, provider, generation, config_revision, status, authority_generation, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, 1, 1, 'launch_reserved', ?5, ?6, ?6)",
            params![role_generation_id, attempt_id, request.role.to_string(), request.provider.to_string(), uuid::Uuid::new_v4().to_string(), now],
        )?;
        transaction.execute(
            "INSERT INTO role_credentials(id, role_generation_id, token_hash, permissions_json, created_at) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![credential_id, role_generation_id, credential_hash, serde_json::to_string(&role_permissions(request.role))?, now],
        )?;
        transaction.execute(
            "INSERT INTO sessions(id, role_generation_id, provider, validation_cell, status, launch_config_json, executable_version, transcript_epoch, hook_trust_state, created_at, updated_at, workflow_version, workflow_hash, prompt_hash, capability_key, capability_identity_json)
             VALUES(?1, ?2, ?3, ?4, 'launch_reserved', ?5, ?6, ?7, 'pending_observation', ?8, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![session_id, role_generation_id, request.provider.to_string(), request.cell, launch_json, launch.executable_version, transcript_epoch, now, crate::workflow_resources::WORKFLOW_VERSION, crate::workflow_resources::workflow_hash(), crate::workflow_resources::prompt_hash(request.role),capability_key,capability_identity_json],
        )?;
        let codex_implementer = request.provider == crate::domain::Provider::Codex
            && request.role == RoleKind::Implementer;
        let capability_status = "unverified";
        let capability_gaps = if codex_implementer {
            serde_json::json!([
                "live implementer confinement not yet exercised",
                "native resume not yet exercised",
                "delivered native PermissionRequest decision not yet exercised"
            ])
        } else {
            serde_json::json!([
                "live reviewer confinement not yet exercised",
                "native resume not yet exercised"
            ])
        };
        transaction.execute(
            "INSERT OR IGNORE INTO capabilities(id, provider, executable_version, role, mode, config_hash, status, gaps_json, checked_at)
             VALUES(?1, ?2, ?3, ?4, 'interactive_pty_reviewer', ?5, ?6, ?7, ?8)",
            params![uuid::Uuid::new_v4().to_string(), request.provider.to_string(), launch.executable_version, request.role.to_string(), capability_key,
                capability_status, capability_gaps.to_string(), now],
        )?;
        let pending = serde_json::json!({"state": "launch_reserved", "session_id": session_id});
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id, actor_key, operation_kind, request_hash, result_json, created_at)
             VALUES(?1, 'human_control', 'capability_launch', ?2, ?3, ?4)",
            params![request.operation_id, request_hash, pending.to_string(), now],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id, operation_id, actor_kind, event_code, entity_kind, entity_id, detail_json, created_at)
             VALUES(?1, ?2, 'human', 'capability.launch.reserved', 'session', ?3, ?4, ?5)",
            params![uuid::Uuid::new_v4().to_string(), request.operation_id, session_id, pending.to_string(), now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finalize_validation_receipt(
        &self,
        operation_id: &str,
        result: &serde_json::Value,
    ) -> Result<()> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE operation_receipts SET result_json = ?1 WHERE operation_id = ?2 AND actor_key = 'human_control' AND operation_kind = 'capability_launch'",
            params![serde_json::to_string(result)?, operation_id],
        )?;
        if changed != 1 {
            bail!("missing validation launch receipt")
        }
        Ok(())
    }

    pub fn reserve_workflow_validation(
        &self,
        session: &str,
        cell: &str,
        operation: &str,
        request_hash: &str,
    ) -> Result<()> {
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed=tx.execute("UPDATE sessions SET validation_cell=?1 WHERE id=?2 AND status='launch_reserved' AND validation_cell IS NULL",params![cell,session])?;
        if changed != 1 {
            bail!("workflow validation session reservation is stale")
        }
        let pending = serde_json::json!({"state":"launch_reserved","session_id":session});
        tx.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','workflow_capability_launch',?2,?3,?4)",params![operation,request_hash,pending.to_string(),Utc::now().to_rfc3339()])?;
        tx.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'human','capability.workflow_launch.reserved','session',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),operation,session,serde_json::json!({"cell":cell}).to_string(),Utc::now().to_rfc3339()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn finalize_workflow_validation(
        &self,
        operation: &str,
        result: &serde_json::Value,
    ) -> Result<()> {
        let connection = self.lock()?;
        let changed=connection.execute("UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2 AND actor_key='human_control' AND operation_kind='workflow_capability_launch'",params![result.to_string(),operation])?;
        if changed != 1 {
            bail!("missing workflow validation receipt")
        }
        Ok(())
    }

    pub fn native_session_id(&self, session_id: &str) -> Result<Option<String>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT native_session_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow!("unknown session {session_id}"))
    }

    pub fn mark_interrupt_requested(&self, session_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE sessions SET status = 'interrupt_requested',
                    interrupt_requested_at=COALESCE(interrupt_requested_at,?1),updated_at = ?1
             WHERE id = ?2 AND status IN ('launch_reserved','running','recovery_required')",
            params![now, session_id],
        )?;
        if changed != 1 {
            bail!("session is not running")
        }
        transaction.execute(
            "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)
             WHERE role_generation_id=(SELECT role_generation_id FROM sessions WHERE id=?2)",
            params![now, session_id],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            session_id,
            "session interrupt invalidated permission response delivery",
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn interrupt_deadline_candidates(&self) -> Result<Vec<InterruptDeadlineReceipt>> {
        let connection = self.lock()?;
        let deadline = (Utc::now()
            - chrono::Duration::seconds(crate::supervisor::GRACEFUL_STOP_SECONDS))
        .to_rfc3339();
        let mut statement = connection.prepare(
            "SELECT s.id,rg.id,s.transcript_epoch,s.process_identity_json,rg.attempt_id,a.task_id,
                    s.interrupt_requested_at
             FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id
             WHERE s.status='interrupt_requested' AND s.interrupt_requested_at IS NOT NULL
               AND s.interrupt_requested_at <= ?1
             ORDER BY s.interrupt_requested_at,s.id",
        )?;
        let rows = statement
            .query_map(params![deadline], |row| {
                Ok(InterruptDeadlineReceipt {
                    session_id: row.get(0)?,
                    generation_id: row.get(1)?,
                    transcript_epoch: row.get(2)?,
                    process_identity_json: row.get(3)?,
                    attempt_id: row.get(4)?,
                    task_id: row.get(5)?,
                    interrupt_requested_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(crate) fn mark_interrupt_deadline_recovery(
        &self,
        receipt: &InterruptDeadlineReceipt,
        process_observation: &serde_json::Value,
        exact_process_live: bool,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let reason = if exact_process_live {
            "the exact managed provider process remained live after the durable graceful-stop deadline"
        } else {
            "the durable graceful-stop deadline elapsed before operating-system evidence could prove exact process quiescence"
        };
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE sessions SET status='recovery_required',launch_state='delivery_unknown',
                    launch_error=?1,readiness_state='unknown',updated_at=?2
             WHERE id=?3 AND role_generation_id=?4 AND transcript_epoch=?5
               AND process_identity_json=?6 AND status='interrupt_requested'
               AND interrupt_requested_at=?7
               AND EXISTS(
                   SELECT 1
                     FROM role_generations current_generation
                     JOIN attempts current_attempt
                       ON current_attempt.id=current_generation.attempt_id
                     JOIN tasks current_task ON current_task.id=current_attempt.task_id
                    WHERE current_generation.id=?4
                      AND current_attempt.id=?8 AND current_task.id=?9
                      AND current_attempt.status NOT IN ('cancelled','done','failed')
                      AND current_task.lifecycle NOT IN ('cancelled','done')
               )",
            params![
                reason,
                now,
                receipt.session_id,
                receipt.generation_id,
                receipt.transcript_epoch,
                receipt.process_identity_json,
                receipt.interrupt_requested_at,
                receipt.attempt_id,
                receipt.task_id
            ],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, receipt.attempt_id],
        )?;
        transaction.execute(
            "UPDATE tasks SET version=version+CASE WHEN attention='needs_recovery' THEN 0 ELSE 1 END,
                    attention='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, receipt.task_id],
        )?;
        transaction.execute(
            "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2
               AND state IN ('reserved','launching','running','stopping')",
            params![now, receipt.attempt_id],
        )?;
        transaction.execute(
            "UPDATE controls SET state='recovery_required',
                    payload_json=json_set(payload_json,'$.failure',?1,'$.next_action',?2),updated_at=?3
             WHERE attempt_id=?4 AND state IN ('requested','draining','switching','waiting_safe_boundary')",
            params![
                reason,
                "Retry graceful stop or force stop the exact managed process after identity verification.",
                now,
                receipt.attempt_id
            ],
        )?;
        transaction.execute(
            "UPDATE switch_intents SET state='recovery_required',updated_at=?1
             WHERE attempt_id=?2 AND state IN ('stopping_old','ready_for_dispatch')",
            params![now, receipt.attempt_id],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            &receipt.session_id,
            "graceful stop exceeded its exact process deadline",
        )?;
        transaction.execute(
            "INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
             SELECT ?1,?2,?3,'attention_required',?4,?5,?6,?6
             WHERE NOT EXISTS(
                 SELECT 1 FROM recovery_records WHERE session_id=?2 AND state='attention_required'
                   AND json_extract(detail_json,'$.kind')='graceful_stop_deadline'
                   AND json_extract(detail_json,'$.interrupt_requested_at')=?7
             )",
            params![
                uuid::Uuid::new_v4().to_string(),
                receipt.session_id,
                receipt.attempt_id,
                receipt.process_identity_json,
                serde_json::json!({
                    "kind":"graceful_stop_deadline",
                    "reason":reason,
                    "role_generation_id":receipt.generation_id,
                    "transcript_epoch":receipt.transcript_epoch,
                    "interrupt_requested_at":receipt.interrupt_requested_at,
                    "process_observation":process_observation,
                    "exact_process_live":exact_process_live,
                    "automatic_signal":false,
                    "automatic_force_stop":false,
                    "replacement_allowed_after_quiescence":true
                }).to_string(),
                now,
                receipt.interrupt_requested_at
            ],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','session.graceful_stop.deadline_elapsed','session',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                receipt.session_id,
                serde_json::json!({
                    "role_generation_id":receipt.generation_id,
                    "transcript_epoch":receipt.transcript_epoch,
                    "attempt_id":receipt.attempt_id,
                    "interrupt_requested_at":receipt.interrupt_requested_at,
                    "process_observation":process_observation,
                    "exact_process_live":exact_process_live,
                    "automatic_force_stop":false
                }).to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn record_permanent_resume_rejection(
        &self,
        session_id: &str,
        category: &str,
        reason: &str,
        attempted: Option<&PreparedResumeIdentity>,
        compatibility: Option<&crate::provider_compatibility::CompatibilityExplanation>,
    ) -> Result<bool> {
        let allowed = [
            "frozen_runtime_identity_changed",
            "profile_authority_changed",
            "native_history_unavailable",
            "hook_trust_unavailable",
            "invocation_provenance_missing",
            "resume_spent",
            "stale_generation",
            "stale_review_or_scope",
            "authority_consumed",
            "provider_compatibility_unsupported",
            "provider_compatibility_contract_changed",
            "provider_compatibility_invalid_manifest",
        ];
        if !allowed.contains(&category) {
            bail!("invalid permanent resume rejection category")
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(
            String,
            String,
            i64,
            Option<String>,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
        )> = transaction
            .query_row(
                "SELECT s.role_generation_id,s.transcript_epoch,s.resume_count,s.capability_key,
                        rg.attempt_id,rg.role,rg.lane_id,rg.config_revision,s.setup_permit_id,
                        (SELECT r.id FROM review_requests r WHERE r.session_id=s.id
                           AND r.role_generation_id=rg.id AND r.delivery_state='delivered'
                         ORDER BY r.created_at DESC LIMIT 1),
                        a.phase,a.candidate_hash,a.plan_hash
                 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE s.id=?1 AND s.status='exited'
                   AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1",
                params![session_id],
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
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            generation_id,
            transcript_epoch,
            resume_count,
            frozen_key,
            attempt_id,
            role,
            lane_id,
            config_revision,
            setup_permit_id,
            review_request_id,
            attempt_phase,
            candidate_hash,
            plan_hash,
        )) = row
        else {
            transaction.commit()?;
            return Ok(false);
        };
        let observed_key = attempted.map(|identity| identity.capability_key.clone());
        let observed_identity = attempted.map(PreparedResumeIdentity::audit_value);
        let frozen_contract_hash: Option<String> = transaction.query_row(
            "SELECT json_extract(capability_identity_json,'$.compatibility.effective_hash') FROM sessions WHERE id=?1",
            params![session_id], |row| row.get(0))?;
        let active_typed_setup_session: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sessions s
                JOIN role_generations rg ON rg.id=s.role_generation_id
                JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
                JOIN trip_setup_operations setup ON setup.id=permit.setup_operation_id
                WHERE s.id=?1 AND s.validation_cell IN ('trip_setup_discovery','trip_setup_probe')
                  AND permit.attempt_id=rg.attempt_id
                  AND permit.role=rg.role AND permit.settings_revision=rg.config_revision
                  AND ((permit.purpose='setup_discovery'
                        AND s.validation_cell='trip_setup_discovery'
                        AND setup.state='discovery' AND setup.discovery_attempt_id=rg.attempt_id)
                    OR (permit.purpose='profile_probe'
                        AND s.validation_cell='trip_setup_probe'
                        AND setup.state='probing' AND setup.probe_attempt_id=rg.attempt_id))
            )",
            params![session_id],
            |row| row.get(0),
        )?;
        // Failure isolation uses durable bindings even after a permit is consumed; resume admission does not.
        let runtime_probe_admission: Option<String> = transaction
            .query_row(
                "SELECT admission.id
                 FROM sessions s
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts attempt ON attempt.id=rg.attempt_id
                 JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
                 JOIN trip_setup_operations setup ON setup.id=permit.setup_operation_id
                   AND setup.validation_task_id=attempt.task_id
                 JOIN trip_runtime_probes probe ON probe.session_id=s.id
                   AND probe.attempt_id=rg.attempt_id AND probe.role=rg.role
                   AND probe.profile_hash=permit.profile_hash
                   AND probe.fixture_project_id=permit.fixture_project_id
                   AND probe.fixture_repository_identity=permit.fixture_repository_identity
                 JOIN trip_runtime_admissions admission ON admission.id=probe.admission_id
                   AND admission.project_id=setup.project_id
                 WHERE s.id=?1 AND s.validation_cell='trip_runtime_probe'
                   AND permit.attempt_id=rg.attempt_id AND permit.role=rg.role
                   AND permit.settings_revision=rg.config_revision
                   AND permit.purpose='runtime_probe'
                   AND permit.setup_operation_id=attempt.setup_operation_id",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        let same_profile_authority: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE s.id=?1 AND rg.id=?2 AND a.id=(SELECT latest.id FROM attempts latest
                     WHERE latest.task_id=t.id ORDER BY latest.created_at DESC LIMIT 1)
                   AND t.archived_at IS NULL AND t.lifecycle IN ('in_progress','validation')
                   AND t.attention NOT IN ('needs_recovery','pause_requested')
                   AND rg.status='exited'
                   AND NOT EXISTS(SELECT 1 FROM role_generations newer WHERE newer.attempt_id=a.id
                     AND newer.role=rg.role AND newer.lane_id=rg.lane_id AND newer.generation>rg.generation)
                   AND ((s.validation_cell IN ('trip_setup_discovery','trip_setup_probe')
                         AND EXISTS(SELECT 1 FROM trip_setup_permits permit
                           JOIN trip_setup_operations setup ON setup.id=permit.setup_operation_id
                           JOIN trip_setup_profile_selections selection
                             ON selection.setup_operation_id=setup.id AND selection.role=permit.role
                           JOIN role_settings setting ON setting.task_id=t.id AND setting.role=rg.role
                             AND setting.revision=rg.config_revision
                           WHERE permit.id=s.setup_permit_id AND permit.state='issued'
                             AND permit.attempt_id=a.id AND permit.role=rg.role
                             AND permit.settings_revision=rg.config_revision
                             AND selection.selection_state='selected'
                             AND selection.profile_hash=permit.profile_hash
                             AND setting.effective_generation_id=rg.id
                             AND ((permit.purpose='setup_discovery'
                                   AND s.validation_cell='trip_setup_discovery'
                                   AND permit.approved_action='bounded_inventory_and_contained_read'
                                   AND permit.nonce IS NULL AND setup.state='discovery'
                                   AND setup.discovery_attempt_id=a.id)
                               OR (permit.purpose='profile_probe'
                                   AND s.validation_cell='trip_setup_probe'
                                   AND permit.approved_action='nonce_only_profile_invocation'
                                   AND permit.nonce IS NOT NULL AND permit.nonce!=''
                                   AND setup.state='probing' AND setup.probe_attempt_id=a.id))))
                     OR (s.validation_cell IS NULL
                         AND EXISTS(SELECT 1 FROM trip_attempt_profiles profile
                           WHERE profile.attempt_id=a.id AND profile.role=rg.role
                             AND profile.settings_revision=rg.config_revision)
                         AND (EXISTS(SELECT 1 FROM role_settings rs
                               WHERE rs.task_id=t.id AND rs.role=rg.role
                                 AND rs.revision=rg.config_revision
                                 AND rs.effective_generation_id=rg.id)
                           OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                               SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id
                                 AND lg.effective_generation_id=rg.id)))))
                   AND rg.role!='final_verifier'
                   AND (rg.role!='explorer' OR EXISTS(
                         SELECT 1 FROM trip_explorer_decisions decision
                          WHERE decision.attempt_id=a.id AND decision.activated=1
                            AND decision.outcome_json IS NULL
                            AND decision.role_generation_id=rg.id
                            AND decision.candidate_hash IS a.candidate_hash))
             )",
            params![session_id, generation_id],
            |row| row.get(0),
        )?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM audit_events WHERE event_code='session.resume.rejected'
               AND entity_kind='session' AND entity_id=?1
               AND json_extract(detail_json,'$.role_generation_id')=?2
               AND json_extract(detail_json,'$.transcript_epoch')=?3
               AND CAST(json_extract(detail_json,'$.resume_count') AS INTEGER)=?4
               AND json_extract(detail_json,'$.category')=?5)",
            params![
                session_id,
                generation_id,
                transcript_epoch,
                resume_count,
                category
            ],
            |row| row.get(0),
        )?;
        let recorded = !exists;
        if recorded {
            transaction.execute(
                "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?2,'service','session.resume.rejected','session',?3,?4,?5)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                    session_id,
                    serde_json::json!({
                        "session_id":session_id,"role_generation_id":generation_id,
                        "transcript_epoch":transcript_epoch,"resume_count":resume_count,
                        "frozen_capability_key":frozen_key,"observed_capability_key":observed_key,
                        "frozen_contract_hash":frozen_contract_hash,
                        "observed_contract_hash":attempted.and_then(|identity| identity.compatibility_hash.as_deref()),
                        "compatibility":compatibility,
                        "observed_identity":observed_identity,
                        "attempt_id":attempt_id,"role":role,"lane_id":lane_id,
                        "config_revision":config_revision,"setup_permit_id":setup_permit_id,
                        "review_request_id":review_request_id,"category":category,"reason":reason,
                        "attempt_phase":attempt_phase,"candidate_hash":candidate_hash,"plan_hash":plan_hash,
                        "same_profile_authority":same_profile_authority,
                    }).to_string(),
                    now
                ],
            )?;
        }
        if let Some(admission_id) = runtime_probe_admission {
            transaction.execute(
                "UPDATE trip_runtime_probes SET state='failed',failure_reason=?1,updated_at=?2
                 WHERE admission_id=?3 AND role=?4 AND attempt_id=?5 AND session_id=?6
                   AND state IN ('running','awaiting_resume')",
                params![
                    format!("{category}: {reason}"),
                    now,
                    admission_id,
                    role,
                    attempt_id,
                    session_id
                ],
            )?;
            crate::trip::refresh_runtime_admission_state(&transaction, &admission_id, &now)?;
        } else if recorded {
            transaction.execute(
                "UPDATE attempts SET status=CASE
                    WHEN ?3 THEN status
                    WHEN status IN ('running','held','restart_parked') THEN 'needs_input'
                    ELSE status END,updated_at=?1 WHERE id=?2",
                params![now, attempt_id, active_typed_setup_session],
            )?;
            transaction.execute(
                "UPDATE tasks SET attention=CASE
                    WHEN ?3 THEN attention
                    WHEN attention IN ('needs_recovery','pause_requested') THEN attention
                    ELSE 'resume_failed' END,
                    version=version+CASE WHEN ?3 OR attention IN ('needs_recovery','pause_requested','resume_failed') THEN 0 ELSE 1 END,
                    updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",
                params![now, attempt_id, active_typed_setup_session],
            )?;
        }
        transaction.commit()?;
        Ok(recorded)
    }

    pub(crate) fn mark_codex_stop_idle_candidate(
        &self,
        receipt: &CodexStopIdleReceipt,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if eligible_codex_stop_idle_reconciliation(&transaction, Some(&receipt.session_id))?
            .as_ref()
            != Some(receipt)
        {
            return Ok(false);
        }
        let changed = transaction.execute(
            "UPDATE sessions SET readiness_state='idle_candidate',updated_at=?1
             WHERE id=?2 AND role_generation_id=?3 AND transcript_epoch=?4
               AND native_session_id=?5 AND status='running'
               AND readiness_state='busy_unresolved_hook_work'",
            params![
                now,
                receipt.session_id,
                receipt.generation_id,
                receipt.transcript_epoch,
                receipt.native_session_id
            ],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','provider.codex_stop_idle.reconciled','session',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                receipt.session_id,
                serde_json::json!({
                    "role_generation_id":receipt.generation_id,
                    "credential_id":receipt.credential_id,
                    "transcript_epoch":receipt.transcript_epoch,
                    "resume_invocation_id":receipt.resume_invocation_id,
                    "invocation_boundary_rowid":receipt.invocation_boundary_rowid,
                    "native_session_id":receipt.native_session_id,
                    "stop_event_rowid":receipt.stop_rowid,
                    "native_process_inventory_verified":true,
                    "completion_inferred":false
                }).to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn claim_setup_retained_first_turn_stop(
        &self,
        receipt: &SetupRetainedFirstTurnStopReceipt,
    ) -> Result<ManagerServiceStopClaim> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if eligible_setup_retained_first_turn_stop(&transaction, Some(&receipt.session_id))?
            .as_ref()
            != Some(receipt)
        {
            let claimed: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM audit_events
                 WHERE event_code='setup.retained_first_turn.stop_claimed'
                   AND entity_kind='session' AND entity_id=?1
                   AND json_extract(detail_json,'$.role_generation_id')=?2
                   AND json_extract(detail_json,'$.transcript_epoch')=?3
                   AND json_extract(detail_json,'$.stop_event_rowid')=?4)",
                params![
                    receipt.session_id,
                    receipt.generation_id,
                    receipt.transcript_epoch,
                    receipt.stop_rowid
                ],
                |row| row.get(0),
            )?;
            return Ok(if claimed {
                ManagerServiceStopClaim::AlreadyRequested
            } else {
                ManagerServiceStopClaim::Stale
            });
        }
        let changed = transaction.execute(
            "UPDATE sessions SET status='interrupt_requested',
                    interrupt_requested_at=COALESCE(interrupt_requested_at,?1),updated_at=?1
             WHERE id=?2 AND role_generation_id=?3 AND transcript_epoch=?4
               AND native_session_id=?5 AND process_identity_json=?6
               AND status='running' AND readiness_state='idle_candidate'",
            params![
                now,
                receipt.session_id,
                receipt.generation_id,
                receipt.transcript_epoch,
                receipt.native_session_id,
                receipt.process_identity_json
            ],
        )?;
        if changed != 1 {
            return Ok(ManagerServiceStopClaim::Stale);
        }
        transaction.execute(
            "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)
             WHERE id=?2 AND role_generation_id=?3 AND revoked_at IS NULL",
            params![now, receipt.credential_id, receipt.generation_id],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            &receipt.session_id,
            "setup retained first-turn completion invalidated permission response delivery",
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','setup.retained_first_turn.stop_claimed','session',?3,?4,?5)",
            params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),receipt.session_id,
                serde_json::json!({"role_generation_id":receipt.generation_id,"credential_id":receipt.credential_id,
                    "transcript_epoch":receipt.transcript_epoch,"native_session_id":receipt.native_session_id,
                    "process_identity_json":receipt.process_identity_json,
                    "stop_event_rowid":receipt.stop_rowid,"setup_permit_id":receipt.setup_permit_id,
                    "timeout_at":(Utc::now() + chrono::Duration::seconds(30)).to_rfc3339(),
                    "resume_spent":false,"replacement_control_created":false}).to_string(),now],
        )?;
        transaction.commit()?;
        Ok(ManagerServiceStopClaim::Claimed)
    }

    pub(crate) fn mark_setup_retained_first_turn_stop_timed_out(
        &self,
        receipt: &SetupRetainedFirstTurnStopTimeoutReceipt,
        process_observation: &serde_json::Value,
        exact_process_live: bool,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if setup_retained_first_turn_stop_timeout_candidate_in(
            &transaction,
            Some(&receipt.session_id),
            &now,
        )?
        .as_ref()
            != Some(receipt)
        {
            return Ok(false);
        }
        let reason = if exact_process_live {
            "retained setup first-turn completion signal was delivered, but the exact provider process did not exit before the bounded deadline"
        } else {
            "retained setup first-turn completion reached its bounded deadline, but operating-system evidence could not establish exact process quiescence"
        };
        let attempt: String = transaction.query_row(
            "SELECT rg.attempt_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1",
            params![receipt.session_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE sessions SET status='recovery_required',launch_state='delivery_unknown',
               launch_error=?1,readiness_state='unknown',updated_at=?2 WHERE id=?3",
            params![reason, now, receipt.session_id],
        )?;
        transaction.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![now, attempt],
        )?;
        transaction.execute(
            "UPDATE tasks SET version=version+CASE WHEN attention='needs_recovery' THEN 0 ELSE 1 END,
               attention='needs_recovery',updated_at=?1
             WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",
            params![now, attempt],
        )?;
        transaction.execute(
            "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
            params![now, attempt],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            &receipt.session_id,
            "retained setup first-turn completion exceeded its bounded exit deadline",
        )?;
        transaction.execute(
            "INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
             SELECT ?1,s.id,rg.attempt_id,'attention_required',s.process_identity_json,?2,?3,?3
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE s.id=?4 AND NOT EXISTS(SELECT 1 FROM recovery_records
               WHERE session_id=?4 AND state='attention_required')",
            params![uuid::Uuid::new_v4().to_string(),serde_json::json!({
                "kind":"setup_retained_first_turn_stop_timeout",
                "reason":reason,
                "process_observation":process_observation,
                "exact_process_live":exact_process_live,
                "automatic_resignal":false,
                "replacement_allowed_after_quiescence":true
            }).to_string(),now,receipt.session_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn mark_setup_retained_first_turn_stop_quiescent(
        &self,
        receipt: &SetupRetainedFirstTurnStopTimeoutReceipt,
        process_observation: &serde_json::Value,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if setup_retained_first_turn_stop_timeout_candidate_in(
            &transaction,
            Some(&receipt.session_id),
            &now,
        )?
        .as_ref()
            != Some(receipt)
        {
            return Ok(false);
        }
        let exit = serde_json::json!({
            "success":true,
            "process_group_quiescent":true,
            "source":"setup_retained_first_turn_stop_timeout_reconciliation",
            "verification":process_observation,
            "observed_at":now
        })
        .to_string();
        let changed = transaction.execute(
            "UPDATE sessions SET status='exited',launch_state='finished',exit_json=?1,
                    interrupt_requested_at=NULL,updated_at=?2
             WHERE id=?3 AND role_generation_id=?4 AND transcript_epoch=?5
               AND native_session_id=?6 AND process_identity_json=?7
               AND status='interrupt_requested' AND readiness_state='idle_candidate'",
            params![
                exit,
                now,
                receipt.session_id,
                receipt.generation_id,
                receipt.transcript_epoch,
                receipt.native_session_id,
                receipt.process_identity_json
            ],
        )?;
        if changed != 1 {
            return Ok(false);
        }
        transaction.execute(
            "UPDATE role_generations SET status='exited',updated_at=?1
             WHERE id=?2 AND status NOT IN ('replaced','revoked')",
            params![now, receipt.generation_id],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            &receipt.session_id,
            "retained setup first-turn process was verified quiescent",
        )?;
        transaction.execute(
            "UPDATE input_leases SET revoked_at=COALESCE(revoked_at,?1),updated_at=?1
             WHERE session_id=?2 AND process_identity_json=?3",
            params![now, receipt.session_id, receipt.process_identity_json],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','setup.retained_first_turn.stop_quiescent','session',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                receipt.session_id,
                serde_json::json!({
                    "role_generation_id":receipt.generation_id,
                    "transcript_epoch":receipt.transcript_epoch,
                    "native_session_id":receipt.native_session_id,
                    "stop_event_rowid":receipt.stop_rowid,
                    "stop_claim_rowid":receipt.claim_rowid,
                    "process_observation":process_observation
                }).to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub(crate) fn setup_retained_first_turn_stop_timeout_candidate(
        &self,
    ) -> Result<Option<SetupRetainedFirstTurnStopTimeoutReceipt>> {
        let connection = self.lock()?;
        setup_retained_first_turn_stop_timeout_candidate_in(
            &connection,
            None,
            &Utc::now().to_rfc3339(),
        )
    }

    pub(crate) fn claim_manager_service_stop(
        &self,
        attempt_id: &str,
        phase: &str,
        receipt: &ManagerServiceStopReceipt,
    ) -> Result<ManagerServiceStopClaim> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = eligible_manager_service_stop(&transaction, attempt_id, phase)?;
        if current.as_ref() != Some(receipt) {
            let already_claimed: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM audit_events
                   WHERE event_code='manager.service_stop.claimed'
                     AND entity_kind='session' AND entity_id=?1
                     AND json_extract(detail_json,'$.attempt_id')=?2
                     AND json_extract(detail_json,'$.phase')=?3
                     AND json_extract(detail_json,'$.role_generation_id')=?4
                     AND json_extract(detail_json,'$.credential_id')=?5
                     AND json_extract(detail_json,'$.transcript_epoch')=?6
                     AND json_extract(detail_json,'$.invocation_boundary_rowid')=?7
                     AND json_extract(detail_json,'$.stop_event_rowid')=?8)",
                params![
                    receipt.session_id,
                    attempt_id,
                    phase,
                    receipt.generation_id,
                    receipt.credential_id,
                    receipt.transcript_epoch,
                    receipt.invocation_boundary_rowid,
                    receipt.stop_rowid
                ],
                |row| row.get(0),
            )?;
            return Ok(if already_claimed {
                ManagerServiceStopClaim::AlreadyRequested
            } else {
                ManagerServiceStopClaim::Stale
            });
        }
        let changed = transaction.execute(
            "UPDATE sessions SET status='interrupt_requested',
                    interrupt_requested_at=COALESCE(interrupt_requested_at,?1),updated_at=?1
             WHERE id=?2 AND role_generation_id=?3 AND transcript_epoch=?4 AND status='running'",
            params![
                now,
                receipt.session_id,
                receipt.generation_id,
                receipt.transcript_epoch
            ],
        )?;
        if changed != 1 {
            return Ok(ManagerServiceStopClaim::Stale);
        }
        transaction.execute(
            "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)
             WHERE role_generation_id=?2",
            params![now, receipt.generation_id],
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            &receipt.session_id,
            "manager service stop invalidated permission response delivery",
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','manager.service_stop.claimed','session',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                receipt.session_id,
                serde_json::json!({
                    "attempt_id":attempt_id,
                    "phase":phase,
                    "role_generation_id":receipt.generation_id,
                    "credential_id":receipt.credential_id,
                    "transcript_epoch":receipt.transcript_epoch,
                    "resume_invocation_id":receipt.resume_invocation_id,
                    "invocation_boundary_rowid":receipt.invocation_boundary_rowid,
                    "stop_event_rowid":receipt.stop_rowid,
                    "task_version":receipt.task_version,
                    "plan_hash":receipt.plan_hash,
                    "candidate_hash":receipt.candidate_hash
                })
                .to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(ManagerServiceStopClaim::Claimed)
    }

    pub fn reserve_session_resume(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
    ) -> Result<()> {
        match self.reserve_session_resume_inner(session_id, epoch, launch, token, None, None)? {
            BrowserLaunchReservation::Reserved => Ok(()),
            BrowserLaunchReservation::Existing(_) => {
                bail!("non-browser session resume unexpectedly found a browser receipt")
            }
        }
    }

    pub(crate) fn reserve_session_resume_with_runtime(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        runtime: &crate::trip::CapabilityRuntime,
    ) -> Result<()> {
        match self.reserve_session_resume_inner(
            session_id,
            epoch,
            launch,
            token,
            Some(runtime),
            None,
        )? {
            BrowserLaunchReservation::Reserved => Ok(()),
            BrowserLaunchReservation::Existing(_) => {
                bail!("non-browser runtime resume unexpectedly found a browser receipt")
            }
        }
    }

    pub(crate) fn reserve_session_resume_with_runtime_and_browser_receipt(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        runtime: &crate::trip::CapabilityRuntime,
        receipt: BrowserLaunchReceipt<'_>,
    ) -> Result<BrowserLaunchReservation> {
        self.reserve_session_resume_inner(
            session_id,
            epoch,
            launch,
            token,
            Some(runtime),
            Some(receipt),
        )
    }

    fn reserve_session_resume_inner(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        runtime: Option<&crate::trip::CapabilityRuntime>,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<BrowserLaunchReservation> {
        self.require_execution_unheld("session resume reservations")?;
        let capability_identity = crate::providers::capability_identity(launch)?;
        crate::providers::require_current_capability_identity_with_bundles(
            &capability_identity,
            &launch.cwd,
            &self.compatibility_bundles,
        )?;
        let capability_key = crate::providers::capability_identity_key(&capability_identity)?;
        let launch_json = serde_json::to_string(launch)?;
        let capability_identity_json = serde_json::to_string(&capability_identity)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = browser_receipt {
            if let Some(existing) = browser_launch_receipt_in(
                &transaction,
                receipt.operation_id,
                receipt.operation_kind,
                receipt.input_hash,
            )? {
                return Ok(BrowserLaunchReservation::Existing(existing));
            }
        }
        let (admission_attempt, setup_permit, attempt_status): (String, Option<String>, String) = transaction.query_row(
            "SELECT rg.attempt_id,s.setup_permit_id,a.status FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id WHERE s.id=?1",
            params![session_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
        )?;
        validate_frozen_capability(
            &transaction,
            session_id,
            &capability_key,
            &launch.cwd,
            &self.compatibility_bundles,
        )?;
        let typed_setup_resume =
            validate_typed_setup_resume(&transaction, session_id, launch, &capability_key)?;
        let runtime_probe_resume = crate::trip::validate_runtime_probe_resume_authority(
            &transaction,
            runtime,
            session_id,
            launch,
            &capability_key,
        )?;
        let legacy_or_runtime_validation_resume: bool = transaction.query_row(
            "SELECT
               (a.status='capability_validation' AND s.validation_cell IS NOT NULL
                 AND EXISTS(SELECT 1 FROM operation_receipts receipt
                   JOIN audit_events audit ON audit.operation_id=receipt.operation_id
                   WHERE receipt.actor_key='human_control' AND receipt.operation_kind='capability_launch'
                     AND json_extract(receipt.result_json,'$.session_id')=s.id
                     AND audit.actor_kind='human' AND audit.event_code='capability.launch.reserved'
                     AND audit.entity_kind='session' AND audit.entity_id=s.id))
               OR (s.validation_cell IS NOT NULL AND t.lifecycle='validation'
                 AND a.status='held' AND t.attention='paused'
                 AND CASE rg.role WHEN 'manager' THEN a.phase IN ('planning','plan_review','awaiting_plan_approval','awaiting_implementation_authorization','implementation','code_review','checks','final_review','manager_handoff')
                   WHEN 'implementer' THEN a.phase='implementation' WHEN 'plan_reviewer' THEN a.phase='plan_review'
                   WHEN 'explorer' THEN a.phase IN ('planning','implementation','code_review','checks','final_review')
                   WHEN 'code_reviewer' THEN a.phase='code_review' WHEN 'final_verifier' THEN a.phase='final_review' ELSE 0 END
                 AND EXISTS(SELECT 1 FROM operation_receipts receipt
                   JOIN audit_events audit ON audit.operation_id=receipt.operation_id
                   WHERE receipt.actor_key='human_control' AND receipt.operation_kind='workflow_capability_launch'
                     AND json_extract(receipt.result_json,'$.session_id')=s.id
                     AND audit.actor_kind='human' AND audit.event_code='capability.workflow_launch.reserved'
                     AND audit.entity_kind='session' AND audit.entity_id=s.id))
               OR (s.validation_cell='trip_runtime_probe' AND a.status='held' AND t.lifecycle='validation'
                 AND EXISTS(SELECT 1 FROM trip_setup_permits sp JOIN trip_runtime_probes probe ON probe.attempt_id=sp.attempt_id AND probe.role=sp.role
                   JOIN trip_runtime_admissions admission ON admission.id=probe.admission_id
                   WHERE sp.id=s.setup_permit_id AND sp.attempt_id=rg.attempt_id AND sp.role=rg.role
                     AND sp.purpose='runtime_probe' AND sp.state='issued' AND probe.session_id=s.id
                     AND admission.state IN ('authorized','running','awaiting_publication')))
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id WHERE s.id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        let authorized_validation_resume =
            legacy_or_runtime_validation_resume || typed_setup_resume || runtime_probe_resume;
        if attempt_status == "capability_validation" {
            if !authorized_validation_resume {
                bail!(
                    "isolated capability validation resume requires its exact human launch receipt"
                )
            }
        } else {
            crate::trip::require_attempt_ready(
                &transaction,
                &admission_attempt,
                setup_permit.as_deref(),
            )?;
        }
        if !authorized_validation_resume {
            crate::providers::require_production_capability(launch)?;
        }
        let capability_supported: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM capabilities current_capability
                WHERE current_capability.rowid=(
                    SELECT latest_capability.rowid FROM capabilities latest_capability
                    WHERE latest_capability.provider=?1
                      AND latest_capability.executable_version=?2
                      AND latest_capability.role=?3
                      AND latest_capability.mode='interactive_pty'
                    ORDER BY latest_capability.checked_at DESC,
                             latest_capability.rowid DESC LIMIT 1)
                  AND current_capability.config_hash=?4
                  AND current_capability.status='supported'
                  AND current_capability.proof_json!='{}')",
            params![
                launch.provider.to_string(),
                launch.executable_version,
                launch.role.to_string(),
                capability_key
            ],
            |row| row.get(0),
        )?;
        let (
            generation,
            session_status,
            quiescent,
            attempt,
            provider,
            role_text,
            generation_status,
            attempt_status,
            phase,
            lifecycle,
            attention,
            not_archived,
            current_attempt,
            credential_current,
            generation_current,
        ): (
            String,
            String,
            bool,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            bool,
            bool,
            bool,
            bool,
        ) = transaction.query_row(
            "SELECT s.role_generation_id,s.status,
                    COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1,
                    rg.attempt_id,s.provider,rg.role,rg.status,a.status,a.phase,t.lifecycle,t.attention,
                    t.archived_at IS NULL,
                    a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id ORDER BY latest.created_at DESC LIMIT 1),
                    EXISTS(SELECT 1 FROM role_credentials rc WHERE rc.role_generation_id=rg.id AND rc.revoked_at IS NULL),
                    EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=t.id AND rs.role=rg.role AND rs.effective_generation_id=rg.id)
                      OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                        SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id AND lg.effective_generation_id=rg.id))
                      OR (a.status='capability_validation' AND EXISTS(SELECT 1 FROM operation_receipts receipt JOIN audit_events audit ON audit.operation_id=receipt.operation_id
                        WHERE receipt.actor_key='human_control' AND receipt.operation_kind='capability_launch'
                          AND json_extract(receipt.result_json,'$.session_id')=s.id AND audit.actor_kind='human'
                          AND audit.event_code='capability.launch.reserved'
                          AND audit.entity_kind='session' AND audit.entity_id=s.id))
                      OR (s.validation_cell='trip_runtime_probe' AND EXISTS(SELECT 1 FROM trip_runtime_probes probe
                        WHERE probe.session_id=s.id AND probe.attempt_id=rg.attempt_id AND probe.role=rg.role
                          AND probe.state IN ('running','awaiting_resume','evidence_recorded') ))
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
            WHERE s.id=?1 AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
               AND s.capability_identity_json IS NOT NULL",
            params![session_id],
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
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        ).optional()?.ok_or_else(|| {
            anyhow!("session lacks a same-generation native candidate and frozen capability identity")
        })?;
        if session_status != "exited" || !quiescent {
            bail!("validation resume requires a quiescent exited session")
        }
        let resolved_recovery_authority = !credential_current
            && has_resolved_validation_recovery_authority(
                &transaction,
                session_id,
                &generation,
                setup_permit.as_deref(),
            )?;
        let completed_setup_first_turn_authority = !credential_current
            && has_completed_setup_first_turn_stop_authority(
                &transaction,
                session_id,
                &generation,
                setup_permit.as_deref(),
            )?;
        if generation_status != "exited"
            || !generation_current
            || (!credential_current
                && !resolved_recovery_authority
                && !completed_setup_first_turn_authority)
        {
            bail!("session generation or credential no longer has current exited authority")
        }
        if !not_archived
            || (!current_attempt && !runtime_probe_resume)
            || !matches!(
                lifecycle.as_str(),
                "in_progress" | "validation" | "awaiting_review"
            )
            || !matches!(
                attempt_status.as_str(),
                "running" | "held" | "capability_validation"
            )
        {
            bail!("archived, cancelled, or noncurrent attempt cannot resume validation authority")
        }
        let held_workflow_validation =
            lifecycle == "validation" && attempt_status == "held" && attention == "paused";
        let phase_allowed = match role_text.as_str() {
            "manager" => matches!(
                phase.as_str(),
                "planning"
                    | "plan_review"
                    | "awaiting_plan_approval"
                    | "awaiting_implementation_authorization"
                    | "implementation"
                    | "code_review"
                    | "checks"
                    | "final_review"
                    | "manager_handoff"
            ),
            "implementer" => phase == "implementation",
            "plan_reviewer" => phase == "plan_review",
            "code_reviewer" => phase == "code_review",
            "explorer" => matches!(
                phase.as_str(),
                "planning" | "implementation" | "code_review" | "checks" | "final_review"
            ),
            "final_verifier" => phase == "final_review",
            _ => false,
        };
        let runtime_validation_resume = setup_permit.as_deref().is_some_and(|permit| {
            transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM trip_setup_permits WHERE id=?1 AND purpose='runtime_probe')",
                    params![permit],
                    |row| row.get(0),
                )
                .unwrap_or(false)
        });
        let standalone_validation_resume = typed_setup_resume
            || (authorized_validation_resume
                && (attempt_status == "capability_validation" || runtime_validation_resume));
        let review_current: bool = !matches!(
            role_text.as_str(),
            "plan_reviewer" | "code_reviewer" | "final_verifier"
        ) || standalone_validation_resume
            || transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_requests WHERE session_id=?1 AND role_generation_id=?2 AND delivery_state='delivered')",
                params![session_id,generation],
                |row| row.get(0),
            )?;
        if (!phase_allowed && !typed_setup_resume) || !review_current {
            bail!("session authority, phase, review request, or process ownership changed; exact resume is blocked")
        }
        if !authorized_validation_resume
            && (attempt_status != "running"
                || matches!(
                    attention.as_str(),
                    "paused" | "pause_requested" | "needs_recovery"
                ))
        {
            bail!("session authority, phase, review request, or process ownership changed; exact resume is blocked")
        }
        if matches!(attention.as_str(), "pause_requested" | "needs_recovery")
            || (attention == "paused" && !held_workflow_validation)
        {
            bail!("session authority, phase, review request, or process ownership changed; exact resume is blocked")
        }
        if !authorized_validation_resume && !capability_supported {
            bail!("exact resume capability is not currently Supported for the complete frozen invocation identity")
        }
        let role: RoleKind = role_text.parse().map_err(|error: String| anyhow!(error))?;
        if role != launch.role {
            bail!("resume role changed")
        }
        if !role_capacity_available(&transaction, &provider, role, Some(session_id), None)? {
            bail!("role capacity is full for exact resume")
        }
        let conflict: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_generations other WHERE other.attempt_id=?1 AND other.id!=?2 AND other.status IN ('launch_reserved','running','stopping') AND ((other.role=?3 AND (?3!='implementer' OR other.lane_id=(SELECT lane_id FROM role_generations WHERE id=?2))) OR (?3='implementer' AND other.role IN ('explorer','plan_reviewer','code_reviewer','final_verifier')) OR (?3 IN ('explorer','plan_reviewer','code_reviewer','final_verifier') AND other.role='implementer'))) OR EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND status IN ('launch_reserved','running','recovery_required')) OR EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1 AND state IN ('reserved','capturing','recovery_required')) OR EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1 AND kind IN ('pause_now','pause_after_role','cancel') AND state IN ('requested','draining')) OR (?3='manager' AND EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1 AND kind='setup_manager_change' AND state='held')) OR EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1 AND kind IN ('manager_stop','manager_change') AND state NOT IN ('finished','cancelled','superseded','rejected'))",
            params![attempt,generation,role_text],
            |row| row.get(0),
        )?;
        if conflict {
            bail!("validation resume conflicts with active authority, check ownership, pause, or recovery")
        }
        let unresolved_recovery: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE session_id=?1 AND state!='resolved_quiescent')",
            params![session_id],
            |row| row.get(0),
        )?;
        if unresolved_recovery {
            bail!("validation resume requires every recovery record to be resolved quiescent")
        }
        let now = Utc::now().to_rfc3339();
        let (prior_exit,prior_epoch,prior_boot,prior_anchor,prior_root,prior_group): (Option<String>,String,Option<String>,Option<String>,Option<i64>,Option<i32>) = transaction.query_row(
            "SELECT exit_json,transcript_epoch,launch_boot_identity,recovery_anchor_json,recovery_root_pid,recovery_process_group_id FROM sessions WHERE id=?1",
            params![session_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        )?;
        if let Some(receipt) = browser_receipt {
            let resume_count: i64 = transaction.query_row(
                "SELECT resume_count FROM sessions WHERE id=?1",
                params![session_id],
                |row| row.get(0),
            )?;
            let config_revision: i64 = transaction.query_row(
                "SELECT config_revision FROM role_generations WHERE id=?1",
                params![generation],
                |row| row.get(0),
            )?;
            let authority = serde_json::json!({
                "session_id":session_id,
                "role_generation_id":generation,
                "transcript_epoch":prior_epoch,
                "resume_count":resume_count,
                "attempt_id":attempt,
                "role":role_text,
                "config_revision":config_revision,
                "phase":phase,
                "attempt_status":attempt_status,
                "lifecycle":lifecycle,
                "attention":attention,
                "setup_permit_id":setup_permit,
                "runtime_probe_resume":runtime_probe_resume,
                "frozen_capability_key":capability_key,
                "capability_current":capability_supported,
                "launch":launch,
            });
            match reserve_browser_launch_receipt_in(&transaction, receipt, authority, &now)? {
                BrowserLaunchReservation::Reserved => {}
                BrowserLaunchReservation::Existing(existing) => {
                    return Ok(BrowserLaunchReservation::Existing(existing));
                }
            }
        }
        let hook_event_boundary_rowid: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(rowid),0) FROM hook_events WHERE session_id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        crate::permissions::expire_session_in_transaction(
            &transaction,
            session_id,
            "native resume changed the permission request delivery generation",
        )?;
        transaction.execute(
            "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1) WHERE role_generation_id=?2",
            params![now,generation],
        )?;
        transaction.execute(
            "INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![uuid::Uuid::new_v4().to_string(),generation,auth::hash_secret(token),serde_json::to_string(&role_permissions(role))?,now],
        )?;
        let changed = transaction.execute(
            "UPDATE sessions SET status='launch_reserved',launch_state='reserved',launch_error=NULL,resume_count=resume_count+1,
                    transcript_epoch=?1,exit_json=NULL,interrupt_requested_at=NULL,readiness_state='unknown',updated_at=?2
             WHERE id=?3 AND status='exited' AND native_session_id IS NOT NULL AND native_session_id!=''
               AND COALESCE(json_extract(exit_json,'$.process_group_quiescent'),0)=1",
            params![epoch, now, session_id],
        )?;
        if changed != 1 {
            bail!(
                "session is not stopped with a same-generation native candidate; resume is blocked"
            )
        }
        let ordinal: i64 = transaction.query_row(
            "SELECT resume_count FROM sessions WHERE id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,capability_key,capability_identity_json,state,prior_exit_json,prior_transcript_epoch,prior_launch_boot_identity,prior_recovery_anchor_json,prior_recovery_root_pid,prior_recovery_process_group_id,hook_event_boundary_rowid,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'reserved',?8,?9,?10,?11,?12,?13,?14,?15,?15)",
            params![uuid::Uuid::new_v4().to_string(),session_id,ordinal,epoch,launch_json,capability_key,capability_identity_json,prior_exit,prior_epoch,prior_boot,prior_anchor,prior_root,prior_group,hook_event_boundary_rowid,now],
        )?;
        transaction.execute("UPDATE role_generations SET status='launch_reserved',updated_at=?1 WHERE id=(SELECT role_generation_id FROM sessions WHERE id=?2)",params![now,session_id])?;
        transaction.execute("UPDATE review_requests SET resume_count=resume_count+1,updated_at=?1 WHERE session_id=?2 AND delivery_state='delivered'",params![now,session_id])?;
        transaction.commit()?;
        Ok(BrowserLaunchReservation::Reserved)
    }

    pub fn rotate_resume_credential(&self, session_id: &str, token: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (admission_attempt, setup_permit): (String, Option<String>) = tx.query_row(
            "SELECT rg.attempt_id,s.setup_permit_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1",
            params![session_id], |row| Ok((row.get(0)?,row.get(1)?))
        )?;
        crate::trip::require_attempt_ready(&tx, &admission_attempt, setup_permit.as_deref())?;
        let(generation,status,recovery_count):(String,String,i64)=tx.query_row("SELECT s.role_generation_id,s.status,(SELECT COUNT(*) FROM recovery_records r WHERE r.session_id=s.id AND r.state='resolved_quiescent') FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1 AND s.native_session_id IS NOT NULL AND s.native_session_id!='' AND s.capability_identity_json IS NOT NULL AND EXISTS(SELECT 1 FROM capabilities current_capability WHERE current_capability.rowid=(SELECT latest_capability.rowid FROM capabilities latest_capability WHERE latest_capability.provider=s.provider AND latest_capability.executable_version=s.executable_version AND latest_capability.role=rg.role AND latest_capability.mode='interactive_pty' ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1) AND current_capability.config_hash=s.capability_key AND current_capability.status='supported' AND current_capability.proof_json!='{}')",params![session_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?.ok_or_else(||anyhow!("session lacks a same-generation native candidate with current Supported frozen capability evidence"))?;
        if status != "exited" {
            bail!("role resume requires a quiescent exited session")
        }
        let had_recovery: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE session_id=?1)",
            params![session_id],
            |row| row.get(0),
        )?;
        if had_recovery && recovery_count == 0 {
            bail!("prior-boot session requires explicit quiescence recovery before authority can be rebound")
        }
        let role_text: String = tx.query_row(
            "SELECT role FROM role_generations WHERE id=?1",
            params![generation],
            |row| row.get(0),
        )?;
        let role: RoleKind = role_text.parse().map_err(|error: String| anyhow!(error))?;
        tx.execute("UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1) WHERE role_generation_id=?2",params![now,generation])?;
        tx.execute("INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at) VALUES(?1,?2,?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),generation,auth::hash_secret(token),serde_json::to_string(&role_permissions(role))?,now])?;
        tx.commit()?;
        Ok(())
    }

    pub fn reserve_role_resume(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
    ) -> Result<()> {
        match self.reserve_role_resume_inner(session_id, epoch, launch, token, None)? {
            BrowserLaunchReservation::Reserved => Ok(()),
            BrowserLaunchReservation::Existing(_) => {
                bail!("non-browser role resume unexpectedly found a browser receipt")
            }
        }
    }

    pub(crate) fn reserve_role_resume_with_browser_receipt(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        receipt: BrowserLaunchReceipt<'_>,
    ) -> Result<BrowserLaunchReservation> {
        self.reserve_role_resume_inner(session_id, epoch, launch, token, Some(receipt))
    }

    fn reserve_role_resume_inner(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<BrowserLaunchReservation> {
        self.require_execution_unheld("role resume reservations")?;
        if launch.role == RoleKind::FinalReviewer {
            bail!("final verifier sessions are always fresh and cannot resume")
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = browser_receipt {
            if let Some(existing) = browser_launch_receipt_in(
                &tx,
                receipt.operation_id,
                receipt.operation_kind,
                receipt.input_hash,
            )? {
                return Ok(BrowserLaunchReservation::Existing(existing));
            }
        }
        let manager_stop_fenced: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM controls c
                JOIN sessions s ON s.id=?1
                JOIN role_generations rg ON rg.id=s.role_generation_id
                 WHERE c.attempt_id=rg.attempt_id AND c.kind='manager_stop' AND c.state='held'
                   AND json_extract(c.payload_json,'$.manager_session_id')=s.id
            )",
            params![session_id],
            |row| row.get(0),
        )?;
        if manager_stop_fenced {
            bail!("native resume is fenced by the settled manager-stop intent until Continue manager releases it or a current manager change supersedes it")
        }
        let manager_change_fenced: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM controls c
                JOIN sessions s ON s.id=?1
                JOIN role_generations rg ON rg.id=s.role_generation_id
                 WHERE c.attempt_id=rg.attempt_id AND c.kind='manager_change'
                   AND c.state NOT IN ('finished','cancelled','superseded','rejected','failed')
                   AND json_extract(c.payload_json,'$.old_generation_id')=rg.id
            )",
            params![session_id],
            |row| row.get(0),
        )?;
        if manager_change_fenced {
            bail!("native resume is fenced by a pending exact-generation manager change until that change reaches a terminal disposition")
        }
        let switch_fence_state: Option<String> = tx
            .query_row(
                "SELECT si.state FROM switch_intents si
                JOIN sessions s ON s.id=?1
                JOIN role_generations rg ON rg.id=s.role_generation_id
                 WHERE si.attempt_id=rg.attempt_id AND si.old_generation_id=rg.id
                   AND si.state NOT IN ('completed','cancelled','superseded')
                 ORDER BY si.updated_at DESC,si.rowid DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(switch_fence_state) = switch_fence_state {
            match switch_fence_state.as_str() {
                "rejected" => bail!("native resume is fenced by a rejected exact-generation switch until a corrected current switch supersedes it"),
                "recovery_required" => bail!("native resume is fenced by a recovery-held exact-generation switch until a corrected current switch supersedes it"),
                _ => bail!("native resume is fenced by an active exact-generation switch until that switch reaches a terminal disposition"),
            }
        }
        crate::providers::require_production_capability(launch)?;
        let capability_identity = crate::providers::capability_identity(launch)?;
        crate::providers::require_current_capability_identity_with_bundles(
            &capability_identity,
            &launch.cwd,
            &self.compatibility_bundles,
        )?;
        let capability_key = crate::providers::capability_identity_key(&capability_identity)?;
        let launch_json = serde_json::to_string(launch)?;
        let capability_identity_json = serde_json::to_string(&capability_identity)?;
        let (admission_attempt, setup_permit): (String, Option<String>) = tx.query_row(
            "SELECT rg.attempt_id,s.setup_permit_id FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1",
            params![session_id], |row| Ok((row.get(0)?,row.get(1)?))
        )?;
        crate::trip::require_attempt_ready(&tx, &admission_attempt, setup_permit.as_deref())?;
        validate_frozen_capability(
            &tx,
            session_id,
            &capability_key,
            &launch.cwd,
            &self.compatibility_bundles,
        )?;
        let capability_supported: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM capabilities current_capability
                WHERE current_capability.rowid=(
                    SELECT latest_capability.rowid FROM capabilities latest_capability
                    WHERE latest_capability.provider=?1
                      AND latest_capability.executable_version=?2
                      AND latest_capability.role=?3
                      AND latest_capability.mode='interactive_pty'
                    ORDER BY latest_capability.checked_at DESC,
                             latest_capability.rowid DESC LIMIT 1)
                  AND current_capability.config_hash=?4
                  AND current_capability.status='supported'
                  AND current_capability.proof_json!='{}')",
            params![
                launch.provider.to_string(),
                launch.executable_version,
                launch.role.to_string(),
                capability_key
            ],
            |row| row.get(0),
        )?;
        if !capability_supported {
            bail!(
                "exact resume capability is no longer supported for the frozen invocation identity"
            )
        }
        let (prior_exit,prior_epoch,prior_boot,prior_anchor,prior_root,prior_group): (Option<String>,String,Option<String>,Option<String>,Option<i64>,Option<i32>) = tx.query_row(
            "SELECT exit_json,transcript_epoch,launch_boot_identity,recovery_anchor_json,recovery_root_pid,recovery_process_group_id FROM sessions WHERE id=?1",
            params![session_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        )?;
        let(generation,status,recovery_count,attempt,role_text,generation_status,provider):(String,String,i64,String,String,String,String)=tx.query_row("SELECT s.role_generation_id,s.status,(SELECT COUNT(*) FROM recovery_records r WHERE r.session_id=s.id AND r.state='resolved_quiescent'),rg.attempt_id,rg.role,rg.status,s.provider FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id WHERE s.id=?1 AND s.native_session_id IS NOT NULL AND s.native_session_id!='' AND s.capability_identity_json IS NOT NULL",params![session_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))).optional()?.ok_or_else(||anyhow!("session lacks a same-generation native candidate and frozen capability identity"))?;
        if role_text == "final_verifier" || role_text == "final_reviewer" {
            bail!("final verifier sessions are always fresh and cannot resume")
        }
        if status != "exited" {
            bail!("role resume requires a quiescent exited session")
        }
        let generation_current:bool=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_generations rg JOIN attempts a ON a.id=rg.attempt_id
             WHERE rg.id=?1 AND (EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=a.task_id AND rs.role=rg.role AND rs.effective_generation_id=rg.id)
               OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id AND lg.effective_generation_id=rg.id))))",
            params![generation],|row|row.get(0)
        )?;
        if generation_status != "exited" || !generation_current {
            bail!("role generation was replaced or otherwise lost current authority")
        }
        let (lifecycle_current, phase_allowed, review_current, held_workflow_validation): (bool, bool, bool, bool) = tx.query_row(
            "SELECT a.status IN ('running','needs_input','held') AND t.lifecycle IN ('in_progress','validation','awaiting_review') AND t.archived_at IS NULL,
                    CASE ?2 WHEN 'manager' THEN a.phase IN ('planning','plan_review','awaiting_plan_approval','awaiting_implementation_authorization','implementation','code_review','checks','final_review','manager_handoff')
                      WHEN 'implementer' THEN a.phase='implementation' WHEN 'plan_reviewer' THEN a.phase='plan_review'
                      WHEN 'explorer' THEN a.phase IN ('planning','implementation','code_review','checks','final_review')
                      WHEN 'code_reviewer' THEN a.phase='code_review' WHEN 'final_verifier' THEN a.phase='final_review' ELSE 0 END,
                    ?2 NOT IN ('plan_reviewer','code_reviewer','final_verifier') OR EXISTS(
                      SELECT 1 FROM review_requests r WHERE r.session_id=?3 AND r.role_generation_id=?4 AND r.delivery_state='delivered'),
                    a.status='held' AND t.lifecycle='validation' AND t.attention='paused'
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
            params![attempt,&role_text,session_id,generation],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        )?;
        if !lifecycle_current || !phase_allowed || !review_current {
            bail!(
                "terminal history, current phase, or review ownership cannot resume role authority"
            )
        }
        let had_recovery: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE session_id=?1)",
            params![session_id],
            |row| row.get(0),
        )?;
        if had_recovery && recovery_count == 0 {
            bail!("prior-boot session requires explicit quiescence recovery")
        }
        let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM role_generations other WHERE other.attempt_id=?1 AND other.id!=?2 AND other.status IN ('launch_reserved','running','stopping') AND ((other.role=?3 AND (?3!='implementer' OR other.lane_id=(SELECT lane_id FROM role_generations WHERE id=?2))) OR (?3='implementer' AND other.role IN ('explorer','plan_reviewer','code_reviewer','final_verifier')) OR (?3 IN ('explorer','plan_reviewer','code_reviewer','final_verifier') AND other.role='implementer'))) OR EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND status IN ('launch_reserved','running','recovery_required')) OR EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1 AND state IN ('reserved','capturing','recovery_required')) OR EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1 AND (t.attention IN ('pause_requested','needs_recovery') OR (t.attention='paused' AND NOT ?4)))",params![attempt,generation,role_text,held_workflow_validation],|row|row.get(0))?;
        if conflict {
            bail!(
                "role resume conflicts with active authority, check ownership, pause, or recovery"
            )
        }
        let role: RoleKind = role_text.parse().map_err(|error: String| anyhow!(error))?;
        if role != launch.role {
            bail!("resume role changed")
        }
        if !role_capacity_available(&tx, &provider, role, Some(session_id), None)? {
            return Err(RoleResumeCapacityError.into());
        }
        if let Some(receipt) = browser_receipt {
            let (task_id, attempt_status, phase, lifecycle, attention, config_revision, resume_count):
                (String, String, String, String, String, i64, i64) = tx.query_row(
                "SELECT a.task_id,a.status,a.phase,t.lifecycle,t.attention,rg.config_revision,s.resume_count
                 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE s.id=?1",
                params![session_id],
                |row| Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?,
                )),
            )?;
            let authority = serde_json::json!({
                "session_id":session_id,
                "role_generation_id":generation,
                "transcript_epoch":prior_epoch,
                "resume_count":resume_count,
                "attempt_id":attempt,
                "task_id":task_id,
                "role":role_text,
                "config_revision":config_revision,
                "phase":phase,
                "attempt_status":attempt_status,
                "lifecycle":lifecycle,
                "attention":attention,
                "setup_permit_id":setup_permit,
                "frozen_capability_key":capability_key,
                "capability_current":capability_supported,
                "launch":launch,
            });
            match reserve_browser_launch_receipt_in(&tx, receipt, authority, &now)? {
                BrowserLaunchReservation::Reserved => {}
                BrowserLaunchReservation::Existing(existing) => {
                    return Ok(BrowserLaunchReservation::Existing(existing));
                }
            }
        }
        let hook_event_boundary_rowid: i64 = tx.query_row(
            "SELECT COALESCE(MAX(rowid),0) FROM hook_events WHERE session_id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        tx.execute("UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1) WHERE role_generation_id=?2",params![now,generation])?;
        let _ = held_workflow_validation;
        crate::permissions::expire_session_in_transaction(
            &tx,
            session_id,
            "native resume changed the permission request delivery generation",
        )?;
        tx.execute("INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at) VALUES(?1,?2,?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),generation,auth::hash_secret(token),serde_json::to_string(&role_permissions(role))?,now])?;
        let changed=tx.execute("UPDATE sessions SET status='launch_reserved',launch_state='reserved',launch_error=NULL,resume_count=resume_count+1,transcript_epoch=?1,exit_json=NULL,interrupt_requested_at=NULL,readiness_state='unknown',updated_at=?2 WHERE id=?3 AND status='exited'",params![epoch,now,session_id])?;
        if changed != 1 {
            bail!("role resume reservation became stale")
        }
        let ordinal: i64 = tx.query_row(
            "SELECT resume_count FROM sessions WHERE id=?1",
            params![session_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,capability_key,capability_identity_json,state,prior_exit_json,prior_transcript_epoch,prior_launch_boot_identity,prior_recovery_anchor_json,prior_recovery_root_pid,prior_recovery_process_group_id,hook_event_boundary_rowid,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'reserved',?8,?9,?10,?11,?12,?13,?14,?15,?15)",
            params![uuid::Uuid::new_v4().to_string(),session_id,ordinal,epoch,launch_json,capability_key,capability_identity_json,prior_exit,prior_epoch,prior_boot,prior_anchor,prior_root,prior_group,hook_event_boundary_rowid,now],
        )?;
        tx.execute(
            "UPDATE role_generations SET status='launch_reserved',updated_at=?1 WHERE id=?2",
            params![now, generation],
        )?;
        tx.execute("UPDATE review_requests SET resume_count=resume_count+1,updated_at=?1 WHERE session_id=?2 AND delivery_state='delivered'",params![now,session_id])?;
        tx.commit()?;
        Ok(BrowserLaunchReservation::Reserved)
    }

    pub fn acquire_input_lease(
        &self,
        session_id: &str,
        lease_secret: &str,
        owner_id: &str,
        process_json: &str,
        role_generation_id: &str,
        expires_at: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: String = transaction.query_row(
            "SELECT status FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        if status != "running" {
            bail!("input lease requires a running session")
        }
        let active: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT expires_at, revoked_at FROM input_leases WHERE session_id = ?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((expiry, revoked)) = active {
            if revoked.is_none() && expiry > now {
                bail!("input owned by another human control lease")
            }
        }
        transaction.execute(
            "INSERT INTO input_leases(session_id, lease_id_hash, owner_kind, owner_id, role_generation_id, process_identity_json, expires_at, revoked_at, created_at, updated_at)
             VALUES(?1, ?2, 'human', ?3, ?4, ?5, ?6, NULL, ?7, ?7)
             ON CONFLICT(session_id) DO UPDATE SET lease_id_hash=excluded.lease_id_hash, owner_kind='human', owner_id=excluded.owner_id,
               role_generation_id=excluded.role_generation_id, process_identity_json=excluded.process_identity_json,
               expires_at=excluded.expires_at, revoked_at=NULL, updated_at=excluded.updated_at",
            params![session_id, auth::hash_secret(lease_secret), owner_id, role_generation_id, process_json, expires_at, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn renew_input_lease(
        &self,
        session_id: &str,
        lease_secret: &str,
        process_json: &str,
        role_generation_id: &str,
        expires_at: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, String, String, String, Option<String>, String)> = transaction
            .query_row(
                "SELECT lease_id_hash, role_generation_id, process_identity_json, expires_at, revoked_at,
                        (SELECT status FROM sessions WHERE id=input_leases.session_id)
                 FROM input_leases WHERE session_id=?1",
                params![session_id],
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
        let Some((hash, generation, process, expiry, revoked, status)) = row else {
            bail!("no input lease exists to renew")
        };
        auth::verify_secret(lease_secret, &hash)?;
        if revoked.is_some() || expiry <= now {
            bail!("input lease is revoked or expired and cannot be renewed")
        }
        if generation != role_generation_id || process != process_json || status != "running" {
            bail!("input lease no longer matches the live generation/process")
        }
        let changed = transaction.execute(
            "UPDATE input_leases SET expires_at=?1,updated_at=?2
             WHERE session_id=?3 AND lease_id_hash=?4 AND role_generation_id=?5
               AND process_identity_json=?6 AND revoked_at IS NULL AND expires_at>?2",
            params![
                expires_at,
                now,
                session_id,
                hash,
                role_generation_id,
                process_json
            ],
        )?;
        if changed != 1 {
            bail!("input lease changed while renewal was in progress")
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn takeover_input_lease(
        &self,
        session_id: &str,
        lease_secret: &str,
        owner_id: &str,
        process_json: &str,
        role_generation_id: &str,
        expires_at: &str,
        operation_id: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, String, String, String, Option<String>)> = transaction
            .query_row(
                "SELECT owner_id, role_generation_id, process_identity_json, expires_at, revoked_at
                 FROM input_leases WHERE session_id=?1",
                params![session_id],
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
            .optional()?;
        let Some((prior_owner, prior_generation, prior_process, prior_expiry, prior_revoked)) =
            current
        else {
            bail!("input takeover requires an existing lease")
        };
        let session_current: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
               WHERE s.id=?1 AND s.status='running' AND rg.status='running'
                 AND s.role_generation_id=?2 AND s.process_identity_json=?3)",
            params![session_id, role_generation_id, process_json],
            |row| row.get(0),
        )?;
        if !session_current {
            bail!("input takeover does not match the live generation/process")
        }
        if prior_revoked.is_some()
            || prior_expiry <= now
            || prior_generation != role_generation_id
            || prior_process != process_json
        {
            bail!("input takeover requires a current active lease")
        }
        transaction.execute(
            "UPDATE input_leases SET revoked_at=?1,updated_at=?1 WHERE session_id=?2 AND revoked_at IS NULL",
            params![now, session_id],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,actor_id,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human',?3,'input.lease.taken_over','input_lease',?4,?5,?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                operation_id,
                owner_id,
                session_id,
                serde_json::json!({"prior_owner_id":prior_owner,"role_generation_id":role_generation_id,"process_identity":serde_json::from_str::<serde_json::Value>(process_json)?}).to_string(),
                now
            ],
        )?;
        transaction.execute(
            "UPDATE input_leases SET lease_id_hash=?1,owner_kind='human',owner_id=?2,
                role_generation_id=?3,process_identity_json=?4,expires_at=?5,revoked_at=NULL,updated_at=?6
             WHERE session_id=?7",
            params![
                auth::hash_secret(lease_secret),
                owner_id,
                role_generation_id,
                process_json,
                expires_at,
                now,
                session_id
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn active_input_lease_owner(&self, session_id: &str) -> Result<Option<(String, String)>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT owner_id,expires_at FROM input_leases
                 WHERE session_id=?1 AND revoked_at IS NULL AND expires_at>?2",
                params![session_id, Utc::now().to_rfc3339()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn verify_input_lease(
        &self,
        session_id: &str,
        lease_secret: &str,
        process_json: &str,
        role_generation_id: &str,
    ) -> Result<()> {
        let connection = self.lock()?;
        let row: Option<(String, String, String, String, Option<String>, String)> = connection.query_row(
            "SELECT lease_id_hash, role_generation_id, process_identity_json, expires_at, revoked_at,
                    (SELECT status FROM sessions WHERE id = input_leases.session_id)
             FROM input_leases WHERE session_id = ?1",
            params![session_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).optional()?;
        let Some((hash, generation, process, expiry, revoked, status)) = row else {
            bail!("no input lease exists")
        };
        auth::verify_secret(lease_secret, &hash)?;
        if revoked.is_some() {
            bail!("input lease is revoked")
        }
        if expiry <= Utc::now().to_rfc3339() {
            bail!("input lease is expired")
        }
        if generation != role_generation_id || process != process_json {
            bail!("input lease does not match the live generation/process")
        }
        if status != "running" {
            bail!("session is no longer running")
        }
        Ok(())
    }

    pub fn release_input_lease(&self, session_id: &str, lease_secret: &str) -> Result<()> {
        let connection = self.lock()?;
        let hash: String = connection.query_row(
            "SELECT lease_id_hash FROM input_leases WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        auth::verify_secret(lease_secret, &hash)?;
        connection.execute(
            "UPDATE input_leases SET revoked_at = ?1, updated_at = ?1 WHERE session_id = ?2",
            params![Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    pub fn revoke_input_lease_for_session(&self, session_id: &str) -> Result<()> {
        let connection = self.lock()?;
        connection.execute(
            "UPDATE input_leases SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1 WHERE session_id = ?2",
            params![Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }
}

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 30;

/// Reads the durable state cursor. It is committed state only when the
/// connection is in autocommit mode.
pub(crate) fn read_state_revision(connection: &Connection) -> rusqlite::Result<i64> {
    connection
        .prepare_cached("SELECT revision FROM state_revision WHERE singleton=1")?
        .query_row([], |row| row.get(0))
}

pub(crate) fn require_current_schema(connection: &Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != CURRENT_SCHEMA_VERSION {
        bail!(
            "unsupported database schema version {version}; current schema version {} is required and this command will not migrate it",
            CURRENT_SCHEMA_VERSION
        )
    }
    Ok(())
}

pub(crate) fn browser_launch_receipt_in(
    transaction: &Transaction<'_>,
    operation_id: &str,
    operation_kind: &str,
    input_hash: &str,
) -> Result<Option<serde_json::Value>> {
    let receipt: Option<String> = transaction
        .query_row(
            "SELECT result_json FROM operation_receipts WHERE operation_id=?1
             AND actor_key='human_control' AND operation_kind=?2",
            params![operation_id, operation_kind],
            |row| row.get(0),
        )
        .optional()?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    let audit: String = transaction
        .query_row(
            "SELECT detail_json FROM audit_events WHERE operation_id=?1
             AND event_code='browser.launch.reserved'
             AND json_extract(detail_json,'$.operation_kind')=?2
             ORDER BY rowid DESC LIMIT 1",
            params![operation_id, operation_kind],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("browser launch receipt lacks its immutable reservation audit"))?;
    let audit: serde_json::Value = serde_json::from_str(&audit)?;
    if audit.get("input_hash").and_then(serde_json::Value::as_str) != Some(input_hash) {
        bail!("operation ID was already used with different input")
    }
    Ok(Some(serde_json::from_str(&receipt)?))
}

pub(crate) fn reserve_browser_launch_receipt_in(
    transaction: &Transaction<'_>,
    receipt: BrowserLaunchReceipt<'_>,
    authority: serde_json::Value,
    now: &str,
) -> Result<BrowserLaunchReservation> {
    if receipt.operation_id.trim().is_empty() {
        bail!("operation_id is required")
    }
    if let Some(existing) = browser_launch_receipt_in(
        transaction,
        receipt.operation_id,
        receipt.operation_kind,
        receipt.input_hash,
    )? {
        return Ok(BrowserLaunchReservation::Existing(existing));
    }
    let request_hash = json_hash(&serde_json::json!({
        "input_hash":receipt.input_hash,
        "authority":authority,
    }))?;
    let pending = serde_json::json!({
        "state":"reserved",
        "entity_id":receipt.entity_id,
        "input_hash":receipt.input_hash,
    });
    transaction.execute(
        "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
         VALUES(?1,'human_control',?2,?3,?4,?5)",
        params![
            receipt.operation_id,
            receipt.operation_kind,
            request_hash,
            pending.to_string(),
            now,
        ],
    )?;
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','browser.launch.reserved','operation',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            receipt.operation_id,
            receipt.entity_id,
            serde_json::json!({
                "operation_kind":receipt.operation_kind,
                "input_hash":receipt.input_hash,
                "authority":authority,
            })
            .to_string(),
            now,
        ],
    )?;
    Ok(BrowserLaunchReservation::Reserved)
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        0 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_001)?;
            transaction.execute_batch(MIGRATION_002)?;
            transaction.execute_batch(MIGRATION_003)?;
            transaction.execute_batch(MIGRATION_004)?;
            transaction.execute_batch(MIGRATION_005)?;
            transaction.execute_batch(MIGRATION_006)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration")?;
        }
        1 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_002)?;
            transaction.execute_batch(MIGRATION_003)?;
            transaction.execute_batch(MIGRATION_004)?;
            transaction.execute_batch(MIGRATION_005)?;
            transaction.execute_batch(MIGRATION_006)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 2")?;
        }
        2 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_003)?;
            transaction.execute_batch(MIGRATION_004)?;
            transaction.execute_batch(MIGRATION_005)?;
            transaction.execute_batch(MIGRATION_006)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 3")?;
        }
        3 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_004)?;
            transaction.execute_batch(MIGRATION_005)?;
            transaction.execute_batch(MIGRATION_006)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 4")?;
        }
        4 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_005)?;
            transaction.execute_batch(MIGRATION_006)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 5")?;
        }
        5 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_006)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 6")?;
        }
        6 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_007)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 7")?;
        }
        7 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_008)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 8")?;
        }
        8 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_009)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 9")?;
        }
        9 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_010)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 10")?;
        }
        10 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_011)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 11")?;
        }
        11 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_012)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 12")?;
        }
        12 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_013)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 13")?;
        }
        13 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(MIGRATION_014)?;
            transaction.pragma_update(None, "user_version", 14)?;
            transaction.commit().context("commit schema migration 14")?;
        }
        14 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 | 23 | 24 | 25 | 26 | 27 | 28 | 29 | 30 => {}
        other => bail!("database schema {other} is newer than this LLMRelay build"),
    }
    if version <= 14 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_015)?;
        transaction.pragma_update(None, "user_version", 15)?;
        transaction
            .commit()
            .context("commit Codex native-policy admission migration")?;
    }
    if version <= 15 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_016)?;
        transaction.pragma_update(None, "user_version", 16)?;
        transaction
            .commit()
            .context("commit Codex capability-display migration")?;
    }
    if version <= 16 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_017)?;
        transaction.pragma_update(None, "user_version", 17)?;
        transaction
            .commit()
            .context("commit Codex denied-read-floor migration")?;
    }
    if version <= 17 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_018)?;
        transaction.pragma_update(None, "user_version", 18)?;
        transaction
            .commit()
            .context("commit TRIP Explorer integration migration")?;
    }
    if version <= 18 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_019)?;
        transaction.pragma_update(None, "user_version", 19)?;
        transaction
            .commit()
            .context("commit TRIP setup finalization migration")?;
    }
    if version <= 19 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_020)?;
        transaction.pragma_update(None, "user_version", 20)?;
        transaction
            .commit()
            .context("commit TRIP capability-binding migration")?;
    }
    if version <= 20 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_021)?;
        transaction.pragma_update(None, "user_version", 21)?;
        transaction
            .commit()
            .context("commit task-profile activation migration")?;
    }
    if version <= 21 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_022)?;
        transaction.pragma_update(None, "user_version", 22)?;
        transaction
            .commit()
            .context("commit ordinary runtime admission migration")?;
    }
    if version <= 22 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_023)?;
        transaction.pragma_update(None, "user_version", 23)?;
        transaction
            .commit()
            .context("commit selected-check permission migration")?;
    }
    if version <= 23 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_024)?;
        transaction.pragma_update(None, "user_version", 24)?;
        transaction
            .commit()
            .context("commit cmux attachment routing migration")?;
    }
    if version <= 24 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_025)?;
        transaction.pragma_update(None, "user_version", 25)?;
        transaction
            .commit()
            .context("commit Claude role-socket requalification migration")?;
    }
    if version <= 25 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_026)?;
        transaction.pragma_update(None, "user_version", 26)?;
        transaction
            .commit()
            .context("commit runtime cmux socket observation migration")?;
    }
    if version <= 26 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_027)?;
        transaction.pragma_update(None, "user_version", 27)?;
        transaction
            .commit()
            .context("commit persistent cmux task-workspace migration")?;
    }
    if version <= 27 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_028)?;
        transaction.pragma_update(None, "user_version", 28)?;
        transaction
            .commit()
            .context("commit session interrupt-deadline migration")?;
    }
    if version <= 28 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_029)?;
        transaction.pragma_update(None, "user_version", 29)?;
        transaction
            .commit()
            .context("commit state revision migration")?;
    }
    if version <= 29 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_030)?;
        transaction.pragma_update(None, "user_version", 30)?;
        transaction.commit().context("commit recipe migration")?;
    }
    Ok(())
}

pub fn json_hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

pub(crate) fn record_plan_rejection(
    transaction: &rusqlite::Transaction<'_>,
    attempt_id: &str,
    review_request_id: &str,
    rejected_plan_hash: &str,
    verdict: &str,
    feedback: &str,
    now: &str,
) -> Result<()> {
    let manager_generation: String = transaction
        .query_row(
            "SELECT rg.id FROM role_generations rg
             JOIN attempts a ON a.id=rg.attempt_id
             JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role='manager'
               AND rs.effective_generation_id=rg.id
             WHERE rg.attempt_id=?1 AND rg.role='manager'
               AND rg.status IN ('launch_reserved','running','exited')
             ORDER BY rg.generation DESC LIMIT 1",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("plan rejection requires an effective current manager generation")
        })?;
    transaction.execute(
        "UPDATE role_results SET consumed_at=?1
         WHERE consumed_at IS NULL AND outcome='plan_ready'
           AND role_generation_id IN (SELECT id FROM role_generations
             WHERE attempt_id=?2 AND role='manager')",
        params![now, attempt_id],
    )?;
    let body = serde_json::json!({
        "instruction":"Produce a revised plan and report plan_ready. The report metadata must include the exact rejected_plan_review_request_id below.",
        "required_metadata":{"rejected_plan_review_request_id":review_request_id},
        "rejected_plan_review_request_id":review_request_id,
        "rejected_plan_hash":rejected_plan_hash,
        "verdict":verdict,
        "feedback":feedback,
    })
    .to_string();
    transaction.execute(
        "INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,reason,created_at)
         SELECT ?1,?2,?3,?4,'queued','engine_plan_rejection_notice',?5
         WHERE NOT EXISTS(SELECT 1 FROM guidance_messages
           WHERE attempt_id=?2 AND role_generation_id=?3 AND body=?4)",
        params![
            uuid::Uuid::new_v4().to_string(),
            attempt_id,
            manager_generation,
            body,
            now
        ],
    )?;
    Ok(())
}

pub(crate) struct EligibleManagerPlan {
    pub result_id: String,
    pub generation_id: String,
    pub session_id: String,
    pub session_status: String,
    pub readiness_state: String,
    pub plan: String,
    pub base_revision: String,
    pub task_version: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagerServiceStopReceipt {
    pub session_id: String,
    pub generation_id: String,
    pub credential_id: String,
    pub transcript_epoch: String,
    pub resume_invocation_id: Option<String>,
    pub invocation_boundary_rowid: i64,
    pub stop_rowid: i64,
    pub task_version: i64,
    pub plan_hash: Option<String>,
    pub candidate_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagerServiceStopClaim {
    Claimed,
    AlreadyRequested,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodexStopIdleReceipt {
    pub session_id: String,
    pub generation_id: String,
    pub credential_id: String,
    pub transcript_epoch: String,
    pub resume_invocation_id: Option<String>,
    pub invocation_boundary_rowid: i64,
    pub native_session_id: String,
    pub stop_rowid: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SetupRetainedFirstTurnStopReceipt {
    pub session_id: String,
    pub generation_id: String,
    pub credential_id: String,
    pub transcript_epoch: String,
    pub native_session_id: String,
    pub process_identity_json: String,
    pub stop_rowid: i64,
    pub setup_permit_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SetupRetainedFirstTurnStopTimeoutReceipt {
    pub session_id: String,
    pub generation_id: String,
    pub transcript_epoch: String,
    pub native_session_id: String,
    pub process_identity_json: String,
    pub stop_rowid: i64,
    pub claim_rowid: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InterruptDeadlineReceipt {
    pub session_id: String,
    pub generation_id: String,
    pub transcript_epoch: String,
    pub process_identity_json: String,
    pub attempt_id: String,
    pub task_id: String,
    pub interrupt_requested_at: String,
}

pub(crate) fn eligible_codex_stop_idle_reconciliation(
    connection: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<Option<CodexStopIdleReceipt>> {
    connection
        .query_row(
            "WITH current_codex AS (
               SELECT s.id AS session_id,rg.id AS generation_id,rc.id AS credential_id,
                      s.transcript_epoch,s.native_session_id,
                      (SELECT ri.id FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1) AS resume_invocation_id,
                      COALESCE((SELECT ri.hook_event_boundary_rowid FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1),s.initial_hook_event_boundary_rowid,0) AS boundary
               FROM sessions s
               JOIN role_generations rg ON rg.id=s.role_generation_id
               JOIN attempts a ON a.id=rg.attempt_id
               JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=rg.role
                 AND rs.effective_generation_id=rg.id
               JOIN role_credentials rc ON rc.role_generation_id=rg.id AND rc.revoked_at IS NULL
               JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
               WHERE (?1 IS NULL OR s.id=?1) AND s.provider='codex'
                 AND rg.status='running' AND s.status='running'
                 AND s.readiness_state='busy_unresolved_hook_work'
                 AND s.resume_count=0 AND rg.role!='final_verifier'
                 AND sp.state='issued' AND sp.attempt_id=a.id AND sp.role=rg.role
                 AND sp.purpose IN ('setup_discovery','profile_probe')
                 AND ((sp.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
                   OR (sp.purpose='profile_probe' AND s.validation_cell='trip_setup_probe'))
                 AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
                 AND s.id=(SELECT latest.id FROM sessions latest
                   WHERE latest.role_generation_id=rg.id
                   ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
             ), latest_stop AS (
               SELECT current_codex.*,h.rowid AS stop_rowid
               FROM current_codex JOIN hook_events h ON h.session_id=current_codex.session_id
               WHERE h.rowid=(SELECT MAX(latest.rowid) FROM hook_events latest
                 WHERE latest.session_id=current_codex.session_id
                   AND latest.rowid>current_codex.boundary)
                 AND h.event_name='Stop' AND h.provider='codex'
                 AND h.role_generation_id=current_codex.generation_id
                 AND h.native_session_id=current_codex.native_session_id
                 AND h.provenance_state='managed_process_group_untrusted_payload'
             ), stopped_turn AS (
               SELECT latest_stop.*,
                      (SELECT MAX(submit.rowid) FROM hook_events submit
                       WHERE submit.session_id=latest_stop.session_id
                         AND submit.role_generation_id=latest_stop.generation_id
                         AND submit.event_name='UserPromptSubmit'
                         AND submit.native_session_id=latest_stop.native_session_id
                         AND submit.provenance_state='managed_process_group_untrusted_payload'
                         AND submit.rowid>latest_stop.boundary
                         AND submit.rowid<latest_stop.stop_rowid) AS submit_rowid
               FROM latest_stop
             )
             SELECT stopped_turn.session_id,stopped_turn.generation_id,
                    stopped_turn.credential_id,stopped_turn.transcript_epoch,
                    stopped_turn.resume_invocation_id,stopped_turn.boundary,
                    stopped_turn.native_session_id,stopped_turn.stop_rowid
             FROM stopped_turn
             WHERE stopped_turn.submit_rowid IS NOT NULL
               AND EXISTS(SELECT 1 FROM hook_events start
                 WHERE start.session_id=stopped_turn.session_id
                   AND start.role_generation_id=stopped_turn.generation_id
                   AND start.event_name='SessionStart'
                   AND start.native_session_id=stopped_turn.native_session_id
                   AND start.provenance_state='managed_process_group_untrusted_payload'
                   AND start.rowid>stopped_turn.boundary
                   AND start.rowid<stopped_turn.submit_rowid)
               AND (SELECT COUNT(*) FROM hook_events started
                    WHERE started.session_id=stopped_turn.session_id
                      AND started.event_name='PreToolUse'
                      AND started.role_generation_id=stopped_turn.generation_id
                      AND started.native_session_id=stopped_turn.native_session_id
                      AND started.provenance_state='managed_process_group_untrusted_payload'
                      AND started.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
                 > (SELECT COUNT(*) FROM hook_events finished
                    WHERE finished.session_id=stopped_turn.session_id
                      AND finished.event_name IN ('PostToolUse','PostToolUseFailure')
                      AND finished.role_generation_id=stopped_turn.generation_id
                      AND finished.native_session_id=stopped_turn.native_session_id
                      AND finished.provenance_state='managed_process_group_untrusted_payload'
                      AND finished.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
               AND (SELECT COUNT(*) FROM hook_events started
                    WHERE started.session_id=stopped_turn.session_id
                      AND started.event_name='SubagentStart'
                      AND started.role_generation_id=stopped_turn.generation_id
                      AND started.native_session_id=stopped_turn.native_session_id
                      AND started.provenance_state='managed_process_group_untrusted_payload'
                      AND started.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
                 = (SELECT COUNT(*) FROM hook_events finished
                    WHERE finished.session_id=stopped_turn.session_id
                      AND finished.event_name='SubagentStop'
                      AND finished.role_generation_id=stopped_turn.generation_id
                      AND finished.native_session_id=stopped_turn.native_session_id
                      AND finished.provenance_state='managed_process_group_untrusted_payload'
                      AND finished.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
               AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                 WHERE permission.session_id=stopped_turn.session_id
                   AND permission.consumed_at IS NULL
                   AND permission.delivery_state NOT IN ('expired','not_delivered'))
               AND NOT EXISTS(SELECT 1 FROM input_leases lease
                 WHERE lease.session_id=stopped_turn.session_id AND lease.revoked_at IS NULL
                   AND julianday(lease.expires_at)>julianday('now'))
               AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
                 WHERE guidance.role_generation_id=stopped_turn.generation_id
                   AND guidance.state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))
               AND NOT EXISTS(SELECT 1 FROM controls control
                 WHERE control.attempt_id=(SELECT attempt_id FROM role_generations
                   WHERE id=stopped_turn.generation_id)
                   AND control.state IN ('requested','draining','held','recovery_required'))
               AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                 WHERE recovery.session_id=stopped_turn.session_id
                   AND recovery.state!='resolved_quiescent')
             ORDER BY stopped_turn.stop_rowid LIMIT 1",
            params![session_id],
            |row| {
                Ok(CodexStopIdleReceipt {
                    session_id: row.get(0)?,
                    generation_id: row.get(1)?,
                    credential_id: row.get(2)?,
                    transcript_epoch: row.get(3)?,
                    resume_invocation_id: row.get(4)?,
                    invocation_boundary_rowid: row.get(5)?,
                    native_session_id: row.get(6)?,
                    stop_rowid: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn eligible_setup_retained_first_turn_stop(
    connection: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<Option<SetupRetainedFirstTurnStopReceipt>> {
    connection.query_row(
        "SELECT s.id,rg.id,rc.id,s.transcript_epoch,s.native_session_id,s.process_identity_json,
                (SELECT MAX(h.rowid) FROM hook_events h WHERE h.session_id=s.id),sp.id
         FROM sessions s
         JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id
         JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=rg.role
           AND rs.effective_generation_id=rg.id
         JOIN role_credentials rc ON rc.role_generation_id=rg.id AND rc.revoked_at IS NULL
         JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
         WHERE (?1 IS NULL OR s.id=?1)
           AND s.status='running' AND s.readiness_state='idle_candidate'
           AND rg.status='running' AND s.resume_count=0
           AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
           AND s.process_identity_json IS NOT NULL AND s.process_identity_json!=''
           AND sp.state='issued' AND sp.attempt_id=a.id AND sp.role=rg.role
           AND sp.purpose IN ('setup_discovery','profile_probe')
           AND rg.role!='final_verifier'
           AND ((sp.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
             OR (sp.purpose='profile_probe' AND s.validation_cell='trip_setup_probe'))
           AND (SELECT h.event_name FROM hook_events h WHERE h.session_id=s.id
                ORDER BY h.rowid DESC LIMIT 1)='Stop'
           AND (SELECT h.native_session_id FROM hook_events h WHERE h.session_id=s.id
                ORDER BY h.rowid DESC LIMIT 1)=s.native_session_id
           AND (SELECT h.provenance_state FROM hook_events h WHERE h.session_id=s.id
                ORDER BY h.rowid DESC LIMIT 1)='managed_process_group_untrusted_payload'
           AND NOT EXISTS(SELECT 1 FROM role_results result
             WHERE result.session_id=s.id AND result.role_generation_id=rg.id
               AND result.outcome='capability_observed')
           AND NOT EXISTS(SELECT 1 FROM permission_requests permission
             WHERE permission.session_id=s.id AND permission.consumed_at IS NULL
               AND permission.delivery_state NOT IN ('expired','not_delivered'))
           AND NOT EXISTS(SELECT 1 FROM input_leases lease
             WHERE lease.session_id=s.id AND lease.revoked_at IS NULL
               AND julianday(lease.expires_at)>julianday('now'))
           AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
             WHERE guidance.role_generation_id=rg.id
               AND guidance.state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))
           AND NOT EXISTS(SELECT 1 FROM controls control WHERE control.attempt_id=a.id
             AND control.state IN ('requested','draining','held','recovery_required'))
           AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
             WHERE recovery.session_id=s.id AND recovery.state!='resolved_quiescent')
         ORDER BY s.updated_at,s.rowid LIMIT 1",
        params![session_id],
        |row| Ok(SetupRetainedFirstTurnStopReceipt {
            session_id: row.get(0)?, generation_id: row.get(1)?, credential_id: row.get(2)?,
            transcript_epoch: row.get(3)?, native_session_id: row.get(4)?,
            process_identity_json: row.get(5)?, stop_rowid: row.get(6)?, setup_permit_id: row.get(7)?,
        }),
    ).optional().map_err(Into::into)
}

fn setup_retained_first_turn_stop_timeout_candidate_in(
    connection: &rusqlite::Connection,
    session_id: Option<&str>,
    now: &str,
) -> Result<Option<SetupRetainedFirstTurnStopTimeoutReceipt>> {
    connection
        .query_row(
            "SELECT s.id,rg.id,s.transcript_epoch,s.native_session_id,s.process_identity_json,
                    CAST(json_extract(audit.detail_json,'$.stop_event_rowid') AS INTEGER),audit.rowid
             FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN audit_events audit ON audit.entity_kind='session' AND audit.entity_id=s.id
               AND audit.actor_kind='service'
               AND audit.event_code='setup.retained_first_turn.stop_claimed'
             WHERE (?1 IS NULL OR s.id=?1)
               AND s.status='interrupt_requested' AND s.readiness_state='idle_candidate'
               AND json_extract(audit.detail_json,'$.role_generation_id')=rg.id
               AND json_extract(audit.detail_json,'$.transcript_epoch')=s.transcript_epoch
               AND json_extract(audit.detail_json,'$.native_session_id')=s.native_session_id
               AND json_extract(audit.detail_json,'$.process_identity_json')=s.process_identity_json
               AND julianday(json_extract(audit.detail_json,'$.timeout_at'))<=julianday(?2)
               AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                 WHERE recovery.session_id=s.id AND recovery.state!='resolved_quiescent')
             ORDER BY audit.created_at,audit.rowid,s.id LIMIT 1",
            params![session_id, now],
            |row| {
                Ok(SetupRetainedFirstTurnStopTimeoutReceipt {
                    session_id: row.get(0)?,
                    generation_id: row.get(1)?,
                    transcript_epoch: row.get(2)?,
                    native_session_id: row.get(3)?,
                    process_identity_json: row.get(4)?,
                    stop_rowid: row.get(5)?,
                    claim_rowid: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn eligible_manager_service_stop(
    connection: &rusqlite::Connection,
    attempt_id: &str,
    phase: &str,
) -> Result<Option<ManagerServiceStopReceipt>> {
    connection
        .query_row(
            "WITH current_manager AS (
               SELECT s.id AS session_id,rg.id AS generation_id,rc.id AS credential_id,
                      s.transcript_epoch,
                      (SELECT ri.id FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1) AS resume_invocation_id,
                      t.version AS task_version,a.plan_hash,a.candidate_hash,
                      COALESCE((SELECT ri.hook_event_boundary_rowid FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1),s.initial_hook_event_boundary_rowid) AS boundary
               FROM attempts a JOIN tasks t ON t.id=a.task_id
               JOIN role_settings rs ON rs.task_id=t.id AND rs.role='manager'
               JOIN role_generations rg ON rg.id=rs.effective_generation_id
                 AND rg.attempt_id=a.id AND rg.role='manager'
               JOIN sessions s ON s.role_generation_id=rg.id
               JOIN role_credentials rc ON rc.role_generation_id=rg.id AND rc.revoked_at IS NULL
               WHERE a.id=?1 AND a.phase=?2 AND a.status='running'
                 AND a.candidate_hash IS NOT NULL
                 AND t.lifecycle IN ('in_progress','validation','awaiting_review')
                 AND t.archived_at IS NULL AND t.attention='none'
                 AND rg.status='running' AND s.status='running'
                 AND s.readiness_state='busy_unresolved_hook_work'
                 AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
                 AND s.id=(SELECT latest.id FROM sessions latest
                   WHERE latest.role_generation_id=rg.id
                   ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
             ), latest_stop AS (
               SELECT current_manager.*,h.rowid AS stop_rowid
               FROM current_manager JOIN hook_events h ON h.session_id=current_manager.session_id
               WHERE h.rowid=(SELECT MAX(latest.rowid) FROM hook_events latest
                 WHERE latest.session_id=current_manager.session_id AND latest.rowid>current_manager.boundary)
                 AND h.event_name='Stop'
                 AND h.role_generation_id=current_manager.generation_id
                 AND h.native_session_id=(SELECT native_session_id FROM sessions
                   WHERE id=current_manager.session_id)
             ), stopped_turn AS (
               SELECT latest_stop.*,
                      (SELECT MAX(submit.rowid) FROM hook_events submit
                       WHERE submit.session_id=latest_stop.session_id
                         AND submit.role_generation_id=latest_stop.generation_id
                         AND submit.event_name='UserPromptSubmit'
                         AND submit.rowid>latest_stop.boundary
                         AND submit.rowid<latest_stop.stop_rowid) AS submit_rowid
               FROM latest_stop
             )
             SELECT stopped_turn.session_id,stopped_turn.generation_id,
                    stopped_turn.credential_id,stopped_turn.transcript_epoch,
                    stopped_turn.resume_invocation_id,stopped_turn.boundary,
                    stopped_turn.stop_rowid,
                    stopped_turn.task_version,stopped_turn.plan_hash,
                    stopped_turn.candidate_hash
             FROM stopped_turn
             WHERE stopped_turn.submit_rowid IS NOT NULL
               AND EXISTS(SELECT 1 FROM hook_events start
                 WHERE start.session_id=stopped_turn.session_id
                   AND start.role_generation_id=stopped_turn.generation_id
                   AND start.event_name='SessionStart'
                   AND start.rowid>stopped_turn.boundary
                   AND start.rowid<stopped_turn.submit_rowid)
               AND (SELECT COUNT(*) FROM hook_events started
                    WHERE started.session_id=stopped_turn.session_id
                      AND started.event_name='PreToolUse'
                      AND started.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
                 > (SELECT COUNT(*) FROM hook_events finished
                    WHERE finished.session_id=stopped_turn.session_id
                      AND finished.event_name IN ('PostToolUse','PostToolUseFailure')
                      AND finished.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
               AND (SELECT COUNT(*) FROM hook_events started
                    WHERE started.session_id=stopped_turn.session_id
                      AND started.event_name='SubagentStart'
                      AND started.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
                 = (SELECT COUNT(*) FROM hook_events finished
                    WHERE finished.session_id=stopped_turn.session_id
                      AND finished.event_name='SubagentStop'
                      AND finished.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid)
               AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                 WHERE permission.session_id=stopped_turn.session_id
                   AND permission.consumed_at IS NULL
                   AND permission.delivery_state NOT IN ('expired','not_delivered'))
               AND NOT EXISTS(SELECT 1 FROM input_leases lease
                 WHERE lease.session_id=stopped_turn.session_id AND lease.revoked_at IS NULL
                   AND julianday(lease.expires_at)>julianday('now'))
               AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
                 WHERE guidance.role_generation_id=stopped_turn.generation_id
                   AND guidance.state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))
               AND NOT EXISTS(SELECT 1 FROM controls control
                 WHERE control.attempt_id=?1 AND control.state IN ('requested','draining'))
               AND NOT EXISTS(SELECT 1 FROM switch_intents switch
                 WHERE switch.attempt_id=?1 AND switch.state IN ('stopping_old','ready_for_dispatch'))
               AND NOT EXISTS(SELECT 1 FROM restart_candidates restart
                 WHERE restart.attempt_id=?1
                   AND restart.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
               AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                 WHERE recovery.attempt_id=?1 AND recovery.state='attention_required')
               AND NOT EXISTS(SELECT 1 FROM freeze_intents freeze
                 WHERE freeze.attempt_id=?1
                   AND freeze.state IN ('reserved','capturing','recovery_required'))
               AND NOT EXISTS(SELECT 1 FROM check_runs check_run
                 WHERE check_run.attempt_id=?1
                   AND check_run.status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))
               AND NOT EXISTS(SELECT 1 FROM role_generations other
                 WHERE other.attempt_id=?1 AND other.id!=stopped_turn.generation_id
                   AND other.status IN ('launch_reserved','running','stopping'))",
            params![attempt_id, phase],
            |row| {
                Ok(ManagerServiceStopReceipt {
                    session_id: row.get(0)?,
                    generation_id: row.get(1)?,
                    credential_id: row.get(2)?,
                    transcript_epoch: row.get(3)?,
                    resume_invocation_id: row.get(4)?,
                    invocation_boundary_rowid: row.get(5)?,
                    stop_rowid: row.get(6)?,
                    task_version: row.get(7)?,
                    plan_hash: row.get(8)?,
                    candidate_hash: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn eligible_manager_plan(
    connection: &rusqlite::Connection,
    attempt_id: &str,
    require_quiescent: bool,
) -> Result<Option<EligibleManagerPlan>> {
    let mut rejected_statement = connection.prepare(
        "SELECT candidate_hash FROM review_requests
         WHERE attempt_id=?1 AND review_kind='plan' AND delivery_state='finished'
           AND verdict IN ('request_changes','needs_rework')",
    )?;
    let rejected_hashes = rejected_statement
        .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(rejected_statement);
    let mut statement = connection.prepare(
        "SELECT rr.id,rr.role_generation_id,s.id,s.status,s.readiness_state,
                json_extract(rr.metadata_json,'$.plan'),a.base_revision,t.version
         FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN sessions s ON s.id=rr.session_id AND s.role_generation_id=rg.id
         JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager'
         WHERE a.id=?1 AND rg.role='manager' AND rr.outcome='plan_ready'
           AND rr.consumed_at IS NULL
           AND (NOT EXISTS(SELECT 1 FROM review_requests rejected
                  WHERE rejected.attempt_id=a.id AND rejected.review_kind='plan'
                    AND rejected.delivery_state='finished'
                    AND rejected.verdict IN ('request_changes','needs_rework'))
                OR json_extract(rr.metadata_json,'$.rejected_plan_review_request_id')=(
                  SELECT latest.id FROM review_requests latest
                  WHERE latest.attempt_id=a.id AND latest.review_kind='plan'
                    AND latest.delivery_state='finished'
                    AND latest.verdict IN ('request_changes','needs_rework')
                  ORDER BY latest.rowid DESC LIMIT 1))
           AND (?2=0 OR s.status='exited'
                OR (s.status='running' AND s.readiness_state='idle_candidate'))
         ORDER BY rr.rowid DESC",
    )?;
    let candidates = statement
        .query_map(params![attempt_id, require_quiescent], |row| {
            Ok(EligibleManagerPlan {
                result_id: row.get(0)?,
                generation_id: row.get(1)?,
                session_id: row.get(2)?,
                session_status: row.get(3)?,
                readiness_state: row.get(4)?,
                plan: row.get(5)?,
                base_revision: row.get(6)?,
                task_version: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(candidates.into_iter().find(|candidate| {
        let hash = hex::encode(Sha256::digest(candidate.plan.as_bytes()));
        !rejected_hashes.iter().any(|rejected| rejected == &hash)
    }))
}

fn fresh_resume_rejection_fields(
    binding: &serde_json::Value,
) -> Result<(String, String, String, String, i64)> {
    let binding = binding
        .as_object()
        .ok_or_else(|| anyhow!("fresh rejected-session binding must be an object"))?;
    let required = |name: &str| {
        binding
            .get(name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("fresh rejected-session binding is missing {name}"))
    };
    let resume_count = binding
        .get("resume_count")
        .and_then(serde_json::Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| anyhow!("fresh rejected-session binding is missing resume_count"))?;
    Ok((
        required("rejection_event_id")?,
        required("session_id")?,
        required("role_generation_id")?,
        required("transcript_epoch")?,
        resume_count,
    ))
}

fn reserve_typed_setup_fresh_route(
    transaction: &Transaction<'_>,
    binding: &serde_json::Value,
    context: &RoleLaunchContext,
    config: &LaunchConfig,
    capability_key: &str,
    attempt_id: &str,
    setup_permit_id: &str,
    setup_validation_cell: &str,
    now: &str,
) -> Result<()> {
    let (event_id, session_id, generation_id, transcript_epoch, resume_count) =
        fresh_resume_rejection_fields(binding)?;
    let authorized: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM audit_events event
            JOIN sessions prior ON prior.id=event.entity_id
            JOIN role_generations prior_generation ON prior_generation.id=prior.role_generation_id
            JOIN attempts prior_attempt ON prior_attempt.id=prior_generation.attempt_id
            JOIN tasks prior_task ON prior_task.id=prior_attempt.task_id
            JOIN trip_setup_permits permit ON permit.id=prior.setup_permit_id
            JOIN trip_setup_operations setup ON setup.id=permit.setup_operation_id
            JOIN trip_setup_profile_selections selection
              ON selection.setup_operation_id=setup.id AND selection.role=permit.role
            JOIN role_settings setting
              ON setting.task_id=prior_task.id AND setting.role=prior_generation.role
             AND setting.revision=prior_generation.config_revision
            WHERE event.id=?1 AND event.event_code='session.resume.rejected'
              AND prior.id=?2 AND prior_generation.id=?3
              AND prior.transcript_epoch=?4 AND prior.resume_count=?5
              AND prior.status='exited'
              AND COALESCE(json_extract(prior.exit_json,'$.process_group_quiescent'),0)=1
              AND prior_generation.status='exited'
              AND json_extract(event.detail_json,'$.session_id')=prior.id
              AND json_extract(event.detail_json,'$.role_generation_id')=prior_generation.id
              AND json_extract(event.detail_json,'$.transcript_epoch')=prior.transcript_epoch
              AND CAST(json_extract(event.detail_json,'$.resume_count') AS INTEGER)=prior.resume_count
              AND json_extract(event.detail_json,'$.attempt_id')=prior_attempt.id
              AND json_extract(event.detail_json,'$.role')=prior_generation.role
              AND CAST(json_extract(event.detail_json,'$.config_revision') AS INTEGER)=prior_generation.config_revision
              AND json_extract(event.detail_json,'$.setup_permit_id')=permit.id
              AND json_extract(event.detail_json,'$.category')='frozen_runtime_identity_changed'
              AND json_extract(event.detail_json,'$.same_profile_authority')=1
              AND prior_attempt.id=?6 AND prior_generation.role=?7
              AND prior_generation.config_revision=?8 AND permit.id=?9
              AND prior.validation_cell=?10
              AND prior_attempt.id=(SELECT latest.id FROM attempts latest
                WHERE latest.task_id=prior_attempt.task_id
                ORDER BY latest.created_at DESC LIMIT 1)
              AND prior_attempt.phase IS json_extract(event.detail_json,'$.attempt_phase')
              AND prior_attempt.candidate_hash IS json_extract(event.detail_json,'$.candidate_hash')
              AND prior_attempt.plan_hash IS json_extract(event.detail_json,'$.plan_hash')
              AND setting.effective_generation_id=prior_generation.id
              AND permit.state='issued' AND permit.attempt_id=prior_attempt.id
              AND permit.role=prior_generation.role
              AND permit.settings_revision=prior_generation.config_revision
              AND selection.selection_state='selected' AND selection.profile_hash=permit.profile_hash
              AND ((permit.purpose='setup_discovery'
                    AND prior.validation_cell='trip_setup_discovery'
                    AND permit.approved_action='bounded_inventory_and_contained_read'
                    AND permit.nonce IS NULL AND setup.state='discovery'
                    AND setup.discovery_attempt_id=prior_attempt.id)
                   OR (permit.purpose='profile_probe'
                    AND prior.validation_cell='trip_setup_probe'
                    AND permit.approved_action='nonce_only_profile_invocation'
                    AND permit.nonce IS NOT NULL AND permit.nonce!=''
                    AND setup.state='probing' AND setup.probe_attempt_id=prior_attempt.id))
              AND json_type(event.detail_json,'$.frozen_capability_key')='text'
              AND json_type(event.detail_json,'$.observed_capability_key')='text'
              AND json_extract(event.detail_json,'$.frozen_capability_key')
                    IS NOT json_extract(event.detail_json,'$.observed_capability_key')
              AND json_type(event.detail_json,'$.observed_identity')='object'
              AND json_extract(event.detail_json,'$.observed_identity.provider')=?11
              AND json_extract(event.detail_json,'$.observed_identity.executable_version')=?12
              AND json_extract(event.detail_json,'$.observed_identity.role')=?13
              AND json_extract(event.detail_json,'$.observed_identity.model')=?14
              AND json_extract(event.detail_json,'$.observed_identity.effort')=?15
              AND json_extract(event.detail_json,'$.observed_identity.mode')='interactive_pty'
              AND json_extract(event.detail_json,'$.observed_capability_key')=?16
              AND EXISTS(
                SELECT 1 FROM capabilities current_capability
                WHERE current_capability.rowid=(
                    SELECT latest_capability.rowid FROM capabilities latest_capability
                    WHERE latest_capability.provider=json_extract(event.detail_json,'$.observed_identity.provider')
                      AND latest_capability.executable_version=json_extract(event.detail_json,'$.observed_identity.executable_version')
                      AND latest_capability.role=json_extract(event.detail_json,'$.observed_identity.role')
                      AND latest_capability.mode=json_extract(event.detail_json,'$.observed_identity.mode')
                    ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1)
                  AND current_capability.status='supported'
                  AND current_capability.proof_json!='{}'
                  AND current_capability.config_hash=json_extract(event.detail_json,'$.observed_capability_key'))
              AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                WHERE recovery.session_id=prior.id AND recovery.state='attention_required')
              AND NOT EXISTS(SELECT 1 FROM role_generations newer
                WHERE newer.attempt_id=prior_generation.attempt_id
                  AND newer.role=prior_generation.role
                  AND newer.lane_id=prior_generation.lane_id
                  AND newer.generation>prior_generation.generation)
              AND NOT EXISTS(SELECT 1 FROM audit_events consumed
                WHERE consumed.event_code='session.resume.fresh_route.reserved'
                  AND json_extract(consumed.detail_json,'$.rejection_event_id')=event.id)
        )",
        params![
            event_id,
            session_id,
            generation_id,
            transcript_epoch,
            resume_count,
            attempt_id,
            config.role.to_string(),
            context.settings_revision,
            setup_permit_id,
            setup_validation_cell,
            config.provider.to_string(),
            &config.executable_version,
            config.role.to_string(),
            &config.model,
            &config.effort,
            capability_key,
        ],
        |row| row.get(0),
    )?;
    if !authorized {
        bail!("fresh typed setup dispatch no longer matches the exact rejected-session authority")
    }
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','session.resume.fresh_route.reserved','session',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            session_id,
            serde_json::json!({
                "rejection_event_id":event_id,
                "role_generation_id":generation_id,
                "transcript_epoch":transcript_epoch,
                "resume_count":resume_count,
                "attempt_id":attempt_id,
                "fresh_dispatch":"trip_setup_dispatch",
            }).to_string(),
            now,
        ],
    )?;
    Ok(())
}

fn validate_frozen_capability(
    connection: &rusqlite::Connection,
    session_id: &str,
    current_key: &str,
    cwd: &std::path::Path,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<()> {
    let frozen: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT capability_key,capability_identity_json FROM sessions WHERE id=?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (frozen_key, frozen_identity) = frozen.ok_or_else(|| {
        anyhow!("session predates frozen capability identity; exact resume requires a fresh accounted session")
    })?;
    let frozen_identity = frozen_identity.ok_or_else(|| {
        anyhow!("session predates frozen capability identity; exact resume requires a fresh accounted session")
    })?;
    let frozen_identity: crate::domain::CapabilityIdentity =
        serde_json::from_str(&frozen_identity)?;
    crate::providers::require_current_capability_identity_with_bundles(
        &frozen_identity,
        cwd,
        bundles,
    )?;
    let frozen = frozen_key.ok_or_else(|| {
        anyhow!("session predates frozen capability identity; exact resume requires a fresh accounted session")
    })?;
    if frozen != current_key {
        bail!("executable, hook, security policy, arguments, environment, model, or effort changed; exact resume requires a fresh accounted session")
    }
    Ok(())
}

fn has_resolved_validation_recovery_authority(
    connection: &rusqlite::Connection,
    session_id: &str,
    role_generation_id: &str,
    setup_permit_id: Option<&str>,
) -> Result<bool> {
    let Some(setup_permit_id) = setup_permit_id else {
        return Ok(false);
    };
    connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM recovery_records recovery
           JOIN sessions s ON s.id=recovery.session_id
           JOIN role_generations rg ON rg.id=s.role_generation_id
           JOIN attempts a ON a.id=rg.attempt_id
           JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
           WHERE recovery.session_id=?1 AND recovery.state='resolved_quiescent'
             AND recovery.resolved_at IS NOT NULL AND s.id=?1 AND rg.id=?2 AND permit.id=?3
             AND permit.state='issued' AND permit.attempt_id=a.id AND permit.role=rg.role
             AND permit.setup_operation_id=a.setup_operation_id
             AND permit.purpose IN ('setup_discovery','profile_probe','runtime_probe')
             AND ((permit.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
               OR (permit.purpose='profile_probe' AND s.validation_cell='trip_setup_probe')
               OR (permit.purpose='runtime_probe' AND s.validation_cell='trip_runtime_probe'))
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.recovery_record_id')=recovery.id
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.session_id')=s.id
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.attempt_id')=a.id
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.role_generation_id')=rg.id
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.setup_permit_id')=permit.id
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.setup_operation_id')=permit.setup_operation_id
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.transcript_epoch')=s.transcript_epoch
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.resume_count')=s.resume_count
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.validation_cell')=s.validation_cell
             AND json_extract(recovery.detail_json,'$.resolved_resume_authority.purpose')=permit.purpose
             AND NOT EXISTS(SELECT 1 FROM recovery_records unresolved
                            WHERE unresolved.session_id=s.id AND unresolved.state!='resolved_quiescent')
         )",
        params![session_id, role_generation_id, setup_permit_id],
        |row| row.get(0),
    ).map_err(Into::into)
}

fn has_completed_setup_first_turn_stop_authority(
    connection: &rusqlite::Connection,
    session_id: &str,
    role_generation_id: &str,
    setup_permit_id: Option<&str>,
) -> Result<bool> {
    let Some(setup_permit_id) = setup_permit_id else {
        return Ok(false);
    };
    connection.query_row(
        "SELECT EXISTS(SELECT 1
         FROM sessions s
         JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
         JOIN audit_events audit ON audit.entity_kind='session' AND audit.entity_id=s.id
           AND audit.actor_kind='service'
           AND audit.event_code='setup.retained_first_turn.stop_claimed'
         JOIN role_credentials revoked ON revoked.id=json_extract(audit.detail_json,'$.credential_id')
         WHERE s.id=?1 AND rg.id=?2 AND sp.id=?3
           AND s.status='exited' AND rg.status='exited' AND s.resume_count=0
           AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
           AND sp.state='issued' AND sp.attempt_id=rg.attempt_id AND sp.role=rg.role
           AND sp.purpose IN ('setup_discovery','profile_probe') AND rg.role!='final_verifier'
           AND ((sp.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
             OR (sp.purpose='profile_probe' AND s.validation_cell='trip_setup_probe'))
           AND json_extract(audit.detail_json,'$.role_generation_id')=rg.id
           AND json_extract(audit.detail_json,'$.transcript_epoch')=s.transcript_epoch
           AND json_extract(audit.detail_json,'$.native_session_id')=s.native_session_id
           AND json_extract(audit.detail_json,'$.process_identity_json')=s.process_identity_json
           AND json_extract(audit.detail_json,'$.setup_permit_id')=sp.id
           AND json_extract(audit.detail_json,'$.stop_event_rowid')=(SELECT MAX(h.rowid)
             FROM hook_events h WHERE h.session_id=s.id)
           AND EXISTS(SELECT 1 FROM hook_events stop
             WHERE stop.rowid=json_extract(audit.detail_json,'$.stop_event_rowid')
               AND stop.session_id=s.id AND stop.role_generation_id=rg.id
               AND stop.event_name='Stop' AND stop.native_session_id=s.native_session_id
               AND stop.provenance_state='managed_process_group_untrusted_payload')
           AND COALESCE(json_extract(audit.detail_json,'$.resume_spent'),1)=0
           AND revoked.role_generation_id=rg.id AND revoked.revoked_at IS NOT NULL
           AND NOT EXISTS(SELECT 1 FROM role_credentials current
             WHERE current.role_generation_id=rg.id AND current.revoked_at IS NULL)
           AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
             WHERE recovery.session_id=s.id AND recovery.state!='resolved_quiescent'))",
        params![session_id, role_generation_id, setup_permit_id],
        |row| row.get(0),
    ).map_err(Into::into)
}

fn validate_typed_setup_launch(
    connection: &rusqlite::Connection,
    attempt_id: &str,
    context: &RoleLaunchContext,
    launch: &LaunchConfig,
    setup_permit_id: &str,
) -> Result<bool> {
    let exact: Option<(String, String, String)> = connection
        .query_row(
            "SELECT rs.config_json,selected.profile_json,sp.profile_hash
             FROM launch_permits lp
             JOIN trip_setup_permits sp ON sp.id=lp.setup_permit_id
             JOIN attempts a ON a.id=lp.attempt_id AND a.id=sp.attempt_id
               AND a.setup_operation_id=sp.setup_operation_id
             JOIN tasks t ON t.id=a.task_id
             JOIN projects fixture ON fixture.id=t.project_id AND fixture.id=sp.fixture_project_id
             JOIN workspaces w ON w.attempt_id=a.id
             JOIN trip_setup_operations so ON so.id=sp.setup_operation_id
               AND so.fixture_project_id=fixture.id AND so.validation_task_id=t.id
             JOIN trip_project_state project_state ON project_state.project_id=so.project_id
               AND project_state.setup_operation_id=so.id
             JOIN trip_setup_profile_selections selected
               ON selected.setup_operation_id=so.id AND selected.role=sp.role
             JOIN role_settings rs ON rs.task_id=t.id AND rs.role=sp.role
               AND rs.revision=sp.settings_revision
             WHERE lp.id=?1 AND lp.attempt_id=?2 AND lp.role=?3
               AND lp.settings_revision=?4 AND lp.state='issued' AND lp.validation_dispatch=1
               AND sp.id=?5 AND sp.role=lp.role AND sp.settings_revision=lp.settings_revision
               AND sp.state='issued' AND sp.purpose IN ('setup_discovery','profile_probe')
               AND ((sp.purpose='setup_discovery'
                     AND sp.approved_action='bounded_inventory_and_contained_read'
                     AND sp.nonce IS NULL AND so.state='discovery' AND so.discovery_attempt_id=a.id)
                    OR (sp.purpose='profile_probe'
                     AND sp.approved_action='nonce_only_profile_invocation'
                     AND sp.nonce IS NOT NULL AND sp.nonce!=''
                     AND so.state='probing' AND so.probe_attempt_id=a.id))
               AND a.phase='planning' AND a.status='held'
               AND t.lifecycle='validation' AND t.attention='paused' AND t.archived_at IS NULL
               AND fixture.internal_purpose='trip_setup_fixture'
               AND fixture.repository_identity=sp.fixture_repository_identity
               AND w.repository_identity=sp.fixture_repository_identity
               AND w.state='ready' AND w.path=?6
               AND (project_state.readiness='setup_in_progress' OR
                    (project_state.readiness='ready' AND so.supersedes_setup_operation_id IS NOT NULL
                     AND EXISTS(SELECT 1 FROM trip_setup_operations active
                       WHERE active.id=so.supersedes_setup_operation_id
                         AND active.project_id=so.project_id AND active.state='activated')))
               AND selected.selection_state='selected' AND selected.profile_hash=sp.profile_hash",
            params![
                context.permit_id,
                attempt_id,
                launch.role.to_string(),
                context.settings_revision,
                setup_permit_id,
                context.workspace.to_string_lossy()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((settings_json, selected_profile_json, profile_hash)) = exact else {
        bail!("typed setup launch authority is stale, consumed, or mismatched")
    };
    let settings: crate::domain::RoleOverride = serde_json::from_str(&settings_json)?;
    let selected_profile: serde_json::Value = serde_json::from_str(&selected_profile_json)?;
    let settings_provider = settings.provider.to_string();
    if hex::encode(Sha256::digest(selected_profile_json.as_bytes())) != profile_hash
        || selected_profile
            .get("provider")
            .and_then(serde_json::Value::as_str)
            != Some(settings_provider.as_str())
        || selected_profile
            .get("model")
            .and_then(serde_json::Value::as_str)
            != Some(settings.model.as_str())
        || selected_profile
            .get("effort")
            .and_then(serde_json::Value::as_str)
            != Some(settings.effort.as_str())
        || settings.provider != launch.provider
        || settings.model != launch.model
        || settings.effort != launch.effort
        || launch.cwd != context.workspace
    {
        bail!("typed setup launch no longer matches its exact frozen profile and workspace")
    }
    Ok(true)
}

fn validate_typed_setup_resume(
    connection: &rusqlite::Connection,
    session_id: &str,
    launch: &LaunchConfig,
    current_key: &str,
) -> Result<bool> {
    let permit: Option<(String, String, Option<String>)> = connection
        .query_row(
            "SELECT sp.id,sp.purpose,s.validation_cell FROM sessions s
             JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
             WHERE s.id=?1",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((permit_id, purpose, validation_cell)) = permit else {
        return Ok(false);
    };
    if purpose == "runtime_probe" {
        if validation_cell.as_deref() != Some("trip_runtime_probe") {
            bail!("runtime probe permit does not match its dedicated validation cell")
        }
        return Ok(false);
    }
    if !matches!(purpose.as_str(), "setup_discovery" | "profile_probe") {
        bail!("setup resume permit has an unsupported typed purpose")
    }
    if launch.role == RoleKind::FinalReviewer {
        bail!("final verifier sessions are always fresh and cannot resume")
    }
    let expected_cell = if purpose == "setup_discovery" {
        "trip_setup_discovery"
    } else {
        "trip_setup_probe"
    };
    let expected_action = if purpose == "setup_discovery" {
        "bounded_inventory_and_contained_read"
    } else {
        "nonce_only_profile_invocation"
    };
    let workspace = launch.cwd.to_string_lossy();
    let exact: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1
         FROM sessions s
         JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id
         JOIN tasks t ON t.id=a.task_id
         JOIN projects fixture ON fixture.id=t.project_id
         JOIN workspaces w ON w.attempt_id=a.id
         JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
         JOIN trip_setup_operations so ON so.id=sp.setup_operation_id
         JOIN trip_project_state project_state ON project_state.project_id=so.project_id
         JOIN trip_setup_profile_selections selected
           ON selected.setup_operation_id=so.id AND selected.role=sp.role
         JOIN role_settings rs
           ON rs.task_id=t.id AND rs.role=rg.role AND rs.revision=rg.config_revision
         WHERE s.id=?1 AND sp.id=?2 AND sp.purpose=?3 AND sp.state='issued'
           AND s.resume_count=0
           AND NOT (sp.role='manager' AND EXISTS(
             SELECT 1 FROM controls c WHERE c.attempt_id=sp.attempt_id
               AND c.kind='setup_manager_change' AND c.state='held'
           ))
           AND s.validation_cell=?4 AND sp.approved_action=?5
           AND sp.attempt_id=a.id AND sp.attempt_id=rg.attempt_id
           AND sp.setup_operation_id=a.setup_operation_id
           AND sp.fixture_project_id=fixture.id AND sp.fixture_project_id=so.fixture_project_id
           AND sp.fixture_repository_identity=fixture.repository_identity
           AND sp.fixture_repository_identity=w.repository_identity
           AND fixture.internal_purpose='trip_setup_fixture'
           AND sp.role=rg.role AND sp.role=?6 AND sp.settings_revision=rg.config_revision
           AND selected.selection_state='selected' AND selected.profile_hash=sp.profile_hash
           AND rs.effective_generation_id=rg.id AND rg.status='exited'
           AND a.status='held' AND t.lifecycle='validation' AND t.attention='paused'
           AND t.archived_at IS NULL AND t.id=so.validation_task_id
           AND a.id=(SELECT latest.id FROM attempts latest
                     WHERE latest.task_id=t.id ORDER BY latest.created_at DESC LIMIT 1)
           AND ((sp.purpose='setup_discovery' AND so.state='discovery'
                 AND so.discovery_attempt_id=a.id AND sp.nonce IS NULL)
                OR (sp.purpose='profile_probe' AND so.state='probing'
                    AND so.probe_attempt_id=a.id AND sp.nonce IS NOT NULL AND sp.nonce!=''))
           AND project_state.setup_operation_id=so.id
           AND (project_state.readiness='setup_in_progress' OR
                (project_state.readiness='ready' AND so.supersedes_setup_operation_id IS NOT NULL
                 AND EXISTS(SELECT 1 FROM trip_setup_operations active
                   WHERE active.id=so.supersedes_setup_operation_id
                     AND active.project_id=so.project_id AND active.state='activated')))
           AND w.state='ready' AND w.path=?7
           AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
           AND s.native_identity_source='managed_descendant_hook_unverified'
           AND s.hook_trust_state='observed_unverified'
           AND s.capability_identity_json IS NOT NULL
           AND s.capability_key=?8)",
        params![
            session_id,
            permit_id,
            purpose,
            expected_cell,
            expected_action,
            launch.role.to_string(),
            workspace.as_ref(),
            current_key
        ],
        |row| row.get(0),
    )?;
    if !exact {
        bail!("typed setup resume authority is stale, consumed, or mismatched")
    }
    let (settings_json, selected_profile_json, profile_hash, original_launch_json, invocation_json, prompt_hash, workflow_version, workflow_hash, role_prompt_hash, generation): (String,String,String,String,String,String,String,String,String,String)=connection.query_row(
        "SELECT rs.config_json,selected.profile_json,sp.profile_hash,s.launch_config_json,s.invocation_input_json,
                s.prompt_hash,s.workflow_version,s.workflow_hash,s.prompt_hash,rg.id
         FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN role_settings rs ON rs.task_id=t.id AND rs.role=rg.role AND rs.revision=rg.config_revision
         JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
         JOIN trip_setup_profile_selections selected
           ON selected.setup_operation_id=sp.setup_operation_id AND selected.role=sp.role
         WHERE s.id=?1 AND sp.id=?2",
        params![session_id,permit_id],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?))
    )?;
    let settings: crate::domain::RoleOverride = serde_json::from_str(&settings_json)?;
    let selected_profile: serde_json::Value = serde_json::from_str(&selected_profile_json)?;
    let settings_provider = settings.provider.to_string();
    if hex::encode(Sha256::digest(selected_profile_json.as_bytes())) != profile_hash
        || selected_profile
            .get("provider")
            .and_then(serde_json::Value::as_str)
            != Some(settings_provider.as_str())
        || selected_profile
            .get("model")
            .and_then(serde_json::Value::as_str)
            != Some(settings.model.as_str())
        || selected_profile
            .get("effort")
            .and_then(serde_json::Value::as_str)
            != Some(settings.effort.as_str())
        || settings.provider != launch.provider
        || settings.model != launch.model
        || settings.effort != launch.effort
    {
        bail!("typed setup resume no longer matches its exact frozen profile")
    }
    let original_launch: LaunchConfig = serde_json::from_str(&original_launch_json)?;
    if original_launch.cwd != launch.cwd
        || original_launch.role != launch.role
        || crate::providers::capability_identity_key(&crate::providers::capability_identity(
            &original_launch,
        )?)? != current_key
    {
        bail!("typed setup resume changed its original configuration or confinement")
    }
    let invocation: serde_json::Value = serde_json::from_str(&invocation_json)?;
    let prompt = invocation
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("typed setup resume lost its original prompt"))?;
    if prompt.trim().is_empty()
        || invocation
            .get("prompt_hash")
            .and_then(serde_json::Value::as_str)
            != Some(json_hash(&prompt)?.as_str())
        || invocation
            .get("review_request_id")
            .is_some_and(|value| !value.is_null())
        || invocation
            .get("workflow_version")
            .and_then(serde_json::Value::as_str)
            != Some(workflow_version.as_str())
        || invocation
            .get("workflow_hash")
            .and_then(serde_json::Value::as_str)
            != Some(workflow_hash.as_str())
        || invocation
            .get("role_prompt_hash")
            .and_then(serde_json::Value::as_str)
            != Some(role_prompt_hash.as_str())
        || prompt_hash != crate::workflow_resources::prompt_hash(launch.role)
        || workflow_version != crate::workflow_resources::WORKFLOW_VERSION
        || workflow_hash != crate::workflow_resources::workflow_hash()
    {
        bail!("typed setup resume lost its original prompt or workflow provenance")
    }
    crate::trip::require_setup_session_confinement(connection, session_id, &generation)?;
    Ok(true)
}

fn require_initial_lane_launch(
    connection: &Connection,
    attempt_id: &str,
    lane_id: &str,
    workspace: &Path,
    own_permit_id: Option<&str>,
) -> Result<()> {
    crate::trip::require_configured_lanes_match_reviewed(connection, attempt_id)?;
    let dispatchable: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM implementation_lanes l
         WHERE l.id=?1 AND l.attempt_id=?2 AND l.required=1 AND l.state='admitted'
           AND NOT EXISTS(SELECT 1 FROM json_each(l.dependencies_json) dependency
             JOIN implementation_lanes prerequisite
               ON prerequisite.attempt_id=l.attempt_id AND prerequisite.lane_key=dependency.value
             WHERE prerequisite.state!='yielded'))",
        params![lane_id, attempt_id],
        |row| row.get(0),
    )?;
    if !dispatchable {
        bail!("implementation lane is no longer admitted or a required predecessor is not yielded")
    }
    let prior_generation: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_generations
         WHERE attempt_id=?1 AND role='implementer' AND lane_id=?2)",
        params![attempt_id, lane_id],
        |row| row.get(0),
    )?;
    if prior_generation {
        bail!("implementation lane already has initial generation history; use its exact retained recovery path")
    }
    let competing_permit: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM launch_permits
         WHERE attempt_id=?1 AND role='implementer' AND lane_id=?2 AND state='issued'
           AND (?3 IS NULL OR id!=?3))",
        params![attempt_id, lane_id, own_permit_id],
        |row| row.get(0),
    )?;
    if competing_permit {
        bail!("implementation lane already has an unconsumed initial launch permit")
    }
    crate::trip::require_lane_sources_current(connection, attempt_id, lane_id, workspace)
}

fn require_default_integration_launch(connection: &Connection, attempt_id: &str) -> Result<()> {
    crate::trip::require_configured_lanes_match_reviewed(connection, attempt_id)?;
    let lanes = {
        let mut statement = connection.prepare(
            "SELECT lane_key,state FROM implementation_lanes
             WHERE attempt_id=?1 AND required=1 ORDER BY lane_key",
        )?;
        let rows = statement
            .query_map(params![attempt_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if lanes.is_empty() || lanes.iter().any(|(_, state)| state != "yielded") {
        bail!("default implementer authority waits for every reviewed parallel lane to yield")
    }
    let writers_active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_generations
         WHERE attempt_id=?1 AND role='implementer'
           AND status IN ('launch_reserved','running','stopping'))",
        params![attempt_id],
        |row| row.get(0),
    )?;
    if writers_active {
        bail!("default integration authority waits for every lane writer to become quiescent")
    }
    let capsule: String = connection
        .query_row(
            "SELECT r.capsule_json FROM trip_integration_requests r
             JOIN role_generations manager ON manager.id=r.requested_by_generation_id
             JOIN attempts a ON a.id=r.attempt_id
             JOIN role_settings current_manager ON current_manager.task_id=a.task_id
               AND current_manager.role='manager'
               AND current_manager.effective_generation_id=manager.id
             WHERE r.attempt_id=?1 AND r.state='requested'
               AND manager.attempt_id=r.attempt_id AND manager.role='manager'
               AND NOT EXISTS(SELECT 1 FROM role_generations newer
                 WHERE newer.attempt_id=r.attempt_id AND newer.role='manager'
                   AND newer.generation>manager.generation)",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("default implementer authority requires the unique current manager integration request")
        })?;
    let capsule: serde_json::Value = serde_json::from_str(&capsule)?;
    let ordered = capsule
        .get("ordered_lanes")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("current integration request lost its ordered lane set"))?
        .iter()
        .map(|lane| {
            lane.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("current integration request has a malformed lane key"))
        })
        .collect::<Result<Vec<_>>>()?;
    let ordered_set = ordered.iter().collect::<BTreeSet<_>>();
    let required_set = lanes.iter().map(|(lane, _)| lane).collect::<BTreeSet<_>>();
    if ordered.len() != ordered_set.len() || ordered_set != required_set {
        bail!("current integration request does not bind the complete reviewed lane set")
    }
    Ok(())
}

pub(crate) fn role_capacity_available(
    connection: &rusqlite::Connection,
    provider: &str,
    role: RoleKind,
    excluded_session: Option<&str>,
    excluded_permit: Option<&str>,
) -> Result<bool> {
    let (global, provider_active, provider_managers): (i64, i64, i64) =
        connection.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN occupied.provider=?1 THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN occupied.provider=?1 AND occupied.role='manager' THEN 1 ELSE 0 END),0)
             FROM (
               SELECT s.provider AS provider,rg.role AS role
               FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
               WHERE s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
                 AND (?2 IS NULL OR s.id!=?2)
               UNION ALL
               SELECT CASE WHEN lp.switch_intent_id IS NULL
                         THEN json_extract(rs.config_json,'$.provider')
                         ELSE json_extract(si.config_json,'$.provider') END AS provider,
                      lp.role AS role
               FROM launch_permits lp
               JOIN attempts a ON a.id=lp.attempt_id
               LEFT JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=lp.role
                 AND rs.revision=lp.settings_revision
               LEFT JOIN switch_intents si ON si.id=lp.switch_intent_id
               WHERE lp.state='issued' AND (?3 IS NULL OR lp.id!=?3)
             ) occupied",
            params![provider, excluded_session, excluded_permit],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    Ok(global < 4 && provider_active < 2 && (role != RoleKind::Manager || provider_managers < 1))
}

pub(crate) fn require_supported_capability_in(
    connection: &rusqlite::Connection,
    config: &LaunchConfig,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<()> {
    crate::providers::require_production_capability(config)?;
    let identity = crate::providers::capability_identity(config)?;
    crate::providers::require_current_capability_identity_with_bundles(
        &identity,
        &config.cwd,
        bundles,
    )?;
    let key = crate::providers::capability_key(config)?;
    let exact: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM capabilities current_capability
            WHERE current_capability.rowid=(
                SELECT latest_capability.rowid FROM capabilities latest_capability
                WHERE latest_capability.provider=?1 AND latest_capability.executable_version=?2
                  AND latest_capability.role=?3 AND latest_capability.mode='interactive_pty'
                ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1)
              AND current_capability.config_hash=?4
              AND current_capability.status='supported'
              AND current_capability.proof_json!='{}'
        )",
        params![
            config.provider.to_string(),
            config.executable_version,
            config.role.to_string(),
            key
        ],
        |row| row.get(0),
    )?;
    if exact {
        return Ok(());
    }
    bail!("provider capability is not proven for this exact effective version, role, tool/network policy, and hook bytes")
}

fn parse_value(value: String) -> serde_json::Value {
    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::Value::String(value))
}

fn parse_optional_value(value: Option<String>) -> serde_json::Value {
    value.map(parse_value).unwrap_or(serde_json::Value::Null)
}

pub fn deserialize_json<T: DeserializeOwned>(value: &str) -> Result<T> {
    Ok(serde_json::from_str(value)?)
}

pub fn role_permissions(role: RoleKind) -> Vec<String> {
    let mut permissions = vec![
        "read_context".to_owned(),
        "report_hook".to_owned(),
        "report_result".to_owned(),
    ];
    if role == RoleKind::Manager {
        permissions.push("request_next_role".to_owned());
    }
    permissions
}

#[cfg(test)]
mod interruption_tests {
    use super::*;
    use crate::domain::Provider;

    #[test]
    fn committed_role_report_replays_receipt_without_duplicate_result_or_audit() {
        let root = std::env::temp_dir().join(format!("llmrelay-report-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("state.sqlite3");
        let store = Store::open(&database).unwrap();
        store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-report-fixture','report-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','manager','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
               VALUES('settings','t','manager',1,'{}','g','2026-01-01T00:00:00Z');
             INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
               VALUES('credential','g','fixture-hash','[\"report_result\"]','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('s','g','codex','running','{}','fixture','epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
        let context = RoleContext {
            project_id: "p".into(),
            task_id: "t".into(),
            attempt_id: "a".into(),
            role_generation_id: "g".into(),
            session_id: "s".into(),
            credential_id: "credential".into(),
            transcript_epoch: "epoch".into(),
            role: RoleKind::Manager,
            provider: Provider::Codex,
            configuration_revision: 1,
            lane_id: "default".into(),
            permissions: vec!["report_result".into()],
        };
        let report = RoleResultReport {
            operation_id: uuid::Uuid::new_v4().to_string(),
            outcome: "blocked".into(),
            summary: "fixture report".into(),
            evidence: vec![],
            metadata: serde_json::json!({}),
        };
        TEST_INTERRUPT_ROLE_REPORT_AFTER_COMMIT.with(|armed| armed.set(true));
        let error = store.save_role_result(&context, &report).unwrap_err();
        assert!(error.to_string().contains("injected interruption"));
        drop(store);

        let reopened = Store::open_current_writable(&database).unwrap();
        let committed_json: String = reopened.lock().unwrap().query_row(
            "SELECT result_json FROM operation_receipts WHERE operation_id=?1 AND operation_kind='role_report'",
            params![report.operation_id], |row| row.get(0),
        ).unwrap();
        let receipt = reopened.save_role_result(&context, &report).unwrap();
        assert_eq!(
            receipt,
            serde_json::from_str::<serde_json::Value>(&committed_json).unwrap()
        );
        assert_eq!(receipt["accepted"], true);
        let changed = RoleResultReport {
            summary: "changed payload".into(),
            ..report.clone()
        };
        assert!(reopened
            .save_role_result(&context, &changed)
            .unwrap_err()
            .to_string()
            .contains("operation ID was already used with different input"));
        let connection = reopened.lock().unwrap();
        for (table, expected) in [
            ("role_results", 1),
            ("operation_receipts", 1),
            ("audit_events", 1),
        ] {
            let count: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE operation_id=?1"),
                    params![report.operation_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, expected, "{table}");
        }
        drop(connection);
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn wakes_registered_waiter(store: &Store, write: impl FnOnce()) -> bool {
        use std::future::Future;
        let changed = store.state_changes().notified();
        let mut changed = std::pin::pin!(changed);
        changed.as_mut().enable();
        write();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        changed.as_mut().poll(&mut context).is_ready()
    }

    #[test]
    fn store_guard_wakes_waiters_only_after_a_committed_revision_advance() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-state-wake-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("state.sqlite3");
        let store = Store::open(&database).unwrap();
        let writer = store.clone();
        let insert_project = |id: &str| {
            format!(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                 VALUES('{id}','{id}','/tmp/{id}','{id}','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
            )
        };
        let before = store.state_revision().unwrap();

        assert!(!wakes_registered_waiter(&store, || {
            writer
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM projects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
        }));
        assert!(!wakes_registered_waiter(&store, || {
            writer.lock().unwrap().execute(
                "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
                 VALUES('unprojected','actor','kind','hash','{}','2026-01-01T00:00:00Z')",
                [],
            ).unwrap();
        }));
        assert!(!wakes_registered_waiter(&store, || {
            let mut connection = writer.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            transaction
                .execute_batch(&insert_project("rolled-back"))
                .unwrap();
        }));
        assert!(!wakes_registered_waiter(&store, || {
            writer
                .lock()
                .unwrap()
                .execute_batch(&format!("BEGIN;{}", insert_project("open-transaction")))
                .unwrap();
        }));
        writer.lock().unwrap().execute_batch("ROLLBACK").unwrap();
        assert_eq!(store.state_revision().unwrap(), before);

        assert!(wakes_registered_waiter(&store, || {
            writer
                .lock()
                .unwrap()
                .execute_batch(&insert_project("committed"))
                .unwrap();
        }));
        // Mirrors the state wait: register, read the cursor, then a commit lands.
        assert!(wakes_registered_waiter(&store, || {
            assert_eq!(writer.state_revision().unwrap(), before + 1);
            writer
                .lock()
                .unwrap()
                .execute_batch(&insert_project("after-read"))
                .unwrap();
        }));

        let readonly = Store::open_current_readonly(&database).unwrap();
        assert!(!wakes_registered_waiter(&readonly, || {
            readonly
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM projects", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap();
        }));
        assert_eq!(readonly.state_revision().unwrap(), before + 2);

        writer
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE state_revision")
            .unwrap();
        assert!(!wakes_registered_waiter(&store, || {
            writer
                .lock()
                .unwrap()
                .execute(
                    "DELETE FROM operation_receipts WHERE operation_id='unprojected'",
                    [],
                )
                .unwrap();
        }));
        assert_eq!(
            writer
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM operation_receipts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "an unobservable revision must not fail or panic the committed write"
        );
        drop((store, writer, readonly));
        std::fs::remove_dir_all(root).unwrap();
    }
}
