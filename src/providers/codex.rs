use super::{shell_quote, toml_string, HookAssets, PreparedLaunch, HOOK_REVISION};
use crate::domain::{CapabilityIdentity, CapabilityStatus, LaunchConfig, Provider, RoleKind};
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const IMPLEMENTER_NATIVE_POLICY_STATUS: &str = "unknown_unattested";
pub const IMPLEMENTER_NATIVE_POLICY_REASON: &str = "Codex Implementer requires exact current native validation, including a delivered PermissionRequest decision; native approvals may be reused without an inbox request and app revocation affects only app-owned rules";
pub const DENIED_READ_FLOOR_VERSION: &str = "codex-denied-read-floor-v2";
pub const DENIED_READ_FLOOR_PROFILE: &str = "agenticjira_role";
pub const LEGACY_DENIED_READ_FLOOR_GAP: &str = "historical Codex configuration predates the canonical control-socket denied-read and restricted-proxy floor; fresh native validation is required";
pub const MCP_COVERAGE_REVISION: &str = "codex-local-mcp-coverage-v1-0.155.1";
pub const MCP_COVERAGE_CLASS: &str = "personal_ineligible_observed_prelaunch";
pub const APPROVAL_OWNERSHIP_REVISION: &str = "codex-native-approval-ownership-v1";
pub const LEGACY_MCP_COVERAGE_GAP: &str = "historical Codex configuration predates exact local MCP-source coverage; fresh native validation is required";
pub const LEGACY_APPROVAL_OWNERSHIP_GAP: &str = "historical Codex Implementer configuration predates native approval ownership; fresh native validation is required";
const EXACT_CODEX_VERSION: &str = "codex-cli 0.155.1";
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
    let (executable, version) = super::executable_and_version(Provider::Codex)?;
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
            hook_revision: format!("{HOOK_REVISION}:{}", assets.codex_revision_hash),
            capability_status: CapabilityStatus::Unverified,
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
    let expected_permission_profile = serde_json::json!({
        "name": DENIED_READ_FLOOR_PROFILE,
        "extends": expected_base,
        "filesystem": {"deny": denied_paths.clone()},
    });
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
    let filesystem = denied
        .iter()
        .map(|path| {
            Ok(format!(
                "{}=\"deny\"",
                toml_string(&path.to_string_lossy())?
            ))
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
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
