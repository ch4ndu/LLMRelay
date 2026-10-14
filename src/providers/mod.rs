pub mod claude;
pub mod codex;
#[cfg(test)]
mod native_qualification;

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

/// Exact verification commands and filesystem denials must come from persisted
/// service-owned authority, never from a natural-language request.
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
    pub(crate) fn request_prompt_hash(&self, prompt: &str) -> Result<String> {
        let mut sockets = self
            .environment
            .iter()
            .filter(|(key, _)| key == "AGENTICJIRA_ROLE_SOCKET");
        let (_, socket) = sockets
            .next()
            .context("prepared role launch has no role socket")?;
        if sockets.next().is_some() {
            bail!("prepared role launch has ambiguous role sockets")
        }
        let delivered = format!(
            "{prompt}\n\n{}",
            role_channel_instructions(
                self.config.role,
                &self.supervision_executable,
                Path::new(socket),
            )
        );
        if self.arguments != self.config.argv
            || self.arguments.last().map(String::as_str) != Some(delivered.as_str())
        {
            bail!("prepared role launch does not carry the exact requested prompt")
        }
        crate::store::json_hash(&prompt)
    }

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
    pub read_denials: &'a [PathBuf],
    pub service_root: &'a Path,
    pub prompt: &'a str,
    pub role_socket: &'a Path,
    pub token: &'a str,
    pub role_generation_id: &'a str,
    pub session_id: &'a str,
    pub native_session_id: Option<&'a str>,
    pub assets: &'a HookAssets,
    pub executable_path: &'a Path,
    pub bundles: &'a crate::provider_compatibility::BundleSet,
    pub checkout_scope: Option<&'a serde_json::Value>,
}

struct SingleAgentReadScope {
    worktree: PathBuf,
    denied_roots: Vec<PathBuf>,
    executable_read: Option<PathBuf>,
}

fn single_agent_read_scope(request: &SingleAgentLaunchRequest<'_>) -> Result<SingleAgentReadScope> {
    if let Some(scope) = request.checkout_scope {
        let root = PathBuf::from(scope["root"].as_str().context("Main scope has no root")?);
        if request.cwd.canonicalize()? != root
            || scope["workspace_mode"] != "main_checkout"
            || scope["service_root"].as_str() != request.service_root.canonicalize()?.to_str()
        {
            bail!("The Main checkout or app data scope changed before native preparation; verify the checkout again")
        }
        let executable = request.executable_path.canonicalize()?;
        if executable.starts_with(&root) {
            bail!("Install LLMRelay outside the writable Main checkout before verifying or launching it")
        }
        let denied_roots: Vec<PathBuf> = serde_json::from_value(scope["protected_roots"].clone())?;
        let executable_read = if denied_roots
            .iter()
            .any(|denied| executable.starts_with(denied))
        {
            let metadata = executable.metadata()?;
            if !metadata.is_file() || metadata.mode() & 0o222 != 0 || metadata.mode() & 0o111 == 0 {
                bail!("Install an immutable LLMRelay executable outside protected data before verifying this checkout")
            }
            Some(executable)
        } else {
            None
        };
        return Ok(SingleAgentReadScope {
            worktree: root,
            denied_roots,
            executable_read,
        });
    }
    if request.read_denials.len() != 2
        || !request.cwd.is_absolute()
        || !request.service_root.is_absolute()
    {
        bail!("Single agent read scope needs the registered repository, Git common directory and service root")
    }
    let worktree = request.cwd.canonicalize()?;
    let service_root = request.service_root.canonicalize()?;
    let worktrees = service_root.join("artifacts/worktrees");
    if worktrees.canonicalize()? != worktrees
        || worktree.parent() != Some(worktrees.as_path())
        || !worktree.is_dir()
    {
        bail!("Single agent worktree must be the current direct child of the service's artifacts/worktrees")
    }
    let mut denied_roots = std::collections::BTreeSet::new();
    for root in request.read_denials {
        if !root.is_absolute() || !root.is_dir() {
            bail!("Single agent repository read denial must name an existing absolute directory")
        }
        let root = root.canonicalize()?;
        if worktree.starts_with(&root) {
            bail!("Single agent worktree overlaps a denied repository or Git common directory")
        }
        denied_roots.insert(root);
    }
    if denied_roots.len() != 2 {
        bail!("Single agent read scope requires distinct repository and Git common directory roots")
    }
    denied_roots.insert(service_root);
    let socket_directory = request
        .role_socket
        .parent()
        .context("Single agent role socket directory")?
        .canonicalize()?;
    if denied_roots
        .iter()
        .any(|root| socket_directory.starts_with(root))
    {
        bail!("Single agent role socket must remain outside the denied roots")
    }
    let executable = request.executable_path.canonicalize()?;
    if executable.as_path() != request.executable_path {
        bail!("Single agent role executable must use its canonical file path")
    }
    let executable_read = if denied_roots.iter().any(|root| executable.starts_with(root)) {
        if executable.starts_with(&worktree) {
            bail!("Single agent executable must be outside its writable worktree")
        }
        let metadata = executable.metadata()?;
        if !metadata.is_file() || metadata.mode() & 0o222 != 0 || metadata.mode() & 0o111 == 0 {
            bail!(
                "Single agent executable inside a denied root must be an immutable executable file"
            )
        }
        Some(executable)
    } else {
        None
    };
    Ok(SingleAgentReadScope {
        worktree,
        denied_roots: denied_roots.into_iter().collect(),
        executable_read,
    })
}

