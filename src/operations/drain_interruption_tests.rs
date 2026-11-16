use super::*;

#[test]
fn interrupted_drain_preserves_capture_without_signalling_or_relaunching() {
    let root = std::env::temp_dir().join(format!("llmrelay-drain-{}", uuid::Uuid::new_v4()));
    let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
    paths.create().unwrap();
    let store = Store::open(&paths.database).unwrap();
    store.lock().unwrap().execute_batch(
            "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-drain-fixture','drain-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
               VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
               VALUES('a','t','context','implementation','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
               VALUES('g','a','manager','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
               VALUES('settings','t','manager',1,'{}','g','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,desired_running,created_at,updated_at)
               VALUES('s','g','codex','running','{}','fixture','epoch',0,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
    let app = Application::new(paths.clone(), store, std::env::current_exe().unwrap()).unwrap();
    TEST_INTERRUPT_DRAIN_AFTER_CAPTURE.with(|armed| armed.set(true));
    let error = app.begin_drain().unwrap_err();
    assert!(error
        .to_string()
        .contains("injected interruption after desired-running capture: 1"));
    assert!(!app.dispatch_enabled());
    assert!(app.draining.load(Ordering::SeqCst));
    let connection = app.store.lock().unwrap();
    let captured: i64 = connection
        .query_row(
            "SELECT desired_running FROM sessions WHERE id='s'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(captured, 1);
    let capture_events: i64 = connection.query_row("SELECT COUNT(*) FROM audit_events WHERE event_code='restart.desired_running.captured' AND json_extract(detail_json,'$.sessions[0]')='s'", [], |row| row.get(0)).unwrap();
    assert_eq!(capture_events, 1);
    let interrupt_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE event_code LIKE '%interrupt%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(interrupt_events, 0);
    drop(connection);
    let _ = app.begin_drain();
    assert_eq!(app.store.lock().unwrap().query_row("SELECT COUNT(*) FROM audit_events WHERE event_code='restart.desired_running.captured'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    crate::checks::assert_partial_drain_interrupt_progress();
    drop(app);

    let reopened = Store::open_current_writable(&paths.database).unwrap();
    let candidates = crate::recovery::prepare_restart_candidates(&reopened).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["source"], "planned_shutdown");
    assert_eq!(candidates[0]["state"], "skipped");
    let reason: String = reopened
        .lock()
        .unwrap()
        .query_row(
            "SELECT reason FROM restart_candidates WHERE session_id='s'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reason, "same-generation native history is unavailable");
    let status: String = reopened
        .lock()
        .unwrap()
        .query_row("SELECT status FROM sessions WHERE id='s'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "running");
    let quiescent: i64 = reopened.lock().unwrap().query_row(
            "SELECT COALESCE(json_extract(exit_json,'$.process_group_quiescent'),0) FROM sessions WHERE id='s'",
            [], |row| row.get(0),
        ).unwrap();
    assert_eq!(quiescent, 0);
    drop(reopened);
    let check_root = root.join("held-check");
    let (store, checks) = crate::checks::seeded_drain_service(&check_root, "held-check");
    let paths = InstancePaths::resolve(Some(check_root.clone())).unwrap();
    paths.create().unwrap();
    let mut held = Application::new(paths, store, std::env::current_exe().unwrap()).unwrap();
    held.checks = checks.clone();
    let coordinator = held.coordinator_lock.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _guard = coordinator.lock().unwrap();
        let result = crate::checks::run_interruptible_drain_check(checks, check_root, ready_tx);
        done_tx.send(result).unwrap();
        release_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(held.checks.active_status().unwrap().len(), 1);
    let waiting = held.begin_drain().unwrap_err();
    assert!(waiting
        .to_string()
        .contains("waiting for the current coordinator operation"));
    assert!(!held.dispatch_enabled());
    assert!(held.drain_progress.lock().unwrap().captured.is_none());
    let receipts = held.drain_progress.lock().unwrap().checks.snapshot();
    assert!(receipts.iter().any(|value| {
        value["check_id"] == "held-check" && value["outcome"]["state"] == "issued"
    }));
    let completed = done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(completed["timed_out"], false);
    assert_eq!(completed["recovery_required"], false);
    assert!(completed["exit_code"].is_null());
    held.begin_drain().unwrap();
    assert_eq!(held.drain_status().unwrap()["quiescent"], true);
    let poisoned = held.coordinator_lock.clone();
    assert!(std::thread::spawn(move || {
        let _guard = poisoned.lock().unwrap();
        panic!("injected coordinator poison after owned check exit");
    })
    .join()
    .is_err());
    assert!(held.coordinator_tick().is_err());
    held.begin_drain().unwrap();
    assert_eq!(held.store.lock().unwrap().query_row("SELECT COUNT(*) FROM audit_events WHERE event_code='restart.desired_running.captured'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    drop(held);
    std::fs::remove_dir_all(root).unwrap();
}
