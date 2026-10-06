use super::{shell_quote, toml_string, HookAssets, PreparedLaunch, CODEX_HOOK_REVISION};
use crate::domain::{CapabilityIdentity, CapabilityStatus, LaunchConfig, Provider, RoleKind};
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const IMPLEMENTER_NATIVE_POLICY_STATUS: &str = "unknown_unattested";
pub const IMPLEMENTER_NATIVE_POLICY_REASON: &str = "Codex Implementer requires exact current native validation, including a delivered PermissionRequest decision; native approvals may be reused without an inbox request and app revocation affects only app-owned rules";
pub const DENIED_READ_FLOOR_VERSION: &str = "codex-denied-read-floor-v2";
pub const DENIED_READ_FLOOR_PROFILE: &str = "agenticjira_role";
pub const LEGACY_DENIED_READ_FLOOR_GAP: &str = "historical Codex configuration predates the canonical control-socket denied-read and restricted-proxy floor; fresh native validation is required";
pub const MCP_COVERAGE_REVISION: &str = "codex-local-mcp-coverage-v1-0.157.1";
pub const MCP_COVERAGE_CLASS: &str = "personal_ineligible_observed_prelaunch";
pub const APPROVAL_OWNERSHIP_REVISION: &str = "codex-native-approval-ownership-v1";
pub const LEGACY_MCP_COVERAGE_GAP: &str = "historical Codex configuration predates exact local MCP-source coverage; fresh native validation is required";
pub const LEGACY_APPROVAL_OWNERSHIP_GAP: &str = "historical Codex Implementer configuration predates native approval ownership; fresh native validation is required";
pub const EXACT_CODEX_VERSION: &str = "codex-cli 0.157.1";
pub const LAUNCH_CONTRACT_REVISION: &str = "llmrelay-codex-launch-v2";
pub const RESUME_CONTRACT_REVISION: &str = "llmrelay-codex-resume-v2";
pub const CREDENTIAL_CONTRACT_REVISION: &str = "llmrelay-local-credential-v1";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_AUTH_BYTES: u64 = 256 * 1024;
const ACCESS_TOKEN_MARGIN_SECONDS: i64 = 15 * 60;

const DISABLED_FEATURES: &[&str] = &[
    "api_key_model_discovery",
    "apps",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "codex_apps_mcp_2026_07_28",
    "computer_use",
    "daemon_auto_start",
    "enable_mcp_apps",
    "external_agent_memory_import",
    "image_generation",
    "in_app_browser",
    "in_app_local_automation",
    "memories",
    "plugins",
    "recommended_plugins",
    "remote_plugin",
    "realtime_conversation",
    "skill_mcp_dependency_install",
    "skill_search",
    "tool_call_mcp_elicitation",
    "use_xaa",
];

#[derive(Debug)]
struct NativeCompatibility {
    disabled_mcp_servers: BTreeSet<String>,
    identity: serde_json::Value,
}

