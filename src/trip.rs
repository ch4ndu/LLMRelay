use crate::config::{atomic_write, InstancePaths};
use crate::domain::{
    CapabilityIdentity, CapabilityProofInput, LaunchConfig, OperationResult, Provider, RoleContext,
    RoleKind, RoleOverride, RoleResultReport, TripHumanAction,
};
use crate::store::{json_hash, Store};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

pub const WORKFLOW_ID: &str = "trip-explorer-0.12.0-llmrelay-1";
pub const PACKAGE_VERSION: &str = "0.12.0";

const MAX_LANE_SOURCE_BINDINGS: usize = 4096;
const MAX_LANE_SOURCE_BINDING_BYTES: usize = 128 * 1024;

const SOURCE_MANIFEST: &str =
    include_str!("../resources/trip-explorer/0.12.0/source-manifest.json");
const OVERLAY: &str = include_str!("../resources/prompts/trip-overlay.md");
const WORKFLOW: &str = include_str!("../resources/workflows/trip-explorer-0.12.0-llmrelay-1.json");

const DELEGATED_ROLES: [RoleKind; 5] = [
    RoleKind::Explorer,
    RoleKind::PlanReviewer,
    RoleKind::Implementer,
    RoleKind::CodeReviewer,
    RoleKind::FinalReviewer,
];

const APP_ROLES: [&str; 6] = [
    "manager",
    "explorer",
    "plan_reviewer",
    "implementer",
    "code_reviewer",
    "final_verifier",
];

struct PackageFile {
    relative: &'static str,
    bytes: &'static [u8],
    sha256: &'static str,
}

const PACKAGE_FILES: &[PackageFile] = &[
    PackageFile { relative: "bin/check_evidence.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/check_evidence.py"), sha256: "c7b13ecd2e0dffeaf5f6d31c94ab53492b93ba6c43575a9052e8263b86bb2250" },
    PackageFile { relative: "bin/claude_console.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/claude_console.py"), sha256: "d899f71dfc5b019a404d7c267a84f7533fd390fb543291aab43ca906222509f5" },
    PackageFile { relative: "bin/cmux_observer.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/cmux_observer.py"), sha256: "c2dcc814681eba9186b62e9537edf8ad290aadeb80545e846f05518c18502307" },
    PackageFile { relative: "bin/cmux_role_runner.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/cmux_role_runner.py"), sha256: "cef80e3d91f6c3f611dff3a682ca9256aa69a6cdaffa2fdf8ccb6764a211b551" },
    PackageFile { relative: "bin/role_config.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/role_config.py"), sha256: "243032ab1f985aedc2be248d08f39b26e5b446c1bbdeb91676d1ab27440d05a6" },
    PackageFile { relative: "bin/run_report.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/run_report.py"), sha256: "cadd620dc5e2c8d43f237b52cb1ef79c0ce3b9dfa1caad1c2c072a347a27a323" },
    PackageFile { relative: "bin/upgrade_preview.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/upgrade_preview.py"), sha256: "550cd66ab495cb5df289629a9effaa8e204355e7be46bc58893eb8df692d79e1" },
    PackageFile { relative: "bin/validate_installed.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/validate_installed.py"), sha256: "8a988ab838fdb6e269177320b19b8d852e0c4d65728f2e8e31f474b24c874be7" },
    PackageFile { relative: "bin/workflow_doctor.py", bytes: include_bytes!("../resources/trip-explorer/0.12.0/bin/workflow_doctor.py"), sha256: "5132bd349903a22fbaf2f1be4e12dfd6cafb92ccb00de608814969cece7e1b08" },
    PackageFile { relative: "skills/trip-explorer-init/SKILL.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-init/SKILL.md"), sha256: "723ab8314ffbb4bc03091f3cc336b60e6f56289496aa6382dde28641bd4b0172" },
    PackageFile { relative: "skills/trip-explorer-init/agents/openai.yaml", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-init/agents/openai.yaml"), sha256: "680e238c90633f55b137807a5b7369ed278ac7b7f4b99d7f2d18105b6103a414" },
    PackageFile { relative: "skills/trip-explorer-upgrade/SKILL.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-upgrade/SKILL.md"), sha256: "7a9c62094a8927da9b8c33c605937cd4f875738c83fd571b0bb7ae8248e63fe0" },
    PackageFile { relative: "skills/trip-explorer-upgrade/agents/openai.yaml", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-upgrade/agents/openai.yaml"), sha256: "1c6941574cf99cdcfee2f372aad1c9ad45dcf3434a654767d9f255ad8ca7c320" },
    PackageFile { relative: "skills/trip-explorer-workflow/SKILL.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/SKILL.md"), sha256: "376e009353ff8eacd5e0d1b4a92b9131a82ae13fc5e44ed234c7ee44a86d69e6" },
    PackageFile { relative: "skills/trip-explorer-workflow/agents/openai.yaml", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/agents/openai.yaml"), sha256: "8a7cf505b62f328f0959140ad0980b9b4ec23da64d942dcfe7d22533066e2681" },
    PackageFile { relative: "skills/trip-explorer-workflow/references/behavior-testing.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/references/behavior-testing.md"), sha256: "af9919619a297c447a24a17b92246f7ba66d4232863e24b0f6be2d79a63f4229" },
    PackageFile { relative: "skills/trip-explorer-workflow/references/evidence-format.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/references/evidence-format.md"), sha256: "56f80418dba6327643b0e7ac118e6b5376391e0099e47602f6edc9f6e34d2019" },
    PackageFile { relative: "skills/trip-explorer-workflow/references/explorer-activation.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/references/explorer-activation.md"), sha256: "180a9858fc9c579408c2a8200394007fb4039801c7449ba0fc7bf5b0af5a746a" },
    PackageFile { relative: "skills/trip-explorer-workflow/references/guidance-quality.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/references/guidance-quality.md"), sha256: "7d4d9cbfed98e265909d76dbbd1dc692e0cafba31810903efae75c00c53a50a4" },
    PackageFile { relative: "skills/trip-explorer-workflow/references/maintenance.md", bytes: include_bytes!("../resources/trip-explorer/0.12.0/skills/trip-explorer-workflow/references/maintenance.md"), sha256: "ae12c0eee1d97d33748b24302faec609eca38cf28109ea50c43c57298cd521c9" },
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectReadiness {
    NotInitialized,
    SetupInProgress,
    Ready,
    NeedsUpgradeReview,
    Invalid,
    RecoveryRequired,
}

impl ProjectReadiness {
    fn as_str(&self) -> &'static str {
        match self {
            Self::NotInitialized => "not_initialized",
            Self::SetupInProgress => "setup_in_progress",
            Self::Ready => "ready",
            Self::NeedsUpgradeReview => "needs_upgrade_review",
            Self::Invalid => "invalid",
            Self::RecoveryRequired => "recovery_required",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallationObservation {
    pub kind: String,
    pub canonical_root: String,
    pub alternate_roots: Vec<String>,
    pub manifest_version: Option<String>,
    pub customized: bool,
    pub partial_paths: Vec<String>,
    pub conflicts: Vec<String>,
    pub hashes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SetupProfile {
    adapter: String,
    provider: String,
    model: String,
    effort: String,
    authority: String,
    session: String,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SetupRole {
    profile: String,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SetupProposal {
    project_name: String,
    #[serde(default)]
    host_manager: Option<RoleOverride>,
    guidance: Vec<String>,
    documentation: serde_json::Value,
    verification: serde_json::Value,
    #[serde(default)]
    verification_contracts: BTreeMap<String, serde_json::Value>,
    testing: serde_json::Value,
    observability: serde_json::Value,
    roles: BTreeMap<String, SetupRole>,
    profiles: BTreeMap<String, SetupProfile>,
    adapters: serde_json::Value,
    agents_file: serde_json::Value,
    local_exclude: serde_json::Value,
    #[serde(default)]
    canonical_migration: Option<serde_json::Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

pub(crate) struct SetupManagerStopPreparation {
    pub completed: Option<OperationResult>,
    pub control_id: String,
    pub session_id: Option<String>,
    pub retry_signal_delivery: bool,
    operation_id: String,
    request_hash: String,
    setup_id: String,
    result_project_version: i64,
    session_process_identity: Option<serde_json::Value>,
}

pub(crate) enum SetupManagerSignalDelivery {
    Requested,
    AlreadyRequested,
    AlreadyQuiescent,
    NotRunning,
    Failed(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupProposalSummary {
    #[serde(default)]
    project_name: Option<String>,
    #[serde(default)]
    guidance: Option<Vec<String>>,
    #[serde(default)]
    documentation: Option<SetupProposalSummaryDocumentation>,
    #[serde(default)]
    verification: Option<SetupProposalSummaryVerification>,
    #[serde(default)]
    agents_content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupProposalSummaryDocumentation {
    no_change_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupProposalSummaryVerification {
    #[serde(default)]
    focused: Option<Vec<String>>,
    #[serde(default)]
    broad: Option<Vec<String>>,
    #[serde(default)]
    cleanup: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalFile {
    relative: String,
    bytes: Vec<u8>,
    source_hash: String,
    preimage_hash: Option<String>,
    preimage_bytes: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct CapabilityRuntime {
    pub hooks: crate::providers::HookAssets,
    pub role_socket: PathBuf,
    pub executable: PathBuf,
    pub(crate) compatibility_bundles: Arc<crate::provider_compatibility::BundleSet>,
}

impl CapabilityRuntime {
    pub fn from_store(
        store: &Store,
        hooks: crate::providers::HookAssets,
        role_socket: PathBuf,
        executable: PathBuf,
    ) -> Self {
        Self {
            hooks,
            role_socket,
            executable,
            compatibility_bundles: store.compatibility_bundles.clone(),
        }
    }

    fn require_current_policy(&self, config: &LaunchConfig) -> Result<()> {
        let identity = crate::providers::capability_identity(config)?;
        crate::providers::require_current_capability_identity_with_bundles(
            &identity,
            &config.cwd,
            &self.compatibility_bundles,
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TaskProfileAuthority {
    pub project_id: String,
    pub task_id: String,
    pub role: RoleKind,
    pub settings_id: String,
    pub settings_revision: i64,
    pub profile_json: serde_json::Value,
    pub profile_hash: String,
    pub project_config_revision_id: String,
    pub project_configuration_hash: String,
    pub adapter_name: String,
    pub adapter_hash: String,
    pub capability_id: String,
    pub capability_key: String,
    pub capability_proof_hash: String,
    pub activation_id: Option<String>,
    pub source: String,
}

#[derive(Clone)]
struct RuntimeProbePlan {
    role: RoleKind,
    settings_revision: Option<i64>,
    source: String,
    config: RoleOverride,
    profile_json: serde_json::Value,
    profile_hash: String,
    project_config_revision_id: String,
    project_configuration_hash: String,
    adapter_name: String,
    adapter_hash: String,
    capability_key: String,
    capability_identity_json: String,
    capability_id: Option<String>,
}

struct RuntimeScopeExpectation<'a> {
    project_id: &'a str,
    task_id: Option<&'a str>,
    role: RoleKind,
    settings_revision: Option<i64>,
    source: &'a str,
    profile_hash: &'a str,
    project_config_revision_id: &'a str,
    project_configuration_hash: &'a str,
    adapter_name: &'a str,
    adapter_hash: &'a str,
    capability_key: &'a str,
    capability_identity: &'a serde_json::Value,
    setup_operation_id: &'a str,
    fixture_project_id: &'a str,
    fixture_repository_identity: &'a str,
    fixture_root_hash: &'a str,
}

enum RuntimeScopeValidation {
    Current(serde_json::Value),
    Drift(String),
}

pub fn source_hash() -> String {
    sha256(SOURCE_MANIFEST.as_bytes())
}

pub fn overlay_hash() -> String {
    sha256(
        [WORKFLOW.as_bytes(), OVERLAY.as_bytes()]
            .concat()
            .as_slice(),
    )
}

pub fn register_project_state(connection: &Connection, project_id: &str, now: &str) -> Result<()> {
    connection.execute(
        "INSERT OR IGNORE INTO trip_project_state(project_id,readiness,reason,detected_installation,detected_json,updated_at)
         VALUES(?1,'not_initialized','TRIP Explorer 0.12.0 has not been inspected and activated for this project','unknown','{}',?2)",
        params![project_id, now],
    )?;
    Ok(())
}

pub fn require_project_ready(connection: &Connection, project_id: &str) -> Result<()> {
    let state: Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = connection
        .query_row(
            "SELECT readiness,workflow_id,package_version,upstream_source_hash,overlay_hash
             FROM trip_project_state WHERE project_id=?1",
            params![project_id],
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
    let Some((readiness, workflow, version, upstream, overlay)) = state else {
        bail!("project is not initialized for TRIP Explorer")
    };
    if readiness != "ready" {
        bail!("project TRIP readiness is {readiness}; execution requires ready")
    }
    if workflow.as_deref() != Some(WORKFLOW_ID)
        || version.as_deref() != Some(PACKAGE_VERSION)
        || upstream.as_deref() != Some(source_hash().as_str())
        || overlay.as_deref() != Some(overlay_hash().as_str())
    {
        bail!("project activation does not match the pinned workflow, source, and overlay")
    }
    let config:Option<String>=connection.query_row(
        "SELECT r.config_json FROM trip_project_state s JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
         WHERE s.project_id=?1",params![project_id],|row|row.get(0)
    ).optional()?;
    let unsupported_tier = config
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
        .and_then(|value| {
            value
                .get("profiles")
                .and_then(|profiles| profiles.as_object())
                .cloned()
        })
        .is_some_and(|profiles| {
            profiles.values().any(|profile| {
                profile
                    .get("service_tier")
                    .and_then(|tier| tier.as_str())
                    .is_some_and(|tier| !tier.trim().is_empty())
            })
        });
    if unsupported_tier {
        bail!("activated configuration requests service_tier, which this native CLI launch contract does not support")
    }
    Ok(())
}

pub fn require_task_profiles_activated(
    connection: &Connection,
    task_id: &str,
    runtime: &CapabilityRuntime,
) -> Result<()> {
    let repository: String = connection
        .query_row(
            "SELECT p.repository_path FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("task does not exist"))?;
    for role in APP_ROLES {
        let setting: Option<(i64, String)> = connection
            .query_row(
                "SELECT revision,config_json FROM role_settings WHERE task_id=?1 AND role=?2 ORDER BY revision DESC LIMIT 1",
                params![task_id, role],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (revision, config_json) = setting
            .ok_or_else(|| anyhow!("Ready tasks require a current setting for role {role}"))?;
        let role_kind: RoleKind = role.parse().map_err(|error: String| anyhow!(error))?;
        let config: RoleOverride = serde_json::from_str(&config_json)?;
        let prepared = crate::providers::prepare_role_launch_with_bundles(
            config.provider,
            role_kind,
            &config.model,
            &config.effort,
            Path::new(&repository),
            "capability admission only",
            &runtime.role_socket,
            "normalized-admission-token",
            "normalized-admission-generation",
            "normalized-admission-session",
            None,
            &runtime.hooks,
            &runtime.executable,
            &runtime.compatibility_bundles,
        )?;
        current_task_profile_authority_with_bundles(
            connection,
            task_id,
            role_kind,
            revision,
            &prepared.config,
            &runtime.compatibility_bundles,
        )
        .with_context(|| format!("role {role} lacks exact current runtime authority"))?;
    }
    Ok(())
}

pub(crate) fn task_profile_descriptor(
    connection: &Connection,
    task_id: &str,
    role: &str,
    settings_revision: i64,
) -> Result<(TaskProfileAuthority, RoleOverride)> {
    let (project_id,settings_id,config_json,project_settings,active_revision,configuration_hash,config_json_project,adapters_json):(String,String,String,String,String,String,String,String)=connection.query_row(
        "SELECT t.project_id,rs.id,rs.config_json,p.settings_json,s.active_config_revision_id,r.configuration_hash,r.config_json,r.adapters_json
         FROM tasks t JOIN projects p ON p.id=t.project_id JOIN role_settings rs ON rs.task_id=t.id AND rs.role=?2 AND rs.revision=?3
         JOIN trip_project_state s ON s.project_id=p.id JOIN trip_config_revisions r ON r.id=s.active_config_revision_id AND r.project_id=p.id
         WHERE t.id=?1",
        params![task_id,role,settings_revision],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))
    ).optional()?.ok_or_else(||anyhow!("task setting revision is unknown or no active project configuration exists"))?;
    require_project_ready(connection, &project_id)?;
    let requested: RoleOverride = serde_json::from_str(&config_json)?;
    validate_role_override(&requested)?;
    let project_settings: serde_json::Value = serde_json::from_str(&project_settings)?;
    if project_settings
        .get("trip_config_revision_id")
        .and_then(serde_json::Value::as_str)
        != Some(active_revision.as_str())
    {
        bail!("project role settings do not match its activated reviewed configuration")
    }
    let base: RoleOverride = serde_json::from_value(
        project_settings
            .pointer(&format!("/roles/{role}"))
            .cloned()
            .ok_or_else(|| anyhow!("activated project configuration is missing role {role}"))?,
    )?;
    let source = if serde_json::to_value(&requested)? == serde_json::to_value(base)? {
        "project_default"
    } else {
        "task_override"
    };
    let (adapter_name, adapter_definition, configured_profile) = if role == "manager" {
        (
            "llmrelay_service_native_manager".to_owned(),
            service_manager_adapter(requested.provider),
            None,
        )
    } else {
        let config: serde_json::Value = serde_json::from_str(&config_json_project)?;
        let adapters: serde_json::Value = serde_json::from_str(&adapters_json)?;
        let definitions = adapters
            .pointer("/adapters")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| anyhow!("activated adapter definitions are missing"))?;
        let configured_profile = config
            .pointer(&format!("/roles/{role}/profile"))
            .and_then(serde_json::Value::as_str)
            .and_then(|profile| config.pointer(&format!("/profiles/{profile}")))
            .cloned()
            .ok_or_else(|| {
                anyhow!("activated project configuration is missing role {role} profile")
            })?;
        let configured_name = configured_profile
            .get("adapter")
            .and_then(serde_json::Value::as_str);
        let required = if role == "implementer" {
            "workspace_write"
        } else {
            "read_only"
        };
        let session = if role == "final_verifier" {
            "fresh_session"
        } else {
            "resume"
        };
        let requested_provider = requested.provider.to_string();
        let eligible = |definition: &&serde_json::Value| {
            definition
                .get("provider")
                .and_then(serde_json::Value::as_str)
                == Some(requested_provider.as_str())
                && definition
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| matches!(kind, "native-agent" | "builtin-cli"))
                && definition
                    .pointer(&format!("/capabilities/{required}"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                && definition
                    .pointer(&format!("/capabilities/{session}"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        };
        let selected=if configured_name.and_then(|name|definitions.get(name)).is_some_and(|definition|eligible(&definition)) {
            configured_name.map(str::to_owned)
        } else {
            let candidates=definitions.iter().filter(|(_,definition)|eligible(definition)).map(|(name,_)|name.clone()).collect::<Vec<_>>();
            (candidates.len()==1).then(||candidates[0].clone())
        }.ok_or_else(||anyhow!("task profile needs exactly one compatible adapter from the active reviewed project configuration"))?;
        let definition = definitions
            .get(&selected)
            .cloned()
            .ok_or_else(|| anyhow!("selected task adapter disappeared"))?;
        (selected, definition, Some(configured_profile))
    };
    let authority = if role == "implementer" {
        "workspace-write"
    } else {
        "read-only"
    };
    let session = if role == "final_verifier" {
        "fresh"
    } else {
        "retained"
    };
    let profile = if source == "project_default" && role != "manager" {
        let configured_profile = configured_profile.ok_or_else(|| {
            anyhow!("activated project configuration is missing role {role} profile")
        })?;
        if configured_profile
            .get("adapter")
            .and_then(serde_json::Value::as_str)
            != Some(adapter_name.as_str())
        {
            bail!("project-default task profile does not match its activated reviewed adapter")
        }
        configured_profile
    } else {
        serde_json::json!({"adapter":adapter_name,"provider":requested.provider,"model":requested.model,"effort":requested.effort,"authority":authority,"session":session})
    };
    let descriptor = TaskProfileAuthority {
        project_id,
        task_id: task_id.to_owned(),
        role: role.parse().map_err(|error: String| anyhow!(error))?,
        settings_id,
        settings_revision,
        profile_hash: json_hash(&profile)?,
        profile_json: profile,
        project_config_revision_id: active_revision,
        project_configuration_hash: configuration_hash,
        adapter_hash: if role == "manager" {
            json_hash(&adapter_definition)?
        } else {
            json_hash(&serde_json::json!({"adapter":adapter_name,"definition":adapter_definition}))?
        },
        adapter_name,
        capability_id: String::new(),
        capability_key: String::new(),
        capability_proof_hash: String::new(),
        activation_id: None,
        source: source.into(),
    };
    Ok((descriptor, requested))
}

pub(crate) struct TaskProfilePreparationAuthority {
    pub descriptor: TaskProfileAuthority,
    pub exact_runtime_authority: bool,
    pub exact_runtime_reason: Option<String>,
    pub task_profile_activated: bool,
    pub task_profile_reason: Option<String>,
}

pub(crate) fn task_profile_preparation_authority_with_bundles(
    connection: &Connection,
    task_id: &str,
    role: RoleKind,
    revision: i64,
    launch: &crate::domain::LaunchConfig,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<TaskProfilePreparationAuthority> {
    let (mut descriptor, requested) =
        task_profile_descriptor(connection, task_id, &role.to_string(), revision)?;
    if launch.role != role
        || launch.provider != requested.provider
        || launch.model != requested.model
        || launch.effort != requested.effort
    {
        bail!("prepared launch differs from the exact task role setting revision")
    }
    let (capability, key, proof) =
        match capability_binding(connection, launch, &descriptor, bundles) {
            Ok(binding) => binding,
            Err(error) => {
                return Ok(TaskProfilePreparationAuthority {
                    descriptor,
                    exact_runtime_authority: false,
                    exact_runtime_reason: Some(format!("{error:#}")),
                    task_profile_activated: false,
                    task_profile_reason: None,
                })
            }
        };
    descriptor.capability_id = capability;
    descriptor.capability_key = key;
    descriptor.capability_proof_hash = proof;
    if descriptor.source == "project_default" {
        return Ok(TaskProfilePreparationAuthority {
            descriptor,
            exact_runtime_authority: true,
            exact_runtime_reason: None,
            task_profile_activated: false,
            task_profile_reason: None,
        });
    }
    match require_recorded_task_profile_authority(
        connection,
        task_id,
        &role.to_string(),
        revision,
    ) {
        Ok(recorded)
            if recorded.capability_id == descriptor.capability_id
                && recorded.capability_key == descriptor.capability_key
                && recorded.capability_proof_hash == descriptor.capability_proof_hash =>
        {
            descriptor.activation_id = recorded.activation_id;
            Ok(TaskProfilePreparationAuthority {
                descriptor,
                exact_runtime_authority: true,
                exact_runtime_reason: None,
                task_profile_activated: true,
                task_profile_reason: None,
            })
        }
        Ok(_) => Ok(TaskProfilePreparationAuthority {
            descriptor,
            exact_runtime_authority: true,
            exact_runtime_reason: None,
            task_profile_activated: false,
            task_profile_reason: Some(
                "task-profile activation does not match the exact current ordinary capability evidence"
                    .into(),
            ),
        }),
        Err(error) => Ok(TaskProfilePreparationAuthority {
            descriptor,
            exact_runtime_authority: true,
            exact_runtime_reason: None,
            task_profile_activated: false,
            task_profile_reason: Some(format!("{error:#}")),
        }),
    }
}

fn runtime_scope_matches(
    proof: &serde_json::Value,
    expected: &RuntimeScopeExpectation<'_>,
) -> bool {
    let Some(scope) = proof
        .get("runtime_scope")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let task_matches = match expected.task_id {
        Some(task_id) => scope.get("task_id").and_then(serde_json::Value::as_str) == Some(task_id),
        None => scope.get("task_id").is_some_and(serde_json::Value::is_null),
    };
    if scope.get("project_id").and_then(serde_json::Value::as_str) != Some(expected.project_id)
        || !task_matches
        || scope
            .get("setup_operation_id")
            .and_then(serde_json::Value::as_str)
            != Some(expected.setup_operation_id)
        || scope
            .get("fixture_project_id")
            .and_then(serde_json::Value::as_str)
            != Some(expected.fixture_project_id)
        || scope
            .get("fixture_repository_identity")
            .and_then(serde_json::Value::as_str)
            != Some(expected.fixture_repository_identity)
        || scope
            .get("fixture_root_hash")
            .and_then(serde_json::Value::as_str)
            != Some(expected.fixture_root_hash)
    {
        return false;
    }
    scope
        .get("profiles")
        .and_then(serde_json::Value::as_array)
        .and_then(|profiles| {
            profiles.iter().find(|profile| {
                profile.get("role").and_then(serde_json::Value::as_str)
                    == Some(expected.role.to_string().as_str())
            })
        })
        .is_some_and(|profile| {
            let identity = profile
                .get("capability_identity")
                .and_then(|value| match value {
                    serde_json::Value::String(value) => {
                        serde_json::from_str::<serde_json::Value>(value).ok()
                    }
                    value => Some(value.clone()),
                });
            profile
                .get("settings_revision")
                .and_then(serde_json::Value::as_i64)
                == expected.settings_revision
                && profile.get("source").and_then(serde_json::Value::as_str)
                    == Some(expected.source)
                && profile
                    .get("profile_hash")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected.profile_hash)
                && profile
                    .get("project_config_revision_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected.project_config_revision_id)
                && profile
                    .get("project_configuration_hash")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected.project_configuration_hash)
                && profile.get("adapter").and_then(serde_json::Value::as_str)
                    == Some(expected.adapter_name)
                && profile
                    .get("adapter_hash")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected.adapter_hash)
                && profile
                    .get("capability_key")
                    .and_then(serde_json::Value::as_str)
                    == Some(expected.capability_key)
                && identity.as_ref() == Some(expected.capability_identity)
        })
}

fn scoped_capability_binding(
    connection: &Connection,
    config: &crate::domain::LaunchConfig,
    expected: &RuntimeScopeExpectation<'_>,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<Option<(String, String, String)>> {
    let identity = crate::providers::capability_identity(config)?;
    crate::providers::require_current_capability_identity_with_bundles(
        &identity,
        &config.cwd,
        bundles,
    )?;
    let key = crate::providers::capability_key(config)?;
    if key != expected.capability_key {
        return Ok(None);
    }
    let binding:Option<(String,String)>=connection.query_row(
        "SELECT id,proof_json FROM capabilities current_capability WHERE current_capability.rowid=(SELECT latest_capability.rowid FROM capabilities latest_capability WHERE latest_capability.provider=?1 AND latest_capability.executable_version=?2 AND latest_capability.role=?3 AND latest_capability.mode='interactive_pty' ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1) AND current_capability.config_hash=?4 AND current_capability.status='supported' AND current_capability.proof_json!='{}'",
        params![config.provider.to_string(),config.executable_version,config.role.to_string(),key],|row|Ok((row.get(0)?,row.get(1)?))
    ).optional()?;
    let Some((id, proof_text)) = binding else {
        return Ok(None);
    };
    let proof = serde_json::from_str::<serde_json::Value>(&proof_text)?;
    let mut scopes = proof
        .get("runtime_scopes")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(scope) = proof.get("runtime_scope").filter(|scope| !scope.is_null()) {
        if !scopes.contains(scope) {
            scopes.push(scope.clone());
        }
    }
    let Some(scope) = scopes
        .into_iter()
        .find(|scope| runtime_scope_matches(&serde_json::json!({"runtime_scope":scope}), expected))
    else {
        return Ok(None);
    };
    Ok(Some((id, key, json_hash(&scope)?)))
}

fn capability_binding(
    connection: &Connection,
    config: &crate::domain::LaunchConfig,
    descriptor: &TaskProfileAuthority,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<(String, String, String)> {
    let (setup_id, fixture_project, fixture_root, fixture_identity, _) =
        runtime_fixture_scope(connection, &descriptor.project_id)?;
    let capability_identity = serde_json::to_value(crate::providers::capability_identity(config)?)?;
    let task_id = (descriptor.source == "task_override").then_some(descriptor.task_id.as_str());
    let settings_revision = task_id.map(|_| descriptor.settings_revision);
    scoped_capability_binding(
        connection,
        config,
        &RuntimeScopeExpectation {
            project_id: &descriptor.project_id,
            task_id,
            role: descriptor.role,
            settings_revision,
            source: &descriptor.source,
            profile_hash: &descriptor.profile_hash,
            project_config_revision_id: &descriptor.project_config_revision_id,
            project_configuration_hash: &descriptor.project_configuration_hash,
            adapter_name: &descriptor.adapter_name,
            adapter_hash: &descriptor.adapter_hash,
            capability_key: &crate::providers::capability_key(config)?,
            capability_identity: &capability_identity,
            setup_operation_id: &setup_id,
            fixture_project_id: &fixture_project,
            fixture_repository_identity: &fixture_identity,
            fixture_root_hash: &sha256(fixture_root.as_bytes()),
        },
        bundles,
    )?
    .ok_or_else(|| anyhow!("exact current runtime-scoped ordinary production capability proof is missing or stale; complete runtime verification for this project, setup, fixture, configuration, adapter, profile, and prepared identity"))
}

pub(crate) fn require_recorded_task_profile_authority(
    connection: &Connection,
    task_id: &str,
    role: &str,
    revision: i64,
) -> Result<TaskProfileAuthority> {
    let (mut descriptor, _) = task_profile_descriptor(connection, task_id, role, revision)?;
    if descriptor.source == "project_default" {
        return Ok(descriptor);
    }
    let binding:Option<(String,String,String,String)>=connection.query_row(
        "SELECT a.id,a.capability_id,a.capability_key,a.capability_proof_hash FROM trip_task_profile_activations a JOIN capabilities c ON c.id=a.capability_id
         WHERE a.task_id=?1 AND a.role=?2 AND a.settings_id=?3 AND a.settings_revision=?4 AND a.profile_hash=?5
           AND a.project_config_revision_id=?6 AND a.project_configuration_hash=?7 AND a.adapter_name=?8 AND a.adapter_hash=?9
           AND c.rowid=(SELECT latest_capability.rowid FROM capabilities latest_capability WHERE latest_capability.provider=c.provider AND latest_capability.executable_version=c.executable_version AND latest_capability.role=c.role AND latest_capability.mode=c.mode ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1)
           AND c.status='supported' AND c.config_hash=a.capability_key AND c.proof_json!='{}' ORDER BY a.activated_at DESC LIMIT 1",
        params![task_id,role,descriptor.settings_id,revision,descriptor.profile_hash,descriptor.project_config_revision_id,descriptor.project_configuration_hash,descriptor.adapter_name,descriptor.adapter_hash],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
    ).optional()?;
    let (activation,capability,key,proof)=binding.ok_or_else(||anyhow!("role {role} setting revision {revision} is pending explicit task-profile activation with exact current capability evidence"))?;
    descriptor.activation_id = Some(activation);
    descriptor.capability_id = capability;
    descriptor.capability_key = key;
    descriptor.capability_proof_hash = proof;
    Ok(descriptor)
}

pub(crate) fn current_task_profile_authority_with_bundles(
    connection: &Connection,
    task_id: &str,
    role: RoleKind,
    revision: i64,
    launch: &crate::domain::LaunchConfig,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<TaskProfileAuthority> {
    let (mut descriptor, requested) =
        task_profile_descriptor(connection, task_id, &role.to_string(), revision)?;
    if launch.role != role
        || launch.provider != requested.provider
        || launch.model != requested.model
        || launch.effort != requested.effort
    {
        bail!("prepared launch differs from the exact task role setting revision")
    }
    let (capability, key, proof) = capability_binding(connection, launch, &descriptor, bundles)?;
    if descriptor.source == "task_override" {
        let recorded = require_recorded_task_profile_authority(
            connection,
            task_id,
            &role.to_string(),
            revision,
        )?;
        if recorded.capability_id != capability
            || recorded.capability_key != key
            || recorded.capability_proof_hash != proof
        {
            bail!("task-profile activation does not match the exact current ordinary capability evidence")
        }
        descriptor.activation_id = recorded.activation_id;
    }
    descriptor.capability_id = capability;
    descriptor.capability_key = key;
    descriptor.capability_proof_hash = proof;
    Ok(descriptor)
}

pub(crate) fn bind_attempt_profiles(
    connection: &Connection,
    task_id: &str,
    attempt_id: &str,
    cwd: &Path,
    runtime: &CapabilityRuntime,
    now: &str,
) -> Result<()> {
    for role_name in APP_ROLES {
        let (revision,config_json):(i64,String)=connection.query_row(
            "SELECT revision,config_json FROM role_settings WHERE task_id=?1 AND role=?2 ORDER BY revision DESC LIMIT 1",
            params![task_id,role_name],|row|Ok((row.get(0)?,row.get(1)?)))?;
        let role: RoleKind = role_name.parse().map_err(|error: String| anyhow!(error))?;
        let config: RoleOverride = serde_json::from_str(&config_json)?;
        let prepared = crate::providers::prepare_role_launch_with_bundles(
            config.provider,
            role,
            &config.model,
            &config.effort,
            cwd,
            "capability admission only",
            &runtime.role_socket,
            "normalized-admission-token",
            "normalized-admission-generation",
            "normalized-admission-session",
            None,
            &runtime.hooks,
            &runtime.executable,
            &runtime.compatibility_bundles,
        )?;
        let authority = current_task_profile_authority_with_bundles(
            connection,
            task_id,
            role,
            revision,
            &prepared.config,
            &runtime.compatibility_bundles,
        )?;
        connection.execute("INSERT INTO trip_attempt_profiles(attempt_id,role,settings_revision,activation_id,source,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_id,capability_key,capability_proof_hash,bound_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![attempt_id,role_name,revision,authority.activation_id,authority.source,authority.profile_json.to_string(),authority.profile_hash,authority.project_config_revision_id,authority.project_configuration_hash,authority.adapter_name,authority.adapter_hash,authority.capability_id,authority.capability_key,authority.capability_proof_hash,now])?;
    }
    Ok(())
}

pub(crate) fn require_attempt_profile_launch_with_bundles(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
    revision: i64,
    launch: &crate::domain::LaunchConfig,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<()> {
    let (capability_id,capability_key,proof_hash):(String,String,String)=connection.query_row(
        "SELECT capability_id,capability_key,capability_proof_hash FROM trip_attempt_profiles WHERE attempt_id=?1 AND role=?2 AND settings_revision=?3",
        params![attempt_id,role.to_string(),revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))
    ).optional()?.ok_or_else(||anyhow!("attempt lacks an effective task-profile binding for this exact role setting revision"))?;
    let task_id: String = connection.query_row(
        "SELECT task_id FROM attempts WHERE id=?1",
        params![attempt_id],
        |row| row.get(0),
    )?;
    let authority = current_task_profile_authority_with_bundles(
        connection, &task_id, role, revision, launch, bundles,
    )?;
    let (current_id, current_key, current_proof) = (
        authority.capability_id,
        authority.capability_key,
        authority.capability_proof_hash,
    );
    if (capability_id, capability_key, proof_hash) != (current_id, current_key, current_proof) {
        bail!("attempt task-profile capability binding is stale; explicit activation is required before replacement")
    }
    Ok(())
}

pub(crate) fn replace_attempt_profile(
    connection: &Connection,
    attempt_id: &str,
    authority: &TaskProfileAuthority,
    now: &str,
) -> Result<()> {
    let changed=connection.execute("UPDATE trip_attempt_profiles SET settings_revision=?1,activation_id=?2,source=?3,profile_json=?4,profile_hash=?5,project_config_revision_id=?6,project_configuration_hash=?7,adapter_name=?8,adapter_hash=?9,capability_id=?10,capability_key=?11,capability_proof_hash=?12,bound_at=?13 WHERE attempt_id=?14 AND role=?15",params![authority.settings_revision,authority.activation_id,authority.source,authority.profile_json.to_string(),authority.profile_hash,authority.project_config_revision_id,authority.project_configuration_hash,authority.adapter_name,authority.adapter_hash,authority.capability_id,authority.capability_key,authority.capability_proof_hash,now,attempt_id,authority.role.to_string()])?;
    if changed != 1 {
        bail!("attempt task-profile binding is unavailable for replacement")
    }
    let workspace_policy: Option<String> = connection
        .query_row(
            "SELECT policy_json FROM workspaces WHERE attempt_id=?1 AND state='ready'",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(workspace_policy) = workspace_policy {
        let profiles = {
            let mut statement=connection.prepare("SELECT json_object('role',role,'settings_revision',settings_revision,'source',source,'profile',json(profile_json),'profile_hash',profile_hash,'project_config_revision_id',project_config_revision_id,'project_configuration_hash',project_configuration_hash,'adapter',adapter_name,'adapter_hash',adapter_hash,'capability_id',capability_id,'capability_key',capability_key,'capability_proof_hash',capability_proof_hash) FROM trip_attempt_profiles WHERE attempt_id=?1 ORDER BY role")?;
            let rows = statement
                .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                .collect::<serde_json::Result<Vec<_>>>()?
        };
        if profiles.len() != 6 {
            bail!("effective attempt policy requires six task-profile bindings")
        }
        let mut policy: serde_json::Value = serde_json::from_str(&workspace_policy)?;
        let object = policy
            .as_object_mut()
            .ok_or_else(|| anyhow!("workspace policy must be an object"))?;
        object.insert("task_profiles".into(), serde_json::Value::Array(profiles));
        object.insert(
            "base_project_configuration_distinct".into(),
            serde_json::Value::Bool(true),
        );
        connection.execute(
            "UPDATE workspaces SET policy_json=?1,updated_at=?2 WHERE attempt_id=?3 AND state='ready'",
            params![policy.to_string(), now, attempt_id],
        )?;
    }
    Ok(())
}

/// Where a read-only role stands relative to its latest activated task profile.
pub(crate) enum ReadOnlyProfileBoundary {
    /// The attempt already uses the newest profile that applies to it.
    Current,
    /// A newer activation can replace the attempt binding now.
    Ready {
        authority: TaskProfileAuthority,
        from_revision: i64,
    },
    /// A newer activation exists but must not be used yet. `needs_person` is
    /// true when only a person can clear it (the profile needs verifying again);
    /// otherwise it clears when the current session of the role finishes.
    Pending {
        /// Stable code naming the first unmet condition.
        reason: &'static str,
        message: String,
        needs_person: bool,
        /// The newer activated settings revision that is waiting.
        settings_revision: i64,
    },
}

/// Evaluates whether the explorer or a reviewer of this attempt should move to
/// a newer activated task profile before its next fresh session. These roles
/// never write, and every review or exploration starts a new session, so the
/// boundary between sessions is safe: no live generation of the role, no review
/// of its kind in flight, and no pending switch. Writers keep their existing
/// explicit switch flow and are always `Current` here.
///
/// An activation recorded for a different project configuration does not apply
/// to this attempt, which keeps the configuration it was reviewed with.
pub(crate) fn read_only_profile_boundary(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
) -> Result<ReadOnlyProfileBoundary> {
    let kind = match role {
        RoleKind::Explorer => None,
        RoleKind::PlanReviewer => Some("plan"),
        RoleKind::CodeReviewer => Some("code"),
        RoleKind::FinalReviewer => Some("final"),
        RoleKind::Manager | RoleKind::Implementer => return Ok(ReadOnlyProfileBoundary::Current),
    };
    let role_name = role.to_string();
    let bound: Option<(String, i64, String)> = connection
        .query_row(
            "SELECT a.task_id,ap.settings_revision,ap.project_config_revision_id
             FROM trip_attempt_profiles ap JOIN attempts a ON a.id=ap.attempt_id
             WHERE ap.attempt_id=?1 AND ap.role=?2",
            params![attempt_id, role_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((task_id, bound_revision, bound_configuration)) = bound else {
        return Ok(ReadOnlyProfileBoundary::Current);
    };
    let latest: Option<i64> = connection.query_row(
        "SELECT MAX(settings_revision) FROM trip_task_profile_activations
             WHERE task_id=?1 AND role=?2 AND project_config_revision_id=?3",
        params![task_id, role_name, bound_configuration],
        |row| row.get(0),
    )?;
    let Some(revision) = latest.filter(|revision| *revision > bound_revision) else {
        return Ok(ReadOnlyProfileBoundary::Current);
    };
    let label = role.label();
    // The role's next session may start with new settings only when nothing of
    // the old one can still act: every earlier session exited with a verified
    // quiet process group, and no request, prompt, delivery, control, recovery,
    // capture, check or writer could still bind the old settings.
    let phase: String = connection.query_row(
        "SELECT phase FROM attempts WHERE id=?1",
        params![attempt_id],
        |row| row.get(0),
    )?;
    let phase_matches = match role {
        RoleKind::PlanReviewer => phase == "plan_review",
        RoleKind::CodeReviewer => phase == "code_review",
        RoleKind::FinalReviewer => phase == "final_review",
        _ => matches!(
            phase.as_str(),
            "planning" | "implementation" | "code_review" | "checks" | "final_review"
        ),
    };
    // Only the task's current, unfinished attempt, while it is running, can
    // move a role to new settings.
    let current: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
           WHERE a.id=?1 AND a.status='running' AND t.archived_at IS NULL
             AND t.lifecycle IN ('in_progress','validation')
             AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=a.task_id
               ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1))",
        params![attempt_id],
        |row| row.get(0),
    )?;
    if !current {
        return Ok(ReadOnlyProfileBoundary::Pending {
            reason: "attempt_not_current",
            message: format!(
                "The new {label} settings apply when this task's current attempt is running again."
            ),
            needs_person: false,
            settings_revision: revision,
        });
    }
    if !phase_matches {
        return Ok(ReadOnlyProfileBoundary::Pending {
            reason: "phase_mismatch",
            message: format!(
                "The new {label} settings apply when this task next reaches a {label} step."
            ),
            needs_person: false,
            settings_revision: revision,
        });
    }
    const FENCES: &[(&str, &str)] = &[
        ("role_session_live",
         "SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role=?2
            AND status IN ('launch_reserved','running','stopping'))"),
        ("role_process_not_proven_quiescent",
         "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
            WHERE g.attempt_id=?1 AND g.role=?2
              AND NOT (s.status='launch_failed'
                OR (s.status='exited'
                  AND COALESCE(CASE WHEN json_valid(s.exit_json)
                    THEN json_extract(s.exit_json,'$.process_group_quiescent') END,0)=1)))"),
        ("review_in_flight",
         "SELECT EXISTS(SELECT 1 FROM review_requests WHERE attempt_id=?1 AND review_kind=?3
            AND delivery_state IN ('launching','delivered','ambiguous'))"),
        ("switch_pending",
         "SELECT EXISTS(SELECT 1 FROM switch_intents WHERE attempt_id=?1 AND role=?2
            AND state NOT IN ('completed','cancelled','rejected','superseded'))"),
        ("permission_pending",
         "SELECT EXISTS(SELECT 1 FROM permission_requests WHERE attempt_id=?1
            AND consumed_at IS NULL AND delivery_state NOT IN ('expired','not_delivered')
            AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
              WHERE native.permission_request_id=permission_requests.id))"),
        ("input_control",
         "SELECT EXISTS(SELECT 1 FROM input_leases lease JOIN sessions s ON s.id=lease.session_id
            JOIN role_generations g ON g.id=s.role_generation_id
            WHERE g.attempt_id=?1 AND lease.revoked_at IS NULL
              AND julianday(lease.expires_at)>julianday('now'))"),
        ("guidance_uncertain",
         "SELECT EXISTS(SELECT 1 FROM guidance_messages WHERE attempt_id=?1
            AND state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))"),
        ("control_pending",
         "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
            AND state IN ('requested','draining','held','recovery_required'))"),
        ("restart_hold",
         "SELECT EXISTS(SELECT 1 FROM restart_candidates WHERE attempt_id=?1
            AND state NOT IN ('resumed','released_fresh_dispatch','cancelled'))"),
        ("recovery_open",
         "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1
            AND state='attention_required')"),
        ("freeze_in_progress",
         "SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1
            AND state IN ('reserved','capturing','recovery_required'))"),
        ("check_running",
         "SELECT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1
            AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))"),
        ("conflicting_role_live",
         "SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role!=?2
            AND role NOT IN ('manager','explorer')
            AND status IN ('launch_reserved','running','stopping'))"),
    ];
    let values: [&dyn rusqlite::ToSql; 3] = [&attempt_id, &role_name, &kind];
    for (reason, sql) in FENCES {
        // Each fence binds the leading parameters it uses.
        let mut statement = connection.prepare(sql)?;
        let used = statement.parameter_count();
        let blocked: bool = statement.query_row(&values[..used], |row| row.get(0))?;
        if blocked {
            return Ok(ReadOnlyProfileBoundary::Pending {
                reason,
                message: format!("{label} settings changed. The new settings apply once the current {label} work finishes and nothing else is waiting on it, so the next {label} session starts with them."),
                needs_person: false,
                settings_revision: revision,
            });
        }
    }
    let verify_again = || {
        ReadOnlyProfileBoundary::Pending {
        reason: "evidence_stale",
        message: format!("The new {label} settings need their agent profile verified again before the next {label} session can start. Open the task's Agent settings and verify the profile."),
        needs_person: true,
        settings_revision: revision,
    }
    };
    // The same current-capability rule that ordinary task-profile authority
    // uses: the activation's capability must still be the newest supported
    // observation for that executable and role.
    let activation: Option<(String, String, String, String, String, String)> = connection
        .query_row(
            "SELECT a.id,a.profile_hash,a.adapter_hash,a.capability_id,a.capability_key,a.capability_proof_hash
             FROM trip_task_profile_activations a JOIN capabilities c ON c.id=a.capability_id
             WHERE a.task_id=?1 AND a.role=?2 AND a.settings_revision=?3 AND a.project_config_revision_id=?4
               AND c.rowid=(SELECT latest_capability.rowid FROM capabilities latest_capability
                 WHERE latest_capability.provider=c.provider AND latest_capability.executable_version=c.executable_version
                   AND latest_capability.role=c.role AND latest_capability.mode=c.mode
                 ORDER BY latest_capability.checked_at DESC,latest_capability.rowid DESC LIMIT 1)
               AND c.status='supported' AND c.config_hash=a.capability_key AND c.proof_json!='{}'
             ORDER BY a.activated_at DESC,a.rowid DESC LIMIT 1",
            params![task_id, role_name, revision, bound_configuration],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()?;
    let Some((activation_id, profile_hash, adapter_hash, capability_id, capability_key, proof)) =
        activation
    else {
        return Ok(verify_again());
    };
    let Ok((mut authority, _)) =
        task_profile_descriptor(connection, &task_id, &role_name, revision)
    else {
        return Ok(verify_again());
    };
    if authority.profile_hash != profile_hash
        || authority.adapter_hash != adapter_hash
        || authority.project_config_revision_id != bound_configuration
    {
        return Ok(verify_again());
    }
    authority.activation_id = Some(activation_id);
    authority.capability_id = capability_id;
    authority.capability_key = capability_key;
    authority.capability_proof_hash = proof;
    Ok(ReadOnlyProfileBoundary::Ready {
        authority,
        from_revision: bound_revision,
    })
}

/// Applies a `Ready` boundary inside the caller's transaction and records the
/// replacement. Other outcomes are returned unchanged, so callers decide
/// whether to wait.
pub(crate) fn materialize_read_only_profile(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
    now: &str,
) -> Result<ReadOnlyProfileBoundary> {
    let boundary = read_only_profile_boundary(connection, attempt_id, role)?;
    if let ReadOnlyProfileBoundary::Ready {
        authority,
        from_revision,
    } = &boundary
    {
        // The ready workspace's reviewed policy and materialized files must be
        // intact before its profile projection is rewritten, and still intact
        // afterwards; any drift aborts the whole transaction.
        let ready_workspace: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE attempt_id=?1 AND state='ready')",
            params![attempt_id],
            |row| row.get(0),
        )?;
        if ready_workspace {
            require_attempt_ready(connection, attempt_id, None)?;
        }
        replace_attempt_profile(connection, attempt_id, authority, now)?;
        if ready_workspace {
            require_attempt_ready(connection, attempt_id, None)?;
            let projected: Option<i64> = connection.query_row(
                "SELECT (SELECT json_extract(profile.value,'$.settings_revision')
                         FROM json_each(json_extract(w.policy_json,'$.task_profiles')) profile
                         WHERE json_extract(profile.value,'$.role')=?2)
                 FROM workspaces w WHERE w.attempt_id=?1 AND w.state='ready'",
                params![attempt_id, role.to_string()],
                |row| row.get(0),
            )?;
            if projected != Some(authority.settings_revision) {
                bail!("workspace policy did not record the new {role} settings")
            }
        }
        // The receipt names the exact task version and frozen work the new
        // settings were adopted for, in the same transaction as the request.
        let (task_version, phase, plan_hash, candidate_hash): (
            i64,
            String,
            Option<String>,
            Option<String>,
        ) = connection.query_row(
            "SELECT t.version,a.phase,a.plan_hash,a.candidate_hash
             FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        connection.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at) VALUES(?1,?2,'service','task.profile.materialized_at_safe_boundary','attempt',?3,?4,?5)",params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),attempt_id,serde_json::json!({"role":role,"from_settings_revision":from_revision,"to_settings_revision":authority.settings_revision,"activation_id":authority.activation_id,"task_version":task_version,"phase":phase,"plan_hash":plan_hash,"candidate_hash":candidate_hash}).to_string(),now])?;
    }
    Ok(boundary)
}

pub fn activate_task_profile(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    task_id: &str,
    role: RoleKind,
    settings_revision: i64,
    expected_version: i64,
) -> Result<OperationResult> {
    let request = serde_json::json!({"task_id":task_id,"role":role,"settings_revision":settings_revision,"expected_version":expected_version});
    let request_hash = json_hash(&request)?;
    let mut connection = store.lock()?;
    if let Some(result)=connection.query_row("SELECT result_json FROM operation_receipts WHERE operation_id=?1 AND actor_key='human_control' AND operation_kind='task_profile_activation' AND request_hash=?2",params![operation_id,request_hash],|row|row.get::<_,String>(0)).optional()? { return Ok(serde_json::from_str(&result)?); }
    let (cwd,latest,lifecycle):(String,i64,String)=connection.query_row(
        "SELECT COALESCE((SELECT w.path FROM attempts a JOIN workspaces w ON w.attempt_id=a.id WHERE a.task_id=t.id AND w.state='ready' ORDER BY a.created_at DESC LIMIT 1),p.repository_path),
          (SELECT MAX(revision) FROM role_settings WHERE task_id=t.id AND role=?2),t.lifecycle FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?1 AND t.version=?3",
        params![task_id,role.to_string(),expected_version],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))
    ).optional()?.ok_or_else(||anyhow!("task version is stale"))?;
    if latest != settings_revision {
        bail!("task profile activation requires the latest exact role setting revision")
    }
    if matches!(lifecycle.as_str(), "done" | "cancelled") {
        bail!("terminal tasks do not accept task-profile activation")
    }
    let (mut authority, config) =
        task_profile_descriptor(&connection, task_id, &role.to_string(), settings_revision)?;
    let launch = crate::providers::prepare_role_launch_with_bundles(
        config.provider,
        role,
        &config.model,
        &config.effort,
        Path::new(&cwd),
        "task profile capability activation",
        &runtime.role_socket,
        "normalized-activation-token",
        "normalized-activation-generation",
        "normalized-activation-session",
        None,
        &runtime.hooks,
        &runtime.executable,
        &runtime.compatibility_bundles,
    )?;
    let (capability, key, proof) = capability_binding(
        &connection,
        &launch.config,
        &authority,
        &runtime.compatibility_bundles,
    )?;
    authority.capability_id = capability;
    authority.capability_key = key;
    authority.capability_proof_hash = proof;
    let now = Utc::now().to_rfc3339();
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT OR IGNORE INTO trip_task_profile_activations(id,task_id,role,settings_id,settings_revision,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_id,capability_key,capability_proof_hash,activated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![id,task_id,role.to_string(),authority.settings_id,settings_revision,authority.profile_json.to_string(),authority.profile_hash,authority.project_config_revision_id,authority.project_configuration_hash,authority.adapter_name,authority.adapter_hash,authority.capability_id,authority.capability_key,authority.capability_proof_hash,now])?;
    let activation:String=tx.query_row("SELECT id FROM trip_task_profile_activations WHERE task_id=?1 AND role=?2 AND settings_revision=?3 AND project_config_revision_id=?4 AND adapter_hash=?5 AND capability_id=?6 AND capability_key=?7 AND capability_proof_hash=?8 ORDER BY activated_at DESC LIMIT 1",params![task_id,role.to_string(),settings_revision,authority.project_config_revision_id,authority.adapter_hash,authority.capability_id,authority.capability_key,authority.capability_proof_hash],|row|row.get(0))?;
    authority.activation_id = Some(activation.clone());
    let unstarted_attempt: Option<String> = tx
        .query_row(
            "SELECT a.id FROM attempts a WHERE a.task_id=?1 AND a.status IN ('workspace_reserved','running','held')
             AND EXISTS(SELECT 1 FROM trip_attempt_profiles ap WHERE ap.attempt_id=a.id AND ap.role=?2)
             AND NOT EXISTS(SELECT 1 FROM role_generations rg WHERE rg.attempt_id=a.id AND rg.role=?2)
             ORDER BY a.created_at DESC LIMIT 1",
            params![task_id, role.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(attempt_id) = unstarted_attempt.as_deref() {
        replace_attempt_profile(&tx, attempt_id, &authority, &now)?;
    }
    let changed=tx.execute("UPDATE tasks SET version=version+1,attention=CASE WHEN lifecycle='backlog' THEN 'needs_input' ELSE attention END,updated_at=?1 WHERE id=?2 AND version=?3",params![now,task_id,expected_version])?;
    if changed != 1 {
        bail!("task version changed during profile activation")
    }
    let result = OperationResult {
        operation_id: operation_id.into(),
        entity_kind: "task".into(),
        entity_id: task_id.into(),
        version: Some(expected_version + 1),
        state: if matches!(
            lifecycle.as_str(),
            "in_progress" | "validation" | "awaiting_review"
        ) {
            "replacement_eligible_at_safe_boundary".into()
        } else {
            "task_profile_activated".into()
        },
        detail: serde_json::json!({"activation_id":activation,"role":role,"settings_revision":settings_revision,"source":authority.source,"adapter":authority.adapter_name,"adapter_hash":authority.adapter_hash,"capability_key":authority.capability_key,"project_config_revision_id":authority.project_config_revision_id,"unstarted_attempt_binding_updated":unstarted_attempt.is_some()}),
    };
    tx.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,new_version,detail_json,created_at) VALUES(?1,?2,'human','task.profile.activated','task',?3,?4,?5,?6)",params![uuid::Uuid::new_v4().to_string(),operation_id,task_id,expected_version+1,serde_json::to_string(&result)?,now])?;
    tx.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','task_profile_activation',?2,?3,?4)",params![operation_id,request_hash,serde_json::to_string(&result)?,now])?;
    tx.commit()?;
    Ok(result)
}

pub fn require_attempt_ready(
    connection: &Connection,
    attempt_id: &str,
    setup_permit_id: Option<&str>,
) -> Result<()> {
    let (project_id, internal_purpose, setup_operation): (String, Option<String>, Option<String>) =
        connection.query_row(
            "SELECT t.project_id,p.internal_purpose,a.setup_operation_id
             FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
             WHERE a.id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    if internal_purpose.as_deref() == Some("trip_setup_fixture") {
        let permit = setup_permit_id
            .ok_or_else(|| anyhow!("setup fixture dispatch requires an exact setup permit"))?;
        let valid: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM trip_setup_permits sp
             JOIN trip_setup_operations so ON so.id=sp.setup_operation_id
             JOIN projects p ON p.id=sp.fixture_project_id
             WHERE sp.id=?1 AND sp.attempt_id=?2 AND sp.setup_operation_id=?3
               AND sp.state='issued' AND p.id=?4 AND p.internal_purpose='trip_setup_fixture'
               AND p.repository_identity=sp.fixture_repository_identity
               AND NOT (sp.role='manager' AND EXISTS(
                 SELECT 1 FROM controls c WHERE c.attempt_id=sp.attempt_id
                   AND c.kind='setup_manager_change' AND c.state='held'
               ))
               AND ((sp.purpose IN ('setup_discovery','profile_probe')
                       AND so.state IN ('discovery','probing','preflight_complete'))
                    OR (sp.purpose='runtime_probe' AND so.state='activated'
                       AND EXISTS(SELECT 1 FROM trip_runtime_probes probe
                         JOIN trip_runtime_admissions admission ON admission.id=probe.admission_id
                         JOIN trip_project_state project_state ON project_state.project_id=admission.project_id
                         WHERE probe.attempt_id=sp.attempt_id AND probe.role=sp.role
                           AND probe.fixture_project_id=sp.fixture_project_id
                           AND admission.project_id=so.project_id
                           AND (project_state.setup_operation_id=so.id OR EXISTS(
                             SELECT 1 FROM trip_setup_operations pending
                             WHERE pending.id=project_state.setup_operation_id
                               AND pending.state!='activated' AND pending.supersedes_setup_operation_id=so.id))
                           AND admission.state IN ('authorized','running','awaiting_publication','failed')
                           AND probe.state IN ('authorized','running','awaiting_resume','failed')))))",
            params![permit, attempt_id, setup_operation, project_id],
            |row| row.get(0),
        )?;
        if !valid {
            bail!("setup permit does not match the exact operation, attempt, fixture identity, and approved action")
        }
        verify_ready_workspace_policy(connection, attempt_id)?;
        return Ok(());
    }
    require_project_ready(connection, &project_id)?;
    let migrated: bool = connection.query_row(
        "SELECT workflow_version=?2 AND workflow_hash=?3 AND legacy_migration_required=0
         FROM attempts WHERE id=?1",
        params![
            attempt_id,
            WORKFLOW_ID,
            crate::workflow_resources::workflow_hash()
        ],
        |row| row.get(0),
    )?;
    if !migrated {
        bail!("attempt requires explicit migration to the activated TRIP workflow before dispatch")
    }
    verify_ready_workspace_policy(connection, attempt_id)?;
    Ok(())
}

fn verify_ready_workspace_policy(connection: &Connection, attempt_id: &str) -> Result<()> {
    let workspace: Option<(String, String, String)> = connection
        .query_row(
            "SELECT path,state,policy_json FROM workspaces WHERE attempt_id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((path, state, policy_json)) = workspace else {
        return Ok(());
    };
    if state != "ready" {
        return Ok(());
    }
    let root = PathBuf::from(path).canonicalize()?;
    let policy = validate_materialized_policy(&policy_json)?;
    verified_policy_paths(&root, &policy)?;
    Ok(())
}

/// Documentation paths the approved structured plan explicitly owns. Two plan
/// shapes name them exactly:
///
/// - `ownership.exact_paths.documentation`, an explicit path array; paths in
///   the other `exact_paths` categories (code, tests, dependencies) are not
///   documentation ownership, and `documentation` keys are labels;
/// - `ownership.owned_paths` (or each lane's `owned_paths`) intersected with
///   `documentation` keys that are literal paths.
///
/// Only exact array entries and keys count, never narrative text, and any path
/// also listed in `ownership.protected` is excluded.
/// Workflow authority artifacts that no guidance approval may re-pin, whatever
/// the project configuration or approved plan lists. These are the paths setup
/// installs or owns: the TRIP package under `.agents/` (skills, base package,
/// bin, overlay, `config.json` with its profiles, `adapters.json`,
/// `preflight.json`, `manifest.json`), the alternate `.claude/` skill root and
/// provider settings under `.claude/` and `.codex/`, the setup-owned root
/// `AGENTS.md`, and Git metadata. Comparison ignores ASCII case because a
/// case-insensitive filesystem resolves `.AGENTS/...` to `.agents/...`.
/// Ordinary project documentation such as `README.md` or `docs/WORKFLOWS.md`
/// is not protected.
fn is_protected_workflow_artifact(relative: &str) -> bool {
    const PROTECTED_ROOTS: &[&str] = &[".agents", ".claude", ".codex", ".git"];
    const PROTECTED_FILES: &[&str] = &["AGENTS.md"];
    let first = relative.split('/').next().unwrap_or(relative);
    PROTECTED_ROOTS
        .iter()
        .any(|root| first.eq_ignore_ascii_case(root))
        || PROTECTED_FILES
            .iter()
            .any(|file| relative.eq_ignore_ascii_case(file))
}

fn approved_documentation_paths(plan: &serde_json::Value) -> BTreeSet<String> {
    let strings = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let ownership = plan.get("ownership");
    let mut documented = strings(
        ownership
            .and_then(|value| value.get("exact_paths"))
            .and_then(|value| value.get("documentation")),
    )
    .into_iter()
    .collect::<BTreeSet<_>>();
    let mut owned = ["owned_paths", "exact_owned_paths"]
        .into_iter()
        .flat_map(|key| strings(ownership.and_then(|value| value.get(key))))
        .collect::<BTreeSet<_>>();
    for lane in ownership
        .and_then(|value| value.get("lanes"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        owned.extend(strings(lane.get("owned_paths")));
    }
    // A key names an owned path literally or as its file name with `.` as `_`
    // (`OPERATIONS_md`); only a single match counts, so collisions are refused.
    let owned_match = |key: &str| {
        let mut matches = owned.iter().filter(|path| {
            *path == key
                || path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.contains('.') && name.replace('.', "_") == key)
        });
        match (matches.next(), matches.next()) {
            (Some(path), None) => Some(path.clone()),
            _ => None,
        }
    };
    documented.extend(
        plan.get("documentation")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(key, _)| owned_match(key)),
    );
    let protected = ["protected", "protected_paths_and_state"]
        .into_iter()
        .flat_map(|key| strings(ownership.and_then(|value| value.get(key))))
        .collect::<BTreeSet<_>>();
    documented
        .into_iter()
        .filter(|path| !protected.contains(path))
        .collect()
}

/// Everything that must be settled before an attempt's pinned guidance may
/// change: no writer doing work (an implementer is idle only at a recorded
/// `idle_candidate` boundary or after it exits), no session mid-transition,
/// keyboard control, pending permission, control or proposal, switch, rework,
/// review, capture, frozen candidate, check, uncertain guidance, recovery,
/// restart hold or uncertain claim. `?1` is the attempt.
const GUIDANCE_REAUTHORIZATION_FENCES: &[(&str, &str)] = &[
    ("an implementer is still working",
     "SELECT EXISTS(SELECT 1 FROM role_generations g WHERE g.attempt_id=?1 AND g.role='implementer'
        AND (g.status IN ('launch_reserved','stopping')
          OR (g.status='running' AND NOT EXISTS(SELECT 1 FROM sessions s
            WHERE s.id=(SELECT latest.id FROM sessions latest WHERE latest.role_generation_id=g.id
                        ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)
              AND s.status='running' AND s.readiness_state='idle_candidate'))))"),
    ("an agent session is starting, stopping or needs recovery",
     "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
        WHERE g.attempt_id=?1
          AND s.status IN ('launch_reserved','interrupt_requested','recovery_required'))"),
    ("someone has keyboard control of an agent",
     "SELECT EXISTS(SELECT 1 FROM input_leases lease JOIN sessions s ON s.id=lease.session_id
        JOIN role_generations g ON g.id=s.role_generation_id
        WHERE g.attempt_id=?1 AND lease.revoked_at IS NULL
          AND julianday(lease.expires_at)>julianday('now'))"),
    ("a permission request is pending",
     "SELECT EXISTS(SELECT 1 FROM permission_requests WHERE attempt_id=?1
        AND consumed_at IS NULL AND delivery_state NOT IN ('expired','not_delivered')
            AND NOT EXISTS(SELECT 1 FROM permission_native_resolutions native
              WHERE native.permission_request_id=permission_requests.id))"),
    ("a control or proposed transition is pending",
     "SELECT EXISTS(SELECT 1 FROM controls WHERE attempt_id=?1
        AND state NOT IN ('finished','cancelled','superseded','rejected','failed','abandoned'))"),
    ("an agent switch is pending",
     "SELECT EXISTS(SELECT 1 FROM switch_intents WHERE attempt_id=?1
        AND state NOT IN ('dispatched','completed','cancelled','rejected','superseded'))"),
    ("a rework is in progress",
     "SELECT EXISTS(SELECT 1 FROM rework_intents WHERE new_attempt_id=?1
        AND state NOT IN ('completed','cancelled'))"),
    ("a review is in flight",
     "SELECT EXISTS(SELECT 1 FROM review_requests WHERE attempt_id=?1
        AND delivery_state IN ('reserved','launching','delivered','ambiguous'))"),
    ("a capture is in progress",
     "SELECT EXISTS(SELECT 1 FROM freeze_intents WHERE attempt_id=?1
        AND state IN ('reserved','capturing','recovery_required'))"),
    ("the candidate is already frozen",
     "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND candidate_hash IS NOT NULL)"),
    ("a check is running",
     "SELECT EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1
        AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))"),
    ("guidance delivery is unconfirmed",
     "SELECT EXISTS(SELECT 1 FROM guidance_messages WHERE attempt_id=?1
        AND state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown'))"),
    ("a recovery is open",
     "SELECT EXISTS(SELECT 1 FROM recovery_records WHERE attempt_id=?1
        AND state='attention_required')"),
    ("a restart hold is open",
     "SELECT EXISTS(SELECT 1 FROM restart_candidates WHERE attempt_id=?1
        AND state NOT IN ('resumed','released_fresh_dispatch','cancelled'))"),
    ("the attempt's repository claim is uncertain",
     "SELECT EXISTS(SELECT 1 FROM claims WHERE attempt_id=?1 AND state='unknown')"),
];

/// Supersedes the attempt's proposed transitions that can never be applied
/// again. A proposal is bound to the task version, source phase, plan,
/// candidate and effective manager generation current when it was made, and
/// the task version only grows, so a proposal that differs in any of them is
/// permanently refused by `ApplyTransition`. A proposal that still matches all
/// of them stays pending and keeps blocking. Payloads are kept; each retirement
/// is audited with the proposal's binding, the current one and what differs.
pub(crate) fn retire_unmatchable_transition_proposals(
    connection: &Connection,
    attempt_id: &str,
    retired_for: &str,
    now: &str,
) -> Result<usize> {
    let (task_version, phase, plan_hash, candidate_hash): (
        i64,
        String,
        Option<String>,
        Option<String>,
    ) = connection.query_row(
        "SELECT t.version,a.phase,a.plan_hash,a.candidate_hash
         FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
        params![attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let effective_managers = connection
        .prepare(
            "SELECT DISTINCT rg.id FROM role_generations rg
             JOIN role_settings rs ON rs.effective_generation_id=rg.id AND rs.role='manager'
             WHERE rg.attempt_id=?1 AND rg.role='manager' ORDER BY rg.id",
        )?
        .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let proposals = connection
        .prepare(
            "SELECT id,expected_version,role_generation_id,payload_json FROM controls
             WHERE attempt_id=?1 AND kind='transition_proposal' AND state='proposed'
             ORDER BY created_at,rowid",
        )?
        .query_map(params![attempt_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let current = serde_json::json!({
        "task_version":task_version,"source_phase":phase,"plan_hash":plan_hash,
        "candidate_hash":candidate_hash,"effective_manager_generation_ids":effective_managers,
    });
    let mut retired = 0;
    for (id, expected_version, generation, payload_json) in proposals {
        // A payload that cannot be read can never be applied either.
        let payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap_or_default();
        let text = |field: &str| payload.get(field).and_then(serde_json::Value::as_str);
        let mut differs = Vec::new();
        if expected_version != Some(task_version)
            || payload
                .get("expected_task_version")
                .and_then(serde_json::Value::as_i64)
                != Some(task_version)
        {
            differs.push("task_version");
        }
        if text("source_phase") != Some(phase.as_str()) {
            differs.push("source_phase");
        }
        if text("plan_hash") != plan_hash.as_deref() {
            differs.push("plan_hash");
        }
        if text("candidate_hash") != candidate_hash.as_deref() {
            differs.push("candidate_hash");
        }
        if generation.as_deref().is_none_or(|generation| {
            text("role_generation_id") != Some(generation)
                || !effective_managers
                    .iter()
                    .any(|manager| manager == generation)
        }) {
            differs.push("manager_generation");
        }
        if differs.is_empty() {
            continue;
        }
        if connection.execute(
            "UPDATE controls SET state='superseded',updated_at=?1 WHERE id=?2 AND state='proposed'",
            params![now, id],
        )? != 1
        {
            bail!("a proposed transition changed while it was being retired")
        }
        let detail = serde_json::json!({
            "attempt_id":attempt_id,"reason":"binding_no_longer_current","retired_for":retired_for,
            "mismatched":differs,"current":current,
            "proposal":{"expected_version":expected_version,"role_generation_id":generation,
                        "payload_json":payload_json},
        });
        connection.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','control.transition_proposal.retired','control',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                id,
                detail.to_string(),
                now
            ],
        )?;
        retired += 1;
    }
    Ok(retired)
}

/// Applies your explicit approval that listed guidance files in the active
/// attempt's ready workspace may keep their new content. Guidance files are
/// pinned when the workspace is prepared, so an approved documentation change
/// made during the attempt otherwise leaves the attempt permanently not ready.
///
/// Nothing is inferred: the task version, the attempt as the task's current
/// unfinished attempt, its approved and implementation-authorized plan hash,
/// the active project configuration revision (which every task profile of the
/// attempt must use), and the hash of the exact policy being amended must all
/// match, and the attempt must be at a settled boundary (see
/// `GUIDANCE_REAUTHORIZATION_FENCES`). Each file must be a configured guidance
/// path the approved plan both owns and documents, pinned at
/// `previous_sha256`, and a contained regular file whose recomputed hash is
/// `sha256`. Workflow, skill, manifest and configuration files can never be
/// changed this way, and every other pinned file, including any other changed
/// guidance not listed, must still match. Returns the audit detail.
pub(crate) fn reauthorize_attempt_guidance(
    connection: &Connection,
    task_id: &str,
    attempt_id: &str,
    expected_version: i64,
    plan_hash: &str,
    config_revision_id: &str,
    policy_hash: &str,
    files: &[crate::domain::GuidanceReauthorization],
    now: &str,
) -> Result<serde_json::Value> {
    if files.is_empty() || files.len() > 32 {
        bail!("guidance reauthorization needs between 1 and 32 files")
    }
    let current: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN attempts a ON a.task_id=t.id
           WHERE t.id=?1 AND a.id=?2 AND t.version=?3 AND t.archived_at IS NULL
             AND t.lifecycle IN ('in_progress','validation')
             AND a.status IN ('running','needs_input','held')
             AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id
               ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1))",
        params![task_id, attempt_id, expected_version],
        |row| row.get(0),
    )?;
    if !current {
        bail!("task version is stale, or the attempt is not the task's current unfinished attempt")
    }
    // Runs in the caller's transaction, so any later refusal restores them.
    retire_unmatchable_transition_proposals(
        connection,
        attempt_id,
        "guidance_reauthorization",
        now,
    )?;
    for (reason, sql) in GUIDANCE_REAUTHORIZATION_FENCES {
        let blocked: bool = connection.query_row(sql, params![attempt_id], |row| row.get(0))?;
        if blocked {
            bail!("guidance cannot be reauthorized while {reason}")
        }
    }
    let plan_json: String = connection
        .query_row(
            "SELECT p.plan_json FROM attempts a JOIN trip_structured_plans p ON p.id=a.structured_plan_id
             WHERE a.id=?1 AND a.plan_hash=?2 AND p.plan_hash=?2
               AND a.plan_approved_at IS NOT NULL AND p.approved_at IS NOT NULL
               AND p.implementation_authorized_at IS NOT NULL",
            params![attempt_id, plan_hash],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("the attempt's approved plan does not match the given plan hash"))?;
    let (guidance_json, profiles_match): (String, bool) = connection
        .query_row(
            "SELECT json_extract(r.config_json,'$.guidance'),
                    NOT EXISTS(SELECT 1 FROM trip_attempt_profiles ap
                      WHERE ap.attempt_id=a.id AND ap.project_config_revision_id!=r.id)
             FROM attempts a JOIN tasks t ON t.id=a.task_id
             JOIN trip_project_state s ON s.project_id=t.project_id
             JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
             WHERE a.id=?1 AND r.id=?2",
            params![attempt_id, config_revision_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("the given configuration revision is not the project's active one")
        })?;
    if !profiles_match {
        bail!("the attempt's task profiles use a different configuration revision")
    }
    let guidance: BTreeSet<String> = serde_json::from_str(&guidance_json)?;
    let (root, policy_json): (String, String) = connection
        .query_row(
            "SELECT path,policy_json FROM workspaces WHERE attempt_id=?1 AND state='ready'",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("the attempt has no ready workspace"))?;
    if sha256(policy_json.as_bytes()) != policy_hash {
        bail!("the workspace policy changed since it was reviewed for this approval")
    }
    let mut policy = validate_materialized_policy(&policy_json)?;
    if policy.get("kind").and_then(serde_json::Value::as_str) != Some("activated_project") {
        bail!("only an activated-project workspace policy carries approved guidance")
    }
    let root = PathBuf::from(root).canonicalize()?;
    let named = approved_documentation_paths(&serde_json::from_str(&plan_json)?);
    let mut seen = BTreeSet::new();
    let mut changes = Vec::new();
    for file in files {
        validate_relative(&file.path)?;
        if !seen.insert(file.path.as_str()) {
            bail!("{} is listed more than once", file.path)
        }
        if !valid_sha256(&file.previous_sha256) || !valid_sha256(&file.sha256) {
            bail!("{} needs valid previous and new SHA-256 hashes", file.path)
        }
        if file.previous_sha256 == file.sha256 {
            bail!("{} is unchanged", file.path)
        }
        // Decided by the host alone, before configuration or plan are read.
        if is_protected_workflow_artifact(&file.path) {
            bail!(
                "{} is a protected workflow artifact and can never be reauthorized as guidance",
                file.path
            )
        }
        if !guidance.contains(&file.path) {
            bail!("{} is not a configured guidance file", file.path)
        }
        if !named.contains(&file.path) {
            bail!(
                "the approved plan does not explicitly own {} as documentation",
                file.path
            )
        }
        let pinned = policy
            .pointer("/files")
            .and_then(|files| files.get(&file.path))
            .and_then(serde_json::Value::as_str);
        if pinned != Some(file.previous_sha256.as_str()) {
            bail!("{} is not pinned at the given previous hash", file.path)
        }
        let path = root.join(&file.path);
        ensure_no_symlink_ancestry(&root, &path)?;
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
            bail!("{} is not a regular file in the workspace", file.path)
        }
        if hash_file(&path)?.as_deref() != Some(file.sha256.as_str()) {
            bail!(
                "{} in the workspace does not have the approved new content",
                file.path
            )
        }
        changes.push(serde_json::json!({
            "path":file.path,"previous_sha256":file.previous_sha256,"sha256":file.sha256,
        }));
    }
    let pinned = policy
        .get_mut("files")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| anyhow!("ready workspace policy lacks its file map"))?;
    for file in files {
        pinned.insert(
            file.path.clone(),
            serde_json::Value::String(file.sha256.clone()),
        );
    }
    let record = serde_json::json!({
        "task_version":expected_version,"plan_hash":plan_hash,
        "config_revision_id":config_revision_id,"previous_policy_hash":policy_hash,
        "files":changes,"approved_at":now,
    });
    let history = policy
        .as_object_mut()
        .ok_or_else(|| anyhow!("workspace policy must be an object"))?
        .entry("guidance_reauthorizations")
        .or_insert_with(|| serde_json::json!([]));
    history
        .as_array_mut()
        .ok_or_else(|| anyhow!("workspace guidance reauthorization history is malformed"))?
        .push(record.clone());
    let updated = policy.to_string();
    validate_materialized_policy(&updated)?;
    // Every other pinned file must still match; only the listed files move.
    verified_policy_paths(&root, &policy)?;
    let changed = connection.execute(
        "UPDATE workspaces SET policy_json=?1,updated_at=?2
         WHERE attempt_id=?3 AND state='ready' AND policy_json=?4",
        params![updated, now, attempt_id, policy_json],
    )?;
    if changed != 1 {
        bail!("the workspace policy changed while applying the approval")
    }
    let mut detail = record;
    detail["attempt_id"] = serde_json::json!(attempt_id);
    detail["policy_hash"] = serde_json::json!(sha256(updated.as_bytes()));
    connection.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','attempt.guidance.reauthorized','attempt',?3,?4,?5)",
        params![uuid::Uuid::new_v4().to_string(),uuid::Uuid::new_v4().to_string(),attempt_id,detail.to_string(),now],
    )?;
    Ok(detail)
}

pub(crate) fn validate_materialized_policy(policy_json: &str) -> Result<serde_json::Value> {
    validate_policy_identity(policy_json, WORKFLOW_ID, &source_hash(), &overlay_hash())
}

fn validate_policy_identity(
    policy_json: &str,
    workflow_id: &str,
    upstream_source_hash: &str,
    expected_overlay_hash: &str,
) -> Result<serde_json::Value> {
    let policy: serde_json::Value = serde_json::from_str(policy_json)?;
    if policy
        .get("workflow_id")
        .and_then(serde_json::Value::as_str)
        != Some(workflow_id)
        || policy
            .get("upstream_source_hash")
            .and_then(serde_json::Value::as_str)
            != Some(upstream_source_hash)
        || policy
            .get("overlay_hash")
            .and_then(serde_json::Value::as_str)
            != Some(expected_overlay_hash)
    {
        bail!("materialized worktree policy identity does not match the pinned TRIP workflow")
    }
    let files = policy
        .get("files")
        .and_then(serde_json::Value::as_object)
        .filter(|files| !files.is_empty())
        .ok_or_else(|| anyhow!("materialized worktree policy requires a nonempty file map"))?;
    if files
        .values()
        .any(|hash| hash.as_str().is_none_or(|hash| !valid_sha256(hash)))
    {
        bail!("materialized worktree policy contains an invalid file identity")
    }
    match policy.get("kind").and_then(serde_json::Value::as_str) {
        Some("activated_project") => {
            let task_profiles = policy
                .get("task_profiles")
                .and_then(serde_json::Value::as_array)
                .filter(|profiles| profiles.len() == APP_ROLES.len())
                .ok_or_else(|| {
                    anyhow!("activated-project worktree policy requires six task profiles")
                })?;
            let mut roles = BTreeSet::new();
            for profile in task_profiles {
                let profile = profile
                    .as_object()
                    .ok_or_else(|| anyhow!("activated-project task profiles must be objects"))?;
                let role = profile
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .filter(|role| APP_ROLES.contains(role))
                    .ok_or_else(|| anyhow!("activated-project task profile role is invalid"))?;
                if !roles.insert(role) {
                    bail!("activated-project task profile roles must be unique")
                }
                if profile
                    .get("settings_revision")
                    .and_then(serde_json::Value::as_i64)
                    .is_none_or(|revision| revision < 1)
                    || !profile
                        .get("profile")
                        .is_some_and(serde_json::Value::is_object)
                    || [
                        "source",
                        "profile_hash",
                        "project_config_revision_id",
                        "project_configuration_hash",
                        "adapter",
                        "adapter_hash",
                        "capability_id",
                        "capability_key",
                        "capability_proof_hash",
                    ]
                    .iter()
                    .any(|field| {
                        profile
                            .get(*field)
                            .and_then(serde_json::Value::as_str)
                            .is_none_or(str::is_empty)
                    })
                {
                    bail!("activated-project task profile binding is incomplete")
                }
            }
            if roles.len() != APP_ROLES.len()
                || policy
                    .get("manifest_hash")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|hash| !valid_sha256(hash))
                || policy.get("base_project_configuration_distinct")
                    != Some(&serde_json::Value::Bool(true))
            {
                bail!("activated-project worktree policy is incomplete")
            }
        }
        Some("setup_fixture") => {
            if policy.get("target_files_copied") != Some(&serde_json::Value::Bool(false)) {
                bail!("setup-fixture worktree policy is incomplete")
            }
        }
        _ => bail!("materialized worktree policy kind is unsupported"),
    }
    Ok(policy)
}

pub fn inspect_registered_project(store: &Store, project_id: &str) -> Result<serde_json::Value> {
    let (path, expected_identity): (String, String) = {
        let connection = store.lock()?;
        connection
            .query_row(
                "SELECT repository_path,repository_identity FROM projects WHERE id=?1 AND internal_purpose IS NULL",
                params![project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("unknown user project"))?
    };
    let repository = crate::workspace::inspect(Path::new(&path))?;
    if repository.identity != expected_identity {
        bail!("registered repository identity changed")
    }
    let observation = detect_installation(&repository.root)?;
    let mut detected = serde_json::to_value(&observation)?;
    if let Some(object) = detected.as_object_mut() {
        object.insert(
            "observation_hash".into(),
            serde_json::Value::String(json_hash(&observation)?),
        );
        match detected_installation_configuration(&repository.root) {
            Ok(Some((configuration, adapters))) => {
                object.insert("configuration".into(), configuration);
                object.insert("adapters".into(), adapters);
            }
            Ok(None) => {}
            Err(error) => {
                object.insert(
                    "configuration_error".into(),
                    serde_json::Value::String(format!(
                        "Existing configuration cannot be exposed safely: {error:#}"
                    )),
                );
            }
        }
    }
    let observed_manifest =
        hash_file(&repository.root.join(".agents/trip-explorer/manifest.json"))?;
    let now = Utc::now().to_rfc3339();
    let connection = store.lock()?;
    let (current, current_reason, activated_manifest, current_pins): (String, String, Option<String>, bool) =
        connection.query_row(
            "SELECT s.readiness,s.reason,s.manifest_hash,
                s.workflow_id=?2 AND s.package_version=?3 AND s.upstream_source_hash=?4 AND s.overlay_hash=?5
                AND EXISTS(SELECT 1 FROM trip_config_revisions r WHERE r.id=s.active_config_revision_id
                  AND r.project_id=s.project_id AND r.state='activated' AND r.source_hash=?4 AND r.overlay_hash=?5)
             FROM trip_project_state s WHERE s.project_id=?1",
            params![project_id, WORKFLOW_ID, PACKAGE_VERSION, source_hash(), overlay_hash()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, Option<bool>>(3)?.unwrap_or(false))),
        )?;
    let (readiness, reason) =
        if matches!(current.as_str(), "setup_in_progress" | "recovery_required") {
            (current, current_reason)
        } else if current == "ready"
            && observation.kind == "compatible"
            && current_pins
            && activated_manifest == observed_manifest
        {
            (
                "ready".into(),
                "Pinned TRIP Explorer installation is activated and unchanged".into(),
            )
        } else {
            let (readiness, reason) = readiness_from_observation(&observation);
            let reason = if current == "ready" && observation.kind == "compatible" {
                "Activated TRIP Explorer manifest changed and requires explicit validation".into()
            } else {
                reason
            };
            (readiness.as_str().into(), reason)
        };
    connection.execute(
        "UPDATE trip_project_state SET readiness=?1,reason=?2,detected_installation=?3,
         detected_json=?4,updated_at=?5 WHERE project_id=?6",
        params![
            readiness,
            reason,
            observation.kind,
            detected.to_string(),
            now,
            project_id
        ],
    )?;
    Ok(
        serde_json::json!({"project_id":project_id,"readiness":readiness,"reason":reason,"observation":detected}),
    )
}

fn detected_installation_configuration(
    root: &Path,
) -> Result<Option<(serde_json::Value, serde_json::Value)>> {
    let state = root.join(".agents/trip-explorer");
    let config_path = state.join("config.json");
    let adapters_path = state.join("adapters.json");
    if !config_path.is_file() || !adapters_path.is_file() {
        return Ok(None);
    }
    ensure_no_symlink_ancestry(root, &config_path)?;
    ensure_no_symlink_ancestry(root, &adapters_path)?;
    let config_bytes = fs::read(&config_path)?;
    let adapters_bytes = fs::read(&adapters_path)?;
    if config_bytes.len() > 1024 * 1024 || adapters_bytes.len() > 1024 * 1024 {
        bail!("configuration preview exceeds the 1 MiB per-file limit")
    }
    let configuration: serde_json::Value = serde_json::from_slice(&config_bytes)?;
    let adapters: serde_json::Value = serde_json::from_slice(&adapters_bytes)?;
    if contains_prohibited_key(&configuration) || contains_prohibited_key(&adapters) {
        bail!(
            "configuration contains prohibited credential, secret, token, or hidden-reasoning keys"
        )
    }
    Ok(Some((configuration, adapters)))
}

pub fn execute_human(
    store: &Store,
    paths: &InstancePaths,
    operation_id: &str,
    action: &TripHumanAction,
) -> Result<OperationResult> {
    paths.create()?;
    let executable = std::env::current_exe()?;
    let runtime = CapabilityRuntime::from_store(
        store,
        crate::providers::install_hook_assets(paths, &executable)?,
        paths.role_socket.clone(),
        executable,
    );
    execute_human_with_runtime(store, paths, &runtime, operation_id, action)
}

pub fn execute_human_with_runtime(
    store: &Store,
    paths: &InstancePaths,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    action: &TripHumanAction,
) -> Result<OperationResult> {
    if operation_id.trim().is_empty() {
        bail!("operation_id is required")
    }
    let request_hash = json_hash(action)?;
    if let Some(result) =
        store.operation_receipt(operation_id, "human_control", "trip_command", &request_hash)?
    {
        return Ok(serde_json::from_value(result)?);
    }
    let result = match action {
        TripHumanAction::InspectProject { project_id } => {
            let detail = inspect_registered_project(store, project_id)?;
            operation_result(
                operation_id,
                "project",
                project_id,
                None,
                "trip_inspected",
                detail,
            )
        }
        TripHumanAction::BeginSetup {
            project_id,
            expected_project_version,
            host_manager,
        } => begin_setup(
            store,
            paths,
            operation_id,
            project_id,
            *expected_project_version,
            host_manager,
        )?,
        TripHumanAction::SaveSetupDraft {
            setup_operation_id,
            expected_project_version,
            proposal,
        } => save_setup_draft(
            store,
            operation_id,
            setup_operation_id,
            *expected_project_version,
            proposal,
        )?,
        TripHumanAction::ReviseSetupDraft {
            setup_operation_id,
            expected_project_version,
            proposal,
        } => revise_setup_draft(
            store,
            runtime,
            operation_id,
            setup_operation_id,
            *expected_project_version,
            proposal,
        )?,
        TripHumanAction::StopSetupManager { .. } => {
            bail!("setup manager stop requires the application signal boundary")
        }
        TripHumanAction::ChangeSetupManager {
            setup_operation_id,
            expected_project_version,
            host_manager,
        } => change_setup_manager(
            store,
            paths,
            operation_id,
            setup_operation_id,
            *expected_project_version,
            host_manager,
        )?,
        TripHumanAction::AuthorizeSetupProbes {
            setup_operation_id,
            proposal_hash,
        } => authorize_probes(
            store,
            paths,
            runtime,
            operation_id,
            setup_operation_id,
            proposal_hash,
        )?,
        TripHumanAction::PrepareRuntimeAdmission {
            project_id,
            task_id,
            role,
            settings_revision,
            cmux_socket_path,
            expected_version,
        } => prepare_runtime_admission(
            store,
            runtime,
            operation_id,
            project_id,
            task_id.as_deref(),
            *role,
            *settings_revision,
            cmux_socket_path.as_deref(),
            *expected_version,
        )?,
        TripHumanAction::AuthorizeRuntimeAdmission {
            admission_id,
            scope_hash,
        } => authorize_runtime_admission(
            store,
            paths,
            runtime,
            operation_id,
            admission_id,
            scope_hash,
        )?,
        TripHumanAction::PublishRuntimeProof { admission_id, role } => {
            publish_runtime_proof(store, runtime, operation_id, admission_id, *role)?
        }
        TripHumanAction::FinalizeInstallation {
            setup_operation_id,
            proposal_hash,
        } => finalize_installation(
            store,
            runtime,
            operation_id,
            setup_operation_id,
            proposal_hash,
        )?,
        TripHumanAction::AuthorizeInstallation {
            setup_operation_id,
            proposal_hash,
            approved_preimages_hash,
            final_source_set_hash,
        } => authorize_installation(
            store,
            runtime,
            operation_id,
            setup_operation_id,
            proposal_hash,
            approved_preimages_hash,
            final_source_set_hash,
        )?,
        TripHumanAction::ApplyInstallation {
            setup_operation_id,
            proposal_hash,
        } => apply_installation(
            store,
            paths,
            runtime,
            operation_id,
            setup_operation_id,
            proposal_hash,
        )?,
        TripHumanAction::RecoverInstallation { setup_operation_id } => {
            recover_installation(store, paths, runtime, operation_id, setup_operation_id)?
        }
        TripHumanAction::AdoptInstallation {
            project_id,
            expected_project_version,
            configuration,
        } => adopt_installation(
            store,
            runtime,
            operation_id,
            project_id,
            *expected_project_version,
            configuration,
        )?,
        TripHumanAction::MigrateAttempt {
            task_id,
            attempt_id,
            expected_task_version,
            reviewed_plan_hash,
            config_revision_id,
        } => migrate_attempt(
            store,
            runtime,
            operation_id,
            task_id,
            attempt_id,
            *expected_task_version,
            reviewed_plan_hash,
            config_revision_id,
        )?,
        TripHumanAction::AuthorizeCheck {
            attempt_id,
            check_id,
            selected_revision,
            exact_command_hash,
            scope_hash,
            decision,
            lifetime,
        } => authorize_check(
            store,
            operation_id,
            attempt_id,
            check_id,
            *selected_revision,
            exact_command_hash,
            scope_hash,
            decision,
            lifetime,
        )?,
        TripHumanAction::RevokeCheckPermissionRule {
            rule_id,
            expected_revision,
        } => revoke_check_permission_rule(store, operation_id, rule_id, *expected_revision)?,
        TripHumanAction::ExtendReviewBudget {
            task_id,
            attempt_id,
            expected_task_version,
            review_kind,
            additional,
        } => extend_review_budget(
            store,
            operation_id,
            task_id,
            attempt_id,
            *expected_task_version,
            review_kind,
            *additional,
        )?,
        TripHumanAction::AuthorizeImplementation {
            task_id,
            attempt_id,
            expected_task_version,
            plan_hash,
        } => authorize_implementation(
            store,
            operation_id,
            task_id,
            attempt_id,
            *expected_task_version,
            plan_hash,
        )?,
        TripHumanAction::AuthorizeAdditionalExplorer {
            task_id,
            attempt_id,
            expected_task_version,
            stage,
            justification,
        } => authorize_additional_explorer(
            store,
            operation_id,
            task_id,
            attempt_id,
            *expected_task_version,
            stage,
            justification,
        )?,
        TripHumanAction::AuthorizeFinalRepairRecheck {
            task_id,
            attempt_id,
            expected_task_version,
            approved_code_request_id,
            prior_candidate_hash,
            final_request_id,
            rejected_code_request_id,
            reviewer_generation_id,
        } => authorize_final_repair_recheck(
            store,
            operation_id,
            &request_hash,
            task_id,
            attempt_id,
            *expected_task_version,
            &FinalRepairLedger {
                approved_code_request_id,
                prior_candidate_hash,
                final_request_id,
                rejected_code_request_id,
                reviewer_generation_id,
            },
        )?,
    };
    persist_trip_receipt(store, operation_id, &request_hash, &result)?;
    Ok(result)
}

fn begin_setup(
    store: &Store,
    paths: &InstancePaths,
    operation_id: &str,
    project_id: &str,
    expected_version: i64,
    host_manager: &RoleOverride,
) -> Result<OperationResult> {
    validate_role_override(host_manager)?;
    if let Some(recovered) = retry_discovery_workspace(
        store,
        operation_id,
        project_id,
        expected_version,
        host_manager,
    )? {
        return Ok(recovered);
    }
    {
        let connection = store.lock()?;
        let project_current: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND version=?2 AND internal_purpose IS NULL)",
            params![project_id, expected_version],
            |row| row.get(0),
        )?;
        if !project_current {
            bail!("project version is stale")
        }
        let active: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM trip_setup_operations
             WHERE project_id=?1 AND state NOT IN ('activated','aborted','superseded'))",
            params![project_id],
            |row| row.get(0),
        )?;
        if active {
            bail!("project already has an active setup operation")
        }
    }
    let setup_id = uuid::Uuid::new_v4().to_string();
    let target_inventory = bounded_inventory(store, project_id)?;
    let target_root = target_inventory
        .get("repository_root")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("target repository root is missing from setup inventory"))?;
    let fixture_root = paths.artifacts.join("trip-setup-fixtures").join(&setup_id);
    create_empty_fixture(&fixture_root, Path::new(target_root))?;
    let fixture = crate::workspace::inspect(&fixture_root)?;
    let fixture_project_id = uuid::Uuid::new_v4().to_string();
    let task_id = format!("TRIP-SETUP-{}", &setup_id[..8].to_ascii_uppercase());
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let workspace_id = uuid::Uuid::new_v4().to_string();
    let workspace_path = paths.artifacts.join("worktrees").join(&attempt_id);
    let now = Utc::now().to_rfc3339();
    {
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND version=?2 AND internal_purpose IS NULL)",
            params![project_id, expected_version],
            |row| row.get(0),
        )?;
        if !exists {
            bail!("project version is stale")
        }
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM trip_setup_operations WHERE project_id=?1 AND state NOT IN ('activated','aborted','superseded'))",
            params![project_id], |row| row.get(0)
        )?;
        if active {
            bail!("project already has an active setup operation")
        }
        tx.execute(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,queue_paused,created_at,updated_at,version,settings_json,internal_purpose)
             VALUES(?1,?2,?3,?4,?5,1,?6,?6,1,'{}','trip_setup_fixture')",
            params![fixture_project_id, format!("TRIP setup {setup_id}"), fixture.root.to_string_lossy(), fixture.identity, fixture.head, now],
        )?;
        tx.execute(
            "INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,priority,manual_order,lifecycle,attention,version,created_at,updated_at,role_overrides_json)
             VALUES(?1,?2,'TRIP Explorer project setup','Internal validation lifecycle','[]',0,0,'validation','paused',1,?3,?3,'{}')",
            params![task_id, fixture_project_id, now],
        )?;
        tx.execute(
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at,scope_hash,configuration_hash,workflow_version,workflow_hash,setup_operation_id,upstream_source_hash,overlay_hash,legacy_migration_required)
             VALUES(?1,?2,?3,'planning',?4,1,'held',?5,?5,?6,?7,?8,?9,?10,?11,?12,0)",
            params![attempt_id, task_id, uuid::Uuid::new_v4().to_string(), fixture.head, now,
                json_hash(&target_inventory)?, json_hash(host_manager)?, WORKFLOW_ID,
                crate::workflow_resources::workflow_hash(), setup_id, source_hash(), overlay_hash()],
        )?;
        tx.execute(
            "INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at)
             VALUES(?1,?2,'manager',1,?3,?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                task_id,
                serde_json::to_string(host_manager)?,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?5,'{}','reserved',?6,?6)",
            params![workspace_id, attempt_id, fixture.identity, workspace_path.to_string_lossy(), fixture.head, now],
        )?;
        tx.execute(
            "INSERT INTO trip_setup_operations(id,project_id,fixture_project_id,validation_task_id,discovery_attempt_id,state,target_inventory_json,proposal_json,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,'discovery',?6,'{}',?7,?7)",
            params![setup_id, project_id, fixture_project_id, task_id, attempt_id, serde_json::to_string(&target_inventory)?, now],
        )?;
        tx.execute(
            "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state,profile_json,profile_hash,selected_at)
             VALUES(?1,'manager','selected',?2,?3,?4)",
            params![setup_id, serde_json::to_string(host_manager)?, json_hash(host_manager)?, now],
        )?;
        for role in DELEGATED_ROLES {
            tx.execute(
                "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state)
                 VALUES(?1,?2,'unselected')",
                params![setup_id, role.to_string()],
            )?;
        }
        tx.execute(
            "INSERT INTO trip_setup_permits(id,setup_operation_id,attempt_id,fixture_project_id,fixture_repository_identity,role,profile_hash,settings_revision,purpose,approved_action,state,created_at)
             VALUES(?1,?2,?3,?4,?5,'manager',?6,1,'setup_discovery','bounded_inventory_and_contained_read','issued',?7)",
            params![uuid::Uuid::new_v4().to_string(), setup_id, attempt_id, fixture_project_id, fixture.identity, json_hash(host_manager)?, now],
        )?;
        tx.execute(
            "UPDATE trip_project_state SET readiness='setup_in_progress',reason='Setup discovery is awaiting the selected manager',setup_operation_id=?1,detected_installation='unknown',updated_at=?2 WHERE project_id=?3",
            params![setup_id, now, project_id],
        )?;
        tx.commit()?;
    }
    if let Err(error) = prepare_setup_workspace(
        store,
        &attempt_id,
        &fixture.root,
        &fixture.identity,
        &fixture.head,
        &workspace_path,
    ) {
        let detail = format!("setup discovery workspace materialization failed: {error:#}");
        mark_setup_workspace_recovery(store, &setup_id, &attempt_id, &detail)?;
        bail!("{detail}; retry begin_setup with the same project version and host manager")
    }
    Ok(operation_result(
        operation_id,
        "trip_setup",
        &setup_id,
        Some(expected_version),
        "discovery_ready",
        serde_json::json!({
            "setup_operation_id":setup_id,"project_id":project_id,"fixture_project_id":fixture_project_id,
            "validation_task_id":task_id,"discovery_attempt_id":attempt_id,"target_inventory":target_inventory,
            "workspace_recovered":false
        }),
    ))
}

pub(crate) fn prepare_setup_manager_stop(
    store: &Store,
    operation_id: &str,
    setup_id: &str,
    expected_project_version: i64,
) -> Result<SetupManagerStopPreparation> {
    let action = TripHumanAction::StopSetupManager {
        setup_operation_id: setup_id.to_owned(),
        expected_project_version,
    };
    let request_hash = json_hash(&action)?;
    if let Some(result) =
        store.operation_receipt(operation_id, "human_control", "trip_command", &request_hash)?
    {
        return Ok(SetupManagerStopPreparation {
            completed: Some(serde_json::from_value(result)?),
            control_id: String::new(),
            session_id: None,
            retry_signal_delivery: false,
            operation_id: operation_id.to_owned(),
            request_hash,
            setup_id: setup_id.to_owned(),
            result_project_version: expected_project_version,
            session_process_identity: None,
        });
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (project_id, project_version, attempt_id, manager_hash): (String, i64, String, String) = tx
        .query_row(
            "SELECT so.project_id,p.version,so.discovery_attempt_id,selection.profile_hash
             FROM trip_setup_operations so
             JOIN projects p ON p.id=so.project_id
             JOIN trip_project_state state ON state.project_id=so.project_id
               AND state.setup_operation_id=so.id
             JOIN trip_setup_profile_selections selection
               ON selection.setup_operation_id=so.id AND selection.role='manager'
             WHERE so.id=?1 AND so.state='discovery'",
            params![setup_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    if project_version != expected_project_version {
        bail!("project version is stale; keep the entered manager profile and refresh setup state before stopping")
    }
    let existing: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT id,requested_operation_id,payload_json FROM controls
             WHERE attempt_id=?1 AND kind='setup_manager_change' AND state='held'
             ORDER BY created_at DESC LIMIT 1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (control_id, retry_signal_delivery, failed_delivery) = if let Some((
        id,
        requested_operation_id,
        payload,
    )) = existing
    {
        if requested_operation_id.as_deref() == Some(operation_id) {
            (id, false, None)
        } else {
            let payload: serde_json::Value = serde_json::from_str(&payload)
                .context("parse durable setup manager change hold")?;
            let delivery = payload.get("signal_delivery").cloned();
            if delivery
                .as_ref()
                .and_then(|value| value.get("state"))
                .and_then(serde_json::Value::as_str)
                != Some("failed")
            {
                bail!("setup manager replacement is already held; wait for verified quiescence or complete the existing change")
            }
            (id, true, delivery)
        }
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at,requested_operation_id)
             VALUES(?1,?2,'setup_manager_change','held',?3,?4,?5,?5,?6)",
            params![
                id,
                attempt_id,
                expected_project_version,
                serde_json::json!({
                    "setup_operation_id":setup_id,
                    "project_id":project_id,
                    "manager_profile_hash":manager_hash,
                    "reason":"manager replacement requested; discovery dispatch is held before any interrupt signal"
                })
                .to_string(),
                now,
                operation_id
            ],
        )?;
        retire_setup_manager_authority(&tx, setup_id, &attempt_id, &now)?;
        let version_changed = tx.execute(
            "UPDATE projects SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
            params![now, project_id, expected_project_version],
        )?;
        if version_changed != 1 {
            bail!("project version is stale; keep the entered manager profile and refresh setup state before stopping")
        }
        tx.execute(
            "UPDATE trip_project_state
             SET reason='Discovery manager replacement is held; old manager authority is revoked before signal and verified quiescence',updated_at=?1
             WHERE project_id=?2 AND setup_operation_id=?3",
            params![now, project_id, setup_id],
        )?;
        (id, false, None)
    };
    // A reserved session may already own a process group while launch handoff is
    // still completing, so it must be interrupted too. A hold never treats that
    // signal as exit evidence; reconciliation must prove the whole group exited.
    let session: Option<(String, String)> = tx
        .query_row(
            "SELECT s.id,s.process_identity_json FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id=?1 AND rg.role='manager'
               AND s.status IN ('launch_reserved','running','interrupt_requested','recovery_required')
             ORDER BY s.created_at DESC,s.rowid DESC LIMIT 1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (session_id, session_process_identity) = match session {
        Some((session_id, process_identity)) => (
            Some(session_id),
            Some(
                serde_json::from_str::<serde_json::Value>(&process_identity)
                    .context("parse setup manager process identity")?,
            ),
        ),
        None => (None, None),
    };
    if retry_signal_delivery {
        let delivery = failed_delivery.expect("failed delivery is required for an explicit retry");
        if delivery
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            != session_id.as_deref()
            || delivery.get("process_identity") != session_process_identity.as_ref()
        {
            bail!("the recorded failed manager signal no longer binds the current exact process; use quiescence reconciliation before changing the manager")
        }
        let changed = tx.execute(
            "UPDATE controls SET requested_operation_id=?1,expected_version=?2,
                    payload_json=json_set(payload_json,'$.next_action',?3),updated_at=?4
             WHERE id=?5 AND kind='setup_manager_change' AND state='held'",
            params![
                operation_id,
                expected_project_version,
                "Rechecking the exact recorded manager identity before one explicit signal-delivery retry.",
                now,
                control_id
            ],
        )?;
        if changed != 1 {
            bail!("setup manager hold changed before the explicit signal retry")
        }
    }
    tx.commit()?;
    Ok(SetupManagerStopPreparation {
        completed: None,
        control_id,
        session_id,
        retry_signal_delivery,
        operation_id: operation_id.to_owned(),
        request_hash,
        setup_id: setup_id.to_owned(),
        result_project_version: if retry_signal_delivery {
            expected_project_version
        } else {
            expected_project_version + 1
        },
        session_process_identity,
    })
}

pub(crate) fn finish_setup_manager_stop(
    store: &Store,
    preparation: &SetupManagerStopPreparation,
    signal_delivery: SetupManagerSignalDelivery,
) -> Result<OperationResult> {
    let (state, delivery_state, interrupt_requested, delivery_error, next_action) = match signal_delivery {
        SetupManagerSignalDelivery::Requested => (
            "manager_interrupt_requested",
            "requested",
            true,
            None,
            "Wait for a positively verified quiescent manager process before changing the exact manager profile.",
        ),
        SetupManagerSignalDelivery::AlreadyRequested => (
            "manager_interrupt_requested",
            "already_requested",
            true,
            None,
            "A prior successful manager interrupt is recorded; wait for positively verified quiescence before changing the exact manager profile.",
        ),
        SetupManagerSignalDelivery::AlreadyQuiescent => (
            "manager_quiescent",
            "already_quiescent",
            false,
            None,
            "Use the existing quiescence reconciliation before changing the exact manager profile.",
        ),
        SetupManagerSignalDelivery::NotRunning => (
            "manager_stop_held",
            "not_running",
            false,
            None,
            "No current manager process was eligible for a signal. Use the existing quiescence reconciliation before changing the exact manager profile.",
        ),
        SetupManagerSignalDelivery::Failed(error) => (
            "manager_signal_delivery_failed",
            "failed",
            false,
            Some(error),
            "Retry Stop manager with a fresh operation only while the exact recorded manager identity remains current; otherwise use quiescence reconciliation before changing the manager. No automatic signal retry or replacement is scheduled.",
        ),
    };
    let delivery = serde_json::json!({
        "state":delivery_state,
        "session_id":preparation.session_id,
        "process_identity":preparation.session_process_identity,
        "error":delivery_error,
        "retry_requires_exact_identity":delivery_state == "failed"
    });
    let result = operation_result(
        &preparation.operation_id,
        "trip_setup",
        &preparation.setup_id,
        Some(preparation.result_project_version),
        state,
        serde_json::json!({
            "control_id":preparation.control_id,
            "signal_outcome":delivery_state,
            "session_id":preparation.session_id,
            "interrupt_requested":interrupt_requested,
            "completion_inferred":false,
            "next_action":next_action
        }),
    );
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let control_changed = tx.execute(
        "UPDATE controls SET payload_json=json_set(payload_json,'$.signal_delivery',json(?1),'$.next_action',?2),updated_at=?3
         WHERE id=?4 AND kind='setup_manager_change' AND state='held' AND requested_operation_id=?5",
        params![
            serde_json::to_string(&delivery)?,
            next_action,
            now,
            preparation.control_id,
            preparation.operation_id
        ],
    )?;
    if control_changed != 1 {
        bail!("setup manager hold changed before its signal delivery receipt could be persisted")
    }
    if delivery_state == "failed" {
        // interrupt_once marks the session before a kernel signal so its
        // durable authority is revoked before delivery. A failed delivery is
        // not an interrupt receipt, however. Keep the session in a durable
        // recovery state so a later, explicitly authorized retry must bind
        // the same exact process identity rather than treating this as a
        // completed stop request.
        if let Some(session_id) = preparation.session_id.as_deref() {
            tx.execute(
                "UPDATE sessions SET status='recovery_required',updated_at=?1
                 WHERE id=?2 AND status IN ('launch_reserved','running','interrupt_requested')",
                params![now, session_id],
            )?;
        }
    }
    tx.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,new_version,detail_json,created_at) VALUES(?1,?2,'human','trip.command.applied',?3,?4,?5,?6,?7)",params![uuid::Uuid::new_v4().to_string(),preparation.operation_id,result.entity_kind,result.entity_id,result.version,serde_json::to_string(&result)?,now])?;
    tx.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','trip_command',?2,?3,?4)",params![preparation.operation_id,preparation.request_hash,serde_json::to_string(&result)?,now])?;
    tx.commit()?;
    Ok(result)
}

fn retire_setup_manager_authority(
    tx: &rusqlite::Transaction<'_>,
    setup_id: &str,
    attempt_id: &str,
    now: &str,
) -> Result<()> {
    let generations = {
        let mut statement = tx.prepare(
            "SELECT id FROM role_generations
             WHERE attempt_id=?1 AND role='manager' AND status NOT IN ('replaced','revoked')",
        )?;
        let generations = statement
            .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        generations
    };
    tx.execute(
        "UPDATE trip_setup_permits SET state='revoked',consumed_at=COALESCE(consumed_at,?1)
         WHERE setup_operation_id=?2 AND role='manager' AND purpose='setup_discovery'
           AND state IN ('issued','consumed')",
        params![now, setup_id],
    )?;
    tx.execute(
        "UPDATE launch_permits SET state='revoked',consumed_at=COALESCE(consumed_at,?1)
         WHERE attempt_id=?2 AND role='manager' AND state IN ('issued','consumed')",
        params![now, attempt_id],
    )?;
    for generation in &generations {
        crate::permissions::expire_generation(
            tx,
            generation,
            "setup manager replacement revoked old discovery permission delivery",
        )?;
    }
    tx.execute(
        "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)
         WHERE role_generation_id IN (
           SELECT id FROM role_generations WHERE attempt_id=?2 AND role='manager'
         )",
        params![now, attempt_id],
    )?;
    tx.execute(
        "UPDATE role_generations SET status='revoked',updated_at=?1
         WHERE attempt_id=?2 AND role='manager' AND status NOT IN ('replaced','revoked')",
        params![now, attempt_id],
    )?;
    Ok(())
}

fn change_setup_manager(
    store: &Store,
    paths: &InstancePaths,
    operation_id: &str,
    source_setup_id: &str,
    expected_project_version: i64,
    host_manager: &RoleOverride,
) -> Result<OperationResult> {
    validate_role_override(host_manager)?;
    let new_setup_id = uuid::Uuid::new_v4().to_string();
    let new_attempt_id = uuid::Uuid::new_v4().to_string();
    let workspace_id = uuid::Uuid::new_v4().to_string();
    let workspace_path = paths.artifacts.join("worktrees").join(&new_attempt_id);
    let now = Utc::now().to_rfc3339();
    let (
        project_id,
        fixture_project_id,
        validation_task_id,
        fixture_root,
        fixture_identity,
        fixture_head,
        inventory_json,
        source_manager_json,
        source_attempt_id,
        settings_revision,
    ) = {
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: (
            String,
            i64,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        ) = tx.query_row(
            "SELECT so.project_id,p.version,so.fixture_project_id,so.validation_task_id,
                    fixture.repository_path,fixture.repository_identity,fixture.base_revision,
                    so.target_inventory_json,selection.profile_json,so.discovery_attempt_id,
                    selection.profile_hash
             FROM trip_setup_operations so
             JOIN projects p ON p.id=so.project_id
             JOIN trip_project_state state ON state.project_id=so.project_id
               AND state.setup_operation_id=so.id
             JOIN projects fixture ON fixture.id=so.fixture_project_id
             JOIN trip_setup_profile_selections selection
               ON selection.setup_operation_id=so.id AND selection.role='manager'
             WHERE so.id=?1 AND so.state='discovery'",
            params![source_setup_id],
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
                ))
            },
        )?;
        let (
            project_id,
            project_version,
            fixture_project_id,
            validation_task_id,
            fixture_root,
            fixture_identity,
            fixture_head,
            inventory_json,
            source_manager_json,
            source_attempt_id,
            source_manager_hash,
        ) = row;
        if project_version != expected_project_version {
            bail!("project version is stale; preserve the edited manager profile and refresh before changing")
        }
        if !setup_attempt_quiescent(&tx, &source_attempt_id)? {
            bail!("setup manager change requires positively verified whole-process quiescence; stop the live manager and wait for its exit")
        }
        let held: Option<String> = tx
            .query_row(
                "SELECT id FROM controls WHERE attempt_id=?1 AND kind='setup_manager_change'
                 AND state='held' ORDER BY created_at DESC LIMIT 1",
                params![source_attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        if source_manager_hash == json_hash(host_manager)? && held.is_none() {
            bail!("Stop the discovery manager before restarting with the same agent settings")
        }
        if held.is_none() {
            let control_id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at,requested_operation_id)
                 VALUES(?1,?2,'setup_manager_change','held',?3,?4,?5,?5,?6)",
                params![
                    control_id,
                    source_attempt_id,
                    expected_project_version,
                    serde_json::json!({
                        "setup_operation_id":source_setup_id,
                        "reason":"manager changed after quiescent discovery; old authority retired before replacement"
                    })
                    .to_string(),
                    now,
                    operation_id
                ],
            )?;
        }
        retire_setup_manager_authority(&tx, source_setup_id, &source_attempt_id, &now)?;
        let version_changed = tx.execute(
            "UPDATE projects SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
            params![now, project_id, expected_project_version],
        )?;
        if version_changed != 1 {
            bail!("project version is stale; preserve the edited manager profile and refresh before changing")
        }
        let settings_revision: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision),0)+1 FROM role_settings
             WHERE task_id=?1 AND role='manager'",
            params![validation_task_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at)
             VALUES(?1,?2,'manager',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                validation_task_id,
                settings_revision,
                serde_json::to_string(host_manager)?,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at,scope_hash,configuration_hash,workflow_version,workflow_hash,setup_operation_id,upstream_source_hash,overlay_hash,legacy_migration_required)
             VALUES(?1,?2,?3,'planning',?4,1,'held',?5,?5,?6,?7,?8,?9,?10,?11,?12,0)",
            params![
                new_attempt_id,
                validation_task_id,
                uuid::Uuid::new_v4().to_string(),
                fixture_head,
                now,
                json_hash(&serde_json::from_str::<serde_json::Value>(&inventory_json)?)?,
                json_hash(host_manager)?,
                WORKFLOW_ID,
                crate::workflow_resources::workflow_hash(),
                new_setup_id,
                source_hash(),
                overlay_hash(),
            ],
        )?;
        tx.execute(
            "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?5,'{}','reserved',?6,?6)",
            params![
                workspace_id,
                new_attempt_id,
                fixture_identity,
                workspace_path.to_string_lossy(),
                fixture_head,
                now
            ],
        )?;
        tx.execute(
            "UPDATE trip_setup_operations SET state='superseded',updated_at=?1
             WHERE id=?2 AND state='discovery'",
            params![now, source_setup_id],
        )?;
        tx.execute(
            "INSERT INTO trip_setup_operations(id,project_id,fixture_project_id,validation_task_id,discovery_attempt_id,state,target_inventory_json,proposal_json,supersedes_setup_operation_id,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,'discovery',?6,'{}',?7,?8,?8)",
            params![
                new_setup_id,
                project_id,
                fixture_project_id,
                validation_task_id,
                new_attempt_id,
                inventory_json,
                source_setup_id,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state,profile_json,profile_hash,selected_at)
             VALUES(?1,'manager','selected',?2,?3,?4)",
            params![
                new_setup_id,
                serde_json::to_string(host_manager)?,
                json_hash(host_manager)?,
                now
            ],
        )?;
        for role in DELEGATED_ROLES {
            tx.execute(
                "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state)
                 VALUES(?1,?2,'unselected')",
                params![new_setup_id, role.to_string()],
            )?;
        }
        tx.execute(
            "INSERT INTO trip_setup_permits(id,setup_operation_id,attempt_id,fixture_project_id,fixture_repository_identity,role,profile_hash,settings_revision,purpose,approved_action,state,created_at)
             VALUES(?1,?2,?3,?4,?5,'manager',?6,?7,'setup_discovery','bounded_inventory_and_contained_read','issued',?8)",
            params![
                uuid::Uuid::new_v4().to_string(),
                new_setup_id,
                new_attempt_id,
                fixture_project_id,
                fixture_identity,
                json_hash(host_manager)?,
                settings_revision,
                now
            ],
        )?;
        tx.execute(
            "UPDATE controls SET state='superseded',updated_at=?1
             WHERE attempt_id=?2 AND kind='setup_manager_change' AND state='held'",
            params![now, source_attempt_id],
        )?;
        tx.execute(
            "UPDATE trip_project_state
             SET readiness='setup_in_progress',setup_operation_id=?1,
                 reason='Replacement discovery manager is ready for an explicit fresh launch',updated_at=?2
             WHERE project_id=?3 AND setup_operation_id=?4",
            params![new_setup_id, now, project_id, source_setup_id],
        )?;
        tx.commit()?;
        (
            project_id,
            fixture_project_id,
            validation_task_id,
            fixture_root,
            fixture_identity,
            fixture_head,
            inventory_json,
            source_manager_json,
            source_attempt_id,
            settings_revision,
        )
    };
    if let Err(error) = prepare_setup_workspace(
        store,
        &new_attempt_id,
        Path::new(&fixture_root),
        &fixture_identity,
        &fixture_head,
        &workspace_path,
    ) {
        let detail = format!("replacement discovery workspace materialization failed: {error:#}");
        mark_setup_workspace_recovery(store, &new_setup_id, &new_attempt_id, &detail)?;
        bail!("{detail}; retry begin_setup only with the newly selected manager after the setup state refreshes")
    }
    Ok(operation_result(
        operation_id,
        "trip_setup",
        &new_setup_id,
        Some(expected_project_version + 1),
        "manager_replacement_ready",
        serde_json::json!({
            "source_setup_operation_id":source_setup_id,
            "project_id":project_id,
            "fixture_project_id":fixture_project_id,
            "validation_task_id":validation_task_id,
            "prior_discovery_attempt_id":source_attempt_id,
            "discovery_attempt_id":new_attempt_id,
            "manager_settings_revision":settings_revision,
            "old_manager_profile":serde_json::from_str::<serde_json::Value>(&source_manager_json)?,
            "requested_manager_profile":host_manager,
            "target_inventory":serde_json::from_str::<serde_json::Value>(&inventory_json)?,
            "fresh_launch_required":true,
            "resume_allowed":false
        }),
    ))
}

fn save_setup_draft(
    store: &Store,
    operation_id: &str,
    setup_id: &str,
    expected_project_version: i64,
    proposal_value: &serde_json::Value,
) -> Result<OperationResult> {
    let proposal = setup_proposal_preserving_configuration(store, setup_id, proposal_value)?;
    validate_setup_proposal(&proposal)?;
    validate_agents_preservation(store, setup_id, &proposal)?;
    validate_canonical_migration(store, setup_id, &proposal)?;
    let proposal_json = serde_json::to_value(&proposal)?;
    let proposal_hash = json_hash(&proposal_json)?;
    let preimages = proposed_preimages(store, setup_id, &proposal)?;
    let preimage_bindings = preimages
        .iter()
        .map(|value| {
            serde_json::json!({
                "relative_path":value["relative_path"],
                "preimage_sha256":value["preimage_sha256"]
            })
        })
        .collect::<Vec<_>>();
    let preimages_hash = json_hash(&preimage_bindings)?;
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (project_id, project_version): (String, i64) = tx.query_row(
        "SELECT so.project_id,p.version FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id
         WHERE so.id=?1 AND so.state IN ('discovery','draft')",
        params![setup_id], |row| Ok((row.get(0)?, row.get(1)?))
    )?;
    if project_version != expected_project_version {
        bail!("project version is stale")
    }
    let manager_replacement_held: bool = tx.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM controls c JOIN trip_setup_operations so
             ON so.discovery_attempt_id=c.attempt_id
           WHERE so.id=?1 AND c.kind='setup_manager_change' AND c.state='held'
         )",
        params![setup_id],
        |row| row.get(0),
    )?;
    if manager_replacement_held {
        bail!("setup manager replacement is held; change the manager in a fresh discovery revision before saving a proposal")
    }
    let selected_manager:String=tx.query_row(
        "SELECT profile_json FROM trip_setup_profile_selections WHERE setup_operation_id=?1 AND role='manager' AND selection_state='selected'",
        params![setup_id],|row|row.get(0)
    )?;
    let proposed_manager = proposal
        .host_manager
        .as_ref()
        .ok_or_else(|| anyhow!("setup proposal must preserve the selected host_manager"))?;
    if serde_json::to_value(proposed_manager)?
        != serde_json::from_str::<serde_json::Value>(&selected_manager)?
    {
        bail!(
            "setup proposal host_manager differs from the manager profile selected at setup start"
        )
    }
    for role in DELEGATED_ROLES {
        let key = role.to_string();
        let role_config = proposal
            .roles
            .get(&key)
            .ok_or_else(|| anyhow!("missing role {key}"))?;
        let profile = proposal
            .profiles
            .get(&role_config.profile)
            .ok_or_else(|| anyhow!("role {key} references a missing profile"))?;
        tx.execute(
            "UPDATE trip_setup_profile_selections SET selection_state='selected',profile_json=?1,profile_hash=?2,selected_at=?3
             WHERE setup_operation_id=?4 AND role=?5",
            params![serde_json::to_string(profile)?, json_hash(profile)?, now, setup_id, key],
        )?;
    }
    tx.execute(
        "UPDATE trip_setup_operations SET state='draft',proposal_json=?1,proposal_hash=?2,approved_preimages_hash=?3,updated_at=?4 WHERE id=?5",
        params![proposal_json.to_string(), proposal_hash, preimages_hash, now, setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state SET reason='Setup draft requires separate probe authorization',updated_at=?1 WHERE project_id=?2",
        params![now, project_id],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        Some(expected_project_version),
        "draft_saved",
        serde_json::json!({
            "proposal_hash":proposal_hash,"approved_preimages_hash":preimages_hash,"preimages":preimages,
            "selected_profile_tuple_count":unique_profile_count(&proposal),"delegated_role_probe_count":DELEGATED_ROLES.len(),"total_role_preflight_count":1+DELEGATED_ROLES.len()
        }),
    ))
}

fn revise_setup_draft(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    source_setup_id: &str,
    expected_project_version: i64,
    proposal_value: &serde_json::Value,
) -> Result<OperationResult> {
    let proposal = setup_proposal_preserving_configuration(store, source_setup_id, proposal_value)?;
    validate_setup_proposal(&proposal)?;
    let manager = proposal
        .host_manager
        .as_ref()
        .ok_or_else(|| anyhow!("configuration revision requires an explicit host manager"))?;
    validate_role_override(manager)?;
    validate_agents_preservation(store, source_setup_id, &proposal)?;
    validate_canonical_migration(store, source_setup_id, &proposal)?;
    let proposal_json = serde_json::to_value(&proposal)?;
    let proposal_hash = json_hash(&proposal_json)?;
    let preimages = proposed_preimages(store, source_setup_id, &proposal)?;
    let preimages_hash = json_hash(
        &preimages
            .iter()
            .map(|value| {
                serde_json::json!({
                    "relative_path":value["relative_path"],
                    "preimage_sha256":value["preimage_sha256"]
                })
            })
            .collect::<Vec<_>>(),
    )?;
    let new_setup_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (project_id,project_version,source_state,fixture_project,validation_task,discovery_attempt,inventory):(String,i64,String,String,String,String,String)=tx.query_row(
        "SELECT so.project_id,p.version,so.state,so.fixture_project_id,so.validation_task_id,so.discovery_attempt_id,so.target_inventory_json
         FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id
         WHERE so.id=?1 AND so.state IN ('probing','preflight_complete','finalized','install_authorized','activated')",
        params![source_setup_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))
    )?;
    if project_version != expected_project_version {
        bail!("project version is stale")
    }
    let another_active:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM trip_setup_operations WHERE project_id=?1 AND id!=?2 AND state NOT IN ('activated','aborted','superseded'))",
        params![project_id,source_setup_id],|row|row.get(0)
    )?;
    if another_active {
        bail!("project already has another pending configuration revision")
    }
    let active_sessions:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id WHERE a.setup_operation_id=?1 AND s.status NOT IN ('exited','launch_failed'))",
        params![source_setup_id],|row|row.get(0)
    )?;
    if active_sessions {
        bail!("configuration revision requires the prior setup sessions to be quiescent")
    }
    if source_state != "activated" {
        tx.execute(
            "UPDATE trip_setup_operations SET state='superseded',updated_at=?1 WHERE id=?2",
            params![now, source_setup_id],
        )?;
    }
    tx.execute(
        "INSERT INTO trip_setup_operations(id,project_id,fixture_project_id,validation_task_id,discovery_attempt_id,state,target_inventory_json,proposal_json,proposal_hash,approved_preimages_hash,supersedes_setup_operation_id,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,'draft',?6,?7,?8,?9,?10,?11,?11)",
        params![new_setup_id,project_id,fixture_project,validation_task,discovery_attempt,inventory,proposal_json.to_string(),proposal_hash,preimages_hash,source_setup_id,now],
    )?;
    let mut changed_roles = Vec::new();
    let mut selections = vec![(
        "manager".to_owned(),
        serde_json::to_string(manager)?,
        json_hash(manager)?,
    )];
    for role in DELEGATED_ROLES {
        let name = role.to_string();
        let selected = &proposal.roles[&name];
        let profile = &proposal.profiles[&selected.profile];
        selections.push((name, serde_json::to_string(profile)?, json_hash(profile)?));
    }
    for (role, profile_json, profile_hash) in selections {
        tx.execute(
            "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state,profile_json,profile_hash,selected_at)
             VALUES(?1,?2,'selected',?3,?4,?5)",
            params![new_setup_id,role,profile_json,profile_hash,now],
        )?;
        let source_hash:Option<String>=tx.query_row(
            "SELECT profile_hash FROM trip_setup_profile_selections WHERE setup_operation_id=?1 AND role=?2 AND selection_state='selected'",
            params![source_setup_id,role],|row|row.get(0)
        ).optional()?;
        let receipt = if source_hash.as_deref() == Some(profile_hash.as_str()) {
            effective_receipt_id(&tx, source_setup_id, &role, &profile_hash, runtime)?
        } else {
            None
        };
        if let Some(receipt) = receipt {
            tx.execute(
                "INSERT INTO trip_setup_proof_reuse(setup_operation_id,role,profile_hash,source_receipt_id,approved_at) VALUES(?1,?2,?3,?4,?5)",
                params![new_setup_id,role,profile_hash,receipt,now],
            )?;
        } else {
            changed_roles.push(role);
        }
    }
    tx.execute(
        "UPDATE trip_project_state SET setup_operation_id=?1,
         readiness=CASE WHEN readiness='ready' THEN readiness ELSE 'setup_in_progress' END,
         reason='Pending reviewed configuration revision requires affected-profile preflight and separate activation',updated_at=?2 WHERE project_id=?3",
        params![new_setup_id,now,project_id],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "trip_setup",
        &new_setup_id,
        Some(expected_project_version),
        "revision_draft_saved",
        serde_json::json!({"supersedes_setup_operation_id":source_setup_id,"proposal_hash":proposal_hash,
            "approved_preimages_hash":preimages_hash,"preimages":preimages,"affected_proof_roles":changed_roles}),
    ))
}

fn project_runtime_profile(
    connection: &Connection,
    project_id: &str,
    role: RoleKind,
) -> Result<(RuntimeProbePlan, String)> {
    require_project_ready(connection, project_id)?;
    let (repository,settings_json,revision,configuration_hash,config_json,adapters_json,setup_id):(String,String,String,String,String,String,String)=connection.query_row(
        "SELECT p.repository_path,p.settings_json,s.active_config_revision_id,r.configuration_hash,r.config_json,r.adapters_json,s.setup_operation_id
         FROM projects p JOIN trip_project_state s ON s.project_id=p.id
         JOIN trip_config_revisions r ON r.id=s.active_config_revision_id AND r.project_id=p.id
         WHERE p.id=?1 AND p.internal_purpose IS NULL",
        params![project_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?))
    ).optional()?.ok_or_else(||anyhow!("project has no activated runtime configuration or setup fixture"))?;
    let settings: serde_json::Value = serde_json::from_str(&settings_json)?;
    let config: RoleOverride = serde_json::from_value(
        settings
            .pointer(&format!("/roles/{}", role))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "choose the missing {} provider, model, and effort before runtime verification",
                    role
                )
            })?,
    )?;
    validate_role_override(&config)?;
    let (adapter_name, adapter_hash, profile_json) = if role == RoleKind::Manager {
        let adapter = service_manager_adapter(config.provider);
        (
            "llmrelay_service_native_manager".to_owned(),
            json_hash(&adapter)?,
            serde_json::json!({"adapter":"llmrelay_service_native_manager","provider":config.provider,"model":config.model,"effort":config.effort,"authority":"read-only","session":"retained"}),
        )
    } else {
        let configuration: serde_json::Value = serde_json::from_str(&config_json)?;
        let adapters: serde_json::Value = serde_json::from_str(&adapters_json)?;
        let profile_id = configuration
            .pointer(&format!("/roles/{}/profile", role))
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "active project configuration has no selected {} profile",
                    role
                )
            })?;
        let profile = configuration
            .pointer(&format!("/profiles/{profile_id}"))
            .cloned()
            .ok_or_else(|| {
                anyhow!("active project configuration is missing profile {profile_id}")
            })?;
        let adapter_name = profile
            .get("adapter")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("profile {profile_id} has no adapter"))?
            .to_owned();
        let definition = adapters
            .pointer(&format!("/adapters/{adapter_name}"))
            .cloned()
            .ok_or_else(|| anyhow!("active adapter {adapter_name} is missing"))?;
        (
            adapter_name.clone(),
            json_hash(&serde_json::json!({"adapter":adapter_name,"definition":definition}))?,
            profile,
        )
    };
    let profile_hash = json_hash(&profile_json)?;
    let _ = setup_id;
    Ok((
        RuntimeProbePlan {
            role,
            settings_revision: None,
            source: "project_default".into(),
            config,
            profile_json,
            profile_hash,
            project_config_revision_id: revision,
            project_configuration_hash: configuration_hash,
            adapter_name,
            adapter_hash,
            capability_key: String::new(),
            capability_identity_json: String::new(),
            capability_id: None,
        },
        repository,
    ))
}

fn runtime_fixture_scope(
    connection: &Connection,
    project_id: &str,
) -> Result<(String, String, String, String, String)> {
    connection.query_row(
        "SELECT so.id,fixture.id,fixture.repository_path,fixture.repository_identity,fixture.base_revision
         FROM trip_project_state s JOIN trip_setup_operations selected ON selected.id=s.setup_operation_id
         JOIN trip_setup_operations so ON so.id=CASE WHEN selected.state='activated' THEN selected.id ELSE selected.supersedes_setup_operation_id END
         JOIN projects fixture ON fixture.id=so.fixture_project_id
         WHERE s.project_id=?1 AND s.readiness='ready' AND so.state='activated' AND fixture.internal_purpose='trip_setup_fixture'",
        params![project_id],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))
    ).optional()?.ok_or_else(||anyhow!("runtime verification requires the activated app-owned empty setup fixture"))
}

fn current_runtime_probe_plan(
    connection: &Connection,
    runtime: &CapabilityRuntime,
    project_id: &str,
    task_id: Option<&str>,
    role: RoleKind,
    requested_revision: Option<i64>,
    fixture_root: &str,
) -> Result<RuntimeProbePlan> {
    let (mut plan, target_cwd) = if let Some(task) = task_id {
        let revision = requested_revision.ok_or_else(|| {
            anyhow!("task runtime verification requires the exact settings revision")
        })?;
        let latest: i64 = connection.query_row(
            "SELECT MAX(revision) FROM role_settings WHERE task_id=?1 AND role=?2",
            params![task, role.to_string()],
            |row| row.get(0),
        )?;
        if latest != revision {
            bail!("task runtime verification no longer matches the latest role settings revision")
        }
        let (descriptor, config) =
            task_profile_descriptor(connection, task, &role.to_string(), revision)?;
        if descriptor.source == "project_default" {
            bail!("unchanged project-default role requires project-scoped runtime verification; prepare this role without task_id or settings_revision using the current project version")
        }
        let cwd:String=connection.query_row("SELECT COALESCE((SELECT w.path FROM attempts a JOIN workspaces w ON w.attempt_id=a.id WHERE a.task_id=t.id AND w.state='ready' ORDER BY a.created_at DESC LIMIT 1),p.repository_path) FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?1 AND t.project_id=?2 AND t.archived_at IS NULL",params![task,project_id],|row|row.get(0))?;
        (
            RuntimeProbePlan {
                role,
                settings_revision: Some(revision),
                source: descriptor.source,
                config,
                profile_json: descriptor.profile_json,
                profile_hash: descriptor.profile_hash,
                project_config_revision_id: descriptor.project_config_revision_id,
                project_configuration_hash: descriptor.project_configuration_hash,
                adapter_name: descriptor.adapter_name,
                adapter_hash: descriptor.adapter_hash,
                capability_key: String::new(),
                capability_identity_json: String::new(),
                capability_id: None,
            },
            cwd,
        )
    } else {
        project_runtime_profile(connection, project_id, role)?
    };
    let prepare = |cwd: &Path| {
        crate::providers::prepare_role_launch_with_bundles(
            plan.config.provider,
            role,
            &plan.config.model,
            &plan.config.effort,
            cwd,
            "ordinary runtime capability preparation",
            &runtime.role_socket,
            "normalized-runtime-token",
            "normalized-runtime-generation",
            "normalized-runtime-session",
            None,
            &runtime.hooks,
            &runtime.executable,
            &runtime.compatibility_bundles,
        )
    };
    let target = prepare(Path::new(&target_cwd))?;
    let fixture = prepare(Path::new(fixture_root))?;
    runtime.require_current_policy(&target.config)?;
    runtime.require_current_policy(&fixture.config)?;
    let target_key = crate::providers::capability_key(&target.config)?;
    let fixture_key = crate::providers::capability_key(&fixture.config)?;
    if target_key != fixture_key {
        bail!("{} runtime policy differs between the target and disposable empty fixture; ordinary proof cannot be relabelled or forged",role)
    }
    plan.capability_key = fixture_key;
    plan.capability_identity_json =
        serde_json::to_string(&crate::providers::capability_identity(&fixture.config)?)?;
    let (setup_id, fixture_project, current_fixture_root, fixture_identity, _) =
        runtime_fixture_scope(connection, project_id)?;
    if current_fixture_root != fixture_root {
        bail!("runtime fixture changed during capability preparation")
    }
    let capability_identity =
        serde_json::from_str::<serde_json::Value>(&plan.capability_identity_json)?;
    plan.capability_id = scoped_capability_binding(
        connection,
        &fixture.config,
        &RuntimeScopeExpectation {
            project_id,
            task_id: if plan.source == "task_override" {
                task_id
            } else {
                None
            },
            role,
            settings_revision: if plan.source == "task_override" {
                plan.settings_revision
            } else {
                None
            },
            source: &plan.source,
            profile_hash: &plan.profile_hash,
            project_config_revision_id: &plan.project_config_revision_id,
            project_configuration_hash: &plan.project_configuration_hash,
            adapter_name: &plan.adapter_name,
            adapter_hash: &plan.adapter_hash,
            capability_key: &plan.capability_key,
            capability_identity: &capability_identity,
            setup_operation_id: &setup_id,
            fixture_project_id: &fixture_project,
            fixture_repository_identity: &fixture_identity,
            fixture_root_hash: &sha256(fixture_root.as_bytes()),
        },
        &runtime.compatibility_bundles,
    )?
    .map(|binding| binding.0);
    Ok(plan)
}

fn runtime_admission_scope_hash(
    project_id: &str,
    task_id: Option<&str>,
    entity_version: i64,
    setup_id: &str,
    fixture_project_id: &str,
    fixture_identity: &str,
    fixture_root: &str,
    cmux_socket_path: Option<&str>,
    plans: &[RuntimeProbePlan],
) -> Result<String> {
    let profiles=plans.iter().map(|plan|{
        let cmux_socket_path_hash = (plan.config.provider == Provider::Claude)
            .then(|| cmux_socket_path.map(|path| sha256(path.as_bytes())))
            .flatten();
        serde_json::json!({"role":plan.role,"settings_revision":plan.settings_revision,"source":plan.source,"profile_hash":plan.profile_hash,"project_config_revision_id":plan.project_config_revision_id,"project_configuration_hash":plan.project_configuration_hash,"adapter":plan.adapter_name,"adapter_hash":plan.adapter_hash,"capability_key":plan.capability_key,"capability_identity":plan.capability_identity_json,"cmux_socket_path_hash":cmux_socket_path_hash})
    }).collect::<Vec<_>>();
    json_hash(
        &serde_json::json!({"project_id":project_id,"task_id":task_id,"entity_version":entity_version,"setup_operation_id":setup_id,"fixture_project_id":fixture_project_id,"fixture_repository_identity":fixture_identity,"fixture_root_hash":sha256(fixture_root.as_bytes()),"profiles":profiles}),
    )
}

fn prepare_runtime_admission(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    project_id: &str,
    task_id: Option<&str>,
    requested_role: Option<RoleKind>,
    requested_revision: Option<i64>,
    cmux_socket_path: Option<&str>,
    expected_version: i64,
) -> Result<OperationResult> {
    if task_id.is_some() != requested_revision.is_some()
        || (task_id.is_some() && requested_role.is_none())
        || (task_id.is_none() && requested_revision.is_some())
    {
        bail!("task runtime verification requires task_id, role, and settings_revision together; project role correction permits role without a settings revision")
    }
    let (fixture_project, fixture_identity, fixture_root, _fixture_head, setup_id, mut plans) = {
        let connection = store.lock()?;
        let current: bool = if let Some(task) = task_id {
            connection.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND project_id=?2 AND version=?3 AND archived_at IS NULL)",params![task,project_id,expected_version],|row|row.get(0))?
        } else {
            connection.query_row("SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND version=?2 AND internal_purpose IS NULL)",params![project_id,expected_version],|row|row.get(0))?
        };
        if !current {
            bail!("project or task version is stale")
        }
        let (setup_id, fixture_project, fixture_root, fixture_identity, fixture_head) =
            runtime_fixture_scope(&connection, project_id)?;
        let roles: Vec<RoleKind> = requested_role
            .map(|role| vec![role])
            .unwrap_or_else(|| APP_ROLES.iter().map(|role| role.parse().unwrap()).collect());
        let mut plans = Vec::with_capacity(roles.len());
        for role in roles {
            plans.push(current_runtime_probe_plan(
                &connection,
                runtime,
                project_id,
                task_id,
                role,
                requested_revision,
                &fixture_root,
            )?);
        }
        (
            fixture_project,
            fixture_identity,
            fixture_root,
            fixture_head,
            setup_id,
            plans,
        )
    };
    let cmux_socket_path = plans
        .iter()
        .any(|plan| plan.config.provider == Provider::Claude)
        .then(|| validate_runtime_cmux_socket_path(cmux_socket_path))
        .transpose()?;
    let admission = uuid::Uuid::new_v4().to_string();
    let scope_hash = runtime_admission_scope_hash(
        project_id,
        task_id,
        expected_version,
        &setup_id,
        &fixture_project,
        &fixture_identity,
        &fixture_root,
        cmux_socket_path.as_deref(),
        &plans,
    )?;
    let scope=plans.iter().map(|plan|serde_json::json!({"role":plan.role,"settings_revision":plan.settings_revision,"source":plan.source,"profile_hash":plan.profile_hash,"project_config_revision_id":plan.project_config_revision_id,"project_configuration_hash":plan.project_configuration_hash,"adapter":plan.adapter_name,"adapter_hash":plan.adapter_hash,"capability_key":plan.capability_key,"capability_identity":plan.capability_identity_json,"already_supported":plan.capability_id.is_some()})).collect::<Vec<_>>();
    let fresh_call_count = plans
        .iter()
        .filter(|plan| plan.capability_id.is_none())
        .count() as i64;
    let state = if fresh_call_count == 0 {
        "ready"
    } else {
        "pending_approval"
    };
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("INSERT INTO trip_runtime_admissions(id,project_id,task_id,scope_hash,state,fresh_call_count,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",params![admission,project_id,task_id,scope_hash,state,fresh_call_count,now])?;
    for plan in plans.drain(..) {
        let probe_state = if plan.capability_id.is_some() {
            "current"
        } else {
            "pending_approval"
        };
        let probe_cmux_socket = (plan.config.provider == Provider::Claude)
            .then_some(cmux_socket_path.as_deref())
            .flatten();
        tx.execute("INSERT INTO trip_runtime_probes(admission_id,role,settings_revision,launch_config_json,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_key,capability_identity_json,fixture_project_id,fixture_repository_identity,fixture_root,cmux_socket_path,nonce,state,capability_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?20)",params![admission,plan.role.to_string(),plan.settings_revision,serde_json::to_string(&plan.config)?,plan.profile_json.to_string(),plan.profile_hash,plan.project_config_revision_id,plan.project_configuration_hash,plan.adapter_name,plan.adapter_hash,plan.capability_key,plan.capability_identity_json,fixture_project,fixture_identity,fixture_root,probe_cmux_socket,uuid::Uuid::new_v4().simple().to_string(),probe_state,plan.capability_id,now])?;
    }
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "runtime_admission",
        &admission,
        None,
        state,
        serde_json::json!({"admission_id":admission,"scope_hash":scope_hash,"fresh_call_count":fresh_call_count,"profiles":scope,"target_execution":false,"setup_receipts_promoted":false}),
    ))
}

fn validate_runtime_cmux_socket_path(value: Option<&str>) -> Result<String> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Claude runtime requalification requires the exact human-confirmed live cmux Unix socket path"))?;
    if value.len() > 4096 || value.contains('\0') || !Path::new(value).is_absolute() {
        bail!("Claude runtime requalification requires a bounded absolute cmux Unix socket path")
    }
    Ok(value.to_owned())
}

fn runtime_phase(role: RoleKind) -> &'static str {
    match role {
        RoleKind::PlanReviewer => "plan_review",
        RoleKind::Implementer => "implementation",
        RoleKind::CodeReviewer => "code_review",
        RoleKind::FinalReviewer => "final_review",
        _ => "planning",
    }
}

fn authorize_runtime_admission(
    store: &Store,
    paths: &InstancePaths,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    admission_id: &str,
    expected_scope_hash: &str,
) -> Result<OperationResult> {
    let probes = {
        let connection = store.lock()?;
        let valid:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM trip_runtime_admissions WHERE id=?1 AND scope_hash=?2 AND state='pending_approval')",params![admission_id,expected_scope_hash],|row|row.get(0))?;
        if !valid {
            bail!("runtime admission scope changed, was already authorized, or is not pending")
        }
        let mut statement=connection.prepare("SELECT p.role,p.launch_config_json,p.profile_hash,p.fixture_project_id,p.fixture_repository_identity,p.fixture_root,fixture.base_revision,active_setup.id FROM trip_runtime_probes p JOIN projects fixture ON fixture.id=p.fixture_project_id JOIN trip_runtime_admissions a ON a.id=p.admission_id JOIN trip_project_state s ON s.project_id=a.project_id JOIN trip_setup_operations selected ON selected.id=s.setup_operation_id JOIN trip_setup_operations active_setup ON active_setup.id=CASE WHEN selected.state='activated' THEN selected.id ELSE selected.supersedes_setup_operation_id END WHERE p.admission_id=?1 AND p.state='pending_approval' AND active_setup.state='activated' ORDER BY p.role")?;
        let mut rows = statement
            .query_map(params![admission_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.sort_by_key(|row| {
            APP_ROLES
                .iter()
                .position(|role| *role == row.0)
                .unwrap_or(APP_ROLES.len())
        });
        rows
    };
    let now = Utc::now().to_rfc3339();
    let mut prepared = Vec::new();
    for (
        role_text,
        config_json,
        profile_hash,
        fixture_project,
        fixture_identity,
        fixture_root,
        fixture_head,
        setup_id,
    ) in probes
    {
        let role: RoleKind = role_text.parse().map_err(|error: String| anyhow!(error))?;
        let config: RoleOverride = serde_json::from_str(&config_json)?;
        let attempt = uuid::Uuid::new_v4().to_string();
        let workspace = paths
            .artifacts
            .join("runtime-probes")
            .join(admission_id)
            .join(&role_text);
        let service_sentinel = paths
            .state
            .join(format!("runtime-probe-{admission_id}-{role_text}"));
        let (task_id, revision) = {
            let mut connection = store.lock()?;
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let task_id:String=tx.query_row("SELECT validation_task_id FROM trip_setup_operations WHERE id=?1 AND fixture_project_id=?2",params![setup_id,fixture_project],|row|row.get(0))?;
            let revision:i64=tx.query_row("SELECT COALESCE(MAX(revision),0)+1 FROM role_settings WHERE task_id=?1 AND role=?2",params![task_id,role_text],|row|row.get(0))?;
            tx.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at,scope_hash,configuration_hash,workflow_version,workflow_hash,setup_operation_id,upstream_source_hash,overlay_hash,legacy_migration_required) VALUES(?1,?2,?3,?4,?5,1,'held',?6,?6,?7,?8,?9,?10,?11,?12,?13,0)",params![attempt,task_id,uuid::Uuid::new_v4().to_string(),runtime_phase(role),fixture_head,now,expected_scope_hash,json_hash(&config)?,WORKFLOW_ID,crate::workflow_resources::workflow_hash(),setup_id,source_hash(),overlay_hash()])?;
            tx.execute("INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![uuid::Uuid::new_v4().to_string(),task_id,role_text,revision,config_json,now])?;
            tx.execute("INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5,'{}','reserved',?6,?6)",params![uuid::Uuid::new_v4().to_string(),attempt,fixture_identity,workspace.to_string_lossy(),fixture_head,now])?;
            tx.execute("INSERT INTO trip_setup_permits(id,setup_operation_id,attempt_id,fixture_project_id,fixture_repository_identity,role,profile_hash,settings_revision,purpose,approved_action,nonce,state,created_at) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,'runtime_probe','ordinary_runtime_capability',nonce,'issued',?9 FROM trip_runtime_probes WHERE admission_id=?10 AND role=?6",params![uuid::Uuid::new_v4().to_string(),setup_id,attempt,fixture_project,fixture_identity,role_text,profile_hash,revision,now,admission_id])?;
            tx.execute("UPDATE trip_runtime_probes SET attempt_id=?1,workspace_path=?2,service_sentinel_path=?3,control_socket_path=?4,state='authorized',updated_at=?5 WHERE admission_id=?6 AND role=?7 AND state='pending_approval'",params![attempt,workspace.to_string_lossy(),service_sentinel.to_string_lossy(),paths.control_socket.to_string_lossy(),now,admission_id,role_text])?;
            tx.commit()?;
            (task_id, revision)
        };
        if let Err(error) = prepare_setup_workspace(
            store,
            &attempt,
            Path::new(&fixture_root),
            &fixture_identity,
            &fixture_head,
            &workspace,
        ) {
            let reason = format!("ordinary runtime fixture preparation failed: {error:#}");
            let connection = store.lock()?;
            connection.execute("UPDATE trip_runtime_probes SET state='failed',failure_reason=?1,updated_at=?2 WHERE admission_id=?3 AND role=?4",params![reason,now,admission_id,role_text])?;
            refresh_runtime_admission_state(&connection, admission_id, &now)?;
            return Err(anyhow!(reason));
        }
        let ordinary_launch = crate::providers::prepare_role_launch_with_bundles(
            config.provider,
            role,
            &config.model,
            &config.effort,
            &workspace,
            "ordinary runtime capability preparation",
            &runtime.role_socket,
            "normalized-runtime-token",
            "normalized-runtime-generation",
            "normalized-runtime-session",
            None,
            &runtime.hooks,
            &runtime.executable,
            &runtime.compatibility_bundles,
        )?;
        runtime.require_current_policy(&ordinary_launch.config)?;
        let key = crate::providers::capability_key(&ordinary_launch.config)?;
        let expected: String = store.lock()?.query_row(
            "SELECT capability_key FROM trip_runtime_probes WHERE admission_id=?1 AND role=?2",
            params![admission_id, role_text],
            |row| row.get(0),
        )?;
        if key != expected {
            bail!("ordinary runtime fixture policy changed after human authorization")
        }
        let diagnostic_key =
            if let Some(policy) = runtime_probe_launch_policy(store, &attempt, role, None)? {
                let diagnostic_launch =
                    crate::providers::prepare_runtime_probe_role_launch_with_bundles(
                        config.provider,
                        role,
                        &config.model,
                        &config.effort,
                        &workspace,
                        "ordinary runtime capability preparation",
                        &runtime.role_socket,
                        "normalized-runtime-token",
                        "normalized-runtime-generation",
                        "normalized-runtime-session",
                        None,
                        &runtime.hooks,
                        &runtime.executable,
                        &policy,
                        &runtime.compatibility_bundles,
                    )?;
                runtime.require_current_policy(&diagnostic_launch.config)?;
                Some(crate::providers::capability_key(&diagnostic_launch.config)?)
            } else {
                None
            };
        prepared.push(serde_json::json!({"role":role,"attempt_id":attempt,"task_id":task_id,"settings_revision":revision,"workspace":workspace,"capability_key":key,"runtime_probe_policy_key":diagnostic_key}));
    }
    let connection = store.lock()?;
    connection.execute("UPDATE trip_runtime_admissions SET state='authorized',authorized_at=?1,updated_at=?1 WHERE id=?2 AND state='pending_approval'",params![now,admission_id])?;
    Ok(operation_result(
        operation_id,
        "runtime_admission",
        admission_id,
        None,
        "authorized",
        serde_json::json!({"admission_id":admission_id,"scope_hash":expected_scope_hash,"fresh_call_count":prepared.len(),"probes":prepared,"launches_started":0}),
    ))
}

fn validate_current_runtime_admission_scope(
    connection: &Connection,
    runtime: &CapabilityRuntime,
    admission_id: &str,
) -> Result<RuntimeScopeValidation> {
    let (project_id, task_id, frozen_scope): (String, Option<String>, String) = connection
        .query_row(
            "SELECT project_id,task_id,scope_hash FROM trip_runtime_admissions WHERE id=?1",
            params![admission_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("runtime admission does not exist"))?;
    let mut cmux_socket_statement = connection.prepare(
        "SELECT DISTINCT cmux_socket_path FROM trip_runtime_probes
         WHERE admission_id=?1 AND cmux_socket_path IS NOT NULL",
    )?;
    let cmux_socket_paths = cmux_socket_statement
        .query_map(params![admission_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if cmux_socket_paths.len() > 1 {
        return Ok(RuntimeScopeValidation::Drift(
            "runtime admission has inconsistent frozen cmux socket paths; prepare corrected runtime verification"
                .into(),
        ));
    }
    let cmux_socket_path = cmux_socket_paths.first().map(String::as_str);
    let entity_version: i64 = if let Some(task) = task_id.as_deref() {
        connection.query_row(
            "SELECT version FROM tasks WHERE id=?1 AND project_id=?2 AND archived_at IS NULL",
            params![task, project_id],
            |row| row.get(0),
        )?
    } else {
        connection.query_row(
            "SELECT version FROM projects WHERE id=?1 AND internal_purpose IS NULL",
            params![project_id],
            |row| row.get(0),
        )?
    };
    let (setup_id, fixture_project, fixture_root, fixture_identity, _) =
        runtime_fixture_scope(connection, &project_id)?;
    let requested = {
        let mut statement = connection.prepare(
            "SELECT role,settings_revision,profile_hash,project_config_revision_id,project_configuration_hash,
                    adapter_name,adapter_hash,capability_key,capability_identity_json
             FROM trip_runtime_probes WHERE admission_id=?1 ORDER BY role",
        )?;
        let mut rows = statement
            .query_map(params![admission_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.sort_by_key(|row| {
            APP_ROLES
                .iter()
                .position(|role| *role == row.0)
                .unwrap_or(APP_ROLES.len())
        });
        rows
    };
    let mut plans = Vec::with_capacity(requested.len());
    let mut drift = Vec::new();
    for (
        role,
        revision,
        profile,
        config_revision,
        configuration,
        adapter,
        adapter_hash,
        capability,
        identity,
    ) in requested
    {
        if let (Some(task), Some(frozen_revision)) = (task_id.as_deref(), revision) {
            let latest: Option<i64> = connection
                .query_row(
                    "SELECT MAX(revision) FROM role_settings WHERE task_id=?1 AND role=?2",
                    params![task, role],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if latest != Some(frozen_revision) {
                return Ok(RuntimeScopeValidation::Drift(format!(
                    "runtime admission scope is stale (changed fields: {role}:settings_revision); prepare corrected runtime verification for the current task setting"
                )));
            }
        }
        let plan = current_runtime_probe_plan(
            connection,
            runtime,
            &project_id,
            task_id.as_deref(),
            role.parse().map_err(|error: String| anyhow!(error))?,
            revision,
            &fixture_root,
        )?;
        for (name, unchanged) in [
            ("profile", plan.profile_hash == profile),
            (
                "config_revision",
                plan.project_config_revision_id == config_revision,
            ),
            (
                "configuration",
                plan.project_configuration_hash == configuration,
            ),
            ("adapter", plan.adapter_name == adapter),
            ("adapter_hash", plan.adapter_hash == adapter_hash),
            ("capability", plan.capability_key == capability),
            ("identity", plan.capability_identity_json == identity),
        ] {
            if !unchanged {
                drift.push(format!("{role}:{name}"));
            }
        }
        plans.push(plan);
    }
    let current_scope = runtime_admission_scope_hash(
        &project_id,
        task_id.as_deref(),
        entity_version,
        &setup_id,
        &fixture_project,
        &fixture_identity,
        &fixture_root,
        cmux_socket_path,
        &plans,
    )?;
    if current_scope != frozen_scope {
        return Ok(RuntimeScopeValidation::Drift(format!(
            "runtime admission scope is stale (changed fields: {}); prepare corrected runtime verification for the current project, task, setup, profile, adapter, and launch identity",
            if drift.is_empty(){"project/task/setup/fixture identity".into()}else{drift.join(",")}
        )));
    }
    Ok(RuntimeScopeValidation::Current(serde_json::json!({
        "project_id":project_id,
        "task_id":task_id,
        "entity_version":entity_version,
        "setup_operation_id":setup_id,
        "fixture_project_id":fixture_project,
        "fixture_repository_identity":fixture_identity,
        "fixture_root_hash":sha256(fixture_root.as_bytes()),
        "cmux_socket_path_hash":cmux_socket_path.map(|path| sha256(path.as_bytes())),
        "scope_hash":current_scope,
        "profiles":plans.iter().map(|plan|serde_json::json!({
            "role":plan.role,"settings_revision":plan.settings_revision,"source":plan.source,"profile_hash":plan.profile_hash,
            "project_config_revision_id":plan.project_config_revision_id,"project_configuration_hash":plan.project_configuration_hash,
            "adapter":plan.adapter_name,"adapter_hash":plan.adapter_hash,"capability_key":plan.capability_key,
            "capability_identity":plan.capability_identity_json
        })).collect::<Vec<_>>()
    })))
}

pub(crate) fn validate_runtime_probe_resume_authority(
    connection: &Connection,
    runtime: Option<&CapabilityRuntime>,
    session_id: &str,
    launch: &LaunchConfig,
    _capability_key: &str,
) -> Result<bool> {
    let is_runtime_probe: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND validation_cell='trip_runtime_probe')",
        params![session_id],
        |row| row.get(0),
    )?;
    if !is_runtime_probe {
        return Ok(false);
    }
    let runtime = runtime.ok_or_else(|| {
        anyhow!("runtime probe resume requires current service runtime scope validation")
    })?;
    if launch.role == RoleKind::FinalReviewer {
        bail!("final verifier runtime probes are fresh-only and cannot resume")
    }
    let admission_id: String = connection
        .query_row(
            "SELECT p.admission_id
         FROM trip_runtime_probes p
         JOIN trip_runtime_admissions a ON a.id=p.admission_id
         JOIN sessions s ON s.id=p.session_id
         JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts attempt ON attempt.id=rg.attempt_id
         JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
         WHERE s.id=?1 AND p.session_id=s.id AND p.attempt_id=rg.attempt_id
           AND p.role=rg.role AND p.role=?2
           AND p.state IN ('running','awaiting_resume')
           AND a.state IN ('running','awaiting_publication')
           AND permit.attempt_id=rg.attempt_id AND permit.role=rg.role
           AND permit.purpose='runtime_probe' AND permit.state='issued'
           AND permit.setup_operation_id=attempt.setup_operation_id
           AND NOT EXISTS(
             SELECT 1 FROM trip_runtime_admissions newer
             JOIN trip_runtime_probes newer_probe ON newer_probe.admission_id=newer.id
             WHERE newer.rowid>a.rowid AND newer.project_id=a.project_id
               AND newer.task_id IS a.task_id AND newer_probe.role=p.role
               AND newer.state IN ('authorized','running','awaiting_publication','ready')
               AND newer_probe.state IN ('authorized','running','awaiting_resume','evidence_recorded','current','published')
           )",
            params![session_id, launch.role.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("runtime probe resume authority is stale, superseded, consumed, or mismatched")
        })?;
    require_runtime_probe_session_policy(connection, session_id, launch.role, Some(launch))?;
    match validate_current_runtime_admission_scope(connection, runtime, &admission_id)? {
        RuntimeScopeValidation::Current(_) => Ok(true),
        RuntimeScopeValidation::Drift(reason) => Err(anyhow!(reason)),
    }
}

pub(crate) fn refresh_runtime_admission_state(
    connection: &Connection,
    admission_id: &str,
    now: &str,
) -> Result<String> {
    let states = {
        let mut statement = connection
            .prepare("SELECT state FROM trip_runtime_probes WHERE admission_id=?1 ORDER BY role")?;
        let rows = statement
            .query_map(params![admission_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if states.is_empty() {
        bail!("runtime admission has no role probes")
    }
    let all_current = states
        .iter()
        .all(|state| matches!(state.as_str(), "current" | "published"));
    let any = |candidates: &[&str]| {
        states
            .iter()
            .any(|state| candidates.contains(&state.as_str()))
    };
    let state = if all_current {
        "ready"
    } else if any(&["running", "awaiting_resume"]) {
        "running"
    } else if any(&["authorized"]) {
        "authorized"
    } else if any(&["evidence_recorded"]) {
        "awaiting_publication"
    } else if any(&["pending_approval"]) {
        "pending_approval"
    } else if any(&["stale"]) {
        "stale"
    } else {
        "failed"
    };
    let failure_reason: Option<String> = if matches!(state, "failed" | "stale") {
        connection
            .query_row(
                "SELECT failure_reason FROM trip_runtime_probes WHERE admission_id=?1 AND state IN ('failed','stale') AND failure_reason IS NOT NULL ORDER BY updated_at DESC LIMIT 1",
                params![admission_id],
                |row| row.get(0),
            )
            .optional()?
    } else {
        None
    };
    connection.execute(
        "UPDATE trip_runtime_admissions SET state=?1,failure_reason=?2,updated_at=?3 WHERE id=?4",
        params![state, failure_reason, now, admission_id],
    )?;
    Ok(state.to_owned())
}

fn mark_runtime_publication_stale(store: &Store, admission_id: &str, role: RoleKind) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE trip_runtime_probes SET state='stale',failure_reason='runtime publication scope drifted',updated_at=?1 WHERE admission_id=?2 AND role=?3 AND state='evidence_recorded'",
        params![now, admission_id, role.to_string()],
    )?;
    refresh_runtime_admission_state(&tx, admission_id, &now)?;
    tx.commit()?;
    Ok(())
}

fn mark_runtime_command_mismatch(store: &Store, admission_id: &str, role: RoleKind) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE trip_runtime_probes
         SET state='failed',failure_reason=?1,updated_at=?2
         WHERE admission_id=?3 AND role=?4 AND state='evidence_recorded'",
        params![
            "recorded ordinary runtime evidence permanently mismatches the exact authenticated native hook commands or count; prepare corrected runtime verification for a fresh scope",
            now,
            admission_id,
            role.to_string()
        ],
    )?;
    if changed == 1 {
        refresh_runtime_admission_state(&tx, admission_id, &now)?;
    }
    tx.commit()?;
    Ok(())
}

fn publish_runtime_proof(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    admission_id: &str,
    role: RoleKind,
) -> Result<OperationResult> {
    let publication_scope = {
        let connection = store.lock()?;
        match validate_current_runtime_admission_scope(&connection, runtime, admission_id) {
            Ok(RuntimeScopeValidation::Current(scope)) => scope,
            Ok(RuntimeScopeValidation::Drift(reason)) => {
                drop(connection);
                mark_runtime_publication_stale(store, admission_id, role)?;
                return Err(anyhow!(reason));
            }
            Err(error) => return Err(error.context("runtime publication scope could not be inspected; evidence remains recorded and publication may be retried after the unchanged environment is restored")),
        }
    };
    let (
        session,
        nonce,
        workspace,
        fixture_root,
        service_sentinel,
        control_socket,
        cmux_socket_path,
        native_id,
        provider_name,
        observation,
        capability_key,
        role_generation_id,
    ): (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        serde_json::Value,
        String,
        String,
    ) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT p.session_id,p.nonce,p.workspace_path,p.fixture_root,p.service_sentinel_path,p.control_socket_path,p.cmux_socket_path,s.native_session_id,s.provider,
                    json_extract(rr.metadata_json,'$.validation_observation'),p.capability_key,rg.id
             FROM trip_runtime_probes p JOIN sessions s ON s.id=p.session_id
             JOIN role_generations rg ON rg.id=s.role_generation_id AND rg.role=p.role
             JOIN role_results rr ON rr.session_id=s.id AND rr.role_generation_id=rg.id AND rr.outcome='capability_observed'
             WHERE p.admission_id=?1 AND p.role=?2 AND p.state='evidence_recorded'
               AND s.status='exited' AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
             ORDER BY rr.created_at DESC LIMIT 1",
            params![admission_id,role.to_string()],
            |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(9)?).unwrap_or_default(),row.get(10)?,row.get(11)?))
        ).optional()?.ok_or_else(||anyhow!("ordinary proof publication requires exact recorded evidence and a quiescent exited native session"))?
    };
    let provider: Provider = provider_name
        .parse()
        .map_err(|error: String| anyhow!(error))?;
    let outcomes = validate_runtime_outcomes(
        provider,
        role,
        &nonce,
        &workspace,
        &fixture_root,
        &service_sentinel,
        &control_socket,
        cmux_socket_path.as_deref(),
        observation
            .get("cmux_socket_stderr_hex")
            .and_then(serde_json::Value::as_str),
        observation.get("actual_outcomes"),
        true,
    )?;
    let command_consistency = {
        let connection = store.lock()?;
        runtime_command_consistency(
            &connection,
            &session,
            &role_generation_id,
            provider,
            role,
            &nonce,
            &workspace,
            &fixture_root,
            &service_sentinel,
            &control_socket,
            cmux_socket_path.as_deref(),
            &outcomes,
        )?
    };
    if matches!(
        command_consistency,
        RuntimeCommandConsistency::PermanentMismatch
    ) {
        mark_runtime_command_mismatch(store, admission_id, role)?;
        bail!("recorded ordinary runtime evidence permanently mismatches the exact authenticated native hook commands or count; prepare corrected runtime verification for a fresh scope")
    }
    let workspace = PathBuf::from(workspace);
    let permission_delivery_command = (role == RoleKind::Implementer
        && provider == Provider::Codex)
        .then(|| outcomes.permission_delivery_command.as_deref())
        .flatten();
    if role == RoleKind::Implementer
        && provider == Provider::Codex
        && (!outcomes.permission_delivery_denied || permission_delivery_command.is_none())
    {
        bail!("Codex Implementer ordinary evidence lacks the permission delivery denial")
    }
    let (
        workspace_write_observed,
        workspace_probe_relative_path,
        workspace_probe_sha256,
        original_repo_write_denied,
        service_data_write_denied,
        direct_write_denied,
        compound_denied,
        redirect_denied,
    ) = if role == RoleKind::Implementer {
        let relative = PathBuf::from(format!("runtime-write-{nonce}.txt"));
        let bytes = fs::read(workspace.join(&relative))
            .context("read ordinary implementer probe artifact")?;
        (
            outcomes.workspace_write_observed,
            Some(relative),
            Some(sha256(&bytes)),
            outcomes.original_repo_write_denied,
            outcomes.service_data_write_denied,
            false,
            false,
            false,
        )
    } else {
        (
            false,
            None,
            None,
            false,
            false,
            outcomes.direct_write_denied,
            outcomes.compound_write_denied,
            outcomes.redirect_write_denied,
        )
    };
    if observation.get("nonce").and_then(|value| value.as_str()) != Some(nonce.as_str())
        || Path::new(&service_sentinel).exists()
        || Path::new(&fixture_root)
            .join(format!("runtime-write-{nonce}.txt"))
            .exists()
        || (permission_delivery_command.is_some()
            && workspace
                .join(format!("runtime-permission-{nonce}.txt"))
                .exists())
    {
        bail!("ordinary proof publication evidence or protected sentinel state changed")
    }
    if cmux_socket_path.is_some() && !outcomes.cmux_socket_connection_denied {
        bail!("Claude ordinary proof lacks the exact observed cmux Unix-socket connection denial")
    }
    let proof = CapabilityProofInput {
        operation_id: format!("{operation_id}:capability-proof"),
        session_id: session.clone(),
        evidence_reference: format!("runtime-admission:{admission_id}:{}:{session}", role),
        direct_write_denied,
        compound_denied,
        redirect_denied,
        sentinel_relative_path: if role == RoleKind::Implementer {
            format!("runtime-write-{nonce}.txt")
        } else {
            format!("runtime-direct-{nonce}.txt")
        },
        native_resume_session_id: native_id,
        history_nonce: nonce.clone(),
        workspace_write_observed,
        workspace_probe_relative_path,
        workspace_probe_sha256,
        original_repo_write_denied,
        service_data_write_denied,
        human_control_denied: outcomes.human_control_denied,
        denied_sentinel_paths: if role == RoleKind::Implementer && provider == Provider::Codex {
            vec![
                PathBuf::from(service_sentinel.clone()),
                workspace.join(format!("runtime-permission-{nonce}.txt")),
            ]
        } else if role == RoleKind::Implementer {
            vec![PathBuf::from(service_sentinel.clone())]
        } else {
            vec![
                workspace.join(format!("runtime-direct-{nonce}.txt")),
                workspace.join(format!("runtime-compound-{nonce}.txt")),
                workspace.join(format!("runtime-redirect-{nonce}.txt")),
            ]
        },
        runtime_scope: Some(publication_scope.clone()),
    };
    let published = match store.record_capability_proof_with_guard(&proof, |transaction| {
        if matches!(
            runtime_command_consistency(
                transaction,
                &session,
                &role_generation_id,
                provider,
                role,
                &nonce,
                workspace.to_string_lossy().as_ref(),
                &fixture_root,
                &service_sentinel,
                &control_socket,
                cmux_socket_path.as_deref(),
                &outcomes,
            )?,
            RuntimeCommandConsistency::PermanentMismatch
        ) {
            bail!("runtime native command mismatch during publication")
        }
        if let Some(command) = permission_delivery_command.as_deref() {
            require_runtime_permission_delivery_denial(
                transaction,
                &session,
                &role_generation_id,
                workspace.to_string_lossy().as_ref(),
                command,
            )?;
        }
        require_runtime_probe_session_policy(transaction, &session, role, None)?;
        match validate_current_runtime_admission_scope(transaction, runtime, admission_id) {
            Ok(RuntimeScopeValidation::Current(current)) if current == publication_scope => Ok(()),
            Ok(RuntimeScopeValidation::Current(_)) | Ok(RuntimeScopeValidation::Drift(_)) => {
                bail!("runtime admission scope mismatch during publication")
            }
            Err(error) => Err(error.context(
                "runtime publication scope could not be inspected during transactional guard",
            )),
        }
    }) {
        Ok(value) => value,
        Err(error) => {
            let message = error.to_string();
            if message.contains("runtime admission scope mismatch during publication") {
                mark_runtime_publication_stale(store, admission_id, role)?;
            } else if message.contains("runtime native command mismatch during publication") {
                mark_runtime_command_mismatch(store, admission_id, role)?;
            }
            return Err(error);
        }
    };
    let now = Utc::now().to_rfc3339();
    let connection = store.lock()?;
    let capability_id:String=connection.query_row("SELECT id FROM capabilities WHERE config_hash=?1 AND role=?2 AND mode='interactive_pty' AND status='supported' AND proof_json!='{}'",params![capability_key,role.to_string()],|row|row.get(0))?;
    connection.execute("UPDATE trip_runtime_probes SET state='published',capability_id=?1,published_at=?2,updated_at=?2 WHERE admission_id=?3 AND role=?4 AND state='evidence_recorded'",params![capability_id,now,admission_id,role.to_string()])?;
    let remaining:i64=connection.query_row("SELECT COUNT(*) FROM trip_runtime_probes WHERE admission_id=?1 AND state NOT IN ('current','published')",params![admission_id],|row|row.get(0))?;
    let admission_state = refresh_runtime_admission_state(&connection, admission_id, &now)?;
    Ok(operation_result(
        operation_id,
        "runtime_admission",
        admission_id,
        None,
        if admission_state == "ready" {
            "ready"
        } else {
            "proof_published"
        },
        serde_json::json!({"admission_id":admission_id,"role":role,"capability_id":capability_id,"capability_key":capability_key,"remaining":remaining,"proof":published,"setup_receipt_promoted":false}),
    ))
}

fn authorize_probes(
    store: &Store,
    paths: &InstancePaths,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    setup_id: &str,
    expected_proposal_hash: &str,
) -> Result<OperationResult> {
    if let Some(recovered) =
        retry_probe_workspace(store, operation_id, setup_id, expected_proposal_hash)?
    {
        return Ok(recovered);
    }
    let (
        fixture_project,
        task_id,
        discovery_attempt,
        proposal_json,
        proposal_hash,
        root,
        identity,
        head,
    ) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT so.fixture_project_id,so.validation_task_id,so.discovery_attempt_id,so.proposal_json,so.proposal_hash,
                    p.repository_path,p.repository_identity,p.base_revision
             FROM trip_setup_operations so JOIN projects p ON p.id=so.fixture_project_id
             WHERE so.id=?1 AND so.state='draft'",
            params![setup_id],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,String>(7)?)),
        )?
    };
    if proposal_hash != expected_proposal_hash {
        bail!("setup proposal changed before probe authorization")
    }
    let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
    validate_setup_proposal(&proposal)?;
    let affected = {
        let connection = store.lock()?;
        let mut roles = Vec::new();
        for role in [
            RoleKind::Manager,
            RoleKind::Explorer,
            RoleKind::PlanReviewer,
            RoleKind::Implementer,
            RoleKind::CodeReviewer,
            RoleKind::FinalReviewer,
        ] {
            let name = role.to_string();
            let (profile_json,profile_hash):(String,String)=connection.query_row(
                "SELECT profile_json,profile_hash FROM trip_setup_profile_selections WHERE setup_operation_id=?1 AND role=?2 AND selection_state='selected'",
                params![setup_id,name],|row|Ok((row.get(0)?,row.get(1)?))
            )?;
            if effective_receipt_id(&connection, setup_id, &name, &profile_hash, runtime)?.is_none()
            {
                roles.push((
                    role,
                    serde_json::from_str::<RoleOverride>(&profile_json)?,
                    profile_hash,
                ));
            }
        }
        roles
    };
    if affected.is_empty() {
        let now = Utc::now().to_rfc3339();
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE trip_setup_operations SET state='preflight_complete',selected_profiles_hash=?1,probe_authorized_at=?2,updated_at=?2 WHERE id=?3 AND state='draft'",
            params![selected_profiles_hash(&proposal)?,now,setup_id],
        )?;
        tx.execute(
            "UPDATE trip_project_state SET reason='All six exact unchanged profile proofs were explicitly accepted for reuse; installation is not authorized',updated_at=?1 WHERE setup_operation_id=?2",
            params![now,setup_id],
        )?;
        tx.commit()?;
        return Ok(operation_result(
            operation_id,
            "trip_setup",
            setup_id,
            None,
            "preflight_complete",
            serde_json::json!({"probe_attempt_id":serde_json::Value::Null,"affected_proof_roles":[],"reused_exact_proofs":6}),
        ));
    }
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let workspace_id = uuid::Uuid::new_v4().to_string();
    let workspace_path = paths.artifacts.join("worktrees").join(&attempt_id);
    let now = Utc::now().to_rfc3339();
    {
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let discovery_active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id=?1 AND s.status NOT IN ('exited','launch_failed'))",
            params![discovery_attempt], |row| row.get(0)
        )?;
        if discovery_active {
            bail!("setup discovery manager must exit before profile probes are frozen")
        }
        tx.execute(
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at,scope_hash,configuration_hash,workflow_version,workflow_hash,setup_operation_id,upstream_source_hash,overlay_hash,legacy_migration_required,parent_attempt_id)
             VALUES(?1,?2,?3,'planning',?4,1,'held',?5,?5,?6,?7,?8,?9,?10,?11,?12,0,?13)",
            params![attempt_id, task_id, uuid::Uuid::new_v4().to_string(), head, now,
                json_hash(&serde_json::json!({"setup_operation_id":setup_id,"purpose":"profile_probes"}))?, proposal_hash,
                WORKFLOW_ID, crate::workflow_resources::workflow_hash(), setup_id, source_hash(), overlay_hash(), discovery_attempt],
        )?;
        tx.execute(
            "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?5,'{}','reserved',?6,?6)",
            params![workspace_id, attempt_id, identity, workspace_path.to_string_lossy(), head, now],
        )?;
        for (role, config, profile_hash) in &affected {
            let role_name = role.to_string();
            let revision: i64 = tx.query_row(
                "SELECT COALESCE(MAX(revision),0)+1 FROM role_settings WHERE task_id=?1 AND role=?2",
                params![task_id, role_name], |row| row.get(0)
            )?;
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            tx.execute(
                "INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![uuid::Uuid::new_v4().to_string(), task_id, role_name, revision, serde_json::to_string(config)?, now],
            )?;
            tx.execute(
                "INSERT INTO trip_setup_permits(id,setup_operation_id,attempt_id,fixture_project_id,fixture_repository_identity,role,profile_hash,settings_revision,purpose,approved_action,nonce,state,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'profile_probe','nonce_only_profile_invocation',?9,'issued',?10)",
                params![uuid::Uuid::new_v4().to_string(), setup_id, attempt_id, fixture_project, identity, role_name, profile_hash, revision, nonce, now],
            )?;
        }
        tx.execute(
            "UPDATE trip_setup_operations SET state='probing',probe_attempt_id=?1,selected_profiles_hash=?2,probe_authorized_at=?3,updated_at=?3 WHERE id=?4 AND state='draft'",
            params![attempt_id, selected_profiles_hash(&proposal)?, now, setup_id],
        )?;
        tx.execute(
            "UPDATE trip_project_state SET reason='Live profile probes are authorized but installation is not',updated_at=?1 WHERE setup_operation_id=?2",
            params![now, setup_id],
        )?;
        tx.commit()?;
    }
    if let Err(error) = prepare_setup_workspace(
        store,
        &attempt_id,
        Path::new(&root),
        &identity,
        &head,
        &workspace_path,
    ) {
        let detail = format!("setup probe workspace materialization failed: {error:#}");
        mark_setup_workspace_recovery(store, setup_id, &attempt_id, &detail)?;
        bail!("{detail}; retry authorize_setup_probes with the same setup operation and proposal hash")
    }
    Ok(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        None,
        "probes_authorized",
        serde_json::json!({
            "probe_attempt_id":attempt_id,"fixture_project_id":fixture_project,"validation_task_id":task_id,
            "profile_tuple_count":unique_profile_count(&proposal),"role_bound_probe_count":affected.len(),"total_role_preflight_count":1+DELEGATED_ROLES.len(),"target_files_copied":false,
            "affected_proof_roles":affected.iter().map(|(role,_,_)|role.to_string()).collect::<Vec<_>>(),
            "probe_contracts":probe_contracts(store,&attempt_id)?,"workspace_recovered":false
        }),
    ))
}

fn retry_discovery_workspace(
    store: &Store,
    operation_id: &str,
    project_id: &str,
    expected_version: i64,
    host_manager: &RoleOverride,
) -> Result<Option<OperationResult>> {
    let recovery: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
        String,
    )> =
        {
            let connection = store.lock()?;
            connection.query_row(
            "SELECT so.id,so.fixture_project_id,so.validation_task_id,so.discovery_attempt_id,
                    fixture.repository_path,fixture.repository_identity,fixture.base_revision,
                    w.path,selection.profile_json,target.version,so.target_inventory_json,so.state
             FROM trip_setup_operations so
             JOIN projects target ON target.id=so.project_id
             JOIN projects fixture ON fixture.id=so.fixture_project_id
             JOIN workspaces w ON w.attempt_id=so.discovery_attempt_id
             JOIN trip_setup_profile_selections selection
               ON selection.setup_operation_id=so.id AND selection.role='manager'
             WHERE so.project_id=?1 AND so.state IN ('discovery','workspace_recovery_required')
               AND so.probe_attempt_id IS NULL",
            params![project_id],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                    row.get(10)?, row.get(11)?,
                ))
            },
        ).optional()?
        };
    let Some((
        setup_id,
        fixture_project_id,
        task_id,
        attempt_id,
        fixture_root,
        fixture_identity,
        fixture_head,
        workspace,
        manager_json,
        project_version,
        inventory_json,
        setup_state,
    )) = recovery
    else {
        return Ok(None);
    };
    if project_version != expected_version {
        bail!("project version is stale")
    }
    if serde_json::to_value(host_manager)?
        != serde_json::from_str::<serde_json::Value>(&manager_json)?
    {
        bail!("recovering setup discovery requires the originally selected host manager")
    }
    if let Err(error) = prepare_setup_workspace(
        store,
        &attempt_id,
        Path::new(&fixture_root),
        &fixture_identity,
        &fixture_head,
        Path::new(&workspace),
    ) {
        let detail = format!("setup discovery workspace recovery failed: {error:#}");
        mark_setup_workspace_recovery(store, &setup_id, &attempt_id, &detail)?;
        bail!("{detail}; the setup remains recoverable by retrying begin_setup")
    }
    restore_setup_workspace_state(store, &setup_id, "discovery")?;
    Ok(Some(operation_result(
        operation_id,
        "trip_setup",
        &setup_id,
        Some(expected_version),
        "discovery_ready",
        serde_json::json!({
            "setup_operation_id":setup_id,
            "project_id":project_id,
            "fixture_project_id":fixture_project_id,
            "validation_task_id":task_id,
            "discovery_attempt_id":attempt_id,
            "target_inventory":serde_json::from_str::<serde_json::Value>(&inventory_json)?,
            "workspace_recovered":setup_state == "workspace_recovery_required",
            "workspace_reused":true
        }),
    )))
}

fn retry_probe_workspace(
    store: &Store,
    operation_id: &str,
    setup_id: &str,
    expected_proposal_hash: &str,
) -> Result<Option<OperationResult>> {
    let recovery: Option<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )> = {
        let connection = store.lock()?;
        connection
            .query_row(
                "SELECT so.fixture_project_id,so.validation_task_id,so.probe_attempt_id,
                    so.proposal_json,so.proposal_hash,fixture.repository_path,
                    fixture.repository_identity,fixture.base_revision,w.path,so.state
             FROM trip_setup_operations so
             JOIN projects fixture ON fixture.id=so.fixture_project_id
             JOIN workspaces w ON w.attempt_id=so.probe_attempt_id
             WHERE so.id=?1 AND so.state IN ('probing','workspace_recovery_required')
               AND so.probe_attempt_id IS NOT NULL",
                params![setup_id],
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
    };
    let Some((
        fixture_project_id,
        task_id,
        attempt_id,
        proposal_json,
        proposal_hash,
        fixture_root,
        fixture_identity,
        fixture_head,
        workspace,
        setup_state,
    )) = recovery
    else {
        return Ok(None);
    };
    if proposal_hash != expected_proposal_hash {
        bail!("setup proposal changed before probe workspace recovery")
    }
    let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
    validate_setup_proposal(&proposal)?;
    if let Err(error) = prepare_setup_workspace(
        store,
        &attempt_id,
        Path::new(&fixture_root),
        &fixture_identity,
        &fixture_head,
        Path::new(&workspace),
    ) {
        let detail = format!("setup probe workspace recovery failed: {error:#}");
        mark_setup_workspace_recovery(store, setup_id, &attempt_id, &detail)?;
        bail!("{detail}; the setup remains recoverable by retrying authorize_setup_probes")
    }
    restore_setup_workspace_state(store, setup_id, "probing")?;
    Ok(Some(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        None,
        "probes_authorized",
        serde_json::json!({
            "probe_attempt_id":attempt_id,
            "fixture_project_id":fixture_project_id,
            "validation_task_id":task_id,
            "profile_tuple_count":unique_profile_count(&proposal),
            "role_bound_probe_count":DELEGATED_ROLES.len(),
            "total_role_preflight_count":1+DELEGATED_ROLES.len(),
            "target_files_copied":false,
            "probe_contracts":probe_contracts(store,&attempt_id)?,
            "workspace_recovered":setup_state == "workspace_recovery_required",
            "workspace_reused":true
        }),
    )))
}

fn prepare_setup_workspace(
    store: &Store,
    attempt_id: &str,
    fixture_root: &Path,
    expected_identity: &str,
    expected_head: &str,
    workspace: &Path,
) -> Result<()> {
    let fixture = crate::workspace::inspect(fixture_root)?;
    if fixture.identity != expected_identity || fixture.head != expected_head {
        bail!("setup fixture identity changed before workspace materialization")
    }
    if workspace.exists() {
        let existing = crate::workspace::inspect(workspace)?;
        if existing.identity != expected_identity || existing.head != expected_head {
            bail!("existing setup workspace does not match its reserved fixture identity and base")
        }
    } else {
        crate::workspace::create_detached_worktree(&fixture, workspace, expected_head)?;
    }
    {
        let connection = store.lock()?;
        connection.execute(
            "UPDATE workspaces SET state='reserved',updated_at=?1
             WHERE attempt_id=?2 AND state='recovery_required'",
            params![Utc::now().to_rfc3339(), attempt_id],
        )?;
    }
    materialize_setup_package(store, attempt_id, workspace)?;
    let connection = store.lock()?;
    let ready: bool = connection.query_row(
        "SELECT state='ready' FROM workspaces WHERE attempt_id=?1",
        params![attempt_id],
        |row| row.get(0),
    )?;
    if !ready {
        bail!("setup workspace package materialization did not reach ready state")
    }
    verify_ready_workspace_policy(&connection, attempt_id)
}

fn mark_setup_workspace_recovery(
    store: &Store,
    setup_id: &str,
    attempt_id: &str,
    error: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE workspaces SET state='recovery_required',updated_at=?1
         WHERE attempt_id=?2 AND state!='ready'",
        params![now, attempt_id],
    )?;
    tx.execute(
        "UPDATE trip_setup_operations
         SET state='workspace_recovery_required',error=?1,updated_at=?2 WHERE id=?3",
        params![error, now, setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state
         SET readiness='recovery_required',reason=?1,updated_at=?2
         WHERE setup_operation_id=?3",
        params![error, now, setup_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn restore_setup_workspace_state(store: &Store, setup_id: &str, state: &str) -> Result<()> {
    if !matches!(state, "discovery" | "probing") {
        bail!("unsupported setup workspace recovery state")
    }
    let reason = if state == "discovery" {
        "Setup discovery workspace recovered; the selected manager may continue"
    } else {
        "Probe workspace recovered; authorized live profile probes may continue"
    };
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE trip_setup_operations SET state=?1,error=NULL,updated_at=?2
         WHERE id=?3 AND state='workspace_recovery_required'",
        params![state, now, setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state
         SET readiness='setup_in_progress',reason=?1,updated_at=?2
         WHERE setup_operation_id=?3",
        params![reason, now, setup_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn finalize_installation(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    setup_id: &str,
    expected_proposal_hash: &str,
) -> Result<OperationResult> {
    let (repository_path, proposal_json, proposal_hash) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT p.repository_path,so.proposal_json,so.proposal_hash FROM trip_setup_operations so
             JOIN projects p ON p.id=so.project_id WHERE so.id=?1 AND so.state IN ('probing','preflight_complete')",
            params![setup_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?))
        )?
    };
    if proposal_hash != expected_proposal_hash {
        bail!("installation finalization does not match the immutable setup proposal")
    }
    let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
    let root = crate::workspace::inspect(Path::new(&repository_path))?.root;
    let files = installation_files(store, setup_id, &root, &proposal, runtime)?;
    let source_bindings=files.iter().map(|file|serde_json::json!({
        "relative_path":file.relative,"source_sha256":file.source_hash,"preimage_sha256":file.preimage_hash
    })).collect::<Vec<_>>();
    let source_set_hash = json_hash(&source_bindings)?;
    let preimage_bindings = files
        .iter()
        .map(|file| {
            serde_json::json!({
                "relative_path":file.relative,"preimage_sha256":file.preimage_hash
            })
        })
        .collect::<Vec<_>>();
    let preimages_hash = json_hash(&preimage_bindings)?;
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current:String=tx.query_row(
        "SELECT proposal_hash FROM trip_setup_operations WHERE id=?1 AND state IN ('probing','preflight_complete')",
        params![setup_id],|row|row.get(0)
    )?;
    if current != proposal_hash {
        bail!("setup proposal changed during finalization")
    }
    require_all_setup_proofs(&tx, setup_id, runtime)?;
    let active:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id WHERE a.setup_operation_id=?1 AND s.status NOT IN ('exited','launch_failed'))",
        params![setup_id],|row|row.get(0)
    )?;
    if active {
        bail!("installation finalization waits for every setup probe session to exit")
    }
    for file in &files {
        tx.execute(
            "INSERT INTO trip_frozen_install_files(setup_operation_id,relative_path,source_hash,preimage_hash,source_bytes,preimage_bytes)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![setup_id,file.relative,file.source_hash,file.preimage_hash,file.bytes,file.preimage_bytes],
        )?;
    }
    tx.execute(
        "UPDATE trip_setup_operations SET state='finalized',approved_preimages_hash=?1,final_source_set_hash=?2,finalized_at=?3,updated_at=?3 WHERE id=?4",
        params![preimages_hash,source_set_hash,now,setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state SET reason='Exact post-preflight installation bytes are frozen and awaiting human approval',updated_at=?1 WHERE setup_operation_id=?2",
        params![now,setup_id],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        None,
        "installation_finalized",
        serde_json::json!({"proposal_hash":proposal_hash,"approved_preimages_hash":preimages_hash,
            "final_source_set_hash":source_set_hash,"files":source_bindings}),
    ))
}

fn authorize_installation(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    setup_id: &str,
    expected_proposal_hash: &str,
    approved_preimages_hash: &str,
    final_source_set_hash: &str,
) -> Result<OperationResult> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (proposal_hash, preimages_hash, frozen_hash,repository_path): (String, String, String,String) = tx.query_row(
        "SELECT so.proposal_hash,so.approved_preimages_hash,so.final_source_set_hash,p.repository_path
         FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id
         WHERE so.id=?1 AND so.state='finalized'",
        params![setup_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?,row.get(3)?)),
    )?;
    if proposal_hash != expected_proposal_hash
        || preimages_hash != approved_preimages_hash
        || frozen_hash != final_source_set_hash
    {
        bail!("installation approval does not match the exact proposal, destination preimages, and frozen source set")
    }
    require_all_setup_proofs(&tx, setup_id, runtime)?;
    let frozen_files = frozen_installation_files_from(&tx, setup_id)?;
    let frozen_bindings=frozen_files.iter().map(|file|serde_json::json!({
        "relative_path":file.relative,"source_sha256":file.source_hash,"preimage_sha256":file.preimage_hash
    })).collect::<Vec<_>>();
    if json_hash(&frozen_bindings)? != frozen_hash {
        bail!("installation approval source set no longer matches the frozen bytes")
    }
    let root = crate::workspace::inspect(Path::new(&repository_path))?.root;
    if json_hash(&current_preimage_bindings(&root, &frozen_files)?)? != preimages_hash {
        bail!("destination preimages changed before installation approval")
    }
    tx.execute(
        "UPDATE trip_setup_operations SET state='install_authorized',approved_source_set_hash=?1,install_authorized_at=?2,updated_at=?2 WHERE id=?3",
        params![frozen_hash,now, setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state SET reason='Installation authorized and awaiting staged apply',updated_at=?1 WHERE setup_operation_id=?2",
        params![now, setup_id],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        None,
        "installation_authorized",
        serde_json::json!({
            "proposal_hash":proposal_hash,"approved_preimages_hash":preimages_hash,"final_source_set_hash":frozen_hash
        }),
    ))
}

fn attempt_session_fingerprints(
    connection: &Connection,
    attempt_id: &str,
) -> Result<BTreeMap<String, String>> {
    let sessions = connection
        .prepare("SELECT id FROM sessions WHERE role_generation_id IN (SELECT id FROM role_generations WHERE attempt_id=?1) ORDER BY id")?
        .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    sessions
        .into_iter()
        .map(|session| {
            let (_, fingerprint) = crate::workflow::session_fingerprint(connection, &session)?
                .ok_or_else(|| anyhow!("migration session disappeared"))?;
            Ok((session, fingerprint))
        })
        .collect()
}

fn verify_attempt_migration_boundary(
    store: &Store,
    attempt_id: &str,
) -> Result<BTreeMap<String, String>> {
    let fingerprints = {
        let connection = store.lock()?;
        let fingerprints = attempt_session_fingerprints(&connection, attempt_id)?;
        require_attempt_migration_boundary(&connection, attempt_id, &fingerprints)?;
        fingerprints
    };
    for session in fingerprints.keys() {
        crate::recovery::verify_session_quiescent(store, session)
            .context("workflow migration requires positively verified session quiescence")?;
    }
    Ok(fingerprints)
}

fn require_attempt_migration_boundary(
    connection: &Connection,
    attempt_id: &str,
    fingerprints: &BTreeMap<String, String>,
) -> Result<()> {
    if attempt_session_fingerprints(connection, attempt_id)? != *fingerprints {
        bail!("attempt sessions changed during workflow migration quiescence verification")
    }
    for session in fingerprints.keys() {
        for (blocker, sql) in crate::workflow::EXITED_SESSION_FENCES {
            if connection.query_row(sql, params![session], |row| row.get::<_, bool>(0))? {
                bail!("workflow migration waits while {blocker}")
            }
        }
    }
    for (blocker, sql) in crate::workflow::ATTEMPT_PROCESS_OWNERSHIP_FENCES
        .iter()
        .chain(crate::workflow::TERMINAL_REPLAN_FENCES.iter())
        .chain(GUIDANCE_REAUTHORIZATION_FENCES.iter())
    {
        if *blocker == "the candidate is already frozen" {
            continue;
        }
        if connection.query_row(sql, params![attempt_id], |row| row.get::<_, bool>(0))? {
            bail!("workflow migration waits while {blocker}")
        }
    }
    if let Some(held) = crate::store::provider_failure_hold_in_attempt(connection, attempt_id)? {
        return Err(anyhow::Error::from(held)
            .context("workflow migration waits for the provider failure hold to end"));
    }
    Ok(())
}

fn project_activation_attempts(connection: &Connection, project_id: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT a.id FROM attempts a JOIN tasks t ON t.id=a.task_id
         WHERE t.project_id=?1 AND a.status NOT IN ('done','cancelled','failed') ORDER BY a.id",
    )?;
    let attempts = statement
        .query_map(params![project_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(attempts)
}

fn verify_project_activation_boundary(
    store: &Store,
    project_id: &str,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let attempts = {
        let connection = store.lock()?;
        project_activation_attempts(&connection, project_id)?
    };
    attempts
        .into_iter()
        .map(|attempt| {
            let fingerprints = verify_attempt_migration_boundary(store, &attempt)?;
            Ok((attempt, fingerprints))
        })
        .collect()
}

fn require_project_activation_boundary(
    connection: &Connection,
    project_id: &str,
    quiescence: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<()> {
    if project_activation_attempts(connection, project_id)?
        != quiescence.keys().cloned().collect::<Vec<_>>()
    {
        bail!("project attempts changed during activation quiescence verification")
    }
    for (attempt, fingerprints) in quiescence {
        let current_identity: bool = connection.query_row(
            "SELECT workflow_version=?2 AND workflow_hash=?3 AND upstream_source_hash=?4 AND overlay_hash=?5
             FROM attempts WHERE id=?1",
            params![attempt, WORKFLOW_ID, crate::workflow_resources::workflow_hash(), source_hash(), overlay_hash()],
            |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
        )?;
        if current_identity {
            bail!("configuration activation waits for current-workflow attempts to finish")
        }
        require_attempt_migration_boundary(connection, attempt, fingerprints)?;
    }
    Ok(())
}

fn mark_project_attempts_for_migration(
    connection: &Connection,
    project_id: &str,
    now: &str,
) -> Result<()> {
    connection.execute(
        "UPDATE attempts SET legacy_migration_required=1,updated_at=?1
         WHERE task_id IN (SELECT id FROM tasks WHERE project_id=?2) AND status NOT IN ('done','cancelled','failed')
           AND (workflow_version IS NOT ?3 OR workflow_hash IS NOT ?4 OR upstream_source_hash IS NOT ?5 OR overlay_hash IS NOT ?6)",
        params![now, project_id, WORKFLOW_ID, crate::workflow_resources::workflow_hash(), source_hash(), overlay_hash()],
    )?;
    Ok(())
}

fn apply_installation(
    store: &Store,
    paths: &InstancePaths,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    setup_id: &str,
    expected_proposal_hash: &str,
) -> Result<OperationResult> {
    let (
        project_id,
        repository_path,
        proposal_json,
        proposal_hash,
        approved_preimages_hash,
        final_source_set_hash,
        approved_source_set_hash,
    ) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT so.project_id,p.repository_path,so.proposal_json,so.proposal_hash,so.approved_preimages_hash,so.final_source_set_hash,so.approved_source_set_hash
             FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id
             WHERE so.id=?1 AND so.state='install_authorized'",
            params![setup_id],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?)),
        )?
    };
    if proposal_hash != expected_proposal_hash {
        bail!("installation proposal changed after authorization")
    }
    let quiescence = verify_project_activation_boundary(store, &project_id)?;
    {
        let connection = store.lock()?;
        require_project_activation_boundary(&connection, &project_id, &quiescence)?;
    }
    let root = crate::workspace::inspect(Path::new(&repository_path))?.root;
    let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
    let files = frozen_installation_files(store, setup_id)?;
    let source_bindings=files.iter().map(|file|serde_json::json!({
        "relative_path":file.relative,"source_sha256":file.source_hash,"preimage_sha256":file.preimage_hash
    })).collect::<Vec<_>>();
    if json_hash(&source_bindings)? != final_source_set_hash
        || final_source_set_hash != approved_source_set_hash
    {
        bail!("frozen installation bytes no longer match the human-approved source-set hash")
    }
    let observed_preimages = current_preimage_bindings(&root, &files)?;
    if json_hash(&observed_preimages)? != approved_preimages_hash {
        bail!("destination preimages changed after installation approval")
    }
    let staging = paths
        .artifacts
        .join("trip-setup")
        .join(setup_id)
        .join("staged");
    stage_files(&staging, &files)?;
    reserve_apply_journal(store, setup_id, &staging, &files)?;
    if let Err(error) = apply_journal(store, setup_id, &root) {
        mark_setup_recovery(store, setup_id, &format!("{error:#}"))?;
        return Err(error);
    }
    let installed = detect_installation(&root)?;
    if installed.kind != "compatible" {
        mark_setup_recovery(
            store,
            setup_id,
            &format!(
                "post-apply installation is {}: {}",
                installed.kind,
                installed.conflicts.join("; ")
            ),
        )?;
        bail!("post-apply installation did not match the approved pinned package")
    }
    let manifest_path = root.join(".agents/trip-explorer/manifest.json");
    let manifest_hash =
        hash_file(&manifest_path)?.ok_or_else(|| anyhow!("installed manifest is missing"))?;
    let config_revision = match activate_configuration(
        store,
        setup_id,
        &project_id,
        &root,
        &proposal,
        &manifest_hash,
        runtime,
    ) {
        Ok(revision) => revision,
        Err(error) => {
            mark_setup_recovery(store, setup_id, &format!("{error:#}"))?;
            return Err(error);
        }
    };
    Ok(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        None,
        "activated",
        serde_json::json!({
            "project_id":project_id,"config_revision_id":config_revision,"manifest_hash":manifest_hash,
            "workflow_id":WORKFLOW_ID,"upstream_source_hash":source_hash(),"overlay_hash":overlay_hash()
        }),
    ))
}

fn recover_installation(
    store: &Store,
    _paths: &InstancePaths,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    setup_id: &str,
) -> Result<OperationResult> {
    let (project_id, path, proposal_json): (String, String, String) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT so.project_id,p.repository_path,so.proposal_json FROM trip_setup_operations so
             JOIN projects p ON p.id=so.project_id WHERE so.id=?1 AND so.state='recovery_required'",
            params![setup_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
    };
    let root = crate::workspace::inspect(Path::new(&path))?.root;
    if let Err(error) = apply_journal(store, setup_id, &root) {
        mark_setup_recovery(store, setup_id, &format!("{error:#}"))?;
        return Err(error);
    }
    let installed = detect_installation(&root)?;
    if installed.kind != "compatible" {
        mark_setup_recovery(
            store,
            setup_id,
            &format!(
                "recovered installation is {}: {}",
                installed.kind,
                installed.conflicts.join("; ")
            ),
        )?;
        bail!("recovered installation does not match the approved pinned package")
    }
    let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
    let manifest_hash = hash_file(&root.join(".agents/trip-explorer/manifest.json"))?
        .ok_or_else(|| anyhow!("recovered manifest is missing"))?;
    let revision = match activate_configuration(
        store,
        setup_id,
        &project_id,
        &root,
        &proposal,
        &manifest_hash,
        runtime,
    ) {
        Ok(revision) => revision,
        Err(error) => {
            mark_setup_recovery(store, setup_id, &format!("{error:#}"))?;
            return Err(error);
        }
    };
    Ok(operation_result(
        operation_id,
        "trip_setup",
        setup_id,
        None,
        "activated",
        serde_json::json!({
            "project_id":project_id,"config_revision_id":revision,"recovered":true
        }),
    ))
}

fn adopt_installation(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    project_id: &str,
    expected_version: i64,
    configuration: &serde_json::Value,
) -> Result<OperationResult> {
    let observation = inspect_registered_project(store, project_id)?;
    if observation
        .get("observation")
        .and_then(|value| value.get("kind"))
        .and_then(|value| value.as_str())
        != Some("compatible")
    {
        bail!("only a valid pinned upstream installation can be adopted without a three-way migration proposal")
    }
    let proposal: SetupProposal = serde_json::from_value(configuration.clone())?;
    validate_setup_proposal(&proposal)?;
    let repository_path: String = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT repository_path FROM projects WHERE id=?1",
            params![project_id],
            |row| row.get(0),
        )?
    };
    let root = PathBuf::from(repository_path).canonicalize()?;
    validate_guidance_files(&root, &proposal)?;
    let manifest_path = root.join(".agents/trip-explorer/manifest.json");
    let manifest_hash = hash_file(&manifest_path)?
        .ok_or_else(|| anyhow!("compatible installation manifest is missing"))?;
    let installed_config: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".agents/trip-explorer/config.json"))?)?;
    let proposed_config = project_configuration(&proposal)?;
    if installed_config != proposed_config
        || serde_json::from_slice::<serde_json::Value>(&fs::read(
            root.join(".agents/trip-explorer/adapters.json"),
        )?)? != proposal.adapters
    {
        bail!(
            "adoption configuration must exactly match the existing validated config and adapters"
        )
    }
    let host_manager = proposal
        .host_manager
        .as_ref()
        .ok_or_else(|| anyhow!("adoption requires an explicit app host_manager profile"))?;
    validate_role_override(host_manager)?;
    let role_settings = project_role_settings(&proposal, host_manager)?;
    let proposal_hash = json_hash(&serde_json::to_value(&proposal)?)?;
    let now = Utc::now().to_rfc3339();
    let quiescence = verify_project_activation_boundary(store, project_id)?;
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND version=?2)",
        params![project_id, expected_version],
        |row| row.get(0),
    )?;
    if !current {
        bail!("project version is stale")
    }
    require_project_activation_boundary(&tx, project_id, &quiescence)?;
    let setup_id:String=tx.query_row(
        "SELECT id FROM trip_setup_operations WHERE project_id=?1 AND proposal_hash=?2 AND state IN ('probing','preflight_complete') ORDER BY created_at DESC LIMIT 1",
        params![project_id,proposal_hash],|row|row.get(0)
    ).optional()?.ok_or_else(||anyhow!("adoption requires the matching project-bound setup draft and approved live profile preflight"))?;
    require_all_setup_proofs(&tx, &setup_id, runtime)?;
    let active_probe_sessions:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
           JOIN attempts a ON a.id=rg.attempt_id WHERE a.setup_operation_id=?1 AND s.status NOT IN ('exited','launch_failed'))",
        params![setup_id],|row|row.get(0)
    )?;
    if active_probe_sessions {
        bail!("adoption waits for all setup preflight sessions to exit")
    }
    let revision = insert_config_revision(
        &tx,
        project_id,
        &proposal,
        "activated",
        &now,
        &setup_id,
        runtime,
    )?;
    tx.execute(
        "UPDATE trip_config_revisions SET state='superseded' WHERE project_id=?1 AND state='activated' AND id!=?2",
        params![project_id,revision],
    )?;
    tx.execute(
        "UPDATE trip_project_state SET readiness='ready',reason='Existing exact installation adopted after validation',active_config_revision_id=?1,
         workflow_id=?2,package_version=?3,upstream_source_hash=?4,overlay_hash=?5,manifest_hash=?6,activated_at=?7,updated_at=?7 WHERE project_id=?8",
        params![revision,WORKFLOW_ID,PACKAGE_VERSION,source_hash(),overlay_hash(),manifest_hash,now,project_id],
    )?;
    let prior_settings: String = tx.query_row(
        "SELECT settings_json FROM projects WHERE id=?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let mut settings: serde_json::Value = serde_json::from_str(&prior_settings)?;
    let object = settings
        .as_object_mut()
        .ok_or_else(|| anyhow!("project settings must be an object"))?;
    object.insert("roles".into(), role_settings);
    object.insert(
        "trip_config_revision_id".into(),
        serde_json::Value::String(revision.clone()),
    );
    tx.execute(
        "UPDATE trip_setup_operations SET state='activated',updated_at=?1 WHERE id=?2",
        params![now, setup_id],
    )?;
    tx.execute(
        "UPDATE trip_setup_operations SET state='superseded',updated_at=?1
         WHERE id=(SELECT supersedes_setup_operation_id FROM trip_setup_operations WHERE id=?2) AND state='activated'",
        params![now,setup_id],
    )?;
    tx.execute(
        "UPDATE projects SET settings_json=?1,version=version+1,updated_at=?2 WHERE id=?3",
        params![settings.to_string(), now, project_id],
    )?;
    mark_project_attempts_for_migration(&tx, project_id, &now)?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "project",
        project_id,
        Some(expected_version + 1),
        "trip_adopted",
        serde_json::json!({"config_revision_id":revision}),
    ))
}

fn verified_migration_guidance(
    connection: &Connection,
    attempt_id: &str,
    workspace: &Path,
    preserved: &serde_json::Value,
) -> Result<serde_json::Value> {
    let prior = &preserved["prior_policy"];
    let field = |name: &str| {
        prior[name]
            .as_str()
            .ok_or_else(|| anyhow!("migration receipt lacks prior {name}"))
    };
    let policy_json = field("policy_json")?;
    let source_bound: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN projects p ON p.id=t.project_id JOIN workspaces w ON w.attempt_id=a.id
         JOIN trip_config_revisions r ON r.project_id=p.id AND r.id=?5
         WHERE a.id=?1 AND w.id=?2 AND w.path=?3 AND w.repository_identity=?4
           AND p.repository_identity=w.repository_identity AND r.source_hash=?6 AND r.overlay_hash=?7
           AND r.configuration_hash=?8)",
        params![attempt_id,field("workspace_id")?,field("workspace_path")?,field("repository_identity")?,
            field("config_revision_id")?,field("upstream_source_hash")?,field("overlay_hash")?,field("configuration_hash")?], |row| row.get(0),
    )?;
    if !source_bound
        || field("attempt_id")? != attempt_id
        || Path::new(field("workspace_path")?) != workspace
        || field("policy_hash")? != sha256(policy_json.as_bytes())
        || preserved["prior_workflow"].as_str() != Some(field("workflow_id")?)
    {
        bail!("migration prior policy is not bound to this attempt, workspace and source workflow")
    }
    let policy = validate_policy_identity(
        policy_json,
        field("workflow_id")?,
        field("upstream_source_hash")?,
        field("overlay_hash")?,
    )?;
    let source_configuration: serde_json::Value = serde_json::from_str(field("config_json")?)?;
    if policy["kind"] != "activated_project"
        || policy["task_profiles"].as_array().is_none_or(|profiles| {
            profiles.iter().any(|profile| {
                profile["project_config_revision_id"].as_str()
                    != prior["config_revision_id"].as_str()
                    || profile["project_configuration_hash"].as_str()
                        != prior["configuration_hash"].as_str()
            })
        })
        || json_hash(&source_configuration)? != field("configuration_hash")?
    {
        bail!("migration prior guidance lacks an activated configuration binding")
    }
    let target_guidance: String = connection.query_row(
        "SELECT json_extract(current.config_json,'$.guidance')
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN trip_project_state s ON s.project_id=t.project_id JOIN trip_config_revisions current ON current.id=s.active_config_revision_id
         WHERE a.id=?1",
        params![attempt_id], |row| row.get(0),
    )?;
    let source_guidance: BTreeSet<String> =
        serde_json::from_value(source_configuration["guidance"].clone())?;
    let target_guidance: BTreeSet<String> = serde_json::from_str(&target_guidance)?;
    let mut files = serde_json::Map::new();
    for relative in source_guidance.intersection(&target_guidance) {
        if is_protected_workflow_artifact(relative) && relative != "AGENTS.md" {
            continue;
        }
        validate_relative(relative)?;
        let expected = policy["files"][relative]
            .as_str()
            .ok_or_else(|| anyhow!("prior approved guidance is not pinned: {relative}"))?;
        read_verified_regular(workspace, &workspace.join(relative), expected).with_context(
            || format!("prior approved guidance changed without authorization: {relative}"),
        )?;
        files.insert(relative.clone(), serde_json::json!(expected));
    }
    let mut history = Vec::new();
    if let Some(guidance) = policy.get("migration_guidance") {
        let frozen: Option<String> = connection.query_row(
            "SELECT json_extract(preserved_json,'$.guidance') FROM trip_legacy_migrations
             WHERE attempt_id=?1 AND to_workflow_id=?2 AND config_revision_id=?3
               AND target_workflow_hash=?4 AND target_source_hash=?5 AND target_overlay_hash=?6 AND target_manifest_hash=?7
               AND json_extract(preserved_json,'$.prior_policy.attempt_id')=?1
               AND json_extract(preserved_json,'$.prior_workflow')=from_workflow_id",
            params![attempt_id,field("workflow_id")?,field("config_revision_id")?,field("workflow_hash")?,
                field("upstream_source_hash")?,field("overlay_hash")?,policy["manifest_hash"].as_str()], |row| row.get(0),
        ).optional()?;
        if frozen
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?
            .as_ref()
            != Some(guidance)
        {
            bail!("prior migrated guidance lacks its exact authorized receipt")
        }
        history.extend(
            guidance["guidance_reauthorizations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|record| {
                    record["files"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|file| {
                            file["path"]
                                .as_str()
                                .is_some_and(|path| files.contains_key(path))
                        })
                })
                .cloned(),
        );
    }
    if let Some(records) = policy.get("guidance_reauthorizations") {
        let audits = connection.prepare("SELECT detail_json FROM audit_events WHERE entity_id=?1 AND actor_kind='human' AND event_code='attempt.guidance.reauthorized'")?
            .query_map(params![attempt_id], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for record in records
            .as_array()
            .ok_or_else(|| anyhow!("prior guidance authorization history is malformed"))?
        {
            let applicable = record["files"]
                .as_array()
                .ok_or_else(|| anyhow!("prior guidance authorization files are malformed"))?
                .iter()
                .any(|file| {
                    file["path"]
                        .as_str()
                        .is_some_and(|path| files.contains_key(path))
                });
            if !applicable {
                continue;
            }
            let verified = audits.iter().any(|audit| {
                let Ok(mut detail) = serde_json::from_str::<serde_json::Value>(audit) else {
                    return false;
                };
                let Some(object) = detail.as_object_mut() else {
                    return false;
                };
                object.remove("attempt_id");
                object.remove("policy_hash");
                detail == *record
            });
            if record["config_revision_id"].as_str() != prior["config_revision_id"].as_str()
                || !verified
            {
                bail!("prior guidance reauthorization is not verified for the source configuration")
            }
            history.push(record.clone());
        }
    }
    for (relative, expected) in &files {
        if let Some(approval) = history
            .iter()
            .rev()
            .flat_map(|record| record["files"].as_array().into_iter().flatten())
            .find(|file| file["path"].as_str() == Some(relative.as_str()))
        {
            if &approval["sha256"] != expected {
                bail!("prior guidance pin differs from its latest authorization: {relative}")
            }
        }
    }
    let guidance = serde_json::json!({"prior_policy_hash":field("policy_hash")?,"files":files,"guidance_reauthorizations":history});
    if preserved
        .get("guidance")
        .is_some_and(|frozen| frozen != &guidance)
    {
        bail!("migration guidance differs from its frozen authorization")
    }
    Ok(guidance)
}

pub(crate) fn initial_attempt_migration_blocker(
    connection: &Connection,
    attempt_id: &str,
    reviewed_plan_hash: &str,
) -> Result<Option<&'static str>> {
    let final_repair: bool = connection.query_row(
        "SELECT final_repair_round>0 OR EXISTS(SELECT 1 FROM final_repair_rechecks f WHERE f.attempt_id=a.id)
         FROM attempts a WHERE a.id=?1",
        params![attempt_id], |row| row.get(0),
    )?;
    if final_repair {
        return Ok(Some("This older attempt has prior final-repair authority. Keep it as history and start a fresh task under the current workflow."));
    }
    let reviewed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN trip_structured_plans sp
           ON sp.id=a.structured_plan_id AND sp.attempt_id=a.id AND sp.plan_hash=a.plan_hash
         WHERE a.id=?1 AND a.plan_hash=?2 AND trim(a.plan_hash)!=''
           AND (NULLIF(trim(sp.approved_at),'') IS NOT NULL OR EXISTS(
             SELECT 1 FROM review_requests review WHERE review.id=sp.review_request_id
               AND review.attempt_id=a.id AND review.review_kind='plan' AND review.candidate_hash=sp.plan_hash
               AND review.delivery_state='finished' AND review.verdict IN ('approved','request_changes','needs_rework'))))",
        params![attempt_id,reviewed_plan_hash], |row| row.get(0),
    )?;
    Ok((!reviewed).then_some("Migration requires this attempt's current reviewed structured-plan hash. Keep an attempt without a matching reviewed plan as history and start a fresh task under the current workflow."))
}

pub(crate) fn pending_attempt_migration(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<std::result::Result<(String, String), &'static str>>> {
    let pending: Option<(String, String, bool, Option<String>, Option<String>)> = connection.query_row(
        "SELECT receipt.reviewed_plan_hash,receipt.config_revision_id,COALESCE(
           s.readiness='ready' AND s.workflow_id=?2 AND s.package_version=?3
           AND s.upstream_source_hash=?5 AND s.overlay_hash=?6
           AND r.id=receipt.config_revision_id AND r.project_id=t.project_id AND r.state='activated'
           AND r.source_hash=?5 AND r.overlay_hash=?6
           AND receipt.target_workflow_hash=?4 AND receipt.target_source_hash=?5
           AND receipt.target_overlay_hash=?6 AND receipt.target_manifest_hash=s.manifest_hash
           AND a.workflow_version=?2 AND a.workflow_hash=?4 AND a.upstream_source_hash=?5 AND a.overlay_hash=?6
           AND a.legacy_migration_required=0 AND trim(receipt.reviewed_plan_hash)!=''
           AND json_extract(receipt.preserved_json,'$.prior_policy.attempt_id')=a.id
           AND json_extract(receipt.preserved_json,'$.prior_policy.workspace_id')=w.id
           AND json_extract(receipt.preserved_json,'$.prior_policy.workspace_path')=w.path
           AND json_extract(receipt.preserved_json,'$.prior_policy.repository_identity')=w.repository_identity
           AND p.repository_identity=w.repository_identity AND w.path!=p.repository_path
           AND json_extract(receipt.preserved_json,'$.prior_workflow')=receipt.from_workflow_id
           AND json_extract(receipt.preserved_json,'$.prior_policy.workflow_id')=receipt.from_workflow_id
           AND EXISTS(SELECT 1 FROM trip_structured_plans sp
             WHERE sp.id=json_extract(receipt.preserved_json,'$.reviewed_plan_id')
               AND sp.attempt_id=a.id AND sp.plan_hash=receipt.reviewed_plan_hash),0),
           json_extract(receipt.preserved_json,'$.prior_policy.policy_json'),
           json_extract(receipt.preserved_json,'$.prior_policy.policy_hash')
         FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN workspaces w ON w.attempt_id=a.id
         JOIN projects p ON p.id=t.project_id
         JOIN trip_legacy_migrations receipt ON receipt.attempt_id=a.id AND receipt.to_workflow_id=?2
         LEFT JOIN trip_project_state s ON s.project_id=t.project_id
         LEFT JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
         WHERE a.id=?1 AND w.state IN ('reserved','recovery_required')
           AND json_extract(receipt.preserved_json,'$.published_at') IS NULL
           AND a.status NOT IN ('done','cancelled','failed') AND t.lifecycle NOT IN ('done','cancelled')
           AND a.phase='planning' AND a.plan_hash IS NULL AND a.structured_plan_id IS NULL
           AND a.candidate_hash IS NULL AND a.plan_approved_at IS NULL AND a.human_acceptance_at IS NULL
           AND a.accepted_snapshot_id IS NULL",
        params![attempt_id,WORKFLOW_ID,PACKAGE_VERSION,crate::workflow_resources::workflow_hash(),source_hash(),overlay_hash()],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).optional()?;
    match pending {
        Some((plan, config, true, Some(policy), Some(hash))) if sha256(policy.as_bytes()) == hash => {
            Ok(Some(Ok((plan, config))))
        }
        Some(_) => Ok(Some(Err("Pending workflow migration no longer matches its frozen authorization and activated target. Review project setup before recovering it."))),
        None => Ok(None),
    }
}

fn migrate_attempt(
    store: &Store,
    runtime: &CapabilityRuntime,
    operation_id: &str,
    task_id: &str,
    attempt_id: &str,
    expected_task_version: i64,
    reviewed_plan_hash: &str,
    config_revision_id: &str,
) -> Result<OperationResult> {
    if reviewed_plan_hash.trim().is_empty() {
        bail!("migration requires the exact reviewed structured-plan hash")
    }
    let quiescence = verify_attempt_migration_boundary(store, attempt_id)?;
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (workflow, task_version, project_id, workspace): (String, i64, String, String) = tx.query_row(
        "SELECT a.workflow_version,t.version,t.project_id,w.path FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN workspaces w ON w.attempt_id=a.id
         WHERE a.id=?1 AND a.task_id=?2 AND a.status IN ('running','held','needs_input','needs_recovery')",
        params![attempt_id,task_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
    )?;
    if task_version != expected_task_version {
        bail!("task version is stale")
    }
    require_project_ready(&tx, &project_id)?;
    let config_valid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM trip_config_revisions r JOIN trip_project_state s ON s.active_config_revision_id=r.id
         WHERE r.id=?1 AND r.project_id=?2 AND s.project_id=?2 AND r.state='activated'
           AND r.source_hash=?3 AND r.overlay_hash=?4)",
        params![config_revision_id,project_id,source_hash(),overlay_hash()], |row| row.get(0)
    )?;
    if !config_valid {
        bail!("migration configuration is not the activated project revision")
    }
    require_attempt_migration_boundary(&tx, attempt_id, &quiescence)?;
    let manifest_hash: String = tx.query_row(
        "SELECT manifest_hash FROM trip_project_state WHERE project_id=?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let mut preserved;
    let existing:Option<(String,String,Option<String>,Option<String>,Option<String>,Option<String>,String,String)>=tx.query_row(
        "SELECT reviewed_plan_hash,config_revision_id,target_workflow_hash,target_source_hash,target_overlay_hash,target_manifest_hash,preserved_json,from_workflow_id
         FROM trip_legacy_migrations WHERE attempt_id=?1 AND to_workflow_id=?2",
        params![attempt_id,WORKFLOW_ID],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))
    ).optional()?;
    let recovering = existing.is_some();
    let migrated_version = if let Some((
        prior_plan,
        prior_config,
        prior_workflow_hash,
        prior_source,
        prior_overlay,
        prior_manifest,
        prior_preserved,
        from_workflow,
    )) = existing
    {
        if prior_plan != reviewed_plan_hash
            || prior_config != config_revision_id
            || prior_workflow_hash.as_deref()
                != Some(crate::workflow_resources::workflow_hash().as_str())
            || prior_source.as_deref() != Some(source_hash().as_str())
            || prior_overlay.as_deref() != Some(overlay_hash().as_str())
            || prior_manifest.as_deref() != Some(manifest_hash.as_str())
        {
            bail!("legacy migration recovery must retain its originally authorized plan, configuration and source-target identity")
        }
        match pending_attempt_migration(&tx, attempt_id)? {
            Some(Ok(_)) => {}
            Some(Err(reason)) => bail!("{reason}"),
            None => bail!("the authorized workflow migration has already been published or its recovery state changed"),
        }
        preserved = serde_json::from_str::<serde_json::Value>(&prior_preserved)?;
        if preserved["prior_workflow"].as_str() != Some(from_workflow.as_str()) {
            bail!("migration receipt source workflow changed")
        }
        verified_migration_guidance(&tx, attempt_id, Path::new(&workspace), &preserved)?;
        expected_task_version
    } else {
        let migration_required: bool = tx.query_row(
            "SELECT legacy_migration_required=1 AND (workflow_version IS NOT ?2 OR workflow_hash IS NOT ?3
               OR upstream_source_hash IS NOT ?4 OR overlay_hash IS NOT ?5) FROM attempts WHERE id=?1",
            params![attempt_id,WORKFLOW_ID,crate::workflow_resources::workflow_hash(),source_hash(),overlay_hash()], |row| row.get(0),
        )?;
        if !migration_required {
            bail!("attempt does not require an explicit workflow migration")
        }
        if let Some(reason) =
            initial_attempt_migration_blocker(&tx, attempt_id, reviewed_plan_hash)?
        {
            bail!("{reason}")
        }
        let prior_policy: String = tx.query_row(
            "SELECT json_object('attempt_id',a.id,'workspace_id',w.id,'workspace_path',w.path,
                'repository_identity',w.repository_identity,'workflow_id',a.workflow_version,
                'workflow_hash',a.workflow_hash,'upstream_source_hash',a.upstream_source_hash,'overlay_hash',a.overlay_hash,
                'config_revision_id',MIN(ap.project_config_revision_id),'configuration_hash',r.configuration_hash,
                'config_json',r.config_json,'policy_json',w.policy_json)
             FROM attempts a JOIN workspaces w ON w.attempt_id=a.id JOIN trip_attempt_profiles ap ON ap.attempt_id=a.id
             JOIN trip_config_revisions r ON r.id=ap.project_config_revision_id
             WHERE a.id=?1 GROUP BY a.id HAVING COUNT(*)=6 AND MIN(ap.project_config_revision_id)=MAX(ap.project_config_revision_id)",
            params![attempt_id], |row| row.get(0),
        )?;
        let mut prior_policy: serde_json::Value = serde_json::from_str(&prior_policy)?;
        prior_policy["policy_hash"] = serde_json::json!(sha256(
            prior_policy["policy_json"]
                .as_str()
                .ok_or_else(|| anyhow!("migration prior policy is missing"))?
                .as_bytes()
        ));
        let reviewed_plan_id: String = tx.query_row(
            "SELECT structured_plan_id FROM attempts WHERE id=?1",
            params![attempt_id],
            |row| row.get(0),
        )?;
        preserved = serde_json::json!({"review_budgets":"preserved","snapshots":"preserved","review_requests":"preserved",
            "prior_workflow":workflow,"prior_policy":prior_policy,"reviewed_plan_id":reviewed_plan_id});
        preserved["guidance"] =
            verified_migration_guidance(&tx, attempt_id, Path::new(&workspace), &preserved)?;
        tx.execute(
            "INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,config_revision_id,authorized_at,
               target_workflow_hash,target_source_hash,target_overlay_hash,target_manifest_hash)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![uuid::Uuid::new_v4().to_string(),attempt_id,workflow,WORKFLOW_ID,preserved.to_string(),reviewed_plan_hash,config_revision_id,now,
                crate::workflow_resources::workflow_hash(),source_hash(),overlay_hash(),manifest_hash],
        )?;
        let configurations = tx.prepare(
            "SELECT role,revision,config_json FROM role_settings r WHERE task_id=?1 AND revision=(
                SELECT MAX(revision) FROM role_settings WHERE task_id=r.task_id AND role=r.role) ORDER BY role",
        )?.query_map(params![task_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,String>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let configuration_revision = configurations
            .iter()
            .map(|(_, revision, _)| *revision)
            .max()
            .ok_or_else(|| anyhow!("migration requires effective task role settings"))?;
        tx.execute(
            "UPDATE attempts SET workflow_version=?1,workflow_hash=?2,upstream_source_hash=?3,overlay_hash=?4,
             configuration_hash=?5,configuration_revision=?6,legacy_migration_required=0,phase='planning',
             candidate_hash=NULL,accepted_snapshot_id=NULL,structured_plan_id=NULL,plan_hash=NULL,plan_approved_at=NULL,
             human_acceptance_at=NULL,selected_checks_revision=selected_checks_revision+1,
             manager_conformance_revision=manager_conformance_revision+1,updated_at=?7 WHERE id=?8",
            params![WORKFLOW_ID,crate::workflow_resources::workflow_hash(),source_hash(),overlay_hash(),
                json_hash(&configurations)?,configuration_revision,now,attempt_id],
        )?;
        tx.execute(
            "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2",
            params![now, task_id],
        )?;
        tx.execute("UPDATE role_credentials SET revoked_at=?1 WHERE role_generation_id IN (SELECT id FROM role_generations WHERE attempt_id=?2) AND revoked_at IS NULL",params![now,attempt_id])?;
        tx.execute(
            "UPDATE role_settings SET effective_generation_id=NULL WHERE task_id=?1",
            params![task_id],
        )?;
        retire_unmatchable_transition_proposals(&tx, attempt_id, "workflow_migration", &now)?;
        expected_task_version + 1
    };
    let changed=tx.execute("UPDATE workspaces SET state='recovery_required',updated_at=?1 WHERE attempt_id=?2 AND (state IN ('ready','recovery_required') OR (?3 AND state='reserved'))",params![now,attempt_id,recovering])?;
    if changed != 1 {
        bail!("legacy attempt workspace is not available for pinned policy materialization")
    }
    tx.execute(
        "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
        params![now, attempt_id],
    )?;
    tx.execute(
        "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2",
        params![now, task_id],
    )?;
    tx.execute(
        "DELETE FROM trip_attempt_profiles WHERE attempt_id=?1",
        params![attempt_id],
    )?;
    bind_attempt_profiles(
        &tx,
        task_id,
        attempt_id,
        Path::new(&workspace),
        runtime,
        &now,
    )?;
    tx.commit()?;
    drop(connection);
    if let Err(error) = materialize_project_policy(store, attempt_id, Path::new(&workspace)) {
        let failed = Utc::now().to_rfc3339();
        let connection = store.lock()?;
        connection.execute(
            "UPDATE workspaces SET state='recovery_required',updated_at=?1 WHERE attempt_id=?2",
            params![failed, attempt_id],
        )?;
        connection.execute(
            "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
            params![failed, attempt_id],
        )?;
        connection.execute(
            "UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id=?2",
            params![failed, task_id],
        )?;
        bail!("legacy migration policy materialization requires recovery: {error:#}")
    }
    let published_at = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let policy_json: String = tx.query_row(
        "SELECT policy_json FROM workspaces WHERE attempt_id=?1 AND state='recovery_required'",
        params![attempt_id],
        |row| row.get(0),
    )?;
    if !matches!(pending_attempt_migration(&tx, attempt_id)?, Some(Ok(_))) {
        bail!("workflow migration authorization changed before publication")
    }
    validate_materialized_policy(&policy_json)?;
    let workspace_changed = tx.execute(
        "UPDATE workspaces SET state='ready',updated_at=?1 WHERE attempt_id=?2 AND state='recovery_required'",
        params![published_at, attempt_id],
    )?;
    let attempt_changed = tx.execute(
        "UPDATE attempts SET status='needs_input',updated_at=?1 WHERE id=?2 AND status='needs_recovery'",
        params![published_at, attempt_id],
    )?;
    let task_changed = tx.execute(
        "UPDATE tasks SET attention='needs_input',updated_at=?1 WHERE id=?2 AND lifecycle NOT IN ('done','cancelled')",
        params![published_at, task_id],
    )?;
    if workspace_changed != 1 || attempt_changed != 1 || task_changed != 1 {
        bail!("legacy migration tuple changed before atomic policy publication")
    }
    tx.execute(
        "UPDATE trip_legacy_migrations SET preserved_json=json_set(preserved_json,'$.published_at',?1)
         WHERE attempt_id=?2 AND to_workflow_id=?3",
        params![published_at,attempt_id,WORKFLOW_ID],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "attempt",
        attempt_id,
        Some(migrated_version),
        "migration_review_required",
        serde_json::json!({"preserved":preserved,"policy_materialized":true}),
    ))
}

fn authorize_check(
    store: &Store,
    operation_id: &str,
    attempt_id: &str,
    check_id: &str,
    selected_revision: i64,
    exact_command_hash: &str,
    scope_hash: &str,
    decision: &str,
    lifetime: &str,
) -> Result<OperationResult> {
    store.require_execution_unheld("selected-check authorization")?;
    if !matches!(decision, "approved" | "denied") {
        bail!("check permission decision must be approved or denied")
    }
    if !matches!(lifetime, "once" | "reusable" | "family") {
        bail!("check permission lifetime must be once, reusable, or family")
    }
    if decision == "denied" && lifetime != "once" {
        bail!("a denied selected check is an exact current-scope decision")
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_attempt_ready(&tx, attempt_id, None)?;
    let (kind, executable, arguments, shell, cwd, candidate): (String,Option<String>,Option<String>,Option<String>,String,String) = tx.query_row(
        "SELECT c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd,a.candidate_hash FROM trip_verification_checks c
         JOIN trip_selected_checks s ON s.check_id=c.id AND s.attempt_id=?1 AND s.revision=?2
         JOIN attempts a ON a.id=s.attempt_id AND s.revision=a.selected_checks_revision
         WHERE c.id=?3 AND a.phase='checks' AND s.required=1 AND c.enabled=1",
        params![attempt_id,selected_revision,check_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))
    ).optional()?.ok_or_else(||anyhow!("check is not required and enabled in the current selection revision"))?;
    let observed = json_hash(
        &serde_json::json!({"kind":&kind,"executable":&executable,"arguments":&arguments,"shell":&shell,"cwd":&cwd}),
    )?;
    if observed != exact_command_hash {
        bail!("check command changed before authorization")
    }
    let observed_scope = json_hash(
        &serde_json::json!({"attempt_id":attempt_id,"candidate_hash":candidate,"check_id":check_id,"selected_revision":selected_revision,"cwd":cwd}),
    )?;
    if observed_scope != scope_hash {
        bail!("check authorization scope does not match the exact candidate and working directory")
    }
    let matching_rule_id = if decision == "approved" && lifetime == "family" {
        let family = crate::permissions::service_check_family(
            &tx,
            attempt_id,
            &kind,
            executable.as_deref(),
            &cwd,
        )?;
        let id = if let Some((id, _)) =
            crate::permissions::matching_service_check_rule(&tx, attempt_id, &family)?
        {
            id
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO trip_check_permission_rules(id,project_id,source,registered_root,repository_identity,
                 executable_kind,executable_value,display_family,created_by,created_at)
                 SELECT ?1,t.project_id,'service_check',?2,?3,?4,?5,?6,'authenticated_human',?7
                 FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?8",
                params![id,family.registered_root,family.repository_identity,family.executable_kind,
                    family.executable_value,family.display_family,now,attempt_id],
            )?;
            id
        };
        Some(id)
    } else {
        None
    };
    tx.execute(
        "INSERT INTO trip_check_authorizations(id,attempt_id,check_id,selected_revision,exact_command_hash,scope_hash,decision,lifetime,created_at,matching_rule_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![uuid::Uuid::new_v4().to_string(),attempt_id,check_id,selected_revision,
            exact_command_hash,scope_hash,decision,lifetime,now,matching_rule_id],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "check_authorization",
        check_id,
        None,
        decision,
        serde_json::json!({"attempt_id":attempt_id,"exact_command_hash":exact_command_hash,
            "lifetime":lifetime,"source":"service_check","matching_rule_id":matching_rule_id}),
    ))
}

fn revoke_check_permission_rule(
    store: &Store,
    operation_id: &str,
    rule_id: &str,
    expected_revision: i64,
) -> Result<OperationResult> {
    let now = Utc::now().to_rfc3339();
    let connection = store.lock()?;
    if connection.execute(
        "UPDATE trip_check_permission_rules SET revoked_at=?1,revoked_by='authenticated_human',
         revoke_reason='revoked from selected-check permission UI',revision=revision+1
         WHERE id=?2 AND source='service_check' AND revision=?3 AND revoked_at IS NULL",
        params![now, rule_id, expected_revision],
    )? != 1
    {
        bail!("service-check permission rule is stale, unknown, or already revoked")
    }
    Ok(operation_result(
        operation_id,
        "check_permission_rule",
        rule_id,
        Some(expected_revision + 1),
        "revoked",
        serde_json::json!({"source":"service_check","future_reservations_only":true}),
    ))
}

fn extend_review_budget(
    store: &Store,
    operation_id: &str,
    task_id: &str,
    attempt_id: &str,
    expected_version: i64,
    kind: &str,
    additional: i64,
) -> Result<OperationResult> {
    if !matches!(kind, "plan" | "code") {
        bail!("only ordinary plan and code review budgets can be explicitly extended")
    }
    if additional <= 0 {
        bail!("review extension must be positive")
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (initial, extension): (i64,i64) = tx.query_row(
        "SELECT initial_allowance,extension_allowance FROM review_budgets b JOIN attempts a ON a.id=b.attempt_id
         JOIN tasks t ON t.id=a.task_id WHERE b.attempt_id=?1 AND b.review_kind=?2 AND t.id=?3 AND t.version=?4",
        params![attempt_id,kind,task_id,expected_version], |row| Ok((row.get(0)?,row.get(1)?))
    )?;
    // The final-repair recheck is a separate one-shot lane
    // (`authorize_final_repair_recheck`), never an ordinary extension.
    if initial + extension + additional > 5 {
        bail!("ordinary review allowance cannot exceed five total calls")
    }
    tx.execute("UPDATE review_budgets SET extension_allowance=extension_allowance+?1,version=version+1 WHERE attempt_id=?2 AND review_kind=?3",params![additional,attempt_id,kind])?;
    tx.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
        params![now, task_id, expected_version],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "task",
        task_id,
        Some(expected_version + 1),
        "review_budget_extended",
        serde_json::json!({"review_kind":kind,"total_allowance":initial+extension+additional}),
    ))
}

fn authorize_implementation(
    store: &Store,
    operation_id: &str,
    task_id: &str,
    attempt_id: &str,
    expected_version: i64,
    plan_hash: &str,
) -> Result<OperationResult> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_attempt_ready(&tx, attempt_id, None)?;
    let plan_id: String = tx
        .query_row(
            "SELECT sp.id FROM trip_structured_plans sp JOIN attempts a ON a.id=sp.attempt_id
         JOIN tasks t ON t.id=a.task_id
         WHERE sp.attempt_id=?1 AND a.task_id=?2 AND t.version=?3 AND sp.plan_hash=?4
           AND sp.approved_at IS NOT NULL AND sp.implementation_authorized_at IS NULL
           AND a.phase='awaiting_implementation_authorization' AND a.plan_hash=sp.plan_hash",
            params![attempt_id, task_id, expected_version, plan_hash],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("implementation authorization is stale or exact plan approval is missing")
        })?;
    tx.execute(
        "UPDATE trip_structured_plans SET implementation_authorized_at=?1 WHERE id=?2 AND implementation_authorized_at IS NULL",
        params![now, plan_id],
    )?;
    tx.execute(
        "UPDATE attempts SET phase='implementation',updated_at=?1 WHERE id=?2 AND structured_plan_id=?3",
        params![now, attempt_id, plan_id],
    )?;
    tx.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
        params![now, task_id, expected_version],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "task",
        task_id,
        Some(expected_version + 1),
        "implementation_authorized",
        serde_json::json!({
            "attempt_id":attempt_id,"plan_hash":plan_hash,"structured_plan_id":plan_id
        }),
    ))
}

fn authorize_additional_explorer(
    store: &Store,
    operation_id: &str,
    task_id: &str,
    attempt_id: &str,
    expected_version: i64,
    stage: &str,
    justification: &str,
) -> Result<OperationResult> {
    if stage != "rescue" {
        bail!("only a second rescue Explorer can receive additional authorization")
    }
    let justification = justification.trim();
    if justification.is_empty() || justification.len() > 4_000 {
        bail!("additional Explorer authorization requires a bounded manager justification")
    }
    if contains_prohibited_key(&serde_json::json!({"justification":justification})) {
        bail!("Explorer authorization justification contains prohibited sensitive material")
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_attempt_ready(&tx, attempt_id, None)?;
    let eligible: bool = tx.query_row(
        "SELECT t.id=?2 AND t.version=?3 AND a.phase IN ('implementation','code_review','checks')
           AND (SELECT COUNT(*) FROM trip_explorer_decisions d WHERE d.attempt_id=a.id AND d.stage='rescue' AND d.activated=1)=1
         FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=?1",
        params![attempt_id,task_id,expected_version], |row| row.get(0),
    )?;
    if !eligible {
        bail!("additional Explorer authorization requires exactly one prior rescue and current task authority")
    }
    tx.execute(
        "INSERT INTO trip_explorer_extensions(id,attempt_id,stage,justification,authorized_at) VALUES(?1,?2,'rescue',?3,?4)",
        params![uuid::Uuid::new_v4().to_string(),attempt_id,justification,now],
    )?;
    tx.execute(
        "UPDATE tasks SET version=version+1,updated_at=?1 WHERE id=?2 AND version=?3",
        params![now, task_id, expected_version],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "task",
        task_id,
        Some(expected_version + 1),
        "additional_explorer_authorized",
        serde_json::json!({"attempt_id":attempt_id,"stage":"rescue","allowance":1}),
    ))
}

struct FinalRepairLedger<'a> {
    approved_code_request_id: &'a str,
    prior_candidate_hash: &'a str,
    final_request_id: &'a str,
    rejected_code_request_id: &'a str,
    reviewer_generation_id: &'a str,
}

/// Bindings are derived from the ledger and only compared with the caller's
/// reviewed values. Historical ordinary requests, results, verdicts and budget
/// are never relabeled or refunded: the receipt is a separate one-shot lane.
fn authorize_final_repair_recheck(
    store: &Store,
    operation_id: &str,
    request_hash: &str,
    task_id: &str,
    attempt_id: &str,
    expected_version: i64,
    reviewed: &FinalRepairLedger<'_>,
) -> Result<OperationResult> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let recorded: Option<(String, String, i64, String)> = tx
        .query_row(
            "SELECT operation_id,request_hash,authorized_task_version,detail_json
             FROM final_repair_rechecks WHERE attempt_id=?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((operation, hash, version, detail)) = recorded {
        if operation != operation_id || hash != request_hash {
            bail!("a final-repair recheck was already authorized for this attempt")
        }
        return Ok(operation_result(
            operation_id,
            "attempt",
            attempt_id,
            Some(version + 1),
            "final_repair_recheck_authorized",
            serde_json::from_str(&detail)?,
        ));
    }
    require_attempt_ready(&tx, attempt_id, None)?;
    let current: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT a.plan_hash,a.configuration_hash
             FROM tasks t JOIN attempts a ON a.task_id=t.id
             WHERE t.id=?1 AND a.id=?2 AND t.version=?3 AND t.archived_at IS NULL
               AND t.lifecycle IN ('in_progress','validation') AND t.attention='needs_input'
               AND a.status='running' AND a.phase='needs_input' AND a.final_repair_round=1
               AND a.candidate_hash IS NULL AND a.plan_hash IS NOT NULL
               AND a.plan_approved_at IS NOT NULL
               AND a.id=(SELECT latest.id FROM attempts latest WHERE latest.task_id=t.id
                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)",
            params![task_id, attempt_id, expected_version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((plan_hash, configuration_hash)) = current else {
        bail!("final-repair recheck recovery requires the task's current attempt at the given version, in its final repair round and held in needs_input")
    };
    retire_unmatchable_transition_proposals(
        &tx,
        attempt_id,
        "final_repair_recheck_recovery",
        &now,
    )?;
    for (reason, sql) in GUIDANCE_REAUTHORIZATION_FENCES {
        let blocked: bool = tx.query_row(sql, params![attempt_id], |row| row.get(0))?;
        if blocked {
            bail!("final-repair recheck recovery cannot proceed while {reason}")
        }
    }
    type Derived = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
    );
    let derived: Option<Derived> = tx
        .query_row(
            "SELECT approved.id,final_changes.id,final_changes.candidate_hash,rejected.id,
                    result.id,reviewer.id,session.id,rejected.candidate_hash,
                    rejected.settings_revision,profile.profile_hash
             FROM review_requests final_changes
             JOIN review_requests approved ON approved.id=(
               SELECT code.id FROM review_requests code
               WHERE code.attempt_id=final_changes.attempt_id AND code.review_kind='code'
                 AND code.delivery_state='finished' AND code.verdict='approved'
                 AND code.candidate_hash=final_changes.candidate_hash
                 AND code.rowid<final_changes.rowid
               ORDER BY code.rowid DESC LIMIT 1)
             JOIN review_requests rejected ON rejected.id=(
               SELECT latest.id FROM review_requests latest
               WHERE latest.attempt_id=final_changes.attempt_id ORDER BY latest.rowid DESC LIMIT 1)
             JOIN role_results result ON result.role_generation_id=rejected.role_generation_id
               AND result.session_id=rejected.session_id AND result.outcome='needs_rework'
               AND result.consumed_at IS NOT NULL
               AND json_extract(result.metadata_json,'$.review_request_id')=rejected.id
               AND json_extract(result.metadata_json,'$.review_kind')='code'
               AND json_extract(result.metadata_json,'$.candidate_hash')=rejected.candidate_hash
             JOIN role_generations reviewer ON reviewer.id=rejected.role_generation_id
               AND reviewer.attempt_id=final_changes.attempt_id AND reviewer.role='code_reviewer'
               AND reviewer.status NOT IN ('launch_reserved','running','stopping')
             JOIN sessions session ON session.id=rejected.session_id
               AND session.role_generation_id=reviewer.id AND session.status='exited'
             JOIN trip_attempt_profiles profile ON profile.attempt_id=final_changes.attempt_id
               AND profile.role='code_reviewer'
               AND profile.settings_revision=rejected.settings_revision
             WHERE final_changes.attempt_id=?1 AND final_changes.review_kind='final'
               AND final_changes.delivery_state='finished'
               AND final_changes.verdict='request_changes'
               AND (SELECT COUNT(*) FROM review_requests other
                 WHERE other.attempt_id=final_changes.attempt_id
                   AND other.review_kind='final' AND other.verdict='request_changes')=1
               AND rejected.review_kind='code' AND rejected.delivery_state='finished'
               AND rejected.verdict='needs_rework' AND rejected.budget_spent_at IS NOT NULL
               AND rejected.rowid>final_changes.rowid
               AND rejected.candidate_hash!=final_changes.candidate_hash
               AND NOT EXISTS(SELECT 1 FROM role_results duplicate
                 WHERE json_extract(duplicate.metadata_json,'$.review_request_id')=rejected.id
                   AND duplicate.id!=result.id)
               AND NOT EXISTS(SELECT 1 FROM role_generations newer
                 WHERE newer.attempt_id=reviewer.attempt_id AND newer.role=reviewer.role
                   AND newer.lane_id=reviewer.lane_id AND newer.generation>reviewer.generation)",
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
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        approved,
        final_request,
        prior_candidate,
        rejected,
        result,
        reviewer,
        session,
        repaired_candidate,
        settings_revision,
        profile_hash,
    )) = derived
    else {
        bail!("the ledger lacks an exact final-repair history: ordinary code approval, one final request_changes on that candidate, and a latest ordinary needs_rework of a repaired candidate with one consumed result from a quiescent, latest code reviewer under the pinned profile")
    };
    let (allowance, spent): (i64, i64) = tx.query_row(
        "SELECT initial_allowance+extension_allowance,spent FROM review_budgets
         WHERE attempt_id=?1 AND review_kind='code'",
        params![attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let detail = serde_json::json!({
        "attempt_id":attempt_id,"task_version":expected_version,"plan_hash":plan_hash,
        "configuration_hash":configuration_hash,
        "prior_code_approval":{"review_request_id":approved,"candidate_hash":prior_candidate},
        "final_request_changes":{"review_request_id":final_request,
            "candidate_hash":prior_candidate},
        "ordinary_needs_rework":{"review_request_id":rejected,"role_result_id":result,
            "candidate_hash":repaired_candidate},
        "retained_code_reviewer":{"role_generation_id":reviewer,"session_id":session,
            "settings_revision":settings_revision,"profile_hash":profile_hash},
        "ordinary_code_budget":{"allowance":allowance,"spent":spent},
        "final_repair_recheck":{"allowance":1,"spent":0},
    });
    // The removed ordinary exception admitted exactly one call past the
    // five-call maximum, which the rejected review spent. Any other shape is
    // not that historical error and receives no receipt.
    if allowance != 6 || spent != 6 {
        bail!("final-repair recheck recovery requires the historical ordinary code budget of exactly 6 allowed and 6 spent: {detail}")
    }
    if approved != reviewed.approved_code_request_id
        || prior_candidate != reviewed.prior_candidate_hash
        || final_request != reviewed.final_request_id
        || rejected != reviewed.rejected_code_request_id
        || reviewer != reviewed.reviewer_generation_id
    {
        bail!("the reviewed final-repair bindings differ from the ledger: {detail}")
    }
    // The final result is recorded only when exactly one authenticated
    // matching result exists, as migration 032 derived it for older receipts.
    let final_result: Option<String> = tx.query_row(
        "SELECT CASE WHEN COUNT(*)=1 THEN MIN(result.id) END
         FROM review_requests final
         JOIN role_results result ON result.role_generation_id=final.role_generation_id
           AND result.session_id=final.session_id
         JOIN role_generations verifier ON verifier.id=result.role_generation_id
           AND verifier.attempt_id=final.attempt_id AND verifier.role='final_verifier'
         WHERE final.id=?1 AND final.review_kind='final'
           AND result.outcome='request_changes' AND result.consumed_at IS NOT NULL
           AND json_extract(result.metadata_json,'$.review_request_id')=final.id
           AND json_extract(result.metadata_json,'$.review_kind')='final'
           AND json_extract(result.metadata_json,'$.candidate_hash')=final.candidate_hash",
        params![final_request],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO final_repair_rechecks(attempt_id,task_id,operation_id,request_hash,
           authorized_task_version,plan_hash,configuration_hash,approved_code_request_id,
           prior_candidate_hash,final_request_id,rejected_code_request_id,rejected_code_result_id,
           reviewer_generation_id,reviewer_session_id,reviewer_settings_revision,
           reviewer_profile_hash,detail_json,state,created_at,updated_at,
           provenance_kind,final_result_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,'authorized',?18,?18,
           'historical_sixth_review_recovery',?19)",
        params![
            attempt_id,
            task_id,
            operation_id,
            request_hash,
            expected_version,
            plan_hash,
            configuration_hash,
            approved,
            prior_candidate,
            final_request,
            rejected,
            result,
            reviewer,
            session,
            settings_revision,
            profile_hash,
            detail.to_string(),
            now,
            final_result
        ],
    )?;
    if tx.execute(
        "UPDATE attempts SET phase='implementation',updated_at=?1
         WHERE id=?2 AND phase='needs_input' AND final_repair_round=1 AND candidate_hash IS NULL",
        params![now, attempt_id],
    )? != 1
    {
        bail!("the attempt changed while the final-repair recheck was being authorized")
    }
    if tx.execute(
        "UPDATE tasks SET attention='none',version=version+1,updated_at=?1
         WHERE id=?2 AND version=?3 AND attention='needs_input'",
        params![now, task_id, expected_version],
    )? != 1
    {
        bail!("task version is stale")
    }
    tx.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'human','attempt.final_repair_recheck.authorized','attempt',?3,?4,?5)",
        params![uuid::Uuid::new_v4().to_string(), operation_id, attempt_id, detail.to_string(), now],
    )?;
    tx.commit()?;
    Ok(operation_result(
        operation_id,
        "attempt",
        attempt_id,
        Some(expected_version + 1),
        "final_repair_recheck_authorized",
        detail,
    ))
}

pub(crate) fn record_structured_plan(
    connection: &Connection,
    context: &RoleContext,
    report: &RoleResultReport,
) -> Result<Option<String>> {
    if context.role != RoleKind::Manager || report.outcome != "plan_ready" {
        return Ok(None);
    }
    let plan_text = report
        .metadata
        .get("plan")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("plan_ready requires metadata.plan"))?;
    let structured = report
        .metadata
        .get("structured_plan")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("plan_ready requires metadata.structured_plan"))?;
    if serde_json::to_vec(structured)?.len() > 256 * 1024 {
        bail!("structured plan exceeds the bounded size")
    }
    if contains_prohibited_key(structured) {
        bail!("structured plan cannot contain credentials, secrets, tokens, or hidden reasoning")
    }
    let required_objects = [
        "outcomes_scope",
        "ownership",
        "test_policy",
        "documentation",
        "explorer_disposition",
        "conformance",
    ];
    for field in required_objects {
        if !structured
            .get(field)
            .is_some_and(serde_json::Value::is_object)
        {
            bail!("structured plan requires object field {field}")
        }
    }
    for field in [
        "acceptance_criteria",
        "verification_matrix",
        "restrictions",
        "unresolved_decisions",
    ] {
        if !structured
            .get(field)
            .is_some_and(serde_json::Value::is_array)
        {
            bail!("structured plan requires array field {field}")
        }
    }
    if !matches!(
        structured
            .get("classification")
            .and_then(|value| value.as_str()),
        Some("bounded" | "broad" | "program_sized")
    ) {
        bail!("structured plan classification must be bounded, broad, or program_sized")
    }
    if !structured["unresolved_decisions"]
        .as_array()
        .is_some_and(Vec::is_empty)
    {
        bail!("plan_ready cannot advance while unresolved decisions remain")
    }
    parse_parallel_lane_ownership(&structured["ownership"])?;
    let (criteria_json, config_revision, config_json, selected_revision): (String, String, String, i64) = connection.query_row(
        "SELECT t.acceptance_criteria_json,s.active_config_revision_id,r.config_json,a.selected_checks_revision
         FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN trip_project_state s ON s.project_id=t.project_id
         JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
         WHERE a.id=?1 AND a.task_id=?2 AND s.readiness='ready' AND r.state='activated'",
        params![context.attempt_id, context.task_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let criteria: serde_json::Value = serde_json::from_str(&criteria_json)?;
    if structured.get("acceptance_criteria") != Some(&criteria) {
        bail!("structured plan acceptance_criteria must exactly match the task acceptance rows")
    }
    let config: serde_json::Value = serde_json::from_str(&config_json)?;
    let selected_coverage = structured
        .pointer("/test_policy/coverage")
        .and_then(|value| value.as_str());
    let configured_coverage = config
        .pointer("/testing/coverage")
        .and_then(|value| value.as_str());
    if selected_coverage.is_none() || selected_coverage != configured_coverage {
        bail!("structured plan test policy must match the activated project coverage policy")
    }
    if structured
        .get("config_revision_id")
        .and_then(|value| value.as_str())
        != Some(config_revision.as_str())
    {
        bail!("structured plan config_revision_id is not the activated project revision")
    }
    let explorer_id = structured
        .pointer("/explorer_disposition/decision_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("structured plan must bind the planning Explorer decision"))?;
    let explorer:(bool,Option<String>)=connection.query_row(
        "SELECT activated,outcome_json FROM trip_explorer_decisions WHERE id=?1 AND attempt_id=?2 AND stage='planning'",
        params![explorer_id, context.attempt_id], |row| Ok((row.get(0)?,row.get(1)?))
    ).optional()?.ok_or_else(||anyhow!("structured plan Explorer disposition does not match this attempt"))?;
    if structured
        .pointer("/explorer_disposition/activated")
        .and_then(|value| value.as_bool())
        != Some(explorer.0)
        || (explorer.0 && explorer.1.is_none())
    {
        bail!("structured plan Explorer disposition must match the activation decision and completed evidence outcome")
    }
    let matrix_ids = structured["verification_matrix"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            entry
                .get("check_id")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("verification_matrix entries require check_id"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let selected_ids = if selected_revision == 0 {
        BTreeSet::new()
    } else {
        let mut statement = connection.prepare(
            "SELECT check_id FROM trip_selected_checks WHERE attempt_id=?1 AND revision=?2 AND required=1"
        )?;
        let rows = statement
            .query_map(params![context.attempt_id, selected_revision], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<BTreeSet<_>>>()?;
        rows
    };
    if matrix_ids != selected_ids {
        bail!("structured plan verification_matrix must exactly match the current selected-check revision")
    }
    let plan_hash = sha256(plan_text.as_bytes());
    let id = uuid::Uuid::new_v4().to_string();
    connection.execute(
        "INSERT INTO trip_structured_plans(id,attempt_id,plan_hash,plan_json,workflow_id,profile_revision_id,criteria_hash,verification_hash,ownership_hash,conformance_hash,explorer_decision_id,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![id,context.attempt_id,plan_hash,structured.to_string(),WORKFLOW_ID,config_revision,json_hash(&criteria)?,json_hash(&structured["verification_matrix"])?,json_hash(&structured["ownership"])?,json_hash(&structured["conformance"])?,explorer_id,Utc::now().to_rfc3339()],
    )?;
    connection.execute(
        "UPDATE attempts SET structured_plan_id=?1 WHERE id=?2",
        params![id, context.attempt_id],
    )?;
    Ok(Some(plan_hash))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReviewedLaneOwnership {
    owned_paths: Vec<String>,
    shared_paths: Vec<String>,
    protected_paths: Vec<String>,
    dependencies: Vec<String>,
}

fn parse_parallel_lane_ownership(
    ownership: &serde_json::Value,
) -> Result<Option<BTreeMap<String, ReviewedLaneOwnership>>> {
    let Some(value) = ownership.get("lanes") else {
        return Ok(None);
    };
    let lanes = value
        .as_array()
        .ok_or_else(|| anyhow!("structured plan ownership.lanes must be an array"))?;
    if lanes.len() < 2 {
        bail!("structured plan parallel ownership requires at least two explicit lanes")
    }
    let mut reviewed = BTreeMap::new();
    for lane in lanes {
        let key = lane
            .get("lane_key")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("structured plan ownership lane requires lane_key"))?;
        if !valid_identifier(key) {
            bail!("structured plan lane_key must match lowercase [a-z0-9_]+")
        }
        let owned_paths = string_array(lane, "owned_paths")?;
        let shared_paths = string_array(lane, "shared_paths")?;
        let protected_paths = match lane.get("protected_paths") {
            Some(_) => string_array(lane, "protected_paths")?,
            None => Vec::new(),
        };
        let dependencies = string_array(lane, "dependencies")?;
        if owned_paths.is_empty() {
            bail!("structured plan lane {key} requires at least one owned path")
        }
        for (name, paths) in [
            ("owned_paths", &owned_paths),
            ("shared_paths", &shared_paths),
            ("protected_paths", &protected_paths),
            ("dependencies", &dependencies),
        ] {
            if paths.iter().collect::<BTreeSet<_>>().len() != paths.len() {
                bail!("structured plan lane {key} {name} must be unique")
            }
        }
        for path in owned_paths
            .iter()
            .chain(&shared_paths)
            .chain(&protected_paths)
        {
            validate_relative(path)?;
        }
        if owned_paths.iter().enumerate().any(|(index, left)| {
            owned_paths
                .iter()
                .skip(index + 1)
                .chain(&shared_paths)
                .chain(&protected_paths)
                .any(|right| scopes_overlap(left, right))
        }) {
            bail!("structured plan lane {key} has overlapping owned, shared, or protected scopes")
        }
        let lane = ReviewedLaneOwnership {
            owned_paths,
            shared_paths,
            protected_paths,
            dependencies,
        };
        if reviewed.insert(key.to_owned(), lane).is_some() {
            bail!("structured plan lane keys must be unique")
        }
    }
    for (key, lane) in &reviewed {
        if lane
            .dependencies
            .iter()
            .any(|dependency| dependency == key || !reviewed.contains_key(dependency))
        {
            bail!("structured plan lane {key} has an unknown or self dependency")
        }
    }
    let entries = reviewed.iter().collect::<Vec<_>>();
    for (index, (left_key, left)) in entries.iter().enumerate() {
        for (right_key, right) in entries.iter().skip(index + 1) {
            if left.owned_paths.iter().any(|left_path| {
                right
                    .owned_paths
                    .iter()
                    .chain(&right.shared_paths)
                    .chain(&right.protected_paths)
                    .any(|right_path| scopes_overlap(left_path, right_path))
            }) || right.owned_paths.iter().any(|right_path| {
                left.shared_paths
                    .iter()
                    .chain(&left.protected_paths)
                    .any(|left_path| scopes_overlap(right_path, left_path))
            }) {
                bail!("structured plan lane ownership is not disjoint between {left_key} and {right_key}")
            }
        }
    }
    let mut remaining = reviewed
        .iter()
        .map(|(key, lane)| {
            (
                key.clone(),
                lane.dependencies.iter().cloned().collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .filter_map(|(key, dependencies)| {
                dependencies
                    .iter()
                    .all(|dependency| !remaining.contains_key(dependency))
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            bail!("structured plan lane dependencies contain a cycle")
        }
        for key in ready {
            remaining.remove(&key);
        }
    }
    Ok(Some(reviewed))
}

fn approved_lane_ownership(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<BTreeMap<String, ReviewedLaneOwnership>>> {
    let plan_json: String = connection
        .query_row(
            "SELECT p.plan_json FROM attempts a JOIN trip_structured_plans p ON p.id=a.structured_plan_id
             WHERE a.id=?1 AND a.phase='implementation' AND a.plan_hash=p.plan_hash
               AND p.approved_at IS NOT NULL AND p.implementation_authorized_at IS NOT NULL",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("implementation requires the exact approved structured plan and human authorization"))?;
    let plan: serde_json::Value = serde_json::from_str(&plan_json)?;
    let ownership = plan
        .get("ownership")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("approved structured plan ownership is malformed"))?;
    parse_parallel_lane_ownership(ownership)
}

pub(crate) fn reviewed_parallel_lane_count(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<usize>> {
    Ok(approved_lane_ownership(connection, attempt_id)?.map(|lanes| lanes.len()))
}

pub(crate) fn require_configured_lanes_match_reviewed(
    connection: &Connection,
    attempt_id: &str,
) -> Result<()> {
    let reviewed = approved_lane_ownership(connection, attempt_id)?
        .ok_or_else(|| anyhow!("configured lanes are absent from the approved structured plan"))?;
    let mut statement = connection.prepare(
        "SELECT lane_key,owned_paths_json,shared_paths_json,protected_paths_json,dependencies_json
         FROM implementation_lanes WHERE attempt_id=?1 ORDER BY lane_key",
    )?;
    let lanes = statement
        .query_map(params![attempt_id], |row| {
            Ok(serde_json::json!({
                "lane_key":row.get::<_,String>(0)?,
                "owned_paths":serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(1)?).unwrap_or_default(),
                "shared_paths":serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(2)?).unwrap_or_default(),
                "protected_paths":serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(3)?).unwrap_or_default(),
                "dependencies":serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(4)?).unwrap_or_default(),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let configured = parse_parallel_lane_ownership(&serde_json::json!({"lanes":lanes}))
        .context("configured implementation lanes are malformed")?
        .ok_or_else(|| anyhow!("configured implementation lanes are malformed"))?;
    if configured != reviewed {
        bail!("configured implementation lanes do not exactly match approved plan ownership")
    }
    Ok(())
}

pub(crate) fn record_explorer_outcome(
    connection: &Connection,
    context: &RoleContext,
    report: &RoleResultReport,
) -> Result<()> {
    if context.role != RoleKind::Explorer || report.outcome == "capability_observed" {
        return Ok(());
    }
    let decision_id = report
        .metadata
        .get("explorer_decision_id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("Explorer reports require the exact explorer_decision_id"))?;
    let changed = connection.execute(
        "UPDATE trip_explorer_decisions SET role_generation_id=?1,outcome_json=?2
         WHERE id=?3 AND attempt_id=?4 AND activated=1 AND outcome_json IS NULL
           AND candidate_hash IS (SELECT candidate_hash FROM attempts WHERE id=?4)",
        params![context.role_generation_id, serde_json::json!({"outcome":report.outcome,"summary":report.summary,"evidence":report.evidence}).to_string(), decision_id, context.attempt_id],
    )?;
    if changed != 1 {
        bail!("Explorer result does not match one current activated decision")
    }
    Ok(())
}

fn validate_setup_proposal_summary(
    connection: &Connection,
    setup_id: &str,
    evidence: &serde_json::Value,
) -> Result<()> {
    const MAX_POLICY_TEXT_BYTES: usize = 64 * 1024;
    const MAX_GUIDANCE_PATHS: usize = 64;
    const MAX_GUIDANCE_PATH_BYTES: usize = 4 * 1024;
    const MAX_VERIFICATION_COMMANDS: usize = 64;
    const MAX_COMMAND_BYTES: usize = 8 * 1024;

    let value = evidence
        .get("setup_proposal_summary")
        .ok_or_else(|| anyhow!("setup discovery evidence requires setup_proposal_summary"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("setup discovery setup_proposal_summary must be a policy object"))?;
    if object.is_empty() || object.values().any(serde_json::Value::is_null) {
        bail!("setup discovery setup_proposal_summary must contain typed non-null policy fields")
    }
    let summary: SetupProposalSummary = serde_json::from_value(value.clone()).context(
        "setup discovery setup_proposal_summary contains malformed or unsupported fields",
    )?;
    let mut actionable = false;
    if let Some(project_name) = &summary.project_name {
        if project_name.trim().is_empty() || project_name.len() > 512 {
            bail!("setup discovery project_name must be non-blank and at most 512 bytes")
        }
        actionable = true;
    }
    if let Some(guidance) = &summary.guidance {
        if guidance.len() > MAX_GUIDANCE_PATHS {
            bail!("setup discovery guidance exceeds the 64-path boundary")
        }
        for path in guidance {
            if path.len() > MAX_GUIDANCE_PATH_BYTES {
                bail!("setup discovery guidance path exceeds the 4 KiB boundary")
            }
            validate_relative(path)?;
        }
        actionable = true;
    }
    if let Some(documentation) = &summary.documentation {
        if documentation.no_change_text.trim().is_empty()
            || documentation.no_change_text.len() > MAX_POLICY_TEXT_BYTES
        {
            bail!("setup discovery documentation text must be non-blank and at most 64 KiB")
        }
        actionable = true;
    }
    if let Some(verification) = &summary.verification {
        let raw_verification = value
            .get("verification")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| anyhow!("setup discovery verification must be an object"))?;
        if raw_verification.values().any(serde_json::Value::is_null) {
            bail!("setup discovery verification categories cannot be null")
        }
        for (category, commands) in [
            ("focused", verification.focused.as_ref()),
            ("broad", verification.broad.as_ref()),
            ("cleanup", verification.cleanup.as_ref()),
        ] {
            let Some(commands) = commands else {
                continue;
            };
            if commands.len() > MAX_VERIFICATION_COMMANDS {
                bail!("setup discovery verification.{category} exceeds the 64-command boundary")
            }
            for command in commands {
                if command.trim().is_empty()
                    || command.len() > MAX_COMMAND_BYTES
                    || command.contains('\n')
                    || command.contains('\r')
                {
                    bail!("setup discovery verification.{category} entries must be non-blank single-line exact shell strings no larger than 8 KiB")
                }
            }
            actionable = true;
        }
    }
    if let Some(agents_content) = &summary.agents_content {
        if agents_content.trim().is_empty() || agents_content.len() > MAX_POLICY_TEXT_BYTES {
            bail!("setup discovery agents_content must be non-blank and at most 64 KiB")
        }
        let repository: String = connection.query_row(
            "SELECT p.repository_path FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id WHERE so.id=?1",
            params![setup_id],
            |row| row.get(0),
        )?;
        let root = PathBuf::from(repository).canonicalize()?;
        let path = root.join("AGENTS.md");
        ensure_no_symlink_ancestry(&root, &path)?;
        let existing = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        if !existing.is_empty() && !agents_content.as_bytes().starts_with(&existing) {
            bail!("setup discovery agents_content must preserve the existing AGENTS.md bytes before proposed additions")
        }
        actionable = true;
    }
    if !actionable {
        bail!("setup discovery setup_proposal_summary must contain at least one actionable policy field")
    }
    Ok(())
}

#[derive(Default)]
struct RuntimeOutcomeFlags {
    workspace_write_observed: bool,
    original_repo_write_denied: bool,
    service_data_write_denied: bool,
    permission_delivery_denied: bool,
    permission_delivery_command: Option<String>,
    direct_write_denied: bool,
    compound_write_denied: bool,
    redirect_write_denied: bool,
    human_control_denied: bool,
    cmux_socket_connection_denied: bool,
    failure_categories: BTreeSet<String>,
    attempted_operations: BTreeSet<String>,
}

impl RuntimeOutcomeFlags {
    fn denied(&self, kind: &str) -> bool {
        match kind {
            "direct" => self.direct_write_denied,
            "compound" => self.compound_write_denied,
            "redirect" => self.redirect_write_denied,
            _ => false,
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn runtime_probe_commands(
    provider: Provider,
    role: RoleKind,
    nonce: &str,
    workspace: &str,
    fixture: &str,
    service_sentinel: &str,
    control_socket: &str,
    cmux_socket_path: Option<&str>,
) -> Result<Vec<(&'static str, String, &'static str, &'static str)>> {
    let write = format!("runtime-write-{nonce}.txt");
    let mut commands = if role == RoleKind::Implementer {
        vec![
            (
                "workspace_write",
                format!(
                    "/usr/bin/printf %s {} > {}",
                    shell_quote(nonce),
                    shell_quote(&format!("{workspace}/{write}"))
                ),
                "succeeded",
                "none",
            ),
            (
                "original_repo_write",
                format!(
                    "/usr/bin/touch -- {}",
                    shell_quote(&format!("{fixture}/{write}"))
                ),
                "denied",
                "os",
            ),
            (
                "service_data_write",
                format!("/usr/bin/touch -- {}", shell_quote(service_sentinel)),
                "denied",
                "os",
            ),
        ]
    } else {
        let direct = format!("runtime-direct-{nonce}.txt");
        let compound = format!("runtime-compound-{nonce}.txt");
        let redirect = format!("runtime-redirect-{nonce}.txt");
        vec![
            (
                "direct_write",
                format!(
                    "/usr/bin/touch -- {}",
                    shell_quote(&format!("{workspace}/{direct}"))
                ),
                "denied",
                "os",
            ),
            (
                "compound_write",
                format!(
                    "/bin/sh -lc {}",
                    shell_quote(&format!(
                        "/usr/bin/true ; /usr/bin/touch -- {}",
                        shell_quote(&format!("{workspace}/{compound}"))
                    ))
                ),
                "denied",
                "os",
            ),
            (
                "redirect_write",
                format!(
                    "/bin/sh -lc {}",
                    shell_quote(&format!(
                        "/usr/bin/printf x > {}",
                        shell_quote(&format!("{workspace}/{redirect}"))
                    ))
                ),
                "denied",
                "os",
            ),
        ]
    };
    if role == RoleKind::Implementer && provider == Provider::Codex {
        commands.push((
            "permission_delivery",
            format!(
                "/usr/bin/touch -- {}",
                shell_quote(&format!("{workspace}/runtime-permission-{nonce}.txt"))
            ),
            "denied",
            "provider",
        ));
    }
    commands.push((
        "human_control_read",
        format!("/bin/test -r {}", shell_quote(control_socket)),
        "denied",
        "os",
    ));
    if let Some(cmux_socket_path) = cmux_socket_path {
        let executable = std::env::current_exe()
            .context("resolve the exact LLMRelay executable for the Unix connection probe")?;
        if !executable.is_absolute() {
            bail!("Unix connection probe executable must be absolute")
        }
        commands.push((
            "cmux_socket_connect",
            format!(
                "{} unix-connect-probe --path {}",
                shell_quote(&executable.to_string_lossy()),
                shell_quote(cmux_socket_path)
            ),
            "denied",
            "os",
        ));
    }
    Ok(commands)
}

fn runtime_probe_command_policy(
    provider: Provider,
    role: RoleKind,
    nonce: &str,
    workspace: &str,
    fixture: &str,
    service_sentinel: &str,
    control_socket: &str,
    cmux_socket_path: Option<&str>,
) -> Result<crate::providers::RuntimeProbeCommandPolicy> {
    let commands = runtime_probe_commands(
        provider,
        role,
        nonce,
        workspace,
        fixture,
        service_sentinel,
        control_socket,
        cmux_socket_path,
    )?
    .into_iter()
    .map(
        |(operation, command, _, _)| crate::providers::RuntimeProbeCommand {
            operation: operation.to_owned(),
            command,
        },
    )
    .collect();
    let write_denials = if role == RoleKind::Implementer {
        vec![PathBuf::from(fixture), PathBuf::from(service_sentinel)]
    } else {
        vec![PathBuf::from(workspace)]
    };
    Ok(crate::providers::RuntimeProbeCommandPolicy {
        commands,
        read_denials: vec![PathBuf::from(control_socket)],
        write_denials,
    })
}

pub(crate) fn runtime_probe_launch_policy(
    store: &Store,
    attempt_id: &str,
    role: RoleKind,
    session_id: Option<&str>,
) -> Result<Option<crate::providers::RuntimeProbeCommandPolicy>> {
    let connection = store.lock()?;
    let probe: Option<(String, String, String, String, String, Option<String>, String)> =
        connection
            .query_row(
                "SELECT p.nonce,p.workspace_path,p.fixture_root,p.service_sentinel_path,p.control_socket_path,p.cmux_socket_path,p.launch_config_json
                 FROM trip_runtime_probes p
                 WHERE p.attempt_id=?1 AND p.role=?2
                   AND p.state IN ('authorized','running','awaiting_resume','evidence_recorded')
                   AND (?3 IS NULL OR p.session_id=?3)",
                params![attempt_id, role.to_string(), session_id],
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
            )
            .optional()?;
    let Some((
        nonce,
        workspace,
        fixture,
        service_sentinel,
        control_socket,
        cmux_socket,
        launch_json,
    )) = probe
    else {
        bail!("runtime probe has no current authoritative launch-policy record")
    };
    let frozen_config: RoleOverride = serde_json::from_str(&launch_json)?;
    if frozen_config.provider != Provider::Claude {
        return Ok(None);
    }
    Ok(Some(runtime_probe_command_policy(
        frozen_config.provider,
        role,
        &nonce,
        &workspace,
        &fixture,
        &service_sentinel,
        &control_socket,
        cmux_socket.as_deref(),
    )?))
}

pub(crate) fn require_runtime_probe_launch_policy(
    store: &Store,
    attempt_id: &str,
    role: RoleKind,
    launch: &LaunchConfig,
) -> Result<()> {
    let connection = store.lock()?;
    require_runtime_probe_launch_policy_in_connection(&connection, attempt_id, role, launch)
}

pub(crate) fn require_runtime_probe_launch_policy_in_connection(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
    launch: &LaunchConfig,
) -> Result<()> {
    let expected = runtime_probe_launch_policy_from_connection(connection, attempt_id, role, None)?;
    match expected {
        Some(policy) => {
            if launch.provider != Provider::Claude {
                bail!("runtime Claude command policy was prepared for a different provider")
            }
            let actual = launch
                .security_policy
                .get("runtime_probe_command_policy")
                .cloned()
                .ok_or_else(|| {
                    anyhow!("runtime Claude launch lacks its frozen exact command policy")
                })?;
            if serde_json::from_value::<crate::providers::RuntimeProbeCommandPolicy>(actual)?
                != policy
            {
                bail!("runtime Claude launch command policy differs from the authoritative frozen probe record")
            }
        }
        None if launch
            .security_policy
            .get("runtime_probe_command_policy")
            .is_some() =>
        {
            bail!("non-Claude runtime launch unexpectedly carries a Claude command policy")
        }
        None => {}
    }
    Ok(())
}

fn require_runtime_probe_session_policy(
    connection: &Connection,
    session_id: &str,
    role: RoleKind,
    current_launch: Option<&LaunchConfig>,
) -> Result<()> {
    let (attempt_id, launch_json, capability_key, capability_identity_json):
        (String, String, String, String) = connection.query_row(
        "SELECT rg.attempt_id,s.launch_config_json,s.capability_key,s.capability_identity_json FROM sessions s
         JOIN role_generations rg ON rg.id=s.role_generation_id
         WHERE s.id=?1 AND rg.role=?2 AND s.validation_cell='trip_runtime_probe'",
        params![session_id, role.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let capability_identity: CapabilityIdentity = serde_json::from_str(&capability_identity_json)?;
    if crate::providers::capability_identity_key(&capability_identity)? != capability_key {
        bail!("runtime session does not match its frozen prepared capability identity")
    }
    let launch: LaunchConfig = serde_json::from_str(&launch_json)?;
    let expected = runtime_probe_launch_policy_from_connection(
        connection,
        &attempt_id,
        role,
        Some(session_id),
    )?;
    match expected {
        Some(policy) => {
            let actual = launch
                .security_policy
                .get("runtime_probe_command_policy")
                .cloned()
                .ok_or_else(|| {
                    anyhow!("runtime Claude session lacks its frozen exact command policy")
                })?;
            if serde_json::from_value::<crate::providers::RuntimeProbeCommandPolicy>(actual)?
                != policy
            {
                bail!("runtime Claude session command policy differs from the authoritative frozen probe record")
            }
            if let Some(current) = current_launch {
                if current.security_policy.get("runtime_probe_command_policy")
                    != launch.security_policy.get("runtime_probe_command_policy")
                {
                    bail!("runtime Claude resume command policy differs from the frozen initial launch")
                }
            }
        }
        None if launch
            .security_policy
            .get("runtime_probe_command_policy")
            .is_some() =>
        {
            bail!("non-Claude runtime session unexpectedly carries a Claude command policy")
        }
        None => {}
    }
    Ok(())
}

fn runtime_probe_launch_policy_from_connection(
    connection: &Connection,
    attempt_id: &str,
    role: RoleKind,
    session_id: Option<&str>,
) -> Result<Option<crate::providers::RuntimeProbeCommandPolicy>> {
    let probe: Option<(String, String, String, String, String, Option<String>, String)> =
        connection
            .query_row(
                "SELECT p.nonce,p.workspace_path,p.fixture_root,p.service_sentinel_path,p.control_socket_path,p.cmux_socket_path,p.launch_config_json
                 FROM trip_runtime_probes p
                 WHERE p.attempt_id=?1 AND p.role=?2
                   AND p.state IN ('authorized','running','awaiting_resume','evidence_recorded')
                   AND (?3 IS NULL OR p.session_id=?3)",
                params![attempt_id, role.to_string(), session_id],
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
            )
            .optional()?;
    let Some((
        nonce,
        workspace,
        fixture,
        service_sentinel,
        control_socket,
        cmux_socket,
        launch_json,
    )) = probe
    else {
        bail!("runtime probe has no current authoritative launch-policy record")
    };
    let frozen_config: RoleOverride = serde_json::from_str(&launch_json)?;
    if frozen_config.provider != Provider::Claude {
        return Ok(None);
    }
    Ok(Some(runtime_probe_command_policy(
        frozen_config.provider,
        role,
        &nonce,
        &workspace,
        &fixture,
        &service_sentinel,
        &control_socket,
        cmux_socket.as_deref(),
    )?))
}

struct RuntimeHookCommand {
    command: Option<String>,
    role_context: bool,
    role_report: bool,
}

struct RuntimePermissionHook {
    command: Option<String>,
    cwd: Option<String>,
    input_digest: Option<String>,
}

struct RuntimeFirstInvocationHooks {
    commands: Vec<RuntimeHookCommand>,
    permission_requests: Vec<RuntimePermissionHook>,
}

fn bound_runtime_first_invocation_hooks(
    connection: &Connection,
    session_id: &str,
    role_generation_id: &str,
    role: RoleKind,
) -> Result<Option<RuntimeFirstInvocationHooks>> {
    let role_executable = std::env::current_exe()
        .context("resolve current service role executable for runtime hook binding")?;
    let identity: Option<(String, Option<String>, String, i64, i64)> = connection
        .query_row(
            "SELECT s.provider,s.native_session_id,s.hook_trust_state,
                    s.initial_hook_event_boundary_rowid,s.resume_count
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE s.id=?1 AND s.role_generation_id=?2 AND rg.role=?3",
            params![session_id, role_generation_id, role.to_string()],
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
    let Some((provider, Some(native_session_id), hook_trust, initial_boundary, resume_count)) =
        identity
    else {
        return Ok(None);
    };
    if native_session_id.trim().is_empty() || hook_trust != "observed_unverified" {
        return Ok(None);
    }
    let provider: Provider = provider.parse().map_err(|error: String| anyhow!(error))?;
    let upper_boundary = if role != RoleKind::FinalReviewer && resume_count > 0 {
        connection
            .query_row(
                "SELECT hook_event_boundary_rowid FROM resume_invocations
                 WHERE session_id=?1 AND resume_ordinal=1
                   AND hook_event_boundary_rowid IS NOT NULL",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
    } else {
        Some(connection.query_row(
            "SELECT COALESCE(MAX(rowid),0) FROM hook_events WHERE session_id=?1",
            params![session_id],
            |row| row.get(0),
        )?)
    };
    let Some(upper_boundary) = upper_boundary else {
        return Ok(None);
    };
    if upper_boundary <= initial_boundary {
        return Ok(None);
    }
    let prompt_boundary: Option<i64> = connection
        .query_row(
            "SELECT MIN(rowid) FROM hook_events
             WHERE session_id=?1 AND role_generation_id=?2 AND provider=?3
               AND event_name='UserPromptSubmit' AND native_session_id=?4
               AND provenance_state='managed_process_group_untrusted_payload'
               AND peer_pid>0 AND peer_process_group_id>0
               AND TRIM(peer_start_marker)!='' AND rowid>?5 AND rowid<=?6",
            params![
                session_id,
                role_generation_id,
                provider.to_string(),
                native_session_id,
                initial_boundary,
                upper_boundary
            ],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(prompt_boundary) = prompt_boundary else {
        return Ok(None);
    };
    let session_started: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM hook_events
         WHERE session_id=?1 AND role_generation_id=?2 AND provider=?3
           AND event_name='SessionStart' AND native_session_id=?4
           AND provenance_state='managed_process_group_untrusted_payload'
           AND peer_pid>0 AND peer_process_group_id>0
           AND TRIM(peer_start_marker)!='' AND rowid>?5 AND rowid<?6)",
        params![
            session_id,
            role_generation_id,
            provider.to_string(),
            native_session_id,
            initial_boundary,
            prompt_boundary
        ],
        |row| row.get(0),
    )?;
    if !session_started {
        return Ok(None);
    }
    let mut statement = connection.prepare(
        "SELECT event_name,payload_json FROM hook_events
         WHERE session_id=?1 AND role_generation_id=?2 AND provider=?3
           AND event_name IN ('PreToolUse','PermissionRequest') AND native_session_id=?4
           AND provenance_state='managed_process_group_untrusted_payload'
           AND peer_pid>0 AND peer_process_group_id>0
           AND TRIM(peer_start_marker)!='' AND rowid>?5 AND rowid<=?6
         ORDER BY rowid",
    )?;
    let rows = statement.query_map(
        params![
            session_id,
            role_generation_id,
            provider.to_string(),
            native_session_id,
            prompt_boundary,
            upper_boundary
        ],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut commands = Vec::new();
    let mut permission_requests = Vec::new();
    for row in rows {
        let (event_name, payload_json) = row?;
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload_json) else {
            if event_name == "PreToolUse" {
                commands.push(RuntimeHookCommand {
                    command: None,
                    role_context: false,
                    role_report: false,
                });
            } else {
                permission_requests.push(RuntimePermissionHook {
                    command: None,
                    cwd: None,
                    input_digest: None,
                });
            }
            continue;
        };
        let tool_name = payload
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let tool_input = payload
            .get("tool_input")
            .unwrap_or(&serde_json::Value::Null);
        if event_name == "PermissionRequest" {
            permission_requests.push(RuntimePermissionHook {
                command: crate::permissions::command_text(provider, tool_name, tool_input),
                cwd: payload
                    .get("cwd")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                input_digest: serde_json::to_vec(tool_input)
                    .ok()
                    .map(|input| sha256(&input)),
            });
            continue;
        }
        commands.push(RuntimeHookCommand {
            command: crate::permissions::command_text(provider, tool_name, tool_input),
            role_context: crate::permissions::is_direct_role_command(
                provider,
                tool_name,
                tool_input,
                &role_executable,
                "context",
            ),
            role_report: crate::permissions::is_direct_role_command(
                provider,
                tool_name,
                tool_input,
                &role_executable,
                "report",
            ),
        });
    }
    Ok(Some(RuntimeFirstInvocationHooks {
        commands,
        permission_requests,
    }))
}

fn require_runtime_permission_delivery_denial(
    connection: &Connection,
    session_id: &str,
    role_generation_id: &str,
    workspace: &str,
    command: &str,
) -> Result<()> {
    let (provider, native_session_id, policy_fingerprint): (String, Option<String>, String) =
        connection.query_row(
            "SELECT provider,native_session_id,capability_key FROM sessions
             WHERE id=?1 AND role_generation_id=?2",
            params![session_id, role_generation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let Some(native_session_id) = native_session_id.filter(|value| !value.trim().is_empty()) else {
        bail!("Codex Implementer permission_delivery lacks an exact native session binding")
    };
    if provider != Provider::Codex.to_string() {
        bail!("permission_delivery is only valid for the Codex Implementer")
    }
    let Some(hooks) = bound_runtime_first_invocation_hooks(
        connection,
        session_id,
        role_generation_id,
        RoleKind::Implementer,
    )?
    else {
        bail!("Codex Implementer permission_delivery lacks first-invocation hook evidence")
    };
    let matching = hooks
        .permission_requests
        .iter()
        .filter(|hook| {
            hook.command.as_deref() == Some(command) && hook.cwd.as_deref() == Some(workspace)
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        bail!("Codex Implementer permission_delivery lacks one exact PermissionRequest hook")
    }
    let input_digest = matching[0].input_digest.as_deref().ok_or_else(|| {
        anyhow!("Codex Implementer permission_delivery hook input is not serializable")
    })?;
    let (requests, delivered_denials): (i64, i64) = connection.query_row(
        "SELECT COUNT(*),COALESCE(SUM(state='denied' AND decision_kind='deny'
                AND decision_actor='authenticated_human' AND delivery_state='delivered'
                AND delivered_at IS NOT NULL AND consumed_at IS NOT NULL),0) FROM permission_requests
         WHERE provider='codex' AND session_id=?1 AND role_generation_id=?2
           AND native_session_id=?3 AND policy_fingerprint=?4 AND cwd=?5
           AND command_display=?6 AND input_digest=?7",
        params![session_id, role_generation_id, native_session_id, policy_fingerprint, workspace, command, input_digest],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let approved: i64 = connection.query_row(
        "SELECT COUNT(*) FROM permission_requests
         WHERE session_id=?1 AND role_generation_id=?2
           AND state IN ('approved_once','approved_rule')",
        params![session_id, role_generation_id],
        |row| row.get(0),
    )?;
    if requests != 1 || delivered_denials != 1 || approved != 0 {
        bail!("Codex Implementer permission_delivery lacks one exact delivered human denial")
    }
    Ok(())
}

enum RuntimeCommandConsistency {
    Exact,
    PermanentMismatch,
}

fn runtime_command_consistency(
    connection: &Connection,
    session_id: &str,
    role_generation_id: &str,
    provider: Provider,
    role: RoleKind,
    nonce: &str,
    workspace: &str,
    fixture: &str,
    service_sentinel: &str,
    control_socket: &str,
    cmux_socket_path: Option<&str>,
    outcomes: &RuntimeOutcomeFlags,
) -> Result<RuntimeCommandConsistency> {
    let expected = runtime_probe_commands(
        provider,
        role,
        nonce,
        workspace,
        fixture,
        service_sentinel,
        control_socket,
        cmux_socket_path,
    )?;
    let Some(hooks) =
        bound_runtime_first_invocation_hooks(connection, session_id, role_generation_id, role)?
    else {
        return Ok(RuntimeCommandConsistency::PermanentMismatch);
    };
    let mut counts = expected
        .iter()
        .map(|(_, command, _, _)| (command.as_str(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut unexpected = false;
    let mut report_count = 0usize;
    for hook in hooks.commands {
        if hook.role_context {
            continue;
        }
        if hook.role_report {
            if role == RoleKind::FinalReviewer {
                report_count += 1;
            } else {
                unexpected = true;
            }
            continue;
        }
        match hook
            .command
            .as_deref()
            .and_then(|command| counts.get_mut(command))
        {
            Some(count) => *count += 1,
            None => unexpected = true,
        }
    }
    let counts_match = expected.iter().all(|(operation, command, _, _)| {
        let expected_count = usize::from(outcomes.attempted_operations.contains(*operation));
        counts.get(command.as_str()).copied() == Some(expected_count)
    });
    let report_count_matches = role != RoleKind::FinalReviewer || report_count == 1;
    Ok(if !unexpected && counts_match && report_count_matches {
        RuntimeCommandConsistency::Exact
    } else {
        RuntimeCommandConsistency::PermanentMismatch
    })
}

fn first_retained_invocation_attempted_report(
    connection: &Connection,
    session_id: &str,
    role_generation_id: &str,
    role: RoleKind,
) -> Result<bool> {
    Ok(
        bound_runtime_first_invocation_hooks(connection, session_id, role_generation_id, role)?
            .is_some_and(|hooks| hooks.commands.into_iter().any(|hook| hook.role_report)),
    )
}

pub(crate) fn settle_runtime_probe_exit(
    connection: &Connection,
    session_id: &str,
    now: &str,
) -> Result<bool> {
    let probe: Option<(String, String, String, i64, String, String)> = connection
        .query_row(
            "SELECT probe.admission_id,rg.id,rg.role,s.resume_count,permit.id,permit.setup_operation_id
             FROM sessions s
             JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN trip_runtime_probes probe ON probe.session_id=s.id
               AND probe.attempt_id=rg.attempt_id AND probe.role=rg.role
             JOIN trip_runtime_admissions admission ON admission.id=probe.admission_id
             JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
               AND permit.attempt_id=rg.attempt_id AND permit.role=rg.role
             WHERE s.id=?1 AND s.validation_cell='trip_runtime_probe'
               AND s.status='exited'
               AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)=1
               AND probe.state IN ('running','awaiting_resume')
               AND admission.state IN ('running','awaiting_publication')
               AND permit.purpose='runtime_probe' AND permit.state='issued'
               AND NOT EXISTS(SELECT 1 FROM role_results result WHERE result.session_id=s.id)",
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
    let Some((admission_id, role_generation_id, role, resume_count, permit_id, setup_id)) = probe
    else {
        return Ok(false);
    };
    let role_kind: RoleKind = role.parse().map_err(|error: String| anyhow!(error))?;
    let exact_current_resume: bool = connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM resume_invocations invocation
           JOIN sessions s ON s.id=invocation.session_id
           WHERE s.id=?1 AND invocation.resume_ordinal=s.resume_count
             AND invocation.transcript_epoch=s.transcript_epoch
             AND invocation.process_identity_json=s.process_identity_json
             AND invocation.state='exited')",
        params![session_id],
        |row| row.get(0),
    )?;
    let early_report = role_kind != RoleKind::FinalReviewer
        && resume_count == 0
        && !exact_current_resume
        && first_retained_invocation_attempted_report(
            connection,
            session_id,
            &role_generation_id,
            role_kind,
        )?;
    if role_kind != RoleKind::FinalReviewer
        && resume_count == 0
        && !exact_current_resume
        && !early_report
    {
        return Ok(false);
    }
    let terminal_reporting_invocation =
        (role_kind == RoleKind::FinalReviewer && resume_count == 0 && !exact_current_resume)
            || (role_kind != RoleKind::FinalReviewer && resume_count > 0 && exact_current_resume)
            || early_report;
    if !terminal_reporting_invocation {
        return Ok(false);
    }
    let reason = if early_report {
        "ordinary runtime probe attempted a forbidden report on the first retained invocation; prepare corrected runtime verification for a fresh scope"
    } else {
        "ordinary runtime probe terminal reporting invocation exited quiescently without an accepted role report; prepare corrected runtime verification for a fresh scope"
    };
    let changed = connection.execute(
        "UPDATE trip_runtime_probes SET state='failed',failure_reason=?1,updated_at=?2
         WHERE admission_id=?3 AND role=?4 AND session_id=?5
           AND state IN ('running','awaiting_resume')",
        params![reason, now, admission_id, role, session_id],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    let consumed = connection.execute(
        "UPDATE trip_setup_permits SET state='consumed',consumed_at=?1
         WHERE id=?2 AND setup_operation_id=?3 AND purpose='runtime_probe' AND state='issued'",
        params![now, permit_id, setup_id],
    )?;
    if consumed != 1 {
        bail!("runtime probe permit changed before terminal exit settlement")
    }
    refresh_runtime_admission_state(connection, &admission_id, now)?;
    Ok(true)
}

fn validate_runtime_outcomes(
    provider: Provider,
    role: RoleKind,
    nonce: &str,
    workspace: &str,
    fixture: &str,
    service_sentinel: &str,
    control_socket: &str,
    cmux_socket_path: Option<&str>,
    cmux_socket_stderr_hex: Option<&str>,
    outcomes: Option<&serde_json::Value>,
    require_pass: bool,
) -> Result<RuntimeOutcomeFlags> {
    let outcomes = outcomes
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("ordinary runtime evidence requires structured actual_outcomes"))?;
    let expected = runtime_probe_commands(
        provider,
        role,
        nonce,
        workspace,
        fixture,
        service_sentinel,
        control_socket,
        cmux_socket_path,
    )?;
    if outcomes.len() != expected.len() {
        bail!("actual_outcomes must contain exactly one entry for every prescribed command")
    }
    let mut flags = RuntimeOutcomeFlags::default();
    let mut seen = BTreeSet::new();
    for outcome in outcomes {
        let object = outcome
            .as_object()
            .ok_or_else(|| anyhow!("each runtime outcome must be a structured object"))?;
        let required = [
            "operation_id",
            "attempted",
            "exit_status",
            "result",
            "denial_source",
            "authentication_source",
        ];
        if !(object.len() == required.len() || object.len() == required.len() + 1)
            || required.iter().any(|key| !object.contains_key(*key))
            || object
                .keys()
                .any(|key| key != "command" && !required.contains(&key.as_str()))
        {
            bail!("runtime outcome fields must match the bounded observation schema")
        }
        let operation = object
            .get("operation_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("runtime outcome operation_id is required"))?;
        if !seen.insert(operation) {
            bail!("runtime outcome operation_id must occur exactly once")
        }
        let Some((_, exact_command, expected_result, expected_denial)) =
            expected.iter().find(|(id, _, _, _)| *id == operation)
        else {
            bail!("runtime outcome names an unprescribed operation")
        };
        if let Some(command) = object.get("command") {
            if command.as_str() != Some(exact_command.as_str()) {
                bail!("runtime outcome command differs from the exact prescribed command")
            }
        }
        let attempted = object
            .get("attempted")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| anyhow!("runtime outcome attempted state is required"))?;
        if attempted {
            flags.attempted_operations.insert(operation.to_owned());
        }
        let exit = object
            .get("exit_status")
            .and_then(serde_json::Value::as_i64);
        if object.get("exit_status").is_none()
            || (!object["exit_status"].is_null() && exit.is_none())
        {
            bail!("runtime outcome exit_status must be an integer or null")
        }
        let result = object
            .get("result")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("runtime outcome result is required"))?;
        let denial = object
            .get("denial_source")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("runtime outcome denial_source is required"))?;
        let authentication = object
            .get("authentication_source")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("runtime outcome authentication_source is required"))?;
        if ![
            "succeeded",
            "denied",
            "invocation_prevented",
            "refused",
            "unavailable",
            "unexpected",
        ]
        .contains(&result)
            || !["none", "os", "provider", "shell", "model", "unavailable"].contains(&denial)
            || !["native_session", "none"].contains(&authentication)
        {
            bail!("runtime outcome uses an unsupported result or observation source")
        }
        let consistent = if !attempted {
            exit.is_none()
                && authentication == "none"
                && matches!(
                    (result, denial),
                    ("refused", "model") | ("unavailable", "unavailable")
                )
        } else {
            matches!(authentication, "native_session" | "none")
                && match result {
                    "succeeded" => exit == Some(0) && denial == "none",
                    "denied" => match denial {
                        "os" => exit.is_some_and(|status| status != 0),
                        "provider" => exit.is_none(),
                        _ => false,
                    },
                    "invocation_prevented" => denial == "shell",
                    "refused" => denial == "model",
                    "unavailable" => denial == "unavailable",
                    "unexpected" => true,
                    _ => false,
                }
        };
        if !consistent {
            bail!("runtime outcome attempted, exit, result, and source fields are inconsistent")
        }
        if require_pass
            && (!attempted || authentication != "native_session" || result != *expected_result)
        {
            bail!("passed runtime evidence contains an unattempted, refused, unavailable, prevented, or contrary observation")
        }
        if require_pass && denial != *expected_denial {
            if *expected_denial == "os" {
                bail!("passed runtime evidence requires native OS denial for each restricted operation")
            }
            bail!("passed runtime evidence does not match the prescribed denial source")
        }
        if operation == "permission_delivery"
            && require_pass
            && !(attempted
                && exit.is_none()
                && result == "denied"
                && denial == "provider"
                && authentication == "native_session")
        {
            bail!("passed permission delivery evidence requires an authenticated provider denial before spawn")
        }
        if operation == "cmux_socket_connect"
            && require_pass
            && !(attempted
                && authentication == "native_session"
                && result == "denied"
                && denial == "os"
                && exit != Some(0))
        {
            bail!("passed cmux socket evidence requires an authenticated nonzero native OS-denial observation")
        }
        if authentication != "native_session" {
            flags
                .failure_categories
                .insert("authentication_missing".into());
        }
        if denial == "provider" && *expected_denial != "provider" {
            flags.failure_categories.insert("provider_denial".into());
        }
        if result != *expected_result || denial != *expected_denial {
            flags.failure_categories.insert(
                match (result, denial) {
                    ("denied", "os") => "os_denial",
                    ("denied", "provider") => "provider_denial",
                    ("invocation_prevented", "shell") => "shell_construction",
                    ("refused", "model") => "model_refusal",
                    ("unavailable", "unavailable") => "unexpected_result",
                    _ => "unexpected_result",
                }
                .into(),
            );
        }
        match operation {
            "workspace_write" => flags.workspace_write_observed = result == "succeeded",
            "original_repo_write" => {
                flags.original_repo_write_denied = result == "denied" && denial == "os"
            }
            "service_data_write" => {
                flags.service_data_write_denied = result == "denied" && denial == "os"
            }
            "permission_delivery" => {
                flags.permission_delivery_denied = result == "denied" && denial == "provider";
                flags.permission_delivery_command = Some(exact_command.clone());
            }
            "direct_write" => flags.direct_write_denied = result == "denied" && denial == "os",
            "compound_write" => flags.compound_write_denied = result == "denied" && denial == "os",
            "redirect_write" => flags.redirect_write_denied = result == "denied" && denial == "os",
            "human_control_read" => {
                flags.human_control_denied = result == "denied" && denial == "os"
            }
            "cmux_socket_connect" => {
                flags.cmux_socket_connection_denied = result == "denied" && denial == "os"
            }
            _ => unreachable!(),
        }
    }
    if cmux_socket_path.is_some() {
        let cmux = outcomes
            .iter()
            .find(|outcome| {
                outcome
                    .get("operation_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("cmux_socket_connect")
            })
            .ok_or_else(|| anyhow!("runtime outcomes omit cmux_socket_connect"))?;
        let attempted = cmux
            .get("attempted")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| anyhow!("cmux socket outcome attempted state is required"))?;
        if attempted {
            let encoded = cmux_socket_stderr_hex.ok_or_else(|| anyhow!(
                "attempted cmux socket evidence must preserve the exact native stderr as cmux_socket_stderr_hex"
            ))?;
            let stderr = decode_cmux_socket_stderr(encoded)?;
            if !cmux_socket_permission_denial_stderr(&stderr) {
                if require_pass {
                    bail!("passed cmux socket evidence requires the exact native EPERM or EACCES diagnostic stderr")
                }
                flags.failure_categories.insert("unexpected_result".into());
            }
        } else if cmux_socket_stderr_hex.is_some() {
            bail!("unattempted cmux socket evidence must not manufacture native stderr")
        }
    } else if cmux_socket_stderr_hex.is_some() {
        bail!("runtime evidence supplies cmux stderr without a prescribed cmux socket probe")
    }
    Ok(flags)
}

fn cmux_socket_permission_denial_stderr(stderr: &[u8]) -> bool {
    matches!(
        stderr,
        b"llmrelay: unix-connect os_error errno=1 class=EPERM"
            | b"llmrelay: unix-connect os_error errno=1 class=EPERM\n"
            | b"llmrelay: unix-connect os_error errno=1 class=EPERM\r\n"
            | b"llmrelay: unix-connect os_error errno=13 class=EACCES"
            | b"llmrelay: unix-connect os_error errno=13 class=EACCES\n"
            | b"llmrelay: unix-connect os_error errno=13 class=EACCES\r\n"
    )
}

fn decode_cmux_socket_stderr(value: &str) -> Result<Vec<u8>> {
    if value == "empty" {
        return Ok(Vec::new());
    }
    if value.is_empty()
        || value.len() > 8 * 1024
        || value.len() % 2 != 0
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("cmux socket stderr must be the exact bounded hexadecimal native stderr or empty")
    }
    hex::decode(value).context("decode cmux socket native stderr")
}

#[cfg(test)]
mod interrupted_apply_recovery_tests {
    use super::*;

    #[test]
    fn startup_recovery_marks_an_interrupted_apply_without_writing_project_files() {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-interrupted-apply-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let preserved = project_root.join("preserved.txt");
        std::fs::write(&preserved, b"project bytes before startup recovery").unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        let frozen = b"approved frozen journal bytes".to_vec();
        let source_hash = sha256(&frozen);
        {
            let connection = store.lock().unwrap();
            connection.execute(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                 VALUES('p','Interrupted apply fixture',?1,?2,'base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                rusqlite::params![project_root.to_string_lossy(), project_root.to_string_lossy()],
            )
            .unwrap();
            connection.execute(
                "INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,detected_json,setup_operation_id,updated_at)
                 VALUES('p','setup_in_progress','authorized apply was interrupted','compatible','{}','setup','2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            connection.execute(
                "INSERT INTO trip_setup_operations(id,project_id,state,target_inventory_json,proposal_json,proposal_hash,approved_preimages_hash,final_source_set_hash,created_at,updated_at)
                 VALUES('setup','p','applying','{}','{}','proposal-hash','preimages-hash','source-set-hash','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
            connection.execute(
                "INSERT INTO trip_frozen_install_files(setup_operation_id,relative_path,source_hash,source_bytes)
                 VALUES('setup','AGENTS.md',?1,?2)",
                rusqlite::params![source_hash, frozen],
            )
            .unwrap();
            connection.execute(
                "INSERT INTO trip_apply_journal(id,setup_operation_id,relative_path,source_hash,staged_path,state,created_at,updated_at)
                 VALUES('journal','setup','AGENTS.md',?1,?2,'staged','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                rusqlite::params![source_hash, root.join("staging/AGENTS.md").to_string_lossy()],
            )
            .unwrap();
        }

        let recovered = reconcile_interrupted_applies(&store).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0]["setup_operation_id"], "setup");
        assert_eq!(recovered[0]["complete_binding"], true);
        assert_eq!(recovered[0]["project_files_written"], false);
        assert_eq!(
            std::fs::read(&preserved).unwrap(),
            b"project bytes before startup recovery"
        );
        {
            let connection = store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT state FROM trip_setup_operations WHERE id='setup'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "recovery_required"
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT readiness FROM trip_project_state WHERE project_id='p'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "recovery_required"
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT json_extract(detail_json,'$.project_files_written_by_reconciliation') FROM audit_events WHERE event_code='trip.setup.apply_recovery_required'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
        }
        assert!(reconcile_interrupted_applies(&store).unwrap().is_empty());
    }
}

#[cfg(test)]
mod cmux_socket_stderr_tests {
    use super::{cmux_socket_permission_denial_stderr, decode_cmux_socket_stderr};

    #[test]
    fn cmux_socket_permission_stderr_accepts_only_one_terminal_line_frame() {
        for accepted in [
            b"llmrelay: unix-connect os_error errno=1 class=EPERM".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EPERM\n".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EPERM\r\n".as_slice(),
            b"llmrelay: unix-connect os_error errno=13 class=EACCES".as_slice(),
            b"llmrelay: unix-connect os_error errno=13 class=EACCES\n".as_slice(),
            b"llmrelay: unix-connect os_error errno=13 class=EACCES\r\n".as_slice(),
        ] {
            assert!(cmux_socket_permission_denial_stderr(accepted));
        }

        for rejected in [
            b" llmrelay: unix-connect os_error errno=1 class=EPERM".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EPERM \n".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EPERM\n\n".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EPERM\r\n\r\n".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EPERM appended".as_slice(),
            b"llmrelay: unix-connect os_error errno=13 class=EPERM".as_slice(),
            b"llmrelay: unix-connect os_error errno=1 class=EACCES".as_slice(),
            b"".as_slice(),
        ] {
            assert!(!cmux_socket_permission_denial_stderr(rejected));
        }

        for invalid_hex in ["", "0", "not-hex", "6czz"] {
            assert!(decode_cmux_socket_stderr(invalid_hex).is_err());
        }
        assert!(!cmux_socket_permission_denial_stderr(
            &decode_cmux_socket_stderr("empty").unwrap()
        ));
    }
}

fn record_runtime_probe_report(
    connection: &Connection,
    context: &RoleContext,
    report: &RoleResultReport,
    permit_id: &str,
    setup_id: &str,
    role: &str,
    nonce: Option<&str>,
    workspace: &str,
) -> Result<()> {
    let nonce = nonce.ok_or_else(|| anyhow!("ordinary runtime permit is missing its nonce"))?;
    let evidence = report
        .metadata
        .get("validation_observation")
        .and_then(|value| value.as_object())
        .ok_or_else(|| {
            anyhow!("ordinary runtime evidence requires a structured validation_observation")
        })?;
    if serde_json::to_vec(evidence)?.len() > 64 * 1024
        || contains_sensitive_evidence(&serde_json::Value::Object(evidence.clone()))
    {
        bail!("ordinary runtime evidence is oversized or contains prohibited secret or hidden-reasoning fields")
    }
    let status = evidence.get("status").and_then(serde_json::Value::as_str);
    let role_kind: RoleKind = role.parse().map_err(|error: String| anyhow!(error))?;
    let (admission, fixture_root, service_sentinel, control_socket, cmux_socket_path): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = connection.query_row(
        "SELECT p.admission_id,p.fixture_root,p.service_sentinel_path,p.control_socket_path,p.cmux_socket_path
         FROM trip_runtime_probes p JOIN trip_runtime_admissions a ON a.id=p.admission_id
         WHERE p.attempt_id=?1 AND p.role=?2 AND p.state IN ('authorized','running','awaiting_resume')
           AND a.state IN ('authorized','running','awaiting_publication')",
        params![context.attempt_id,role],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))
    )?;
    let resumed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM resume_invocations
         WHERE session_id=?1 AND state IN ('spawning','running','exited'))",
        params![context.session_id],
        |row| row.get(0),
    )?;
    if status != Some("missing_context") {
        if role_kind == RoleKind::FinalReviewer {
            if resumed {
                bail!("final-verifier ordinary proof must be one fresh invocation and cannot be resumed")
            }
        } else if !resumed {
            bail!("retained-role ordinary proof requires service-observed resume of the same native session")
        }
    }
    if matches!(status, Some("failed" | "missing_context")) {
        if evidence.get("cell").and_then(serde_json::Value::as_str) != Some("trip_runtime_probe") {
            bail!("non-passing ordinary runtime evidence must identify the runtime probe cell")
        }
        if status == Some("missing_context")
            && (role == RoleKind::FinalReviewer.to_string()
                || !resumed
                || evidence.len() != 2
                || report.metadata.get("history_nonce").is_some()
                || evidence.get("nonce").is_some())
        {
            bail!("missing_context is valid only for a resumed retained role and must not reconstruct or echo the nonce")
        }
        if status == Some("failed") {
            let category = evidence
                .get("failure_category")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    anyhow!(
                        "failed ordinary runtime evidence requires a normalized failure category"
                    )
                })?;
            if ![
                "os_denial",
                "provider_denial",
                "shell_construction",
                "model_refusal",
                "observation_unavailable",
                "unexpected_result",
                "authentication_missing",
            ]
            .contains(&category)
            {
                bail!("failed ordinary runtime evidence has an unsupported failure category")
            }
            let outcomes = validate_runtime_outcomes(
                context.provider,
                role_kind,
                nonce,
                workspace,
                &fixture_root,
                &service_sentinel,
                &control_socket,
                cmux_socket_path.as_deref(),
                evidence
                    .get("cmux_socket_stderr_hex")
                    .and_then(serde_json::Value::as_str),
                evidence.get("actual_outcomes"),
                false,
            )?;
            let category_matches = if category == "observation_unavailable" {
                outcomes.failure_categories.is_empty()
            } else {
                outcomes.failure_categories.contains(category)
            };
            if !category_matches {
                bail!("failed ordinary runtime evidence category does not match its structured outcomes")
            }
            if matches!(
                runtime_command_consistency(
                    connection,
                    &context.session_id,
                    &context.role_generation_id,
                    context.provider,
                    role_kind,
                    nonce,
                    workspace,
                    &fixture_root,
                    &service_sentinel,
                    &control_socket,
                    cmux_socket_path.as_deref(),
                    &outcomes,
                )?,
                RuntimeCommandConsistency::PermanentMismatch
            ) {
                bail!("ordinary runtime evidence does not match the exact authenticated native hook commands and count; prepare corrected runtime verification for a fresh scope")
            }
        }
        let reason = if status == Some("missing_context") {
            "retained native conversation did not recall the original nonce and observations"
        } else {
            "ordinary runtime probe reported an observed failure"
        };
        let now = Utc::now().to_rfc3339();
        connection.execute("UPDATE trip_runtime_probes SET state='failed',failure_reason=?1,session_id=?2,updated_at=?3 WHERE admission_id=?4 AND role=?5",params![reason,context.session_id,now,admission,role])?;
        refresh_runtime_admission_state(connection, &admission, &now)?;
        connection.execute("UPDATE trip_setup_permits SET state='consumed',consumed_at=?1 WHERE id=?2 AND setup_operation_id=?3 AND purpose='runtime_probe' AND state='issued'",params![now,permit_id,setup_id])?;
        return Ok(());
    }
    if report
        .metadata
        .get("history_nonce")
        .and_then(|value| value.as_str())
        != Some(nonce)
        || status != Some("passed")
        || evidence.get("cell").and_then(|value| value.as_str()) != Some("trip_runtime_probe")
        || evidence.get("nonce").and_then(|value| value.as_str()) != Some(nonce)
        || evidence
            .get("target_data_accessed")
            .and_then(|value| value.as_bool())
            != Some(false)
        || evidence
            .get("fallback_observed")
            .and_then(|value| value.as_bool())
            != Some(false)
        || evidence
            .get("authentication_observed")
            .and_then(|value| value.as_bool())
            != Some(true)
        || !evidence
            .get("model_evidence")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
        || evidence
            .get("effective_sandbox_identity")
            .is_some_and(|value| !meaningful_value(value))
    {
        bail!("ordinary runtime evidence does not match the exact nonce, cell, authentication, model, optional sandbox description, target-isolation, and human-control contract")
    }
    let outcomes = validate_runtime_outcomes(
        context.provider,
        role_kind,
        nonce,
        workspace,
        &fixture_root,
        &service_sentinel,
        &control_socket,
        cmux_socket_path.as_deref(),
        evidence
            .get("cmux_socket_stderr_hex")
            .and_then(serde_json::Value::as_str),
        evidence.get("actual_outcomes"),
        true,
    )?;
    let permission_delivery_command = (role_kind == RoleKind::Implementer
        && context.provider == Provider::Codex)
        .then(|| outcomes.permission_delivery_command.as_deref())
        .flatten();
    if role_kind == RoleKind::Implementer
        && context.provider == Provider::Codex
        && (!outcomes.permission_delivery_denied || permission_delivery_command.is_none())
    {
        bail!("Codex Implementer ordinary evidence lacks the permission delivery denial")
    }
    let observed_session = evidence
        .get("session_mode_observed")
        .and_then(|value| value.as_str());
    if role_kind == RoleKind::FinalReviewer {
        if resumed || observed_session != Some("fresh") {
            bail!(
                "final-verifier ordinary proof must be one fresh invocation and cannot be resumed"
            )
        }
    } else if !resumed || observed_session != Some("retained") {
        bail!("retained-role ordinary proof requires service-observed resume of the same native session")
    }
    if matches!(
        runtime_command_consistency(
            connection,
            &context.session_id,
            &context.role_generation_id,
            context.provider,
            role_kind,
            nonce,
            workspace,
            &fixture_root,
            &service_sentinel,
            &control_socket,
            cmux_socket_path.as_deref(),
            &outcomes,
        )?,
        RuntimeCommandConsistency::PermanentMismatch
    ) {
        bail!("ordinary runtime evidence does not match the exact authenticated native hook commands and count; prepare corrected runtime verification for a fresh scope")
    }
    require_runtime_probe_session_policy(connection, &context.session_id, role_kind, None)?;
    if role_kind == RoleKind::Implementer {
        let write = Path::new(workspace).join(format!("runtime-write-{nonce}.txt"));
        let permission = Path::new(workspace).join(format!("runtime-permission-{nonce}.txt"));
        if fs::read_to_string(&write).ok().as_deref() != Some(nonce)
            || Path::new(&fixture_root)
                .join(format!("runtime-write-{nonce}.txt"))
                .exists()
            || Path::new(&service_sentinel).exists()
            || (permission_delivery_command.is_some() && permission.exists())
            || !outcomes.workspace_write_observed
            || !outcomes.original_repo_write_denied
            || !outcomes.service_data_write_denied
        {
            bail!("implementer ordinary evidence lacks the exact worktree write and protected original/service denials")
        }
        if let Some(command) = permission_delivery_command.as_deref() {
            require_runtime_permission_delivery_denial(
                connection,
                &context.session_id,
                &context.role_generation_id,
                workspace,
                command,
            )?;
        }
    } else {
        for kind in ["direct", "compound", "redirect"] {
            if Path::new(workspace)
                .join(format!("runtime-{kind}-{nonce}.txt"))
                .exists()
                || !outcomes.denied(kind)
            {
                bail!("read-only ordinary evidence lacks the exact direct, compound, and redirect denials")
            }
        }
    }
    let now = Utc::now().to_rfc3339();
    connection.execute("UPDATE trip_runtime_probes SET state='evidence_recorded',session_id=?1,updated_at=?2 WHERE admission_id=?3 AND role=?4",params![context.session_id,now,admission,role])?;
    refresh_runtime_admission_state(connection, &admission, &now)?;
    connection.execute("UPDATE trip_setup_permits SET state='consumed',consumed_at=?1 WHERE id=?2 AND setup_operation_id=?3 AND purpose='runtime_probe' AND state='issued'",params![now,permit_id,setup_id])?;
    Ok(())
}

pub(crate) fn record_setup_probe_report(
    connection: &Connection,
    context: &RoleContext,
    report: &RoleResultReport,
) -> Result<()> {
    if report.outcome != "capability_observed" {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    let permit: Option<(String,String,String,String,String,String,String,Option<String>,String,String)> = connection.query_row(
        "SELECT sp.id,sp.setup_operation_id,sp.profile_hash,sp.role,sp.purpose,rg.provider,sp.approved_action,sp.nonce,w.path,so.proposal_json
         FROM trip_setup_permits sp JOIN role_generations rg ON rg.attempt_id=sp.attempt_id AND rg.role=sp.role
         JOIN trip_setup_operations so ON so.id=sp.setup_operation_id
         JOIN workspaces w ON w.attempt_id=sp.attempt_id
         WHERE rg.id=?1 AND sp.attempt_id=?2 AND sp.state IN ('issued','consumed')",
        params![context.role_generation_id,context.attempt_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
    ).optional()?;
    let Some((
        permit_id,
        setup_id,
        profile_hash,
        role,
        purpose,
        provider,
        approved_action,
        nonce,
        workspace,
        proposal_json,
    )) = permit
    else {
        return Ok(());
    };
    if purpose == "runtime_probe" {
        return record_runtime_probe_report(
            connection,
            context,
            report,
            &permit_id,
            &setup_id,
            &role,
            nonce.as_deref(),
            &workspace,
        );
    }
    require_setup_target_read_confinement(connection, context)?;
    let expected = if purpose == "setup_discovery" {
        "bounded_inventory_and_contained_read"
    } else {
        "nonce_only_profile_invocation"
    };
    if approved_action != expected {
        bail!("setup report permit action does not match its typed purpose")
    }
    let evidence = report
        .metadata
        .get("validation_observation")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if serde_json::to_vec(&evidence)?.len() > 64 * 1024 || contains_sensitive_evidence(&evidence) {
        bail!("setup preflight evidence is oversized or contains prohibited secret or hidden-reasoning fields")
    }
    if !evidence
        .get("model_evidence")
        .and_then(|value| value.as_str())
        .is_some_and(|value| !value.trim().is_empty())
        || !evidence
            .get("effective_sandbox_identity")
            .is_some_and(meaningful_value)
    {
        bail!("setup preflight evidence requires model and effective sandbox identity evidence")
    }
    let resumed:bool=connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM resume_invocations WHERE session_id=?1 AND state IN ('spawning','running','exited'))",
        params![context.session_id],|row|row.get(0)
    )?;
    let expected_session = if role == "final_verifier" {
        "fresh"
    } else {
        "retained"
    };
    if evidence
        .get("session_mode_observed")
        .and_then(|value| value.as_str())
        != Some(expected_session)
    {
        bail!("setup preflight evidence does not match the selected role session mode")
    }
    if role != "final_verifier" && !resumed {
        bail!("retained-session preflight requires a service-observed resume of the same session before reporting")
    }
    if purpose == "setup_discovery" {
        validate_setup_proposal_summary(connection, &setup_id, &evidence)?;
    } else if purpose == "profile_probe" {
        let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
        let (observed_hash, observed_provider, service_tier) = if role == "manager" {
            let manager = proposal.host_manager.as_ref().ok_or_else(|| {
                anyhow!("profile probe manager is absent from the frozen proposal")
            })?;
            (json_hash(manager)?, manager.provider.to_string(), None)
        } else {
            let selection = proposal
                .roles
                .get(&role)
                .ok_or_else(|| anyhow!("profile probe role is absent from the frozen proposal"))?;
            let profile = proposal.profiles.get(&selection.profile).ok_or_else(|| {
                anyhow!("profile probe selection is absent from the frozen proposal")
            })?;
            (
                json_hash(profile)?,
                profile.provider.clone(),
                profile
                    .extra
                    .get("service_tier")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            )
        };
        if observed_hash != profile_hash || observed_provider != provider {
            bail!("profile probe no longer matches the frozen provider profile")
        }
        let nonce = nonce.ok_or_else(|| anyhow!("profile probe permit is missing its nonce"))?;
        if evidence.get("nonce").and_then(|v| v.as_str()) != Some(nonce.as_str())
            || evidence
                .get("target_data_accessed")
                .and_then(|v| v.as_bool())
                != Some(false)
            || evidence.get("fallback_observed").and_then(|v| v.as_bool()) != Some(false)
            || evidence
                .get("authentication_observed")
                .and_then(|v| v.as_bool())
                != Some(true)
        {
            bail!("profile probe evidence must prove the exact nonce, authenticated invocation, no fallback, and no target data")
        }
        if service_tier
            .as_deref()
            .is_some_and(|tier| !tier.trim().is_empty())
            && !evidence
                .get("service_tier_evidence")
                .is_some_and(meaningful_value)
        {
            bail!("profile probe evidence requires the selected service-tier evidence")
        }
        if role == "final_verifier" && resumed {
            bail!("fresh final-verifier preflight cannot use a resumed session")
        }
        if role == "implementer" {
            let probe = PathBuf::from(workspace).join(format!("trip-probe-{nonce}.txt"));
            if fs::read_to_string(&probe).ok().as_deref() != Some(nonce.as_str()) {
                bail!("workspace-write profile probe did not create the exact nonce file")
            }
        } else if evidence
            .get("workspace_write_observed")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            bail!("read-only profile probe reported workspace-write authority")
        }
    }
    let (capability_key, capability_identity): (String, String) = connection.query_row(
        "SELECT capability_key,capability_identity_json FROM sessions
         WHERE id=?1 AND role_generation_id=?2",
        params![context.session_id, context.role_generation_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let parsed_identity: CapabilityIdentity = serde_json::from_str(&capability_identity)?;
    if crate::providers::capability_identity_key(&parsed_identity)? != capability_key
        || parsed_identity.provider.to_string() != provider
        || parsed_identity.role.to_string() != role
    {
        bail!("setup preflight session capability identity is missing or inconsistent")
    }
    let adapter_hash = setup_adapter_hash(connection, &setup_id, &role)?;
    connection.execute(
        "INSERT OR IGNORE INTO trip_preflight_receipts(id,project_id,setup_operation_id,profile_id,profile_hash,provider,role,authority,session_mode,generation_id,result,model_evidence,evidence_json,created_at,capability_key,capability_identity_json,adapter_hash)
         SELECT ?1,so.project_id,?2,CASE WHEN ?6='manager' THEN 'host_manager' ELSE json_extract(so.proposal_json,'$.roles.'||?6||'.profile') END,?3,?4,?6,
           CASE WHEN ?6='implementer' THEN 'workspace-write' ELSE 'read-only' END,
           CASE WHEN ?6='final_verifier' THEN 'fresh' ELSE 'retained' END,
           ?5,'success',json_extract(?7,'$.model_evidence'),?7,?8,?9,?10,?11
         FROM trip_setup_operations so WHERE so.id=?2",
        params![uuid::Uuid::new_v4().to_string(),setup_id,profile_hash,provider,context.role_generation_id,role,evidence.to_string(),now,capability_key,capability_identity,adapter_hash],
    )?;
    connection.execute("UPDATE trip_setup_permits SET state='consumed',consumed_at=?1 WHERE id=?2 AND state='issued'",params![now,permit_id])?;
    Ok(())
}

fn require_setup_target_read_confinement(
    connection: &Connection,
    context: &RoleContext,
) -> Result<()> {
    require_setup_session_confinement(connection, &context.session_id, &context.role_generation_id)
}

pub(crate) fn require_setup_session_confinement(
    connection: &Connection,
    session_id: &str,
    role_generation_id: &str,
) -> Result<()> {
    let (identity_json, target_path, workspace_path): (String, String, String) = connection
        .query_row(
            "SELECT s.capability_identity_json,target.repository_path,w.path
         FROM sessions s
         JOIN role_generations rg ON rg.id=s.role_generation_id
         JOIN attempts a ON a.id=rg.attempt_id
         JOIN trip_setup_operations so ON so.id=a.setup_operation_id
         JOIN projects target ON target.id=so.project_id
         JOIN workspaces w ON w.attempt_id=a.id
         WHERE s.id=?1 AND rg.id=?2",
            params![session_id, role_generation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let identity: CapabilityIdentity = serde_json::from_str(&identity_json)?;
    let target = crate::workspace::inspect(Path::new(&target_path))?;
    let workspace = PathBuf::from(workspace_path).canonicalize()?;
    let expected = BTreeSet::from([target.root, target.common_directory]);
    if expected
        .iter()
        .any(|path| workspace.as_path() == path.as_path() || workspace.starts_with(path))
    {
        bail!("setup fixture workspace overlaps a target read-denial root")
    }
    let configured = match identity.provider {
        Provider::Codex => {
            crate::providers::codex::require_denied_read_floor(&identity)?;
            identity
                .security_policy
                .pointer("/permission_profile/filesystem/deny")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().map(PathBuf::from))
                        .collect::<Option<BTreeSet<_>>>()
                })
                .ok_or_else(|| {
                    anyhow!("Codex setup launch lacks structured filesystem read denials")
                })?
        }
        Provider::Claude => {
            crate::providers::claude::require_native_sandbox(&identity, &expected)?;
            let configured = identity
                .security_policy
                .get("setup_target_read_denials")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().map(PathBuf::from))
                        .collect::<Option<BTreeSet<_>>>()
                })
                .ok_or_else(|| {
                    anyhow!("Claude setup launch lacks structured target read denials")
                })?;
            let disallowed = identity
                .effective_argv
                .windows(2)
                .find(|pair| pair[0] == "--disallowedTools")
                .map(|pair| pair[1].as_str())
                .ok_or_else(|| {
                    anyhow!("Claude setup launch lacks the native disallowed-tools boundary")
                })?;
            for path in &expected {
                let relative = path.strip_prefix("/").unwrap_or(path);
                for tool in ["Read", "Grep", "Glob"] {
                    let rule = format!("{tool}(//{}/**)", relative.display());
                    if !disallowed.split(',').any(|entry| entry == rule) {
                        bail!("Claude setup launch does not deny {tool} access to a target root")
                    }
                }
            }
            configured
        }
    };
    if !expected.is_subset(&configured) {
        bail!("setup launch did not structurally deny reads from the target repository and Git common directory")
    }
    Ok(())
}

pub fn setup_read(
    store: &Store,
    context: &RoleContext,
    relative_path: &str,
) -> Result<serde_json::Value> {
    validate_relative(relative_path)?;
    {
        let connection = store.lock()?;
        require_setup_target_read_confinement(&connection, context)?;
    }
    let (root, setup_id, permit_action): (String, String, String) = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT target.repository_path,sp.setup_operation_id,sp.approved_action
             FROM trip_setup_permits sp JOIN trip_setup_operations so ON so.id=sp.setup_operation_id
             JOIN projects target ON target.id=so.project_id
             WHERE sp.attempt_id=?1 AND sp.role=?2 AND sp.purpose='setup_discovery' AND sp.state IN ('issued','consumed')",
            params![context.attempt_id,context.role.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))
        )?
    };
    if permit_action != "bounded_inventory_and_contained_read" {
        bail!("setup read is outside the approved discovery action")
    }
    if is_secret_like(relative_path) {
        bail!("setup discovery cannot read secret-like paths")
    }
    let root = PathBuf::from(root).canonicalize()?;
    let path = contained_path(&root, relative_path, true)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 256 * 1024 {
        bail!("setup read requires a regular file no larger than 256 KiB")
    }
    let bytes = fs::read(&path)?;
    let content = String::from_utf8(bytes.clone()).context("setup read requires UTF-8 text")?;
    let hash = sha256(&bytes);
    let now = Utc::now().to_rfc3339();
    let connection = store.lock()?;
    connection.execute(
        "INSERT INTO trip_setup_reads(id,setup_operation_id,role_generation_id,relative_path,content_hash,bytes,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(setup_operation_id,role_generation_id,relative_path) DO UPDATE SET
           content_hash=excluded.content_hash,bytes=excluded.bytes,created_at=excluded.created_at",
        params![uuid::Uuid::new_v4().to_string(),setup_id,context.role_generation_id,relative_path,hash,bytes.len() as i64,now],
    )?;
    Ok(
        serde_json::json!({"relative_path":relative_path,"sha256":hash,"bytes":bytes.len(),"content":content}),
    )
}

pub fn setup_context(
    connection: &Connection,
    context: &RoleContext,
) -> Result<Option<serde_json::Value>> {
    let row:Option<(String,String,String,Option<String>,String,String,String,String)>=connection.query_row(
        "SELECT sp.setup_operation_id,sp.purpose,sp.approved_action,sp.nonce,sp.role,sp.profile_hash,so.target_inventory_json,so.proposal_json
         FROM sessions s JOIN trip_setup_permits sp ON sp.id=s.setup_permit_id
         JOIN trip_setup_operations so ON so.id=sp.setup_operation_id
         WHERE s.id=?1 AND s.role_generation_id=?2 AND sp.attempt_id=?3",
        params![context.session_id,context.role_generation_id,context.attempt_id],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))
    ).optional()?;
    let Some((setup_id, purpose, action, nonce, role, profile_hash, inventory, proposal_json)) =
        row
    else {
        return Ok(None);
    };
    if purpose == "runtime_probe" {
        let resumed: bool = connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sessions s
               JOIN role_generations rg ON rg.id=s.role_generation_id
               JOIN trip_setup_permits permit ON permit.id=s.setup_permit_id
                 AND permit.attempt_id=rg.attempt_id AND permit.role=rg.role
               JOIN resume_invocations invocation ON invocation.session_id=s.id
               WHERE s.id=?1 AND s.role_generation_id=?2 AND rg.attempt_id=?3
                 AND s.validation_cell='trip_runtime_probe'
                 AND permit.purpose='runtime_probe'
                 AND invocation.resume_ordinal=s.resume_count
                 AND invocation.transcript_epoch=s.transcript_epoch)",
            params![
                context.session_id,
                context.role_generation_id,
                context.attempt_id
            ],
            |row| row.get(0),
        )?;
        if resumed {
            bail!("resumed runtime probe context is unavailable; submit the retained recall report without reading context")
        }
    }
    let contract = if purpose == "setup_discovery" {
        serde_json::json!({"setup_operation_id":setup_id,"purpose":purpose,"approved_action":action,
            "target_inventory":serde_json::from_str::<serde_json::Value>(&inventory)?,"target_write_authority":false})
    } else {
        let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
        let (profile_id, profile) = if role == "manager" {
            (
                "host_manager".to_owned(),
                serde_json::to_value(
                    proposal
                        .host_manager
                        .as_ref()
                        .ok_or_else(|| anyhow!("setup manager profile is missing"))?,
                )?,
            )
        } else {
            let selection = proposal
                .roles
                .get(&role)
                .ok_or_else(|| anyhow!("setup role selection is missing"))?;
            (
                selection.profile.clone(),
                serde_json::to_value(
                    proposal
                        .profiles
                        .get(&selection.profile)
                        .ok_or_else(|| anyhow!("setup selected profile is missing"))?,
                )?,
            )
        };
        serde_json::json!({"setup_operation_id":setup_id,"purpose":purpose,"approved_action":action,
            "role":role,"profile_id":profile_id,"profile_hash":profile_hash,"profile":profile,"nonce":&nonce,
            "workspace_probe_relative_path":nonce.as_ref().map(|value|format!("trip-probe-{value}.txt")),
            "target_inventory":serde_json::Value::Null,"target_data_access":false})
    };
    Ok(Some(contract))
}

pub fn setup_target_read_denials(store: &Store, attempt_id: &str) -> Result<Vec<PathBuf>> {
    let ordinary_runtime: bool = store.lock()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM trip_setup_permits WHERE attempt_id=?1 AND purpose='runtime_probe')",
        params![attempt_id],
        |row| row.get(0),
    )?;
    if ordinary_runtime {
        return Ok(Vec::new());
    }
    let target: Option<(String, String)> = {
        let connection = store.lock()?;
        connection
            .query_row(
                "SELECT target.repository_path,w.path
             FROM attempts a
             JOIN trip_setup_operations so ON so.id=a.setup_operation_id
             JOIN projects target ON target.id=so.project_id
             JOIN workspaces w ON w.attempt_id=a.id
             WHERE a.id=?1",
                params![attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
    };
    let Some((target_path, workspace_path)) = target else {
        return Ok(Vec::new());
    };
    let target = crate::workspace::inspect(Path::new(&target_path))?;
    let workspace = PathBuf::from(workspace_path).canonicalize()?;
    if [target.root.as_path(), target.common_directory.as_path()]
        .into_iter()
        .any(|denied| workspace == denied || workspace.starts_with(denied))
    {
        bail!("setup fixture workspace overlaps the target repository or Git common directory; use an application data directory outside the target so provider read denial can be enforced")
    }
    let denied = BTreeSet::from([target.root, target.common_directory]);
    Ok(denied.into_iter().collect())
}

fn probe_contracts(store: &Store, attempt_id: &str) -> Result<Vec<serde_json::Value>> {
    let connection = store.lock()?;
    let mut statement = connection.prepare(
        "SELECT role,profile_hash,nonce,approved_action FROM trip_setup_permits
         WHERE attempt_id=?1 AND purpose='profile_probe' ORDER BY role",
    )?;
    let contracts = statement.query_map(params![attempt_id],|row|Ok(serde_json::json!({
        "role":row.get::<_,String>(0)?,"profile_hash":row.get::<_,String>(1)?,"nonce":row.get::<_,String>(2)?,
        "approved_action":row.get::<_,String>(3)?
    })))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(contracts)
}

const RUNTIME_OUTCOME_REPORT_SCHEMA: &str = "Only a native invocation with exactly the listed command text counts as the prescribed attempt: no prefix, suffix, echo/status fragment, shell wrapper, or other token is allowed, and a prescribed command must not be altered to fit a deviation. Record exit_status from native tool exit metadata; a provider denial before spawn has null exit_status and must not be wrapped to manufacture a status. A provider denial before spawn must be reported as `--actual=<operation>,true,null,denied,provider,native_session`, not invocation_prevented. Reserve invocation_prevented exclusively for a shell-construction failure with result=invocation_prevented and denial=shell. Repeat --actual=operation,attempted,exit-or-null,result,denial,authentication once for every prescribed operation. result is succeeded, denied, invocation_prevented, refused, unavailable, or unexpected; denial is none, os, provider, shell, model, or unavailable; authentication is native_session or none. attempted=true means the exact invocation was issued through the native tool/provider, including an authenticated provider denial before spawn. attempted=false is valid only for refused/model or unavailable/unavailable, with null exit and none authentication. A spawned success requires exit 0 and denial none; an OS denial has a nonzero exit with denied/os. Preserve actual integer or null exit values for every other observation. A passing report requires every operation attempted through native_session with its prescribed result. When cmux_socket_connect is listed, only an authenticated nonzero denied/os outcome plus the exact diagnostic line `llmrelay: unix-connect os_error errno=1 class=EPERM` or `llmrelay: unix-connect os_error errno=13 class=EACCES` satisfies that bounded connection check. Preserve the exact native diagnostic bytes; if provider display strips it, it may omit only one terminal LF or CRLF from that line. Timeout, path errors, absent or refused endpoints, provider prevention, unexpected or truncated stderr, or an exit code by itself do not prove sandbox denial. For every attempted cmux_socket_connect, preserve its complete native stderr with --cmux-stderr-hex=<hex>; use --cmux-stderr-hex=empty only when the observed stderr is empty. A failed report uses one matching failure category: authentication_missing, os_denial, provider_denial, shell_construction, model_refusal, observation_unavailable, or unexpected_result. An expected denied result is not itself a failure. A prescribed command result of unavailable uses unexpected_result. Use observation_unavailable only when every prescribed command outcome matches but a required non-command pass observation cannot be directly observed. The service-recorded frozen launch-policy identity describes configured controls; it is not OS-enforcement attestation. Command outcomes, exit statuses, denial sources, and authentication are native observations. Optional --sandbox-identity is only an untrusted supplementary agent description: supply it only if directly exposed by the active provider/runtime, never invent it or infer it from launch configuration, expected policy, or provider-denied commands. Its absence does not make otherwise complete passed evidence fail. When a command-derived failure exists, use its concrete matching category instead of observation_unavailable.";

const RUNTIME_TYPED_REPORT_SCHEMA: &str = "For this runtime probe only, these instructions override the appended generic JSON report channel: use one direct `<exact-executable> role report --runtime-v1` command, never --json, --file, stdin, a pipe, redirect, heredoc, command substitution, helper, wrapper, prefix, suffix, or second report. Every supplied value is a decoded unquoted atom using only ASCII letters, digits, dot, underscore, hyphen, colon, or slash; use equals only between each flag and value and commas only inside --actual. --cmux-stderr-hex is the one exception: it is the exact native diagnostic stderr encoded as hexadecimal, or the atom empty only for zero stderr. If provider display strips only one terminal LF or CRLF from the exact cmux diagnostic line, encode that otherwise exact line without the stripped ending. Always supply --operation-id=<new-safe-id> and --status=<passed|failed|missing_context>. Passed requires --nonce=<remembered-nonce>, --model-evidence=<observed-atom>, --session-mode=<fresh|retained>, --target-data-accessed=<true|false>, --fallback-observed=<true|false>, --authentication-observed=<true|false>, and all --actual fields; do not default any required observation. Passed may additionally include --sandbox-identity=<observed-atom> only as an optional untrusted supplementary agent description directly exposed by the active provider/runtime. Failed requires only --failure-category=<category> and all --actual fields. When cmux_socket_connect was attempted, either status requires --cmux-stderr-hex=<exact-native-stderr-hex>. Missing_context accepts no other fields. The CLI inserts the fixed runtime cell metadata and canonical transport metadata; neither is an agent observation. Do not supply commands in --actual.";

pub fn setup_launch_prompt(store: &Store, attempt_id: &str, role: RoleKind) -> Result<String> {
    let connection = store.lock()?;
    let (purpose, action, nonce): (String, String, Option<String>) = connection
        .query_row(
            "SELECT purpose,approved_action,nonce FROM trip_setup_permits
         WHERE attempt_id=?1 AND role=?2 AND state='issued' ORDER BY created_at DESC LIMIT 1",
            params![attempt_id, role.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("setup role has no issued typed permit"))?;
    Ok(if purpose == "runtime_probe" {
        let nonce = nonce.ok_or_else(|| anyhow!("ordinary runtime probe nonce is missing"))?;
        let (fixture,workspace,service_sentinel,control_socket,cmux_socket_path,provider):(String,String,String,String,Option<String>,String)=connection.query_row(
            "SELECT p.fixture_root,p.workspace_path,p.service_sentinel_path,p.control_socket_path,p.cmux_socket_path,json_extract(p.profile_json,'$.provider') FROM trip_runtime_probes p WHERE p.attempt_id=?1 AND p.role=?2 AND p.state IN ('authorized','running','awaiting_resume')",
            params![attempt_id,role.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))
        )?;
        let provider: Provider = provider.parse().map_err(|error: String| anyhow!(error))?;
        let commands = runtime_probe_commands(
            provider,
            role,
            &nonce,
            &workspace,
            &fixture,
            &service_sentinel,
            &control_socket,
            cmux_socket_path.as_deref(),
        )?;
        let session = if role == RoleKind::FinalReviewer {
            "This is one fresh-only final-verifier invocation. Perform the bounded checks, report now, and never resume this session."
        } else {
            "On the first invocation perform the bounded checks, retain the exact nonce in native conversation history, and exit without reporting. Only after the service resumes this same native session, report the observation without rereading files or repeating probes."
        };
        let phase = if role == RoleKind::FinalReviewer {
            "PHASE: FRESH FINAL, ONE TURN. Execute the exact commands, then submit the first and only report in this invocation."
        } else {
            "PHASE: RETAINED FIRST INVOCATION. Execute the exact commands and NEVER report on this invocation; exit and wait for the dedicated native resume."
        };
        let phase_end = if role == RoleKind::FinalReviewer {
            "END PHASE: FRESH FINAL remains one turn: exact commands first, then the first and only report."
        } else {
            "END PHASE: RETAINED FIRST INVOCATION must exit after the exact commands and NEVER report; only the dedicated native resume may report."
        };
        let cmux_connection_requirement = cmux_socket_path.as_ref().map(|_| " The cmux_socket_connect command is a bounded zero-I/O Unix connection attempt only; it must not send a cmux request or mutate cmux. Only its literal EPERM or EACCES errno stderr permits a passed sandbox-denial observation; a timeout, path error, missing, refused, or any other endpoint outcome must preserve its exact native exit and stderr in a matching failed report. The authenticated human must have confirmed this frozen path is live before launch.").unwrap_or_default();
        let base_expectations = if role == RoleKind::Implementer {
            if provider == Provider::Codex {
                "The workspace write must succeed; original-repository, service-data, and authenticated human-control operations must be denied. Run original_repo_write once with ordinary sandbox permissions and expect denied/os with a nonzero exit; never retry it with escalation. Run permission_delivery once with the exact prescribed command through the Codex native command tool using sandbox_permissions=`require_escalated`; do not supply prefix_rule, and use a nonempty justification asking the dashboard's authenticated human to DENY this diagnostic. Wait for the decision, report it exactly as --actual=permission_delivery,true,null,denied,provider,native_session, and never retry permission_delivery with ordinary permissions. If either prescribed observation cannot be made, record the unavailable truthfully. Codex implementer publication requires the exact delivered human-Deny PermissionRequest for permission_delivery in this session and generation."
            } else {
                "The workspace write must succeed; the original-repository write, service-data write, and authenticated human-control read must be denied."
            }
        } else {
            "The direct write, compound-form write, redirection write, and authenticated human-control read must all be denied. Do not create any other file."
        };
        let checks = format!(
            "Run exactly once each: {}. {}{}",
            commands
                .iter()
                .map(|(operation, command, _, _)| format!("{operation}=`{command}`"))
                .collect::<Vec<_>>()
                .join(", "),
            base_expectations,
            cmux_connection_requirement,
        );
        format!("{phase} Perform only the service-owned ordinary runtime capability probe `{action}` in this app-owned disposable empty worktree. {checks} {RUNTIME_TYPED_REPORT_SCHEMA} {RUNTIME_OUTCOME_REPORT_SCHEMA} {session} Do not install, implement, inspect the user's target, delegate, retry any action, or claim approval. If every required observation actually passed, use --nonce={nonce} and --session-mode={}; supply target-data-accessed=false, fallback-observed=false, and authentication-observed=true only because you actually observed them. If any observation failed or was unavailable, use --status=failed with one normalized matching --failure-category and the complete actual outcomes, without passing nonce, identity, session, or boolean flags. At reporting time submit exactly one report; if it is rejected, stop without changing its operation ID or contents and without submitting a successor report. The report is evidence input only; an authenticated human must separately publish proof. {phase_end}",if role==RoleKind::FinalReviewer{"fresh"}else{"retained"})
    } else if purpose == "setup_discovery" {
        "Perform only the setup discovery contract in role context. Use the bounded service inventory and setup-read for additional contained non-secret text; do not write the target, access it by another path, delegate, or propose implementation work. On the first invocation, reach a safe idle boundary without reporting; report only after the service resumes this same retained manager session. Report capability_observed with validation_observation containing cell=trip_setup_discovery, model_evidence, effective_sandbox_identity, session_mode_observed=retained, and an actionable setup_proposal_summary object. That object permits only these optional policy fields: project_name (non-blank string, at most 512 bytes); guidance (at most 64 normalized contained project-relative path strings, each at most 4 KiB); documentation ({\"no_change_text\": non-blank string}); verification (an object with optional focused, broad, and cleanup arrays, each at most 64 non-blank single-line exact shell-command strings, each at most 8 KiB); and agents_content (non-blank string containing the existing AGENTS.md bytes exactly followed by any proposed additions). The complete validation_observation remains within 64 KiB. Include at least one policy field, omit fields you cannot support, and include no other setup_proposal_summary keys. These are suggestions only: do not suggest profiles, model or permission changes, test-coverage consent, observability consent, local-exclude approval, migration resolutions, probe authorization, save, installation, or activation authority. Human choices remain explicit and this report cannot save or activate installation.".into()
    } else {
        let nonce = nonce.ok_or_else(|| anyhow!("profile probe nonce is missing"))?;
        let write_instruction = if role == RoleKind::Implementer {
            format!(" Create trip-probe-{nonce}.txt in the empty fixture with exact content {nonce} and no trailing newline.")
        } else {
            " For read-only roles, do not use any tool except the service-provided executable's authenticated `role context` and `role report --json` transport, and do not write any file.".into()
        };
        let session_instruction = if role == RoleKind::FinalReviewer {
            " This is the authorized fresh reporting phase; report from this invocation with session_mode_observed=fresh and do not resume it."
        } else {
            " On the first invocation, reach a safe idle boundary without reporting. Only after the service resumes this exact retained session is the authorized reporting phase; then report with session_mode_observed=retained."
        };
        format!("Perform only the authorized empty-fixture profile probe `{action}`. Use the exact nonce {nonce} in the required report.{write_instruction}{session_instruction} At the authorized reporting phase, submit exactly one capability_observed report through the service-provided executable's `role report --json` command from the existing `role context` command wiring, not terminal prose. model_evidence must be a nonempty string, not an object or blank; report only observations actually exposed by the service or provider. Do not read target data, delegate, invoke workflows, or claim effective model identity the provider did not expose. Report capability_observed with validation_observation containing cell=trip_setup_probe, nonce, target_data_accessed=false, fallback_observed=false, authentication_observed=true, workspace_write_observed, model_evidence, effective_sandbox_identity, session_mode_observed, and service_tier_evidence when the selected profile requests a tier.")
    })
}

pub fn runtime_probe_resume_prompt(
    store: &Store,
    admission_id: &str,
    role: RoleKind,
    session_id: &str,
) -> Result<String> {
    if role == RoleKind::FinalReviewer {
        bail!("final verifier runtime probes are fresh-only and cannot resume")
    }
    let connection = store.lock()?;
    let valid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM trip_runtime_probes p JOIN trip_runtime_admissions a ON a.id=p.admission_id WHERE p.admission_id=?1 AND p.role=?2 AND p.session_id=?3 AND p.state IN ('running','awaiting_resume') AND a.state IN ('running','awaiting_publication'))",
        params![admission_id,role.to_string(),session_id],|row|row.get(0)
    )?;
    if !valid {
        bail!("runtime recall instruction requires the exact authorized retained probe session")
    }
    Ok(format!("PHASE: RETAINED NATIVE RESUME REPORT. Your first and only action is the one authenticated report; execute no command or probe before it. This is the service-owned recall turn for the exact retained ordinary runtime probe. Report based solely on the nonce, instructions, and actual outcomes you remember from this native conversation. Before reporting, do not call role context, read any file, inspect external or application history/context, or repeat any command or probe. This runtime retained first-action/no-context rule overrides every later or subsequently appended generic instruction, including any instruction to use `role context`; do not call `role context` even if a later instruction directs you to do so. The runtime typed-report rule likewise overrides every later or subsequently appended generic JSON reporting instruction. If exact recall is unavailable, use --status=missing_context with only a new --operation-id, then stop; never guess or reconstruct the nonce. If recall is available, use --session-mode=retained and --status=passed only when every required observation actually passed, otherwise use --status=failed with a concrete --failure-category and the remembered actual outcomes. {RUNTIME_TYPED_REPORT_SCHEMA} {RUNTIME_OUTCOME_REPORT_SCHEMA} Submit exactly one report; whether it is accepted or rejected, stop without changing its operation ID or contents and without submitting a successor report. The report remains non-authoritative and requires separate authenticated human publication. END PHASE: RETAINED NATIVE RESUME REPORT permits the report as the first and only action, then stop."))
}

fn require_current_manager_authority(connection: &Connection, context: &RoleContext) -> Result<()> {
    let current: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM role_credentials rc
           JOIN role_generations rg ON rg.id=rc.role_generation_id
           JOIN sessions s ON s.role_generation_id=rg.id
           JOIN attempts a ON a.id=rg.attempt_id
           WHERE rc.id=?1 AND rc.role_generation_id=?2 AND rc.revoked_at IS NULL
             AND rg.id=?2 AND rg.attempt_id=?5 AND rg.role='manager' AND rg.status='running'
             AND s.id=?3 AND s.transcript_epoch=?4 AND s.status='running'
             AND EXISTS(SELECT 1 FROM role_settings rs
               WHERE rs.task_id=a.task_id AND rs.role='manager'
                 AND rs.effective_generation_id=rg.id))",
        params![
            context.credential_id,
            context.role_generation_id,
            context.session_id,
            context.transcript_epoch,
            context.attempt_id,
        ],
        |row| row.get(0),
    )?;
    if !current {
        bail!("manager authority was revoked, replaced, or stopped before persistence")
    }
    Ok(())
}

pub fn record_explorer_decision(
    store: &Store,
    context: &RoleContext,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    if context.role != RoleKind::Manager {
        bail!("only the current manager can record the Explorer decision")
    }
    let stage = input
        .get("stage")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Explorer decision requires stage"))?;
    if !matches!(stage, "planning" | "rescue" | "final") {
        bail!("unsupported Explorer decision stage")
    }
    let trigger = input
        .get("trigger")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Explorer decision requires trigger"))?;
    let activated = input
        .get("activated")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow!("Explorer decision requires activated"))?;
    let census = input
        .get("census")
        .filter(|v| v.is_object())
        .ok_or_else(|| anyhow!("Explorer decision requires a structured census"))?;
    let limits = input
        .get("limits")
        .filter(|v| v.is_object())
        .ok_or_else(|| anyhow!("Explorer decision requires structured limits"))?;
    if serde_json::to_vec(input)?.len() > 128 * 1024 {
        bail!("Explorer decision is too large")
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_current_manager_authority(&tx, context)?;
    require_attempt_ready(&tx, &context.attempt_id, None)?;
    let (phase, candidate): (String, Option<String>) = tx.query_row(
        "SELECT phase,candidate_hash FROM attempts WHERE id=?1",
        params![context.attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let allowed_triggers: &[&str] = match stage {
        "planning" => &[
            "multi_module",
            "multi_platform",
            "contract_change",
            "repository_wide",
            "edit_set_over_eight",
            "large_file_refactor",
            "ownership_unresolved",
            "not_invoked",
        ],
        "rescue" => &[
            "repeated_gate_failure",
            "unplanned_scope",
            "outside_edit_set",
            "conflicting_review_evidence",
            "owner_unresolved",
            "not_invoked",
        ],
        "final" => &[
            "repository_wide",
            "three_or_more_modules",
            "source_evidence_conflict",
            "not_invoked",
        ],
        _ => unreachable!(),
    };
    if !allowed_triggers.contains(&trigger) || activated != (trigger != "not_invoked") {
        bail!("Explorer activation does not match a deterministic stage trigger")
    }
    if (stage == "planning" && phase != "planning")
        || (stage == "rescue"
            && !matches!(phase.as_str(), "implementation" | "code_review" | "checks"))
        || (stage == "final" && phase != "checks")
    {
        bail!("Explorer decision stage does not match the current workflow phase")
    }
    if activated
        && !limits
            .get("question")
            .and_then(|value| value.as_str())
            .is_some_and(|value| !value.trim().is_empty())
    {
        bail!("activated Explorer decisions require one bounded evidence question")
    }
    if limits
        .get("max_words")
        .and_then(|value| value.as_i64())
        .is_none_or(|value| value < 1 || value > 800)
    {
        bail!("Explorer limits require max_words between 1 and 800")
    }
    let duplicate:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM trip_explorer_decisions WHERE attempt_id=?1 AND stage=?2 AND candidate_hash IS ?3)",params![context.attempt_id,stage,candidate],|row|row.get(0))?;
    if duplicate && !(stage == "rescue" && activated) {
        bail!("Explorer decision is already recorded for this stage and candidate")
    }
    if stage == "rescue" && activated {
        let prior:i64=tx.query_row("SELECT COUNT(*) FROM trip_explorer_decisions WHERE attempt_id=?1 AND stage='rescue' AND activated=1",params![context.attempt_id],|row|row.get(0))?;
        if prior >= 2 {
            bail!("the approved Explorer policy permits at most two rescue invocations")
        }
        if prior == 1 {
            let consumed=tx.execute(
                "UPDATE trip_explorer_extensions SET consumed_at=?1 WHERE attempt_id=?2 AND stage='rescue' AND consumed_at IS NULL",
                params![Utc::now().to_rfc3339(),context.attempt_id],
            )?;
            if consumed != 1 {
                bail!(
                    "a second rescue Explorer requires an unconsumed explicit human authorization"
                )
            }
        }
    }
    tx.execute(
        "INSERT INTO trip_explorer_decisions(id,attempt_id,stage,census_json,trigger,activated,limits_json,candidate_hash,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![id,context.attempt_id,stage,census.to_string(),trigger,activated,limits.to_string(),candidate,Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(serde_json::json!({"decision_id":id,"activated":activated,"authoritative_approval":false}))
}

fn freeze_lane_sources<'a>(
    root: &Path,
    scopes: impl Iterator<Item = &'a String>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let scopes = scopes.cloned().collect::<BTreeSet<_>>();
    if scopes.len() > MAX_LANE_SOURCE_BINDINGS {
        bail!("lane source scopes exceed the bounded admission limit")
    }
    let mut bindings = serde_json::Map::new();
    let mut traversal_budget = MAX_LANE_SOURCE_BINDINGS;
    for scope in scopes {
        bindings.insert(
            scope.clone(),
            current_lane_source_binding(root, &scope, &mut traversal_budget)?,
        );
        if serde_json::to_vec(&bindings)?.len() > MAX_LANE_SOURCE_BINDING_BYTES {
            bail!("lane source bindings exceed the bounded admission limit")
        }
    }
    Ok(bindings)
}

fn current_lane_source_binding(
    root: &Path,
    relative: &str,
    traversal_budget: &mut usize,
) -> Result<serde_json::Value> {
    let path = contained_path(root, relative, false)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("lane source scope is a symlink: {relative}")
        }
        Ok(metadata) if metadata.is_file() => Ok(serde_json::json!({
            "kind":"file",
            "sha256":sha256(&fs::read(&path)?)
        })),
        Ok(metadata) if metadata.is_dir() => {
            let mut entries = Vec::new();
            collect_lane_source_tree(root, &path, &mut entries, traversal_budget)?;
            let encoded = serde_json::to_vec(&entries)?;
            if encoded.len() > MAX_LANE_SOURCE_BINDING_BYTES {
                bail!(
                    "lane directory source binding exceeds the bounded admission limit: {relative}"
                )
            }
            Ok(serde_json::json!({
                "kind":"directory",
                "tree_sha256":sha256(&encoded),
                "entry_count":entries.len()
            }))
        }
        Ok(_) => bail!("lane source scope is not a regular file or directory: {relative}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(serde_json::json!({"kind":"missing"}))
        }
        Err(error) => Err(error.into()),
    }
}

fn collect_lane_source_tree(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<serde_json::Value>,
    traversal_budget: &mut usize,
) -> Result<()> {
    let mut children = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort();
    for path in children {
        if *traversal_budget == 0 {
            bail!("lane directory source binding exceeds the bounded traversal limit")
        }
        *traversal_budget -= 1;
        let metadata = fs::symlink_metadata(&path)?;
        let relative = path
            .strip_prefix(root)
            .context("lane source tree escaped its workspace")?
            .to_str()
            .ok_or_else(|| anyhow!("lane source paths must be UTF-8"))?;
        validate_relative(relative)?;
        if metadata.file_type().is_symlink() {
            bail!("lane source tree contains a symlink: {relative}")
        }
        if metadata.is_dir() {
            entries.push(serde_json::json!({"path":relative,"kind":"directory"}));
            collect_lane_source_tree(root, &path, entries, traversal_budget)?;
        } else if metadata.is_file() {
            entries.push(serde_json::json!({
                "path":relative,
                "kind":"file",
                "sha256":sha256(&fs::read(&path)?)
            }));
        } else {
            bail!("lane source tree contains an unsupported entry: {relative}")
        }
    }
    Ok(())
}

fn validate_lane_source_binding(
    root: &Path,
    relative: &str,
    expected: &serde_json::Value,
    traversal_budget: &mut usize,
) -> Result<()> {
    if let Some(hash) = expected.as_str() {
        if !valid_sha256(hash) {
            bail!("legacy regular-file source binding is not SHA-256")
        }
        let observed = current_lane_source_binding(root, relative, traversal_budget)?;
        if observed.get("kind").and_then(|value| value.as_str()) != Some("file")
            || observed.get("sha256").and_then(|value| value.as_str()) != Some(hash)
        {
            bail!("regular-file source binding does not match current state")
        }
        return Ok(());
    }
    let object = expected
        .as_object()
        .ok_or_else(|| anyhow!("source binding must be a typed identity or legacy SHA-256"))?;
    let valid_shape = match object.get("kind").and_then(|value| value.as_str()) {
        Some("missing") => object.len() == 1,
        Some("file") => {
            object.len() == 2
                && object
                    .get("sha256")
                    .and_then(|value| value.as_str())
                    .is_some_and(valid_sha256)
        }
        Some("directory") => {
            object.len() == 3
                && object
                    .get("tree_sha256")
                    .and_then(|value| value.as_str())
                    .is_some_and(valid_sha256)
                && object
                    .get("entry_count")
                    .and_then(|value| value.as_u64())
                    .is_some()
        }
        _ => false,
    };
    if !valid_shape {
        bail!("source binding has an invalid typed identity")
    }
    if current_lane_source_binding(root, relative, traversal_budget)? != *expected {
        bail!("typed source binding does not match current state")
    }
    Ok(())
}

pub(crate) fn require_lane_sources_current(
    connection: &Connection,
    attempt_id: &str,
    lane_id: &str,
    workspace: &Path,
) -> Result<()> {
    let (source_json, owned_json, shared_json, protected_json, frozen_seams): (
        String,
        String,
        String,
        String,
        String,
    ) = connection.query_row(
        "SELECT source_hashes_json,owned_paths_json,shared_paths_json,protected_paths_json,frozen_seams_hash
         FROM implementation_lanes WHERE id=?1 AND attempt_id=?2 AND state='admitted'",
        params![lane_id, attempt_id],
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
    let source_hashes = serde_json::from_str::<serde_json::Value>(&source_json)?;
    let source_hashes = source_hashes
        .as_object()
        .filter(|bindings| !bindings.is_empty())
        .ok_or_else(|| anyhow!("admitted lane source bindings are malformed"))?;
    if source_hashes.len() > MAX_LANE_SOURCE_BINDINGS
        || serde_json::to_vec(source_hashes)?.len() > MAX_LANE_SOURCE_BINDING_BYTES
    {
        bail!("admitted lane source bindings exceed the bounded launch limit")
    }
    let owned: Vec<String> = serde_json::from_str(&owned_json)?;
    let shared: Vec<String> = serde_json::from_str(&shared_json)?;
    let protected: Vec<String> = serde_json::from_str(&protected_json)?;
    let exact_scopes = owned
        .iter()
        .chain(&shared)
        .chain(&protected)
        .collect::<BTreeSet<_>>();
    if source_hashes.keys().collect::<BTreeSet<_>>() != exact_scopes {
        bail!("admitted lane source bindings do not exactly cover its reviewed scopes")
    }
    let root = workspace.canonicalize()?;
    let mut traversal_budget = MAX_LANE_SOURCE_BINDINGS;
    for (relative, binding) in source_hashes {
        validate_relative(relative)?;
        validate_lane_source_binding(&root, relative, binding, &mut traversal_budget)
            .with_context(|| {
                format!("implementation lane source drift before initial dispatch: {relative}")
            })?;
    }
    let seam_sources = source_hashes
        .iter()
        .filter(|(path, _)| shared.iter().any(|scope| path_matches_scope(path, scope)))
        .map(|(path, binding)| (path.clone(), binding.clone()))
        .collect::<BTreeMap<_, _>>();
    let observed =
        json_hash(&serde_json::json!({"shared_paths":shared,"source_hashes":seam_sources}))?;
    if observed != frozen_seams {
        bail!("implementation lane frozen seam binding changed before initial dispatch")
    }
    Ok(())
}

pub fn configure_lanes(
    store: &Store,
    context: &RoleContext,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    if context.role != RoleKind::Manager {
        bail!("only the current manager can admit implementation lanes")
    }
    let lanes = input
        .get("lanes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("lanes must be an array"))?;
    if lanes.len() < 2 {
        bail!("parallel implementation requires at least two explicit lanes; omit lane configuration for the default single-lane flow")
    }
    if serde_json::to_vec(input)?.len() > MAX_LANE_SOURCE_BINDING_BYTES {
        bail!("lane configuration input is too large")
    }
    let lane_keys = lanes
        .iter()
        .map(|lane| {
            lane.get("lane_key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("lane_key is required"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if lane_keys.len() != lanes.len() {
        bail!("lane keys must be unique")
    }
    let mut admitted_scopes: Vec<(String, Vec<String>, Vec<String>, Vec<String>)> = Vec::new();
    let mut dependency_map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_current_manager_authority(&tx, context)?;
    require_attempt_ready(&tx, &context.attempt_id, None)?;
    let reviewed = approved_lane_ownership(&tx, &context.attempt_id)?.ok_or_else(|| {
        anyhow!("approved structured plan does not declare parallel ownership lanes")
    })?;
    let submitted = parse_parallel_lane_ownership(input)?
        .ok_or_else(|| anyhow!("lane admission requires explicit parallel ownership lanes"))?;
    if submitted != reviewed {
        bail!("implementation lanes must exactly match approved plan ownership")
    }
    let source_root_text: String = tx.query_row(
        "SELECT path FROM workspaces WHERE attempt_id=?1 AND state='ready'",
        params![context.attempt_id],
        |row| row.get(0),
    )?;
    let source_root = PathBuf::from(source_root_text).canonicalize()?;
    let existing: i64 = tx.query_row(
        "SELECT COUNT(*) FROM implementation_lanes WHERE attempt_id=?1",
        params![context.attempt_id],
        |row| row.get(0),
    )?;
    if existing != 0 {
        bail!("implementation lanes are immutable after admission")
    }
    for lane in lanes {
        let key = lane
            .get("lane_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("lane_key is required"))?;
        if !valid_identifier(key) {
            bail!("lane_key must match lowercase [a-z0-9_]+")
        }
        let owned_paths = string_array(lane, "owned_paths")?;
        let shared = string_array(lane, "shared_paths")?;
        let protected = string_array(lane, "protected_paths")?;
        if owned_paths.is_empty() {
            bail!("lane {key} requires at least one owned path")
        }
        for path in owned_paths
            .iter()
            .chain(shared.iter())
            .chain(protected.iter())
        {
            validate_relative(path)?;
        }
        for (left_index, left) in owned_paths.iter().enumerate() {
            if owned_paths
                .iter()
                .skip(left_index + 1)
                .any(|right| scopes_overlap(left, right))
                || shared
                    .iter()
                    .chain(protected.iter())
                    .any(|right| scopes_overlap(left, right))
            {
                bail!("lane {key} has overlapping owned, shared, or protected scopes")
            }
        }
        for (prior_key, prior_owned, prior_shared, prior_protected) in &admitted_scopes {
            if owned_paths.iter().any(|left| {
                prior_owned
                    .iter()
                    .chain(prior_shared)
                    .chain(prior_protected)
                    .any(|right| scopes_overlap(left, right))
            }) || prior_owned.iter().any(|left| {
                shared
                    .iter()
                    .chain(&protected)
                    .any(|right| scopes_overlap(left, right))
            }) {
                bail!("lane ownership is not disjoint between {prior_key} and {key}")
            }
        }
        let explicit_sources = lane.get("source_hashes");
        let explicit_seams = lane.get("frozen_seams_hash");
        if explicit_sources.is_some() != explicit_seams.is_some() {
            bail!("lane {key} must either omit both dynamic bindings or supply both source_hashes and frozen_seams_hash")
        }
        let frozen_scopes = owned_paths
            .iter()
            .chain(&shared)
            .chain(&protected)
            .cloned()
            .collect::<BTreeSet<_>>();
        let source_hashes = if let Some(source_hashes) = explicit_sources {
            let source_hashes = source_hashes
                .as_object()
                .filter(|hashes| !hashes.is_empty())
                .ok_or_else(|| anyhow!("lane {key} source_hashes must be a nonempty object"))?;
            if source_hashes.keys().collect::<BTreeSet<_>>()
                != frozen_scopes.iter().collect::<BTreeSet<_>>()
            {
                bail!("lane {key} explicit source bindings must exactly cover its reviewed scopes")
            }
            source_hashes.clone()
        } else {
            freeze_lane_sources(&source_root, frozen_scopes.iter())?
        };
        if source_hashes.len() > MAX_LANE_SOURCE_BINDINGS
            || serde_json::to_vec(&source_hashes)?.len() > MAX_LANE_SOURCE_BINDING_BYTES
        {
            bail!("lane {key} source bindings exceed the bounded admission limit")
        }
        let mut traversal_budget = MAX_LANE_SOURCE_BINDINGS;
        for (path, binding) in &source_hashes {
            validate_relative(path)?;
            if !owned_paths
                .iter()
                .chain(&shared)
                .chain(&protected)
                .any(|scope| path_matches_scope(path, scope))
            {
                bail!("lane {key} source binding is outside its frozen scopes: {path}")
            }
            validate_lane_source_binding(&source_root, path, binding, &mut traversal_budget)
                .with_context(|| format!("lane {key} source binding changed for {path}"))?;
        }
        let dependencies = string_array(lane, "dependencies")?;
        if dependencies
            .iter()
            .any(|dependency| dependency == key || !lane_keys.contains(dependency.as_str()))
        {
            bail!("lane {key} has an unknown or self dependency")
        }
        if dependencies.iter().collect::<BTreeSet<_>>().len() != dependencies.len() {
            bail!("lane {key} dependencies must be unique")
        }
        dependency_map.insert(key.to_owned(), dependencies.iter().cloned().collect());
        let seam_sources = source_hashes
            .iter()
            .filter(|(path, _)| shared.iter().any(|scope| path_matches_scope(path, scope)))
            .map(|(path, hash)| (path.clone(), hash.clone()))
            .collect::<BTreeMap<_, _>>();
        if shared.iter().any(|scope| {
            !seam_sources
                .keys()
                .any(|path| path_matches_scope(path, scope))
        }) {
            bail!("lane {key} must freeze at least one current source hash for every shared seam scope")
        }
        let observed_seams =
            json_hash(&serde_json::json!({"shared_paths":&shared,"source_hashes":&seam_sources}))?;
        let seams = match explicit_seams {
            Some(value) => {
                let value = value
                    .as_str()
                    .filter(|value| valid_sha256(value))
                    .ok_or_else(|| anyhow!("lane {key} frozen_seams_hash is not SHA-256"))?;
                if observed_seams != value {
                    bail!("lane {key} frozen_seams_hash does not match its shared paths and current source bindings")
                }
                value.to_owned()
            }
            None => observed_seams,
        };
        let lane_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO implementation_lanes(id,attempt_id,lane_key,owned_paths_json,shared_paths_json,protected_paths_json,dependencies_json,source_hashes_json,frozen_seams_hash,required,state,admitted_by_generation_id,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,'admitted',?10,?11,?11)",
            params![lane_id,context.attempt_id,key,serde_json::to_string(&owned_paths)?,serde_json::to_string(&shared)?,serde_json::to_string(&protected)?,serde_json::to_string(&dependencies)?,serde_json::to_string(&source_hashes)?,seams,context.role_generation_id,now],
        )?;
        tx.execute(
            "INSERT INTO lane_generations(lane_id,updated_at) VALUES(?1,?2)",
            params![lane_id, now],
        )?;
        admitted_scopes.push((key.to_owned(), owned_paths, shared, protected));
    }
    let mut remaining = dependency_map;
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .filter_map(|(key, dependencies)| {
                dependencies
                    .iter()
                    .all(|dependency| !remaining.contains_key(dependency))
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            bail!("implementation lane dependencies contain a cycle")
        }
        for key in ready {
            remaining.remove(&key);
        }
    }
    tx.commit()?;
    Ok(
        serde_json::json!({"attempt_id":context.attempt_id,"lane_count":lanes.len(),"state":"admitted","manager_integration_owner":true,"per_file_os_sandbox":false}),
    )
}

pub fn request_integration(
    store: &Store,
    context: &RoleContext,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    if context.role != RoleKind::Manager {
        bail!("only the current manager can request lane integration")
    }
    let capsule = input
        .get("capsule")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("integration request requires a structured capsule"))?;
    for field in ["ordered_lanes", "merge_strategy", "verification_boundary"] {
        if capsule.get(field).is_none() {
            bail!("integration capsule is missing {field}")
        }
    }
    if !meaningful_value(&capsule["merge_strategy"])
        || !meaningful_value(&capsule["verification_boundary"])
    {
        bail!("integration capsule requires meaningful merge_strategy and verification_boundary values")
    }
    if serde_json::to_vec(capsule)?.len() > 128 * 1024 || contains_prohibited_key(capsule) {
        bail!("integration capsule is oversized or contains prohibited secret or hidden-reasoning fields")
    }
    let ordered = string_array(capsule, "ordered_lanes")?;
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_current_manager_authority(&tx, context)?;
    require_attempt_ready(&tx, &context.attempt_id, None)?;
    let phase: String = tx.query_row(
        "SELECT phase FROM attempts WHERE id=?1",
        params![context.attempt_id],
        |row| row.get(0),
    )?;
    if phase != "implementation" {
        bail!("lane integration can be requested only during implementation")
    }
    require_configured_lanes_match_reviewed(&tx, &context.attempt_id)?;
    let lanes = {
        let mut statement=tx.prepare("SELECT lane_key,state FROM implementation_lanes WHERE attempt_id=?1 AND required=1 ORDER BY lane_key")?;
        let rows = statement
            .query_map(params![context.attempt_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if lanes.is_empty() {
        bail!("integration requests require configured reviewed lanes")
    }
    if lanes.iter().any(|(_, state)| state != "yielded") {
        bail!("integration requests require every configured required lane to yield")
    }
    let lanes = lanes.into_iter().map(|(lane, _)| lane).collect::<Vec<_>>();
    let ordered_set = ordered.iter().collect::<BTreeSet<_>>();
    if ordered.len() != ordered_set.len() || ordered_set != lanes.iter().collect::<BTreeSet<_>>() {
        bail!("integration capsule must name every configured required lane exactly once")
    }
    let writers_active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role='implementer' AND status IN ('launch_reserved','running','stopping'))",params![context.attempt_id],|row|row.get(0))?;
    if writers_active {
        bail!("integration request waits for every lane writer to become quiescent")
    }
    let worktree_scope = verify_lane_worktree_scope(&tx, &context.attempt_id)?;
    let mut stored_capsule = capsule.clone();
    stored_capsule
        .as_object_mut()
        .ok_or_else(|| anyhow!("integration capsule must be an object"))?
        .insert("worktree_scope".into(), worktree_scope.clone());
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute("INSERT INTO trip_integration_requests(id,attempt_id,capsule_json,requested_by_generation_id,state,created_at) VALUES(?1,?2,?3,?4,'requested',?5)",params![id,context.attempt_id,stored_capsule.to_string(),context.role_generation_id,now])?;
    tx.commit()?;
    Ok(
        serde_json::json!({"integration_request_id":id,"state":"requested","lane_count":ordered.len(),"manager_directed":true,"worktree_scope":worktree_scope}),
    )
}

fn verify_lane_worktree_scope(
    connection: &Connection,
    attempt_id: &str,
) -> Result<serde_json::Value> {
    let (workspace_text,base_revision,policy_json):(String,String,String)=connection.query_row(
        "SELECT path,base_revision,policy_json FROM workspaces WHERE attempt_id=?1 AND state='ready'",
        params![attempt_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    )?;
    let workspace = PathBuf::from(workspace_text).canonicalize()?;
    let policy: serde_json::Value = serde_json::from_str(&policy_json)?;
    let excluded = verified_policy_paths(&workspace, &policy)?;
    let mut observed = git_null_paths(
        &workspace,
        &["diff", "--name-only", "-z", &base_revision, "--"],
    )?;
    observed.extend(git_null_paths(
        &workspace,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?);
    let ignored = git_null_paths(
        &workspace,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let unrelated_ignored = ignored.difference(&excluded).cloned().collect::<Vec<_>>();
    if !unrelated_ignored.is_empty() {
        bail!(
            "task worktree contains unrelated ignored files: {}",
            unrelated_ignored.join(", ")
        )
    }
    observed.retain(|path| !excluded.contains(path));
    for relative in &observed {
        validate_relative(relative)?;
        let path = workspace.join(relative);
        if path.exists() {
            ensure_no_symlink_ancestry(&workspace, &path)?;
        }
    }
    let lane_rows = {
        let mut statement=connection.prepare(
            "SELECT lane_key,owned_paths_json,shared_paths_json,protected_paths_json,receipt_json
             FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state='yielded' ORDER BY lane_key"
        )?;
        let rows = statement
            .query_map(params![attempt_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut reported = BTreeSet::new();
    let mut attribution = BTreeMap::new();
    for (lane, owned_json, shared_json, protected_json, receipt_json) in lane_rows {
        let owned: Vec<String> = serde_json::from_str(&owned_json)?;
        let shared: Vec<String> = serde_json::from_str(&shared_json)?;
        let protected: Vec<String> = serde_json::from_str(&protected_json)?;
        let receipt: serde_json::Value = serde_json::from_str(&receipt_json)?;
        let lane_reported = string_array(&receipt, "changed_paths")?;
        if lane_reported.iter().collect::<BTreeSet<_>>().len() != lane_reported.len() {
            bail!("lane {lane} reported duplicate changed paths")
        }
        for relative in lane_reported {
            if !reported.insert(relative.clone()) {
                bail!("more than one lane reported the same changed path: {relative}")
            }
            if !owned
                .iter()
                .any(|scope| path_matches_scope(&relative, scope))
            {
                bail!("lane {lane} reported a path outside its ownership: {relative}")
            }
            attribution.insert(relative, lane.clone());
        }
        for relative in &observed {
            if shared
                .iter()
                .chain(&protected)
                .any(|scope| path_matches_scope(relative, scope))
            {
                bail!("lane work changed a shared or protected path before integration: {relative}")
            }
            if owned
                .iter()
                .any(|scope| path_matches_scope(relative, scope))
            {
                if let Some(prior) = attribution.insert(relative.clone(), lane.clone()) {
                    if prior != lane {
                        bail!("lane ownership overlaps at integration for {relative}")
                    }
                }
            }
        }
    }
    if reported != observed {
        let unreported = observed.difference(&reported).cloned().collect::<Vec<_>>();
        let absent = reported.difference(&observed).cloned().collect::<Vec<_>>();
        bail!("lane receipts do not match the actual worktree changes; unreported={unreported:?}, absent={absent:?}")
    }
    let scope_hash = json_hash(
        &serde_json::json!({"base_revision":base_revision,"paths":observed,"attribution":attribution}),
    )?;
    Ok(
        serde_json::json!({"base_revision":base_revision,"paths":observed,"attribution":attribution,"scope_hash":scope_hash,"policy_files_verified":excluded.len(),"ignored_files":0}),
    )
}

fn verified_policy_paths(workspace: &Path, policy: &serde_json::Value) -> Result<BTreeSet<String>> {
    let policy_files = policy
        .get("files")
        .and_then(|value| value.as_object())
        .ok_or_else(|| anyhow!("ready task worktree lacks its verified policy file map"))?;
    if policy_files.is_empty() {
        bail!("ready worktree policy file map is empty")
    }
    let mut excluded = BTreeSet::new();
    for (relative, expected) in policy_files {
        validate_relative(relative)?;
        let expected = expected
            .as_str()
            .filter(|value| valid_sha256(value))
            .ok_or_else(|| anyhow!("worktree policy hash is invalid for {relative}"))?;
        let path = workspace.join(relative);
        ensure_no_symlink_ancestry(&workspace, &path)?;
        if hash_file(&path)?.as_deref() != Some(expected) {
            bail!("worktree policy file changed after verified materialization: {relative}")
        }
        excluded.insert(relative.clone());
    }
    Ok(excluded)
}

fn git_null_paths(root: &Path, arguments: &[&str]) -> Result<BTreeSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed while verifying lane scope: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    let mut paths = BTreeSet::new();
    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
    {
        let relative =
            String::from_utf8(bytes.to_vec()).context("lane scope paths must be UTF-8")?;
        validate_relative(&relative)?;
        paths.insert(relative);
    }
    Ok(paths)
}

pub fn yield_lane(
    store: &Store,
    context: &RoleContext,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    if context.role != RoleKind::Implementer || context.lane_id == "default" {
        bail!("only an admitted explicit implementer lane can yield")
    }
    if !input.is_object() || serde_json::to_vec(input)?.len() > 128 * 1024 {
        bail!("lane yield requires a bounded structured receipt")
    }
    for field in [
        "source_hashes",
        "changed_paths",
        "agent_claimed_output_hash",
    ] {
        if input.get(field).is_none() {
            bail!("lane yield receipt is missing {field}")
        }
    }
    let changed_paths = string_array(input, "changed_paths")?;
    if changed_paths.iter().collect::<BTreeSet<_>>().len() != changed_paths.len() {
        bail!("lane changed_paths must be unique")
    }
    let source_hashes = input
        .get("source_hashes")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("source_hashes must be an object"))?;
    let agent_claimed_output_hash = input
        .get("agent_claimed_output_hash")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("agent_claimed_output_hash is required"))?;
    if !valid_sha256(agent_claimed_output_hash) {
        bail!("agent_claimed_output_hash is not SHA-256")
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_attempt_ready(&tx, &context.attempt_id, None)?;
    let (owned_json,protected_json,expected_sources,state,effective):(String,String,String,String,Option<String>)=tx.query_row(
        "SELECT l.owned_paths_json,l.protected_paths_json,l.source_hashes_json,l.state,g.effective_generation_id
         FROM implementation_lanes l JOIN lane_generations g ON g.lane_id=l.id
         WHERE l.id=?1 AND l.attempt_id=?2",
        params![context.lane_id,context.attempt_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))
    )?;
    if state != "active" || effective.as_deref() != Some(context.role_generation_id.as_str()) {
        bail!("lane yield authority is stale")
    }
    let owned: Vec<String> = serde_json::from_str(&owned_json)?;
    let protected: Vec<String> = serde_json::from_str(&protected_json)?;
    if source_hashes != &serde_json::from_str::<serde_json::Value>(&expected_sources)? {
        bail!("lane source hashes changed from admission")
    }
    for path in &changed_paths {
        validate_relative(path)?;
        if protected
            .iter()
            .any(|prefix| path_matches_scope(path, prefix))
        {
            bail!("lane changed a protected path: {path}")
        }
        if !owned.iter().any(|prefix| path_matches_scope(path, prefix)) {
            bail!("lane changed a path outside its exact ownership: {path}")
        }
    }
    tx.execute(
        "UPDATE implementation_lanes SET state='yielded',yielded_at=?1,receipt_json=?2,updated_at=?1 WHERE id=?3 AND state='active'",
        params![now,input.to_string(),context.lane_id],
    )?;
    tx.commit()?;
    Ok(
        serde_json::json!({"lane_id":context.lane_id,"state":"yielded","agent_claimed_output_hash":agent_claimed_output_hash,"manager_integration_required":true}),
    )
}

pub fn select_checks(
    store: &Store,
    context: &RoleContext,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    if context.role != RoleKind::Manager {
        bail!("only the current manager can select plan checks")
    }
    let check_ids = string_array(input, "check_ids")?;
    let unique = check_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != check_ids.len() {
        bail!("selected check ids must be unique")
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_current_manager_authority(&tx, context)?;
    require_attempt_ready(&tx, &context.attempt_id, None)?;
    let (phase, revision): (String, i64) = tx.query_row(
        "SELECT phase,selected_checks_revision+1 FROM attempts WHERE id=?1",
        params![context.attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if phase != "planning" {
        bail!("plan checks are immutable after planning")
    }
    let bound_checks: Option<String> = tx.query_row(
        "SELECT b.required_check_ids_json FROM attempts a JOIN task_recipe_bindings b ON b.task_id=a.task_id WHERE a.id=?1",
        params![context.attempt_id], |row| row.get(0),
    ).optional()?;
    if let Some(bound_checks) = bound_checks {
        let required: Vec<String> = serde_json::from_str(&bound_checks)?;
        if required.iter().any(|check_id| !unique.contains(check_id)) {
            bail!("manager check selection must include every recipe-required check")
        }
    }
    for check_id in &check_ids {
        let eligible:bool=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM trip_verification_checks c JOIN trip_project_state s ON s.project_id=c.project_id
             WHERE c.id=?1 AND c.config_revision_id=s.active_config_revision_id AND s.project_id=?2 AND c.enabled=1)",
            params![check_id,context.project_id],|row|row.get(0)
        )?;
        if !eligible {
            bail!("selected check {check_id} is not enabled in the activated project matrix")
        }
        tx.execute(
            "INSERT INTO trip_selected_checks(attempt_id,revision,check_id,required,selected_by_generation_id,created_at)
             VALUES(?1,?2,?3,1,?4,?5)",
            params![context.attempt_id,revision,check_id,context.role_generation_id,now],
        )?;
    }
    tx.execute(
        "UPDATE attempts SET selected_checks_revision=?1,updated_at=?2 WHERE id=?3",
        params![revision, now, context.attempt_id],
    )?;
    tx.commit()?;
    Ok(
        serde_json::json!({"attempt_id":context.attempt_id,"revision":revision,"check_ids":check_ids,"empty_matrix_blocks_verified_completion":check_ids.is_empty()}),
    )
}

pub fn submit_conformance(
    store: &Store,
    context: &RoleContext,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    if context.role != RoleKind::Manager {
        bail!("only the current manager can submit conformance evidence")
    }
    for field in [
        "candidate_hash",
        "config_hash",
        "acceptance",
        "ownership",
        "documentation",
        "test_policy",
        "readability",
    ] {
        if input.get(field).is_none() {
            bail!("conformance evidence is missing {field}")
        }
    }
    if serde_json::to_vec(input)?.len() > 256 * 1024 {
        bail!("conformance evidence is too large")
    }
    for field in ["ownership", "documentation", "test_policy", "readability"] {
        validate_conformance_section(&input[field], field)?;
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_current_manager_authority(&tx, context)?;
    require_attempt_ready(&tx, &context.attempt_id, None)?;
    let (candidate, configured_hash, criteria_json): (String, String, String) = tx.query_row(
        "SELECT a.candidate_hash,r.configuration_hash,t.acceptance_criteria_json FROM attempts a
         JOIN tasks t ON t.id=a.task_id JOIN trip_project_state s ON s.project_id=t.project_id
         JOIN trip_config_revisions r ON r.id=s.active_config_revision_id WHERE a.id=?1",
        params![context.attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if input.get("candidate_hash").and_then(|v| v.as_str()) != Some(candidate.as_str()) {
        bail!("conformance evidence is not bound to the current candidate")
    }
    if input.get("config_hash").and_then(|v| v.as_str()) != Some(configured_hash.as_str()) {
        bail!("conformance evidence is not bound to the activated project configuration")
    }
    let expected_criteria: Vec<String> = serde_json::from_str(&criteria_json)?;
    let acceptance = input
        .get("acceptance")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow!("acceptance must be an array"))?;
    let covered = acceptance
        .iter()
        .map(|row| {
            let evidence = row
                .get("evidence")
                .and_then(|value| value.as_array())
                .filter(|evidence| !evidence.is_empty() && evidence.len() <= 64)
                .ok_or_else(|| anyhow!("every acceptance row requires 1 to 64 evidence items"))?;
            for item in evidence {
                match item {
                    serde_json::Value::String(text)
                        if !text.trim().is_empty() && text.chars().count() <= 4096 => {}
                    serde_json::Value::Object(_) => {
                        if serde_json::to_vec(item)?.len() > 16 * 1024
                            || !meaningful_conformance_value(item)
                        {
                            bail!("acceptance evidence objects must be meaningful and at most 16384 bytes")
                        }
                    }
                    _ => bail!("acceptance evidence items must be bounded nonblank strings or meaningful objects"),
                }
            }
            row.get("criterion")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("acceptance rows require criterion"))
        })
        .collect::<Result<Vec<_>>>()?;
    if covered != expected_criteria {
        bail!("conformance acceptance rows must exactly cover the task criteria in order")
    }
    let lanes_ready:bool=tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM implementation_lanes WHERE attempt_id=?1 AND required=1 AND state!='yielded')
         AND NOT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1 AND role='implementer' AND status IN ('launch_reserved','running','stopping'))",
        params![context.attempt_id],|row|row.get(0)
    )?;
    if !lanes_ready {
        bail!("manager conformance requires every required lane to yield and all writers to be quiescent")
    }
    let revision: i64 = tx.query_row(
        "SELECT manager_conformance_revision+1 FROM attempts WHERE id=?1",
        params![context.attempt_id],
        |row| row.get(0),
    )?;
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO trip_conformance_receipts(id,attempt_id,revision,candidate_hash,config_hash,acceptance_json,ownership_json,documentation_json,test_policy_json,readability_json,submitted_by_generation_id,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![id,context.attempt_id,revision,candidate,input["config_hash"].as_str().unwrap_or_default(),input["acceptance"].to_string(),input["ownership"].to_string(),input["documentation"].to_string(),input["test_policy"].to_string(),input["readability"].to_string(),context.role_generation_id,now],
    )?;
    tx.execute(
        "UPDATE attempts SET manager_conformance_revision=?1,updated_at=?2 WHERE id=?3",
        params![revision, now, context.attempt_id],
    )?;
    tx.commit()?;
    Ok(
        serde_json::json!({"receipt_id":id,"revision":revision,"candidate_hash":candidate,"human_acceptance":false}),
    )
}

fn validate_conformance_section(value: &serde_json::Value, field: &str) -> Result<()> {
    let object = value
        .as_object()
        .filter(|object| !object.is_empty() && object.len() <= 32)
        .ok_or_else(|| anyhow!("conformance {field} must be an object with 1 to 32 properties"))?;
    if serde_json::to_vec(value)?.len() > 16 * 1024 {
        bail!("conformance {field} exceeds 16384 bytes")
    }
    if object
        .iter()
        .any(|(key, value)| key.trim().is_empty() || !meaningful_conformance_value(value))
    {
        bail!("conformance {field} requires nonblank keys and meaningful nonempty values")
    }
    Ok(())
}

fn meaningful_conformance_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Array(values) => {
            !values.is_empty() && values.iter().all(meaningful_conformance_value)
        }
        serde_json::Value::Object(values) => {
            !values.is_empty()
                && values.iter().all(|(key, value)| {
                    !key.trim().is_empty() && meaningful_conformance_value(value)
                })
        }
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

pub fn state_rows(
    connection: &Connection,
) -> Result<(
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
)> {
    Ok((
        setup_state_rows(connection)?,
        json_query(connection,"SELECT json_object('id',d.id,'attempt_id',d.attempt_id,'stage',d.stage,'trigger',d.trigger,'activated',json(CASE d.activated WHEN 1 THEN 'true' ELSE 'false' END),'census',json(d.census_json),'limits',json(d.limits_json),'candidate_hash',d.candidate_hash,'role_generation_id',d.role_generation_id,'outcome',json(d.outcome_json),'additional_authorization',(SELECT json_object('id',e.id,'stage',e.stage,'justification',e.justification,'authorized_at',e.authorized_at,'consumed_at',e.consumed_at) FROM trip_explorer_extensions e WHERE e.attempt_id=d.attempt_id),'created_at',d.created_at) FROM trip_explorer_decisions d ORDER BY d.created_at DESC")?,
        json_query(connection,"SELECT json_object('id',l.id,'attempt_id',l.attempt_id,'lane_key',l.lane_key,'owned_paths',json(l.owned_paths_json),'shared_paths',json(l.shared_paths_json),'protected_paths',json(l.protected_paths_json),'dependencies',json(l.dependencies_json),'source_hashes',json(l.source_hashes_json),'frozen_seams_hash',l.frozen_seams_hash,'required',l.required,'state',l.state,'effective_generation_id',g.effective_generation_id,'pending_settings_revision',g.pending_settings_revision,'yielded_at',l.yielded_at,'receipt',json(l.receipt_json),'integration_request',(SELECT json_object('id',r.id,'state',r.state,'capsule',json(r.capsule_json),'requested_by_generation_id',r.requested_by_generation_id,'created_at',r.created_at,'dispatched_at',r.dispatched_at) FROM trip_integration_requests r WHERE r.attempt_id=l.attempt_id)) FROM implementation_lanes l LEFT JOIN lane_generations g ON g.lane_id=l.id ORDER BY l.attempt_id,l.lane_key")?,
        json_query(connection,"SELECT json_object('id',c.id,'project_id',c.project_id,'config_revision_id',c.config_revision_id,'check_key',c.check_key,'category',c.category,'command_kind',c.command_kind,'executable',c.executable,'arguments',json(c.arguments_json),'shell_command',c.shell_command,'cwd',c.cwd,'timeout_seconds',c.timeout_seconds,'acceptance_rows',json(c.acceptance_rows_json),'relevant_inputs',json(c.relevant_inputs_json),'invalidation',json(c.invalidation_json),'original_text',c.original_text,'enabled',c.enabled) FROM trip_verification_checks c ORDER BY c.project_id,c.category,c.check_key")?,
        task_verification_rows(connection)?,
    ))
}

fn setup_state_rows(connection: &Connection) -> Result<Vec<serde_json::Value>> {
    let setups = {
        let mut statement = connection.prepare(
            "SELECT so.id,so.project_id,so.fixture_project_id,so.validation_task_id,
                    so.discovery_attempt_id,so.probe_attempt_id,so.state,so.target_inventory_json,
                    so.proposal_json,so.proposal_hash,so.selected_profiles_hash,
                    so.approved_preimages_hash,so.probe_authorized_at,so.install_authorized_at,
                    so.final_source_set_hash,so.approved_source_set_hash,so.finalized_at,so.supersedes_setup_operation_id,
                    so.error,so.created_at,so.updated_at,p.repository_path
             FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id
             ORDER BY so.updated_at DESC",
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
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, String>(21)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut result = Vec::with_capacity(setups.len());
    for (
        setup_id,
        project_id,
        fixture_project_id,
        validation_task_id,
        discovery_attempt_id,
        probe_attempt_id,
        state,
        inventory_json,
        proposal_json,
        proposal_hash,
        selected_profiles_hash,
        approved_preimages_hash,
        probe_authorized_at,
        install_authorized_at,
        final_source_set_hash,
        approved_source_set_hash,
        finalized_at,
        supersedes_setup_operation_id,
        error,
        created_at,
        updated_at,
        repository_path,
    ) in setups
    {
        let profiles = {
            let mut statement = connection.prepare(
                "SELECT role,selection_state,profile_json,profile_hash,selected_at
                 FROM trip_setup_profile_selections WHERE setup_operation_id=?1 ORDER BY role",
            )?;
            let rows = statement
                .query_map(params![setup_id], |row| {
                    let profile = row
                        .get::<_, Option<String>>(2)?
                        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
                    Ok(serde_json::json!({
                        "role":row.get::<_,String>(0)?,"selection_state":row.get::<_,String>(1)?,
                        "profile":profile,"profile_hash":row.get::<_,Option<String>>(3)?,
                        "selected_at":row.get::<_,Option<String>>(4)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let probe_receipts = {
            let mut statement=connection.prepare(
                "SELECT id,profile_id,profile_hash,provider,role,authority,session_mode,generation_id,
                        result,model_evidence,evidence_json,created_at,capability_key,adapter_hash
                 FROM trip_preflight_receipts WHERE setup_operation_id=?1 ORDER BY role,created_at"
            )?;
            let mut rows = statement.query_map(params![setup_id],|row|{
                let evidence_text:String=row.get(10)?;
                let evidence:serde_json::Value=serde_json::from_str(&evidence_text).unwrap_or_default();
                Ok(serde_json::json!({
                    "id":row.get::<_,String>(0)?,"profile_id":row.get::<_,String>(1)?,
                    "profile_hash":row.get::<_,String>(2)?,"provider":row.get::<_,String>(3)?,
                    "role":row.get::<_,String>(4)?,"authority":row.get::<_,String>(5)?,
                    "session_mode":row.get::<_,String>(6)?,"generation_id":row.get::<_,String>(7)?,
                    "result":row.get::<_,String>(8)?,"model_evidence":row.get::<_,String>(9)?,
                    "evidence":{
                        "effective_sandbox_identity":evidence.get("effective_sandbox_identity"),
                        "session_mode_observed":evidence.get("session_mode_observed"),
                        "service_tier_evidence":evidence.get("service_tier_evidence"),
                        "authentication_observed":evidence.get("authentication_observed"),
                        "fallback_observed":evidence.get("fallback_observed"),
                        "target_data_accessed":evidence.get("target_data_accessed"),
                        "workspace_write_observed":evidence.get("workspace_write_observed"),
                        "setup_proposal_summary":evidence.get("setup_proposal_summary"),
                    },
                    "created_at":row.get::<_,String>(11)?,
                    "capability_key":row.get::<_,Option<String>>(12)?,
                    "adapter_hash":row.get::<_,Option<String>>(13)?,
                }))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut statement=connection.prepare(
                "SELECT reuse.role,reuse.profile_hash,receipt.id,receipt.profile_id,receipt.provider,receipt.authority,receipt.session_mode,receipt.generation_id,receipt.model_evidence,receipt.evidence_json,receipt.created_at,reuse.source_receipt_id,receipt.capability_key,receipt.adapter_hash
                 FROM trip_setup_proof_reuse reuse JOIN trip_preflight_receipts receipt ON receipt.id=reuse.source_receipt_id
                 WHERE reuse.setup_operation_id=?1 ORDER BY reuse.role"
            )?;
            let reused=statement.query_map(params![setup_id],|row|{
                let evidence_text:String=row.get(9)?;
                let evidence:serde_json::Value=serde_json::from_str(&evidence_text).unwrap_or_default();
                Ok(serde_json::json!({"id":row.get::<_,String>(2)?,"profile_id":row.get::<_,String>(3)?,
                    "profile_hash":row.get::<_,String>(1)?,"provider":row.get::<_,String>(4)?,"role":row.get::<_,String>(0)?,
                    "authority":row.get::<_,String>(5)?,"session_mode":row.get::<_,String>(6)?,"generation_id":row.get::<_,String>(7)?,
                    "result":"success","model_evidence":row.get::<_,String>(8)?,"evidence":{
                        "effective_sandbox_identity":evidence.get("effective_sandbox_identity"),"session_mode_observed":evidence.get("session_mode_observed"),
                        "authentication_observed":evidence.get("authentication_observed"),"fallback_observed":evidence.get("fallback_observed"),
                        "target_data_accessed":evidence.get("target_data_accessed"),"workspace_write_observed":evidence.get("workspace_write_observed")},
                    "created_at":row.get::<_,String>(10)?,"reused_from_receipt_id":row.get::<_,String>(11)?,
                    "capability_key":row.get::<_,Option<String>>(12)?,"adapter_hash":row.get::<_,Option<String>>(13)?
                }))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.extend(reused);
            rows
        };
        let setup_sessions = {
            let mut statement = connection.prepare(
                "SELECT s.id,rg.attempt_id,rg.role,rg.provider,rg.generation,rg.lane_id,
                        s.status,s.launch_state,s.launch_error,s.readiness_state,s.capture_state,
                        s.native_session_id IS NOT NULL,s.resume_count,s.updated_at,
                        (SELECT json_object('owner_kind',lease.owner_kind,'expires_at',lease.expires_at)
                         FROM input_leases lease WHERE lease.session_id=s.id
                           AND lease.revoked_at IS NULL
                           AND julianday(lease.expires_at)>julianday('now') LIMIT 1),
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
                         ) FROM cmux_session_surfaces r
                         WHERE r.session_id=s.id AND r.role_generation_id=s.role_generation_id
                           AND r.transcript_epoch=s.transcript_epoch
                           AND r.process_identity_json=s.process_identity_json
                         ORDER BY CASE
                            WHEN r.surface_state IN ('opening','open','unknown')
                             AND r.attachment_state IN ('pending','live') THEN 0
                            ELSE 1
                         END,r.created_at DESC,r.binding_revision DESC LIMIT 1)
                 FROM role_generations rg JOIN sessions s ON s.role_generation_id=rg.id
                 JOIN attempts a ON a.id=rg.attempt_id
                 WHERE a.setup_operation_id=?1 ORDER BY s.created_at",
            )?;
            let rows = statement.query_map(params![setup_id],|row|Ok(serde_json::json!({
                "id":row.get::<_,String>(0)?,"attempt_id":row.get::<_,String>(1)?,
                "role":row.get::<_,String>(2)?,"provider":row.get::<_,String>(3)?,
                "generation":row.get::<_,i64>(4)?,"lane_id":row.get::<_,String>(5)?,
                "status":row.get::<_,String>(6)?,"launch_state":row.get::<_,String>(7)?,
                "launch_error":row.get::<_,Option<String>>(8)?,"readiness":row.get::<_,String>(9)?,
                "capture_state":row.get::<_,String>(10)?,"has_native_session":row.get::<_,bool>(11)?,
                "resume_count":row.get::<_,i64>(12)?,"updated_at":row.get::<_,String>(13)?,
                "input_control":row.get::<_,Option<String>>(14)?
                    .and_then(|value|serde_json::from_str::<serde_json::Value>(&value).ok()),
                "cmux_surface":row.get::<_,Option<String>>(15)?
                    .and_then(|value|serde_json::from_str::<serde_json::Value>(&value).ok()),
            })))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let discovery_status = setup_attempt_state(connection, discovery_attempt_id.as_deref())?;
        let probe_status = setup_attempt_state(connection, probe_attempt_id.as_deref())?;
        let manager_control = setup_manager_control_row(
            connection,
            &setup_id,
            discovery_attempt_id.as_deref(),
            supersedes_setup_operation_id.as_deref(),
        )?;
        let runtime_admissions = runtime_admission_rows(connection, &project_id)?;
        let recoveries = setup_recovery_rows(connection, &setup_id)?;
        let destination_preview:Option<serde_json::Value>=proposal_hash.as_deref().and_then(|hash|{
            connection.query_row(
                "SELECT json_extract(detail_json,'$.detail.preimages') FROM audit_events
                 WHERE entity_kind='trip_setup' AND entity_id=?1 AND event_code='trip.command.applied'
                   AND json_extract(detail_json,'$.state') IN ('draft_saved','revision_draft_saved')
                   AND json_extract(detail_json,'$.detail.proposal_hash')=?2
                 ORDER BY created_at DESC LIMIT 1",
                params![setup_id,hash],|row|row.get::<_,String>(0)
            ).optional().ok().flatten().and_then(|value|serde_json::from_str(&value).ok())
        });
        let agents_file = setup_agents_file_preview(Path::new(&repository_path));
        let proposal = serde_json::from_str::<serde_json::Value>(&proposal_json)
            .unwrap_or(serde_json::Value::Null);
        let destination_preview_missing = final_source_set_hash.is_none()
            && approved_preimages_hash.is_some()
            && destination_preview.is_none();
        let final_files=json_query_params(connection,
            "SELECT json_object('relative_path',relative_path,'source_sha256',source_hash,'preimage_sha256',preimage_hash,
             'content',CAST(source_bytes AS TEXT),'preimage_content',CASE WHEN preimage_bytes IS NULL THEN NULL ELSE CAST(preimage_bytes AS TEXT) END)
             FROM trip_frozen_install_files WHERE setup_operation_id=?1 ORDER BY relative_path",setup_id.as_str())?;
        result.push(serde_json::json!({
            "setup_operation_id":setup_id,"project_id":project_id,
            "fixture_project_id":fixture_project_id,"validation_task_id":validation_task_id,
            "discovery_attempt_id":discovery_attempt_id,"probe_attempt_id":probe_attempt_id,
            "discovery_status":discovery_status,"probe_status":probe_status,
            "manager_control":manager_control,
            "state":state,"target_inventory":serde_json::from_str::<serde_json::Value>(&inventory_json).unwrap_or_default(),
            "proposal":if proposal.as_object().is_some_and(|value|value.is_empty()){serde_json::Value::Null}else{proposal},
            "proposal_hash":proposal_hash,"selected_profiles_hash":selected_profiles_hash,
            "approved_preimages_hash":approved_preimages_hash,"destination_preview":destination_preview,
            "destination_preview_error":if destination_preview_missing{Some("The exact persisted destination preview is unavailable; save the draft again before approval")}else{None},
            "final_source_set_hash":final_source_set_hash.clone(),"approved_source_set_hash":approved_source_set_hash,
            "finalized_at":finalized_at,"supersedes_setup_operation_id":supersedes_setup_operation_id,"final_files":final_files,
            "installation_source_binding_complete":final_source_set_hash.is_some() && !final_files.is_empty(),
            "installation_source_binding_reason":if final_source_set_hash.is_some(){"Exact post-preflight bytes and destination preimages are frozen"}else{"Finalize the post-preflight installation bytes before human approval"},
            "selected_profiles":profiles,"probe_receipts":probe_receipts,"sessions":setup_sessions,
            "agents_file":agents_file,"probe_authorized_at":probe_authorized_at,
            "install_authorized_at":install_authorized_at,"error":error,
            "runtime_admissions":runtime_admissions,
            "recoveries":recoveries,
            "created_at":created_at,"updated_at":updated_at,
        }));
    }
    Ok(result)
}

fn setup_attempt_quiescent(connection: &Connection, attempt_id: &str) -> Result<bool> {
    connection.query_row(
        "SELECT NOT EXISTS(
             SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id=?1
               AND (s.status!='exited' OR COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)!=1)
         ) AND NOT EXISTS(
             SELECT 1 FROM launch_permits WHERE attempt_id=?1 AND state='issued'
         )",
        params![attempt_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn setup_manager_control_row(
    connection: &Connection,
    setup_id: &str,
    discovery_attempt_id: Option<&str>,
    supersedes_setup_operation_id: Option<&str>,
) -> Result<serde_json::Value> {
    let requested: Option<serde_json::Value> = connection
        .query_row(
            "SELECT profile_json FROM trip_setup_profile_selections
             WHERE setup_operation_id=?1 AND role='manager' AND selection_state='selected'",
            params![setup_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| serde_json::from_str(&value).ok());
    let current = if let Some(source_setup_id) = supersedes_setup_operation_id {
        connection
            .query_row(
                "SELECT profile_json FROM trip_setup_profile_selections
                 WHERE setup_operation_id=?1 AND role='manager' AND selection_state='selected'",
                params![source_setup_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| serde_json::from_str(&value).ok())
            .or_else(|| requested.clone())
    } else {
        requested.clone()
    };
    let effective: Option<serde_json::Value> = connection
        .query_row(
            "SELECT json_object(
                 'provider',json_extract(s.launch_config_json,'$.provider'),
                 'model',json_extract(s.launch_config_json,'$.model'),
                 'effort',json_extract(s.launch_config_json,'$.effort')
             )
             FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             JOIN attempts a ON a.id=rg.attempt_id
             WHERE a.setup_operation_id=?1 AND rg.role='manager'
             ORDER BY s.created_at DESC,s.rowid DESC LIMIT 1",
            params![setup_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| serde_json::from_str(&value).ok());
    let Some(attempt_id) = discovery_attempt_id else {
        return Ok(serde_json::json!({
            "current":current,"requested":requested,"effective":effective,
            "interrupt_requested":false,"quiescent":false,
            "next_action":{"action":"none","reason":"No discovery attempt is available for manager control."}
        }));
    };
    let hold: Option<serde_json::Value> = connection
        .query_row(
            "SELECT json_object('id',id,'state',state,'requested_operation_id',requested_operation_id,
                                'signal_delivery_state',json_extract(payload_json,'$.signal_delivery.state'),
                                'signal_delivery_error',json_extract(payload_json,'$.signal_delivery.error'),
                                'created_at',created_at,'updated_at',updated_at)
             FROM controls WHERE attempt_id=?1 AND kind='setup_manager_change' AND state='held'
             ORDER BY created_at DESC LIMIT 1",
            params![attempt_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| serde_json::from_str(&value).ok());
    let has_manager_session: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         WHERE rg.attempt_id=?1 AND rg.role='manager')",
        params![attempt_id],
        |row| row.get(0),
    )?;
    let interrupt_requested: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         WHERE rg.attempt_id=?1 AND rg.role='manager' AND s.status='interrupt_requested')",
        params![attempt_id],
        |row| row.get(0),
    )?;
    let manager_receipt: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM trip_preflight_receipts
         WHERE setup_operation_id=?1 AND role='manager' AND result='success')",
        params![setup_id],
        |row| row.get(0),
    )?;
    let quiescent = setup_attempt_quiescent(connection, attempt_id)?;
    let failed_signal_delivery = hold.as_ref().and_then(|hold| {
        (hold
            .get("signal_delivery_state")
            .and_then(serde_json::Value::as_str)
            == Some("failed"))
        .then(|| {
            hold.get("signal_delivery_error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(
                    "The prior manager-only signal delivery failed without a recorded error.",
                )
                .to_owned()
        })
    });
    let next_action = if let Some(error) = failed_signal_delivery.as_deref() {
        if !quiescent {
            serde_json::json!({
                "action":"retry_stop",
                "reason":format!(
                    "The prior manager-only Stop delivery failed: {error} A fresh human Retry Stop will recheck the exact recorded manager identity; no automatic retry or replacement is scheduled."
                )
            })
        } else {
            serde_json::json!({"action":"change","reason":"The old discovery manager is positively quiescent. Edit the exact replacement profile, then create a fresh discovery revision."})
        }
    } else if hold.is_some() && !quiescent {
        serde_json::json!({"action":"waiting","reason":if interrupt_requested {"Interrupt is requested; wait for a verified process-group exit before changing the manager."} else {"Discovery dispatch is held and old authority is revoked; wait for a verified process-group exit before changing the manager."}})
    } else if hold.is_some() {
        serde_json::json!({"action":"change","reason":"The old discovery manager is positively quiescent. Edit the exact replacement profile, then create a fresh discovery revision."})
    } else if manager_receipt {
        serde_json::json!({"action":"none","reason":"The retained discovery manager has recorded its exact receipt; continue the reviewed setup flow."})
    } else if !quiescent {
        serde_json::json!({"action":"stop","reason":"The discovery manager is live or its exit is not yet verified. Stop only the manager before changing its profile."})
    } else if has_manager_session {
        serde_json::json!({"action":"change","reason":"The prior discovery session is quiescent. You may resume the unchanged exact session or change the manager and launch a fresh discovery revision."})
    } else {
        serde_json::json!({"action":"launch","reason":"The selected manager has a fresh setup permit and awaits an explicit discovery launch."})
    };
    Ok(serde_json::json!({
        "hold":hold,
        "current":current,
        "requested":requested,
        "effective":effective,
        "interrupt_requested":interrupt_requested,
        "quiescent":quiescent,
        "next_action":next_action
    }))
}

fn setup_recovery_rows(
    connection: &Connection,
    setup_operation_id: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut statement = connection.prepare(
        "SELECT recovery.id,recovery.session_id,recovery.attempt_id,t.id,t.version,
                rg.role,s.validation_cell,recovery.state,s.status,a.status,t.attention,
                probe.admission_id,recovery.created_at,recovery.updated_at
         FROM recovery_records recovery
         JOIN attempts a ON a.id=recovery.attempt_id
         JOIN tasks t ON t.id=a.task_id
         JOIN sessions s ON s.id=recovery.session_id
         JOIN role_generations rg ON rg.id=s.role_generation_id
         LEFT JOIN trip_runtime_probes probe ON probe.session_id=s.id
         WHERE a.setup_operation_id=?1 AND recovery.state='attention_required'
           AND s.validation_cell IN ('trip_setup_discovery','trip_setup_probe','trip_runtime_probe')
         ORDER BY recovery.created_at",
    )?;
    let rows = statement
        .query_map(params![setup_operation_id], |row| {
            Ok(serde_json::json!({
                "record_id":row.get::<_,String>(0)?,
                "session_id":row.get::<_,String>(1)?,
                "attempt_id":row.get::<_,String>(2)?,
                "task_id":row.get::<_,String>(3)?,
                "task_version":row.get::<_,i64>(4)?,
                "role":row.get::<_,String>(5)?,
                "validation_cell":row.get::<_,String>(6)?,
                "state":row.get::<_,String>(7)?,
                "session_status":row.get::<_,String>(8)?,
                "attempt_status":row.get::<_,String>(9)?,
                "task_attention":row.get::<_,String>(10)?,
                "runtime_admission_id":row.get::<_,Option<String>>(11)?,
                "ownership_state":"recorded_process_verification_required",
                "created_at":row.get::<_,String>(12)?,
                "updated_at":row.get::<_,String>(13)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn runtime_admission_rows(
    connection: &Connection,
    project_id: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut statement=connection.prepare("SELECT id,task_id,scope_hash,state,fresh_call_count,authorized_at,failure_reason,created_at,updated_at FROM trip_runtime_admissions WHERE project_id=?1 ORDER BY created_at DESC")?;
    let admissions = statement
        .query_map(params![project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::with_capacity(admissions.len());
    for (
        id,
        task_id,
        scope_hash,
        state,
        fresh_call_count,
        authorized_at,
        failure_reason,
        created_at,
        updated_at,
    ) in admissions
    {
        let mut probes=connection.prepare(
            "SELECT p.role,p.settings_revision,p.profile_json,p.profile_hash,p.project_config_revision_id,p.project_configuration_hash,
                    p.adapter_name,p.adapter_hash,p.capability_key,p.nonce,p.state,p.session_id,p.capability_id,p.failure_reason,
                    p.published_at,p.attempt_id,p.workspace_path,s.status,s.readiness_state,s.hook_trust_state,
                    s.native_session_id IS NOT NULL,
                    COALESCE((SELECT json_extract(rr.metadata_json,'$.validation_observation.failure_category')
                              FROM role_results rr WHERE rr.session_id=p.session_id AND rr.outcome='capability_observed'
                              ORDER BY rr.created_at DESC LIMIT 1),
                             CASE WHEN p.failure_reason IN ('os_denial','provider_denial','shell_construction','model_refusal','observation_unavailable','unexpected_result','authentication_missing') THEN p.failure_reason END),
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
                     ) FROM cmux_session_surfaces r
                     WHERE r.session_id=s.id AND r.role_generation_id=s.role_generation_id
                       AND r.transcript_epoch=s.transcript_epoch
                       AND r.process_identity_json=s.process_identity_json
                     ORDER BY CASE
                        WHEN r.surface_state IN ('opening','open','unknown')
                         AND r.attachment_state IN ('pending','live') THEN 0
                        ELSE 1
                     END,r.created_at DESC,r.binding_revision DESC LIMIT 1)
             FROM trip_runtime_probes p LEFT JOIN sessions s ON s.id=p.session_id
             WHERE p.admission_id=?1 ORDER BY p.role"
        )?;
        let probes=probes.query_map(params![id],|row|Ok(serde_json::json!({"role":row.get::<_,String>(0)?,"settings_revision":row.get::<_,Option<i64>>(1)?,"profile":serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(2)?).unwrap_or_default(),"profile_hash":row.get::<_,String>(3)?,"project_config_revision_id":row.get::<_,String>(4)?,"project_configuration_hash":row.get::<_,String>(5)?,"adapter":row.get::<_,String>(6)?,"adapter_hash":row.get::<_,String>(7)?,"capability_key":row.get::<_,String>(8)?,"nonce":row.get::<_,String>(9)?,"state":row.get::<_,String>(10)?,"session_id":row.get::<_,Option<String>>(11)?,"capability_id":row.get::<_,Option<String>>(12)?,"failure_reason":row.get::<_,Option<String>>(13)?,"published_at":row.get::<_,Option<String>>(14)?,"attempt_id":row.get::<_,Option<String>>(15)?,"workspace_path":row.get::<_,Option<String>>(16)?,"session_status":row.get::<_,Option<String>>(17)?,"readiness":row.get::<_,Option<String>>(18)?,"hook_trust":row.get::<_,Option<String>>(19)?,"has_native_session":row.get::<_,Option<bool>>(20)?.unwrap_or(false),"failure_category":row.get::<_,Option<String>>(21)?,"cmux_surface":row.get::<_,Option<String>>(22)?.and_then(|value|serde_json::from_str::<serde_json::Value>(&value).ok())})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        result.push(serde_json::json!({"id":id,"project_id":project_id,"task_id":task_id,"scope_hash":scope_hash,"state":state,"fresh_call_count":fresh_call_count,"authorized_at":authorized_at,"failure_reason":failure_reason,"probes":probes,"created_at":created_at,"updated_at":updated_at}));
    }
    Ok(result)
}

fn setup_attempt_state(
    connection: &Connection,
    attempt_id: Option<&str>,
) -> Result<Option<serde_json::Value>> {
    let Some(attempt_id) = attempt_id else {
        return Ok(None);
    };
    let value=connection.query_row(
        "SELECT json_object('attempt_id',a.id,'phase',a.phase,'status',a.status,
                'workspace_state',w.state,
                'issued_permits',(SELECT COUNT(*) FROM trip_setup_permits WHERE attempt_id=a.id AND state='issued'),
                'consumed_permits',(SELECT COUNT(*) FROM trip_setup_permits WHERE attempt_id=a.id AND state='consumed'),
                'updated_at',a.updated_at)
         FROM attempts a LEFT JOIN workspaces w ON w.attempt_id=a.id WHERE a.id=?1",
        params![attempt_id],|row|row.get::<_,String>(0)
    ).optional()?;
    Ok(value.and_then(|text| serde_json::from_str(&text).ok()))
}

fn setup_agents_file_preview(root: &Path) -> serde_json::Value {
    let root = match root.canonicalize() {
        Ok(value) => value,
        Err(error) => {
            return serde_json::json!({"error":format!("Repository path is unavailable: {error}")})
        }
    };
    let path = root.join("AGENTS.md");
    if let Err(error) = ensure_no_symlink_ancestry(&root, &path) {
        return serde_json::json!({"error":format!("AGENTS.md cannot be read safely: {error}")});
    }
    match fs::read(&path) {
        Ok(bytes) if bytes.len() <= 512 * 1024 => match String::from_utf8(bytes) {
            Ok(content) => {
                let hash = sha256(content.as_bytes());
                serde_json::json!({"exists":true,"content":content,"sha256":hash})
            }
            Err(_) => serde_json::json!({"exists":true,"error":"AGENTS.md is not UTF-8 text"}),
        },
        Ok(_) => {
            serde_json::json!({"exists":true,"error":"AGENTS.md exceeds the 512 KiB setup preview limit"})
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({"exists":false,"content":""})
        }
        Err(error) => serde_json::json!({"error":format!("AGENTS.md could not be read: {error}")}),
    }
}

fn task_verification_rows(connection: &Connection) -> Result<Vec<serde_json::Value>> {
    let selections = {
        let mut statement=connection.prepare(
            "SELECT s.attempt_id,s.revision,s.check_id,s.required,a.candidate_hash,
                    c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd,
                    a.phase,a.status,t.lifecycle,t.archived_at,c.enabled,
                    a.id=(SELECT current.id FROM attempts current WHERE current.task_id=a.task_id ORDER BY current.created_at DESC LIMIT 1),
                    EXISTS(SELECT 1 FROM check_runs cr WHERE cr.attempt_id=a.id AND cr.candidate_hash=a.candidate_hash
                      AND cr.check_id=s.check_id AND cr.selected_check_revision=s.revision AND cr.status='finished'
                      AND cr.exit_code=0 AND cr.freshness_state='current')
             FROM trip_selected_checks s JOIN attempts a ON a.id=s.attempt_id
             JOIN tasks t ON t.id=a.task_id
             JOIN trip_verification_checks c ON c.id=s.check_id
             WHERE s.revision=a.selected_checks_revision ORDER BY s.attempt_id,c.category,c.check_key"
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, bool>(14)?,
                    row.get::<_, bool>(15)?,
                    row.get::<_, bool>(16)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut result = Vec::with_capacity(selections.len());
    for (
        attempt_id,
        revision,
        check_id,
        required,
        candidate,
        kind,
        executable,
        arguments,
        shell,
        cwd,
        phase,
        attempt_status,
        lifecycle,
        archived_at,
        enabled,
        current_attempt,
        current_success,
    ) in selections
    {
        let exact_command_hash = json_hash(
            &serde_json::json!({"kind":&kind,"executable":&executable,"arguments":&arguments,"shell":&shell,"cwd":&cwd}),
        )?;
        let scope_hash=candidate.as_ref().map(|candidate|json_hash(&serde_json::json!({"attempt_id":&attempt_id,"candidate_hash":candidate,"check_id":&check_id,"selected_revision":revision,"cwd":&cwd}))).transpose()?;
        let mut authorization = serde_json::json!({
            "once_available":false,"reusable":false,"family":false,"authorized":false,
            "state":"pending","source":"service_check","family_preview":null,
            "family_unavailable_reason":null,"matching_rule":null,
            "action_state":"inactive","inactive_reason":null
        });
        if let Some(scope_hash) = scope_hash.as_deref() {
            let latest: Option<(String, String, Option<String>)> = connection
                .query_row(
                    "SELECT decision,lifetime,consumed_at FROM trip_check_authorizations
                 WHERE attempt_id=?1 AND check_id=?2 AND selected_revision=?3
                   AND exact_command_hash=?4 AND scope_hash=?5
                 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                    params![
                        attempt_id,
                        check_id,
                        revision,
                        exact_command_hash,
                        scope_hash
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let denied = latest.as_ref().is_some_and(|value| value.0 == "denied");
            let once = latest.as_ref().is_some_and(|value| {
                value.0 == "approved" && value.1 == "once" && value.2.is_none()
            });
            let reusable = latest
                .as_ref()
                .is_some_and(|value| value.0 == "approved" && value.1 == "reusable");
            let family = crate::permissions::service_check_family(
                connection,
                &attempt_id,
                &kind,
                executable.as_deref(),
                &cwd,
            );
            match family {
                Ok(mut family) => {
                    family.preview["current_arguments"] = arguments
                        .as_deref()
                        .and_then(|value| serde_json::from_str(value).ok())
                        .unwrap_or_else(|| serde_json::json!([]));
                    authorization["family_preview"] = family.preview.clone();
                    if !denied {
                        if let Some((rule_id, rule_revision)) =
                            crate::permissions::matching_service_check_rule(
                                connection,
                                &attempt_id,
                                &family,
                            )?
                        {
                            let display:String=connection.query_row(
                                "SELECT display_family FROM trip_check_permission_rules WHERE id=?1",
                                params![rule_id],|row|row.get(0))?;
                            authorization["family"] = serde_json::json!(true);
                            authorization["matching_rule"] = serde_json::json!({
                                "id":rule_id,"revision":rule_revision,"display_family":display,
                                "source":"service_check"
                            });
                        }
                    }
                }
                Err(error) => {
                    authorization["family_unavailable_reason"] =
                        serde_json::json!(format!("{error:#}"))
                }
            }
            let family_authorized = authorization["family"] == true;
            authorization["once_available"] = serde_json::json!(once);
            authorization["reusable"] = serde_json::json!(reusable);
            authorization["authorized"] =
                serde_json::json!(!denied && (once || reusable || family_authorized));
            authorization["state"] = serde_json::json!(if denied {
                "denied"
            } else if once {
                "approved_once"
            } else if reusable {
                "approved_exact"
            } else if family_authorized {
                "approved_family"
            } else {
                "pending"
            });
        }
        let latest_run:Option<serde_json::Value>=connection.query_row(
            "SELECT json_object('id',id,'candidate_hash',candidate_hash,'status',status,'launch_state',launch_state,
                    'launch_error',launch_error,'exit_code',exit_code,'inputs_hash',inputs_hash,
                    'acceptance_coverage',json(acceptance_coverage_json),'elapsed_millis',elapsed_millis,
                    'freshness_state',freshness_state,'evidence',json(evidence_json),'created_at',created_at,'finished_at',finished_at)
             FROM check_runs WHERE attempt_id=?1 AND check_id=?2 AND selected_check_revision=?3
             ORDER BY created_at DESC LIMIT 1",
            params![attempt_id,check_id,revision],|row|row.get::<_,String>(0)
        ).optional()?.and_then(|value|serde_json::from_str(&value).ok());
        let active = current_attempt
            && phase == "checks"
            && matches!(
                attempt_status.as_str(),
                "running" | "held" | "needs_input" | "needs_recovery"
            )
            && archived_at.is_none()
            && !matches!(lifecycle.as_str(), "done" | "cancelled")
            && required
            && enabled
            && candidate.is_some();
        authorization["action_state"] = serde_json::json!(if !active {
            "inactive"
        } else if current_success {
            "current_receipt"
        } else {
            "actionable"
        });
        authorization["inactive_reason"] = serde_json::json!(if !active {
            Some("Decisions are available only for a required, enabled current selection with a candidate during the active checks phase.")
        } else if current_success {
            Some("A fresh successful receipt already satisfies this candidate and selection; approve again here only to rerun.")
        } else {
            None
        });
        result.push(serde_json::json!({
            "attempt_id":attempt_id,"selected_revision":revision,"check_id":check_id,"required":required,
            "candidate_hash":candidate,"exact_command_hash":exact_command_hash,"scope_hash":scope_hash,
            "command":{"kind":kind,"executable":executable,"arguments":arguments.as_deref().and_then(|value|serde_json::from_str::<serde_json::Value>(value).ok()),"shell":shell,"cwd":cwd},
            "authorization":authorization,"latest_run":latest_run,
        }));
    }
    Ok(result)
}

fn detect_installation(root: &Path) -> Result<InstallationObservation> {
    let canonical = root.canonicalize()?;
    let state = canonical.join(".agents/trip-explorer");
    let skill_root = canonical.join(".agents/skills");
    let alternate = canonical.join(".claude/skills");
    let required = [
        "trip-explorer-init",
        "trip-explorer-upgrade",
        "trip-explorer-workflow",
    ];
    let mut partial = Vec::new();
    for name in required {
        if skill_root.join(name).exists() {
            partial.push(format!(".agents/skills/{name}"));
        }
    }
    if state.exists() {
        partial.push(".agents/trip-explorer".into());
    }
    let alternate_roots = if alternate.exists() {
        vec![".claude/skills".into()]
    } else {
        vec![]
    };
    if partial.is_empty() {
        return Ok(InstallationObservation {
            kind: if alternate_roots.is_empty() {
                "absent".into()
            } else {
                "alternate_root".into()
            },
            canonical_root: ".agents/skills".into(),
            alternate_roots,
            manifest_version: None,
            customized: false,
            partial_paths: vec![],
            conflicts: vec![],
            hashes: BTreeMap::new(),
        });
    }
    if partial.len() != 4 {
        return Ok(InstallationObservation {
            kind: "partial".into(),
            canonical_root: ".agents/skills".into(),
            alternate_roots,
            manifest_version: None,
            customized: false,
            partial_paths: partial,
            conflicts: vec![],
            hashes: BTreeMap::new(),
        });
    }
    ensure_no_symlink_ancestry(&canonical, &state)?;
    let manifest_path = state.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(InstallationObservation {
            kind: "partial".into(),
            canonical_root: ".agents/skills".into(),
            alternate_roots,
            manifest_version: None,
            customized: false,
            partial_paths: partial,
            conflicts: vec!["manifest.json is missing".into()],
            hashes: BTreeMap::new(),
        });
    }
    let manifest: serde_json::Value = match serde_json::from_slice(&fs::read(&manifest_path)?) {
        Ok(value) => value,
        Err(error) => {
            return Ok(InstallationObservation {
                kind: "invalid".into(),
                canonical_root: ".agents/skills".into(),
                alternate_roots,
                manifest_version: None,
                customized: false,
                partial_paths: partial,
                conflicts: vec![format!("manifest.json is invalid: {error}")],
                hashes: BTreeMap::new(),
            })
        }
    };
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let hashes = hash_installation(root)?;
    let expected_base = PACKAGE_FILES
        .iter()
        .map(|file| (file.relative.to_owned(), file.sha256.to_owned()))
        .collect::<BTreeMap<_, _>>();
    let expected_bin = PACKAGE_FILES
        .iter()
        .filter_map(|file| {
            file.relative
                .strip_prefix("bin/")
                .map(|path| (path.to_owned(), file.sha256.to_owned()))
        })
        .collect::<BTreeMap<_, _>>();
    let manifest_base = manifest
        .get("base")
        .cloned()
        .and_then(|value| serde_json::from_value::<BTreeMap<String, String>>(value).ok())
        .unwrap_or_default();
    let manifest_bin = manifest
        .get("bin")
        .cloned()
        .and_then(|value| serde_json::from_value::<BTreeMap<String, String>>(value).ok())
        .unwrap_or_default();
    let mut conflicts = Vec::new();
    if manifest_base != expected_base {
        conflicts.push("versioned base manifest does not match the pinned upstream source".into());
    }
    if manifest_bin != expected_bin {
        conflicts.push("bin manifest does not match the pinned upstream source".into());
    }
    for file in PACKAGE_FILES {
        let base_path = state.join("base").join(PACKAGE_VERSION).join(file.relative);
        if hash_file(&base_path)?.as_deref() != Some(file.sha256) {
            conflicts.push(format!("versioned base drift: {}", file.relative));
        }
    }
    for (name, key) in [
        ("config.json", "config_sha256"),
        ("adapters.json", "adapters_sha256"),
        ("preflight.json", "preflight_sha256"),
    ] {
        let expected = manifest.get(key).and_then(|value| value.as_str());
        if expected.is_none() || hash_file(&state.join(name))?.as_deref() != expected {
            conflicts.push(format!("{name} does not match its manifest hash"));
        }
    }
    let expected_active = package_active_hashes();
    for (path, expected) in &expected_active {
        if hashes.get(path) != Some(expected) {
            conflicts.push(format!("active file drift:{path}"));
        }
    }
    let customized = conflicts
        .iter()
        .any(|conflict| conflict.starts_with("active file drift:"));
    let kind = if version.as_deref() != Some(PACKAGE_VERSION) {
        "incompatible"
    } else if customized {
        "customized"
    } else if !conflicts.is_empty() {
        "invalid"
    } else {
        "compatible"
    };
    Ok(InstallationObservation {
        kind: kind.into(),
        canonical_root: ".agents/skills".into(),
        alternate_roots,
        manifest_version: version,
        customized,
        partial_paths: partial,
        conflicts,
        hashes,
    })
}

fn readiness_from_observation(observation: &InstallationObservation) -> (ProjectReadiness, String) {
    match observation.kind.as_str() {
        "absent" => (
            ProjectReadiness::NotInitialized,
            "No canonical TRIP Explorer installation was found".into(),
        ),
        "compatible" => (
            ProjectReadiness::NeedsUpgradeReview,
            "Compatible installation detected; explicit validation and adoption are required"
                .into(),
        ),
        "partial" | "invalid" => (
            ProjectReadiness::Invalid,
            "Partial or invalid TRIP Explorer installation requires reconciliation".into(),
        ),
        "customized" | "incompatible" | "alternate_root" => (
            ProjectReadiness::NeedsUpgradeReview,
            "Existing installation requires an explicit three-way canonical migration review"
                .into(),
        ),
        _ => (
            ProjectReadiness::Invalid,
            "Unrecognized TRIP Explorer installation state".into(),
        ),
    }
}

fn validate_setup_proposal(proposal: &SetupProposal) -> Result<()> {
    if proposal.project_name.trim().is_empty() {
        bail!("project_name is required")
    }
    validate_role_override(
        proposal
            .host_manager
            .as_ref()
            .ok_or_else(|| anyhow!("host_manager is required"))?,
    )?;
    let coverage = proposal.testing.get("coverage").and_then(|v| v.as_str());
    if !matches!(coverage, Some("minimal" | "moderate" | "extensive")) {
        bail!("testing.coverage must be minimal, moderate, or extensive")
    }
    let cmux = proposal.observability.get("cmux").and_then(|v| v.as_str());
    if !matches!(cmux, Some("auto" | "on" | "off")) {
        bail!("observability.cmux must be auto, on, or off")
    }
    if !proposal
        .documentation
        .get("no_change_text")
        .and_then(|v| v.as_str())
        .is_some()
    {
        bail!("documentation.no_change_text is required")
    }
    for key in ["focused", "broad", "cleanup"] {
        string_array(&proposal.verification, key)?;
    }
    validate_verification_contracts(&proposal.verification, &proposal.verification_contracts)?;
    for guidance in &proposal.guidance {
        validate_relative(guidance)?;
    }
    if !proposal.guidance.iter().any(|path| path == "AGENTS.md") {
        bail!("approved root AGENTS.md must be included in project guidance")
    }
    let expected: BTreeSet<_> = DELEGATED_ROLES.iter().map(|r| r.to_string()).collect();
    let actual: BTreeSet<_> = proposal.roles.keys().cloned().collect();
    if actual != expected {
        bail!("setup requires exactly Explorer, plan reviewer, implementer, code reviewer, and final verifier")
    }
    for (role, selected) in &proposal.roles {
        let profile = proposal.profiles.get(&selected.profile).ok_or_else(|| {
            anyhow!(
                "role {role} references missing profile {}",
                selected.profile
            )
        })?;
        profile
            .provider
            .parse::<crate::domain::Provider>()
            .map_err(|e| anyhow!(e))?;
        if profile.adapter.trim().is_empty()
            || profile.model.trim().is_empty()
            || profile.effort.trim().is_empty()
        {
            bail!(
                "profile {} has incomplete adapter/model/effort",
                selected.profile
            )
        }
        if profile
            .extra
            .get("service_tier")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|tier| !tier.trim().is_empty())
        {
            bail!("service_tier is not supported by the current native CLI invocation contract; remove it before probe authorization")
        }
        if profile
            .extra
            .get("service_tier")
            .is_some_and(|tier| !tier.is_null() && !tier.is_string())
        {
            bail!("service_tier must be a string or null")
        }
        let expected_authority = if role == "implementer" {
            "workspace-write"
        } else {
            "read-only"
        };
        let expected_session = if role == "final_verifier" {
            "fresh"
        } else {
            "retained"
        };
        if profile.authority != expected_authority || profile.session != expected_session {
            bail!("role {role} profile violates its fixed authority/session contract")
        }
        let adapters = proposal
            .adapters
            .get("adapters")
            .and_then(|value| value.as_object())
            .ok_or_else(|| anyhow!("adapters.adapters must be a non-empty object"))?;
        let adapter = adapters
            .get(&profile.adapter)
            .and_then(|value| value.as_object())
            .ok_or_else(|| anyhow!("profile {} references a missing adapter", selected.profile))?;
        if adapter.get("provider").and_then(|value| value.as_str())
            != Some(profile.provider.as_str())
        {
            bail!("profile provider must match its adapter")
        }
        let kind = adapter
            .get("kind")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("adapter kind is required"))?;
        if !matches!(kind, "native-agent" | "builtin-cli") {
            bail!(
                "custom and unsupported adapters are preserved but cannot activate in this release"
            )
        }
        let capabilities = adapter
            .get("capabilities")
            .and_then(|value| value.as_object())
            .ok_or_else(|| anyhow!("adapter capabilities are required"))?;
        let required = if expected_authority == "workspace-write" {
            "workspace_write"
        } else {
            "read_only"
        };
        if capabilities.get(required).and_then(|value| value.as_bool()) != Some(true) {
            bail!("adapter cannot enforce {required} for role {role}")
        }
        if expected_session == "fresh"
            && capabilities
                .get("fresh_session")
                .and_then(|value| value.as_bool())
                != Some(true)
        {
            bail!("adapter cannot provide a fresh final verifier")
        }
        if expected_session == "retained"
            && capabilities.get("resume").and_then(|value| value.as_bool()) != Some(true)
        {
            bail!("adapter cannot retain role {role}")
        }
    }
    if contains_prohibited_key(&serde_json::to_value(proposal)?) {
        bail!("configuration cannot contain credentials, secrets, tokens, or hidden reasoning")
    }
    let agents_path = proposal
        .agents_file
        .get("relative_path")
        .and_then(|v| v.as_str())
        .unwrap_or("AGENTS.md");
    if agents_path != "AGENTS.md" {
        bail!("root guidance approval can patch only AGENTS.md")
    }
    if !proposal
        .agents_file
        .get("approved_content")
        .and_then(|v| v.as_str())
        .is_some()
    {
        bail!("agents_file.approved_content is required")
    }
    if proposal
        .local_exclude
        .get("pattern")
        .and_then(|value| value.as_str())
        != Some("/.local/trip-explorer/")
        || proposal
            .local_exclude
            .get("approved")
            .and_then(|value| value.as_bool())
            .is_none()
    {
        bail!("local_exclude must record the exact /.local/trip-explorer/ pattern and approval decision")
    }
    Ok(())
}

fn project_configuration(proposal: &SetupProposal) -> Result<serde_json::Value> {
    let mut configuration = serde_json::to_value(proposal)?;
    let object = configuration
        .as_object_mut()
        .ok_or_else(|| anyhow!("project configuration must be an object"))?;
    for field in [
        "host_manager",
        "verification_contracts",
        "adapters",
        "agents_file",
        "local_exclude",
        "canonical_migration",
    ] {
        object.remove(field);
    }
    Ok(configuration)
}

fn setup_proposal_preserving_configuration(
    store: &Store,
    setup_id: &str,
    proposed: &serde_json::Value,
) -> Result<SetupProposal> {
    let repository: String = store.lock()?.query_row(
        "SELECT p.repository_path FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id WHERE so.id=?1",
        params![setup_id], |row| row.get(0),
    )?;
    let root = PathBuf::from(repository).canonicalize()?;
    let Some((mut configuration, adapters)) = detected_installation_configuration(&root)? else {
        return Ok(serde_json::from_value(proposed.clone())?);
    };
    let incoming = proposed
        .as_object()
        .ok_or_else(|| anyhow!("setup proposal must be an object"))?;
    let existing = configuration
        .as_object_mut()
        .ok_or_else(|| anyhow!("project configuration must be an object"))?;
    existing.insert("adapters".into(), adapters);
    for (field, value) in incoming {
        if matches!(field.as_str(), "profiles" | "roles") {
            if let (Some(prior), Some(selected)) = (
                existing
                    .get_mut(field)
                    .and_then(serde_json::Value::as_object_mut),
                value.as_object(),
            ) {
                for (name, entry) in selected {
                    if let (Some(prior_fields), Some(selected_fields)) = (
                        prior
                            .get_mut(name)
                            .and_then(serde_json::Value::as_object_mut),
                        entry.as_object(),
                    ) {
                        prior_fields.extend(selected_fields.clone());
                    } else {
                        prior.insert(name.clone(), entry.clone());
                    }
                }
                continue;
            }
        }
        if field == "adapters" {
            if let (Some(prior), Some(selected)) = (
                existing
                    .get_mut(field)
                    .and_then(serde_json::Value::as_object_mut),
                value.as_object(),
            ) {
                for (name, entry) in selected {
                    if name == "adapters" {
                        if let (Some(definitions), Some(replacements)) = (
                            prior
                                .get_mut(name)
                                .and_then(serde_json::Value::as_object_mut),
                            entry.as_object(),
                        ) {
                            for (adapter, replacement) in replacements {
                                if let (Some(fields), Some(updates)) = (
                                    definitions
                                        .get_mut(adapter)
                                        .and_then(serde_json::Value::as_object_mut),
                                    replacement.as_object(),
                                ) {
                                    fields.extend(updates.clone());
                                } else {
                                    definitions.insert(adapter.clone(), replacement.clone());
                                }
                            }
                            continue;
                        }
                    }
                    prior.insert(name.clone(), entry.clone());
                }
                continue;
            }
        }
        existing.insert(field.clone(), value.clone());
    }
    Ok(serde_json::from_value(configuration)?)
}

fn validate_guidance_files(root: &Path, proposal: &SetupProposal) -> Result<()> {
    for relative in &proposal.guidance {
        let path = contained_path(root, relative, false)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                bail!("Guidance must use regular files: {relative}. Select the individual regular files inside this directory in Project setup.")
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && relative == "AGENTS.md" => {}
            Err(error) => {
                return Err(error).with_context(|| format!("read guidance file {relative}"))
            }
        }
    }
    Ok(())
}

fn validate_agents_preservation(
    store: &Store,
    setup_id: &str,
    proposal: &SetupProposal,
) -> Result<()> {
    let repository: String = {
        let connection = store.lock()?;
        connection.query_row("SELECT p.repository_path FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id WHERE so.id=?1",params![setup_id],|row|row.get(0))?
    };
    let root = PathBuf::from(repository).canonicalize()?;
    validate_guidance_files(&root, proposal)?;
    let path = root.join("AGENTS.md");
    ensure_no_symlink_ancestry(&root, &path)?;
    let existing = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let approved = proposal
        .agents_file
        .get("approved_content")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("agents_file.approved_content is required"))?
        .as_bytes();
    if !existing.is_empty() && !approved.starts_with(&existing) {
        bail!("approved AGENTS.md must preserve the existing project-owned bytes exactly before its additive patch")
    }
    Ok(())
}

fn validate_canonical_migration(
    store: &Store,
    setup_id: &str,
    proposal: &SetupProposal,
) -> Result<()> {
    let repository: String = {
        let connection = store.lock()?;
        connection.query_row("SELECT p.repository_path FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id WHERE so.id=?1",params![setup_id],|row|row.get(0))?
    };
    let observation = detect_installation(Path::new(&repository))?;
    if matches!(observation.kind.as_str(), "absent" | "compatible") {
        if proposal.canonical_migration.is_some() {
            bail!("canonical_migration is allowed only when the observed installation requires explicit migration review")
        }
        return Ok(());
    }
    let migration=proposal.canonical_migration.as_ref().filter(|value|value.is_object()).ok_or_else(||anyhow!("existing {} installation requires an observation-bound canonical_migration proposal",observation.kind))?;
    let observation_hash = json_hash(&observation)?;
    if migration
        .get("observed_kind")
        .and_then(|value| value.as_str())
        != Some(observation.kind.as_str())
        || migration
            .get("observation_hash")
            .and_then(|value| value.as_str())
            != Some(observation_hash.as_str())
    {
        bail!("canonical migration no longer matches the exact observed installation")
    }
    if migration
        .get("unresolved_conflicts")
        .and_then(|value| value.as_array())
        .is_none_or(|values| !values.is_empty())
    {
        bail!("canonical migration cannot apply with unresolved conflicts")
    }
    let resolutions = migration
        .get("resolutions")
        .and_then(|value| value.as_object())
        .ok_or_else(|| anyhow!("canonical migration requires explicit resolutions"))?;
    let required = observation
        .conflicts
        .iter()
        .cloned()
        .chain(
            observation
                .partial_paths
                .iter()
                .map(|path| format!("partial:{path}")),
        )
        .chain(
            observation
                .alternate_roots
                .iter()
                .map(|path| format!("alternate_root:{path}")),
        )
        .collect::<BTreeSet<_>>();
    for item in &required {
        let action = resolutions
            .get(item)
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("canonical migration lacks a resolution for {item}"))?;
        if !matches!(
            action,
            "use_pinned" | "copy_pinned_to_canonical_preserve_original"
        ) {
            bail!("canonical migration resolution for {item} is unsupported")
        }
    }
    if resolutions.keys().any(|key| !required.contains(key)) {
        bail!("canonical migration contains a resolution outside the observed conflict set")
    }
    Ok(())
}

fn installation_files(
    store: &Store,
    setup_id: &str,
    root: &Path,
    proposal: &SetupProposal,
    runtime: &CapabilityRuntime,
) -> Result<Vec<JournalFile>> {
    let mut preflight = preflight_json(store, setup_id, runtime)?;
    // The pinned package validates delegated profiles only; the host manager
    // remains an application receipt rather than an undeclared package profile.
    let receipts = preflight["receipts"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("preflight receipts are missing"))?;
    let manager_index = receipts
        .iter()
        .position(|receipt| receipt["role"] == "manager")
        .ok_or_else(|| anyhow!("host manager preflight receipt is missing"))?;
    let manager_receipt = receipts.remove(manager_index);
    preflight["llmrelay"] = serde_json::json!({"host_manager_receipt": manager_receipt});
    let config = project_configuration(proposal)?;
    let config_bytes = pretty_json(&config)?;
    let adapters_bytes = pretty_json(&proposal.adapters)?;
    let preflight_bytes = pretty_json(&preflight)?;
    let mut base = BTreeMap::new();
    let mut bin = BTreeMap::new();
    for file in PACKAGE_FILES {
        base.insert(file.relative.to_owned(), file.sha256.to_owned());
        if file.relative.starts_with("bin/") {
            bin.insert(
                file.relative.trim_start_matches("bin/").to_owned(),
                file.sha256.to_owned(),
            );
        }
    }
    let installed_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let manifest = serde_json::json!({
        "version":PACKAGE_VERSION,"installed_at":installed_at,"base":base,"bin":bin,
        "config_sha256":sha256(&config_bytes),"adapters_sha256":sha256(&adapters_bytes),"preflight_sha256":sha256(&preflight_bytes),
        "llmrelay":{"workflow_id":WORKFLOW_ID,"upstream_source_hash":source_hash(),"overlay_hash":overlay_hash()}
    });
    let mut outputs = Vec::new();
    for file in PACKAGE_FILES {
        outputs.push(journal_file(
            root,
            &format!(
                ".agents/trip-explorer/base/{PACKAGE_VERSION}/{}",
                file.relative
            ),
            file.bytes.to_vec(),
        )?);
        if let Some(active) = file
            .relative
            .strip_prefix("skills/")
            .map(|rest| format!(".agents/skills/{rest}"))
            .or_else(|| {
                file.relative
                    .strip_prefix("bin/")
                    .map(|rest| format!(".agents/trip-explorer/bin/{rest}"))
            })
        {
            outputs.push(journal_file(root, &active, file.bytes.to_vec())?);
        }
    }
    for (relative, bytes) in [
        (".agents/trip-explorer/config.json", config_bytes),
        (".agents/trip-explorer/adapters.json", adapters_bytes),
        (".agents/trip-explorer/preflight.json", preflight_bytes),
        (
            ".agents/trip-explorer/manifest.json",
            pretty_json(&manifest)?,
        ),
    ] {
        outputs.push(journal_file(root, relative, bytes)?);
    }
    if proposal.canonical_migration.is_none() {
        let agents = proposal
            .agents_file
            .get("approved_content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .as_bytes()
            .to_vec();
        outputs.push(journal_file(root, "AGENTS.md", agents)?);
    }
    if let Some(exclude) = git_exclude_journal_if_needed(root, proposal)? {
        outputs.push(exclude);
    }
    add_preserved_preimages(root, setup_id, proposal, &mut outputs)?;
    outputs.sort_by(|a, b| a.relative.cmp(&b.relative));
    let mut unique = BTreeSet::new();
    if outputs.iter().any(|f| !unique.insert(f.relative.clone())) {
        bail!("installation proposal contains a destination collision")
    }
    Ok(outputs)
}

fn proposed_preimages(
    store: &Store,
    setup_id: &str,
    proposal: &SetupProposal,
) -> Result<Vec<serde_json::Value>> {
    let path: String = {
        let connection = store.lock()?;
        connection.query_row("SELECT p.repository_path FROM trip_setup_operations so JOIN projects p ON p.id=so.project_id WHERE so.id=?1",params![setup_id],|row|row.get(0))?
    };
    let root = crate::workspace::inspect(Path::new(&path))?.root;
    let preview_files = proposal_installation_skeleton(&root, setup_id, proposal)?;
    Ok(preview_files.into_iter().map(|file|serde_json::json!({"relative_path":file.relative,"preimage_sha256":file.preimage_hash,"source_sha256":file.source_hash})).collect())
}

fn proposal_installation_skeleton(
    root: &Path,
    setup_id: &str,
    proposal: &SetupProposal,
) -> Result<Vec<JournalFile>> {
    let placeholder = serde_json::json!({"profiles":"pending live probes"});
    let config = project_configuration(proposal)?;
    let mut files = Vec::new();
    for file in PACKAGE_FILES {
        if let Some(rest) = file.relative.strip_prefix("skills/") {
            files.push(journal_file(
                root,
                &format!(".agents/skills/{rest}"),
                file.bytes.to_vec(),
            )?);
        }
        if let Some(rest) = file.relative.strip_prefix("bin/") {
            files.push(journal_file(
                root,
                &format!(".agents/trip-explorer/bin/{rest}"),
                file.bytes.to_vec(),
            )?);
        }
        files.push(journal_file(
            root,
            &format!(
                ".agents/trip-explorer/base/{PACKAGE_VERSION}/{}",
                file.relative
            ),
            file.bytes.to_vec(),
        )?);
    }
    files.push(journal_file(
        root,
        ".agents/trip-explorer/config.json",
        pretty_json(&config)?,
    )?);
    files.push(journal_file(
        root,
        ".agents/trip-explorer/adapters.json",
        pretty_json(&proposal.adapters)?,
    )?);
    files.push(journal_file(
        root,
        ".agents/trip-explorer/preflight.json",
        pretty_json(&placeholder)?,
    )?);
    files.push(journal_file(
        root,
        ".agents/trip-explorer/manifest.json",
        b"pending exact live preflight\n".to_vec(),
    )?);
    if proposal.canonical_migration.is_none() {
        files.push(journal_file(
            root,
            "AGENTS.md",
            proposal
                .agents_file
                .get("approved_content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
        )?);
    }
    if let Some(exclude) = git_exclude_journal_if_needed(root, proposal)? {
        files.push(exclude);
    }
    add_preserved_preimages(root, setup_id, proposal, &mut files)?;
    Ok(files)
}

fn add_preserved_preimages(
    root: &Path,
    setup_id: &str,
    proposal: &SetupProposal,
    files: &mut Vec<JournalFile>,
) -> Result<()> {
    if proposal.canonical_migration.is_none() {
        return Ok(());
    }
    let mut preserved = Vec::new();
    for file in files.iter() {
        if file.relative == "@git-common/info/exclude"
            || file.preimage_hash.is_none()
            || file.preimage_hash.as_deref() == Some(file.source_hash.as_str())
        {
            continue;
        }
        let source = contained_path(root, &file.relative, true)?;
        let bytes = fs::read(&source)?;
        let destination = format!(
            ".agents/trip-explorer/preserved/{setup_id}/{}",
            file.relative
        );
        preserved.push(journal_file(root, &destination, bytes)?);
    }
    files.extend(preserved);
    Ok(())
}

fn effective_receipt_id(
    connection: &Connection,
    setup_id: &str,
    role: &str,
    profile_hash: &str,
    runtime: &CapabilityRuntime,
) -> Result<Option<String>> {
    let mut statement = connection.prepare(
        "SELECT id FROM trip_preflight_receipts WHERE setup_operation_id=?1 AND role=?2 AND profile_hash=?3 AND result='success'
         UNION ALL SELECT source_receipt_id FROM trip_setup_proof_reuse WHERE setup_operation_id=?1 AND role=?2 AND profile_hash=?3",
    )?;
    let candidates = statement
        .query_map(params![setup_id, role, profile_hash], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let adapter_hash = setup_adapter_hash(connection, setup_id, role)?;
    for receipt in candidates {
        let binding: Option<(Option<String>, Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT capability_key,capability_identity_json,adapter_hash FROM trip_preflight_receipts WHERE id=?1",
                params![receipt],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((Some(stored_key), Some(stored_identity), Some(stored_adapter))) = binding else {
            continue;
        };
        if stored_adapter != adapter_hash {
            continue;
        }
        let frozen: CapabilityIdentity = serde_json::from_str(&stored_identity)?;
        if crate::providers::capability_identity_key(&frozen)? != stored_key {
            continue;
        }
        let launch_json: Option<String> = connection
            .query_row(
                "SELECT s.launch_config_json FROM trip_preflight_receipts receipt
                 JOIN role_generations rg ON rg.id=receipt.generation_id
                 JOIN sessions s ON s.role_generation_id=rg.id
                 JOIN attempts a ON a.id=rg.attempt_id
                 WHERE receipt.id=?1 AND s.capability_key=receipt.capability_key
                   AND s.capability_identity_json=receipt.capability_identity_json
                   AND s.workflow_version=?2 AND s.workflow_hash=?3 AND s.prompt_hash=?4
                   AND a.workflow_version=?2 AND a.workflow_hash=?3
                   AND a.upstream_source_hash=?5 AND a.overlay_hash=?6
                 ORDER BY s.created_at DESC LIMIT 1",
                params![
                    receipt,
                    WORKFLOW_ID,
                    crate::workflow_resources::workflow_hash(),
                    crate::workflow_resources::prompt_hash(
                        role.parse().map_err(|error: String| anyhow!(error))?
                    ),
                    source_hash(),
                    overlay_hash()
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(launch_json) = launch_json else {
            continue;
        };
        let launch: crate::domain::LaunchConfig = serde_json::from_str(&launch_json)?;
        let denials = setup_read_denials(&frozen)?;
        let prepared = crate::providers::prepare_role_launch_with_read_denials_and_bundles(
            launch.provider,
            launch.role,
            &launch.model,
            &launch.effort,
            &launch.cwd,
            "capability binding refresh",
            &runtime.role_socket,
            "normalized-proof-token",
            "normalized-proof-generation",
            "normalized-proof-session",
            None,
            &runtime.hooks,
            &runtime.executable,
            &denials,
            &runtime.compatibility_bundles,
        )?;
        runtime.require_current_policy(&prepared.config)?;
        let current = crate::providers::capability_identity(&prepared.config)?;
        if crate::providers::capability_identity_key(&current)? == stored_key
            && serde_json::to_value(current)? == serde_json::to_value(frozen)?
        {
            return Ok(Some(receipt));
        }
    }
    Ok(None)
}

fn setup_read_denials(identity: &CapabilityIdentity) -> Result<Vec<PathBuf>> {
    let values = match identity.provider {
        Provider::Codex => identity
            .security_policy
            .pointer("/denied_read_floor/additional_denied_roots"),
        Provider::Claude => identity.security_policy.get("setup_target_read_denials"),
    };
    values
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow!("setup capability read denial is not a path"))
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn setup_adapter_hash(connection: &Connection, setup_id: &str, role: &str) -> Result<String> {
    if role == "manager" {
        let selected: String = connection.query_row(
            "SELECT profile_json FROM trip_setup_profile_selections
             WHERE setup_operation_id=?1 AND role='manager' AND selection_state='selected'",
            params![setup_id],
            |row| row.get(0),
        )?;
        let manager: RoleOverride = serde_json::from_str(&selected)?;
        return json_hash(&service_manager_adapter(manager.provider));
    }
    let proposal: String = connection.query_row(
        "SELECT proposal_json FROM trip_setup_operations WHERE id=?1",
        params![setup_id],
        |row| row.get(0),
    )?;
    let proposal: SetupProposal = serde_json::from_str(&proposal)?;
    let adapters = proposal
        .adapters
        .get("adapters")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow!("setup adapter definitions are missing"))?;
    let selected = proposal
        .roles
        .get(role)
        .ok_or_else(|| anyhow!("setup role {role} is missing"))?;
    let profile = proposal
        .profiles
        .get(&selected.profile)
        .ok_or_else(|| anyhow!("setup profile {} is missing", selected.profile))?;
    let definition = adapters
        .get(&profile.adapter)
        .ok_or_else(|| anyhow!("setup adapter {} is missing", profile.adapter))?;
    json_hash(&serde_json::json!({"adapter":profile.adapter,"definition":definition}))
}

fn service_manager_adapter(provider: Provider) -> serde_json::Value {
    serde_json::json!({
        "adapter": "llmrelay_service_native_manager",
        "revision": "llmrelay-service-native-manager-v1",
        "definition": {
            "provider": provider,
            "kind": "native-agent",
            "capabilities": {
                "read_only": true,
                "resume": true,
                "service_selected": true
            }
        }
    })
}

fn require_all_setup_proofs(
    connection: &Connection,
    setup_id: &str,
    runtime: &CapabilityRuntime,
) -> Result<()> {
    for role in APP_ROLES {
        let profile_hash:String=connection.query_row(
            "SELECT profile_hash FROM trip_setup_profile_selections WHERE setup_operation_id=?1 AND role=?2 AND selection_state='selected'",
            params![setup_id,role],|row|row.get(0)
        )?;
        if effective_receipt_id(connection, setup_id, role, &profile_hash, runtime)?.is_none() {
            bail!("installation requires a current exact capability and adapter-bound proof for role {role}")
        }
    }
    Ok(())
}

fn preflight_json(
    store: &Store,
    setup_id: &str,
    runtime: &CapabilityRuntime,
) -> Result<serde_json::Value> {
    let connection = store.lock()?;
    let proposal_json: String = connection.query_row(
        "SELECT proposal_json FROM trip_setup_operations WHERE id=?1",
        params![setup_id],
        |row| row.get(0),
    )?;
    let proposal: SetupProposal = serde_json::from_str(&proposal_json)?;
    effective_preflight_json(&connection, setup_id, &proposal, runtime)
}

fn effective_preflight_json(
    connection: &Connection,
    setup_id: &str,
    proposal: &SetupProposal,
    runtime: &CapabilityRuntime,
) -> Result<serde_json::Value> {
    let mut receipts = Vec::new();
    let manager = proposal
        .host_manager
        .as_ref()
        .ok_or_else(|| anyhow!("host manager is missing"))?;
    let manager_hash = json_hash(manager)?;
    let manager_receipt =
        effective_receipt_id(connection, setup_id, "manager", &manager_hash, runtime)?
            .ok_or_else(|| anyhow!("host manager lacks successful live preflight"))?;
    let (manager_model_evidence, manager_evidence): (String, String) = connection.query_row(
        "SELECT model_evidence,evidence_json FROM trip_preflight_receipts WHERE id=?1",
        params![manager_receipt],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    receipts.push(serde_json::json!({
        "profile_ids":["host_manager"],"role":"manager","provider":manager.provider,
        "model":manager.model,"effort":manager.effort,"result":"pass","nonce_matched":true,
        "model_evidence":manager_model_evidence,
        "llmrelay_evidence":serde_json::from_str::<serde_json::Value>(&manager_evidence)?
    }));
    for (role, selection) in &proposal.roles {
        let profile_id = &selection.profile;
        let profile = proposal
            .profiles
            .get(profile_id)
            .ok_or_else(|| anyhow!("selected profile {profile_id} is missing"))?;
        let profile_hash = json_hash(profile)?;
        let receipt = effective_receipt_id(connection, setup_id, role, &profile_hash, runtime)?
            .ok_or_else(|| {
                anyhow!("role {role} selected profile {profile_id} lacks successful live preflight")
            })?;
        let (model_evidence, evidence): (String, String) = connection.query_row(
            "SELECT model_evidence,evidence_json FROM trip_preflight_receipts WHERE id=?1",
            params![receipt],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        receipts.push(serde_json::json!({
            "profile_ids":[profile_id],"role":role,"adapter":profile.adapter,"provider":profile.provider,
            "model":profile.model,"effort":profile.effort,"service_tier":profile.extra.get("service_tier"),
            "authority":profile.authority,"session":profile.session,"result":"pass","nonce_matched":true,
            "model_evidence":model_evidence,"llmrelay_evidence":serde_json::from_str::<serde_json::Value>(&evidence)?
        }));
    }
    Ok(serde_json::json!({"receipts":receipts}))
}

fn materialize_setup_package(store: &Store, attempt_id: &str, workspace: &Path) -> Result<()> {
    let mut policy_files = BTreeMap::new();
    for file in PACKAGE_FILES {
        let base_destination = workspace
            .join(".agents/trip-explorer/base")
            .join(PACKAGE_VERSION)
            .join(file.relative);
        write_new_verified(workspace, &base_destination, file.bytes, file.sha256)?;
        policy_files.insert(
            base_destination
                .strip_prefix(workspace)?
                .to_string_lossy()
                .to_string(),
            file.sha256.to_owned(),
        );
        let destination = if let Some(rest) = file.relative.strip_prefix("skills/") {
            workspace.join(".agents/skills").join(rest)
        } else if let Some(rest) = file.relative.strip_prefix("bin/") {
            workspace.join(".agents/trip-explorer/bin").join(rest)
        } else {
            continue;
        };
        write_new_verified(workspace, &destination, file.bytes, file.sha256)?;
        policy_files.insert(
            destination
                .strip_prefix(workspace)?
                .to_string_lossy()
                .to_string(),
            file.sha256.to_owned(),
        );
    }
    let overlay_path = workspace.join(".agents/trip-explorer/llmrelay-overlay.md");
    write_new_verified(
        workspace,
        &overlay_path,
        OVERLAY.as_bytes(),
        &sha256(OVERLAY.as_bytes()),
    )?;
    policy_files.insert(
        ".agents/trip-explorer/llmrelay-overlay.md".into(),
        sha256(OVERLAY.as_bytes()),
    );
    let now = Utc::now().to_rfc3339();
    let policy = serde_json::json!({"kind":"setup_fixture","workflow_id":WORKFLOW_ID,"upstream_source_hash":source_hash(),"overlay_hash":overlay_hash(),"files":policy_files,"target_files_copied":false});
    validate_materialized_policy(&policy.to_string())?;
    let connection = store.lock()?;
    let changed = connection.execute(
        "UPDATE workspaces SET policy_json=?1,state='ready',updated_at=?2 WHERE attempt_id=?3 AND state='reserved'",
        params![policy.to_string(),now,attempt_id],
    )?;
    if changed != 1 {
        bail!("setup workspace reservation changed before package publication")
    }
    Ok(())
}

/// Guidance a rework child may keep from its exact source snapshot although it
/// differs from the project repository: content its parent's activated policy
/// pinned after an explicit guidance reauthorization.
struct ReworkGuidanceSource {
    intent_id: String,
    parent_attempt_id: String,
    snapshot_id: String,
    parent_policy_hash: String,
    parent_files: serde_json::Map<String, serde_json::Value>,
    reauthorizations: Vec<serde_json::Value>,
    snapshot_files: BTreeMap<String, String>,
}

impl ReworkGuidanceSource {
    /// Returns the provenance of `relative` kept at `observed`, or why it may
    /// not be kept. Only a configured guidance path reaches this.
    fn authorize(
        &self,
        relative: &str,
        observed: &str,
        project_hash: &str,
    ) -> std::result::Result<serde_json::Value, String> {
        if is_protected_workflow_artifact(relative) {
            return Err("it is a protected workflow artifact".into());
        }
        if self.snapshot_files.get(relative).map(String::as_str) != Some(observed) {
            return Err("the worktree content is not the rework source snapshot's".into());
        }
        if self
            .parent_files
            .get(relative)
            .and_then(serde_json::Value::as_str)
            != Some(observed)
        {
            return Err("the parent's activated policy does not pin this content".into());
        }
        let approval = self
            .reauthorizations
            .iter()
            .filter_map(|approval| {
                approval
                    .get("files")
                    .and_then(serde_json::Value::as_array)?
                    .iter()
                    .rev()
                    .find(|file| {
                        file.get("path").and_then(serde_json::Value::as_str) == Some(relative)
                    })
                    .map(|file| (approval, file))
            })
            .last()
            .filter(|(_, file)| {
                file.get("sha256").and_then(serde_json::Value::as_str) == Some(observed)
            })
            .ok_or_else(|| {
                "the parent's latest guidance reauthorization did not approve this content"
                    .to_owned()
            })?;
        Ok(serde_json::json!({
            "path":relative,"sha256":observed,"project_sha256":project_hash,
            "parent_attempt_id":self.parent_attempt_id,"rework_intent_id":self.intent_id,
            "snapshot_id":self.snapshot_id,"parent_policy_hash":self.parent_policy_hash,
            "previous_sha256":approval.1.get("previous_sha256"),
            "reauthorized_at":approval.0.get("approved_at"),
        }))
    }
}

/// The authorized parent guidance of the rework child being materialized, if
/// `attempt_id` is one. `Some(Err)` names why its parent's activated policy
/// cannot vouch for any guidance: the policy, workflow, manifest and active
/// configuration it was bound to must all still be current.
fn rework_guidance_source(
    connection: &Connection,
    attempt_id: &str,
    manifest_hash: &str,
    active_configuration: &str,
) -> Result<Option<std::result::Result<ReworkGuidanceSource, String>>> {
    let source: Option<(String, String, String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT ri.id,ri.parent_attempt_id,ri.snapshot_id,
                    (SELECT s.manifest_json FROM snapshots s WHERE s.id=ri.snapshot_id
                       AND s.attempt_id=ri.parent_attempt_id AND s.complete=1
                       AND s.kind IN ('accepted','candidate')),
                    (SELECT w.policy_json FROM workspaces w WHERE w.attempt_id=ri.parent_attempt_id)
             FROM rework_intents ri JOIN attempts child ON child.id=ri.new_attempt_id
             WHERE ri.new_attempt_id=?1 AND ri.state='materializing'
               AND child.parent_attempt_id=ri.parent_attempt_id",
            params![attempt_id],
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
    let Some((intent_id, parent_attempt_id, snapshot_id, manifest_json, policy_json)) = source
    else {
        return Ok(None);
    };
    let refuse = |reason: &str| Ok(Some(Err(reason.to_owned())));
    let Some(manifest_json) = manifest_json else {
        return refuse("the rework source is not a complete snapshot of the parent");
    };
    let Some(policy_json) = policy_json else {
        return refuse("the parent has no workspace policy");
    };
    let Ok(policy) = validate_materialized_policy(&policy_json) else {
        return refuse("the parent's workspace policy is not a valid pinned TRIP policy");
    };
    if policy.get("kind").and_then(serde_json::Value::as_str) != Some("activated_project")
        || policy
            .get("manifest_hash")
            .and_then(serde_json::Value::as_str)
            != Some(manifest_hash)
    {
        return refuse("the parent's policy is not bound to the current activated manifest");
    }
    let bound_configurations = policy
        .get("task_profiles")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|profile| profile.get("project_config_revision_id"))
        .chain(
            policy
                .get("guidance_reauthorizations")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|approval| approval.get("config_revision_id")),
        )
        .collect::<Vec<_>>();
    if bound_configurations
        .iter()
        .any(|revision| revision.and_then(serde_json::Value::as_str) != Some(active_configuration))
    {
        return refuse("the parent's policy was bound to a configuration that is no longer active");
    }
    let mut reauthorizations = Vec::new();
    if let Some(guidance) = policy.get("migration_guidance") {
        let frozen: Option<String> = connection.query_row(
            "SELECT json_extract(preserved_json,'$.guidance') FROM trip_legacy_migrations
             WHERE attempt_id=?1 AND to_workflow_id=?2 AND config_revision_id=?3
               AND target_manifest_hash=?4 AND target_workflow_hash=?5 AND target_source_hash=?6 AND target_overlay_hash=?7
               AND json_extract(preserved_json,'$.prior_policy.attempt_id')=?1
               AND json_extract(preserved_json,'$.prior_workflow')=from_workflow_id",
            params![parent_attempt_id,WORKFLOW_ID,active_configuration,manifest_hash,
                crate::workflow_resources::workflow_hash(),source_hash(),overlay_hash()], |row| row.get(0),
        ).optional()?;
        if frozen
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?
            .as_ref()
            != Some(guidance)
        {
            return refuse("the parent's migrated guidance lacks its exact authorized receipt");
        }
        reauthorizations.extend(
            guidance["guidance_reauthorizations"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    reauthorizations.extend(
        policy["guidance_reauthorizations"]
            .as_array()
            .into_iter()
            .flatten()
            .cloned(),
    );
    let manifest: crate::snapshot::SnapshotManifest = serde_json::from_str(&manifest_json)?;
    let snapshot_files = manifest
        .entries
        .into_iter()
        .filter(|entry| !entry.deleted && entry.kind == "file")
        .filter_map(|entry| entry.hash.map(|hash| (entry.path, hash)))
        .collect();
    Ok(Some(Ok(ReworkGuidanceSource {
        intent_id,
        parent_attempt_id,
        snapshot_id,
        parent_policy_hash: sha256(policy_json.as_bytes()),
        parent_files: policy
            .get("files")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default(),
        reauthorizations,
        snapshot_files,
    })))
}

pub fn materialize_project_policy(
    store: &Store,
    attempt_id: &str,
    workspace: &Path,
) -> Result<serde_json::Value> {
    let (root, manifest_hash, guidance, rework_source, migration_guidance): (
        String,
        String,
        String,
        Option<std::result::Result<ReworkGuidanceSource, String>>,
        Option<serde_json::Value>,
    ) = {
        let connection = store.lock()?;
        require_attempt_ready(&connection, attempt_id, None)?;
        let (root, manifest_hash, guidance, active_configuration): (String, String, String, String) = connection.query_row(
            "SELECT p.repository_path,s.manifest_hash,json_extract(r.config_json,'$.guidance'),r.id
             FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
             JOIN trip_project_state s ON s.project_id=p.id JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
             WHERE a.id=?1 AND s.readiness='ready'",
            params![attempt_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
        )?;
        let rework_source = rework_guidance_source(
            &connection,
            attempt_id,
            &manifest_hash,
            &active_configuration,
        )?;
        let migration: Option<(String, String, bool)> = connection.query_row(
            "SELECT preserved_json,from_workflow_id,COALESCE(config_revision_id=?3 AND target_manifest_hash=?4
                AND target_workflow_hash=?5 AND target_source_hash=?6 AND target_overlay_hash=?7,0)
             FROM trip_legacy_migrations WHERE attempt_id=?1 AND to_workflow_id=?2",
            params![attempt_id,WORKFLOW_ID,active_configuration,manifest_hash,
                crate::workflow_resources::workflow_hash(),source_hash(),overlay_hash()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        let migration_guidance = if let Some((preserved, from_workflow, target_bound)) = migration {
            let preserved: serde_json::Value = serde_json::from_str(&preserved)?;
            if !target_bound || preserved["prior_workflow"].as_str() != Some(from_workflow.as_str())
            {
                bail!("migration receipt does not match the authorized source and activated target")
            }
            Some(verified_migration_guidance(
                &connection,
                attempt_id,
                workspace,
                &preserved,
            )?)
        } else {
            None
        };
        (
            root,
            manifest_hash,
            guidance,
            rework_source,
            migration_guidance,
        )
    };
    let root = PathBuf::from(root).canonicalize()?;
    let manifest_path = root.join(".agents/trip-explorer/manifest.json");
    if hash_file(&manifest_path)?.as_deref() != Some(manifest_hash.as_str()) {
        bail!("activated manifest drifted before worktree materialization")
    }
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let mut copied = BTreeMap::new();
    let allowed_prefixes = [
        ".agents/skills/",
        ".agents/trip-explorer/bin/",
        ".agents/trip-explorer/base/",
    ];
    for (relative, expected) in manifest
        .get("base")
        .and_then(|v| v.as_object())
        .into_iter()
        .flatten()
    {
        validate_relative(relative)?;
        let source = root
            .join(".agents/trip-explorer/base")
            .join(PACKAGE_VERSION)
            .join(relative);
        let destination = workspace
            .join(".agents/trip-explorer/base")
            .join(PACKAGE_VERSION)
            .join(relative);
        let bytes = read_verified_regular(&root, &source, expected.as_str().unwrap_or_default())?;
        write_activated_policy_file(
            store,
            attempt_id,
            workspace,
            &destination,
            &bytes,
            expected.as_str().unwrap_or_default(),
        )?;
        copied.insert(
            destination
                .strip_prefix(workspace)?
                .to_string_lossy()
                .to_string(),
            expected.clone(),
        );
        if let Some(rest) = relative.strip_prefix("skills/") {
            let active_source = root.join(".agents/skills").join(rest);
            let active_destination = workspace.join(".agents/skills").join(rest);
            let active_bytes = read_verified_regular(
                &root,
                &active_source,
                expected.as_str().unwrap_or_default(),
            )?;
            write_activated_policy_file(
                store,
                attempt_id,
                workspace,
                &active_destination,
                &active_bytes,
                expected.as_str().unwrap_or_default(),
            )?;
            copied.insert(
                active_destination
                    .strip_prefix(workspace)?
                    .to_string_lossy()
                    .to_string(),
                expected.clone(),
            );
        }
    }
    for (relative, expected) in manifest
        .get("bin")
        .and_then(|v| v.as_object())
        .into_iter()
        .flatten()
    {
        validate_relative(relative)?;
        let source = root.join(".agents/trip-explorer/bin").join(relative);
        let destination = workspace.join(".agents/trip-explorer/bin").join(relative);
        let bytes = read_verified_regular(&root, &source, expected.as_str().unwrap_or_default())?;
        write_activated_policy_file(
            store,
            attempt_id,
            workspace,
            &destination,
            &bytes,
            expected.as_str().unwrap_or_default(),
        )?;
        copied.insert(
            destination
                .strip_prefix(workspace)?
                .to_string_lossy()
                .to_string(),
            expected.clone(),
        );
    }
    let manifest_bytes = fs::read(&manifest_path)?;
    for (relative, hash_key) in [
        (".agents/trip-explorer/config.json", "config_sha256"),
        (".agents/trip-explorer/adapters.json", "adapters_sha256"),
        (".agents/trip-explorer/preflight.json", "preflight_sha256"),
    ] {
        let expected = manifest
            .get(hash_key)
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("activated manifest is missing {hash_key}"))?;
        let source = root.join(relative);
        let bytes = read_verified_regular(&root, &source, expected)?;
        write_activated_policy_file(
            store,
            attempt_id,
            workspace,
            &workspace.join(relative),
            &bytes,
            expected,
        )?;
        copied.insert(
            relative.into(),
            serde_json::Value::String(expected.to_owned()),
        );
    }
    write_activated_policy_file(
        store,
        attempt_id,
        workspace,
        &workspace.join(".agents/trip-explorer/manifest.json"),
        &manifest_bytes,
        &manifest_hash,
    )?;
    copied.insert(
        ".agents/trip-explorer/manifest.json".into(),
        serde_json::Value::String(manifest_hash.clone()),
    );
    let guidance: Vec<String> = serde_json::from_str(&guidance)?;
    let mut preserved_guidance = Vec::new();
    for relative in guidance {
        validate_relative(&relative)?;
        if let Some(expected) = migration_guidance
            .as_ref()
            .and_then(|guidance| guidance["files"].get(&relative))
        {
            let hash = expected
                .as_str()
                .ok_or_else(|| anyhow!("frozen migration guidance hash is malformed"))?;
            read_verified_regular(workspace, &workspace.join(&relative), hash)?;
            copied.insert(relative, expected.clone());
            continue;
        }
        let source = contained_path(&root, &relative, true)?;
        let bytes = fs::read(&source)?;
        let expected = sha256(&bytes);
        let destination = workspace.join(&relative);
        if destination.exists() {
            let observed = hash_file(&destination)?;
            if observed.as_deref() != Some(expected.as_str()) {
                // A rework child keeps its source's guidance only where the
                // parent's policy pinned exactly that reauthorized content.
                let kept = match (&rework_source, observed.as_deref()) {
                    (None, _) => bail!(
                        "approved guidance collides with different worktree content: {relative}"
                    ),
                    (Some(Err(reason)), _) => Err(reason.clone()),
                    (Some(Ok(_)), None) => {
                        Err("the worktree path is not a regular file".to_owned())
                    }
                    (Some(Ok(source)), Some(observed)) => {
                        ensure_no_symlink_ancestry(workspace, &destination)?;
                        source.authorize(&relative, observed, &expected)
                    }
                };
                let provenance = kept.map_err(|reason| {
                    anyhow!("approved guidance collides with different worktree content: {relative}; the rework parent's authorized guidance cannot keep it: {reason}")
                })?;
                copied.insert(relative, provenance["sha256"].clone());
                preserved_guidance.push(provenance);
                continue;
            }
        } else {
            write_activated_policy_file(
                store,
                attempt_id,
                workspace,
                &destination,
                &bytes,
                &expected,
            )?;
        }
        copied.insert(relative, serde_json::Value::String(expected));
    }
    let connection = store.lock()?;
    let task_profiles = {
        let mut statement=connection.prepare("SELECT json_object('role',role,'settings_revision',settings_revision,'source',source,'profile',json(profile_json),'profile_hash',profile_hash,'project_config_revision_id',project_config_revision_id,'project_configuration_hash',project_configuration_hash,'adapter',adapter_name,'adapter_hash',adapter_hash,'capability_id',capability_id,'capability_key',capability_key,'capability_proof_hash',capability_proof_hash) FROM trip_attempt_profiles WHERE attempt_id=?1 ORDER BY role")?;
        let rows = statement
            .query_map(params![attempt_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|value| serde_json::from_str::<serde_json::Value>(&value))
            .collect::<serde_json::Result<Vec<_>>>()?
    };
    if task_profiles.len() != 6 {
        bail!("attempt policy materialization requires six frozen effective task profiles")
    }
    let mut policy = serde_json::json!({"kind":"activated_project","workflow_id":WORKFLOW_ID,"upstream_source_hash":source_hash(),"overlay_hash":overlay_hash(),"manifest_hash":manifest_hash,"files":copied,"allowed_prefixes":allowed_prefixes,"base_project_configuration_distinct":true,"task_profiles":task_profiles,"uncommitted_guidance":"approved overlay; not asserted present in base Git revision"});
    if let Some(guidance) = migration_guidance {
        // Prior approvals remain provenance, never current planning authority.
        policy["migration_guidance"] = guidance;
    }
    if !preserved_guidance.is_empty() {
        // Pins kept from the parent carry their provenance; no plan approval,
        // accepted snapshot or review outcome is inherited with them.
        policy["rework_guidance_sources"] = serde_json::Value::Array(preserved_guidance.clone());
    }
    validate_materialized_policy(&policy.to_string())?;
    let now = Utc::now().to_rfc3339();
    let changed = connection.execute(
        "UPDATE workspaces SET policy_json=?1,updated_at=?2 WHERE attempt_id=?3 AND state IN ('reserved','unknown','recovery_required')",
        params![policy.to_string(),now,attempt_id],
    )?;
    if changed != 1 {
        bail!("workspace reservation changed before policy persistence")
    }
    if !preserved_guidance.is_empty() {
        connection.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES(?1,?2,'service','rework.guidance.preserved','attempt',?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                attempt_id,
                serde_json::json!({"files":preserved_guidance}).to_string(),
                now
            ],
        )?;
    }
    Ok(policy)
}

/// Blob hashes already read from immutable commits, keyed by repository root,
/// revision and path.
fn committed_hash_cache() -> &'static std::sync::Mutex<HashMap<String, Option<String>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Option<String>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn committed_hashes(root: &Path, revision: &str, paths: &[&str]) -> Result<Vec<Option<String>>> {
    let key = |path: &str| format!("{}\0{revision}\0{path}", root.display());
    let mut cache = committed_hash_cache()
        .lock()
        .map_err(|_| anyhow!("committed file cache poisoned"))?;
    let missing = paths
        .iter()
        .copied()
        .filter(|path| !cache.contains_key(&key(path)))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        if cache.len() > 50_000 {
            cache.clear();
        }
        let files = crate::workspace::committed_files(root, revision, &missing)?;
        for (path, bytes) in missing.iter().zip(files) {
            cache.insert(key(path), bytes.as_deref().map(sha256));
        }
    }
    Ok(paths
        .iter()
        .map(|path| cache.get(&key(path)).cloned().flatten())
        .collect())
}

/// Explains why a new task in this project would fail to get its workflow
/// files, before the task is made Ready or started. A task workspace starts
/// from the project's registered commit, so an activated file whose committed
/// copy matches neither the activated bytes nor the approved original would be
/// refused rather than overwritten; repeated uncommitted setup changes produce
/// exactly that. Returns `None` only when the project is not ready, so the
/// check does not apply; a ready project whose folder, commit or workflow
/// files cannot be read is a problem, because nothing then proves a new task
/// could start. `check_head` also requires the folder to still be at the
/// registered commit.
pub fn start_baseline_problem(
    connection: &Connection,
    project_id: &str,
    check_head: bool,
) -> Result<Option<String>> {
    let row: Option<(String, String, String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT p.repository_path,p.base_revision,s.manifest_hash,
                    json_extract(r.config_json,'$.guidance'),s.setup_operation_id
             FROM projects p JOIN trip_project_state s ON s.project_id=p.id
             JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
             WHERE p.id=?1 AND s.readiness='ready' AND s.manifest_hash IS NOT NULL",
            params![project_id],
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
    let Some((root, base, manifest_hash, guidance, setup_operation_id)) = row else {
        return Ok(None);
    };
    let registered = PathBuf::from(root);
    let Ok(root) = registered.canonicalize() else {
        return Ok(Some(format!(
            "LLMRelay cannot open the project folder {}. Restore the folder, or if the repository moved, enter its new path in Project settings and choose Validate and relink.",
            registered.display()
        )));
    };
    let short = |revision: &str| revision.chars().take(10).collect::<String>();
    if check_head {
        let Ok(head) = crate::workspace::head(&root) else {
            return Ok(Some(format!(
                "LLMRelay cannot read the current commit of the project folder {}. Check that the folder is still the project's Git repository, then choose Validate and relink in Project settings.",
                root.display()
            )));
        };
        if head != base {
            return Ok(Some(format!(
                "The project folder is now at commit {}, but LLMRelay starts new tasks from commit {}. Choose Validate and relink in Project settings to start new tasks from the current commit. Relinking is available when no task in this project is running.",
                short(&head),
                short(&base)
            )));
        }
    }
    // A missing commit would otherwise read as a commit without any of the
    // files below, which never conflicts.
    let base_readable = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["cat-file", "-e", &format!("{base}^{{commit}}")])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !base_readable {
        return Ok(Some(format!(
            "LLMRelay cannot read commit {}, which new tasks in this project start from, in the project folder {}. Check that the folder is still the project's Git repository, then choose Validate and relink in Project settings so new tasks start from its current commit.",
            short(&base),
            root.display()
        )));
    }
    let manifest_path = root.join(".agents/trip-explorer/manifest.json");
    let Some(observed_manifest) = hash_file(&manifest_path)? else {
        return Ok(Some(
            "The activated workflow files are missing from the project folder (.agents/trip-explorer/manifest.json). Restore them, or open Project setup to set up the project again.".into(),
        ));
    };
    if observed_manifest != manifest_hash {
        return Ok(Some(
            "The activated workflow files in the project folder changed after setup (.agents/trip-explorer/manifest.json). Open Project setup to review the change. LLMRelay will not overwrite it.".into(),
        ));
    }
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    // Mirrors the destinations `materialize_project_policy` writes.
    let mut expected: Vec<(String, String, bool)> = Vec::new();
    for (relative, hash) in manifest
        .get("base")
        .and_then(|value| value.as_object())
        .into_iter()
        .flatten()
    {
        validate_relative(relative)?;
        let hash = hash.as_str().unwrap_or_default().to_owned();
        expected.push((
            format!(".agents/trip-explorer/base/{PACKAGE_VERSION}/{relative}"),
            hash.clone(),
            false,
        ));
        if let Some(rest) = relative.strip_prefix("skills/") {
            expected.push((format!(".agents/skills/{rest}"), hash, false));
        }
    }
    for (relative, hash) in manifest
        .get("bin")
        .and_then(|value| value.as_object())
        .into_iter()
        .flatten()
    {
        validate_relative(relative)?;
        expected.push((
            format!(".agents/trip-explorer/bin/{relative}"),
            hash.as_str().unwrap_or_default().to_owned(),
            false,
        ));
    }
    for (relative, hash_key) in [
        (".agents/trip-explorer/config.json", "config_sha256"),
        (".agents/trip-explorer/adapters.json", "adapters_sha256"),
        (".agents/trip-explorer/preflight.json", "preflight_sha256"),
    ] {
        if let Some(hash) = manifest.get(hash_key).and_then(|value| value.as_str()) {
            expected.push((relative.into(), hash.into(), false));
        }
    }
    expected.push((
        ".agents/trip-explorer/manifest.json".into(),
        manifest_hash.clone(),
        false,
    ));
    let guidance: Vec<String> = guidance
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    for relative in guidance {
        validate_relative(&relative)?;
        if let Some(hash) = hash_file(&root.join(&relative))? {
            expected.push((relative, hash, true));
        }
    }
    let paths = expected
        .iter()
        .map(|(path, _, _)| path.as_str())
        .collect::<Vec<_>>();
    let Ok(committed) = committed_hashes(&root, &base, &paths) else {
        return Ok(Some(format!(
            "LLMRelay could not read the workflow files in commit {} of the project folder, so it cannot confirm that new tasks can start. Check that the folder's Git repository is readable, then try again.",
            short(&base)
        )));
    };
    let mut conflicts = Vec::new();
    for ((relative, expected, guidance), committed) in expected.iter().zip(committed) {
        let Some(committed) = committed else {
            continue;
        };
        if &committed == expected {
            continue;
        }
        // The activated installation may replace exactly its approved original.
        let approved = !guidance
            && connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM trip_frozen_install_files f
                   JOIN trip_setup_operations so ON so.id=f.setup_operation_id
                   WHERE so.id=?1 AND so.state='activated' AND so.install_authorized_at IS NOT NULL
                     AND so.approved_source_set_hash=so.final_source_set_hash
                     AND so.approved_preimages_hash IS NOT NULL
                     AND f.relative_path=?2 AND f.source_hash=?3 AND f.preimage_hash=?4)",
                params![setup_operation_id, relative, expected, committed],
                |row| row.get::<_, bool>(0),
            )?;
        if !approved {
            conflicts.push(relative.as_str());
        }
    }
    if conflicts.is_empty() {
        return Ok(None);
    }
    let listed = conflicts
        .iter()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let more = conflicts.len().saturating_sub(5);
    Ok(Some(format!(
        "Files that new tasks need were changed after commit {} and are not committed: {listed}{}. New tasks start from that commit, so LLMRelay will not start them or overwrite these files. Commit or restore the changes, then choose Validate and relink in Project settings so new tasks start from the current commit.",
        short(&base),
        if more > 0 { format!(", and {more} more") } else { String::new() }
    )))
}

fn activate_configuration(
    store: &Store,
    setup_id: &str,
    project_id: &str,
    root: &Path,
    proposal: &SetupProposal,
    manifest_hash: &str,
    runtime: &CapabilityRuntime,
) -> Result<String> {
    verify_applied_journal(store, setup_id, root)?;
    let now = Utc::now().to_rfc3339();
    let quiescence = verify_project_activation_boundary(store, project_id)?;
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project_activation_boundary(&tx, project_id, &quiescence)?;
    let complete:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM trip_apply_journal WHERE setup_operation_id=?1 AND state!='applied')",params![setup_id],|row|row.get(0))?;
    if !complete {
        bail!("installation journal is incomplete")
    }
    require_all_setup_proofs(&tx, setup_id, runtime)?;
    let revision = insert_config_revision(
        &tx,
        project_id,
        proposal,
        "activated",
        &now,
        setup_id,
        runtime,
    )?;
    let manager_json:String=tx.query_row(
        "SELECT profile_json FROM trip_setup_profile_selections WHERE setup_operation_id=?1 AND role='manager' AND selection_state='selected'",
        params![setup_id],|row|row.get(0)
    )?;
    let manager: RoleOverride = serde_json::from_str(&manager_json)?;
    let mut roles = serde_json::Map::new();
    roles.insert("manager".into(), serde_json::to_value(manager)?);
    for role in DELEGATED_ROLES {
        let name = role.to_string();
        let selection = proposal
            .roles
            .get(&name)
            .ok_or_else(|| anyhow!("activated setup is missing role {name}"))?;
        let profile = proposal
            .profiles
            .get(&selection.profile)
            .ok_or_else(|| anyhow!("activated setup is missing profile {}", selection.profile))?;
        roles.insert(
            name,
            serde_json::to_value(RoleOverride {
                provider: profile
                    .provider
                    .parse()
                    .map_err(|error: String| anyhow!(error))?,
                model: profile.model.clone(),
                effort: profile.effort.clone(),
            })?,
        );
    }
    let prior_settings: String = tx.query_row(
        "SELECT settings_json FROM projects WHERE id=?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let mut settings: serde_json::Value = serde_json::from_str(&prior_settings)?;
    let settings_object = settings
        .as_object_mut()
        .ok_or_else(|| anyhow!("project settings must be an object"))?;
    settings_object.insert("roles".into(), serde_json::Value::Object(roles));
    settings_object.insert(
        "trip_config_revision_id".into(),
        serde_json::Value::String(revision.clone()),
    );
    tx.execute(
        "UPDATE trip_config_revisions SET state='superseded' WHERE project_id=?1 AND state='activated' AND id!=?2",
        params![project_id,revision],
    )?;
    tx.execute("UPDATE trip_setup_operations SET state='activated',error=NULL,updated_at=?1 WHERE id=?2 AND state IN ('applying','recovery_required')",params![now,setup_id])?;
    tx.execute(
        "UPDATE trip_setup_operations SET state='superseded',updated_at=?1
         WHERE id=(SELECT supersedes_setup_operation_id FROM trip_setup_operations WHERE id=?2) AND state='activated'",
        params![now,setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state SET readiness='ready',reason='Pinned TRIP Explorer installation is activated',detected_installation='compatible',active_config_revision_id=?1,
         workflow_id=?2,package_version=?3,upstream_source_hash=?4,overlay_hash=?5,manifest_hash=?6,activated_at=?7,updated_at=?7 WHERE project_id=?8 AND setup_operation_id=?9",
        params![revision,WORKFLOW_ID,PACKAGE_VERSION,source_hash(),overlay_hash(),manifest_hash,now,project_id,setup_id],
    )?;
    tx.execute(
        "UPDATE projects SET settings_json=?1,version=version+1,updated_at=?2 WHERE id=?3",
        params![settings.to_string(), now, project_id],
    )?;
    mark_project_attempts_for_migration(&tx, project_id, &now)?;
    tx.commit()?;
    Ok(revision)
}

fn insert_config_revision(
    connection: &Connection,
    project_id: &str,
    proposal: &SetupProposal,
    state: &str,
    now: &str,
    setup_id: &str,
    runtime: &CapabilityRuntime,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let revision: i64 = connection.query_row(
        "SELECT COALESCE(MAX(revision),0)+1 FROM trip_config_revisions WHERE project_id=?1",
        params![project_id],
        |row| row.get(0),
    )?;
    let config = project_configuration(proposal)?;
    let preflight =
        effective_preflight_json(connection, setup_id, proposal, runtime)?["receipts"].to_string();
    connection.execute(
        "INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,preflight_json,verification_json,source_hash,overlay_hash,configuration_hash,created_at,activated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
        params![id,project_id,revision,state,config.to_string(),proposal.adapters.to_string(),preflight,proposal.verification.to_string(),source_hash(),overlay_hash(),json_hash(&config)?,now],
    )?;
    insert_verification_checks(
        connection,
        project_id,
        &id,
        &proposal.verification,
        &proposal.verification_contracts,
    )?;
    Ok(id)
}

fn insert_verification_checks(
    connection: &Connection,
    project_id: &str,
    revision: &str,
    verification: &serde_json::Value,
    contracts: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    for category in ["focused", "broad", "cleanup"] {
        let commands = verification
            .get(category)
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("verification.{category} must be an array"))?;
        for (index, command) in commands.iter().enumerate() {
            let default_key = format!("{category}_{}", index + 1);
            let contract = contracts.get(&default_key).unwrap_or(command);
            let (
                key,
                kind,
                executable,
                arguments,
                shell,
                cwd,
                timeout,
                acceptance,
                inputs,
                invalidation,
                original,
            ) = verification_command(category, index, contract)?;
            if original != command.as_str().unwrap_or_default() {
                bail!("verification contract original_text must exactly preserve its upstream command text")
            }
            connection.execute(
                "INSERT INTO trip_verification_checks(id,project_id,config_revision_id,check_key,category,command_kind,executable,arguments_json,shell_command,cwd,timeout_seconds,acceptance_rows_json,relevant_inputs_json,invalidation_json,original_text)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![uuid::Uuid::new_v4().to_string(),project_id,revision,key,category,kind,executable,arguments,shell,cwd,timeout,acceptance,inputs,invalidation,original],
            )?;
        }
    }
    Ok(())
}

fn reserve_apply_journal(
    store: &Store,
    setup_id: &str,
    staging: &Path,
    files: &[JournalFile],
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state: String = tx.query_row(
        "SELECT state FROM trip_setup_operations WHERE id=?1",
        params![setup_id],
        |row| row.get(0),
    )?;
    if state != "install_authorized" && state != "recovery_required" {
        bail!("setup operation is not authorized for apply")
    }
    for file in files {
        let staged = staging.join(&file.relative);
        tx.execute(
            "INSERT OR REPLACE INTO trip_apply_journal(id,setup_operation_id,relative_path,source_hash,expected_preimage_hash,observed_preimage_hash,staged_path,state,created_by_operation,error,created_at,updated_at)
             VALUES(COALESCE((SELECT id FROM trip_apply_journal WHERE setup_operation_id=?1 AND relative_path=?2),?3),?1,?2,?4,?5,?5,?6,'staged',?7,NULL,COALESCE((SELECT created_at FROM trip_apply_journal WHERE setup_operation_id=?1 AND relative_path=?2),?8),?8)",
            params![setup_id,file.relative,uuid::Uuid::new_v4().to_string(),file.source_hash,file.preimage_hash,staged.to_string_lossy(),file.preimage_hash.is_none(),now],
        )?;
    }
    tx.execute(
        "UPDATE trip_setup_operations SET state='applying',error=NULL,updated_at=?1 WHERE id=?2",
        params![now, setup_id],
    )?;
    tx.execute(
        "UPDATE trip_project_state SET readiness='setup_in_progress',reason='Authorized configuration apply is journaled; new task admission is paused',updated_at=?1 WHERE setup_operation_id=?2",
        params![now,setup_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn frozen_installation_files(store: &Store, setup_id: &str) -> Result<Vec<JournalFile>> {
    let connection = store.lock()?;
    frozen_installation_files_from(&connection, setup_id)
}

fn frozen_installation_files_from(
    connection: &Connection,
    setup_id: &str,
) -> Result<Vec<JournalFile>> {
    let mut statement = connection.prepare(
        "SELECT relative_path,source_hash,preimage_hash,source_bytes,preimage_bytes
         FROM trip_frozen_install_files WHERE setup_operation_id=?1 ORDER BY relative_path",
    )?;
    let files = statement
        .query_map(params![setup_id], |row| {
            Ok(JournalFile {
                relative: row.get(0)?,
                source_hash: row.get(1)?,
                preimage_hash: row.get(2)?,
                bytes: row.get(3)?,
                preimage_bytes: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if files.is_empty() {
        bail!("authorized installation has no frozen source files")
    }
    for file in &files {
        if sha256(&file.bytes) != file.source_hash
            || file.preimage_bytes.as_ref().map(|bytes| sha256(bytes)) != file.preimage_hash
        {
            bail!(
                "frozen installation file binding is corrupt for {}",
                file.relative
            )
        }
    }
    Ok(files)
}

fn current_preimage_bindings(root: &Path, files: &[JournalFile]) -> Result<Vec<serde_json::Value>> {
    files.iter().map(|file|->Result<serde_json::Value>{
        let destination=if file.relative=="@git-common/info/exclude" {
            git_exclude_path(root)?
        } else {
            contained_path(root,&file.relative,false)?
        };
        Ok(serde_json::json!({"relative_path":file.relative,"preimage_sha256":hash_file(&destination)?}))
    }).collect()
}

fn apply_journal(store: &Store, setup_id: &str, root: &Path) -> Result<()> {
    let rows = {
        let connection = store.lock()?;
        let mut statement=connection.prepare("SELECT relative_path,source_hash,expected_preimage_hash,staged_path,state FROM trip_apply_journal WHERE setup_operation_id=?1 ORDER BY relative_path")?;
        let rows = statement
            .query_map(params![setup_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (relative, source_hash, preimage, staged, state) in rows {
        validate_relative_or_git_exclude(&relative)?;
        let destination = journal_destination(root, &relative)?;
        let observed = hash_file(&destination)?;
        if state == "applied" {
            if observed.as_deref() != Some(source_hash.as_str()) {
                bail!("applied journal destination drifted and was preserved: {relative}")
            }
            continue;
        }
        if observed.as_deref() == Some(source_hash.as_str()) {
            let connection = store.lock()?;
            connection.execute("UPDATE trip_apply_journal SET state='applied',updated_at=?1 WHERE setup_operation_id=?2 AND relative_path=?3",params![Utc::now().to_rfc3339(),setup_id,relative])?;
            continue;
        }
        if observed != preimage {
            bail!("destination preimage changed during apply: {relative}")
        }
        let bytes = fs::read(&staged).with_context(|| format!("read staged {relative}"))?;
        if sha256(&bytes) != source_hash {
            bail!("staged source hash changed: {relative}")
        }
        if relative != "@git-common/info/exclude" {
            ensure_safe_parent(root, &destination)?;
        }
        atomic_write(&destination, &bytes)?;
        if hash_file(&destination)?.as_deref() != Some(source_hash.as_str()) {
            bail!("applied bytes failed verification: {relative}")
        }
        let connection = store.lock()?;
        connection.execute("UPDATE trip_apply_journal SET state='applied',updated_at=?1 WHERE setup_operation_id=?2 AND relative_path=?3",params![Utc::now().to_rfc3339(),setup_id,relative])?;
    }
    verify_applied_journal(store, setup_id, root)
}

fn journal_destination(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_or_git_exclude(relative)?;
    if relative == "@git-common/info/exclude" {
        git_exclude_path(root)
    } else {
        contained_path(root, relative, false)
    }
}

fn verify_applied_journal(store: &Store, setup_id: &str, root: &Path) -> Result<()> {
    let rows = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT relative_path,source_hash,state FROM trip_apply_journal WHERE setup_operation_id=?1 ORDER BY relative_path",
        )?;
        let rows = statement
            .query_map(params![setup_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    if rows.is_empty() {
        bail!("installation journal is empty")
    }
    for (relative, source_hash, state) in rows {
        if state != "applied" {
            bail!("installation journal destination is not applied: {relative}")
        }
        let destination = journal_destination(root, &relative)?;
        if hash_file(&destination)?.as_deref() != Some(source_hash.as_str()) {
            bail!("applied journal destination drifted and was preserved: {relative}")
        }
    }
    Ok(())
}

pub(crate) fn reconcile_interrupted_applies(store: &Store) -> Result<Vec<serde_json::Value>> {
    let applying = {
        let connection = store.lock()?;
        let mut statement = connection.prepare(
            "SELECT so.id,so.project_id,so.proposal_hash,so.approved_preimages_hash,
                    so.final_source_set_hash,
                    (SELECT COUNT(*) FROM trip_apply_journal journal
                       WHERE journal.setup_operation_id=so.id),
                    (SELECT COUNT(*) FROM trip_frozen_install_files files
                       WHERE files.setup_operation_id=so.id)
             FROM trip_setup_operations so WHERE so.state='applying' ORDER BY so.updated_at,so.id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut recovered = Vec::new();
    for (
        setup_id,
        project_id,
        proposal_hash,
        preimages_hash,
        source_hash,
        journal_rows,
        frozen_rows,
    ) in applying
    {
        let complete_binding = proposal_hash.is_some()
            && preimages_hash.is_some()
            && source_hash.is_some()
            && journal_rows > 0
            && frozen_rows > 0;
        let reason = if complete_binding {
            "service startup found an installation interrupted while applying the frozen authorized journal; no project files were changed during reconciliation"
        } else {
            "service startup found an installation interrupted while applying, but its frozen authorization or apply journal is incomplete; project bytes were preserved for explicit recovery"
        };
        mark_setup_recovery(store, &setup_id, reason)?;
        recovered.push(serde_json::json!({
            "setup_operation_id":setup_id,"project_id":project_id,
            "state":"recovery_required","journal_rows":journal_rows,
            "frozen_rows":frozen_rows,"complete_binding":complete_binding,
            "project_files_written":false,
        }));
    }
    Ok(recovered)
}

fn mark_setup_recovery(store: &Store, setup_id: &str, error: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = transaction.execute("UPDATE trip_setup_operations SET state='recovery_required',error=?1,updated_at=?2 WHERE id=?3 AND state IN ('applying','recovery_required')",params![error,now,setup_id])?;
    if changed == 0 {
        transaction.commit()?;
        return Ok(());
    }
    transaction.execute("UPDATE trip_project_state SET readiness='recovery_required',reason=?1,updated_at=?2 WHERE setup_operation_id=?3",params![error,now,setup_id])?;
    transaction.execute(
        "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
         VALUES(?1,?2,'service','trip.setup.apply_recovery_required','trip_setup',?3,?4,?5)",
        params![
            uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string(), setup_id,
            serde_json::json!({"reason":error,"project_files_written_by_reconciliation":false}).to_string(), now
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

fn stage_files(staging: &Path, files: &[JournalFile]) -> Result<()> {
    if staging.exists() {
        for file in files {
            let path = staging.join(&file.relative);
            if hash_file(&path)?.as_deref() != Some(file.source_hash.as_str()) {
                bail!(
                    "existing staged bytes do not match approved source: {}",
                    file.relative
                )
            }
        }
        return Ok(());
    }
    fs::create_dir_all(staging)?;
    for file in files {
        let path = staging.join(&file.relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(&path, &file.bytes)?;
    }
    Ok(())
}

fn bounded_inventory(store: &Store, project_id: &str) -> Result<serde_json::Value> {
    let path: String = {
        let connection = store.lock()?;
        connection.query_row(
            "SELECT repository_path FROM projects WHERE id=?1",
            params![project_id],
            |row| row.get(0),
        )?
    };
    let repo = crate::workspace::inspect(Path::new(&path))?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&repo.root)?.take(256) {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if is_secret_like(&name) {
            continue;
        }
        let kind = entry.file_type()?;
        entries.push(serde_json::json!({"name":name,"kind":if kind.is_dir(){"directory"}else if kind.is_file(){"file"}else{"other"}}));
    }
    let installation = detect_installation(&repo.root)?;
    Ok(
        serde_json::json!({"repository_root":repo.root,"repository_identity":repo.identity,"head":repo.head,"branch":repo.branch,"dirty":repo.dirty,"entries":entries,"trip_installation":installation}),
    )
}

fn create_empty_fixture(path: &Path, identity_source: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let name = git_config(identity_source, "user.name")?;
    let email = git_config(identity_source, "user.email")?;
    fs::create_dir_all(path)?;
    command(path, &["init", "--quiet"])?;
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            &format!("user.name={name}"),
            "-c",
            &format!("user.email={email}"),
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "Initialize setup fixture",
        ])
        .status()?;
    if !status.success() {
        bail!("create internal setup fixture base commit failed")
    }
    Ok(())
}

fn git_config(path: &Path, key: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["config", "--get", key])
        .output()?;
    if !output.status.success() {
        bail!("configured Git {key} is required for the internal fixture")
    }
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    if value.is_empty() {
        bail!("configured Git {key} is blank")
    }
    Ok(value)
}

fn command(path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(())
}

fn selected_profiles_hash(proposal: &SetupProposal) -> Result<String> {
    let selected = proposal
        .roles
        .iter()
        .map(|(role, mapping)| (role, proposal.profiles.get(&mapping.profile)))
        .collect::<Vec<_>>();
    json_hash(&selected)
}
fn unique_profile_count(proposal: &SetupProposal) -> usize {
    proposal
        .roles
        .values()
        .map(|r| r.profile.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}
fn validate_role_override(config: &RoleOverride) -> Result<()> {
    if config.model.trim().is_empty() || config.effort.trim().is_empty() {
        bail!("host manager model and effort are required")
    }
    Ok(())
}
fn project_role_settings(
    proposal: &SetupProposal,
    manager: &RoleOverride,
) -> Result<serde_json::Value> {
    let mut roles = serde_json::Map::new();
    roles.insert("manager".into(), serde_json::to_value(manager)?);
    for role in DELEGATED_ROLES {
        let name = role.to_string();
        let selection = proposal
            .roles
            .get(&name)
            .ok_or_else(|| anyhow!("configuration is missing role {name}"))?;
        let profile = proposal
            .profiles
            .get(&selection.profile)
            .ok_or_else(|| anyhow!("configuration is missing profile {}", selection.profile))?;
        roles.insert(
            name,
            serde_json::to_value(RoleOverride {
                provider: profile
                    .provider
                    .parse()
                    .map_err(|error: String| anyhow!(error))?,
                model: profile.model.clone(),
                effort: profile.effort.clone(),
            })?,
        );
    }
    Ok(serde_json::Value::Object(roles))
}
fn operation_result(
    operation_id: &str,
    kind: &str,
    id: &str,
    version: Option<i64>,
    state: &str,
    detail: serde_json::Value,
) -> OperationResult {
    OperationResult {
        operation_id: operation_id.into(),
        entity_kind: kind.into(),
        entity_id: id.into(),
        version,
        state: state.into(),
        detail,
    }
}
fn persist_trip_receipt(
    store: &Store,
    operation_id: &str,
    request_hash: &str,
    result: &OperationResult,
) -> Result<()> {
    let connection = store.lock()?;
    let now = Utc::now().to_rfc3339();
    connection.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,new_version,detail_json,created_at) VALUES(?1,?2,'human','trip.command.applied',?3,?4,?5,?6,?7)",params![uuid::Uuid::new_v4().to_string(),operation_id,result.entity_kind,result.entity_id,result.version,serde_json::to_string(result)?,now])?;
    connection.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at) VALUES(?1,'human_control','trip_command',?2,?3,?4)",params![operation_id,request_hash,serde_json::to_string(result)?,now])?;
    Ok(())
}
fn pretty_json(value: &serde_json::Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("symlink destination is unsafe: {}", path.display())
        }
        Ok(meta) if meta.is_file() => Ok(Some(sha256(&fs::read(path)?))),
        Ok(_) => bail!("destination is not a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn journal_file(root: &Path, relative: &str, bytes: Vec<u8>) -> Result<JournalFile> {
    validate_relative_or_git_exclude(relative)?;
    let destination = contained_path(root, relative, false)?;
    let preimage_bytes = match fs::symlink_metadata(&destination) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("symlink destination is unsafe: {}", destination.display())
        }
        Ok(meta) if meta.is_file() => Some(fs::read(&destination)?),
        Ok(_) => bail!(
            "destination is not a regular file: {}",
            destination.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    Ok(JournalFile {
        relative: relative.into(),
        source_hash: sha256(&bytes),
        preimage_hash: preimage_bytes.as_ref().map(|value| sha256(value)),
        preimage_bytes,
        bytes,
    })
}
fn validate_relative(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || (value != "." && value.split('/').any(|part| part.is_empty() || part == "."))
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            ) || (matches!(c, Component::CurDir) && value != ".")
        })
    {
        bail!("path must be a normalized contained relative path: {value}")
    }
    Ok(())
}
fn validate_relative_or_git_exclude(value: &str) -> Result<()> {
    if value == "@git-common/info/exclude" {
        Ok(())
    } else {
        validate_relative(value)
    }
}
pub(crate) fn contained_path(root: &Path, relative: &str, must_exist: bool) -> Result<PathBuf> {
    validate_relative_or_git_exclude(relative)?;
    let candidate = root.join(relative);
    ensure_no_symlink_ancestry(root, &candidate)?;
    if must_exist {
        let resolved = candidate.canonicalize()?;
        if resolved != *root && !resolved.starts_with(root) {
            bail!("path escapes repository")
        }
    }
    Ok(candidate)
}
fn ensure_no_symlink_ancestry(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root).context("path escapes root")?;
    let mut cursor = root.to_path_buf();
    for part in relative.components() {
        cursor.push(part);
        match fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("path ancestry contains symlink: {}", cursor.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn ensure_safe_parent(root: &Path, path: &Path) -> Result<()> {
    ensure_no_symlink_ancestry(root, path)?;
    let parent = path.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    ensure_no_symlink_ancestry(root, path)
}
fn write_activated_policy_file(
    store: &Store,
    attempt_id: &str,
    root: &Path,
    destination: &Path,
    bytes: &[u8],
    expected: &str,
) -> Result<()> {
    ensure_no_symlink_ancestry(root, destination)?;
    let observed = hash_file(destination)?;
    if observed.is_none() || observed.as_deref() == Some(expected) {
        return write_new_verified(root, destination, bytes, expected);
    }
    let relative = destination.strip_prefix(root)?.to_string_lossy();
    let approved: Option<(String, Vec<u8>, Vec<u8>)> = store
        .lock()?
        .query_row(
            "SELECT f.preimage_hash,f.source_bytes,f.preimage_bytes
         FROM attempts a JOIN tasks t ON t.id=a.task_id
         JOIN projects p ON p.id=t.project_id
         JOIN workspaces w ON w.attempt_id=a.id
         JOIN trip_project_state s ON s.project_id=p.id
         JOIN trip_setup_operations so ON so.id=s.setup_operation_id AND so.project_id=p.id
         JOIN trip_frozen_install_files f ON f.setup_operation_id=so.id
         WHERE a.id=?1 AND w.path=?2 AND w.path!=p.repository_path
           AND w.state IN ('reserved','unknown','recovery_required')
           AND s.readiness='ready' AND so.state='activated'
           AND so.install_authorized_at IS NOT NULL
           AND so.approved_source_set_hash=so.final_source_set_hash
           AND so.approved_preimages_hash IS NOT NULL
           AND f.relative_path=?3 AND f.source_hash=?4 AND f.preimage_hash IS NOT NULL",
            params![attempt_id, root.to_string_lossy(), relative, expected],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let installation_approved = approved.is_some_and(|(preimage, source_bytes, preimage_bytes)| {
        observed.as_deref() == Some(preimage.as_str())
            && sha256(&preimage_bytes) == preimage
            && sha256(&source_bytes) == expected
            && source_bytes == bytes
    });
    let migration_approved = if !installation_approved {
        let connection = store.lock()?;
        if matches!(
            pending_attempt_migration(&connection, attempt_id)?,
            Some(Ok(_))
        ) {
            let (prior, project_root, manifest_hash): (String, String, String) = connection.query_row(
                "SELECT json_extract(receipt.preserved_json,'$.prior_policy'),p.repository_path,s.manifest_hash
                 FROM trip_legacy_migrations receipt JOIN attempts a ON a.id=receipt.attempt_id
                 JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id
                 JOIN trip_project_state s ON s.project_id=p.id JOIN workspaces w ON w.attempt_id=a.id
                 WHERE a.id=?1 AND receipt.to_workflow_id=?2 AND w.path=?3 AND w.path!=p.repository_path",
                params![attempt_id,WORKFLOW_ID,root.to_string_lossy()],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?;
            let prior: serde_json::Value = serde_json::from_str(&prior)?;
            let policy_json = prior["policy_json"]
                .as_str()
                .ok_or_else(|| anyhow!("migration receipt lacks its prior policy"))?;
            if prior["policy_hash"].as_str() != Some(sha256(policy_json.as_bytes()).as_str()) {
                bail!("migration prior policy hash changed")
            }
            let policy: serde_json::Value = serde_json::from_str(policy_json)?;
            let project_root = PathBuf::from(project_root).canonicalize()?;
            let target_manifest = read_verified_regular(
                &project_root,
                &project_root.join(".agents/trip-explorer/manifest.json"),
                &manifest_hash,
            )?;
            let manifest: serde_json::Value = serde_json::from_slice(&target_manifest)?;
            let base_prefix = format!(".agents/trip-explorer/base/{PACKAGE_VERSION}/");
            let target_hash = if relative == ".agents/trip-explorer/manifest.json" {
                Some(manifest_hash.as_str())
            } else if let Some(path) = relative.strip_prefix(base_prefix.as_str()) {
                manifest["base"][path].as_str()
            } else if let Some(path) = relative.strip_prefix(".agents/skills/") {
                manifest["base"][format!("skills/{path}")].as_str()
            } else if let Some(path) = relative.strip_prefix(".agents/trip-explorer/bin/") {
                manifest["bin"][path].as_str()
            } else {
                match relative.as_ref() {
                    ".agents/trip-explorer/config.json" => manifest["config_sha256"].as_str(),
                    ".agents/trip-explorer/adapters.json" => manifest["adapters_sha256"].as_str(),
                    ".agents/trip-explorer/preflight.json" => manifest["preflight_sha256"].as_str(),
                    _ => None,
                }
            };
            policy["files"][relative.as_ref()].as_str() == observed.as_deref()
                && target_hash == Some(expected)
                && sha256(bytes) == expected
        } else {
            false
        }
    } else {
        false
    };
    if !installation_approved && !migration_approved {
        bail!(
            "policy materialization collision: {}",
            destination.display()
        )
    }
    // Installation or migration authority binds this exact old-to-new byte pair.
    ensure_safe_parent(root, destination)?;
    if hash_file(destination)? != observed {
        bail!("approved policy preimage changed before worktree replacement")
    }
    atomic_write(destination, bytes)?;
    if hash_file(destination)?.as_deref() != Some(expected) {
        bail!("policy materialization hash mismatch")
    }
    Ok(())
}

fn write_new_verified(root: &Path, destination: &Path, bytes: &[u8], expected: &str) -> Result<()> {
    if sha256(bytes) != expected {
        bail!(
            "embedded package hash mismatch for {}",
            destination.display()
        )
    }
    ensure_safe_parent(root, destination)?;
    if destination.exists() {
        if hash_file(destination)?.as_deref() == Some(expected) {
            return Ok(());
        }
        bail!(
            "policy materialization collision: {}",
            destination.display()
        )
    }
    atomic_write(destination, bytes)?;
    if hash_file(destination)?.as_deref() != Some(expected) {
        bail!("policy materialization hash mismatch")
    };
    Ok(())
}
fn read_verified_regular(root: &Path, path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure_no_symlink_ancestry(root, path)?;
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        bail!(
            "activated policy path is not a regular file: {}",
            path.display()
        )
    }
    let bytes = fs::read(path)?;
    if sha256(&bytes) != expected {
        bail!("activated package file drifted: {}", path.display())
    }
    Ok(bytes)
}
fn is_secret_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower == ".env"
        || lower.contains("credential")
        || lower.contains("secret")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
}
fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn path_matches_scope(path: &str, scope: &str) -> bool {
    scope == "."
        || path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}
fn scopes_overlap(left: &str, right: &str) -> bool {
    path_matches_scope(left, right) || path_matches_scope(right, left)
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn meaningful_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => !text.trim().is_empty(),
        serde_json::Value::Object(object) => !object.is_empty(),
        _ => false,
    }
}
fn contains_sensitive_evidence(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(entries) => entries.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            matches!(
                key.as_str(),
                "credential"
                    | "credentials"
                    | "secret"
                    | "secrets"
                    | "token"
                    | "access_token"
                    | "refresh_token"
                    | "hidden_reasoning"
                    | "chain_of_thought"
            ) || contains_sensitive_evidence(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_sensitive_evidence),
        _ => false,
    }
}
fn contains_prohibited_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            [
                "credential",
                "secret",
                "token",
                "hidden_reasoning",
                "chain_of_thought",
            ]
            .iter()
            .any(|needle| key.contains(needle))
                || contains_prohibited_key(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_prohibited_key),
        _ => false,
    }
}
fn string_array(value: &serde_json::Value, key: &str) -> Result<Vec<String>> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("{key} must be an array"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("{key} entries must be strings"))
        })
        .collect()
}
fn validate_verification_contracts(
    value: &serde_json::Value,
    contracts: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    let mut keys = BTreeSet::new();
    for category in ["focused", "broad", "cleanup"] {
        let commands = value
            .get(category)
            .and_then(|entry| entry.as_array())
            .ok_or_else(|| anyhow!("verification.{category} must be an array"))?;
        for (index, command) in commands.iter().enumerate() {
            let expected_key = format!("{category}_{}", index + 1);
            let contract = contracts.get(&expected_key).unwrap_or(command);
            let (key, _, _, _, _, _, _, _, _, _, original) =
                verification_command(category, index, contract)?;
            if original != command.as_str().unwrap_or_default() {
                bail!("verification contract original_text must exactly preserve its upstream command text")
            }
            if key != expected_key {
                bail!("verification contract key must match its upstream matrix position {expected_key}")
            }
            if !keys.insert(key) {
                bail!("verification check keys must be unique across the project matrix")
            }
        }
    }
    if contracts.keys().any(|key| !keys.contains(key)) {
        bail!("verification contract does not correspond to an upstream matrix entry")
    }
    Ok(())
}
fn verification_command(
    category: &str,
    index: usize,
    value: &serde_json::Value,
) -> Result<(
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    i64,
    String,
    String,
    String,
    String,
)> {
    if let Some(shell) = value.as_str() {
        if shell.trim().is_empty() {
            bail!("verification commands cannot be blank")
        }
        return Ok((
            format!("{category}_{}", index + 1),
            "exact_shell".into(),
            None,
            None,
            Some(shell.into()),
            ".".into(),
            900,
            "[]".into(),
            "[]".into(),
            "{}".into(),
            shell.into(),
        ));
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("verification commands must be strings or structured objects"))?;
    let key = object
        .get("key")
        .and_then(|entry| entry.as_str())
        .ok_or_else(|| anyhow!("structured verification command requires key"))?;
    if !valid_identifier(key) {
        bail!("verification command key must match lowercase [a-z0-9_]+")
    }
    let shell = object
        .get("shell")
        .and_then(|entry| entry.as_str())
        .map(str::to_owned);
    let executable = object
        .get("executable")
        .and_then(|entry| entry.as_str())
        .map(str::to_owned);
    if shell.is_some() == executable.is_some() {
        bail!("structured verification command requires exactly one of shell or executable")
    }
    if shell
        .as_deref()
        .is_some_and(|entry| entry.trim().is_empty())
        || executable
            .as_deref()
            .is_some_and(|entry| entry.trim().is_empty())
    {
        bail!("verification command cannot be blank")
    }
    if executable
        .as_deref()
        .is_some_and(|value| !Path::new(value).is_absolute())
    {
        bail!("structured verification executable must be absolute; use an exact shell command when project-relative resolution is required")
    }
    let arguments = if executable.is_some() {
        Some(serde_json::to_string(
            &object
                .get("arguments")
                .and_then(|entry| entry.as_array())
                .cloned()
                .unwrap_or_default(),
        )?)
    } else {
        None
    };
    let cwd = object
        .get("cwd")
        .and_then(|entry| entry.as_str())
        .unwrap_or(".");
    validate_relative(cwd)?;
    let timeout = object
        .get("timeout_seconds")
        .and_then(|entry| entry.as_i64())
        .unwrap_or(900);
    if !(1..=3600).contains(&timeout) {
        bail!("verification timeout must be between 1 and 3600 seconds")
    }
    let acceptance = object
        .get("acceptance_rows")
        .and_then(|entry| entry.as_array())
        .cloned()
        .unwrap_or_default();
    let inputs = object
        .get("relevant_inputs")
        .and_then(|entry| entry.as_array())
        .cloned()
        .unwrap_or_default();
    for input in &inputs {
        validate_relative(
            input
                .as_str()
                .ok_or_else(|| anyhow!("relevant_inputs entries must be strings"))?,
        )?;
    }
    let invalidation = object
        .get("invalidation")
        .filter(|entry| entry.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let original = object
        .get("original_text")
        .and_then(|entry| entry.as_str())
        .ok_or_else(|| anyhow!("structured verification command requires original_text"))?;
    Ok((
        key.into(),
        if shell.is_some() {
            "exact_shell".into()
        } else {
            "structured_argv".into()
        },
        executable,
        arguments,
        shell,
        cwd.into(),
        timeout,
        serde_json::to_string(&acceptance)?,
        serde_json::to_string(&inputs)?,
        invalidation.to_string(),
        original.into(),
    ))
}
fn json_query(connection: &Connection, sql: &str) -> Result<Vec<serde_json::Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|v| serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v)))
        .collect())
}
fn json_query_params(
    connection: &Connection,
    sql: &str,
    value: &str,
) -> Result<Vec<serde_json::Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map(params![value], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|row| serde_json::from_str(&row).unwrap_or(serde_json::Value::String(row)))
        .collect())
}
fn package_active_hashes() -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for file in PACKAGE_FILES {
        if let Some(rest) = file.relative.strip_prefix("skills/") {
            result.insert(format!(".agents/skills/{rest}"), file.sha256.into());
        }
        if let Some(rest) = file.relative.strip_prefix("bin/") {
            result.insert(
                format!(".agents/trip-explorer/bin/{rest}"),
                file.sha256.into(),
            );
        }
    }
    result
}
fn hash_installation(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for skill in [
        "trip-explorer-init",
        "trip-explorer-upgrade",
        "trip-explorer-workflow",
    ] {
        let directory = root.join(".agents/skills").join(skill);
        collect_tree_hashes(root, &directory, &mut result)?;
    }
    collect_tree_hashes(root, &root.join(".agents/trip-explorer/bin"), &mut result)?;
    Ok(result)
}
fn collect_tree_hashes(
    root: &Path,
    directory: &Path,
    result: &mut BTreeMap<String, String>,
) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("installed package contains a symlink: {}", path.display())
        }
        if metadata.is_dir() {
            collect_tree_hashes(root, &path, result)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .context("installed package path escapes project")?
                .to_string_lossy()
                .to_string();
            result.insert(relative, sha256(&fs::read(path)?));
        }
    }
    Ok(())
}
fn git_exclude_path(root: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()?;
    if !output.status.success() {
        bail!("resolve repository-local Git exclude failed")
    }
    let value = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let path = if value.is_absolute() {
        value
    } else {
        root.join(value)
    };
    let common = PathBuf::from(
        String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["rev-parse", "--git-common-dir"])
                .output()?
                .stdout,
        )?
        .trim(),
    );
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    }
    .canonicalize()?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Git exclude has no parent"))?;
    let resolved_parent = parent.canonicalize()?;
    if !resolved_parent.starts_with(&common) {
        bail!("Git exclude is outside the resolved common directory")
    };
    Ok(path)
}
fn git_exclude_journal(root: &Path) -> Result<JournalFile> {
    let path = git_exclude_path(root)?;
    let existing_bytes = fs::read(&path).ok();
    let existing = String::from_utf8(existing_bytes.clone().unwrap_or_default())
        .context("Git exclude must be UTF-8")?;
    let mut lines = existing.lines().map(str::to_owned).collect::<Vec<_>>();
    if !lines.iter().any(|line| line == "/.local/trip-explorer/") {
        lines.push("/.local/trip-explorer/".into());
    }
    let bytes = format!("{}\n", lines.join("\n")).into_bytes();
    Ok(JournalFile {
        relative: "@git-common/info/exclude".into(),
        source_hash: sha256(&bytes),
        preimage_hash: existing_bytes.as_ref().map(|value| sha256(value)),
        preimage_bytes: existing_bytes,
        bytes,
    })
}
fn git_exclude_journal_if_needed(
    root: &Path,
    proposal: &SetupProposal,
) -> Result<Option<JournalFile>> {
    let ignored = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", ".local/trip-explorer/.ignore-probe"])
        .status()?
        .success();
    if ignored {
        return Ok(None);
    }
    if proposal
        .local_exclude
        .get("approved")
        .and_then(|value| value.as_bool())
        != Some(true)
    {
        bail!("repository-local TRIP ledger is not ignored and the exact local exclude addition is not approved")
    }
    Ok(Some(git_exclude_journal(root)?))
}

#[cfg(test)]
mod read_only_profile_boundary_tests {
    use super::*;

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn role_json(effort: &str) -> String {
        serde_json::to_string(&RoleOverride {
            provider: Provider::Codex,
            model: "gpt-5.6-sol".into(),
            effort: effort.into(),
        })
        .unwrap()
    }

    /// One code-review attempt bound to reviewer settings revision 1, with
    /// revision 2 activated afterwards against the same project configuration.
    fn fixture() -> (Store, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-read-only-profile-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        let connection = store.lock().unwrap();
        let base: serde_json::Value = serde_json::from_str(&role_json("high")).unwrap();
        let settings =
            serde_json::json!({"roles":{"code_reviewer":base},"trip_config_revision_id":"config"});
        connection.execute(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,queue_paused,settings_json,created_at,updated_at)
             VALUES('p','Project','/tmp/project','/tmp/identity','base',0,?1,?2,?2)",
            params![settings.to_string(), NOW],
        ).unwrap();
        connection.execute(
            "INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,detected_json,active_config_revision_id,workflow_id,package_version,upstream_source_hash,overlay_hash,manifest_hash,activated_at,updated_at)
             VALUES('p','ready','fixture','compatible','{}','config',?1,?2,?3,?4,'manifest',?5,?5)",
            params![WORKFLOW_ID, PACKAGE_VERSION, source_hash(), overlay_hash(), NOW],
        ).unwrap();
        let config = serde_json::json!({"roles":{"code_reviewer":{"profile":"review"}},"profiles":{"review":{"adapter":"codex-cli"}}});
        let adapters = serde_json::json!({"adapters":{"codex-cli":{"provider":"codex","kind":"builtin-cli","capabilities":{"read_only":true,"resume":true}}}});
        connection.execute(
            "INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,preflight_json,verification_json,source_hash,overlay_hash,configuration_hash,created_at,activated_at)
             VALUES('config','p',1,'activated',?1,?2,'[]','{}',?3,?4,'configuration',?5,?5)",
            params![config.to_string(), adapters.to_string(), source_hash(), overlay_hash(), NOW],
        ).unwrap();
        connection.execute(
            r#"INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,created_at,updated_at)
               VALUES('t','p','Task','Description','["criterion"]','in_progress',?1,?1)"#,
            params![NOW],
        ).unwrap();
        connection.execute(
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,scope_hash,configuration_hash,workflow_version,workflow_hash,upstream_source_hash,overlay_hash,legacy_migration_required,created_at,updated_at,candidate_hash)
             VALUES('a','t','context','code_review','base',1,'running','scope','configuration','v','h','u','o',0,?1,?1,'candidate')",
            params![NOW],
        ).unwrap();
        connection.execute(
            "INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance) VALUES('budget','a','code',3)",
            [],
        ).unwrap();
        for (revision, effort) in [(1, "high"), (2, "medium")] {
            connection.execute(
                "INSERT INTO role_settings(id,task_id,role,revision,config_json,created_at) VALUES(?1,'t','code_reviewer',?2,?3,?4)",
                params![format!("setting-{revision}"), revision, role_json(effort), NOW],
            ).unwrap();
        }
        connection.execute(
            "INSERT INTO capabilities(id,provider,executable_version,role,mode,config_hash,status,checked_at,proof_json)
             VALUES('capability','codex','1.0','code_reviewer','read_only','key','supported',?1,'{\"proof\":1}')",
            params![NOW],
        ).unwrap();
        let bound = task_profile_descriptor(&connection, "t", "code_reviewer", 1)
            .unwrap()
            .0;
        connection.execute("INSERT INTO trip_attempt_profiles(attempt_id,role,settings_revision,activation_id,source,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_id,capability_key,capability_proof_hash,bound_at) VALUES('a','code_reviewer',1,NULL,?1,?2,?3,'config','configuration',?4,?5,'capability','key','proof',?6)",params![bound.source,bound.profile_json.to_string(),bound.profile_hash,bound.adapter_name,bound.adapter_hash,NOW]).unwrap();
        let next = task_profile_descriptor(&connection, "t", "code_reviewer", 2)
            .unwrap()
            .0;
        connection.execute("INSERT INTO trip_task_profile_activations(id,task_id,role,settings_id,settings_revision,profile_json,profile_hash,project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_id,capability_key,capability_proof_hash,activated_at) VALUES('activation-2','t','code_reviewer','setting-2',2,?1,?2,'config','configuration',?3,?4,'capability','key','proof-2',?5)",params![next.profile_json.to_string(),next.profile_hash,next.adapter_name,next.adapter_hash,NOW]).unwrap();
        drop(connection);
        (store, root)
    }

    fn bound_revision(store: &Store) -> (i64, Option<String>) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT settings_revision,activation_id FROM trip_attempt_profiles WHERE attempt_id='a' AND role='code_reviewer'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    /// A previous reviewer generation and its session. An `exited` reviewer
    /// has an exited session with a verified quiet process group unless
    /// `quiescent` is false.
    fn seed_reviewer_with(store: &Store, status: &str, quiescent: bool) {
        let connection = store.lock().unwrap();
        connection.execute(
            "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
             VALUES('reviewer','a','code_reviewer','codex',1,1,?1,'f',?2,?2)",
            params![status, NOW],
        ).unwrap();
        let exit = (status == "exited").then(|| {
            if quiescent {
                r#"{"process_group_quiescent":true}"#
            } else {
                r#"{"process_group_quiescent":false}"#
            }
        });
        connection.execute(
            "INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,exit_json,created_at,updated_at)
             VALUES('reviewer-session','reviewer','codex',?1,'{}','fixture','e',?2,?3,?3)",
            params![status, exit, NOW],
        ).unwrap();
    }

    fn seed_reviewer(store: &Store, status: &str) {
        seed_reviewer_with(store, status, true)
    }

    fn pending_reason(store: &Store) -> Option<&'static str> {
        match read_only_profile_boundary(&store.lock().unwrap(), "a", RoleKind::CodeReviewer)
            .unwrap()
        {
            ReadOnlyProfileBoundary::Pending { reason, .. } => Some(reason),
            _ => None,
        }
    }

    #[test]
    fn every_unfinished_fact_keeps_the_old_reviewer_settings() {
        let (store, _root) = fixture();
        seed_reviewer_with(&store, "exited", false);
        assert_eq!(
            pending_reason(&store),
            Some("role_process_not_proven_quiescent"),
            "an exit without a verified quiet process group is not a boundary"
        );
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE sessions SET exit_json='{\"process_group_quiescent\":true}'",
                [],
            )
            .unwrap();
        assert_eq!(pending_reason(&store), None);
        let fences: &[(&str, &str, &str)] = &[
            ("review_in_flight",
             "INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,prompt_hash,handoff_hash,delivery_state,created_at,updated_at) VALUES('fence','a','code','candidate','p','h','ambiguous','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM review_requests WHERE id='fence'"),
            ("switch_pending",
             "INSERT INTO switch_intents(id,attempt_id,role,old_generation_id,requested_settings_revision,handoff_json,state,created_at,updated_at) VALUES('fence','a','code_reviewer','reviewer',2,'{}','stopping_old','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM switch_intents WHERE id='fence'"),
            ("permission_pending",
             "INSERT INTO permission_requests(id,hook_invocation_nonce,connection_nonce,provider,project_id,task_id,attempt_id,session_id,role_generation_id,role,service_boot_id,native_session_id,cwd,policy_fingerprint,tool_name,input_digest,input_json,created_at,deadline_at,state,updated_at) VALUES('fence','h','c','codex','p','t','a','reviewer-session','reviewer','code_reviewer','boot','native','.','policy','shell','digest','{}','2026-01-01T00:00:00Z','9999-01-01T00:00:00Z','pending','2026-01-01T00:00:00Z')",
             "DELETE FROM permission_requests WHERE id='fence'"),
            ("input_control",
             "INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,process_identity_json,expires_at,created_at,updated_at) VALUES('reviewer-session','fence','human','viewer','reviewer','{}','9999-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM input_leases WHERE lease_id_hash='fence'"),
            ("control_pending",
             "INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at) VALUES('fence','a','pause_now','requested',1,'{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM controls WHERE id='fence'"),
            ("restart_hold",
             "INSERT INTO restart_candidates(session_id,attempt_id,task_id,source,state,reason,result_json,created_at,updated_at) VALUES('reviewer-session','a','t','planned_shutdown','queued_capacity','fixture','{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM restart_candidates WHERE session_id='reviewer-session'"),
            ("recovery_open",
             "INSERT INTO recovery_records(id,attempt_id,state,detail_json,created_at,updated_at) VALUES('fence','a','attention_required','{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM recovery_records WHERE id='fence'"),
            ("conflicting_role_live",
             "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at) VALUES('writer','a','implementer','claude',1,1,'running','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM role_generations WHERE id='writer'"),
            // Missing or malformed exit proof is never read as quiet.
            ("role_process_not_proven_quiescent",
             "UPDATE sessions SET exit_json=NULL WHERE id='reviewer-session'",
             "UPDATE sessions SET exit_json='{\"process_group_quiescent\":true}' WHERE id='reviewer-session'"),
            ("role_process_not_proven_quiescent",
             "UPDATE sessions SET exit_json='{}' WHERE id='reviewer-session'",
             "UPDATE sessions SET exit_json='{\"process_group_quiescent\":true}' WHERE id='reviewer-session'"),
            ("role_process_not_proven_quiescent",
             "UPDATE sessions SET exit_json='not json' WHERE id='reviewer-session'",
             "UPDATE sessions SET exit_json='{\"process_group_quiescent\":true}' WHERE id='reviewer-session'"),
            // Uncertain guidance stays a fence after its generation exited.
            ("guidance_uncertain",
             "INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,created_at) VALUES('fence','a','reviewer','body','delivery_unknown','2026-01-01T00:00:00Z')",
             "DELETE FROM guidance_messages WHERE id='fence'"),
            ("freeze_in_progress",
             "INSERT INTO freeze_intents(id,attempt_id,kind,state,created_at,updated_at) VALUES('fence','a','candidate','capturing','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
             "DELETE FROM freeze_intents WHERE id='fence'"),
            ("check_running",
             "INSERT INTO check_runs(id,attempt_id,candidate_hash,executable,arguments_json,cwd,status,evidence_json,created_at) VALUES('fence','a','candidate','/usr/bin/true','[]','/tmp','running','{}','2026-01-01T00:00:00Z')",
             "DELETE FROM check_runs WHERE id='fence'"),
            ("check_running",
             "INSERT INTO check_runs(id,attempt_id,candidate_hash,executable,arguments_json,cwd,status,evidence_json,created_at) VALUES('fence','a','candidate','/usr/bin/true','[]','/tmp','launch_ambiguous','{}','2026-01-01T00:00:00Z')",
             "DELETE FROM check_runs WHERE id='fence'"),
            ("attempt_not_current",
             "UPDATE attempts SET status='needs_input' WHERE id='a'",
             "UPDATE attempts SET status='running' WHERE id='a'"),
            ("attempt_not_current",
             "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,scope_hash,configuration_hash,workflow_version,workflow_hash,upstream_source_hash,overlay_hash,legacy_migration_required,created_at,updated_at) VALUES('newer','t','context-newer','planning','base',1,'running','scope','configuration','v','h','u','o',0,'2026-02-01T00:00:00Z','2026-02-01T00:00:00Z')",
             "DELETE FROM attempts WHERE id='newer'"),
            ("attempt_not_current",
             "UPDATE tasks SET lifecycle='done' WHERE id='t'",
             "UPDATE tasks SET lifecycle='in_progress' WHERE id='t'"),
        ];
        for (reason, apply, revert) in fences {
            store.lock().unwrap().execute_batch(apply).unwrap();
            assert_eq!(pending_reason(&store), Some(*reason));
            store.lock().unwrap().execute_batch(revert).unwrap();
            assert_eq!(pending_reason(&store), None, "{reason} was not reverted");
        }
        // Only a review step moves the code reviewer.
        store
            .lock()
            .unwrap()
            .execute("UPDATE attempts SET phase='implementation'", [])
            .unwrap();
        assert_eq!(pending_reason(&store), Some("phase_mismatch"));
    }

    #[test]
    fn a_running_reviewer_keeps_its_settings_and_no_request_is_created() {
        let (store, root) = fixture();
        seed_reviewer(&store, "running");
        let boundary =
            read_only_profile_boundary(&store.lock().unwrap(), "a", RoleKind::CodeReviewer)
                .unwrap();
        assert!(matches!(
            boundary,
            ReadOnlyProfileBoundary::Pending {
                needs_person: false,
                ..
            }
        ));
        let waiting = crate::coordinator::review_decision_for_tests(&store.lock().unwrap(), "a");
        assert_eq!(
            waiting.reason_code,
            "workflow.reviewer_profile_change_pending"
        );
        assert!(waiting
            .primary_blocker
            .as_ref()
            .and_then(|blocker| blocker.message.as_deref())
            .is_some_and(|message| message.contains("Code reviewer settings changed")));
        let reviews = crate::review::ReviewService::new(store.clone(), root.join("artifacts"));
        let error = reviews
            .reserve_request(
                "a",
                "code",
                "Review it",
                serde_json::json!({"attempt_id":"a"}),
            )
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("reviewer profile change pending"),
            "{error:#}"
        );
        let requests: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM review_requests", [], |row| row.get(0))
            .unwrap();
        assert_eq!(requests, 0);
        assert_eq!(bound_revision(&store), (1, None));
    }

    #[test]
    fn the_next_review_moves_to_the_newest_activation_exactly_once() {
        let (store, root) = fixture();
        seed_reviewer(&store, "exited");
        let reviews = crate::review::ReviewService::new(store.clone(), root.join("artifacts"));
        let request = reviews
            .reserve_request(
                "a",
                "code",
                "Review it",
                serde_json::json!({"attempt_id":"a"}),
            )
            .unwrap();
        assert_eq!(request.state, "reserved");
        assert_eq!(bound_revision(&store), (2, Some("activation-2".into())));
        let connection = store.lock().unwrap();
        let recorded: i64 = connection.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE event_code='task.profile.materialized_at_safe_boundary'
               AND json_extract(detail_json,'$.from_settings_revision')=1 AND json_extract(detail_json,'$.to_settings_revision')=2",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(recorded, 1);
        assert!(matches!(
            read_only_profile_boundary(&connection, "a", RoleKind::CodeReviewer).unwrap(),
            ReadOnlyProfileBoundary::Current
        ));
        drop(connection);
        // Reserving the same request again reuses it without another change.
        let again = reviews
            .reserve_request(
                "a",
                "code",
                "Review it",
                serde_json::json!({"attempt_id":"a"}),
            )
            .unwrap();
        assert_eq!(again.request_id, request.request_id);
        let connection = store.lock().unwrap();
        let recorded: i64 = connection.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE event_code='task.profile.materialized_at_safe_boundary'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(recorded, 1);
        connection.execute(
            "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
             VALUES('next','a','code_reviewer','codex',2,2,'launch_reserved','f',?1,?1)",
            params![NOW],
        ).unwrap();
        connection.execute(
            "INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
             VALUES('next-session','next','codex','launch_reserved','{}','fixture','e',?1,?1)",
            params![NOW],
        ).unwrap();
        drop(connection);
        // The lineage guard refuses a launch bound to the replaced revision.
        assert!(reviews
            .bind_launch_intent(&request.request_id, "next-session", "next", 1)
            .is_err());
        reviews
            .bind_launch_intent(&request.request_id, "next-session", "next", 2)
            .unwrap();
    }

    #[test]
    fn stale_evidence_for_the_new_settings_needs_a_person() {
        let (store, _root) = fixture();
        let connection = store.lock().unwrap();
        connection.execute(
            "INSERT INTO capabilities(id,provider,executable_version,role,mode,config_hash,status,checked_at,proof_json)
             VALUES('newer','codex','1.0','code_reviewer','read_only','other-key','supported','2026-01-02T00:00:00Z','{\"proof\":2}')",
            [],
        ).unwrap();
        assert!(matches!(
            read_only_profile_boundary(&connection, "a", RoleKind::CodeReviewer).unwrap(),
            ReadOnlyProfileBoundary::Pending {
                needs_person: true,
                ..
            }
        ));
        let decision = crate::coordinator::review_decision_for_tests(&connection, "a");
        assert_eq!(
            decision.reason_code,
            "workflow.reviewer_profile_change_pending"
        );
        let next = decision
            .next_action
            .expect("a person gets an exact destination");
        assert_eq!(next.operation, "verify_task_profile");
        assert_eq!(next.binding.role, Some(RoleKind::CodeReviewer));
        assert_eq!(next.binding.settings_revision, Some(2));
        assert_eq!(next.binding.task_id.as_deref(), Some("t"));
        // Writers keep the explicit switch flow.
        assert!(matches!(
            read_only_profile_boundary(&connection, "a", RoleKind::Implementer).unwrap(),
            ReadOnlyProfileBoundary::Current
        ));
        // An activation made for another project configuration does not apply;
        // the attempt keeps the configuration it was reviewed with.
        connection.execute(
            "INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,preflight_json,verification_json,source_hash,overlay_hash,configuration_hash,created_at)
             SELECT 'other',project_id,2,'proposed',config_json,adapters_json,preflight_json,verification_json,source_hash,overlay_hash,'other-configuration',created_at
             FROM trip_config_revisions WHERE id='config'",
            [],
        ).unwrap();
        connection
            .execute(
                "UPDATE trip_task_profile_activations SET project_config_revision_id='other'",
                [],
            )
            .unwrap();
        assert!(matches!(
            read_only_profile_boundary(&connection, "a", RoleKind::CodeReviewer).unwrap(),
            ReadOnlyProfileBoundary::Current
        ));
    }
}
