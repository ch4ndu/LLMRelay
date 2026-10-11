pub mod claude;
pub mod codex;

use crate::config::{atomic_write, InstancePaths};
use crate::domain::{
    CapabilityIdentity, ExecutableFingerprint, LaunchConfig, Provider, RoleKind,
    ROLE_RESULT_REPORT_CONTRACT,
};
use anyhow::{bail, Context, Result};
use portable_pty::CommandBuilder;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CODEX_HOOK_REVISION: &str = "agenticjira-hook-v3-permission-events";
/// Claude also registers Notification, so every Claude capability proof bound
/// to the earlier generated hook settings is superseded rather than reused.
pub const CLAUDE_HOOK_REVISION: &str = "agenticjira-hook-v4-claude-notification";

/// The narrow provider-side permissions needed for one service-owned runtime
/// probe. These values are generated from the persisted probe row, never from
/// its natural-language prompt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeProbeCommandPolicy {
    pub commands: Vec<RuntimeProbeCommand>,
    pub read_denials: Vec<PathBuf>,
    pub write_denials: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeProbeCommand {
    pub operation: String,
    pub command: String,
}

#[derive(Clone, Debug)]
pub struct PreparedLaunch {
    pub config: LaunchConfig,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub supervision_executable: PathBuf,
}

impl PreparedLaunch {
    pub fn process(&self) -> ProviderProcess<'_> {
        ProviderProcess {
            provider: self.config.provider,
            executable: &self.executable,
            arguments: &self.arguments,
            environment: &self.environment,
            cwd: &self.config.cwd,
            supervision_executable: &self.supervision_executable,
        }
    }

    pub fn command(&self, anchor_path: &Path) -> CommandBuilder {
        self.process().command(anchor_path)
    }
}

/// Everything the supervisor needs to start one provider process, shared by
/// role launches and app-owned provider checks.
#[derive(Clone, Copy, Debug)]
pub struct ProviderProcess<'a> {
    pub provider: Provider,
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub environment: &'a [(String, String)],
    pub cwd: &'a Path,
    pub supervision_executable: &'a Path,
}

impl ProviderProcess<'_> {
    pub fn command(&self, anchor_path: &Path) -> CommandBuilder {
        let mut command = CommandBuilder::new(self.supervision_executable);
        command.env_clear();
        for (key, value) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            let portable = matches!(
                key_text.as_ref(),
                "HOME"
                    | "PATH"
                    | "SHELL"
                    | "TERM"
                    | "TMPDIR"
                    | "USER"
                    | "LOGNAME"
                    | "LANG"
                    | "COLORTERM"
                    | "SSL_CERT_FILE"
                    | "SSL_CERT_DIR"
            ) || key_text.starts_with("LC_");
            if portable {
                command.env(&key, &value);
            }
        }
        let wrapper_arguments = vec![
            "internal-launch".to_owned(),
            "--anchor".to_owned(),
            anchor_path.to_string_lossy().into_owned(),
            "--executable".to_owned(),
            self.executable.to_string_lossy().into_owned(),
            "--".to_owned(),
        ];
        command.args(&wrapper_arguments);
        command.args(self.arguments);
        command.cwd(self.cwd);
        for (key, value) in self.environment {
            command.env(key, value);
        }
        command
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProviderInstallation {
    pub provider: Provider,
    pub executable: ExecutableFingerprint,
    pub executable_version: String,
}

/// Locates the provider on PATH and binds its canonical file identity and
/// self-reported version; no model is contacted.
pub fn inspect_installation(provider: Provider) -> Result<ProviderInstallation> {
    let (path, executable_version) = executable_and_version(provider)?;
    Ok(ProviderInstallation {
        provider,
        executable: executable_fingerprint(&path)?,
        executable_version,
    })
}

fn executable_fingerprint(path: &Path) -> Result<ExecutableFingerprint> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve provider executable {}", path.display()))?;
    let metadata = canonical.metadata()?;
    Ok(ExecutableFingerprint {
        canonical_path: canonical,
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
    })
}

/// Non-secret local sign-in facts for an app-wide check. `account_identity`
/// is set only from a provider-specific allowlisted field.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderAccountObservation {
    pub account_identity: Option<String>,
    pub authentication: String,
    pub config_identity: serde_json::Value,
}

/// Reported when no reviewed local sign-in inspection exists for a provider.
pub const AUTHENTICATION_NOT_INSPECTED: &str = "not_inspected";