#[allow(clippy::too_many_arguments)]
pub fn prepare(
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
    prepare_with_bundles(
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
        &crate::provider_compatibility::BundleSet::embedded(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_with_bundles(
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
    let (executable, version) = super::executable_and_version(Provider::Codex)?;
    let contract_binding = bundles.resolve(Provider::Codex, &version, role)?;
    let compatibility = inspect_native_compatibility(cwd, &version)?;
    let hook_command = shell_quote(&assets.runner.to_string_lossy());
    let (canonical_role_socket, canonical_control_socket) =
        canonical_socket_endpoints(role_socket)?;
    let permission_base = if role == RoleKind::Implementer {
        ":workspace"
    } else {
        ":read-only"
    };
    let role_profile =
        permission_profile(permission_base, &canonical_control_socket, read_denials)?;
    let network_proxy = network_proxy(&canonical_role_socket)?;
    let approval = if role == RoleKind::Implementer {
        "on-request"
    } else {
        "never"
    };
    let mut arguments = vec![
        "--strict-config".to_owned(),
        "--no-daemon".to_owned(),
        "--no-alt-screen".to_owned(),
        "--cd".to_owned(),
        cwd.to_string_lossy().into_owned(),
        "--model".to_owned(),
        model.to_owned(),
        "--ask-for-approval".to_owned(),
        approval.to_owned(),
        "--config".to_owned(),
        "approvals_reviewer=\"user\"".to_owned(),
        "--config".to_owned(),
        format!("model_reasoning_effort={}", toml_string(effort)?),
        "--config".to_owned(),
        role_profile,
        "--config".to_owned(),
        format!("default_permissions=\"{DENIED_READ_FLOOR_PROFILE}\""),
        "--config".to_owned(),
        "features.exec_permission_approvals=false".to_owned(),
        "--config".to_owned(),
        "features.request_permissions_tool=false".to_owned(),
        "--config".to_owned(),
        "cli_auth_credentials_store=\"file\"".to_owned(),
        "--config".to_owned(),
        "features.multi_agent=false".to_owned(),
        "--config".to_owned(),
        "features.multi_agent_v2=false".to_owned(),
        "--config".to_owned(),
        "features.hooks=true".to_owned(),
        "--config".to_owned(),
        network_proxy,
    ];
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "Stop",
        "Interrupt",
        "SubagentStart",
        "SubagentStop",
        "SessionEnd",
    ] {
        let timeout = if role == RoleKind::Implementer && event == "PermissionRequest" {
            crate::permissions::PROVIDER_PERMISSION_TIMEOUT_SECONDS
        } else {
            2
        };
        let event_hook_command = format!("{hook_command} --event {}", shell_quote(event));
        let hook_value = format!(
            "[{{hooks=[{{type=\"command\",command={},timeout={timeout}}}]}}]",
            toml_string(&event_hook_command)?
        );
        arguments.extend(["--config".to_owned(), format!("hooks.{event}={hook_value}")]);
    }
    for feature in DISABLED_FEATURES {
        arguments.extend(["--disable".to_owned(), (*feature).to_owned()]);
    }
    arguments.extend([
        "--config".to_owned(),
        mcp_servers_override(&compatibility.disabled_mcp_servers)?,
    ]);
    if let Some(native_id) = native_session_id {
        arguments.extend(["resume".to_owned(), native_id.to_owned()]);
    }
    arguments.push(augmented_prompt(
        prompt,
        role,
        executable_path,
        &canonical_role_socket,
    ));
    let mut denied_paths = BTreeSet::from([canonical_control_socket.clone()]);
    denied_paths.extend(read_denials.iter().cloned());
    let denied_paths = denied_paths.into_iter().collect::<Vec<_>>();
    let mut denied_read_floor = serde_json::json!({
        "version": DENIED_READ_FLOOR_VERSION,
        "profile": DENIED_READ_FLOOR_PROFILE,
        "extends": permission_base,
        "control_socket": canonical_control_socket,
        "role_socket": canonical_role_socket,
    });
    if !read_denials.is_empty() {
        denied_read_floor["additional_denied_roots"] = serde_json::json!(read_denials);
    }
    let mut security_policy = serde_json::json!({
        "permission_profile": {
            "name": DENIED_READ_FLOOR_PROFILE,
            "extends": permission_base,
            "filesystem": {"deny": denied_paths},
        },
        "denied_read_floor": denied_read_floor,
        "network": {
            "enabled": true,
        },
        "approval": approval,
        "approval_reviewer": "user",
        "features": {
            "exec_permission_approvals": false,
            "request_permissions_tool": false,
            "network_proxy": {
                "enabled": true,
                "mode": "limited",
                "domains": {},
                "unix_sockets": [{"path": canonical_role_socket, "access": "allow"}],
                "allow_local_binding": false,
                "allow_upstream_proxy": false,
                "enable_socks5": false,
                "enable_socks5_udp": false,
                "credential_broker": false,
                "dangerously_allow_non_loopback_proxy": false,
                "dangerously_allow_all_unix_sockets": false,
            },
        },
        "native_command_policy": {
            "status": IMPLEMENTER_NATIVE_POLICY_STATUS,
            "approval_coverage": "not_attested",
            "sandbox_bypass_floor": DENIED_READ_FLOOR_VERSION,
            "production_capability": if role == RoleKind::Implementer { "validation_required" } else { "unverified" },
        },
        "permission_request_timeout_seconds": if role == RoleKind::Implementer {
            Some(crate::permissions::PROVIDER_PERMISSION_TIMEOUT_SECONDS)
        } else {
            None
        },
        "delegation": false,
        "multi_agent": false,
        "local_mcp_coverage": compatibility.identity,
        "hooks": "native_explicit_trust",
    });
    if role == RoleKind::Implementer {
        security_policy["permission_profile"]["filesystem"]["temporary_directories"] =
            serde_json::json!("read-only");
        security_policy["native_approval_ownership"] = serde_json::json!({
            "revision": APPROVAL_OWNERSHIP_REVISION,
            "native_approvals": "honored",
            "new_native_requests": "existing_approval_inbox",
            "revocation_scope": "agenticjira_rules_only",
        });
    }
    Ok(PreparedLaunch {
        config: LaunchConfig {
            provider: Provider::Codex,
            role,
            executable: executable.clone(),
            executable_version: version,
            model: model.to_owned(),
            effort: effort.to_owned(),
            cwd: cwd.to_path_buf(),
            argv: arguments.clone(),
            environment_keys: vec![
                "AGENTICJIRA_PROVIDER".to_owned(),
                "AGENTICJIRA_ROLE_SOCKET".to_owned(),
                "AGENTICJIRA_ROLE_TOKEN".to_owned(),
                "AGENTICJIRA_ROLE_GENERATION_ID".to_owned(),
                "AGENTICJIRA_SESSION_ID".to_owned(),
            ],
            permission_policy: if role == RoleKind::Implementer {
                format!("codex denied-read floor {DENIED_READ_FLOOR_VERSION}; named {permission_base} profile {DENIED_READ_FLOOR_PROFILE}; canonical control socket {} denied; restricted proxy enabled with no allowed domains and only canonical role socket {} allowed; local binding, upstream proxy, SOCKS5, credential broker, non-loopback proxy, and arbitrary Unix sockets disabled; configured local MCP servers disabled under {MCP_COVERAGE_REVISION}; approvals on-request with user reviewer; native approvals honored and new emitted requests use the existing inbox; app revocation affects app-owned rules only; additive permission features disabled; exact native validation required; no sandbox or hook-trust bypass", canonical_control_socket.display(), canonical_role_socket.display())
            } else {
                format!("codex denied-read floor {DENIED_READ_FLOOR_VERSION}; named {permission_base} profile {DENIED_READ_FLOOR_PROFILE}; canonical control socket {} denied; restricted proxy enabled with no allowed domains and only canonical role socket {} allowed; local binding, upstream proxy, SOCKS5, credential broker, non-loopback proxy, and arbitrary Unix sockets disabled; configured local MCP servers disabled under {MCP_COVERAGE_REVISION}; approvals never with user reviewer; additive permission features disabled; native Allow may skip the AgenticJira inbox but cannot discard the local-command sandbox; no sandbox or hook-trust bypass", canonical_control_socket.display(), canonical_role_socket.display())
            },
            security_policy,
            hook_revision: format!("{CODEX_HOOK_REVISION}:{}", assets.codex_revision_hash),
            capability_status: CapabilityStatus::Unverified,
            compatibility: Some(contract_binding),
        },
        executable,
        arguments,
        environment: environment(
            Provider::Codex,
            &canonical_role_socket,
            role_token,
            role_generation_id,
            session_id,
        ),
        supervision_executable: executable_path.to_path_buf(),
    })
}

pub fn require_denied_read_floor(identity: &CapabilityIdentity) -> Result<()> {
    if identity.provider != Provider::Codex {
        return Ok(());
    }
    let floor = identity
        .security_policy
        .get("denied_read_floor")
        .ok_or_else(|| anyhow::anyhow!(LEGACY_DENIED_READ_FLOOR_GAP))?;
    let control_socket = floor
        .get("control_socket")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!(LEGACY_DENIED_READ_FLOOR_GAP))?;
    let role_socket = floor
        .get("role_socket")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!(LEGACY_DENIED_READ_FLOOR_GAP))?;
    let expected_base = if identity.role == RoleKind::Implementer {
        ":workspace"
    } else {
        ":read-only"
    };
    let expected_approval = if identity.role == RoleKind::Implementer {
        "on-request"
    } else {
        "never"
    };
    let endpoints_match = control_socket.is_absolute()
        && role_socket.is_absolute()
        && control_socket.file_name().and_then(|name| name.to_str()) == Some("control.sock")
        && role_socket.file_name().and_then(|name| name.to_str()) == Some("role.sock")
        && control_socket.parent() == role_socket.parent();
    let denied_paths = identity
        .security_policy
        .pointer("/permission_profile/filesystem/deny")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .map(|value| value.as_str().map(PathBuf::from))
                .collect::<Option<Vec<_>>>()
        });
    let Some(denied_paths) = denied_paths else {
        bail!(LEGACY_DENIED_READ_FLOOR_GAP)
    };
    if denied_paths.iter().any(|path| !path.is_absolute())
        || !denied_paths.iter().any(|path| path == &control_socket)
    {
        bail!(LEGACY_DENIED_READ_FLOOR_GAP)
    }
    let mut expected_permission_profile = serde_json::json!({
        "name": DENIED_READ_FLOOR_PROFILE,
        "extends": expected_base,
        "filesystem": {"deny": denied_paths.clone()},
    });
    if identity.role == RoleKind::Implementer {
        expected_permission_profile["filesystem"]["temporary_directories"] =
            serde_json::json!("read-only");
    }
    let expected_network = serde_json::json!({"enabled": true});
    let expected_network_proxy = serde_json::json!({
        "enabled": true,
        "mode": "limited",
        "domains": {},
        "unix_sockets": [{"path": role_socket, "access": "allow"}],
        "allow_local_binding": false,
        "allow_upstream_proxy": false,
        "enable_socks5": false,
        "enable_socks5_udp": false,
        "credential_broker": false,
        "dangerously_allow_non_loopback_proxy": false,
        "dangerously_allow_all_unix_sockets": false,
    });
    let policy_matches = floor.get("version").and_then(serde_json::Value::as_str)
        == Some(DENIED_READ_FLOOR_VERSION)
        && floor.get("profile").and_then(serde_json::Value::as_str)
            == Some(DENIED_READ_FLOOR_PROFILE)
        && floor.get("extends").and_then(serde_json::Value::as_str) == Some(expected_base)
        && identity.security_policy.get("permission_profile") == Some(&expected_permission_profile)
        && identity.security_policy.get("network") == Some(&expected_network)
        && identity.security_policy.pointer("/features/network_proxy")
            == Some(&expected_network_proxy)
        && identity
            .security_policy
            .get("approval")
            .and_then(serde_json::Value::as_str)
            == Some(expected_approval)
        && identity
            .security_policy
            .get("approval_reviewer")
            .and_then(serde_json::Value::as_str)
            == Some("user")
        && identity
            .security_policy
            .pointer("/features/exec_permission_approvals")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && identity
            .security_policy
            .pointer("/features/request_permissions_tool")
            .and_then(serde_json::Value::as_bool)
            == Some(false);
    let profile = permission_profile(expected_base, &control_socket, &denied_paths)?;
    let proxy = network_proxy(&role_socket)?;
    let argv_matches = identity.effective_argv.first().map(String::as_str)
        == Some("--strict-config")
        && identity
            .effective_argv
            .iter()
            .filter(|argument| *argument == "--no-daemon")
            .count()
            == 1
        && argument_pair_count(
            &identity.effective_argv,
            "--ask-for-approval",
            expected_approval,
        ) == 1
        && argument_pair_count(
            &identity.effective_argv,
            "--config",
            "approvals_reviewer=\"user\"",
        ) == 1
        && argument_pair_count(
            &identity.effective_argv,
            "--config",
            &format!("default_permissions=\"{DENIED_READ_FLOOR_PROFILE}\""),
        ) == 1
        && argument_pair_count(&identity.effective_argv, "--config", &profile) == 1
        && config_key_count(
            &identity.effective_argv,
            &format!("permissions.{DENIED_READ_FLOOR_PROFILE}"),
        ) == 1
        && argument_pair_count(&identity.effective_argv, "--config", &proxy) == 1
        && config_key_count(&identity.effective_argv, "features.network_proxy") == 1
        && argument_pair_count(
            &identity.effective_argv,
            "--config",
            "features.exec_permission_approvals=false",
        ) == 1
        && argument_pair_count(
            &identity.effective_argv,
            "--config",
            "features.request_permissions_tool=false",
        ) == 1
        && !identity.effective_argv.iter().any(|argument| {
            let argument = argument.as_str();
            matches!(
                argument,
                "--ignore-rules"
                    | "--dangerously-bypass-approvals-and-sandbox"
                    | "--no-sandbox"
                    | "--allow-unix-socket"
            ) || argument.starts_with("--allow-unix-socket=")
        });
    if !endpoints_match || !policy_matches || !argv_matches {
        bail!(LEGACY_DENIED_READ_FLOOR_GAP)
    }
    Ok(())
}