pub(crate) fn checkout_launch_scope(
    policy: &serde_json::Value,
    service_root: &Path,
    role_socket: &Path,
    inventory: &[(String, String, String)],
    helper_executable: &Path,
) -> Result<serde_json::Value> {
    let authorization: crate::domain::TaskCheckoutScope =
        serde_json::from_value(policy["checkout_authorization"].clone())?;
    crate::workspace::require_checkout_environment()?;
    let repository = crate::workspace::inspect(Path::new(&authorization.checkout_root))?;
    if authorization.workspace_mode != crate::domain::WorkspaceMode::MainCheckout
        || policy["workspace_ownership"] != "borrowed_checkout"
        || repository.root.to_str() != Some(authorization.checkout_root.as_str())
        || repository.identity != authorization.repository_identity
        || !inventory.iter().any(|(id, path, identity)| {
            id == &authorization.project_id
                && Path::new(path).canonicalize().ok().as_ref() == Some(&repository.root)
                && identity == &repository.identity
        })
    {
        bail!("The authorized checkout or registered repository inventory changed. Relink the project and authorize it with Make Ready.")
    }
    let service_root = service_root.canonicalize()?;
    let helper_executable = executable_fingerprint(helper_executable)?;
    let helper_metadata = helper_executable.canonical_path.metadata()?;
    if !helper_metadata.is_file()
        || helper_metadata.nlink() != 1
        || helper_metadata.mode() & 0o022 != 0
    {
        bail!("Main checkout requires a regular app helper with one link and no group or other write permission; install it outside the checkout")
    }
    if helper_executable
        .canonical_path
        .starts_with(&repository.root)
    {
        bail!("Install LLMRelay outside the writable Main checkout before verifying or launching task roles")
    }
    if policy["checkout_identity"] != crate::workspace::checkout_identity(&repository)? {
        bail!("The borrowed checkout changed physical identity or lacks its starting identity; recover preparation before verifying permissions")
    }
    let mut roots = std::collections::BTreeSet::from([
        service_root.clone(),
        repository.common_directory.clone(),
    ]);
    for (_, path, identity) in inventory {
        let other = crate::workspace::inspect(Path::new(path))?;
        if other.identity != *identity {
            bail!("A registered repository changed identity. Relink it before verifying this Main checkout.")
        }
        if other.root != repository.root {
            roots.insert(other.root);
            roots.insert(other.common_directory);
        }
    }
    for relative in [".local/trip-explorer", ".local/worktree-cleanup"] {
        roots.insert(canonical_protection_path(&repository.root.join(relative))?);
    }
    let home = PathBuf::from(
        std::env::var_os("HOME")
            .context("Native credential protection requires the account home")?,
    );
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let claude_home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    for path in [
        codex_home,
        claude_home,
        home.join(".claude.json"),
        role_socket.with_file_name("control.sock"),
    ] {
        roots.insert(canonical_protection_path(&path)?);
    }
    if roots.contains(&repository.root) {
        bail!("The Main checkout overlaps protected app or credential data. Select a separate checkout; its write grant cannot reopen protected data.")
    }
    for protected in &roots {
        if repository.root.starts_with(protected) && protected != &service_root {
            bail!("The selected checkout is inside a protected repository or credential root; use a separate checkout")
        }
    }
    let metadata = repository.root.metadata()?;
    let common = repository.common_directory.metadata()?;
    let mut observed = Vec::new();
    let mut unobserved = Vec::new();
    for root in &roots {
        if protection_target_exists(root)? {
            observed.push(root.clone());
        } else {
            unobserved.push(root.clone());
        }
    }
    Ok(
        json!({"schema":1,"workspace_mode":"main_checkout","authorization":authorization,
        "root":repository.root,"repository_identity":repository.identity,"common_directory":repository.common_directory,
        "root_identity":{"device":metadata.dev(),"inode":metadata.ino()},
        "common_identity":{"device":common.dev(),"inode":common.ino()},
        "service_root":service_root,"role_socket":role_socket,"protected_roots":roots,"helper_executable":helper_executable,
        "protected_read_targets":observed,"policy_denied_unobserved":unobserved,
        "helper_admission":{"mode":helper_metadata.mode(),"uid":helper_metadata.uid(),"links":helper_metadata.nlink()},
        "repository_inventory_hash":crate::store::json_hash(&inventory)?}),
    )
}

