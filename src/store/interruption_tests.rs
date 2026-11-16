use super::*;
use crate::domain::Provider;
use crate::{config::InstancePaths, database};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn idle_disconnect_holds_through_eof_and_settles_only_at_exact_quiescence() {
    use std::os::unix::process::CommandExt;
    let root = std::env::temp_dir().join(format!("llmrelay-disconnect-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("state.sqlite3")).unwrap();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    let anchor = crate::domain::ProcessGenerationAnchor {
        pid,
        process_group_id: pid as i32,
        native_start_marker: crate::supervisor::native_start_marker(pid).unwrap(),
        boot_identity: crate::supervisor::system_boot_identity().unwrap(),
    };
    let process = serde_json::to_string(&crate::domain::ProcessIdentity {
        pid,
        process_group_id: anchor.process_group_id,
        native_start_marker: anchor.native_start_marker.clone(),
        observed_started_at: "now".into(),
    })
    .unwrap();
    store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/synthetic/repository','repository','head','now','now');
             INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
               VALUES('e','p',1,'{}','config','now','now');
             INSERT INTO conversation_profiles(id,project_id,execution_config_revision_id,profile_json,profile_hash,created_at)
               VALUES('pr','p','e','{}','profile','now');
             INSERT INTO conversation_admissions(id,profile_revision_id,identity_json,identity_hash,state,created_at,updated_at)
               VALUES('a','pr','{}','identity','active','now','now');
             INSERT INTO conversations(id,project_id,admission_id,identity_json,identity_hash,generation,native_thread_id,state,created_at,updated_at)
               VALUES('c','p','a','{}','identity','g','thread','running','now','now');
             INSERT INTO sessions(id,conversation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('s','c','codex','running','{}','synthetic','g','now','now');
             INSERT INTO conversation_reservations(conversation_id,session_id,repository_identity,state,created_at,updated_at)
               VALUES('c','s','repository','running','now','now');"
        ).unwrap();
    store.lock().unwrap().execute(
            "UPDATE sessions SET process_identity_json=?1,recovery_anchor_json=?2,recovery_process_group_id=?3,launch_boot_identity=?4 WHERE id='s'",
            params![process,serde_json::to_string(&anchor).unwrap(),anchor.process_group_id,anchor.boot_identity],
        ).unwrap();
    let request = serde_json::json!({"action":"disconnect_idle"});
    let reserve = |session, generation, process: &str| {
        store.conversation_reserve_disconnect(
            "disconnect",
            &request,
            "c",
            session,
            generation,
            process,
        )
    };
    assert!(reserve("s", "g", &process).is_err(), "running is not idle");
    store
        .lock()
        .unwrap()
        .execute("UPDATE conversations SET state='idle' WHERE id='c'", [])
        .unwrap();
    assert!(reserve("wrong", "g", &process).is_err());
    assert!(reserve("s", "stale", &process).is_err());
    assert!(reserve("s", "g", "wrong-process").is_err());
    assert!(reserve("s", "g", &process).unwrap().1);
    assert!(store
        .conversation_reserve_input("submit", &request, "c", "new", None)
        .is_err());
    store.conversation_transport_closed("c", "g").unwrap();
    assert!(store
        .conversation_reconcile_quiescence("inspect-live", &request, "c")
        .is_err());
    let receipt = store
        .conversation_receipt("disconnect", &request)
        .unwrap()
        .unwrap();
    assert_eq!(receipt["delivery_state"], "sending");
    assert!(
        !reserve("s", "g", &process).unwrap().1,
        "replay cannot deliver"
    );
    let held: bool = store.lock().unwrap().query_row(
            "SELECT c.binding_valid=1 AND c.state='stopping' AND r.state='stopping'
             FROM conversations c JOIN conversation_reservations r ON r.conversation_id=c.id WHERE c.id='c'",
            [], |r|r.get(0),
        ).unwrap();
    assert!(held, "EOF is neither input uncertainty nor quiescence");
    store.reconcile_conversation_restart().unwrap();
    assert_eq!(
        store
            .conversation_receipt("disconnect", &request)
            .unwrap()
            .unwrap()["delivery_state"],
        "unknown"
    );
    assert!(!reserve("s", "g", &process).unwrap().1);
    child.kill().unwrap();
    child.wait().unwrap();
    let reconciled = store
        .conversation_reconcile_quiescence("quiet", &request, "c")
        .unwrap();
    assert_eq!(reconciled["reservation"], "released");
    assert_eq!(reconciled["unknown_delivery_preserved"], false);
    let receipt = store
        .conversation_receipt("disconnect", &request)
        .unwrap()
        .unwrap();
    assert_eq!(receipt["delivery_state"], "completed");
    assert_eq!(receipt["quiescence"], "verified");
    let preserved: bool = store.lock().unwrap().query_row(
            "SELECT binding_valid=1 AND native_thread_id='thread' AND generation='g' AND state='idle'
             FROM conversations WHERE id='c'", [], |r|r.get(0),
        ).unwrap();
    assert!(preserved);
    assert!(!reserve("s", "g", &process).unwrap().1);
    let unknown = serde_json::json!({"action":"submit","operation_id":"uncertain","conversation_id":"c","input":"unknown"});
    store.lock().unwrap().execute("INSERT INTO conversation_operations(operation_id,actor_key,request_hash,kind,conversation_id,generation,state,input_json,result_json,created_at,updated_at) VALUES('uncertain','human_control',?1,'submit','c','g','unknown',?2,'{}','now','now')",params![json_hash(&unknown).unwrap(),unknown.to_string()]).unwrap();
    let lookup = || {
        store
            .conversation_lookup_receipt("p", "uncertain", &unknown, &json_hash(&unknown).unwrap())
            .unwrap()
    };
    assert!(lookup().retained_uncertainty_session_id.is_none());
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE conversations SET state='history_only',binding_valid=0 WHERE id='c'",
            [],
        )
        .unwrap();
    assert_eq!(
        lookup().retained_uncertainty_session_id.as_deref(),
        Some("s")
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE conversation_operations SET generation='stale' WHERE operation_id='uncertain'",
            [],
        )
        .unwrap();
    assert!(lookup().retained_uncertainty_session_id.is_none());
}