pub fn require_native_policy_identity(identity: &CapabilityIdentity) -> Result<()> {
    if identity.provider != Provider::Codex {
        return Ok(());
    }
    require_denied_read_floor(identity)?;
    let coverage = identity
        .security_policy
        .get("local_mcp_coverage")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!(LEGACY_MCP_COVERAGE_GAP))?;
    let disabled_names = coverage
        .get("disabled_names")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!(LEGACY_MCP_COVERAGE_GAP))?;
    let names = disabled_names
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| anyhow::anyhow!(LEGACY_MCP_COVERAGE_GAP))?;
    let sorted = names.iter().cloned().collect::<BTreeSet<_>>();
    let fixed_features = coverage
        .get("disabled_features")
        .and_then(serde_json::Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .map(serde_json::Value::as_str)
                .collect::<Option<Vec<_>>>()
        });
    let coverage_matches = coverage.get("revision").and_then(serde_json::Value::as_str)
        == Some(MCP_COVERAGE_REVISION)
        && coverage.get("class").and_then(serde_json::Value::as_str) == Some(MCP_COVERAGE_CLASS)
        && coverage
            .get("codex_version")
            .and_then(serde_json::Value::as_str)
            == Some(EXACT_CODEX_VERSION)
        && coverage
            .get("system_config")
            .and_then(serde_json::Value::as_str)
            == Some("enumerated_if_present")
        && coverage
            .get("user_config")
            .and_then(serde_json::Value::as_str)
            == Some("enumerated_if_present")
        && coverage
            .get("ancestor_configs")
            .and_then(serde_json::Value::as_str)
            == Some("no_additional_mcp_names")
        && coverage
            .get("managed_sources")
            .and_then(serde_json::Value::as_str)
            == Some("absent")
        && coverage
            .get("profile_selector")
            .and_then(serde_json::Value::as_str)
            == Some("absent")
        && coverage
            .get("shared_app_server")
            .and_then(serde_json::Value::as_str)
            == Some("absent")
        && coverage
            .get("capability_roots")
            .and_then(serde_json::Value::as_str)
            == Some("not_supplied_by_local_cli_adapter")
        && coverage
            .get("remote_executor")
            .and_then(serde_json::Value::as_str)
            == Some("not_forwarded_by_cleared_child_environment")
        && coverage
            .get("executor_environment_config")
            .and_then(serde_json::Value::as_str)
            == Some("absent_at_prelaunch_check")
        && coverage
            .get("credential_store")
            .and_then(serde_json::Value::as_str)
            == Some("file")
        && coverage
            .get("cloud_cache")
            .and_then(serde_json::Value::as_str)
            == Some("absent_at_prelaunch_check")
        && names.len() == sorted.len()
        && names == sorted.iter().cloned().collect::<Vec<_>>()
        && fixed_features.as_deref() == Some(DISABLED_FEATURES);
    if !coverage_matches {
        bail!(LEGACY_MCP_COVERAGE_GAP)
    }
    for feature in DISABLED_FEATURES {
        if argument_pair_count(&identity.effective_argv, "--disable", feature) != 1 {
            bail!(LEGACY_MCP_COVERAGE_GAP)
        }
    }
    if argument_pair_count(
        &identity.effective_argv,
        "--config",
        "cli_auth_credentials_store=\"file\"",
    ) != 1
    {
        bail!(LEGACY_MCP_COVERAGE_GAP)
    }
    let expected_environment = [
        "AGENTICJIRA_PROVIDER",
        "AGENTICJIRA_ROLE_SOCKET",
        "AGENTICJIRA_ROLE_TOKEN",
        "AGENTICJIRA_ROLE_GENERATION_ID",
        "AGENTICJIRA_SESSION_ID",
    ];
    if identity
        .environment_contract
        .iter()
        .map(String::as_str)
        .ne(expected_environment)
        || identity.effective_argv.iter().any(|argument| {
            argument == "--profile"
                || argument.starts_with("--profile=")
                || argument == "app-server"
        })
    {
        bail!(LEGACY_MCP_COVERAGE_GAP)
    }
    let combined_override = mcp_servers_override(&sorted)?;
    if argument_pair_count(&identity.effective_argv, "--config", &combined_override) != 1
        || config_key_count(&identity.effective_argv, "mcp_servers") != 1
        || identity
            .effective_argv
            .windows(2)
            .any(|pair| pair[0] == "--config" && pair[1].starts_with("mcp_servers."))
    {
        bail!(LEGACY_MCP_COVERAGE_GAP)
    }
    if identity.role == RoleKind::Implementer {
        let expected = serde_json::json!({
            "revision": APPROVAL_OWNERSHIP_REVISION,
            "native_approvals": "honored",
            "new_native_requests": "existing_approval_inbox",
            "revocation_scope": "agenticjira_rules_only",
        });
        if identity.security_policy.get("native_approval_ownership") != Some(&expected) {
            bail!(LEGACY_APPROVAL_OWNERSHIP_GAP)
        }
    } else if identity
        .security_policy
        .get("native_approval_ownership")
        .is_some()
    {
        bail!(LEGACY_APPROVAL_OWNERSHIP_GAP)
    }
    Ok(())
}

pub fn require_current_native_policy(identity: &CapabilityIdentity, cwd: &Path) -> Result<()> {
    if identity.provider != Provider::Codex {
        return Ok(());
    }
    require_native_policy_identity(identity)?;
    let current = inspect_native_compatibility(cwd, &identity.executable_version)?;
    if identity.security_policy.get("local_mcp_coverage") != Some(&current.identity) {
        bail!("current Codex local configuration no longer matches the validated MCP and personal-account compatibility policy")
    }
    Ok(())
}

fn canonical_socket_endpoints(role_socket: &Path) -> Result<(PathBuf, PathBuf)> {
    if !role_socket.is_absolute()
        || role_socket.file_name().and_then(|name| name.to_str()) != Some("role.sock")
    {
        bail!("Codex role socket must be an absolute role.sock path")
    }
    if fs::symlink_metadata(role_socket)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("Codex role socket must not be a symlink")
    }
    let parent = role_socket
        .parent()
        .context("Codex role socket has no parent directory")?
        .canonicalize()
        .context("canonicalize Codex role socket directory")?;
    let canonical_role_socket = parent.join("role.sock");
    let canonical_control_socket = parent.join("control.sock");
    Ok((canonical_role_socket, canonical_control_socket))
}

