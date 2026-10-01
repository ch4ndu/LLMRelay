use super::{HookAssets, PreparedLaunch, RuntimeProbeCommandPolicy, CLAUDE_HOOK_REVISION};
use crate::domain::{CapabilityIdentity, CapabilityStatus, LaunchConfig, Provider, RoleKind};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

pub const NATIVE_SANDBOX_POLICY_REVISION: &str = "claude-native-sandbox-role-socket-v1";
pub const LAUNCH_CONTRACT_REVISION: &str = "llmrelay-claude-launch-v2";
pub const RESUME_CONTRACT_REVISION: &str = "llmrelay-claude-resume-v2";
pub const CREDENTIAL_CONTRACT_REVISION: &str = "llmrelay-local-credential-v1";

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
    prepare_with_runtime_probe_policy(
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
    )
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_with_runtime_probe_policy(
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
        runtime_probe_policy,
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
    runtime_probe_policy: Option<&RuntimeProbeCommandPolicy>,
    bundles: &crate::provider_compatibility::BundleSet,
) -> Result<PreparedLaunch> {
    let (executable, version) = if bundles.is_synthetic() {
        // Synthetic bundle preparation stays inside the fixture and never inspects or launches Claude.
        (executable_path.to_path_buf(), "synthetic-claude-v1".into())
    } else {
        super::executable_and_version(Provider::Claude)?
    };
    let contract_binding = bundles.resolve(Provider::Claude, &version, role)?;
    let report_prefix = format!("Bash({} role report:*)", executable_path.display());
    let context_command = format!("Bash({} role context)", executable_path.display());
    let readonly = role != RoleKind::Implementer;
    let tools = if readonly {
        "Read,Grep,Glob,Bash"
    } else {
        "Read,Grep,Glob,Edit,Write,Bash"
    };
    let mut denied = if readonly {
        "Edit,Write,NotebookEdit,WebFetch,WebSearch,Task,Agent"
    } else {
        "NotebookEdit,WebFetch,WebSearch,Task,Agent"
    }
    .to_owned();
    for path in read_denials {
        let relative = path.strip_prefix("/").unwrap_or(path);
        let rule_path = relative.to_string_lossy();
        if rule_path
            .chars()
            .any(|character| matches!(character, ',' | '(' | ')' | '*' | '?' | '\n' | '\r'))
        {
            bail!(
                "Claude target read denial cannot safely represent path {}",
                path.display()
            )
        }
        denied.push_str(&format!(
            ",Read(//{}/**),Grep(//{}/**),Glob(//{}/**)",
            rule_path, rule_path, rule_path,
        ));
    }
    let settings = if readonly {
        &assets.claude_settings
    } else {
        &assets.claude_implementer_settings
    };
    let hook_hash = if readonly {
        &assets.claude_revision_hash
    } else {
        &assets.claude_implementer_revision_hash
    };
    if let Some(policy) = runtime_probe_policy {
        validate_runtime_probe_policy(policy)?;
        if !read_denials.is_empty() {
            bail!("runtime probe launch cannot combine diagnostic control-socket denial with setup target read denials")
        }
    }
    let canonical_role_socket = canonical_role_socket(role_socket)?;
    let effective_read_denials = runtime_probe_policy
        .map(|policy| policy.read_denials.as_slice())
        .unwrap_or(read_denials);
    let write_denials = runtime_probe_policy.map(|policy| policy.write_denials.as_slice());
    let settings_argument = native_sandbox_settings_argument(
        settings,
        &canonical_role_socket,
        effective_read_denials,
        write_denials,
    )?;
    let relative = cwd.strip_prefix("/").unwrap_or(cwd);
    let mut allowed = vec![
        format!("Read(//{}/**)", relative.display()),
        format!("Grep(//{}/**)", relative.display()),
        format!("Glob(//{}/**)", relative.display()),
        context_command,
        report_prefix,
    ];
    if role == RoleKind::Manager {
        for command in [
            "propose-transition",
            "acknowledge-guidance",
            "record-explorer-decision",
            "configure-lanes",
            "request-integration",
            "select-checks",
            "submit-conformance",
        ] {
            allowed.push(format!(
                "Bash({} role {command}:*)",
                executable_path.display()
            ));
        }
        if !read_denials.is_empty() {
            allowed.push(format!(
                "Bash({} role setup-read:*)",
                executable_path.display()
            ));
        }
    }
    if role == RoleKind::Implementer {
        allowed.push(format!("Edit(//{}/**)", relative.display()));
        allowed.push(format!(
            "Bash({} role yield-lane:*)",
            executable_path.display()
        ));
    }
    if let Some(policy) = runtime_probe_policy {
        for command in &policy.commands {
            allowed.push(format!("Bash({})", command.command));
        }
    }
    let mut arguments = vec![
        "--model".to_owned(),
        model.to_owned(),
        "--effort".to_owned(),
        effort.to_owned(),
        "--restricted".to_owned(),
        "--permission-mode".to_owned(),
        if role == RoleKind::Implementer {
            "default".to_owned()
        } else {
            "dontAsk".to_owned()
        },
        "--tools".to_owned(),
        tools.to_owned(),
        "--allowedTools".to_owned(),
    ];
    arguments.extend(allowed.clone());
    arguments.extend([
        "--disallowedTools".to_owned(),
        denied.clone(),
        "--settings".to_owned(),
        settings_argument,
        "--strict-mcp-config".to_owned(),
        "--disable-slash-commands".to_owned(),
        "--no-chrome".to_owned(),
        "--prompt-suggestions".to_owned(),
        "false".to_owned(),
    ]);
    if let Some(native_id) = native_session_id {
        arguments.extend(["--resume".to_owned(), native_id.to_owned()]);
    } else {
        arguments.extend(["--session-id".to_owned(), session_id.to_owned()]);
    }
    arguments.push(augmented_prompt(
        prompt,
        role,
        executable_path,
        &canonical_role_socket,
    ));
    let environment = vec![
        ("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned()),
        ("DISABLE_UPDATES".to_owned(), "1".to_owned()),
        (
            "AGENTICJIRA_PROVIDER".to_owned(),
            Provider::Claude.to_string(),
        ),
        (
            "AGENTICJIRA_ROLE_SOCKET".to_owned(),
            canonical_role_socket.to_string_lossy().into_owned(),
        ),
        ("AGENTICJIRA_ROLE_TOKEN".to_owned(), role_token.to_owned()),
        (
            "AGENTICJIRA_ROLE_GENERATION_ID".to_owned(),
            role_generation_id.to_owned(),
        ),
        ("AGENTICJIRA_SESSION_ID".to_owned(), session_id.to_owned()),
    ];
    let runtime_policy_description = runtime_probe_policy
        .map(|policy| {
            format!(
            "; exact frozen runtime Bash commands for operations {}; sandbox denyRead roots {}; sandbox denyWrite roots {}",
            policy
                .commands
                .iter()
                .map(|command| command.operation.as_str())
                .collect::<Vec<_>>()
                .join(","),
            policy
                .read_denials
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(","),
            policy
                .write_denials
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
        })
        .unwrap_or_default();
    let permission_policy = format!("claude --restricted + {}; tools {tools}, exact role-specific AgenticJira commands, denied {denied}; native settings require the sandbox, fail closed if unavailable, refuse unsandboxed commands, and allow only the canonical authenticated role socket{runtime_policy_description}; managed child and settings pin DISABLE_AUTOUPDATER=1 and DISABLE_UPDATES=1; base settings {}; Implementer PermissionRequest decisions are human-only through AgenticJira; no permission bypass",if readonly{"dontAsk"}else{"default"},settings.display());
    let mut security_policy = serde_json::json!({"restricted":true,"permission_mode":if readonly{"dontAsk"}else{"default"},"permission_request_timeout_seconds":if readonly{None}else{Some(crate::permissions::PROVIDER_PERMISSION_TIMEOUT_SECONDS)},"tools":tools,"allowed_operations":allowed,"denied":denied,"mcp":"strict_empty","chrome":false,"slash_commands":false,"prompt_suggestions":false,"settings_kind":if readonly{"reviewer"}else{"implementer"},"hooks":"native_explicit_trust","update_environment":{"DISABLE_AUTOUPDATER":"1","DISABLE_UPDATES":"1"},"update_scope":"managed_claude_child_only"});
    security_policy["native_sandbox"] = serde_json::json!({
        "policy_revision": NATIVE_SANDBOX_POLICY_REVISION,
        "enabled": true,
        "fail_if_unavailable": true,
        "auto_allow_bash_if_sandboxed": false,
        "allow_unsandboxed_commands": false,
        "filesystem_deny_read": effective_read_denials,
        "network_allow_unix_sockets": [&canonical_role_socket],
        "network_allow_all_unix_sockets": false,
        "configured_via": "native_claude_inline_settings",
        "native_validation": "required"
    });
    if let Some(write_denials) = write_denials {
        security_policy["native_sandbox"]["filesystem_deny_write"] =
            serde_json::json!(write_denials);
    }
    if !read_denials.is_empty() {
        security_policy["setup_target_read_denials"] = serde_json::json!(read_denials);
    }
    if let Some(policy) = runtime_probe_policy {
        security_policy["runtime_probe_command_policy"] = serde_json::to_value(policy)?;
    }
    Ok(PreparedLaunch {
        config: LaunchConfig {
            provider: Provider::Claude,
            role,
            executable: executable.clone(),
            executable_version: version,
            model: model.to_owned(),
            effort: effort.to_owned(),
            cwd: cwd.to_path_buf(),
            argv: arguments.clone(),
            environment_keys: environment.iter().map(|(key, _)| key.clone()).collect(),
            permission_policy,
            security_policy,
            hook_revision: format!("{CLAUDE_HOOK_REVISION}:{hook_hash}"),
            capability_status: CapabilityStatus::Unverified,
            compatibility: Some(contract_binding),
        },
        executable,
        arguments,
        environment,
        supervision_executable: executable_path.to_path_buf(),
    })
}

fn native_sandbox_settings_argument(
    settings: &Path,
    role_socket: &Path,
    read_denials: &[PathBuf],
    write_denials: Option<&[PathBuf]>,
) -> Result<String> {
    let bytes = std::fs::read(settings)
        .with_context(|| format!("read Claude settings {}", settings.display()))?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse Claude settings {}", settings.display()))?;
    let root = value
        .as_object_mut()
        .context("Claude settings must be a JSON object")?;
    let sandbox = root
        .entry("sandbox")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("Claude sandbox settings must be a JSON object")?;
    sandbox.insert("enabled".to_owned(), serde_json::Value::Bool(true));
    sandbox.insert(
        "failIfUnavailable".to_owned(),
        serde_json::Value::Bool(true),
    );
    sandbox.insert(
        "autoAllowBashIfSandboxed".to_owned(),
        serde_json::Value::Bool(false),
    );
    sandbox.insert(
        "allowUnsandboxedCommands".to_owned(),
        serde_json::Value::Bool(false),
    );
    let network = sandbox
        .entry("network")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("Claude sandbox network settings must be a JSON object")?;
    network.insert(
        "allowUnixSockets".to_owned(),
        serde_json::json!([role_socket]),
    );
    network.insert(
        "allowAllUnixSockets".to_owned(),
        serde_json::Value::Bool(false),
    );
    let filesystem = sandbox
        .entry("filesystem")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("Claude sandbox filesystem settings must be a JSON object")?;
    let denied = filesystem
        .entry("denyRead")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .context("Claude sandbox filesystem denyRead setting must be an array")?;
    for path in read_denials {
        let path = serde_json::Value::String(path.to_string_lossy().into_owned());
        if !denied.contains(&path) {
            denied.push(path);
        }
    }
    if let Some(write_denials) = write_denials {
        let denied_writes = filesystem
            .entry("denyWrite")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .context("Claude sandbox filesystem denyWrite setting must be an array")?;
        for path in write_denials {
            let path = serde_json::Value::String(path.to_string_lossy().into_owned());
            if !denied_writes.contains(&path) {
                denied_writes.push(path);
            }
        }
    }
    serde_json::to_string(&value).context("serialize native-sandboxed Claude settings")
}

fn validate_runtime_probe_policy(policy: &RuntimeProbeCommandPolicy) -> Result<()> {
    if policy.commands.is_empty()
        || policy.commands.len() > 8
        || policy.read_denials.len() != 1
        || policy.write_denials.is_empty()
    {
        bail!("Claude runtime probe policy requires bounded exact commands plus one denyRead and denyWrite roots")
    }
    let mut operations = std::collections::BTreeSet::new();
    let mut commands = std::collections::BTreeSet::new();
    for entry in &policy.commands {
        if entry.operation.is_empty()
            || !entry
                .operation
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            || !operations.insert(entry.operation.as_str())
            || entry.command.is_empty()
            || entry.command.len() > 16 * 1024
            || entry
                .command
                .chars()
                .any(|character| matches!(character, '\n' | '\r' | '*' | '?'))
            || !commands.insert(entry.command.as_str())
        {
            bail!("Claude runtime probe commands must be unique exact literal command strings")
        }
    }
    if policy.write_denials.iter().any(|path| {
        !path.is_absolute()
            && !path
                .to_string_lossy()
                .starts_with("<runtime-probe-write-denial:")
            && path.to_string_lossy() != "<attempt-worktree>"
    }) {
        bail!("Claude runtime probe denyWrite roots must be absolute")
    }
    if policy.read_denials.iter().any(|path| {
        !path.is_absolute()
            && !path
                .to_string_lossy()
                .starts_with("<runtime-probe-read-denial:")
    }) {
        bail!("Claude runtime probe denyRead roots must be absolute")
    }
    Ok(())
}

pub fn require_native_sandbox(
    identity: &CapabilityIdentity,
    expected_denials: &std::collections::BTreeSet<PathBuf>,
) -> Result<()> {
    if identity.provider != Provider::Claude {
        bail!("Claude native sandbox validation received a different provider")
    }
    let settings = identity
        .effective_argv
        .windows(2)
        .filter(|pair| pair[0] == "--settings")
        .map(|pair| pair[1].as_str())
        .collect::<Vec<_>>();
    if settings.len() != 1 || !settings[0].trim_start().starts_with('{') {
        bail!("Claude launch requires exactly one inline native-sandbox settings JSON argument")
    }
    let value: serde_json::Value =
        serde_json::from_str(settings[0]).context("parse effective inline Claude settings")?;
    for (pointer, expected) in [
        ("/sandbox/enabled", true),
        ("/sandbox/failIfUnavailable", true),
        ("/sandbox/autoAllowBashIfSandboxed", false),
        ("/sandbox/allowUnsandboxedCommands", false),
        ("/sandbox/network/allowAllUnixSockets", false),
    ] {
        if value.pointer(pointer).and_then(|item| item.as_bool()) != Some(expected) {
            bail!("Claude effective inline settings do not enforce {pointer}={expected}")
        }
    }
    let sockets = value
        .pointer("/sandbox/network/allowUnixSockets")
        .and_then(|item| item.as_array())
        .ok_or_else(|| anyhow::anyhow!("Claude effective inline settings lack allowUnixSockets"))?;
    if sockets.len() != 1 || sockets[0].as_str() != Some("<role-socket>") {
        bail!("Claude native sandbox must allow only the singleton canonical role.sock")
    }
    let denied = value
        .pointer("/sandbox/filesystem/denyRead")
        .and_then(|item| item.as_array())
        .ok_or_else(|| {
            anyhow::anyhow!("Claude effective inline settings lack filesystem denyRead")
        })?;
    let denied = denied
        .iter()
        .map(|item| item.as_str().map(PathBuf::from))
        .collect::<Option<std::collections::BTreeSet<_>>>()
        .ok_or_else(|| anyhow::anyhow!("Claude denyRead entries must be paths"))?;
    if !expected_denials.is_subset(&denied) {
        bail!("Claude effective inline settings omit an exact canonical target denyRead path")
    }
    if let Some(policy) = identity.security_policy.get("runtime_probe_command_policy") {
        let policy: RuntimeProbeCommandPolicy = serde_json::from_value(policy.clone())
            .context("Claude runtime probe command policy is malformed")?;
        validate_runtime_probe_policy(&policy)?;
        let expected_reads = policy
            .read_denials
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if !expected_reads.is_subset(&denied) {
            bail!("Claude runtime settings omit a frozen denyRead root")
        }
        let deny_writes = value
            .pointer("/sandbox/filesystem/denyWrite")
            .and_then(|item| item.as_array())
            .ok_or_else(|| anyhow::anyhow!("Claude runtime settings omit filesystem denyWrite"))?
            .iter()
            .map(|item| item.as_str().map(PathBuf::from))
            .collect::<Option<std::collections::BTreeSet<_>>>()
            .ok_or_else(|| anyhow::anyhow!("Claude runtime denyWrite entries must be paths"))?;
        let expected_writes = policy
            .write_denials
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if !expected_writes.is_subset(&deny_writes) {
            bail!("Claude runtime settings omit a frozen denyWrite root")
        }
        let allowed = identity
            .security_policy
            .get("allowed_operations")
            .and_then(|value| value.as_array())
            .ok_or_else(|| anyhow::anyhow!("Claude runtime policy omits allowed operations"))?;
        let allowed = allowed
            .iter()
            .map(|value| value.as_str())
            .collect::<Option<std::collections::BTreeSet<_>>>()
            .ok_or_else(|| anyhow::anyhow!("Claude runtime allowed operations are not strings"))?;
        for command in &policy.commands {
            if !allowed.contains(format!("Bash({})", command.command).as_str()) {
                bail!("Claude runtime policy omits a frozen exact Bash command")
            }
        }
    }
    let hooks = value
        .get("hooks")
        .and_then(|item| item.as_object())
        .ok_or_else(|| anyhow::anyhow!("Claude effective inline settings lost generated hooks"))?;
    let required_hooks = [
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
    ];
    let expected_mode = if identity.role == RoleKind::Implementer {
        "default"
    } else {
        "dontAsk"
    };
    let denied_tools = value
        .pointer("/permissions/deny")
        .and_then(|item| item.as_array())
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str())
                .collect::<Option<std::collections::BTreeSet<_>>>()
        })
        .ok_or_else(|| {
            anyhow::anyhow!("Claude effective inline settings lost native permission denials")
        })?;
    let required_denials: &[&str] = if identity.role == RoleKind::Implementer {
        &["NotebookEdit", "WebFetch", "WebSearch", "Task", "Agent"]
    } else {
        &[
            "Edit",
            "Write",
            "NotebookEdit",
            "WebFetch",
            "WebSearch",
            "Task",
            "Agent",
        ]
    };
    if value
        .pointer("/env/DISABLE_AUTOUPDATER")
        .and_then(|item| item.as_str())
        != Some("1")
        || value
            .pointer("/env/DISABLE_UPDATES")
            .and_then(|item| item.as_str())
            != Some("1")
        || value
            .pointer("/permissions/defaultMode")
            .and_then(|item| item.as_str())
            != Some(expected_mode)
        || required_hooks.iter().any(|event| {
            hooks
                .get(*event)
                .and_then(|item| item.as_array())
                .is_none_or(|entries| entries.is_empty())
        })
        || required_denials
            .iter()
            .any(|tool| !denied_tools.contains(*tool))
    {
        bail!("Claude effective inline settings lost generated hooks, environment, or permission ownership")
    }
    Ok(())
}