fn canonical_protection_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("Protection paths must be absolute")
    }
    if protection_target_exists(path)? {
        return path.canonicalize().with_context(|| {
            format!(
                "Cannot resolve protected path {}; check for a broken symlink",
                path.display()
            )
        });
    }
    let parent = path.parent().context("Protection path has no parent")?;
    Ok(canonical_protection_path(parent)?
        .join(path.file_name().context("Protection path has no name")?))
}

fn protection_target_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("Cannot inspect protected path {}", path.display()))
        }
    }
}

pub(crate) fn require_main_protection_partition(scope: &serde_json::Value) -> Result<()> {
    let roots: Vec<PathBuf> = serde_json::from_value(scope["protected_roots"].clone())
        .context("Main verification lost its protected roots; verify the current checkout again")?;
    let observed: Vec<PathBuf> = serde_json::from_value(scope["protected_read_targets"].clone())
        .context(
            "Main verification lacks observed-root bindings; verify the current checkout again",
        )?;
    let absent: Vec<PathBuf> = serde_json::from_value(scope["policy_denied_unobserved"].clone())
        .context(
            "Main verification lacks policy-only root bindings; verify the current checkout again",
        )?;
    let all = roots.iter().collect::<std::collections::BTreeSet<_>>();
    let reads = observed.iter().collect::<std::collections::BTreeSet<_>>();
    let missing = absent.iter().collect::<std::collections::BTreeSet<_>>();
    if all.is_empty()
        || roots.iter().any(|root| !root.is_absolute())
        || roots.len() != all.len()
        || observed.len() != reads.len()
        || absent.len() != missing.len()
        || !reads.is_disjoint(&missing)
        || reads
            .union(&missing)
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            != all
    {
        bail!("Main protection observations must partition every protected root exactly once")
    }
    for root in &roots {
        if protection_target_exists(root)? != reads.contains(&root) {
            bail!(
                "Protected path {} appeared or disappeared; verify the current Main checkout again",
                root.display()
            )
        }
    }
    Ok(())
}

pub(crate) fn main_checkout_capability_key(
    actor: &str,
    scope: &serde_json::Value,
    native: &serde_json::Value,
) -> Result<String> {
    crate::store::json_hash(
        &json!({"kind":"main_checkout_proof_v1","actor":actor,"scope":scope,"native_identity":native}),
    )
}

