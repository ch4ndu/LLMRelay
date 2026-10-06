use crate::auth;
use crate::domain::{
    AttachmentBinding, CapabilityIdentity, CapabilityProofInput, CmuxAttachmentControlDirective,
    CmuxAttachmentControlDisposition, CmuxAttachmentMode, CmuxAttachmentRoute,
    CmuxKeyboardControlAction, CmuxKeyboardControlOutcome, CmuxSessionSurface, CmuxTaskWorkspace,
    CmuxViewOutcome, HookEnvelope, LaunchConfig, NativeTurnFailureKind, ObservedProcessIdentity,
    RestartCandidateResult, RoleContext, RoleKind, RolePeerProvenance, RoleResultReport,
    ValidationLaunchRequest,
};
use crate::providers::codex::CapacityFileIdentity;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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

pub(crate) fn failure_stop_target_quiescent(
    connection: &Connection,
    control_id: &str,
) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM controls c JOIN sessions s
           ON s.id=json_extract(c.payload_json,'$.session_id')
         JOIN role_generations g ON g.id=s.role_generation_id
         WHERE c.id=?1 AND c.kind='pause_after_role'
           AND json_type(c.payload_json,'$.failure_stop_operation_id')='text'
           AND s.role_generation_id=json_extract(c.payload_json,'$.role_generation_id')
           AND g.attempt_id=c.attempt_id
           AND s.transcript_epoch=json_extract(c.payload_json,'$.transcript_epoch')
           AND json_extract(s.process_identity_json,'$.pid')=json_extract(c.payload_json,'$.process_identity.pid')
           AND json_extract(s.process_identity_json,'$.process_group_id')=json_extract(c.payload_json,'$.process_identity.process_group_id')
           AND json_extract(s.process_identity_json,'$.native_start_marker')=json_extract(c.payload_json,'$.process_identity.native_start_marker')
           AND json_extract(s.process_identity_json,'$.observed_started_at')=json_extract(c.payload_json,'$.process_identity.observed_started_at')
           AND s.status='exited' AND json_extract(s.exit_json,'$.process_group_quiescent')=1)",
        params![control_id], |row| row.get(0),
    )?)
}

// Finished controls remain authoritative until an applied human continuation releases them.
pub(crate) fn failure_stop_fence(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<bool>> {
    let mut statement = connection.prepare(
        "SELECT id,state FROM controls WHERE attempt_id=?1 AND kind='pause_after_role'
           AND json_type(payload_json,'$.failure_stop_operation_id')='text'
           AND json_type(payload_json,'$.failure_stop_released_by') IS NULL",
    )?;
    let controls = statement
        .query_map(params![attempt_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if controls.is_empty() {
        return Ok(None);
    }
    for (id, state) in controls {
        if state != "finished" || !failure_stop_target_quiescent(connection, &id)? {
            return Ok(Some(false));
        }
    }
    Ok(Some(true))
}

pub(crate) fn require_failure_stop_released(
    connection: &Connection,
    attempt_id: &str,
) -> Result<()> {
    if failure_stop_fence(connection, attempt_id)?.is_some() {
        bail!("The failed-session stop keeps this task paused. Wait for its verified exit and finished pause, then choose Continue or Run next.")
    }
    Ok(())
}

pub(crate) fn require_session_failure_stop_released(
    connection: &Connection,
    session_id: &str,
) -> Result<()> {
    let attempt: String = connection.query_row(
        "SELECT g.attempt_id FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id WHERE s.id=?1",
        params![session_id], |row| row.get(0),
    )?;
    require_failure_stop_released(connection, &attempt)
}

pub(crate) fn require_failure_stop_continuation_ready(
    connection: &Connection,
    attempt_id: &str,
) -> Result<()> {
    if failure_stop_fence(connection, attempt_id)? == Some(false) {
        bail!("The failed-session pause is not finished or its captured process has not been verified stopped.")
    }
    Ok(())
}

// Raised only before issuing a launch permit or creating a native session.
#[derive(Debug)]
pub(crate) struct RoleLaunchCapacityError;

impl std::fmt::Display for RoleLaunchCapacityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("role capacity is full; retry when an active session exits")
    }
}

impl std::error::Error for RoleLaunchCapacityError {}

/// An expected wait on the held role and lane, never a failed step.
#[derive(Clone, Debug)]
pub(crate) struct ProviderFailureHeld {
    pub(crate) hold_id: String,
    pub(crate) session_id: String,
    pub(crate) role: RoleKind,
    pub(crate) kind: NativeTurnFailureKind,
    pub(crate) expires_at: Option<String>,
}

impl ProviderFailureHeld {
    pub(crate) fn in_error(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref::<Self>())
    }
}

impl std::fmt::Display for ProviderFailureHeld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} is on hold after a provider failure; release provider failure hold {} before starting, resuming or sending input to this role",
            self.role.label(),
            self.hold_id
        )
    }
}

impl std::error::Error for ProviderFailureHeld {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderFailureHoldRelease {
    Released,
    AlreadyReleased,
    Expired,
    Superseded,
}

impl ProviderFailureHoldRelease {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::AlreadyReleased => "already_released",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }
}

/// A provider failure hold refuses automated guidance input; a person keeps keyboard control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputLeasePurpose {
    HumanControl,
    AutomatedGuidance,
}

const REPORT_REMINDER_ACTOR: &str = "service:role_report_reminders";
const REPORT_REMINDER_OPERATION: &str = "role_report_reminder";
const REPORT_REMINDER_EVENT: &str = "role.report_reminder.reserved";
const REPORT_REMINDER_LIMIT: usize = 2;
const REPORT_REMINDER_PROMPT: &str = "Inspect your current role context. If your assigned work is finished or blocked, submit the required allowed structured role report with the actual outcome and evidence. Do not repeat prior commands, invent success, expand permissions, or treat this reminder as approval.";

#[derive(Debug)]
pub(crate) struct ReportReminderRefused(pub(crate) String);

impl std::fmt::Display for ReportReminderRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReportReminderRefused {}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct ReportReminderReservation {
    binding: crate::workflow::ReportReminderBinding,
    guidance_id: String,
    ordinal: usize,
}

fn report_reminder_operation_id(generation: &str, ordinal: usize) -> String {
    format!("role-report-reminder:{generation}:{ordinal}")
}

fn report_reminder_reservations(
    connection: &Connection,
    generation: &str,
) -> Result<Vec<ReportReminderReservation>> {
    let mut statement = connection.prepare(
        "SELECT operation_id,request_hash,result_json FROM operation_receipts
         WHERE actor_key=?1 AND operation_kind=?2
           AND (operation_id=?3 OR operation_id=?4
             OR json_extract(result_json,'$.binding.role_generation_id')=?5)",
    )?;
    let rows = statement
        .query_map(
            params![
                REPORT_REMINDER_ACTOR,
                REPORT_REMINDER_OPERATION,
                report_reminder_operation_id(generation, 1),
                report_reminder_operation_id(generation, 2),
                generation,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut reservations = Vec::new();
    for (operation, hash, result) in rows {
        let reservation: ReportReminderReservation =
            serde_json::from_str(&result).context("parse canonical report reminder reservation")?;
        if reservation.binding.role_generation_id != generation
            || !(1..=REPORT_REMINDER_LIMIT).contains(&reservation.ordinal)
            || operation != report_reminder_operation_id(generation, reservation.ordinal)
            || json_hash(&reservation)? != hash
        {
            bail!("canonical report reminder provenance is invalid")
        }
        let audit: Option<String> = connection
            .query_row(
                "SELECT detail_json FROM audit_events WHERE id=?1 AND operation_id=?1
               AND actor_kind='service' AND event_code=?2 AND entity_kind='role_generation'
               AND entity_id=?3",
                params![operation, REPORT_REMINDER_EVENT, generation],
                |row| row.get(0),
            )
            .optional()?;
        if audit.as_deref() != Some(result.as_str()) {
            bail!("canonical report reminder audit is missing or changed")
        }
        reservations.push(reservation);
    }
    reservations.sort_by_key(|reservation| reservation.ordinal);
    if reservations.len() > REPORT_REMINDER_LIMIT
        || reservations
            .iter()
            .enumerate()
            .any(|(index, reservation)| reservation.ordinal != index + 1)
    {
        bail!("canonical report reminder ordinals are invalid")
    }
    let audits: i64 = connection.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE actor_kind='service' AND event_code=?1
           AND entity_kind='role_generation' AND entity_id=?2",
        params![REPORT_REMINDER_EVENT, generation],
        |row| row.get(0),
    )?;
    if audits != reservations.len() as i64 {
        bail!("canonical report reminder receipt is missing")
    }
    Ok(reservations)
}

fn report_reminder_for_guidance(
    connection: &Connection,
    guidance_id: &str,
) -> Result<Option<ReportReminderReservation>> {
    let generation: String = connection.query_row(
        "SELECT COALESCE(
           (SELECT entity_id FROM audit_events WHERE actor_kind='service'
             AND event_code=?2 AND entity_kind='role_generation'
             AND json_extract(detail_json,'$.guidance_id')=?1 LIMIT 1),
           (SELECT json_extract(result_json,'$.binding.role_generation_id') FROM operation_receipts
             WHERE actor_key=?3 AND operation_kind=?4
               AND json_extract(result_json,'$.guidance_id')=?1 LIMIT 1),
           role_generation_id) FROM guidance_messages WHERE id=?1",
        params![
            guidance_id,
            REPORT_REMINDER_EVENT,
            REPORT_REMINDER_ACTOR,
            REPORT_REMINDER_OPERATION
        ],
        |row| row.get(0),
    )?;
    let reservations = report_reminder_reservations(connection, &generation)?;
    Ok(reservations
        .into_iter()
        .find(|reservation| reservation.guidance_id == guidance_id))
}

pub(crate) fn is_report_reminder(connection: &Connection, guidance_id: &str) -> Result<bool> {
    Ok(report_reminder_for_guidance(connection, guidance_id)?.is_some())
}

pub(crate) fn report_reminder_display(
    connection: &Connection,
    guidance_id: &str,
) -> Result<Option<serde_json::Value>> {
    let Some(reservation) = report_reminder_for_guidance(connection, guidance_id)? else {
        return Ok(None);
    };
    let reservations =
        report_reminder_reservations(connection, &reservation.binding.role_generation_id)?;
    Ok(Some(serde_json::json!({
        "ordinal": reservation.ordinal,
        "reservations_spent": reservations.len(),
        "generation_limit": REPORT_REMINDER_LIMIT,
        "required_report": reservation.binding.requirement,
    })))
}

pub(crate) fn require_current_report_reminder(
    connection: &Connection,
    guidance_id: &str,
) -> Result<bool> {
    let Some(reservation) = report_reminder_for_guidance(connection, guidance_id)? else {
        return Ok(false);
    };
    let current = crate::workflow::report_reminder_binding(
        connection,
        &reservation.binding.session_id,
        Some(guidance_id),
    )?;
    let intact: bool = connection.query_row(
        "SELECT body=?2 AND attempt_id=?3 AND role_generation_id=?4
           AND state IN ('queued','delivery_reserved','written_awaiting_submit')
         FROM guidance_messages WHERE id=?1",
        params![
            guidance_id,
            REPORT_REMINDER_PROMPT,
            reservation.binding.attempt_id,
            reservation.binding.role_generation_id
        ],
        |row| row.get(0),
    )?;
    let input_owned: bool = connection.query_row(
        "SELECT state='queued' OR EXISTS(SELECT 1 FROM input_leases lease
           JOIN sessions s ON s.id=lease.session_id
           WHERE lease.session_id=guidance_messages.delivery_session_id
             AND lease.owner_id='guidance:'||guidance_messages.id
             AND lease.role_generation_id=guidance_messages.role_generation_id
             AND lease.process_identity_json=s.process_identity_json AND lease.revoked_at IS NULL
             AND julianday(lease.expires_at)>julianday('now'))
         FROM guidance_messages WHERE id=?1",
        params![guidance_id],
        |row| row.get(0),
    )?;
    if !intact
        || !input_owned
        || current.as_ref() != Some(&reservation.binding)
        || reservation.binding.role == RoleKind::FinalReviewer
    {
        return Err(ReportReminderRefused(
            "The reminder was cancelled because its settings, report, turn or input authority changed.".into(),
        ).into());
    }
    Ok(true)
}

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct RestartAdmissionBinding<'a> {
    pub admission_id: &'a str,
    pub attempt_id: &'a str,
    pub task_id: &'a str,
    pub role_generation_id: &'a str,
    pub expected_task_version: i64,
    pub prior_transcript_epoch: &'a str,
    pub expected_resume_ordinal: u32,
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
const MIGRATION_031: &str = include_str!("../migrations/031_final_repair_recheck.sql");
const MIGRATION_032: &str = include_str!("../migrations/032_normal_final_repair.sql");
const MIGRATION_033: &str = include_str!("../migrations/033_native_resolution.sql");
const MIGRATION_034: &str = include_str!("../migrations/034_guidance_submitted_text.sql");
const MIGRATION_035: &str = include_str!("../migrations/035_provider_failure_holds.sql");
const MIGRATION_036: &str = include_str!("../migrations/036_attention_observations.sql");
const MIGRATION_037: &str = include_str!("../migrations/037_trip_workflow_migrations.sql");
const MIGRATION_038: &str = include_str!("../migrations/038_codex_observed_usage.sql");
// Same value rusqlite installs at open; set explicitly before any pragma or DDL can contend.
pub(crate) const STATE_DATABASE_BUSY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);

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
    codex_usage_cache: Arc<Mutex<BTreeMap<String, (CodexUsageCandidate, CapacityFileIdentity)>>>,
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
            codex_usage_cache: Arc::new(Mutex::new(BTreeMap::new())),
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
        connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        migrate(&mut connection)?;
        Ok(Self::from_connection(connection))
    }

    /// The caller holds the instance lock and has passed the database startup gate,
    /// which publishes a verified restore point before a prior-schema writable open.
    pub(crate) fn open_service(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Self::open(path);
        }
        upgrade_supported_service_schema(path)?;
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
        connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
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
        connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        require_current_schema(&connection)?;
        Ok(Self::from_connection(connection))
    }

    pub(crate) fn open_maintenance_readonly(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open maintenance database read-only {}", path.display()))?;
        connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "query_only", "ON")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        require_maintenance_schema(&connection)?;
        Ok(Self::from_connection(connection))
    }

    pub(crate) fn open_maintenance_writable(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open maintenance database {}", path.display()))?;
        connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        require_maintenance_schema(&connection)?;
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
                | "Notification"
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
        let hook_event_id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO hook_events(id, session_id, role_generation_id, provider, event_name, native_session_id, payload_json,
                    peer_pid, peer_process_group_id, peer_start_marker, provenance_state, received_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![hook_event_id, context.session_id, context.role_generation_id, envelope.provider.to_string(), event_name, native_session_id,
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
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM permission_requests pr WHERE pr.session_id=?1 AND {})",
                        crate::permissions::ACTIONABLE_REQUEST_SQL
                    ),
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
        let natively_resolved_permission = match current_invocation_start_rowid {
            Some(start) => crate::permissions::record_native_resolution(
                &transaction,
                context,
                &hook_event_id,
                hook_rowid,
                &envelope.payload,
                start,
                &now,
            )?,
            None => None,
        };
        let provider_failure_hold =
            if event_name == "StopFailure" && current_invocation_start_rowid.is_some() {
                record_provider_failure_hold(
                    &transaction,
                    context,
                    &hook_event_id,
                    &envelope.payload,
                    &now,
                )?
            } else {
                None
            };
        let current_invocation_submit =
            event_name == "UserPromptSubmit" && current_invocation_start_rowid.is_some();
        if current_invocation_submit {
            transaction.execute(
                "UPDATE provider_failure_holds SET state='superseded',resolved_at=?1,
                        resolution_kind='accepted_turn',resolution_ref=?2
                 WHERE session_id=?3 AND state='active'",
                params![now, hook_event_id, context.session_id],
            )?;
            let submitted_text = envelope
                .payload
                .get("prompt")
                .or_else(|| envelope.payload.get("user_prompt"))
                .or_else(|| envelope.payload.get("input"))
                .or_else(|| envelope.payload.get("message"))
                .and_then(|value| value.as_str());
            if let Some(text) = submitted_text {
                let mut statement = transaction.prepare(&format!(
                    "SELECT id,COALESCE(submitted_text,body),submitted_digest
                     FROM guidance_messages
                     WHERE role_generation_id=?1 AND state IN ('written_awaiting_submit','delivery_unknown')
                       AND {CURRENT_GUIDANCE_DELIVERY}
                     ORDER BY rowid DESC LIMIT 101"
                ))?;
                let pending = statement
                    .query_map(
                        params![context.role_generation_id, context.session_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                let matching = pending
                    .iter()
                    .filter(|(_, submitted, digest)| {
                        submitted_form_intact(submitted, digest.as_deref())
                            && submitted.trim() == text.trim()
                    })
                    .collect::<Vec<_>>();
                // Bound comparison work and fail closed when more than one current
                // delivery has the same complete edge-normalized submitted form.
                if pending.len() <= 100 && matching.len() == 1 {
                    transaction.execute(
                        &format!(
                            "UPDATE guidance_messages
                             SET state='submitted',reason='matched_native_user_prompt_submit',submitted_at=?3
                             WHERE id=?4 AND role_generation_id=?1
                               AND state IN ('written_awaiting_submit','delivery_unknown')
                               AND submitted_digest IS ?5 AND {CURRENT_GUIDANCE_DELIVERY}"
                        ),
                        params![
                            context.role_generation_id,
                            context.session_id,
                            now,
                            matching[0].0.as_str(),
                            matching[0].2.as_deref()
                        ],
                    )?;
                }
            }
        }
        let resume_reconciliation = if current_invocation_submit {
            reconcile_accepted_resume_turn(&transaction, context, &hook_event_id, &now)?
        } else {
            None
        };
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
            params![uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), context.role_generation_id, context.session_id, serde_json::json!({"event_name": event_name,"hook_event_id":hook_event_id,"safe_idle_boundary":safe_idle_boundary}).to_string(), now],
        )?;
        transaction.commit()?;
        Ok(serde_json::json!({
            "recorded": true, "session_id": context.session_id, "event_name": event_name,
            "hook_event_id": hook_event_id,
            "natively_resolved_permission_request_id": natively_resolved_permission,
            "provider_failure_hold_id": provider_failure_hold,
            "resume_reconciliation": resume_reconciliation,
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
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let submitted: Option<(String, Option<String>)> = transaction
            .query_row(
                &format!(
                    "SELECT COALESCE(submitted_text,body),submitted_digest FROM guidance_messages
                     WHERE id=?3 AND role_generation_id=?1 AND state='submitted'
                       AND {CURRENT_GUIDANCE_DELIVERY}"
                ),
                params![context.role_generation_id, context.session_id, guidance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let acknowledged = match submitted {
            Some((text, digest)) if submitted_form_intact(&text, digest.as_deref()) => {
                transaction.execute(
                    &format!(
                        "UPDATE guidance_messages SET state='acknowledged',acknowledged_at=?4,reason=NULL
                         WHERE id=?3 AND role_generation_id=?1 AND state='submitted'
                           AND submitted_digest IS ?5 AND {CURRENT_GUIDANCE_DELIVERY}"
                    ),
                    params![
                        context.role_generation_id,
                        context.session_id,
                        guidance_id,
                        now,
                        digest
                    ],
                )? == 1
            }
            _ => false,
        };
        if !acknowledged {
            bail!("guidance is unknown, lacks a matching native submit for this session invocation, or belongs to another role generation")
        }
        transaction.commit()?;
        Ok(serde_json::json!({"guidance_id":guidance_id,"state":"acknowledged"}))
    }

    /// Reserves queued guidance for this exact idle invocation and records its submitted form once.
    pub fn reserve_guidance_delivery(
        &self,
        guidance_id: &str,
        session_id: &str,
        transcript_epoch: &str,
        resume_invocation_id: Option<&str>,
        submitted_text: &str,
    ) -> Result<bool> {
        let submitted_digest = json_hash(&submitted_text)?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_current_report_reminder(&transaction, guidance_id)?;
        require_no_session_provider_failure_hold(&transaction, session_id)?;
        require_session_failure_stop_released(&transaction, session_id)?;
        let reserved = transaction.execute(
            "UPDATE guidance_messages SET state='delivery_reserved',reason='automatic_post_hook_idle_verified',
               delivery_session_id=?2,delivery_transcript_epoch=?3,delivery_resume_invocation_id=?4,
               submitted_text=?5,submitted_digest=?6
             WHERE id=?1 AND state='queued'
               AND (submitted_text IS NULL OR (submitted_text=?5 AND submitted_digest=?6))
               AND EXISTS(SELECT 1 FROM sessions s
                 WHERE s.id=?2 AND s.role_generation_id=guidance_messages.role_generation_id
                   AND s.status='running' AND s.readiness_state='idle_candidate'
                   AND s.transcript_epoch=?3
                   AND (SELECT ri.id FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1) IS ?4)
               AND NOT(COALESCE(reason,'') IN ('engine_plan_rejection_notice','engine_plan_rejection_notice_blocked_hold')
                 AND EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
                   WHERE a.id=guidance_messages.attempt_id
                     AND t.attention IN ('paused','pause_requested','needs_recovery')))",
            params![
                guidance_id,
                session_id,
                transcript_epoch,
                resume_invocation_id,
                submitted_text,
                submitted_digest
            ],
        )? == 1;
        if reserved {
            transaction.execute(
                "UPDATE sessions SET readiness_state='idle_verified',updated_at=?1 WHERE id=?2",
                params![now, session_id],
            )?;
        }
        transaction.commit()?;
        Ok(reserved)
    }

    pub(crate) fn reserve_report_reminder(
        &self,
        binding: &crate::workflow::ReportReminderBinding,
    ) -> Result<Option<serde_json::Value>> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if binding.role == RoleKind::FinalReviewer
            || crate::workflow::report_reminder_binding(&transaction, &binding.session_id, None)?
                .as_ref()
                != Some(binding)
        {
            return Ok(None);
        }
        let reservations = report_reminder_reservations(&transaction, &binding.role_generation_id)?;
        if reservations.len() >= REPORT_REMINDER_LIMIT {
            return Ok(None);
        }
        for prior in &reservations {
            // Canonical receipts own the spent turn and ordinal.
            let later_turn: bool = transaction
                .query_row(
                    "SELECT current.rowid>prior.rowid AND prior_stop.rowid>prior.rowid
                   AND prior_stop.session_id=?4 AND prior_stop.event_name='Stop'
                 FROM hook_events current JOIN hook_events prior ON prior.id=?2
                 JOIN hook_events prior_stop ON prior_stop.id=?3 WHERE current.id=?1",
                    params![
                        binding.accepted_hook_event_id,
                        prior.binding.accepted_hook_event_id,
                        prior.binding.stop_hook_event_id,
                        prior.binding.session_id
                    ],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if !later_turn {
                return Ok(None);
            }
        }
        let reservation = ReportReminderReservation {
            binding: binding.clone(),
            guidance_id: uuid::Uuid::new_v4().to_string(),
            ordinal: reservations.len() + 1,
        };
        let operation =
            report_reminder_operation_id(&binding.role_generation_id, reservation.ordinal);
        let now = Utc::now().to_rfc3339();
        let provenance = serde_json::to_string(&reservation)?;
        transaction.execute(
            "INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,reason,created_at)
             VALUES(?1,?2,?3,?4,'queued','engine_role_report_reminder',?5)",
            params![reservation.guidance_id,binding.attempt_id,binding.role_generation_id,REPORT_REMINDER_PROMPT,now],
        )?;
        transaction.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![operation,REPORT_REMINDER_ACTOR,REPORT_REMINDER_OPERATION,json_hash(&reservation)?,provenance,now],
        )?;
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?1,'service',?2,'role_generation',?3,?4,?5)",
            params![operation,REPORT_REMINDER_EVENT,binding.role_generation_id,provenance,now],
        )?;
        transaction.execute(
            "UPDATE attempts SET last_coordinator_at=?1 WHERE id=?2",
            params![now, binding.attempt_id],
        )?;
        transaction.commit()?;
        Ok(Some(
            serde_json::json!({"action":"report_reminder_reserved","attempt_id":binding.attempt_id,
            "session_id":binding.session_id,"guidance_id":reservation.guidance_id,
            "ordinal":reservation.ordinal,"generation_limit":REPORT_REMINDER_LIMIT,"state":"queued"}),
        ))
    }

    pub(crate) fn cancel_stale_report_reminder(
        &self,
        guidance_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let queued: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM guidance_messages WHERE id=?1 AND state='queued')",
            params![guidance_id],
            |row| row.get(0),
        )?;
        if !queued {
            return Ok(None);
        }
        let refusal = match require_current_report_reminder(&transaction, guidance_id) {
            Ok(_) => return Ok(None),
            Err(error) if error.is::<ReportReminderRefused>() => error.to_string(),
            Err(error) => return Err(error),
        };
        let cancelled =
            Self::cancel_report_reminder_in(&transaction, guidance_id, &refusal, false)?;
        transaction.commit()?;
        Ok(cancelled)
    }

    pub(crate) fn cancel_report_reminder_before_write(
        &self,
        guidance_id: &str,
        reason: &str,
    ) -> Result<serde_json::Value> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cancelled = Self::cancel_report_reminder_in(&transaction, guidance_id, reason, true)?
            .ok_or_else(|| {
            anyhow!("reminder cancellation no longer owns an unwritten reservation")
        })?;
        transaction.commit()?;
        Ok(cancelled)
    }

    fn cancel_report_reminder_in(
        transaction: &Transaction<'_>,
        guidance_id: &str,
        reason: &str,
        before_first_write: bool,
    ) -> Result<Option<serde_json::Value>> {
        let reservation = report_reminder_for_guidance(transaction, guidance_id)?
            .ok_or_else(|| anyhow!("guidance has no canonical reminder reservation"))?;
        let binding = &reservation.binding;
        let unchanged_turn =
            crate::workflow::report_reminder_turn(transaction, &binding.session_id)?
                == Some((
                    binding.accepted_hook_event_id.clone(),
                    binding.stop_hook_event_id.clone(),
                ));
        // Only the delivery owner can prove that a reserved paste has not started.
        let restored = if before_first_write && unchanged_turn {
            transaction.execute(
                "UPDATE sessions SET readiness_state='idle_candidate',updated_at=?1
                 WHERE id=?2 AND role_generation_id=?3 AND transcript_epoch=?4
                   AND status='running' AND readiness_state='idle_verified'
                   AND process_identity_json=?7
                   AND EXISTS(SELECT 1 FROM role_generations generation JOIN role_settings settings
                     ON settings.effective_generation_id=generation.id AND settings.role=generation.role
                     WHERE generation.id=?3 AND generation.status='running'
                       AND generation.role=?8 AND settings.revision=generation.config_revision)
                   AND (SELECT ri.id FROM resume_invocations ri WHERE ri.session_id=sessions.id
                     AND ri.transcript_epoch=sessions.transcript_epoch
                     ORDER BY ri.resume_ordinal DESC LIMIT 1) IS ?5
                   AND EXISTS(SELECT 1 FROM guidance_messages g WHERE g.id=?6
                     AND g.state='delivery_reserved' AND g.written_at IS NULL
                     AND g.role_generation_id=?3 AND g.delivery_session_id=?2
                     AND g.delivery_transcript_epoch=?4 AND g.delivery_resume_invocation_id IS ?5)",
                params![Utc::now().to_rfc3339(),binding.session_id,binding.role_generation_id,
                    binding.transcript_epoch,binding.resume_invocation_id,guidance_id,
                    binding.authority["process_identity"].as_str(),binding.role.to_string()],
            )? == 1
        } else {
            false
        };
        let changed = transaction.execute(
            "UPDATE guidance_messages SET state='cancelled',reason=?1
             WHERE id=?2 AND (state='queued' OR (?3 AND state='delivery_reserved')) AND written_at IS NULL",
            params![reason, guidance_id, before_first_write],
        )?;
        if changed != 1 {
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?1,'service','role.report_reminder.cancelled','guidance',?2,?3,?4)",
            params![uuid::Uuid::new_v4().to_string(),guidance_id,
                serde_json::json!({"reason":reason,"ordinal":reservation.ordinal,"readiness_restored":restored}).to_string(),
                Utc::now().to_rfc3339()],
        )?;
        transaction.execute(
            "UPDATE attempts SET last_coordinator_at=?1 WHERE id=?2",
            params![Utc::now().to_rfc3339(), binding.attempt_id],
        )?;
        Ok(Some(
            serde_json::json!({"action":"report_reminder_cancelled","guidance_id":guidance_id,
            "attempt_id":binding.attempt_id,"session_id":binding.session_id,"state":"cancelled",
            "reason":reason,"ordinal":reservation.ordinal,"readiness_restored":restored,"engine_generated":true}),
        ))
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
                    if matches!(kind, "pause_now" | "cancel")
                        || failure_stop_fence(transaction, &attempt_id)?.is_some()
                    {
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
        let prior_stop: Option<(String, Option<String>)> = transaction
            .query_row(
                "SELECT status,interrupt_requested_at FROM sessions
                 WHERE id=?1 AND transcript_epoch=?2 AND process_identity_json=?3
                   AND status IN ('running','interrupt_requested','recovery_required')",
                params![session_id, transcript_epoch, process_json],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((prior_status, interrupt_requested_at)) = prior_stop else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut exit = serde_json::from_str::<serde_json::Value>(exit_json)?;
        // Preserve general stop intent before clearing the interrupt marker.
        exit.as_object_mut()
            .ok_or_else(|| anyhow!("session exit record must be a JSON object"))?
            .insert(
                "app_stop_requested".into(),
                (prior_status == "interrupt_requested" || interrupt_requested_at.is_some()).into(),
            );
        let recorded_exit = exit.to_string();
        let changed = transaction.execute(
            "UPDATE sessions SET status='exited',launch_state='finished',exit_json=?1,
                    interrupt_requested_at=NULL,updated_at=?2
             WHERE id=?3 AND transcript_epoch=?4 AND process_identity_json=?5
               AND status IN ('running','interrupt_requested','recovery_required')",
            params![
                recorded_exit,
                now,
                session_id,
                transcript_epoch,
                process_json
            ],
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
            &recorded_exit,
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
        // Each exploration is a fresh read-only session, so it starts on the
        // explorer's newest activated profile; reviewers do the same when
        // their request is reserved.
        if role == RoleKind::Explorer && !validation_dispatch && setup_runtime_probe.is_none() {
            let now = Utc::now().to_rfc3339();
            if let crate::trip::ReadOnlyProfileBoundary::Pending { message, .. } =
                crate::trip::materialize_read_only_profile(&transaction, attempt_id, role, &now)?
            {
                bail!("explorer profile change pending: {message}")
            }
        }
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
        require_no_provider_failure_hold(&transaction, attempt_id, role, &lane_id)?;
        require_failure_stop_released(&transaction, attempt_id)?;
        if !role_capacity_available(&transaction, &provider, role, None, None)? {
            return Err(RoleLaunchCapacityError.into());
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
        require_no_provider_failure_hold(&transaction, &attempt_id, role, &old_lane)?;
        require_failure_stop_released(&transaction, &attempt_id)?;
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
        require_failure_stop_released(&transaction, &attempt_id)?;
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
        require_no_provider_failure_hold(&transaction, &attempt_id, config.role, &context.lane_id)?;
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
        self.verify_attachment_binding_for_guidance(binding, None)
    }

    pub(crate) fn verify_attachment_binding_for_guidance(
        &self,
        binding: &AttachmentBinding,
        guidance_id: Option<&str>,
    ) -> Result<()> {
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
            if let Some(guidance) = guidance_id {
                if is_report_reminder(&connection, guidance)? {
                    return Err(ReportReminderRefused(
                        "terminal attachment is stale, revoked, or no longer running".into(),
                    )
                    .into());
                }
            }
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
        Self::mark_interrupt_requested_in(&transaction, session_id, &now)?;
        transaction.commit()?;
        Ok(())
    }

    fn mark_interrupt_requested_in(
        transaction: &Transaction<'_>,
        session_id: &str,
        now: &str,
    ) -> Result<()> {
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
            transaction,
            session_id,
            "session interrupt invalidated permission response delivery",
        )?;
        Ok(())
    }

    pub(crate) fn reserve_failed_session_stop(
        &self,
        command: &crate::domain::HumanCommand,
    ) -> Result<(crate::domain::OperationResult, bool)> {
        let crate::domain::HumanCommand::StopFailedSession {
            operation_id,
            binding,
        } = command
        else {
            bail!("expected a failed-session stop command")
        };
        if operation_id.trim().is_empty() {
            bail!("operation_id must not be empty")
        }
        let request_hash = json_hash(command)?;
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((hash, result)) = tx.query_row(
            "SELECT request_hash,result_json FROM operation_receipts
             WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='human_command'",
            params![operation_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()? {
            if hash != request_hash {
                bail!("operation_id was already used for another request")
            }
            return Ok((serde_json::from_str(&result)?, false));
        }
        let current = crate::workflow::failed_session_stop_binding(&tx, &binding.session_id)?;
        if current.as_ref() != Some(binding) {
            bail!("The failed session or task changed. Refresh before choosing how to continue.")
        }
        let control_id = uuid::Uuid::new_v4().to_string();
        let mut payload = serde_json::to_value(binding)?;
        payload["failure_stop_operation_id"] = serde_json::json!(operation_id);
        tx.execute(
            "INSERT INTO controls(id,attempt_id,role_generation_id,kind,state,expected_version,
               payload_json,created_at,updated_at,requested_operation_id)
             VALUES(?1,?2,?3,'pause_after_role','requested',?4,?5,?6,?6,?7)",
            params![
                control_id,
                binding.attempt_id,
                binding.role_generation_id,
                binding.expected_task_version,
                payload.to_string(),
                now,
                operation_id
            ],
        )?;
        tx.execute(
            "UPDATE tasks SET attention='pause_requested',version=version+1,updated_at=?1
             WHERE id=?2 AND version=?3",
            params![now, binding.task_id, binding.expected_task_version],
        )?;
        Self::mark_interrupt_requested_in(&tx, &binding.session_id, &now)?;
        let result = crate::domain::OperationResult {
            operation_id: operation_id.clone(),
            entity_kind: "session".into(),
            entity_id: binding.session_id.clone(),
            version: Some(binding.expected_task_version + 1),
            state: "stop_recovery_required".into(),
            detail: serde_json::json!({
                "control_id":control_id,
                "message":"The task pause is reserved. Signal delivery is not confirmed; refresh to check the exit or use the task's recovery controls.",
            }),
        };
        tx.execute(
            "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
             VALUES(?1,'human_control','human_command',?2,?3,?4)",
            params![operation_id, request_hash, serde_json::to_string(&result)?, now],
        )?;
        tx.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'human','session.failure_stop.reserved','session',?3,?4,?5)",
            params![uuid::Uuid::new_v4().to_string(), operation_id, binding.session_id, payload.to_string(), now],
        )?;
        tx.commit()?;
        Ok((result, true))
    }

    pub(crate) fn finish_failed_session_stop(
        &self,
        result: &crate::domain::OperationResult,
    ) -> Result<()> {
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE operation_receipts SET result_json=?1 WHERE operation_id=?2
             AND actor_key='human_control' AND operation_kind='human_command'",
            params![serde_json::to_string(result)?, result.operation_id],
        )?;
        tx.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','session.failure_stop.signal_outcome','session',?3,?4,?5)",
            params![uuid::Uuid::new_v4().to_string(), result.operation_id, result.entity_id,
                serde_json::to_string(result)?, now],
        )?;
        tx.commit()?;
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
               AND json_extract(detail_json,'$.category')=?5
               AND (?6 IS NULL OR ?5!='native_history_unavailable'
                    OR json_extract(detail_json,'$.observed_capability_key')=?6))",
            params![
                session_id,
                generation_id,
                transcript_epoch,
                resume_count,
                category,
                observed_key
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
               AND readiness_state IN ('busy_unresolved_hook_work','busy')",
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
                    "scope":receipt.scope,
                    "attempt_id":receipt.attempt_id,
                    "task_version":receipt.task_version,
                    "late_terminal_hooks":receipt.late_terminal_hooks,
                    "accepted_result_id":receipt.accepted_result_id,
                    "phase":receipt.phase,
                    "plan_hash":receipt.plan_hash,
                    "candidate_hash":receipt.candidate_hash,
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
        {
            let connection = self.lock()?;
            require_session_failure_stop_released(&connection, session_id)?;
        }
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
        require_failure_stop_released(&transaction, &admission_attempt)?;
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
        require_no_session_provider_failure_hold(&transaction, session_id)?;
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
        match self.reserve_role_resume_inner(session_id, epoch, launch, token, None, None)? {
            BrowserLaunchReservation::Reserved => Ok(()),
            BrowserLaunchReservation::Existing(_) => {
                bail!("non-browser role resume unexpectedly found a browser receipt")
            }
        }
    }

    pub(crate) fn reserve_role_resume_for_restart_admission(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        admission: RestartAdmissionBinding<'_>,
    ) -> Result<()> {
        match self.reserve_role_resume_inner(
            session_id,
            epoch,
            launch,
            token,
            None,
            Some(admission),
        )? {
            BrowserLaunchReservation::Reserved => Ok(()),
            BrowserLaunchReservation::Existing(_) => {
                bail!("host-restart role resume unexpectedly found a browser receipt")
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
        self.reserve_role_resume_inner(session_id, epoch, launch, token, Some(receipt), None)
    }

    fn reserve_role_resume_inner(
        &self,
        session_id: &str,
        epoch: &str,
        launch: &LaunchConfig,
        token: &str,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
        restart_admission: Option<RestartAdmissionBinding<'_>>,
    ) -> Result<BrowserLaunchReservation> {
        self.require_execution_unheld("role resume reservations")?;
        if launch.role == RoleKind::FinalReviewer {
            bail!("final verifier sessions are always fresh and cannot resume")
        }
        let now = Utc::now().to_rfc3339();
        let mut connection = self.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(admission) = restart_admission {
            require_current_restart_admission(&tx, session_id, admission)?;
        }
        require_session_failure_stop_released(&tx, session_id)?;
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
        require_no_session_provider_failure_hold(&tx, session_id)?;
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
        self.acquire_input_lease_for(
            InputLeasePurpose::HumanControl,
            session_id,
            lease_secret,
            owner_id,
            process_json,
            role_generation_id,
            expires_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acquire_input_lease_for(
        &self,
        purpose: InputLeasePurpose,
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
        if purpose == InputLeasePurpose::AutomatedGuidance {
            require_session_failure_stop_released(&transaction, session_id)?;
            require_no_session_provider_failure_hold(&transaction, session_id)?;
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
        verify_input_lease_in(
            &connection,
            session_id,
            lease_secret,
            process_json,
            role_generation_id,
        )
    }

    pub(crate) fn reminder_guidance_for_lease(
        connection: &Connection,
        session_id: &str,
    ) -> Result<Option<String>> {
        let guidance: Option<String> = connection
            .query_row(
                "SELECT g.id FROM input_leases lease JOIN guidance_messages g
               ON lease.owner_id='guidance:'||g.id WHERE lease.session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        match guidance {
            Some(guidance) if report_reminder_for_guidance(connection, &guidance)?.is_some() => {
                Ok(Some(guidance))
            }
            _ => Ok(None),
        }
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

    pub(crate) fn release_report_reminder_input(
        &self,
        session_id: &str,
        lease_secret: &str,
        guidance_id: &str,
    ) -> Result<()> {
        let connection = self.lock()?;
        if !is_report_reminder(&connection, guidance_id)? {
            bail!("input cleanup requires a canonical report reminder")
        }
        // A replacement lease owns its own cleanup, even when the reminder lost authority.
        connection.execute(
            "UPDATE input_leases SET revoked_at=COALESCE(revoked_at,?1),updated_at=?1
             WHERE session_id=?2 AND lease_id_hash=?3 AND owner_id='guidance:'||?4",
            params![
                Utc::now().to_rfc3339(),
                session_id,
                auth::hash_secret(lease_secret),
                guidance_id
            ],
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

pub(crate) fn verify_input_lease_in(
    connection: &Connection,
    session_id: &str,
    lease_secret: &str,
    process_json: &str,
    role_generation_id: &str,
) -> Result<()> {
    let row: Option<(String, String, String, String, Option<String>, String)> = connection.query_row(
        "SELECT lease_id_hash, role_generation_id, process_identity_json, expires_at, revoked_at,
                    (SELECT status FROM sessions WHERE id = input_leases.session_id)
             FROM input_leases WHERE session_id = ?1",
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
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
    let guidance_delivery: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM input_leases lease JOIN guidance_messages guidance
               ON lease.owner_id='guidance:'||guidance.id
             WHERE lease.session_id=?1 AND guidance.delivery_session_id=lease.session_id
               AND guidance.role_generation_id=lease.role_generation_id)",
        params![session_id],
        |row| row.get(0),
    )?;
    if guidance_delivery {
        require_session_failure_stop_released(connection, session_id)?;
    }
    Ok(())
}

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 38;

/// Reads the durable state cursor. It is committed state only when the
/// connection is in autocommit mode.
pub(crate) fn read_state_revision(connection: &Connection) -> rusqlite::Result<i64> {
    connection
        .prepare_cached("SELECT revision FROM state_revision WHERE singleton=1")?
        .query_row([], |row| row.get(0))
}

/// The prior schemas an existing database may be migrated from at service
/// start. Every other non-current version is left unchanged and refused.
pub(crate) const SERVICE_UPGRADABLE_SCHEMA_VERSIONS: [i64; 7] = [31, 32, 33, 34, 35, 36, 37];

pub(crate) fn require_maintenance_schema(connection: &Connection) -> Result<i64> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != CURRENT_SCHEMA_VERSION && !SERVICE_UPGRADABLE_SCHEMA_VERSIONS.contains(&version) {
        bail!("unsupported database schema version {version}; maintenance supports only schemas 31–38")
    }
    Ok(version)
}

fn upgrade_supported_service_schema(path: &Path) -> Result<()> {
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open state database for upgrade {}", path.display()))?;
    connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !SERVICE_UPGRADABLE_SCHEMA_VERSIONS.contains(&version) {
        return Ok(());
    }
    connection.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&mut connection)
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
        14..=CURRENT_SCHEMA_VERSION => {}
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
    if version <= 30 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_031)?;
        transaction.pragma_update(None, "user_version", 31)?;
        transaction
            .commit()
            .context("commit final-repair recheck migration")?;
    }
    if version <= 31 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_032)?;
        let violations: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_check('final_repair_rechecks')",
            [],
            |row| row.get(0),
        )?;
        if violations != 0 {
            bail!("rebuilt final-repair recheck receipts violate {violations} foreign keys")
        }
        transaction.pragma_update(None, "user_version", 32)?;
        transaction
            .commit()
            .context("commit normal final-repair receipt migration")?;
    }
    if version <= 32 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_033)?;
        transaction.pragma_update(None, "user_version", 33)?;
        transaction
            .commit()
            .context("commit native-resolution provenance migration")?;
    }
    if version <= 33 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_034)?;
        transaction.pragma_update(None, "user_version", 34)?;
        transaction
            .commit()
            .context("commit guidance submitted-form migration")?;
    }
    if version <= 34 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_035)?;
        transaction.pragma_update(None, "user_version", 35)?;
        transaction
            .commit()
            .context("commit provider failure hold migration")?;
    }
    if version <= 35 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_036)?;
        transaction.pragma_update(None, "user_version", 36)?;
        transaction
            .commit()
            .context("commit attention observation migration")?;
    }
    if version <= 36 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_037)?;
        transaction.pragma_update(None, "user_version", 37)?;
        transaction
            .commit()
            .context("commit workflow migration receipt migration")?;
    }
    if version <= 37 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(MIGRATION_038)?;
        transaction.pragma_update(None, "user_version", 38)?;
        transaction
            .commit()
            .context("commit Codex observed usage migration")?;
    }
    Ok(())
}

