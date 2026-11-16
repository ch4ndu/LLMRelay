use super::{
    continuation_actions, missing_selection_observation, permanent_resume_rejection_is_current,
    selected_setup_capability, workspace_recovery_reason, AttentionSources,
};
use crate::domain::{AttentionTarget, ProjectDto, RoleKind, TaskDto};
use serde_json::{json, Value};

#[test]
fn missing_profile_observation_names_the_stale_fact_instead_of_an_unknown_version() {
    let selection = json!({
        "role":"explorer","profile":{"provider":"claude"},"profile_hash":"current"
    });
    let receipt = |hash: &str, result: &str| {
        json!({"role":"explorer","provider":"claude","profile_hash":hash,"result":result,
                   "created_at":"2026-01-01T00:00:00Z"})
    };
    assert!(missing_selection_observation(&selection, &[]).contains("not been verified yet"));
    assert!(
        missing_selection_observation(&selection, &[receipt("older", "success")])
            .contains("changed after they were last verified")
    );
    assert!(
        missing_selection_observation(&selection, &[receipt("current", "failure")])
            .contains("did not succeed")
    );
    assert!(
        missing_selection_observation(&selection, &[receipt("current", "success")])
            .contains("Newer agent evidence replaced")
    );
    for message in [
        missing_selection_observation(&selection, &[]),
        missing_selection_observation(&selection, &[receipt("older", "success")]),
    ] {
        assert!(!message.contains("version"), "{message}");
    }
}

#[test]
fn workspace_recovery_explains_whether_retrying_can_succeed() {
    let guidance = workspace_recovery_reason(&json!({
        "reason":"TRIP policy materialization requires recovery: approved guidance collides with different worktree content: AGENTS.md",
    }));
    assert!(
        guidance.contains("AGENTS.md") && guidance.contains("Inspect and retry"),
        "{guidance}"
    );
    let collision = workspace_recovery_reason(&json!({
        "reason":"TRIP policy materialization requires recovery: policy materialization collision: /w/.agents/trip-explorer/config.json",
        "observed_filesystem":{"stage":"policy_materialization"}
    }));
    assert!(
        collision.contains("retrying cannot fix")
            && collision.contains("Validate and relink")
            && collision.contains("cancels the task"),
        "{collision}"
    );
    assert!(
        workspace_recovery_reason(&json!({"reason":"worktree creation failed"}))
            .contains("Inspect and retry workspace reservation")
    );
}

#[test]
fn exact_selected_profile_ignores_unrelated_and_newer_failed_evidence() {
    let selected =
        json!({"role":"explorer","profile":{"provider":"codex"},"profile_hash":"selected"});
    let other =
        json!({"role":"plan_reviewer","profile":{"provider":"codex"},"profile_hash":"other"});
    let receipts = vec![
        json!({"role":"explorer","provider":"codex","profile_hash":"selected","result":"success","capability_key":"selected-key","created_at":"2026-01-01"}),
        json!({"role":"plan_reviewer","provider":"codex","profile_hash":"other","result":"success","capability_key":"other-key","created_at":"2026-01-02"}),
    ];
    let capabilities = vec![
        json!({"rowid":1,"provider":"codex","version":"v1","role":"explorer","mode":"interactive_pty","config_hash":"selected-key","checked_at":"2026-01-01","compatibility":{"status":"matched"}}),
        json!({"rowid":2,"provider":"codex","version":"v2","role":"explorer","mode":"interactive_pty","config_hash":"unrelated-key","checked_at":"2026-01-03","compatibility":{"status":"unknown_version"}}),
        json!({"rowid":3,"provider":"codex","version":"v1","role":"plan_reviewer","mode":"interactive_pty","config_hash":"other-key","checked_at":"2026-01-02","compatibility":{"status":"matched"}}),
    ];
    assert_eq!(
        selected_setup_capability(&selected, &receipts, &capabilities).unwrap()["config_hash"],
        "selected-key"
    );
    assert_eq!(
        selected_setup_capability(&other, &receipts, &capabilities).unwrap()["config_hash"],
        "other-key"
    );
    let newer_same_scope = json!({"rowid":4,"provider":"codex","version":"v1","role":"explorer","mode":"interactive_pty","config_hash":"new-key","checked_at":"2026-01-04","compatibility":{"status":"evidence_stale"}});
    let mut superseded = capabilities.clone();
    superseded.push(newer_same_scope);
    assert!(selected_setup_capability(&selected, &receipts, &superseded).is_none());
    assert!(selected_setup_capability(&other, &receipts, &superseded).is_some());
    let mut failed = receipts.clone();
    failed.push(json!({"role":"explorer","provider":"codex","profile_hash":"selected","result":"failure","capability_key":"selected-key","created_at":"2026-01-05"}));
    assert!(selected_setup_capability(&selected, &failed, &capabilities).is_none());
    assert!(selected_setup_capability(&other, &failed, &capabilities).is_some());
    let wrong_profile: Value =
        json!({"role":"explorer","profile":{"provider":"codex"},"profile_hash":"new-profile"});
    assert!(selected_setup_capability(&wrong_profile, &receipts, &capabilities).is_none());
}

