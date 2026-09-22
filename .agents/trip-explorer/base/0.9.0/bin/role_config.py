#!/usr/bin/env python3
"""Validate provider-neutral TRIP role, adapter, and preflight configuration."""

from __future__ import annotations

from typing import Any


ROLE_REQUIREMENTS = {
    "explorer": ("read-only", "retained"),
    "plan_reviewer": ("read-only", "retained"),
    "implementer": ("workspace-write", "retained"),
    "code_reviewer": ("read-only", "retained"),
    "final_verifier": ("read-only", "fresh"),
}
ADAPTER_KINDS = ("native-agent", "builtin-cli", "custom-cli")
BUILTIN_CLIS = ("codex", "claude")
ALLOWED_ARGUMENT_PLACEHOLDERS = {
    "{authority}",
    "{completion_file}",
    "{effort}",
    "{model}",
    "{project}",
    "{prompt_file}",
    "{result_file}",
    "{service_tier}",
    "{session_id}",
}
SECRET_KEY_PARTS = ("api_key", "password", "secret", "token")


def _contains_secret(value: Any, path: str = "") -> list[str]:
    problems: list[str] = []
    if isinstance(value, dict):
        for key, item in value.items():
            child = f"{path}.{key}" if path else str(key)
            normalized = str(key).lower().replace("-", "_")
            if any(part in normalized for part in SECRET_KEY_PARTS):
                problems.append(f"secret-like field is forbidden: {child}")
            problems.extend(_contains_secret(item, child))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            problems.extend(_contains_secret(item, f"{path}[{index}]"))
    return problems


def _capabilities(adapter: dict[str, Any]) -> dict[str, bool]:
    value = adapter.get("capabilities")
    return value if isinstance(value, dict) else {}


def _argument_placeholders(argument: str) -> set[str]:
    found: set[str] = set()
    cursor = 0
    while cursor < len(argument):
        start = argument.find("{", cursor)
        if start < 0:
            break
        end = argument.find("}", start + 1)
        if end < 0:
            found.add(argument[start:])
            break
        found.add(argument[start : end + 1])
        cursor = end + 1
    return found


