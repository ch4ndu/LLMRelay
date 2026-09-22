use crate::config::{atomic_write, InstancePaths};
use crate::diagnostics::{sanitize_value, DiagnosticSink};
use crate::store::Store;
use anyhow::{bail, Result};
use chrono::Utc;
use std::path::Path;

pub fn export_sanitized(paths: &InstancePaths, output: &Path) -> Result<serde_json::Value> {
    let store = Store::open(&paths.database)?;
    let state = sanitize_state(serde_json::to_value(crate::workflow::state(&store)?)?);
    let diagnostics = sanitize_value(serde_json::Value::Array(
        DiagnosticSink::new(paths.logs.clone())?.read_sanitized(2_000)?,
    ));
    let manifest = serde_json::json!({
        "schema":1,
        "created_at":Utc::now().to_rfc3339(),
        "version":crate::VERSION,
        "format":"zip-store",
        "included":["manifest.json","state.json","diagnostics.json"],
        "excluded":["raw SQLite database","role/browser credentials","native provider histories","transcript payloads","repository source","raw hook and permission request payload bodies","permission command-family scope paths","runtime sockets"],
        "sanitization":"credential-shaped keys and plaintext authorization/cookie/password/secret/token/bearer values are redacted; user-authored freeform state, repository paths, provider launch arguments, permission inputs/access/scope previews, and command payloads are omitted",
        "limits":{"diagnostic_events":2000}
    });
    let entries = vec![
        ("manifest.json", serde_json::to_vec_pretty(&manifest)?),
        ("state.json", serde_json::to_vec_pretty(&state)?),
        ("diagnostics.json", serde_json::to_vec_pretty(&diagnostics)?),
    ];
    let archive = stored_zip(&entries)?;
    atomic_write(output, &archive)?;
    Ok(serde_json::json!({"output":output,"bytes":archive.len(),"manifest":manifest}))
}

fn sanitize_state(value: serde_json::Value) -> serde_json::Value {
    fn omit(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(entries) => serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        let omitted = [
                            "acceptance_criteria",
                            "arguments",
                            "actual_outcomes",
                            "body",
                            "capability_identity",
                            "command",
                            "command_display",
                            "content",
                            "description",
                            "decision_reason",
                            "detail",
                            "display_name",
                            "display_family",
                            "evidence",
                            "error",
                            "executable",
                            "family_preview",
                            "family_unavailable_reason",
                            "feedback",
                            "failure_reason",
                            "fixture_root",
                            "gaps",
                            "handoff",
                            "launch",
                            "launch_error",
                            "legacy",
                            "manifest",
                            "model_evidence",
                            "native_session_id",
                            "nonce",
                            "payload",
                            "preimage_content",
                            "plan",
                            "proposal",
                            "input",
                            "reason",
                            "requested_access",
                            "repository_identity",
                            "repository_path",
                            "repository_root",
                            "shell",
                            "shell_command",
                            "role_overrides",
                            "scope",
                            "settings",
                            "entries",
                            "canonical_root",
                            "alternate_roots",
                            "partial_paths",
                            "conflicts",
                            "relative_path",
                            "path",
                            "staged_path",
                            "service_sentinel_path",
                            "control_socket_path",
                            "control_socket",
                            "fixture_repository_identity",
                            "permission_policy",
                            "role_socket",
                            "workspace_path",
                            "title",
                            "original_text",
                            "cwd",
                        ]
                        .contains(&key.as_str());
                        let value = if omitted {
                            serde_json::Value::String("[OMITTED FROM SANITIZED EXPORT]".to_owned())
                        } else if key == "hashes" {
                            match value {
                                serde_json::Value::Object(hashes) => serde_json::Value::Array(
                                    hashes.into_values().map(omit).collect(),
                                ),
                                value => omit(value),
                            }
                        } else {
                            omit(value)
                        };
                        (key, value)
                    })
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(omit).collect())
            }
            other => other,
        }
    }
    omit(sanitize_value(value))
}

pub fn offline_logs(paths: &InstancePaths, lines: usize) -> Result<Vec<serde_json::Value>> {
    DiagnosticSink::new(paths.logs.clone())?.read_sanitized(lines)
}

fn stored_zip(entries: &[(&str, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        if name.len() > u16::MAX as usize
            || data.len() > u32::MAX as usize
            || output.len() > u32::MAX as usize
        {
            bail!("sanitized export exceeds ZIP32 limits")
        }
        let offset = output.len() as u32;
        let crc = crc32(data);
        push_u32(&mut output, 0x04034b50);
        push_u16(&mut output, 20);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0x21);
        push_u32(&mut output, crc);
        push_u32(&mut output, data.len() as u32);
        push_u32(&mut output, data.len() as u32);
        push_u16(&mut output, name.len() as u16);
        push_u16(&mut output, 0);
        output.extend_from_slice(name.as_bytes());
        output.extend_from_slice(data);

        push_u32(&mut central, 0x02014b50);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0x21);
        push_u32(&mut central, crc);
        push_u32(&mut central, data.len() as u32);
        push_u32(&mut central, data.len() as u32);
        push_u16(&mut central, name.len() as u16);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0);
        push_u32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = output.len() as u32;
    let central_size = central.len() as u32;
    output.extend_from_slice(&central);
    push_u32(&mut output, 0x06054b50);
    push_u16(&mut output, 0);
    push_u16(&mut output, 0);
    push_u16(&mut output, entries.len() as u16);
    push_u16(&mut output, entries.len() as u16);
    push_u32(&mut output, central_size);
    push_u32(&mut output, central_offset);
    push_u16(&mut output, 0);
    Ok(output)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320_u32 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}