fn permission_profile(
    base: &str,
    control_socket: &Path,
    read_denials: &[PathBuf],
) -> Result<String> {
    let mut denied = BTreeSet::from([control_socket.to_path_buf()]);
    denied.extend(read_denials.iter().cloned());
    let mut filesystem = denied
        .iter()
        .map(|path| {
            Ok(format!(
                "{}=\"deny\"",
                toml_string(&path.to_string_lossy())?
            ))
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
    if base == ":workspace" {
        filesystem.push_str(",\":tmpdir\"=\"read\",\":slash_tmp\"=\"read\"");
    }
    Ok(format!(
        "permissions.{DENIED_READ_FLOOR_PROFILE}={{extends={},filesystem={{{filesystem}}},network={{enabled=true}}}}",
        toml_string(base)?,
    ))
}

fn network_proxy(role_socket: &Path) -> Result<String> {
    Ok(format!(
        "features.network_proxy={{enabled=true,mode=\"limited\",domains={{}},unix_sockets={{{}=\"allow\"}},allow_local_binding=false,allow_upstream_proxy=false,enable_socks5=false,enable_socks5_udp=false,credential_broker=false,dangerously_allow_non_loopback_proxy=false,dangerously_allow_all_unix_sockets=false}}",
        toml_string(&role_socket.to_string_lossy())?,
    ))
}

fn argument_pair_count(arguments: &[String], flag: &str, value: &str) -> usize {
    arguments
        .windows(2)
        .filter(|pair| pair[0] == flag && pair[1] == value)
        .count()
}

fn config_key_count(arguments: &[String], expected_key: &str) -> usize {
    arguments
        .windows(2)
        .filter(|pair| {
            if pair[0] != "--config" {
                return false;
            }
            let key = pair[1].split_once('=').map(|(key, _)| key).unwrap_or("");
            key == expected_key
                || key
                    .strip_prefix(expected_key)
                    .map(|suffix| suffix.starts_with('.'))
                    .unwrap_or(false)
        })
        .count()
}

fn inspect_native_compatibility(cwd: &Path, version: &str) -> Result<NativeCompatibility> {
    if version != EXACT_CODEX_VERSION {
        bail!("Codex local compatibility requires exact {EXACT_CODEX_VERSION}")
    }
    if std::env::var_os("CODEX_HOME").is_some() {
        bail!("Codex local compatibility does not support a parent CODEX_HOME override")
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| anyhow::anyhow!("Codex local compatibility requires an absolute HOME"))?;
    let codex_home = home.join(".codex");
    require_local_executor_home(&codex_home)?;
    for (path, label) in [
        (
            Path::new("/etc/codex/managed_config.toml"),
            "managed Codex configuration",
        ),
        (
            Path::new("/etc/codex/requirements.toml"),
            "managed Codex requirements",
        ),
    ] {
        if path_exists(path)? {
            bail!("Codex local compatibility does not support {label}")
        }
    }
    if managed_preferences_present()? {
        bail!("Codex local compatibility does not support managed Codex configuration or requirements preferences")
    }
    if path_exists(&codex_home.join("cloud-config-bundle-cache.json"))? {
        bail!("Codex local compatibility requires cloud-config-bundle-cache.json to be absent; AgenticJira never deletes or parses it")
    }
    if path_exists(
        &codex_home
            .join("app-server-control")
            .join("app-server-control.sock"),
    )? {
        bail!("Codex local compatibility does not support a shared app-server attachment")
    }

    let mut unconditional = BTreeSet::new();
    for (path, label) in [
        (PathBuf::from("/etc/codex/config.toml"), "system"),
        (codex_home.join("config.toml"), "user"),
    ] {
        if let Some(value) = parse_config(&path, label)? {
            validate_local_config(&value, label)?;
            unconditional.extend(mcp_names(&value, label)?);
        }
    }
    let canonical_cwd = cwd
        .canonicalize()
        .with_context(|| format!("resolve Codex working directory {}", cwd.display()))?;
    let mut ancestors = canonical_cwd.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        let path = ancestor.join(".codex").join("config.toml");
        let Some(value) = parse_config(&path, "project ancestor")? else {
            continue;
        };
        validate_local_config(&value, "project ancestor")?;
        let names = mcp_names(&value, "project ancestor")?;
        if names.iter().any(|name| !unconditional.contains(name)) {
            bail!("Codex project-layer MCP configuration introduces a conditional server name; v1 supports only names already present in unconditional system/user layers")
        }
    }
    classify_file_auth(&codex_home.join("auth.json"))?;
    let identity = serde_json::json!({
        "revision": MCP_COVERAGE_REVISION,
        "class": MCP_COVERAGE_CLASS,
        "codex_version": EXACT_CODEX_VERSION,
        "system_config": "enumerated_if_present",
        "user_config": "enumerated_if_present",
        "ancestor_configs": "no_additional_mcp_names",
        "managed_sources": "absent",
        "profile_selector": "absent",
        "shared_app_server": "absent",
        "capability_roots": "not_supplied_by_local_cli_adapter",
        "remote_executor": "not_forwarded_by_cleared_child_environment",
        "executor_environment_config": "absent_at_prelaunch_check",
        "credential_store": "file",
        "cloud_cache": "absent_at_prelaunch_check",
        "disabled_features": DISABLED_FEATURES,
        "disabled_names": unconditional.iter().cloned().collect::<Vec<_>>(),
    });
    Ok(NativeCompatibility {
        disabled_mcp_servers: unconditional,
        identity,
    })
}

fn require_local_executor_home(codex_home: &Path) -> Result<()> {
    if path_exists(&codex_home.join("environments.toml"))? {
        bail!("Codex local compatibility requires environments.toml to be absent; configured executor environments are not qualified and LLMRelay never removes them")
    }
    Ok(())
}

const CAPACITY_RECORD_BYTES: usize = 256 * 1024;
const CAPACITY_TAIL_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CapacityFileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanos: i64,
    changed_seconds: i64,
    changed_nanos: i64,
}

impl CapacityFileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Option<Self> {
        metadata.is_file().then(|| Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanos: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanos: metadata.ctime_nsec(),
        })
    }
}

pub(crate) fn capacity_history_root() -> Option<PathBuf> {
    if std::env::var_os("CODEX_HOME").is_some() {
        return None;
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".codex/sessions"))
}

fn capacity_history_parent(root: &Path, path: &Path) -> Option<(fs::File, CString)> {
    use std::path::Component;
    let normal = |path: &Path| {
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
            && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
    };
    if !normal(root) || !normal(path) || !path.starts_with(root) || path == root {
        return None;
    }
    let mut directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/")
        .ok()?;
    for part in path.parent()?.components().skip(1) {
        let name = CString::new(part.as_os_str().as_bytes()).ok()?;
        directory = capacity_openat(&directory, &name, libc::O_DIRECTORY)?;
    }
    Some((directory, CString::new(path.file_name()?.as_bytes()).ok()?))
}

fn capacity_openat(directory: &fs::File, name: &CStr, flags: i32) -> Option<fs::File> {
    // The directory FD and NUL-terminated name stay alive through openat; only
    // a successful, newly owned FD is transferred to File.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | flags,
        )
    };
    if fd < 0 {
        return None;
    }
    Some(unsafe { fs::File::from_raw_fd(fd) })
}

fn capacity_history_unchanged(
    root: &Path,
    path: &Path,
    file: &fs::File,
    before: &CapacityFileIdentity,
) -> Option<()> {
    if CapacityFileIdentity::from_metadata(&file.metadata().ok()?).as_ref() != Some(before) {
        return None;
    }
    let (directory, name) = capacity_history_parent(root, path)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // fstatat initializes the buffer only on success. Rewalk without following
    // symlinks before comparing the named file to the original descriptor.
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return None;
    }
    let metadata = unsafe { metadata.assume_init() };
    (metadata.st_mode & libc::S_IFMT == libc::S_IFREG
        && metadata.st_dev as u64 == before.device
        && metadata.st_ino == before.inode
        && metadata.st_size as u64 == before.bytes
        && metadata.st_mtime == before.modified_seconds
        && metadata.st_mtime_nsec == before.modified_nanos
        && metadata.st_ctime == before.changed_seconds
        && metadata.st_ctime_nsec == before.changed_nanos)
        .then_some(())
}

