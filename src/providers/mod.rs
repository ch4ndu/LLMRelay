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

pub const HOOK_REVISION: &str = "agenticjira-hook-v3-permission-events";

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
    pub fn command(&self, anchor_path: &Path) -> CommandBuilder {
        let mut command = CommandBuilder::new(&self.supervision_executable);
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
        command.args(&self.arguments);
        command.cwd(&self.config.cwd);
        for (key, value) in &self.environment {
            command.env(key, value);
        }
        command
    }
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
        "revision": HOOK_REVISION,
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

#[allow(clippy::too_many_arguments)]
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
        if read_denials.iter().any(|denied| {
            role_executable.as_path() == denied.as_path() || role_executable.starts_with(denied)
        }) {
            bail!("setup read isolation requires the LLMRelay executable to be installed outside the target repository and Git common directory")
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
    let canonical = config.executable.canonicalize().with_context(|| {
        format!(
            "resolve provider executable {}",
            config.executable.display()
        )
    })?;
    let metadata = canonical.metadata()?;
    Ok(CapabilityIdentity {
        provider: config.provider,
        executable: ExecutableFingerprint {
            canonical_path: canonical,
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanos: metadata.mtime_nsec(),
        },
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
    let path_output = Command::new("/usr/bin/env")
        .args(["which", name])
        .output()
        .with_context(|| format!("locate {name}"))?;
    if !path_output.status.success() {
        bail!("{name} executable was not found on PATH")
    }
    let path = PathBuf::from(String::from_utf8(path_output.stdout)?.trim());
    let mut version_command = Command::new(&path);
    version_command.arg("--version");
    if provider == Provider::Claude {
        version_command
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_UPDATES", "1");
    }
    let version_output = version_command
        .output()
        .with_context(|| format!("read {} version", path.display()))?;
    if !version_output.status.success() {
        bail!("{} --version failed", path.display())
    }
    let version = String::from_utf8(version_output.stdout)?.trim().to_owned();
    if version.is_empty() {
        bail!("{} returned an empty version", path.display())
    }
    Ok((path, version))
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