pub(crate) fn bind_checkout_launch(
    launch: &mut PreparedLaunch,
    scope: &serde_json::Value,
    readonly_policy: Option<&RuntimeProbeCommandPolicy>,
) -> Result<()> {
    let config = &mut launch.config;
    if config.cwd.canonicalize()?.to_str() != scope["root"].as_str() {
        bail!("Native preparation does not target the authorized Main checkout")
    }
    // Path normalization in ordinary capability keys must not erase checkout authority.
    config.security_policy["checkout_scope_hash"] = crate::store::json_hash(scope)?.into();
    config.security_policy["checkout_scope"] = scope.clone();
    config.security_policy["execution_actor"] = config.role.to_string().into();
    if let Some(policy) = readonly_policy {
        config.security_policy["main_readonly_command_policy"] = serde_json::to_value(policy)?;
        if config.provider == Provider::Claude {
            claude::bind_main_readonly_policy(launch, policy)?;
        }
    }
    Ok(())
}

pub(crate) fn main_checkout_observation_request(
    scope: &serde_json::Value,
    database: &Path,
) -> Result<serde_json::Value> {
    require_main_protection_partition(scope)?;
    let service_root = Path::new(
        scope["service_root"]
            .as_str()
            .context("Main scope has no service root")?,
    );
    if !database.is_absolute() || !database.starts_with(service_root) {
        bail!("Main verification's write-open target must be the selected app instance's database")
    }
    let roots = scope["protected_read_targets"]
        .as_array()
        .context("Main verification has no protected roots")?;
    use std::os::unix::fs::FileTypeExt;
    let mut targets = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        let root = root
            .as_str()
            .context("Main protection root is not a path")?;
        let metadata = std::fs::symlink_metadata(root)
            .with_context(|| format!("Cannot inspect protected observation target {root}"))?;
        let access = if metadata.file_type().is_socket() {
            "connect"
        } else {
            "read"
        };
        targets.push(
            json!({"operation":format!("protected_read_{index}"),"path":root,"access":access}),
        );
    }
    targets.push(json!({"operation":"service_write_open","path":database,"access":"write"}));
    let control = Path::new(
        scope["role_socket"]
            .as_str()
            .context("Main scope has no role socket")?,
    )
    .with_file_name("control.sock");
    if !std::fs::symlink_metadata(database)
        .with_context(|| {
            format!(
                "Main verification needs the instance database {}",
                database.display()
            )
        })?
        .is_file()
        || !std::fs::symlink_metadata(&control)
            .with_context(|| {
                format!(
                    "Main verification needs the active control socket {}",
                    control.display()
                )
            })?
            .file_type()
            .is_socket()
    {
        bail!("Main verification needs a regular instance database and active control socket")
    }
    targets.push(json!({"operation":"human_control_connect","path":control,"access":"connect"}));
    let encoded = serde_json::to_string(&serde_json::to_string(&targets)?)?;
    // Opens never read, create, truncate or write bytes; the socket sends no protocol request.
    let script = format!("import json,os,socket\nfor target in json.loads({encoded}):\n try:\n  if target['access']=='connect':\n   with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as s:\n    s.settimeout(0.5)\n    s.connect(target['path'])\n  else:\n   fd=os.open(target['path'],(os.O_WRONLY if target['access']=='write' else os.O_RDONLY)|os.O_NOFOLLOW|os.O_NONBLOCK)\n   os.close(fd)\n  result=0\n except OSError as error:\n  result=error.errno\n print(json.dumps({{'operation':target['operation'],'path':target['path'],'access':target['access'],'errno':result}}),flush=True)\n");
    let executable_script = format!("exec({})", serde_json::to_string(&script)?);
    Ok(
        json!({"command":format!("python3 -c {}",shell_quote(&executable_script)),"targets":targets,
        "protected_read_targets":scope["protected_read_targets"],"policy_denied_unobserved":scope["policy_denied_unobserved"]}),
    )
}