impl ProviderAccountObservation {
    pub fn authentication_inspected(&self) -> bool {
        self.authentication != AUTHENTICATION_NOT_INSPECTED
    }
}

/// One app-owned provider check. It carries no role socket, token,
/// generation, hook or project path; its scope is an empty app directory.
#[derive(Clone, Debug)]
pub struct ObservationLaunch {
    pub provider: Provider,
    pub executable: PathBuf,
    pub executable_version: String,
    pub model: String,
    pub effort: String,
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub scope: PathBuf,
    pub supervision_executable: PathBuf,
    pub policy: crate::provider_compatibility::PolicyBinding,
}

impl ObservationLaunch {
    pub fn process(&self) -> ProviderProcess<'_> {
        ProviderProcess {
            provider: self.provider,
            executable: &self.executable,
            arguments: &self.arguments,
            environment: &self.environment,
            cwd: &self.scope,
            supervision_executable: &self.supervision_executable,
        }
    }

    /// The session record omits the prompt, which carries the check value.
    pub fn record(&self) -> serde_json::Value {
        let mut arguments = self.arguments.clone();
        if let Some(prompt) = arguments.last_mut() {
            *prompt = "<provider-check-prompt>".to_owned();
        }
        json!({
            "kind": "provider_observation",
            "provider": self.provider,
            "executable": self.executable,
            "executable_version": self.executable_version,
            "model": self.model,
            "effort": self.effort,
            "argv": arguments,
            "environment_keys": self.environment.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            "scope": self.scope,
            "policy": self.policy,
        })
    }
}

/// Model and effort become separate argv elements, never shell text; a
/// leading dash or whitespace is still refused so neither can become a flag.
fn require_provider_word(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('-')
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/' | b'[' | b']')
        })
    {
        bail!("{label} must be a plain provider identifier")
    }
    Ok(())
}

pub(crate) fn require_observation_selection(model: &str, effort: &str) -> Result<()> {
    require_provider_word("model", model)?;
    require_provider_word("effort", effort)
}