/// LLMRelay's own wait after a rate limit or overload, never the provider's reset time.
const PROVIDER_FAILURE_COOLDOWN_SECONDS: i64 = 60;

/// Expiry is only observed here; nothing runs when a cooldown passes.
pub(crate) fn provider_failure_hold_active(hold: &str) -> String {
    format!(
        "({hold}.state='active' AND ({hold}.expires_at IS NULL
           OR julianday({hold}.expires_at)>julianday('now')))"
    )
}

/// Keyed by role and lane, so a newer generation cannot escape an earlier one's hold.
pub(crate) fn provider_failure_hold_restricts(attempt: &str, role: &str, lane: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM provider_failure_holds provider_hold
           JOIN role_generations provider_hold_generation
             ON provider_hold_generation.id=provider_hold.role_generation_id
           WHERE provider_hold.attempt_id={attempt} AND provider_hold_generation.role={role}
             AND provider_hold_generation.lane_id={lane} AND {})",
        provider_failure_hold_active("provider_hold")
    )
}

fn first_provider_failure_hold(
    connection: &Connection,
    subject: &str,
    params: impl rusqlite::Params,
) -> Result<Option<ProviderFailureHeld>> {
    let hold: Option<(String, String, String, String, Option<String>)> = connection
        .query_row(
            &format!(
                "SELECT provider_hold.id,provider_hold.session_id,provider_hold_generation.role,
                        provider_hold.failure_kind,provider_hold.expires_at
                 FROM provider_failure_holds provider_hold
                 JOIN role_generations provider_hold_generation
                   ON provider_hold_generation.id=provider_hold.role_generation_id
                 WHERE {subject} AND {}
                 ORDER BY provider_hold.created_at,provider_hold.rowid LIMIT 1",
                provider_failure_hold_active("provider_hold")
            ),
            params,
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
    hold.map(|(hold_id, session_id, role, kind, expires_at)| {
        Ok(ProviderFailureHeld {
            hold_id,
            session_id,
            role: role.parse().map_err(|error: String| anyhow!(error))?,
            kind: NativeTurnFailureKind::from_provider_error(&kind),
            expires_at,
        })
    })
    .transpose()
}

pub(crate) fn provider_failure_hold_on(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
    lane_id: &str,
) -> Result<Option<ProviderFailureHeld>> {
    first_provider_failure_hold(
        connection,
        "provider_hold.attempt_id=?1 AND provider_hold_generation.role=?2
         AND provider_hold_generation.lane_id=?3",
        params![attempt_id, role.to_string(), lane_id],
    )
}

/// Any role's active hold in the attempt, which a replacement attempt would escape.
pub(crate) fn provider_failure_hold_in_attempt(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<ProviderFailureHeld>> {
    first_provider_failure_hold(
        connection,
        "provider_hold.attempt_id=?1",
        params![attempt_id],
    )
}

/// Keyed by the session's role and lane, not only by holds this session recorded.
pub(crate) fn provider_failure_hold_for_session(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<ProviderFailureHeld>> {
    first_provider_failure_hold(
        connection,
        "EXISTS(SELECT 1 FROM sessions held_session JOIN role_generations held_generation
           ON held_generation.id=held_session.role_generation_id
           WHERE held_session.id=?1 AND held_generation.attempt_id=provider_hold.attempt_id
             AND held_generation.role=provider_hold_generation.role
             AND held_generation.lane_id=provider_hold_generation.lane_id)",
        params![session_id],
    )
}

fn require_no_provider_failure_hold(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
    lane_id: &str,
) -> Result<()> {
    match provider_failure_hold_on(connection, attempt_id, role, lane_id)? {
        Some(held) => Err(held.into()),
        None => Ok(()),
    }
}

fn require_no_session_provider_failure_hold(
    connection: &Connection,
    session_id: &str,
) -> Result<()> {
    match provider_failure_hold_for_session(connection, session_id)? {
        Some(held) => Err(held.into()),
        None => Ok(()),
    }
}

/// Repeat failures of one turn and kind join the standing hold without renewing its deadline.
fn record_provider_failure_hold(
    transaction: &Transaction<'_>,
    context: &RoleContext,
    hook_event_id: &str,
    payload: &serde_json::Value,
    now: &str,
) -> Result<Option<String>> {
    let attributed: Option<(Option<String>, String)> = transaction
        .query_row(
            &format!(
                "WITH {}
                 SELECT (SELECT id FROM accepted),
                        CASE WHEN NOT EXISTS(SELECT 1 FROM accepted) THEN 'startup_invocation'
                             WHEN json_type(failed.payload_json,'$.prompt_id')='text'
                              AND (SELECT json_type(payload_json,'$.prompt_id') FROM accepted)='text'
                             THEN 'prompt_id_matched' ELSE 'arrival_order' END
                 FROM current_hooks failed
                 WHERE failed.id=?2 AND failed.event_name='StopFailure'
                   AND failed.hook_rowid>COALESCE((SELECT hook_rowid FROM accepted),0)
                   AND {}",
                crate::workflow::CURRENT_TURN_HOOKS_SQL,
                crate::workflow::belongs_to_accepted_turn("failed")
            ),
            params![context.session_id, hook_event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((accepted_hook_event_id, attribution)) = attributed else {
        return Ok(None);
    };
    let kind = NativeTurnFailureKind::from_provider_error(
        payload
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
    );
    let standing: Option<String> = transaction
        .query_row(
            &format!(
                "SELECT standing.id FROM provider_failure_holds standing
                 WHERE standing.session_id=?1 AND standing.transcript_epoch=?2
                   AND standing.accepted_hook_event_id IS ?3 AND standing.failure_kind=?4
                   AND {}",
                provider_failure_hold_active("standing")
            ),
            params![
                context.session_id,
                context.transcript_epoch,
                accepted_hook_event_id,
                kind.as_str()
            ],
            |row| row.get(0),
        )
        .optional()?;
    if standing.is_some() {
        return Ok(standing);
    }
    let expires_at = kind
        .app_cooldown()
        .then(|| {
            chrono::DateTime::parse_from_rfc3339(now).map(|created| {
                (created + chrono::Duration::seconds(PROVIDER_FAILURE_COOLDOWN_SECONDS))
                    .to_rfc3339()
            })
        })
        .transpose()?;
    let hold_id = uuid::Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,
            transcript_epoch,accepted_hook_event_id,failure_hook_event_id,failure_kind,
            attribution,created_at,expires_at,state)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active')",
        params![
            hold_id,
            context.attempt_id,
            context.role_generation_id,
            context.session_id,
            context.transcript_epoch,
            accepted_hook_event_id,
            hook_event_id,
            kind.as_str(),
            attribution,
            now,
            expires_at
        ],
    )?;
    Ok(Some(hold_id))
}

/// Identity is checked before any state is revealed, then the task version.
#[allow(clippy::too_many_arguments)]
pub(crate) fn release_provider_failure_hold(
    transaction: &Transaction<'_>,
    operation_id: &str,
    task_id: &str,
    attempt_id: &str,
    session_id: &str,
    hold_id: &str,
    expected_task_version: i64,
    now: &str,
) -> Result<ProviderFailureHoldRelease> {
    let hold: Option<(String, bool, bool, i64)> = transaction
        .query_row(
            &format!(
                "SELECT hold.state,{},
                        t.archived_at IS NULL AND t.lifecycle NOT IN ('done','cancelled')
                          AND (a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id
                                     ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
                            OR EXISTS(SELECT 1 FROM rework_intents rework
                                      WHERE rework.parent_attempt_id=a.id
                                        AND rework.state NOT IN ('completed','cancelled'))),
                        t.version
                 FROM provider_failure_holds hold
                 JOIN attempts a ON a.id=hold.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE hold.id=?1 AND hold.session_id=?2 AND hold.attempt_id=?3 AND t.id=?4",
                provider_failure_hold_active("hold")
            ),
            params![hold_id, session_id, attempt_id, task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((state, restricting, current, task_version)) = hold else {
        bail!("provider failure hold binding is stale: hold {hold_id} does not belong to session {session_id}, attempt {attempt_id} and task {task_id}")
    };
    if task_version != expected_task_version {
        bail!("task version is stale")
    }
    let disposition = match state.as_str() {
        "human_released" => ProviderFailureHoldRelease::AlreadyReleased,
        "superseded" => ProviderFailureHoldRelease::Superseded,
        "active" if !current => ProviderFailureHoldRelease::Superseded,
        "active" if !restricting => ProviderFailureHoldRelease::Expired,
        "active" => ProviderFailureHoldRelease::Released,
        other => bail!("provider failure hold has unsupported state {other}"),
    };
    if disposition == ProviderFailureHoldRelease::Released {
        transaction.execute(
            "UPDATE provider_failure_holds SET state='human_released',resolved_at=?1,
                    resolution_kind='human_release',resolution_ref=?2
             WHERE id=?3 AND state='active'",
            params![now, operation_id, hold_id],
        )?;
    }
    Ok(disposition)
}

/// Only an explicit human replacement retires holds; a manager change keeps them.
pub(crate) fn retire_replaced_role_holds(
    transaction: &Transaction<'_>,
    old_generation_id: &str,
    switch_intent_id: &str,
    operation_id: &str,
    now: &str,
) -> Result<()> {
    let replaced_role_holds = "SELECT hold.id FROM provider_failure_holds hold
         JOIN role_generations source ON source.id=hold.role_generation_id
         JOIN role_generations replaced ON replaced.id=?1
         WHERE hold.state='active' AND hold.attempt_id=replaced.attempt_id
           AND source.role=replaced.role AND source.lane_id=replaced.lane_id";
    let retired = transaction
        .prepare(replaced_role_holds)?
        .query_map(params![old_generation_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if retired.is_empty() {
        return Ok(());
    }
    transaction.execute(
        &format!(
            "UPDATE provider_failure_holds SET state='superseded',resolved_at=?2,
                    resolution_kind='role_replacement',resolution_ref=?3
             WHERE id IN ({replaced_role_holds})"
        ),
        params![old_generation_id, now, switch_intent_id],
    )?;
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','provider_failure_hold.retired','switch_intent',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            operation_id,
            switch_intent_id,
            serde_json::json!({
                "hold_ids":retired,
                "old_generation_id":old_generation_id,
                "reason":"explicit role replacement",
            })
            .to_string(),
            now
        ],
    )?;
    Ok(())
}

/// App observation thresholds, never provider promises.
const QUIET_TURN_AGE_SECONDS: i64 = 600;
const QUIET_TURN_UNCHANGED_SECONDS: i64 = 300;
const NO_LIVE_SESSION_SECONDS: i64 = 60;
const CONFIRMATION_SPACING_SECONDS: i64 = 1;
const RECURRENCE_CAP: i64 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ObservationKind {
    ProcessWithoutAcceptedTurn,
    QuietTurn,
    AttemptWithoutLiveSession,
    GuidanceUnaccepted,
    PermissionOnExitedSession,
    ResumeFailureAfterAcceptance,
    BusyAfterExit,
    RecurringBlock,
}

impl ObservationKind {
    const ALL: [Self; 8] = [
        Self::ProcessWithoutAcceptedTurn,
        Self::QuietTurn,
        Self::AttemptWithoutLiveSession,
        Self::GuidanceUnaccepted,
        Self::PermissionOnExitedSession,
        Self::ResumeFailureAfterAcceptance,
        Self::BusyAfterExit,
        Self::RecurringBlock,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProcessWithoutAcceptedTurn => "process_without_accepted_turn",
            Self::QuietTurn => "quiet_turn",
            Self::AttemptWithoutLiveSession => "attempt_without_live_session",
            Self::GuidanceUnaccepted => "guidance_unaccepted",
            Self::PermissionOnExitedSession => "permission_on_exited_session",
            Self::ResumeFailureAfterAcceptance => "resume_failure_after_acceptance",
            Self::BusyAfterExit => "busy_after_exit",
            Self::RecurringBlock => "recurring_block",
        }
    }

    fn entity_kind(self) -> &'static str {
        match self {
            Self::ProcessWithoutAcceptedTurn | Self::QuietTurn | Self::BusyAfterExit => "session",
            Self::AttemptWithoutLiveSession => "attempt",
            Self::GuidanceUnaccepted => "guidance_message",
            Self::PermissionOnExitedSession => "permission_request",
            Self::ResumeFailureAfterAcceptance => "resume_rejection",
            Self::RecurringBlock => "role_lane",
        }
    }

    fn confirmations_required(self) -> i64 {
        match self {
            Self::GuidanceUnaccepted | Self::PermissionOnExitedSession => 1,
            Self::ProcessWithoutAcceptedTurn
            | Self::QuietTurn
            | Self::AttemptWithoutLiveSession
            | Self::ResumeFailureAfterAcceptance
            | Self::BusyAfterExit
            | Self::RecurringBlock => 2,
        }
    }
}

pub(crate) struct OpenObservation {
    pub(crate) id: String,
    pub(crate) kind: ObservationKind,
    pub(crate) task_id: Option<String>,
    pub(crate) attempt_id: Option<String>,
    pub(crate) role: Option<RoleKind>,
    pub(crate) session_id: Option<String>,
    pub(crate) role_generation_id: Option<String>,
    pub(crate) transcript_epoch: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) evidence: serde_json::Value,
    pub(crate) uncertain: bool,
    pub(crate) recurrence_count: i64,
    pub(crate) last_counted_result_id: Option<String>,
    pub(crate) opened_at: String,
    pub(crate) current_block: bool,
}

pub(crate) fn open_attention_observations(connection: &Connection) -> Result<Vec<OpenObservation>> {
    let mut statement = connection.prepare(
        "SELECT id,kind,task_id,attempt_id,role,session_id,role_generation_id,source_id,
                evidence_json,uncertain,recurrence_count,last_counted_result_id,opened_at,lane_id,transcript_epoch
         FROM attention_observations WHERE state='open'
           AND (kind!='recurring_block' OR recurrence_count=2) ORDER BY opened_at,id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, bool>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|row| {
            let kind = ObservationKind::ALL
                .into_iter()
                .find(|kind| kind.as_str() == row.1)
                .ok_or_else(|| anyhow!("unknown attention observation kind {}", row.1))?;
            let evidence: serde_json::Value = serde_json::from_str(&row.8)?;
            let current_block = if kind == ObservationKind::RecurringBlock {
                match (
                    row.3.as_deref(),
                    row.4.as_deref(),
                    row.13.as_deref(),
                    evidence["outcome"].as_str(),
                ) {
                    (Some(attempt), Some(role), Some(lane), Some(outcome)) => {
                        current_block(connection, attempt, role, lane, outcome)?.unwrap_or(false)
                    }
                    _ => false,
                }
            } else {
                false
            };
            Ok(OpenObservation {
                id: row.0,
                kind,
                task_id: row.2,
                attempt_id: row.3,
                role: row.4.and_then(|role| role.parse().ok()),
                session_id: row.5,
                role_generation_id: row.6,
                transcript_epoch: row.14,
                source_id: row.7,
                evidence,
                uncertain: row.9,
                recurrence_count: row.10,
                last_counted_result_id: row.11,
                opened_at: row.12,
                current_block,
            })
        })
        .collect()
}

/// Task `t`'s attempt `a` is its newest one and the task is still live.
const OBSERVED_CURRENT_ATTEMPT_SQL: &str = "COALESCE(t.archived_at IS NULL
    AND t.lifecycle IN ('in_progress','validation','awaiting_review')
    AND a.status NOT IN ('done','completed','cancelled','failed')
    AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id
              ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1),0)";

const OBSERVED_EFFECTIVE_GENERATION_SQL: &str = "COALESCE(rg.status NOT IN ('replaced','revoked')
    AND (EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=t.id AND rs.role=rg.role
                  AND rs.revision=(SELECT MAX(current.revision) FROM role_settings current
                                   WHERE current.task_id=t.id AND current.role=rg.role)
                  AND rs.effective_generation_id=rg.id)
      OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
            SELECT 1 FROM lane_generations lg WHERE lg.lane_id=rg.lane_id
              AND lg.effective_generation_id=rg.id))),0)";

const CODEX_CAPACITY_EVENT: &str = "provider.codex_capacity_observed";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize)]
pub(crate) struct CodexCapacityBinding {
    pub(crate) session_id: String,
    pub(crate) role_generation_id: String,
    pub(crate) attempt_id: String,
    pub(crate) accepted_hook_id: String,
    transcript_epoch: String,
    native_session_id: String,
    process_identity_json: String,
    cwd: String,
    launch_cwd: String,
    hook_cwd: String,
    turn_id: String,
    invocation_start: i64,
}

impl CodexCapacityBinding {
    fn audit_id(&self) -> Result<String> {
        Ok(format!(
            "codex-capacity:{}",
            hex::encode(Sha256::digest(serde_json::to_vec(self)?))
        ))
    }

    fn is_live(&self, processes: &crate::supervisor::ProcessSnapshot) -> bool {
        matches!(processes, crate::supervisor::ProcessSnapshot::Read(live) if live.iter().any(|process|
            process.session_id == self.session_id
                && process.role_generation_id == self.role_generation_id
                && process.transcript_epoch == self.transcript_epoch
                && process.process_identity_json == self.process_identity_json))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CodexCapacityCandidate {
    binding: CodexCapacityBinding,
    hook_frontier: i64,
    transcript_path: std::path::PathBuf,
}

#[derive(Serialize, serde::Deserialize)]
struct CodexCapacityObservation {
    binding: CodexCapacityBinding,
    hook_frontier: i64,
    source: crate::providers::codex::CapacityFileIdentity,
    error_kind: String,
}

fn codex_capacity_candidate(
    connection: &Connection,
    session_id: &str,
    frontier: Option<i64>,
) -> Result<Option<CodexCapacityCandidate>> {
    codex_history_candidate(connection, session_id, frontier, false)
}

fn codex_history_candidate(
    connection: &Connection,
    session_id: &str,
    frontier: Option<i64>,
    observe_usage: bool,
) -> Result<Option<CodexCapacityCandidate>> {
    let candidate = connection.query_row(
        &format!("WITH {}, frontier AS (
           SELECT COALESCE(?2,MAX(hook_rowid)) AS hook_rowid FROM current_hooks)
         SELECT s.role_generation_id,a.id,s.transcript_epoch,s.native_session_id,s.process_identity_json,
                COALESCE((SELECT path FROM workspaces WHERE attempt_id=a.id),p.repository_path),
                accepted.id,json_extract(accepted.payload_json,'$.cwd'),
                json_extract(accepted.payload_json,'$.turn_id'),
                json_extract(accepted.payload_json,'$.transcript_path'),
                (SELECT hook_rowid FROM invocation_start),frontier.hook_rowid,s.launch_config_json,rg.role
         FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN projects p ON p.id=t.project_id CROSS JOIN accepted CROSS JOIN frontier
         WHERE s.id=?1 AND s.provider='codex' AND rg.provider='codex'
           AND s.status='running' AND rg.status='running' AND s.exit_json IS NULL
           AND s.executable_version='codex-cli 0.157.1' AND s.process_identity_json IS NOT NULL
           AND {OBSERVED_CURRENT_ATTEMPT_SQL} AND {OBSERVED_EFFECTIVE_GENERATION_SQL}
           AND json_type(accepted.payload_json,'$.session_id')='text'
           AND json_extract(accepted.payload_json,'$.session_id')=s.native_session_id
           AND json_type(accepted.payload_json,'$.cwd')='text'
           AND json_type(accepted.payload_json,'$.turn_id')='text'
           AND length(json_extract(accepted.payload_json,'$.turn_id')) BETWEEN 1 AND 128
           AND json_type(accepted.payload_json,'$.transcript_path')='text'
           AND frontier.hook_rowid>=accepted.hook_rowid
           AND NOT EXISTS(SELECT 1 FROM current_hooks h WHERE h.event_name IN ('SessionStart','SessionEnd'))
           AND (?3 OR NOT EXISTS(SELECT 1 FROM current_hooks h WHERE
             h.hook_rowid>accepted.hook_rowid
                 AND json_type(h.payload_json,'$.turn_id')='text'
                 AND json_extract(h.payload_json,'$.turn_id')=json_extract(accepted.payload_json,'$.turn_id')
                 AND (h.event_name IN ('Stop','Interrupt')
                   OR (h.hook_rowid>frontier.hook_rowid AND h.event_name IN
                       ('PreToolUse','PostToolUse','PermissionRequest','SubagentStart','SubagentStop')))))",
            crate::workflow::CURRENT_TURN_HOOKS_SQL),
        params![session_id, frontier, observe_usage],
        |row| {
            Ok((
                CodexCapacityCandidate {
                    binding: CodexCapacityBinding {
                        session_id: session_id.to_owned(),
                        role_generation_id: row.get(0)?,
                        attempt_id: row.get(1)?,
                        transcript_epoch: row.get(2)?,
                        native_session_id: row.get(3)?,
                        process_identity_json: row.get(4)?,
                        cwd: row.get(5)?,
                        accepted_hook_id: row.get(6)?,
                        hook_cwd: row.get(7)?,
                        turn_id: row.get(8)?,
                        invocation_start: row.get(10)?,
                        launch_cwd: String::new(),
                    },
                    transcript_path: std::path::PathBuf::from(row.get::<_, String>(9)?),
                    hook_frontier: row.get(11)?,
                },
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
            ))
        },
    ).optional()?;
    let Some((mut candidate, launch, role)) = candidate else {
        return Ok(None);
    };
    let Ok(launch) = serde_json::from_str::<LaunchConfig>(&launch) else {
        return Ok(None);
    };
    let Some(binding) = launch.compatibility.as_ref() else {
        return Ok(None);
    };
    let Ok(admitted) = crate::provider_compatibility::BundleSet::embedded().resolve(
        crate::domain::Provider::Codex,
        crate::providers::codex::EXACT_CODEX_VERSION,
        launch.role,
    ) else {
        return Ok(None);
    };
    if launch.provider != crate::domain::Provider::Codex
        || launch.role.to_string() != role
        || launch.executable_version != crate::providers::codex::EXACT_CODEX_VERSION
        || crate::provider_compatibility::AuthorityBinding::from(binding)
            != crate::provider_compatibility::AuthorityBinding::from(&admitted)
        || uuid::Uuid::parse_str(&candidate.binding.native_session_id).is_err()
    {
        return Ok(None);
    }
    candidate.binding.launch_cwd = launch.cwd.to_string_lossy().into_owned();
    if [
        &candidate.binding.cwd,
        &candidate.binding.launch_cwd,
        &candidate.binding.hook_cwd,
        &candidate.binding.process_identity_json,
        &candidate.binding.turn_id,
    ]
    .iter()
    .any(|text| text.trim().is_empty() || text.len() > 4096 || text.contains('\0'))
        || candidate.transcript_path.as_os_str().len() > 4096
    {
        return Ok(None);
    }
    Ok(Some(candidate))
}

impl Store {
    pub(crate) fn read_codex_capacity(
        &self,
        root: &Path,
        processes: &crate::supervisor::ProcessSnapshot,
    ) -> Result<
        Vec<(
            CodexCapacityCandidate,
            crate::providers::codex::CapacityFileIdentity,
        )>,
    > {
        let crate::supervisor::ProcessSnapshot::Read(live) = processes else {
            return Ok(Vec::new());
        };
        let candidates = {
            let connection = self.lock()?;
            let mut candidates = Vec::new();
            for process in live {
                let Some(candidate) =
                    codex_capacity_candidate(&connection, &process.session_id, None)?
                else {
                    continue;
                };
                let recorded: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM audit_events WHERE id=?1)",
                    params![candidate.binding.audit_id()?],
                    |row| row.get(0),
                )?;
                if candidate.binding.is_live(processes) && !recorded {
                    candidates.push(candidate);
                }
            }
            candidates
        };
        Ok(candidates
            .into_iter()
            .filter_map(|candidate| {
                let binding = &candidate.binding;
                let cwd = std::fs::canonicalize(&binding.cwd).ok()?;
                if std::fs::canonicalize(&binding.launch_cwd).ok()? != cwd
                    || std::fs::canonicalize(&binding.hook_cwd).ok()? != cwd
                {
                    return None;
                }
                let source = crate::providers::codex::read_capacity_history(
                    root,
                    &candidate.transcript_path,
                    &binding.native_session_id,
                    &binding.turn_id,
                    &cwd,
                )?;
                Some((candidate, source))
            })
            .collect())
    }

    pub(crate) fn record_codex_capacity(
        &self,
        candidate: &CodexCapacityCandidate,
        source: &crate::providers::codex::CapacityFileIdentity,
        processes: &crate::supervisor::ProcessSnapshot,
    ) -> Result<()> {
        if !candidate.binding.is_live(processes) {
            return Ok(());
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = codex_capacity_candidate(
            &transaction,
            &candidate.binding.session_id,
            Some(candidate.hook_frontier),
        )?;
        if current.as_ref() == Some(candidate) {
            let id = candidate.binding.audit_id()?;
            let observation = CodexCapacityObservation {
                binding: candidate.binding.clone(),
                hook_frontier: candidate.hook_frontier,
                source: source.clone(),
                error_kind: "server_overloaded".into(),
            };
            // The frontier orders activity, but is excluded from identity so a
            // retired binding can never acquire a fresh observation frontier.
            transaction.execute(
                "INSERT OR IGNORE INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                 VALUES(?1,?1,'service',?2,'session',?3,?4,?5)",
                params![
                    id, CODEX_CAPACITY_EVENT, candidate.binding.session_id,
                    serde_json::to_string(&observation)?, Utc::now().to_rfc3339()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

const CODEX_CAPACITY_AUDITS_SQL: &str =
    "SELECT id,detail_json FROM audit_events INDEXED BY audit_events_entity
    WHERE entity_kind='session' AND entity_id=?1 AND actor_kind='service'
      AND event_code='provider.codex_capacity_observed' AND length(detail_json)<=32768";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodexUsageCandidate {
    binding: CodexCapacityBinding,
    task_id: String,
    transcript_path: std::path::PathBuf,
    canonical_cwd: std::path::PathBuf,
    cwd_identity: (u64, u64),
}

fn codex_usage_candidate(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<CodexUsageCandidate>> {
    use std::os::unix::fs::MetadataExt;
    let Some(candidate) = codex_history_candidate(connection, session_id, None, true)? else {
        return Ok(None);
    };
    let binding = candidate.binding;
    if !crate::providers::codex::valid_usage_id(&binding.native_session_id)
        || !crate::providers::codex::valid_usage_id(&binding.turn_id)
    {
        return Ok(None);
    }
    let Ok(cwd) = std::fs::canonicalize(&binding.cwd) else {
        return Ok(None);
    };
    let Ok(cwd_metadata) = std::fs::metadata(&cwd) else {
        return Ok(None);
    };
    if !cwd_metadata.is_dir() {
        return Ok(None);
    }
    if std::fs::canonicalize(&binding.launch_cwd).ok().as_ref() != Some(&cwd)
        || std::fs::canonicalize(&binding.hook_cwd).ok().as_ref() != Some(&cwd)
    {
        return Ok(None);
    }
    let task_id = connection.query_row(
        "SELECT task_id FROM attempts WHERE id=?1",
        params![binding.attempt_id],
        |row| row.get(0),
    )?;
    Ok(Some(CodexUsageCandidate {
        binding,
        task_id,
        transcript_path: candidate.transcript_path,
        canonical_cwd: cwd,
        cwd_identity: (cwd_metadata.dev(), cwd_metadata.ino()),
    }))
}

impl Store {
    pub(crate) fn read_codex_usage(
        &self,
        root: &Path,
        processes: &crate::supervisor::ProcessSnapshot,
    ) -> Result<
        Vec<(
            CodexUsageCandidate,
            crate::providers::codex::UsageHistoryRead,
        )>,
    > {
        let mut candidates = Vec::new();
        {
            let connection = self.lock()?;
            if let crate::supervisor::ProcessSnapshot::Read(live) = processes {
                for process in live {
                    if let Some(candidate) =
                        codex_usage_candidate(&connection, &process.session_id)?
                    {
                        if candidate.binding.is_live(processes) {
                            candidates.push(candidate);
                        }
                    }
                }
            }
        }
        let cached = {
            let mut cache = self
                .codex_usage_cache
                .lock()
                .map_err(|_| anyhow!("Codex usage cache is poisoned"))?;
            cache.retain(|_, (binding, _)| candidates.contains(binding));
            cache.clone()
        };
        let mut observations = Vec::new();
        for candidate in candidates {
            let binding = &candidate.binding;
            let previous = cached
                .get(&binding.session_id)
                .filter(|(old, _)| old == &candidate)
                .map(|(_, identity)| identity);
            match crate::providers::codex::read_usage_history(
                root,
                &candidate.transcript_path,
                &binding.native_session_id,
                &binding.turn_id,
                &candidate.canonical_cwd,
                previous,
            ) {
                Some(crate::providers::codex::UsageHistoryRead::Unchanged) => {}
                Some(observed) => observations.push((candidate, observed)),
                None => {
                    self.codex_usage_cache
                        .lock()
                        .map_err(|_| anyhow!("Codex usage cache is poisoned"))?
                        .remove(&binding.session_id);
                }
            }
        }
        Ok(observations)
    }

    pub(crate) fn record_codex_usage(
        &self,
        root: &Path,
        candidate: &CodexUsageCandidate,
        observed: &crate::providers::codex::UsageHistoryRead,
        processes: &crate::supervisor::ProcessSnapshot,
    ) -> Result<()> {
        let crate::providers::codex::UsageHistoryRead::Observed {
            identity,
            responses,
        } = observed
        else {
            return Ok(());
        };
        let binding = &candidate.binding;
        if !binding.is_live(processes) {
            return Ok(());
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if codex_usage_candidate(&transaction, &binding.session_id)?.as_ref() != Some(candidate)
            || crate::providers::codex::usage_history_identity(root, &candidate.transcript_path)
                .as_ref()
                != Some(identity)
        {
            return Ok(());
        }
        let source = serde_json::to_string(identity)?;
        let now = Utc::now().to_rfc3339();
        let owner = serde_json::to_string(&serde_json::json!({"task_id":candidate.task_id,
            "session_id":binding.session_id,"accepted_hook_event_id":binding.accepted_hook_id}))?;
        for response in responses {
            let counters = &response.counters;
            let existing: Option<bool> = transaction.query_row(
                "SELECT task_id=?4 AND attempt_id=?5 AND role_generation_id=?6 AND session_id=?7
                    AND transcript_epoch=?8 AND invocation_start=?9 AND accepted_hook_event_id=?10
                    AND input_tokens=?11 AND cached_input_tokens=?12 AND cache_write_input_tokens IS ?13
                    AND output_tokens=?14 AND reasoning_output_tokens=?15 AND total_tokens=?16
                    AND source_version='0.157.1' AND contract_revision=?17
                 FROM codex_usage_observations WHERE provider='codex' AND native_thread_id=?1
                    AND native_turn_id=?2 AND response_id=?3",
                params![binding.native_session_id, binding.turn_id, response.response_id, candidate.task_id,
                    binding.attempt_id, binding.role_generation_id, binding.session_id, binding.transcript_epoch,
                    binding.invocation_start, binding.accepted_hook_id, counters.input_tokens, counters.cached_input_tokens,
                    counters.cache_write_input_tokens, counters.output_tokens, counters.reasoning_output_tokens,
                    counters.total_tokens, crate::providers::codex::USAGE_CONTRACT_REVISION], |row| row.get(0)
            ).optional()?;
            match existing {
                Some(true) => {}
                Some(false) => {
                    // Preserve the first owner so all conflicting owners stay
                    // unavailable after restart.
                    transaction.execute(
                        "UPDATE codex_usage_observations SET invalid=1,
                            conflict_owners_json=CASE WHEN EXISTS(SELECT 1 FROM json_each(conflict_owners_json)
                                WHERE value=json(?4)) THEN conflict_owners_json
                                ELSE json_insert(conflict_owners_json,'$[#]',json(?4)) END
                         WHERE provider='codex' AND native_thread_id=?1 AND native_turn_id=?2 AND response_id=?3",
                        params![binding.native_session_id,binding.turn_id,response.response_id,owner])?;
                }
                None => {
                    transaction.execute(
                        "INSERT INTO codex_usage_observations(provider,native_thread_id,native_turn_id,response_id,
                            task_id,attempt_id,role_generation_id,session_id,transcript_epoch,invocation_start,
                            accepted_hook_event_id,input_tokens,cached_input_tokens,cache_write_input_tokens,
                            output_tokens,reasoning_output_tokens,total_tokens,source_version,contract_revision,
                            source_identity_json,received_at)
                         VALUES('codex',?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,'0.157.1',?17,?18,?19)",
                        params![binding.native_session_id, binding.turn_id, response.response_id, candidate.task_id,
                            binding.attempt_id, binding.role_generation_id, binding.session_id, binding.transcript_epoch,
                            binding.invocation_start, binding.accepted_hook_id, counters.input_tokens, counters.cached_input_tokens,
                            counters.cache_write_input_tokens, counters.output_tokens, counters.reasoning_output_tokens,
                            counters.total_tokens, crate::providers::codex::USAGE_CONTRACT_REVISION, source, now])?;
                }
            }
        }
        transaction.commit()?;
        self.codex_usage_cache
            .lock()
            .map_err(|_| anyhow!("Codex usage cache is poisoned"))?
            .insert(
                binding.session_id.clone(),
                (candidate.clone(), identity.clone()),
            );
        Ok(())
    }
}

pub(crate) fn current_codex_capacity(connection: &Connection) -> Result<Vec<CodexCapacityBinding>> {
    let mut sessions = connection
        .prepare("SELECT id FROM sessions WHERE provider='codex' AND status='running'")?;
    let sessions = sessions
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut current = Vec::new();
    let mut audits = connection.prepare(CODEX_CAPACITY_AUDITS_SQL)?;
    for session in sessions {
        let rows = audits.query_map(params![session], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, detail) = row?;
            let Ok(observation) = serde_json::from_str::<CodexCapacityObservation>(&detail) else {
                continue;
            };
            if observation.error_kind != "server_overloaded"
                || observation.binding.audit_id()? != id
            {
                continue;
            }
            if let Some(candidate) =
                codex_capacity_candidate(connection, &session, Some(observation.hook_frontier))?
            {
                if candidate.binding == observation.binding {
                    current.push(observation.binding);
                }
            }
        }
    }
    Ok(current)
}

/// Session `s` waits on a person through a persisted gate; `?2` is the pass time.
const OBSERVED_PERSON_GATE_SQL: &str = "(EXISTS(SELECT 1 FROM permission_requests request
       WHERE request.session_id=s.id AND request.state='pending'
         AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                        WHERE native.permission_request_id=request.id))
     OR EXISTS(SELECT 1 FROM input_leases lease WHERE lease.session_id=s.id
         AND lease.revoked_at IS NULL AND julianday(lease.expires_at)>julianday(?2)))";

#[derive(Default)]
struct ObservationBinding {
    task_id: Option<String>,
    attempt_id: Option<String>,
    role: Option<String>,
    lane_id: Option<String>,
    role_generation_id: Option<String>,
    session_id: Option<String>,
    transcript_epoch: Option<String>,
    source_id: Option<String>,
}

struct ObservationSubject {
    kind: ObservationKind,
    entity_key: String,
    binding: ObservationBinding,
}

impl ObservationSubject {
    fn new(kind: ObservationKind, entity_key: String) -> Self {
        Self {
            kind,
            entity_key,
            binding: ObservationBinding::default(),
        }
    }

    fn bound(mut self, binding: ObservationBinding) -> Self {
        self.binding = binding;
        self
    }
}

enum Finding {
    Holds {
        fingerprint: String,
        evidence: serde_json::Value,
        /// Thresholds measured from persisted facts are met.
        ready: bool,
        /// The same fingerprint must also have been observed this long.
        unchanged_seconds: Option<i64>,
    },
    /// Facts are unavailable or a person's gate suppresses classification.
    Suspended { uncertain: bool },
    /// Positive evidence that the condition ended.
    Ended(String),
}

impl Finding {
    fn ended(reason: &str) -> Self {
        Self::Ended(reason.to_owned())
    }
}

struct StoredObservation {
    id: String,
    state: String,
    fingerprint: String,
    uncertain: bool,
    observed_since: String,
    last_eligible_at: Option<String>,
    confirmations: i64,
}

struct UnresolvedObservation {
    attempt_id: Option<String>,
    session_id: Option<String>,
    transcript_epoch: Option<String>,
    source_id: Option<String>,
}

/// False for an unparseable time or one after `now`, which only a clock moved back can produce.
fn at_least(earlier: &str, now: chrono::DateTime<Utc>, seconds: i64) -> bool {
    chrono::DateTime::parse_from_rfc3339(earlier).is_ok_and(|earlier| {
        now - earlier.with_timezone(&Utc) >= chrono::Duration::seconds(seconds)
    })
}

struct ObservationPass<'a> {
    transaction: &'a Transaction<'a>,
    now: chrono::DateTime<Utc>,
    now_text: String,
    processes: &'a crate::supervisor::ProcessSnapshot,
}

