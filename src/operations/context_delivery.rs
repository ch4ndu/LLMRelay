use super::{Application, ROLE_CONTEXT_OUTPUT_BYTES};
use crate::domain::{RoleContext, ROLE_RESULT_REPORT_CONTRACT};
use crate::providers;
use crate::store::json_hash;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::OptionalExtension;
use sha2::Digest;
use std::collections::HashSet;

const ROLE_CONTEXT_MAX_PAGES: usize = 1_000_000;

fn role_context_cursor(section: &str, page: usize, revision: &str, scope: &str) -> String {
    format!("v1:{section}:{page}:{}:{scope}", &revision[..32])
}

fn role_context_envelope(
    section: &str,
    revision: &str,
    scope: &str,
    page: usize,
    pages: usize,
    content: serde_json::Value,
) -> serde_json::Value {
    let next = (page + 1 < pages).then(|| role_context_cursor(section, page + 1, revision, scope));
    let mut envelope = serde_json::json!({"v":1,"section":section,"revision":revision,
        "scope":scope,"page":page,"pages":pages,"next":next});
    if section == "index" {
        envelope["entries"] = content;
        envelope["complete"] = (page + 1 == pages).into();
    } else {
        envelope["encoding"] = "utf8_json_chunk".into();
        envelope["chunk"] = content;
    }
    envelope
}

fn role_context_output_fits(value: &serde_json::Value) -> Result<bool> {
    Ok(serde_json::to_vec(value)?.len() + 1 <= ROLE_CONTEXT_OUTPUT_BYTES)
}

fn role_context_content_ranges(
    section: &str,
    content: &str,
    revision: &str,
    scope: &str,
) -> Result<Vec<std::ops::Range<usize>>> {
    // Reserve the longest page numbers and next cursor before choosing any chunks.
    let overhead = serde_json::to_vec(&role_context_envelope(
        section,
        revision,
        scope,
        ROLE_CONTEXT_MAX_PAGES - 2,
        ROLE_CONTEXT_MAX_PAGES,
        serde_json::json!(""),
    ))?
    .len()
        + 1;
    let capacity = ROLE_CONTEXT_OUTPUT_BYTES
        .checked_sub(overhead)
        .filter(|capacity| *capacity >= 6)
        .context("role context page metadata exceeds the output bound")?;
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (offset, character) in content.char_indices() {
        let bytes = match character {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => character.len_utf8(),
        };
        if used + bytes > capacity {
            ranges.push(start..offset);
            start = offset;
            used = 0;
        }
        used += bytes;
    }
    ranges.push(start..content.len());
    if ranges.len() > ROLE_CONTEXT_MAX_PAGES {
        bail!("role context section exceeds the supported page count")
    }
    Ok(ranges)
}

fn role_context_index_ranges(
    entries: &[serde_json::Value],
    revision: &str,
    scope: &str,
) -> Result<Vec<std::ops::Range<usize>>> {
    let mut pages = 1;
    // Total-page digit growth can change how many whole entries fit.
    for _ in 0..7 {
        let mut ranges = Vec::new();
        let mut start = 0;
        for end in 1..=entries.len() {
            let page = ranges.len();
            let sizing_pages = if end == entries.len() {
                page + 1
            } else {
                pages.max(page + 2)
            };
            let candidate = role_context_envelope(
                "index",
                revision,
                scope,
                page,
                sizing_pages,
                serde_json::json!(&entries[start..end]),
            );
            if !role_context_output_fits(&candidate)? {
                if end - 1 == start {
                    bail!("a role context index entry exceeds the output bound")
                }
                ranges.push(start..end - 1);
                start = end - 1;
                let sizing_pages = if end == entries.len() {
                    ranges.len() + 1
                } else {
                    pages.max(ranges.len() + 2)
                };
                let candidate = role_context_envelope(
                    "index",
                    revision,
                    scope,
                    ranges.len(),
                    sizing_pages,
                    serde_json::json!(&entries[start..end]),
                );
                if !role_context_output_fits(&candidate)? {
                    bail!("a role context index entry exceeds the output bound")
                }
            }
        }
        ranges.push(start..entries.len());
        if ranges.len() == pages {
            return Ok(ranges);
        }
        pages = ranges.len();
    }
    bail!("role context index page count did not stabilize")
}