pub fn prepare_observation_launch(
    installation: &ProviderInstallation,
    policy: crate::provider_compatibility::PolicyBinding,
    model: &str,
    effort: &str,
    scope: &Path,
    prompt: &str,
    supervision_executable: &Path,
) -> Result<ObservationLaunch> {
    require_observation_selection(model, effort)?;
    if policy.mode != crate::provider_compatibility::PolicyMode::ProviderObservation
        || policy.provider != installation.provider
        || policy.exact_version != installation.executable_version
    {
        bail!("provider check launch requires the observation contract for this exact installation")
    }
    let (arguments, environment) = match installation.provider {
        Provider::Codex => (
            codex::observation_arguments(model, effort, scope, prompt)?,
            Vec::new(),
        ),
        Provider::Claude => (
            claude::observation_arguments(model, effort, prompt),
            vec![
                ("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned()),
                ("DISABLE_UPDATES".to_owned(), "1".to_owned()),
            ],
        ),
    };
    Ok(ObservationLaunch {
        provider: installation.provider,
        executable: installation.executable.canonical_path.clone(),
        executable_version: installation.executable_version.clone(),
        model: model.to_owned(),
        effort: effort.to_owned(),
        arguments,
        environment,
        scope: scope.to_path_buf(),
        supervision_executable: supervision_executable.to_path_buf(),
        policy,
    })
}

#[derive(Clone, Debug)]
pub struct HookAssets {
    pub runner: PathBuf,
    pub claude_settings: PathBuf,
    pub claude_implementer_settings: PathBuf,
    pub codex_revision_hash: String,
    pub claude_revision_hash: String,
    pub claude_implementer_revision_hash: String,
}

pub fn install_hook_assets(paths: &InstancePaths, executable: &Path) -> Result<HookAssets> {
    let runner = paths.hooks.join("agenticjira-hook");
    let runner_body = format!(
        "#!/bin/sh\nexec {} hook --provider \"${{AGENTICJIRA_PROVIDER:?missing AGENTICJIRA_PROVIDER}}\" \"$@\"\n",
        shell_quote(&executable.to_string_lossy())
    );
    atomic_write(&runner, runner_body.as_bytes())?;
    fs::set_permissions(&runner, fs::Permissions::from_mode(0o555))?;

    let hook_command = shell_quote(&runner.to_string_lossy());
    let mut reviewer_hooks = serde_json::Map::new();
    let mut implementer_hooks = serde_json::Map::new();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PermissionDenied",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
        "Notification",
        "SubagentStart",
        "SubagentStop",
        "SessionEnd",
    ] {
        let event_hook_command = format!("{hook_command} --event {}", shell_quote(event));
        reviewer_hooks.insert(
            event.to_owned(),
            json!([{
                "hooks": [{"type": "command", "command": event_hook_command.clone(), "timeout": 2}]
            }]),
        );
        implementer_hooks.insert(
            event.to_owned(),
            json!([{
                "hooks": [{
                    "type": "command",
                    "command": event_hook_command,
                    "timeout": if event == "PermissionRequest" {
                        crate::permissions::PROVIDER_PERMISSION_TIMEOUT_SECONDS
                    } else {
                        2
                    }
                }]
            }]),
        );
    }
    let settings = json!({
        "env": {"DISABLE_AUTOUPDATER":"1","DISABLE_UPDATES":"1"},
        "permissions": {"defaultMode":"dontAsk","allow":[],"deny":["Edit","Write","NotebookEdit","WebFetch","WebSearch","Task","Agent"]},
        "hooks": reviewer_hooks
    });
    let settings_bytes = serde_json::to_vec_pretty(&settings)?;
    let claude_settings = paths.hooks.join("claude-reviewer-settings.json");
    atomic_write(&claude_settings, &settings_bytes)?;
    fs::set_permissions(&claude_settings, fs::Permissions::from_mode(0o444))?;
    let implementer_settings_bytes = serde_json::to_vec_pretty(&json!({
        "env": {"DISABLE_AUTOUPDATER":"1","DISABLE_UPDATES":"1"},
        "permissions": {
            "defaultMode":"default",
            "allow":[],
            "deny":["NotebookEdit","WebFetch","WebSearch","Task","Agent"]
        },
        "hooks":implementer_hooks
    }))?;
    let claude_implementer_settings = paths.hooks.join("claude-implementer-settings.json");
    atomic_write(&claude_implementer_settings, &implementer_settings_bytes)?;
    fs::set_permissions(
        &claude_implementer_settings,
        fs::Permissions::from_mode(0o444),
    )?;

    let codex_definition = json!({
        "revision": CODEX_HOOK_REVISION,
        "source": "per-run command-line config",
        "command": format!("{hook_command} --event <trusted-configured-event>"),
        "events": ["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse", "Stop", "Interrupt", "SubagentStart", "SubagentStop", "SessionEnd"],
        "timeouts": {"lifecycle_seconds":2,"implementer_permission_request_seconds":crate::permissions::PROVIDER_PERMISSION_TIMEOUT_SECONDS},
        "trust": "native Codex hook trust; no bypass"
    });
    let codex_definition_path = paths.hooks.join("codex-reviewer-hooks.json");
    atomic_write(
        &codex_definition_path,
        &serde_json::to_vec_pretty(&codex_definition)?,
    )?;
    fs::set_permissions(&codex_definition_path, fs::Permissions::from_mode(0o444))?;

    let mut codex_digest = Sha256::new();
    codex_digest.update(runner_body.as_bytes());
    codex_digest.update(serde_json::to_vec(&codex_definition)?);
    let mut claude_digest = Sha256::new();
    claude_digest.update(runner_body.as_bytes());
    claude_digest.update(&settings_bytes);
    let mut claude_implementer_digest = Sha256::new();
    claude_implementer_digest.update(runner_body.as_bytes());
    claude_implementer_digest.update(&implementer_settings_bytes);
    Ok(HookAssets {
        runner,
        claude_settings,
        claude_implementer_settings,
        codex_revision_hash: hex::encode(codex_digest.finalize()),
        claude_revision_hash: hex::encode(claude_digest.finalize()),
        claude_implementer_revision_hash: hex::encode(claude_implementer_digest.finalize()),
    })
}