#[test]
fn recorded_message_sources_require_complete_identity_and_confirmed_delivery() -> Result<()> {
    use crate::providers::codex_app_server::ConversationSourceSelection as Selection;
    let root =
        std::env::temp_dir().join(format!("llmrelay-message-sources-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root)?;
    let store = Store::open(&root.join("state.sqlite3"))?;
    store.lock()?.execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/synthetic/repository','repository','head','now','now');
             INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
               VALUES('e','p',1,'{}','config','now','now');
             INSERT INTO conversation_profiles(id,project_id,execution_config_revision_id,profile_json,profile_hash,created_at)
               VALUES('pr','p','e','{}','profile','now');
             INSERT INTO conversation_admissions(id,profile_revision_id,identity_json,identity_hash,state,created_at,updated_at)
               VALUES('a','pr','{}','identity','active','now','now');
             INSERT INTO conversations(id,project_id,admission_id,identity_json,identity_hash,generation,native_thread_id,state,active_turn_id,created_at,updated_at)
               VALUES('c','p','a','{}','identity','g','thread','running','turn','now','now');
             INSERT INTO conversation_operations(operation_id,actor_key,request_hash,kind,conversation_id,generation,native_thread_id,state,input_json,created_at,updated_at)
               VALUES('input','human_control','hash','submit','c','g','thread','sending','{}','now','now');
             INSERT INTO conversation_events(conversation_id,sequence,generation,native_thread_id,operation_id,kind,payload_json,observed_at)
               VALUES('c',1,'g','thread','input','user_message','{\"text\":\"question\"}','now');"
        )?;
    let pending = store.conversation_messages("p", "c")?;
    assert!(!pending.messages[0].selectable);
    let ack = serde_json::json!({"turn":{"id":"turn"}});
    store.conversation_response("c", "g", "input", &ack, false)?;
    let event =
        |kind, payload| store.conversation_event("c", "g", "thread", Some("turn"), kind, &payload);
    event(
        "agent_message",
        serde_json::json!({"item_id":"item","delta":"first "}),
    )?;
    for _ in 0..51 {
        event("usage", serde_json::json!({}))?;
    }
    event(
        "agent_message",
        serde_json::json!({"item_id":"item","delta":"second"}),
    )?;
    event("agent_message", serde_json::json!({"delta":"missing item"}))?;
    let before = store.conversation_messages("p", "c")?;
    assert_eq!(before.messages[0].id, pending.messages[0].id);
    assert!(before.messages[0].selectable);
    assert_eq!(before.messages[1].text, "first second");
    assert!(!before.messages[1].selectable);
    assert_eq!(before.messages[1].first_sequence, 2);
    assert_eq!(before.messages[1].last_sequence, 54);
    event("turn_completed", serde_json::json!({"status":"completed"}))?;
    let complete = store.conversation_messages("p", "c")?;
    assert_eq!(complete.messages[1].id, before.messages[1].id);
    assert!(complete.messages[1].selectable);
    assert!(!complete.messages[2].selectable);
    let source = Selection {
        message_id: complete.messages[1].id.clone(),
        content_hash: complete.messages[1].content_hash.clone(),
    };
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let select = |sources: &[Selection]| conversation_messages_in(&tx, "p", "c", Some(sources));
    assert_eq!(select(&[source.clone()])?.messages[0].text, "first second");
    let invalid = Selection {
        message_id: "absent".into(),
        ..source.clone()
    };
    assert!(select(&[invalid]).is_err());
    let changed = Selection {
        content_hash: "changed".into(),
        ..source.clone()
    };
    assert!(select(&[changed]).is_err());
    assert!(conversation_messages_in(&tx, "other", "c", None).is_err());
    tx.execute("UPDATE conversation_events SET payload_json='{\"status\":\"interrupted\"}' WHERE kind='turn_completed'",[])?;
    assert!(!conversation_messages_in(&tx, "p", "c", None)?.messages[1].selectable);
    assert!(select(&[source.clone()]).is_err());
    tx.execute("UPDATE conversation_events SET payload_json='{\"status\":\"failed\"}' WHERE kind='turn_completed'",[])?;
    assert!(!conversation_messages_in(&tx, "p", "c", None)?.messages[1].selectable);
    tx.rollback()?;
    drop(connection);
    let refused = serde_json::json!({"action":"prepare","operation_id":"refused-setup","profile_revision_id":"pr"});
    store.conversation_record_pre_delivery_refusal(
        &refused,
        "Saved profile is stale; prepare current setup.",
    )?;
    let lookup =
        store.conversation_lookup_receipt("p", "refused-setup", &refused, &json_hash(&refused)?)?;
    assert!(lookup.found);
    assert_eq!(lookup.state.as_deref(), Some("refused"));
    assert_eq!(lookup.delivery_state.as_deref(), Some("failed"));
    assert_eq!(lookup.profile_revision_id.as_deref(), Some("pr"));
    assert!(store
        .conversation_receipt("refused-setup", &refused)
        .unwrap_err()
        .to_string()
        .contains("Saved profile is stale"));
    store.conversation_record_pre_delivery_refusal(&refused, "replacement message")?;
    assert!(store
        .conversation_receipt("refused-setup", &refused)
        .unwrap_err()
        .to_string()
        .contains("Saved profile is stale"));
    let mut changed = refused.clone();
    changed["profile_revision_id"] = "other".into();
    assert!(store
        .conversation_record_pre_delivery_refusal(&changed, "changed")
        .is_err());
    let unknown = serde_json::json!({"action":"submit","operation_id":"held-input","conversation_id":"c","input":"held"});
    store.lock()?.execute("INSERT INTO conversation_operations(operation_id,actor_key,request_hash,kind,conversation_id,generation,state,input_json,result_json,created_at,updated_at) VALUES('held-input','human_control',?1,'submit','c','g','unknown',?2,'{}','now','now')",params![json_hash(&unknown)?,unknown.to_string()])?;
    store.conversation_record_pre_delivery_refusal(&unknown, "not proof of nondelivery")?;
    let held =
        store.conversation_lookup_receipt("p", "held-input", &unknown, &json_hash(&unknown)?)?;
    assert_eq!(held.delivery_state.as_deref(), Some("unknown"));
    assert_ne!(held.state.as_deref(), Some("refused"));
    let oversized = serde_json::json!({"action":"submit","operation_id":"legacy-large","conversation_id":"c","input":"\"".repeat(10000)});
    let parsed: crate::providers::codex_app_server::ConversationOperation =
        serde_json::from_value(oversized.clone())?;
    assert!(parsed.validate().is_err());
    let envelope = crate::providers::codex_app_server::ConversationOperation::Receipt {
        project_id: "p".into(),
        operation_id: "legacy-large".into(),
        original_request: oversized.clone(),
        request_hash: json_hash(&oversized)?,
    };
    envelope.validate()?;
    let mut accepted = oversized.clone();
    accepted["input"] = "\"".repeat(8000).into();
    let accepted_op: crate::providers::codex_app_server::ConversationOperation =
        serde_json::from_value(accepted.clone())?;
    accepted_op.validate()?;
    let accepted_envelope = crate::providers::codex_app_server::ConversationOperation::Receipt {
        project_id: "p".repeat(128),
        operation_id: "legacy-large".into(),
        original_request: accepted,
        request_hash: "0".repeat(64),
    };
    accepted_envelope.validate()?;
    let refused = store.conversation_lookup_receipt(
        "p",
        "legacy-large",
        &oversized,
        &json_hash(&oversized)?,
    )?;
    assert_eq!(refused.state.as_deref(), Some("refused"));
    assert!(store
        .conversation_lookup_receipt(
            "p",
            "legacy-large",
            &serde_json::to_value(envelope)?,
            &"0".repeat(64)
        )
        .is_err());
    store.lock()?.execute_batch(
            "INSERT INTO sessions(id,conversation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('spawn-session','c','codex','launch_reserved','{}','synthetic','g','now','now');
             INSERT INTO conversation_reservations(conversation_id,session_id,repository_identity,state,created_at,updated_at)
               VALUES('c','spawn-session','repository','reserved','now','now');"
        )?;
    store.lock()?.execute_batch("INSERT INTO conversation_operations(operation_id,actor_key,request_hash,kind,conversation_id,generation,state,input_json,result_json,created_at,updated_at) VALUES('peer-reserved','human_control','peer','start','c','g','reserved','{}','{}','now','now'),('peer-sending','human_control','peer','submit','c','g','sending','{}','{}','now','now');")?;
    for kind in ["start", "probe", "resume"] {
        let op = format!("spawn-{kind}");
        let request = if kind == "resume" {
            serde_json::json!({"action":kind,"operation_id":op,"conversation_id":"c"})
        } else if kind == "start" {
            serde_json::json!({"action":kind,"operation_id":op,"admission_id":"a","input":"hello"})
        } else {
            serde_json::json!({"action":kind,"operation_id":op,"admission_id":"a"})
        };
        store.lock()?.execute_batch("UPDATE conversations SET state='reserved',binding_valid=1 WHERE id='c'; UPDATE sessions SET launch_state='spawning',status='launch_reserved' WHERE id='spawn-session'; UPDATE conversation_reservations SET state='reserved' WHERE conversation_id='c';")?;
        store.lock()?.execute("INSERT INTO conversation_operations(operation_id,actor_key,request_hash,kind,conversation_id,generation,state,input_json,result_json,created_at,updated_at) VALUES(?1,'human_control',?2,?3,'c','g','reserved',?4,?5,'now','now')",params![op,json_hash(&request)?,kind,request.to_string(),serde_json::json!({"session_id":"spawn-session"}).to_string()])?;
        assert!(store
            .conversation_proven_spawn_failure("c", "stale-session")
            .is_err());
        store.conversation_proven_spawn_failure("c", "spawn-session")?;
        let receipt =
            store.conversation_lookup_receipt("p", &op, &request, &json_hash(&request)?)?;
        assert_eq!(receipt.state.as_deref(), Some("refused"));
        assert_eq!(receipt.delivery_state.as_deref(), Some("failed"));
        assert!(store.conversation_receipt(&op, &request).is_err());
        let peer_states: (String,String) = store.lock()?.query_row("SELECT (SELECT state FROM conversation_operations WHERE operation_id='peer-reserved'),(SELECT state FROM conversation_operations WHERE operation_id='peer-sending')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(peer_states, ("reserved".into(), "sending".into()));
        assert_eq!(
            store
                .conversation_lookup_receipt("p", "held-input", &unknown, &json_hash(&unknown)?)?
                .delivery_state
                .as_deref(),
            Some("unknown")
        );
    }
    let unowned = serde_json::json!({"action":"prepare","operation_id":"no-owner","profile_revision_id":"missing"});
    store.conversation_record_pre_delivery_refusal(&unowned, "No saved owner")?;
    assert!(
        !store
            .conversation_lookup_receipt("p", "no-owner", &unowned, &json_hash(&unowned)?)?
            .found
    );
    let create = serde_json::json!({"operation_id":"refused-profile","project_id":"p","execution_config_revision_id":"missing","model":"gpt-6.1-sol","effort":"medium","instructions":"read"});
    store.conversation_record_pre_delivery_refusal(
        &create,
        "Choose a saved execution configuration",
    )?;
    let original = serde_json::json!({"action":"create_profile","input":create});
    assert!(
        store
            .conversation_lookup_receipt(
                "p",
                "refused-profile",
                &original,
                &json_hash(&original["input"])?
            )?
            .found
    );
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn resumed_historical_usage_is_ignored_only_for_validated_history() {
    let root = std::env::temp_dir().join(format!("llmrelay-resume-usage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("state.sqlite3")).unwrap();
    store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/synthetic/repository','repository','head','now','now');
             INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
               VALUES('e','p',1,'{}','config','now','now');
             INSERT INTO conversation_profiles(id,project_id,execution_config_revision_id,profile_json,profile_hash,created_at)
               VALUES('pr','p','e','{}','profile','now');
             INSERT INTO conversation_admissions(id,profile_revision_id,identity_json,identity_hash,state,created_at,updated_at)
               VALUES('a','pr','{}','identity','active','now','now');
             INSERT INTO conversations(id,project_id,admission_id,identity_json,identity_hash,generation,native_thread_id,state,created_at,updated_at)
               VALUES('c','p','a','{}','identity','new','thread','reserved','now','now');
             INSERT INTO sessions(id,conversation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('s','c','codex','running','{}','synthetic','new','now','now');
             INSERT INTO conversation_reservations(conversation_id,session_id,repository_identity,state,created_at,updated_at)
               VALUES('c','s','repository','running','now','now');
             INSERT INTO conversation_operations(operation_id,actor_key,request_hash,kind,conversation_id,generation,native_thread_id,native_turn_id,state,input_json,result_json,created_at,updated_at)
               VALUES('first','human_control','first','start','c','old','thread','prior','completed','{}','{\"outcome\":\"completed\"}','now','now'),
                     ('resume','human_control','resume','resume','c','new','thread',NULL,'reserved','{}','{}','now','now');"
        ).unwrap();
    let history = serde_json::json!({"id":"thread","turns":[{"id":"prior","status":"completed"}]});
    let mut divergent = history.clone();
    divergent["turns"][0]["status"] = "failed".into();
    assert!(store
        .conversation_validate_history("c", &divergent)
        .is_err());
    let validated = store.conversation_validate_history("c", &history).unwrap();
    assert!(store
        .conversation_restored_usage("c", "new", "thread", Some("prior"), "usage", &validated)
        .is_err());
    store
        .conversation_bind_thread("c", "new", "thread")
        .unwrap();
    store
        .conversation_resume_bound("c", "new", "resume")
        .unwrap();
    let payload = serde_json::json!({"last":{"total_tokens":10},"cumulative":{"total_tokens":10}});
    // The original event fence rejects this legitimate post-resume snapshot.
    assert!(store
        .conversation_event("c", "new", "thread", Some("prior"), "usage", &payload)
        .is_err());
    let dispatch = |generation, thread, turn, kind, history: &[String]| -> Result<()> {
        if !store.conversation_restored_usage("c", generation, thread, Some(turn), kind, history)? {
            store.conversation_event("c", generation, thread, Some(turn), kind, &payload)?;
        }
        Ok(())
    };
    dispatch("new", "thread", "prior", "usage", &validated).unwrap();
    dispatch("new", "thread", "prior", "usage", &validated).unwrap();
    for (generation, thread, turn, kind) in [
        ("old", "thread", "prior", "usage"),
        ("new", "wrong-thread", "prior", "usage"),
        ("new", "thread", "unknown", "usage"),
        ("new", "thread", "prior", "agent_message"),
        ("new", "thread", "prior", "tool_activity"),
        ("new", "thread", "prior", "turn_completed"),
    ] {
        assert!(dispatch(generation, thread, turn, kind, &validated).is_err());
    }
    assert!(dispatch("new", "thread", "prior", "usage", &[]).is_err());
    let unchanged: bool = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT state='idle' AND active_turn_id IS NULL AND binding_valid=1
             AND NOT EXISTS(SELECT 1 FROM conversation_events WHERE conversation_id='c')
             AND NOT EXISTS(SELECT 1 FROM conversation_operations WHERE state!='completed')
             FROM conversations WHERE id='c'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        unchanged,
        "restored usage must not create usage, a turn, or work"
    );
    let request = serde_json::json!({"input":"next"});
    let (_, fresh) = store
        .conversation_reserve_input("next", &request, "c", "next", None)
        .unwrap();
    assert!(fresh);
    let (_, input, kind, turn) = store.conversation_send_intent("c", "new", "next").unwrap();
    assert_eq!(
        (input.as_str(), kind.as_str(), turn),
        ("next", "submit", None)
    );
    store
        .conversation_response(
            "c",
            "new",
            "next",
            &serde_json::json!({"turn":{"id":"live"}}),
            false,
        )
        .unwrap();
    dispatch("new", "thread", "live", "usage", &validated).unwrap();
    let recorded: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM conversation_events WHERE kind='usage' AND generation='new'
             AND native_turn_id='live'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        recorded, 1,
        "live usage still uses the normal event projection"
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn committed_role_report_replays_receipt_without_duplicate_result_or_audit() {
    let root = std::env::temp_dir().join(format!("llmrelay-report-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("state.sqlite3");
    let store = Store::open(&database).unwrap();
    store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-report-fixture','report-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','manager','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
               VALUES('settings','t','manager',1,'{}','g','2026-01-01T00:00:00Z');
             INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
               VALUES('credential','g','fixture-hash','[\"report_result\"]','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('s','g','codex','running','{}','fixture','epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
    let context = RoleContext {
        project_id: "p".into(),
        task_id: "t".into(),
        attempt_id: "a".into(),
        role_generation_id: "g".into(),
        session_id: "s".into(),
        credential_id: "credential".into(),
        transcript_epoch: "epoch".into(),
        role: RoleKind::Manager,
        provider: Provider::Codex,
        configuration_revision: 1,
        lane_id: "default".into(),
        permissions: vec!["report_result".into()],
    };
    let report = RoleResultReport {
        operation_id: uuid::Uuid::new_v4().to_string(),
        outcome: "blocked".into(),
        summary: "fixture report".into(),
        evidence: vec![],
        metadata: serde_json::json!({}),
    };
    TEST_INTERRUPT_ROLE_REPORT_AFTER_COMMIT.with(|armed| armed.set(true));
    let error = store.save_role_result(&context, &report).unwrap_err();
    assert!(error.to_string().contains("injected interruption"));
    drop(store);

    let reopened = Store::open_current_writable(&database).unwrap();
    let committed_json: String = reopened.lock().unwrap().query_row(
            "SELECT result_json FROM operation_receipts WHERE operation_id=?1 AND operation_kind='role_report'",
            params![report.operation_id], |row| row.get(0),
        ).unwrap();
    let receipt = reopened.save_role_result(&context, &report).unwrap();
    assert_eq!(
        receipt,
        serde_json::from_str::<serde_json::Value>(&committed_json).unwrap()
    );
    assert_eq!(receipt["accepted"], true);
    let changed = RoleResultReport {
        summary: "changed payload".into(),
        ..report.clone()
    };
    assert!(reopened
        .save_role_result(&context, &changed)
        .unwrap_err()
        .to_string()
        .contains("operation ID was already used with different input"));
    let connection = reopened.lock().unwrap();
    for (table, expected) in [
        ("role_results", 1),
        ("operation_receipts", 1),
        ("audit_events", 1),
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE operation_id=?1"),
                params![report.operation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, expected, "{table}");
    }
    drop(connection);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

/// A stop selected for one retained turn is claimed only if that turn
/// still owns the session, so it can never reach a newer resumed turn.
#[test]
fn single_agent_stop_receipt_never_reaches_a_newer_turn() {
    let root = std::env::temp_dir().join(format!(
        "llmrelay-single-agent-stop-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("state.sqlite3")).unwrap();
    store.lock().unwrap().execute_batch(r#"
            INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
              VALUES('p','p','/tmp/llmrelay-single-agent-stop','single-agent-stop','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
              VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
              VALUES('a','t','context','agent_execution','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO workflow_run_bindings(attempt_id,family,definition_state,evaluator_revision,selection_hash,scope_hash,created_at)
              VALUES('a','single_agent','unavailable',1,'selection','scope','2026-01-01T00:00:00Z');
            INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
              VALUES('config','p',1,'{}','config-hash','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO agent_profiles(id,project_id,display_name,created_at,updated_at)
              VALUES('profile','p','Writer','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO agent_profile_revisions(id,profile_id,revision,provider,adapter_id,adapter_hash,model,effort,
                instructions_text,instructions_hash,policy_id,policy_revision,policy_hash,authority,session_contract,profile_hash,created_at)
              VALUES('revision','profile',1,'codex','adapter','adapter-hash','model','medium','','instructions','policy',1,
                'policy-hash','workspace_write','retained','profile-hash','2026-01-01T00:00:00Z');
            INSERT INTO profile_admissions(id,project_id,profile_revision_id,execution_config_revision_id,policy_hash,adapter_hash,
                session_contract,prepared_scope_hash,state,created_at,updated_at)
              VALUES('admission','p','revision','config','policy-hash','adapter-hash','retained','scope','pending',
                '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO workflow_run_profiles(attempt_id,slot_key,profile_revision_id,admission_id,policy_hash,session_contract,profile_hash,created_at)
              VALUES('a','single_agent','revision','admission','policy-hash','retained','profile-hash','2026-01-01T00:00:00Z');
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
              VALUES('g','a','single_agent','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO execution_bindings(role_generation_id,attempt_id,slot_key,actor,policy_hash,session_contract,channel_operations_json,created_at)
              VALUES('g','a','single_agent','single_agent','policy-hash','retained','[]','2026-01-01T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,
                process_identity_json,created_at,updated_at)
              VALUES('s','g','codex','running','{}','fixture','first-turn',
                '{"pid":4242,"process_group_id":4242,"native_start_marker":"first","observed_started_at":"2026-01-01T00:00:00Z"}',
                '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at)
              VALUES('question-1','question-1','s','g','needs_input','Which file?','[]','{}','2026-01-01T00:00:01Z');"#,
        ).unwrap();
    let eligible = || eligible_single_agent_stop(&store.lock().unwrap(), "a").unwrap();
    let session =
        || -> (String, String, Option<String>) {
            store.lock().unwrap().query_row(
                "SELECT status,transcript_epoch,interrupt_requested_at FROM sessions WHERE id='s'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap()
        };
    let reservations = || -> i64 {
        store.lock().unwrap().query_row(
                "SELECT COUNT(*) FROM audit_events WHERE event_code='single_agent.report.stop_reserved'",
                [],
                |row| row.get(0),
            ).unwrap()
    };
    let obsolete = eligible().expect("the reporting turn is eligible to stop");
    assert_eq!(obsolete.transcript_epoch, "first-turn");
    assert_eq!(obsolete.role_result_id, "question-1");

    // As reserve_single_agent_resume does before the claim: the answer
    // consumes the question and a newer turn of the same session starts
    // in a new process, then asks again.
    store.lock().unwrap().execute_batch(r#"
            UPDATE role_results SET consumed_at='2026-01-01T00:00:02Z' WHERE id='question-1';
            UPDATE sessions SET transcript_epoch='second-turn',resume_count=1,
                process_identity_json='{"pid":4343,"process_group_id":4343,"native_start_marker":"second","observed_started_at":"2026-01-01T00:00:02Z"}'
              WHERE id='s';
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,capability_key,
                capability_identity_json,state,created_at,updated_at)
              VALUES('resume','s',1,'second-turn','{}','capability','{}','running','2026-01-01T00:00:02Z','2026-01-01T00:00:02Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at)
              VALUES('question-2','question-2','s','g','needs_input','Which file now?','[]','{}','2026-01-01T00:00:03Z');"#,
        ).unwrap();
    assert!(!store.reserve_single_agent_stop(&obsolete).unwrap());
    assert_eq!(session(), ("running".into(), "second-turn".into(), None));
    assert_eq!(reservations(), 0);

    let current = eligible().expect("the newer turn's question is eligible to stop");
    assert_eq!(current.role_result_id, "question-2");
    assert_ne!(current.process, obsolete.process);
    assert!(store.reserve_single_agent_stop(&current).unwrap());
    assert!(!store.reserve_single_agent_stop(&current).unwrap());
    assert_eq!(session().0, "interrupt_requested");
    assert_eq!(reservations(), 1);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

fn schema_48_schedule_fixture() -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    let scope = serde_json::json!({"schema":1,"project_id":"p"});
    migrate(&mut connection).unwrap();
    connection
        .execute_batch(&prior_service_schema_sql(48))
        .unwrap();
    connection.execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/synthetic/repository','repository','base','now','now');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','now','now');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','agent_execution','base',1,'running','now','now'),
                     ('child','t','context','agent_execution','base',1,'running','now','now');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','implementer','codex',1,1,'running','authority','now','now'),
                     ('child-g','child','implementer','codex',1,1,'running','authority','now','now');
             INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
               VALUES('config','p',1,'{}','config','now','now');
             INSERT INTO workflow_definitions(id,family,version,content_hash,definition_text,evaluator_id,evaluator_revision,result_schema_revision,check_requirement,provenance,created_at)
               VALUES('definition','single_agent','fixture','definition','{}','single_agent',1,1,'none','fixture','now');
             INSERT INTO agent_profiles(id,project_id,display_name,created_at,updated_at)
               VALUES('profile','p','Reader','now','now');
             INSERT INTO agent_profile_revisions(id,profile_id,revision,provider,adapter_id,adapter_hash,model,effort,instructions_text,instructions_hash,policy_id,policy_revision,policy_hash,authority,session_contract,profile_hash,created_at,purpose)
               VALUES('profile-revision','profile',1,'codex','fixture','adapter','fixture','medium','','instructions','reader',1,'policy','read_only','retained','profile','now','repository_reader');
             INSERT INTO task_recipes(id,project_id,name,current_revision,version,created_at,updated_at)
               VALUES('recipe','p','Research',1,1,'now','now');
             INSERT INTO task_recipe_revisions(id,recipe_id,revision,title,description,acceptance_criteria_json,priority,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash)
               VALUES('revision','recipe',1,'Research','Inspect','[]',0,'[]','config','fixture','definition','now','single_agent','repository_research','definition','config','profile-revision','profile');
             INSERT INTO recipe_schedules(id,project_id,name,recipe_revision_id,cadence,anchor_utc,paused,version,created_at,updated_at)
               VALUES('schedule','p','Research','revision','daily','2030-01-01T00:00:00Z',1,1,'now','now'),
                     ('draft','p','Draft','revision','daily','2030-01-01T00:00:00Z',1,1,'now','now');"
        ).unwrap();
    connection.execute(
            "INSERT INTO recipe_schedule_authorizations(id,project_id,schedule_id,recipe_revision_id,scope_json,scope_hash,max_occurrences,max_invocations_per_occurrence,max_runtime_seconds,start_window_seconds,created_by,created_at)
               VALUES('grant','p','schedule','revision',?1,?2,4,4,120,60,'human','now')",
            params![scope.to_string(), json_hash(&scope).unwrap()],
        ).unwrap();
    connection.execute_batch(
            "UPDATE recipe_schedules SET mode='run_authorized_research',active_authorization_id='grant' WHERE id='schedule';
             INSERT INTO task_recipe_bindings(task_id,project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash)
               VALUES('t','p','revision','[]','config','fixture','definition','schedule','2030-01-01T00:00:00Z','now','single_agent','repository_research','definition','config','profile-revision','profile');
             INSERT INTO recipe_schedule_fires(id,schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,authorization_id,attempt_id,allowance_reserved,invocations_spent,first_reserved_at,runtime_deadline_utc,created_at)
               VALUES('occurrence','schedule','2030-01-01T00:00:00Z','revision','running','t','grant','a',1,1,'2030-01-01T00:00:00Z','2030-01-01T00:02:00Z','now');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at,schedule_occurrence_id,schedule_authorization_id)
               VALUES('s','g','codex','running','{}','fixture','epoch','now','now','occurrence','grant');
             UPDATE recipe_schedule_fires SET outcome='stop_requested',stop_requested_at='2030-01-01T00:00:30Z',stop_reason='human',stop_session_id='s',stop_transcript_epoch='epoch',stop_process_identity_json=json_object('history','original') WHERE id='occurrence';
             INSERT INTO recipe_schedule_fires(id,schedule_id,scheduled_for_utc,recipe_revision_id,outcome,created_at)
               VALUES('draft-occurrence','draft','2030-01-01T00:00:00Z','revision','missed','now');"
        ).unwrap();
    connection
}

#[test]
fn schema_49_preserves_research_drafts_and_historical_stop_identity() {
    let mut connection = schema_48_schedule_fixture();
    let history = "SELECT json_array(
            (SELECT json_array(mode,active_authorization_id,version) FROM recipe_schedules WHERE id='schedule'),
            (SELECT json_array(mode,active_authorization_id,version) FROM recipe_schedules WHERE id='draft'),
            (SELECT json_array(scope_json,scope_hash,max_occurrences,max_invocations_per_occurrence,max_runtime_seconds,start_window_seconds,created_by,created_at) FROM recipe_schedule_authorizations WHERE id='grant'),
            (SELECT json_array(id,attempt_id,invocations_spent,first_reserved_at,runtime_deadline_utc,stop_requested_at,stop_reason,stop_session_id,stop_resume_invocation_id,stop_transcript_epoch,stop_process_identity_json) FROM recipe_schedule_fires WHERE id='occurrence'),
            (SELECT json_array(id,outcome,task_id) FROM recipe_schedule_fires WHERE id='draft-occurrence'),
            (SELECT json_array(id,schedule_occurrence_id,schedule_authorization_id) FROM sessions WHERE id='s'),
            (SELECT revision FROM state_revision WHERE singleton=1))";
    let before: String = connection.query_row(history, [], |row| row.get(0)).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .unwrap();
    connection.execute_batch(MIGRATION_049).unwrap();
    connection.pragma_update(None, "user_version", 49).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let after: String = connection.query_row(history, [], |row| row.get(0)).unwrap();
    assert_eq!(after, before);
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        49
    );
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    for sql in [
            "UPDATE recipe_schedule_authorizations SET scope_json='{\"schema\":2}' WHERE id='grant'",
            "DELETE FROM recipe_schedule_authorizations WHERE id='grant'",
            "UPDATE recipe_schedule_fires SET stop_transcript_epoch='replacement' WHERE id='occurrence'",
            "UPDATE recipe_schedule_fires SET attempt_id='child' WHERE id='occurrence'",
            "UPDATE recipe_schedule_fires SET invocations_spent=0 WHERE id='occurrence'",
        ] {
            let error = connection.execute(sql, []).unwrap_err().to_string();
            assert!(
                error.contains("immutable")
                    || error.contains("cannot be rewritten")
                    || error.contains("stop target does not match its occurrence"),
                "{error}"
            );
        }
    let error = connection
        .execute(
            "UPDATE recipe_schedules SET mode='run_authorized_recipe' WHERE id='schedule'",
            [],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("own active authorization"), "{error}");
    // Seeded schema-49 history exists before exercising the ordinary upgrade seam.
    connection.execute_batch(
            "INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('completed-task','p','Completed historical task','done','now','now');
             INSERT INTO task_recipe_bindings(task_id,project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash)
               SELECT 'completed-task',project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,'2029-12-31T00:00:00Z',created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash FROM task_recipe_bindings WHERE task_id='t';
             INSERT INTO recipe_schedule_fires(id,schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,authorization_id,allowance_reserved,created_at)
               VALUES('completed-occurrence','schedule','2029-12-31T00:00:00Z','revision','finished','completed-task','grant',1,'now');
             INSERT INTO permission_requests(id,hook_invocation_nonce,connection_nonce,provider,project_id,task_id,attempt_id,session_id,role_generation_id,role,service_boot_id,native_session_id,cwd,policy_fingerprint,tool_name,input_digest,input_json,created_at,deadline_at,state,updated_at)
               VALUES('pending-permission','hook','connection','codex','p','t','a','s','g','single_agent','boot','native','/synthetic/repository','policy','Bash','digest','{}','now','2999-01-01T00:00:00Z','pending','now');"
        ).unwrap();
    let reader_scope = serde_json::json!({"schema":1,"project_id":"p"});
    connection.execute(
            "INSERT INTO recipe_schedule_authorizations(id,project_id,schedule_id,recipe_revision_id,scope_json,scope_hash,max_occurrences,max_invocations_per_occurrence,max_runtime_seconds,start_window_seconds,created_by,created_at,revoked_at,revoked_by,revocation_reason)
               VALUES('already-revoked','p','schedule','revision',?1,?2,4,4,120,60,'human','earlier','earlier','original-human','original reason')",
            params![reader_scope.to_string(), json_hash(&reader_scope).unwrap()],
        ).unwrap();
    connection.execute_batch(
            "INSERT INTO agent_profiles(id,project_id,display_name,created_at,updated_at) VALUES('writer-profile','p','Writer','now','now');
             INSERT INTO agent_profile_revisions(id,profile_id,revision,provider,adapter_id,adapter_hash,model,effort,instructions_text,instructions_hash,policy_id,policy_revision,policy_hash,authority,session_contract,profile_hash,created_at,purpose)
               SELECT 'writer-profile-revision','writer-profile',1,provider,adapter_id,adapter_hash,model,effort,instructions_text,instructions_hash,'writer',1,'writer-policy','workspace_write','retained','writer-profile','now','workspace_writer' FROM agent_profile_revisions WHERE id='profile-revision';
             INSERT INTO task_recipes(id,project_id,name,current_revision,version,created_at,updated_at) VALUES('writer-recipe','p','Writer',1,1,'now','now');
             INSERT INTO task_recipe_revisions(id,recipe_id,revision,title,description,acceptance_criteria_json,priority,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash)
               VALUES('writer-revision','writer-recipe',1,'Writer','Write','[]',0,'[]','config','fixture','definition','now','single_agent','single_agent','definition','config','writer-profile-revision','writer-profile');
             INSERT INTO recipe_schedules(id,project_id,name,recipe_revision_id,cadence,anchor_utc,paused,version,created_at,updated_at) VALUES('writer-schedule','p','Automatic writer','writer-revision','daily','2030-01-01T00:00:00Z',0,1,'now','now');"
        ).unwrap();
    let writer_scope = serde_json::json!({"schema":2,"project_id":"p","preset":"single_agent"});
    connection.execute(
            "INSERT INTO recipe_schedule_authorizations(id,project_id,schedule_id,recipe_revision_id,scope_json,scope_hash,max_occurrences,max_invocations_per_occurrence,max_runtime_seconds,start_window_seconds,created_by,created_at)
               VALUES('writer-grant','p','writer-schedule','writer-revision',?1,?2,1,1,120,60,'human','now')",
            params![writer_scope.to_string(), json_hash(&writer_scope).unwrap()],
        ).unwrap();
    connection.execute_batch(
            "UPDATE recipe_schedules SET mode='run_authorized_recipe',active_authorization_id='writer-grant',next_fire_utc='2030-01-01T00:00:00Z' WHERE id='writer-schedule';
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at) VALUES('writer-task','p','Automatic plain writer','in_progress','now','now');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at) VALUES('writer-attempt','writer-task','context','agent_execution','base',1,'running','now','now');
             INSERT INTO task_recipe_bindings(task_id,project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash)
               VALUES('writer-task','p','writer-revision','[]','config','fixture','definition','writer-schedule','2030-01-01T00:00:00Z','now','single_agent','single_agent','definition','config','writer-profile-revision','writer-profile');
             INSERT INTO recipe_schedule_fires(id,schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,authorization_id,attempt_id,allowance_reserved,invocations_spent,first_reserved_at,runtime_deadline_utc,created_at)
               VALUES('writer-occurrence','writer-schedule','2030-01-01T00:00:00Z','writer-revision','reserved','writer-task','writer-grant','writer-attempt',1,1,'2030-01-01T00:00:00Z','2030-01-01T00:02:00Z','now');"
        ).unwrap();
    let completed_before: String = connection.query_row("SELECT json_array(outcome,task_id,authorization_id,allowance_reserved,invocations_spent,created_at) FROM recipe_schedule_fires WHERE id='completed-occurrence'",[],|row|row.get(0)).unwrap();
    let old_revocation: String = connection.query_row("SELECT json_array(revoked_at,revoked_by,revocation_reason) FROM recipe_schedule_authorizations WHERE id='already-revoked'",[],|row|row.get(0)).unwrap();
    let draft_before: String = connection.query_row(
            "SELECT json_array(mode,active_authorization_id,paused,next_fire_utc,version,updated_at) FROM recipe_schedules WHERE id='draft'", [], |row| row.get(0),
        ).unwrap();
    let scope_before: String = connection
        .query_row(
            "SELECT scope_json FROM recipe_schedule_authorizations WHERE id='grant'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    migrate(&mut connection).unwrap();
    let retired: bool = connection.query_row(
            "SELECT mode='run_authorized_research' AND active_authorization_id='grant' AND paused=1 AND next_fire_utc IS NULL AND version=2 FROM recipe_schedules WHERE id='schedule'", [], |row| row.get(0),
        ).unwrap();
    assert!(retired);
    assert!(connection.query_row("SELECT mode='run_authorized_recipe' AND active_authorization_id='writer-grant' AND paused=1 AND next_fire_utc IS NULL FROM recipe_schedules WHERE id='writer-schedule'",[],|row|row.get::<_,bool>(0)).unwrap());
    assert_eq!(
            connection
                .query_row(
                    "SELECT outcome||':'||attempt_id||':'||invocations_spent||':'||first_reserved_at||':'||runtime_deadline_utc FROM recipe_schedule_fires WHERE id='writer-occurrence'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "recovery_required:writer-attempt:1:2030-01-01T00:00:00Z:2030-01-01T00:02:00Z"
        );
    assert!(
        crate::workflow::require_supported_attempt_execution(&connection, "writer-attempt")
            .unwrap_err()
            .to_string()
            .contains("retired")
    );
    let draft_after: String = connection.query_row(
            "SELECT json_array(mode,active_authorization_id,paused,next_fire_utc,version,updated_at) FROM recipe_schedules WHERE id='draft'", [], |row| row.get(0),
        ).unwrap();
    assert_eq!(draft_after, draft_before);
    let (scope_after, actor, reason, revoked): (String, String, String, String) = connection.query_row(
            "SELECT scope_json,revoked_by,revocation_reason,revoked_at FROM recipe_schedule_authorizations WHERE id='grant'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
    assert_eq!(connection.query_row("SELECT json_array(outcome,task_id,authorization_id,allowance_reserved,invocations_spent,created_at) FROM recipe_schedule_fires WHERE id='completed-occurrence'",[],|row|row.get::<_,String>(0)).unwrap(),completed_before);
    assert_eq!(connection.query_row("SELECT json_array(revoked_at,revoked_by,revocation_reason) FROM recipe_schedule_authorizations WHERE id='already-revoked'",[],|row|row.get::<_,String>(0)).unwrap(),old_revocation);
    assert_eq!(connection.query_row("SELECT state||':'||delivery_state FROM permission_requests WHERE id='pending-permission'",[],|row|row.get::<_,String>(0)).unwrap(),"expired:deny_required");
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM audit_events WHERE entity_id='pending-permission' AND event_code='permission.response.deny_required'",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(scope_after, scope_before);
    assert_eq!(actor, "execution_retirement");
    assert!(reason.contains("retired"));
    assert!(!revoked.is_empty());
    let outcome: String = connection
        .query_row(
            "SELECT outcome FROM recipe_schedule_fires WHERE id='occurrence'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "recovery_required");
    assert!(
        crate::workflow::require_supported_attempt_execution(&connection, "a")
            .unwrap_err()
            .to_string()
            .contains("retired")
    );
    let first_revocation: String = connection.query_row("SELECT json_array(revoked_at,revoked_by,revocation_reason) FROM recipe_schedule_authorizations WHERE id='grant'", [], |row| row.get(0)).unwrap();
    connection.execute_batch(MIGRATION_050).unwrap();
    let replay_revocation: String = connection.query_row("SELECT json_array(revoked_at,revoked_by,revocation_reason) FROM recipe_schedule_authorizations WHERE id='grant'", [], |row| row.get(0)).unwrap();
    assert_eq!(first_revocation, replay_revocation);
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        50
    );
    let paths = restorepoint_paths();
    connection
        .execute("VACUUM INTO ?1", [paths.database.to_str().unwrap()])
        .unwrap();
    let backup = database::backup(&paths, None).unwrap();
    let backup_path = std::path::PathBuf::from(backup["backup"].as_str().unwrap());
    let restored = restorepoint_paths();
    database::restore(&restored, &backup_path).unwrap();
    let historical = Store::open_maintenance_readonly(&restored.database).unwrap();
    assert!(historical.restore_hold().unwrap().is_some());
    assert!(historical.require_execution_unheld("dispatch").is_err());
    let held = historical.lock().unwrap();
    assert!(held.query_row("SELECT mode='run_authorized_research' AND active_authorization_id='grant' AND paused=1 AND next_fire_utc IS NULL FROM recipe_schedules WHERE id='schedule'",[],|row|row.get::<_,bool>(0)).unwrap());
    assert!(
        crate::workflow::require_supported_attempt_execution(&held, "a")
            .unwrap_err()
            .to_string()
            .contains("retired")
    );
    assert_eq!(held.query_row("SELECT json_array(outcome,task_id,authorization_id,allowance_reserved,invocations_spent,created_at) FROM recipe_schedule_fires WHERE id='completed-occurrence'",[],|row|row.get::<_,String>(0)).unwrap(),completed_before);
    assert_eq!(held.query_row("SELECT json_array(revoked_at,revoked_by,revocation_reason) FROM recipe_schedule_authorizations WHERE id='already-revoked'",[],|row|row.get::<_,String>(0)).unwrap(),old_revocation);
    drop(held);
    drop(historical);
    std::fs::remove_dir_all(paths.root).unwrap();
    std::fs::remove_dir_all(restored.root).unwrap();
}

#[test]
fn schema_49_migration_rolls_back_and_restores_foreign_keys_on_invalid_history() {
    let mut connection = schema_48_schedule_fixture();
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             DELETE FROM agent_profiles WHERE id='profile';
             PRAGMA foreign_keys=ON;",
        )
        .unwrap();
    let error = migrate(&mut connection).unwrap_err().to_string();
    assert!(error.contains("foreign-key violations"), "{error}");
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        48
    );
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='recipe_schedule_stop_targets'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT invocations_spent FROM recipe_schedule_fires WHERE id='occurrence'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn schema_49_stop_intent_needs_no_native_reservation_or_root() {
    let mut connection = schema_48_schedule_fixture();
    migrate(&mut connection).unwrap();
    connection.execute_batch(
            "UPDATE recipe_schedule_fires SET outcome='finished' WHERE id='occurrence';
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('waiting-task','p','Waiting','draft','now','now');
             INSERT INTO task_recipe_bindings(task_id,project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash)
               SELECT 'waiting-task',project_id,recipe_revision_id,required_check_ids_json,configuration_hash,workflow_version,workflow_hash,schedule_id,'2030-01-02T00:00:00Z',created_at,family,preset,definition_id,execution_config_revision_id,primary_profile_revision_id,primary_profile_hash
               FROM task_recipe_bindings WHERE task_id='t';
             INSERT INTO recipe_schedule_fires(id,schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,authorization_id,allowance_reserved,created_at)
               VALUES('unspent','schedule','2030-01-02T00:00:00Z','revision','waiting','waiting-task','grant',1,'now');
             UPDATE recipe_schedule_fires SET outcome='stop_requested',stop_requested_at='2030-01-02T00:00:00Z',stop_reason='human' WHERE id='unspent';"
        ).unwrap();
    // materialize_authorized_in commits the task and reserved occurrence together.
    let error = connection.execute(
            "INSERT INTO recipe_schedule_fires(id,schedule_id,scheduled_for_utc,recipe_revision_id,outcome,authorization_id,allowance_reserved,created_at)
               VALUES('invalid-finished','schedule','2030-01-03T00:00:00Z','revision','finished','grant',1,'now')",
            [],
        ).unwrap_err().to_string();
    assert!(error.contains("CHECK constraint failed"), "{error}");
    assert!(error.contains("task_id IS NOT NULL"), "{error}");
    let unspent: (i64, Option<String>, Option<String>, Option<String>) = connection.query_row(
            "SELECT invocations_spent,attempt_id,first_reserved_at,runtime_deadline_utc FROM recipe_schedule_fires WHERE id='unspent'", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
    assert_eq!(unspent, (0, None, None, None));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM recipe_schedule_stop_targets",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let error = connection.execute("UPDATE recipe_schedule_fires SET stop_requested_at=NULL,stop_reason=NULL,outcome='held' WHERE id='unspent'", []).unwrap_err().to_string();
    assert!(error.contains("cannot be rewritten"), "{error}");
    // Readiness/spawn fencing is a later runtime obligation; storage preserves the intent.
    connection
        .execute(
            "UPDATE recipe_schedule_fires SET outcome='finished' WHERE id='unspent'",
            [],
        )
        .unwrap();
    let settled: String = connection.query_row(
            "SELECT json_array(outcome,task_id,authorization_id,allowance_reserved,invocations_spent,attempt_id,first_reserved_at,runtime_deadline_utc,stop_requested_at,stop_reason)
             FROM recipe_schedule_fires WHERE id='unspent'",
            [],
            |row| row.get(0),
        ).unwrap();
    assert_eq!(
        settled,
        r#"["finished","waiting-task","grant",1,0,null,null,null,"2030-01-02T00:00:00Z","human"]"#
    );
}

#[test]
fn schema_49_stop_targets_reject_rebinding_duplicates_and_unrelated_attempts() {
    let mut connection = schema_48_schedule_fixture();
    migrate(&mut connection).unwrap();
    let insert = "INSERT INTO recipe_schedule_stop_targets(id,occurrence_id,authorization_id,attempt_id,target_kind,session_id,role_generation_id,transcript_epoch,created_at,observed_at)
            VALUES(?1,'occurrence','grant',?2,'session','s',?3,?4,'now','now')";
    for (id, attempt, generation, epoch) in [
        ("child-target", "child", "child-g", "epoch"),
        ("stale-epoch", "a", "g", "stale"),
    ] {
        let error = connection
            .execute(insert, params![id, attempt, generation, epoch])
            .unwrap_err()
            .to_string();
        assert!(error.contains("exact occurrence invocation"), "{error}");
    }
    let before = read_state_revision(&connection).unwrap();
    connection
        .execute(insert, params!["target", "a", "g", "epoch"])
        .unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), before + 1);
    let error = connection
        .execute(insert, params!["duplicate", "a", "g", "epoch"])
        .unwrap_err()
        .to_string();
    assert!(error.contains("UNIQUE constraint failed"), "{error}");
    for sql in [
        "UPDATE recipe_schedule_stop_targets SET transcript_epoch='replacement' WHERE id='target'",
        "UPDATE recipe_schedule_stop_targets SET process_identity_json='{}' WHERE id='target'",
        "DELETE FROM recipe_schedule_stop_targets WHERE id='target'",
    ] {
        let error = connection.execute(sql, []).unwrap_err().to_string();
        assert!(error.contains("immutable"), "{error}");
    }
    connection.execute("UPDATE sessions SET launch_state='spawning',process_identity_json='{\"pid\":42}' WHERE id='s'", []).unwrap();
    connection
        .execute(
            "UPDATE sessions SET transcript_epoch='later' WHERE id='s'",
            [],
        )
        .unwrap();
    let bind = "UPDATE recipe_schedule_stop_targets SET process_identity_json='{\"pid\":42}',observed_at='published' WHERE id='target'";
    assert!(connection
        .execute(bind, [])
        .unwrap_err()
        .to_string()
        .contains("immutable"));
    connection
        .execute(
            "UPDATE sessions SET transcript_epoch='epoch' WHERE id='s'",
            [],
        )
        .unwrap();
    connection.execute(bind, []).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT process_identity_json FROM recipe_schedule_stop_targets WHERE id='target'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "{\"pid\":42}"
    );
    for replacement in ["NULL", "'{\"pid\":43}'"] {
        let error = connection.execute(&format!("UPDATE recipe_schedule_stop_targets SET process_identity_json={replacement} WHERE id='target'"), []).unwrap_err();
        assert!(error.to_string().contains("immutable"));
    }
    let before_status = read_state_revision(&connection).unwrap();
    connection
        .execute(
            "UPDATE recipe_schedule_stop_targets SET status='quiescent' WHERE id='target'",
            [],
        )
        .unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), before_status + 1);
    connection
        .execute(
            "UPDATE recipe_schedule_stop_targets SET status='quiescent' WHERE id='target'",
            [],
        )
        .unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), before_status + 1);
    connection.execute_batch(
            "INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,capability_key,capability_identity_json,state,created_at,updated_at,schedule_occurrence_id,schedule_authorization_id)
               VALUES('resume','s',1,'resume-epoch','{}','capability','{}','running','now','now','occurrence','grant');
             INSERT INTO check_runs(id,attempt_id,candidate_hash,executable,arguments_json,cwd,status,created_at)
               VALUES('check','a','candidate','true','[]','/synthetic/repository','launch_reserved','now');"
        ).unwrap();
    let resume_target = "INSERT INTO recipe_schedule_stop_targets(id,occurrence_id,authorization_id,attempt_id,target_kind,session_id,role_generation_id,resume_invocation_id,transcript_epoch,created_at,observed_at)
            VALUES(?1,'occurrence','grant','a','resume','s','g','resume',?2,'now','now')";
    let error = connection
        .execute(resume_target, params!["stale-resume", "epoch"])
        .unwrap_err()
        .to_string();
    assert!(error.contains("exact occurrence invocation"), "{error}");
    connection
        .execute(resume_target, params!["resume-target", "resume-epoch"])
        .unwrap();
    let check_target = "INSERT INTO recipe_schedule_stop_targets(id,occurrence_id,authorization_id,attempt_id,target_kind,check_run_id,process_identity_json,created_at,observed_at)
            VALUES(?1,'occurrence','grant','a','check','check',?2,'now','now')";
    let error = connection
        .execute(check_target, params!["wrong-process", "{}"])
        .unwrap_err()
        .to_string();
    assert!(error.contains("exact occurrence invocation"), "{error}");
    connection
        .execute(
            check_target,
            params!["check-target", Option::<String>::None],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE check_runs SET recovery_anchor_json='{\"pid\":51}' WHERE id='check'",
            [],
        )
        .unwrap();
    let check_bind = "UPDATE recipe_schedule_stop_targets SET process_identity_json='{\"pid\":51}',observed_at='published' WHERE id='check-target'";
    assert!(connection.execute("UPDATE recipe_schedule_stop_targets SET process_identity_json='{\"pid\":52}',observed_at='published' WHERE id='check-target'", []).unwrap_err().to_string().contains("immutable"));
    connection.execute("UPDATE recipe_schedule_stop_targets SET restore_invalidated_at='restored' WHERE id='check-target'", []).unwrap();
    assert!(connection
        .execute(check_bind, [])
        .unwrap_err()
        .to_string()
        .contains("immutable"));
    connection
        .execute(
            "UPDATE resume_invocations SET process_identity_json='{\"pid\":61}' WHERE id='resume'",
            [],
        )
        .unwrap();
    assert!(connection.execute("UPDATE recipe_schedule_stop_targets SET process_identity_json='{\"pid\":61}',observed_at='published' WHERE id='resume-target'", []).unwrap_err().to_string().contains("immutable"));
    connection
        .execute(
            "UPDATE sessions SET transcript_epoch='resume-epoch',resume_count=1 WHERE id='s'",
            [],
        )
        .unwrap();
    connection.execute("UPDATE recipe_schedule_authorizations SET restore_invalidated_at='restored' WHERE id='grant'", []).unwrap();
    assert!(connection.execute("UPDATE recipe_schedule_stop_targets SET process_identity_json='{\"pid\":61}',observed_at='published' WHERE id='resume-target'", []).unwrap_err().to_string().contains("immutable"));
    assert!(connection
        .execute(check_bind, [])
        .unwrap_err()
        .to_string()
        .contains("immutable"));
    let error = connection
        .execute(insert, params!["restored-target", "a", "g", "epoch"])
        .unwrap_err()
        .to_string();
    assert!(error.contains("exact occurrence invocation"), "{error}");
    let error = connection
        .execute(
            "UPDATE sessions SET role_generation_id='child-g' WHERE id='s'",
            [],
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("occurrence attempt"), "{error}");
}

pub(super) fn prior_service_schema_sql(version: i64) -> String {
    if version == 49 {
        return "PRAGMA user_version=49;".into();
    }

    let mut drop_schema_49 = String::from("PRAGMA foreign_keys=OFF;");
    for declaration in MIGRATION_049.split("CREATE TRIGGER ").skip(1) {
        let name = declaration.split_whitespace().next().unwrap();
        drop_schema_49.push_str(&format!("DROP TRIGGER IF EXISTS {name};"));
    }
    drop_schema_49.push_str("DROP TABLE recipe_schedule_stop_targets;");
    for (table, declaration) in [
            (
                "recipe_schedules",
                MIGRATION_030.split_once("CREATE TABLE recipe_schedules (").unwrap().1
                    .split_once("\n);").unwrap().0.replace(
                        "    UNIQUE(project_id, name),",
                        "    mode TEXT NOT NULL DEFAULT 'create_draft' CHECK(mode IN ('create_draft','run_authorized_research')),\n    active_authorization_id TEXT REFERENCES recipe_schedule_authorizations(id),\n    UNIQUE(project_id, name),"
                    ),
            ),
            (
                "recipe_schedule_fires",
                MIGRATION_048.split_once("CREATE TABLE recipe_schedule_fires_rebuilt (").unwrap().1
                    .split_once("\n);").unwrap().0.to_owned(),
            ),
        ] {
            // Schema 49 added no columns to either existing table.
            drop_schema_49.push_str(&format!(
                "CREATE TEMP TABLE schedule_schema_48_rows AS SELECT * FROM {table};
                 DROP TABLE {table}; CREATE TABLE {table} ({declaration}\n);
                 INSERT INTO {table} SELECT * FROM schedule_schema_48_rows;
                 DROP TABLE schedule_schema_48_rows;"
            ));
        }
    drop_schema_49.push_str(
        "CREATE UNIQUE INDEX recipe_schedules_project_identity ON recipe_schedules(project_id,id);",
    );
    let indexes = MIGRATION_048
        .split_once("CREATE UNIQUE INDEX recipe_schedule_fires_authorized_task")
        .unwrap()
        .1
        .split_once("ALTER TABLE sessions")
        .unwrap()
        .0;
    drop_schema_49.push_str(&format!(
        "CREATE UNIQUE INDEX recipe_schedule_fires_authorized_task{indexes}"
    ));
    for declaration in MIGRATION_048.split("CREATE TRIGGER ").skip(1) {
        let body = declaration.split_once("END;").unwrap().0;
        drop_schema_49.push_str(&format!("CREATE TRIGGER {body}END;"));
    }
    for event in ["insert", "delete"] {
        let name = format!("CREATE TRIGGER state_revision_recipe_schedules_{event}");
        let body = MIGRATION_030
            .split_once(&name)
            .unwrap()
            .1
            .split_once("END;")
            .unwrap()
            .0;
        drop_schema_49.push_str(&format!("{name}{body}END;"));
    }
    if version == 48 {
        return format!("{drop_schema_49} PRAGMA user_version=48; PRAGMA foreign_keys=ON;");
    }
    let mut drop_schema_48 = drop_schema_49;
    for declaration in MIGRATION_048.split("CREATE TRIGGER ").skip(1) {
        let name = declaration.split_whitespace().next().unwrap();
        drop_schema_48.push_str(&format!("DROP TRIGGER IF EXISTS {name};"));
    }
    for table in ["sessions", "resume_invocations"] {
        drop_schema_48.push_str(&format!(
            "DROP INDEX IF EXISTS {table}_schedule_occurrence;
                 ALTER TABLE {table} DROP COLUMN schedule_occurrence_id;
                 ALTER TABLE {table} DROP COLUMN schedule_authorization_id;"
        ));
    }
    for (table, columns) in [
            ("recipe_schedule_fires", "schedule_id,scheduled_for_utc,recipe_revision_id,outcome,task_id,reason,missed_first_utc,missed_last_utc,missed_count,created_at"),
            ("recipe_schedules", "id,project_id,name,recipe_revision_id,cadence,anchor_utc,next_fire_utc,paused,version,archived_at,created_at,updated_at"),
        ] {
            let declaration = MIGRATION_030.split_once(&format!("CREATE TABLE {table} (")).unwrap().1
                .split_once("\n);").unwrap().0;
            drop_schema_48.push_str(&format!(
                "CREATE TEMP TABLE schedule_schema_47_rows AS SELECT {columns} FROM {table};
                 DROP TABLE {table}; CREATE TABLE {table} ({declaration}\n);
                 INSERT INTO {table} ({columns}) SELECT {columns} FROM schedule_schema_47_rows;
                 DROP TABLE schedule_schema_47_rows;"
            ));
        }
    drop_schema_48.push_str("DROP TABLE recipe_schedule_authorizations;");
    for (source, table, events) in [
        (
            MIGRATION_030,
            "recipe_schedules",
            &["insert", "update", "delete"][..],
        ),
        (
            MIGRATION_030,
            "recipe_schedule_fires",
            &["insert", "update", "delete"][..],
        ),
        (MIGRATION_044, "sessions", &["update"][..]),
        (MIGRATION_029, "resume_invocations", &["update"][..]),
    ] {
        for event in events {
            let name = format!("CREATE TRIGGER state_revision_{table}_{event}");
            let body = source
                .split_once(&name)
                .unwrap()
                .1
                .split_once("END;")
                .unwrap()
                .0;
            drop_schema_48.push_str(&format!("{name}{body}END;"));
        }
    }
    if version == 47 {
        return format!("{drop_schema_48} PRAGMA user_version=47; PRAGMA foreign_keys=ON;");
    }
    let tasks_trigger_name = "CREATE TRIGGER state_revision_tasks_update";
    let tasks_trigger = MIGRATION_029
        .split_once(tasks_trigger_name)
        .unwrap()
        .1
        .split_once("END;")
        .unwrap()
        .0;
    let drop_schema_47 = format!(
            "{drop_schema_48} PRAGMA foreign_keys=OFF;
             DROP TRIGGER state_revision_tasks_update;
             ALTER TABLE tasks DROP COLUMN knowledge_selection_json;
             ALTER TABLE tasks DROP COLUMN knowledge_preparation_json;
             {tasks_trigger_name}{tasks_trigger}END;
             DROP TABLE knowledge_manifest_items; DROP TABLE knowledge_manifests;
             DROP TABLE knowledge_operation_receipts; DROP TABLE knowledge_observations;
             DROP TABLE knowledge_events; DROP TABLE knowledge_revisions; DROP TABLE knowledge_entries;"
        );
    if version == 46 {
        return format!("{drop_schema_47} PRAGMA user_version=46; PRAGMA foreign_keys=ON;");
    }
    let drop_schema_46 = format!(
        "{drop_schema_47} DROP TABLE IF EXISTS workflow_execution_boundary_conflicts;
            DROP TABLE IF EXISTS workflow_execution_boundaries;"
    );
    if version == 45 {
        return format!("{drop_schema_46} PRAGMA user_version=45; PRAGMA foreign_keys=ON;");
    }
    let drop_schema_45 = format!("{drop_schema_46} DROP TRIGGER task_conversation_source_task_delete; DROP TABLE task_conversation_sources;");
    let sessions_43=MIGRATION_039.split_once("CREATE TABLE sessions_rebuilt (").unwrap().1
            .split_once("-- Presentation only:").unwrap().0
            .replace("setup_permit_id,interrupt_requested_at,NULL\nFROM sessions", "setup_permit_id,interrupt_requested_at,provider_validation_invocation_id\nFROM sessions");
    let drop_schema_44=format!("PRAGMA foreign_keys=OFF; {drop_schema_45}
            DROP TRIGGER claims_conversation_admission; DROP TRIGGER claims_conversation_readmission;
            DROP TABLE conversation_events; DROP TABLE conversation_requests;
            DROP TABLE conversation_operations; DROP TABLE conversation_reservations;
            CREATE TABLE sessions_rebuilt ({sessions_43}
            DROP TABLE conversations; DROP TABLE conversation_admissions;
            DROP TABLE conversation_profiles; DROP TABLE conversation_runtime;");
    let mut drop_schema_43 = format!("{drop_schema_44}");
    drop_schema_43.push_str(
        "
            DROP TABLE single_agent_repair_receipts; DROP TABLE single_agent_review_results;
            DROP TABLE single_agent_review_deliveries; DROP TABLE single_agent_review_budgets;
            DROP TABLE single_agent_admission_challenges;",
    );
    for (table, source, columns) in [
            ("workflow_run_profiles",MIGRATION_039,"attempt_id,slot_key,profile_revision_id,admission_id,policy_hash,session_contract,profile_hash,created_at"),
            ("execution_bindings",MIGRATION_039,"role_generation_id,attempt_id,slot_key,actor,policy_hash,session_contract,channel_operations_json,created_at"),
            ("workflow_result_bindings",MIGRATION_039,"id,attempt_id,role_result_id,role_generation_id,session_id,transcript_epoch,criteria_hash,evidence_hash,profile_binding_hash,selected_check_revision,snapshot_id,candidate_hash,state,state_reason,created_at,updated_at"),
            ("task_recipe_revisions",MIGRATION_030,"id,recipe_id,revision,title,description,acceptance_criteria_json,priority,profile_revision_id,required_check_ids_json,config_revision_id,configuration_hash,workflow_version,workflow_hash,created_at"),
            ("task_recipe_bindings",MIGRATION_030,"task_id,project_id,recipe_revision_id,profile_revision_id,required_check_ids_json,config_revision_id,configuration_hash,workflow_version,workflow_hash,schedule_id,scheduled_for_utc,created_at"),
        ] {
            let declaration = source.split_once(&format!("CREATE TABLE {table} (")).unwrap().1.split_once("\n);").unwrap().0;
            drop_schema_43.push_str(&format!("CREATE TABLE {table}_schema42 ({declaration}\n); INSERT INTO {table}_schema42 SELECT {columns} FROM {table}; DROP TABLE {table}; ALTER TABLE {table}_schema42 RENAME TO {table};"));
            for event in ["insert","update","delete"] {
                let name = format!("CREATE TRIGGER state_revision_{table}_{event}");
                let body = source.split_once(&name).unwrap().1.split_once("END;").unwrap().0;
                drop_schema_43.push_str(&format!("{name}{body}END;"));
            }
        }
    for name in [
        "CREATE UNIQUE INDEX workflow_result_bindings_one_current",
        "CREATE INDEX workflow_result_bindings_attempt",
    ] {
        let body = MIGRATION_039
            .split_once(name)
            .unwrap()
            .1
            .split_once(';')
            .unwrap()
            .0;
        drop_schema_43.push_str(&format!("{name}{body};"));
    }
    for (table, columns, source) in [
        ("agent_profile_revisions", &["purpose"][..], MIGRATION_039),
        ("project_workflow_defaults", &["preset"][..], MIGRATION_039),
        (
            "task_workflow_selections",
            &["preset", "reviewer_profile_revision_id"][..],
            MIGRATION_040,
        ),
        (
            "workflow_run_bindings",
            &["research_snapshot_id"][..],
            MIGRATION_039,
        ),
    ] {
        let name = format!("state_revision_{table}_update");
        drop_schema_43.push_str(&format!("DROP TRIGGER {name};"));
        for column in columns {
            drop_schema_43.push_str(&format!("ALTER TABLE {table} DROP COLUMN {column};"));
        }
        let body = source
            .split_once(&format!("CREATE TRIGGER {name}"))
            .unwrap()
            .1
            .split_once("END;")
            .unwrap()
            .0;
        drop_schema_43.push_str(&format!("CREATE TRIGGER {name}{body}END;"));
    }
    let guidance_trigger_name = "CREATE TRIGGER state_revision_guidance_messages_update";
    let guidance_trigger = MIGRATION_029.split_once(guidance_trigger_name).unwrap().1;
    let guidance_trigger = guidance_trigger.split_once("END;").unwrap().0;
    let drop_schema_42 = format!(
        "{drop_schema_43} DROP TABLE task_comments;
             DROP TRIGGER IF EXISTS state_revision_guidance_messages_update;
             ALTER TABLE guidance_messages DROP COLUMN message_revision;
             {guidance_trigger_name}{guidance_trigger}END;"
    );
    let restore_schema_40 = format!(
            "{drop_schema_42}
             DROP TRIGGER state_revision_provider_failure_holds_insert;
             DROP TRIGGER state_revision_provider_failure_holds_update;
             DROP TRIGGER state_revision_provider_failure_holds_delete;
             DROP INDEX provider_failure_holds_attempt; DROP INDEX provider_failure_holds_session;
             ALTER TABLE provider_failure_holds RENAME TO provider_failure_holds_current;
             {MIGRATION_035}
             INSERT INTO provider_failure_holds
             SELECT id,attempt_id,role_generation_id,session_id,transcript_epoch,accepted_hook_event_id,
                    failure_hook_event_id,failure_kind,attribution,created_at,expires_at,state,
                    resolved_at,resolution_kind,resolution_ref FROM provider_failure_holds_current;
             DROP TABLE provider_failure_holds_current;"
        );
    let drop_schema_37 = "CREATE TABLE trip_legacy_migrations_previous (
            id TEXT PRIMARY KEY,attempt_id TEXT NOT NULL UNIQUE REFERENCES attempts(id),
            from_workflow_id TEXT NOT NULL,to_workflow_id TEXT NOT NULL,preserved_json TEXT NOT NULL,
            reviewed_plan_hash TEXT NOT NULL,config_revision_id TEXT NOT NULL REFERENCES trip_config_revisions(id),
            authorized_at TEXT NOT NULL);
            INSERT INTO trip_legacy_migrations_previous SELECT id,attempt_id,from_workflow_id,to_workflow_id,
                preserved_json,reviewed_plan_hash,config_revision_id,authorized_at FROM trip_legacy_migrations;
            DROP TABLE trip_legacy_migrations;
            ALTER TABLE trip_legacy_migrations_previous RENAME TO trip_legacy_migrations;";
    let drop_schema_36 = format!(
            "{drop_schema_37} DROP TABLE attention_observations; DROP INDEX role_results_session_generation;"
        );
    let drop_schema_35 = format!("{drop_schema_36} DROP TABLE provider_failure_holds;");
    let drop_schema_34 = format!(
        "{drop_schema_35} DROP TRIGGER guidance_submitted_form_paired;
             DROP TRIGGER guidance_submitted_form_once;
             ALTER TABLE guidance_messages DROP COLUMN submitted_digest;
             ALTER TABLE guidance_messages DROP COLUMN submitted_text;"
    );
    let drop_after_schema_32 = format!(
        "{drop_schema_34} DROP TABLE permission_native_resolutions;
             DROP TABLE permission_request_hooks; DROP TABLE role_result_supersessions;"
    );
    let drop_later = match version {
        31 => {
            format!("{drop_after_schema_32} DROP TABLE final_repair_rechecks; {MIGRATION_031}")
        }
        32 => drop_after_schema_32,
        33 => drop_schema_34,
        34 => drop_schema_35,
        35 => drop_schema_36,
        36 => drop_schema_37.to_owned(),
        37 => String::new(),
        38 => {
            return format!("{restore_schema_40} {RESTORE_SCHEMA_38_SQL} PRAGMA user_version=38;")
        }
        39 => {
            let trigger_name = "CREATE TRIGGER state_revision_task_workflow_selections_update";
            let trigger = MIGRATION_039.split_once(trigger_name).unwrap().1;
            let trigger = trigger.split_once("END;").unwrap().0;
            return format!(
                "{restore_schema_40} DROP TRIGGER state_revision_task_workflow_selections_update;
                     {trigger_name}{trigger}END;
                     ALTER TABLE task_workflow_selections DROP COLUMN workspace_mode;
                     PRAGMA user_version=39;"
            );
        }
        44 => return format!("{drop_schema_45} PRAGMA user_version=44; PRAGMA foreign_keys=ON;"),
        43 => return format!("{drop_schema_44} PRAGMA user_version=43;"),
        42 => return format!("{drop_schema_43} PRAGMA user_version=42;"),
        41 => return format!("{drop_schema_42} PRAGMA user_version=41;"),
        40 => return format!("{restore_schema_40} PRAGMA user_version=40;"),
        _ => panic!("unsupported fixture schema {version}"),
    };
    format!(
            "{restore_schema_40} {RESTORE_SCHEMA_38_SQL} DROP TABLE codex_usage_observations; {drop_later} PRAGMA user_version={version};"
        )
}

/// Undoes migration 039: drops its tables and rebuilds `sessions` in the
/// schema-38 shape with a required role generation. Fixtures always upgrade
/// afterwards, and 039 recreates the sessions indexes and triggers.
const RESTORE_SCHEMA_38_SQL: &str = "PRAGMA foreign_keys=OFF;
        DROP TABLE history_pane_views;
        DROP TABLE workflow_result_acceptances; DROP TABLE workflow_result_bindings;
        DROP TABLE workflow_check_authorizations; DROP TABLE workflow_check_selections;
        DROP TABLE execution_bindings; DROP TABLE workflow_run_profiles;
        DROP TABLE workflow_run_bindings; DROP TABLE task_workflow_selections;
        DROP TABLE project_workflow_defaults; DROP TABLE profile_admissions;
        DROP TABLE agent_profile_revisions; DROP TABLE agent_profiles;
        DROP TABLE execution_verification_checks; DROP TABLE execution_config_revisions;
        DROP TABLE workflow_definitions;
        CREATE TABLE sessions_schema_38 (
            id TEXT PRIMARY KEY, role_generation_id TEXT NOT NULL REFERENCES role_generations(id),
            provider TEXT NOT NULL, validation_cell TEXT, status TEXT NOT NULL,
            launch_config_json TEXT NOT NULL, executable_version TEXT NOT NULL,
            native_session_id TEXT, native_identity_source TEXT, process_identity_json TEXT,
            transcript_epoch TEXT NOT NULL, transcript_last_sequence INTEGER NOT NULL DEFAULT 0,
            capture_state TEXT NOT NULL DEFAULT 'capturing', capture_error TEXT,
            hook_trust_state TEXT NOT NULL DEFAULT 'pending_observation', exit_json TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, native_identity_verified_at TEXT,
            readiness_state TEXT NOT NULL DEFAULT 'unknown', invocation_input_json TEXT,
            workflow_version TEXT NOT NULL DEFAULT 'trip-v1', workflow_hash TEXT NOT NULL DEFAULT '',
            prompt_hash TEXT NOT NULL DEFAULT '', launch_state TEXT NOT NULL DEFAULT 'reserved',
            launch_error TEXT, resume_count INTEGER NOT NULL DEFAULT 0, capability_key TEXT,
            capability_identity_json TEXT, recovery_root_pid INTEGER, recovery_process_group_id INTEGER,
            desired_running INTEGER NOT NULL DEFAULT 0, launch_boot_identity TEXT,
            recovery_anchor_json TEXT, initial_hook_event_boundary_rowid INTEGER NOT NULL DEFAULT 0,
            lane_id TEXT NOT NULL DEFAULT 'default', setup_permit_id TEXT, interrupt_requested_at TEXT);
        INSERT INTO sessions_schema_38 SELECT id,role_generation_id,provider,validation_cell,status,
            launch_config_json,executable_version,native_session_id,native_identity_source,
            process_identity_json,transcript_epoch,transcript_last_sequence,capture_state,capture_error,
            hook_trust_state,exit_json,created_at,updated_at,native_identity_verified_at,readiness_state,
            invocation_input_json,workflow_version,workflow_hash,prompt_hash,launch_state,launch_error,
            resume_count,capability_key,capability_identity_json,recovery_root_pid,
            recovery_process_group_id,desired_running,launch_boot_identity,recovery_anchor_json,
            initial_hook_event_boundary_rowid,lane_id,setup_permit_id,interrupt_requested_at
          FROM sessions;
        DROP TABLE sessions;
        DROP TABLE provider_observations; DROP TABLE provider_validation_invocations;
        DROP TABLE provider_validation_batches;
        ALTER TABLE sessions_schema_38 RENAME TO sessions;
        PRAGMA foreign_keys=ON;";

fn schema_31_service_fixture(store: &Store) {
    store
        .lock()
        .unwrap()
        .execute_batch(&prior_service_schema_sql(31))
        .unwrap();
    store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-service-upgrade-fixture','service-upgrade-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','needs_input','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO review_budgets(id,attempt_id,review_kind,initial_allowance,extension_allowance,spent)
               VALUES('b','a','code',2,4,6);
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','code_reviewer','codex',1,1,'exited','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
               VALUES('s','g','codex','exited','{}','fixture','e','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,prompt_hash,handoff_hash,delivery_state,verdict,created_at,updated_at)
               VALUES('approved','a','code','c1','p','h','finished','approved','2026-01-01T00:00:01Z','2026-01-01T00:00:01Z'),
                     ('final','a','final','c1','p','h','finished','request_changes','2026-01-01T00:00:02Z','2026-01-01T00:00:02Z'),
                     ('sixth','a','code','c2','p','h','finished','needs_rework','2026-01-01T00:00:03Z','2026-01-01T00:00:03Z');
             INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
               VALUES('sixth-result','sixth','s','g','needs_rework','x','[]','{}','2026-01-01T00:00:03Z','2026-01-01T00:00:03Z');
             INSERT INTO final_repair_rechecks(attempt_id,task_id,operation_id,request_hash,authorized_task_version,plan_hash,configuration_hash,
                 approved_code_request_id,prior_candidate_hash,final_request_id,rejected_code_request_id,rejected_code_result_id,
                 reviewer_generation_id,reviewer_session_id,reviewer_settings_revision,reviewer_profile_hash,detail_json,state,created_at,updated_at)
               VALUES('a','t','recover','hash',7,'plan',NULL,'approved','c1','final','sixth','sixth-result','g','s',1,'profile','{}','authorized',
                 '2026-01-01T00:00:04Z','2026-01-01T00:00:04Z');"
        ).unwrap();
}

const SERVICE_FIXTURE_HISTORY: &str = "SELECT (SELECT group_concat(id||':'||lifecycle) FROM tasks)
         ||'|'||(SELECT group_concat(id||':'||phase||':'||status) FROM attempts)
         ||'|'||(SELECT group_concat(review_kind||':'||(initial_allowance+extension_allowance)||':'||spent) FROM review_budgets)
         ||'|'||(SELECT group_concat(id||':'||delivery_state||':'||verdict) FROM review_requests)
         ||'|'||(SELECT group_concat(attempt_id||':'||operation_id||':'||state||':'||reviewer_session_id
              ||':'||rejected_code_request_id||':'||rejected_code_result_id) FROM final_repair_rechecks)
         ||'|'||(SELECT revision FROM state_revision WHERE singleton=1)";

fn restorepoint_paths() -> InstancePaths {
    let root = std::env::temp_dir().join(format!("llmrelay-restorepoint-{}", uuid::Uuid::new_v4()));
    let paths = InstancePaths::resolve(Some(root)).unwrap();
    paths.create().unwrap();
    paths
}

fn restorepoint_fixture(version: i64) -> (InstancePaths, Store) {
    let paths = restorepoint_paths();
    let mut store = Store::open(&paths.database).unwrap();
    store
        .lock()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    schema_31_service_fixture(&store);
    if version != 31 {
        drop(store);
        store = Store::open_service(&paths.database).unwrap();
        store
            .lock()
            .unwrap()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute_batch(&prior_service_schema_sql(version))
            .unwrap();
    }
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE projects SET repository_path=?1",
            [paths.root.to_str().unwrap()],
        )
        .unwrap();
    (paths, store)
}

fn restorepoint_root(paths: &InstancePaths) -> std::path::PathBuf {
    paths.root.parent().unwrap().join(format!(
        "{}.backups",
        paths.root.file_name().unwrap().to_str().unwrap()
    ))
}

fn restorepoint_snapshots(paths: &InstancePaths) -> Vec<std::path::PathBuf> {
    let root = restorepoint_root(paths);
    if !root.exists() {
        return Vec::new();
    }
    let mut snapshots: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            !path.file_name().unwrap().to_str().unwrap().starts_with('.')
                && path.join("manifest.json").is_file()
        })
        .collect();
    snapshots.sort();
    snapshots
}

#[test]
fn restorepoint_upgrade_restore_and_held_restart_preserve_each_prior_schema() {
    for version in SERVICE_UPGRADABLE_SCHEMA_VERSIONS {
        let (paths, old) = restorepoint_fixture(version);
        assert_eq!(
                old.lock().unwrap().query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='recipe_schedule_authorizations'",
                    [], |row| row.get::<_, i64>(0),
                ).unwrap(),
                i64::from(version >= 48)
            );
        assert_eq!(
                old.lock().unwrap().query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='knowledge_entries'",
                    [], |row| row.get::<_, i64>(0),
                ).unwrap(),
                i64::from(version >= 47)
            );
        old.lock().unwrap().execute_batch(
                "INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
                   VALUES('credential','g','old-hash','[]','2026-01-01T00:00:00Z');
                 INSERT INTO permission_rules(id,provider,project_id,role,lifetime,registered_root,repository_identity,
                   executable_kind,executable_value,display_family,policy_fingerprint,created_by,created_at)
                   VALUES('rule','codex','p','code_reviewer','project','/tmp','fixture','exact','true','shell','policy','human','2026-01-01T00:00:00Z');
                 INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,process_identity_json,expires_at,created_at,updated_at)
                   VALUES('s','lease','human','human','g','{}','2099-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO launch_permits(id,attempt_id,role,settings_revision,state,created_at)
                   VALUES('permit','a','code_reviewer',1,'issued','2026-01-01T00:00:00Z');
                 INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
                   VALUES('control','a','resume','requested',1,'{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 UPDATE instance_settings SET auto_resume_eligible=1;
                 UPDATE tasks SET title='committed WAL history' WHERE id='t';"
            ).unwrap();
        let main_bytes = std::fs::read(&paths.database).unwrap();
        assert_eq!(
            u32::from_be_bytes(main_bytes[60..64].try_into().unwrap()),
            CURRENT_SCHEMA_VERSION as u32
        );
        assert!(
            std::fs::metadata(format!("{}-wal", paths.database.display()))
                .unwrap()
                .len()
                > 0
        );
        let history: String = old
            .lock()
            .unwrap()
            .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
            .unwrap();
        assert!(Store::open_current_readonly(&paths.database).is_err());
        assert!(Store::open_current_writable(&paths.database).is_err());
        assert!(database::inspect(&paths).is_err());
        assert!(database::backup(&paths, None).is_err());
        assert!(restorepoint_snapshots(&paths).is_empty());

        let lock = database::InstanceLock::acquire(&paths).unwrap();
        let upgraded = database::open_service_locked(&paths, &lock).unwrap();
        assert_eq!(
            upgraded
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM codex_usage_observations", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            require_maintenance_schema(&upgraded.lock().unwrap()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            upgraded
                .lock()
                .unwrap()
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
                .unwrap(),
            history
        );
        assert!(upgraded.restore_hold().unwrap().is_none());
        let snapshots = restorepoint_snapshots(&paths);
        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        let verified = database::verify(snapshot).unwrap();
        assert_eq!(verified["schema_version"], version);
        let snapshot_connection = Connection::open_with_flags(
            format!(
                "file:{}?immutable=1",
                snapshot.join("database.sqlite3").display()
            ),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .unwrap();
        assert_eq!(
            snapshot_connection
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
                .unwrap(),
            history
        );
        assert_eq!(
            snapshot_connection
                .query_row("SELECT title FROM tasks", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "committed WAL history"
        );
        drop(snapshot_connection);
        drop(upgraded);
        drop(old);
        drop(lock);

        let restored = database::restore(&paths, snapshot).unwrap();
        assert_eq!(restored["automatic_resume"], false);
        let held = Store::open_maintenance_readonly(&paths.database).unwrap();
        assert_eq!(
            require_maintenance_schema(&held.lock().unwrap()).unwrap(),
            version
        );
        assert_eq!(
            held.restore_hold().unwrap().unwrap()["operation_id"],
            restored["operation_id"]
        );
        assert!(held.require_execution_unheld("dispatch").is_err());
        let authority: String = held.lock().unwrap().query_row(
                "SELECT (SELECT revoked_at IS NOT NULL FROM role_credentials WHERE id='credential')||':'||
                   (SELECT revoked_at IS NOT NULL FROM permission_rules WHERE id='rule')||':'||
                   (SELECT revoked_at IS NOT NULL FROM input_leases WHERE session_id='s')||':'||
                   (SELECT state FROM launch_permits WHERE id='permit')||':'||
                   (SELECT state FROM controls WHERE id='control')||':'||
                   (SELECT auto_resume_eligible FROM instance_settings)||':'||
                   (SELECT queue_paused FROM projects WHERE id='p')", [], |row| row.get(0),
            ).unwrap();
        assert_eq!(authority, "1:1:1:released:cancelled:0:1");
        let held_history: String = held
            .lock()
            .unwrap()
            .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            held_history.rsplit_once('|').unwrap().0,
            history.rsplit_once('|').unwrap().0
        );
        let inode = std::fs::metadata(&paths.database).unwrap().ino();
        drop(held);

        let lock = database::InstanceLock::acquire(&paths).unwrap();
        database::recover_interrupted_restore_locked(&paths).unwrap();
        let next = database::open_service_locked(&paths, &lock).unwrap();
        assert_eq!(
            require_maintenance_schema(&next.lock().unwrap()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(std::fs::metadata(&paths.database).unwrap().ino(), inode);
        assert_eq!(
            next.restore_hold().unwrap().unwrap()["operation_id"],
            restored["operation_id"]
        );
        assert!(next.require_execution_unheld("resume").is_err());
        assert_eq!(
            next.lock()
                .unwrap()
                .query_row(
                    "SELECT auto_resume_eligible FROM instance_settings",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(next);
        database::recover_interrupted_restore_locked(&paths).unwrap();
        drop(lock);
        assert_eq!(database::verify(snapshot).unwrap(), verified);
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }
}

#[test]
fn restorepoint_new_current_and_unlocked_maintenance_do_not_gain_upgrade_authority() {
    let paths = restorepoint_paths();
    assert!(Store::open_maintenance_readonly(&paths.database).is_err());
    assert!(Store::open_maintenance_writable(&paths.database).is_err());
    assert!(!paths.database.exists());
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    for _ in 0..2 {
        drop(database::open_service_locked(&paths, &lock).unwrap());
        assert!(!restorepoint_root(&paths).exists());
    }
    assert!(database::InstanceLock::acquire(&paths).is_err());
    assert!(database::backup(&paths, None)
        .unwrap_err()
        .to_string()
        .contains("another LLMRelay instance owns"));
    let other = restorepoint_paths();
    assert!(database::open_service_locked(&other, &lock).is_err());
    assert!(!other.database.exists());
    drop(lock);
    std::fs::remove_dir_all(paths.root).unwrap();
    std::fs::remove_dir_all(other.root).unwrap();
}

#[test]
fn restorepoint_capacity_and_prepublication_refusals_leave_source_and_points_unchanged() {
    let (paths, source) = restorepoint_fixture(35);
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    let main = std::fs::read(&paths.database).unwrap();
    let wal_path = format!("{}-wal", paths.database.display());
    let wal = std::fs::read(&wal_path).unwrap();
    database::TEST_AVAILABLE_CAPACITY.with(|values| *values.borrow_mut() = [u64::MAX, 0].into());
    let error = database::open_service_locked(&paths, &lock).err().unwrap();
    assert!(format!("{error:#}").contains("migration capacity after verified restore point"));
    let prior = restorepoint_snapshots(&paths);
    assert_eq!(prior.len(), 1);
    let verified = database::verify(&prior[0]).unwrap();
    for interrupt in [false, true] {
        database::TEST_AVAILABLE_CAPACITY
            .with(|values| *values.borrow_mut() = [if interrupt { u64::MAX } else { 0 }].into());
        database::TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH.with(|armed| armed.set(interrupt));
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        let expected = if interrupt {
            "injected interruption before backup publication"
        } else {
            "shared filesystem backup and migration capacity"
        };
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        database::TEST_AVAILABLE_CAPACITY.with(|values| assert!(values.borrow().is_empty()));
        assert_eq!(
            require_maintenance_schema(&source.lock().unwrap()).unwrap(),
            35
        );
        assert_eq!(std::fs::read(&paths.database).unwrap(), main);
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal);
        assert_eq!(restorepoint_snapshots(&paths), prior);
        assert_eq!(database::verify(&prior[0]).unwrap(), verified);
    }
    assert_eq!(
        std::fs::read_dir(restorepoint_root(&paths))
            .unwrap()
            .count(),
        2
    );
    drop(source);
    drop(lock);
    std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
    std::fs::remove_dir_all(paths.root).unwrap();
}

#[test]
fn restorepoint_invalid_sources_refuse_before_upgrade_and_preserve_verified_points() {
    let newer_schema = format!("schema_{}", CURRENT_SCHEMA_VERSION + 1);
    for fault in [
        "corrupt",
        "foreign_keys",
        "schema_0",
        "schema_30",
        newer_schema.as_str(),
    ] {
        let (paths, source) = restorepoint_fixture(31);
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        database::TEST_AVAILABLE_CAPACITY
            .with(|values| *values.borrow_mut() = [u64::MAX, 0].into());
        assert!(database::open_service_locked(&paths, &lock).is_err());
        let points = restorepoint_snapshots(&paths);
        let verified = database::verify(&points[0]).unwrap();
        match fault {
            "corrupt" => {}
            "foreign_keys" => source
                .lock()
                .unwrap()
                .execute_batch("PRAGMA foreign_keys=OFF; UPDATE tasks SET project_id='missing';")
                .unwrap(),
            _ => source
                .lock()
                .unwrap()
                .pragma_update(
                    None,
                    "user_version",
                    fault
                        .strip_prefix("schema_")
                        .unwrap()
                        .parse::<i64>()
                        .unwrap(),
                )
                .unwrap(),
        }
        drop(source);
        if fault == "corrupt" {
            std::fs::write(&paths.database, b"not a SQLite database").unwrap();
        }
        let before = std::fs::read(&paths.database).unwrap();
        let error = database::open_service_locked(&paths, &lock).err().unwrap();
        let expected = match fault {
            "corrupt" => "not a database",
            "foreign_keys" => "foreign_key_violations=1",
            _ => "unsupported database schema version",
        };
        assert!(
            format!("{error:#}").contains(expected),
            "{fault}: {error:#}"
        );
        assert_eq!(std::fs::read(&paths.database).unwrap(), before);
        assert_eq!(restorepoint_snapshots(&paths), points);
        assert_eq!(database::verify(&points[0]).unwrap(), verified);
        assert!(!paths.state.join("database-restore-journal.json").exists());
        drop(lock);
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }
}

#[test]
fn restorepoint_contention_and_prepublication_failure_do_not_enable_wal() {
    let (paths, source) = restorepoint_fixture(31);
    drop(source);
    let writer = Connection::open(&paths.database).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
        .unwrap();
    let before = std::fs::read(&paths.database).unwrap();
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    let error = database::open_service_locked(&paths, &lock).err().unwrap();
    assert!(
        format!("{error:#}").contains("database is locked"),
        "{error:#}"
    );
    assert!(restorepoint_snapshots(&paths).is_empty());
    writer.execute_batch("ROLLBACK;").unwrap();
    database::TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH.with(|armed| armed.set(true));
    let error = database::open_service_locked(&paths, &lock).err().unwrap();
    assert!(format!("{error:#}").contains("injected interruption"));
    assert!(restorepoint_snapshots(&paths).is_empty());
    assert_eq!(
        writer
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(require_maintenance_schema(&writer).unwrap(), 31);
    assert_eq!(std::fs::read(&paths.database).unwrap(), before);
    assert!(!std::path::PathBuf::from(format!("{}-wal", paths.database.display())).exists());
    drop(writer);
    drop(lock);
    std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
    std::fs::remove_dir_all(paths.root).unwrap();
}

#[test]
fn restorepoint_old_repository_and_inventory_errors_cannot_bypass_restore_guards() {
    let (paths, source) = restorepoint_fixture(31);
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    database::TEST_AVAILABLE_CAPACITY.with(|values| *values.borrow_mut() = [u64::MAX, 0].into());
    assert!(database::open_service_locked(&paths, &lock).is_err());
    let points = restorepoint_snapshots(&paths);
    source
        .lock()
        .unwrap()
        .execute(
            "UPDATE projects SET repository_path=?1",
            [paths.root.parent().unwrap().to_str().unwrap()],
        )
        .unwrap();
    let error = database::open_service_locked(&paths, &lock).err().unwrap();
    assert!(format!("{error:#}").contains("overlaps registered repository"));
    assert_eq!(
        require_maintenance_schema(&source.lock().unwrap()).unwrap(),
        31
    );
    drop(lock);
    let error = database::restore(&paths, &points[0]).unwrap_err();
    assert!(format!("{error:#}").contains("inside a registered repository"));
    source
        .lock()
        .unwrap()
        .execute(
            "UPDATE projects SET repository_path=?1",
            [paths.root.to_str().unwrap()],
        )
        .unwrap();
    source
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE cmux_session_surfaces;")
        .unwrap();
    drop(source);
    let before = std::fs::read(&paths.database).unwrap();
    let error = database::restore(&paths, &points[0]).unwrap_err();
    assert!(format!("{error:#}").contains("capture inventory from readable displaced database"));
    assert_eq!(std::fs::read(&paths.database).unwrap(), before);
    assert_eq!(restorepoint_snapshots(&paths), points);
    assert!(!paths.state.join("database-restore-journal.json").exists());
    assert!(!paths.state.join("database-quarantine").exists());
    std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
    std::fs::remove_dir_all(paths.root).unwrap();
}

#[test]
fn restorepoint_manifest_must_match_the_actual_admitted_schema() {
    let (paths, source) = restorepoint_fixture(31);
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    drop(database::open_service_locked(&paths, &lock).unwrap());
    drop(source);
    drop(lock);
    let point = restorepoint_snapshots(&paths).remove(0);
    let manifest_path = point.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["sqlite_schema_version"] = 32.into();
    std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(database::verify(&point)
        .unwrap_err()
        .to_string()
        .contains("does not match actual SQLite schema 31"));
    let before = std::fs::read(&paths.database).unwrap();
    assert!(database::restore(&paths, &point)
        .unwrap_err()
        .to_string()
        .contains("does not match actual SQLite schema 31"));
    assert_eq!(std::fs::read(&paths.database).unwrap(), before);
    assert!(!paths.state.join("database-restore-journal.json").exists());
    std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
    std::fs::remove_dir_all(paths.root).unwrap();
}

#[test]
fn restorepoint_intermediate_failure_retains_original_recovery_and_held_journal() {
    let (paths, source) = restorepoint_fixture(31);
    source
        .lock()
        .unwrap()
        .execute_batch("CREATE VIEW provider_failure_holds AS SELECT 1 AS marker;")
        .unwrap();
    let history: String = source
        .lock()
        .unwrap()
        .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
        .unwrap();
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    let error = database::open_service_locked(&paths, &lock).err().unwrap();
    assert!(
        format!("{error:#}").contains("provider_failure_holds already exists"),
        "{error:#}"
    );
    let original = restorepoint_snapshots(&paths).remove(0);
    assert!(error.to_string().contains(original.to_str().unwrap()));
    assert!(error.to_string().contains("offline recovery"));
    assert_eq!(database::verify(&original).unwrap()["schema_version"], 31);
    assert_eq!(
        require_maintenance_schema(&source.lock().unwrap()).unwrap(),
        34
    );
    assert_eq!(
        source
            .lock()
            .unwrap()
            .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
            .unwrap(),
        history
    );
    for _ in 0..11 {
        assert!(database::open_service_locked(&paths, &lock).is_err());
    }
    assert!(original.exists());
    assert_eq!(restorepoint_snapshots(&paths).len(), 10);
    assert_eq!(database::verify(&original).unwrap()["schema_version"], 31);
    assert!(!paths.state.join("database-restore-journal.json").exists());
    drop(source);
    drop(lock);

    let restored = database::restore(&paths, &original).unwrap();
    let lock = database::InstanceLock::acquire(&paths).unwrap();
    database::recover_interrupted_restore_locked(&paths).unwrap();
    assert!(database::open_service_locked(&paths, &lock).is_err());
    database::recover_interrupted_restore_locked(&paths).unwrap();
    let intermediate = Store::open_maintenance_writable(&paths.database).unwrap();
    assert_eq!(
        require_maintenance_schema(&intermediate.lock().unwrap()).unwrap(),
        34
    );
    assert_eq!(
        intermediate.restore_hold().unwrap().unwrap()["operation_id"],
        restored["operation_id"]
    );
    assert!(intermediate.require_execution_unheld("dispatch").is_err());
    intermediate
        .lock()
        .unwrap()
        .execute_batch("DROP VIEW provider_failure_holds;")
        .unwrap();
    drop(intermediate);
    database::recover_interrupted_restore_locked(&paths).unwrap();
    let repaired = database::open_service_locked(&paths, &lock).unwrap();
    assert_eq!(
        require_maintenance_schema(&repaired.lock().unwrap()).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        repaired.restore_hold().unwrap().unwrap()["operation_id"],
        restored["operation_id"]
    );
    drop(repaired);
    database::recover_interrupted_restore_locked(&paths).unwrap();
    drop(lock);
    std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
    std::fs::remove_dir_all(paths.root).unwrap();
}

#[test]
fn restorepoint_old_hold_pending_restarts_and_offline_release_preserve_bindings() {
    for version in SERVICE_UPGRADABLE_SCHEMA_VERSIONS {
        let (paths, source) = restorepoint_fixture(version);
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        drop(database::open_service_locked(&paths, &lock).unwrap());
        drop(lock);
        drop(source);
        let point = restorepoint_snapshots(&paths).remove(0);
        for committed in [false, true] {
            let restored = database::restore(&paths, &point).unwrap();
            let journal_path = paths.state.join("database-restore-journal.json");
            let mut journal: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
            if !committed {
                // Reconstruct the existing pre-hold boundary without replacing its bound inode.
                std::fs::write(
                    &paths.database,
                    std::fs::read(point.join("database.sqlite3")).unwrap(),
                )
                .unwrap();
                for suffix in ["-wal", "-shm"] {
                    let sidecar =
                        std::path::PathBuf::from(format!("{}{suffix}", paths.database.display()));
                    if sidecar.exists() {
                        std::fs::remove_file(sidecar).unwrap();
                    }
                }
            }
            journal["phase"] = "hold_pending".into();
            std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
            let lock = database::InstanceLock::acquire(&paths).unwrap();
            database::recover_interrupted_restore_locked(&paths).unwrap();
            let held = Store::open_maintenance_readonly(&paths.database).unwrap();
            assert_eq!(
                require_maintenance_schema(&held.lock().unwrap()).unwrap(),
                version
            );
            assert_eq!(
                held.restore_hold().unwrap().unwrap()["operation_id"],
                restored["operation_id"]
            );
            assert_eq!(
                std::fs::metadata(&paths.database).unwrap().ino(),
                journal["staged_binding"]["inode"].as_u64().unwrap()
            );
            assert_eq!(journal["inventory"]["state"], "available");
            drop(held);
            if committed {
                let journal_before = std::fs::read(&journal_path).unwrap();
                let quarantine =
                    std::path::PathBuf::from(journal["moves"][0]["quarantine"].as_str().unwrap());
                let original = std::fs::read(&quarantine).unwrap();
                let mut changed = original.clone();
                changed.push(1);
                std::fs::write(&quarantine, &changed).unwrap();
                let error = database::recover_interrupted_restore_locked(&paths).unwrap_err();
                assert!(format!("{error:#}").contains("recorded binding"));
                assert_eq!(std::fs::read(&quarantine).unwrap(), changed);
                assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
                std::fs::write(&quarantine, original).unwrap();
                let bound = paths.state.join("bound-old.sqlite3");
                std::fs::rename(&paths.database, &bound).unwrap();
                std::fs::copy(&bound, &paths.database).unwrap();
                let error = database::recover_interrupted_restore_locked(&paths).unwrap_err();
                assert!(format!("{error:#}").contains("staged-file identity"));
                assert_eq!(std::fs::read(&journal_path).unwrap(), journal_before);
                std::fs::remove_file(&paths.database).unwrap();
                std::fs::rename(bound, &paths.database).unwrap();
            }
            drop(lock);
            assert_eq!(
                database::release_hold(&paths).unwrap()["automatic_resume"],
                false
            );
            let released = Store::open_maintenance_readonly(&paths.database).unwrap();
            assert_eq!(
                require_maintenance_schema(&released.lock().unwrap()).unwrap(),
                version
            );
            assert!(released.restore_hold().unwrap().is_none());
            let paused: String = released.lock().unwrap().query_row(
                    "SELECT (SELECT queue_paused FROM projects)||':'||(SELECT attention FROM tasks)||':'||
                       (SELECT auto_resume_eligible FROM instance_settings)||':'||(SELECT desired_running FROM sessions)", [], |row| row.get(0),
                ).unwrap();
            assert_eq!(paused, "1:paused:0:0");
        }
        std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
        std::fs::remove_dir_all(paths.root).unwrap();
    }
}

#[test]
fn restorepoint_retention_keeps_each_schema_anchor_and_ignores_unverified_content() {
    let mut host = None;
    let mut host_instance = serde_json::Value::Null;
    let mut anchors = Vec::new();
    for version in SERVICE_UPGRADABLE_SCHEMA_VERSIONS {
        let (paths, source) = restorepoint_fixture(version);
        let lock = database::InstanceLock::acquire(&paths).unwrap();
        drop(database::open_service_locked(&paths, &lock).unwrap());
        drop(lock);
        drop(source);
        let point = restorepoint_snapshots(&paths).remove(0);
        let manifest_path = point.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["created_at"] = "2000-01-01T00:00:00Z".into();
        // Moved points stand in for the host's own earlier upgrades, so they join its retention domain.
        if host.is_some() {
            manifest["source_instance"] = host_instance.clone();
        } else {
            host_instance = manifest["source_instance"].clone();
            assert!(host_instance.is_string());
        }
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        if let Some(host_paths) = &host {
            let moved = restorepoint_root(host_paths).join(point.file_name().unwrap());
            std::fs::rename(&point, &moved).unwrap();
            anchors.push(moved);
            std::fs::remove_dir_all(restorepoint_root(&paths)).unwrap();
            std::fs::remove_dir_all(paths.root).unwrap();
        } else {
            anchors.push(point);
            host = Some(paths);
        }
    }
    let host = host.unwrap();
    let root = restorepoint_root(&host);
    let copy = |name: &str| {
        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        for file in ["database.sqlite3", "manifest.json"] {
            std::fs::copy(anchors[0].join(file), path.join(file)).unwrap();
        }
        path
    };
    let tied_anchor = copy("zzz-schema31-tie");
    let mismatched = copy("mismatched-schema");
    let corrupted = copy("unverified-hash");
    let unowned = copy("operator-owned");
    let manifest_path = mismatched.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["sqlite_schema_version"] = 32.into();
    manifest["created_at"] = "2999-01-01T00:00:00Z".into();
    std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        corrupted.join("database.sqlite3"),
        b"unverified operator evidence",
    )
    .unwrap();
    std::fs::write(unowned.join("operator-notes.txt"), b"retain").unwrap();
    let mut newest = serde_json::Value::Null;
    for _ in 0..12 {
        newest = database::backup(&host, None).unwrap();
    }
    assert!(
        !anchors[0].exists(),
        "the deterministic newer tie supersedes the expired schema-31 point"
    );
    assert!(tied_anchor.exists());
    for point in &anchors[1..] {
        assert!(point.exists());
    }
    assert_eq!(
        database::verify(&tied_anchor).unwrap()["schema_version"],
        31
    );
    let newest_path = Path::new(newest["backup"].as_str().unwrap());
    assert!(newest_path.exists());
    assert_eq!(
        database::verify(newest_path).unwrap()["schema_version"],
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
            restorepoint_snapshots(&host).len(),
            SERVICE_UPGRADABLE_SCHEMA_VERSIONS.len() + 1 + 3,
            "one protected point per prior schema, the newest current-schema point and three unrecognized points"
        );
    assert!(mismatched.exists());
    assert!(database::verify(&mismatched).is_err());
    assert_eq!(
        std::fs::read(corrupted.join("database.sqlite3")).unwrap(),
        b"unverified operator evidence"
    );
    assert_eq!(
        std::fs::read(unowned.join("operator-notes.txt")).unwrap(),
        b"retain"
    );
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(host.root).unwrap();
}

#[test]
fn schema_36_upgrade_preserves_migration_receipts_without_revision_triggers() {
    let (paths, old) = restorepoint_fixture(36);
    let receipt_rows =
        "SELECT json_group_array(json_array(id,attempt_id,from_workflow_id,to_workflow_id,
            preserved_json,reviewed_plan_hash,config_revision_id,authorized_at))
            FROM (SELECT * FROM trip_legacy_migrations ORDER BY id)";
    let (receipts, history): (String, String) = {
        let connection = old.lock().unwrap();
        connection.execute_batch(
                "INSERT INTO trip_config_revisions(id,project_id,revision,state,config_json,adapters_json,preflight_json,
                    verification_json,source_hash,overlay_hash,configuration_hash,created_at)
                 VALUES('config','p',1,'activated','{}','{}','[]','{}','source','overlay','configuration','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                 VALUES('a2','t','second-context','planning','base',1,'needs_input','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,config_revision_id,authorized_at)
                 VALUES('old-a','a','trip-v1','trip-0.9','{\"history\":true}','approved-a','config','2026-01-01T00:00:01Z'),
                       ('old-a2','a2','trip-v1','trip-0.9','{\"history\":[1,2]}','approved-a2','config','2026-01-01T00:00:02Z');"
            ).unwrap();
        (
            connection
                .query_row(receipt_rows, [], |row| row.get(0))
                .unwrap(),
            connection
                .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get(0))
                .unwrap(),
        )
    };
    drop(old);
    let upgraded = Store::open_service(&paths.database).unwrap();
    let connection = upgraded.lock().unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        connection
            .query_row(receipt_rows, [], |row| row.get::<_, String>(0))
            .unwrap(),
        receipts
    );
    assert_eq!(
        connection
            .query_row(SERVICE_FIXTURE_HISTORY, [], |row| row.get::<_, String>(0))
            .unwrap(),
        history
    );
    assert_eq!(connection.query_row(
            "SELECT COUNT(*) FROM trip_legacy_migrations WHERE target_workflow_hash IS NULL AND target_source_hash IS NULL
             AND target_overlay_hash IS NULL AND target_manifest_hash IS NULL", [], |row| row.get::<_, i64>(0),
        ).unwrap(), 2);
    assert_eq!(connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='trip_legacy_migrations'",
            [], |row| row.get::<_, i64>(0),
        ).unwrap(), 0);
    let revision: i64 = connection
        .query_row(
            "SELECT revision FROM state_revision WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection.execute_batch(
            "INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,
                config_revision_id,authorized_at,target_workflow_hash,target_source_hash,target_overlay_hash,target_manifest_hash)
             VALUES('new-a','a','trip-0.9','trip-0.11','{}','approved-new','config','2026-01-02T00:00:00Z','workflow','source','overlay','manifest');"
        ).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT revision FROM state_revision WHERE singleton=1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        revision
    );
    let duplicate = connection.execute(
            "INSERT INTO trip_legacy_migrations(id,attempt_id,from_workflow_id,to_workflow_id,preserved_json,reviewed_plan_hash,config_revision_id,authorized_at)
             VALUES('duplicate','a','trip-v1','trip-0.11','{}','other-plan','config','2026-01-03T00:00:00Z')", [],
        ).unwrap_err();
    assert!(
        duplicate.to_string().contains("UNIQUE constraint failed"),
        "{duplicate}"
    );
    for sql in [
        "UPDATE trip_legacy_migrations SET preserved_json='{\"retained\":true}' WHERE id='new-a'",
        "DELETE FROM trip_legacy_migrations WHERE id='new-a'",
    ] {
        connection.execute(sql, []).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT revision FROM state_revision WHERE singleton=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            revision
        );
    }
    assert_eq!(
        connection
            .query_row(receipt_rows, [], |row| row.get::<_, String>(0))
            .unwrap(),
        receipts
    );
    drop(connection);
    drop(upgraded);
    std::fs::remove_dir_all(paths.root).unwrap();
}