pub fn require_current_native_policy(identity: &CapabilityIdentity) -> Result<()> {
    require_native_sandbox(identity, &std::collections::BTreeSet::new())
}

fn canonical_role_socket(role_socket: &Path) -> Result<PathBuf> {
    if !role_socket.is_absolute()
        || role_socket.file_name().and_then(|name| name.to_str()) != Some("role.sock")
    {
        bail!("Claude role socket must be an absolute role.sock path")
    }
    if std::fs::symlink_metadata(role_socket)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        bail!("Claude role socket must not be a symlink")
    }
    let parent = role_socket
        .parent()
        .context("Claude role socket has no parent directory")?
        .canonicalize()
        .context("canonicalize Claude role socket directory")?;
    Ok(parent.join("role.sock"))
}

fn augmented_prompt(prompt: &str, role: RoleKind, executable: &Path, socket: &Path) -> String {
    format!(
        "{prompt}\n\n{}",
        super::role_channel_instructions(role, executable, socket)
    )
}

// Only this version was observed: pasted `/`, `@`, `!` or image paths trigger actions; this envelope does not.
const LITERAL_GUIDANCE_PREDICATE: &str = "claude-code-2.1.283";
const LITERAL_GUIDANCE_PREFIX: &str =
    "LLMRelay task guidance (JSON-encoded string; decode to read):\n";