#[test]
fn runtime_failure_attention_tracks_exact_published_replacements() {
    let probe = |role: &str, state: &str| {
        json!({
            "role":role,"state":state,"settings_revision":null,
            "profile":{"provider":"codex","model":"model","effort":"high"},
            "profile_hash":format!("profile-{role}"),"project_config_revision_id":"config",
            "project_configuration_hash":"configuration-hash","adapter":"codex",
            "adapter_hash":"adapter-hash","capability_key":format!("key-{role}"),
            "capability_id":format!("capability-{role}"),"published_at":"2026-01-02T01:00:00Z",
            "failure_category":"model_refusal","failure_reason":"model_refusal"
        })
    };
    let old = json!({
        "id":"original","project_id":"project","task_id":null,"state":"failed",
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T01:00:00Z",
        "failure_reason":"GROUP-IDENTITY-DUMP",
        "probes":[probe("manager","published"),probe("explorer","published"),
            probe("final_verifier","failed"),probe("plan_reviewer","failed"),
            probe("implementer","failed"),probe("code_reviewer","failed")]
    });
    let corrections = json!({
        "id":"corrections","project_id":"project","task_id":null,"state":"ready",
        "created_at":"2026-01-02T00:00:00Z",
        "probes":[probe("final_verifier","published"),probe("plan_reviewer","published"),
            probe("implementer","published")]
    });
    let capabilities = corrections["probes"].as_array().unwrap().iter().map(|probe| json!({
            "rowid":1,"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,
            "role":probe["role"],"mode":"interactive_pty","config_hash":probe["capability_key"],
            "status":"supported","checked_at":"2026-01-02T00:59:00Z",
            "compatibility":{"status":"matched"},
            "proof":{"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,
                "role":probe["role"],"config_hash":probe["capability_key"],"model":"model","effort":"high",
                "runtime_scopes":[{"project_id":"project","task_id":null,"profiles":[probe]}]}
        })).collect::<Vec<_>>();
    let projects = [ProjectDto {
        id: "project".into(),
        display_name: "Project".into(),
        repository_path: "/tmp/project".into(),
        repository_identity: "/tmp/project".into(),
        base_revision: "base".into(),
        queue_paused: false,
        version: 1,
        settings: json!({}),
        trip: json!({"readiness":"ready","setup_operation_id":"setup"}),
    }];
    let tasks = [TaskDto {
        id: "task".into(),
        project_id: "project".into(),
        knowledge: serde_json::Value::Null,
        workspace_mode: Default::default(),
        title: "Task-specific profile".into(),
        description: String::new(),
        acceptance_criteria: vec![],
        priority: 0,
        manual_order: 0,
        lifecycle: "backlog".into(),
        attention: "none".into(),
        version: 7,
        archived: false,
        can_archive: false,
        draft_deletion: None,
        recipe_provenance: None,
        permission_waiting: false,
        role_overrides: json!({}),
        dependencies: vec![],
        active_attempt: None,
        role_settings: vec![
            json!({"role":"code_reviewer","revision":1,"config":{"provider":"codex","model":"model","effort":"high"}}),
        ],
        reviews: vec![],
        snapshots: vec![],
        review_budgets: vec![],
        legacy: json!({}),
        progress: None,
        observed_usage: None,
        workflow: None,
    }];
    let attention = |admissions: &[Value], capabilities: &[Value]| {
        let setups = [json!({"project_id":"project","runtime_admissions":admissions})];
        let actions = continuation_actions(
            &projects,
            &tasks,
            capabilities,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &setups,
            &[],
            &[],
            &[],
        );
        AttentionSources {
            projects: &projects,
            workflow_execution: &[],
            tasks: &tasks,
            sessions: &[],
            recovery: &[],
            permission_requests: &[],
            trip_setups: &setups,
            decisions: &[],
            continuation_actions: &actions,
            coordinator_deferral: None,
            observations: &[],
            codex_capacity: &[],
        }
        .items()
    };
    let unresolved = attention(&[old.clone(), corrections.clone()], &capabilities);
    assert_eq!(unresolved.len(), 1);
    let reviewer = &unresolved[0];
    assert_eq!(reviewer.role, Some(RoleKind::CodeReviewer));
    assert_eq!(
        reviewer.id,
        "continuation:prepare_runtime_admission:original:code_reviewer"
    );
    assert!(reviewer.title.contains("Code reviewer"));
    assert!(reviewer
        .reason
        .contains("declined a required verification check"));
    assert!(reviewer
        .reason
        .contains("Project setup > Runtime verification"));
    assert!(reviewer
        .reason
        .contains("inspect the Code reviewer's recorded output"));
    assert!(reviewer.reason.contains("Prepare a new verification"));
    assert!(!reviewer.reason.contains("GROUP-IDENTITY-DUMP"));
    assert!(reviewer
        .details
        .as_deref()
        .unwrap()
        .contains("model_refusal"));
    assert_eq!(
        reviewer.target,
        Some(AttentionTarget::ProjectSetup {
            project_id: "project".into(),
            setup_operation_id: Some("setup".into()),
        })
    );

    let mut task_admission = old.clone();
    task_admission["task_id"] = json!("task");
    task_admission["probes"] = json!([probe("code_reviewer", "failed")]);
    task_admission["probes"][0]["settings_revision"] = json!(1);
    let task_attention = attention(&[task_admission], &capabilities);
    assert_eq!(task_attention.len(), 1);
    assert_eq!(task_attention[0].role, Some(RoleKind::CodeReviewer));
    assert!(task_attention[0]
        .reason
        .contains("Open the task's Roles tab"));
    assert!(task_attention[0]
        .reason
        .contains("inspect the Code reviewer's recorded output"));
    assert!(!task_attention[0].reason.contains("Project setup"));
    assert_eq!(
        task_attention[0].target,
        Some(AttentionTarget::Task(crate::domain::TaskAttentionTarget {
            project_id: "project".into(),
            task_id: "task".into(),
            task_version: 7,
        }))
    );

    let separate = attention(&[old.clone()], &capabilities);
    assert_eq!(
        separate
            .iter()
            .map(|item| item.role.unwrap())
            .collect::<Vec<_>>(),
        [
            RoleKind::FinalReviewer,
            RoleKind::PlanReviewer,
            RoleKind::Implementer,
            RoleKind::CodeReviewer,
        ]
    );
    assert_eq!(
        separate
            .iter()
            .map(|item| &item.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );

    let mut unpublished = corrections.clone();
    for probe in unpublished["probes"].as_array_mut().unwrap() {
        probe["state"] = json!("evidence_recorded");
    }
    assert_eq!(
        attention(&[old.clone(), unpublished], &capabilities).len(),
        4
    );
    let mut foreign_scope = corrections.clone();
    foreign_scope["task_id"] = json!("another-task");
    assert_eq!(
        attention(&[old.clone(), foreign_scope], &capabilities).len(),
        4
    );
    let mut simultaneous = corrections.clone();
    simultaneous["created_at"] = old["created_at"].clone();
    assert_eq!(
        attention(&[old.clone(), simultaneous], &capabilities).len(),
        4
    );

    let mut superseded = capabilities.clone();
    let mut latest = superseded[2].clone();
    latest["rowid"] = json!(2);
    latest["config_hash"] = json!("unrelated-key");
    latest["status"] = json!("unverified");
    latest["proof"] = json!({});
    superseded.push(latest);
    assert_eq!(
        attention(&[old.clone(), corrections.clone()], &superseded)
            .iter()
            .map(|item| item.role.unwrap())
            .collect::<Vec<_>>(),
        [RoleKind::Implementer, RoleKind::CodeReviewer,]
    );
    let mut incomplete_old = old.clone();
    let mut incomplete_corrections = corrections.clone();
    incomplete_old["probes"][4]
        .as_object_mut()
        .unwrap()
        .remove("adapter_hash");
    incomplete_corrections["probes"][2]
        .as_object_mut()
        .unwrap()
        .remove("adapter_hash");
    assert_eq!(
        attention(&[incomplete_old, incomplete_corrections], &capabilities).len(),
        2
    );
    let mut unrelated_proof = capabilities.clone();
    unrelated_proof[2]["proof"]["runtime_scopes"][0]["project_id"] = json!("another-project");
    assert_eq!(
        attention(&[old.clone(), corrections.clone()], &unrelated_proof).len(),
        2
    );

    let mut newer_failure = corrections.clone();
    newer_failure["id"] = json!("newer-failure");
    newer_failure["created_at"] = json!("2026-01-03T00:00:00Z");
    newer_failure["state"] = json!("failed");
    newer_failure["probes"] = json!([probe("code_reviewer", "failed")]);
    let newest = attention(&[old.clone(), corrections, newer_failure], &capabilities);
    assert_eq!(newest.len(), 1);
    assert_eq!(
        newest[0].id,
        "continuation:prepare_runtime_admission:newer-failure:code_reviewer"
    );
    let mut legacy = old;
    legacy["probes"] = json!([]);
    let fallback = attention(&[legacy], &capabilities);
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].role, None);
    assert!(!fallback[0].reason.contains("GROUP-IDENTITY-DUMP"));
    assert!(fallback[0]
        .details
        .as_deref()
        .unwrap()
        .contains("GROUP-IDENTITY-DUMP"));
}

