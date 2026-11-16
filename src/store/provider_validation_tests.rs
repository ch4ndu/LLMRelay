use super::*;
use crate::domain::ExecutableFingerprint;
use crate::provider_compatibility::{BundleSet, PolicyMode};
use crate::providers::{ProviderAccountObservation, ProviderInstallation};

const PROCESS: &str = "{\"pid\":7}";

fn qualified(manifest: &str) -> String {
    let mut pack: serde_json::Value = serde_json::from_str(manifest).unwrap();
    for selector in pack["selectors"].as_array_mut().unwrap() {
        for contract in selector["contracts"].as_array_mut().unwrap() {
            if contract["mode"] == "provider_observation" {
                contract["qualification"] = serde_json::json!({
                    "state": "qualified",
                    "evidence_reference": "synthetic-review"
                });
            }
        }
    }
    pack.to_string()
}

pub(crate) fn policy_bundles(qualify: bool) -> BundleSet {
    if !qualify {
        return BundleSet::embedded();
    }
    BundleSet::embedded().with_policy_packs_for_tests(
        &qualified(include_str!(
            "../../resources/provider-compatibility/single-agent-codex.json"
        )),
        &qualified(include_str!(
            "../../resources/provider-compatibility/single-agent-claude.json"
        )),
    )
}

pub(crate) fn inspection(
    bundles: &BundleSet,
    provider: Provider,
    account: Option<&str>,
) -> ProviderLocalInspection {
    let version = match provider {
        Provider::Codex => crate::providers::codex::EXACT_CODEX_VERSION,
        Provider::Claude => crate::providers::claude::EXACT_CLAUDE_VERSION,
    };
    ProviderLocalInspection {
        provider,
        installation: Ok(ProviderInstallation {
            provider,
            executable: ExecutableFingerprint {
                canonical_path: format!("/synthetic/{provider}").into(),
                device: 1,
                inode: 2,
                bytes: 3,
                modified_seconds: 4,
                modified_nanos: 5,
            },
            executable_version: version.to_owned(),
        }),
        account: Ok(match provider {
            Provider::Codex => ProviderAccountObservation {
                account_identity: account.map(str::to_owned),
                authentication: "chatgpt_pro".into(),
                config_identity: serde_json::json!({"revision": "synthetic"}),
            },
            Provider::Claude => crate::providers::claude::inspect_observation_account(),
        }),
        policy: bundles
            .resolve_policy(provider, version, PolicyMode::ProviderObservation)
            .map_err(|error| error.explanation().message.clone()),
        catalog: Some(vec![("model-a".into(), vec!["low".into()])]),
    }
}

pub(crate) fn select(provider: Provider, model: &str) -> ProviderValidationSelection {
    ProviderValidationSelection {
        provider,
        model: model.into(),
        effort: "low".into(),
    }
}

fn request(
    operation: &str,
    preview_hash: &str,
    selections: &[ProviderValidationSelection],
    authorize: bool,
    max: u32,
) -> ProviderValidationStartRequest {
    ProviderValidationStartRequest {
        operation_id: operation.into(),
        preview_hash: preview_hash.into(),
        selections: selections.to_vec(),
        authorize_model_calls: authorize,
        max_model_calls: max,
    }
}