pub(crate) fn main_readonly_command_policy(
    scope: &serde_json::Value,
    request: &serde_json::Value,
    paths: &serde_json::Value,
) -> Result<RuntimeProbeCommandPolicy> {
    let paths: Vec<String> = serde_json::from_value(paths.clone())?;
    let root = Path::new(
        scope["root"]
            .as_str()
            .context("Main policy has no checkout")?,
    );
    if paths.len() != 3
        || paths
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != 3
        || paths.iter().any(|path| {
            !path.starts_with(".llmrelay-main-")
                || path.contains('/')
                || !path
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        })
    {
        bail!("Main read-only probes require three unique service-owned checkout filenames")
    }
    let direct = shell_quote(&root.join(&paths[0]).to_string_lossy());
    let compound = shell_quote(&format!(
        "/usr/bin/true ; /usr/bin/touch -- {}",
        shell_quote(&root.join(&paths[1]).to_string_lossy())
    ));
    let redirect = shell_quote(&format!(
        "/usr/bin/printf %s x > {}",
        shell_quote(&root.join(&paths[2]).to_string_lossy())
    ));
    let commands = vec![
        RuntimeProbeCommand {
            operation: "boundary".into(),
            command: request["command"]
                .as_str()
                .context("Main request has no boundary command")?
                .into(),
        },
        RuntimeProbeCommand {
            operation: "direct".into(),
            command: format!("/usr/bin/touch -- {direct}"),
        },
        RuntimeProbeCommand {
            operation: "compound".into(),
            command: format!("/bin/sh -lc {compound}"),
        },
        RuntimeProbeCommand {
            operation: "redirect".into(),
            command: format!("/bin/sh -lc {redirect}"),
        },
    ];
    let read_denials: Vec<PathBuf> = serde_json::from_value(scope["protected_roots"].clone())?;
    let mut write_denials = read_denials.clone();
    write_denials.push(root.to_path_buf());
    write_denials.sort();
    write_denials.dedup();
    Ok(RuntimeProbeCommandPolicy {
        commands,
        read_denials,
        write_denials,
    })
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
    assemble_single_agent_launch(request, native, &binding)
}