#[test]
fn invalid_manifest_rejection_retires_only_for_newer_exact_proof() {
    let task = TaskDto {
        id: "task".into(),
        project_id: "project".into(),
        knowledge: serde_json::Value::Null,
        workspace_mode: Default::default(),
        title: String::new(),
        description: String::new(),
        acceptance_criteria: vec![],
        priority: 0,
        manual_order: 0,
        lifecycle: "active".into(),
        attention: "active".into(),
        version: 1,
        archived: false,
        can_archive: false,
        draft_deletion: None,
        recipe_provenance: None,
        permission_waiting: false,
        role_overrides: json!({}),
        dependencies: vec![],
        active_attempt: Some(json!({"id":"attempt"})),
        role_settings: vec![
            json!({"role":"manager","revision":1,"effective_generation_id":"generation","config":{"provider":"codex","model":"model","effort":"high"}}),
        ],
        reviews: vec![],
        snapshots: vec![],
        review_budgets: vec![],
        legacy: json!({}),
        progress: None,
        observed_usage: None,
        workflow: None,
    };
    let session = json!({"id":"session","task_id":"task","attempt_id":"attempt","role":"manager","role_generation_id":"generation","generation_status":"exited","config_revision":1,"provider":"codex","status":"exited","resume_count":0,"transcript_epoch":"epoch","capability_current":false});
    let rejection = json!({"category":"provider_compatibility_invalid_manifest"});
    let binding = crate::provider_compatibility::BundleSet::embedded()
        .resolve(
            crate::domain::Provider::Codex,
            crate::provider_compatibility::CODEX_EXACT_VERSION,
            crate::domain::RoleKind::Manager,
        )
        .unwrap();
    let proof = json!({"compatibility":crate::provider_compatibility::AuthorityBinding::from(&binding),"model":"model","effort":"high"});
    let exact = json!({"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,"role":"manager","mode":"interactive_pty","config_hash":"new-key","status":"supported","checked_at":"2026-01-03","proof":proof});
    let unrelated = json!({"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,"role":"explorer","mode":"interactive_pty","config_hash":"other-key","status":"supported","checked_at":"2026-01-04","proof":proof});
    assert!(permanent_resume_rejection_is_current(
        &[task.clone()],
        &[],
        &[unrelated.clone()],
        Some("2026-01-02"),
        &rejection,
        &session
    ));
    assert!(permanent_resume_rejection_is_current(
        &[task.clone()],
        &[],
        &[exact.clone()],
        Some("2026-01-04"),
        &rejection,
        &session
    ));
    assert!(!permanent_resume_rejection_is_current(
        &[task.clone()],
        &[],
        &[unrelated.clone(), exact.clone()],
        Some("2026-01-02"),
        &rejection,
        &session
    ));
    let newer_unverified = json!({"provider":"codex","version":crate::provider_compatibility::CODEX_EXACT_VERSION,"role":"manager","mode":"interactive_pty","config_hash":"newest-key","status":"unverified","checked_at":"2026-01-05","proof":{}});
    assert!(permanent_resume_rejection_is_current(
        &[task.clone()],
        &[],
        &[exact.clone(), newer_unverified],
        Some("2026-01-02"),
        &rejection,
        &session
    ));
    let detail = json!({"category":"provider_compatibility_invalid_manifest","role_generation_id":"generation","transcript_epoch":"epoch","resume_count":0,"attempt_id":"attempt","role":"manager","config_revision":1});
    let history = vec![
        json!({"id":"rejection","event_code":"session.resume.rejected","entity_id":"session","created_at":"2026-01-02","detail":detail}),
    ];
    let decision_actions = |evidence: &[Value]| {
        continuation_actions(
            &[],
            &[task.clone()],
            evidence,
            &[session.clone()],
            &[],
            &[],
            &[],
            &history,
            &[],
            &[],
            &[],
            &[],
            &[],
        )
    };
    let held = decision_actions(&[unrelated.clone()]);
    assert!(held
        .iter()
        .any(|action| action.binding["session_id"] == "session"
            && action.operation == "terminal_incomplete"
            && !action.enabled));
    let released = decision_actions(&[unrelated, exact]);
    assert!(!released
        .iter()
        .any(|action| action.binding["session_id"] == "session"
            && action.operation == "terminal_incomplete"));
}