def validate_role_configuration(
    config: object,
    adapters: object,
    preflight: object,
) -> list[str]:
    problems: list[str] = []
    if not isinstance(config, dict):
        return ["config must be an object"]
    if not isinstance(adapters, dict):
        return ["adapters must be an object"]
    if not isinstance(preflight, dict):
        return ["preflight must be an object"]

    problems.extend(_contains_secret(config, "config"))
    problems.extend(_contains_secret(adapters, "adapters"))
    problems.extend(_contains_secret(preflight, "preflight"))

    adapter_map = adapters.get("adapters")
    if not isinstance(adapter_map, dict) or not adapter_map:
        problems.append("adapters.adapters must be a nonempty object")
        adapter_map = {}
    for adapter_id, value in adapter_map.items():
        if not isinstance(adapter_id, str) or not adapter_id:
            problems.append("adapter ids must be nonempty strings")
            continue
        if not isinstance(value, dict):
            problems.append(f"adapter must be an object: {adapter_id}")
            continue
        kind = value.get("kind")
        if kind not in ADAPTER_KINDS:
            problems.append(f"invalid adapter kind for {adapter_id}: {kind}")
        provider = value.get("provider")
        if not isinstance(provider, str) or not provider:
            problems.append(f"adapter provider is required: {adapter_id}")
        capabilities = _capabilities(value)
        for capability in ("fresh_session", "read_only", "resume", "workspace_write"):
            if not isinstance(capabilities.get(capability), bool):
                problems.append(f"adapter capability {capability} must be boolean: {adapter_id}")
        if kind == "builtin-cli":
            if value.get("builtin") not in BUILTIN_CLIS:
                problems.append(f"invalid builtin CLI adapter: {adapter_id}")
            if not isinstance(value.get("executable"), str) or not value.get("executable"):
                problems.append(f"builtin CLI executable is required: {adapter_id}")
        if kind == "custom-cli":
            if not isinstance(value.get("executable"), str) or not value.get("executable"):
                problems.append(f"custom CLI executable is required: {adapter_id}")
            invocation = value.get("invocation")
            arguments = invocation.get("arguments") if isinstance(invocation, dict) else None
            if not isinstance(arguments, list) or not arguments or not all(isinstance(item, str) for item in arguments):
                problems.append(f"custom CLI arguments must be a nonempty string array: {adapter_id}")
            else:
                for argument in arguments:
                    placeholders = _argument_placeholders(argument)
                    unknown = placeholders - ALLOWED_ARGUMENT_PLACEHOLDERS
                    if unknown:
                        problems.append(
                            f"custom CLI argument has unsupported placeholders for {adapter_id}: {sorted(unknown)}"
                        )
                    if any(value in argument for value in ("$(`", "$(", "`", "${")):
                        problems.append(f"custom CLI arguments must not use shell evaluation: {adapter_id}")

    roles = config.get("roles")
    profiles = config.get("profiles")
    if not isinstance(roles, dict):
        problems.append("config.roles must be an object")
        roles = {}
    if set(roles) != set(ROLE_REQUIREMENTS):
        problems.append(f"config.roles must contain exactly: {sorted(ROLE_REQUIREMENTS)}")
    if not isinstance(profiles, dict) or not profiles:
        problems.append("config.profiles must be a nonempty object")
        profiles = {}

    for role, requirement in ROLE_REQUIREMENTS.items():
        role_value = roles.get(role)
        profile_id = role_value.get("profile") if isinstance(role_value, dict) else None
        if not isinstance(profile_id, str) or profile_id not in profiles:
            problems.append(f"role {role} must reference an existing profile")
            continue
        profile = profiles[profile_id]
        if not isinstance(profile, dict):
            problems.append(f"profile must be an object: {profile_id}")
            continue
        adapter_id = profile.get("adapter")
        if not isinstance(adapter_id, str) or adapter_id not in adapter_map:
            problems.append(f"profile {profile_id} must reference an existing adapter")
            continue
        for field in ("provider", "model"):
            if not isinstance(profile.get(field), str) or not profile.get(field):
                problems.append(f"profile {profile_id} requires {field}")
        authority, session = requirement
        if profile.get("authority") != authority:
            problems.append(f"role {role} requires authority {authority}")
        if profile.get("session") != session:
            problems.append(f"role {role} requires session {session}")
        adapter = adapter_map[adapter_id]
        if not isinstance(adapter, dict):
            continue
        if profile.get("provider") != adapter.get("provider"):
            problems.append(
                f"profile {profile_id} provider must match adapter {adapter_id}"
            )
        capabilities = _capabilities(adapter)
        if authority == "read-only" and capabilities.get("read_only") is not True:
            problems.append(f"adapter {adapter_id} cannot enforce read-only for role {role}")
        if authority == "workspace-write" and capabilities.get("workspace_write") is not True:
            problems.append(f"adapter {adapter_id} cannot provide workspace-write for role {role}")
        if session == "fresh" and capabilities.get("fresh_session") is not True:
            problems.append(f"adapter {adapter_id} cannot start a fresh session for role {role}")
        if session == "retained" and capabilities.get("resume") is not True:
            problems.append(f"adapter {adapter_id} cannot resume retained role {role}")

    receipts = preflight.get("receipts")
    if not isinstance(receipts, list):
        problems.append("preflight.receipts must be an array")
        receipts = []
    passed_profiles: set[str] = set()
    for index, receipt in enumerate(receipts):
        if not isinstance(receipt, dict):
            problems.append(f"preflight receipt {index} must be an object")
            continue
        profile_ids = receipt.get("profile_ids")
        if not isinstance(profile_ids, list) or not all(isinstance(item, str) for item in profile_ids):
            problems.append(f"preflight receipt {index} requires profile_ids")
            continue
        if receipt.get("result") != "pass" or receipt.get("nonce_matched") is not True:
            problems.append(f"preflight receipt did not pass: {index}")
            continue
        for profile_id in profile_ids:
            profile = profiles.get(profile_id)
            if not isinstance(profile, dict):
                problems.append(f"preflight references unknown profile: {profile_id}")
                continue
            for field in ("adapter", "provider", "model", "authority", "session"):
                if receipt.get(field) != profile.get(field):
                    problems.append(f"preflight {field} mismatch for profile {profile_id}")
            if receipt.get("effort") != profile.get("effort"):
                problems.append(f"preflight effort mismatch for profile {profile_id}")
            if not isinstance(receipt.get("model_evidence"), str) or not receipt.get("model_evidence"):
                problems.append(f"preflight model evidence missing for profile {profile_id}")
            else:
                passed_profiles.add(profile_id)
    referenced_profiles = {
        value.get("profile") for value in roles.values() if isinstance(value, dict) and isinstance(value.get("profile"), str)
    }
    missing = referenced_profiles - passed_profiles
    if missing:
        problems.append(f"profiles missing successful preflight: {sorted(missing)}")
    return problems