fn lane_source_binding_schema() -> serde_json::Value {
    serde_json::json!({
        "anyOf":[
            {"type":"string","pattern":"^[0-9A-Fa-f]{64}$","description":"legacy regular-file SHA-256 identity"},
            {"type":"object","additionalProperties":false,"required":["kind"],"properties":{"kind":{"const":"missing"}}},
            {"type":"object","additionalProperties":false,"required":["kind","sha256"],"properties":{"kind":{"const":"file"},"sha256":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"}}},
            {"type":"object","additionalProperties":false,"required":["kind","tree_sha256","entry_count"],"properties":{"kind":{"const":"directory"},"tree_sha256":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"},"entry_count":{"type":"integer","minimum":0,"maximum":4096}}}
        ]
    })
}

fn manager_command_schemas() -> serde_json::Value {
    serde_json::json!({
        "record_explorer_decision":{
            "type":"object",
            "required":["stage","trigger","activated","census","limits"],
            "properties":{
                "stage":{"type":"string","enum":["planning","rescue","final"]},
                "trigger":{"type":"string"},
                "activated":{"type":"boolean"},
                "census":{"type":"object"},
                "limits":{"type":"object","required":["max_words"],"properties":{
                    "max_words":{"type":"integer","minimum":1,"maximum":800},
                    "question":{"type":"string","minLength":1}
                }}
            },
            "stage_trigger_catalog":{
                "planning":["multi_module","multi_platform","contract_change","repository_wide","edit_set_over_eight","large_file_refactor","ownership_unresolved","not_invoked"],
                "rescue":["repeated_gate_failure","unplanned_scope","outside_edit_set","conflicting_review_evidence","owner_unresolved","not_invoked"],
                "final":["repository_wide","three_or_more_modules","source_evidence_conflict","not_invoked"]
            },
            "constraints":[
                "the serialized Explorer decision input must be at most 131072 bytes",
                "trigger must come from the selected stage catalog",
                "activated must equal whether trigger is not not_invoked",
                "an activated decision requires a nonblank limits.question",
                "the stage must match the current workflow phase"
            ]
        },
        "structured_plan":{
            "location":"role report metadata.structured_plan when outcome is plan_ready",
            "type":"object",
            "required":["outcomes_scope","classification","ownership","acceptance_criteria","test_policy","verification_matrix","documentation","restrictions","explorer_disposition","unresolved_decisions","config_revision_id","conformance"],
            "properties":{
                "outcomes_scope":{"type":"object"},
                "classification":{"type":"string","enum":["bounded","broad","program_sized"]},
                "ownership":{"type":"object","properties":{"lanes":{"type":"array","minItems":2,"items":{"type":"object",
                    "required":["lane_key","owned_paths","shared_paths","dependencies"],
                    "properties":{
                        "lane_key":{"type":"string","pattern":"^[a-z0-9_]+$"},
                        "owned_paths":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"string"}},
                        "shared_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                        "protected_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                        "dependencies":{"type":"array","uniqueItems":true,"items":{"type":"string"}}
                    }
                }}}},
                "acceptance_criteria":{"type":"array"},
                "test_policy":{"type":"object","required":["coverage"],"properties":{"coverage":{"type":"string"}}},
                "verification_matrix":{"type":"array","items":{"type":"object","required":["check_id"],"properties":{"check_id":{"type":"string","minLength":1}}}},
                "documentation":{"type":"object"},
                "restrictions":{"type":"array"},
                "explorer_disposition":{"type":"object","required":["decision_id","activated"],"properties":{"decision_id":{"type":"string","minLength":1},"activated":{"type":"boolean"}}},
                "unresolved_decisions":{"type":"array","maxItems":0},
                "config_revision_id":{"type":"string","minLength":1},
                "conformance":{"type":"object"}
            },
            "report_siblings":{
                "plan":{"type":"string","minLength":1,"maxUtf8Bytes":262144}
            },
            "constraints":[
                "metadata.plan must be nonblank after trimming and at most 262144 UTF-8 bytes",
                "metadata.structured_plan serialized JSON must be at most 262144 bytes",
                "metadata.structured_plan object keys must not recursively contain the case-insensitive substrings credential, secret, token, hidden_reasoning, or chain_of_thought",
                "acceptance_criteria must exactly equal task.acceptance_criteria in order",
                "test_policy.coverage must equal project_policy.testing.coverage",
                "config_revision_id must equal project_policy.config_revision_id",
                "verification_matrix check IDs as a set must exactly equal selected_checks.check_ids at selected_checks.revision",
                "explorer_disposition must bind the current planning decision and its actual activation/evidence state",
                "ownership.lanes is optional for the default single-lane flow; when present it requires at least two uniquely named explicit lanes",
                "each ownership lane requires unique normalized owned_paths, shared_paths, and dependencies; protected_paths is optional but must contain unique normalized paths when present",
                "each owned path must be non-overlapping with every other owned, shared, or protected path in the same or another lane; shared and protected scopes may overlap one another",
                "dependencies must uniquely name other reviewed lanes, must not name the same lane, and must be acyclic",
                "no unresolved decision may remain"
            ]
        },
        "configure_lanes":{
            "type":"object","required":["lanes"],
            "properties":{"lanes":{"type":"array","minItems":2,"items":{"type":"object",
                "required":["lane_key","owned_paths","shared_paths","protected_paths","dependencies"],
                "properties":{
                    "lane_key":{"type":"string","pattern":"^[a-z0-9_]+$"},
                    "owned_paths":{"type":"array","minItems":1,"uniqueItems":true,"items":{"type":"string"}},
                    "shared_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                    "protected_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                    "dependencies":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                    "source_hashes":{"type":"object","minProperties":1,"maxProperties":4096,"additionalProperties":lane_source_binding_schema()},
                    "frozen_seams_hash":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"}
                }
            }}},
            "constraints":["the serialized lane configuration input must be at most 131072 bytes","requires the exact approved structured plan and human implementation authorization","lane_key, owned_paths, shared_paths, protected_paths, and dependencies must exactly match approved structured plan ownership; omitted reviewed protected_paths is admitted as an empty array","all paths must be normalized relative paths; each owned path must be non-overlapping with every other owned, shared, or protected path in the same or another lane; shared and protected scopes may overlap one another","normally omit both source_hashes and frozen_seams_hash; the authenticated service computes and freezes typed current bindings for every exact reviewed owned, shared, and protected scope","if either dynamic field is supplied then both are required; explicit source bindings must exactly cover the reviewed scopes and each must match current missing, regular-file, or bounded directory state","missing, regular-file, and directory identities are distinct; directory identities bind contained membership, entry types, and file hashes without projecting target contents","frozen_seams_hash is SHA-256 of the lane shared_paths and their current shared source bindings","initial lane dispatch rechecks every admitted source binding and exact scope coverage before creating authority; retained resume and yield use the already-admitted lane authority","dependencies must uniquely name other reviewed lanes, must not name the same lane, and must be acyclic","lane admission is immutable after the first successful configuration"]
        },
        "request_integration":{
            "type":"object","required":["capsule"],
            "properties":{"capsule":{"type":"object","required":["ordered_lanes","merge_strategy","verification_boundary"],"properties":{
                "ordered_lanes":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
                "merge_strategy":{"anyOf":[{"type":"string","minLength":1},{"type":"object","minProperties":1}]},
                "verification_boundary":{"anyOf":[{"type":"string","minLength":1},{"type":"object","minProperties":1}]}
            }}},
            "constraints":["the capsule serialized JSON must be at most 131072 bytes","capsule object keys must not recursively contain the case-insensitive substrings credential, secret, token, hidden_reasoning, or chain_of_thought","merge_strategy and verification_boundary must each be either a string that is nonblank after trimming or a nonempty object","every configured required lane must already be yielded","ordered_lanes must name every configured required lane exactly once","all lane writers must be quiescent","the service verifies the actual worktree scope before recording the request"]
        },
        "select_checks":{
            "type":"object","required":["check_ids"],
            "properties":{"check_ids":{"type":"array","uniqueItems":true,"items":{"type":"string"}}},
            "constraints":["every ID must be present in verification_catalog.checks for its exact activated config revision","selection is planning-only and each accepted call creates the next immutable revision","an empty selection remains an explicit revision and blocks verified completion"]
        },
        "submit_conformance":{
            "type":"object",
            "required":["candidate_hash","config_hash","acceptance","ownership","documentation","test_policy","readability"],
            "properties":{
                "candidate_hash":{"type":"string","minLength":1},
                "config_hash":{"type":"string","minLength":1},
                "acceptance":{"type":"array","items":{"type":"object","required":["criterion","evidence"],"properties":{"criterion":{"type":"string"},"evidence":{"type":"array","minItems":1,"maxItems":64,"items":{"anyOf":[{"type":"string","minLength":1,"maxLength":4096,"pattern":"\\S"},{"type":"object","minProperties":1}]}}}}},
                "ownership":{"type":"object","minProperties":1,"maxProperties":32},
                "documentation":{"type":"object","minProperties":1,"maxProperties":32},
                "test_policy":{"type":"object","minProperties":1,"maxProperties":32},
                "readability":{"type":"object","minProperties":1,"maxProperties":32}
            },
            "constraints":["the serialized conformance input must be at most 262144 bytes","ownership, documentation, test_policy, and readability must each serialize to at most 16384 bytes and contain only nonblank keys with meaningful nonempty values","candidate_hash and config_hash must match the current attempt and activated project configuration","acceptance rows must exactly cover task.acceptance_criteria in order; each row requires 1 to 64 evidence items, each either a nonblank string of at most 4096 characters or a meaningful nonempty object of at most 16384 bytes","all required lanes must have yielded and all implementer writers must be quiescent"]
        }
    })
}

fn lane_yield_schema() -> serde_json::Value {
    serde_json::json!({
        "type":"object",
        "required":["source_hashes","changed_paths","agent_claimed_output_hash"],
        "properties":{
            "source_hashes":{"type":"object","maxProperties":4096,"additionalProperties":lane_source_binding_schema()},
            "changed_paths":{"type":"array","uniqueItems":true,"items":{"type":"string"}},
            "agent_claimed_output_hash":{"type":"string","pattern":"^[0-9A-Fa-f]{64}$"}
        },
        "constraints":["the serialized lane-yield input must be at most 131072 bytes","the source_hashes object must exactly equal the admitted lane source hashes","changed paths must remain within owned paths and outside protected paths","only the current active generation for the exact lane may yield"]
    })
}

impl Application {
    pub fn role_context_payload(
        &self,
        context: &crate::domain::RoleContext,
    ) -> Result<serde_json::Value> {
        use rusqlite::params;
        let connection = self.store.lock()?;
        // Runtime recall contexts fail closed before any ordinary task policy or
        // catalog data is assembled. The resumed turn must rely only on retained
        // native-conversation recall.
        let setup_contract = crate::trip::setup_context(&connection, context)?;
        if setup_contract
            .as_ref()
            .is_some_and(|contract| contract["qualification_contract"] == "functional-v2")
        {
            let executable = providers::shell_quote(self.executable.to_string_lossy().as_ref());
            let (mode, phase) = if context.role == crate::domain::RoleKind::FinalReviewer {
                (
                    "fresh",
                    "Report only after the prescribed witnesses in this fresh invocation.",
                )
            } else {
                ("retained", "Do not report on this first invocation; retain the binding and challenge for the service-owned recall turn.")
            };
            let value = serde_json::json!({
                "context_scope":"functional_qualification",
                "identity":context,
                "setup_contract":setup_contract,
                "commands":{
                    "context":format!("{executable} role context"),
                    "report":format!("{executable} role report --functional-v2 --operation-id=<new-safe-id> --status=passed --binding-hash=<setup_contract.binding_hash> --challenge=<exact-read-content> --session-mode={mode}"),
                    "reporting_contract":format!("{phase} {}", crate::trip::FUNCTIONAL_REPORT_SCHEMA.replace("<exact-executable>", &executable)),
                },
            });
            if serde_json::to_vec(&value)?.len() > 1024 * 1024 {
                bail!("task-scoped role context exceeds the 1 MiB response bound")
            }
            return Ok(value);
        }
        let isolated_validation: bool = connection.query_row(
            "SELECT a.status='capability_validation' FROM attempts a
             JOIN tasks t ON t.id=a.task_id
             WHERE a.id=?1 AND t.id=?2 AND t.project_id=?3",
            params![context.attempt_id, context.task_id, context.project_id],
            |row| row.get(0),
        )?;
        let ordinary_task = setup_contract.is_none() && !isolated_validation;
        let task:String=connection.query_row("SELECT json_object('id',id,'project_id',project_id,'title',title,'description',description,'acceptance_criteria',json(acceptance_criteria_json),'priority',priority,'lifecycle',lifecycle,'attention',attention,'version',version) FROM tasks WHERE id=?1",params![context.task_id],|row|row.get(0))?;
        let attempt:String=connection.query_row("SELECT json_object('id',id,'phase',phase,'status',status,'base_revision',base_revision,'plan_hash',plan_hash,'candidate_hash',candidate_hash,'accepted_snapshot_id',accepted_snapshot_id,'parent_attempt_id',parent_attempt_id,'scope_hash',scope_hash,'configuration_hash',configuration_hash,'workflow_version',workflow_version,'workflow_hash',workflow_hash) FROM attempts WHERE id=?1",params![context.attempt_id],|row|row.get(0))?;
        let plan_record: Option<(String, String)> = connection
            .query_row(
                "SELECT s.id,s.manifest_hash FROM snapshots s
             JOIN attempts a ON a.id=s.attempt_id JOIN tasks t ON t.id=a.task_id
             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND s.kind='plan'
               AND s.complete=1 AND s.manifest_hash=a.plan_hash",
                params![context.project_id, context.task_id, context.attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let current_review:Option<(String,String,String,Option<String>,String,String)>=connection.query_row(
            "SELECT r.id,r.review_kind,r.candidate_hash,a.plan_hash,a.phase,
             json_object('id',r.id,'review_kind',r.review_kind,'candidate_hash',r.candidate_hash,
               'prompt',r.prompt_text,'handoff',json(COALESCE(r.handoff_json,'{}')),
               'delivery_state',r.delivery_state,'resume_count',r.resume_count)
             FROM review_requests r JOIN attempts a ON a.id=r.attempt_id
             JOIN tasks t ON t.id=a.task_id
             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND r.role_generation_id=?4
               AND r.review_kind=CASE ?5 WHEN 'plan_reviewer' THEN 'plan'
                 WHEN 'code_reviewer' THEN 'code' WHEN 'final_verifier' THEN 'final' ELSE '' END
               AND r.delivery_state IN ('launching','delivered','ambiguous')
               AND r.candidate_hash=CASE r.review_kind WHEN 'plan' THEN a.plan_hash ELSE a.candidate_hash END
             ORDER BY r.created_at DESC LIMIT 1",
            params![context.project_id,context.task_id,context.attempt_id,context.role_generation_id,context.role.to_string()],
            |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))
        ).optional()?;
        let completed_final_review: Option<String> = if context.role
            == crate::domain::RoleKind::Manager
        {
            connection.query_row("SELECT json_object('id',r.id,'candidate_hash',r.candidate_hash,'reviewer_generation_id',r.role_generation_id,'verdict',r.verdict,'delivery_state',r.delivery_state,'updated_at',r.updated_at) FROM review_requests r JOIN attempts a ON a.id=r.attempt_id WHERE r.attempt_id=?1 AND r.review_kind='final' AND r.candidate_hash=a.candidate_hash AND r.verdict='approved' AND r.delivery_state='finished' ORDER BY r.updated_at DESC LIMIT 1",params![context.attempt_id],|row|row.get(0)).optional()?
        } else {
            None
        };
        let feedback = {
            let mut statement = connection.prepare(
                "SELECT json_object('request_id',r.id,'kind',r.review_kind,'verdict',r.verdict,
                   'feedback',r.feedback,'candidate_hash',r.candidate_hash,'result',json((
                     SELECT json_object('result_id',rr.id,'summary',rr.summary,
                       'evidence',json(rr.evidence_json),'metadata',json(rr.metadata_json))
                     FROM role_results rr JOIN role_generations g ON g.id=rr.role_generation_id
                     WHERE rr.session_id=r.session_id AND rr.role_generation_id=r.role_generation_id
                       AND g.attempt_id=r.attempt_id AND rr.outcome=r.verdict
                       AND g.role=CASE r.review_kind WHEN 'plan' THEN 'plan_reviewer'
                         WHEN 'code' THEN 'code_reviewer' WHEN 'final' THEN 'final_verifier' END
                       AND rr.consumed_at IS NOT NULL
                       AND json_extract(rr.metadata_json,'$.review_request_id')=r.id
                       AND json_extract(rr.metadata_json,'$.review_kind')=r.review_kind
                       AND json_extract(rr.metadata_json,'$.candidate_hash')=r.candidate_hash
                     ORDER BY rr.created_at DESC,rr.id DESC LIMIT 1)))
                 FROM review_requests r WHERE r.attempt_id=?1 AND r.delivery_state='finished'
                   AND r.verdict!='approved' ORDER BY r.updated_at DESC LIMIT 10",
            )?;
            let rows = statement
                .query_map(params![context.attempt_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let checks = {
            let mut statement=connection.prepare("SELECT json_object('id',c.id,'check_id',c.check_id,'candidate_hash',c.candidate_hash,'selected_check_revision',c.selected_check_revision,'suite_name',c.suite_name,'suite_version',c.check_suite_version,'executable',c.executable,'arguments',json(c.arguments_json),'status',c.status,'launch_state',c.launch_state,'launch_error',c.launch_error,'exit_code',c.exit_code,'inputs_hash',c.inputs_hash,'acceptance_coverage',json(c.acceptance_coverage_json),'elapsed_millis',c.elapsed_millis,'freshness_state',c.freshness_state,'evidence',json(c.evidence_json),'created_at',c.created_at,'finished_at',c.finished_at) FROM check_runs c JOIN attempts a ON a.id=c.attempt_id JOIN tasks t ON t.id=a.task_id WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND c.candidate_hash=a.candidate_hash ORDER BY c.created_at DESC LIMIT 20")?;
            let rows = statement
                .query_map(
                    params![context.project_id, context.task_id, context.attempt_id],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let budgets = {
            let mut statement=connection.prepare("SELECT json_object('kind',review_kind,'initial',initial_allowance,'extension',extension_allowance,'spent',spent) FROM review_budgets WHERE attempt_id=?1 ORDER BY review_kind")?;
            let rows = statement
                .query_map(params![context.attempt_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let final_repair_recheck: Option<String> = connection
            .query_row(
                "SELECT json_object('accounting_lane','final_repair_recheck','allowance',1,
                   'provenance_kind',provenance_kind,
                   'spent',CASE WHEN spent_at IS NULL THEN 0 ELSE 1 END,'state',state,
                   'candidate_hash',candidate_hash,'review_request_id',review_request_id,
                   'verdict',verdict,'closed_reason',closed_reason,
                   'invalid_ordinary_review_request_id',rejected_code_request_id,
                   'ordinary_review_budget','separate and never spent by this lane')
                 FROM final_repair_rechecks WHERE attempt_id=?1",
                params![context.attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let guidance = if context.role == crate::domain::RoleKind::Manager {
            let mut statement=connection.prepare("SELECT json_object(
                'id',g.id,'body',g.body,'state',g.state,'reason',g.reason,
                'created_at',g.created_at,'written_at',g.written_at,'submitted_at',g.submitted_at,
                'acknowledgement_eligible',json(CASE WHEN g.state='submitted'
                  AND g.delivery_session_id=?2
                  AND g.delivery_transcript_epoch=(SELECT s.transcript_epoch FROM sessions s WHERE s.id=?2)
                  AND g.delivery_resume_invocation_id IS (
                    SELECT ri.id FROM resume_invocations ri JOIN sessions s ON s.id=?2
                    WHERE ri.session_id=s.id AND ri.transcript_epoch=s.transcript_epoch
                    ORDER BY ri.resume_ordinal DESC LIMIT 1)
                  THEN 'true' ELSE 'false' END))
                FROM guidance_messages g WHERE g.role_generation_id=?1 AND g.state NOT IN ('acknowledged')
                ORDER BY g.created_at LIMIT 50")?;
            let rows = statement
                .query_map(
                    params![context.role_generation_id, context.session_id],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        } else {
            Vec::new()
        };
        let task_profiles = {
            let mut statement=connection.prepare("SELECT json_object('role',ap.role,'settings_revision',ap.settings_revision,'source',ap.source,'profile',json(ap.profile_json),'profile_hash',ap.profile_hash,'base_project_config_revision_id',ap.project_config_revision_id,'base_project_configuration_hash',ap.project_configuration_hash,'adapter',ap.adapter_name,'adapter_hash',ap.adapter_hash,'capability_id',ap.capability_id,'capability_key',ap.capability_key,'capability_proof_hash',ap.capability_proof_hash,'effective_for_this_generation',ap.role=?2 AND ap.settings_revision=(SELECT config_revision FROM role_generations WHERE id=?3)) FROM trip_attempt_profiles ap WHERE ap.attempt_id=?1 ORDER BY ap.role")?;
            let rows = statement
                .query_map(
                    params![
                        context.attempt_id,
                        context.role.to_string(),
                        context.role_generation_id
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let (project_policy, verification_catalog, selected_checks, explorer_decisions) =
            if ordinary_task {
                let (config_revision_id, config_revision, configuration_hash, config_json):(
                    String,i64,String,String
                )=connection.query_row(
                    "SELECT s.active_config_revision_id,r.revision,r.configuration_hash,r.config_json
                     FROM attempts a JOIN tasks t ON t.id=a.task_id
                     JOIN trip_project_state s ON s.project_id=t.project_id
                     JOIN trip_config_revisions r ON r.id=s.active_config_revision_id AND r.project_id=t.project_id
                     WHERE a.id=?1 AND t.id=?2 AND t.project_id=?3
                       AND s.readiness='ready' AND r.state='activated'",
                    params![context.attempt_id,context.task_id,context.project_id],
                    |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
                )?;
                let config: serde_json::Value = serde_json::from_str(&config_json)?;
                let testing = config
                    .get("testing")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        anyhow!("activated project configuration lacks structured testing policy")
                    })?;
                testing
                    .get("coverage")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow!("activated project configuration lacks testing coverage")
                    })?;
                let documentation = config
                    .get("documentation")
                    .filter(|value| value.is_object())
                    .ok_or_else(|| {
                        anyhow!(
                            "activated project configuration lacks structured documentation policy"
                        )
                    })?;
                let guidance_paths = config
                    .get("guidance")
                    .filter(|value| value.is_array())
                    .ok_or_else(|| {
                        anyhow!("activated project configuration lacks guidance paths")
                    })?;
                let policy = serde_json::json!({
                    "config_revision_id":config_revision_id,
                    "config_revision":config_revision,
                    "configuration_hash":configuration_hash,
                    "testing":testing,
                    "documentation":documentation,
                    "guidance":{
                        "configured_paths":guidance_paths,
                        "content_available_in_role_context":false,
                        "boundary":"These are reviewed project-relative guidance paths. Ordinary role context does not perform target-file reads; use content already assigned in the workspace and report unavailable content honestly. setup-read is setup-discovery-only."
                    }
                });
                let catalog = {
                    let mut statement=connection.prepare(
                        "SELECT json_object('id',c.id,'config_revision_id',c.config_revision_id,
                         'check_key',c.check_key,'category',c.category,'command_kind',c.command_kind,
                         'executable',c.executable,'arguments',json(c.arguments_json),
                         'shell_command',c.shell_command,'cwd',c.cwd,'timeout_seconds',c.timeout_seconds,
                         'acceptance_criteria',json(c.acceptance_rows_json),
                         'relevant_inputs',json(c.relevant_inputs_json),'invalidation',json(c.invalidation_json),
                         'original_text',c.original_text)
                         FROM trip_verification_checks c
                         JOIN trip_project_state s ON s.project_id=c.project_id
                         JOIN attempts a ON a.id=?1 JOIN tasks t ON t.id=a.task_id
                         WHERE t.id=?2 AND t.project_id=?3 AND c.project_id=t.project_id
                           AND c.config_revision_id=s.active_config_revision_id AND c.enabled=1
                         ORDER BY c.category,c.check_key,c.id")?;
                    let rows = statement
                        .query_map(
                            params![context.attempt_id, context.task_id, context.project_id],
                            |row| row.get::<_, String>(0),
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let selected_revision: i64 = connection.query_row(
                    "SELECT selected_checks_revision FROM attempts WHERE id=?1 AND task_id=?2",
                    params![context.attempt_id, context.task_id],
                    |row| row.get(0),
                )?;
                let selected_ids = if selected_revision == 0 {
                    Vec::new()
                } else {
                    let mut statement = connection.prepare(
                        "SELECT check_id FROM trip_selected_checks
                         WHERE attempt_id=?1 AND revision=?2 AND required=1 ORDER BY check_id",
                    )?;
                    let rows = statement
                        .query_map(params![context.attempt_id, selected_revision], |row| {
                            row.get::<_, String>(0)
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let decisions = {
                    let mut statement=connection.prepare(
                        "SELECT json_object('decision_id',id,'stage',stage,'trigger',trigger,
                         'activated',json(CASE activated WHEN 1 THEN 'true' ELSE 'false' END),
                         'census',json(census_json),'limits',json(limits_json),'candidate_hash',candidate_hash,
                         'evidence_submitted',json(CASE WHEN outcome_json IS NULL THEN 'false' ELSE 'true' END))
                         FROM trip_explorer_decisions WHERE attempt_id=?1
                         ORDER BY created_at,id LIMIT 10")?;
                    let rows = statement
                        .query_map(params![context.attempt_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                (
                    Some(policy),
                    Some(serde_json::json!({
                        "scope":{
                            "project_id":context.project_id,
                            "task_id":context.task_id,
                            "attempt_id":context.attempt_id,
                            "config_revision_id":config_revision_id
                        },
                        "checks":catalog.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?
                    })),
                    Some(serde_json::json!({
                        "revision":selected_revision,
                        "check_ids":selected_ids,
                        "config_revision_id":config_revision_id
                    })),
                    decisions
                        .into_iter()
                        .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                        .collect::<serde_json::Result<Vec<_>>>()?,
                )
            } else {
                (None, None, None, Vec::new())
            };
        let current_explorer_evidence = if ordinary_task {
            let current: Option<(
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                bool,
            )> = connection.query_row(
                "SELECT d.id,d.stage,d.candidate_hash,d.role_generation_id,d.outcome_json,
                   EXISTS(SELECT 1 FROM role_generations g WHERE g.id=d.role_generation_id
                     AND g.attempt_id=a.id AND g.role='explorer' AND g.status='exited')
                   AND EXISTS(SELECT 1 FROM sessions s WHERE s.role_generation_id=d.role_generation_id)
                   AND NOT EXISTS(SELECT 1 FROM sessions s WHERE s.role_generation_id=d.role_generation_id
                     AND (s.status!='exited' OR COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)!=1))
                 FROM trip_explorer_decisions d JOIN attempts a ON a.id=d.attempt_id
                 JOIN tasks t ON t.id=a.task_id
                 WHERE a.id=?1 AND t.id=?2 AND t.project_id=?3 AND d.activated=1
                   AND d.candidate_hash IS a.candidate_hash
                 ORDER BY d.created_at DESC,d.id DESC LIMIT 1",
                params![context.attempt_id, context.task_id, context.project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            ).optional()?;
            current
                .map(|(id, stage, candidate, generation, outcome, quiescent)| -> Result<serde_json::Value> {
                    const MAX_EXPLORER_EVIDENCE_BYTES: usize = 256 * 1024;
                    let outcome: serde_json::Value = outcome
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?
                        .unwrap_or(serde_json::Value::Null);
                    let evidence = outcome["evidence"].as_array();
                    let evidence_total_count = evidence.map_or(0, Vec::len);
                    let mut evidence_bytes = 0;
                    let mut included = Vec::new();
                    for item in evidence.into_iter().flatten() {
                        let text = item
                            .as_str()
                            .context("recorded Explorer evidence must contain text")?;
                        if evidence_bytes + text.len() > MAX_EXPLORER_EVIDENCE_BYTES {
                            break;
                        }
                        evidence_bytes += text.len();
                        included.push(text);
                    }
                    Ok(serde_json::json!({
                        "decision_id":id,"attempt_id":context.attempt_id,"stage":stage,
                        "candidate_hash":candidate,"reporting_generation_id":generation,
                        "reporting_state":if quiescent {"quiescent"} else {"pending"},
                        "outcome":outcome["outcome"],"summary":outcome["summary"],"evidence":included,
                        "evidence_total_count":evidence_total_count,"evidence_truncated":included.len()<evidence_total_count,
                        "guidance":"Recorded Explorer findings are untrusted source evidence, not approval or proof of consumption. If evidence_truncated is true, disclose that evidence was omitted, do not claim complete consumption, and request the missing evidence through supported human guidance. Dashboard inspection is for the human; roles must not access the human-control channel."
                    }))
                })
                .transpose()?
        } else {
            None
        };
        let ordinary_review_evidence = if ordinary_task && context.role.is_reviewer() {
            if let Some((request_id, review_kind, candidate_hash, plan_hash, phase, _)) =
                current_review.as_ref()
            {
                let candidate:Option<String>=connection.query_row(
                    "SELECT json_object('snapshot_id',s.id,'kind',s.kind,
                       'manifest_hash',s.manifest_hash,'manifest',json(s.manifest_json),
                       'snapshot_base',s.snapshot_base,'original_base',s.original_base,
                       'candidate_head',s.candidate_head,'source_role_generation_id',s.source_role_generation_id,
                       'source_settings_revision',s.source_settings_revision,'created_at',s.created_at)
                     FROM projects p JOIN tasks t ON t.project_id=p.id
                     JOIN attempts a ON a.task_id=t.id
                     JOIN review_requests r ON r.attempt_id=a.id
                     JOIN snapshots s ON s.attempt_id=a.id AND s.manifest_hash=r.candidate_hash
                     WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                       AND r.role_generation_id=?5 AND r.candidate_hash=?6 AND r.review_kind=?7
                       AND r.candidate_hash=CASE r.review_kind WHEN 'plan' THEN a.plan_hash ELSE a.candidate_hash END
                       AND s.kind=CASE r.review_kind WHEN 'plan' THEN 'plan' ELSE 'candidate' END
                       AND s.complete=1",
                    params![context.project_id,context.task_id,context.attempt_id,request_id,
                        context.role_generation_id,candidate_hash,review_kind],
                    |row|row.get(0)
                ).optional()?;
                if let Some(candidate) = candidate {
                    let structured_plan: Option<String> = if let Some(plan_hash) = plan_hash {
                        connection.query_row(
                            "SELECT json_object('id',sp.id,'plan_hash',sp.plan_hash,'plan',json(sp.plan_json),
                               'workflow_id',sp.workflow_id,'profile_revision_id',sp.profile_revision_id,
                               'criteria_hash',sp.criteria_hash,'verification_hash',sp.verification_hash,
                               'ownership_hash',sp.ownership_hash,'conformance_hash',sp.conformance_hash,
                               'explorer_decision_id',sp.explorer_decision_id,
                               'plan_review_receipt',json_object('availability',CASE WHEN sp.reviewed_at IS NULL THEN 'unavailable' ELSE 'recorded' END,'review_request_id',sp.review_request_id,'at',sp.reviewed_at),
                               'human_plan_approval_receipt',json_object('availability',CASE WHEN sp.approved_at IS NULL THEN 'unavailable' ELSE 'recorded' END,'at',sp.approved_at),
                               'implementation_authorization_receipt',json_object('availability',CASE WHEN sp.implementation_authorized_at IS NULL THEN 'unavailable' ELSE 'recorded' END,'at',sp.implementation_authorized_at),
                               'created_at',sp.created_at)
                             FROM projects p JOIN tasks t ON t.project_id=p.id
                             JOIN attempts a ON a.task_id=t.id
                             JOIN trip_structured_plans sp ON sp.id=a.structured_plan_id AND sp.attempt_id=a.id AND sp.plan_hash=a.plan_hash
                             JOIN review_requests r ON r.attempt_id=a.id
                             WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                               AND r.role_generation_id=?5 AND r.candidate_hash=?6
                               AND a.plan_hash=?7",
                            params![context.project_id,context.task_id,context.attempt_id,request_id,
                                context.role_generation_id,candidate_hash,plan_hash],
                            |row|row.get(0)
                        ).optional()?
                    } else {
                        None
                    };
                    let lanes = if let Some(plan_hash) = plan_hash {
                        let mut statement=connection.prepare(
                            "SELECT json_object('id',l.id,'lane_key',l.lane_key,
                               'owned_paths',json(l.owned_paths_json),'shared_paths',json(l.shared_paths_json),
                               'protected_paths',json(l.protected_paths_json),'dependencies',json(l.dependencies_json),
                               'source_hashes',json(l.source_hashes_json),'frozen_seams_hash',l.frozen_seams_hash,
                               'required',json(CASE l.required WHEN 1 THEN 'true' ELSE 'false' END),
                               'state',l.state,'effective_generation_id',lg.effective_generation_id,
                               'pending_settings_revision',lg.pending_settings_revision,
                               'yielded_at',l.yielded_at,'yield_receipt',json(l.receipt_json))
                             FROM projects p JOIN tasks t ON t.project_id=p.id
                             JOIN attempts a ON a.task_id=t.id
                             JOIN trip_structured_plans sp ON sp.id=a.structured_plan_id AND sp.attempt_id=a.id AND sp.plan_hash=a.plan_hash
                             JOIN review_requests r ON r.attempt_id=a.id
                             JOIN implementation_lanes l ON l.attempt_id=a.id
                             LEFT JOIN lane_generations lg ON lg.lane_id=l.id
                             WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                               AND r.role_generation_id=?5 AND r.candidate_hash=?6 AND a.plan_hash=?7
                             ORDER BY l.lane_key")?;
                        let rows = statement
                            .query_map(
                                params![
                                    context.project_id,
                                    context.task_id,
                                    context.attempt_id,
                                    request_id,
                                    context.role_generation_id,
                                    candidate_hash,
                                    plan_hash
                                ],
                                |row| row.get::<_, String>(0),
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows
                    } else {
                        Vec::new()
                    };
                    let integration: Option<String> = if let Some(plan_hash) = plan_hash {
                        connection.query_row(
                            "SELECT json_object('id',i.id,'state',i.state,'capsule',json(i.capsule_json),
                               'requested_by_generation_id',i.requested_by_generation_id,
                               'created_at',i.created_at,'dispatched_at',i.dispatched_at)
                             FROM projects p JOIN tasks t ON t.project_id=p.id
                             JOIN attempts a ON a.task_id=t.id
                             JOIN trip_structured_plans sp ON sp.id=a.structured_plan_id AND sp.attempt_id=a.id AND sp.plan_hash=a.plan_hash
                             JOIN review_requests r ON r.attempt_id=a.id
                             JOIN trip_integration_requests i ON i.attempt_id=a.id
                             WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND r.id=?4
                               AND r.role_generation_id=?5 AND r.candidate_hash=?6 AND a.plan_hash=?7",
                            params![context.project_id,context.task_id,context.attempt_id,request_id,
                                context.role_generation_id,candidate_hash,plan_hash],
                            |row|row.get(0)
                        ).optional()?
                    } else {
                        None
                    };
                    let approved_code_review: Option<String> = connection.query_row(
                        "SELECT json_object('availability','recorded','id',code.id,
                           'candidate_hash',code.candidate_hash,'reviewer_generation_id',code.role_generation_id,
                           'verdict',code.verdict,'delivery_state',code.delivery_state,'updated_at',code.updated_at)
                         FROM projects p JOIN tasks t ON t.project_id=p.id
                         JOIN attempts a ON a.task_id=t.id
                         JOIN review_requests code ON code.attempt_id=a.id
                         WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND a.candidate_hash=?4
                           AND code.review_kind='code' AND code.candidate_hash=a.candidate_hash
                           AND code.verdict='approved' AND code.delivery_state='finished'
                         ORDER BY code.updated_at DESC LIMIT 1",
                        params![context.project_id,context.task_id,context.attempt_id,candidate_hash],
                        |row|row.get(0)
                    ).optional()?;
                    let manager_conformance: Option<String> = connection.query_row(
                        "SELECT json_object('availability','recorded','evidence_kind','manager_submitted_assertions','id',c.id,'revision',c.revision,
                           'candidate_hash',c.candidate_hash,'config_hash',c.config_hash,
                           'active_config_revision_id',s.active_config_revision_id,
                           'active_configuration_hash',r.configuration_hash,
                           'acceptance',json(c.acceptance_json),'ownership',json(c.ownership_json),
                           'documentation',json(c.documentation_json),'test_policy',json(c.test_policy_json),
                           'readability',json(c.readability_json),
                           'submitted_by_generation_id',c.submitted_by_generation_id,'created_at',c.created_at)
                         FROM projects p JOIN tasks t ON t.project_id=p.id
                         JOIN attempts a ON a.task_id=t.id
                         JOIN trip_project_state s ON s.project_id=p.id AND s.readiness='ready'
                         JOIN trip_config_revisions r ON r.id=s.active_config_revision_id
                           AND r.project_id=p.id AND r.state='activated'
                         JOIN trip_conformance_receipts c ON c.attempt_id=a.id
                           AND c.revision=a.manager_conformance_revision
                           AND c.candidate_hash=a.candidate_hash AND c.config_hash=r.configuration_hash
                         WHERE p.id=?1 AND t.id=?2 AND a.id=?3 AND a.candidate_hash=?4",
                        params![context.project_id,context.task_id,context.attempt_id,candidate_hash],
                        |row|row.get(0)
                    ).optional()?;
                    let current_selected_runs = {
                        let mut statement=connection.prepare(
                            "SELECT json_object('id',c.id,'check_id',c.check_id,
                               'candidate_hash',c.candidate_hash,'selected_check_revision',c.selected_check_revision,
                               'suite_name',c.suite_name,'suite_version',c.check_suite_version,
                               'executable',c.executable,'arguments',json(c.arguments_json),
                               'status',c.status,'launch_state',c.launch_state,'launch_error',c.launch_error,
                               'exit_code',c.exit_code,'inputs_hash',c.inputs_hash,
                               'acceptance_coverage',json(c.acceptance_coverage_json),
                               'elapsed_millis',c.elapsed_millis,'freshness_state',c.freshness_state,
                               'evidence',json(c.evidence_json),
                               'created_at',c.created_at,'finished_at',c.finished_at)
                             FROM trip_selected_checks selected
                             JOIN attempts a ON a.id=selected.attempt_id
                             JOIN tasks t ON t.id=a.task_id
                             JOIN check_runs c ON c.id=(SELECT latest.id FROM check_runs latest
                               WHERE latest.attempt_id=a.id AND latest.candidate_hash=a.candidate_hash
                                 AND latest.check_id=selected.check_id
                                 AND latest.selected_check_revision=selected.revision
                                 AND latest.status!='retry_superseded'
                               ORDER BY latest.created_at DESC,latest.id DESC LIMIT 1)
                             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3
                               AND selected.revision=a.selected_checks_revision AND selected.required=1
                             ORDER BY selected.check_id")?;
                        let rows = statement
                            .query_map(
                                params![context.project_id, context.task_id, context.attempt_id],
                                |row| row.get::<_, String>(0),
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows.into_iter()
                            .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                            .collect::<serde_json::Result<Vec<_>>>()?
                    };
                    let selected_ids = selected_checks
                        .as_ref()
                        .and_then(|value| value["check_ids"].as_array())
                        .cloned()
                        .unwrap_or_default();
                    let selected_revision = selected_checks
                        .as_ref()
                        .and_then(|value| value["revision"].as_i64())
                        .unwrap_or(0);
                    let failed = current_selected_runs.iter().any(|run| {
                        matches!(
                            run["status"].as_str(),
                            Some(
                                "failed"
                                    | "launch_ambiguous"
                                    | "precondition_failed"
                                    | "launch_failed"
                                    | "recovery_required"
                            )
                        ) || run["launch_state"] == "capture_failed"
                            || (run["status"] == "finished" && run["exit_code"].as_i64() != Some(0))
                    });
                    let passed = !selected_ids.is_empty()
                        && selected_ids.iter().all(|id| {
                            current_selected_runs.iter().any(|run| {
                                run["check_id"] == *id
                                    && run["status"] == "finished"
                                    && run["exit_code"] == 0
                                    && run["freshness_state"] == "current"
                            })
                        });
                    let check_state = if failed {
                        "failed"
                    } else if passed {
                        "passed"
                    } else if selected_ids.is_empty() {
                        "not_selected"
                    } else if phase == "code_review" {
                        "pending_after_code_approval"
                    } else {
                        "pending"
                    };
                    let candidate_snapshot: serde_json::Value = serde_json::from_str(&candidate)?;
                    let structured_plan = structured_plan
                        .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                        .transpose()?;
                    let ownership = structured_plan
                        .as_ref()
                        .filter(|plan| {
                            plan["plan_review_receipt"]["availability"] == "recorded"
                                && plan["human_plan_approval_receipt"]["availability"] == "recorded"
                                && plan["implementation_authorization_receipt"]["availability"]
                                    == "recorded"
                        })
                        .and_then(|plan| {
                            plan.pointer("/plan/ownership").filter(|value| {
                                value
                                    .as_object()
                                    .is_some_and(|ownership| !ownership.is_empty())
                            })
                        });
                    let ownership_mode = match ownership {
                        Some(ownership) if ownership.get("lanes").is_some() => "configured_lanes",
                        Some(_) => "default_single_writer",
                        None => "unavailable",
                    };
                    let lanes = lanes
                        .into_iter()
                        .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                        .collect::<serde_json::Result<Vec<_>>>()?;
                    let empty_paths = serde_json::json!([]);
                    let configured_lanes_match = ownership
                        .and_then(|value| value["lanes"].as_array())
                        .is_some_and(|reviewed| {
                            reviewed.len() >= 2
                                && reviewed.len() == lanes.len()
                                && reviewed
                                    .iter()
                                    .filter_map(|lane| lane["lane_key"].as_str())
                                    .collect::<HashSet<_>>()
                                    .len()
                                    == reviewed.len()
                                && reviewed.iter().all(|reviewed| {
                                    lanes.iter().any(|recorded| {
                                        ["lane_key", "owned_paths", "shared_paths", "dependencies"]
                                            .iter()
                                            .all(|field| {
                                                reviewed.get(*field).is_some()
                                                    && reviewed.get(*field) == recorded.get(*field)
                                            })
                                            && reviewed
                                                .get("protected_paths")
                                                .unwrap_or(&empty_paths)
                                                == &recorded["protected_paths"]
                                    })
                                })
                        });
                    let lane_availability = match ownership_mode {
                        "default_single_writer" if lanes.is_empty() => "not_applicable",
                        "configured_lanes" if configured_lanes_match => "available",
                        _ => "unavailable",
                    };
                    let mut integration = integration
                        .map(|value| serde_json::from_str::<serde_json::Value>(&value))
                        .transpose()?
                        .unwrap_or_else(|| serde_json::json!({}));
                    integration["availability"] = if ownership_mode == "default_single_writer"
                        && lanes.is_empty()
                        && integration.get("id").is_none()
                    {
                        "not_applicable"
                    } else if configured_lanes_match && integration.get("id").is_some() {
                        "available"
                    } else {
                        "unavailable"
                    }
                    .into();
                    let candidate_writer_evidence = if review_kind == "plan" {
                        serde_json::json!({"availability":"not_applicable","reason":"plan review has no implementation candidate writer"})
                    } else {
                        let writer_session_conflict: bool = connection.query_row(
                            crate::review::ACTIVE_WRITER_SQL,
                            params![context.attempt_id],
                            |row| row.get(0),
                        )?;
                        let writer_generation_conflict: bool = connection.query_row(
                            "SELECT EXISTS(SELECT 1 FROM role_generations WHERE attempt_id=?1
                               AND role='implementer' AND status IN ('launch_reserved','running','stopping'))",
                            params![context.attempt_id], |row| row.get(0),
                        )?;
                        // Freeze reservations are serialized per attempt. Bind the last completed
                        // publication at review creation before validating its source; never fall back.
                        // UTC suffix normalization retains full timestamp precision in that fence.
                        let mut statement = connection.prepare(
                            "SELECT json_object('role_generation_id',writer.id,'settings_revision',writer.config_revision,
                               'settings_id',writer_settings.id,
                               'session_id',session.id,'native_session_id',session.native_session_id,
                               'transcript_epoch',session.transcript_epoch,'result_id',result.id,
                               'generation_status',writer.status,'session_status',session.status,
                               'recorded_quiescence',json_object(
                                 'process_group_quiescent',json(CASE json_extract(session.exit_json,'$.process_group_quiescent')
                                   WHEN 1 THEN 'true' WHEN 0 THEN 'false' ELSE 'null' END),
                                 'exit_source',json_extract(session.exit_json,'$.source'),
                                 'observed_at',json_extract(session.exit_json,'$.observed_at'),
                                 'session_record_updated_at',session.updated_at),
                               'freeze_observation',json_object('freeze_intent_id',freeze.id,'state',freeze.state,
                                 'completed_at',freeze.updated_at,'snapshot_id',snapshot.id,
                                 'source_role_generation_id',freeze.source_role_generation_id,
                                 'review_created_at',review.created_at))
                             FROM review_requests review JOIN attempts a ON a.id=review.attempt_id
                             JOIN tasks t ON t.id=a.task_id
                             JOIN trip_structured_plans plan ON plan.id=a.structured_plan_id
                               AND plan.attempt_id=a.id AND plan.plan_hash=a.plan_hash
                               AND plan.reviewed_at IS NOT NULL AND plan.approved_at IS NOT NULL
                               AND plan.implementation_authorized_at IS NOT NULL
                             JOIN snapshots snapshot ON snapshot.attempt_id=a.id
                               AND snapshot.manifest_hash=review.candidate_hash
                             JOIN freeze_intents freeze ON freeze.rowid=(
                               SELECT publication.rowid FROM freeze_intents publication
                               WHERE publication.attempt_id=a.id AND publication.kind='candidate'
                                 AND publication.state='complete'
                                 AND rtrim(replace(publication.updated_at,'+00:00',''),'Z')
                                   <=rtrim(replace(review.created_at,'+00:00',''),'Z')
                               ORDER BY publication.rowid DESC LIMIT 1)
                               AND freeze.result_snapshot_id=snapshot.id
                             JOIN role_generations writer ON writer.id=freeze.source_role_generation_id
                               AND writer.attempt_id=a.id AND writer.role='implementer' AND writer.lane_id='default'
                             JOIN role_settings writer_settings ON writer_settings.task_id=t.id
                               AND writer_settings.role='implementer' AND writer_settings.revision=writer.config_revision
                             JOIN role_results result ON result.role_generation_id=writer.id
                               AND result.outcome='candidate_ready' AND result.consumed_at=freeze.updated_at
                               AND rtrim(replace(result.created_at,'+00:00',''),'Z')
                                 <=rtrim(replace(freeze.created_at,'+00:00',''),'Z')
                               AND rtrim(replace(freeze.created_at,'+00:00',''),'Z')
                                 <=rtrim(replace(freeze.updated_at,'+00:00',''),'Z')
                             JOIN sessions session ON session.id=result.session_id AND session.role_generation_id=writer.id
                             WHERE t.project_id=?1 AND t.id=?2 AND a.id=?3 AND review.id=?4
                               AND review.role_generation_id=?5 AND review.session_id=?6
                               AND review.candidate_hash=?7 AND review.candidate_hash=a.candidate_hash
                               AND snapshot.id=?8 AND snapshot.kind='candidate' AND snapshot.complete=1
                               AND review.review_kind=?9 AND review.delivery_state IN ('launching','delivered','ambiguous')
                               AND (json_type(result.metadata_json,'$.lane_yield.plan_hash') IS NULL
                                 OR json_extract(result.metadata_json,'$.lane_yield.plan_hash')=a.plan_hash)
                             LIMIT 2",
                        )?;
                        let writers = statement
                            .query_map(
                                params![
                                    context.project_id,
                                    context.task_id,
                                    context.attempt_id,
                                    request_id,
                                    context.role_generation_id,
                                    context.session_id,
                                    candidate_hash,
                                    candidate_snapshot["snapshot_id"].as_str(),
                                    review_kind
                                ],
                                |row| row.get::<_, String>(0),
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        let writer = if writers.len() == 1 {
                            Some(serde_json::from_str::<serde_json::Value>(&writers[0])?)
                        } else {
                            None
                        };
                        let source_quiescent = writer.as_ref().is_some_and(|writer| {
                            writer["generation_status"] == "exited"
                                && writer["session_status"] == "exited"
                                && writer["recorded_quiescence"]["process_group_quiescent"] == true
                        });
                        serde_json::json!({
                            "availability":if source_quiescent && !writer_session_conflict && !writer_generation_conflict {"available"}else{"unavailable"},
                            "reason":if writer.is_none() {Some("exact review, snapshot, freeze, source generation and consumed-report session correlation is missing or ambiguous")}
                                else if !source_quiescent {Some("the exact source writer lacks a recorded quiescent exit for both session and generation")}
                                else if writer_session_conflict || writer_generation_conflict {Some("current service records contain an active or unresolved writer")}
                                else {None},
                            "review_request_id":request_id,"attempt_id":context.attempt_id,
                            "candidate_hash":candidate_hash,"snapshot_id":candidate_snapshot["snapshot_id"],
                            "snapshot_creation_provenance":{"created_at":candidate_snapshot["created_at"],
                                "source_role_generation_id":candidate_snapshot["source_role_generation_id"],
                                "source_settings_revision":candidate_snapshot["source_settings_revision"]},
                            "source_writer":writer,
                            "current_evaluation":{"source":"service_records","persisted_receipt":false,
                                "recorded_source_quiescent":source_quiescent,
                                "writer_session_conflict":writer_session_conflict,"writer_generation_conflict":writer_generation_conflict},
                            "guidance":"Snapshot creation provenance is immutable history and may name an earlier writer when identical bytes reuse a snapshot. The applicable publication is the last completed candidate freeze at this review request's creation, correlated through its consumed candidate report and freeze source generation to the exact writer settings and session. Later publications do not rebind an existing review, and invalid applicable evidence never falls back to an earlier freeze. Quiescence and writer conflicts are evaluated now from service records, not a new historical validation receipt or a live process probe. Missing, ambiguous, or mismatched bindings and false or absent recorded quiescence remain unavailable. Manager conformance prose is a separate submitted assertion, not native quiescence evidence."
                        })
                    };
                    Some(serde_json::json!({
                        "availability":"available","review_request_id":request_id,"review_kind":review_kind,
                        "candidate_hash":candidate_hash,"candidate_snapshot":candidate_snapshot,
                        "structured_plan":structured_plan
                            .unwrap_or_else(||serde_json::json!({"availability":"unavailable","reason":"exact current structured plan receipt is missing or stale"})),
                        "ownership_mode":ownership_mode,
                        "lanes":{"availability":lane_availability,"records":lanes,
                            "guidance":"Explicit lane yields and integration receipts are not applicable to approved default single-writer ownership. Configured lane records must exactly match the approved ownership; missing or mismatched records remain unavailable."},
                        "integration":integration,
                        "candidate_writer_evidence":candidate_writer_evidence,
                        "approved_code_review_receipt":approved_code_review.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?
                            .unwrap_or_else(||serde_json::json!({"availability":"unavailable","reason":"exact current-candidate finished approved code-review receipt is missing or stale"})),
                        "manager_conformance_receipt":manager_conformance.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?
                            .unwrap_or_else(||serde_json::json!({"availability":"unavailable","reason":"exact current-candidate conformance receipt for the activated project configuration is missing or stale"})),
                        "check_gate":{"state":check_state,"selected_check_ids":selected_ids,
                            "selected_check_revision":selected_revision,"current_selected_runs":current_selected_runs,
                            "guidance":"A missing selected-check result before code approval is pending, not failed. It never bypasses the checks gate; only the latest non-superseded exact current-candidate run for each current selection row, at the current selection revision and with current freshness, establishes pass. Historical, unselected, wrong-revision, stale, and superseded runs do not satisfy or fail the current gate."},
                        "workspace_inspection":{"authority":"read_only",
                            "guidance":"Inspect the assigned workspace bytes with native read-only commands against this frozen manifest. Manifest hashes identify scope but do not substitute for byte inspection. Do not modify files."}
                    }))
                } else {
                    Some(serde_json::json!({"availability":"unavailable",
                        "reason":"exact current review candidate snapshot is missing or stale"}))
                }
            } else {
                Some(serde_json::json!({"availability":"unavailable",
                    "reason":"exact current delivered review request is missing or stale"}))
            }
        } else {
            None
        };
        let repair_feedback = if ordinary_task
            && matches!(
                context.role,
                crate::domain::RoleKind::Manager | crate::domain::RoleKind::Implementer
            ) {
            Some(crate::repair_handoff::feedback(
                &connection,
                &context.attempt_id,
            )?)
        } else {
            None
        };
        let repair_bindings = if ordinary_task && context.role == crate::domain::RoleKind::Manager {
            crate::repair_handoff::bindings(&connection, &context.attempt_id)?
        } else {
            None
        };
        let implementation_handoff: Option<String> = if ordinary_task
            && matches!(
                context.role,
                crate::domain::RoleKind::Manager | crate::domain::RoleKind::Implementer
            )
            && context.lane_id == "default"
        {
            connection.query_row(
                "SELECT json_object('structured_plan',json(p.plan_json),'bindings',json_object(
                  'task_id',t.id,'attempt_id',a.id,'task_version',t.version,'source_phase',a.phase,
                  'approved_plan_hash',a.plan_hash,'approved_plan_record_id',p.id,'candidate_hash',a.candidate_hash,
                  'base_revision',a.base_revision,'scope_hash',a.scope_hash,'attempt_configuration_hash',a.configuration_hash,
                  'implementer_role_generation_id',g.id,'session_id',s.id,'transcript_epoch',s.transcript_epoch,'lane_id',g.lane_id,
                  'reviewed_at',p.reviewed_at,'approved_at',p.approved_at,'implementation_authorized_at',p.implementation_authorized_at))
                 FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN trip_structured_plans p ON p.id=a.structured_plan_id AND p.attempt_id=a.id AND p.plan_hash=a.plan_hash
                 JOIN role_generations g ON g.id=?4 AND g.attempt_id=a.id AND g.role=?6 AND g.lane_id='default'
                 JOIN role_settings settings ON settings.task_id=t.id AND settings.role=g.role AND settings.effective_generation_id=g.id
                 JOIN sessions s ON s.id=?5 AND s.role_generation_id=g.id AND s.validation_cell IS NULL
                 WHERE a.id=?1 AND t.id=?2 AND t.project_id=?3
                   AND ((?6='implementer' AND a.phase='implementation' AND a.candidate_hash IS NULL)
                     OR (?6='manager' AND a.phase IN ('implementation','code_review','checks','final_review','manager_handoff')))
                   AND p.reviewed_at IS NOT NULL AND p.approved_at IS NOT NULL AND p.implementation_authorized_at IS NOT NULL
                   AND g.status NOT IN ('replaced','revoked')",
                params![context.attempt_id,context.task_id,context.project_id,context.role_generation_id,context.session_id,context.role.to_string()],|row|row.get(0),
            ).optional()?
        } else {
            None
        };
        let human_approval_context =
            if ordinary_task && context.role == crate::domain::RoleKind::Manager {
                Some(crate::trip::human_approval_context(
                    &connection,
                    &context.attempt_id,
                )?)
            } else {
                None
            };
        let task_source_reference = if ordinary_task {
            crate::store::task_source_reference_in(
                &connection,
                &context.project_id,
                &context.task_id,
                &context.session_id,
                &context.attempt_id,
            )?
        } else {
            None
        };
        drop(connection);
        let plan = if let Some((id, hash)) = plan_record {
            let path = self
                .paths
                .artifacts
                .join("snapshots")
                .join(id)
                .join("plan.md");
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read approved plan {}", path.display()))?;
            if bytes.len() > 256 * 1024 || hex::encode(sha2::Sha256::digest(&bytes)) != hash {
                bail!("approved plan artifact is missing, oversized, or changed")
            };
            Some(String::from_utf8(bytes)?)
        } else {
            None
        };
        let executable = providers::shell_quote(self.executable.to_string_lossy().as_ref());
        let context_command = format!("{executable} role context");
        let report = format!("{executable} role report --json '<literal JSON>'");
        let propose_transition = format!("{executable} role propose-transition --operation-id <UUID> --phase <PHASE> --evidence <TEXT>");
        let acknowledge_guidance =
            format!("{executable} role acknowledge-guidance --guidance <ID>");
        let json_command = |name: &str| format!("{executable} role {name} --json '<literal JSON>'");
        let setup_discovery = setup_contract
            .as_ref()
            .and_then(|value| value.get("purpose"))
            .and_then(|value| value.as_str())
            == Some("setup_discovery");
        let commands = match context.role {
            crate::domain::RoleKind::Manager => {
                let mut commands = serde_json::json!({"context":context_command,"report":report,"reporting_contract":ROLE_RESULT_REPORT_CONTRACT,
                    "propose_transition":propose_transition,"acknowledge_guidance":acknowledge_guidance,
                    "record_explorer_decision":json_command("record-explorer-decision"),
                    "configure_lanes":json_command("configure-lanes"),"request_integration":json_command("request-integration"),"select_checks":json_command("select-checks"),
                    "submit_conformance":json_command("submit-conformance")});
                if setup_discovery {
                    commands["setup_read"] = serde_json::Value::String(format!(
                        "{executable} role setup-read --relative-path <CONTAINED_PATH>"
                    ));
                }
                if ordinary_task {
                    commands["schemas"] = manager_command_schemas();
                }
                commands
            }
            crate::domain::RoleKind::Implementer if context.lane_id != "default" => {
                serde_json::json!({"context":context_command,"report":report,"reporting_contract":ROLE_RESULT_REPORT_CONTRACT,
                    "yield_lane":json_command("yield-lane"),"schemas":{"yield_lane":lane_yield_schema()}})
            }
            _ => {
                serde_json::json!({"context":context_command,"report":report,"reporting_contract":ROLE_RESULT_REPORT_CONTRACT})
            }
        };
        let guidance = guidance
            .into_iter()
            .map(|value| serde_json::from_str::<serde_json::Value>(&value))
            .collect::<serde_json::Result<Vec<_>>>()?;
        let guidance_contract=(context.role==crate::domain::RoleKind::Manager).then(||serde_json::json!({
            "content_available":!guidance.is_empty(),
            "acknowledgement_rule":"Only a row with acknowledgement_eligible=true may be acknowledged. queued, delivery_reserved, written_awaiting_submit, and delivery_unknown do not prove native submission.",
            "server_guard":"The service still requires the exact current manager generation and correlated submitted state."
        }));
        let mut value = serde_json::json!({"identity":context,"task":serde_json::from_str::<serde_json::Value>(&task)?,"attempt":serde_json::from_str::<serde_json::Value>(&attempt)?,"task_profiles":task_profiles.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"project_policy":project_policy,"verification_catalog":verification_catalog,"selected_checks":selected_checks,"explorer_decisions":explorer_decisions,"approved_plan":plan,"active_review":current_review.map(|value|serde_json::from_str::<serde_json::Value>(&value.5)).transpose()?,"completed_final_review":completed_final_review.map(|value|serde_json::from_str::<serde_json::Value>(&value)).transpose()?,"review_feedback":feedback.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"review_budgets":budgets.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"check_results":checks.into_iter().map(|value|serde_json::from_str::<serde_json::Value>(&value)).collect::<serde_json::Result<Vec<_>>>()?,"guidance":guidance,"guidance_contract":guidance_contract,"setup_contract":setup_contract,"commands":commands});
        if let Some(evidence) = current_explorer_evidence {
            value["current_explorer_evidence"] = evidence;
        }
        if let Some(evidence) = ordinary_review_evidence {
            value["ordinary_review_evidence"] = evidence;
        }
        if let Some(approvals) = human_approval_context {
            value["human_approval_context"] = approvals;
        }
        if let Some(recheck) = final_repair_recheck {
            value["final_repair_recheck"] = serde_json::from_str(&recheck)?;
        }
        if let Some(feedback) = repair_feedback {
            if context.role == crate::domain::RoleKind::Implementer {
                if let Some(result) = feedback.result.filter(|result| {
                    result["metadata"].get("implementation_capsule").is_none()
                        || (result["metadata"]["implementation_capsule"]["bindings"]["session_id"]
                            == context.session_id
                            && result["metadata"]["implementation_capsule"]["bindings"]
                                ["implementer_role_generation_id"]
                                == context.role_generation_id)
                }) {
                    value["manager_feedback"] = serde_json::json!({
                        "result":result,
                        "authority":"Source repair guidance only. Preserve approved ownership and human approval boundaries. This is not a new approval, a selected-check receipt, or permission to report unverified completion. A source candidate may list pending checks for later service verification."
                    });
                }
            }
            if let Some(blocker) = feedback.blocker {
                value["manager_feedback_blocker"] = blocker;
            }
        }
        if let Some(bindings) = repair_bindings {
            value["repair_handoff_template"] = serde_json::json!({
                "bindings":bindings,
                "report_metadata":{"approved_plan_hash":bindings["approved_plan_hash"],
                    "candidate_hash":bindings["candidate_hash"],"implementation_capsule":{"bindings":bindings}},
                "guidance":"Use this exact metadata shape only when Manager source directions are needed. Routine accepted review repairs use review_feedback and the complete approved plan. Add directions beside bindings. Never pin a future retained turn to a historical task_version, transcript_epoch or resume_count. This template grants no new scope, check execution, review allowance or human approval."
            });
        }
        if let Some(handoff) = implementation_handoff {
            let handoff: serde_json::Value = serde_json::from_str(&handoff)?;
            value["structured_plan"] = handoff["structured_plan"].clone();
            if context.role == crate::domain::RoleKind::Implementer {
                value["implementation_capsule"] = serde_json::json!({
                    "bindings":handoff["bindings"],"ownership":handoff["structured_plan"]["ownership"],
                    "restrictions":handoff["structured_plan"]["restrictions"],"documentation":handoff["structured_plan"]["documentation"],
                    "verification_matrix":handoff["structured_plan"]["verification_matrix"],
                    "config_revision_id":value["project_policy"]["config_revision_id"],
                    "project_configuration_hash":value["project_policy"]["configuration_hash"],
                    "selected_checks":value["selected_checks"],
                    "authority":"Exact recorded approved plan and separate implementation authorization. This projection grants no broader scope, check execution, native permission or completion authority."
                });
            }
        }
        if let Some(reference) = task_source_reference {
            value["task"]["untrusted_conversation_reference"] = reference;
        }
        if serde_json::to_vec(&value)?.len() > 1024 * 1024 {
            bail!("task-scoped role context exceeds the 1 MiB response bound; shorten task context or guidance before retrying")
        }
        {
            let connection = self.store.lock()?;
            crate::knowledge::attach_frozen_task_knowledge_in(
                &connection,
                &context.attempt_id,
                &mut value,
            )?;
        }
        Ok(value)
    }

    fn require_ordinary_context_delivery(&self, context: &RoleContext) -> Result<()> {
        if self.store.restore_hold()?.is_some() {
            bail!("role authority is disabled by the database restore hold")
        }
        let connection = self.store.lock()?;
        let current: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM role_credentials credential
             JOIN role_generations generation ON generation.id=credential.role_generation_id
             JOIN sessions session ON session.role_generation_id=generation.id
             JOIN attempts attempt ON attempt.id=generation.attempt_id
             JOIN tasks task ON task.id=attempt.task_id
             WHERE credential.id=?1 AND credential.revoked_at IS NULL
               AND generation.status NOT IN ('replaced','revoked')
               AND session.id=?2 AND generation.id=?3 AND attempt.id=?4 AND task.id=?5
               AND task.project_id=?6 AND session.transcript_epoch=?7 AND generation.role=?8
               AND generation.provider=?9 AND EXISTS(SELECT 1 FROM json_each(credential.permissions_json)
                 WHERE value='read_context'))",
            rusqlite::params![context.credential_id, context.session_id, context.role_generation_id,
                context.attempt_id, context.task_id, context.project_id, context.transcript_epoch,
                context.role.to_string(), context.provider.to_string()],
            |row| row.get(0),
        )?;
        if !current {
            bail!("role context delivery credential or invocation is no longer current")
        }
        if crate::trip::setup_context(&connection, context)?.is_some() {
            bail!("paged role context is unavailable during qualification; use the prescribed direct context command")
        }
        let qualification: bool = connection.query_row(
            "SELECT session.validation_cell IS NOT NULL OR attempt.status='capability_validation'
               OR EXISTS(SELECT 1 FROM controls verification
                 WHERE verification.attempt_id=attempt.id AND verification.kind='main_checkout_verify'
                   AND verification.state IN ('native_running','native_stopping','awaiting_native_evidence')
                   AND (json_extract(verification.payload_json,'$.session_id')=session.id
                     OR json_extract(verification.payload_json,'$.launch_reservation.session_id')=session.id))
             FROM sessions session JOIN role_generations generation ON generation.id=session.role_generation_id
             JOIN attempts attempt ON attempt.id=generation.attempt_id WHERE session.id=?1",
            rusqlite::params![context.session_id], |row| row.get(0),
        )?;
        if qualification {
            bail!("paged role context is unavailable during qualification; use the prescribed direct context command")
        }
        Ok(())
    }

    pub fn role_context_delivery_payload(
        &self,
        context: &RoleContext,
        cursor: Option<&str>,
    ) -> Result<serde_json::Value> {
        use crate::task_cli::{RoleContextCursor, RoleContextSection};
        let cursor = cursor.map(RoleContextCursor::parse).transpose()?;
        self.require_ordinary_context_delivery(context)?;
        let projection = self.role_context_payload(context)?;
        let revision = json_hash(&projection)?;
        let scope = json_hash(&serde_json::json!([
            context.task_id,
            context.attempt_id,
            context.session_id,
            context.role_generation_id,
            context.transcript_epoch,
            context.credential_id
        ]))?;
        let scope = &scope[..32];
        if let Some(cursor) = &cursor {
            if cursor.scope != scope || cursor.revision != revision[..32] {
                bail!("role context cursor is stale or belongs to another invocation; restart with role context --index")
            }
        }
        let sections = projection
            .as_object()
            .context("role context projection must be an object")?;
        for name in sections.keys() {
            if RoleContextSection::parse(name)? == RoleContextSection::Index {
                bail!("role context projection contains the reserved index section")
            }
        }
        let page = cursor.as_ref().map_or(0, |cursor| cursor.page);
        let section = cursor
            .as_ref()
            .map_or(RoleContextSection::Index, |cursor| cursor.section);
        let result = if section == RoleContextSection::Index {
            let mut entries = Vec::new();
            for (name, value) in sections {
                let content = serde_json::to_string(value)?;
                let ranges = role_context_content_ranges(name, &content, &revision, scope)?;
                entries.push(
                    serde_json::json!({"name":name,"bytes":content.len(),"pages":ranges.len(),
                    "digest":&hex::encode(sha2::Sha256::digest(content.as_bytes()))[..32],
                    "first":role_context_cursor(name, 0, &revision, scope)}),
                );
            }
            entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
            let ranges = role_context_index_ranges(&entries, &revision, scope)?;
            let range = ranges
                .get(page)
                .context("role context index page is out of range")?;
            role_context_envelope(
                "index",
                &revision,
                scope,
                page,
                ranges.len(),
                serde_json::json!(&entries[range.clone()]),
            )
        } else {
            let name: String = serde_json::from_value(serde_json::to_value(section)?)?;
            let value = sections
                .get(&name)
                .context("role context section is not present in this projection")?;
            let content = serde_json::to_string(value)?;
            let ranges = role_context_content_ranges(&name, &content, &revision, scope)?;
            let range = ranges
                .get(page)
                .context("role context section page is out of range")?;
            role_context_envelope(
                &name,
                &revision,
                scope,
                page,
                ranges.len(),
                serde_json::json!(&content[range.clone()]),
            )
        };
        if !role_context_output_fits(&result)? {
            bail!("role context page exceeds the 4096-byte output bound")
        }
        self.require_ordinary_context_delivery(context)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        role_context_content_ranges, role_context_cursor, role_context_envelope,
        role_context_index_ranges, role_context_output_fits, ROLE_CONTEXT_MAX_PAGES,
        ROLE_CONTEXT_OUTPUT_BYTES,
    };

    #[test]
    fn context_index_pages_preserve_whole_entries_and_bound_complete_envelopes() {
        let revision = "ab".repeat(32);
        let scope = "cd".repeat(16);
        let entries = (0..27).map(|index| serde_json::json!({
            "name":format!("section_{index:02}"),"bytes":999999,"pages":999999,
            "digest":"ef".repeat(16),"first":role_context_cursor("current_explorer_evidence",999999,&revision,&scope)
        })).collect::<Vec<_>>();
        let ranges = role_context_index_ranges(&entries, &revision, &scope).unwrap();
        assert!(ranges.len() > 1);
        let mut reconstructed = Vec::new();
        for (page, range) in ranges.iter().enumerate() {
            let envelope = role_context_envelope(
                "index",
                &revision,
                &scope,
                page,
                ranges.len(),
                serde_json::json!(&entries[range.clone()]),
            );
            assert!(role_context_output_fits(&envelope).unwrap());
            assert_eq!(envelope["complete"], page + 1 == ranges.len());
            assert_eq!(envelope["next"].is_null(), page + 1 == ranges.len());
            reconstructed.extend(envelope["entries"].as_array().unwrap().iter().cloned());
        }
        assert_eq!(reconstructed, entries);
        let oversized = vec![serde_json::json!({"name":"x".repeat(4096)})];
        assert!(role_context_index_ranges(&oversized, &revision, &scope)
            .unwrap_err()
            .to_string()
            .contains("entry exceeds"));

        let content =
            serde_json::to_string(&serde_json::json!({"text":"é中😀\"\\\n\u{0}".repeat(6000)}))
                .unwrap();
        let ranges = role_context_content_ranges("task", &content, &revision, &scope).unwrap();
        assert!(ranges.len() > 1);
        let largest_counters = role_context_envelope(
            "task",
            &revision,
            &scope,
            ROLE_CONTEXT_MAX_PAGES - 2,
            ROLE_CONTEXT_MAX_PAGES,
            serde_json::json!(&content[ranges[0].clone()]),
        );
        assert!(role_context_output_fits(&largest_counters).unwrap());
        crate::task_cli::RoleContextCursor::parse(largest_counters["next"].as_str().unwrap())
            .unwrap();
        let mut reconstructed = String::new();
        for (page, range) in ranges.iter().enumerate() {
            let envelope = role_context_envelope(
                "task",
                &revision,
                &scope,
                page,
                ranges.len(),
                serde_json::json!(&content[range.clone()]),
            );
            let cli_output = format!("{}\n", serde_json::to_string(&envelope).unwrap());
            assert!(cli_output.len() <= ROLE_CONTEXT_OUTPUT_BYTES);
            reconstructed.push_str(envelope["chunk"].as_str().unwrap());
        }
        assert_eq!(reconstructed.as_bytes(), content.as_bytes());
    }
}