pub(crate) fn read_capacity_history(
    root: &Path,
    path: &Path,
    native_id: &str,
    turn_id: &str,
    cwd: &Path,
) -> Option<CapacityFileIdentity> {
    let (directory, name) = capacity_history_parent(root, path)?;
    let mut file = capacity_openat(&directory, &name, 0)?;
    let identity = CapacityFileIdentity::from_metadata(&file.metadata().ok()?)?;
    let mut head = Vec::new();
    (&mut file)
        .take(CAPACITY_RECORD_BYTES as u64)
        .read_to_end(&mut head)
        .ok()?;
    let end = head.iter().position(|byte| *byte == b'\n')?;
    let meta: serde_json::Value = serde_json::from_slice(&head[..end]).ok()?;
    let payload = meta.get("payload")?;
    if meta.get("type")?.as_str()? != "session_meta"
        || payload.get("id")?.as_str()? != native_id
        || payload.get("session_id")?.as_str()? != native_id
        || payload.get("cli_version")?.as_str()? != "0.157.1"
        || payload.get("source")?.as_str()? != "cli"
        || payload.get("originator")?.as_str()? != "codex-tui"
        || payload.get("thread_source")?.as_str()? != "user"
        || fs::canonicalize(payload.get("cwd")?.as_str()?).ok()? != cwd
    {
        return None;
    }
    let tail_start = identity.bytes.saturating_sub(CAPACITY_TAIL_BYTES - 1);
    file.seek(SeekFrom::Start(tail_start.saturating_sub(1)))
        .ok()?;
    let mut tail = Vec::new();
    (&mut file)
        .take(CAPACITY_TAIL_BYTES)
        .read_to_end(&mut tail)
        .ok()?;
    if tail.last() != Some(&b'\n') {
        return None;
    }
    // The first byte is lookbehind when clipped, keeping the entire tail read
    // within 1 MiB without mistaking a fragment for a complete record.
    let tail = if tail_start > 0 {
        let complete_start = if tail.first() == Some(&b'\n') {
            1
        } else {
            tail.iter().position(|byte| *byte == b'\n')? + 1
        };
        &tail[complete_start..]
    } else {
        tail.as_slice()
    };
    capacity_turn_complete(tail, turn_id)?;
    capacity_history_unchanged(root, path, &file, &identity)?;
    Some(identity)
}

fn capacity_turn_complete(tail: &[u8], turn_id: &str) -> Option<()> {
    let mut started = false;
    let mut completed = false;
    for line in tail.strip_suffix(b"\n")?.split(|byte| *byte == b'\n') {
        if line.len() > CAPACITY_RECORD_BYTES {
            return None;
        }
        let record: serde_json::Value = serde_json::from_slice(line).ok()?;
        if record.get("type")?.as_str()? != "event_msg" {
            continue;
        }
        let event = record.get("payload")?;
        let kind = event.get("type")?.as_str()?;
        if !matches!(kind, "task_started" | "task_complete" | "turn_aborted") {
            continue;
        }
        if event.get("turn_id")?.as_str()? != turn_id {
            if started {
                return None;
            }
            continue;
        }
        match kind {
            "task_started" if !started => started = true,
            "task_complete"
                if started
                    && !completed
                    && event.pointer("/error/codex_error_info")?.as_str()?
                        == "server_overloaded" =>
            {
                completed = true;
            }
            _ => return None,
        }
    }
    completed.then_some(())
}

pub(crate) const USAGE_CONTRACT_REVISION: &str = "codex-0.157.1-response-usage-v1";

pub(crate) fn valid_usage_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

#[derive(Debug)]
pub(crate) struct CodexResponseUsage {
    pub(crate) response_id: String,
    pub(crate) counters: crate::domain::ObservedUsageCounters,
}

pub(crate) enum UsageHistoryRead {
    Unchanged,
    Observed {
        identity: CapacityFileIdentity,
        responses: Vec<CodexResponseUsage>,
    },
}

pub(crate) fn usage_history_identity(root: &Path, path: &Path) -> Option<CapacityFileIdentity> {
    let (directory, name) = capacity_history_parent(root, path)?;
    let file = capacity_openat(&directory, &name, 0)?;
    let identity = CapacityFileIdentity::from_metadata(&file.metadata().ok()?)?;
    capacity_history_unchanged(root, path, &file, &identity)?;
    Some(identity)
}

pub(crate) fn read_usage_history(
    root: &Path,
    path: &Path,
    native_id: &str,
    turn_id: &str,
    cwd: &Path,
    cached: Option<&CapacityFileIdentity>,
) -> Option<UsageHistoryRead> {
    if !valid_usage_id(native_id)
        || uuid::Uuid::parse_str(native_id).is_err()
        || !valid_usage_id(turn_id)
    {
        return None;
    }
    let (directory, name) = capacity_history_parent(root, path)?;
    let mut file = capacity_openat(&directory, &name, 0)?;
    let identity = CapacityFileIdentity::from_metadata(&file.metadata().ok()?)?;
    if cached == Some(&identity) {
        capacity_history_unchanged(root, path, &file, &identity)?;
        return Some(UsageHistoryRead::Unchanged);
    }
    let mut head = Vec::new();
    (&mut file)
        .take(CAPACITY_RECORD_BYTES as u64)
        .read_to_end(&mut head)
        .ok()?;
    let end = head.iter().position(|byte| *byte == b'\n')?;
    let meta: serde_json::Value = serde_json::from_slice(&head[..end]).ok()?;
    let payload = meta.get("payload")?;
    if meta.get("type")?.as_str()? != "session_meta"
        || payload.get("id")?.as_str()? != native_id
        || payload.get("session_id")?.as_str()? != native_id
        || payload.get("cli_version")?.as_str()? != "0.157.1"
        || payload.get("source")?.as_str()? != "cli"
        || payload.get("originator")?.as_str()? != "codex-tui"
        || payload.get("thread_source")?.as_str()? != "user"
        || fs::canonicalize(payload.get("cwd")?.as_str()?).ok()? != cwd
    {
        return None;
    }
    let tail_start = identity.bytes.saturating_sub(CAPACITY_TAIL_BYTES - 1);
    file.seek(SeekFrom::Start(tail_start.saturating_sub(1)))
        .ok()?;
    let mut tail = Vec::new();
    (&mut file)
        .take(CAPACITY_TAIL_BYTES)
        .read_to_end(&mut tail)
        .ok()?;
    if tail.last() != Some(&b'\n') {
        return None;
    }
    let tail = if tail_start > 0 {
        let start = if tail.first() == Some(&b'\n') {
            1
        } else {
            tail.iter().position(|byte| *byte == b'\n')? + 1
        };
        &tail[start..]
    } else {
        tail.as_slice()
    };
    let responses = usage_responses(tail, native_id, turn_id)?;
    capacity_history_unchanged(root, path, &file, &identity)?;
    Some(UsageHistoryRead::Observed {
        identity,
        responses,
    })
}

fn usage_responses(tail: &[u8], native_id: &str, turn_id: &str) -> Option<Vec<CodexResponseUsage>> {
    let mut started = false;
    let mut responses = Vec::new();
    for line in tail.strip_suffix(b"\n")?.split(|byte| *byte == b'\n') {
        if line.len() > CAPACITY_RECORD_BYTES {
            return None;
        }
        let record: serde_json::Value = serde_json::from_slice(line).ok()?;
        let kind = record.get("type")?.as_str()?;
        if kind == "event_msg" && record.pointer("/payload/type")?.as_str()? == "task_started" {
            if record.pointer("/payload/turn_id")?.as_str()? == turn_id {
                if started {
                    return None;
                }
                started = true;
            } else if started {
                return None;
            }
        }
        if kind != "token_usage_record" || !started {
            continue;
        }
        let payload = record.get("payload")?.as_object()?;
        let id = |key| payload.get(key)?.as_str().filter(|id| valid_usage_id(id));
        let thread = id("thread_id")?;
        let session = id("session_id")?;
        let turn = id("turn_id")?;
        let root = id("root_turn_id")?;
        let response_id = id("response_id")?;
        if uuid::Uuid::parse_str(thread).is_err() || uuid::Uuid::parse_str(session).is_err() {
            return None;
        }
        if thread != native_id || session != native_id || turn != turn_id || root != turn_id {
            continue;
        }
        let usage = payload.get("usage")?.as_object()?;
        if usage
            .get("cache_write_input_tokens")
            .is_some_and(serde_json::Value::is_null)
        {
            return None;
        }
        let counters: crate::domain::ObservedUsageCounters =
            serde_json::from_value(serde_json::Value::Object(usage.clone())).ok()?;
        if !counters.valid() {
            return None;
        }
        responses.push(CodexResponseUsage {
            response_id: response_id.into(),
            counters,
        });
    }
    started.then_some(responses)
}