#[test]
fn service_start_upgrades_supported_prior_schemas_and_preserves_receipts() {
    let root =
        std::env::temp_dir().join(format!("llmrelay-service-upgrade-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("state.sqlite3");
    let store = Store::open(&database).unwrap();
    let scalar = |sql: &str| -> String {
        Connection::open(&database)
            .unwrap()
            .query_row(sql, [], |row| row.get::<_, rusqlite::types::Value>(0))
            .map(|value| format!("{value:?}"))
            .unwrap()
    };
    let set_version = |version: i64| {
        Connection::open(&database)
            .unwrap()
            .pragma_update(None, "user_version", version)
            .unwrap()
    };
    let schema = "SELECT group_concat(name||':'||COALESCE(sql,''), char(10))
             FROM (SELECT name, sql FROM sqlite_master ORDER BY name)";
    let version = "PRAGMA user_version";
    let current_schema = scalar(schema);
    schema_31_service_fixture(&store);
    drop(store);
    let history = SERVICE_FIXTURE_HISTORY;
    let schema_31 = scalar(schema);
    for refused in [
        Store::open_current_readonly(&database).err(),
        Store::open_current_writable(&database).err(),
    ] {
        assert!(refused
            .unwrap()
            .to_string()
            .contains("unsupported database schema version 31"));
    }
    assert_eq!(scalar(version), "Integer(31)");

    // A receipt that fails the rebuilt table's foreign keys rolls the whole
    // upgrade back, leaving schema 31 and every row intact.
    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
                 UPDATE final_repair_rechecks SET reviewer_session_id='missing-session';",
        )
        .unwrap();
    let broken_history = scalar(history);
    let error = Store::open_service(&database).err().unwrap();
    assert!(
        format!("{error:#}").contains("FOREIGN KEY constraint failed"),
        "{error:#}"
    );
    assert_eq!(scalar(version), "Integer(31)");
    assert_eq!(scalar(history), broken_history);
    assert_eq!(scalar(schema), schema_31);
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE final_repair_rechecks SET reviewer_session_id='s'",
            [],
        )
        .unwrap();
    let expected_history = scalar(history);

    for _ in 0..2 {
        drop(Store::open_service(&database).unwrap());
        assert_eq!(
            scalar(version),
            format!("Integer({CURRENT_SCHEMA_VERSION})")
        );
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
        assert_eq!(
            scalar(
                "SELECT provenance_kind||':'||COALESCE(final_result_id,'none')
                     FROM final_repair_rechecks WHERE attempt_id='a'"
            ),
            "Text(\"historical_sixth_review_recovery:none\")"
        );
    }
    // Schema 32 gains only native-resolution provenance and the later migrations.
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(32))
        .unwrap();
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    // Schema 33 gains only the guidance submitted form and the later migrations.
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(33))
        .unwrap();
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(34))
        .unwrap();
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(35))
        .unwrap();
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(36))
        .unwrap();
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(37))
        .unwrap();
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    assert_eq!(
        scalar("SELECT COUNT(*) FROM codex_usage_observations"),
        "Integer(0)"
    );
    Connection::open(&database)
        .unwrap()
        .execute_batch(&prior_service_schema_sql(38))
        .unwrap();
    assert_eq!(scalar(version), "Integer(38)");
    drop(Store::open_service(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    // An ordinary open of the current schema reopens it without migrating;
    // only a newer schema is refused as unknown.
    drop(Store::open(&database).unwrap());
    assert_eq!(
        scalar(version),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(history), expected_history);
    assert_eq!(scalar(schema), current_schema);
    let future_version = CURRENT_SCHEMA_VERSION + 1;
    set_version(future_version);
    let error = Store::open(&database).err().unwrap();
    assert!(
        error
            .to_string()
            .contains(&format!("database schema {future_version} is newer")),
        "{error:#}"
    );
    assert_eq!(scalar(version), format!("Integer({future_version})"));
    assert_eq!(scalar(history), expected_history);
    set_version(CURRENT_SCHEMA_VERSION);

    for unsupported in [0, 14, 29, 30, future_version] {
        set_version(unsupported);
        let error = Store::open_service(&database).err().unwrap();
        assert!(
            error.to_string().contains(&format!(
                "unsupported database schema version {unsupported}"
            )),
            "{error:#}"
        );
        assert_eq!(scalar(version), format!("Integer({unsupported})"));
        assert_eq!(scalar(history), expected_history);
        assert_eq!(scalar(schema), current_schema);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn trusted_checkout_migration_keeps_legacy_worktrees_and_rejects_unknown_modes() {
    use crate::domain::{TaskWorkflowSelectionInput, WorkspaceMode};
    let root = std::env::temp_dir().join(format!(
        "llmrelay-trusted-checkout-migration-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("state.sqlite3");
    let store = Store::open(&database).unwrap();
    store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-trusted-checkout-fixture','trusted-checkout-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
    let selected_hash = json_hash(&serde_json::json!({"family":"trip"})).unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO task_workflow_selections(task_id,family,check_ids_json,selection_revision,
                selection_hash,admission_state,created_at,updated_at)
             VALUES('t','trip','[]',1,?1,'draft','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            params![selected_hash],
        )
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute_batch(&prior_service_schema_sql(39))
        .unwrap();
    drop(store);
    assert!(Store::open_current_readonly(&database).is_err());
    let reopened = Store::open_service(&database).unwrap();
    let connection = Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
            connection.query_row(
                "SELECT workspace_mode||':'||selection_hash FROM task_workflow_selections WHERE task_id='t'",
                [], |row| row.get::<_, String>(0),
            ).unwrap(),
            format!("isolated_worktree:{selected_hash}")
        );
    connection.execute_batch(
            "INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
             VALUES('legacy-unselected','p','Legacy','backlog','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        ).unwrap();
    let state = crate::workflow::state(&reopened).unwrap();
    assert!(state
        .tasks
        .iter()
        .all(|task| task.workspace_mode == WorkspaceMode::IsolatedWorktree));
    assert!(state
        .tasks
        .iter()
        .find(|task| task.id == "legacy-unselected")
        .unwrap()
        .workflow
        .is_none());
    let legacy: TaskWorkflowSelectionInput =
        serde_json::from_value(serde_json::json!({"family":"trip"})).unwrap();
    assert_eq!(legacy.workspace_mode, WorkspaceMode::IsolatedWorktree);
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::json!({
            "family":"trip","execution_config_revision_id":null,"profile_revision_id":null,"check_ids":[]
        })
    );
    assert!(serde_json::from_value::<TaskWorkflowSelectionInput>(
        serde_json::json!({"family":"trip","workspace_mode":"unknown"})
    )
    .is_err());
    let error = connection
        .execute(
            "UPDATE task_workflow_selections SET workspace_mode='unknown' WHERE task_id='t'",
            [],
        )
        .unwrap_err();
    assert!(error.to_string().contains("CHECK constraint"), "{error}");
    drop(connection);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn borrowed_checkout_baseline_requires_claim_and_preserves_start_identity() {
    let paths = restorepoint_paths();
    let checkout = paths.root.join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(checkout.join("preexisting.txt"), b"user bytes\n").unwrap();
    let repository = crate::workspace::inspect(&checkout).unwrap();
    let store = Store::open(&paths.database).unwrap();
    let policy =
        crate::provider_compatibility::ExecutionPolicy::single_agent_writer(Provider::Codex);
    {
        let connection = store.lock().unwrap();
        connection.execute("INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                VALUES('p','p',?1,?2,?3,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![repository.root.to_str().unwrap(),repository.identity,repository.head]).unwrap();
        connection.execute_batch(
                "INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
                   VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO execution_config_revisions(id,project_id,revision,config_json,content_hash,approved_at,created_at)
                   VALUES('config','p',1,'{}','config-hash','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO agent_profiles(id,project_id,display_name,created_at,updated_at)
                   VALUES('profile','p','writer','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
            ).unwrap();
        connection.execute("INSERT INTO agent_profile_revisions(id,profile_id,revision,provider,adapter_id,adapter_hash,model,effort,instructions_text,instructions_hash,policy_id,policy_revision,policy_hash,authority,session_contract,profile_hash,created_at)
                VALUES('revision','profile',1,'codex','adapter',?1,'fixture','high','','instructions','writer',1,?2,'workspace_write','retained','profile-hash','2026-01-01T00:00:00Z')",
                params![policy.adapter_hash(),policy.content_hash()]).unwrap();
        let (definition, _) =
            crate::workflow::ensure_single_agent_definition(&connection, "2026-01-01T00:00:00Z")
                .unwrap();
        connection.execute(
                "INSERT INTO task_workflow_selections(task_id,family,definition_id,execution_config_revision_id,profile_revision_id,check_ids_json,selection_revision,selection_hash,admission_state,workspace_mode,created_at,updated_at)
                   VALUES('t','single_agent',?1,'config','revision','[]',1,'selection','draft','main_checkout','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![definition],
            ).unwrap();
        let scope = crate::workflow::task_checkout_scope(&connection, "t").unwrap();
        let receipt = serde_json::json!({"entity_kind":"task","entity_id":"t","state":"ready",
                "detail":{"checkout_authorization":scope}});
        connection.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
                VALUES('ready','human_control','human_command','request',?1,'2026-01-01T00:00:00Z')",
                params![receipt.to_string()]).unwrap();
        let frozen = crate::workflow::workspace_policy_for_task(&connection, "t").unwrap();
        connection.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                VALUES('a','t','context','agent_execution',?1,1,'workspace_reserved','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![repository.head]).unwrap();
        connection.execute("INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
                VALUES('claim','t','a',?1,'failed','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![repository.identity]).unwrap();
        connection.execute("INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
                VALUES('w','a',?1,?2,?3,?3,?4,'reserved','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![repository.identity,repository.root.to_str().unwrap(),repository.head,frozen.to_string()]).unwrap();
        assert!(crate::workflow::require_workspace_execution_supported(&connection, "t").is_err());
    }
    let reviews = crate::review::ReviewService::new(store.clone(), paths.artifacts.clone());
    assert!(reviews.capture_workspace_baseline("a", "w").is_err());
    assert_eq!(
        store
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        std::fs::read(checkout.join("preexisting.txt")).unwrap(),
        b"user bytes\n"
    );
    store
        .lock()
        .unwrap()
        .execute("UPDATE claims SET state='reserved' WHERE id='claim'", [])
        .unwrap();
    let first = reviews.capture_workspace_baseline("a", "w").unwrap();
    let policy_json: String = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT policy_json FROM workspaces WHERE id='w'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let frozen: serde_json::Value = serde_json::from_str(&policy_json).unwrap();
    assert_eq!(frozen["workspace_mode"], "main_checkout");
    assert_eq!(frozen["workspace_ownership"], "borrowed_checkout");
    let baseline_id = frozen["baseline_snapshot_id"].as_str().unwrap();
    assert_eq!(
        std::fs::read(
            paths
                .artifacts
                .join("snapshots")
                .join(baseline_id)
                .join("preexisting.txt")
        )
        .unwrap(),
        b"user bytes\n"
    );
    let repeated = reviews.capture_workspace_baseline("a", "w").unwrap();
    crate::snapshot::require_stable_preparation(&first, &repeated).unwrap();
    store.lock().unwrap().execute_batch("UPDATE claims SET state='running' WHERE id='claim'; UPDATE workspaces SET state='ready' WHERE id='w'; UPDATE attempts SET status='running' WHERE id='a';").unwrap();
    let scope = crate::providers::checkout_launch_scope(
        &frozen,
        &paths.root,
        &paths.role_socket,
        &store.repository_inventory().unwrap(),
        &std::env::current_exe().unwrap(),
    )
    .unwrap();
    let native = crate::providers::SingleAgentNativeIdentity {
        provider: Provider::Codex,
        executable: crate::domain::ExecutableFingerprint {
            canonical_path: "/synthetic/codex".into(),
            device: 1,
            inode: 2,
            bytes: 3,
            modified_seconds: 4,
            modified_nanos: 5,
        },
        executable_version: "fixture".into(),
        account_identity: None,
        config_identity: serde_json::json!({"model":"fixture","effort":"high"}),
        security_policy: serde_json::json!({"qualification_contract":"functional-v2",
                "checkout_scope":scope,"execution_actor":"single_agent","synthetic_fixture":true}),
        hook_revision: "synthetic".into(),
        policy_binding_hash: policy.content_hash(),
    };
    let native = serde_json::to_value(native).unwrap();
    let main_key =
        crate::providers::main_checkout_capability_key("single_agent", &scope, &native).unwrap();
    let proof = serde_json::json!({"authorization":frozen["checkout_authorization"],
            "scope_hash":json_hash(&scope).unwrap(),"scope":scope,"mode":"main_checkout",
            "actor":"single_agent","config_hash":main_key,"session_id":"main-proof-source",
            "qualification_contract":native["security_policy"]["qualification_contract"],"synthetic_fixture":true});
    {
        let connection = store.lock().unwrap();
        let turn = CheckoutTurn {
            session: "session",
            epoch: "epoch",
            generation: "generation",
        };
        assert!(require_checkout_reservation(
            &connection,
            "t",
            "a",
            "single_agent",
            &checkout,
            Some(&scope),
            &main_key,
            Some("prompt"),
            turn,
            false
        )
        .is_err());
        connection.execute_batch(
                "INSERT INTO tasks(id,project_id,title,lifecycle,attention,created_at,updated_at)
                   VALUES('proof-task','p','Synthetic qualification','validation','paused','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                   VALUES('proof-attempt','proof-task','proof-context','agent_execution','base',1,'held','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                   VALUES('proof-generation','proof-attempt','single_agent','codex',1,1,'exited','proof-authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
            ).unwrap();
        connection.execute(
                "INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,capability_key,capability_identity_json,exit_json,created_at,updated_at)
                   VALUES('main-proof-source','proof-generation','codex','exited',?1,'fixture','proof-epoch',?2,?3,
                     '{\"process_group_quiescent\":true}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                params![serde_json::json!({"native_identity":native,"synthetic_fixture":true}).to_string(),main_key,native.to_string()],
            ).unwrap();
        connection.execute("INSERT INTO capabilities(id,provider,executable_version,role,mode,config_hash,status,evidence_reference,gaps_json,checked_at,proof_json)
                VALUES('main-proof','codex','fixture','single_agent','main_checkout',?1,'supported','synthetic-boundary-proof','[]','2026-01-01T00:00:00Z',?2)",params![main_key,proof.to_string()]).unwrap();
        assert!(!require_checkout_reservation(
            &connection,
            "t",
            "a",
            "single_agent",
            &checkout,
            Some(&scope),
            &main_key,
            Some("prompt"),
            turn,
            false
        )
        .unwrap());
        assert!(require_checkout_reservation(
            &connection,
            "t",
            "a",
            "implementer",
            &checkout,
            Some(&scope),
            &main_key,
            Some("prompt"),
            turn,
            false
        )
        .is_err());
        assert!(require_checkout_reservation(
            &connection,
            "t",
            "a",
            "single_agent",
            &checkout,
            Some(&scope),
            "worktree-key",
            Some("prompt"),
            turn,
            false
        )
        .is_err());
    }
    let alias = paths.root.join("checkout-alias");
    std::os::unix::fs::symlink(&checkout, &alias).unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE projects SET repository_path=?1 WHERE id='p'",
            params![alias.to_str().unwrap()],
        )
        .unwrap();
    assert!(store.check_checkout_preparation(Some(&scope)).is_err());
    {
        let connection = store.lock().unwrap();
        let error = require_checkout_reservation(
            &connection,
            "t",
            "a",
            "single_agent",
            &checkout,
            Some(&scope),
            &main_key,
            Some("prompt"),
            CheckoutTurn {
                session: "session",
                epoch: "epoch",
                generation: "generation",
            },
            false,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("protection inventory changed"),
            "{error}"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE id!='main-proof-source'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        connection
            .execute(
                "UPDATE projects SET repository_path=?1 WHERE id='p'",
                params![checkout.to_str().unwrap()],
            )
            .unwrap();
        connection.execute_batch("UPDATE claims SET state='reserved' WHERE id='claim'; UPDATE workspaces SET state='reserved' WHERE id='w'; UPDATE attempts SET status='workspace_reserved' WHERE id='a';").unwrap();
    }
    std::fs::write(checkout.join("preexisting.txt"), b"external edits\n").unwrap();
    let error = reviews.capture_workspace_baseline("a", "w").unwrap_err();
    assert!(error.to_string().contains("checkout changed"), "{error}");
    let connection = store.lock().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT policy_json FROM workspaces WHERE id='w'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        policy_json
    );
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM snapshots WHERE kind='baseline' AND attempt_id='a' AND workspace_id='w'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM workflow_run_bindings WHERE source_snapshot_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id!='main-proof-source'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        std::fs::read(checkout.join("preexisting.txt")).unwrap(),
        b"external edits\n"
    );
    drop(connection);
    std::fs::write(checkout.join("preexisting.txt"), b"user bytes\n").unwrap();
    std::fs::write(checkout.join("task-output.txt"), b"task bytes\n").unwrap();
    let candidate_id = uuid::Uuid::new_v4().to_string();
    let (mut candidate, _) = crate::snapshot::capture(
        &repository,
        &checkout,
        &paths.artifacts.join("snapshots").join(&candidate_id),
    )
    .unwrap();
    candidate.baseline_snapshot_id = Some(baseline_id.into());
    candidate.task_changed_paths = crate::snapshot::task_changed_paths(&first, &candidate).unwrap();
    assert_eq!(candidate.task_changed_paths, vec!["task-output.txt"]);
    let candidate_hash = crate::snapshot::write_manifest(
        &candidate,
        &paths.artifacts.join("snapshots").join(&candidate_id),
    )
    .unwrap();
    {
        let connection = store.lock().unwrap();
        connection.execute("INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at,workspace_id,candidate_head)
                VALUES(?1,'a','accepted',?2,?3,?4,1,'2026-01-01T00:00:00Z','w',?2)",params![candidate_id,candidate.candidate_head,candidate_hash,serde_json::to_string(&candidate).unwrap()]).unwrap();
        connection.execute("UPDATE attempts SET status='running',phase='awaiting_human',candidate_hash=?1,accepted_snapshot_id=?2 WHERE id='a'",params![candidate_hash,candidate_id]).unwrap();
        connection.execute_batch("UPDATE claims SET state='running' WHERE id='claim'; UPDATE workspaces SET state='ready' WHERE id='w'; UPDATE tasks SET lifecycle='awaiting_review',attention='none' WHERE id='t';").unwrap();
    }
    assert!(crate::workspace::check_reviewed_checkout(&store, "a")
        .unwrap()
        .is_some());
    std::fs::write(
        checkout.join("task-output.txt"),
        b"external post-review bytes\n",
    )
    .unwrap();
    let error = crate::workspace::check_reviewed_checkout(&store, "a")
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("changed after review"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(checkout.join("task-output.txt")).unwrap(),
        b"external post-review bytes\n"
    );
    let connection = store.lock().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT status FROM attempts WHERE id='a'", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "held"
    );
    assert_eq!(
        connection
            .query_row("SELECT state FROM claims WHERE id='claim'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "running"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM snapshots WHERE kind='baseline'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    drop(connection);
    std::fs::write(checkout.join("task-output.txt"), b"task bytes\n").unwrap();
    {
        let connection = store.lock().unwrap();
        connection.execute_batch("UPDATE attempts SET status='running' WHERE id='a';
                UPDATE tasks SET attention='none' WHERE id='t';
                INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                VALUES('review-generation','a','single_agent','codex',1,1,'exited','review-authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,exit_json,created_at,updated_at)
                VALUES('review-session','review-generation','codex','exited','{}','fixture','review-epoch','{\"process_group_quiescent\":true}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at)
                VALUES('review-report','review-report-operation','review-session','review-generation','result_ready','Synthetic Main review','[]','{}','2026-01-01T00:00:00Z');").unwrap();
        let (definition, definition_hash) =
            crate::workflow::ensure_single_agent_definition(&connection, "2026-01-01T00:00:00Z")
                .unwrap();
        connection.execute("INSERT INTO workflow_run_bindings(attempt_id,family,definition_id,definition_hash,definition_state,evaluator_revision,execution_config_revision_id,execution_config_hash,selection_hash,scope_hash,created_at)
                VALUES('a','single_agent',?1,?2,'available',?3,'config','config-hash','selection','scope','2026-01-01T00:00:00Z')",
                params![definition,definition_hash,crate::workflow_resources::SINGLE_AGENT_EVALUATOR_REVISION]).unwrap();
        connection.execute("INSERT INTO workflow_result_bindings(id,attempt_id,role_result_id,role_generation_id,session_id,transcript_epoch,snapshot_id,candidate_hash,criteria_hash,evidence_hash,profile_binding_hash,selected_check_revision,state,created_at,updated_at)
                VALUES('review-result','a','review-report','review-generation','review-session','review-epoch',?1,?2,'criteria','evidence','profile',0,'awaiting_human','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",params![candidate_id,candidate_hash]).unwrap();
    }
    assert!(crate::workspace::check_reviewed_checkout(&store, "a")
        .unwrap()
        .is_some());
    std::fs::create_dir(checkout.join("module")).unwrap();
    std::fs::write(
        checkout.join("module/unreviewed.txt"),
        b"external module bytes\n",
    )
    .unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args([
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &repository.head,
            "module"
        ])
        .status()
        .unwrap()
        .success());
    let version: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT version FROM tasks WHERE id='t'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let error = crate::workflow::execute_workflow_command(
        &store,
        &crate::domain::WorkflowCommand::AcceptResult {
            operation_id: "accept-main-after-staged-gitlink".into(),
            task_id: "t".into(),
            attempt_id: "a".into(),
            expected_version: version,
            result_binding_id: "review-result".into(),
            candidate_hash: candidate_hash.clone(),
            guidance_change_hash: None,
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("changed after review"),
        "{error}"
    );
    {
        let connection = store.lock().unwrap();
        let held: (String, String, String, String, String, i64) = connection
            .query_row(
                "SELECT a.status,t.lifecycle,t.attention,c.state,r.state,t.version
                 FROM attempts a JOIN tasks t ON t.id=a.task_id JOIN claims c ON c.attempt_id=a.id
                 JOIN workflow_result_bindings r ON r.attempt_id=a.id WHERE a.id='a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            held,
            (
                "held".into(),
                "awaiting_review".into(),
                "needs_input".into(),
                "running".into(),
                "awaiting_human".into(),
                version + 1
            )
        );
        let reason: String = connection
            .query_row(
                "SELECT state_reason FROM workflow_result_bindings WHERE id='review-result'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            reason.contains("live Main checkout changed after review"),
            "{reason}"
        );
        assert_eq!(connection.query_row(
                "SELECT COUNT(*) FROM operation_receipts WHERE operation_id='accept-main-after-staged-gitlink'", [], |row| row.get::<_, i64>(0),
            ).unwrap(), 0);
        assert_eq!(connection.query_row(
                "SELECT COUNT(*) FROM workflow_result_acceptances WHERE result_binding_id='review-result'", [], |row| row.get::<_, i64>(0),
            ).unwrap(), 0);
    }
    let index = std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["ls-files", "--stage", "--", "module"])
        .output()
        .unwrap();
    assert!(index.status.success());
    assert!(index.stdout.starts_with(b"160000 "));
    assert_eq!(
        crate::workspace::inspect(&checkout).unwrap().head,
        repository.head
    );
    assert_eq!(
        std::fs::read(checkout.join("module/unreviewed.txt")).unwrap(),
        b"external module bytes\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("task-output.txt")).unwrap(),
        b"task bytes\n"
    );
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&checkout)
        .args(["update-index", "--force-remove", "module"])
        .status()
        .unwrap()
        .success());
    std::fs::remove_dir_all(checkout.join("module")).unwrap();
    {
        let connection = store.lock().unwrap();
        let (definition, hash) =
            crate::workflow::ensure_single_agent_definition(&connection, "2026-01-01T00:00:00Z")
                .unwrap();
        connection.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,parent_attempt_id,created_at,updated_at)
                VALUES('child','t','child-context','agent_execution',?1,1,'materialization_pending','a','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z')",params![repository.head]).unwrap();
        connection.execute("INSERT INTO workflow_run_bindings(attempt_id,family,definition_id,definition_hash,definition_state,evaluator_revision,
                execution_config_revision_id,execution_config_hash,selection_hash,scope_hash,parent_attempt_id,source_snapshot_id,created_at)
                VALUES('child','single_agent',?1,?2,'available',?3,'config','configuration','selection','scope','a',?4,'2026-01-02T00:00:00Z')",
                params![definition,hash,crate::workflow_resources::SINGLE_AGENT_EVALUATOR_REVISION,candidate_id]).unwrap();
        connection.execute("INSERT INTO rework_intents(id,operation_id,parent_attempt_id,new_attempt_id,snapshot_id,feedback,carry_plan_approval,scope_hash,configuration_hash,state,created_at,updated_at)
                VALUES('rework','rework','a','child',?1,'Improve task output',0,'scope','configuration','reserved','2026-01-02T00:00:00Z','2026-01-02T00:00:00Z')",params![candidate_id]).unwrap();
        connection.execute_batch("UPDATE attempts SET status='rework_staging' WHERE id='a'; UPDATE tasks SET lifecycle='in_progress',attention='none' WHERE id='t';").unwrap();
    }
    let app = crate::operations::Application::new(
        paths.clone(),
        store.clone(),
        std::env::current_exe().unwrap(),
    )
    .unwrap();
    assert!(app.prepare_rework("child").unwrap());
    assert!(app.prepare_rework("child").unwrap());
    assert_eq!(
        std::fs::read(checkout.join("task-output.txt")).unwrap(),
        b"task bytes\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("preexisting.txt")).unwrap(),
        b"user bytes\n"
    );
    let connection = store.lock().unwrap();
    let child_policy: String = connection
        .query_row(
            "SELECT policy_json FROM workspaces WHERE attempt_id='child'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let child_policy: serde_json::Value = serde_json::from_str(&child_policy).unwrap();
    assert_eq!(child_policy["baseline_snapshot_id"], baseline_id);
    assert_eq!(child_policy["rework_start_snapshot_id"], candidate_id);
    assert_eq!(
        child_policy["checkout_authorization"],
        frozen["checkout_authorization"]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT attempt_id FROM claims WHERE id='claim'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "child"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT path FROM workspaces WHERE attempt_id='child'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        checkout.to_str().unwrap()
    );
    assert!(!paths.artifacts.join("worktrees/child").exists());
    drop(connection);
    drop(app);
    drop(reviews);
    drop(store);
    std::fs::remove_dir_all(paths.root).unwrap();
}

const SCHEMA_38_SESSION_ROWS: &str = "SELECT group_concat(
        quote(id)||quote(role_generation_id)||quote(provider)||quote(validation_cell)||quote(status)
        ||quote(launch_config_json)||quote(executable_version)||quote(native_session_id)
        ||quote(native_identity_source)||quote(process_identity_json)||quote(transcript_epoch)
        ||quote(transcript_last_sequence)||quote(capture_state)||quote(capture_error)
        ||quote(hook_trust_state)||quote(exit_json)||quote(created_at)||quote(updated_at)
        ||quote(native_identity_verified_at)||quote(readiness_state)||quote(invocation_input_json)
        ||quote(workflow_version)||quote(workflow_hash)||quote(prompt_hash)||quote(launch_state)
        ||quote(launch_error)||quote(resume_count)||quote(capability_key)
        ||quote(capability_identity_json)||quote(recovery_root_pid)||quote(recovery_process_group_id)
        ||quote(desired_running)||quote(launch_boot_identity)||quote(recovery_anchor_json)
        ||quote(initial_hook_event_boundary_rowid)||quote(lane_id)||quote(setup_permit_id)
        ||quote(interrupt_requested_at),';')
        FROM (SELECT * FROM sessions ORDER BY id)";

/// A schema-38 database whose sessions have referencing hook, lease and
/// process rows; every value is set so the rebuild cannot drop one.
fn schema_38_session_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("llmrelay-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("state.sqlite3");
    drop(Store::open(&database).unwrap());
    Connection::open(&database)
            .unwrap()
            .execute_batch(&format!(
                "{} INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                   VALUES('p','p','/tmp/llmrelay-039-fixture','llmrelay-039-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
                   VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                   VALUES('a','t','context','implementation','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                   VALUES('g','a','implementer','codex',1,1,'running','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO sessions VALUES('s','g','codex','cell','running','{{\"launch\":1}}','1.0','native','hook',
                   '{{\"pid\":7}}','e',9,'complete','capture error','trusted','{{\"exit\":1}}','2026-01-01T00:00:00Z',
                   '2026-01-01T00:00:01Z','2026-01-01T00:00:02Z','idle_candidate','{{\"prompt\":1}}','trip-v2','workflow',
                   'prompt','started','launch error',2,'key','{{\"identity\":1}}',7,7,1,'boot','{{\"anchor\":1}}',3,'lane',
                   'permit','2026-01-01T00:00:03Z');
                 INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,payload_json,peer_pid,
                   peer_process_group_id,peer_start_marker,provenance_state,received_at)
                   VALUES('h','s','g','codex','Stop','{{}}',7,7,'start','trusted','2026-01-01T00:00:00Z');
                 INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,process_identity_json,
                   expires_at,created_at,updated_at)
                   VALUES('s','lease','human','human','g','{{\"pid\":7}}','2099-01-01T00:00:00Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO session_processes(session_id,pid,native_start_marker,process_group_id,parent_pid,last_seen_at)
                   VALUES('s',7,'start',7,1,'2026-01-01T00:00:00Z');",
                prior_service_schema_sql(38)
            ))
            .unwrap();
    (root, database)
}

#[test]
fn multi_workflow_migration_keeps_session_rows_references_and_owner_constraint() {
    let (root, database) = schema_38_session_fixture("039-upgrade");
    let scalar = |sql: &str| -> String {
        Connection::open(&database)
            .unwrap()
            .query_row(sql, [], |row| row.get::<_, rusqlite::types::Value>(0))
            .map(|value| format!("{value:?}"))
            .unwrap()
    };
    let sessions = scalar(SCHEMA_38_SESSION_ROWS);
    let references =
        "SELECT (SELECT COUNT(*) FROM hook_events h JOIN sessions s ON s.id=h.session_id)
            ||':'||(SELECT COUNT(*) FROM input_leases l JOIN sessions s ON s.id=l.session_id)
            ||':'||(SELECT COUNT(*) FROM session_processes p JOIN sessions s ON s.id=p.session_id)";
    assert_eq!(scalar(references), "Text(\"1:1:1\")");
    assert_eq!(scalar("PRAGMA user_version"), "Integer(38)");

    let store = Store::open(&database).unwrap();
    assert_eq!(
        store
            .lock()
            .unwrap()
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(store);
    assert_eq!(
        scalar("PRAGMA user_version"),
        format!("Integer({CURRENT_SCHEMA_VERSION})")
    );
    assert_eq!(scalar(SCHEMA_38_SESSION_ROWS), sessions);
    assert_eq!(scalar(references), "Text(\"1:1:1\")");
    assert_eq!(
        scalar("SELECT COUNT(*) FROM sessions WHERE provider_validation_invocation_id IS NOT NULL"),
        "Integer(0)"
    );
    assert_eq!(
        scalar("SELECT COUNT(*) FROM pragma_foreign_key_check"),
        "Integer(0)"
    );
    // Children still name `sessions`; a rename of the old parent would have retargeted them.
    assert_eq!(
            scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('hook_events','input_leases','session_processes')
                 AND sql LIKE '%REFERENCES sessions(id)%'"
            ),
            "Integer(3)"
        );
    assert_eq!(
            scalar(
                "SELECT group_concat(name,',') FROM (SELECT name FROM sqlite_master
                 WHERE tbl_name='sessions' AND type IN ('index','trigger') AND sql IS NOT NULL ORDER BY name)"
            ),
            "Text(\"sessions_interrupt_requested_deadline,sessions_launch_state_idx,\
             sessions_one_live_conversation,sessions_one_per_validation_invocation,\
             sessions_schedule_binding_insert,sessions_schedule_binding_update,sessions_schedule_occurrence,\
             state_revision_sessions_delete,\
             state_revision_sessions_insert,state_revision_sessions_update\")"
        );

    let connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let revision = || -> i64 {
        connection
            .query_row("SELECT revision FROM state_revision", [], |row| row.get(0))
            .unwrap()
    };
    let before = revision();
    connection
        .execute("UPDATE sessions SET status='exited' WHERE id='s'", [])
        .unwrap();
    assert_eq!(revision(), before + 1);
    connection
            .execute_batch(
                "INSERT INTO provider_validation_batches(id,operation_id,request_hash,preview_hash,selection_json,
                   max_fresh_calls,native_calls_authorized,state,created_at,updated_at)
                   VALUES('b','op','h','p','[]',1,1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO provider_validation_invocations(id,batch_id,ordinal,provider,model,effort,evidence_key,
                   state,created_at,updated_at)
                   VALUES('i','b',0,'codex','model','low','key','pending','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
    let insert = "INSERT INTO sessions(id,role_generation_id,provider_validation_invocation_id,provider,status,
            launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES(?1,?2,?3,'codex','launch_reserved','{}','1.0',?1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')";
    for (session, generation, invocation) in
        [("neither", None, None), ("both", Some("g"), Some("i"))]
    {
        let error = connection
            .execute(insert, params![session, generation, invocation])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("CHECK constraint failed"),
            "{session}: {error}"
        );
    }
    let error = connection
        .execute(insert, params!["unknown", None::<&str>, Some("missing")])
        .unwrap_err()
        .to_string();
    assert!(error.contains("FOREIGN KEY constraint failed"), "{error}");
    connection
        .execute(insert, params!["validation", None::<&str>, Some("i")])
        .unwrap();
    drop(connection);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn multi_workflow_migration_failure_rolls_back_and_restores_foreign_keys() {
    let (root, database) = schema_38_session_fixture("039-rollback");
    Connection::open(&database)
            .unwrap()
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,payload_json,peer_pid,
                   peer_process_group_id,peer_start_marker,provenance_state,received_at)
                   VALUES('orphan','missing-session','g','codex','Stop','{}',7,7,'start','trusted','2026-01-01T00:00:00Z');",
            )
            .unwrap();
    let schema_text = "SELECT group_concat(name||':'||COALESCE(sql,''), char(10))
             FROM (SELECT name, sql FROM sqlite_master ORDER BY name)";
    let mut connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let schema: String = connection
        .query_row(schema_text, [], |row| row.get(0))
        .unwrap();
    let sessions: String = connection
        .query_row(SCHEMA_38_SESSION_ROWS, [], |row| row.get(0))
        .unwrap();

    let error = format!("{:#}", migrate(&mut connection).unwrap_err());
    assert!(error.contains("1 foreign-key violation"), "{error}");
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1,
        "a failed rebuild must not leave enforcement off"
    );
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        38
    );
    assert_eq!(
        connection
            .query_row(schema_text, [], |row| row.get::<_, String>(0))
            .unwrap(),
        schema
    );
    assert_eq!(
        connection
            .query_row(SCHEMA_38_SESSION_ROWS, [], |row| row.get::<_, String>(0))
            .unwrap(),
        sessions
    );
    drop(connection);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_validation_sessions_share_capacity_but_never_touch_workflow_records() {
    let root = std::env::temp_dir().join(format!(
        "llmrelay-validation-owner-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("state.sqlite3")).unwrap();
    store
            .lock()
            .unwrap()
            .execute_batch(
                "INSERT INTO provider_validation_batches(id,operation_id,request_hash,preview_hash,selection_json,
                   max_fresh_calls,native_calls_authorized,state,created_at,updated_at)
                   VALUES('b','op','h','p','[]',3,1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO provider_validation_invocations(id,batch_id,ordinal,provider,model,effort,evidence_key,
                   state,created_at,updated_at) VALUES
                   ('i0','b',0,'codex','model','low','key-0','pending','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
                   ('i1','b',1,'codex','model','low','key-1','pending','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
                   ('i2','b',2,'codex','model','low','key-2','pending','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
    let session_ids = [
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    ];
    let reserve = |index: usize| {
        store.reserve_provider_validation_session(&ProviderValidationSessionReservation {
            invocation_id: &format!("i{index}"),
            session_id: &session_ids[index],
            transcript_epoch: &format!("epoch-{index}"),
            provider: crate::domain::Provider::Codex,
            executable_version: "1.0",
            launch_json: "{\"kind\":\"provider_validation\"}",
            scope_root: "/tmp/llmrelay-validation-scope",
            nonce_hash: "nonce",
        })
    };
    let state = |invocation: &str| -> String {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM provider_validation_invocations WHERE id=?1",
                params![invocation],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(reserve(0).unwrap());
    assert_eq!(
        store.session_owner(&session_ids[0]).unwrap(),
        SessionOwner::ProviderValidationInvocation("i0".into())
    );
    assert_eq!(state("i0"), "reserved");

    for refused in [
        store
            .verify_attachment_binding(&AttachmentBinding {
                session_id: session_ids[0].clone(),
                role_generation_id: uuid::Uuid::new_v4().to_string(),
                transcript_epoch: uuid::Uuid::new_v4().to_string(),
                process: crate::domain::ProcessIdentity {
                    pid: 7,
                    process_group_id: 7,
                    native_start_marker: "start".into(),
                    observed_started_at: "2026-01-01T00:00:00Z".into(),
                },
            })
            .unwrap_err(),
        store
            .acquire_input_lease_for(
                InputLeasePurpose::HumanControl,
                &session_ids[0],
                "secret",
                "human",
                "{}",
                "g",
                "2099-01-01T00:00:00Z",
            )
            .unwrap_err(),
    ] {
        assert!(
            refused
                .to_string()
                .contains("not provider validation checks"),
            "{refused:#}"
        );
    }

    store
        .mark_session_spawning(&session_ids[0], "epoch-0", "boot")
        .unwrap();
    store
        .update_session_running(&session_ids[0], "epoch-0", "{\"pid\":7}")
        .unwrap();
    assert_eq!(state("i0"), "running");
    store
        .mark_session_delivery_ambiguous(
            &session_ids[0],
            "provider root exited with live descendants",
        )
        .unwrap();
    assert_eq!(state("i0"), "delivery_unknown");

    // The uncertain call still holds one of codex's two provider slots.
    assert!(reserve(1).unwrap());
    assert!(!reserve(2).unwrap(), "both codex slots are occupied");
    assert_eq!(state("i2"), "pending");

    store
        .mark_session_spawning(&session_ids[1], "epoch-1", "boot")
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE provider_validation_batches SET state='cancel_requested' WHERE id='b'",
            [],
        )
        .unwrap();
    assert!(store
        .update_session_running(&session_ids[1], "epoch-1", "{\"pid\":8}")
        .unwrap_err()
        .to_string()
        .contains("stale"));
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE provider_validation_invocations SET state='cancel_requested' WHERE id='i1'",
            [],
        )
        .unwrap();
    store
        .update_session_launch_failed(&session_ids[1], "spawn refused before provider exec")
        .unwrap();
    assert_eq!(state("i1"), "cancelled");

    let connection = store.lock().unwrap();
    let workflow_rows: i64 = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM projects)+(SELECT COUNT(*) FROM tasks)
                   +(SELECT COUNT(*) FROM attempts)+(SELECT COUNT(*) FROM role_generations)
                   +(SELECT COUNT(*) FROM role_credentials)+(SELECT COUNT(*) FROM recovery_records)",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(workflow_rows, 0);
    drop(connection);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn setup_first_turn_stop_binds_its_stop_and_accepts_only_a_terminal_suffix() {
    const TRUSTED: &str = "managed_process_group_untrusted_payload";
    let root = std::env::temp_dir().join(format!("llmrelay-setup-stop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&root.join("state.sqlite3")).unwrap();
    store.lock().unwrap().execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','validation','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','planning','base',1,'held','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','manager','claude',1,1,'running','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
               VALUES('rs','t','manager',1,'{}','g','2026-01-01T00:00:00Z');
             INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
               VALUES('c','g','hash','[]','2026-01-01T00:00:00Z');
             INSERT INTO trip_setup_permits(id,setup_operation_id,attempt_id,fixture_project_id,fixture_repository_identity,
                 role,profile_hash,settings_revision,purpose,approved_action,state,created_at)
               VALUES('permit','setup','a','p','identity','manager','profile',1,'profile_probe',
                 'nonce_only_profile_invocation','issued','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,
                 native_session_id,process_identity_json,readiness_state,setup_permit_id,validation_cell,created_at,updated_at)
               VALUES('s','g','claude','running','{}','fixture','e','native','{\"pid\":41}','idle_candidate','permit',
                 'trip_setup_probe','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        ).unwrap();
    let hook = |event: &str, generation: &str, native: &str, provenance: &str, payload: &str| {
        let connection = store.lock().unwrap();
        connection.execute(
                "INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,native_session_id,
                   payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                 VALUES(?1,'s',?2,'claude',?3,?4,?5,42,42,'peer',?6,'2026-01-01T00:00:00Z')",
                params![uuid::Uuid::new_v4().to_string(), generation, event, native, payload, provenance],
            ).unwrap();
        connection.last_insert_rowid()
    };
    let eligible =
        || eligible_setup_retained_first_turn_stop(&store.lock().unwrap(), None).unwrap();
    // The observed Claude 2.1.289 order: Stop, then SubagentStop, then a notification.
    hook("PostToolUse", "g", "native", TRUSTED, "{}");
    let stop = hook("Stop", "g", "native", TRUSTED, "{}");
    hook("SubagentStop", "g", "native", TRUSTED, "{}");
    hook(
        "Notification",
        "g",
        "native",
        TRUSTED,
        r#"{"notification_type":"idle_prompt"}"#,
    );
    let receipt = eligible().unwrap();
    assert_eq!(receipt.stop_rowid, stop);

    for (event, generation, native, provenance, payload) in [
        (
            "Notification",
            "g",
            "native",
            TRUSTED,
            r#"{"notification_type":"permission_prompt"}"#,
        ),
        (
            "Notification",
            "g",
            "native",
            TRUSTED,
            r#"{"notification_type":"push_notification"}"#,
        ),
        (
            "Notification",
            "g",
            "native",
            TRUSTED,
            r#"{"notification_type":"agent_completed"}"#,
        ),
        ("Notification", "g", "native", TRUSTED, "{}"),
        (
            "StopFailure",
            "g",
            "native",
            TRUSTED,
            r#"{"error":"overloaded"}"#,
        ),
        ("UserPromptSubmit", "g", "native", TRUSTED, "{}"),
        ("PreToolUse", "g", "native", TRUSTED, "{}"),
        ("PermissionRequest", "g", "native", TRUSTED, "{}"),
        ("UntrustedNativeEvent", "g", "native", TRUSTED, "{}"),
        ("SubagentStop", "g", "other-native", TRUSTED, "{}"),
        ("SubagentStop", "other-generation", "native", TRUSTED, "{}"),
        ("SubagentStop", "g", "native", "unmanaged", "{}"),
    ] {
        let late = hook(event, generation, native, provenance, payload);
        assert_eq!(eligible(), None, "{event} {native} {provenance} {payload}");
        // The receipt selected before this hook arrived is stale at claim time.
        assert_eq!(
            store
                .claim_setup_retained_first_turn_stop(&receipt)
                .unwrap(),
            ManagerServiceStopClaim::Stale,
            "{event} {payload}"
        );
        store
            .lock()
            .unwrap()
            .execute("DELETE FROM hook_events WHERE rowid=?1", params![late])
            .unwrap();
    }
    assert_eq!(
        store
            .claim_setup_retained_first_turn_stop(&receipt)
            .unwrap(),
        ManagerServiceStopClaim::Claimed
    );
    store.lock().unwrap().execute_batch(
            "UPDATE sessions SET status='exited',exit_json='{\"process_group_quiescent\":true}' WHERE id='s';
             UPDATE role_generations SET status='exited' WHERE id='g';",
        ).unwrap();
    let authority = || {
        has_completed_setup_first_turn_stop_authority(
            &store.lock().unwrap(),
            "s",
            "g",
            Some("permit"),
        )
        .unwrap()
    };
    assert!(authority());
    // Ingestion refuses the revoked credential; a row written anyway still
    // withdraws the authority under the same suffix rule.
    hook(
        "Notification",
        "g",
        "native",
        TRUSTED,
        r#"{"notification_type":"agent_needs_input"}"#,
    );
    assert!(!authority());
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

fn wakes_registered_waiter(store: &Store, write: impl FnOnce()) -> bool {
    use std::future::Future;
    let changed = store.state_changes().notified();
    let mut changed = std::pin::pin!(changed);
    changed.as_mut().enable();
    write();
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    changed.as_mut().poll(&mut context).is_ready()
}

#[test]
fn store_guard_wakes_waiters_only_after_a_committed_revision_advance() {
    let root = std::env::temp_dir().join(format!("llmrelay-state-wake-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let database = root.join("state.sqlite3");
    let store = Store::open(&database).unwrap();
    let writer = store.clone();
    let insert_project = |id: &str| {
        format!(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                 VALUES('{id}','{id}','/tmp/{id}','{id}','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
            )
    };
    let before = store.state_revision().unwrap();

    assert!(!wakes_registered_waiter(&store, || {
        writer
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM projects", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
    }));
    assert!(!wakes_registered_waiter(&store, || {
        writer.lock().unwrap().execute(
                "INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
                 VALUES('unprojected','actor','kind','hash','{}','2026-01-01T00:00:00Z')",
                [],
            ).unwrap();
    }));
    assert!(!wakes_registered_waiter(&store, || {
        let mut connection = writer.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute_batch(&insert_project("rolled-back"))
            .unwrap();
    }));
    assert!(!wakes_registered_waiter(&store, || {
        writer
            .lock()
            .unwrap()
            .execute_batch(&format!("BEGIN;{}", insert_project("open-transaction")))
            .unwrap();
    }));
    writer.lock().unwrap().execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.state_revision().unwrap(), before);

    assert!(wakes_registered_waiter(&store, || {
        writer
            .lock()
            .unwrap()
            .execute_batch(&insert_project("committed"))
            .unwrap();
    }));
    // Mirrors the state wait: register, read the cursor, then a commit lands.
    assert!(wakes_registered_waiter(&store, || {
        assert_eq!(writer.state_revision().unwrap(), before + 1);
        writer
            .lock()
            .unwrap()
            .execute_batch(&insert_project("after-read"))
            .unwrap();
    }));

    let readonly = Store::open_current_readonly(&database).unwrap();
    assert!(!wakes_registered_waiter(&readonly, || {
        readonly
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM projects", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
    }));
    assert_eq!(readonly.state_revision().unwrap(), before + 2);

    writer
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE state_revision")
        .unwrap();
    assert!(!wakes_registered_waiter(&store, || {
        writer
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM operation_receipts WHERE operation_id='unprojected'",
                [],
            )
            .unwrap();
    }));
    assert_eq!(
        writer
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM operation_receipts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "an unobservable revision must not fail or panic the committed write"
    );
    drop((store, writer, readonly));
    std::fs::remove_dir_all(root).unwrap();
}
