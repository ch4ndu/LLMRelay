use crate::config::{atomic_write, InstancePaths};
use crate::diagnostics::{sanitize_value, DiagnosticSink};
use crate::store::Store;
use anyhow::{bail, Result};
use chrono::Utc;
use std::path::Path;

pub fn export_sanitized(paths: &InstancePaths, output: &Path) -> Result<serde_json::Value> {
    crate::database::validate_live_database_family(paths)?;
    let store = Store::open_current_readonly(&paths.database)?;
    let state = sanitize_state(serde_json::to_value(crate::workflow::state(&store)?)?);
    let diagnostics =
        sanitize_diagnostics(DiagnosticSink::new(paths.logs.clone())?.read_sanitized(2_000)?);
    let manifest = serde_json::json!({
        "schema":1,
        "created_at":Utc::now().to_rfc3339(),
        "version":crate::VERSION,
        "format":"zip-store",
        "included":["manifest.json","state.json","diagnostics.json"],
        "excluded":["raw SQLite database","role/browser credentials","native provider histories","transcript payloads","repository source","raw hook and permission request payload bodies","permission command-family scope paths","runtime sockets"],
        "sanitization":"credential-shaped keys and plaintext authorization/cookie/password/secret/token/bearer values are redacted; user-authored freeform state, repository paths, provider launch arguments, permission inputs/access/scope previews, and command payloads are omitted; diagnostic details retain only selected correlation hashes and corrupt-record gap metadata",
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

fn sanitize_diagnostics(events: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::Value::Array(
        events
            .into_iter()
            .map(|event| {
                let mut exported = serde_json::Map::new();
                if let Some(schema) = event["schema"].as_u64() {
                    exported.insert("schema".into(), schema.into());
                }
                for key in [
                    "timestamp",
                    "severity",
                    "event_code",
                    "component",
                    "outcome",
                    "operation_id",
                ] {
                    if let Some(value) = event[key].as_str() {
                        exported.insert(key.to_owned(), value.into());
                    }
                }
                let mut detail = serde_json::Map::new();
                for key in [
                    "candidate_hash",
                    "manifest_hash",
                    "configuration_hash",
                    "scope_hash",
                ] {
                    if let Some(hash) = event["detail"][key].as_str().filter(|hash| {
                        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    }) {
                        detail.insert(key.to_owned(), hash.into());
                    }
                }
                if event["event_code"] == "diagnostics.corrupt_record" {
                    for key in ["byte_offset", "bytes"] {
                        if let Some(value) = event["detail"][key].as_u64() {
                            detail.insert(key.to_owned(), value.into());
                        }
                    }
                    if let Some(value) = event["detail"]["unterminated"].as_bool() {
                        detail.insert("unterminated".into(), value.into());
                    }
                }
                exported.insert("detail".into(), detail.into());
                sanitize_value(exported.into())
            })
            .collect(),
    )
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
                            "acceptance_coverage",
                            // These payloads accept authored extension fields. Their
                            // surrounding DTOs retain the diagnostic IDs and hashes.
                            "adapters",
                            "capsule",
                            "census",
                            "config",
                            "invalidation",
                            "limits",
                            "profile",
                            "proof",
                            "receipt",
                            "roles",
                            "verification",
                            "arguments",
                            "actual_outcomes",
                            "body",
                            "capability_identity",
                            "command",
                            "command_display",
                            "content",
                            "criterion",
                            "description",
                            "documentation_policy",
                            "decision_reason",
                            "detail",
                            "details",
                            "display_name",
                            "display_family",
                            "evidence",
                            "error",
                            "executable",
                            "exit_reason",
                            "family_preview",
                            "family_unavailable_reason",
                            "feedback",
                            "failure_reason",
                            "fixture_root",
                            "gaps",
                            "handoff",
                            "justification",
                            "last_error",
                            "launch",
                            "launch_error",
                            "legacy",
                            "manifest",
                            "message",
                            "model_evidence",
                            "name",
                            "native_session_id",
                            "nonce",
                            "payload",
                            "preimage_content",
                            "plan",
                            "proposal",
                            "provider_error",
                            "input",
                            "reason",
                            "recipe_name",
                            "requested_access",
                            "relevant_inputs",
                            "retry_prerequisite",
                            "repository_identity",
                            "repository_path",
                            "repository_root",
                            "shell",
                            "shell_command",
                            "signal_delivery_error",
                            "role_overrides",
                            "scope",
                            "settings",
                            "summary",
                            "suite_name",
                            "testing_coverage",
                            "entries",
                            "canonical_root",
                            "checkout_root",
                            "alternate_roots",
                            "partial_paths",
                            "owned_paths",
                            "shared_paths",
                            "protected_paths",
                            "source_hashes",
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
                            "task_title",
                            "stage_reason",
                            "waiting_reason",
                            "original_text",
                            "cwd",
                        ]
                        .contains(&key.as_str());
                        let value = if omitted {
                            serde_json::Value::String("[OMITTED FROM SANITIZED EXPORT]".to_owned())
                        } else if key == "acceptance_rows" {
                            // Check rows allow arbitrary JSON; only result statuses
                            // have a fixed public vocabulary.
                            match value {
                                serde_json::Value::Array(rows) => serde_json::Value::Array(
                                    rows.into_iter()
                                        .map(|row| {
                                            match row
                                                .get("status")
                                                .and_then(serde_json::Value::as_str)
                                            {
                                                Some(status @ ("met" | "unmet")) => {
                                                    serde_json::json!({"status": status})
                                                }
                                                _ => serde_json::Value::String(
                                                    "[OMITTED FROM SANITIZED EXPORT]".to_owned(),
                                                ),
                                            }
                                        })
                                        .collect(),
                                ),
                                _ => serde_json::Value::String(
                                    "[OMITTED FROM SANITIZED EXPORT]".to_owned(),
                                ),
                            }
                        } else if key == "manager_control" {
                            // Setup duplicates selected profile JSON under these aliases.
                            let mut control = value;
                            if let Some(fields) = control.as_object_mut() {
                                for key in ["current", "requested"] {
                                    if let Some(profile) = fields.get_mut(key) {
                                        *profile = serde_json::Value::String(
                                            "[OMITTED FROM SANITIZED EXPORT]".to_owned(),
                                        );
                                    }
                                }
                            }
                            omit(control)
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
    let mut value = value;
    if let Some(projects) = value
        .get_mut("projects")
        .and_then(serde_json::Value::as_array_mut)
    {
        for project in projects {
            if let Some(detected) = project
                .pointer_mut("/trip/detected")
                .and_then(serde_json::Value::as_object_mut)
            {
                // Inspection exposes raw file JSON and path-bearing parse errors;
                // the observation's status and correlation hash remain useful.
                for key in ["configuration", "configuration_error"] {
                    if let Some(field) = detected.get_mut(key) {
                        *field = "[OMITTED FROM SANITIZED EXPORT]".into();
                    }
                }
            }
        }
    }
    let runtime_scope_hashes = value
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
        .map(|capabilities| {
            capabilities
                .iter()
                .map(|capability| {
                    capability
                        .pointer("/proof/runtime_scope/scope_hash")
                        .and_then(serde_json::Value::as_str)
                        .filter(|hash| {
                            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                        })
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut sanitized = omit(sanitize_value(value));
    if let Some(capabilities) = sanitized
        .get_mut("capabilities")
        .and_then(serde_json::Value::as_array_mut)
    {
        for (capability, scope_hash) in capabilities.iter_mut().zip(runtime_scope_hashes) {
            if let (Some(proof), Some(scope_hash)) = (capability.get_mut("proof"), scope_hash) {
                // Retain the published runtime binding without restoring its opaque proof.
                *proof = serde_json::json!({"runtime_scope":{"scope_hash":scope_hash}});
            }
        }
    }
    sanitized
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use sha2::{Digest, Sha256};

    #[test]
    fn capability_proof_retains_only_a_valid_runtime_scope_hash() {
        let valid_hash = hex::encode(Sha256::digest(b"published runtime scope"));
        for (hash, retained) in [
            (serde_json::json!(valid_hash), true),
            (serde_json::json!("a".repeat(63)), false),
            (serde_json::json!("z".repeat(64)), false),
            (serde_json::json!("/private/PRIVATE-SCOPE-PATH"), false),
            (
                serde_json::json!({"extension":"PRIVATE-SCOPE-EXTENSION"}),
                false,
            ),
            (serde_json::Value::Null, false),
        ] {
            let proof = serde_json::json!({
                "runtime_scope":{
                    "scope_hash":hash,
                    "fixture_repository_identity":"PRIVATE-FIXTURE-IDENTITY",
                    "extension":{"arbitrary":"PRIVATE-SCOPE-EXTENSION"}
                },
                "extension":"PRIVATE-PROOF-EXTENSION"
            });
            let sanitized = sanitize_state(serde_json::json!({
                "capabilities":[{"id":"capability-id","status":"supported","proof":proof}],
                "unrelated":{"proof":proof}
            }));
            assert_eq!(sanitized["capabilities"][0]["id"], "capability-id");
            assert_eq!(sanitized["capabilities"][0]["status"], "supported");
            assert_eq!(
                sanitized["unrelated"]["proof"],
                "[OMITTED FROM SANITIZED EXPORT]"
            );
            if retained {
                assert_eq!(
                    sanitized["capabilities"][0]["proof"],
                    serde_json::json!({"runtime_scope":{"scope_hash":valid_hash}})
                );
            } else {
                assert_eq!(
                    sanitized["capabilities"][0]["proof"],
                    "[OMITTED FROM SANITIZED EXPORT]"
                );
            }
            assert!(!sanitized.to_string().contains("PRIVATE-"));
            assert_eq!(sanitize_state(sanitized.clone()), sanitized);
        }
    }

    #[test]
    fn public_export_omits_inspected_configuration_and_path_errors() {
        let root = std::env::temp_dir().join(format!(
            "llmrelay-export-inspection-{}",
            uuid::Uuid::new_v4()
        ));
        let repository = root.join("PRIVATE-INSPECTION-ROOT");
        std::fs::create_dir_all(&repository).unwrap();
        let git = |path: &Path, arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&repository, &["init", "-q"]);
        for key in ["user.name", "user.email"] {
            let configured = git(Path::new(env!("CARGO_MANIFEST_DIR")), &["config", key]);
            git(&repository, &["config", key, &configured]);
        }
        git(
            &repository,
            &[
                "commit",
                "--allow-empty",
                "-qm",
                "Create export inspection fixture",
            ],
        );
        assert_eq!(
            git(&repository, &["log", "-1", "--format=%B"]),
            "Create export inspection fixture"
        );
        let repository = repository.canonicalize().unwrap();
        let installation = repository.join(".agents/trip-explorer");
        std::fs::create_dir_all(&installation).unwrap();
        let configuration = serde_json::json!({
            "project_name":"PRIVATE-INSPECTED-PROJECT",
            "guidance":["docs/PRIVATE-INSPECTED-GUIDANCE.md"],
            "documentation":{"no_change_text":"PRIVATE-INSPECTED-POLICY"},
            "extension":{"private_location":"/Users/person/PRIVATE-INSPECTED-EXTENSION"}
        });
        let config_path = installation.join("config.json");
        let adapters_path = installation.join("adapters.json");
        std::fs::write(&config_path, configuration.to_string()).unwrap();
        std::fs::write(&adapters_path, b"{}").unwrap();

        let paths = InstancePaths::resolve(Some(root.join("instance"))).unwrap();
        std::fs::create_dir_all(&paths.state).unwrap();
        let store = Store::open(&paths.database).unwrap();
        let project = crate::workflow::execute(
            &store,
            &crate::domain::HumanCommand::AddProject {
                operation_id: "register-export-inspection-project".into(),
                path: repository.clone(),
                display_name: "PRIVATE-INSPECTED-PROJECT".into(),
            },
        )
        .unwrap()
        .entity_id;
        for omitted_field in ["configuration", "configuration_error"] {
            if omitted_field == "configuration_error" {
                std::fs::remove_file(&config_path).unwrap();
                std::os::unix::fs::symlink(&adapters_path, &config_path).unwrap();
            }
            let inspected = crate::trip::inspect_registered_project(&store, &project).unwrap();
            let projected = serde_json::to_value(crate::workflow::state(&store).unwrap()).unwrap();
            let detected = &projected["projects"][0]["trip"]["detected"];
            assert_eq!(detected, &inspected["observation"]);
            if omitted_field == "configuration" {
                assert_eq!(detected[omitted_field], configuration);
            } else {
                assert!(detected[omitted_field]
                    .as_str()
                    .unwrap()
                    .contains(repository.to_str().unwrap()));
            }
            let sanitized = sanitize_state(projected.clone());
            let exported_observation = &sanitized["projects"][0]["trip"]["detected"];
            assert_eq!(
                exported_observation[omitted_field],
                "[OMITTED FROM SANITIZED EXPORT]"
            );
            assert_eq!(exported_observation["kind"], "partial");
            assert_eq!(exported_observation["customized"], false);
            assert_eq!(
                exported_observation["observation_hash"],
                detected["observation_hash"]
            );
            assert_eq!(sanitized["projects"][0]["id"], project);

            let archive = root.join(format!("{omitted_field}.zip"));
            export_sanitized(&paths, &archive).unwrap();
            let bytes = std::fs::read(archive).unwrap();
            let exported = String::from_utf8_lossy(&bytes);
            for private in [
                "PRIVATE-INSPECTION-ROOT",
                "PRIVATE-INSPECTED-PROJECT",
                "PRIVATE-INSPECTED-GUIDANCE",
                "PRIVATE-INSPECTED-POLICY",
                "PRIVATE-INSPECTED-EXTENSION",
            ] {
                assert!(
                    !exported.contains(private),
                    "{omitted_field} leaked {private}"
                );
            }
            assert!(exported.contains(&format!("\"id\": \"{project}\"")));
            assert!(exported.contains("\"kind\": \"partial\""));
            assert!(exported.contains("\"customized\": false"));
            assert!(exported.contains(detected["observation_hash"].as_str().unwrap()));
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_export_omits_freeform_aliases_and_corrupt_log_bytes() {
        let root =
            std::env::temp_dir().join(format!("llmrelay-export-private-{}", uuid::Uuid::new_v4()));
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        std::fs::create_dir_all(&paths.state).unwrap();
        let store = Store::open(&paths.database).unwrap();
        {
            let connection = store.lock().unwrap();
            let now = "2026-10-29T00:00:00Z";
            let (definition, definition_hash) =
                crate::workflow::ensure_single_agent_definition(&connection, now).unwrap();
            connection.execute_batch(r#"INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                VALUES('export-project','Private project','/unused/export-project','export-repository','base','2026-10-29','2026-10-29');
                INSERT INTO tasks(id,project_id,title,lifecycle,attention,created_at,updated_at)
                VALUES('export-task','export-project','PRIVATE-TASK-TITLE','in_progress','needs_input','2026-10-29','2026-10-29');
                INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                VALUES('export-attempt','export-task','export-context','agent_execution','base',1,'needs_input','2026-10-29','2026-10-29');
                INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                VALUES('export-generation','export-attempt','implementer','codex',1,1,'exited','export-authority','2026-10-29','2026-10-29');
                INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,launch_state,exit_json,created_at,updated_at)
                VALUES('export-session','export-generation','codex','exited','{}','synthetic','export-epoch','finished',
                    '{"success":false,"code":7,"process_group_quiescent":true,"output_tail":"PRIVATE-TRANSCRIPT-TAIL"}',
                    '2026-10-29','2026-10-29');"#).unwrap();
            connection.execute("INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
                VALUES('export-config','export-project',1,?1,'configuration-hash',?2,?2)",params![serde_json::json!({"schema":1,"base_revision":"base","guidance":[{"path":"docs/PRIVATE-GUIDANCE.md","sha256":"guidance-hash","bytes":64}],"documentation_policy":"PRIVATE-DOCUMENTATION","testing_coverage":"moderate"}).to_string(),now]).unwrap();
            connection.execute_batch("INSERT INTO execution_verification_checks(id,execution_config_revision_id,check_key,command_kind,executable,arguments_json,shell_command,cwd,timeout_seconds,acceptance_rows_json,relevant_inputs_json,invalidation_json,original_text)
                VALUES('export-check','export-config','focused','exact_argv','true','[]',NULL,'.',30,
                    '[\"PRIVATE-ACCEPTANCE-TEXT\"]','[\"src/PRIVATE-REPOSITORY-PATH.rs\"]','[]','PRIVATE-CHECK-DESCRIPTION');
                INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                VALUES('export-held','export-held-operation','service','single_agent.attempt.held','attempt','export-attempt','{\"reason\":\"PRIVATE-HELD-REASON\"}','2026-10-29');").unwrap();
            connection.execute("INSERT INTO task_workflow_selections(task_id,family,definition_id,execution_config_revision_id,selection_revision,selection_hash,admission_state,created_at,updated_at)
                VALUES('export-task','single_agent',?1,'export-config',1,'selection-hash','draft',?2,?2)",params![definition,now]).unwrap();
            connection.execute("INSERT INTO workflow_run_bindings(attempt_id,family,definition_id,definition_hash,definition_state,evaluator_revision,execution_config_revision_id,execution_config_hash,selection_hash,scope_hash,created_at)
                VALUES('export-attempt','single_agent',?1,?2,'available',1,'export-config','configuration-hash','selection-hash','scope-hash',?3)",params![definition,definition_hash,now]).unwrap();
            connection.execute("INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at)
                VALUES('export-report','export-operation','export-session','export-generation','result_ready','PRIVATE-SUMMARY','[]',?1,?2)",params![serde_json::json!({"acceptance_rows":[{"criterion":"PRIVATE-CRITERION","status":"met","evidence":[]}]}).to_string(),now]).unwrap();
            connection.execute("INSERT INTO workflow_result_bindings(id,attempt_id,role_result_id,role_generation_id,session_id,transcript_epoch,criteria_hash,evidence_hash,profile_binding_hash,selected_check_revision,state,created_at,updated_at)
                VALUES('export-result','export-attempt','export-report','export-generation','export-session','export-epoch','criteria-hash','evidence-hash','profile-hash',0,'superseded',?1,?1)",params![now]).unwrap();
            connection
                .execute_batch(
                    r#"INSERT INTO check_suites(id,project_id,name,position,executable,arguments_json,
                        timeout_seconds,created_at,updated_at)
                    VALUES('export-suite','export-project','PRIVATE-SUITE-NAME',0,'true','[]',30,
                        '2026-10-29','2026-10-29');
                    INSERT INTO check_runs(id,attempt_id,candidate_hash,check_id,executable,arguments_json,
                        cwd,status,launch_state,exit_code,suite_name,check_suite_version,
                        acceptance_coverage_json,created_at)
                    SELECT 'export-run','export-attempt','export-candidate-hash',c.id,c.executable,
                        c.arguments_json,c.cwd,'finished','finished',0,s.name,s.version,
                        c.acceptance_rows_json,'2026-10-29'
                    FROM execution_verification_checks c CROSS JOIN check_suites s
                    WHERE c.id='export-check' AND s.id='export-suite';
                    INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,
                        preflight_json,verification_json,source_hash,overlay_hash,configuration_hash,created_at)
                    VALUES('export-trip-config','export-project',1,'activated','{}','{}','[]','{}',
                        'source-hash','overlay-hash','export-recipe-configuration-hash','2026-10-29');
                    INSERT INTO project_profile_sets(id,project_id,name,current_revision,version,created_at,updated_at)
                    VALUES('export-profile-set','export-project','PRIVATE-PROFILE-SET-NAME',1,1,
                        '2026-10-29','2026-10-29');
                    INSERT INTO project_profile_set_revisions(id,profile_set_id,revision,roles_json,
                        config_revision_id,configuration_hash,created_at)
                    VALUES('export-profile-revision','export-profile-set',1,'{}','export-trip-config',
                        'export-recipe-configuration-hash','2026-10-29');
                    INSERT INTO task_recipes(id,project_id,name,current_revision,version,created_at,updated_at)
                    VALUES('export-recipe','export-project','PRIVATE-RECIPE-NAME',1,1,
                        '2026-10-29','2026-10-29');
                    INSERT INTO task_recipe_revisions(id,recipe_id,revision,title,description,
                        acceptance_criteria_json,priority,profile_revision_id,required_check_ids_json,
                        config_revision_id,configuration_hash,workflow_version,workflow_hash,created_at)
                    VALUES('export-recipe-revision','export-recipe',1,'PRIVATE-TASK-TITLE','',
                        '[]',0,'export-profile-revision','[]','export-trip-config',
                        'export-recipe-configuration-hash','fixture','workflow-hash','2026-10-29');
                    INSERT INTO recipe_schedules(id,project_id,name,recipe_revision_id,cadence,
                        anchor_utc,paused,version,created_at,updated_at)
                    VALUES('export-schedule','export-project','PRIVATE-SCHEDULE-NAME',
                        'export-recipe-revision','daily','2026-10-29T00:00:00Z',1,1,
                        '2026-10-29','2026-10-29');"#,
                )
                .unwrap();
            connection.execute(
                "UPDATE trip_config_revisions SET config_json=?1,adapters_json=?2,verification_json=?3 WHERE id='export-trip-config'",
                params![
                    serde_json::json!({
                        "project_name":"PRIVATE-ACTIVATED-PROJECT",
                        "guidance":["docs/PRIVATE-ACTIVATED-GUIDANCE.md"],
                        "documentation":{"no_change_text":"PRIVATE-ACTIVATED-DOCUMENTATION"},
                        "verification":{"focused":["PRIVATE-CONFIG-FOCUSED"],"broad":["PRIVATE-CONFIG-BROAD"],"cleanup":["PRIVATE-CONFIG-CLEANUP"]},
                        "extension":{"arbitrary":"PRIVATE-CONFIG-EXTENSION"}
                    }).to_string(),
                    serde_json::json!({"custom":{"arbitrary":"PRIVATE-ADAPTER-EXTENSION"}}).to_string(),
                    serde_json::json!({"focused":["PRIVATE-VERIFICATION-FOCUSED"],"broad":["PRIVATE-VERIFICATION-BROAD"],"cleanup":["PRIVATE-VERIFICATION-CLEANUP"]}).to_string(),
                ],
            ).unwrap();
            connection.execute_batch(r#"
                INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,active_config_revision_id,updated_at)
                VALUES('export-project','ready','fixture','compatible','export-trip-config','2026-10-29');
                INSERT INTO implementation_lanes(id,attempt_id,lane_key,owned_paths_json,shared_paths_json,
                    protected_paths_json,dependencies_json,source_hashes_json,frozen_seams_hash,state,receipt_json,created_at,updated_at)
                VALUES('export-lane','export-attempt','lane-one','["src/PRIVATE-OWNED-PATH.rs"]',
                    '["src/PRIVATE-SHARED-PATH.rs"]','["src/PRIVATE-PROTECTED-PATH.rs"]','[]',
                    '{"src/PRIVATE-SOURCE-KEY.rs":{"hash":"source-hash"}}','export-frozen-seams-hash','yielded',
                    '{"changed_paths":["src/PRIVATE-RECEIPT-PATH.rs"],"source_hashes":{"src/PRIVATE-RECEIPT-KEY.rs":"hash"},"arbitrary":"PRIVATE-RECEIPT-EXTENSION"}',
                    '2026-10-29','2026-10-29');
                INSERT INTO trip_integration_requests(id,attempt_id,capsule_json,requested_by_generation_id,state,created_at)
                VALUES('export-integration','export-attempt',
                    '{"ordered_lanes":["lane-one"],"merge_strategy":"PRIVATE-MERGE-STRATEGY","verification_boundary":"PRIVATE-VERIFICATION-BOUNDARY","worktree_scope":{"paths":["src/PRIVATE-CAPSULE-PATH.rs"],"attribution":{"src/PRIVATE-ATTRIBUTION-KEY.rs":"lane-one"}},"arbitrary":"PRIVATE-CAPSULE-EXTENSION"}',
                    'export-generation','requested','2026-10-29');
                INSERT INTO trip_explorer_decisions(id,attempt_id,stage,census_json,trigger,activated,limits_json,candidate_hash,created_at)
                VALUES('export-explorer','export-attempt','planning','{"arbitrary":"PRIVATE-CENSUS-EXTENSION"}',
                    'multi_module',1,'{"question":"PRIVATE-EXPLORER-QUESTION","max_words":100,"arbitrary":"PRIVATE-LIMITS-EXTENSION"}',
                    'export-explorer-candidate-hash','2026-10-29');
                INSERT INTO trip_explorer_extensions(id,attempt_id,stage,justification,authorized_at)
                VALUES('export-extension','export-attempt','rescue','PRIVATE-EXPLORER-JUSTIFICATION','2026-10-29');
                UPDATE sessions SET process_identity_json='{}' WHERE id='export-session';
                INSERT INTO cmux_task_workspaces(id,service_boot_id,task_id,generation,state,created_at,updated_at)
                VALUES('export-cmux-workspace','export-boot','export-task',1,'unknown','2026-10-29','2026-10-29');
                INSERT INTO cmux_session_surfaces(id,task_workspace_id,service_boot_id,session_id,role_generation_id,
                    transcript_epoch,process_identity_json,binding_revision,surface_state,attachment_state,
                    desired_input_state,actual_input_state,last_error,created_at,updated_at)
                VALUES('export-cmux-surface','export-cmux-workspace','export-boot','export-session','export-generation',
                    'export-epoch','{}',1,'unknown','pending','view_only','blocked',
                    'PRIVATE-CMUX-ERROR /private/PRIVATE-CMUX-PATH.sock','2026-10-29','2026-10-29');
                INSERT INTO trip_setup_operations(id,project_id,discovery_attempt_id,state,target_inventory_json,proposal_json,created_at,updated_at)
                VALUES('export-setup','export-project','export-attempt','activated','{}','{}','2026-10-29','2026-10-29');
                UPDATE attempts SET setup_operation_id='export-setup' WHERE id='export-attempt';
                INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
                VALUES('export-setup-stop','export-attempt','setup_manager_change','held',1,
                    '{"signal_delivery":{"state":"failed","error":"PRIVATE-SIGNAL-ERROR /private/PRIVATE-SIGNAL-PATH.sock"}}',
                    '2026-10-29','2026-10-29');
            "#).unwrap();
            connection.execute(
                "INSERT INTO capabilities(id,provider,executable_version,role,mode,config_hash,status,proof_json,checked_at)
                 VALUES('export-capability','codex','synthetic','implementer','main_checkout','export-capability-hash','supported',?1,?2)",
                params![serde_json::json!({
                    "qualification_contract":"functional-v2",
                    "session_id":"export-session",
                    "configured_protections":{
                        "checkout_scope":{
                            "root":"/private/PRIVATE-PROOF-CHECKOUT",
                            "protected_roots":["/private/PRIVATE-PROOF-PROTECTED"]
                        },
                        "denied_read_floor":{"additional_denied_roots":["/private/PRIVATE-PROOF-DENIAL"]}
                    },
                    "retained_observations":{"diagnostic_input":{"prompt":"PRIVATE-PROOF-PROMPT"}},
                    "evidence_reference":"PRIVATE-PROOF-EVIDENCE"
                }).to_string(),now],
            ).unwrap();
            // Structured TRIP check contracts retain arbitrary invalidation objects
            // and acceptance-row members, unlike the typed result report DTO.
            connection.execute(
                "INSERT INTO trip_verification_checks(id,project_id,config_revision_id,check_key,category,command_kind,
                    shell_command,cwd,timeout_seconds,acceptance_rows_json,relevant_inputs_json,invalidation_json,original_text)
                 VALUES('export-trip-check','export-project','export-trip-config','focused_1','focused','exact_shell',
                    'true','.',30,?1,'[]',?2,'true')",
                params![serde_json::json!([
                    {"status":"met","note":"PRIVATE-TRIP-ACCEPTANCE-NOTE","extension":{"arbitrary":"PRIVATE-TRIP-ACCEPTANCE-EXTENSION"}},
                    {"status":"unmet","criterion":"PRIVATE-TRIP-UNMET-CRITERION"},
                    {"status":"PRIVATE-TRIP-ACCEPTANCE-STATUS"}
                ]).to_string(),serde_json::json!({"dependency":"/private/PRIVATE-TRIP-INVALIDATION"}).to_string()],
            ).unwrap();
            let profile = serde_json::json!({
                "adapter":"codex","provider":"codex","model":"model","effort":"high",
                "authority":"workspace-write","session":"retained",
                "notes":"PRIVATE-SELECTED-PROFILE-NOTE",
                "private_location":"/private/PRIVATE-SELECTED-PROFILE-PATH"
            });
            connection.execute(
                "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state,profile_json,profile_hash,selected_at)
                 VALUES('export-setup','implementer','selected',?1,'export-selected-profile-hash',?2)",
                params![profile.to_string(),now],
            ).unwrap();
            connection.execute(
                "INSERT INTO trip_setup_profile_selections(setup_operation_id,role,selection_state,profile_json,profile_hash,selected_at)
                 VALUES('export-setup','manager','selected',?1,'export-manager-profile-hash',?2)",
                params![serde_json::json!({"provider":"codex","model":"PRIVATE-MANAGER-PROFILE-MODEL","effort":"high"}).to_string(),now],
            ).unwrap();
            connection.execute(
                "INSERT INTO trip_runtime_admissions(id,project_id,scope_hash,state,fresh_call_count,created_at,updated_at)
                 VALUES('export-runtime-admission','export-project','export-runtime-scope-hash','prepared',0,?1,?1)",
                params![now],
            ).unwrap();
            connection.execute(
                "INSERT INTO trip_runtime_probes(admission_id,role,launch_config_json,profile_json,profile_hash,
                    project_config_revision_id,project_configuration_hash,adapter_name,adapter_hash,capability_key,
                    capability_identity_json,fixture_project_id,fixture_repository_identity,fixture_root,nonce,state,created_at,updated_at)
                 VALUES('export-runtime-admission','implementer','{}',?1,'export-runtime-profile-hash',
                    'export-trip-config','export-recipe-configuration-hash','codex','export-adapter-hash','export-capability-hash',
                    '{}','export-project','export-repository','/unused/export-fixture','fixture-nonce','prepared',?2,?2)",
                params![profile.to_string(),now],
            ).unwrap();
            connection.execute_batch(r#"
                UPDATE sessions SET native_session_id='export-native-session',initial_hook_event_boundary_rowid=0
                    WHERE id='export-session';
                INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,native_session_id,
                    payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                VALUES('export-start-hook','export-session','export-generation','codex','SessionStart','export-native-session',
                    '{}',1,1,'fixture','managed_process_group_untrusted_payload','2026-10-29T00:00:00Z');
                INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,native_session_id,
                    payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                VALUES('export-failure-hook','export-session','export-generation','codex','StopFailure','export-native-session',
                    '{"error":"PRIVATE-NATIVE-ERROR /private/PRIVATE-NATIVE-ERROR-PATH"}',1,1,'fixture',
                    'managed_process_group_untrusted_payload','2026-10-29T00:00:01Z');
            "#).unwrap();
        }
        let role_names = [
            "manager",
            "explorer",
            "plan_reviewer",
            "implementer",
            "code_reviewer",
            "final_verifier",
        ];
        let mut roles = serde_json::Map::new();
        for role in role_names {
            roles.insert(
                role.into(),
                serde_json::json!({"provider":"codex","model":"model","effort":"high"}),
            );
        }
        roles.get_mut("manager").unwrap()["notes"] = "PRIVATE-PROFILE-SET-NOTE".into();
        roles.get_mut("manager").unwrap()["extension"] = serde_json::json!({
            "private_location":"/Users/person/PRIVATE-PROFILE-SET-PATH"
        });
        let guidance_path = "docs/PRIVATE-RECOVERY-GUIDANCE.md";
        let workspace = root.join("guidance-workspace");
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::write(workspace.join(guidance_path), b"approved guidance\n").unwrap();
        let policy = serde_json::json!({
            "kind":"activated_project","workflow_id":crate::trip::WORKFLOW_ID,
            "upstream_source_hash":crate::trip::source_hash(),
            "overlay_hash":crate::trip::overlay_hash(),
            "manifest_hash":hex::encode(Sha256::digest(b"export policy manifest")),
            "files":{(guidance_path):hex::encode(Sha256::digest(b"approved guidance\n"))},
            "task_profiles":role_names.map(|role|serde_json::json!({
                "role":role,"settings_revision":1,"profile":{},"source":"project_default",
                "profile_hash":"profile-hash","project_config_revision_id":"export-trip-config",
                "project_configuration_hash":"export-recipe-configuration-hash",
                "adapter":"codex","adapter_hash":"adapter-hash","capability_id":"export-capability",
                "capability_key":"capability-hash","capability_proof_hash":"proof-hash"
            })),
            "base_project_configuration_distinct":true,
        });
        let saved_profiles = {
            let mut connection = store.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            transaction.execute(
                "UPDATE trip_project_state SET workflow_id=?1,package_version=?2,upstream_source_hash=?3,overlay_hash=?4
                 WHERE project_id='export-project'",
                params![crate::trip::WORKFLOW_ID,crate::trip::PACKAGE_VERSION,crate::trip::source_hash(),crate::trip::overlay_hash()],
            ).unwrap();
            let saved = crate::recipes::apply_command(
                &transaction,
                &crate::domain::HumanCommand::UpsertProfileSet {
                    operation_id: "export-opaque-profile-operation".into(),
                    project_id: "export-project".into(),
                    profile_set_id: None,
                    expected_version: None,
                    name: "ZZ opaque profiles".into(),
                    roles: roles.into(),
                },
                "2026-10-29T00:00:00Z",
            )
            .unwrap();
            transaction.execute_batch(
                "INSERT INTO tasks(id,project_id,title,lifecycle,attention,created_at,updated_at)
                 VALUES('export-guidance-task','export-project','Guidance task','in_progress','needs_input','2026-10-29','2026-10-29');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                 VALUES('export-guidance-attempt','export-guidance-task','export-guidance-context','implementation','base',1,'held','2026-10-29','2026-10-29');
                 INSERT INTO recovery_records(id,attempt_id,state,detail_json,created_at,updated_at)
                 VALUES('export-guidance-recovery','export-guidance-attempt','attention_required',
                 '{\"kind\":\"coordinator_failure\",\"operation\":\"guidance_delivery\",\"effect_certainty\":\"none\"}',
                 '2026-10-29','2026-10-29');"
            ).unwrap();
            transaction.execute(
                "UPDATE attempts SET workflow_version=?1,workflow_hash=?2,legacy_migration_required=0 WHERE id='export-guidance-attempt'",
                params![crate::trip::WORKFLOW_ID,crate::workflow_resources::workflow_hash()],
            ).unwrap();
            transaction.execute(
                "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
                 VALUES('export-guidance-workspace','export-guidance-attempt','export-repository',?1,'base','base',?2,'ready','2026-10-29','2026-10-29')",
                params![workspace.to_string_lossy(),policy.to_string()],
            ).unwrap();
            assert!(crate::workflow::failed_step_prerequisite(
                &transaction,
                "export-guidance-attempt",
                Some("guidance_delivery")
            )
            .is_none());
            transaction.commit().unwrap();
            saved
        };
        std::fs::write(workspace.join(guidance_path), b"changed guidance\n").unwrap();
        let sentinels = [
            "PRIVATE-SUMMARY",
            "PRIVATE-CRITERION",
            "PRIVATE-DOCUMENTATION",
            "PRIVATE-GUIDANCE",
            "PRIVATE-TASK-TITLE",
            "PRIVATE-ACCEPTANCE-TEXT",
            "PRIVATE-REPOSITORY-PATH",
            "PRIVATE-CHECK-DESCRIPTION",
            "PRIVATE-HELD-REASON",
            "PRIVATE-TRANSCRIPT-TAIL",
            "PRIVATE-SUITE-NAME",
            "PRIVATE-PROFILE-SET-NAME",
            "PRIVATE-RECIPE-NAME",
            "PRIVATE-SCHEDULE-NAME",
            "PRIVATE-ACTIVATED-PROJECT",
            "PRIVATE-ACTIVATED-GUIDANCE",
            "PRIVATE-ACTIVATED-DOCUMENTATION",
            "PRIVATE-CONFIG-FOCUSED",
            "PRIVATE-CONFIG-BROAD",
            "PRIVATE-CONFIG-CLEANUP",
            "PRIVATE-CONFIG-EXTENSION",
            "PRIVATE-ADAPTER-EXTENSION",
            "PRIVATE-VERIFICATION-FOCUSED",
            "PRIVATE-VERIFICATION-BROAD",
            "PRIVATE-VERIFICATION-CLEANUP",
            "PRIVATE-OWNED-PATH",
            "PRIVATE-SHARED-PATH",
            "PRIVATE-PROTECTED-PATH",
            "PRIVATE-SOURCE-KEY",
            "PRIVATE-RECEIPT-PATH",
            "PRIVATE-RECEIPT-KEY",
            "PRIVATE-RECEIPT-EXTENSION",
            "PRIVATE-MERGE-STRATEGY",
            "PRIVATE-VERIFICATION-BOUNDARY",
            "PRIVATE-CAPSULE-PATH",
            "PRIVATE-ATTRIBUTION-KEY",
            "PRIVATE-CAPSULE-EXTENSION",
            "PRIVATE-CENSUS-EXTENSION",
            "PRIVATE-EXPLORER-QUESTION",
            "PRIVATE-LIMITS-EXTENSION",
            "PRIVATE-EXPLORER-JUSTIFICATION",
            "PRIVATE-CMUX-ERROR",
            "PRIVATE-CMUX-PATH",
            "PRIVATE-SIGNAL-ERROR",
            "PRIVATE-SIGNAL-PATH",
            "PRIVATE-PROOF-CHECKOUT",
            "PRIVATE-PROOF-PROTECTED",
            "PRIVATE-PROOF-DENIAL",
            "PRIVATE-PROOF-PROMPT",
            "PRIVATE-PROOF-EVIDENCE",
            "PRIVATE-TRIP-ACCEPTANCE-NOTE",
            "PRIVATE-TRIP-ACCEPTANCE-EXTENSION",
            "PRIVATE-TRIP-UNMET-CRITERION",
            "PRIVATE-TRIP-ACCEPTANCE-STATUS",
            "PRIVATE-TRIP-INVALIDATION",
            "PRIVATE-SELECTED-PROFILE-NOTE",
            "PRIVATE-SELECTED-PROFILE-PATH",
            "PRIVATE-MANAGER-PROFILE-MODEL",
            "PRIVATE-NATIVE-ERROR",
            "PRIVATE-NATIVE-ERROR-PATH",
            "PRIVATE-PROFILE-SET-NOTE",
            "PRIVATE-PROFILE-SET-PATH",
            "PRIVATE-RECOVERY-GUIDANCE",
        ];
        let projected = crate::workflow::state(&store).unwrap();
        let profiles = projected
            .profile_sets
            .iter()
            .find(|profiles| profiles["id"] == saved_profiles.entity_id)
            .unwrap();
        assert_eq!(profiles["roles"].as_object().unwrap().len(), 6);
        assert_eq!(
            profiles["roles"]["manager"]["notes"],
            "PRIVATE-PROFILE-SET-NOTE"
        );
        assert_eq!(
            profiles["roles"]["manager"]["extension"]["private_location"],
            "/Users/person/PRIVATE-PROFILE-SET-PATH"
        );
        let recovery = projected
            .recovery
            .iter()
            .find(|record| record["id"] == "export-guidance-recovery")
            .unwrap();
        assert_eq!(
            recovery["retry_prerequisite"],
            format!("worktree policy file changed after verified materialization: {guidance_path}")
        );
        let recovery_action = projected
            .continuation_actions
            .iter()
            .find(|action| action.binding["recovery_id"] == "export-guidance-recovery")
            .unwrap();
        assert_eq!(
            recovery_action.binding["retry_prerequisite"],
            recovery["retry_prerequisite"]
        );
        assert_eq!(recovery_action.binding["retry_enabled"], false);
        let attention = projected
            .attention
            .iter()
            .find(|item| item.task_title.as_deref() == Some("PRIVATE-TASK-TITLE"))
            .unwrap();
        assert_eq!(
            attention.target.as_ref().unwrap().task_id(),
            Some("export-task")
        );
        let check = &projected.workflow_execution[0].configurations[0].checks[0];
        assert_eq!(check["acceptance_rows"][0], "PRIVATE-ACCEPTANCE-TEXT");
        assert_eq!(
            check["relevant_inputs"][0],
            "src/PRIVATE-REPOSITORY-PATH.rs"
        );
        assert_eq!(
            projected.active_sessions[0]["exit_reason"],
            "PRIVATE-TRANSCRIPT-TAIL"
        );
        assert_eq!(
            projected.checks[0]["acceptance_coverage"][0],
            "PRIVATE-ACCEPTANCE-TEXT"
        );
        assert_eq!(projected.checks[0]["suite_name"], "PRIVATE-SUITE-NAME");
        assert_eq!(projected.check_suites[0]["name"], "PRIVATE-SUITE-NAME");
        assert_eq!(
            projected.profile_sets[0]["name"],
            "PRIVATE-PROFILE-SET-NAME"
        );
        assert_eq!(projected.task_recipes[0]["name"], "PRIVATE-RECIPE-NAME");
        assert_eq!(
            projected.recipe_schedules[0]["name"],
            "PRIVATE-SCHEDULE-NAME"
        );
        assert_eq!(
            projected.recipe_schedules[0]["recipe_name"],
            "PRIVATE-RECIPE-NAME"
        );
        assert_eq!(
            projected.projects[0].trip["configuration"]["config"]["project_name"],
            "PRIVATE-ACTIVATED-PROJECT"
        );
        assert_eq!(projected.trip_lanes[0]["id"], "export-lane");
        assert_eq!(projected.trip_explorer[0]["id"], "export-explorer");
        assert_eq!(
            projected.active_sessions[0]["cmux_surface"]["last_error"],
            "PRIVATE-CMUX-ERROR /private/PRIVATE-CMUX-PATH.sock"
        );
        assert_eq!(
            projected.trip_setups[0]["manager_control"]["hold"]["signal_delivery_error"],
            "PRIVATE-SIGNAL-ERROR /private/PRIVATE-SIGNAL-PATH.sock"
        );
        assert_eq!(
            projected.capabilities[0]["proof"]["configured_protections"]["checkout_scope"]["root"],
            "/private/PRIVATE-PROOF-CHECKOUT"
        );
        assert_eq!(
            projected.active_sessions[0]["native_turn"]["failure"]["provider_error"],
            "PRIVATE-NATIVE-ERROR /private/PRIVATE-NATIVE-ERROR-PATH"
        );
        assert_eq!(
            projected.trip_checks[0]["invalidation"]["dependency"],
            "/private/PRIVATE-TRIP-INVALIDATION"
        );
        let setup = &projected.trip_setups[0];
        let implementer = setup["selected_profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|selection| selection["role"] == "implementer")
            .unwrap();
        assert_eq!(
            implementer["profile"]["notes"],
            "PRIVATE-SELECTED-PROFILE-NOTE"
        );
        assert_eq!(
            setup["runtime_admissions"][0]["probes"][0]["profile"]["notes"],
            "PRIVATE-SELECTED-PROFILE-NOTE"
        );
        for alias in ["current", "requested"] {
            assert_eq!(
                setup["manager_control"][alias]["model"],
                "PRIVATE-MANAGER-PROFILE-MODEL"
            );
        }
        let sanitized = sanitize_state(serde_json::to_value(&projected).unwrap());
        let sanitized_profiles = sanitized["profile_sets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|profiles| profiles["id"] == saved_profiles.entity_id)
            .unwrap();
        assert_eq!(
            sanitized_profiles["roles"],
            "[OMITTED FROM SANITIZED EXPORT]"
        );
        assert_eq!(
            sanitized_profiles["revision_id"],
            saved_profiles.detail["revision_id"]
        );
        assert_eq!(
            sanitized_profiles["configuration_hash"],
            "export-recipe-configuration-hash"
        );
        let sanitized_recovery = sanitized["recovery"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"] == "export-guidance-recovery")
            .unwrap();
        assert_eq!(
            sanitized_recovery["retry_prerequisite"],
            "[OMITTED FROM SANITIZED EXPORT]"
        );
        assert_eq!(sanitized_recovery["attempt_id"], "export-guidance-attempt");
        assert_eq!(sanitized_recovery["state"], "attention_required");
        let sanitized_action = sanitized["continuation_actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["binding"]["recovery_id"] == "export-guidance-recovery")
            .unwrap();
        assert_eq!(
            sanitized_action["binding"]["retry_prerequisite"],
            "[OMITTED FROM SANITIZED EXPORT]"
        );
        assert_eq!(sanitized_action["binding"]["retry_enabled"], false);
        assert_eq!(
            sanitized["trip_checks"][0]["acceptance_rows"],
            serde_json::json!([
                {"status":"met"}, {"status":"unmet"}, "[OMITTED FROM SANITIZED EXPORT]"
            ])
        );
        let state = serde_json::to_string(&projected).unwrap();
        for sentinel in sentinels {
            assert!(
                state.contains(sentinel),
                "missing fixture projection {sentinel}"
            );
        }
        let mut diagnostic_attention = attention.clone();
        diagnostic_attention.details = Some("PRIVATE-PROVIDER-ERROR".into());
        let checkout_scope = crate::domain::TaskCheckoutScope {
            task_id: "export-task".into(),
            project_id: "export-project".into(),
            checkout_root: "/PRIVATE-CHECKOUT-ROOT/project".into(),
            repository_identity: "git:/PRIVATE-CHECKOUT-ROOT/project/.git".into(),
            workspace_mode: crate::domain::WorkspaceMode::MainCheckout,
            selection_revision: 1,
            selection_hash: "selection-hash".into(),
            writer_policy_hash: "writer-policy-hash".into(),
            configuration_hash: "configuration-hash".into(),
            scope_hash: "checkout-scope-hash".into(),
        };
        let aliases = sanitize_state(serde_json::json!({
            "attention": diagnostic_attention,
            "checkout_scope": checkout_scope,
        }));
        for private in ["PRIVATE-PROVIDER-ERROR", "PRIVATE-CHECKOUT-ROOT"] {
            assert!(!aliases.to_string().contains(private), "leaked {private}");
        }
        assert_eq!(
            aliases["checkout_scope"]["scope_hash"],
            "checkout-scope-hash"
        );
        assert_eq!(aliases["attention"]["target"]["task_id"], "export-task");
        let sink = DiagnosticSink::new(paths.logs.clone()).unwrap();
        std::fs::write(
            paths.logs.join("agenticjira.jsonl"),
            b"{\"private\":\"CORRUPT-PRIVATE-BYTES\xf0\x9f",
        )
        .unwrap();
        sink.record(
            "info",
            "export.fixture",
            "test",
            "ok",
            Some("export-diagnostic-operation"),
            serde_json::json!({
                "cause":"PRIVATE-DIAGNOSTIC-CAUSE /private/PRIVATE-DIAGNOSTIC-PATH.rs",
                "message":"password=\"prefix\n\\\"PRIVATE-DIAGNOSTIC-PASSWORD\"",
                "extension":{"arbitrary":"PRIVATE-DIAGNOSTIC-EXTENSION"},
                "candidate_hash":"a".repeat(64),
                "scope_hash":"PRIVATE-INVALID-HASH",
            }),
        )
        .unwrap();
        assert!(!String::from_utf8_lossy(
            &std::fs::read(paths.logs.join("agenticjira.jsonl")).unwrap()
        )
        .contains("PRIVATE-DIAGNOSTIC-PASSWORD"));
        let local_logs = offline_logs(&paths, 2_000).unwrap();
        let local_event = local_logs.last().unwrap();
        assert!(local_event["detail"]["cause"]
            .as_str()
            .unwrap()
            .contains("PRIVATE-DIAGNOSTIC-CAUSE"));
        assert_eq!(
            local_event["detail"]["extension"]["arbitrary"],
            "PRIVATE-DIAGNOSTIC-EXTENSION"
        );
        assert_eq!(local_event["detail"]["message"], r#"password="[REDACTED]""#);
        let archive = root.join("diagnostics.zip");
        export_sanitized(&paths, &archive).unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        let exported = String::from_utf8_lossy(&bytes);
        for sentinel in sentinels.into_iter().chain([
            "CORRUPT-PRIVATE-BYTES",
            "PRIVATE-DIAGNOSTIC-CAUSE",
            "PRIVATE-DIAGNOSTIC-PATH",
            "PRIVATE-DIAGNOSTIC-PASSWORD",
            "PRIVATE-DIAGNOSTIC-EXTENSION",
            "PRIVATE-INVALID-HASH",
        ]) {
            assert!(!exported.contains(sentinel), "leaked {sentinel}");
        }
        assert!(exported.contains("export-result"));
        assert!(exported.contains("export-task"));
        assert!(exported.contains("export-check"));
        assert!(exported.contains(&saved_profiles.entity_id));
        assert!(exported.contains(saved_profiles.detail["revision_id"].as_str().unwrap()));
        assert!(exported.contains("export-guidance-recovery"));
        assert!(exported.contains("export-guidance-attempt"));
        for (key, id) in [
            ("id", "export-session"),
            ("id", "export-run"),
            ("id", "export-suite"),
            ("id", "export-profile-set"),
            ("revision_id", "export-profile-revision"),
            ("id", "export-recipe"),
            ("revision_id", "export-recipe-revision"),
            ("id", "export-schedule"),
            ("id", "export-trip-config"),
            ("id", "export-lane"),
            ("id", "export-integration"),
            ("id", "export-explorer"),
            ("id", "export-cmux-surface"),
            ("id", "export-setup-stop"),
            ("id", "export-capability"),
            ("id", "export-trip-check"),
            ("id", "export-runtime-admission"),
            ("hook_event_id", "export-failure-hook"),
            ("operation_id", "export-diagnostic-operation"),
        ] {
            assert!(
                exported.contains(&format!("\"{key}\": \"{id}\"")),
                "omitted {id}"
            );
        }
        assert!(exported.contains("export-candidate-hash"));
        assert!(exported.contains("export-recipe-configuration-hash"));
        assert!(exported.contains("\"exit_code\": 7"));
        assert!(exported.contains("\"status\": \"exited\""));
        assert!(exported.contains("\"status\": \"finished\""));
        assert!(exported.contains("\"status\": \"met\""));
        assert!(exported.contains("\"state\": \"superseded\""));
        assert!(exported.contains("configuration-hash"));
        assert!(exported.contains("guidance-hash"));
        assert!(exported.contains("diagnostics.corrupt_record"));
        assert!(exported.contains("export.fixture"));
        assert!(exported.contains("export-frozen-seams-hash"));
        assert!(exported.contains("export-explorer-candidate-hash"));
        for hash in [
            "export-capability-hash",
            "export-selected-profile-hash",
            "export-manager-profile-hash",
            "export-runtime-profile-hash",
            "export-runtime-scope-hash",
        ] {
            assert!(exported.contains(hash), "omitted {hash}");
        }
        assert!(exported.contains("\"status\": \"unmet\""));
        assert!(exported.contains(&"a".repeat(64)));
        assert!(exported.contains("\"byte_offset\": 0"));
        assert!(exported.contains("\"unterminated\": false"));
        assert!(exported.contains("\"surface_state\": \"unknown\""));
        assert!(exported.contains("\"signal_delivery_state\": \"failed\""));
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