pub(crate) fn literal_guidance_submission(
    compatibility: Option<&crate::provider_compatibility::AuthorityBinding>,
    guidance: &str,
) -> Result<String> {
    if !compatibility.is_some_and(|binding| {
        binding.provider == Provider::Claude
            && !binding.synthetic_origin
            && binding.predicate_id == LITERAL_GUIDANCE_PREDICATE
    }) {
        bail!("literal guidance delivery is qualified only for the admitted Claude Code 2.1.283 contract, and this session is not bound to it")
    }
    let encoded = serde_json::to_string(guidance)?
        .replace('@', "\\u0040")
        .replace('!', "\\u0021")
        .replace('/', "\\u002f");
    Ok(format!("{LITERAL_GUIDANCE_PREFIX}{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal_guidance_binding() -> crate::provider_compatibility::AuthorityBinding {
        crate::provider_compatibility::AuthorityBinding {
            synthetic_origin: false,
            provider: Provider::Claude,
            schema: 1,
            pack_id: "llmrelay-claude-compatibility".into(),
            predicate_id: LITERAL_GUIDANCE_PREDICATE.into(),
            exact_version: "2.1.283 (Claude Code)".into(),
            contract_id: "claude-manager".into(),
            contract_revision: "claude-role-contract-v1".into(),
            effective_hash: "fixture".into(),
            session_class: crate::provider_compatibility::SessionClass::Retained,
            required_evidence: Vec::new(),
        }
    }

    #[test]
    fn admitted_guidance_envelope_matches_the_observed_submission_and_decodes_verbatim() {
        let binding = literal_guidance_binding();
        let observed = "/hardening_literal_probe_20261001\n@envelope-only.txt\n!echo LLMRELAY_NO_SHELL_FROM_ENVELOPE\n./literal-valid.png";
        assert_eq!(
            literal_guidance_submission(Some(&binding), observed).unwrap(),
            "LLMRelay task guidance (JSON-encoded string; decode to read):\n\"\\u002fhardening_literal_probe_20261001\\n\\u0040envelope-only.txt\\n\\u0021echo LLMRELAY_NO_SHELL_FROM_ENVELOPE\\n.\\u002fliteral-valid.png\""
        );
        for original in [
            observed,
            "Reply exactly LLMRELAY_TYPEAHEAD_ACCEPTED. Do not use tools.",
            "quote \" backslash \\ tab\t carriage\r\n/compact @file !ls ./shot.png",
            "\u{1}control, é and 🚀 stay exact",
        ] {
            let submitted = literal_guidance_submission(Some(&binding), original).unwrap();
            let encoded = submitted.strip_prefix(LITERAL_GUIDANCE_PREFIX).unwrap();
            assert!(!encoded.contains(&['/', '@', '!'][..]), "{encoded}");
            assert_eq!(serde_json::from_str::<String>(encoded).unwrap(), original);
        }
    }

    #[test]
    fn unqualified_claude_bindings_never_receive_the_guidance_envelope() {
        let qualified = literal_guidance_binding();
        let synthetic = crate::provider_compatibility::AuthorityBinding {
            synthetic_origin: true,
            ..qualified.clone()
        };
        let other_version = crate::provider_compatibility::AuthorityBinding {
            predicate_id: "claude-code-2.1.284".into(),
            ..qualified.clone()
        };
        let other_provider = crate::provider_compatibility::AuthorityBinding {
            provider: Provider::Codex,
            ..qualified
        };
        for binding in [
            None,
            Some(&synthetic),
            Some(&other_version),
            Some(&other_provider),
        ] {
            assert!(literal_guidance_submission(binding, "/compact").is_err());
        }
    }

    #[test]
    fn synthetic_compatibility_preparation_preserves_claude_native_policy() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-claude-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = std::env::current_exe().unwrap();
        let paths = crate::config::InstancePaths::resolve(Some(root.join("app"))).unwrap();
        paths.create().unwrap();
        let assets =
            crate::providers::install_hook_assets(&paths, &std::env::current_exe().unwrap())
                .unwrap();
        let mut claude: serde_json::Value = serde_json::from_str(include_str!(
            "../../resources/provider-compatibility/claude.json"
        ))
        .unwrap();
        let codex: serde_json::Value = serde_json::from_str(include_str!(
            "../../resources/provider-compatibility/codex.json"
        ))
        .unwrap();
        let mut selector = codex["selectors"][0].clone();
        selector["predicate_id"] = "synthetic-claude-v1".into();
        selector["exact_version"] = "synthetic-claude-v1".into();
        for contract in selector["contracts"].as_array_mut().unwrap() {
            contract["contract_id"] =
                format!("synthetic-claude-{}", contract["role"].as_str().unwrap()).into();
            contract["native_policy_revision"] = NATIVE_SANDBOX_POLICY_REVISION.into();
            contract["launch_revision"] = LAUNCH_CONTRACT_REVISION.into();
            contract["resume_revision"] = RESUME_CONTRACT_REVISION.into();
            contract["hook_revision"] = CLAUDE_HOOK_REVISION.into();
        }
        claude["selectors"] = serde_json::json!([selector]);
        let bundles = crate::provider_compatibility::BundleSet::synthetic_for_tests(
            include_str!("../../resources/provider-compatibility/codex.json"),
            &claude.to_string(),
        );
        let prepared = prepare_with_bundles(
            RoleKind::Manager,
            "claude-opus-4-1",
            "high",
            &root,
            "synthetic preparation",
            &paths.role_socket,
            "fixture-token",
            "fixture-generation",
            "fixture-session",
            None,
            &assets,
            &std::env::current_exe().unwrap(),
            &[],
            None,
            &bundles,
        )
        .unwrap();
        assert_eq!(prepared.executable, executable);
        assert_eq!(prepared.config.argv, prepared.arguments);
        assert_eq!(prepared.config.executable_version, "synthetic-claude-v1");
        assert!(prepared.arguments.iter().any(|arg| arg == "--restricted"));
        assert!(prepared
            .arguments
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "dontAsk"]));
        assert!(prepared
            .environment
            .iter()
            .any(|pair| pair == &("AGENTICJIRA_ROLE_TOKEN".into(), "fixture-token".into())));
        assert!(prepared
            .environment
            .iter()
            .any(|pair| pair == &("DISABLE_UPDATES".into(), "1".into())));
        let command = prepared.command(&root.join("anchor"));
        assert!(command.get_argv().iter().any(|arg| arg == &executable));
        assert_eq!(
            command.get_env("AGENTICJIRA_ROLE_TOKEN"),
            Some(std::ffi::OsStr::new("fixture-token"))
        );
        assert_eq!(command.get_cwd(), Some(&root.as_os_str().to_owned()));
        let binding = prepared.config.compatibility.as_ref().unwrap();
        assert_eq!(binding.contract_id, "synthetic-claude-manager");
        assert!(binding.synthetic_origin);
        let identity = crate::providers::capability_identity(&prepared.config).unwrap();
        assert_eq!(
            identity.compatibility.as_ref().unwrap().effective_hash,
            binding.effective_hash
        );
        assert_eq!(identity.hook_revision, prepared.config.hook_revision);
        assert!(identity.hook_revision.starts_with(CLAUDE_HOOK_REVISION));
        assert_eq!(
            identity.environment_contract,
            prepared.config.environment_keys
        );
        assert!(identity
            .effective_argv
            .iter()
            .any(|arg| arg == "--settings"));
        assert_eq!(
            identity.security_policy["native_sandbox"]["policy_revision"],
            NATIVE_SANDBOX_POLICY_REVISION
        );
        crate::providers::require_current_capability_identity_with_bundles(
            &identity, &root, &bundles,
        )
        .unwrap();

        let mut corrupted = identity.clone();
        let settings = corrupted
            .effective_argv
            .iter_mut()
            .find(|arg| arg.starts_with("{\"env\""))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(settings).unwrap();
        value["sandbox"]["failIfUnavailable"] = false.into();
        *settings = value.to_string();
        let error = crate::providers::require_current_capability_identity_with_bundles(
            &corrupted, &root, &bundles,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("/sandbox/failIfUnavailable=true"));
    }
}