impl Store {
    /// One classification pass. It writes only attention observations and their
    /// transition audits; it never holds, retries, signals or settles work.
    pub(crate) fn observe_attention(
        &self,
        now: chrono::DateTime<Utc>,
        processes: &crate::supervisor::ProcessSnapshot,
        fresh_boot: bool,
    ) -> Result<()> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if fresh_boot {
            // Time before this boot is neither quiet nor waiting history.
            transaction.execute(
                "DELETE FROM attention_observations
                 WHERE state='candidate' AND kind!='recurring_block'",
                [],
            )?;
        }
        let pass = ObservationPass {
            transaction: &transaction,
            now,
            now_text: now.to_rfc3339(),
            processes,
        };
        pass.observe_unaccepted_processes()?;
        pass.observe_quiet_turns()?;
        pass.observe_attempts_without_live_session()?;
        pass.observe_unaccepted_guidance()?;
        pass.observe_permissions_on_exited_sessions()?;
        pass.observe_resume_failures_after_acceptance()?;
        pass.observe_busy_after_exit()?;
        pass.reconcile_blocking_episodes()?;
        pass.observe_recurring_blocks()?;
        transaction.commit()?;
        Ok(())
    }
}

impl ObservationPass<'_> {
    fn stored(&self, kind: ObservationKind, entity_key: &str) -> Result<Option<StoredObservation>> {
        Ok(self
            .transaction
            .query_row(
                "SELECT id,state,evidence_fingerprint,uncertain,observed_since,last_eligible_at,
                        confirmations
                 FROM attention_observations WHERE entity_kind=?1 AND entity_key=?2 AND kind=?3",
                params![kind.entity_kind(), entity_key, kind.as_str()],
                |row| {
                    Ok(StoredObservation {
                        id: row.get(0)?,
                        state: row.get(1)?,
                        fingerprint: row.get(2)?,
                        uncertain: row.get(3)?,
                        observed_since: row.get(4)?,
                        last_eligible_at: row.get(5)?,
                        confirmations: row.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    fn unresolved(&self, kind: ObservationKind) -> Result<Vec<UnresolvedObservation>> {
        let mut statement = self.transaction.prepare(
            "SELECT attempt_id,session_id,transcript_epoch,source_id FROM attention_observations
             WHERE kind=?1 AND state IN ('candidate','open') ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![kind.as_str()], |row| {
                Ok(UnresolvedObservation {
                    attempt_id: row.get(0)?,
                    session_id: row.get(1)?,
                    transcript_epoch: row.get(2)?,
                    source_id: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `None` while the process table is unknown; otherwise whether that exact
    /// invocation's recorded root process was alive.
    fn invocation_live(
        &self,
        session_id: &str,
        generation_id: &str,
        epoch: &str,
        process_json: Option<&str>,
    ) -> Option<bool> {
        match self.processes {
            crate::supervisor::ProcessSnapshot::Unavailable => None,
            crate::supervisor::ProcessSnapshot::Read(live) => Some(live.iter().any(|invocation| {
                invocation.session_id == session_id
                    && invocation.role_generation_id == generation_id
                    && invocation.transcript_epoch == epoch
                    && Some(invocation.process_identity_json.as_str()) == process_json
            })),
        }
    }

    fn native_prompt_waiting(&self, session_id: &str) -> Result<bool> {
        Ok(crate::workflow::native_prompt(self.transaction, session_id)?.is_some())
    }

    fn audit(
        &self,
        observation_id: &str,
        event_code: &str,
        detail: serde_json::Value,
    ) -> Result<()> {
        self.transaction.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service',?3,'attention_observation',?4,?5,?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                event_code,
                observation_id,
                detail.to_string(),
                self.now_text
            ],
        )?;
        Ok(())
    }

    /// Candidates confirm on distinct sweeps and are discarded the moment their
    /// evidence is interrupted; an open record ends only on positive evidence.
    fn advance(&self, subject: ObservationSubject, finding: Finding) -> Result<()> {
        let kind = subject.kind;
        let stored = self.stored(kind, &subject.entity_key)?;
        match (stored, finding) {
            (
                stored,
                Finding::Holds {
                    fingerprint,
                    mut evidence,
                    ready,
                    unchanged_seconds,
                },
            ) => {
                if let Some(stored) = stored.as_ref().filter(|stored| stored.state == "open") {
                    if stored.uncertain || stored.fingerprint != fingerprint {
                        self.transaction.execute(
                            "UPDATE attention_observations SET uncertain=0,evidence_fingerprint=?1,
                                    evidence_json=CASE WHEN evidence_fingerprint=?1 THEN evidence_json ELSE ?2 END,
                                    last_seen_at=?3
                             WHERE id=?4",
                            params![fingerprint, evidence.to_string(), self.now_text, stored.id],
                        )?;
                    }
                    return Ok(());
                }
                let continuing = stored.as_ref().filter(|stored| {
                    stored.state == "candidate"
                        && stored.fingerprint == fingerprint
                        && at_least(&stored.observed_since, self.now, 0)
                        && stored
                            .last_eligible_at
                            .as_deref()
                            .is_none_or(|last| at_least(last, self.now, 0))
                });
                let observed_since = continuing.map_or(self.now_text.clone(), |stored| {
                    stored.observed_since.clone()
                });
                let eligible = ready
                    && unchanged_seconds
                        .is_none_or(|required| at_least(&observed_since, self.now, required));
                let (confirmations, last_eligible_at) = match (continuing, eligible) {
                    (_, false) => (0, None),
                    (Some(stored), true) => match stored.last_eligible_at.as_deref() {
                        Some(last) if !at_least(last, self.now, CONFIRMATION_SPACING_SECONDS) => {
                            (stored.confirmations, Some(last.to_owned()))
                        }
                        _ => (
                            (stored.confirmations + 1).min(kind.confirmations_required()),
                            Some(self.now_text.clone()),
                        ),
                    },
                    (None, true) => (1, Some(self.now_text.clone())),
                };
                let opens = confirmations >= kind.confirmations_required();
                if let Some(stored) = continuing {
                    if !opens
                        && stored.confirmations == confirmations
                        && stored.last_eligible_at == last_eligible_at
                    {
                        return Ok(());
                    }
                }
                if unchanged_seconds.is_some() {
                    evidence["observed_since"] = observed_since.clone().into();
                }
                let state = if opens { "open" } else { "candidate" };
                let binding = &subject.binding;
                let id = match stored {
                    Some(stored) => {
                        self.transaction.execute(
                            "UPDATE attention_observations SET state=?1,evidence_fingerprint=?2,
                                    evidence_json=?3,uncertain=0,last_seen_at=?4,observed_since=?5,
                                    last_eligible_at=?6,confirmations=?7,
                                    opened_at=CASE WHEN ?1='open' THEN ?4 ELSE NULL END,
                                    resolved_at=NULL,resolution_reason=NULL,task_id=?8,attempt_id=?9,
                                    role=?10,lane_id=?11,role_generation_id=?12,session_id=?13,
                                    transcript_epoch=?14,source_id=?15
                             WHERE id=?16",
                            params![
                                state,
                                fingerprint,
                                evidence.to_string(),
                                self.now_text,
                                observed_since,
                                last_eligible_at,
                                confirmations,
                                binding.task_id,
                                binding.attempt_id,
                                binding.role,
                                binding.lane_id,
                                binding.role_generation_id,
                                binding.session_id,
                                binding.transcript_epoch,
                                binding.source_id,
                                stored.id
                            ],
                        )?;
                        stored.id
                    }
                    None => {
                        let id = uuid::Uuid::new_v4().to_string();
                        self.transaction.execute(
                            "INSERT INTO attention_observations(id,entity_kind,entity_key,kind,task_id,
                                attempt_id,role,lane_id,role_generation_id,session_id,transcript_epoch,
                                source_id,state,evidence_fingerprint,evidence_json,first_seen_at,
                                last_seen_at,observed_since,last_eligible_at,confirmations,opened_at)
                             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?16,?16,?17,?18,
                                    CASE WHEN ?13='open' THEN ?16 ELSE NULL END)",
                            params![
                                id,
                                kind.entity_kind(),
                                subject.entity_key,
                                kind.as_str(),
                                binding.task_id,
                                binding.attempt_id,
                                binding.role,
                                binding.lane_id,
                                binding.role_generation_id,
                                binding.session_id,
                                binding.transcript_epoch,
                                binding.source_id,
                                state,
                                fingerprint,
                                evidence.to_string(),
                                self.now_text,
                                last_eligible_at,
                                confirmations
                            ],
                        )?;
                        id
                    }
                };
                if opens {
                    self.audit(
                        &id,
                        "attention.observation.opened",
                        serde_json::json!({"kind":kind.as_str(),"entity_key":subject.entity_key}),
                    )?;
                }
                Ok(())
            }
            (None, Finding::Suspended { .. } | Finding::Ended(_)) => Ok(()),
            (Some(stored), Finding::Suspended { uncertain }) => {
                match stored.state.as_str() {
                    "candidate" => {
                        self.transaction.execute(
                            "DELETE FROM attention_observations WHERE id=?1",
                            params![stored.id],
                        )?;
                    }
                    "open" if uncertain && !stored.uncertain => {
                        self.transaction.execute(
                            "UPDATE attention_observations SET uncertain=1,last_seen_at=?1 WHERE id=?2",
                            params![self.now_text, stored.id],
                        )?;
                    }
                    _ => {}
                }
                Ok(())
            }
            (Some(stored), Finding::Ended(reason)) => {
                match stored.state.as_str() {
                    "candidate" => {
                        self.transaction.execute(
                            "DELETE FROM attention_observations WHERE id=?1",
                            params![stored.id],
                        )?;
                    }
                    "open" => {
                        self.transaction.execute(
                            "UPDATE attention_observations SET state='resolved',uncertain=0,
                                    resolved_at=?1,resolution_reason=?2,last_seen_at=?1
                             WHERE id=?3",
                            params![self.now_text, reason, stored.id],
                        )?;
                        self.audit(
                            &stored.id,
                            "attention.observation.resolved",
                            serde_json::json!({"kind":kind.as_str(),"entity_key":subject.entity_key,"reason":reason}),
                        )?;
                    }
                    _ => {}
                }
                Ok(())
            }
        }
    }

    fn running_current_sessions(&self) -> Result<BTreeSet<(String, String)>> {
        let mut statement = self.transaction.prepare(&format!(
            "SELECT s.id,s.transcript_epoch FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE s.status='running' AND {OBSERVED_CURRENT_ATTEMPT_SQL}"
        ))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        Ok(rows)
    }

    fn observe_unaccepted_processes(&self) -> Result<()> {
        let kind = ObservationKind::ProcessWithoutAcceptedTurn;
        let mut subjects = self.running_current_sessions()?;
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| Some((row.session_id?, row.transcript_epoch?))),
        );
        for (session_id, epoch) in subjects {
            let (subject, finding) = self.unaccepted_process(&session_id, &epoch)?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    fn unaccepted_process(
        &self,
        session_id: &str,
        epoch: &str,
    ) -> Result<(ObservationSubject, Finding)> {
        type Row = (
            ObservationBinding,
            String,
            String,
            bool,
            Option<String>,
            bool,
            bool,
            Option<String>,
            Option<String>,
            bool,
            bool,
        );
        let subject = ObservationSubject::new(
            ObservationKind::ProcessWithoutAcceptedTurn,
            format!("{session_id}:{epoch}"),
        );
        let row: Option<Row> = self
            .transaction
            .query_row(
                &format!(
                    "WITH {}
                     SELECT t.id,a.id,rg.role,rg.lane_id,rg.id,
                            (SELECT ri.id FROM resume_invocations ri WHERE ri.session_id=s.id
                               AND ri.transcript_epoch=s.transcript_epoch
                             ORDER BY ri.resume_ordinal DESC LIMIT 1),
                            s.transcript_epoch,s.status,s.exit_json IS NOT NULL,s.process_identity_json,
                            {OBSERVED_CURRENT_ATTEMPT_SQL},{OBSERVED_EFFECTIVE_GENERATION_SQL},
                            (SELECT start.id FROM hook_events start
                             WHERE start.rowid=(SELECT hook_rowid FROM invocation_start)),
                            (SELECT start.received_at FROM hook_events start
                             WHERE start.rowid=(SELECT hook_rowid FROM invocation_start)),
                            EXISTS(SELECT 1 FROM accepted),{OBSERVED_PERSON_GATE_SQL}
                     FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                     JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                     WHERE s.id=?1",
                    crate::workflow::CURRENT_TURN_HOOKS_SQL
                ),
                params![session_id, self.now_text],
                |row| {
                    Ok((
                        ObservationBinding {
                            task_id: row.get(0)?,
                            attempt_id: row.get(1)?,
                            role: row.get(2)?,
                            lane_id: row.get(3)?,
                            role_generation_id: row.get(4)?,
                            session_id: Some(session_id.to_owned()),
                            transcript_epoch: Some(epoch.to_owned()),
                            source_id: row.get(5)?,
                        },
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
                    ))
                },
            )
            .optional()?;
        let Some((
            binding,
            current_epoch,
            status,
            exit_recorded,
            process_json,
            current_attempt,
            effective,
            start_id,
            started_at,
            accepted,
            gated,
        )) = row
        else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let finding = if !current_attempt {
            Finding::ended("attempt_retired")
        } else if !effective {
            Finding::ended("role_replaced")
        } else if current_epoch != epoch {
            Finding::ended("invocation_replaced")
        } else if accepted {
            Finding::ended("turn_accepted")
        } else if status == "exited" && exit_recorded {
            Finding::ended("process_exited")
        } else if status != "running" {
            Finding::Suspended { uncertain: false }
        } else if let (Some(start_id), Some(started_at)) = (start_id, started_at) {
            let generation = binding.role_generation_id.as_deref().unwrap_or_default();
            match self.invocation_live(session_id, generation, epoch, process_json.as_deref()) {
                None | Some(false) => Finding::Suspended { uncertain: true },
                Some(true) if gated || self.native_prompt_waiting(session_id)? => {
                    Finding::Suspended { uncertain: false }
                }
                Some(true) => Finding::Holds {
                    ready: at_least(
                        &started_at,
                        self.now,
                        crate::workflow::ACCEPTANCE_OBSERVATION_SECONDS,
                    ),
                    evidence: serde_json::json!({
                        "session_start_hook_event_id":start_id,
                        "session_started_at":started_at,
                        "threshold_seconds":crate::workflow::ACCEPTANCE_OBSERVATION_SECONDS,
                    }),
                    fingerprint: start_id,
                    unchanged_seconds: None,
                },
            }
        } else {
            // Without this invocation's trusted start, absence of acceptance means nothing.
            Finding::Suspended { uncertain: false }
        };
        Ok((subject.bound(binding), finding))
    }

    fn observe_quiet_turns(&self) -> Result<()> {
        let kind = ObservationKind::QuietTurn;
        let mut subjects = BTreeSet::new();
        for (session_id, epoch) in self.running_current_sessions()? {
            let accepted: Option<String> = self
                .transaction
                .query_row(
                    &format!(
                        "WITH {} SELECT id FROM accepted",
                        crate::workflow::CURRENT_TURN_HOOKS_SQL
                    ),
                    params![session_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(accepted) = accepted {
                subjects.insert((session_id, epoch, accepted));
            }
        }
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| Some((row.session_id?, row.transcript_epoch?, row.source_id?))),
        );
        for (session_id, epoch, accepted) in subjects {
            let (subject, finding) = self.quiet_turn(&session_id, &epoch, &accepted)?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    fn quiet_turn(
        &self,
        session_id: &str,
        epoch: &str,
        accepted_hook_id: &str,
    ) -> Result<(ObservationSubject, Finding)> {
        type Row = (
            ObservationBinding,
            String,
            String,
            bool,
            Option<String>,
            bool,
            bool,
            Option<String>,
            Option<String>,
            Option<i64>,
            i64,
            String,
            bool,
            bool,
            bool,
        );
        let kind = ObservationKind::QuietTurn;
        let subject =
            ObservationSubject::new(kind, format!("{session_id}:{epoch}:{accepted_hook_id}"));
        let row: Option<Row> = self
            .transaction
            .query_row(
                &format!(
                    "WITH {}
                     SELECT t.id,a.id,rg.role,rg.lane_id,rg.id,s.transcript_epoch,s.status,
                            s.exit_json IS NOT NULL,s.process_identity_json,
                            {OBSERVED_CURRENT_ATTEMPT_SQL},{OBSERVED_EFFECTIVE_GENERATION_SQL},
                            (SELECT id FROM accepted),(SELECT received_at FROM accepted),
                            (SELECT MAX(hook_rowid) FROM current_hooks),s.transcript_last_sequence,
                            s.capture_state,
                            EXISTS(SELECT 1 FROM current_hooks later WHERE later.event_name='Stop'
                              AND later.hook_rowid>(SELECT hook_rowid FROM accepted)
                              AND {}),
                            EXISTS(SELECT 1 FROM role_results report WHERE report.session_id=s.id
                              AND report.role_generation_id=s.role_generation_id
                              AND julianday(report.created_at)>=julianday((SELECT received_at FROM accepted))
                              AND NOT EXISTS(SELECT 1 FROM role_result_supersessions retired WHERE retired.role_result_id=report.id)
                              AND NOT EXISTS(SELECT 1 FROM audit_events retired WHERE retired.event_code='role_result.superseded'
                                AND retired.entity_kind='role_result' AND retired.entity_id=report.id)),
                            {OBSERVED_PERSON_GATE_SQL}
                     FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                     JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                     WHERE s.id=?1",
                    crate::workflow::CURRENT_TURN_HOOKS_SQL,
                    crate::workflow::belongs_to_accepted_turn("later")
                ),
                params![session_id, self.now_text],
                |row| {
                    Ok((
                        ObservationBinding {
                            task_id: row.get(0)?,
                            attempt_id: row.get(1)?,
                            role: row.get(2)?,
                            lane_id: row.get(3)?,
                            role_generation_id: row.get(4)?,
                            session_id: Some(session_id.to_owned()),
                            transcript_epoch: Some(epoch.to_owned()),
                            source_id: Some(accepted_hook_id.to_owned()),
                        },
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
            .optional()?;
        let Some((
            binding,
            current_epoch,
            status,
            exit_recorded,
            process_json,
            current_attempt,
            effective,
            current_accepted,
            accepted_at,
            hook_watermark,
            sequence,
            capture_state,
            stopped,
            reported,
            gated,
        )) = row
        else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let fingerprint = format!("{}:{sequence}", hook_watermark.unwrap_or_default());
        let finding = if !current_attempt {
            Finding::ended("attempt_retired")
        } else if !effective {
            Finding::ended("role_replaced")
        } else if current_epoch != epoch {
            Finding::ended("invocation_replaced")
        } else if status == "exited" && exit_recorded {
            Finding::ended("process_exited")
        } else if let (Some(current_accepted), Some(accepted_at)) = (current_accepted, accepted_at)
        {
            let generation = binding.role_generation_id.as_deref().unwrap_or_default();
            if current_accepted != accepted_hook_id {
                Finding::ended("superseded_by_newer_turn")
            } else if stopped || reported {
                Finding::ended("turn_completed")
            } else if status != "running" {
                Finding::Suspended { uncertain: false }
            } else if capture_state != "capturing" {
                Finding::Suspended { uncertain: true }
            } else if self.invocation_live(session_id, generation, epoch, process_json.as_deref())
                != Some(true)
            {
                Finding::Suspended { uncertain: true }
            } else if self
                .stored(kind, &subject.entity_key)?
                .is_some_and(|stored| stored.state == "open" && stored.fingerprint != fingerprint)
            {
                Finding::ended("activity_resumed")
            } else if gated || self.native_prompt_waiting(session_id)? {
                Finding::Suspended { uncertain: false }
            } else {
                Finding::Holds {
                    ready: at_least(&accepted_at, self.now, QUIET_TURN_AGE_SECONDS),
                    evidence: serde_json::json!({
                        "accepted_hook_event_id":accepted_hook_id,
                        "accepted_at":accepted_at,
                        "turn_age_threshold_seconds":QUIET_TURN_AGE_SECONDS,
                        "quiet_threshold_seconds":QUIET_TURN_UNCHANGED_SECONDS,
                    }),
                    fingerprint,
                    unchanged_seconds: Some(QUIET_TURN_UNCHANGED_SECONDS),
                }
            }
        } else {
            Finding::Suspended { uncertain: false }
        };
        Ok((subject.bound(binding), finding))
    }

    fn observe_attempts_without_live_session(&self) -> Result<()> {
        let kind = ObservationKind::AttemptWithoutLiveSession;
        let waiting = crate::coordinator::attempts_waiting_on_manager_turn(self.transaction)?;
        let mut subjects = waiting.iter().cloned().collect::<BTreeSet<_>>();
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| row.attempt_id),
        );
        for attempt_id in subjects {
            let (subject, finding) =
                self.attempt_without_live_session(&attempt_id, waiting.contains(&attempt_id))?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    fn attempt_without_live_session(
        &self,
        attempt_id: &str,
        waiting_on_manager: bool,
    ) -> Result<(ObservationSubject, Finding)> {
        let subject = ObservationSubject::new(
            ObservationKind::AttemptWithoutLiveSession,
            attempt_id.to_owned(),
        );
        let attempt: Option<(String, String, String, bool, bool)> = self
            .transaction
            .query_row(
                &format!(
                    "SELECT t.id,a.status,a.phase,{OBSERVED_CURRENT_ATTEMPT_SQL},
                            EXISTS(SELECT 1 FROM role_generations reserved WHERE reserved.attempt_id=a.id
                              AND reserved.role='manager' AND reserved.status='launch_reserved')
                     FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1"
                ),
                params![attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        let Some((task_id, status, phase, current_attempt, manager_reserved)) = attempt else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let binding = ObservationBinding {
            task_id: Some(task_id),
            attempt_id: Some(attempt_id.to_owned()),
            role: Some(RoleKind::Manager.to_string()),
            ..ObservationBinding::default()
        };
        if !current_attempt {
            return Ok((subject.bound(binding), Finding::ended("attempt_retired")));
        }
        if status != "running" {
            return Ok((
                subject.bound(binding),
                Finding::ended("attempt_not_running"),
            ));
        }
        if !waiting_on_manager {
            return Ok((
                subject.bound(binding),
                Finding::ended("session_not_required"),
            ));
        }
        if matches!(
            self.processes,
            crate::supervisor::ProcessSnapshot::Unavailable
        ) {
            return Ok((
                subject.bound(binding),
                Finding::Suspended { uncertain: true },
            ));
        }
        let sessions: Vec<(String, String, String, Option<String>, String, bool)> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT s.id,s.role_generation_id,s.transcript_epoch,s.process_identity_json,s.status,
                        s.exit_json IS NOT NULL
                 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE rg.attempt_id=?1 AND {OBSERVED_EFFECTIVE_GENERATION_SQL} ORDER BY s.id"
            ))?;
            let rows = statement
                .query_map(params![attempt_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut uncertain = false;
        for (session_id, generation_id, epoch, process_json, session_status, exit_recorded) in
            &sessions
        {
            let live =
                self.invocation_live(session_id, generation_id, epoch, process_json.as_deref());
            if live == Some(true) {
                return Ok((
                    subject.bound(binding),
                    Finding::ended("effective_session_live"),
                ));
            }
            let confirmed_ended =
                (session_status == "exited" && *exit_recorded) || session_status == "launch_failed";
            uncertain |= !confirmed_ended;
        }
        let finding = if uncertain {
            Finding::Suspended { uncertain: true }
        } else if manager_reserved {
            Finding::Suspended { uncertain: false }
        } else {
            Finding::Holds {
                fingerprint: phase.clone(),
                evidence: serde_json::json!({
                    "phase":phase,
                    "threshold_seconds":NO_LIVE_SESSION_SECONDS,
                }),
                ready: true,
                unchanged_seconds: Some(NO_LIVE_SESSION_SECONDS),
            }
        };
        Ok((subject.bound(binding), finding))
    }

    fn observe_unaccepted_guidance(&self) -> Result<()> {
        let kind = ObservationKind::GuidanceUnaccepted;
        let mut subjects: BTreeSet<(String, String, String)> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT g.id,g.delivery_session_id,g.delivery_transcript_epoch
                 FROM guidance_messages g JOIN attempts a ON a.id=g.attempt_id
                 JOIN tasks t ON t.id=a.task_id
                 WHERE g.state='written_awaiting_submit' AND g.delivery_session_id IS NOT NULL
                   AND g.delivery_transcript_epoch IS NOT NULL AND {OBSERVED_CURRENT_ATTEMPT_SQL}"
            ))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            rows
        };
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| Some((row.source_id?, row.session_id?, row.transcript_epoch?))),
        );
        for (guidance_id, session_id, epoch) in subjects {
            let (subject, finding) = self.unaccepted_guidance(&guidance_id, &session_id, &epoch)?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    fn unaccepted_guidance(
        &self,
        guidance_id: &str,
        session_id: &str,
        epoch: &str,
    ) -> Result<(ObservationSubject, Finding)> {
        type Row = (
            ObservationBinding,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            bool,
            Option<String>,
        );
        let subject = ObservationSubject::new(
            ObservationKind::GuidanceUnaccepted,
            format!("{guidance_id}:{session_id}:{epoch}"),
        );
        let row: Option<Row> = self
            .transaction
            .query_row(
                &format!(
                    "SELECT t.id,a.id,rg.role,rg.lane_id,rg.id,g.state,g.written_at,
                            g.delivery_session_id,g.delivery_transcript_epoch,rg.status,
                            {OBSERVED_CURRENT_ATTEMPT_SQL},s.transcript_epoch
                     FROM guidance_messages g JOIN role_generations rg ON rg.id=g.role_generation_id
                     JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
                     LEFT JOIN sessions s ON s.id=g.delivery_session_id
                     WHERE g.id=?1"
                ),
                params![guidance_id],
                |row| {
                    Ok((
                        ObservationBinding {
                            task_id: row.get(0)?,
                            attempt_id: row.get(1)?,
                            role: row.get(2)?,
                            lane_id: row.get(3)?,
                            role_generation_id: row.get(4)?,
                            session_id: Some(session_id.to_owned()),
                            transcript_epoch: Some(epoch.to_owned()),
                            source_id: Some(guidance_id.to_owned()),
                        },
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            binding,
            state,
            written_at,
            delivery_session,
            delivery_epoch,
            generation_status,
            current_attempt,
            current_epoch,
        )) = row
        else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let finding = if !current_attempt {
            Finding::ended("attempt_retired")
        } else if matches!(
            state.as_str(),
            "submitted" | "acknowledged" | "cancelled" | "abandoned"
        ) {
            Finding::Ended(format!("guidance_{state}"))
        } else if matches!(generation_status.as_str(), "replaced" | "revoked") {
            Finding::ended("invocation_retired")
        } else if current_epoch
            .as_deref()
            .is_some_and(|current| current != epoch)
        {
            Finding::ended("invocation_retired")
        } else if delivery_session
            .as_deref()
            .is_some_and(|delivery| delivery != session_id)
            || delivery_epoch
                .as_deref()
                .is_some_and(|delivery| delivery != epoch)
        {
            Finding::ended("delivery_replaced")
        } else if state != "written_awaiting_submit"
            || current_epoch.is_none()
            || delivery_session.is_none()
            || delivery_epoch.is_none()
        {
            Finding::Suspended { uncertain: true }
        } else if let Some(written_at) = written_at {
            Finding::Holds {
                ready: at_least(
                    &written_at,
                    self.now,
                    crate::workflow::ACCEPTANCE_OBSERVATION_SECONDS,
                ),
                evidence: serde_json::json!({
                    "written_at":written_at,
                    "threshold_seconds":crate::workflow::ACCEPTANCE_OBSERVATION_SECONDS,
                }),
                fingerprint: written_at,
                unchanged_seconds: None,
            }
        } else {
            Finding::Suspended { uncertain: false }
        };
        Ok((subject.bound(binding), finding))
    }

    fn observe_permissions_on_exited_sessions(&self) -> Result<()> {
        let kind = ObservationKind::PermissionOnExitedSession;
        let mut subjects: BTreeSet<String> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT p.id FROM permission_requests p JOIN sessions s ON s.id=p.session_id
                 JOIN attempts a ON a.id=p.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE p.state='pending' AND s.status='exited'
                   AND {OBSERVED_CURRENT_ATTEMPT_SQL}
                   AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                                  WHERE native.permission_request_id=p.id)"
            ))?;
            let rows = statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            rows
        };
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| row.source_id),
        );
        for request_id in subjects {
            let (subject, finding) = self.permission_on_exited_session(&request_id)?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    fn permission_on_exited_session(
        &self,
        request_id: &str,
    ) -> Result<(ObservationSubject, Finding)> {
        type Row = (ObservationBinding, String, bool, String, bool, bool, bool);
        let subject = ObservationSubject::new(
            ObservationKind::PermissionOnExitedSession,
            request_id.to_owned(),
        );
        let row: Option<Row> = self
            .transaction
            .query_row(
                &format!(
                    "SELECT t.id,a.id,p.role,rg.lane_id,p.role_generation_id,p.session_id,
                            s.transcript_epoch,p.state,
                            EXISTS(SELECT 1 FROM permission_native_resolutions native
                                   WHERE native.permission_request_id=p.id),
                            s.status,s.exit_json IS NOT NULL,
                            s.role_generation_id=p.role_generation_id AND {OBSERVED_EFFECTIVE_GENERATION_SQL},
                            {OBSERVED_CURRENT_ATTEMPT_SQL}
                     FROM permission_requests p JOIN sessions s ON s.id=p.session_id
                     JOIN role_generations rg ON rg.id=p.role_generation_id
                     JOIN attempts a ON a.id=p.attempt_id JOIN tasks t ON t.id=a.task_id
                     WHERE p.id=?1"
                ),
                params![request_id],
                |row| {
                    Ok((
                        ObservationBinding {
                            task_id: row.get(0)?,
                            attempt_id: row.get(1)?,
                            role: row.get(2)?,
                            lane_id: row.get(3)?,
                            role_generation_id: row.get(4)?,
                            session_id: row.get(5)?,
                            transcript_epoch: row.get(6)?,
                            source_id: Some(request_id.to_owned()),
                        },
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
            binding,
            state,
            natively_resolved,
            session_status,
            exit_recorded,
            same_binding,
            current_attempt,
        )) = row
        else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let prior_epoch: Option<String> = self.transaction.query_row(
            "SELECT transcript_epoch FROM attention_observations WHERE kind='permission_on_exited_session'
               AND entity_key=?1 AND state IN ('candidate','open')", params![request_id], |row| row.get(0),
        ).optional()?.flatten();
        let finding = if !current_attempt {
            Finding::ended("attempt_retired")
        } else if state != "pending" || natively_resolved {
            Finding::ended("request_settled")
        } else if !same_binding
            || prior_epoch
                .as_deref()
                .is_some_and(|prior| binding.transcript_epoch.as_deref() != Some(prior))
        {
            Finding::ended("binding_superseded")
        } else if session_status != "exited" || !exit_recorded {
            Finding::Suspended { uncertain: false }
        } else {
            Finding::Holds {
                fingerprint: request_id.to_owned(),
                evidence: serde_json::json!({"session_status":session_status}),
                ready: true,
                unchanged_seconds: None,
            }
        };
        Ok((subject.bound(binding), finding))
    }

    fn observe_resume_failures_after_acceptance(&self) -> Result<()> {
        let kind = ObservationKind::ResumeFailureAfterAcceptance;
        let mut subjects: BTreeSet<String> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT rejection.id FROM tasks t JOIN attempts a ON a.task_id=t.id
                 JOIN role_generations rg ON rg.attempt_id=a.id
                 JOIN sessions s ON s.role_generation_id=rg.id
                 JOIN audit_events rejection ON rejection.entity_kind='session'
                   AND rejection.entity_id=s.id AND rejection.event_code='session.resume.rejected'
                   AND json_extract(rejection.detail_json,'$.attempt_id')=a.id
                 WHERE t.attention='resume_failed' AND {OBSERVED_CURRENT_ATTEMPT_SQL}"
            ))?;
            let rows = statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            rows
        };
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| row.source_id),
        );
        for rejection_id in subjects {
            let (subject, finding) = self.resume_failure_after_acceptance(&rejection_id)?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    /// The causal conditions of `reconcile_accepted_resume_turn`, read without its mutations.
    fn resume_failure_after_acceptance(
        &self,
        rejection_id: &str,
    ) -> Result<(ObservationSubject, Finding)> {
        type Row = (
            ObservationBinding,
            String,
            String,
            bool,
            Option<String>,
            bool,
            bool,
            bool,
        );
        let subject = ObservationSubject::new(
            ObservationKind::ResumeFailureAfterAcceptance,
            rejection_id.to_owned(),
        );
        let row: Option<Row> = self
            .transaction
            .query_row(
                &format!(
                    "SELECT t.id,a.id,json_extract(e.detail_json,'$.role'),
                            json_extract(e.detail_json,'$.lane_id'),
                            json_extract(e.detail_json,'$.role_generation_id'),e.entity_id,
                            s.transcript_epoch,
                            t.attention,a.status,{OBSERVED_CURRENT_ATTEMPT_SQL},
                            (SELECT reconciled.id FROM audit_events reconciled,
                                    json_each(reconciled.detail_json,'$.rejection_event_ids') listed
                             JOIN resume_invocations ri ON ri.id=json_extract(reconciled.detail_json,'$.resume_invocation_id')
                             JOIN hook_events accepted ON accepted.id=json_extract(reconciled.detail_json,'$.superseding_hook_event_id')
                             WHERE reconciled.event_code='session.resume.turn_reconciled'
                               AND reconciled.entity_kind='session' AND reconciled.entity_id=s.id
                               AND json_extract(reconciled.detail_json,'$.role_generation_id')=rg.id
                               AND json_extract(reconciled.detail_json,'$.transcript_epoch')=s.transcript_epoch
                               AND ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                               AND ri.prior_transcript_epoch=json_extract(e.detail_json,'$.transcript_epoch')
                               AND ri.resume_ordinal-1=CAST(json_extract(e.detail_json,'$.resume_count') AS INTEGER)
                               AND julianday(e.created_at)<=julianday(ri.created_at)
                               AND accepted.session_id=s.id AND accepted.role_generation_id=rg.id
                               AND accepted.event_name='UserPromptSubmit'
                               AND listed.value=e.id ORDER BY reconciled.rowid DESC LIMIT 1),
                            (EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=a.id
                               AND c.kind!='transition_proposal'
                               AND c.state NOT IN ('finished','cancelled','superseded','rejected','failed','abandoned'))
                             OR EXISTS(SELECT 1 FROM restart_candidates rc WHERE rc.attempt_id=a.id
                               AND rc.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
                             OR EXISTS(SELECT 1 FROM recovery_records r WHERE r.attempt_id=a.id
                               AND r.state='attention_required')
                             OR EXISTS(SELECT 1 FROM rework_intents rw WHERE rw.new_attempt_id=a.id
                               AND rw.state NOT IN ('completed','cancelled','failed'))
                             OR EXISTS(SELECT 1 FROM switch_intents si WHERE si.attempt_id=a.id
                               AND si.state NOT IN ('completed','cancelled','superseded'))),
                            EXISTS(SELECT 1 FROM audit_events other JOIN sessions other_session ON other_session.id=other.entity_id
                              WHERE other.event_code='session.resume.rejected' AND other.entity_kind='session'
                                AND json_extract(other.detail_json,'$.attempt_id')=a.id AND other.id!=e.id
                                AND other_session.transcript_epoch=json_extract(other.detail_json,'$.transcript_epoch')
                                AND other_session.resume_count=CAST(json_extract(other.detail_json,'$.resume_count') AS INTEGER)
                                AND NOT EXISTS(SELECT 1 FROM audit_events route
                                  WHERE route.event_code='session.resume.fresh_route.reserved'
                                    AND json_extract(route.detail_json,'$.rejection_event_id')=other.id)
                                AND NOT EXISTS(SELECT 1 FROM audit_events reconciled,
                                    json_each(reconciled.detail_json,'$.rejection_event_ids') listed
                                  WHERE reconciled.event_code='session.resume.turn_reconciled'
                                    AND listed.value=other.id)),({OBSERVED_EFFECTIVE_GENERATION_SQL}
                                      AND s.role_generation_id=json_extract(e.detail_json,'$.role_generation_id'))
                     FROM audit_events e
                     JOIN attempts a ON a.id=json_extract(e.detail_json,'$.attempt_id')
                     JOIN tasks t ON t.id=a.task_id
                     JOIN sessions s ON s.id=e.entity_id
                     JOIN role_generations rg ON rg.id=s.role_generation_id
                     WHERE e.id=?1 AND e.event_code='session.resume.rejected'"
                ),
                params![rejection_id],
                |row| {
                    Ok((
                        ObservationBinding {
                            task_id: row.get(0)?,
                            attempt_id: row.get(1)?,
                            role: row.get(2)?,
                            lane_id: row.get(3)?,
                            role_generation_id: row.get(4)?,
                            session_id: row.get(5)?,
                            transcript_epoch: row.get(6)?,
                            source_id: Some(rejection_id.to_owned()),
                        },
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            binding,
            attention,
            attempt_status,
            current_attempt,
            reconciled,
            independent_hold,
            other_rejection,
            effective,
        )) = row
        else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let prior_epoch: Option<String> = self.transaction.query_row(
            "SELECT transcript_epoch FROM attention_observations WHERE kind='resume_failure_after_acceptance'
               AND entity_key=?1 AND state IN ('candidate','open')", params![rejection_id], |row| row.get(0),
        ).optional()?.flatten();
        let provider_hold = match binding.session_id.as_deref() {
            Some(session_id) => {
                provider_failure_hold_for_session(self.transaction, session_id)?.is_some()
            }
            None => false,
        };
        let finding = if !current_attempt {
            Finding::ended("attempt_retired")
        } else if !effective {
            Finding::ended("role_replaced")
        } else if prior_epoch
            .as_deref()
            .is_some_and(|prior| binding.transcript_epoch.as_deref() != Some(prior))
        {
            Finding::ended("invocation_replaced")
        } else if attention != "resume_failed" {
            Finding::ended("hold_reconciled")
        } else if independent_hold {
            Finding::ended("independent_hold")
        } else if provider_hold {
            if self
                .stored(subject.kind, &subject.entity_key)?
                .is_some_and(|stored| stored.state == "open")
            {
                Finding::ended("provider_failure_hold")
            } else {
                Finding::Suspended { uncertain: false }
            }
        } else if other_rejection {
            Finding::ended("other_rejection_binds")
        } else if attempt_status != "needs_input" {
            Finding::ended("attempt_not_waiting_on_resume")
        } else if let Some(reconciled) = reconciled {
            let accepted_current: bool = self.transaction.query_row(
                &format!("WITH {} SELECT EXISTS(SELECT 1 FROM current_hooks current
                    JOIN audit_events reconciled ON reconciled.id=?2
                    WHERE current.id=json_extract(reconciled.detail_json,'$.superseding_hook_event_id')
                      AND current.event_name='UserPromptSubmit')", crate::workflow::CURRENT_TURN_HOOKS_SQL),
                params![binding.session_id, reconciled], |row| row.get(0),
            )?;
            if accepted_current {
                Finding::Holds {
                    evidence: serde_json::json!({"turn_reconciled_event_id":reconciled}),
                    fingerprint: reconciled,
                    ready: true,
                    unchanged_seconds: None,
                }
            } else {
                Finding::Suspended { uncertain: false }
            }
        } else {
            Finding::Suspended { uncertain: false }
        };
        Ok((subject.bound(binding), finding))
    }

    fn observe_busy_after_exit(&self) -> Result<()> {
        let kind = ObservationKind::BusyAfterExit;
        let mut subjects: BTreeSet<(String, String)> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT s.id,s.transcript_epoch FROM sessions s
                 JOIN role_generations rg ON rg.id=s.role_generation_id
                 JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                 WHERE s.status='exited' AND s.readiness_state IN ('busy','busy_unresolved_hook_work')
                   AND json_type(s.exit_json,'$.app_stop_requested') IN ('true','false')
                   AND {OBSERVED_CURRENT_ATTEMPT_SQL}"
            ))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<BTreeSet<_>>>()?;
            rows
        };
        subjects.extend(
            self.unresolved(kind)?
                .into_iter()
                .filter_map(|row| Some((row.session_id?, row.transcript_epoch?))),
        );
        for (session_id, epoch) in subjects {
            let (subject, finding) = self.busy_after_exit(&session_id, &epoch)?;
            self.advance(subject, finding)?;
        }
        Ok(())
    }

    fn busy_after_exit(
        &self,
        session_id: &str,
        epoch: &str,
    ) -> Result<(ObservationSubject, Finding)> {
        type Row = (
            ObservationBinding,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            bool,
            bool,
            Option<String>,
            bool,
            bool,
            bool,
        );
        let subject = ObservationSubject::new(
            ObservationKind::BusyAfterExit,
            format!("{session_id}:{epoch}"),
        );
        let row: Option<Row> = self
            .transaction
            .query_row(
                &format!(
                    "WITH {}
                     SELECT t.id,a.id,rg.role,rg.lane_id,rg.id,s.transcript_epoch,s.status,
                            s.readiness_state,json_type(s.exit_json,'$.app_stop_requested'),
                            json_extract(s.exit_json,'$.observed_at'),
                            {OBSERVED_CURRENT_ATTEMPT_SQL},{OBSERVED_EFFECTIVE_GENERATION_SQL},
                            (SELECT id FROM accepted),
                            EXISTS(SELECT 1 FROM role_results report WHERE report.session_id=s.id
                              AND report.role_generation_id=s.role_generation_id
                              AND julianday(report.created_at)>=julianday((SELECT received_at FROM accepted))
                              AND NOT EXISTS(SELECT 1 FROM role_result_supersessions retired WHERE retired.role_result_id=report.id)
                              AND NOT EXISTS(SELECT 1 FROM audit_events retired WHERE retired.event_code='role_result.superseded'
                                AND retired.entity_kind='role_result' AND retired.entity_id=report.id)),
                            EXISTS(SELECT 1 FROM audit_events stop
                              WHERE stop.event_code='manager.service_stop.claimed'
                                AND stop.entity_kind='session' AND stop.entity_id=s.id
                                AND json_extract(stop.detail_json,'$.role_generation_id')=s.role_generation_id
                                AND json_extract(stop.detail_json,'$.transcript_epoch')=s.transcript_epoch),
                            EXISTS(SELECT 1 FROM recovery_records recovery WHERE recovery.session_id=s.id
                              AND json_extract(recovery.detail_json,'$.kind')='graceful_stop_deadline'
                              AND json_extract(recovery.detail_json,'$.role_generation_id')=rg.id
                              AND json_extract(recovery.detail_json,'$.transcript_epoch')=s.transcript_epoch
                              AND recovery.process_identity_json=s.process_identity_json)
                              OR EXISTS(SELECT 1 FROM restart_candidates restart WHERE restart.session_id=s.id
                                AND restart.attempt_id=a.id
                                AND json_extract(restart.result_json,'$.restart.admission.role_generation_id')=rg.id
                                AND json_extract(restart.result_json,'$.restart.admission.prior_transcript_epoch')=s.transcript_epoch)
                     FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
                     JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
                     WHERE s.id=?1",
                    crate::workflow::CURRENT_TURN_HOOKS_SQL
                ),
                params![session_id],
                |row| {
                    Ok((
                        ObservationBinding {
                            task_id: row.get(0)?,
                            attempt_id: row.get(1)?,
                            role: row.get(2)?,
                            lane_id: row.get(3)?,
                            role_generation_id: row.get(4)?,
                            session_id: Some(session_id.to_owned()),
                            transcript_epoch: Some(epoch.to_owned()),
                            source_id: None,
                        },
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
                    ))
                },
            )
            .optional()?;
        let Some((
            binding,
            current_epoch,
            status,
            readiness,
            stop_flag,
            exited_at,
            current_attempt,
            effective,
            accepted,
            reported,
            service_stop_claimed,
            recovery_bound,
        )) = row
        else {
            return Ok((subject, Finding::Suspended { uncertain: false }));
        };
        let finding = if !current_attempt {
            Finding::ended("attempt_retired")
        } else if !effective {
            Finding::ended("role_replaced")
        } else if current_epoch != epoch {
            Finding::ended("invocation_replaced")
        } else if status != "exited" {
            Finding::Suspended { uncertain: false }
        } else if !matches!(readiness.as_str(), "busy" | "busy_unresolved_hook_work") {
            Finding::ended("readiness_corrected")
        } else if recovery_bound {
            Finding::ended("recovery_explains_exit")
        } else if reported {
            Finding::ended("report_recorded")
        } else if stop_flag.as_deref() == Some("true") || service_stop_claimed {
            Finding::ended("app_stop_requested")
        } else if stop_flag.as_deref() != Some("false") || accepted.is_none() {
            // Historical exits lost stop intent and cannot establish this disagreement.
            Finding::Suspended { uncertain: false }
        } else {
            Finding::Holds {
                fingerprint: epoch.to_owned(),
                evidence: serde_json::json!({"readiness":readiness,"exited_at":exited_at}),
                ready: true,
                unchanged_seconds: None,
            }
        };
        Ok((subject.bound(binding), finding))
    }

    fn reconcile_blocking_episodes(&self) -> Result<()> {
        let attempts: Vec<String> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT a.id FROM attempts a JOIN tasks t ON t.id=a.task_id
                 WHERE {OBSERVED_CURRENT_ATTEMPT_SQL}
                   OR EXISTS(SELECT 1 FROM attention_observations observation
                     WHERE observation.attempt_id=a.id AND observation.kind='recurring_block'
                       AND observation.state='open') ORDER BY a.id"
            ))?;
            let rows = statement
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for attempt_id in attempts {
            reconcile_audited_blocking_episodes(
                self.transaction,
                &attempt_id,
                None,
                None,
                &self.now_text,
            )?;
        }
        Ok(())
    }

    fn observe_recurring_blocks(&self) -> Result<()> {
        type Row = (
            String,
            String,
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
        );
        let rows: Vec<Row> = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT observation.id,observation.state,observation.attempt_id,observation.role,
                        observation.lane_id,observation.entity_key,observation.recurrence_count,
                        observation.last_counted_result_rowid,observation.reset_watermark_rowid
                 FROM attention_observations observation
                 LEFT JOIN attempts a ON a.id=observation.attempt_id LEFT JOIN tasks t ON t.id=a.task_id
                 WHERE observation.kind='recurring_block'
                   AND (observation.state='open' OR
                        (observation.recurrence_count>0 AND {OBSERVED_CURRENT_ATTEMPT_SQL}))
                   AND observation.attempt_id IS NOT NULL AND observation.role IS NOT NULL
                   AND observation.lane_id IS NOT NULL ORDER BY observation.id"
            ))?;
            let rows = statement
                .query_map([], |row| {
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
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, state, attempt_id, role, lane_id, entity_key, count, counted_rowid, reset_rowid) in
            rows
        {
            let outcome = entity_key.rsplit(':').next().unwrap_or_default().to_owned();
            let mut count = count;
            let mut reset = false;
            if count > 0 {
                let after = counted_rowid.max(reset_rowid).unwrap_or_default();
                if let Some(progress) =
                    published_progress(self.transaction, &attempt_id, &role, &lane_id, after, None)?
                {
                    record_progress_reset(self.transaction, &id, &progress, &self.now_text)?;
                    count = 0;
                    reset = true;
                }
            }
            let current_attempt: Option<bool> = self
                .transaction
                .query_row(
                    &format!(
                        "SELECT {OBSERVED_CURRENT_ATTEMPT_SQL} FROM attempts a
                         JOIN tasks t ON t.id=a.task_id WHERE a.id=?1"
                    ),
                    params![attempt_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(current_attempt) = current_attempt else {
                continue;
            };
            let retired = !current_attempt;
            let binding = if retired {
                Some(false)
            } else {
                current_block(self.transaction, &attempt_id, &role, &lane_id, &outcome)?
            };
            if count == RECURRENCE_CAP && binding.is_none() {
                continue;
            }
            let visible = !retired && count == RECURRENCE_CAP && binding == Some(true);
            match (state.as_str(), visible) {
                ("open", false) => {
                    let reason = if retired {
                        "attempt_retired"
                    } else if reset {
                        "progress_published"
                    } else {
                        "block_ended"
                    };
                    self.transaction.execute(
                        "UPDATE attention_observations SET state='resolved',resolved_at=?1,
                                resolution_reason=?2,last_seen_at=?1
                         WHERE id=?3",
                        params![self.now_text, reason, id],
                    )?;
                    self.audit(
                        &id,
                        "attention.observation.resolved",
                        serde_json::json!({"kind":"recurring_block","entity_key":entity_key,"reason":reason}),
                    )?;
                }
                ("candidate" | "resolved", true) => {
                    self.transaction.execute(
                        "UPDATE attention_observations SET state='open',opened_at=?1,resolved_at=NULL,
                                resolution_reason=NULL,last_seen_at=?1
                         WHERE id=?2",
                        params![self.now_text, id],
                    )?;
                    self.audit(
                        &id,
                        "attention.observation.opened",
                        serde_json::json!({"kind":"recurring_block","entity_key":entity_key}),
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// A consumed plan or candidate report of this exact attempt, role and lane after
/// `after` (and before `before`), proven by the freeze that published it. Duplicate
/// and role-switch retirement also set `consumed_at` but complete no freeze.
fn published_progress(
    connection: &Connection,
    attempt_id: &str,
    role: &str,
    lane_id: &str,
    after: i64,
    before: Option<i64>,
) -> Result<Option<(String, i64, String)>> {
    Ok(connection
        .query_row(
            "SELECT progress.id,progress.rowid,freeze.id FROM role_results progress
             JOIN role_generations rg ON rg.id=progress.role_generation_id
             JOIN freeze_intents freeze ON freeze.attempt_id=rg.attempt_id
               AND freeze.source_role_generation_id=progress.role_generation_id
               AND freeze.kind=CASE progress.outcome WHEN 'plan_ready' THEN 'plan' ELSE 'candidate' END
               AND freeze.state='complete' AND freeze.updated_at=progress.consumed_at
             JOIN snapshots published ON published.id=freeze.result_snapshot_id
               AND published.attempt_id=freeze.attempt_id AND published.kind=freeze.kind
               AND published.complete=1
             WHERE rg.attempt_id=?1 AND rg.role=?2 AND rg.lane_id=?3
               AND progress.outcome IN ('plan_ready','candidate_ready')
               AND progress.consumed_at IS NOT NULL AND progress.rowid>?4
               AND (?5 IS NULL OR progress.rowid<?5)
               AND NOT EXISTS(SELECT 1 FROM role_result_supersessions superseded
                              WHERE superseded.role_result_id=progress.id)
               AND NOT EXISTS(SELECT 1 FROM audit_events retired
                 WHERE retired.event_code='role_result.superseded'
                   AND retired.entity_kind='role_result' AND retired.entity_id=progress.id)
             ORDER BY progress.rowid DESC LIMIT 1",
            params![attempt_id, role, lane_id, after, before],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

fn record_progress_reset(
    transaction: &Transaction<'_>,
    observation_id: &str,
    (result_id, result_rowid, freeze_intent_id): &(String, i64, String),
    now: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE attention_observations SET recurrence_count=0,reset_watermark_rowid=?1,
                reset_evidence_json=?2,last_seen_at=?3
         WHERE id=?4",
        params![
            result_rowid,
            serde_json::json!({
                "progress_result_id":result_id,
                "progress_result_rowid":result_rowid,
                "freeze_intent_id":freeze_intent_id,
            })
            .to_string(),
            now,
            observation_id
        ],
    )?;
    Ok(())
}

/// Whether the attempt is still held by a report of this exact role, lane and
/// outcome from that role's current binding; missing source evidence is unknown.
fn current_block(
    connection: &Connection,
    attempt_id: &str,
    role: &str,
    lane_id: &str,
    outcome: &str,
) -> Result<Option<bool>> {
    let reason = if outcome == "needs_input" {
        "role_needs_input"
    } else {
        "role_blocked"
    };
    let source: Option<(String, String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT t.attention,a.status,json_extract(hold.detail_json,'$.reason'),held.id
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         LEFT JOIN audit_events hold ON hold.id=(SELECT latest.id FROM audit_events latest
             WHERE latest.event_code='attempt.attention.changed' AND latest.entity_kind='attempt'
               AND latest.entity_id=a.id ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
         LEFT JOIN role_results held ON held.id=json_extract(hold.detail_json,'$.result_id')
         WHERE a.id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((attention, status, hold_reason, held_result)) = source else {
        return Ok(None);
    };
    if attention != "needs_input" || status != "needs_input" {
        return Ok(Some(false));
    }
    if hold_reason.is_none() || (hold_reason.as_deref() == Some(reason) && held_result.is_none()) {
        return Ok(None);
    }
    Ok(Some(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
           JOIN audit_events hold ON hold.id=(
             SELECT latest.id FROM audit_events latest
             WHERE latest.event_code='attempt.attention.changed' AND latest.entity_kind='attempt'
               AND latest.entity_id=a.id
             ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
           JOIN role_results held ON held.id=json_extract(hold.detail_json,'$.result_id')
             AND held.role_generation_id=json_extract(hold.detail_json,'$.role_generation_id')
           JOIN role_generations rg ON rg.id=held.role_generation_id AND rg.attempt_id=a.id
           WHERE a.id=?1 AND t.attention='needs_input' AND a.status='needs_input'
             AND hold.created_at>=a.updated_at
             AND json_extract(hold.detail_json,'$.attention')='needs_input'
             AND json_extract(hold.detail_json,'$.reason')=?5
             AND rg.role=?2 AND rg.lane_id=?3 AND held.outcome=?4
             AND rg.status NOT IN ('replaced','revoked')
             AND NOT EXISTS(SELECT 1 FROM role_generations newer
               WHERE newer.attempt_id=rg.attempt_id AND newer.role=rg.role
                 AND newer.lane_id=rg.lane_id AND newer.generation>rg.generation))",
        params![attempt_id, role, lane_id, outcome, reason],
        |row| row.get(0),
    )?))
}

/// Counts this effective hold and earlier uncounted holds inside the transaction
/// consuming `result_id`; source watermarks prevent replays from counting again.
pub(crate) fn count_blocking_episode(
    connection: &Connection,
    result_id: &str,
    now: &str,
) -> Result<()> {
    if connection.is_autocommit() {
        bail!("blocking observations require the effective hold transaction")
    }
    let (attempt_id, role, lane_id, outcome, result_rowid): (String, String, String, String, i64) =
        connection.query_row(
            "SELECT rg.attempt_id,rg.role,rg.lane_id,rr.outcome,rr.rowid
         FROM role_results rr JOIN role_generations rg ON rg.id=rr.role_generation_id
         WHERE rr.id=?1",
            params![result_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    reconcile_audited_blocking_episodes(
        connection,
        &attempt_id,
        Some((&role, &lane_id, &outcome)),
        Some(result_rowid),
        now,
    )
}

/// The hold audit, not consumption alone, proves an effective episode. A later
/// successful count folds older pending episodes before advancing its watermark.
fn reconcile_audited_blocking_episodes(
    connection: &Connection,
    attempt_id: &str,
    key: Option<(&str, &str, &str)>,
    through: Option<i64>,
    now: &str,
) -> Result<()> {
    type Episode = (String, i64, String);
    let mut by_key: BTreeMap<(String, String, String), Vec<Episode>> = BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT rg.role,rg.lane_id,rr.outcome,rr.id,rr.rowid,rg.id
             FROM audit_events hold INDEXED BY audit_events_entity
             JOIN role_results rr ON rr.id=json_extract(hold.detail_json,'$.result_id')
             JOIN role_generations rg ON rg.id=rr.role_generation_id
             LEFT JOIN attention_observations observation
               ON observation.entity_kind='role_lane' AND observation.kind='recurring_block'
               AND observation.entity_key=rg.attempt_id||':'||rg.role||':'||rg.lane_id||':'||rr.outcome
             WHERE hold.entity_kind='attempt' AND hold.entity_id=?1
               AND hold.event_code='attempt.attention.changed'
               AND json_extract(hold.detail_json,'$.attention')='needs_input'
               AND json_extract(hold.detail_json,'$.role_generation_id')=rg.id
               AND json_extract(hold.detail_json,'$.reason')=
                   CASE rr.outcome WHEN 'blocked' THEN 'role_blocked' ELSE 'role_needs_input' END
               AND rg.attempt_id=?1 AND rr.outcome IN ('blocked','needs_input')
               AND rr.consumed_at IS NOT NULL AND hold.created_at=rr.consumed_at
               AND (?2 IS NULL OR (rg.role=?2 AND rg.lane_id=?3 AND rr.outcome=?4))
               AND rr.rowid>MAX(COALESCE(observation.last_counted_result_rowid,0),
                               COALESCE(observation.reset_watermark_rowid,0))
               AND (?5 IS NULL OR rr.rowid<=?5)
             ORDER BY rr.rowid",
        )?;
        let rows = statement.query_map(
            params![
                attempt_id,
                key.map(|key| key.0),
                key.map(|key| key.1),
                key.map(|key| key.2),
                through
            ],
            |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ),
                    (
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ),
                ))
            },
        )?;
        for row in rows {
            let (key, episode) = row?;
            by_key.entry(key).or_default().push(episode);
        }
    }
    for ((role, lane_id, outcome), episodes) in by_key {
        let entity_key = format!("{attempt_id}:{role}:{lane_id}:{outcome}");
        let stored: Option<(String, i64, Option<i64>, Option<i64>)> = connection
            .query_row(
                "SELECT id,recurrence_count,last_counted_result_rowid,reset_watermark_rowid
             FROM attention_observations WHERE entity_kind='role_lane'
               AND entity_key=?1 AND kind='recurring_block'",
                params![entity_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let mut count = stored.as_ref().map_or(0, |row| row.1);
        let mut watermark = stored
            .as_ref()
            .and_then(|row| row.2.max(row.3))
            .unwrap_or_default();
        let mut reset: Option<(String, i64, String)> = None;
        for (_, rowid, _) in &episodes {
            if let Some(progress) = published_progress(
                connection,
                attempt_id,
                &role,
                &lane_id,
                watermark,
                Some(*rowid),
            )? {
                count = 0;
                reset = Some(progress);
            }
            count = (count + 1).min(RECURRENCE_CAP);
            watermark = *rowid;
        }
        let (result_id, result_rowid, generation_id) = episodes
            .last()
            .ok_or_else(|| anyhow!("audited episode group is empty"))?;
        let reset_rowid = reset.as_ref().map(|progress| progress.1);
        let reset_evidence = reset.as_ref().map(|(id, rowid, freeze)| {
            serde_json::json!({
                "progress_result_id":id,"progress_result_rowid":rowid,"freeze_intent_id":freeze,
            })
            .to_string()
        });
        if let Some((id, _, _, _)) = stored {
            connection.execute(
                "UPDATE attention_observations SET recurrence_count=?1,last_counted_result_id=?2,
                    last_counted_result_rowid=?3,evidence_fingerprint=?2,role_generation_id=?4,
                    last_seen_at=?5,reset_watermark_rowid=COALESCE(?7,reset_watermark_rowid),
                    reset_evidence_json=CASE WHEN ?7 IS NULL THEN reset_evidence_json ELSE ?8 END
                 WHERE id=?6",
                params![
                    count,
                    result_id,
                    result_rowid,
                    generation_id,
                    now,
                    id,
                    reset_rowid,
                    reset_evidence
                ],
            )?;
        } else {
            let task_id: String = connection.query_row(
                "SELECT task_id FROM attempts WHERE id=?1",
                params![attempt_id],
                |row| row.get(0),
            )?;
            connection.execute(
                "INSERT INTO attention_observations(id,entity_kind,entity_key,kind,task_id,attempt_id,
                    role,lane_id,role_generation_id,state,evidence_fingerprint,evidence_json,
                    first_seen_at,last_seen_at,observed_since,recurrence_count,last_counted_result_id,
                    last_counted_result_rowid,reset_watermark_rowid,reset_evidence_json)
                 VALUES(?1,'role_lane',?2,'recurring_block',?3,?4,?5,?6,?7,'candidate',?8,?9,?10,?10,?10,
                        ?11,?8,?12,?13,?14)", params![uuid::Uuid::new_v4().to_string(),entity_key,task_id,
                    attempt_id,role,lane_id,generation_id,result_id,serde_json::json!({"outcome":outcome}).to_string(),
                    now,count,result_rowid,reset_rowid,reset_evidence],
            )?;
        }
    }
    Ok(())
}

struct AcceptedResumeAuthority {
    resume_invocation_id: String,
    resume_ordinal: i64,
    prior_transcript_epoch: Option<String>,
    reserved_at: String,
    native_session_id: String,
    settings_revision: i64,
    attempt_id: String,
    task_id: String,
    task_version: i64,
    attention: String,
    attempt_status: String,
}

fn require_current_restart_admission(
    transaction: &Transaction<'_>,
    session_id: &str,
    admission: RestartAdmissionBinding<'_>,
) -> Result<()> {
    let candidate: Option<(String, String)> = transaction
        .query_row(
            "SELECT rc.state,rc.result_json FROM restart_candidates rc
             JOIN sessions s ON s.id=rc.session_id
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE rc.session_id=?1 AND rc.attempt_id=?2 AND rc.task_id=?3
               AND a.id=?2 AND t.id=?3 AND rg.id=?4 AND t.version=?5
               AND s.transcript_epoch=?6 AND s.resume_count+1=?7",
            params![
                session_id,
                admission.attempt_id,
                admission.task_id,
                admission.role_generation_id,
                admission.expected_task_version,
                admission.prior_transcript_epoch,
                admission.expected_resume_ordinal
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let owned = candidate.is_some_and(|(state, result)| {
        state == "admitting"
            && RestartCandidateResult::parse(&result)
                .ok()
                .and_then(|result| result.restart.admission)
                .is_some_and(|durable| {
                    durable.id == admission.admission_id
                        && durable.attempt_id == admission.attempt_id
                        && durable.role_generation_id == admission.role_generation_id
                        && durable.expected_task_version == admission.expected_task_version
                        && durable.prior_transcript_epoch == admission.prior_transcript_epoch
                        && durable.expected_resume_ordinal == admission.expected_resume_ordinal
                })
    });
    if !owned {
        bail!("host-restart admission was superseded before its exact resume was reserved")
    }
    Ok(())
}

/// A guidance row delivered to the exact current invocation of session `?2`.
const CURRENT_GUIDANCE_DELIVERY: &str = "delivery_session_id=?2
    AND delivery_transcript_epoch=(SELECT transcript_epoch FROM sessions WHERE id=?2)
    AND delivery_resume_invocation_id IS (
      SELECT ri.id FROM resume_invocations ri JOIN sessions s ON s.id=?2
      WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
      ORDER BY ri.resume_ordinal DESC LIMIT 1)";

// A row without a digest was written before submitted forms were recorded, as its body.
fn submitted_form_intact(submitted: &str, digest: Option<&str>) -> bool {
    digest.map_or(true, |digest| {
        json_hash(&submitted).is_ok_and(|actual| actual == digest)
    })
}

/// A trusted UserPromptSubmit of an exact running resume proves the session
/// accepted a new native turn. That supersedes the session's own earlier
/// unconsumed blocked or needs_input reports and the resume rejection of the
/// exact state this resume replaced; its resume_failed hold is released only
/// when no independent hold or other still-binding rejection remains. Runs
/// inside the hook's transaction, so a failed update or later error rolls the
/// hook and every reconciliation write back together.
fn reconcile_accepted_resume_turn(
    transaction: &Transaction<'_>,
    context: &RoleContext,
    hook_event_id: &str,
    now: &str,
) -> Result<Option<serde_json::Value>> {
    let authority = transaction
        .query_row(
            "SELECT ri.id,ri.resume_ordinal,ri.prior_transcript_epoch,ri.created_at,
                    s.native_session_id,rg.config_revision,a.id,t.id,t.version,t.attention,a.status
             FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN resume_invocations ri ON ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
             JOIN attempts a ON a.id=rg.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE s.id=?1 AND rg.id=?2 AND s.transcript_epoch=?3
               AND s.status='running' AND rg.status='running' AND ri.state='running'
               AND ri.capability_key=s.capability_key AND s.native_session_id IS NOT NULL
               AND t.archived_at IS NULL
               AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id
                         ORDER BY latest.created_at DESC LIMIT 1)
               AND (EXISTS(SELECT 1 FROM role_settings rs WHERE rs.task_id=t.id AND rs.role=rg.role
                             AND rs.revision=rg.config_revision AND rs.effective_generation_id=rg.id)
                 OR (rg.role='implementer' AND rg.lane_id!='default' AND EXISTS(
                       SELECT 1 FROM lane_generations lg
                       WHERE lg.lane_id=rg.lane_id AND lg.effective_generation_id=rg.id)))",
            params![
                context.session_id,
                context.role_generation_id,
                context.transcript_epoch
            ],
            |row| {
                Ok(AcceptedResumeAuthority {
                    resume_invocation_id: row.get(0)?,
                    resume_ordinal: row.get(1)?,
                    prior_transcript_epoch: row.get(2)?,
                    reserved_at: row.get(3)?,
                    native_session_id: row.get(4)?,
                    settings_revision: row.get(5)?,
                    attempt_id: row.get(6)?,
                    task_id: row.get(7)?,
                    task_version: row.get(8)?,
                    attention: row.get(9)?,
                    attempt_status: row.get(10)?,
                })
            },
        )
        .optional()?;
    let Some(authority) = authority else {
        return Ok(None);
    };
    let superseded_results = {
        let mut statement = transaction.prepare(
            "SELECT rr.id FROM role_results rr
             WHERE rr.session_id=?1 AND rr.role_generation_id=?2
               AND rr.outcome IN ('blocked','needs_input') AND rr.consumed_at IS NULL
               AND julianday(rr.created_at)<julianday(?3)
               AND NOT EXISTS(SELECT 1 FROM role_result_supersessions superseded
                              WHERE superseded.role_result_id=rr.id)
             ORDER BY rr.rowid",
        )?;
        let rows = statement
            .query_map(
                params![
                    context.session_id,
                    context.role_generation_id,
                    authority.reserved_at
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    // Only a rejection of the exact exited state this resume replaced counts;
    // one already consumed by a fresh route or reconciled stays history.
    let rejections = {
        let mut statement = transaction.prepare(
            "SELECT event.id FROM audit_events event
             WHERE event.event_code='session.resume.rejected' AND event.entity_kind='session'
               AND event.entity_id=?1
               AND json_extract(event.detail_json,'$.role_generation_id')=?2
               AND json_extract(event.detail_json,'$.attempt_id')=?3
               AND json_extract(event.detail_json,'$.transcript_epoch')=?4
               AND CAST(json_extract(event.detail_json,'$.resume_count') AS INTEGER)=?5
               AND julianday(event.created_at)<=julianday(?6)
               AND NOT EXISTS(SELECT 1 FROM audit_events route
                 WHERE route.event_code='session.resume.fresh_route.reserved'
                   AND json_extract(route.detail_json,'$.rejection_event_id')=event.id)
               AND NOT EXISTS(SELECT 1 FROM audit_events reconciled,
                   json_each(reconciled.detail_json,'$.rejection_event_ids') reconciled_rejection
                 WHERE reconciled.event_code='session.resume.turn_reconciled'
                   AND reconciled_rejection.value=event.id)
             ORDER BY event.rowid",
        )?;
        let rows = statement
            .query_map(
                params![
                    context.session_id,
                    context.role_generation_id,
                    authority.attempt_id,
                    authority.prior_transcript_epoch,
                    authority.resume_ordinal - 1,
                    authority.reserved_at
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let retained_hold = if rejections.is_empty() || authority.attention != "resume_failed" {
        None
    } else if authority.attempt_status != "needs_input" {
        Some("attempt_not_waiting_on_resume")
    } else if transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM controls c WHERE c.attempt_id=?1
             AND c.kind!='transition_proposal'
             AND c.state NOT IN ('finished','cancelled','superseded','rejected','failed','abandoned'))
           OR EXISTS(SELECT 1 FROM restart_candidates rc WHERE rc.attempt_id=?1
             AND rc.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
           OR EXISTS(SELECT 1 FROM recovery_records r WHERE r.attempt_id=?1
             AND r.state='attention_required')
           OR EXISTS(SELECT 1 FROM rework_intents rw WHERE rw.new_attempt_id=?1
             AND rw.state NOT IN ('completed','cancelled','failed'))
           OR EXISTS(SELECT 1 FROM switch_intents si WHERE si.attempt_id=?1
             AND si.state NOT IN ('completed','cancelled','superseded'))",
        params![authority.attempt_id],
        |row| row.get::<_, bool>(0),
    )? {
        Some("independent_hold")
    } else if transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM audit_events event
           JOIN sessions other ON other.id=event.entity_id
           WHERE event.event_code='session.resume.rejected' AND event.entity_kind='session'
             AND json_extract(event.detail_json,'$.attempt_id')=?1 AND other.id!=?2
             AND other.transcript_epoch=json_extract(event.detail_json,'$.transcript_epoch')
             AND other.resume_count=CAST(json_extract(event.detail_json,'$.resume_count') AS INTEGER)
             AND NOT EXISTS(SELECT 1 FROM audit_events route
               WHERE route.event_code='session.resume.fresh_route.reserved'
                 AND json_extract(route.detail_json,'$.rejection_event_id')=event.id))",
        params![authority.attempt_id, context.session_id],
        |row| row.get::<_, bool>(0),
    )? {
        Some("other_resume_rejection")
    } else {
        None
    };
    // A rejection whose hold must stay stays unreconciled, so a later accepted
    // turn can release it once the independent hold is gone.
    let reconciled_rejections = if retained_hold.is_some() {
        Vec::new()
    } else {
        rejections
    };
    let hold_released = !reconciled_rejections.is_empty() && authority.attention == "resume_failed";
    if superseded_results.is_empty() && reconciled_rejections.is_empty() {
        return Ok(None);
    }
    let mut retired_proposals = 0;
    if hold_released {
        if transaction.execute(
            "UPDATE tasks SET attention='none',version=version+1,updated_at=?1
             WHERE id=?2 AND version=?3 AND attention='resume_failed'",
            params![now, authority.task_id, authority.task_version],
        )? != 1
        {
            bail!("task changed while releasing a superseded resume failure")
        }
        // The attempt's change time is left alone: it anchors which reports
        // are current and is not evidence of this recovery.
        if transaction.execute(
            "UPDATE attempts SET status='running' WHERE id=?1 AND status='needs_input'",
            params![authority.attempt_id],
        )? != 1
        {
            bail!("attempt changed while releasing a superseded resume failure")
        }
        crate::coordinator::record_hold_release(
            transaction,
            &authority.attempt_id,
            "resume_failure_superseded_by_accepted_turn",
            None,
            now,
        )?;
        retired_proposals = crate::trip::retire_unmatchable_transition_proposals(
            transaction,
            &authority.attempt_id,
            "resume_failure_superseded_by_accepted_turn",
            now,
        )?;
    }
    let new_version = authority.task_version + i64::from(hold_released);
    let detail = serde_json::json!({
        "session_id":context.session_id,
        "role_generation_id":context.role_generation_id,
        "native_session_id":authority.native_session_id,
        "transcript_epoch":context.transcript_epoch,
        "resume_invocation_id":authority.resume_invocation_id,
        "settings_revision":authority.settings_revision,
        "superseding_hook_event_id":hook_event_id,
        "rejection_event_ids":reconciled_rejections,
        "superseded_result_ids":superseded_results,
        "hold_released":hold_released,
        "hold_retained_reason":retained_hold,
        "retired_transition_proposals":retired_proposals,
        "workflow_consumption_recorded":false,
    });
    let audit_event_id = uuid::Uuid::new_v4().to_string();
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,actor_id,event_code,entity_kind,entity_id,old_version,new_version,detail_json,created_at)
         VALUES(?1,?2,'hook',?3,'session.resume.turn_reconciled','session',?4,?5,?6,?7,?8)",
        params![
            audit_event_id,
            uuid::Uuid::new_v4().to_string(),
            context.role_generation_id,
            context.session_id,
            authority.task_version,
            new_version,
            detail.to_string(),
            now
        ],
    )?;
    for result_id in &superseded_results {
        transaction.execute(
            "INSERT INTO role_result_supersessions(role_result_id,superseding_hook_event_id,session_id,
                 role_generation_id,native_session_id,transcript_epoch,resume_invocation_id,
                 settings_revision,superseded_rejection_event_id,audit_event_id,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                result_id,
                hook_event_id,
                context.session_id,
                context.role_generation_id,
                authority.native_session_id,
                context.transcript_epoch,
                authority.resume_invocation_id,
                authority.settings_revision,
                reconciled_rejections.last(),
                audit_event_id,
                now
            ],
        )?;
    }
    Ok(Some(detail))
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
    /// `setup` for a retained setup discovery or profile probe turn,
    /// `ordinary_manager` for the task's current manager.
    pub scope: String,
    pub attempt_id: String,
    pub task_version: i64,
    /// Late tool-completion hooks received after the Stop, which match tool
    /// starts inside the stopped turn.
    pub late_terminal_hooks: i64,
    /// The accepted report the stopped turn made, between its correlated
    /// prompt and its Stop. Required for the ordinary manager.
    pub accepted_result_id: Option<String>,
    pub phase: String,
    pub plan_hash: Option<String>,
    pub candidate_hash: Option<String>,
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
    pub attempt_id: String,
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
    pub attempt_id: String,
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

/// A Codex turn whose final Stop arrived with tool bookkeeping still open.
/// Codex does not always emit a completion hook for every started tool, so such
/// a Stop cannot be a safe idle boundary on its own. The coordinator promotes
/// the session to `idle_candidate` only after this exact receipt is still
/// current and the supervisor has positively inventoried the native process
/// and its descendants as idle; elapsed time and terminal quiet are never used.
///
/// Two scopes qualify. A retained setup discovery or profile probe first turn
/// keeps its original, narrower rules. The task's current ordinary manager
/// additionally requires its current resume invocation to be running, no
/// untrusted hook in the invocation, and no permission, input lease, uncertain
/// guidance delivery, control, switch, restart, recovery, freeze, or running
/// check for the attempt. For the manager, completion hooks that arrive after
/// the Stop and match a tool started in the stopped turn are late bookkeeping,
/// not new work; any other later hook keeps the turn busy.
pub(crate) fn eligible_codex_stop_idle_reconciliation(
    connection: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<Option<CodexStopIdleReceipt>> {
    connection
        .query_row(
            &format!("WITH current_codex AS (
               SELECT s.id AS session_id,rg.id AS generation_id,rc.id AS credential_id,
                      s.transcript_epoch,s.native_session_id,a.id AS attempt_id,COALESCE(t.version,0) AS task_version,
                      a.phase,a.plan_hash,a.candidate_hash,
                      CASE WHEN s.setup_permit_id IS NULL THEN 'ordinary_manager' ELSE 'setup' END AS scope,
                      (SELECT ri.id FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1) AS resume_invocation_id,
                      (SELECT ri.state FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1) AS resume_state,
                      COALESCE((SELECT ri.created_at FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1),s.created_at) AS invocation_started_at,
                      COALESCE((SELECT ri.hook_event_boundary_rowid FROM resume_invocations ri
                        WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                        ORDER BY ri.resume_ordinal DESC LIMIT 1),s.initial_hook_event_boundary_rowid,0) AS boundary,
                      s.readiness_state
               FROM sessions s
               JOIN role_generations rg ON rg.id=s.role_generation_id
               JOIN attempts a ON a.id=rg.attempt_id
               LEFT JOIN tasks t ON t.id=a.task_id
               JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=rg.role
                 AND rs.effective_generation_id=rg.id
               JOIN role_credentials rc ON rc.role_generation_id=rg.id AND rc.revoked_at IS NULL
               LEFT JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
               WHERE (?1 IS NULL OR s.id=?1) AND s.provider='codex'
                 AND rg.status='running' AND s.status='running'
                 AND rg.role!='final_verifier'
                 AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
                 AND s.id=(SELECT latest.id FROM sessions latest
                   WHERE latest.role_generation_id=rg.id
                   ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
                 AND ((s.setup_permit_id IS NOT NULL
                       AND s.readiness_state='busy_unresolved_hook_work'
                       AND s.resume_count=0
                       AND sp.state='issued' AND sp.attempt_id=a.id AND sp.role=rg.role
                       AND sp.purpose IN ('setup_discovery','profile_probe')
                       AND ((sp.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
                         OR (sp.purpose='profile_probe' AND s.validation_cell='trip_setup_probe')))
                   OR (s.setup_permit_id IS NULL AND rg.role='manager'
                       AND s.readiness_state IN ('busy_unresolved_hook_work','busy')
                       AND a.status IN ('running','needs_input')
                       -- A pending service-owned obligation on a frozen candidate
                       -- keeps its own one-shot service stop instead.
                       AND (a.candidate_hash IS NULL OR t.attention!='none')
                       AND t.lifecycle IN ('in_progress','validation','awaiting_review')
                       AND t.archived_at IS NULL
                       -- No second live manager authority for the attempt.
                       AND NOT EXISTS(SELECT 1 FROM role_generations other
                         WHERE other.attempt_id=a.id AND other.role='manager' AND other.id!=rg.id
                           AND other.status IN ('launch_reserved','running','stopping'))))
             ), latest_stop AS (
               SELECT current_codex.*,h.rowid AS stop_rowid
               FROM current_codex JOIN hook_events h ON h.session_id=current_codex.session_id
               WHERE h.rowid=(SELECT MAX(latest.rowid) FROM hook_events latest
                 WHERE latest.session_id=current_codex.session_id
                   AND latest.rowid>current_codex.boundary
                   AND (current_codex.scope='setup' OR latest.event_name='Stop'))
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
             ), bounded_turn AS (
               SELECT stopped_turn.*,
                      (SELECT COUNT(*) FROM hook_events late
                       WHERE late.session_id=stopped_turn.session_id
                         AND late.rowid>stopped_turn.stop_rowid) AS later_hooks,
                      (SELECT COUNT(*) FROM hook_events late
                       WHERE late.session_id=stopped_turn.session_id
                         AND late.rowid>stopped_turn.stop_rowid
                         AND late.event_name IN ('PostToolUse','PostToolUseFailure')
                         AND late.role_generation_id=stopped_turn.generation_id
                         AND late.native_session_id=stopped_turn.native_session_id
                         AND late.provenance_state='managed_process_group_untrusted_payload'
                         AND json_valid(late.payload_json)
                         AND json_extract(late.payload_json,'$.tool_use_id') IN (
                           -- Tools started in the stopped turn that had no
                           -- completion before its Stop.
                           SELECT json_extract(started.payload_json,'$.tool_use_id')
                           FROM hook_events started
                           WHERE started.session_id=stopped_turn.session_id
                             AND started.event_name='PreToolUse'
                             AND started.role_generation_id=stopped_turn.generation_id
                             AND started.native_session_id=stopped_turn.native_session_id
                             AND started.rowid BETWEEN stopped_turn.submit_rowid AND stopped_turn.stop_rowid
                             AND json_valid(started.payload_json)
                             AND NOT EXISTS(SELECT 1 FROM hook_events finished
                               WHERE finished.session_id=stopped_turn.session_id
                                 AND finished.event_name IN ('PostToolUse','PostToolUseFailure')
                                 AND finished.rowid>started.rowid
                                 AND finished.rowid<stopped_turn.stop_rowid
                                 AND json_valid(finished.payload_json)
                                 AND json_extract(finished.payload_json,'$.tool_use_id')
                                   =json_extract(started.payload_json,'$.tool_use_id')))) AS late_terminal_hooks,
                      (SELECT COUNT(DISTINCT json_extract(late.payload_json,'$.tool_use_id'))
                       FROM hook_events late
                       WHERE late.session_id=stopped_turn.session_id
                         AND late.rowid>stopped_turn.stop_rowid
                         AND late.event_name IN ('PostToolUse','PostToolUseFailure')
                         AND json_valid(late.payload_json)) AS late_terminal_tools,
                      (SELECT received_at FROM hook_events WHERE rowid=stopped_turn.submit_rowid) AS submit_at,
                      (SELECT received_at FROM hook_events WHERE rowid=stopped_turn.stop_rowid) AS stop_at
               FROM stopped_turn
             )
             SELECT bounded_turn.session_id,bounded_turn.generation_id,
                    bounded_turn.credential_id,bounded_turn.transcript_epoch,
                    bounded_turn.resume_invocation_id,bounded_turn.boundary,
                    bounded_turn.native_session_id,bounded_turn.stop_rowid,
                    bounded_turn.scope,bounded_turn.attempt_id,bounded_turn.task_version,
                    bounded_turn.late_terminal_hooks,
                    (SELECT result.id FROM role_results result
                     WHERE result.session_id=bounded_turn.session_id
                       AND result.role_generation_id=bounded_turn.generation_id
                       AND julianday(result.created_at)>=julianday(bounded_turn.submit_at)
                       AND julianday(result.created_at)<=julianday(bounded_turn.stop_at)
                       AND NOT EXISTS(SELECT 1 FROM audit_events retired
                         WHERE retired.event_code='role_result.superseded'
                           AND retired.entity_id=result.id)
                     ORDER BY result.rowid DESC LIMIT 1) AS accepted_result_id,
                    bounded_turn.phase,bounded_turn.plan_hash,bounded_turn.candidate_hash
             FROM bounded_turn
             WHERE bounded_turn.submit_rowid IS NOT NULL AND {hold_absent}
               AND bounded_turn.later_hooks=bounded_turn.late_terminal_hooks
               -- Each late completion closes a different unmatched tool.
               AND bounded_turn.late_terminal_hooks=bounded_turn.late_terminal_tools
               -- The ordinary manager must have reported in this exact turn;
               -- a Stop alone never makes it idle.
               AND (bounded_turn.scope='setup' OR accepted_result_id IS NOT NULL)
               AND (bounded_turn.readiness_state='busy_unresolved_hook_work'
                 OR bounded_turn.late_terminal_hooks>0)
               AND EXISTS(SELECT 1 FROM hook_events start
                 WHERE start.session_id=bounded_turn.session_id
                   AND start.role_generation_id=bounded_turn.generation_id
                   AND start.event_name='SessionStart'
                   AND start.native_session_id=bounded_turn.native_session_id
                   AND start.provenance_state='managed_process_group_untrusted_payload'
                   AND start.rowid>bounded_turn.boundary
                   AND start.rowid<bounded_turn.submit_rowid)
               AND (SELECT COUNT(*) FROM hook_events started
                    WHERE started.session_id=bounded_turn.session_id
                      AND started.event_name='PreToolUse'
                      AND started.role_generation_id=bounded_turn.generation_id
                      AND started.native_session_id=bounded_turn.native_session_id
                      AND started.provenance_state='managed_process_group_untrusted_payload'
                      AND started.rowid BETWEEN bounded_turn.submit_rowid AND bounded_turn.stop_rowid)
                 > (SELECT COUNT(*) FROM hook_events finished
                    WHERE finished.session_id=bounded_turn.session_id
                      AND finished.event_name IN ('PostToolUse','PostToolUseFailure')
                      AND finished.role_generation_id=bounded_turn.generation_id
                      AND finished.native_session_id=bounded_turn.native_session_id
                      AND finished.provenance_state='managed_process_group_untrusted_payload'
                      AND finished.rowid BETWEEN bounded_turn.submit_rowid AND bounded_turn.stop_rowid)
               AND (SELECT COUNT(*) FROM hook_events started
                    WHERE started.session_id=bounded_turn.session_id
                      AND started.event_name='SubagentStart'
                      AND started.role_generation_id=bounded_turn.generation_id
                      AND started.native_session_id=bounded_turn.native_session_id
                      AND started.provenance_state='managed_process_group_untrusted_payload'
                      AND started.rowid BETWEEN bounded_turn.submit_rowid AND bounded_turn.stop_rowid)
                 = (SELECT COUNT(*) FROM hook_events finished
                    WHERE finished.session_id=bounded_turn.session_id
                      AND finished.event_name='SubagentStop'
                      AND finished.role_generation_id=bounded_turn.generation_id
                      AND finished.native_session_id=bounded_turn.native_session_id
                      AND finished.provenance_state='managed_process_group_untrusted_payload'
                      AND finished.rowid BETWEEN bounded_turn.submit_rowid AND bounded_turn.stop_rowid)
               AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                 WHERE permission.session_id=bounded_turn.session_id
                   AND permission.consumed_at IS NULL
                   AND permission.delivery_state NOT IN ('expired','not_delivered')
                   AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                     WHERE native.permission_request_id=permission.id))
               AND NOT EXISTS(SELECT 1 FROM input_leases lease
                 WHERE lease.session_id=bounded_turn.session_id AND lease.revoked_at IS NULL
                   AND julianday(lease.expires_at)>julianday('now'))
               AND NOT EXISTS(SELECT 1 FROM guidance_messages guidance
                 WHERE guidance.role_generation_id=bounded_turn.generation_id
                   AND guidance.state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))
               AND NOT EXISTS(SELECT 1 FROM controls control
                 WHERE control.attempt_id=bounded_turn.attempt_id
                   AND control.state IN ('requested','draining','held','recovery_required'))
               AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                 WHERE recovery.session_id=bounded_turn.session_id
                   AND recovery.state!='resolved_quiescent')
               AND (bounded_turn.scope='setup' OR (
                 bounded_turn.resume_state IS NULL OR bounded_turn.resume_state='running')
                 AND NOT EXISTS(SELECT 1 FROM hook_events untrusted
                   WHERE untrusted.session_id=bounded_turn.session_id
                     AND untrusted.rowid>bounded_turn.boundary
                     AND untrusted.event_name='UntrustedNativeEvent')
                 AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                   WHERE permission.attempt_id=bounded_turn.attempt_id
                     AND permission.consumed_at IS NULL
                     AND permission.delivery_state NOT IN ('expired','not_delivered')
                     AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                       WHERE native.permission_request_id=permission.id))
                 AND NOT EXISTS(SELECT 1 FROM controls control
                   WHERE control.attempt_id=bounded_turn.attempt_id
                     AND control.state NOT IN ('finished','cancelled','superseded','rejected','failed','abandoned')
                     AND NOT (control.kind='transition_proposal' AND control.state='proposed'))
                 AND NOT EXISTS(SELECT 1 FROM switch_intents switch
                   WHERE switch.attempt_id=bounded_turn.attempt_id
                     AND switch.state NOT IN ('completed','cancelled','rejected','superseded'))
                 AND NOT EXISTS(SELECT 1 FROM restart_candidates restart
                   WHERE restart.attempt_id=bounded_turn.attempt_id
                     AND restart.state NOT IN ('resumed','released_fresh_dispatch','cancelled'))
                 AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                   WHERE recovery.attempt_id=bounded_turn.attempt_id
                     AND recovery.state='attention_required')
                 AND NOT EXISTS(SELECT 1 FROM freeze_intents freeze
                   WHERE freeze.attempt_id=bounded_turn.attempt_id
                     AND freeze.state IN ('reserved','capturing','recovery_required'))
                 AND NOT EXISTS(SELECT 1 FROM check_runs check_run
                   WHERE check_run.attempt_id=bounded_turn.attempt_id
                     AND check_run.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')))
             ORDER BY bounded_turn.stop_rowid LIMIT 1",
                hold_absent = crate::coordinator::coordinator_hold_absent("bounded_turn.attempt_id"),
            ),
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
                    scope: row.get(8)?,
                    attempt_id: row.get(9)?,
                    task_version: row.get(10)?,
                    late_terminal_hooks: row.get(11)?,
                    accepted_result_id: row.get(12)?,
                    phase: row.get(13)?,
                    plan_hash: row.get(14)?,
                    candidate_hash: row.get(15)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// The newest trusted Stop of the setup session's first invocation, over
/// aliases `s` (sessions) and `rg` (role_generations).
const SETUP_FIRST_TURN_STOP_ROWID_SQL: &str = "(SELECT MAX(stop.rowid) FROM hook_events stop
    WHERE stop.session_id=s.id AND stop.role_generation_id=rg.id AND stop.event_name='Stop'
      AND stop.native_session_id=s.native_session_id
      AND stop.provenance_state='managed_process_group_untrusted_payload'
      AND stop.rowid>s.initial_hook_event_boundary_rowid)";

/// The terminal notification observed from the admitted Claude release.
const BENIGN_TERMINAL_NOTIFICATION_TYPES: [&str; 1] = ["idle_prompt"];

/// Whether every hook after a first-turn Stop is a terminal hook of that same
/// stopped turn: a subagent shutdown or a benign notification, each from the
/// exact generation, native session and managed process group. Any other or
/// unclassifiable hook, a supported native prompt, or a turn failure means the
/// turn is not over, so the stop cannot be claimed or relied on after exit.
fn setup_first_turn_stop_has_terminal_suffix(
    connection: &rusqlite::Connection,
    session_id: &str,
    generation_id: &str,
    native_session_id: &str,
    stop_rowid: i64,
) -> Result<bool> {
    let mut statement = connection.prepare(
        "SELECT event_name,role_generation_id,native_session_id,provenance_state,payload_json
         FROM hook_events WHERE session_id=?1 AND rowid>?2 ORDER BY rowid",
    )?;
    let suffix = statement
        .query_map(params![session_id, stop_rowid], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(suffix.iter().all(
        |(event_name, generation, native, provenance, payload_json)| {
            generation == generation_id
                && native.as_deref() == Some(native_session_id)
                && provenance == "managed_process_group_untrusted_payload"
                && match event_name.as_str() {
                    "SubagentStop" => true,
                    "Notification" => serde_json::from_str::<serde_json::Value>(payload_json)
                        .ok()
                        .and_then(|payload| {
                            payload
                                .get("notification_type")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                        .is_some_and(|kind| {
                            kind.parse::<crate::domain::NativePromptKind>().is_err()
                                && BENIGN_TERMINAL_NOTIFICATION_TYPES.contains(&kind.as_str())
                        }),
                    _ => false,
                }
        },
    ))
}

pub(crate) fn eligible_setup_retained_first_turn_stop(
    connection: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<Option<SetupRetainedFirstTurnStopReceipt>> {
    let candidates = {
        let mut statement = connection.prepare(&format!(
            "SELECT s.id,rg.id,rc.id,s.transcript_epoch,s.native_session_id,s.process_identity_json,
                    {SETUP_FIRST_TURN_STOP_ROWID_SQL},sp.id,a.id
             FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id
             JOIN role_settings rs ON rs.task_id=a.task_id AND rs.role=rg.role
               AND rs.effective_generation_id=rg.id
             JOIN role_credentials rc ON rc.role_generation_id=rg.id AND rc.revoked_at IS NULL
             JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
             WHERE (?1 IS NULL OR s.id=?1) AND {hold_absent}
               AND s.status='running' AND s.readiness_state='idle_candidate'
               AND rg.status='running' AND s.resume_count=0
               AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
               AND s.process_identity_json IS NOT NULL AND s.process_identity_json!=''
               AND sp.state='issued' AND sp.attempt_id=a.id AND sp.role=rg.role
               AND sp.purpose IN ('setup_discovery','profile_probe')
               AND rg.role!='final_verifier'
               AND ((sp.purpose='setup_discovery' AND s.validation_cell='trip_setup_discovery')
                 OR (sp.purpose='profile_probe' AND s.validation_cell='trip_setup_probe'))
               AND {SETUP_FIRST_TURN_STOP_ROWID_SQL} IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM role_results result
                 WHERE result.session_id=s.id AND result.role_generation_id=rg.id
                   AND result.outcome='capability_observed')
               AND NOT EXISTS(SELECT 1 FROM permission_requests permission
                 WHERE permission.session_id=s.id AND permission.consumed_at IS NULL
                   AND permission.delivery_state NOT IN ('expired','not_delivered')
                   AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                     WHERE native.permission_request_id=permission.id))
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
             ORDER BY s.updated_at,s.rowid",
            hold_absent = crate::coordinator::coordinator_hold_absent("a.id"),
        ))?;
        let rows = statement
            .query_map(params![session_id], |row| {
                Ok(SetupRetainedFirstTurnStopReceipt {
                    session_id: row.get(0)?,
                    generation_id: row.get(1)?,
                    credential_id: row.get(2)?,
                    transcript_epoch: row.get(3)?,
                    native_session_id: row.get(4)?,
                    process_identity_json: row.get(5)?,
                    stop_rowid: row.get(6)?,
                    setup_permit_id: row.get(7)?,
                    attempt_id: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for receipt in candidates {
        if setup_first_turn_stop_has_terminal_suffix(
            connection,
            &receipt.session_id,
            &receipt.generation_id,
            &receipt.native_session_id,
            receipt.stop_rowid,
        )? {
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

fn setup_retained_first_turn_stop_timeout_candidate_in(
    connection: &rusqlite::Connection,
    session_id: Option<&str>,
    now: &str,
) -> Result<Option<SetupRetainedFirstTurnStopTimeoutReceipt>> {
    connection
        .query_row(
            &format!(
                "SELECT s.id,rg.id,s.transcript_epoch,s.native_session_id,s.process_identity_json,
                    CAST(json_extract(audit.detail_json,'$.stop_event_rowid') AS INTEGER),audit.rowid,
                    rg.attempt_id
             FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN audit_events audit ON audit.entity_kind='session' AND audit.entity_id=s.id
               AND audit.actor_kind='service'
               AND audit.event_code='setup.retained_first_turn.stop_claimed'
             WHERE (?1 IS NULL OR s.id=?1) AND {}
               AND s.status='interrupt_requested' AND s.readiness_state='idle_candidate'
               AND json_extract(audit.detail_json,'$.role_generation_id')=rg.id
               AND json_extract(audit.detail_json,'$.transcript_epoch')=s.transcript_epoch
               AND json_extract(audit.detail_json,'$.native_session_id')=s.native_session_id
               AND json_extract(audit.detail_json,'$.process_identity_json')=s.process_identity_json
               AND julianday(json_extract(audit.detail_json,'$.timeout_at'))<=julianday(?2)
               AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
                 WHERE recovery.session_id=s.id AND recovery.state!='resolved_quiescent')
             ORDER BY audit.created_at,audit.rowid,s.id LIMIT 1",
                crate::coordinator::coordinator_hold_absent("rg.attempt_id")
            ),
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
                    attempt_id: row.get(7)?,
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
                   AND permission.delivery_state NOT IN ('expired','not_delivered')
                   AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
                     WHERE native.permission_request_id=permission.id))
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

pub(crate) fn eligible_implementer_candidate(
    connection: &rusqlite::Connection,
    attempt_id: &str,
    require_quiescent: bool,
) -> Result<Option<(String, String, String)>> {
    Ok(connection
        .query_row(
            "SELECT r.id,g.id,s.id FROM role_results r
         JOIN role_generations g ON g.id=r.role_generation_id
         JOIN sessions s ON s.id=r.session_id AND s.role_generation_id=g.id
         JOIN attempts a ON a.id=g.attempt_id
         JOIN role_settings settings ON settings.task_id=a.task_id AND settings.role='implementer'
           AND settings.effective_generation_id=g.id
         WHERE a.id=?1 AND a.phase='implementation' AND a.plan_approved_at IS NOT NULL
           AND g.role='implementer' AND g.lane_id='default'
           AND r.outcome='candidate_ready' AND r.consumed_at IS NULL
           AND CASE WHEN json_type(r.metadata_json,'$.lane_yield.plan_hash') IS NULL
               THEN r.created_at>=a.updated_at
               ELSE json_extract(r.metadata_json,'$.lane_yield.plan_hash')=a.plan_hash END
           AND (?2=0 OR (s.status='exited' AND g.status='exited'
             AND json_extract(s.exit_json,'$.process_group_quiescent')=1))
         ORDER BY r.created_at DESC,r.id DESC LIMIT 1",
            params![attempt_id, require_quiescent],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
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
    let claimed_stop: Option<(i64, String)> = connection.query_row(
        &format!("SELECT CAST(json_extract(audit.detail_json,'$.stop_event_rowid') AS INTEGER),s.native_session_id
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
           AND json_extract(audit.detail_json,'$.stop_event_rowid')={SETUP_FIRST_TURN_STOP_ROWID_SQL}
           AND COALESCE(json_extract(audit.detail_json,'$.resume_spent'),1)=0
           AND revoked.role_generation_id=rg.id AND revoked.revoked_at IS NOT NULL
           AND NOT EXISTS(SELECT 1 FROM role_credentials current
             WHERE current.role_generation_id=rg.id AND current.revoked_at IS NULL)
           AND NOT EXISTS(SELECT 1 FROM recovery_records recovery
             WHERE recovery.session_id=s.id AND recovery.state!='resolved_quiescent')
         ORDER BY audit.rowid DESC LIMIT 1"),
        params![session_id, role_generation_id, setup_permit_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((stop_rowid, native_session_id)) = claimed_stop else {
        return Ok(false);
    };
    setup_first_turn_stop_has_terminal_suffix(
        connection,
        session_id,
        role_generation_id,
        &native_session_id,
        stop_rowid,
    )
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
    use crate::{config::InstancePaths, database};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

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

    fn prior_service_schema_sql(version: i64) -> String {
        let drop_schema_37 = "CREATE TABLE trip_legacy_migrations_previous (
            id TEXT PRIMARY KEY,attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
            from_workflow_id TEXT NOT NULL,to_workflow_id TEXT NOT NULL,preserved_json TEXT NOT NULL,
            reviewed_plan_hash TEXT NOT NULL,config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
            authorized_at TEXT NOT NULL);
            INSERT INTO trip_legacy_migrations_previous SELECT id,attempt_id,from_workflow_id,to_workflow_id,
                preserved_json,reviewed_plan_hash,config_revision_id,authorized_at FROM trip_legacy_migrations;
            DROP TABLE trip_legacy_migrations;
            ALTER TABLE trip_legacy_migrations_previous RENAME TO trip_legacy_migrations;";
        let drop_schema_36 = format!(
            "{drop_schema_37} DROP TABLE attention_observations; DROP INDEX role_results_session_generation;"
        );
        let drop_schema_35 = format!("{drop_schema_36} DROP TABLE provider_failure_holds;");
        let drop_schema_34 = format!(
            "{drop_schema_35} DROP TRIGGER guidance_submitted_form_paired;
             DROP TRIGGER guidance_submitted_form_once;
             ALTER TABLE guidance_messages DROP COLUMN submitted_digest;
             ALTER TABLE guidance_messages DROP COLUMN submitted_text;"
        );
        let drop_after_schema_32 = format!(
            "{drop_schema_34} DROP TABLE permission_native_resolutions;
             DROP TABLE permission_request_hooks; DROP TABLE role_result_supersessions;"
        );
        let drop_later = match version {
            31 => {
                format!("{drop_after_schema_32} DROP TABLE final_repair_rechecks; {MIGRATION_031}")
            }
            32 => drop_after_schema_32,
            33 => drop_schema_34,
            34 => drop_schema_35,
            35 => drop_schema_36,
            36 => drop_schema_37.to_owned(),
            37 => String::new(),
            _ => panic!("unsupported fixture schema {version}"),
        };
        format!("DROP TABLE codex_usage_observations; {drop_later} PRAGMA user_version={version};")
    }

    fn schema_31_service_fixture(store: &Store) {
        store
            .lock()
            .unwrap()
            .execute_batch(&prior_service_schema_sql(31))
            .unwrap();
        store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-service-upgrade-fixture','service-upgrade-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','needs_input','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance,extension_allowance,spent)
               VALUES('b','a','code',2,4,6);
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','code_reviewer','codex',1,1,'exited','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('s','g','codex','exited','{}','fixture','e','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,prompt_hash,handoff_hash,delivery_state,verdict,created_at,updated_at)
               VALUES('approved','a','code','c1','p','h','finished','approved','2026-01-01T00:00:01Z','2026-01-01T00:00:01Z'),
                     ('final','a','final','c1','p','h','finished','request_changes','2026-01-01T00:00:02Z','2026-01-01T00:00:02Z'),
                     ('sixth','a','code','c2','p','h','finished','needs_rework','2026-01-01T00:00:03Z','2026-01-01T00:00:03Z');
             INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
               VALUES('sixth-result','sixth','s','g','needs_rework','x','[]','{}','2026-01-01T00:00:03Z','2026-01-01T00:00:03Z');
             INSERT INTO final_repair_rechecks(attempt_id,task_id,operation_id,request_hash,authorized_task_version,plan_hash,configuration_hash,
                 approved_code_request_id,prior_candidate_hash,final_request_id,rejected_code_request_id,rejected_code_result_id,
                 reviewer_generation_id,reviewer_session_id,reviewer_settings_revision,reviewer_profile_hash,detail_json,state,created_at,updated_at)
               VALUES('a','t','recover','hash',7,'plan',NULL,'approved','c1','final','sixth','sixth-result','g','s',1,'profile','{}','authorized',
                 '2026-01-01T00:00:04Z','2026-01-01T00:00:04Z');"
        ).unwrap();
    }

    const SERVICE_FIXTURE_HISTORY: &str = "SELECT (SELECT group_concat(id||':'||lifecycle) FROM tasks)
         ||'|'||(SELECT group_concat(id||':'||phase||':'||status) FROM attempts)
         ||'|'||(SELECT group_concat(review_kind||':'||(initial_allowance+extension_allowance)||':'||spent) FROM review_budgets)
         ||'|'||(SELECT group_concat(id||':'||delivery_state||':'||verdict) FROM review_requests)
         ||'|'||(SELECT group_concat(attempt_id||':'||operation_id||':'||state||':'||reviewer_session_id
              ||':'||rejected_code_request_id||':'||rejected_code_result_id) FROM final_repair_rechecks)
         ||'|'||(SELECT revision FROM state_revision WHERE singleton=1)";

    fn restorepoint_paths() -> InstancePaths {
        let root =
            std::env::temp_dir().join(format!("llmrelay-restorepoint-{}", uuid::Uuid::new_v4()));
        let paths = InstancePaths::resolve(Some(root)).unwrap();
        paths.create().unwrap();
        paths
    }

    fn restorepoint_fixture(version: i64) -> (InstancePaths, Store) {
        let paths = restorepoint_paths();
        let mut store = Store::open(&paths.database).unwrap();
        store
            .lock()
            .unwrap()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        schema_31_service_fixture(&store);
        if version != 31 {
            drop(store);
            store = Store::open_service(&paths.database).unwrap();
            store
                .lock()
                .unwrap()
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=0;")
                .unwrap();
            store
                .lock()
                .unwrap()
                .execute_batch(&prior_service_schema_sql(version))
                .unwrap();
        }
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE projects SET repository_path=?1",
                [paths.root.to_str().unwrap()],
            )
            .unwrap();
        (paths, store)
    }

    fn restorepoint_root(paths: &InstancePaths) -> std::path::PathBuf {
        paths.root.parent().unwrap().join(format!(
            "{}.backups",
            paths.root.file_name().unwrap().to_str().unwrap()
        ))
    }

    fn restorepoint_snapshots(paths: &InstancePaths) -> Vec<std::path::PathBuf> {
        let root = restorepoint_root(paths);
        if !root.exists() {
            return Vec::new();
        }
        let mut snapshots: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                !path.file_name().unwrap().to_str().unwrap().starts_with('.')
                    && path.join("manifest.json").is_file()
            })
            .collect();
        snapshots.sort();
        snapshots
    }

    #[test]
    fn restorepoint_upgrade_restore_and_held_restart_preserve_each_prior_schema() {
        for version in SERVICE_UPGRADABLE_SCHEMA_VERSIONS {
            let (paths, old) = restorepoint_fixture(version);
            old.lock().unwrap().execute_batch(
                "INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
                   VALUES('credential','g','old-hash','[]','2026-01-01T00:00:00Z');
                 INSERT INTO permission_rules(id,provider,project_id,role,lifetime,registered_root,repository_identity,
                   executable_kind,executable_value,display_family,policy_fingerprint,created_by,created_at)
                   VALUES('rule','codex','p','code_reviewer','project','/tmp','fixture','exact','true','shell','policy','human','2026-01-01T00:00:00Z');
                 INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,process_identity_json,expires_at,created_at,updated_at)
                   VALUES('s','lease','human','human','g','{}','2099-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO launch_permits(id,attempt_id,role,settings_revision,state,created_at)
                   VALUES('permit','a','code_reviewer',1,'issued','2026-01-01T00:00:00Z');
                 INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
                   VALUES('control','a','resume','requested',1,'{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 UPDATE instance_settings SET auto_resume_eligible=1;
                 UPDATE tasks SET title='committed WAL history' WHERE id='t';"
            ).unwrap();
            let main_bytes = std::fs::read(&paths.database).unwrap();
            assert_eq!(
                u32::from_be_bytes(main_bytes[60..64].try_into().unwrap()),
                CURRENT_SCHEMA_VERSION as u32
            );
            assert!(
                std::fs::metadata(format!("{}-wal", paths.database.display()))
                    .unwrap()
                    .len()
                    > 0
            );
            let history: String = old
                .lock()
                .unwrap()
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
                .unwrap();
            assert!(Store::open_current_readonly(&paths.database).is_err());
            assert!(Store::open_current_writable(&paths.database).is_err());
            assert!(database::inspect(&paths).is_err());
            assert!(database::backup(&paths, None).is_err());
            assert!(restorepoint_snapshots(&paths).is_empty());

            let lock = database::InstanceLock::acquire(&paths).unwrap();
            let upgraded = database::open_service_locked(&paths, &lock).unwrap();
            assert_eq!(
                upgraded
                    .lock()
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM codex_usage_observations", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                0
            );
            assert_eq!(
                require_maintenance_schema(&upgraded.lock().unwrap()).unwrap(),
                CURRENT_SCHEMA_VERSION
            );
            assert_eq!(
                upgraded
                    .lock()
                    .unwrap()
                    .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
                    .unwrap(),
                history
            );
            assert!(upgraded.restore_hold().unwrap().is_none());
            let snapshots = restorepoint_snapshots(&paths);
            assert_eq!(snapshots.len(), 1);
            let snapshot = &snapshots[0];
            let verified = database::verify(snapshot).unwrap();
            assert_eq!(verified["schema_version"], version);
            let snapshot_connection = Connection::open_with_flags(
                format!(
                    "file:{}?immutable=1",
                    snapshot.join("database.sqlite3").display()
                ),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
            .unwrap();
            assert_eq!(
                snapshot_connection
                    .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
                    .unwrap(),
                history
            );
            assert_eq!(
                snapshot_connection
                    .query_row("SELECT title FROM tasks", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "committed WAL history"
            );
            drop(snapshot_connection);
            drop(upgraded);
            drop(old);
            drop(lock);

            let restored = database::restore(&paths, snapshot).unwrap();
            assert_eq!(restored["automatic_resume"], false);
            let held = Store::open_maintenance_readonly(&paths.database).unwrap();
            assert_eq!(
                require_maintenance_schema(&held.lock().unwrap()).unwrap(),
                version
            );
            assert_eq!(
                held.restore_hold().unwrap().unwrap()["operation_id"],
                restored["operation_id"]
            );
            assert!(held.require_execution_unheld("dispatch").is_err());
            let authority: String = held.lock().unwrap().query_row(
                "SELECT (SELECT revoked_at IS NOT NULL FROM role_credentials WHERE id='credential')||':'||
                   (SELECT revoked_at IS NOT NULL FROM permission_rules WHERE id='rule')||':'||
                   (SELECT revoked_at IS NOT NULL FROM input_leases WHERE session_id='s')||':'||
                   (SELECT state FROM launch_permits WHERE id='permit')||':'||
                   (SELECT state FROM controls WHERE id='control')||':'||
                   (SELECT auto_resume_eligible FROM instance_settings)||':'||
                   (SELECT queue_paused FROM projects WHERE id='p')", [], |row| row.get(0),
            ).unwrap();
            assert_eq!(authority, "1:1:1:released:cancelled:0:1");
            let held_history: String = held
                .lock()
                .unwrap()
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                held_history.rsplit_once('|').unwrap().0,
                history.rsplit_once('|').unwrap().0
            );
            let inode = std::fs::metadata(&paths.database).unwrap().ino();
            drop(held);

            let lock = database::InstanceLock::acquire(&paths).unwrap();
            database::recover_interrupted_restore_locked(&paths).unwrap();
            let next = database::open_service_locked(&paths, &lock).unwrap();
            assert_eq!(
                require_maintenance_schema(&next.lock().unwrap()).unwrap(),
                CURRENT_SCHEMA_VERSION
            );
            assert_eq!(std::fs::metadata(&paths.database).unwrap().ino(), inode);
            assert_eq!(
                next.restore_hold().unwrap().unwrap()["operation_id"],
                restored["operation_id"]
            );
            assert!(next.require_execution_unheld("resume").is_err());
            assert_eq!(
                next.lock()
                    .unwrap()
                    .query_row(
                        "SELECT auto_resume_eligible FROM instance_settings",
                        [],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
            drop(next);
            database::recover_interrupted_restore_locked(&paths).unwrap();
            drop(lock);
            assert_eq!(database::verify(snapshot).unwrap(), verified);
            std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
            std::fs::remove_dir_all(paths.root).unwrap();
        }
    }

    #[test]
    fn restorepoint_new_current_and_unlocked_maintenance_do_not_gain_upgrade_authority() {
        let paths = restorepoint_paths();
        assert!(Store::open_maintenance_readonly(&paths.database).is_err());
        assert!(Store::open_maintenance_writable(&paths.database).is_err());
        assert!(!paths.database.exists());
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        for _ in 0..2 {
            drop(database::open_service_locked(&paths, &lock).unwrap());
            assert!(!restorepoint_root(&paths).exists());
        }
        assert!(database::InstanceLock::acquire(&paths).is_err());
        assert!(database::backup(&paths, None)
            .unwrap_err()
            .to_string()
            .contains("another LLMRelay instance owns"));
        let other = restorepoint_paths();
        assert!(database::open_service_locked(&other, &lock).is_err());
        assert!(!other.database.exists());
        drop(lock);
        std::fs::remove_dir_all(paths.root).unwrap();
        std::fs::remove_dir_all(other.root).unwrap();
    }

    #[test]
    fn restorepoint_capacity_and_prepublication_refusals_leave_source_and_points_unchanged() {
        let (paths, source) = restorepoint_fixture(35);
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        let main = std::fs::read(&paths.database).unwrap();
        let wal_path = format!("{}-wal", paths.database.display());
        let wal = std::fs::read(&wal_path).unwrap();
        database::TEST_AVAILABLE_CAPACITY
            .with(|values| *values.borrow_mut() = [u64::MAX, 0].into());
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        assert!(format!("{error:#}").contains("migration capacity after verified restore point"));
        let prior = restorepoint_snapshots(&paths);
        assert_eq!(prior.len(), 1);
        let verified = database::verify(&prior[0]).unwrap();
        for interrupt in [false, true] {
            database::TEST_AVAILABLE_CAPACITY.with(|values| {
                *values.borrow_mut() = [if interrupt { u64::MAX } else { 0 }].into()
            });
            database::TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH.with(|armed| armed.set(interrupt));
            let error = database::open_service_locked(&paths, &lock).err().unwrap();
            let expected = if interrupt {
                "injected interruption before backup publication"
            } else {
                "shared filesystem backup and migration capacity"
            };
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            database::TEST_AVAILABLE_CAPACITY.with(|values| assert!(values.borrow().is_empty()));
            assert_eq!(
                require_maintenance_schema(&source.lock().unwrap()).unwrap(),
                35
            );
            assert_eq!(std::fs::read(&paths.database).unwrap(), main);
            assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
            assert_eq!(restorepoint_snapshots(&paths), prior);
            assert_eq!(database::verify(&prior[0]).unwrap(), verified);
        }
        assert_eq!(
            std::fs::read_dir(restorepoint_root(&paths))
                .unwrap()
                .count(),
            2
        );
        drop(source);
        drop(lock);
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn restorepoint_invalid_sources_refuse_before_upgrade_and_preserve_verified_points() {
        for fault in [
            "corrupt",
            "foreign_keys",
            "schema_0",
            "schema_30",
            "schema_39",
        ] {
            let (paths, source) = restorepoint_fixture(31);
            let lock = database::InstanceLock::acquire(&paths).unwrap();
            database::TEST_AVAILABLE_CAPACITY
                .with(|values| *values.borrow_mut() = [u64::MAX, 0].into());
            assert!(database::open_service_locked(&paths, &lock).is_err());
            let points = restorepoint_snapshots(&paths);
            let verified = database::verify(&points[0]).unwrap();
            match fault {
                "corrupt" => {}
                "foreign_keys" => source
                    .lock()
                    .unwrap()
                    .execute_batch(
                        "PRAGMA foreign_keys=OFF; UPDATE tasks SET project_id='missing';",
                    )
                    .unwrap(),
                _ => source
                    .lock()
                    .unwrap()
                    .pragma_update(
                        None,
                        "user_version",
                        fault
                            .strip_prefix("schema_")
                            .unwrap()
                            .parse::<i64>()
                            .unwrap(),
                    )
                    .unwrap(),
            }
            drop(source);
            if fault == "corrupt" {
                std::fs::write(&paths.database, b"not a SQLite database").unwrap();
            }
            let before = std::fs::read(&paths.database).unwrap();
            let error = database::open_service_locked(&paths, &lock).err().unwrap();
            let expected = match fault {
                "corrupt" => "not a database",
                "foreign_keys" => "foreign_key_violations=1",
                _ => "unsupported database schema version",
            };
            assert!(
                format!("{error:#}").contains(expected),
                "{fault}: {error:#}"
            );
            assert_eq!(std::fs::read(&paths.database).unwrap(), before);
            assert_eq!(restorepoint_snapshots(&paths), points);
            assert_eq!(database::verify(&points[0]).unwrap(), verified);
            assert!(!paths.state.join("database-restore-journal.json").exists());
            drop(lock);
            std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
            std::fs::remove_dir_all(paths.root).unwrap();
        }
    }

    #[test]
    fn restorepoint_contention_and_prepublication_failure_do_not_enable_wal() {
        let (paths, source) = restorepoint_fixture(31);
        drop(source);
        let writer = Connection::open(&paths.database).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .unwrap();
        let before = std::fs::read(&paths.database).unwrap();
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        assert!(
            format!("{error:#}").contains("database is locked"),
            "{error:#}"
        );
        assert!(restorepoint_snapshots(&paths).is_empty());
        writer.execute_batch("ROLLBACK;").unwrap();
        database::TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH.with(|armed| armed.set(true));
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        assert!(format!("{error:#}").contains("injected interruption"));
        assert!(restorepoint_snapshots(&paths).is_empty());
        assert_eq!(
            writer
                .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        assert_eq!(require_maintenance_schema(&writer).unwrap(), 31);
        assert_eq!(std::fs::read(&paths.database).unwrap(), before);
        assert!(!std::path::PathBuf::from(format!("{}-wal", paths.database.display())).exists());
        drop(writer);
        drop(lock);
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn restorepoint_old_repository_and_inventory_errors_cannot_bypass_restore_guards() {
        let (paths, source) = restorepoint_fixture(31);
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        database::TEST_AVAILABLE_CAPACITY
            .with(|values| *values.borrow_mut() = [u64::MAX, 0].into());
        assert!(database::open_service_locked(&paths, &lock).is_err());
        let points = restorepoint_snapshots(&paths);
        source
            .lock()
            .unwrap()
            .execute(
                "UPDATE projects SET repository_path=?1",
                [paths.root.parent().unwrap().to_str().unwrap()],
            )
            .unwrap();
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        assert!(format!("{error:#}").contains("overlaps registered repository"));
        assert_eq!(
            require_maintenance_schema(&source.lock().unwrap()).unwrap(),
            31
        );
        drop(lock);
        let error = database::restore(&paths, &points[0]).unwrap_err();
        assert!(format!("{error:#}").contains("inside a registered repository"));
        source
            .lock()
            .unwrap()
            .execute(
                "UPDATE projects SET repository_path=?1",
                [paths.root.to_str().unwrap()],
            )
            .unwrap();
        source
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE cmux_session_surfaces;")
            .unwrap();
        drop(source);
        let before = std::fs::read(&paths.database).unwrap();
        let error = database::restore(&paths, &points[0]).unwrap_err();
        assert!(format!("{error:#}").contains("capture inventory from readable displaced database"));
        assert_eq!(std::fs::read(&paths.database).unwrap(), before);
        assert_eq!(restorepoint_snapshots(&paths), points);
        assert!(!paths.state.join("database-restore-journal.json").exists());
        assert!(!paths.state.join("database-quarantine").exists());
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn restorepoint_manifest_must_match_the_actual_admitted_schema() {
        let (paths, source) = restorepoint_fixture(31);
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        drop(database::open_service_locked(&paths, &lock).unwrap());
        drop(source);
        drop(lock);
        let point = restorepoint_snapshots(&paths).remove(0);
        let manifest_path = point.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["sqlite_schema_version"] = 32.into();
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(database::verify(&point)
            .unwrap_err()
            .to_string()
            .contains("does not match actual SQLite schema 31"));
        let before = std::fs::read(&paths.database).unwrap();
        assert!(database::restore(&paths, &point)
            .unwrap_err()
            .to_string()
            .contains("does not match actual SQLite schema 31"));
        assert_eq!(std::fs::read(&paths.database).unwrap(), before);
        assert!(!paths.state.join("database-restore-journal.json").exists());
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn restorepoint_intermediate_failure_retains_original_recovery_and_held_journal() {
        let (paths, source) = restorepoint_fixture(31);
        source
            .lock()
            .unwrap()
            .execute_batch("CREATE VIEW provider_failure_holds AS SELECT 1 AS marker;")
            .unwrap();
        let history: String = source
            .lock()
            .unwrap()
            .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
            .unwrap();
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        assert!(
            format!("{error:#}").contains("provider_failure_holds already exists"),
            "{error:#}"
        );
        let original = restorepoint_snapshots(&paths).remove(0);
        assert!(error.to_string().contains(original.to_str().unwrap()));
        assert!(error.to_string().contains("offline recovery"));
        assert_eq!(database::verify(&original).unwrap()["schema_version"], 31);
        assert_eq!(
            require_maintenance_schema(&source.lock().unwrap()).unwrap(),
            34
        );
        assert_eq!(
            source
                .lock()
                .unwrap()
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
                .unwrap(),
            history
        );
        for _ in 0..11 {
            assert!(database::open_service_locked(&paths, &lock).is_err());
        }
        assert!(original.exists());
        assert_eq!(restorepoint_snapshots(&paths).len(), 10);
        assert_eq!(database::verify(&original).unwrap()["schema_version"], 31);
        assert!(!paths.state.join("database-restore-journal.json").exists());
        drop(source);
        drop(lock);

        let restored = database::restore(&paths, &original).unwrap();
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        database::recover_interrupted_restore_locked(&paths).unwrap();
        assert!(database::open_service_locked(&paths, &lock).is_err());
        database::recover_interrupted_restore_locked(&paths).unwrap();
        let intermediate = Store::open_maintenance_writable(&paths.database).unwrap();
        assert_eq!(
            require_maintenance_schema(&intermediate.lock().unwrap()).unwrap(),
            34
        );
        assert_eq!(
            intermediate.restore_hold().unwrap().unwrap()["operation_id"],
            restored["operation_id"]
        );
        assert!(intermediate.require_execution_unheld("dispatch").is_err());
        intermediate
            .lock()
            .unwrap()
            .execute_batch("DROP VIEW provider_failure_holds;")
            .unwrap();
        drop(intermediate);
        database::recover_interrupted_restore_locked(&paths).unwrap();
        let repaired = database::open_service_locked(&paths, &lock).unwrap();
        assert_eq!(
            require_maintenance_schema(&repaired.lock().unwrap()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            repaired.restore_hold().unwrap().unwrap()["operation_id"],
            restored["operation_id"]
        );
        drop(repaired);
        database::recover_interrupted_restore_locked(&paths).unwrap();
        drop(lock);
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn restorepoint_old_hold_pending_restarts_and_offline_release_preserve_bindings() {
        for version in SERVICE_UPGRADABLE_SCHEMA_VERSIONS {
            let (paths, source) = restorepoint_fixture(version);
            let lock = database::InstanceLock::acquire(&paths).unwrap();
            drop(database::open_service_locked(&paths, &lock).unwrap());
            drop(lock);
            drop(source);
            let point = restorepoint_snapshots(&paths).remove(0);
            for committed in [false, true] {
                let restored = database::restore(&paths, &point).unwrap();
                let journal_path = paths.state.join("database-restore-journal.json");
                let mut journal: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
                if !committed {
                    // Reconstruct the existing pre-hold boundary without replacing its bound inode.
                    std::fs::write(
                        &paths.database,
                        std::fs::read(point.join("database.sqlite3")).unwrap(),
                    )
                    .unwrap();
                    for suffix in ["-wal", "-shm"] {
                        let sidecar = std::path::PathBuf::from(format!(
                            "{}{suffix}",
                            paths.database.display()
                        ));
                        if sidecar.exists() {
                            std::fs::remove_file(sidecar).unwrap();
                        }
                    }
                }
                journal["phase"] = "hold_pending".into();
                std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
                let lock = database::InstanceLock::acquire(&paths).unwrap();
                database::recover_interrupted_restore_locked(&paths).unwrap();
                let held = Store::open_maintenance_readonly(&paths.database).unwrap();
                assert_eq!(
                    require_maintenance_schema(&held.lock().unwrap()).unwrap(),
                    version
                );
                assert_eq!(
                    held.restore_hold().unwrap().unwrap()["operation_id"],
                    restored["operation_id"]
                );
                assert_eq!(
                    std::fs::metadata(&paths.database).unwrap().ino(),
                    journal["staged_binding"]["inode"].as_u64().unwrap()
                );
                assert_eq!(journal["inventory"]["state"], "available");
                drop(held);
                if committed {
                    let journal_before = std::fs::read(&journal_path).unwrap();
                    let quarantine = std::path::PathBuf::from(
                        journal["moves"][0]["quarantine"].as_str().unwrap(),
                    );
                    let original = std::fs::read(&quarantine).unwrap();
                    let mut changed = original.clone();
                    changed.push(1);
                    std::fs::write(&quarantine, &changed).unwrap();
                    let error = database::recover_interrupted_restore_locked(&paths).unwrap_err();
                    assert!(format!("{error:#}").contains("recorded binding"));
                    assert_eq!(std::fs::read(&quarantine).unwrap(), changed);
                    assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
                    std::fs::write(&quarantine, original).unwrap();
                    let bound = paths.state.join("bound-old.sqlite3");
                    std::fs::rename(&paths.database, &bound).unwrap();
                    std::fs::copy(&bound, &paths.database).unwrap();
                    let error = database::recover_interrupted_restore_locked(&paths).unwrap_err();
                    assert!(format!("{error:#}").contains("staged-file identity"));
                    assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
                    std::fs::remove_file(&paths.database).unwrap();
                    std::fs::rename(bound, &paths.database).unwrap();
                }
                drop(lock);
                assert_eq!(
                    database::release_hold(&paths).unwrap()["automatic_resume"],
                    false
                );
                let released = Store::open_maintenance_readonly(&paths.database).unwrap();
                assert_eq!(
                    require_maintenance_schema(&released.lock().unwrap()).unwrap(),
                    version
                );
                assert!(released.restore_hold().unwrap().is_none());
                let paused: String = released.lock().unwrap().query_row(
                    "SELECT (SELECT queue_paused FROM projects)||':'||(SELECT attention FROM tasks)||':'||
                       (SELECT auto_resume_eligible FROM instance_settings)||':'||(SELECT desired_running FROM sessions)", [], |row| row.get(0),
                ).unwrap();
                assert_eq!(paused, "1:paused:0:0");
            }
            std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
            std::fs::remove_dir_all(paths.root).unwrap();
        }
    }

    #[test]
    fn restorepoint_retention_keeps_each_schema_anchor_and_ignores_unverified_content() {
        let mut host = None;
        let mut anchors = Vec::new();
        for version in SERVICE_UPGRADABLE_SCHEMA_VERSIONS {
            let (paths, source) = restorepoint_fixture(version);
            let lock = database::InstanceLock::acquire(&paths).unwrap();
            drop(database::open_service_locked(&paths, &lock).unwrap());
            drop(lock);
            drop(source);
            let point = restorepoint_snapshots(&paths).remove(0);
            let manifest_path = point.join("manifest.json");
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
            manifest["created_at"] = "2000-01-01T00:00:00Z".into();
            std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            if let Some(host_paths) = &host {
                let moved = restorepoint_root(host_paths).join(point.file_name().unwrap());
                std::fs::rename(&point, &moved).unwrap();
                anchors.push(moved);
                std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
                std::fs::remove_dir_all(paths.root).unwrap();
            } else {
                anchors.push(point);
                host = Some(paths);
            }
        }
        let host = host.unwrap();
        let root = restorepoint_root(&host);
        let copy = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            for file in ["database.sqlite3", "manifest.json"] {
                std::fs::copy(anchors[0].join(file), path.join(file)).unwrap();
            }
            path
        };
        let tied_anchor = copy("zzz-schema31-tie");
        let mismatched = copy("mismatched-schema");
        let corrupted = copy("unverified-hash");
        let unowned = copy("operator-owned");
        let manifest_path = mismatched.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["sqlite_schema_version"] = 32.into();
        manifest["created_at"] = "2999-01-01T00:00:00Z".into();
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(
            corrupted.join("database.sqlite3"),
            b"unverified operator evidence",
        )
        .unwrap();
        std::fs::write(unowned.join("operator-notes.txt"), b"retain").unwrap();
        let mut newest = serde_json::Value::Null;
        for _ in 0..12 {
            newest = database::backup(&host, None).unwrap();
        }
        assert!(
            !anchors[0].exists(),
            "the deterministic newer tie supersedes the expired schema-31 point"
        );
        assert!(tied_anchor.exists());
        for point in &anchors[1..] {
            assert!(point.exists());
        }
        assert_eq!(
            database::verify(&tied_anchor).unwrap()["schema_version"],
            31
        );
        assert!(Path::new(newest["backup"].as_str().unwrap()).exists());
        assert_eq!(
            restorepoint_snapshots(&host).len(),
            13,
            "ten eligible points plus three unrecognized points"
        );
        assert!(database::verify(&mismatched).is_err());
        assert_eq!(
            std::fs::read(corrupted.join("database.sqlite3")).unwrap(),
            b"unverified operator evidence"
        );
        assert_eq!(
            std::fs::read(unowned.join("operator-notes.txt")).unwrap(),
            b"retain"
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(host.root).unwrap();
    }

    #[test]
    fn schema_36_upgrade_preserves_migration_receipts_without_revision_triggers() {
        let (paths, old) = restorepoint_fixture(36);
        let receipt_rows =
            "SELECT json_group_array(json_array(id,attempt_id,from_workflow_id,to_workflow_id,
            preserved_json,reviewed_plan_hash,config_revision_id,authorized_at))
            FROM (SELECT * FROM trip_legacy_migrations ORDER BY id)";
        let (receipts, history): (String, String) = {
            let connection = old.lock().unwrap();
            connection.execute_batch(
                "INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,preflight_json,
                    verification_json,source_hash,overlay_hash,configuration_hash,created_at)
                 VALUES('config','p',1,'activated','{}','{}','[]','{}','source','overlay','configuration','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                 VALUES('a2','t','second-context','planning','base',1,'needs_input','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,config_revision_id,authorized_at)
                 VALUES('old-a','a','trip-v1','trip-0.9','{\"history\":true}','approved-a','config','2026-01-01T00:00:01Z'),
                       ('old-a2','a2','trip-v1','trip-0.9','{\"history\":[1,2]}','approved-a2','config','2026-01-01T00:00:02Z');"
            ).unwrap();
            (
                connection
                    .query_row(receipt_rows, [], |row| row.get(0))
                    .unwrap(),
                connection
                    .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
                    .unwrap(),
            )
        };
        drop(old);
        let upgraded = Store::open_service(&paths.database).unwrap();
        let connection = upgraded.lock().unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(receipt_rows, [], |row| row.get::<_, String>(0))
                .unwrap(),
            receipts
        );
        assert_eq!(
            connection
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
                .unwrap(),
            history
        );
        assert_eq!(connection.query_row(
            "SELECT COUNT(*) FROM trip_legacy_migrations WHERE target_workflow_hash IS NULL AND target_source_hash IS NULL
             AND target_overlay_hash IS NULL AND target_manifest_hash IS NULL", [], |row| row.get::<_, i64>(0),
        ).unwrap(), 2);
        assert_eq!(connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='trip_legacy_migrations'",
            [], |row| row.get::<_, i64>(0),
        ).unwrap(), 0);
        let revision: i64 = connection
            .query_row(
                "SELECT revision FROM state_revision WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch(
            "INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,
                config_revision_id,authorized_at,target_workflow_hash,target_source_hash,target_overlay_hash,target_manifest_hash)
             VALUES('new-a','a','trip-0.9','trip-0.11','{}','approved-new','config','2026-01-02T00:00:00Z','workflow','source','overlay','manifest');"
        ).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT revision FROM state_revision WHERE singleton=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            revision
        );
        let duplicate = connection.execute(
            "INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,config_revision_id,authorized_at)
             VALUES('duplicate','a','trip-v1','trip-0.11','{}','other-plan','config','2026-01-03T00:00:00Z')", [],
        ).unwrap_err();
        assert!(
            duplicate.to_string().contains("UNIQUE constraint failed"),
            "{duplicate}"
        );
        for sql in [
            "UPDATE trip_legacy_migrations SET preserved_json='{\"retained\":true}' WHERE id='new-a'",
            "DELETE FROM trip_legacy_migrations WHERE id='new-a'",
        ] {
            connection.execute(sql, []).unwrap();
            assert_eq!(connection.query_row("SELECT revision FROM state_revision WHERE singleton=1", [], |row| row.get::<_, i64>(0)).unwrap(), revision);
        }
        assert_eq!(
            connection
                .query_row(receipt_rows, [], |row| row.get::<_, String>(0))
                .unwrap(),
            receipts
        );
        drop(connection);
        drop(upgraded);
        std::fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn service_start_upgrades_only_schemas_thirty_one_to_thirty_seven_and_preserves_receipts() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-service-upgrade-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join("state.sqlite3");
        let store = Store::open(&database).unwrap();
        let scalar = |sql: &str| -> String {
            Connection::open(&database)
                .unwrap()
                .query_row(sql, [], |row| row.get::<_, rusqlite::types::Value>(0))
                .map(|value| format!("{value:?}"))
                .unwrap()
        };
        let set_version = |version: i64| {
            Connection::open(&database)
                .unwrap()
                .pragma_update(None, "user_version", version)
                .unwrap()
        };
        let schema = "SELECT group_concat(name||':'||COALESCE(sql,''), char(10))
             FROM (SELECT name, sql FROM sqlite_master ORDER BY name)";
        let version = "PRAGMA user_version";
        let current_schema = scalar(schema);
        schema_31_service_fixture(&store);
        drop(store);
        let history = SERVICE_FIXTURE_HISTORY;
        let schema_31 = scalar(schema);
        for refused in [
            Store::open_current_readonly(&database).err(),
            Store::open_current_writable(&database).err(),
        ] {
            assert!(refused
                .unwrap()
                .to_string()
                .contains("unsupported database schema version 31"));
        }
        assert_eq!(scalar(version), "Integer(31)");

        // A receipt that fails the rebuilt table's foreign keys rolls the whole
        // upgrade back, leaving schema 31 and every row intact.
        Connection::open(&database)
            .unwrap()
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 UPDATE final_repair_rechecks SET reviewer_session_id='missing-session';",
            )
            .unwrap();
        let broken_history = scalar(history);
        let error = Store::open_service(&database).err().unwrap();
        assert!(
            format!("{error:#}").contains("FOREIGN KEY constraint failed"),
            "{error:#}"
        );
        assert_eq!(scalar(version), "Integer(31)");
        assert_eq!(scalar(history), broken_history);
        assert_eq!(scalar(schema), schema_31);
        Connection::open(&database)
            .unwrap()
            .execute(
                "UPDATE final_repair_rechecks SET reviewer_session_id='s'",
                [],
            )
            .unwrap();
        let expected_history = scalar(history);

        for _ in 0..2 {
            drop(Store::open_service(&database).unwrap());
            assert_eq!(scalar(version), "Integer(38)");
            assert_eq!(scalar(history), expected_history);
            assert_eq!(scalar(schema), current_schema);
            assert_eq!(
                scalar(
                    "SELECT provenance_kind||':'||COALESCE(final_result_id,'none')
                     FROM final_repair_rechecks WHERE attempt_id='a'"
                ),
                "Text(\"historical_sixth_review_recovery:none\")"
            );
        }
        // Schema 32 gains only native-resolution provenance and the later migrations.
        Connection::open(&database)
            .unwrap()
            .execute_batch(&prior_service_schema_sql(32))
            .unwrap();
        drop(Store::open_service(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        // Schema 33 gains only the guidance submitted form and the later migrations.
        Connection::open(&database)
            .unwrap()
            .execute_batch(&prior_service_schema_sql(33))
            .unwrap();
        drop(Store::open_service(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        Connection::open(&database)
            .unwrap()
            .execute_batch(&prior_service_schema_sql(34))
            .unwrap();
        drop(Store::open_service(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        Connection::open(&database)
            .unwrap()
            .execute_batch(&prior_service_schema_sql(35))
            .unwrap();
        drop(Store::open_service(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        Connection::open(&database)
            .unwrap()
            .execute_batch(&prior_service_schema_sql(36))
            .unwrap();
        drop(Store::open_service(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        Connection::open(&database)
            .unwrap()
            .execute_batch(&prior_service_schema_sql(37))
            .unwrap();
        drop(Store::open_service(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        assert_eq!(
            scalar("SELECT COUNT(*) FROM codex_usage_observations"),
            "Integer(0)"
        );
        // An ordinary open of the current schema reopens it without migrating;
        // only a newer schema is refused as unknown.
        drop(Store::open(&database).unwrap());
        assert_eq!(scalar(version), "Integer(38)");
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        set_version(CURRENT_SCHEMA_VERSION + 1);
        let error = Store::open(&database).err().unwrap();
        assert!(
            error.to_string().contains("database schema 39 is newer"),
            "{error:#}"
        );
        assert_eq!(scalar(version), "Integer(39)");
        assert_eq!(scalar(history), expected_history);
        set_version(CURRENT_SCHEMA_VERSION);

        for unsupported in [0, 14, 29, 30, 39] {
            set_version(unsupported);
            let error = Store::open_service(&database).err().unwrap();
            assert!(
                error.to_string().contains(&format!(
                    "unsupported database schema version {unsupported}"
                )),
                "{error:#}"
            );
            assert_eq!(scalar(version), format!("Integer({unsupported})"));
            assert_eq!(scalar(history), expected_history);
            assert_eq!(scalar(schema), current_schema);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_first_turn_stop_binds_its_stop_and_accepts_only_a_terminal_suffix() {
        const TRUSTED: &str = "managed_process_group_untrusted_payload";
        let root =
            std::env::temp_dir().join(format!("llmrelay-setup-stop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        store.lock().unwrap().execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','validation','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','planning','base',1,'held','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','manager','claude',1,1,'running','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
               VALUES('rs','t','manager',1,'{}','g','2026-01-01T00:00:00Z');
             INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
               VALUES('c','g','hash','[]','2026-01-01T00:00:00Z');
             INSERT INTO trip_setup_permits(id,setup_operation_id,attempt_id,fixture_project_id,fixture_repository_identity,
                 role,profile_hash,settings_revision,purpose,approved_action,state,created_at)
               VALUES('permit','setup','a','p','identity','manager','profile',1,'profile_probe',
                 'nonce_only_profile_invocation','issued','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,
                 native_session_id,process_identity_json,readiness_state,setup_permit_id,validation_cell,created_at,updated_at)
               VALUES('s','g','claude','running','{}','fixture','e','native','{\"pid\":41}','idle_candidate','permit',
                 'trip_setup_probe','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        ).unwrap();
        let hook = |event: &str,
                    generation: &str,
                    native: &str,
                    provenance: &str,
                    payload: &str| {
            let connection = store.lock().unwrap();
            connection.execute(
                "INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,native_session_id,
                   payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                 VALUES(?1,'s',?2,'claude',?3,?4,?5,42,42,'peer',?6,'2026-01-01T00:00:00Z')",
                params![uuid::Uuid::new_v4().to_string(), generation, event, native, payload, provenance],
            ).unwrap();
            connection.last_insert_rowid()
        };
        let eligible =
            || eligible_setup_retained_first_turn_stop(&store.lock().unwrap(), None).unwrap();
        // The observed Claude 2.1.283 order: Stop, then SubagentStop, then a notification.
        hook("PostToolUse", "g", "native", TRUSTED, "{}");
        let stop = hook("Stop", "g", "native", TRUSTED, "{}");
        hook("SubagentStop", "g", "native", TRUSTED, "{}");
        hook(
            "Notification",
            "g",
            "native",
            TRUSTED,
            r#"{"notification_type":"idle_prompt"}"#,
        );
        let receipt = eligible().unwrap();
        assert_eq!(receipt.stop_rowid, stop);

        for (event, generation, native, provenance, payload) in [
            (
                "Notification",
                "g",
                "native",
                TRUSTED,
                r#"{"notification_type":"permission_prompt"}"#,
            ),
            (
                "Notification",
                "g",
                "native",
                TRUSTED,
                r#"{"notification_type":"push_notification"}"#,
            ),
            (
                "Notification",
                "g",
                "native",
                TRUSTED,
                r#"{"notification_type":"agent_completed"}"#,
            ),
            ("Notification", "g", "native", TRUSTED, "{}"),
            (
                "StopFailure",
                "g",
                "native",
                TRUSTED,
                r#"{"error":"overloaded"}"#,
            ),
            ("UserPromptSubmit", "g", "native", TRUSTED, "{}"),
            ("PreToolUse", "g", "native", TRUSTED, "{}"),
            ("PermissionRequest", "g", "native", TRUSTED, "{}"),
            ("UntrustedNativeEvent", "g", "native", TRUSTED, "{}"),
            ("SubagentStop", "g", "other-native", TRUSTED, "{}"),
            ("SubagentStop", "other-generation", "native", TRUSTED, "{}"),
            ("SubagentStop", "g", "native", "unmanaged", "{}"),
        ] {
            let late = hook(event, generation, native, provenance, payload);
            assert_eq!(eligible(), None, "{event} {native} {provenance} {payload}");
            // The receipt selected before this hook arrived is stale at claim time.
            assert_eq!(
                store
                    .claim_setup_retained_first_turn_stop(&receipt)
                    .unwrap(),
                ManagerServiceStopClaim::Stale,
                "{event} {payload}"
            );
            store
                .lock()
                .unwrap()
                .execute("DELETE FROM hook_events WHERE rowid=?1", params![late])
                .unwrap();
        }
        assert_eq!(
            store
                .claim_setup_retained_first_turn_stop(&receipt)
                .unwrap(),
            ManagerServiceStopClaim::Claimed
        );
        store.lock().unwrap().execute_batch(
            "UPDATE sessions SET status='exited',exit_json='{\"process_group_quiescent\":true}' WHERE id='s';
             UPDATE role_generations SET status='exited' WHERE id='g';",
        ).unwrap();
        let authority = || {
            has_completed_setup_first_turn_stop_authority(
                &store.lock().unwrap(),
                "s",
                "g",
                Some("permit"),
            )
            .unwrap()
        };
        assert!(authority());
        // Ingestion refuses the revoked credential; a row written anyway still
        // withdraws the authority under the same suffix rule.
        hook(
            "Notification",
            "g",
            "native",
            TRUSTED,
            r#"{"notification_type":"agent_needs_input"}"#,
        );
        assert!(!authority());
        drop(store);
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

#[cfg(test)]
mod attention_observation_tests {
    use super::*;
    use crate::supervisor::{LiveInvocation, ProcessSnapshot};

    const PROCESS: &str = "{\"pid\":4242}";
    const CAPACITY_NATIVE: &str = "01a0fad1-a051-7941-84b4-e64979f74d26";
    const CAPACITY_TURN: &str = "01a0fad1-a06e-7da3-8bfc-1a96b443ad5d";

    struct ObservedSession {
        root: std::path::PathBuf,
        store: Store,
        base: chrono::DateTime<Utc>,
    }

    impl ObservedSession {
        fn codex_capacity() -> Self {
            use crate::domain::{CapabilityStatus, Provider};
            let mut session = Self::new("codex-capacity");
            session.root = session.root.canonicalize().unwrap();
            session.base = Utc::now();
            let launch = LaunchConfig {
                provider: Provider::Codex,
                role: RoleKind::Implementer,
                executable: "/unused/codex".into(),
                executable_version: crate::providers::codex::EXACT_CODEX_VERSION.into(),
                model: "fixture".into(),
                effort: "max".into(),
                cwd: session.root.clone(),
                argv: Vec::new(),
                environment_keys: Vec::new(),
                permission_policy: "workspace-write".into(),
                security_policy: serde_json::json!({}),
                hook_revision: crate::providers::CODEX_HOOK_REVISION.into(),
                capability_status: CapabilityStatus::Supported,
                compatibility: Some(
                    crate::provider_compatibility::BundleSet::embedded()
                        .resolve(
                            Provider::Codex,
                            crate::providers::codex::EXACT_CODEX_VERSION,
                            RoleKind::Implementer,
                        )
                        .unwrap(),
                ),
            };
            {
                let connection = session.store.lock().unwrap();
                connection
                    .execute(
                        "UPDATE projects SET repository_path=?1",
                        params![session.root.to_str().unwrap()],
                    )
                    .unwrap();
                connection
                    .execute("UPDATE role_generations SET provider='codex'", [])
                    .unwrap();
                connection.execute("UPDATE sessions SET provider='codex',native_session_id=NULL,launch_config_json=?1,executable_version=?2",
                    params![serde_json::to_string(&launch).unwrap(), launch.executable_version]).unwrap();
                connection.execute("INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
                    VALUES('capacity-credential','g',?1,'[\"report_hook\"]','2026-01-01T00:00:00Z')",
                    params![auth::hash_secret("capacity-token")]).unwrap();
            }
            std::fs::create_dir_all(session.root.join("sessions/day")).unwrap();
            let records = [
                serde_json::json!({"type":"session_meta","payload":{"id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,
                    "cwd":session.root,"cli_version":"0.157.1","source":"cli","originator":"codex-tui","thread_source":"user"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":CAPACITY_TURN}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":CAPACITY_TURN,
                    "error":{"codex_error_info":"server_overloaded","message":"private provider text"}}}),
            ];
            std::fs::write(
                session.root.join("sessions/day/turn.jsonl"),
                records
                    .iter()
                    .map(|record| format!("{record}\n"))
                    .collect::<String>(),
            )
            .unwrap();
            session.capacity_hook("SessionStart", serde_json::json!({}));
            session.capacity_hook(
                "UserPromptSubmit",
                serde_json::json!({"turn_id":CAPACITY_TURN,
                "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
            );
            session
        }

        fn capacity_hook(&self, event: &str, mut payload: serde_json::Value) {
            let context = self.store.role_context("capacity-token").unwrap();
            payload["hook_event_name"] = serde_json::json!(event);
            payload["session_id"] = serde_json::json!(CAPACITY_NATIVE);
            payload["cwd"] = serde_json::json!(self.root);
            self.store
                .save_hook_event(
                    &context,
                    &HookEnvelope {
                        provider: context.provider,
                        payload,
                    },
                    &RolePeerProvenance {
                        peer_pid: 4242,
                        peer_process_group_id: 4242,
                        peer_start_marker: "peer".into(),
                        managed_root_pid: 4242,
                        managed_root_start_marker: "root".into(),
                        state: "managed_process_group_untrusted_payload".into(),
                    },
                )
                .unwrap();
        }

        fn enable_report_reminders(&self) {
            {
                let connection = self.store.lock().unwrap();
                connection.execute("INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,
                    detected_json,workflow_id,package_version,upstream_source_hash,overlay_hash,updated_at)
                    VALUES('p','ready','fixture','compatible','{}',?1,?2,?3,?4,'2026-01-01T00:00:00Z')",
                    params![crate::trip::WORKFLOW_ID, crate::trip::PACKAGE_VERSION,
                        crate::trip::source_hash(), crate::trip::overlay_hash()]).unwrap();
                connection
                    .execute(
                        "UPDATE attempts SET workflow_version=?1,workflow_hash=?2",
                        params![
                            crate::trip::WORKFLOW_ID,
                            crate::workflow_resources::workflow_hash()
                        ],
                    )
                    .unwrap();
            }
            self.execute("UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('true'));
                UPDATE attempts SET plan_hash='approved-plan',plan_approved_at='2026-01-01T00:00:00Z',legacy_migration_required=0;
                UPDATE sessions SET launch_state='started';
                UPDATE role_credentials SET permissions_json='[\"report_hook\",\"report_result\"]';");
            self.capacity_hook(
                "Stop",
                serde_json::json!({"background_tasks":[],"session_crons":[]}),
            );
        }

        fn read_capacity(
            &self,
        ) -> Vec<(
            CodexCapacityCandidate,
            crate::providers::codex::CapacityFileIdentity,
        )> {
            self.store
                .read_codex_capacity(&self.root.join("sessions"), &live())
                .unwrap()
        }

        fn write_usage(&self, turn: &str, responses: &[serde_json::Value]) {
            let mut records = vec![
                serde_json::json!({"type":"session_meta","payload":{"id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,
                    "cwd":self.root,"cli_version":"0.157.1","source":"cli","originator":"codex-tui","thread_source":"user"}}),
                serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}}),
            ];
            records.extend(responses.iter().cloned().map(|mut record| {
                let usage = record["payload"]["usage"].clone();
                let payload = record["payload"].as_object_mut().unwrap();
                payload
                    .entry("turn_token_usage")
                    .or_insert_with(|| usage.clone());
                payload.entry("thread_token_usage").or_insert(usage);
                record
            }));
            std::fs::write(
                self.root.join("sessions/day/turn.jsonl"),
                records
                    .iter()
                    .map(|record| format!("{record}\n"))
                    .collect::<String>(),
            )
            .unwrap();
        }

        fn read_usage(
            &self,
        ) -> Vec<(
            CodexUsageCandidate,
            crate::providers::codex::UsageHistoryRead,
        )> {
            self.store
                .read_codex_usage(&self.root.join("sessions"), &live())
                .unwrap()
        }

        fn record_usage(
            &self,
            observation: &(
                CodexUsageCandidate,
                crate::providers::codex::UsageHistoryRead,
            ),
        ) -> Result<()> {
            self.store.record_codex_usage(
                &self.root.join("sessions"),
                &observation.0,
                &observation.1,
                &live(),
            )
        }

        fn capacity_items(&self) -> Vec<crate::domain::AttentionItem> {
            crate::workflow::state(&self.store)
                .unwrap()
                .attention
                .into_iter()
                .filter(|item| item.id.starts_with("codex_capacity:"))
                .collect()
        }

        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "llmrelay-attention-{name}-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let store = Store::open(&root.join("state.sqlite3")).unwrap();
            store.lock().unwrap().execute_batch(&format!(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                   VALUES('p','p','/tmp/llmrelay-attention','attention-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
                   VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                   VALUES('a','t','context','implementation','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                   VALUES('g','a','implementer','claude',1,1,'running','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
                   VALUES('rs','t','implementer',1,'{{}}','g','2026-01-01T00:00:00Z');
                 INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,
                     native_session_id,process_identity_json,readiness_state,created_at,updated_at)
                   VALUES('s','g','claude','running','{{}}','fixture','e','native','{PROCESS}','busy',
                     '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
            ))
            .unwrap();
            let base = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc);
            Self { root, store, base }
        }

        fn at(&self, milliseconds: i64) -> chrono::DateTime<Utc> {
            self.base + chrono::Duration::milliseconds(milliseconds)
        }

        fn hook(&self, id: &str, event: &str, payload: &str, milliseconds: i64) {
            self.store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,native_session_id,
                         payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                     VALUES(?1,'s','g','claude',?2,'native',?3,42,42,'peer','managed_process_group_untrusted_payload',?4)",
                    params![id, event, payload, self.at(milliseconds).to_rfc3339()],
                )
                .unwrap();
        }

        fn observe(&self, milliseconds: i64, processes: &ProcessSnapshot, fresh_boot: bool) {
            self.store
                .observe_attention(self.at(milliseconds), processes, fresh_boot)
                .unwrap();
        }

        fn state(&self, kind: &str) -> String {
            self.store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COALESCE((SELECT state||':'||confirmations||':'||uncertain||':'
                              ||COALESCE(resolution_reason,'-')
                            FROM attention_observations WHERE kind=?1),'none')",
                    params![kind],
                    |row| row.get(0),
                )
                .unwrap()
        }

        fn revision(&self) -> i64 {
            read_state_revision(&self.store.lock().unwrap()).unwrap()
        }

        fn execute(&self, sql: &str) {
            self.store.lock().unwrap().execute_batch(sql).unwrap();
        }

        fn scalar<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
            self.store
                .lock()
                .unwrap()
                .query_row(sql, [], |row| row.get(0))
                .unwrap()
        }

        fn permission(&self) {
            self.execute("INSERT INTO permission_requests(id,hook_invocation_nonce,connection_nonce,provider,
                project_id,task_id,attempt_id,session_id,role_generation_id,role,service_boot_id,
                native_session_id,cwd,policy_fingerprint,tool_name,input_digest,input_json,
                created_at,deadline_at,state,updated_at)
                VALUES('permission','nonce','connection','claude','p','t','a','s','g','implementer',
                'boot','native','/tmp','policy','Read','digest','{}','2026-03-01T00:00:00Z',
                '2999-01-01T00:00:00Z','pending','2026-03-01T00:00:00Z');");
        }

        fn business_rows(&self) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
            let connection = self.store.lock().unwrap();
            let tables = connection
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table'
                AND name NOT IN ('attention_observations','codex_usage_observations','audit_events','state_revision')
                AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            tables
                .into_iter()
                .map(|table| {
                    let mut statement = connection
                        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                        .unwrap();
                    let columns = statement.column_count();
                    let rows = statement
                        .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap();
                    (table, rows)
                })
                .collect()
        }
    }

    impl Drop for ObservedSession {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn report_reminder_requirements_follow_each_role_and_admitted_report() {
        use crate::workflow::ReportRequirement;

        for (role, phase) in [
            (RoleKind::Manager, "planning"),
            (RoleKind::Explorer, "implementation"),
            (RoleKind::Implementer, "implementation"),
            (RoleKind::PlanReviewer, "plan_review"),
            (RoleKind::CodeReviewer, "code_review"),
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            {
                let connection = session.store.lock().unwrap();
                connection
                    .execute(
                        "UPDATE role_generations SET role=?1",
                        params![role.to_string()],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE role_settings SET role=?1",
                        params![role.to_string()],
                    )
                    .unwrap();
                connection
                    .execute(
                        "UPDATE attempts SET phase=?1,candidate_hash='candidate'",
                        params![phase],
                    )
                    .unwrap();
            }
            let (expected, invalidate, restore, metadata, outcome) = match role {
                RoleKind::Manager => {
                    session.execute("UPDATE attempts SET plan_hash=NULL,plan_approved_at=NULL");
                    (
                        ReportRequirement::ManagerPlan,
                        "UPDATE attempts SET phase='manager_handoff'",
                        "UPDATE attempts SET phase='planning'",
                        serde_json::json!({}),
                        "blocked",
                    )
                }
                RoleKind::Explorer => {
                    session.execute("INSERT INTO trip_explorer_decisions(id,attempt_id,stage,census_json,trigger,
                        activated,limits_json,role_generation_id,candidate_hash,created_at)
                        VALUES('decision','a','implementation','{}','complexity',1,'{}','g','candidate','2026-01-01T00:00:00Z')");
                    (
                        ReportRequirement::Explorer {
                            decision_id: "decision".into(),
                        },
                        "UPDATE trip_explorer_decisions SET activated=0",
                        "UPDATE trip_explorer_decisions SET activated=1",
                        serde_json::json!({"explorer_decision_id":"decision"}),
                        "needs_input",
                    )
                }
                RoleKind::Implementer => (
                    ReportRequirement::ImplementerCandidate {
                        plan_hash: "approved-plan".into(),
                    },
                    "UPDATE attempts SET plan_approved_at=NULL",
                    "UPDATE attempts SET plan_approved_at='2026-01-01T00:00:00Z'",
                    serde_json::json!({}),
                    "blocked",
                ),
                RoleKind::PlanReviewer | RoleKind::CodeReviewer => {
                    let (kind, hash) = if role == RoleKind::PlanReviewer {
                        ("plan", "approved-plan")
                    } else {
                        ("code", "candidate")
                    };
                    session.store.lock().unwrap().execute(
                        "INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,
                         session_id,prompt_hash,handoff_hash,delivery_state,created_at,updated_at)
                         VALUES('review','a',?1,?2,'g','s','prompt','handoff','delivered','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                        params![kind,hash],
                    ).unwrap();
                    (
                        ReportRequirement::Review {
                            request_id: "review".into(),
                            kind: kind.into(),
                            candidate_hash: hash.into(),
                        },
                        "UPDATE review_requests SET session_id=NULL",
                        "UPDATE review_requests SET session_id='s'",
                        serde_json::json!({"review_request_id":"review","review_kind":kind,"candidate_hash":hash}),
                        "needs_rework",
                    )
                }
                RoleKind::FinalReviewer => unreachable!(),
            };
            let selected =
                crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                    .unwrap()
                    .unwrap();
            assert_eq!(selected.role, role);
            assert_eq!(selected.requirement, expected);
            session.execute(invalidate);
            assert!(crate::workflow::report_reminder_binding(
                &session.store.lock().unwrap(),
                "s",
                None
            )
            .unwrap()
            .is_none());
            assert!(session
                .store
                .reserve_report_reminder(&selected)
                .unwrap()
                .is_none());
            assert_eq!(
                session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),
                0
            );
            session.execute(restore);
            let reminder = session
                .store
                .reserve_report_reminder(&selected)
                .unwrap()
                .unwrap();
            let context = session.store.role_context("capacity-token").unwrap();
            let report = session
                .store
                .save_role_result(
                    &context,
                    &RoleResultReport {
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        outcome: outcome.into(),
                        summary: "The assigned work needs a decision.".into(),
                        evidence: vec![],
                        metadata,
                    },
                )
                .unwrap();
            assert_eq!(report["accepted"], true);
            session.execute("UPDATE role_results SET consumed_at='2999-01-01T00:00:00Z'");
            let id = reminder["guidance_id"].as_str().unwrap();
            assert!(crate::workflow::report_reminder_binding(
                &session.store.lock().unwrap(),
                "s",
                Some(id)
            )
            .unwrap()
            .is_none());
            assert_eq!(
                session
                    .store
                    .cancel_stale_report_reminder(id)
                    .unwrap()
                    .unwrap()["state"],
                "cancelled"
            );
            assert_eq!(
                session.scalar::<i64>(
                    "SELECT COUNT(*) FROM guidance_messages WHERE written_at IS NOT NULL"
                ),
                0
            );
        }
    }

    #[test]
    fn report_reminder_old_turn_invocation_and_review_reports_do_not_satisfy_new_work() {
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        let context = session.store.role_context("capacity-token").unwrap();
        session
            .store
            .save_role_result(
                &context,
                &RoleResultReport {
                    operation_id: "old-report".into(),
                    outcome: "blocked".into(),
                    summary: "An earlier turn was blocked.".into(),
                    evidence: vec![],
                    metadata: serde_json::json!({}),
                },
            )
            .unwrap();
        assert!(crate::workflow::report_reminder_binding(
            &session.store.lock().unwrap(),
            "s",
            None
        )
        .unwrap()
        .is_none());
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"prompt":"new obligation"}),
        );
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        assert!(crate::workflow::report_reminder_binding(
            &session.store.lock().unwrap(),
            "s",
            None
        )
        .unwrap()
        .is_some());
        session.store.lock().unwrap().execute(
            "INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,
             capability_key,capability_identity_json,state,hook_event_boundary_rowid,created_at,updated_at)
             VALUES('resume','s',1,'resumed','{}','fixture','{}','running',(SELECT MAX(rowid) FROM hook_events),?1,?1)",
            params![Utc::now().to_rfc3339()],
        ).unwrap();
        session.execute("UPDATE sessions SET transcript_epoch='resumed',readiness_state='unknown'");
        session.capacity_hook("SessionStart", serde_json::json!({}));
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"prompt":"resumed obligation"}),
        );
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        assert!(crate::workflow::report_reminder_binding(
            &session.store.lock().unwrap(),
            "s",
            None
        )
        .unwrap()
        .is_some());

        for (role, phase, kind, hash) in [
            ("plan_reviewer", "plan_review", "plan", "approved-plan"),
            ("code_reviewer", "code_review", "code", "candidate"),
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            {
                let connection = session.store.lock().unwrap();
                connection
                    .execute("UPDATE role_generations SET role=?1", params![role])
                    .unwrap();
                connection
                    .execute("UPDATE role_settings SET role=?1", params![role])
                    .unwrap();
                connection
                    .execute(
                        "UPDATE attempts SET phase=?1,candidate_hash='candidate'",
                        params![phase],
                    )
                    .unwrap();
                connection.execute(
                    "INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,
                     session_id,prompt_hash,handoff_hash,delivery_state,created_at,updated_at)
                     VALUES('old-review','a',?1,?2,'g','s','prompt','handoff','delivered','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                    params![kind,hash],
                ).unwrap();
            }
            let context = session.store.role_context("capacity-token").unwrap();
            session.store.save_role_result(&context,&RoleResultReport {
                operation_id:"old-review-report".into(),outcome:"approved".into(),summary:"Earlier review complete.".into(),
                evidence:vec![],metadata:serde_json::json!({"review_request_id":"old-review","review_kind":kind,"candidate_hash":hash}),
            }).unwrap();
            assert!(crate::workflow::report_reminder_binding(
                &session.store.lock().unwrap(),
                "s",
                None
            )
            .unwrap()
            .is_none());
            session.execute("UPDATE review_requests SET delivery_state='finished';
                INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,session_id,
                    prompt_hash,handoff_hash,delivery_state,created_at,updated_at)
                    SELECT 'current-review',attempt_id,review_kind,candidate_hash,role_generation_id,session_id,
                        prompt_hash,handoff_hash,'delivered',created_at,updated_at FROM review_requests WHERE id='old-review';");
            let selected =
                crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                    .unwrap()
                    .unwrap();
            assert!(
                matches!(selected.requirement,crate::workflow::ReportRequirement::Review { ref request_id,.. } if request_id=="current-review")
            );
            session.execute("UPDATE review_requests SET candidate_hash='stale-candidate'");
            assert!(session
                .store
                .reserve_report_reminder(&selected)
                .unwrap()
                .is_none());
            assert_eq!(
                session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),
                0
            );
        }
    }

    #[test]
    fn report_reminder_setup_probe_final_and_unsafe_sessions_cannot_reserve() {
        for mutation in [
            "UPDATE sessions SET setup_permit_id='setup'",
            "UPDATE attempts SET setup_operation_id='setup'",
            "UPDATE sessions SET validation_cell='trip_runtime_probe'",
            "UPDATE attempts SET status='capability_validation'",
            "UPDATE projects SET internal_purpose='capability_validation'",
            "UPDATE trip_project_state SET readiness='needs_attention'",
            "UPDATE attempts SET workflow_hash='obsolete-workflow'",
            "UPDATE role_generations SET role='final_verifier'; UPDATE role_settings SET role='final_verifier'",
            "UPDATE role_generations SET role='final_reviewer'; UPDATE role_settings SET role='final_reviewer'",
            "UPDATE role_generations SET lane_id='worker-lane'; UPDATE sessions SET lane_id='worker-lane'",
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            let selected = crate::workflow::report_reminder_binding(&session.store.lock().unwrap(),"s",None).unwrap().unwrap();
            session.execute(mutation);
            assert!(crate::workflow::report_reminder_binding(&session.store.lock().unwrap(),"s",None).unwrap().is_none(),"{mutation}");
            assert!(session.store.reserve_report_reminder(&selected).unwrap().is_none(),"{mutation}");
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),0);
        }
        for (provider, event, payload) in [
            (
                "codex",
                "UserPromptSubmit",
                serde_json::json!({"prompt":"still working"}),
            ),
            ("claude", "Stop", serde_json::json!({})),
            (
                "claude",
                "Stop",
                serde_json::json!({"background_tasks":[{"id":"active"}],"session_crons":[]}),
            ),
            (
                "codex",
                "StopFailure",
                serde_json::json!({"error":"turn failed"}),
            ),
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            let selected =
                crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                    .unwrap()
                    .unwrap();
            {
                let connection = session.store.lock().unwrap();
                connection
                    .execute("UPDATE role_generations SET provider=?1", params![provider])
                    .unwrap();
                connection
                    .execute("UPDATE sessions SET provider=?1", params![provider])
                    .unwrap();
            }
            session.capacity_hook("UserPromptSubmit", serde_json::json!({"prompt":"new work"}));
            session.capacity_hook(event, payload);
            assert!(
                crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                    .unwrap()
                    .is_none(),
                "{event}"
            );
            assert!(session
                .store
                .reserve_report_reminder(&selected)
                .unwrap()
                .is_none());
            assert_eq!(
                session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),
                0
            );
        }
    }

    #[test]
    fn report_reminder_cap_survives_concurrent_reservation_noise_reopen_and_resume() {
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let first = std::thread::scope(|scope| {
            let workers = (0..2)
                .map(|_| {
                    let barrier = barrier.clone();
                    let selected = &selected;
                    let path = session.root.join("state.sqlite3");
                    scope.spawn(move || {
                        let store = Store::open(&path).unwrap();
                        barrier.wait();
                        store.reserve_report_reminder(selected).unwrap()
                    })
                })
                .collect::<Vec<_>>();
            let reservations = workers
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(reservations.len(), 1);
            reservations.into_iter().next().unwrap()
        });
        let first_id = first["guidance_id"].as_str().unwrap();
        session.execute("UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('false')); ");
        session
            .store
            .cancel_stale_report_reminder(first_id)
            .unwrap()
            .unwrap();
        session.execute("UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('true'));");
        assert!(session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .is_none());
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        let same_turn =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        assert!(session
            .store
            .reserve_report_reminder(&same_turn)
            .unwrap()
            .is_none());
        {
            let connection = session.store.lock().unwrap();
            for index in 0..510 {
                connection.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                    VALUES(?1,?1,'service','fixture.noise','session','s','{}',?2)",params![format!("noise-{index}"),Utc::now().to_rfc3339()]).unwrap();
            }
            connection.execute_batch("INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,
                capability_key,capability_identity_json,state,hook_event_boundary_rowid,created_at,updated_at)
                VALUES('resume','s',1,'resumed','{}','fixture','{}','running',(SELECT MAX(rowid) FROM hook_events),'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                UPDATE sessions SET transcript_epoch='resumed',readiness_state='unknown';").unwrap();
        }
        session.capacity_hook("SessionStart", serde_json::json!({}));
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"prompt":"report if finished"}),
        );
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        let resumed = Store::open(&session.root.join("state.sqlite3")).unwrap();
        let selected =
            crate::workflow::report_reminder_binding(&resumed.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        let second = resumed.reserve_report_reminder(&selected).unwrap().unwrap();
        assert_eq!(second["ordinal"], 2);
        session.execute("UPDATE guidance_messages SET state='delivery_unknown',reason='uncertain write' WHERE state='queued';");
        assert_eq!(
            report_reminder_reservations(&resumed.lock().unwrap(), "g")
                .unwrap()
                .len(),
            2
        );
        session.execute(
            "UPDATE guidance_messages SET state='abandoned' WHERE state='delivery_unknown';",
        );
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"prompt":"new obligation"}),
        );
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        let latest = crate::workflow::report_reminder_binding(&resumed.lock().unwrap(), "s", None)
            .unwrap()
            .unwrap();
        assert!(resumed.reserve_report_reminder(&latest).unwrap().is_none());
        session.execute("UPDATE role_generations SET status='replaced' WHERE id='g';
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
              SELECT 'g-new',attempt_id,role,provider,2,config_revision,'running','new',created_at,updated_at FROM role_generations WHERE id='g';
            UPDATE role_settings SET effective_generation_id='g-new'; UPDATE role_credentials SET role_generation_id='g-new';
            UPDATE sessions SET role_generation_id='g-new',transcript_epoch='new-generation',initial_hook_event_boundary_rowid=(SELECT MAX(rowid) FROM hook_events);");
        session.capacity_hook("SessionStart", serde_json::json!({}));
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"prompt":"new generation"}),
        );
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        let replacement =
            crate::workflow::report_reminder_binding(&resumed.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        assert_eq!(
            resumed
                .reserve_report_reminder(&replacement)
                .unwrap()
                .unwrap()["ordinal"],
            1
        );
    }

    #[test]
    fn report_reminder_selection_rechecks_policy_report_turn_and_permission() {
        for mutation in [
            "UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('false'))",
            "INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at)
             VALUES('report','report','s','g','blocked','blocked','[]','{}','2999-01-01T00:00:00Z')",
            "UPDATE tasks SET attention='paused'",
            "UPDATE role_settings SET effective_generation_id=NULL",
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            let selected = crate::workflow::report_reminder_binding(&session.store.lock().unwrap(),"s",None).unwrap().unwrap();
            session.execute(mutation);
            assert!(session.store.reserve_report_reminder(&selected).unwrap().is_none(),"{mutation}");
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),0);
        }
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        session.capacity_hook("UserPromptSubmit", serde_json::json!({"prompt":"new turn"}));
        assert!(session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .is_none());
        session.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        session.permission();
        assert!(session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .is_none());
    }

    #[test]
    fn report_reminder_cancellation_never_overwrites_a_new_turn_or_invocation() {
        for changed in ["turn", "invocation", "uncertain"] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            let selected =
                crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                    .unwrap()
                    .unwrap();
            let reminder = session
                .store
                .reserve_report_reminder(&selected)
                .unwrap()
                .unwrap();
            let id = reminder["guidance_id"].as_str().unwrap();
            assert!(session
                .store
                .reserve_guidance_delivery(id, "s", "e", None, REPORT_REMINDER_PROMPT)
                .unwrap());
            match changed {
                "turn" => {
                    session
                        .capacity_hook("UserPromptSubmit", serde_json::json!({"prompt":"later"}));
                    session.capacity_hook(
                        "Stop",
                        serde_json::json!({"background_tasks":[],"session_crons":[]}),
                    );
                }
                "invocation" => session.execute(
                    "UPDATE sessions SET transcript_epoch='replaced',readiness_state='unknown'",
                ),
                "uncertain" => {
                    session.execute("UPDATE guidance_messages SET state='delivery_unknown'")
                }
                _ => unreachable!(),
            }
            let before: String =
                session.scalar("SELECT readiness_state FROM sessions WHERE id='s'");
            let cancellation = session
                .store
                .cancel_report_reminder_before_write(id, "stale reservation");
            if changed == "uncertain" {
                assert!(cancellation.is_err());
            } else {
                assert_eq!(cancellation.unwrap()["readiness_restored"], false);
            }
            assert_eq!(
                session.scalar::<String>("SELECT readiness_state FROM sessions WHERE id='s'"),
                before
            );
            assert_eq!(
                report_reminder_reservations(&session.store.lock().unwrap(), "g")
                    .unwrap()
                    .len(),
                1
            );
        }
    }

    fn live() -> ProcessSnapshot {
        ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "s".into(),
            role_generation_id: "g".into(),
            transcript_epoch: "e".into(),
            process_identity_json: PROCESS.into(),
        }])
    }

    #[test]
    fn codex_usage_deduplicates_committed_records_and_preserves_authority() {
        use crate::domain::ObservedUsageStatus;
        let session = ObservedSession::codex_capacity();
        let response = |id: &str, count| {
            serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":id,"usage":{"input_tokens":count,
                "cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":count},
            "turn_token_usage":{"input_tokens":999,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":999},
            "thread_token_usage":{"input_tokens":9999,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":9999}}})
        };
        let mut measured = response("a", 15);
        measured["payload"]["usage"]["cache_write_input_tokens"] = serde_json::json!(3);
        let records = [measured, response("b", 15), response("zero", 0)];
        session.write_usage(CAPACITY_TURN, &records);
        session.capacity_hook("Stop", serde_json::json!({"turn_id":CAPACITY_TURN}));
        let observations = session.read_usage();
        assert_eq!(
            observations.len(),
            1,
            "matching Stop must not suppress observed usage"
        );
        let before = session.business_rows();
        let revision = session.revision();
        session.record_usage(&observations[0]).unwrap();
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.revision(), revision + 3);
        let state = crate::workflow::state(&session.store).unwrap();
        let task = state.tasks[0].observed_usage.as_ref().unwrap();
        assert_eq!(task.status, ObservedUsageStatus::Observed);
        assert_eq!(task.observed_responses, Some(3));
        assert_eq!(task.counters.as_ref().unwrap().total_tokens, 30);
        assert_eq!(
            task.counters.as_ref().unwrap().cache_write_input_tokens,
            None
        );
        assert_eq!(
            state.active_sessions[0]["observed_usage"]["current_turn"]["counters"]["total_tokens"],
            30
        );
        assert!(
            session.read_usage().is_empty(),
            "unchanged source must skip parsing"
        );
        session.record_usage(&observations[0]).unwrap();
        assert_eq!(session.revision(), revision + 3);
        let mut changed = records.to_vec();
        changed.push(records[2].clone());
        session.write_usage(CAPACITY_TURN, &changed);
        assert_eq!(
            session.read_usage().len(),
            1,
            "changed identity must be read again"
        );
        let restarted = Store::open(&session.root.join("state.sqlite3")).unwrap();
        let reread = restarted
            .read_codex_usage(&session.root.join("sessions"), &live())
            .unwrap();
        restarted
            .record_codex_usage(
                &session.root.join("sessions"),
                &reread[0].0,
                &reread[0].1,
                &live(),
            )
            .unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            3
        );
        assert_eq!(
            crate::workflow::state(&restarted).unwrap().tasks[0]
                .observed_usage
                .as_ref()
                .unwrap()
                .counters
                .as_ref()
                .unwrap()
                .total_tokens,
            30
        );
        session.execute("UPDATE sessions SET transcript_epoch='next';
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,
                capability_key,capability_identity_json,state,created_at,updated_at,hook_event_boundary_rowid)
            VALUES('resume','s',1,'next','{}','fixture','{}','running','2026-01-01T00:00:00Z',
                '2026-01-01T00:00:00Z',(SELECT MAX(rowid) FROM hook_events));");
        session.capacity_hook("SessionStart", serde_json::json!({}));
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":"next-turn",
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        let mut next = response("next-response", 7);
        next["payload"]["turn_id"] = serde_json::json!("next-turn");
        next["payload"]["root_turn_id"] = serde_json::json!("next-turn");
        session.write_usage("next-turn", &[records[0].clone(), next]);
        let processes = ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "s".into(),
            role_generation_id: "g".into(),
            transcript_epoch: "next".into(),
            process_identity_json: PROCESS.into(),
        }]);
        let observed = session
            .store
            .read_codex_usage(&session.root.join("sessions"), &processes)
            .unwrap();
        assert_eq!(observed.len(), 1);
        session
            .store
            .record_codex_usage(
                &session.root.join("sessions"),
                &observed[0].0,
                &observed[0].1,
                &processes,
            )
            .unwrap();
        let state = crate::workflow::state(&session.store).unwrap();
        assert_eq!(
            state.active_sessions[0]["observed_usage"]["current_turn"]["counters"]["total_tokens"],
            7
        );
        assert_eq!(
            state.active_sessions[0]["observed_usage"]["session"]["counters"]["total_tokens"],
            37
        );
        assert_eq!(
            session.scalar::<String>(
                "SELECT transcript_epoch FROM codex_usage_observations WHERE response_id='a'"
            ),
            "e"
        );
    }

    #[test]
    fn codex_usage_revalidation_and_transaction_failure_never_advance_cache() {
        let session = ObservedSession::codex_capacity();
        session.write_usage(CAPACITY_TURN, &[]);
        let empty = session.read_usage();
        session.record_usage(&empty[0]).unwrap();
        assert!(
            session.read_usage().is_empty(),
            "valid empty read can be cached"
        );
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":CAPACITY_TURN,
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        assert_eq!(
            session.read_usage().len(),
            1,
            "new accepted binding requires a fresh read"
        );
        let record = serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"one","usage":{"input_tokens":0,
                "cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":0}}});
        session.write_usage(CAPACITY_TURN, &[record.clone()]);
        let observed = session.read_usage();
        session.execute(
            "CREATE TRIGGER fail_usage BEFORE INSERT ON codex_usage_observations
            BEGIN SELECT RAISE(ABORT,'usage persistence fixture failure'); END;",
        );
        let before = session.business_rows();
        let revision = session.revision();
        let error = session.record_usage(&observed[0]).unwrap_err();
        assert!(error
            .to_string()
            .contains("usage persistence fixture failure"));
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.revision(), revision);
        assert_eq!(
            session.read_usage().len(),
            1,
            "failed persistence cannot cache the new identity"
        );
        session.execute("DROP TRIGGER fail_usage;");
        session.write_usage(CAPACITY_TURN, &[]);
        session.record_usage(&observed[0]).unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            0,
            "changed file identity after reading must discard the source"
        );
        session.write_usage(CAPACITY_TURN, &[record.clone()]);
        let fresh = session.read_usage();
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":"later-turn",
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        session.record_usage(&fresh[0]).unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            0
        );
        for turn in ["x".repeat(129), "invalid\u{0085}turn".into()] {
            session.capacity_hook(
                "UserPromptSubmit",
                serde_json::json!({"turn_id":turn,
                "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
            );
            assert!(session.read_usage().is_empty());
        }
        let session = ObservedSession::codex_capacity();
        let cwd = session.root.join("working");
        std::fs::create_dir(&cwd).unwrap();
        {
            let connection = session.store.lock().unwrap();
            connection
                .execute(
                    "UPDATE projects SET repository_path=?1",
                    params![cwd.to_str().unwrap()],
                )
                .unwrap();
            connection.execute("UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.cwd',?1)",
                params![cwd.to_str().unwrap()]).unwrap();
            connection.execute("UPDATE hook_events SET payload_json=json_set(payload_json,'$.cwd',?1) WHERE event_name='UserPromptSubmit'",
                params![cwd.to_str().unwrap()]).unwrap();
        }
        session.write_usage(CAPACITY_TURN, &[record]);
        let path = session.root.join("sessions/day/turn.jsonl");
        let mut records: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        records[0]["payload"]["cwd"] = serde_json::json!(cwd);
        std::fs::write(
            &path,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let original = session.read_usage();
        assert_eq!(original.len(), 1);
        std::fs::rename(&cwd, session.root.join("previous-working")).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        session.record_usage(&original[0]).unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            0
        );
        let replacement = session.read_usage();
        assert_eq!(original[0].0.canonical_cwd, replacement[0].0.canonical_cwd);
        assert_ne!(original[0].0.cwd_identity, replacement[0].0.cwd_identity);
    }

    #[test]
    fn codex_usage_conflicts_keep_original_provenance_and_invalidate_both_turn_owners() {
        use crate::domain::ObservedUsageStatus;
        let session = ObservedSession::codex_capacity();
        let mut record = serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"conflict","usage":{"input_tokens":10,
                "cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":10}}});
        session.write_usage(CAPACITY_TURN, &[record.clone()]);
        let original = session.read_usage();
        session.record_usage(&original[0]).unwrap();
        let provenance: String = session.scalar("SELECT accepted_hook_event_id||':'||transcript_epoch||':'||input_tokens FROM codex_usage_observations");
        record["payload"]["usage"]["input_tokens"] = serde_json::json!(200);
        record["payload"]["usage"]["total_tokens"] = serde_json::json!(200);
        session.write_usage(CAPACITY_TURN, &[record]);
        let conflict = session.read_usage();
        session.record_usage(&conflict[0]).unwrap();
        let revision = session.revision();
        session.record_usage(&conflict[0]).unwrap();
        assert_eq!(
            session.revision(),
            revision,
            "identical conflict replay is idempotent"
        );
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":CAPACITY_TURN,
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        let reassignment = session.read_usage();
        session.record_usage(&reassignment[0]).unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            1
        );
        assert_eq!(session.scalar::<String>("SELECT accepted_hook_event_id||':'||transcript_epoch||':'||input_tokens FROM codex_usage_observations"), provenance);
        let reopened = Store::open(&session.root.join("state.sqlite3")).unwrap();
        let state = crate::workflow::state(&reopened).unwrap();
        let task = state.tasks[0].observed_usage.as_ref().unwrap();
        assert_eq!(task.status, ObservedUsageStatus::Invalid);
        assert!(task.counters.is_none());
        assert_eq!(
            state.active_sessions[0]["observed_usage"]["current_turn"]["status"],
            "invalid"
        );
        assert!(state.active_sessions[0]["observed_usage"]["current_turn"]["counters"].is_null());
        assert_eq!(
            session.scalar::<i64>(
                "SELECT json_array_length(conflict_owners_json) FROM codex_usage_observations"
            ),
            2
        );
    }

    #[test]
    fn codex_usage_task_projection_survives_session_window_and_rejects_overflow() {
        use crate::domain::ObservedUsageStatus;
        let session = ObservedSession::codex_capacity();
        let response = |id: &str| {
            serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":id,"usage":{"input_tokens":15,
                "cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":15}}})
        };
        session.write_usage(CAPACITY_TURN, &[response("one"), response("two")]);
        let observations = session.read_usage();
        session.execute("UPDATE sessions SET status='exited',exit_json='{}'");
        session.record_usage(&observations[0]).unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            0,
            "exit after reading retires the binding"
        );
        session.execute("UPDATE sessions SET status='running',exit_json=NULL");
        session
            .store
            .record_codex_usage(
                &session.root.join("sessions"),
                &observations[0].0,
                &observations[0].1,
                &ProcessSnapshot::Unavailable,
            )
            .unwrap();
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
            0
        );
        session.record_usage(&observations[0]).unwrap();
        session.execute("UPDATE sessions SET status='exited',exit_json='{}'");
        for index in 0..201 {
            session.store.lock().unwrap().execute("INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,
                executable_version,transcript_epoch,created_at,updated_at)
                VALUES(?1,'g','codex','exited','{}','fixture',?1,'2026-02-01T00:00:00Z','2026-02-01T00:00:00Z')",
                params![format!("newer-{index}")]).unwrap();
        }
        let state = crate::workflow::state(&session.store).unwrap();
        assert!(!state.active_sessions.iter().any(|row| row["id"] == "s"));
        assert_eq!(
            state.tasks[0]
                .observed_usage
                .as_ref()
                .unwrap()
                .counters
                .as_ref()
                .unwrap()
                .total_tokens,
            30
        );
        assert!(session.read_usage().is_empty());
        assert!(session.store.codex_usage_cache.lock().unwrap().is_empty());
        let revision = session.revision();
        session.store.lock().unwrap().execute("UPDATE codex_usage_observations SET input_tokens=?1,total_tokens=?1 WHERE response_id='two'",
            params![crate::domain::MAX_OBSERVED_TOKENS]).unwrap();
        assert_eq!(session.revision(), revision + 1);
        let usage = crate::workflow::state(&session.store).unwrap().tasks[0]
            .observed_usage
            .clone()
            .unwrap();
        assert_eq!(usage.status, ObservedUsageStatus::Invalid);
        assert!(usage.counters.is_none());
        session.execute("DELETE FROM codex_usage_observations WHERE response_id='two'");
        assert_eq!(session.revision(), revision + 2);
        assert_eq!(
            crate::workflow::state(&session.store).unwrap().tasks[0]
                .observed_usage
                .as_ref()
                .unwrap()
                .counters
                .as_ref()
                .unwrap()
                .total_tokens,
            15
        );
    }

    #[test]
    fn codex_capacity_pipeline_preserves_authority_and_immutable_frontier() {
        let session = ObservedSession::codex_capacity();
        session.capacity_hook(
            "PreToolUse",
            serde_json::json!({"turn_id":CAPACITY_TURN,"tool_use_id":"before"}),
        );
        let observations = session.read_capacity();
        assert_eq!(
            observations.len(),
            1,
            "activity before capture does not mask a later native completion"
        );
        let (candidate, source) = &observations[0];
        for event in ["PostToolUse", "Stop", "Interrupt"] {
            for payload in [
                serde_json::json!({"turn_id":"other"}),
                serde_json::json!({}),
                serde_json::json!({"turn_id":12}),
                serde_json::json!({"prompt_id":CAPACITY_TURN}),
            ] {
                session.capacity_hook(event, payload);
            }
        }
        let before = session.business_rows();
        let revision = session.revision();
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.revision(), revision + 1);
        let items = session.capacity_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].reason, "Codex reported that the selected model is at capacity; open agent output to inspect it and choose the next action.");
        assert_eq!(
            serde_json::to_value(&items[0].target).unwrap(),
            serde_json::json!({"kind":"session",
            "project_id":"p","task_id":"t","attempt_id":"a","session_id":"s","role_generation_id":"g"})
        );
        assert_eq!(
            serde_json::to_value(&items[0].action).unwrap()["kind"],
            "open_agent_output"
        );
        let detail: String = session.scalar("SELECT detail_json FROM audit_events WHERE event_code='provider.codex_capacity_observed'");
        assert!(!detail.contains("private provider text") && !detail.contains("turn.jsonl"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&detail).unwrap()["hook_frontier"],
            candidate.hook_frontier
        );
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        assert!(session.read_capacity().is_empty());
        assert_eq!(session.revision(), revision + 1);
        session.capacity_hook(
            "PostToolUse",
            serde_json::json!({"turn_id":CAPACITY_TURN,"tool_use_id":"after"}),
        );
        let retired_revision = session.revision();
        assert!(session.capacity_items().is_empty());
        assert!(session.read_capacity().is_empty());
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        assert_eq!(session.revision(), retired_revision);
        assert_eq!(session.scalar::<String>("SELECT detail_json FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), detail);
        assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 1);
    }

    #[test]
    fn codex_capacity_commit_revalidates_identity_attempt_and_activity() {
        for mutation in [
            "UPDATE sessions SET transcript_epoch='new'",
            "UPDATE sessions SET native_session_id='01a0fad3-1e45-71e2-8686-83184958e2c9'",
            "UPDATE sessions SET process_identity_json='{\"pid\":9999}'",
            "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
             SELECT 'replacement',attempt_id,role,provider,2,config_revision,status,authority_generation,created_at,updated_at FROM role_generations WHERE id='g';
             UPDATE sessions SET role_generation_id='replacement'; UPDATE role_settings SET effective_generation_id='replacement'",
            "UPDATE role_settings SET effective_generation_id=NULL",
            "UPDATE role_generations SET status='replaced'",
            "UPDATE projects SET repository_path='/changed'",
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.cwd','/changed')",
            "UPDATE hook_events SET payload_json=json_set(payload_json,'$.cwd','/changed') WHERE event_name='UserPromptSubmit'",
            "UPDATE hook_events SET payload_json=json_set(payload_json,'$.transcript_path','/changed') WHERE event_name='UserPromptSubmit'",
            "UPDATE attempts SET status='completed'",
            "UPDATE sessions SET status='recovery_required'",
            "UPDATE sessions SET status='exited',exit_json='{}'",
        ] {
            let session = ObservedSession::codex_capacity();
            let observations = session.read_capacity();
            assert_eq!(observations.len(), 1);
            session.execute(mutation);
            let before = session.business_rows();
            let revision = session.revision();
            session.store.record_codex_capacity(&observations[0].0, &observations[0].1, &live()).unwrap();
            assert!(session.capacity_items().is_empty(), "{mutation}");
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 0);
            assert_eq!(session.business_rows(), before);
            assert_eq!(session.revision(), revision);
        }
        for event in [
            "PreToolUse",
            "PostToolUse",
            "PermissionRequest",
            "SubagentStart",
            "SubagentStop",
            "Stop",
            "Interrupt",
            "SessionEnd",
            "SessionStart",
            "UserPromptSubmit",
        ] {
            let session = ObservedSession::codex_capacity();
            let observations = session.read_capacity();
            let payload = if matches!(event, "SessionEnd" | "SessionStart") {
                serde_json::json!({})
            } else {
                serde_json::json!({"turn_id":CAPACITY_TURN,
                    "transcript_path":session.root.join("sessions/day/turn.jsonl")})
            };
            session.capacity_hook(event, payload);
            let revision = session.revision();
            session
                .store
                .record_codex_capacity(&observations[0].0, &observations[0].1, &live())
                .unwrap();
            assert_eq!(session.revision(), revision, "{event}");
            assert!(session.capacity_items().is_empty(), "{event}");
        }
        let session = ObservedSession::codex_capacity();
        let observations = session.read_capacity();
        for processes in [
            ProcessSnapshot::Unavailable,
            ProcessSnapshot::Read(vec![]),
            ProcessSnapshot::Read(vec![LiveInvocation {
                session_id: "s".into(),
                role_generation_id: "g".into(),
                transcript_epoch: "old".into(),
                process_identity_json: PROCESS.into(),
            }]),
            ProcessSnapshot::Read(vec![LiveInvocation {
                session_id: "s".into(),
                role_generation_id: "other".into(),
                transcript_epoch: "e".into(),
                process_identity_json: PROCESS.into(),
            }]),
            ProcessSnapshot::Read(vec![LiveInvocation {
                session_id: "s".into(),
                role_generation_id: "g".into(),
                transcript_epoch: "e".into(),
                process_identity_json: "{\"pid\":99}".into(),
            }]),
            ProcessSnapshot::Read(vec![LiveInvocation {
                session_id: "other".into(),
                role_generation_id: "g".into(),
                transcript_epoch: "e".into(),
                process_identity_json: PROCESS.into(),
            }]),
        ] {
            session
                .store
                .record_codex_capacity(&observations[0].0, &observations[0].1, &processes)
                .unwrap();
            assert!(session.capacity_items().is_empty());
            assert!(session
                .store
                .read_codex_capacity(&session.root.join("sessions"), &processes)
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn codex_capacity_selection_requires_admitted_explicit_current_turn() {
        for mutation in [
            "UPDATE sessions SET executable_version='codex-cli 0.159.2'",
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.compatibility.synthetic_origin',json('true'))",
            "UPDATE sessions SET launch_config_json=json_remove(launch_config_json,'$.compatibility')",
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.compatibility.effective_hash','wrong')",
            "UPDATE hook_events SET event_name='UntrustedNativeEvent' WHERE event_name='UserPromptSubmit'",
            "DELETE FROM hook_events WHERE event_name='SessionStart'",
        ] {
            let session = ObservedSession::codex_capacity();
            session.execute(mutation);
            assert!(session.read_capacity().is_empty(), "{mutation}");
        }
        for turn in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!(4),
            serde_json::json!("wrong-turn"),
        ] {
            let session = ObservedSession::codex_capacity();
            session.capacity_hook(
                "UserPromptSubmit",
                serde_json::json!({"turn_id":turn,"prompt_id":CAPACITY_TURN,
                "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
            );
            assert!(session.read_capacity().is_empty());
        }
        for event in ["Stop", "Interrupt", "SessionEnd"] {
            let session = ObservedSession::codex_capacity();
            session.capacity_hook(
                event,
                if event == "SessionEnd" {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"turn_id":CAPACITY_TURN})
                },
            );
            assert!(
                session.read_capacity().is_empty(),
                "{event} before snapshot"
            );
        }
    }

    #[test]
    fn codex_capacity_projection_retires_current_binding_without_new_audits() {
        for event in [
            "UserPromptSubmit",
            "SessionStart",
            "SessionEnd",
            "Stop",
            "Interrupt",
            "PreToolUse",
            "PostToolUse",
            "PermissionRequest",
            "SubagentStart",
            "SubagentStop",
        ] {
            let session = ObservedSession::codex_capacity();
            let observations = session.read_capacity();
            session
                .store
                .record_codex_capacity(&observations[0].0, &observations[0].1, &live())
                .unwrap();
            assert_eq!(session.capacity_items().len(), 1);
            session.capacity_hook(event, if matches!(event, "SessionStart" | "SessionEnd") { serde_json::json!({}) }
                else { serde_json::json!({"turn_id":if event == "UserPromptSubmit" { "new-turn" } else { CAPACITY_TURN },
                    "transcript_path":session.root.join("sessions/day/turn.jsonl")}) });
            let revision = session.revision();
            assert!(session.capacity_items().is_empty(), "{event}");
            assert!(session.read_capacity().is_empty(), "{event}");
            assert_eq!(session.revision(), revision);
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 1);
        }
        for mutation in ["UPDATE sessions SET status='exited',exit_json='{}'", "UPDATE sessions SET status='recovery_required'",
            "UPDATE sessions SET transcript_epoch='new'", "UPDATE role_generations SET status='replaced'",
            "UPDATE audit_events SET actor_kind='hook' WHERE event_code='provider.codex_capacity_observed'",
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
             VALUES('new-attempt','t','context','implementation','base',1,'running','2999-01-01','2999-01-01')"]
        {
            let session = ObservedSession::codex_capacity();
            let observations = session.read_capacity();
            session.store.record_codex_capacity(&observations[0].0, &observations[0].1, &live()).unwrap();
            session.execute(mutation);
            let revision = session.revision();
            assert!(session.capacity_items().is_empty(), "{mutation}");
            assert!(session.read_capacity().is_empty());
            assert_eq!(session.revision(), revision);
        }
    }

    #[test]
    fn codex_capacity_fifo_does_not_starve_later_candidate_or_h3() {
        use std::os::unix::ffi::OsStrExt;
        let mut session = ObservedSession::codex_capacity();
        let fifo = session.root.join("sessions/day/fifo");
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // mkfifo receives a live NUL-terminated path; no writer ever opens it.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        session.execute("INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,
            transcript_epoch,native_session_id,process_identity_json,created_at,updated_at)
            SELECT 's-bad',role_generation_id,provider,status,launch_config_json,executable_version,
                transcript_epoch,native_session_id,process_identity_json,created_at,updated_at FROM sessions WHERE id='s'");
        let mut context = session.store.role_context("capacity-token").unwrap();
        context.session_id = "s-bad".into();
        for event in ["SessionStart", "UserPromptSubmit"] {
            session.store.save_hook_event(&context, &HookEnvelope { provider: context.provider,
                payload: serde_json::json!({"hook_event_name":event,"session_id":CAPACITY_NATIVE,"cwd":session.root,
                    "turn_id":CAPACITY_TURN,"transcript_path":fifo}) },
                &RolePeerProvenance { peer_pid:4242, peer_process_group_id:4242, peer_start_marker:"peer".into(),
                    managed_root_pid:4242, managed_root_start_marker:"root".into(), state:"managed_process_group_untrusted_payload".into() }).unwrap();
        }
        let processes = ProcessSnapshot::Read(
            ["s-bad", "s"]
                .into_iter()
                .map(|id| LiveInvocation {
                    session_id: id.into(),
                    role_generation_id: "g".into(),
                    transcript_epoch: "e".into(),
                    process_identity_json: PROCESS.into(),
                })
                .collect(),
        );
        session.base = Utc::now();
        let before = session.business_rows();
        let observed = session
            .store
            .read_codex_capacity(&session.root.join("sessions"), &processes)
            .unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0.binding.session_id, "s");
        session
            .store
            .record_codex_capacity(&observed[0].0, &observed[0].1, &processes)
            .unwrap();
        session.observe(1_000, &processes, true);
        session.observe(601_000, &processes, false);
        session.observe(602_000, &processes, false);
        assert!(session.scalar::<i64>("SELECT COUNT(*) FROM attention_observations WHERE kind='quiet_turn' AND state='open'") > 0);
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.capacity_items().len(), 1);
    }

    #[test]
    fn codex_capacity_projection_uses_entity_index_beyond_display_windows() {
        let session = ObservedSession::codex_capacity();
        let observed = session.read_capacity();
        session
            .store
            .record_codex_capacity(&observed[0].0, &observed[0].1, &live())
            .unwrap();
        session.execute("WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<205)
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            SELECT 'history-'||n,'g','codex','exited','{}','fixture','old','2999-01-01','2999-01-01' FROM numbers;
            WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<505)
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            SELECT 'noise-'||n,'noise','service','unrelated','session','unrelated','{}','2999-01-01' FROM numbers;");
        let revision = session.revision();
        let state = crate::workflow::state(&session.store).unwrap();
        assert!(state
            .attention
            .iter()
            .any(|item| item.id.starts_with("codex_capacity:")));
        assert!(state.active_sessions.iter().any(|value| value["id"] == "s"
            && value["role_generation_id"] == "g"
            && value["attempt_id"] == "a"
            && value["status"] == "running"));
        assert_eq!(session.revision(), revision);
        let connection = session.store.lock().unwrap();
        let plan = connection
            .prepare(&format!("EXPLAIN QUERY PLAN {CODEX_CAPACITY_AUDITS_SQL}"))
            .unwrap()
            .query_map(params!["s"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains("SEARCH audit_events USING INDEX audit_events_entity"),
            "{plan}"
        );
        assert!(!plan.contains("SCAN audit_events"), "{plan}");
    }

    #[test]
    fn dismissed_native_prompt_still_suppresses_raw_wait_observations() {
        let session = ObservedSession::new("dismissed-native-wait");
        session.hook("start", "SessionStart", "{}", 0);
        session.hook(
            "notice",
            "Notification",
            r#"{"notification_type":"agent_needs_input"}"#,
            1_000,
        );
        session.observe(5_000, &live(), true);
        let projected = crate::workflow::state(&session.store).unwrap();
        let binding = serde_json::from_value(
            projected.active_sessions[0]["native_prompt"]["dismissal"].clone(),
        )
        .unwrap();
        crate::workflow::execute(
            &session.store,
            &crate::domain::HumanCommand::DismissNativePrompt {
                operation_id: "dismiss".into(),
                binding,
            },
        )
        .unwrap();
        let revision = session.revision();
        session.observe(31_000, &live(), false);
        session.observe(33_000, &live(), false);
        assert_eq!(session.state("process_without_accepted_turn"), "none");
        assert_eq!(session.revision(), revision);
        assert!(
            crate::workflow::native_prompt(&session.store.lock().unwrap(), "s")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            crate::workflow::state(&session.store)
                .unwrap()
                .active_sessions[0]["native_prompt"]["dismissed"],
            true
        );
        assert_eq!(
            session.scalar::<String>("SELECT readiness_state FROM sessions WHERE id='s'"),
            "busy"
        );
    }

    #[test]
    fn an_unaccepted_live_invocation_opens_on_two_spaced_checks_and_ends_on_its_acceptance() {
        let session = ObservedSession::new("unaccepted");
        let kind = "process_without_accepted_turn";
        // Without its own trusted start, absent acceptance proves nothing.
        session.observe(5_000, &live(), true);
        assert_eq!(session.state(kind), "none");
        session.hook("start", "SessionStart", "{}", 0);
        session.observe(10_000, &live(), false);
        assert_eq!(session.state(kind), "candidate:0:0:-");
        let quiet = session.revision();
        // An unreadable process table discards the candidate; it confirms nothing.
        session.observe(31_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "none");
        session.observe(32_000, &live(), false);
        assert_eq!(session.state(kind), "candidate:1:0:-");
        // A second check needs a later, distinct time at least a second on.
        session.observe(32_500, &live(), false);
        session.observe(31_500, &live(), false);
        assert_eq!(session.state(kind), "candidate:1:0:-");
        assert_eq!(
            session.revision(),
            quiet,
            "candidate bookkeeping moved the cursor"
        );
        session.observe(32_500, &live(), false);
        assert_eq!(session.state(kind), "open:2:0:-");
        assert!(session.revision() > quiet);
        // Keyboard control suppresses classification; it is not acceptance.
        session.execute(&format!(
            "INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,
                 process_identity_json,expires_at,revoked_at,created_at,updated_at)
               VALUES('s','hash','human','person','g','{PROCESS}','2999-01-01T00:00:00Z',NULL,
                 '2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');"
        ));
        let gated = session.revision();
        session.observe(36_000, &live(), false);
        assert_eq!(session.state(kind), "open:2:0:-");
        assert_eq!(session.revision(), gated);
        session.execute("DELETE FROM input_leases;");
        // Unknown inventory keeps the open record, marked uncertain.
        session.observe(37_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:2:1:-");
        session.hook("accepted", "UserPromptSubmit", r#"{"prompt":"go"}"#, 38_000);
        session.observe(39_000, &live(), false);
        assert_eq!(session.state(kind), "resolved:2:0:turn_accepted");
        assert_eq!(
            session
                .store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT group_concat(event_code) FROM (SELECT event_code FROM audit_events
                       WHERE entity_kind='attention_observation' ORDER BY rowid)",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "attention.observation.opened,attention.observation.resolved"
        );
        // Classification never touched the work it describes.
        assert_eq!(
            session
                .store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT t.attention||':'||t.version||':'||a.status||':'||s.status||':'||s.readiness_state
                     FROM tasks t JOIN attempts a ON a.task_id=t.id
                     JOIN role_generations rg ON rg.attempt_id=a.id JOIN sessions s ON s.role_generation_id=rg.id",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "none:1:running:running:busy"
        );
    }

    #[test]
    fn a_quiet_turn_starts_its_own_clock_and_ends_on_activity() {
        let session = ObservedSession::new("quiet");
        let kind = "quiet_turn";
        session.hook("start", "SessionStart", "{}", -1_200_000);
        session.hook(
            "accepted",
            "UserPromptSubmit",
            r#"{"prompt":"go"}"#,
            -660_000,
        );
        session.execute("UPDATE sessions SET transcript_last_sequence=7;");
        // The old session timestamp is not quiet history; the first look starts the clock.
        session.observe(0, &live(), true);
        assert_eq!(session.state(kind), "candidate:0:0:-");
        session.observe(299_000, &live(), false);
        assert_eq!(session.state(kind), "candidate:0:0:-");
        // A restart forgets the candidate, so its quiet time starts over.
        session.observe(299_500, &live(), true);
        session.observe(300_000, &live(), false);
        assert_eq!(session.state(kind), "candidate:0:0:-");
        session.observe(599_500, &live(), false);
        assert_eq!(session.state(kind), "candidate:1:0:-");
        session.observe(600_500, &live(), false);
        assert_eq!(session.state(kind), "open:2:0:-");
        let opened = session.revision();
        // Lost capture keeps the last confirmed evidence, marked uncertain.
        session.execute("UPDATE sessions SET capture_state='failed';");
        session.observe(601_500, &live(), false);
        assert_eq!(session.state(kind), "open:2:1:-");
        assert!(session.revision() > opened);
        session.execute("UPDATE sessions SET capture_state='capturing';");
        session.observe(602_500, &live(), false);
        assert_eq!(session.state(kind), "open:2:0:-");
        session.execute(&format!("UPDATE sessions SET transcript_last_sequence=8;
            INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,
                process_identity_json,expires_at,created_at,updated_at)
            VALUES('s','quiet-control','human','person','g','{PROCESS}','2999-01-01T00:00:00Z',
                '2026-03-01T00:10:03Z','2026-03-01T00:10:03Z');"));
        session.observe(603_500, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:2:1:-");
        let before = session.business_rows();
        session.observe(604_500, &live(), false);
        assert_eq!(session.state(kind), "resolved:2:0:activity_resumed");
        assert_eq!(session.business_rows(), before);
    }

    #[test]
    fn observation_identity_survives_reopen_and_retirement_has_no_workflow_effects() {
        let session = ObservedSession::new("identity");
        session.hook("start", "SessionStart", "{}", 0);
        let before = session.business_rows();
        session.observe(30_000, &live(), true);
        session.observe(31_000, &live(), false);
        let id: String = session.scalar("SELECT id FROM attention_observations WHERE state='open'");
        let revision = session.revision();
        session.observe(32_000, &live(), false);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.business_rows(), before);
        let reopened = Store::open(&session.root.join("state.sqlite3")).unwrap();
        reopened
            .observe_attention(session.at(33_000), &ProcessSnapshot::Unavailable, true)
            .unwrap();
        assert_eq!(
            session.scalar::<String>("SELECT id FROM attention_observations WHERE state='open'"),
            id
        );
        assert_eq!(session.state("process_without_accepted_turn"), "open:2:1:-");
        session.execute(
            "UPDATE tasks SET lifecycle='cancelled'; UPDATE attempts SET status='cancelled';",
        );
        session.observe(34_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            session.state("process_without_accepted_turn"),
            "resolved:2:0:attempt_retired"
        );
        session.execute(
            "UPDATE tasks SET lifecycle='in_progress'; UPDATE attempts SET status='running';",
        );
        session.observe(35_000, &live(), false);
        session.observe(36_000, &live(), false);
        assert_eq!(
            session.scalar::<String>("SELECT id FROM attention_observations WHERE state='open'"),
            id
        );
        session.execute("UPDATE sessions SET transcript_epoch='replacement';");
        session.observe(37_000, &live(), false);
        assert_eq!(session.scalar::<String>("SELECT resolution_reason FROM attention_observations WHERE id=(SELECT id FROM attention_observations WHERE entity_key='s:e')"), "invocation_replaced");
    }

    #[test]
    fn quiet_candidates_require_uninterrupted_capture_activity_and_person_gates() {
        let session = ObservedSession::new("quiet-gates");
        let kind = "quiet_turn";
        session.hook("start", "SessionStart", "{}", -1_000_000);
        session.hook(
            "accepted",
            "UserPromptSubmit",
            r#"{"prompt_id":"turn"}"#,
            -700_000,
        );
        session.observe(0, &live(), true);
        session.observe(300_000, &live(), false);
        session.permission();
        session.observe(301_000, &live(), false);
        assert_eq!(session.state(kind), "none");
        session.execute("UPDATE permission_requests SET state='expired';");
        session.observe(302_000, &live(), false);
        session.observe(602_000, &live(), false);
        session.observe(602_500, &ProcessSnapshot::Read(vec![]), false);
        assert_eq!(
            session.state(kind),
            "none",
            "an unattached process is unknown"
        );
        session.observe(603_000, &live(), false);
        session.execute("UPDATE sessions SET transcript_last_sequence=1;");
        session.observe(903_000, &live(), false);
        assert_eq!(session.state(kind), "candidate:0:0:-");
        session.observe(1_203_000, &live(), false);
        session.observe(1_204_000, &live(), false);
        assert_eq!(session.state(kind), "open:2:0:-");
        session.hook("stop", "Stop", r#"{"prompt_id":"turn"}"#, 1_205_000);
        session.observe(1_206_000, &live(), false);
        assert_eq!(session.state(kind), "resolved:2:0:turn_completed");
    }

    #[test]
    fn a_failed_observation_transaction_rolls_back_classification_only() {
        let session = ObservedSession::new("rollback");
        session.hook("start", "SessionStart", "{}", 0);
        session.observe(30_000, &live(), true);
        session.execute("INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,
            created_at,written_at,delivery_session_id,delivery_transcript_epoch)
            VALUES('guidance','a','g','private','written_awaiting_submit','2026-03-01T00:00:00Z',
                '2026-03-01T00:00:00Z','s','e');
            CREATE TRIGGER fail_guidance_observation BEFORE INSERT ON attention_observations
            WHEN NEW.kind='guidance_unaccepted' BEGIN SELECT RAISE(ABORT,'observation write fault'); END;");
        let before = session.business_rows();
        let revision = session.revision();
        let error = session
            .store
            .observe_attention(session.at(31_000), &live(), false)
            .unwrap_err();
        assert!(
            error.to_string().contains("observation write fault"),
            "{error}"
        );
        assert_eq!(
            session.state("process_without_accepted_turn"),
            "candidate:1:0:-"
        );
        assert_eq!(session.state("guidance_unaccepted"), "none");
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.revision(), revision);
        assert_eq!(
            session.scalar::<i64>(
                "SELECT COUNT(*) FROM audit_events WHERE entity_kind='attention_observation'"
            ),
            0
        );
    }

    #[test]
    fn no_session_requires_complete_effective_inventory_and_a_session_required_decision() {
        let session = ObservedSession::new("no-session");
        let kind = "attempt_without_live_session";
        let classify = |milliseconds, processes: &ProcessSnapshot, required| {
            let mut connection = session.store.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            let pass = ObservationPass {
                transaction: &transaction,
                now: session.at(milliseconds),
                now_text: session.at(milliseconds).to_rfc3339(),
                processes,
            };
            let (subject, finding) = pass.attempt_without_live_session("a", required).unwrap();
            pass.advance(subject, finding).unwrap();
            transaction.commit().unwrap();
        };
        classify(0, &ProcessSnapshot::Read(vec![]), true);
        assert_eq!(
            session.state(kind),
            "none",
            "missing handle alone is unknown"
        );
        session.execute("UPDATE sessions SET status='exited',exit_json='{}';");
        classify(0, &ProcessSnapshot::Read(vec![]), true);
        classify(60_000, &ProcessSnapshot::Read(vec![]), true);
        classify(60_500, &ProcessSnapshot::Unavailable, true);
        assert_eq!(session.state(kind), "none");
        classify(61_000, &ProcessSnapshot::Read(vec![]), true);
        classify(121_000, &ProcessSnapshot::Read(vec![]), true);
        classify(122_000, &ProcessSnapshot::Read(vec![]), true);
        assert_eq!(session.state(kind), "open:2:0:-");
        classify(123_000, &live(), true);
        assert_eq!(session.state(kind), "resolved:2:0:effective_session_live");
        classify(124_000, &ProcessSnapshot::Read(vec![]), false);
        assert_eq!(session.state(kind), "resolved:2:0:effective_session_live");
    }

    #[test]
    fn no_session_helper_uses_only_the_four_reviewed_manager_waits() {
        let session = ObservedSession::new("manager-waits");
        {
            let connection = session.store.lock().unwrap();
            connection.execute("INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,
                detected_json,workflow_id,package_version,upstream_source_hash,overlay_hash,updated_at)
                VALUES('p','ready','fixture','compatible','{}',?1,?2,?3,?4,'2026-01-01T00:00:00Z')",
                params![crate::trip::WORKFLOW_ID, crate::trip::PACKAGE_VERSION, crate::trip::source_hash(), crate::trip::overlay_hash()]).unwrap();
            connection.execute("UPDATE attempts SET workflow_version=?1,workflow_hash=?2,legacy_migration_required=0",
                params![crate::trip::WORKFLOW_ID, crate::workflow_resources::workflow_hash()]).unwrap();
        }
        session.execute(
            "UPDATE role_generations SET role='manager'; UPDATE role_settings SET role='manager';",
        );
        let required = || {
            crate::coordinator::attempts_waiting_on_manager_turn(&session.store.lock().unwrap())
                .unwrap()
                .contains("a")
        };
        for (phase, plan_hash, candidate_hash) in [
            ("planning", None, None),
            ("planning", Some("plan"), None),
            ("implementation", Some("plan"), Some("candidate")),
            ("manager_handoff", Some("plan"), Some("candidate")),
        ] {
            session
                .store
                .lock()
                .unwrap()
                .execute(
                    "UPDATE attempts SET phase=?1,plan_hash=?2,candidate_hash=?3",
                    params![phase, plan_hash, candidate_hash],
                )
                .unwrap();
            assert!(required(), "{phase} must use the reviewed manager wait");
        }
        session.execute("UPDATE tasks SET attention='paused';");
        assert!(
            !required(),
            "a persisted hold is not a missing-session wait"
        );
        session.execute("UPDATE tasks SET attention='none'; UPDATE sessions SET status='exited',exit_json='{}'; UPDATE role_generations SET status='exited';");
        assert!(
            !required(),
            "dispatching a manager is outside the reviewed set"
        );
    }

    #[test]
    fn guidance_requires_its_delivery_and_exit_or_an_unrelated_prompt_never_clears_it() {
        let session = ObservedSession::new("guidance");
        let kind = "guidance_unaccepted";
        session.execute("INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,
            created_at,written_at,delivery_session_id,delivery_transcript_epoch)
            VALUES('guidance','a','g','private body','written_awaiting_submit','2026-03-01T00:00:00Z',
            '2026-03-01T00:00:00Z','s','e');");
        session.observe(29_999, &ProcessSnapshot::Unavailable, true);
        assert_eq!(session.state(kind), "candidate:0:0:-");
        session.observe(30_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:1:0:-");
        session.hook("start", "SessionStart", "{}", 31_000);
        session.hook(
            "unrelated",
            "UserPromptSubmit",
            r#"{"prompt":"another instruction"}"#,
            32_000,
        );
        session.execute("UPDATE sessions SET status='exited',exit_json='{}';");
        session.observe(33_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:1:0:-");
        session.execute("UPDATE guidance_messages SET state='delivery_unknown';");
        session.observe(33_500, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            session.state(kind),
            "open:1:1:-",
            "uncertain delivery is not disposition"
        );
        session.execute("UPDATE guidance_messages SET state='written_awaiting_submit';");
        assert!(!session
            .scalar::<String>(
                "SELECT evidence_json FROM attention_observations WHERE kind='guidance_unaccepted'"
            )
            .contains("private body"));
        session.execute("UPDATE guidance_messages SET state='submitted';");
        session.observe(34_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "resolved:1:0:guidance_submitted");
    }

    #[test]
    fn permission_observes_only_a_residual_native_unresolved_request_and_never_answers_it() {
        let session = ObservedSession::new("permission");
        let kind = "permission_on_exited_session";
        session.permission();
        session.observe(0, &ProcessSnapshot::Unavailable, true);
        assert_eq!(session.state(kind), "none");
        session.execute("UPDATE sessions SET status='exited';");
        session.observe(1_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            session.state(kind),
            "none",
            "status alone is not matched exit evidence"
        );
        session.execute("UPDATE sessions SET exit_json='{}';");
        let before = session.business_rows();
        session.observe(2_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:1:0:-");
        assert_eq!(session.business_rows(), before);
        session.execute("UPDATE permission_requests SET state='expired';");
        session.observe(3_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "resolved:1:0:request_settled");
    }

    #[test]
    fn resume_contradiction_requires_exact_supersession_and_keeps_independent_holds() {
        let session = ObservedSession::new("resume");
        let kind = "resume_failure_after_acceptance";
        session.execute(r#"UPDATE tasks SET attention='resume_failed'; UPDATE attempts SET status='needs_input';
            UPDATE sessions SET resume_count=1,capability_key='key';
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,prior_transcript_epoch,
                launch_config_json,capability_key,capability_identity_json,state,hook_event_boundary_rowid,created_at,updated_at)
            VALUES('resume','s',1,'e','prior','{}','key','{}','running',
                (SELECT COALESCE(MAX(rowid),0) FROM hook_events WHERE session_id='s'),'2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('rejection','rejection','service','session.resume.rejected','session','s',
                '{"attempt_id":"a","role":"implementer","lane_id":"default","role_generation_id":"g","transcript_epoch":"prior","resume_count":0}',
                '2026-02-28T23:59:00Z');"#);
        session.hook("start", "SessionStart", "{}", 0);
        session.hook("accepted", "UserPromptSubmit", "{}", 1_000);
        session.observe(2_000, &ProcessSnapshot::Unavailable, true);
        assert_eq!(
            session.state(kind),
            "none",
            "a submit alone is not supersession"
        );
        session.execute(r#"INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('reconciled','reconciled','hook','session.resume.turn_reconciled','session','s',
            '{"role_generation_id":"g","transcript_epoch":"e","resume_invocation_id":"resume","superseding_hook_event_id":"accepted","rejection_event_ids":["rejection"]}',
            '2026-03-01T00:00:01Z');"#);
        session.hook("gated-failure", "StopFailure", "{}", 2_500);
        session.execute("INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,
            transcript_epoch,accepted_hook_event_id,failure_hook_event_id,failure_kind,attribution,created_at,state)
            VALUES('candidate-hold','a','g','s','e','accepted','gated-failure','authentication_failed',
                'arrival_order','2026-03-01T00:00:02.500Z','active');");
        let before = session.business_rows();
        session.observe(3_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            session.state(kind),
            "none",
            "an H4 hold suppresses a candidate"
        );
        assert_eq!(session.business_rows(), before);
        session.execute("UPDATE provider_failure_holds SET state='human_released',resolved_at='2026-03-01T00:00:03.500Z',
                resolution_kind='human_release',resolution_ref='fixture-release' WHERE id='candidate-hold';
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                VALUES('g-manager','a','manager','claude',1,1,'running','f','2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
                VALUES('s-manager','g-manager','claude','exited','{}','fixture','manager-epoch','2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');
            INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,payload_json,peer_pid,peer_process_group_id,
                peer_start_marker,provenance_state,received_at) VALUES('manager-failure','s-manager','g-manager','claude','StopFailure',
                '{}',42,42,'peer','managed_process_group_untrusted_payload','2026-03-01T00:00:03.500Z');
            INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
                failure_hook_event_id,failure_kind,attribution,created_at,state)
                VALUES('unrelated-hold','a','g-manager','s-manager','manager-epoch','manager-failure','authentication_failed',
                    'startup_invocation','2026-03-01T00:00:03.500Z','active');");
        let before = session.business_rows();
        session.observe(4_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "candidate:1:0:-");
        session.observe(5_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:2:0:-");
        assert_eq!(session.business_rows(), before);
        session.hook("standing-failure", "StopFailure", "{}", 5_500);
        session.execute("INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
            accepted_hook_event_id,failure_hook_event_id,failure_kind,attribution,created_at,state)
            VALUES('standing-hold','a','g','s','e','accepted','standing-failure','authentication_failed','arrival_order',
                '2026-03-01T00:00:05.500Z','active');");
        let before = session.business_rows();
        session.observe(6_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "resolved:2:0:provider_failure_hold");
        assert_eq!(session.business_rows(), before);
        let projected = crate::workflow::state(&session.store).unwrap();
        assert!(projected
            .attention
            .iter()
            .any(|item| item.id == "provider_failure_hold:standing-hold"));
        assert!(!projected
            .attention
            .iter()
            .any(|item| item.reason.contains("nothing else explains the hold")));
        session.execute("UPDATE provider_failure_holds SET state='human_released',resolved_at='2026-03-01T00:00:06.500Z',
            resolution_kind='human_release',resolution_ref='fixture-release' WHERE id='standing-hold';");
        session.observe(7_000, &ProcessSnapshot::Unavailable, false);
        session.observe(8_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:2:0:-");
        session.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at)
            VALUES('independent',NULL,'a','attention_required','{}','2026-03-01T00:00:09Z','2026-03-01T00:00:09Z');");
        session.observe(9_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "resolved:2:0:independent_hold");
    }

    #[test]
    fn busy_exit_preserves_app_stop_intent_and_excludes_history_and_completed_turns() {
        let session = ObservedSession::new("busy-exit");
        let kind = "busy_after_exit";
        session.hook("start", "SessionStart", "{}", 0);
        session.hook("accepted", "UserPromptSubmit", "{}", 1_000);
        session.execute("UPDATE sessions SET status='exited',exit_json='{}';");
        session.observe(2_000, &ProcessSnapshot::Unavailable, true);
        assert_eq!(session.state(kind), "none");
        session.execute("UPDATE sessions SET status='interrupt_requested',interrupt_requested_at='2026-03-01T00:00:02Z';");
        assert!(session
            .store
            .update_session_exit(
                "s",
                "e",
                PROCESS,
                r#"{"app_stop_requested":false,"observed_at":"2026-03-01T00:00:03Z","exit_code":0}"#
            )
            .unwrap());
        assert_eq!(
            session.scalar::<i64>(
                "SELECT json_extract(exit_json,'$.app_stop_requested') FROM sessions"
            ),
            1
        );
        assert_eq!(
            session.scalar::<i64>("SELECT json_extract(exit_json,'$.exit_code') FROM sessions"),
            0
        );
        assert!(session
            .scalar::<Option<String>>("SELECT interrupt_requested_at FROM sessions")
            .is_none());
        session.observe(3_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "none");
        session.execute("UPDATE sessions SET status='running';");
        assert!(session
            .store
            .update_session_exit(
                "s",
                "e",
                PROCESS,
                r#"{"app_stop_requested":true,"observed_at":"2026-03-01T00:00:04Z","exit_code":1}"#
            )
            .unwrap());
        assert_eq!(
            session.scalar::<i64>(
                "SELECT json_extract(exit_json,'$.app_stop_requested') FROM sessions"
            ),
            0
        );
        session.observe(4_000, &ProcessSnapshot::Unavailable, false);
        session.observe(5_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "open:2:0:-");
        session.execute("INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,
            summary,evidence_json,metadata_json,created_at) VALUES('report','report','s','g','candidate_ready',
            'completed','[]','{}','2026-03-01T00:00:06Z');");
        session.observe(7_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state(kind), "resolved:2:0:report_recorded");
    }

    #[test]
    fn recurring_episodes_reset_only_at_published_progress_in_source_order() {
        let session = ObservedSession::new("recurrence");
        let consume = |id: &str, milliseconds: i64| {
            let now = session.at(milliseconds).to_rfc3339();
            let mut connection = session.store.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            transaction.execute("INSERT OR IGNORE INTO role_results(id,operation_id,session_id,role_generation_id,
                outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
                VALUES(?1,?1,'s','g','blocked','same wording','[]','{}',?2,?2)", params![id, now]).unwrap();
            transaction
                .execute("UPDATE tasks SET attention='needs_input'", [])
                .unwrap();
            transaction
                .execute(
                    "UPDATE attempts SET status='needs_input',updated_at=?1",
                    params![now],
                )
                .unwrap();
            transaction.execute("INSERT OR IGNORE INTO audit_events(id,operation_id,actor_kind,event_code,
                entity_kind,entity_id,detail_json,created_at) VALUES(?1,?1,'service','attempt.attention.changed',
                'attempt','a',?2,?3)", params![format!("hold-{id}"), serde_json::json!({"attention":"needs_input",
                "reason":"role_blocked","result_id":id,"role_generation_id":"g"}).to_string(), now]).unwrap();
            count_blocking_episode(&transaction, id, &now).unwrap();
            transaction.commit().unwrap();
        };
        let count = || {
            session.scalar::<i64>(
                "SELECT recurrence_count FROM attention_observations WHERE kind='recurring_block'",
            )
        };
        consume("block-1", 0);
        assert_eq!(count(), 1);
        consume("block-1", 0);
        assert_eq!(count(), 1);
        session.observe(1_000, &ProcessSnapshot::Unavailable, true);
        assert_eq!(session.state("recurring_block"), "candidate:0:0:-");
        consume("block-2", 2_000);
        session.observe(3_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(session.state("recurring_block"), "open:0:0:-");
        let id: String =
            session.scalar("SELECT id FROM attention_observations WHERE kind='recurring_block'");
        consume("block-3", 4_000);
        assert_eq!(count(), 2);
        session.execute("UPDATE tasks SET attention='none'; UPDATE attempts SET status='running';");
        session.hook("start", "SessionStart", "{}", 5_000);
        session.hook("prompt", "UserPromptSubmit", "{}", 6_000);
        session.observe(7_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            count(),
            2,
            "Continue and a new prompt preserve episode history"
        );
        assert_eq!(session.state("recurring_block"), "resolved:0:0:block_ended");
        session.execute("INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,
            summary,evidence_json,metadata_json,created_at,consumed_at) VALUES('retired-progress','retired-progress',
            's','g','candidate_ready','retired','[]','{}','2026-03-01T00:00:08Z','2026-03-01T00:00:08Z');
            UPDATE role_generations SET status='replaced';");
        session.observe(9_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(count(), 2, "switch retirement completed no freeze");
        session.execute("UPDATE role_generations SET status='running';
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at) VALUES('duplicate-progress','duplicate-progress','s','g',
                'candidate_ready','duplicate','[]','{}','2026-03-01T00:00:10Z','2026-03-01T00:00:10Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                VALUES('duplicate-retirement','duplicate-retirement','service','role_result.superseded',
                'role_result','duplicate-progress','{}','2026-03-01T00:00:10Z');");
        session.observe(11_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(count(), 2);
        session.execute("INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at)
            VALUES('reused','a','candidate','base','same-content','{}',1,'2026-01-01T00:00:00Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at) VALUES('published','published','s','g','candidate_ready',
                'published','[]','{}','2026-03-01T00:00:12Z','2026-03-01T00:00:12Z');
            INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,result_snapshot_id,created_at,updated_at)
                VALUES('new-freeze','a','candidate','g','complete','reused','2026-03-01T00:00:12Z','2026-03-01T00:00:12Z');");
        consume("block-after-publication", 13_000);
        assert_eq!(
            count(),
            1,
            "an older publication resets before the later episode, not after it"
        );
        session.observe(14_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(count(), 1);
        assert_eq!(session.scalar::<String>("SELECT json_extract(reset_evidence_json,'$.freeze_intent_id') FROM attention_observations WHERE kind='recurring_block'"), "new-freeze");
        consume("second-after-publication", 15_000);
        session.observe(16_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(count(), 2);
        assert_eq!(
            session.scalar::<String>(
                "SELECT id FROM attention_observations WHERE kind='recurring_block'"
            ),
            id
        );
        session.execute("INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,
            authority_generation,created_at,updated_at) VALUES('manager','a','manager','claude',1,1,'exited','f',
            '2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES('manager-session','manager','claude','exited','{}','fixture','manager-epoch','2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
            VALUES('other-role-progress','other-role-progress','manager-session','manager','plan_ready','published','[]','{}',
                '2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');
            INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at)
            VALUES('manager-plan','a','plan','base','plan-content','{}',1,'2026-03-01T00:00:17Z');
            INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,result_snapshot_id,created_at,updated_at)
            VALUES('manager-freeze','a','plan','manager','complete','manager-plan','2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');");
        session.observe(18_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            count(),
            2,
            "another role's published progress does not reset this role"
        );
        session.execute(r#"INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,
                evidence_json,metadata_json,created_at,consumed_at)
            VALUES('pending-before-freeze','pending-before-freeze','s','g','blocked','blocked','[]','{}',
                '2026-03-01T00:00:19Z','2026-03-01T00:00:19Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('pending-hold-before','pending-hold-before','service','attempt.attention.changed','attempt','a',
                '{"attention":"needs_input","reason":"role_blocked","result_id":"pending-before-freeze","role_generation_id":"g"}',
                '2026-03-01T00:00:19Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at)
            VALUES('pending-publication','pending-publication','s','g','candidate_ready','same content','[]','{}',
                '2026-03-01T00:00:22Z','2026-03-01T00:00:22Z');
            INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,result_snapshot_id,created_at,updated_at)
            VALUES('pending-freeze','a','candidate','g','complete','reused','2026-03-01T00:00:22Z','2026-03-01T00:00:22Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at)
            VALUES('pending-after-freeze','pending-after-freeze','s','g','blocked','blocked','[]','{}',
                '2026-03-01T00:00:23Z','2026-03-01T00:00:23Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('pending-hold-after','pending-hold-after','service','attempt.attention.changed','attempt','a',
                '{"attention":"needs_input","reason":"role_blocked","result_id":"pending-after-freeze","role_generation_id":"g"}',
                '2026-03-01T00:00:23Z');
            UPDATE attempts SET status='needs_input',updated_at='2026-03-01T00:00:23Z' WHERE id='a';
            UPDATE tasks SET attention='needs_input';"#);
        let before = session.business_rows();
        session.observe(24_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(
            count(),
            1,
            "catch-up orders the publication between pending episodes"
        );
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.scalar::<String>("SELECT last_counted_result_id FROM attention_observations WHERE kind='recurring_block'"), "pending-after-freeze");
        assert_eq!(session.scalar::<String>("SELECT json_extract(reset_evidence_json,'$.freeze_intent_id') FROM attention_observations WHERE kind='recurring_block'"), "pending-freeze");
        session.observe(24_500, &ProcessSnapshot::Unavailable, false);
        assert_eq!(count(), 1);
        consume("second-after-deferred-publication", 25_000);
        session.observe(26_000, &ProcessSnapshot::Unavailable, false);
        assert_eq!(count(), 2);
        session.execute(r#"INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
            VALUES('other-attempt','t','context','implementation','base',1,'running','2026-03-02T00:00:00Z','2026-03-02T00:00:00Z');
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,lane_id,created_at,updated_at)
            VALUES('lane-generation','other-attempt','implementer','claude',1,1,'running','f','other-lane','2026-03-02T00:00:00Z','2026-03-02T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES('lane-session','lane-generation','claude','exited','{}','fixture','lane-epoch','2026-03-02T00:00:00Z','2026-03-02T00:00:00Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
            VALUES('retired-lane-result','retired-lane-result','lane-session','lane-generation','needs_input','retired','[]','{}','2026-03-02T00:00:01Z','2026-03-02T00:00:01Z'),
                ('lane-result','lane-result','lane-session','lane-generation','needs_input','same wording','[]','{}','2026-03-02T00:00:01Z','2026-03-02T00:00:01Z');
            UPDATE tasks SET attention='needs_input'; UPDATE attempts SET status='needs_input',updated_at='2026-03-02T00:00:01Z' WHERE id='other-attempt';
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('lane-hold','lane-hold','service','attempt.attention.changed','attempt','other-attempt',
                '{"attention":"needs_input","reason":"role_needs_input","result_id":"lane-result","role_generation_id":"lane-generation"}',
                '2026-03-02T00:00:01Z');"#);
        {
            let mut connection = session.store.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            count_blocking_episode(&transaction, "lane-result", "2026-03-02T00:00:01Z").unwrap();
            transaction.commit().unwrap();
        }
        assert_eq!(
            session.scalar::<i64>(
                "SELECT recurrence_count FROM attention_observations WHERE attempt_id='a'"
            ),
            2
        );
        assert_eq!(session.scalar::<i64>("SELECT recurrence_count FROM attention_observations WHERE attempt_id='other-attempt' AND lane_id='other-lane' AND entity_key LIKE '%:needs_input'"), 1);
    }
}