pub fn prepare_role_launch(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
) -> Result<PreparedLaunch> {
    prepare_role_launch_with_read_denials(
        provider,
        role,
        model,
        effort,
        cwd,
        prompt,
        role_socket,
        role_token,
        role_generation_id,
        session_id,
        native_session_id,
        assets,
        executable_path,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn prepare_role_launch_with_bundles(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<PreparedLaunch> {
    prepare_role_launch_with_policies(
        provider,
        role,
        model,
        effort,
        cwd,
        prompt,
        role_socket,
        role_token,
        role_generation_id,
        session_id,
        native_session_id,
        assets,
        executable_path,
        &[],
        None,
        bundles,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_role_launch_with_read_denials(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
    read_denials: &[PathBuf],
) -> Result<PreparedLaunch> {
    prepare_role_launch_with_policies(
        provider,
        role,
        model,
        effort,
        cwd,
        prompt,
        role_socket,
        role_token,
        role_generation_id,
        session_id,
        native_session_id,
        assets,
        executable_path,
        read_denials,
        None,
        &crate::provider_compatibility::BundleSet::embedded(),
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn prepare_role_launch_with_read_denials_and_bundles(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
    read_denials: &[PathBuf],
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<PreparedLaunch> {
    prepare_role_launch_with_policies(
        provider,
        role,
        model,
        effort,
        cwd,
        prompt,
        role_socket,
        role_token,
        role_generation_id,
        session_id,
        native_session_id,
        assets,
        executable_path,
        read_denials,
        None,
        bundles,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_runtime_probe_role_launch(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
    policy: &RuntimeProbeCommandPolicy,
) -> Result<PreparedLaunch> {
    prepare_role_launch_with_policies(
        provider,
        role,
        model,
        effort,
        cwd,
        prompt,
        role_socket,
        role_token,
        role_generation_id,
        session_id,
        native_session_id,
        assets,
        executable_path,
        &[],
        Some(policy),
        &crate::provider_compatibility::BundleSet::embedded(),
    )
}

#[allow(clippy::too_many_arguments)]
#[doc(hidden)]
pub fn prepare_runtime_probe_role_launch_with_bundles(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
    policy: &RuntimeProbeCommandPolicy,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<PreparedLaunch> {
    prepare_role_launch_with_policies(
        provider,
        role,
        model,
        effort,
        cwd,
        prompt,
        role_socket,
        role_token,
        role_generation_id,
        session_id,
        native_session_id,
        assets,
        executable_path,
        &[],
        Some(policy),
        bundles,
    )
}

/// Everything one Single agent writer turn is prepared from. A retained turn
/// names the native conversation it continues; a first turn names none.
pub struct SingleAgentLaunchRequest<'a> {
    pub provider: Provider,
    pub model: &'a str,
    pub effort: &'a str,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub role_socket: &'a Path,
    pub token: &'a str,
    pub role_generation_id: &'a str,
    pub session_id: &'a str,
    pub native_session_id: Option<&'a str>,
    pub assets: &'a HookAssets,
    pub executable_path: &'a Path,
    pub bundles: &'a crate::provider_compatibility::BundleSet,
}

/// A provider's part of a Single agent launch, before the policy binding.
pub(crate) struct SingleAgentNative {
    executable: PathBuf,
    executable_version: String,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    role_socket: PathBuf,
    account_identity: Option<String>,
    config_identity: serde_json::Value,
    security_policy: serde_json::Value,
    hook_revision: String,
}

/// The native facts a Single agent capability proof binds besides the
/// profile and its prepared scope. Any change makes earlier proof stale.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SingleAgentNativeIdentity {
    pub provider: Provider,
    pub executable: ExecutableFingerprint,
    pub executable_version: String,
    pub account_identity: Option<String>,
    pub config_identity: serde_json::Value,
    pub security_policy: serde_json::Value,
    pub hook_revision: String,
    pub policy_binding_hash: String,
}

/// One prepared Single agent writer turn and the record its session stores.
/// It binds the separately compiled Single agent policy; no TRIP role
/// contract, role prompt or role capability takes part.
pub struct SingleAgentLaunch {
    pub provider: Provider,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub supervision_executable: PathBuf,
    pub identity: SingleAgentNativeIdentity,
    pub record: serde_json::Value,
}

impl SingleAgentLaunch {
    pub fn process(&self) -> ProviderProcess<'_> {
        ProviderProcess {
            provider: self.provider,
            executable: &self.executable,
            arguments: &self.arguments,
            environment: &self.environment,
            cwd: &self.cwd,
            supervision_executable: &self.supervision_executable,
        }
    }
}

/// Prepares one Single agent writer turn, refused while the installed
/// version's Single agent writer contract is unqualified.
pub fn prepare_single_agent_launch(
    request: &SingleAgentLaunchRequest<'_>,
) -> Result<SingleAgentLaunch> {
    require_provider_word("model", request.model)?;
    require_provider_word("effort", request.effort)?;
    let native = match request.provider {
        Provider::Codex => codex::single_agent_native(request)?,
        Provider::Claude => claude::single_agent_native(request)?,
    };
    let binding = request.bundles.resolve_policy(
        request.provider,
        &native.executable_version,
        crate::provider_compatibility::PolicyMode::SingleAgentWriter,
    )?;
    if let Some(reason) = binding.native_calls_disabled_reason() {
        bail!("{reason}")
    }
    let identity = SingleAgentNativeIdentity {
        provider: request.provider,
        executable: executable_fingerprint(&native.executable)?,
        executable_version: native.executable_version,
        account_identity: native.account_identity,
        config_identity: native.config_identity,
        security_policy: native.security_policy,
        hook_revision: native.hook_revision,
        policy_binding_hash: binding.effective_hash.clone(),
    };
    let policy =
        crate::provider_compatibility::ExecutionPolicy::single_agent_writer(request.provider);
    let mut arguments = native.arguments;
    arguments.push(format!(
        "{}\n\n{}",
        request.prompt,
        single_agent_channel_instructions(request.executable_path, &native.role_socket)
    ));
    let mut recorded_arguments = arguments.clone();
    if let Some(prompt) = recorded_arguments.last_mut() {
        *prompt = "<single-agent-prompt>".to_owned();
    }
    let record = json!({
        "schema": 1,
        "actor": crate::domain::ExecutionActor::SINGLE_AGENT_KEY,
        "provider": request.provider,
        "executable": native.executable,
        "executable_version": identity.executable_version,
        "model": request.model,
        "effort": request.effort,
        "cwd": request.cwd,
        "argv": recorded_arguments,
        "environment_keys": native.environment.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        "native_session_id": request.native_session_id,
        "execution_policy": policy,
        "execution_policy_hash": policy.content_hash(),
        "policy_binding": binding,
    });
    Ok(SingleAgentLaunch {
        provider: request.provider,
        executable: native.executable,
        arguments,
        environment: native.environment,
        cwd: request.cwd.to_path_buf(),
        supervision_executable: request.executable_path.to_path_buf(),
        identity,
        record,
    })
}

fn single_agent_channel_instructions(executable: &Path, socket: &Path) -> String {
    let executable = shell_quote(executable.to_string_lossy().as_ref());
    format!(
        "Use `{executable} role context` to read your task, guidance and result contract, and `{executable} role report` to report as that context describes. Terminal prose is not a result. The channel is scoped at `{}`.",
        socket.display()
    )
}

fn prepare_role_launch_with_policies(
    provider: Provider,
    role: RoleKind,
    model: &str,
    effort: &str,
    cwd: &Path,
    prompt: &str,
    role_socket: &Path,
    role_token: &str,
    role_generation_id: &str,
    session_id: &str,
    native_session_id: Option<&str>,
    assets: &HookAssets,
    executable_path: &Path,
    read_denials: &[PathBuf],
    runtime_probe_policy: Option<&RuntimeProbeCommandPolicy>,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<PreparedLaunch> {
    if !read_denials.is_empty() {
        let role_executable = executable_path
            .canonicalize()
            .context("resolve role-channel executable for setup confinement")?;
        if let Some(denied) = read_denials.iter().find(|denied| {
            role_executable.as_path() == denied.as_path() || role_executable.starts_with(denied)
        }) {
            bail!(
                "launch preflight refused this setup launch before contacting the provider: the LLMRelay executable {} is inside the denied target path {}. Setup read isolation requires the executable to be installed outside the target repository and Git common directory",
                role_executable.display(),
                denied.display()
            )
        }
    }
    match provider {
        Provider::Codex => codex::prepare_with_bundles(
            role,
            model,
            effort,
            cwd,
            prompt,
            role_socket,
            role_token,
            role_generation_id,
            session_id,
            native_session_id,
            assets,
            executable_path,
            read_denials,
            bundles,
        ),
        Provider::Claude => claude::prepare_with_bundles(
            role,
            model,
            effort,
            cwd,
            prompt,
            role_socket,
            role_token,
            role_generation_id,
            session_id,
            native_session_id,
            assets,
            executable_path,
            read_denials,
            runtime_probe_policy,
            bundles,
        ),
    }
}

pub fn capability_key(config: &LaunchConfig) -> Result<String> {
    if config.compatibility.is_none() {
        bail!("current capability key requires a resolved provider compatibility binding")
    }
    capability_identity_key(&capability_identity(config)?)
}

pub fn capability_identity(config: &LaunchConfig) -> Result<CapabilityIdentity> {
    if config.security_policy.is_null() {
        bail!("capability identity requires an explicit effective security policy")
    }
    Ok(CapabilityIdentity {
        provider: config.provider,
        executable: executable_fingerprint(&config.executable)?,
        executable_version: config.executable_version.clone(),
        role: config.role,
        model: config.model.clone(),
        effort: config.effort.clone(),
        permission_policy: normalize_text(&config.permission_policy, config),
        security_policy: normalize_value(config.security_policy.clone(), config),
        effective_argv: normalized_provider_argv(config),
        environment_contract: config.environment_keys.clone(),
        hook_revision: config.hook_revision.clone(),
        capability_status: (config.provider == Provider::Codex
            && config.role == RoleKind::Implementer)
            .then_some(config.capability_status),
        compatibility: config.compatibility.as_ref().map(Into::into),
    })
}

pub fn require_production_capability(config: &LaunchConfig) -> Result<()> {
    require_production_role(config.provider, config.role)
}

pub fn require_production_role(provider: Provider, role: RoleKind) -> Result<()> {
    let _ = (provider, role);
    Ok(())
}

pub fn require_current_capability_policy(config: &LaunchConfig) -> Result<()> {
    let identity = capability_identity(config)?;
    require_current_capability_identity(&identity, &config.cwd)
}

pub fn require_current_capability_identity(
    identity: &CapabilityIdentity,
    cwd: &Path,
) -> Result<()> {
    require_current_capability_identity_with_bundles(
        identity,
        cwd,
        &crate::provider_compatibility::BundleSet::embedded(),
    )
}

#[doc(hidden)]
pub fn require_current_capability_identity_with_bundles(
    identity: &CapabilityIdentity,
    cwd: &Path,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<()> {
    let current = bundles.resolve(
        identity.provider,
        &identity.executable_version,
        identity.role,
    )?;
    if identity.compatibility.as_ref() != Some(&(&current).into()) {
        return Err(
            crate::provider_compatibility::CompatibilityError::ContractChanged {
                explanation: crate::provider_compatibility::CompatibilityExplanation {
                    status: crate::provider_compatibility::CompatibilityStatus::ContractChanged,
                    observed_version: None,
                    pack_id: Some(current.pack_id),
                    pack_revision: Some(current.pack_revision),
                    contract_id: Some(current.contract_id),
                    contract_revision: Some(current.contract_revision),
                    short_hash: Some(current.effective_hash.chars().take(12).collect()),
                    predicate_id: Some(current.predicate_id),
                    missing_evidence: vec!["current_contract_proof".into()],
                    action: crate::provider_compatibility::SafeAction::RequalifyExactProfile,
                    message: "The frozen capability has no matching current compatibility binding."
                        .into(),
                },
            }
            .into(),
        );
    }
    match identity.provider {
        Provider::Codex => codex::require_current_native_policy(identity, cwd),
        Provider::Claude => claude::require_current_native_policy(identity),
    }
}

pub fn capability_identity_key(identity: &CapabilityIdentity) -> Result<String> {
    crate::store::json_hash(identity)
}

fn normalized_provider_argv(config: &LaunchConfig) -> Vec<String> {
    let mut values = config.argv.clone();
    values.pop(); // augmented task prompt
    match config.provider {
        Provider::Codex => {
            if values.len() >= 2 && values[values.len() - 2] == "resume" {
                values.truncate(values.len() - 2);
            }
        }
        Provider::Claude => {
            if let Some(index) = values
                .iter()
                .position(|value| value == "--resume" || value == "--session-id")
            {
                values.drain(index..(index + 2).min(values.len()));
            }
        }
    }
    values
        .into_iter()
        .map(|value| normalize_text(&value, config))
        .collect()
}

fn normalize_value(value: serde_json::Value, config: &LaunchConfig) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => {
            serde_json::Value::String(normalize_text(&value, config))
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| normalize_value(value, config))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (normalize_text(&key, config), normalize_value(value, config)))
                .collect(),
        ),
        other => other,
    }
}

fn normalize_text(value: &str, config: &LaunchConfig) -> String {
    let mut normalized = value.to_owned();
    let policy = config
        .security_policy
        .get("runtime_probe_command_policy")
        .and_then(|value| serde_json::from_value::<RuntimeProbeCommandPolicy>(value.clone()).ok());
    if let Some(policy) = &policy {
        let mut command_replacements = policy
            .commands
            .iter()
            .map(|entry| {
                (
                    entry.command.clone(),
                    format!("<runtime-probe-command:{}>", entry.operation),
                )
            })
            .collect::<Vec<_>>();
        command_replacements.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));
        for (raw, replacement) in command_replacements {
            normalized = normalized.replace(&raw, &replacement);
        }
    }
    let cwd = config.cwd.to_string_lossy();
    let cwd = cwd.trim_end_matches('/');
    let mut paths = Vec::new();
    if !cwd.is_empty() {
        paths.push((cwd.to_owned(), "<attempt-worktree>".to_owned()));
    }
    if let Some(policy) = &policy {
        for (index, path) in policy.write_denials.iter().enumerate() {
            let path = path.to_string_lossy();
            let path = path.trim_end_matches('/');
            if !path.is_empty() && path != cwd {
                paths.push((
                    path.to_owned(),
                    format!("<runtime-probe-write-denial:{index}>"),
                ));
            }
        }
        for (index, path) in policy.read_denials.iter().enumerate() {
            let path = path.to_string_lossy();
            let path = path.trim_end_matches('/');
            if !path.is_empty() {
                paths.push((
                    path.to_owned(),
                    format!("<runtime-probe-read-denial:{index}>"),
                ));
            }
        }
    }
    paths.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
    normalized = replace_bounded_paths(&normalized, &paths);
    if config.provider == Provider::Claude {
        normalize_path_ending(&mut normalized, "/role.sock", "<role-socket>");
    }
    for (suffix, replacement) in [
        ("/agenticjira-hook", "<agenticjira-hook>"),
        (
            "/claude-reviewer-settings.json",
            "<claude-reviewer-settings>",
        ),
        (
            "/claude-implementer-settings.json",
            "<claude-implementer-settings>",
        ),
    ] {
        normalize_path_ending(&mut normalized, suffix, replacement);
    }
    normalized
}

fn replace_bounded_paths(value: &str, paths: &[(String, String)]) -> String {
    // Scan the source once so a path inside an emitted placeholder is never matched again.
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(offset) = value[cursor..].find('/') {
        let start = cursor + offset;
        result.push_str(&value[cursor..start]);
        let preceding = start == 0
            || value[..start].chars().next_back().is_some_and(|character| {
                character.is_whitespace()
                    || matches!(character, '"' | '\'' | '=' | ',' | ';' | '(' | '{' | '[')
            })
            // Claude spells absolute roots in tool rules as Read(//path/**).
            || value[..start].ends_with("(/");
        let replacement = if preceding {
            paths.iter().find(|(path, _)| {
                value[start..].starts_with(path)
                    && value[start + path.len()..]
                        .chars()
                        .next()
                        .is_none_or(|character| {
                            character.is_whitespace()
                                || matches!(
                                    character,
                                    '/' | '"' | '\'' | ',' | ';' | ')' | '}' | ']' | '='
                                )
                        })
            })
        } else {
            None
        };
        if let Some((path, placeholder)) = replacement {
            result.push_str(placeholder);
            cursor = start + path.len();
        } else {
            result.push('/');
            cursor = start + 1;
        }
    }
    result.push_str(&value[cursor..]);
    result
}

fn normalize_path_ending(value: &mut String, suffix: &str, replacement: &str) {
    while let Some(end) = value.find(suffix) {
        let start = value[..end]
            .rfind(|character: char| {
                character == '\"'
                    || character == '\''
                    || character == '='
                    || character == '{'
                    || character.is_whitespace()
            })
            .map(|index| index + 1)
            .unwrap_or(0);
        value.replace_range(start..end + suffix.len(), replacement);
    }
}

pub fn executable_and_version(provider: Provider) -> Result<(PathBuf, String)> {
    let name = match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
    };
    let (located, path_output) = bounded_inspection(
        Command::new("/usr/bin/env").args(["which", name]),
        &format!("locate {name}"),
        INSPECTION_TIMEOUT,
    )?;
    if !located.success() {
        bail!("{name} executable was not found on PATH")
    }
    let path = PathBuf::from(String::from_utf8(path_output)?.trim());
    let mut version_command = Command::new(&path);
    version_command.arg("--version");
    if provider == Provider::Claude {
        version_command
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_UPDATES", "1");
    }
    let (status, version_output) = bounded_inspection(
        &mut version_command,
        &format!("read {} version", path.display()),
        INSPECTION_TIMEOUT,
    )?;
    if !status.success() {
        bail!("{} --version failed", path.display())
    }
    let version = String::from_utf8(version_output)?.trim().to_owned();
    if version.is_empty() {
        bail!("{} returned an empty version", path.display())
    }
    Ok((path, version))
}

const INSPECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const INSPECTION_OUTPUT_LIMIT: usize = 64 * 1024;

/// Runs one short local inspection command with bounded time and stdout. It
/// runs in its own process group so a timeout or oversized output kills
/// every process it started before the child is reaped.
fn bounded_inspection(
    command: &mut Command,
    label: &str,
    timeout: std::time::Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let deadline = std::time::Instant::now() + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("{label}: could not start"))?;
    let stop = |child: &mut std::process::Child| {
        // The child leads its own group, so this reaches its descendants too.
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = child.wait();
    };
    let Some(mut stdout) = child.stdout.take() else {
        stop(&mut child);
        bail!("{label}: output was not captured")
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = (&mut stdout)
            .take(INSPECTION_OUTPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(read);
    });
    let output = match receiver.recv_timeout(timeout) {
        Ok(Ok(bytes)) if bytes.len() <= INSPECTION_OUTPUT_LIMIT => bytes,
        Ok(Ok(_)) => {
            stop(&mut child);
            bail!("{label}: printed more than {INSPECTION_OUTPUT_LIMIT} bytes")
        }
        Ok(Err(error)) => {
            stop(&mut child);
            return Err(error).with_context(|| format!("{label}: could not read output"));
        }
        Err(_) => {
            stop(&mut child);
            bail!(
                "{label}: did not finish within {} seconds",
                timeout.as_secs_f32()
            )
        }
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, output));
        }
        if std::time::Instant::now() >= deadline {
            stop(&mut child);
            bail!(
                "{label}: did not exit within {} seconds",
                timeout.as_secs_f32()
            )
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn toml_string(value: &str) -> Result<String> {
    Ok(toml::Value::String(value.to_owned()).to_string())
}

pub fn role_channel_instructions(role: RoleKind, executable: &Path, socket: &Path) -> String {
    let executable = shell_quote(executable.to_string_lossy().as_ref());
    let common=format!("Use `{executable} role context` to read the bounded task-scoped plan, review/check evidence, feedback, budgets, and command contract. The exact executable for this role channel is `{executable}`; terminal prose is not a result. The role channel is scoped at `{}`. {ROLE_RESULT_REPORT_CONTRACT}",socket.display());
    if role == RoleKind::Manager {
        format!("{common} Manager-only commands are `role propose-transition --operation-id <UUID> --phase <PHASE> --evidence <TEXT>` and `role acknowledge-guidance --guidance <ID>`. Read context again after guidance, role completion, or a review.")
    } else {
        common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_inspection_is_bounded_in_time_and_output() {
        let generous = std::time::Duration::from_secs(5);
        let (status, output) = bounded_inspection(
            Command::new("/bin/sh").args(["-c", "echo 'codex-cli 0.160.0'"]),
            "fixture version",
            generous,
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(
            String::from_utf8(output).unwrap().trim(),
            "codex-cli 0.160.0"
        );

        let pid_file =
            std::env::temp_dir().join(format!("llmrelay-inspection-{}", uuid::Uuid::new_v4()));
        let started = std::time::Instant::now();
        let error = bounded_inspection(
            Command::new("/bin/sh")
                .args(["-c", "echo $$ > \"$0\"; exec sleep 30"])
                .arg(&pid_file),
            "fixture hang",
            std::time::Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("did not finish"), "{error:#}");
        assert!(started.elapsed() < generous);
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the hung inspection was killed and reaped"
        );
        std::fs::remove_file(pid_file).unwrap();

        let error = bounded_inspection(
            Command::new("/bin/sh").args(["-c", "head -c 70000 /dev/zero; sleep 30"]),
            "fixture flood",
            generous,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("printed more than 65536 bytes"),
            "{error:#}"
        );
    }
}