fn open(label: &str) -> (std::path::PathBuf, Store) {
    let root = std::env::temp_dir().join(format!(
        "llmrelay-provider-check-{label}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("state.sqlite3")).unwrap();
    (root, store)
}

pub(crate) fn historical_batch(
    store: &Store,
    operation: &str,
    selections: &[ProviderValidationSelection],
    inspections: &[ProviderLocalInspection],
    max: u32,
) -> ProviderValidationBatchDto {
    let preview = store
        .preview_provider_validation(selections, inspections)
        .unwrap();
    let request = request(operation, &preview.preview_hash, selections, max > 0, max);
    let batch_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let mut connection = store.lock().unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    transaction
            .execute(
                "INSERT INTO provider_validation_batches(id,operation_id,request_hash,preview_hash,selection_json,
                   max_fresh_calls,native_calls_authorized,state,created_at,updated_at,finished_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10)",
                params![
                    batch_id,
                    operation,
                    provider_validation_request_hash(&request).unwrap(),
                    preview.preview_hash,
                    serde_json::to_string(selections).unwrap(),
                    max,
                    max > 0,
                    if max > 0 { "running" } else { "finished" },
                    now,
                    (max == 0).then_some(&now)
                ],
            )
            .unwrap();
    for inspection in inspections {
        record_local_provider_observations_in(&transaction, &batch_id, inspection, &now).unwrap();
    }
    for (ordinal, selection) in selections.iter().enumerate() {
        let evidence = inspections
            .iter()
            .find(|value| value.provider == selection.provider)
            .unwrap()
            .evidence()
            .unwrap();
        transaction
                .execute(
                    "INSERT INTO provider_validation_invocations(id,batch_id,ordinal,provider,model,effort,
                       evidence_key,state,created_at,updated_at,finished_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        batch_id,
                        ordinal as i64,
                        selection.provider.to_string(),
                        selection.model,
                        selection.effort,
                        evidence.model_access_key(&selection.model, &selection.effort).unwrap(),
                        if max > 0 { "pending" } else { "skipped" },
                        now,
                        (max == 0).then_some(&now)
                    ],
                )
                .unwrap();
    }
    transaction.commit().unwrap();
    provider_validation_batch_in(&connection, &batch_id).unwrap()
}

fn reserve(
    store: &Store,
    check: &PendingProviderCheck,
    inspection: &ProviderLocalInspection,
    session: &str,
) -> bool {
    reserve_expecting(store, check, inspection, session, "synthetic-nonce-hash")
}

pub(crate) fn reserve_expecting(
    store: &Store,
    check: &PendingProviderCheck,
    inspection: &ProviderLocalInspection,
    session: &str,
    nonce_hash: &str,
) -> bool {
    let launch = serde_json::json!({"kind": "provider_observation", "evidence": inspection.evidence().unwrap()});
    store
        .reserve_provider_validation_session(&ProviderValidationSessionReservation {
            invocation_id: &check.invocation_id,
            session_id: session,
            transcript_epoch: &format!("{session}-epoch"),
            provider: check.provider,
            executable_version: "synthetic",
            launch_json: &launch.to_string(),
            scope_root: "/synthetic/scope",
            nonce_hash,
        })
        .unwrap()
}

/// Plays the supervisor's part: spawn, run, then exit with a quiescent group.
pub(crate) fn launch(store: &Store, session: &str) {
    let epoch = format!("{session}-epoch");
    store
        .mark_session_spawning(session, &epoch, "boot")
        .unwrap();
    store
        .update_session_running(session, &epoch, PROCESS)
        .unwrap();
}

pub(crate) fn exit(store: &Store, session: &str, success: bool) {
    let exit = serde_json::json!({"success": success, "process_group_quiescent": true});
    assert!(store
        .update_session_exit(
            session,
            &format!("{session}-epoch"),
            PROCESS,
            &exit.to_string()
        )
        .unwrap());
}

fn scalar<T: rusqlite::types::FromSql>(store: &Store, sql: &str, value: &str) -> T {
    store
        .lock()
        .unwrap()
        .query_row(sql, params![value], |row| row.get(0))
        .unwrap()
}

fn count(store: &Store, sql: &str) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn check_state(store: &Store, invocation: &str) -> String {
    scalar(
        store,
        "SELECT state FROM provider_validation_invocations WHERE id=?1",
        invocation,
    )
}

