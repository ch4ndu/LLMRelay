use super::*;
use chrono::DateTime;

fn time(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn schedule_runtime_uses_original_elapsed_allowance_and_bounded_grace() {
    let now = time("2030-01-01T09:00:00Z");
    let deadline = now + ChronoDuration::seconds(120);
    let clock = ScheduleClock {
        origin: std::time::Instant::now(),
        allowance: std::time::Duration::from_secs(120),
    };
    assert_eq!(
        schedule_timing(Some(&clock), now, deadline, None, None),
        (false, false, false)
    );
    assert_eq!(
        schedule_timing(
            Some(&clock),
            deadline - ChronoDuration::seconds(30),
            deadline,
            None,
            None
        ),
        (true, false, false)
    );
    assert_eq!(
        schedule_timing(Some(&clock), deadline, deadline, None, None),
        (true, true, false)
    );
    let stopped = now - ChronoDuration::seconds(30);
    assert_eq!(
        schedule_timing(Some(&clock), now, deadline, None, Some(stopped)),
        (true, true, false)
    );
    // A later resumed turn cannot replace the occurrence's elapsed allowance with its own start.
    let original = ScheduleClock {
        origin: std::time::Instant::now() - std::time::Duration::from_secs(121),
        allowance: std::time::Duration::from_secs(120),
    };
    assert_eq!(
        schedule_timing(Some(&original), now, deadline, None, None),
        (true, true, false)
    );
}

#[test]
fn schedule_restart_and_wall_rollback_do_not_grant_a_new_clock() {
    let now = time("2030-01-01T09:00:00Z");
    let deadline = now + ChronoDuration::seconds(120);
    assert_eq!(
        schedule_timing(None, now, deadline, None, None),
        (true, true, false)
    );
    let clock = ScheduleClock {
        origin: std::time::Instant::now(),
        allowance: std::time::Duration::from_secs(120),
    };
    assert_eq!(
        schedule_timing(
            Some(&clock),
            now,
            deadline,
            Some(now + ChronoDuration::seconds(1)),
            None
        ),
        (true, true, true)
    );
}

#[test]
fn application_rechecks_drain_and_hold_after_discovery_and_before_each_fire() {
    let root = std::env::temp_dir().join(format!("llmrelay-intake-gate-{}", uuid::Uuid::new_v4()));
    let paths = InstancePaths::resolve(Some(root)).unwrap();
    paths.create().unwrap();
    let store = Store::open(&paths.database).unwrap();
    store.lock().unwrap().execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
               VALUES('p','p','/tmp/llmrelay-intake','intake','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO recipe_schedules(id,project_id,name,recipe_revision_id,cadence,anchor_utc,next_fire_utc,paused,version,created_at,updated_at)
               VALUES('s1','p','first','revision','daily','2026-09-25T09:00:00Z','2026-09-25T09:00:00Z',0,1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
                     ('s2','p','second','revision','daily','2026-09-25T09:00:00Z','2026-09-25T09:00:00Z',0,1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
        ).unwrap();
    let app = Application::new(
        paths.clone(),
        store.clone(),
        std::env::current_exe().unwrap(),
    )
    .unwrap();
    let drain_app = app.clone();
    crate::recipes::set_test_before_fire(Some(Box::new(move |_| {
        let result = drain_app.begin_drain();
        assert_drain_started_or_inventory_denied(&drain_app, result);
    })));
    assert_eq!(
        app.scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
            .unwrap(),
        0
    );
    crate::recipes::set_test_before_fire(None);
    let counts =
        || -> (i64, i64, i64, String) {
            store.lock().unwrap().query_row(
                "SELECT (SELECT COUNT(*) FROM tasks),(SELECT COUNT(*) FROM recipe_schedule_fires),
                        (SELECT COUNT(*) FROM audit_events WHERE event_code='recipe.schedule.fire'),
                        (SELECT group_concat(next_fire_utc,',') FROM recipe_schedules ORDER BY id)",
                [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).unwrap()
        };
    assert_eq!(
        counts(),
        (0, 0, 0, "2026-09-25T09:00:00Z,2026-09-25T09:00:00Z".into())
    );

    let app = Application::new(
        paths.clone(),
        store.clone(),
        std::env::current_exe().unwrap(),
    )
    .unwrap();
    let held_store = store.clone();
    crate::recipes::set_test_before_fire(Some(Box::new(move |_| {
        held_store.lock().unwrap().execute(
                "INSERT INTO recovery_records(id,state,detail_json,created_at,updated_at)
                 VALUES('database-restore-hold','attention_required','{}','2026-09-25T09:00:00Z','2026-09-25T09:00:00Z')", []
            ).unwrap();
    })));
    assert_eq!(
        app.scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
            .unwrap(),
        0
    );
    crate::recipes::set_test_before_fire(None);
    assert_eq!(counts().0, 0);
    assert_eq!(counts().1, 0);
    assert_eq!(counts().2, 0);
    store
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM recovery_records WHERE id='database-restore-hold'",
            [],
        )
        .unwrap();

    let close_app = app.clone();
    crate::recipes::set_test_before_fire(Some(Box::new(move |index| {
        if index == 1 {
            let result = close_app.begin_drain();
            assert_drain_started_or_inventory_denied(&close_app, result);
        }
    })));
    assert_eq!(
        app.scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
            .unwrap(),
        1
    );
    crate::recipes::set_test_before_fire(None);
    let after = counts();
    assert_eq!((after.0, after.1, after.2), (0, 1, 1));
    assert_eq!(
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT next_fire_utc FROM recipe_schedules WHERE id='s2'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "2026-09-25T09:00:00Z"
    );
    let independent =
        Application::new(paths, store.clone(), std::env::current_exe().unwrap()).unwrap();
    let poison = independent.coordinator_lock.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poison.lock().unwrap();
        panic!("injected coordinator mutex failure");
    })
    .join();
    assert!(independent.coordinator_tick().is_err());
    assert_eq!(
        independent
            .scheduled_intake_tick(time("2026-09-25T09:00:00Z"), time("2026-09-26T00:00:00Z"))
            .unwrap(),
        1
    );
    assert_eq!(counts().1, 2);
}