#[cfg(test)]
mod executor_environment_tests {
    use super::*;

    const CAPACITY_NATIVE: &str = "01a0fad1-a051-7941-84b4-e64979f74d26";
    const CAPACITY_TURN: &str = "01a0fad1-a06e-7da3-8bfc-1a96b443ad5d";

    fn capacity_fixture() -> (PathBuf, PathBuf, Vec<serde_json::Value>) {
        let root = std::env::temp_dir().join(format!("llmrelay-capacity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("sessions/day")).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("sessions/day/rollout.jsonl");
        let records = vec![
            serde_json::json!({"type":"session_meta","payload":{"id":CAPACITY_NATIVE,
                "session_id":CAPACITY_NATIVE,"cwd":root,"cli_version":"0.157.1",
                "source":"cli","originator":"codex-tui","thread_source":"user","history_mode":"paginated"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":CAPACITY_TURN}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":CAPACITY_TURN,
                "error":{"codex_error_info":"server_overloaded","message":"Selected model is at capacity."}}}),
        ];
        (root, path, records)
    }

    fn write_capacity_records(path: &Path, records: &[serde_json::Value]) {
        fs::write(
            path,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }

    #[test]
    fn codex_usage_reads_only_current_response_records_and_preserves_zero() {
        let (root, path, mut records) = capacity_fixture();
        let response = serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"response-a","usage":{
                "input_tokens":10,"cached_input_tokens":4,"cache_write_input_tokens":3,
                "output_tokens":5,"reasoning_output_tokens":2,"total_tokens":17},
            "turn_token_usage":{"input_tokens":9000,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":9000},
            "thread_token_usage":{"input_tokens":90000,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":90000}}});
        records.insert(1, response.clone());
        records.push(response.clone());
        let mut zero = response.clone();
        zero["payload"]["response_id"] = serde_json::json!("response-zero");
        zero["payload"]["usage"] = serde_json::json!({"input_tokens":0,"cached_input_tokens":0,
            "output_tokens":0,"reasoning_output_tokens":0,"total_tokens":0});
        records.push(zero.clone());
        records.push(serde_json::json!({"type":"event_msg","payload":{"type":"token_count",
            "info":{"total_token_usage":{"input_tokens":999999,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":999999},
                "last_token_usage":{"input_tokens":999999,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":999999},"model_context_window":null},"rate_limits":null}}));
        records.push(serde_json::json!({"type":"compacted","payload":{"message":"","latest_token_usage_record":response}}));
        let mut subagent = response;
        subagent["payload"]["turn_id"] = serde_json::json!("subagent-turn");
        records.push(subagent);
        write_capacity_records(&path, &records);
        let read = |cached| {
            read_usage_history(
                &root.join("sessions"),
                &path,
                CAPACITY_NATIVE,
                CAPACITY_TURN,
                &root,
                cached,
            )
        };
        let Some(UsageHistoryRead::Observed {
            identity,
            responses,
        }) = read(None)
        else {
            panic!("missing observation");
        };
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].counters.total_tokens, 17);
        assert_eq!(responses[0].counters.cached_input_tokens, 4);
        assert_eq!(responses[0].counters.reasoning_output_tokens, 2);
        assert_eq!(responses[1].counters.total_tokens, 0);
        assert_eq!(responses[1].counters.cache_write_input_tokens, None);
        assert!(matches!(
            read(Some(&identity)),
            Some(UsageHistoryRead::Unchanged)
        ));
        records.push(zero);
        write_capacity_records(&path, &records);
        assert!(matches!(
            read(Some(&identity)),
            Some(UsageHistoryRead::Observed { .. })
        ));
        assert!(read_usage_history(
            &root.join("sessions"),
            &path,
            CAPACITY_NATIVE,
            "other-turn",
            &root,
            None
        )
        .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_usage_rejects_unsafe_contracts_and_missing_or_incomplete_boundaries() {
        let (root, path, mut records) = capacity_fixture();
        records.push(serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"response","usage":{
                "input_tokens":10,"cached_input_tokens":4,"cache_write_input_tokens":0,"output_tokens":5,
                "reasoning_output_tokens":2,"total_tokens":15},
            "turn_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":5,
                "reasoning_output_tokens":2,"total_tokens":15},
            "thread_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":5,
                "reasoning_output_tokens":2,"total_tokens":15}}}));
        let read = || {
            read_usage_history(
                &root.join("sessions"),
                &path,
                CAPACITY_NATIVE,
                CAPACITY_TURN,
                &root,
                None,
            )
        };
        for (index, pointer, value) in [
            (0, "/payload/cli_version", serde_json::json!("0.160.0")),
            (0, "/payload/id", serde_json::json!("other-native-session")),
            (0, "/payload/cwd", serde_json::json!(root.join("sessions"))),
            (
                3,
                "/payload/response_id",
                serde_json::json!("bad\u{0085}id"),
            ),
            (
                3,
                "/payload/response_id",
                serde_json::json!("x".repeat(129)),
            ),
            (3, "/payload/usage/input_tokens", serde_json::json!(-1)),
            (
                3,
                "/payload/usage/cached_input_tokens",
                serde_json::json!(11),
            ),
            (3, "/payload/usage/output_tokens", serde_json::json!(1.5)),
            (
                3,
                "/payload/usage/total_tokens",
                serde_json::json!(9_007_199_254_740_992_i64),
            ),
            (
                3,
                "/payload/usage/cache_write_input_tokens",
                serde_json::Value::Null,
            ),
        ] {
            let mut changed = records.clone();
            *changed[index]
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("missing {pointer}")) = value;
            write_capacity_records(&path, &changed);
            assert!(read().is_none(), "{pointer}");
        }
        for key in ["thread_id", "session_id", "root_turn_id"] {
            let mut changed = records.clone();
            changed[3]["payload"][key] = serde_json::json!("01a0fad3-1e45-71e2-8686-83184958e2c9");
            write_capacity_records(&path, &changed);
            let Some(UsageHistoryRead::Observed { responses, .. }) = read() else {
                panic!("missing safe read for mismatched {key}");
            };
            assert!(responses.is_empty(), "{key} must not contribute usage");
        }
        let mut absent = records.clone();
        absent[3]["payload"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("total_tokens");
        write_capacity_records(&path, &absent);
        assert!(read().is_none());
        write_capacity_records(&path, &records);
        let mut bytes = fs::read(&path).unwrap();
        bytes.pop();
        fs::write(&path, bytes).unwrap();
        assert!(read().is_none());
        let oversized =
            serde_json::json!({"type":"notice","payload":"x".repeat(CAPACITY_RECORD_BYTES)});
        write_capacity_records(&path, &[records.clone(), vec![oversized]].concat());
        assert!(read().is_none());
        records.remove(1);
        write_capacity_records(&path, &records);
        assert!(read().is_none());
        fs::write(&path, b"{\"type\":\"session_meta\"").unwrap();
        assert!(read().is_none());
        let link = root.join("sessions/link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(usage_history_identity(&root.join("sessions"), &link).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_capacity_reader_requires_exact_identity_and_structured_overload() {
        let (root, path, records) = capacity_fixture();
        let read = || {
            read_capacity_history(
                &root.join("sessions"),
                &path,
                CAPACITY_NATIVE,
                CAPACITY_TURN,
                &root,
            )
        };
        write_capacity_records(&path, &records);
        assert!(read().is_some());
        for (index, pointer, value) in [
            (0, "/payload/id", serde_json::json!("another-root")),
            (
                0,
                "/payload/session_id",
                serde_json::json!("another-session"),
            ),
            (0, "/payload/cwd", serde_json::json!(root.join("sessions"))),
            (0, "/payload/cli_version", serde_json::json!("0.159.2")),
            (0, "/payload/source", serde_json::json!("subagent")),
            (0, "/payload/originator", serde_json::json!("other")),
            (0, "/payload/thread_source", serde_json::json!("subagent")),
            (1, "/payload/turn_id", serde_json::json!("other-turn")),
            (2, "/payload/turn_id", serde_json::Value::Null),
            (
                2,
                "/payload/error/codex_error_info",
                serde_json::json!("rate_limit_exceeded"),
            ),
            (
                2,
                "/payload/error/codex_error_info",
                serde_json::json!({"server_overloaded":true}),
            ),
            (
                2,
                "/payload/error/codex_error_info",
                serde_json::Value::Null,
            ),
        ] {
            let mut changed = records.clone();
            *changed[index].pointer_mut(pointer).unwrap() = value;
            write_capacity_records(&path, &changed);
            assert!(read().is_none(), "{index}:{pointer}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_capacity_reader_defers_ambiguous_sequences_and_bounded_partial_records() {
        let (root, path, records) = capacity_fixture();
        let read = || {
            read_capacity_history(
                &root.join("sessions"),
                &path,
                CAPACITY_NATIVE,
                CAPACITY_TURN,
                &root,
            )
        };
        let mut later = records[1].clone();
        later["payload"]["turn_id"] = serde_json::json!("later-turn");
        let mut aborted = records[1].clone();
        aborted["payload"]["type"] = serde_json::json!("turn_aborted");
        let mut conflict = records[2].clone();
        conflict["payload"]["error"]["codex_error_info"] = serde_json::json!("rate_limit_exceeded");
        for sequence in [
            vec![records[0].clone(), records[2].clone()],
            vec![records[0].clone(), records[2].clone(), records[1].clone()],
            vec![
                records[0].clone(),
                records[1].clone(),
                records[1].clone(),
                records[2].clone(),
            ],
            [records.clone(), vec![records[2].clone()]].concat(),
            [records.clone(), vec![conflict]].concat(),
            [records.clone(), vec![later.clone()]].concat(),
            [records.clone(), vec![aborted]].concat(),
        ] {
            write_capacity_records(&path, &sequence);
            assert!(read().is_none());
        }
        later["payload"]["type"] = serde_json::json!("task_complete");
        write_capacity_records(&path, &[records.clone(), vec![later]].concat());
        assert!(read().is_none());
        write_capacity_records(&path, &records);
        let complete = fs::read(&path).unwrap();
        fs::write(&path, &complete[..complete.len() - 1]).unwrap();
        assert!(read().is_none());
        fs::write(&path, [complete.as_slice(), b"{broken}\n"].concat()).unwrap();
        assert!(read().is_none());
        let mut huge_meta = records.clone();
        huge_meta[0]["padding"] = serde_json::json!("x".repeat(CAPACITY_RECORD_BYTES));
        write_capacity_records(&path, &huge_meta);
        assert!(read().is_none());
        let huge =
            serde_json::json!({"type":"response_item","payload":"x".repeat(CAPACITY_RECORD_BYTES)});
        write_capacity_records(&path, &[records.clone(), vec![huge]].concat());
        assert!(read().is_none());
        let padding =
            vec![serde_json::json!({"type":"response_item","payload":"x".repeat(128 * 1024)}); 9];
        write_capacity_records(
            &path,
            &[
                records[..1].to_vec(),
                padding.clone(),
                records[1..].to_vec(),
            ]
            .concat(),
        );
        assert!(
            read().is_some(),
            "a clipped unrelated prefix must not hide the complete current turn"
        );
        write_capacity_records(
            &path,
            &[records[..2].to_vec(), padding, records[2..].to_vec()].concat(),
        );
        assert!(read().is_none(), "start outside the tail is not inferred");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_capacity_reader_rejects_nonregular_paths_and_changed_file_identity() {
        let (root, path, records) = capacity_fixture();
        let sessions = root.join("sessions");
        write_capacity_records(&path, &records);
        write_capacity_records(&root.join("outside.jsonl"), &records);
        for bad in [
            root.join("outside.jsonl"),
            sessions.join("day"),
            root.join("sessions/../sessions/day/rollout.jsonl"),
            root.join("sessions/./day/rollout.jsonl"),
            root.join("sessions/day/missing"),
            root.join("sessions/day/nul\0"),
        ] {
            assert!(
                read_capacity_history(&sessions, &bad, CAPACITY_NATIVE, CAPACITY_TURN, &root)
                    .is_none()
            );
        }
        let link = sessions.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(
            read_capacity_history(&sessions, &link, CAPACITY_NATIVE, CAPACITY_TURN, &root)
                .is_none()
        );
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(sessions.join("day"), &link).unwrap();
        assert!(read_capacity_history(
            &sessions,
            &link.join("rollout.jsonl"),
            CAPACITY_NATIVE,
            CAPACITY_TURN,
            &root
        )
        .is_none());
        let root_link = root.join("root-link");
        std::os::unix::fs::symlink(&sessions, &root_link).unwrap();
        assert!(read_capacity_history(
            &root_link,
            &root_link.join("day/rollout.jsonl"),
            CAPACITY_NATIVE,
            CAPACITY_TURN,
            &root
        )
        .is_none());
        for mutation in ["truncate", "replace", "mtime"] {
            write_capacity_records(&path, &records);
            let (parent, name) = capacity_history_parent(&sessions, &path).unwrap();
            let file = capacity_openat(&parent, &name, 0).unwrap();
            let before = CapacityFileIdentity::from_metadata(&file.metadata().unwrap()).unwrap();
            match mutation {
                "replace" => {
                    fs::rename(&path, sessions.join("old.jsonl")).unwrap();
                    write_capacity_records(&path, &records);
                }
                "mtime" => file
                    .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
                    .unwrap(),
                _ => fs::write(&path, b"truncated\n").unwrap(),
            }
            assert!(capacity_history_unchanged(&sessions, &path, &file, &before).is_none());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_executor_is_rejected_without_reading_or_modifying_it() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-environments-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        require_local_executor_home(&root).unwrap();
        let path = root.join("environments.toml");
        let content = b"default = 'external'\n[[environments]]\nid = 'external'\nurl = 'ws://example.invalid'\n";
        fs::write(&path, content).unwrap();
        assert!(require_local_executor_home(&root)
            .unwrap_err()
            .to_string()
            .contains("environments.toml"));
        assert_eq!(fs::read(&path).unwrap(), content);
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), &path).unwrap();
        assert!(require_local_executor_home(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}

fn parse_config(path: &Path, label: &str) -> Result<Option<toml::Value>> {
    let Some(bytes) = read_bounded(path, MAX_CONFIG_BYTES, label)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("Codex {label} configuration is not valid UTF-8"))?;
    text.parse::<toml::Value>()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("Codex {label} configuration is malformed TOML"))
}

fn validate_local_config(value: &toml::Value, label: &str) -> Result<()> {
    let table = value
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("Codex {label} configuration must be a TOML table"))?;
    if table.contains_key("profile") {
        bail!("Codex {label} configuration selects a profile; profiles are unsupported by this v1 adapter")
    }
    if let Some(store) = table.get("cli_auth_credentials_store") {
        if store.as_str() != Some("file") {
            bail!("Codex {label} configuration uses an unsupported credential store; v1 requires file")
        }
    }
    if let Some(features) = table.get("features") {
        let features = features.as_table().ok_or_else(|| {
            anyhow::anyhow!("Codex {label} configuration has a non-table features value")
        })?;
        for feature in DISABLED_FEATURES {
            if features.get(*feature).is_some_and(|value| {
                if *feature == "memories" {
                    value.as_bool().is_none()
                } else {
                    value.as_bool() != Some(false)
                }
            }) {
                bail!("Codex {label} configuration enables an excluded feature: {feature}")
            }
        }
    }
    for key in [
        "app_server",
        "app_server_endpoint",
        "remote_executor",
        "executor",
        "capability_roots",
        "selected_capability_roots",
    ] {
        if table.contains_key(key) {
            bail!("Codex {label} configuration contains unsupported app-server, executor, or capability-root selection")
        }
    }
    Ok(())
}

fn mcp_names(value: &toml::Value, label: &str) -> Result<BTreeSet<String>> {
    let Some(servers) = value.get("mcp_servers") else {
        return Ok(BTreeSet::new());
    };
    let table = servers.as_table().ok_or_else(|| {
        anyhow::anyhow!("Codex {label} configuration has a non-table mcp_servers value")
    })?;
    for (name, server) in table {
        if name.is_empty() {
            bail!("Codex {label} configuration has an empty MCP server name")
        }
        if !server.is_table() {
            bail!("Codex {label} configuration has a non-table MCP server definition")
        }
    }
    Ok(table.keys().cloned().collect())
}

fn mcp_servers_override(names: &BTreeSet<String>) -> Result<String> {
    let mut servers = toml::map::Map::new();
    for name in names {
        let mut disabled = toml::map::Map::new();
        disabled.insert("enabled".to_owned(), toml::Value::Boolean(false));
        servers.insert(name.clone(), toml::Value::Table(disabled));
    }
    let value = toml::Value::Table(servers);
    let rendered = value.to_string();
    let roundtrip = format!("mcp_servers={rendered}")
        .parse::<toml::Value>()
        .ok()
        .and_then(|document| document.get("mcp_servers").cloned());
    if roundtrip.as_ref() != Some(&value) {
        bail!("MCP server disable table could not be represented as a TOML inline table")
    }
    Ok(format!("mcp_servers={rendered}"))
}

fn classify_file_auth(path: &Path) -> Result<()> {
    let bytes = read_bounded(path, MAX_AUTH_BYTES, "auth")?.ok_or_else(|| {
        anyhow::anyhow!("Codex native setup requires existing file-backed ChatGPT credentials")
    })?;
    let auth: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Codex auth.json is malformed or unsupported"))?;
    let object = auth
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Codex auth.json has an unsupported shape"))?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "auth_mode" | "OPENAI_API_KEY" | "tokens" | "last_refresh"
        )
    }) {
        bail!("Codex auth.json contains an unknown authentication form")
    }
    if object.get("auth_mode").and_then(serde_json::Value::as_str) != Some("chatgpt") {
        bail!("Codex auth.json must use ordinary ChatGPT authentication")
    }
    if object
        .get("OPENAI_API_KEY")
        .is_some_and(|value| !value.is_null())
    {
        bail!("Codex auth.json contains an API key; API-key fallback is not supported")
    }
    for key in [
        "external_token_provider",
        "token_provider",
        "auth_provider",
        "keyring",
    ] {
        if object.contains_key(key) {
            bail!("Codex auth.json uses an external or alternate credential provider")
        }
    }
    let tokens = object
        .get("tokens")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Codex auth.json has no supported ChatGPT token set"))?;
    if tokens.keys().any(|key| {
        !matches!(
            key.as_str(),
            "id_token" | "access_token" | "refresh_token" | "account_id"
        )
    }) {
        bail!("Codex auth.json contains an unknown token form")
    }
    let id_token = bounded_token(tokens, "id_token")?;
    let access_token = bounded_token(tokens, "access_token")?;
    let id_claims = jwt_claims(id_token, "ID")?;
    let plan = id_claims
        .get("https://api.openai.com/auth")
        .and_then(serde_json::Value::as_object)
        .and_then(|claims| claims.get("chatgpt_plan_type"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            id_claims
                .get("chatgpt_plan_type")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_ascii_lowercase);
    if !matches!(plan.as_deref(), Some("free" | "plus" | "pro")) {
        bail!("Codex ChatGPT credentials are managed, workspace-scoped, or use an unknown plan classification")
    }
    let access_claims = jwt_claims(access_token, "access")?;
    let expires = access_claims
        .get("exp")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("Codex access token has no parseable expiration"))?;
    let required = chrono::Utc::now()
        .timestamp()
        .checked_add(ACCESS_TOKEN_MARGIN_SECONDS)
        .ok_or_else(|| anyhow::anyhow!("Codex access-token time check overflowed"))?;
    if expires <= required {
        bail!(
            "Codex access token is expired or inside the native refresh window plus safety margin"
        )
    }
    Ok(())
}

fn bounded_token<'a>(
    tokens: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str> {
    tokens
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
        .ok_or_else(|| anyhow::anyhow!("Codex auth.json has a missing or invalid {key}"))
}

fn jwt_claims(token: &str, label: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut parts = token.split('.');
    let (_header, payload, signature) = (parts.next(), parts.next(), parts.next());
    if _header.is_none() || payload.is_none() || signature.is_none() || parts.next().is_some() {
        bail!("Codex {label} token is malformed")
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.unwrap())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload.unwrap()))
        .map_err(|_| anyhow::anyhow!("Codex {label} token is malformed"))?;
    if decoded.len() > 64 * 1024 {
        bail!("Codex {label} token claims exceed the supported bound")
    }
    serde_json::from_slice::<serde_json::Value>(&decoded)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| anyhow::anyhow!("Codex {label} token claims are malformed"))
}

fn read_bounded(path: &Path, maximum: u64, label: &str) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("Codex {label} source is unreadable"),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        bail!("Codex {label} source is not a bounded regular file")
    }
    let file =
        fs::File::open(path).map_err(|_| anyhow::anyhow!("Codex {label} source is unreadable"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Codex {label} source is unreadable"))?;
    if bytes.len() as u64 > maximum {
        bail!("Codex {label} source exceeds the supported bound")
    }
    Ok(Some(bytes))
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => bail!("Codex compatibility source presence could not be determined"),
    }
}

#[cfg(target_os = "macos")]
fn managed_preferences_present() -> Result<bool> {
    use std::ffi::{c_void, CString};
    use std::os::raw::c_char;

    type CFStringRef = *const c_void;
    type CFPropertyListRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFPreferencesCopyAppValue(
            key: CFStringRef,
            application_id: CFStringRef,
        ) -> CFPropertyListRef;
        fn CFRelease(value: *const c_void);
    }
    unsafe fn cf_string(value: &str) -> Result<CFStringRef> {
        let value = CString::new(value).map_err(|_| anyhow::anyhow!("invalid preference key"))?;
        let string = unsafe { CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), UTF8) };
        if string.is_null() {
            bail!("Codex managed preference presence could not be determined")
        }
        Ok(string)
    }
    let app = unsafe { cf_string("com.openai.codex")? };
    let mut present = false;
    for name in ["config_toml_base64", "requirements_toml_base64"] {
        let key = unsafe { cf_string(name)? };
        let value = unsafe { CFPreferencesCopyAppValue(key, app) };
        unsafe { CFRelease(key) };
        if !value.is_null() {
            present = true;
            unsafe { CFRelease(value) };
        }
    }
    unsafe { CFRelease(app) };
    Ok(present)
}

#[cfg(not(target_os = "macos"))]
fn managed_preferences_present() -> Result<bool> {
    Ok(false)
}

fn augmented_prompt(prompt: &str, role: RoleKind, executable: &Path, socket: &Path) -> String {
    format!(
        "{prompt}\n\n{}",
        super::role_channel_instructions(role, executable, socket)
    )
}

fn environment(
    provider: Provider,
    socket: &Path,
    token: &str,
    generation: &str,
    session: &str,
) -> Vec<(String, String)> {
    vec![
        ("AGENTICJIRA_PROVIDER".to_owned(), provider.to_string()),
        (
            "AGENTICJIRA_ROLE_SOCKET".to_owned(),
            socket.to_string_lossy().into_owned(),
        ),
        ("AGENTICJIRA_ROLE_TOKEN".to_owned(), token.to_owned()),
        (
            "AGENTICJIRA_ROLE_GENERATION_ID".to_owned(),
            generation.to_owned(),
        ),
        ("AGENTICJIRA_SESSION_ID".to_owned(), session.to_owned()),
    ]
}
