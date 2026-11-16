use super::*;

#[test]
fn main_checkout_verification_reads_and_removes_only_its_exact_regular_output() {
    let root = std::env::temp_dir().join(format!("llmrelay-main-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let nonce = "0123456789abcdef0123456789abcdef";
    let expected = format!("{nonce}\n");
    let target = root.join("user-file");
    std::fs::write(&target, &expected).unwrap();
    for case in [
        "correct",
        "wrong",
        "missing_lf",
        "crlf",
        "extra_lf",
        "space",
        "symlink",
        "hardlink",
    ] {
        let path = root.join(case);
        match case {
            "symlink" => std::os::unix::fs::symlink(&target, &path).unwrap(),
            "hardlink" => std::fs::hard_link(&target, &path).unwrap(),
            "wrong" => std::fs::write(&path, "fedcba9876543210fedcba9876543210\n").unwrap(),
            "missing_lf" => std::fs::write(&path, nonce).unwrap(),
            "crlf" => std::fs::write(&path, format!("{nonce}\r\n")).unwrap(),
            "extra_lf" => std::fs::write(&path, format!("{nonce}\n\n")).unwrap(),
            "space" => std::fs::write(&path, format!("{nonce} \n")).unwrap(),
            _ => std::fs::write(&path, &expected).unwrap(),
        }
        let result = remove_owned_verification_output(&path, nonce);
        if case == "correct" {
            assert!(result.unwrap());
            assert!(!path.exists());
        } else {
            assert!(!matches!(result, Ok(true)), "{case}");
            assert!(std::fs::symlink_metadata(&path).is_ok());
            std::fs::remove_file(path).unwrap();
        }
        assert_eq!(std::fs::read(&target).unwrap(), expected.as_bytes());
    }
    let canonical = root.canonicalize().unwrap();
    let probe = "fedcba9876543210fedcba9876543210";
    let challenge = canonical.join(format!(".llmrelay-main-{probe}-challenge"));
    let writer = canonical.join(format!(".llmrelay-main-{probe}-first"));
    std::fs::write(&challenge, nonce).unwrap();
    std::fs::write(&writer, nonce).unwrap();
    let protected = target.canonicalize().unwrap();
    let scope = serde_json::json!({"root":canonical,"protected_roots":[protected],"protected_read_targets":[protected],"policy_denied_unobserved":[]});
    let request =
        providers::main_checkout_functional_observation_request(&scope, &challenge, Some(&writer))
            .unwrap();
    let payload = serde_json::json!({"functional_challenge_created":true,"scope":scope,"probe_id":probe,"nonce":nonce,"observation_request":request});
    std::fs::write(&writer, b"changed by owner").unwrap();
    assert!(cleanup_functional_main_artifacts(&payload).is_err());
    assert!(!challenge.exists());
    assert_eq!(std::fs::read(&writer).unwrap(), b"changed by owner");
    std::fs::write(&writer, nonce).unwrap();
    cleanup_functional_main_artifacts(&payload).unwrap();
    assert!(!writer.exists());
    assert_eq!(std::fs::read(&target).unwrap(), expected.as_bytes());
    std::fs::remove_dir_all(root).unwrap();
}
use crate::domain::Provider;
use crate::store::provider_validation_tests::{
    historical_batch, inspection, launch, policy_bundles, reserve_expecting, select,
};
use crate::store::ProviderLocalInspection;

fn synthetic_app(label: &str) -> (PathBuf, Application, ProviderLocalInspection) {
    let root = std::env::temp_dir().join(format!(
        "llmrelay-provider-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
    paths.create().unwrap();
    let mut store = Store::open(&paths.database).unwrap();
    store.compatibility_bundles = Arc::new(policy_bundles(true));
    let claude = inspection(&store.compatibility_bundles, Provider::Claude, None);
    let hooks = HookAssets {
        runner: root.join("hook"),
        claude_settings: root.join("claude-settings.json"),
        claude_implementer_settings: root.join("claude-implementer-settings.json"),
        codex_revision_hash: "synthetic".into(),
        claude_revision_hash: "synthetic".into(),
        claude_implementer_revision_hash: "synthetic".into(),
    };
    let app = Application::new_with_synthetic_compatibility_for_tests(
        paths,
        store,
        std::env::current_exe().unwrap(),
        hooks,
    )
    .unwrap();
    (root, app, claude)
}

fn model_proofs(app: &Application) -> i64 {
    app.store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM provider_observations WHERE kind='model_access'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn codex_check_reasons_name_the_actual_blocker_without_sign_in_advice() {
    // Never created: a version mismatch is decided before the scope or any
    // account is read.
    let scope =
        std::env::temp_dir().join(format!("llmrelay-unused-scope-{}", uuid::Uuid::new_v4()));
    let mismatch = codex_observation_account(&scope, "codex-cli 0.162.0").unwrap_err();
    assert!(
        mismatch.contains(providers::codex::EXACT_CODEX_VERSION)
            && mismatch.contains("codex-cli 0.162.0"),
        "{mismatch}"
    );
    let other = codex_setup_unconfirmed(&anyhow!(
        "Codex local compatibility does not support managed Codex configuration"
    ));
    assert!(
        other.contains("does not support managed Codex configuration"),
        "{other}"
    );
    for reason in [mismatch, other] {
        assert!(!reason.to_ascii_lowercase().contains("sign"), "{reason}");
    }
}

#[test]
fn recorded_answer_publishes_proof_only_for_the_exact_nonce_and_a_normal_exit() {
    let (root, app, claude) = synthetic_app("answer");
    let process = r#"{"pid":7}"#;
    let answer =
        format!("{PROVIDER_CHECK_PREFIX}0123456789abcdef0123456789abcdef").to_ascii_uppercase();
    let expected = hex::encode(sha2::Sha256::digest(answer.as_bytes()));
    let echo = format!(
        "Reply with exactly one line: the text {} written entirely in uppercase letters.\r\n",
        answer.to_ascii_lowercase()
    );
    let wrong = format!("{PROVIDER_CHECK_PREFIX}{}\r\n", "F".repeat(32));
    let check_output = |session: &str, output: &str| {
        let batch = historical_batch(
            &app.store,
            session,
            &[select(Provider::Claude, "model-a")],
            &[claude.clone()],
            1,
        );
        let check = app.store.pending_provider_checks().unwrap().remove(0);
        let epoch = uuid::Uuid::new_v4().to_string();
        let launch = serde_json::json!({
            "kind": "provider_observation", "evidence": claude.evidence().unwrap()
        });
        assert!(app
            .store
            .reserve_provider_validation_session(
                &crate::store::ProviderValidationSessionReservation {
                    invocation_id: &check.invocation_id,
                    session_id: session,
                    transcript_epoch: &epoch,
                    provider: check.provider,
                    executable_version: "synthetic",
                    launch_json: &launch.to_string(),
                    scope_root: "/synthetic/scope",
                    nonce_hash: &expected,
                }
            )
            .unwrap());
        app.store
            .mark_session_spawning(session, &epoch, "boot")
            .unwrap();
        app.store
            .update_session_running(session, &epoch, process)
            .unwrap();
        crate::transcript::TranscriptSink::create(&app.paths.transcripts, session, &epoch)
            .unwrap()
            .append(output.as_bytes())
            .unwrap();
        (batch.id, epoch)
    };
    for (session, output, normal_exit, expected_state) in [
        ("exact", format!("{echo}{answer}\r\n"), true, "succeeded"),
        ("echo-only", echo.clone(), true, "failed"),
        ("wrong-nonce", wrong, true, "failed"),
        ("abnormal-exit", format!("{answer}\r\n"), false, "failed"),
    ] {
        let (batch_id, epoch) = check_output(session, &output);
        if session == "exact" {
            app.store.lock().unwrap().execute(
                    "UPDATE sessions SET launch_config_json=json_remove(launch_config_json,'$.provider_check') WHERE id=?1",
                    rusqlite::params![session],
                ).unwrap();
        }
        let exit = serde_json::json!({
            "success": normal_exit, "process_group_quiescent": true
        });
        assert!(app
            .store
            .update_session_exit(session, &epoch, process, &exit.to_string())
            .unwrap());
        assert_eq!(
            app.settle_one_provider_check().unwrap().unwrap()["action"],
            "provider_check_settled"
        );
        let settled = app.store.provider_validation_batch(&batch_id).unwrap();
        assert_eq!(settled.checks[0].state, expected_state, "{session}");
        assert_eq!(settled.state, "finished");
    }
    assert_eq!(model_proofs(&app), 1, "only the exact answer is proof");

    // A matching answer from a session whose processes are unproven stays unknown.
    let (batch_id, _) = check_output("uncertain", &format!("{answer}\r\n"));
    app.store
        .mark_session_delivery_ambiguous("uncertain", "provider root exited with live descendants")
        .unwrap();
    assert_eq!(app.settle_one_provider_check().unwrap(), None);
    let uncertain = app.store.provider_validation_batch(&batch_id).unwrap();
    assert_eq!(uncertain.checks[0].state, "delivery_unknown");
    assert_eq!(uncertain.state, "running");
    assert_eq!(model_proofs(&app), 1);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn expired_provider_check_requests_only_its_owner_once_and_keeps_unknown_stop_visible() {
    let (root, app, claude) = synthetic_app("deadline-stop");
    let batch = historical_batch(
        &app.store,
        "deadline-stop",
        &[
            select(Provider::Claude, "model-a"),
            select(Provider::Claude, "model-b"),
        ],
        &[claude.clone()],
        2,
    );
    let pending = app.store.pending_provider_checks().unwrap();
    for (check, session) in pending.iter().zip(["expired", "sibling"]) {
        assert!(reserve_expecting(
            &app.store, check, &claude, session, "hash"
        ));
        launch(&app.store, session);
    }
    let connection = app.store.lock().unwrap();
    let (created, deadline): (String, String) = connection
            .query_row(
                "SELECT created_at,json_extract(launch_config_json,'$.provider_check.deadline_at') FROM sessions WHERE id='expired'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&deadline).unwrap()
            - chrono::DateTime::parse_from_rfc3339(&created).unwrap(),
        ChronoDuration::seconds(300),
    );
    connection.execute(
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.provider_check.deadline_at','2000-01-01T00:00:00Z') WHERE id='expired'",
            [],
        ).unwrap();
    drop(connection);
    let requested = app.settle_one_provider_check().unwrap().unwrap();
    assert_eq!(requested["action"], "provider_check_timeout_requested");
    assert_eq!(requested["session_id"], "expired");
    assert_eq!(
        requested["provider_validation_invocation_id"],
        pending[0].invocation_id
    );
    let cancelled = app.store.provider_validation_batch(&batch.id).unwrap();
    assert_eq!(cancelled.state, "running");
    assert_eq!(cancelled.checks[0].state, "cancel_requested");
    let reason = cancelled.checks[0].reason.as_deref().unwrap();
    assert!(
        reason.contains("five-minute limit")
            && reason.contains("not attached to this service boot"),
        "{reason}"
    );
    assert_eq!(cancelled.checks[1].state, "running");
    let session = |store: &Store| -> (String, String, String, Option<String>, String, String) {
        store.lock().unwrap().query_row(
                "SELECT status,launch_config_json,transcript_epoch,process_identity_json,provider_validation_invocation_id,updated_at FROM sessions WHERE id='expired'",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            ).unwrap()
    };
    let snapshot = session(&app.store);
    assert_eq!(snapshot.0, "running");
    assert_eq!(snapshot.4, pending[0].invocation_id);
    assert_eq!(app.settle_one_provider_check().unwrap(), None);
    assert_eq!(
        app.store.provider_validation_batch(&batch.id).unwrap(),
        cancelled
    );
    assert_eq!(session(&app.store), snapshot);
    assert_eq!(model_proofs(&app), 0);

    let reopened = Store::open(&app.paths.database).unwrap();
    assert_eq!(session(&reopened), snapshot);
    assert_eq!(
        reopened.request_expired_provider_check_stop().unwrap(),
        None
    );
    drop(reopened);
    app.store
        .mark_session_delivery_ambiguous("expired", "stop ownership is unproven")
        .unwrap();
    assert_eq!(app.settle_one_provider_check().unwrap(), None);
    assert_eq!(model_proofs(&app), 0);
    assert!(app
        .store
        .record_provider_check_quiescent(
            "expired",
            &pending[0].invocation_id,
            &serde_json::json!({"source": "synthetic"}),
        )
        .unwrap());
    assert_eq!(
        app.settle_one_provider_check().unwrap().unwrap()["action"],
        "provider_check_settled"
    );
    let settled = app.store.provider_validation_batch(&batch.id).unwrap();
    assert_eq!(settled.checks[0].state, "cancelled");
    assert_eq!(
        settled.checks[0].reason.as_deref(),
        Some(crate::store::PROVIDER_CHECK_TIMEOUT)
    );
    assert_eq!(settled.checks[1], cancelled.checks[1]);
    assert_eq!(settled.state, "running");
    assert_eq!(app.settle_one_provider_check().unwrap(), None);
    assert!(app.store.pending_provider_checks().unwrap().is_empty());
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unconfirmed_cancel_stop_stays_visible_on_the_check() {
    let (root, app, claude) = synthetic_app("cancel-stop");
    let batch = historical_batch(
        &app.store,
        "cancel-stop",
        &[select(Provider::Claude, "model-a")],
        &[claude.clone()],
        1,
    );
    let check = app.store.pending_provider_checks().unwrap().remove(0);
    assert!(reserve_expecting(
        &app.store,
        &check,
        &claude,
        "unattached",
        "hash"
    ));
    // Running in the store with no supervised handle, so the stop cannot be delivered.
    launch(&app.store, "unattached");
    let cancelled = app.cancel_provider_validation(&batch.id).unwrap();
    assert_eq!(cancelled.state, "cancel_requested");
    assert_eq!(cancelled.checks[0].state, "cancel_requested");
    let reason = cancelled.checks[0].reason.as_deref().unwrap_or_default();
    assert!(reason.contains("could not confirm the stop"), "{reason}");
    assert!(
        reason.contains("not attached to this service boot"),
        "{reason}"
    );
    let status: String = app
        .store
        .lock()
        .unwrap()
        .query_row(
            "SELECT status FROM sessions WHERE id='unattached'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "running",
        "a failed stop is not reported as stopped"
    );
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retired_app_wide_start_and_dispatch_create_no_session_or_project_authority() {
    let (root, app, claude) = synthetic_app("retired-dispatch");
    let selections = [
        select(Provider::Claude, "model-a"),
        select(Provider::Claude, "model-b"),
    ];
    let batch = historical_batch(&app.store, "historical", &selections, &[claude], 2);
    let request = crate::domain::ProviderValidationStartRequest {
        operation_id: "historical".into(),
        preview_hash: app
            .store
            .lock()
            .unwrap()
            .query_row(
                "SELECT preview_hash FROM provider_validation_batches WHERE id=?1",
                rusqlite::params![batch.id],
                |row| row.get(0),
            )
            .unwrap(),
        selections: selections.to_vec(),
        authorize_model_calls: true,
        max_model_calls: 2,
    };
    assert_eq!(app.start_provider_validation(&request).unwrap(), batch);
    let mut fresh = request.clone();
    fresh.operation_id = "fresh".into();
    fresh.preview_hash = "stale-client".into();
    assert_eq!(
        app.start_provider_validation(&fresh)
            .unwrap_err()
            .to_string(),
        crate::store::PROVIDER_CHECK_PROJECT_REQUIRED
    );
    for check in &batch.checks {
        let skipped = app.dispatch_one_provider_check().unwrap().unwrap();
        assert_eq!(skipped["action"], "provider_check_skipped");
        assert_eq!(skipped["provider_validation_invocation_id"], check.id);
    }
    assert_eq!(app.dispatch_one_provider_check().unwrap(), None);
    let settled = app.inspect_provider_validation(&batch.id).unwrap();
    assert_eq!(settled.state, "finished");
    assert!(settled.checks.iter().all(|check| check.state == "skipped"
        && check.reason.as_deref() == Some(crate::store::PROVIDER_CHECK_PROJECT_REQUIRED)));
    assert_eq!(app.start_provider_validation(&request).unwrap(), settled);
    let connection = app.store.lock().unwrap();
    let authority: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions)+(SELECT COUNT(*) FROM projects)
                +(SELECT COUNT(*) FROM role_generations)+(SELECT COUNT(*) FROM role_credentials)
                +(SELECT COUNT(*) FROM capabilities)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(authority, 0);
    assert!(
        !app.paths.artifacts.join("provider-validation").exists(),
        "start refusal and historical dispatch perform no local preparation"
    );
    drop(connection);
    drop(app);
    std::fs::remove_dir_all(root).unwrap();
}