#[test]
fn provider_inspection_is_local_and_new_app_wide_starts_require_a_project() {
    let (root, store) = open("local-inspection");
    let bundles = policy_bundles(false);
    let selections = [
        select(Provider::Codex, "model-a"),
        select(Provider::Claude, "model-b"),
    ];
    let mut inspections = [
        inspection(&bundles, Provider::Codex, Some("chatgpt:one")),
        inspection(&bundles, Provider::Claude, None),
    ];
    inspections[0].policy = Err("coding qualification unavailable".into());
    let preview = store
        .preview_provider_validation(&selections, &inspections)
        .unwrap();
    assert_eq!(preview.model_calls_required, 0);
    assert!(preview
        .installations
        .iter()
        .all(|value| value.ready && value.guidance.is_none()));
    assert_eq!(
        preview.installations[0].account_identity.as_deref(),
        Some("chatgpt:one")
    );
    assert_eq!(
        preview.installations[1].authentication.as_deref(),
        Some("not_inspected")
    );
    assert!(preview
        .checks
        .iter()
        .all(|value| value.plan == ProviderCheckPlan::Unavailable
            && value.reason == PROVIDER_CHECK_PROJECT_REQUIRED
            && value.proof_observed_at.is_none()));
    assert_eq!(preview.checks[0].model_listed, Some(true));
    assert_eq!(preview.checks[1].model_listed, Some(false));
    inspections[0].policy =
        inspection(&policy_bundles(true), Provider::Codex, Some("chatgpt:one")).policy;
    assert_eq!(
        preview,
        store
            .preview_provider_validation(&selections, &inspections)
            .unwrap()
    );
    inspections[0]
        .installation
        .as_mut()
        .unwrap()
        .executable_version = "changed-local-version".into();
    assert_ne!(
        preview.preview_hash,
        store
            .preview_provider_validation(&selections, &inspections)
            .unwrap()
            .preview_hash
    );
    inspections[1].installation = Err("Install Claude Code or fix PATH.".into());
    let missing = store
        .preview_provider_validation(&selections, &inspections)
        .unwrap();
    assert!(!missing.installations[1].ready);
    assert_eq!(
        missing.installations[1].guidance.as_deref(),
        Some("Install Claude Code or fix PATH.")
    );
    for refused in [
        request("authorized", &preview.preview_hash, &selections, true, 2),
        request("stale-client", "old-preview", &selections, true, 8),
        request("local-only", &preview.preview_hash, &selections, false, 0),
    ] {
        assert_eq!(
            store
                .start_provider_validation(&refused)
                .unwrap_err()
                .to_string(),
            PROVIDER_CHECK_PROJECT_REQUIRED
        );
    }
    assert_eq!(
        count(
            &store,
            "SELECT (SELECT COUNT(*) FROM provider_validation_batches)
            +(SELECT COUNT(*) FROM provider_observations)+(SELECT COUNT(*) FROM sessions)
            +(SELECT COUNT(*) FROM capabilities)+(SELECT COUNT(*) FROM projects)
            +(SELECT COUNT(*) FROM role_generations)+(SELECT COUNT(*) FROM role_credentials)"
        ),
        0
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn historical_start_replays_and_unknown_start_closes_without_new_work() {
    use crate::domain::ProviderValidationStartOutcome;
    let (root, store) = open("reconcile");
    let bundles = policy_bundles(true);
    let selections = [select(Provider::Codex, "model-a")];
    let inspections = [inspection(&bundles, Provider::Codex, Some("chatgpt:one"))];
    let preview = store
        .preview_provider_validation(&selections, &inspections)
        .unwrap();
    let first = request("first", &preview.preview_hash, &selections, true, 1);
    let batch = historical_batch(&store, "first", &selections, &inspections, 1);
    for index in 0..5 {
        historical_batch(
            &store,
            &format!("later-{index}"),
            &selections,
            &inspections,
            0,
        );
    }
    let recent = provider_validation_state_in(&store.lock().unwrap()).unwrap();
    assert!(recent.batches.iter().all(|listed| listed.id != batch.id));
    let started = ProviderValidationStartOutcome::Started {
        batch: batch.clone(),
    };
    assert_eq!(
        store.provider_validation_start_outcome(&first).unwrap(),
        Some(started.clone())
    );
    assert_eq!(
        store.reconcile_provider_validation_start(&first).unwrap(),
        started
    );
    assert_eq!(store.start_provider_validation(&first).unwrap(), batch);
    let changed = request("first", &preview.preview_hash, &selections, true, 2);
    assert!(store
        .start_provider_validation(&changed)
        .unwrap_err()
        .to_string()
        .contains("different provider check request"));

    let lost = request("lost", &preview.preview_hash, &selections, true, 1);
    let closed = store.reconcile_provider_validation_start(&lost).unwrap();
    assert!(
        matches!(&closed, ProviderValidationStartOutcome::ClosedWithoutStart { operation_id, .. }
            if operation_id == "lost")
    );
    assert_eq!(
        store.reconcile_provider_validation_start(&lost).unwrap(),
        closed
    );
    assert_eq!(
        store.provider_validation_start_outcome(&lost).unwrap(),
        Some(closed)
    );
    assert!(store
        .start_provider_validation(&lost)
        .unwrap_err()
        .to_string()
        .contains("closed without starting"));
    let changed = request("lost", &preview.preview_hash, &selections, true, 2);
    for error in [
        store
            .reconcile_provider_validation_start(&changed)
            .unwrap_err(),
        store.start_provider_validation(&changed).unwrap_err(),
    ] {
        assert!(
            error
                .to_string()
                .contains("different provider check request"),
            "{error}"
        );
    }
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM provider_validation_batches WHERE operation_id='lost'"
        ),
        0
    );
    assert_eq!(store.pending_provider_checks().unwrap().len(), 1);
    assert_eq!(
        store
            .start_provider_validation(&request("fresh", "stale-preview", &selections, true, 1))
            .unwrap_err()
            .to_string(),
        PROVIDER_CHECK_PROJECT_REQUIRED
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM sessions"), 0);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM provider_validation_batches"),
        6
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn historical_provider_results_remain_visible_without_reusing_proof_for_new_checks() {
    let (root, store) = open("historical-proof");
    let bundles = policy_bundles(true);
    let codex = inspection(&bundles, Provider::Codex, Some("chatgpt:one"));
    let claude = inspection(&bundles, Provider::Claude, None);
    let inspections = [codex.clone(), claude.clone()];
    let selections = [
        select(Provider::Codex, "model-a"),
        select(Provider::Codex, "model-b"),
        select(Provider::Claude, "model-a"),
    ];
    let batch = historical_batch(&store, "historical", &selections, &inspections, 3);
    let pending = store.pending_provider_checks().unwrap();
    for (index, succeeded) in [(0, true), (1, false), (2, true)] {
        let check = &pending[index];
        let session = format!("session-{index}");
        let owner = if check.provider == Provider::Codex {
            &codex
        } else {
            &claude
        };
        assert!(reserve(&store, check, owner, &session));
        launch(&store, &session);
        assert!(!store
            .settle_provider_check(
                &check.invocation_id,
                &session,
                ProviderCheckOutcome::Succeeded
            )
            .unwrap());
        exit(&store, &session, succeeded);
        assert!(store
            .settle_provider_check(
                &check.invocation_id,
                &session,
                if succeeded {
                    ProviderCheckOutcome::Succeeded
                } else {
                    ProviderCheckOutcome::Failed("The provider exited with an error.")
                }
            )
            .unwrap());
    }
    let settled = store.provider_validation_batch(&batch.id).unwrap();
    assert_eq!(settled.state, "finished");
    assert_eq!(
        settled
            .checks
            .iter()
            .map(|check| check.state.as_str())
            .collect::<Vec<_>>(),
        ["succeeded", "failed", "succeeded"]
    );
    let observed = chrono::DateTime::parse_from_rfc3339(
        settled.checks[0].proof_observed_at.as_deref().unwrap(),
    )
    .unwrap();
    let expires = chrono::DateTime::parse_from_rfc3339(
        settled.checks[0].proof_expires_at.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(expires - observed, chrono::Duration::hours(24));
    let before = provider_validation_state_in(&store.lock().unwrap()).unwrap();
    assert_eq!(before.proofs.len(), 2);
    let preview = store
        .preview_provider_validation(&selections, &inspections)
        .unwrap();
    assert!(preview
        .checks
        .iter()
        .all(|check| check.plan == ProviderCheckPlan::Unavailable
            && check.proof_observed_at.is_none()
            && check.proof_expires_at.is_none()));
    assert_eq!(
        store
            .start_provider_validation(&request(
                "fresh",
                &preview.preview_hash,
                &selections,
                true,
                3
            ))
            .unwrap_err()
            .to_string(),
        PROVIDER_CHECK_PROJECT_REQUIRED
    );
    assert_eq!(
        before,
        provider_validation_state_in(&store.lock().unwrap()).unwrap()
    );
    assert_eq!(
        count(
            &store,
            "SELECT (SELECT COUNT(*) FROM capabilities)+(SELECT COUNT(*) FROM projects)
            +(SELECT COUNT(*) FROM attempts)"
        ),
        0
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn historical_check_deadline_and_retired_metadata_cannot_publish_success() {
    let (root, store) = open("deadline-publication");
    let codex = inspection(&policy_bundles(true), Provider::Codex, Some("chatgpt:one"));
    for (session, expected_state) in [
        ("late", "cancelled"),
        ("malformed", "failed"),
        ("retired", "failed"),
        ("unknown", "failed"),
        ("old-without-deadline", "succeeded"),
    ] {
        let batch = historical_batch(
            &store,
            session,
            &[select(Provider::Codex, session)],
            &[codex.clone()],
            1,
        );
        let check = store.pending_provider_checks().unwrap().remove(0);
        assert!(reserve(&store, &check, &codex, session));
        launch(&store, session);
        exit(&store, session, true);
        let snapshot = store.unsettled_provider_checks().unwrap().remove(0);
        assert!(snapshot.quiescent && snapshot.exit_success && !snapshot.deadline_expired);
        assert_eq!(snapshot.challenge, ProviderCheckChallenge::Answer);
        let expression = match session {
                "late" => "json_set(launch_config_json,'$.provider_check.deadline_at','2000-01-01T00:00:00Z')",
                "malformed" => "json_set(launch_config_json,'$.provider_check.deadline_at','invalid')",
                "retired" => "json_set(launch_config_json,'$.policy.authority','disposable_scope_write','$.evidence.adapter_id','codex-provider-disposable-check','$.provider_check.challenge','owned_file_v1','$.evidence.protection_inputs_hash','old-hash')",
                "unknown" => "json_set(launch_config_json,'$.provider_check.challenge','unknown')",
                _ => "json_remove(launch_config_json,'$.provider_check')",
            };
        store
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE sessions SET launch_config_json={expression} WHERE id=?1"),
                params![session],
            )
            .unwrap();
        assert_eq!(store.request_expired_provider_check_stop().unwrap(), None);
        let current = store.unsettled_provider_checks().unwrap().remove(0);
        assert_eq!(current.deadline_expired, session == "late");
        if matches!(session, "retired" | "unknown" | "malformed") {
            assert_eq!(current.challenge, ProviderCheckChallenge::Invalid);
        }
        assert!(store
            .settle_provider_check(
                &check.invocation_id,
                session,
                ProviderCheckOutcome::Succeeded
            )
            .unwrap());
        let settled = store.provider_validation_batch(&batch.id).unwrap();
        assert_eq!(settled.checks[0].state, expected_state, "{session}");
        if session == "late" {
            assert_eq!(
                settled.checks[0].reason.as_deref(),
                Some(PROVIDER_CHECK_TIMEOUT)
            );
        }
        let result: Option<String> = scalar(
            &store,
            "SELECT result_hash FROM provider_validation_invocations WHERE id=?1",
            &check.invocation_id,
        );
        assert_eq!(result.is_some(), expected_state == "succeeded");
        assert!(!store
            .settle_provider_check(
                &check.invocation_id,
                session,
                ProviderCheckOutcome::Succeeded
            )
            .unwrap());
    }
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM provider_observations WHERE kind='model_access'"
        ),
        1
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_check_cancel_and_restart_never_relaunch_or_guess() {
    let (root, store) = open("cancel");
    let bundles = policy_bundles(true);
    let codex = inspection(&bundles, Provider::Codex, Some("chatgpt:one"));
    let selections = [
        select(Provider::Codex, "model-a"),
        select(Provider::Codex, "model-b"),
        select(Provider::Codex, "model-c"),
    ];
    let batch = historical_batch(&store, "cancel", &selections, &[codex.clone()], 3);
    let pending = store.pending_provider_checks().unwrap();
    assert!(reserve(&store, &pending[0], &codex, "running"));
    launch(&store, "running");
    assert!(reserve(&store, &pending[1], &codex, "reserved"));
    assert!(!store
        .provider_check_capacity_available(Provider::Codex)
        .unwrap());
    assert!(
        !reserve(&store, &pending[2], &codex, "waiting"),
        "a full provider waits"
    );
    assert_eq!(check_state(&store, &pending[2].invocation_id), "pending");

    let (cancelled, running) = store.cancel_provider_validation(&batch.id).unwrap();
    assert_eq!(running, ["running"]);
    assert_eq!(cancelled.state, "cancel_requested");
    assert_eq!(
        cancelled
            .checks
            .iter()
            .map(|check| check.state.as_str())
            .collect::<Vec<_>>(),
        ["cancel_requested", "cancelled", "cancelled"]
    );
    assert!(cancelled.checks[1..].iter().all(|check| check
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("No model was contacted"))));
    assert!(
        store
            .mark_session_spawning("reserved", "reserved-epoch", "boot")
            .is_err(),
        "a check cancelled before spawning can never spawn"
    );
    assert!(store.pending_provider_checks().unwrap().is_empty());

    exit(&store, "running", true);
    assert!(
        !store
            .record_provider_check_stop_failure("running", "signal failed")
            .unwrap(),
        "a check that already stopped records no stop failure"
    );
    let unsettled = store.unsettled_provider_checks().unwrap();
    assert!(unsettled[0].cancel_requested);
    assert!(store
        .settle_provider_check(
            &pending[0].invocation_id,
            "running",
            ProviderCheckOutcome::Cancelled("Cancelled.")
        )
        .unwrap());
    assert_eq!(
        store.provider_validation_batch(&batch.id).unwrap().state,
        "cancelled"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM provider_observations WHERE kind='model_access'"
        ),
        0
    );

    // A success computed before a later cancel commits cannot publish proof.
    let batch = historical_batch(&store, "stale", &selections[..1], &[codex.clone()], 1);
    let pending = store.pending_provider_checks().unwrap();
    assert!(reserve(&store, &pending[0], &codex, "answered"));
    launch(&store, "answered");
    exit(&store, "answered", true);
    let snapshot = store.unsettled_provider_checks().unwrap();
    assert!(!snapshot[0].cancel_requested && snapshot[0].exit_success);
    let (_, running) = store.cancel_provider_validation(&batch.id).unwrap();
    assert!(running.is_empty(), "an exited check needs no stop");
    assert!(store
        .settle_provider_check(
            &pending[0].invocation_id,
            "answered",
            ProviderCheckOutcome::Succeeded
        )
        .unwrap());
    let stale = store.provider_validation_batch(&batch.id).unwrap();
    assert_eq!(
        (stale.state.as_str(), stale.checks[0].state.as_str()),
        ("cancelled", "cancelled")
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM provider_observations WHERE kind='model_access'"
        ),
        0
    );

    // A restart proves a never-spawned check was not run and leaves a launched one uncertain.
    let batch = historical_batch(&store, "restart", &selections[..2], &[codex.clone()], 2);
    let pending = store.pending_provider_checks().unwrap();
    assert!(reserve(&store, &pending[0], &codex, "unspawned"));
    assert!(reserve(&store, &pending[1], &codex, "launched"));
    launch(&store, "launched");
    {
        let mut connection = store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        let results = reconcile_prior_boot_provider_checks_in(&transaction).unwrap();
        transaction.commit().unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result["relaunched"] == false));
    }
    assert_eq!(check_state(&store, &pending[0].invocation_id), "failed");
    assert_eq!(
        check_state(&store, &pending[1].invocation_id),
        "delivery_unknown"
    );
    let status = |session: &str| -> String {
        scalar(&store, "SELECT status FROM sessions WHERE id=?1", session)
    };
    assert_eq!(status("unspawned"), "launch_failed");
    assert_eq!(status("launched"), "recovery_required");
    assert_eq!(
        store.provider_validation_batch(&batch.id).unwrap().state,
        "running"
    );
    assert!(store.pending_provider_checks().unwrap().is_empty());
    assert!(
        !store
            .settle_provider_check(
                &pending[1].invocation_id,
                "launched",
                ProviderCheckOutcome::Succeeded
            )
            .unwrap(),
        "an uncertain check settles only after its processes are proven gone"
    );
    assert!(store
        .record_provider_check_quiescent(
            "launched",
            &pending[1].invocation_id,
            &serde_json::json!({"source": "synthetic"})
        )
        .unwrap());
    let unsettled = store.unsettled_provider_checks().unwrap();
    assert!(unsettled[0].quiescent && !unsettled[0].exit_success);
    assert!(store
        .settle_provider_check(
            &pending[1].invocation_id,
            "launched",
            ProviderCheckOutcome::Failed(
                "LLMRelay could not confirm this check finished normally."
            )
        )
        .unwrap());
    assert_eq!(
        store.provider_validation_batch(&batch.id).unwrap().state,
        "finished"
    );

    assert_eq!(
        count(
            &store,
            "SELECT (SELECT COUNT(*) FROM recovery_records)+(SELECT COUNT(*) FROM attempts)"
        ),
        0
    );
    let summary = store.session_status_summary().unwrap();
    assert!(summary["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|session| {
            session["owner"] == "provider_validation_invocation" && session["task_id"].is_null()
        }));
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