/// Production launches must pass the qualification check in
/// `prepare_single_agent_launch` before reaching this shared assembly.
fn assemble_single_agent_launch(
    request: &SingleAgentLaunchRequest<'_>,
    native: SingleAgentNative,
    binding: &crate::provider_compatibility::PolicyBinding,
) -> Result<SingleAgentLaunch> {
    let mut identity = SingleAgentNativeIdentity {
        provider: request.provider,
        executable: executable_fingerprint(&native.executable)?,
        executable_version: native.executable_version,
        account_identity: native.account_identity,
        config_identity: native.config_identity,
        security_policy: native.security_policy,
        hook_revision: native.hook_revision,
        policy_binding_hash: binding.effective_hash.clone(),
    };
    if let Some(scope) = request.checkout_scope {
        // The inherited workspace profile permits ambient reads beyond these explicit denials.
        identity.security_policy["checkout_scope"] = scope.clone();
        identity.security_policy["execution_actor"] = "single_agent".into();
    }
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
    let mut record = json!({
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
    if let Some(scope) = request.checkout_scope {
        record["checkout_scope"] = scope.clone();
        record["native_identity"] = serde_json::to_value(&identity)?;
    }
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
    // Main command authority binds literal requests, including paths with hook/socket suffixes.
    if config.security_policy["main_readonly_command_policy"]["commands"]
        .as_array()
        .is_some_and(|commands| {
            commands.iter().any(|entry| {
                entry["command"]
                    .as_str()
                    .is_some_and(|command| value == command || value == format!("Bash({command})"))
            })
        })
    {
        return value.to_owned();
    }
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
    let main_checkout = config
        .security_policy
        .pointer("/checkout_scope/workspace_mode")
        .and_then(serde_json::Value::as_str)
        == Some("main_checkout");
    let mut paths = Vec::new();
    if !cwd.is_empty() && !main_checkout {
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
    fn main_checkout_observation_request_uses_connect_for_protected_sockets() {
        let root = PathBuf::from("/tmp").join(format!(
            "llmrelay-main-observations-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("protected-file");
        let directory = root.join("protected-directory");
        let database = root.join("database");
        let control = root.join("control.sock");
        std::fs::write(&file, b"protected surrogate").unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(&database, b"instance surrogate").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&control).unwrap();
        let protected = vec![file.clone(), directory.clone(), control.clone()];
        let scope = json!({
            "service_root":root,"role_socket":root.join("role.sock"),
            "protected_roots":protected,"protected_read_targets":protected,
            "policy_denied_unobserved":[]
        });
        let request = main_checkout_observation_request(&scope, &database).unwrap();
        assert_eq!(
            request["targets"],
            json!([
                {"operation":"protected_read_0","path":file,"access":"read"},
                {"operation":"protected_read_1","path":directory,"access":"read"},
                {"operation":"protected_read_2","path":control,"access":"connect"},
                {"operation":"service_write_open","path":database,"access":"write"},
                {"operation":"human_control_connect","path":control,"access":"connect"}
            ])
        );
        assert_eq!(
            request["protected_read_targets"],
            scope["protected_read_targets"]
        );
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_role_request_binding_preserves_exact_prompt_and_rejects_argument_drift() {
        let helper = PathBuf::from("/fixture/llmrelay");
        let socket = "/fixture/role.sock";
        let instructions = role_channel_instructions(RoleKind::Manager, &helper, Path::new(socket));
        let first = format!("  Main verification first\r\n\n\n{instructions}\nretain nonce  ");
        let recall = "Main verification recall. Use conversation memory only.";
        for provider in [Provider::Codex, Provider::Claude] {
            let arguments = vec![
                match provider {
                    Provider::Codex => "--strict-config",
                    Provider::Claude => "--restricted",
                }
                .to_owned(),
                format!("{first}\n\n{instructions}"),
            ];
            let config = LaunchConfig {
                provider,
                role: RoleKind::Manager,
                executable: PathBuf::from("/fixture/provider"),
                executable_version: "fixture".into(),
                model: "selected-model".into(),
                effort: "selected-effort".into(),
                cwd: PathBuf::from("/fixture/checkout"),
                argv: arguments.clone(),
                environment_keys: vec!["AGENTICJIRA_ROLE_SOCKET".into()],
                permission_policy: "fixture".into(),
                security_policy: json!({}),
                hook_revision: "fixture".into(),
                capability_status: crate::domain::CapabilityStatus::Unverified,
                compatibility: None,
            };
            let mut launch = PreparedLaunch {
                config,
                executable: PathBuf::from("/fixture/provider"),
                arguments,
                environment: vec![("AGENTICJIRA_ROLE_SOCKET".into(), socket.into())],
                supervision_executable: helper.clone(),
            };
            let first_hash = launch.request_prompt_hash(&first).unwrap();
            assert_eq!(first_hash, crate::store::json_hash(&first).unwrap());
            assert_ne!(first_hash, crate::store::json_hash(&launch.config).unwrap());
            assert!(launch.request_prompt_hash(first.trim()).is_err());
            assert!(launch.request_prompt_hash(recall).is_err());
            let effective_argv = normalized_provider_argv(&launch.config);

            launch.arguments[1] = format!("{recall}\n\n{instructions}");
            assert!(launch.request_prompt_hash(recall).is_err());
            launch.config.argv = launch.arguments.clone();
            let recall_hash = launch.request_prompt_hash(recall).unwrap();
            assert_eq!(recall_hash, crate::store::json_hash(&recall).unwrap());
            assert_ne!(first_hash, recall_hash);
            assert_eq!(effective_argv, normalized_provider_argv(&launch.config));
            assert!(launch.request_prompt_hash(&first).is_err());

            launch.arguments[1].push_str("\nother output");
            launch.config.argv = launch.arguments.clone();
            assert!(launch.request_prompt_hash(recall).is_err());
            launch.arguments[1] = format!("{recall}\n\n{instructions}");
            launch.config.argv = launch.arguments.clone();
            launch.environment.push(launch.environment[0].clone());
            assert!(launch.request_prompt_hash(recall).is_err());
            launch.environment.clear();
            assert!(launch.request_prompt_hash(recall).is_err());
        }
    }

    #[test]
    fn local_inspection_is_bounded_in_time_and_output() {
        let generous = std::time::Duration::from_secs(5);
        let (status, output) = bounded_inspection(
            Command::new("/bin/sh").args([
                "-c",
                "printf '%s\\n' \"$1\"",
                "fixture",
                codex::EXACT_CODEX_VERSION,
            ]),
            "fixture version",
            generous,
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(
            String::from_utf8(output).unwrap().trim(),
            codex::EXACT_CODEX_VERSION
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
