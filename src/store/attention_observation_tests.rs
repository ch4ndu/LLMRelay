use super::*;
use crate::supervisor::{LiveInvocation, ProcessSnapshot};

const PROCESS: &str = r#"{"pid":4242,"process_group_id":4242,"native_start_marker":"root","observed_started_at":"2026-01-01T00:00:00Z"}"#;
const CAPACITY_NATIVE: &str = "01a0fad1-a051-7941-84b4-e64979f74d26";
const CAPACITY_TURN: &str = "01a0fad1-a06e-7da3-8bfc-1a96b443ad5d";

struct ObservedSession {
    root: std::path::PathBuf,
    store: Store,
    base: chrono::DateTime<Utc>,
}

impl ObservedSession {
    fn codex_capacity() -> Self {
        use crate::domain::{CapabilityStatus, Provider};
        let mut session = Self::new("codex-capacity");
        session.root = session.root.canonicalize().unwrap();
        session.base = Utc::now();
        let launch = LaunchConfig {
            provider: Provider::Codex,
            role: RoleKind::Implementer,
            executable: "/unused/codex".into(),
            executable_version: crate::providers::codex::EXACT_CODEX_VERSION.into(),
            model: "fixture".into(),
            effort: "max".into(),
            cwd: session.root.clone(),
            argv: Vec::new(),
            environment_keys: Vec::new(),
            permission_policy: "workspace-write".into(),
            security_policy: serde_json::json!({}),
            hook_revision: crate::providers::CODEX_HOOK_REVISION.into(),
            capability_status: CapabilityStatus::Supported,
            compatibility: Some(
                crate::provider_compatibility::BundleSet::embedded()
                    .resolve(
                        Provider::Codex,
                        crate::providers::codex::EXACT_CODEX_VERSION,
                        RoleKind::Implementer,
                    )
                    .unwrap(),
            ),
        };
        {
            let connection = session.store.lock().unwrap();
            connection
                .execute(
                    "UPDATE projects SET repository_path=?1",
                    params![session.root.to_str().unwrap()],
                )
                .unwrap();
            connection
                .execute("UPDATE role_generations SET provider='codex'", [])
                .unwrap();
            connection.execute("UPDATE sessions SET provider='codex',native_session_id=NULL,launch_config_json=?1,executable_version=?2",
                    params![serde_json::to_string(&launch).unwrap(), launch.executable_version]).unwrap();
            connection.execute("INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
                    VALUES('capacity-credential','g',?1,'[\"report_hook\"]','2026-01-01T00:00:00Z')",
                    params![auth::hash_secret("capacity-token")]).unwrap();
        }
        std::fs::create_dir_all(session.root.join("sessions/day")).unwrap();
        let records = [
            serde_json::json!({"type":"session_meta","payload":{"id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,
                    "cwd":session.root,"cli_version":"0.162.1","source":"cli","originator":"codex-tui","thread_source":"user"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":CAPACITY_TURN}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":CAPACITY_TURN,
                    "error":{"codex_error_info":"server_overloaded","message":"private provider text"}}}),
        ];
        std::fs::write(
            session.root.join("sessions/day/turn.jsonl"),
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
        session.capacity_hook("SessionStart", serde_json::json!({}));
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":CAPACITY_TURN,
                "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        session
    }

    fn capacity_hook(&self, event: &str, mut payload: serde_json::Value) {
        let context = self.store.role_context("capacity-token").unwrap();
        payload["hook_event_name"] = serde_json::json!(event);
        payload["session_id"] = serde_json::json!(CAPACITY_NATIVE);
        payload["cwd"] = serde_json::json!(self.root);
        self.store
            .save_hook_event(
                &context,
                &HookEnvelope {
                    provider: context.provider,
                    payload,
                },
                &RolePeerProvenance {
                    peer_pid: 4242,
                    peer_process_group_id: 4242,
                    peer_start_marker: "peer".into(),
                    managed_root_pid: 4242,
                    managed_root_start_marker: "root".into(),
                    state: "managed_process_group_untrusted_payload".into(),
                },
            )
            .unwrap();
    }

    fn enable_report_reminders(&self) {
        {
            let connection = self.store.lock().unwrap();
            connection.execute("INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,
                    detected_json,workflow_id,package_version,upstream_source_hash,overlay_hash,updated_at)
                    VALUES('p','ready','fixture','compatible','{}',?1,?2,?3,?4,'2026-01-01T00:00:00Z')",
                    params![crate::trip::WORKFLOW_ID, crate::trip::PACKAGE_VERSION,
                        crate::trip::source_hash(), crate::trip::overlay_hash()]).unwrap();
            connection
                .execute(
                    "UPDATE attempts SET workflow_version=?1,workflow_hash=?2",
                    params![
                        crate::trip::WORKFLOW_ID,
                        crate::workflow_resources::workflow_hash()
                    ],
                )
                .unwrap();
        }
        self.execute("UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('true'));
                UPDATE attempts SET plan_hash='approved-plan',plan_approved_at='2026-01-01T00:00:00Z',legacy_migration_required=0;
                UPDATE sessions SET launch_state='started';
                UPDATE role_credentials SET permissions_json='[\"report_hook\",\"report_result\"]';");
        self.capacity_hook(
            "Stop",
            serde_json::json!({"background_tasks":[],"session_crons":[]}),
        );
    }

    fn read_capacity(
        &self,
    ) -> Vec<(
        CodexCapacityCandidate,
        crate::providers::codex::CapacityHistoryFailure,
    )> {
        self.store
            .read_codex_capacity(&self.root.join("sessions"), &live())
            .unwrap()
    }

    fn write_capacity_failure(&self, tag: &str) {
        let path = self.root.join("sessions/day/turn.jsonl");
        let records: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                if record["payload"]["type"] == "task_complete" {
                    record["payload"]["error"]["codex_error_info"] = serde_json::json!(tag);
                }
                record
            })
            .collect();
        std::fs::write(
            path,
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }

    fn write_usage(&self, turn: &str, responses: &[serde_json::Value]) {
        let mut records = vec![
            serde_json::json!({"type":"session_meta","payload":{"id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,
                    "cwd":self.root,"cli_version":"0.162.1","source":"cli","originator":"codex-tui","thread_source":"user"}}),
            serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}}),
        ];
        records.extend(responses.iter().cloned().map(|mut record| {
            let usage = record["payload"]["usage"].clone();
            let payload = record["payload"].as_object_mut().unwrap();
            payload
                .entry("turn_token_usage")
                .or_insert_with(|| usage.clone());
            payload.entry("thread_token_usage").or_insert(usage);
            record
        }));
        std::fs::write(
            self.root.join("sessions/day/turn.jsonl"),
            records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }

    fn read_usage(
        &self,
    ) -> Vec<(
        CodexUsageCandidate,
        crate::providers::codex::UsageHistoryRead,
    )> {
        self.store
            .read_codex_usage(&self.root.join("sessions"), &live())
            .unwrap()
    }

    fn record_usage(
        &self,
        observation: &(
            CodexUsageCandidate,
            crate::providers::codex::UsageHistoryRead,
        ),
    ) -> Result<()> {
        self.store.record_codex_usage(
            &self.root.join("sessions"),
            &observation.0,
            &observation.1,
            &live(),
        )
    }

    fn capacity_items(&self) -> Vec<crate::domain::AttentionItem> {
        crate::workflow::state(&self.store)
            .unwrap()
            .attention
            .into_iter()
            .filter(|item| item.id.starts_with("codex_capacity:"))
            .collect()
    }

    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "llmrelay-attention-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        store.lock().unwrap().execute_batch(&format!(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                   VALUES('p','p','/tmp/llmrelay-attention','attention-fixture','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
                   VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                   VALUES('a','t','context','implementation','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                   VALUES('g','a','implementer','claude',1,1,'running','f','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
                   VALUES('rs','t','implementer',1,'{{}}','g','2026-01-01T00:00:00Z');
                 INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,
                     native_session_id,process_identity_json,readiness_state,created_at,updated_at)
                   VALUES('s','g','claude','running','{{}}','fixture','e','native','{PROCESS}','busy',
                     '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');"
            ))
            .unwrap();
        let base = chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        Self { root, store, base }
    }

    fn at(&self, milliseconds: i64) -> chrono::DateTime<Utc> {
        self.base + chrono::Duration::milliseconds(milliseconds)
    }

    fn hook(&self, id: &str, event: &str, payload: &str, milliseconds: i64) {
        self.store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,native_session_id,
                         payload_json,peer_pid,peer_process_group_id,peer_start_marker,provenance_state,received_at)
                     VALUES(?1,'s','g','claude',?2,'native',?3,42,42,'peer','managed_process_group_untrusted_payload',?4)",
                    params![id, event, payload, self.at(milliseconds).to_rfc3339()],
                )
                .unwrap();
    }

    fn observe(&self, milliseconds: i64, processes: &ProcessSnapshot, fresh_boot: bool) {
        self.store
            .observe_attention(self.at(milliseconds), processes, fresh_boot)
            .unwrap();
    }

    fn state(&self, kind: &str) -> String {
        self.store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COALESCE((SELECT state||':'||confirmations||':'||uncertain||':'
                              ||COALESCE(resolution_reason,'-')
                            FROM attention_observations WHERE kind=?1),'none')",
                params![kind],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn revision(&self) -> i64 {
        read_state_revision(&self.store.lock().unwrap()).unwrap()
    }

    fn execute(&self, sql: &str) {
        self.store.lock().unwrap().execute_batch(sql).unwrap();
    }

    fn scalar<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .lock()
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    fn permission(&self) {
        self.execute(
            "INSERT INTO permission_requests(id,hook_invocation_nonce,connection_nonce,provider,
                project_id,task_id,attempt_id,session_id,role_generation_id,role,service_boot_id,
                native_session_id,cwd,policy_fingerprint,tool_name,input_digest,input_json,
                created_at,deadline_at,state,updated_at)
                VALUES('permission','nonce','connection','claude','p','t','a','s','g','implementer',
                'boot','native','/tmp','policy','Read','digest','{}','2026-03-01T00:00:00Z',
                '2999-01-01T00:00:00Z','pending','2026-03-01T00:00:00Z');",
        );
    }

    fn business_rows(&self) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        let connection = self.store.lock().unwrap();
        let tables = connection
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table'
                AND name NOT IN ('attention_observations','codex_usage_observations','audit_events','state_revision')
                AND name NOT LIKE 'sqlite_%' ORDER BY name",
                )
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
        tables
            .into_iter()
            .map(|table| {
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                    .unwrap();
                let columns = statement.column_count();
                let rows = statement
                    .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (table, rows)
            })
            .collect()
    }
}

impl Drop for ObservedSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn comparison_timing_atomic_boundaries_replay_conflict_and_backup() {
    let session = ObservedSession::new("comparison-boundaries");
    session.execute("UPDATE sessions SET status='launch_reserved',launch_state='spawning',process_identity_json=NULL;
            UPDATE role_generations SET status='launch_reserved';
            CREATE TEMP TRIGGER refuse_timing_start BEFORE INSERT ON workflow_execution_boundaries
            BEGIN SELECT RAISE(ABORT,'timing write refused'); END;");
    assert!(session
        .store
        .update_session_running("s", "e", PROCESS)
        .is_err());
    assert_eq!(
        session.scalar::<String>("SELECT status FROM sessions WHERE id='s'"),
        "launch_reserved"
    );
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM workflow_execution_boundaries"),
        0
    );
    session.execute("DROP TRIGGER refuse_timing_start");
    session
        .store
        .update_session_running("s", "e", PROCESS)
        .unwrap();
    let applied = read_state_revision(&session.store.lock().unwrap()).unwrap();
    {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        record_execution_boundary_in(
            &transaction,
            "s",
            "e",
            PROCESS,
            ExecutionBoundaryKind::Started,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(
        read_state_revision(&session.store.lock().unwrap()).unwrap(),
        applied
    );
    let exit = r#"{"observed_at":"2026-01-01T00:01:00Z","process_group_quiescent":true}"#;
    session.execute(
        "CREATE TEMP TRIGGER refuse_timing_end BEFORE INSERT ON workflow_execution_boundaries
            WHEN NEW.kind='ended' BEGIN SELECT RAISE(ABORT,'timing end refused'); END;",
    );
    assert!(session
        .store
        .update_session_observed_exit("s", "e", PROCESS, exit)
        .is_err());
    assert_eq!(
        session.scalar::<String>("SELECT status FROM sessions WHERE id='s'"),
        "running"
    );
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM workflow_execution_boundaries"),
        1
    );
    session.execute("DROP TRIGGER refuse_timing_end");
    assert!(session
        .store
        .update_session_observed_exit("s", "e", PROCESS, exit)
        .unwrap());
    assert!(!session
        .store
        .update_session_observed_exit("s", "e", PROCESS, exit)
        .unwrap());
    let timing = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert_eq!(
        timing.runs[0].complete_observed_interval_millis,
        Some(60_000)
    );
    assert_eq!(timing.state, crate::domain::ComparisonTimingState::Ongoing);
    assert_ne!(timing.state_revision, applied);
    let before_conflict = timing.state_revision;
    {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        for _ in 0..2 {
            record_execution_boundary_in(
                &transaction,
                "s",
                "e",
                PROCESS,
                ExecutionBoundaryKind::Ended,
                "2026-01-01T00:02:00Z",
            )
            .unwrap();
        }
        transaction.commit().unwrap();
    }
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM workflow_execution_boundary_conflicts"),
        1
    );
    assert_eq!(
        session.scalar::<String>(
            "SELECT observed_at FROM workflow_execution_boundaries WHERE kind='ended'"
        ),
        "2026-01-01T00:01:00Z"
    );
    let invalid = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert!(invalid.state_revision > before_conflict);
    assert_eq!(invalid.runs[0].complete_observed_interval_millis, None);
    assert_eq!(invalid.runs[0].missing_epochs, 1);
    let mut copied = Connection::open_in_memory().unwrap();
    {
        let connection = session.store.lock().unwrap();
        let backup = rusqlite::backup::Backup::new(&connection, &mut copied).unwrap();
        assert!(matches!(
            backup.step(-1).unwrap(),
            rusqlite::backup::StepResult::Done
        ));
    }
    assert_eq!(
        copied
            .query_row(
                "SELECT COUNT(*) FROM workflow_execution_boundaries",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        copied
            .query_row(
                "SELECT COUNT(*) FROM workflow_execution_boundary_conflicts",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        copied
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        CURRENT_SCHEMA_VERSION
    );
}

#[test]
fn comparison_timing_missing_earlier_epoch_prevents_accepted_elapsed_sample() {
    let session = ObservedSession::new("comparison-epochs");
    {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        record_execution_boundary_in(
            &transaction,
            "s",
            "e",
            PROCESS,
            ExecutionBoundaryKind::Started,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    assert!(session
        .store
        .update_session_observed_exit(
            "s",
            "e",
            PROCESS,
            r#"{"observed_at":"2026-01-01T00:01:00Z","process_group_quiescent":true}"#
        )
        .unwrap());
    session.execute(
        "UPDATE tasks SET lifecycle='done';
            UPDATE attempts SET status='done',human_acceptance_at='2026-01-01T00:02:00Z';",
    );
    let complete = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert_eq!(complete.elapsed_to_acceptance_millis, Some(120_000));
    assert!(complete.first_start_coverage_complete);
    session.execute("UPDATE sessions SET resume_count=1;
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,prior_transcript_epoch,
                launch_config_json,capability_key,capability_identity_json,state,process_identity_json,created_at,updated_at)
            SELECT 'resume','s',1,'e','earlier','{}','key','{}','exited',process_identity_json,created_at,updated_at
              FROM sessions WHERE id='s';");
    let partial = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert_eq!(
        partial.state,
        crate::domain::ComparisonTimingState::Unavailable
    );
    assert_eq!(partial.elapsed_to_acceptance_millis, None);
    assert_eq!(
        partial.human_acceptance_at.as_deref(),
        Some("2026-01-01T00:02:00Z")
    );
    assert!(!partial.first_start_coverage_complete);
    assert_eq!(partial.runs[0].complete_observed_interval_millis, None);
    assert_eq!(partial.runs[0].known_partial_interval_millis, Some(60_000));
    assert_eq!(
        (
            partial.runs[0].complete_epochs,
            partial.runs[0].missing_epochs
        ),
        (1, 1)
    );
    assert!(partial.runs[0].invocations.iter().any(|invocation| {
        invocation.transcript_epoch.as_deref() == Some("earlier")
            && invocation.start_boundary_id.is_none()
    }));
    session.execute("UPDATE resume_invocations SET prior_transcript_epoch=NULL;");
    let missing_history = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert!(missing_history.runs[0]
        .invocations
        .iter()
        .any(|invocation| {
            invocation.transcript_epoch.is_none()
                && invocation.unavailable_reason.as_deref() == Some("incomplete_invocation_history")
        }));
    session.execute("INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
            VALUES('old-run','t','old-context','implementation','base',1,'failed','2025-12-31T00:00:00Z','2025-12-31T00:00:00Z');
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
            VALUES('old-generation','old-run','implementer','claude',1,1,'exited','old-authority','2025-12-31T00:00:00Z','2025-12-31T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES('old-session','old-generation','claude','exited','{}','fixture','old-epoch','2025-12-31T00:00:00Z','2025-12-31T00:00:00Z');");
    let historical = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert_eq!(historical.runs.len(), 2);
    assert_eq!(historical.runs[0].attempt_id, "old-run");
    assert_eq!(historical.runs[0].invocations[0].session_id, "old-session");
    assert_eq!(historical.elapsed_to_acceptance_millis, None);
}

#[test]
fn comparison_timing_ongoing_absence_and_invalid_intervals_never_become_zero() {
    let session = ObservedSession::new("comparison-missing-end");
    {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        record_execution_boundary_in(
            &transaction,
            "s",
            "e",
            PROCESS,
            ExecutionBoundaryKind::Started,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        transaction.commit().unwrap();
    }
    let ongoing = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert_eq!(ongoing.runs[0].ongoing_epochs, 1);
    assert_eq!(ongoing.runs[0].known_partial_interval_millis, None);
    assert!(session.store.update_session_exit("s", "e", PROCESS,
            r#"{"observed_at":"2026-01-01T00:01:00Z","process_group_quiescent":true,"source":"absence_only"}"#).unwrap());
    assert_eq!(
        session
            .scalar::<i64>("SELECT COUNT(*) FROM workflow_execution_boundaries WHERE kind='ended'"),
        0
    );
    let absent = crate::comparisons::task_timing(&session.store, "t").unwrap();
    assert_eq!(absent.runs[0].missing_epochs, 1);
    assert_eq!(absent.runs[0].complete_observed_interval_millis, None);
    for end in ["2025-12-31T23:59:59Z", "not-a-timestamp"] {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        record_execution_boundary_in(
            &transaction,
            "s",
            "e",
            PROCESS,
            ExecutionBoundaryKind::Ended,
            end,
        )
        .unwrap();
        transaction.commit().unwrap();
        drop(connection);
        let invalid = crate::comparisons::task_timing(&session.store, "t").unwrap();
        assert_eq!(invalid.runs[0].complete_observed_interval_millis, None);
        assert_eq!(invalid.runs[0].known_partial_interval_millis, None);
    }
    let copied_path = session.root.join("timing-restore.sqlite3");
    {
        let mut copy = Connection::open(&copied_path).unwrap();
        let source = session.store.lock().unwrap();
        let backup = rusqlite::backup::Backup::new(&source, &mut copy).unwrap();
        assert!(matches!(
            backup.step(-1).unwrap(),
            rusqlite::backup::StepResult::Done
        ));
    }
    let restored = Store::open_current_readonly(&copied_path).unwrap();
    assert_eq!(
        crate::comparisons::task_timing(&restored, "t")
            .unwrap()
            .runs[0]
            .complete_observed_interval_millis,
        None
    );
    for (end, expected) in [("not-a-timestamp", None), ("2026-01-01T00:00:00Z", Some(0))] {
        let isolated = ObservedSession::new("comparison-interval-value");
        {
            let mut connection = isolated.store.lock().unwrap();
            let transaction = connection.transaction().unwrap();
            record_execution_boundary_in(
                &transaction,
                "s",
                "e",
                PROCESS,
                ExecutionBoundaryKind::Started,
                "2026-01-01T00:00:00Z",
            )
            .unwrap();
            transaction.commit().unwrap();
        }
        isolated
            .store
            .update_session_observed_exit(
                "s",
                "e",
                PROCESS,
                &serde_json::json!({"observed_at":end,"process_group_quiescent":true}).to_string(),
            )
            .unwrap();
        let value = crate::comparisons::task_timing(&isolated.store, "t").unwrap();
        assert_eq!(value.runs[0].complete_observed_interval_millis, expected);
    }
}

#[test]
fn report_reminder_requirements_follow_each_role_and_admitted_report() {
    use crate::workflow::ReportRequirement;

    for (role, phase) in [
        (RoleKind::Manager, "planning"),
        (RoleKind::Explorer, "implementation"),
        (RoleKind::Implementer, "implementation"),
        (RoleKind::PlanReviewer, "plan_review"),
        (RoleKind::CodeReviewer, "code_review"),
    ] {
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        {
            let connection = session.store.lock().unwrap();
            connection
                .execute(
                    "UPDATE role_generations SET role=?1",
                    params![role.to_string()],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE role_settings SET role=?1",
                    params![role.to_string()],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE attempts SET phase=?1,candidate_hash='candidate'",
                    params![phase],
                )
                .unwrap();
        }
        let (expected, invalidate, restore, metadata, outcome) = match role {
            RoleKind::Manager => {
                session.execute("UPDATE attempts SET plan_hash=NULL,plan_approved_at=NULL");
                (
                    ReportRequirement::ManagerPlan,
                    "UPDATE attempts SET phase='manager_handoff'",
                    "UPDATE attempts SET phase='planning'",
                    serde_json::json!({}),
                    "blocked",
                )
            }
            RoleKind::Explorer => {
                session.execute("INSERT INTO trip_explorer_decisions(id,attempt_id,stage,census_json,trigger,
                        activated,limits_json,role_generation_id,candidate_hash,created_at)
                        VALUES('decision','a','implementation','{}','complexity',1,'{}','g','candidate','2026-01-01T00:00:00Z')");
                (
                    ReportRequirement::Explorer {
                        decision_id: "decision".into(),
                    },
                    "UPDATE trip_explorer_decisions SET activated=0",
                    "UPDATE trip_explorer_decisions SET activated=1",
                    serde_json::json!({"explorer_decision_id":"decision"}),
                    "needs_input",
                )
            }
            RoleKind::Implementer => (
                ReportRequirement::ImplementerCandidate {
                    plan_hash: "approved-plan".into(),
                },
                "UPDATE attempts SET plan_approved_at=NULL",
                "UPDATE attempts SET plan_approved_at='2026-01-01T00:00:00Z'",
                serde_json::json!({}),
                "blocked",
            ),
            RoleKind::PlanReviewer | RoleKind::CodeReviewer => {
                let (kind, hash) = if role == RoleKind::PlanReviewer {
                    ("plan", "approved-plan")
                } else {
                    ("code", "candidate")
                };
                session.store.lock().unwrap().execute(
                        "INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,
                         session_id,prompt_hash,handoff_hash,delivery_state,created_at,updated_at)
                         VALUES('review','a',?1,?2,'g','s','prompt','handoff','delivered','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                        params![kind,hash],
                    ).unwrap();
                (
                    ReportRequirement::Review {
                        request_id: "review".into(),
                        kind: kind.into(),
                        candidate_hash: hash.into(),
                    },
                    "UPDATE review_requests SET session_id=NULL",
                    "UPDATE review_requests SET session_id='s'",
                    serde_json::json!({"review_request_id":"review","review_kind":kind,"candidate_hash":hash}),
                    "needs_rework",
                )
            }
            RoleKind::FinalReviewer => unreachable!(),
        };
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        assert_eq!(selected.role, role);
        assert_eq!(selected.requirement, expected);
        session.execute(invalidate);
        assert!(crate::workflow::report_reminder_binding(
            &session.store.lock().unwrap(),
            "s",
            None
        )
        .unwrap()
        .is_none());
        assert!(session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .is_none());
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),
            0
        );
        session.execute(restore);
        let reminder = session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .unwrap();
        let context = session.store.role_context("capacity-token").unwrap();
        let report = session
            .store
            .save_role_result(
                &context,
                &RoleResultReport {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    outcome: outcome.into(),
                    summary: "The assigned work needs a decision.".into(),
                    evidence: vec![],
                    metadata,
                },
            )
            .unwrap();
        assert_eq!(report["accepted"], true);
        session.execute("UPDATE role_results SET consumed_at='2999-01-01T00:00:00Z'");
        let id = reminder["guidance_id"].as_str().unwrap();
        assert!(crate::workflow::report_reminder_binding(
            &session.store.lock().unwrap(),
            "s",
            Some(id)
        )
        .unwrap()
        .is_none());
        assert_eq!(
            session
                .store
                .cancel_stale_report_reminder(id)
                .unwrap()
                .unwrap()["state"],
            "cancelled"
        );
        assert_eq!(
            session.scalar::<i64>(
                "SELECT COUNT(*) FROM guidance_messages WHERE written_at IS NOT NULL"
            ),
            0
        );
    }
}

#[test]
fn report_reminder_old_turn_invocation_and_review_reports_do_not_satisfy_new_work() {
    let session = ObservedSession::codex_capacity();
    session.enable_report_reminders();
    let context = session.store.role_context("capacity-token").unwrap();
    session
        .store
        .save_role_result(
            &context,
            &RoleResultReport {
                operation_id: "old-report".into(),
                outcome: "blocked".into(),
                summary: "An earlier turn was blocked.".into(),
                evidence: vec![],
                metadata: serde_json::json!({}),
            },
        )
        .unwrap();
    assert!(
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .is_none()
    );
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt":"new obligation"}),
    );
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    assert!(
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .is_some()
    );
    session.store.lock().unwrap().execute(
            "INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,
             capability_key,capability_identity_json,state,hook_event_boundary_rowid,created_at,updated_at)
             VALUES('resume','s',1,'resumed','{}','fixture','{}','running',(SELECT MAX(rowid) FROM hook_events),?1,?1)",
            params![Utc::now().to_rfc3339()],
        ).unwrap();
    session.execute("UPDATE sessions SET transcript_epoch='resumed',readiness_state='unknown'");
    session.capacity_hook("SessionStart", serde_json::json!({}));
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt":"resumed obligation"}),
    );
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    assert!(
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .is_some()
    );

    for (role, phase, kind, hash) in [
        ("plan_reviewer", "plan_review", "plan", "approved-plan"),
        ("code_reviewer", "code_review", "code", "candidate"),
    ] {
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        {
            let connection = session.store.lock().unwrap();
            connection
                .execute("UPDATE role_generations SET role=?1", params![role])
                .unwrap();
            connection
                .execute("UPDATE role_settings SET role=?1", params![role])
                .unwrap();
            connection
                .execute(
                    "UPDATE attempts SET phase=?1,candidate_hash='candidate'",
                    params![phase],
                )
                .unwrap();
            connection.execute(
                    "INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,
                     session_id,prompt_hash,handoff_hash,delivery_state,created_at,updated_at)
                     VALUES('old-review','a',?1,?2,'g','s','prompt','handoff','delivered','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                    params![kind,hash],
                ).unwrap();
        }
        let context = session.store.role_context("capacity-token").unwrap();
        session.store.save_role_result(&context,&RoleResultReport {
                operation_id:"old-review-report".into(),outcome:"approved".into(),summary:"Earlier review complete.".into(),
                evidence:vec![],metadata:serde_json::json!({"review_request_id":"old-review","review_kind":kind,"candidate_hash":hash}),
            }).unwrap();
        assert!(crate::workflow::report_reminder_binding(
            &session.store.lock().unwrap(),
            "s",
            None
        )
        .unwrap()
        .is_none());
        session.execute("UPDATE review_requests SET delivery_state='finished';
                INSERT INTO review_requests(id,attempt_id,review_kind,candidate_hash,role_generation_id,session_id,
                    prompt_hash,handoff_hash,delivery_state,created_at,updated_at)
                    SELECT 'current-review',attempt_id,review_kind,candidate_hash,role_generation_id,session_id,
                        prompt_hash,handoff_hash,'delivered',created_at,updated_at FROM review_requests WHERE id='old-review';");
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        assert!(
            matches!(selected.requirement,crate::workflow::ReportRequirement::Review { ref request_id,.. } if request_id=="current-review")
        );
        session.execute("UPDATE review_requests SET candidate_hash='stale-candidate'");
        assert!(session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .is_none());
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),
            0
        );
    }
}

#[test]
fn report_reminder_setup_probe_final_and_unsafe_sessions_cannot_reserve() {
    for mutation in [
            "UPDATE sessions SET setup_permit_id='setup'",
            "UPDATE attempts SET setup_operation_id='setup'",
            "UPDATE sessions SET validation_cell='trip_runtime_probe'",
            "UPDATE attempts SET status='capability_validation'",
            "UPDATE projects SET internal_purpose='capability_validation'",
            "UPDATE trip_project_state SET readiness='needs_attention'",
            "UPDATE attempts SET workflow_hash='obsolete-workflow'",
            "UPDATE role_generations SET role='final_verifier'; UPDATE role_settings SET role='final_verifier'",
            "UPDATE role_generations SET role='final_reviewer'; UPDATE role_settings SET role='final_reviewer'",
            "UPDATE role_generations SET lane_id='worker-lane'; UPDATE sessions SET lane_id='worker-lane'",
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            let selected = crate::workflow::report_reminder_binding(&session.store.lock().unwrap(),"s",None).unwrap().unwrap();
            session.execute(mutation);
            assert!(crate::workflow::report_reminder_binding(&session.store.lock().unwrap(),"s",None).unwrap().is_none(),"{mutation}");
            assert!(session.store.reserve_report_reminder(&selected).unwrap().is_none(),"{mutation}");
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),0);
        }
    for (provider, event, payload) in [
        (
            "codex",
            "UserPromptSubmit",
            serde_json::json!({"prompt":"still working"}),
        ),
        ("claude", "Stop", serde_json::json!({})),
        (
            "claude",
            "Stop",
            serde_json::json!({"background_tasks":[{"id":"active"}],"session_crons":[]}),
        ),
        (
            "codex",
            "StopFailure",
            serde_json::json!({"error":"turn failed"}),
        ),
    ] {
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        {
            let connection = session.store.lock().unwrap();
            connection
                .execute("UPDATE role_generations SET provider=?1", params![provider])
                .unwrap();
            connection
                .execute("UPDATE sessions SET provider=?1", params![provider])
                .unwrap();
        }
        session.capacity_hook("UserPromptSubmit", serde_json::json!({"prompt":"new work"}));
        session.capacity_hook(event, payload);
        assert!(
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .is_none(),
            "{event}"
        );
        assert!(session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .is_none());
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),
            0
        );
    }
}

#[test]
fn report_reminder_cap_survives_concurrent_reservation_noise_reopen_and_resume() {
    let session = ObservedSession::codex_capacity();
    session.enable_report_reminders();
    let selected =
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let first = std::thread::scope(|scope| {
        let workers = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let selected = &selected;
                let path = session.root.join("state.sqlite3");
                scope.spawn(move || {
                    let store = Store::open(&path).unwrap();
                    barrier.wait();
                    store.reserve_report_reminder(selected).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let reservations = workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(reservations.len(), 1);
        reservations.into_iter().next().unwrap()
    });
    let first_id = first["guidance_id"].as_str().unwrap();
    session.execute("UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('false')); ");
    session
        .store
        .cancel_stale_report_reminder(first_id)
        .unwrap()
        .unwrap();
    session.execute("UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('true'));");
    assert!(session
        .store
        .reserve_report_reminder(&selected)
        .unwrap()
        .is_none());
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    let same_turn =
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .unwrap();
    assert!(session
        .store
        .reserve_report_reminder(&same_turn)
        .unwrap()
        .is_none());
    {
        let connection = session.store.lock().unwrap();
        for index in 0..510 {
            connection.execute("INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                    VALUES(?1,?1,'service','fixture.noise','session','s','{}',?2)",params![format!("noise-{index}"),Utc::now().to_rfc3339()]).unwrap();
        }
        connection.execute_batch("INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,
                capability_key,capability_identity_json,state,hook_event_boundary_rowid,created_at,updated_at)
                VALUES('resume','s',1,'resumed','{}','fixture','{}','running',(SELECT MAX(rowid) FROM hook_events),'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                UPDATE sessions SET transcript_epoch='resumed',readiness_state='unknown';").unwrap();
    }
    session.capacity_hook("SessionStart", serde_json::json!({}));
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt":"report if finished"}),
    );
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    let resumed = Store::open(&session.root.join("state.sqlite3")).unwrap();
    let selected = crate::workflow::report_reminder_binding(&resumed.lock().unwrap(), "s", None)
        .unwrap()
        .unwrap();
    let second = resumed.reserve_report_reminder(&selected).unwrap().unwrap();
    assert_eq!(second["ordinal"], 2);
    session.execute("UPDATE guidance_messages SET state='delivery_unknown',reason='uncertain write' WHERE state='queued';");
    assert_eq!(
        report_reminder_reservations(&resumed.lock().unwrap(), "g")
            .unwrap()
            .len(),
        2
    );
    session
        .execute("UPDATE guidance_messages SET state='abandoned' WHERE state='delivery_unknown';");
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt":"new obligation"}),
    );
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    let latest = crate::workflow::report_reminder_binding(&resumed.lock().unwrap(), "s", None)
        .unwrap()
        .unwrap();
    assert!(resumed.reserve_report_reminder(&latest).unwrap().is_none());
    session.execute("UPDATE role_generations SET status='replaced' WHERE id='g';
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
              SELECT 'g-new',attempt_id,role,provider,2,config_revision,'running','new',created_at,updated_at FROM role_generations WHERE id='g';
            UPDATE role_settings SET effective_generation_id='g-new'; UPDATE role_credentials SET role_generation_id='g-new';
            UPDATE sessions SET role_generation_id='g-new',transcript_epoch='new-generation',initial_hook_event_boundary_rowid=(SELECT MAX(rowid) FROM hook_events);");
    session.capacity_hook("SessionStart", serde_json::json!({}));
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt":"new generation"}),
    );
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    let replacement = crate::workflow::report_reminder_binding(&resumed.lock().unwrap(), "s", None)
        .unwrap()
        .unwrap();
    assert_eq!(
        resumed
            .reserve_report_reminder(&replacement)
            .unwrap()
            .unwrap()["ordinal"],
        1
    );
}

#[test]
fn report_reminder_selection_rechecks_policy_report_turn_and_permission() {
    for mutation in [
            "UPDATE projects SET settings_json=json_set(settings_json,'$.role_report_reminders',json('false'))",
            "INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at)
             VALUES('report','report','s','g','blocked','blocked','[]','{}','2999-01-01T00:00:00Z')",
            "UPDATE tasks SET attention='paused'",
            "UPDATE role_settings SET effective_generation_id=NULL",
        ] {
            let session = ObservedSession::codex_capacity();
            session.enable_report_reminders();
            let selected = crate::workflow::report_reminder_binding(&session.store.lock().unwrap(),"s",None).unwrap().unwrap();
            session.execute(mutation);
            assert!(session.store.reserve_report_reminder(&selected).unwrap().is_none(),"{mutation}");
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM guidance_messages"),0);
        }
    let session = ObservedSession::codex_capacity();
    session.enable_report_reminders();
    let selected =
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .unwrap();
    session.capacity_hook("UserPromptSubmit", serde_json::json!({"prompt":"new turn"}));
    assert!(session
        .store
        .reserve_report_reminder(&selected)
        .unwrap()
        .is_none());
    session.capacity_hook(
        "Stop",
        serde_json::json!({"background_tasks":[],"session_crons":[]}),
    );
    let selected =
        crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
            .unwrap()
            .unwrap();
    session.permission();
    assert!(session
        .store
        .reserve_report_reminder(&selected)
        .unwrap()
        .is_none());
}

#[test]
fn report_reminder_cancellation_never_overwrites_a_new_turn_or_invocation() {
    for changed in ["turn", "invocation", "uncertain"] {
        let session = ObservedSession::codex_capacity();
        session.enable_report_reminders();
        let selected =
            crate::workflow::report_reminder_binding(&session.store.lock().unwrap(), "s", None)
                .unwrap()
                .unwrap();
        let reminder = session
            .store
            .reserve_report_reminder(&selected)
            .unwrap()
            .unwrap();
        let id = reminder["guidance_id"].as_str().unwrap();
        assert!(session
            .store
            .reserve_guidance_delivery(id, "s", "e", None, REPORT_REMINDER_PROMPT, 1)
            .map(|outcome| outcome == GuidanceReservation::Reserved)
            .unwrap());
        match changed {
            "turn" => {
                session.capacity_hook("UserPromptSubmit", serde_json::json!({"prompt":"later"}));
                session.capacity_hook(
                    "Stop",
                    serde_json::json!({"background_tasks":[],"session_crons":[]}),
                );
            }
            "invocation" => session.execute(
                "UPDATE sessions SET transcript_epoch='replaced',readiness_state='unknown'",
            ),
            "uncertain" => session.execute("UPDATE guidance_messages SET state='delivery_unknown'"),
            _ => unreachable!(),
        }
        let before: String = session.scalar("SELECT readiness_state FROM sessions WHERE id='s'");
        let cancellation = session
            .store
            .cancel_report_reminder_before_write(id, "stale reservation");
        if changed == "uncertain" {
            assert!(cancellation.is_err());
        } else {
            assert_eq!(cancellation.unwrap()["readiness_restored"], false);
        }
        assert_eq!(
            session.scalar::<String>("SELECT readiness_state FROM sessions WHERE id='s'"),
            before
        );
        assert_eq!(
            report_reminder_reservations(&session.store.lock().unwrap(), "g")
                .unwrap()
                .len(),
            1
        );
    }
}

fn live() -> ProcessSnapshot {
    ProcessSnapshot::Read(vec![LiveInvocation {
        session_id: "s".into(),
        owner: SessionOwner::RoleGeneration("g".into()),
        transcript_epoch: "e".into(),
        process_identity_json: PROCESS.into(),
    }])
}

#[test]
fn codex_usage_deduplicates_committed_records_and_preserves_authority() {
    use crate::domain::ObservedUsageStatus;
    let session = ObservedSession::codex_capacity();
    let response = |id: &str, count| {
        serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":id,"usage":{"input_tokens":count,
                "cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":count},
            "turn_token_usage":{"input_tokens":999,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":999},
            "thread_token_usage":{"input_tokens":9999,"cached_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":9999}}})
    };
    let mut measured = response("a", 15);
    measured["payload"]["usage"]["cache_write_input_tokens"] = serde_json::json!(3);
    let records = [measured, response("b", 15), response("zero", 0)];
    session.write_usage(CAPACITY_TURN, &records);
    session.capacity_hook("Stop", serde_json::json!({"turn_id":CAPACITY_TURN}));
    let observations = session.read_usage();
    assert_eq!(
        observations.len(),
        1,
        "matching Stop must not suppress observed usage"
    );
    let before = session.business_rows();
    let revision = session.revision();
    session.record_usage(&observations[0]).unwrap();
    assert_eq!(session.business_rows(), before);
    assert_eq!(session.revision(), revision + 3);
    assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations
                WHERE source_version='0.162.1' AND contract_revision='codex-0.162.1-response-usage-v1'"),
            3
        );
    let state = crate::workflow::state(&session.store).unwrap();
    let task = state.tasks[0].observed_usage.as_ref().unwrap();
    assert_eq!(task.status, ObservedUsageStatus::Observed);
    assert_eq!(task.observed_responses, Some(3));
    assert_eq!(task.counters.as_ref().unwrap().total_tokens, 30);
    assert_eq!(
        task.counters.as_ref().unwrap().cache_write_input_tokens,
        None
    );
    assert_eq!(
        state.active_sessions[0]["observed_usage"]["current_turn"]["counters"]["total_tokens"],
        30
    );
    assert!(
        session.read_usage().is_empty(),
        "unchanged source must skip parsing"
    );
    session.record_usage(&observations[0]).unwrap();
    assert_eq!(session.revision(), revision + 3);
    let mut changed = records.to_vec();
    changed.push(records[2].clone());
    session.write_usage(CAPACITY_TURN, &changed);
    assert_eq!(
        session.read_usage().len(),
        1,
        "changed identity must be read again"
    );
    let restarted = Store::open(&session.root.join("state.sqlite3")).unwrap();
    let reread = restarted
        .read_codex_usage(&session.root.join("sessions"), &live())
        .unwrap();
    restarted
        .record_codex_usage(
            &session.root.join("sessions"),
            &reread[0].0,
            &reread[0].1,
            &live(),
        )
        .unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        3
    );
    assert_eq!(
        crate::workflow::state(&restarted).unwrap().tasks[0]
            .observed_usage
            .as_ref()
            .unwrap()
            .counters
            .as_ref()
            .unwrap()
            .total_tokens,
        30
    );
    session.execute("UPDATE sessions SET transcript_epoch='next';
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,launch_config_json,
                capability_key,capability_identity_json,state,created_at,updated_at,hook_event_boundary_rowid)
            VALUES('resume','s',1,'next','{}','fixture','{}','running','2026-01-01T00:00:00Z',
                '2026-01-01T00:00:00Z',(SELECT MAX(rowid) FROM hook_events));");
    session.capacity_hook("SessionStart", serde_json::json!({}));
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"turn_id":"next-turn",
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
    );
    let mut next = response("next-response", 7);
    next["payload"]["turn_id"] = serde_json::json!("next-turn");
    next["payload"]["root_turn_id"] = serde_json::json!("next-turn");
    session.write_usage("next-turn", &[records[0].clone(), next]);
    let processes = ProcessSnapshot::Read(vec![LiveInvocation {
        session_id: "s".into(),
        owner: SessionOwner::RoleGeneration("g".into()),
        transcript_epoch: "next".into(),
        process_identity_json: PROCESS.into(),
    }]);
    let observed = session
        .store
        .read_codex_usage(&session.root.join("sessions"), &processes)
        .unwrap();
    assert_eq!(observed.len(), 1);
    session
        .store
        .record_codex_usage(
            &session.root.join("sessions"),
            &observed[0].0,
            &observed[0].1,
            &processes,
        )
        .unwrap();
    let state = crate::workflow::state(&session.store).unwrap();
    assert_eq!(
        state.active_sessions[0]["observed_usage"]["current_turn"]["counters"]["total_tokens"],
        7
    );
    assert_eq!(
        state.active_sessions[0]["observed_usage"]["session"]["counters"]["total_tokens"],
        37
    );
    assert_eq!(
        session.scalar::<String>(
            "SELECT transcript_epoch FROM codex_usage_observations WHERE response_id='a'"
        ),
        "e"
    );
}

#[test]
fn codex_usage_revalidation_and_transaction_failure_never_advance_cache() {
    let session = ObservedSession::codex_capacity();
    session.write_usage(CAPACITY_TURN, &[]);
    let empty = session.read_usage();
    session.record_usage(&empty[0]).unwrap();
    assert!(
        session.read_usage().is_empty(),
        "valid empty read can be cached"
    );
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"turn_id":CAPACITY_TURN,
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
    );
    assert_eq!(
        session.read_usage().len(),
        1,
        "new accepted binding requires a fresh read"
    );
    let record = serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"one","usage":{"input_tokens":0,
                "cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":0}}});
    session.write_usage(CAPACITY_TURN, &[record.clone()]);
    let observed = session.read_usage();
    session.execute(
        "CREATE TRIGGER fail_usage BEFORE INSERT ON codex_usage_observations
            BEGIN SELECT RAISE(ABORT,'usage persistence fixture failure'); END;",
    );
    let before = session.business_rows();
    let revision = session.revision();
    let error = session.record_usage(&observed[0]).unwrap_err();
    assert!(error
        .to_string()
        .contains("usage persistence fixture failure"));
    assert_eq!(session.business_rows(), before);
    assert_eq!(session.revision(), revision);
    assert_eq!(
        session.read_usage().len(),
        1,
        "failed persistence cannot cache the new identity"
    );
    session.execute("DROP TRIGGER fail_usage;");
    session.write_usage(CAPACITY_TURN, &[]);
    session.record_usage(&observed[0]).unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        0,
        "changed file identity after reading must discard the source"
    );
    session.write_usage(CAPACITY_TURN, &[record.clone()]);
    let fresh = session.read_usage();
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"turn_id":"later-turn",
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
    );
    session.record_usage(&fresh[0]).unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        0
    );
    for turn in ["x".repeat(129), "invalid\u{0085}turn".into()] {
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":turn,
                "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        assert!(session.read_usage().is_empty());
    }
    let session = ObservedSession::codex_capacity();
    let cwd = session.root.join("working");
    std::fs::create_dir(&cwd).unwrap();
    {
        let connection = session.store.lock().unwrap();
        connection
            .execute(
                "UPDATE projects SET repository_path=?1",
                params![cwd.to_str().unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.cwd',?1)",
                params![cwd.to_str().unwrap()],
            )
            .unwrap();
        connection.execute("UPDATE hook_events SET payload_json=json_set(payload_json,'$.cwd',?1) WHERE event_name='UserPromptSubmit'",
                params![cwd.to_str().unwrap()]).unwrap();
    }
    session.write_usage(CAPACITY_TURN, &[record]);
    let path = session.root.join("sessions/day/turn.jsonl");
    let mut records: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    records[0]["payload"]["cwd"] = serde_json::json!(cwd);
    std::fs::write(
        &path,
        records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let original = session.read_usage();
    assert_eq!(original.len(), 1);
    std::fs::rename(&cwd, session.root.join("previous-working")).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    session.record_usage(&original[0]).unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        0
    );
    let replacement = session.read_usage();
    assert_eq!(original[0].0.canonical_cwd, replacement[0].0.canonical_cwd);
    assert_ne!(original[0].0.cwd_identity, replacement[0].0.cwd_identity);
}

#[test]
fn stage_b_usage_conflicts_reach_visible_alternates_and_keep_zero_distinct() -> Result<()> {
    use crate::domain::{ObservedUsageStatus, UsageComparisonQuery as Query, UsageGroupBy};
    let session = ObservedSession::codex_capacity();
    let record = serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"zero","usage":{"input_tokens":0,
                "cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":0}}});
    session.write_usage(CAPACITY_TURN, &[record]);
    session.record_usage(&session.read_usage()[0])?;
    session.store.lock()?.execute(
        "UPDATE attempts SET workflow_version=?1",
        params![crate::trip::WORKFLOW_ID],
    )?;
    session.execute("INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
            SELECT 'p-alt','Alternate','/tmp/alternate','alternate',base_revision,created_at,updated_at FROM projects;
            INSERT INTO tasks(id,project_id,title,lifecycle,archived_at,created_at,updated_at)
            SELECT 't-alt','p-alt','Archived','done',created_at,created_at,updated_at FROM tasks;
            INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,workflow_version,created_at,updated_at)
            SELECT 'a-alt','t-alt','alternate',phase,base_revision,configuration_revision,'completed',workflow_version,created_at,updated_at FROM attempts;
            INSERT INTO workflow_run_bindings(attempt_id,family,definition_state,evaluator_revision,selection_hash,scope_hash,created_at)
            VALUES('a-alt','single_agent','unavailable',1,'selection','scope','2026-01-01');
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
            SELECT 'g-alt','a-alt','code_reviewer','codex',1,1,'exited','alt',created_at,updated_at FROM role_generations;
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES('s-alt','g-alt','codex','exited','{}','fixture','alt','2026-02-01T00:00:00Z','2026-02-01'),
                  ('s-missing','g-alt','claude','exited','{}','fixture','missing','2026-02-01T23:59:59Z','2026-02-01');");
    let compare = |query| crate::workflow::usage_comparisons(&session.store, query, "boot");
    let before = compare(Query::default())?;
    let zero = &before.groups[0];
    assert_eq!(zero.key, "p");
    assert_eq!(zero.usage.status, ObservedUsageStatus::Observed);
    let counters = zero.usage.counters.as_ref().unwrap();
    assert_eq!(counters.total_tokens, 0);
    assert_eq!(counters.cache_write_input_tokens, None);
    let missing = &before.groups[1];
    assert_eq!(missing.key, "p-alt");
    assert_eq!(missing.usage.status, ObservedUsageStatus::Unavailable);
    assert!(missing.usage.counters.is_none());
    assert_eq!(missing.sessions_without_observations, 2);
    let families = compare(Query {
        group_by: UsageGroupBy::Workflow,
        ..Query::default()
    })?;
    assert!(families.groups.iter().any(|group| group.key == "trip"));
    assert!(families
        .groups
        .iter()
        .any(|group| group.key == "single_agent"));
    for (identity, expected) in [
        (crate::trip::WORKFLOW_ID, "trip"),
        ("trip-explorer-0.11.0-llmrelay-1", "trip"),
        ("trip-explorer-0.9.0-llmrelay-1", "trip"),
        ("trip-v1", "trip"),
        ("trip-explorer-0.99.0-llmrelay-1", "unknown"),
        ("", "unknown"),
    ] {
        session
            .store
            .lock()?
            .execute("UPDATE attempts SET workflow_version=?1", params![identity])?;
        let page = compare(Query {
            group_by: UsageGroupBy::Workflow,
            workflow: Some(expected.into()),
            ..Query::default()
        })?;
        assert_eq!(page.groups.len(), 1);
        assert_eq!(page.groups[0].key, expected);
        assert_eq!(page.groups[0].sessions, 1);
    }
    session.execute(r#"UPDATE projects SET internal_purpose='trip_setup_fixture' WHERE id='p';
            UPDATE codex_usage_observations SET invalid=1,conflict_owners_json=
            '[{"task_id":"t-alt","session_id":"s-alt"},{"task_id":"t-alt","session_id":"s-alt"}]';"#);
    let query = Query {
        group_by: UsageGroupBy::Role,
        project_id: Some("p-alt".into()),
        workflow: Some("single_agent".into()),
        role: Some("code_reviewer".into()),
        start_date: Some("2026-02-01".into()),
        end_date: Some("2026-02-01".into()),
        ..Query::default()
    };
    let revision = session.revision();
    let page = compare(query.clone())?;
    assert_eq!(page.groups.len(), 1);
    let group = &page.groups[0];
    assert_eq!(group.key, "code_reviewer");
    assert_eq!(group.usage.status, ObservedUsageStatus::Invalid);
    assert!(group.usage.counters.is_none());
    assert_eq!(group.usage.observed_responses, Some(1));
    assert_eq!(group.sessions_with_observations, 1);
    assert_eq!(group.sessions_without_observations, 1);
    assert_eq!(group.claude_sessions, 1);
    assert_eq!(session.revision(), revision);
    session.execute("UPDATE codex_usage_observations SET conflict_owners_json='[{}]'");
    let unresolved = compare(query).unwrap_err().to_string();
    assert!(unresolved.contains("cannot be linked"));
    Ok(())
}

#[test]
fn codex_usage_conflicts_keep_original_provenance_and_invalidate_both_turn_owners() {
    use crate::domain::ObservedUsageStatus;
    let session = ObservedSession::codex_capacity();
    let mut record = serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":"conflict","usage":{"input_tokens":10,
                "cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":10}}});
    session.write_usage(CAPACITY_TURN, &[record.clone()]);
    let original = session.read_usage();
    session.record_usage(&original[0]).unwrap();
    let provenance: String = session.scalar("SELECT accepted_hook_event_id||':'||transcript_epoch||':'||input_tokens FROM codex_usage_observations");
    record["payload"]["usage"]["input_tokens"] = serde_json::json!(200);
    record["payload"]["usage"]["total_tokens"] = serde_json::json!(200);
    session.write_usage(CAPACITY_TURN, &[record]);
    let conflict = session.read_usage();
    session.record_usage(&conflict[0]).unwrap();
    let revision = session.revision();
    session.record_usage(&conflict[0]).unwrap();
    assert_eq!(
        session.revision(),
        revision,
        "identical conflict replay is idempotent"
    );
    session.capacity_hook(
        "UserPromptSubmit",
        serde_json::json!({"turn_id":CAPACITY_TURN,
            "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
    );
    let reassignment = session.read_usage();
    session.record_usage(&reassignment[0]).unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        1
    );
    assert_eq!(session.scalar::<String>("SELECT accepted_hook_event_id||':'||transcript_epoch||':'||input_tokens FROM codex_usage_observations"), provenance);
    let reopened = Store::open(&session.root.join("state.sqlite3")).unwrap();
    let state = crate::workflow::state(&reopened).unwrap();
    let task = state.tasks[0].observed_usage.as_ref().unwrap();
    assert_eq!(task.status, ObservedUsageStatus::Invalid);
    assert!(task.counters.is_none());
    assert_eq!(
        state.active_sessions[0]["observed_usage"]["current_turn"]["status"],
        "invalid"
    );
    assert!(state.active_sessions[0]["observed_usage"]["current_turn"]["counters"].is_null());
    assert_eq!(
        session.scalar::<i64>(
            "SELECT json_array_length(conflict_owners_json) FROM codex_usage_observations"
        ),
        2
    );
}

#[test]
fn codex_usage_task_projection_survives_session_window_and_rejects_overflow() {
    use crate::domain::ObservedUsageStatus;
    let session = ObservedSession::codex_capacity();
    let response = |id: &str| {
        serde_json::json!({"type":"token_usage_record","payload":{
            "thread_id":CAPACITY_NATIVE,"session_id":CAPACITY_NATIVE,"turn_id":CAPACITY_TURN,
            "root_turn_id":CAPACITY_TURN,"response_id":id,"usage":{"input_tokens":15,
                "cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,
                "reasoning_output_tokens":0,"total_tokens":15}}})
    };
    session.write_usage(CAPACITY_TURN, &[response("one"), response("two")]);
    let observations = session.read_usage();
    session.execute("UPDATE sessions SET status='exited',exit_json='{}'");
    session.record_usage(&observations[0]).unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        0,
        "exit after reading retires the binding"
    );
    session.execute("UPDATE sessions SET status='running',exit_json=NULL");
    session
        .store
        .record_codex_usage(
            &session.root.join("sessions"),
            &observations[0].0,
            &observations[0].1,
            &ProcessSnapshot::Unavailable,
        )
        .unwrap();
    assert_eq!(
        session.scalar::<i64>("SELECT COUNT(*) FROM codex_usage_observations"),
        0
    );
    session.record_usage(&observations[0]).unwrap();
    session.execute("UPDATE sessions SET status='exited',exit_json='{}'");
    for index in 0..201 {
        session.store.lock().unwrap().execute("INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,
                executable_version,transcript_epoch,created_at,updated_at)
                VALUES(?1,'g','codex','exited','{}','fixture',?1,'2026-02-01T00:00:00Z','2026-02-01T00:00:00Z')",
                params![format!("newer-{index}")]).unwrap();
    }
    let state = crate::workflow::state(&session.store).unwrap();
    assert!(!state.active_sessions.iter().any(|row| row["id"] == "s"));
    assert_eq!(
        state.tasks[0]
            .observed_usage
            .as_ref()
            .unwrap()
            .counters
            .as_ref()
            .unwrap()
            .total_tokens,
        30
    );
    assert!(session.read_usage().is_empty());
    assert!(session.store.codex_usage_cache.lock().unwrap().is_empty());
    session.execute("UPDATE codex_usage_observations SET source_version='0.157.1',contract_revision='codex-0.157.1-response-usage-v1' WHERE response_id='one'");
    let historical = crate::workflow::state(&session.store).unwrap().tasks[0]
        .observed_usage
        .clone()
        .unwrap();
    assert_eq!(historical.status, ObservedUsageStatus::Observed);
    assert_eq!(historical.counters.unwrap().total_tokens, 30);
    session.execute("UPDATE codex_usage_observations SET contract_revision='codex-0.160.0-response-usage-v1' WHERE response_id='one'");
    assert_eq!(
        crate::workflow::state(&session.store).unwrap().tasks[0]
            .observed_usage
            .as_ref()
            .unwrap()
            .status,
        ObservedUsageStatus::Invalid
    );
    session.execute(
        "UPDATE codex_usage_observations SET source_version='0.160.0' WHERE response_id='one'",
    );
    let historical = crate::workflow::state(&session.store).unwrap().tasks[0]
        .observed_usage
        .clone()
        .unwrap();
    assert_eq!(historical.status, ObservedUsageStatus::Observed);
    assert_eq!(historical.counters.unwrap().total_tokens, 30);
    session
        .store
        .lock()
        .unwrap()
        .execute(
            "UPDATE codex_usage_observations SET contract_revision=?1 WHERE response_id='one'",
            params![crate::providers::codex::USAGE_CONTRACT_REVISION],
        )
        .unwrap();
    assert_eq!(
        crate::workflow::state(&session.store).unwrap().tasks[0]
            .observed_usage
            .as_ref()
            .unwrap()
            .status,
        ObservedUsageStatus::Invalid
    );
    session.execute("UPDATE codex_usage_observations SET source_version='0.162.1',contract_revision='codex-0.160.0-response-usage-v1' WHERE response_id='one'");
    assert_eq!(
        crate::workflow::state(&session.store).unwrap().tasks[0]
            .observed_usage
            .as_ref()
            .unwrap()
            .status,
        ObservedUsageStatus::Invalid
    );
    session.execute("UPDATE codex_usage_observations SET source_version='0.157.1',contract_revision='codex-0.157.1-response-usage-v1' WHERE response_id='one'");
    let revision = session.revision();
    session.store.lock().unwrap().execute("UPDATE codex_usage_observations SET input_tokens=?1,total_tokens=?1 WHERE response_id='two'",
            params![crate::domain::MAX_OBSERVED_TOKENS]).unwrap();
    assert_eq!(session.revision(), revision + 1);
    let usage = crate::workflow::state(&session.store).unwrap().tasks[0]
        .observed_usage
        .clone()
        .unwrap();
    assert_eq!(usage.status, ObservedUsageStatus::Invalid);
    assert!(usage.counters.is_none());
    session.execute("DELETE FROM codex_usage_observations WHERE response_id='two'");
    assert_eq!(session.revision(), revision + 2);
    assert_eq!(
        crate::workflow::state(&session.store).unwrap().tasks[0]
            .observed_usage
            .as_ref()
            .unwrap()
            .counters
            .as_ref()
            .unwrap()
            .total_tokens,
        15
    );
}

#[test]
fn provider_error_recovery_migration_preserves_holds_and_enforces_sources() {
    let session = ObservedSession::new("041-upgrade");
    for id in ["active-hook", "released-hook", "superseded-hook"] {
        session.hook(id, "StopFailure", "{}", 0);
    }
    session.hook("accepted", "UserPromptSubmit", "{}", 0);
    session.execute(
            "INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,
                transcript_epoch,accepted_hook_event_id,failure_hook_event_id,failure_kind,
                attribution,created_at,expires_at,state,resolved_at,resolution_kind,resolution_ref)
             VALUES('active','a','g','s','e','accepted','active-hook','rate_limit','prompt_id_matched',
                    '2026-01-01T00:00:00Z','2026-01-01T00:01:00Z','active',NULL,NULL,NULL),
                   ('released','a','g','s','e','accepted','released-hook','billing_error','arrival_order',
                    '2026-01-01T00:00:01Z',NULL,'human_released','2026-01-01T00:00:02Z','human_release','release'),
                   ('superseded','a','g','s','e',NULL,'superseded-hook','authentication_failed','startup_invocation',
                    '2026-01-01T00:00:03Z',NULL,'superseded','2026-01-01T00:00:04Z','role_replacement','switch');"
        );
    session.execute(&super::interruption_tests::prior_service_schema_sql(40));
    const OLD_ROWS: &str = "SELECT group_concat(quote(id)||quote(attempt_id)||quote(role_generation_id)
            ||quote(session_id)||quote(transcript_epoch)||quote(accepted_hook_event_id)
            ||quote(failure_hook_event_id)||quote(failure_kind)||quote(attribution)||quote(created_at)
            ||quote(expires_at)||quote(state)||quote(resolved_at)||quote(resolution_kind)||quote(resolution_ref),';')
            FROM (SELECT * FROM provider_failure_holds ORDER BY id)";
    let rows: String = session.scalar(OLD_ROWS);
    let revision = session.revision();
    let reopened = Store::open(&session.root.join("state.sqlite3")).unwrap();
    assert_eq!(
        session.scalar::<i64>("PRAGMA user_version"),
        CURRENT_SCHEMA_VERSION
    );
    assert_eq!(session.scalar::<String>(OLD_ROWS), rows);
    assert_eq!(session.revision(), revision);
    let connection = reopened.lock().unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(connection.query_row(
            "SELECT COUNT(*) FROM provider_failure_holds WHERE source='failure_hook' AND history_observation_id IS NULL",
            [], |row| row.get::<_, i64>(0),
        ).unwrap(), 3);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
               AND name IN ('provider_failure_holds_attempt','provider_failure_holds_session')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    connection.execute_batch(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES('history','history','service','provider.codex_capacity_observed','session','s','{}','2026-01-01T00:00:00Z'),
                   ('history-next','history-next','service','provider.codex_capacity_observed','session','s','{}','2026-01-01T00:00:01Z');"
        ).unwrap();
    let revision = read_state_revision(&connection).unwrap();
    for (source, hook, history, accepted, attribution, expected_error) in [
        (
            "codex_history",
            Some("active-hook"),
            Some("history"),
            Some("accepted"),
            "accepted_turn_history",
            "CHECK constraint failed",
        ),
        (
            "failure_hook",
            None,
            None,
            None,
            "startup_invocation",
            "CHECK constraint failed",
        ),
        (
            "failure_hook",
            None,
            Some("history"),
            Some("accepted"),
            "arrival_order",
            "CHECK constraint failed",
        ),
        (
            "codex_history",
            None,
            Some("history"),
            Some("accepted"),
            "arrival_order",
            "CHECK constraint failed",
        ),
        (
            "failure_hook",
            Some("active-hook"),
            None,
            Some("accepted"),
            "accepted_turn_history",
            "CHECK constraint failed",
        ),
        (
            "codex_history",
            None,
            Some("history"),
            None,
            "accepted_turn_history",
            "CHECK constraint failed",
        ),
        (
            "unknown",
            None,
            Some("history"),
            Some("accepted"),
            "accepted_turn_history",
            "CHECK constraint failed",
        ),
        (
            "codex_history",
            None,
            Some("missing"),
            Some("accepted"),
            "accepted_turn_history",
            "FOREIGN KEY constraint failed",
        ),
        (
            "failure_hook",
            Some("missing"),
            None,
            None,
            "startup_invocation",
            "FOREIGN KEY constraint failed",
        ),
        (
            "codex_history",
            None,
            Some("history"),
            Some("missing"),
            "accepted_turn_history",
            "FOREIGN KEY constraint failed",
        ),
    ] {
        let error = connection.execute(
                "INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
                    source,failure_hook_event_id,history_observation_id,accepted_hook_event_id,failure_kind,attribution,created_at,state)
                 VALUES('invalid','a','g','s','e',?1,?2,?3,?4,'usage_limit_exceeded',?5,'2026-01-01T00:00:00Z','active')",
                params![source, hook, history, accepted, attribution],
            ).unwrap_err();
        assert!(
            error.to_string().contains(expected_error),
            "{source}: {error}"
        );
        assert_eq!(read_state_revision(&connection).unwrap(), revision);
    }
    connection.execute_batch(
            "INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
                source,history_observation_id,accepted_hook_event_id,failure_kind,attribution,created_at,state)
             VALUES('history-hold','a','g','s','e','codex_history','history','accepted',
                'usage_limit_exceeded','accepted_turn_history','2026-01-01T00:00:00Z','active');"
        ).unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), revision + 1);
    let duplicate = connection.execute(
            "INSERT INTO provider_failure_holds SELECT 'duplicate',attempt_id,role_generation_id,session_id,
                transcript_epoch,accepted_hook_event_id,source,failure_hook_event_id,history_observation_id,
                failure_kind,attribution,created_at,expires_at,state,resolved_at,resolution_kind,resolution_ref
             FROM provider_failure_holds WHERE id='history-hold'", [],
        ).unwrap_err();
    assert!(duplicate.to_string().contains("UNIQUE constraint failed"));
    connection
        .execute(
            "UPDATE provider_failure_holds SET source=source WHERE id='history-hold'",
            [],
        )
        .unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), revision + 1);
    connection.execute("UPDATE provider_failure_holds SET history_observation_id='history-next' WHERE id='history-hold'", []).unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), revision + 2);
    connection.execute(
            "UPDATE provider_failure_holds SET source='failure_hook',history_observation_id=NULL,
                failure_hook_event_id='accepted',attribution='arrival_order' WHERE id='history-hold'", [],
        ).unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), revision + 3);
    connection
        .execute(
            "DELETE FROM provider_failure_holds WHERE id='history-hold'",
            [],
        )
        .unwrap();
    assert_eq!(read_state_revision(&connection).unwrap(), revision + 4);
    assert_eq!(session.scalar::<String>(OLD_ROWS), rows);
}

#[test]
fn codex_capacity_pipeline_preserves_authority_and_immutable_frontier() {
    for (tag, kind) in [
        ("server_overloaded", NativeTurnFailureKind::Overloaded),
        ("rate_limit_exceeded", NativeTurnFailureKind::RateLimit),
        (
            "usage_limit_exceeded",
            NativeTurnFailureKind::UsageLimitExceeded,
        ),
    ] {
        let mut session = ObservedSession::codex_capacity();
        session.write_capacity_failure(tag);
        session.execute("UPDATE sessions SET launch_state='started';");
        session.capacity_hook(
            "PreToolUse",
            serde_json::json!({"turn_id":CAPACITY_TURN,"tool_use_id":"before"}),
        );
        let observations = session.read_capacity();
        assert_eq!(
            observations.len(),
            1,
            "activity before capture does not mask a later native completion"
        );
        let (candidate, source) = &observations[0];
        for event in ["PostToolUse", "Stop", "Interrupt"] {
            for payload in [
                serde_json::json!({"turn_id":"other"}),
                serde_json::json!({}),
                serde_json::json!({"turn_id":12}),
                serde_json::json!({"prompt_id":CAPACITY_TURN}),
            ] {
                session.capacity_hook(event, payload);
            }
        }
        let before = session.business_rows();
        let revision = session.revision();
        session.execute(
            "CREATE TEMP TRIGGER refuse_history_hold BEFORE INSERT ON provider_failure_holds
                BEGIN SELECT RAISE(ABORT,'fixture hold insertion failure'); END;",
        );
        assert!(session
            .store
            .record_codex_capacity(candidate, source, &live())
            .is_err());
        assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 0);
        assert_eq!(session.business_rows(), before);
        assert_eq!(session.revision(), revision);
        session.execute("DROP TRIGGER refuse_history_hold;");
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        let after = session.business_rows();
        assert_eq!(
            after
                .iter()
                .filter(|(table, _)| table != "provider_failure_holds")
                .collect::<Vec<_>>(),
            before
                .iter()
                .filter(|(table, _)| table != "provider_failure_holds")
                .collect::<Vec<_>>()
        );
        assert_eq!(session.revision(), revision + 2);
        let hold_id: String = session.scalar("SELECT id FROM provider_failure_holds");
        assert_eq!(session.scalar::<String>("SELECT source||':'||attribution||':'||(failure_hook_event_id IS NULL) FROM provider_failure_holds"), "codex_history:accepted_turn_history:1");
        assert_eq!(
            session.scalar::<String>("SELECT failure_kind FROM provider_failure_holds"),
            kind.as_str()
        );
        assert_eq!(
            session.scalar::<bool>("SELECT expires_at IS NULL FROM provider_failure_holds"),
            !kind.app_cooldown()
        );
        assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM provider_failure_holds hold JOIN audit_events audit ON audit.id=hold.history_observation_id"), 1);
        let state = crate::workflow::state(&session.store).unwrap();
        let projected = state
            .active_sessions
            .iter()
            .find(|value| value["id"] == "s")
            .unwrap();
        assert!(projected["native_turn"]["failure"].is_null());
        assert_eq!(projected["readiness"], "busy");
        assert_eq!(
            projected["provider_failure_holds"][0]["source"],
            "codex_history"
        );
        let binding: crate::domain::FailedSessionStopBinding =
            serde_json::from_value(projected["failed_session_stop"].clone()).unwrap();
        assert_eq!(
            binding.process_identity,
            serde_json::from_str::<crate::domain::ProcessIdentity>(PROCESS).unwrap()
        );
        let audit_id: String = session.scalar(
            "SELECT id FROM audit_events WHERE event_code='provider.codex_capacity_observed'",
        );
        assert!(
            matches!(&binding.failure_source, crate::domain::ProviderFailureSource::CodexHistory { history_observation_id }
                if history_observation_id == &audit_id)
        );
        let mut mismatched = binding.clone();
        mismatched.failure_source = crate::domain::ProviderFailureSource::FailureHook {
            failure_hook_event_id: binding.accepted_hook_event_id.clone().unwrap(),
        };
        assert!(session
            .store
            .reserve_failed_session_stop(&crate::domain::HumanCommand::StopFailedSession {
                operation_id: "wrong-source".into(),
                binding: mismatched,
            })
            .is_err());
        assert_eq!(session.business_rows(), after);
        if kind == NativeTurnFailureKind::UsageLimitExceeded {
            let item = state
                .attention
                .iter()
                .find(|item| item.id == format!("provider_failure_hold:{hold_id}"))
                .unwrap();
            assert!(item.reason.contains("usage allowance or plan access"));
            assert!(item.reason.contains("releasing it sends no input"));
            assert!(!item.reason.contains("reset"));
        }
        let items = session.capacity_items();
        assert_eq!(items.len(), 1);
        if kind == NativeTurnFailureKind::Overloaded {
            assert_eq!(items[0].reason, "Codex reported that the selected model is at capacity; open agent output to inspect it and choose the next action.");
        }
        assert_eq!(
            serde_json::to_value(&items[0].target).unwrap(),
            serde_json::json!({"kind":"session",
                "project_id":"p","task_id":"t","attempt_id":"a","session_id":"s","role_generation_id":"g"})
        );
        assert_eq!(
            serde_json::to_value(&items[0].action).unwrap()["kind"],
            "open_agent_output"
        );
        let detail: String = session.scalar("SELECT detail_json FROM audit_events WHERE event_code='provider.codex_capacity_observed'");
        assert!(!detail.contains("private provider text") && !detail.contains("turn.jsonl"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&detail).unwrap()["hook_frontier"],
            candidate.hook_frontier
        );
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        assert!(session.read_capacity().is_empty());
        assert_eq!(session.revision(), revision + 2);
        let released = crate::workflow::execute(
            &session.store,
            &crate::domain::HumanCommand::ReleaseProviderFailureHold {
                operation_id: "release-history".into(),
                task_id: binding.task_id.clone(),
                attempt_id: binding.attempt_id.clone(),
                session_id: binding.session_id.clone(),
                hold_id: hold_id.clone(),
                expected_task_version: binding.expected_task_version,
            },
        )
        .unwrap();
        assert_eq!(released.state, "released");
        let released_revision = session.revision();
        let released_rows = session.business_rows();
        session.store = Store::open(&session.root.join("state.sqlite3")).unwrap();
        assert!(session.read_capacity().is_empty());
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        assert_eq!(session.revision(), released_revision);
        assert_eq!(session.business_rows(), released_rows);
        assert_eq!(
            session.scalar::<String>("SELECT state FROM provider_failure_holds"),
            "human_released"
        );
        let binding =
            crate::workflow::failed_session_stop_binding(&session.store.lock().unwrap(), "s")
                .unwrap()
                .unwrap();
        session.capacity_hook(
            "PostToolUse",
            serde_json::json!({"turn_id":CAPACITY_TURN,"tool_use_id":"after"}),
        );
        let retired_revision = session.revision();
        assert!(session.capacity_items().is_empty());
        assert!(session.read_capacity().is_empty());
        assert!(session
            .store
            .reserve_failed_session_stop(&crate::domain::HumanCommand::StopFailedSession {
                operation_id: "superseded-source".into(),
                binding,
            })
            .is_err());
        session
            .store
            .record_codex_capacity(candidate, source, &live())
            .unwrap();
        assert_eq!(session.revision(), retired_revision);
        assert_eq!(session.scalar::<String>("SELECT detail_json FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), detail);
        assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 1);
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM provider_failure_holds"),
            1
        );
    }
}

#[test]
fn codex_capacity_commit_revalidates_identity_attempt_and_activity() {
    for mutation in [
            "UPDATE sessions SET transcript_epoch='new'",
            "UPDATE sessions SET native_session_id='01a0fad3-1e45-71e2-8686-83184958e2c9'",
            "UPDATE sessions SET process_identity_json=json_set(process_identity_json,'$.pid',9999,'$.process_group_id',9999)",
            "INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
             SELECT 'replacement',attempt_id,role,provider,2,config_revision,status,authority_generation,created_at,updated_at FROM role_generations WHERE id='g';
             UPDATE sessions SET role_generation_id='replacement'; UPDATE role_settings SET effective_generation_id='replacement'",
            "UPDATE role_settings SET effective_generation_id=NULL",
            "UPDATE role_generations SET status='replaced'",
            "UPDATE projects SET repository_path='/changed'",
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.cwd','/changed')",
            "UPDATE hook_events SET payload_json=json_set(payload_json,'$.cwd','/changed') WHERE event_name='UserPromptSubmit'",
            "UPDATE hook_events SET payload_json=json_set(payload_json,'$.transcript_path','/changed') WHERE event_name='UserPromptSubmit'",
            "UPDATE attempts SET status='completed'",
            "UPDATE sessions SET status='recovery_required'",
            "UPDATE sessions SET status='exited',exit_json='{}'",
        ] {
            let session = ObservedSession::codex_capacity();
            let observations = session.read_capacity();
            assert_eq!(observations.len(), 1);
            session.execute(mutation);
            let before = session.business_rows();
            let revision = session.revision();
            session.store.record_codex_capacity(&observations[0].0, &observations[0].1, &live()).unwrap();
            assert!(session.capacity_items().is_empty(), "{mutation}");
            assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 0);
            assert_eq!(session.business_rows(), before);
            assert_eq!(session.revision(), revision);
        }
    for event in [
        "PreToolUse",
        "PostToolUse",
        "PermissionRequest",
        "SubagentStart",
        "SubagentStop",
        "Stop",
        "Interrupt",
        "SessionEnd",
        "SessionStart",
        "UserPromptSubmit",
    ] {
        let session = ObservedSession::codex_capacity();
        let observations = session.read_capacity();
        let payload = if matches!(event, "SessionEnd" | "SessionStart") {
            serde_json::json!({})
        } else {
            serde_json::json!({"turn_id":CAPACITY_TURN,
                    "transcript_path":session.root.join("sessions/day/turn.jsonl")})
        };
        session.capacity_hook(event, payload);
        let revision = session.revision();
        session
            .store
            .record_codex_capacity(&observations[0].0, &observations[0].1, &live())
            .unwrap();
        assert_eq!(session.revision(), revision, "{event}");
        assert!(session.capacity_items().is_empty(), "{event}");
    }
    let session = ObservedSession::codex_capacity();
    let observations = session.read_capacity();
    let mut mismatched_process: crate::domain::ProcessIdentity =
        serde_json::from_str(PROCESS).unwrap();
    mismatched_process.pid = 99;
    mismatched_process.process_group_id = 99;
    for processes in [
        ProcessSnapshot::Unavailable,
        ProcessSnapshot::Read(vec![]),
        ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "s".into(),
            owner: SessionOwner::RoleGeneration("g".into()),
            transcript_epoch: "old".into(),
            process_identity_json: PROCESS.into(),
        }]),
        ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "s".into(),
            owner: SessionOwner::RoleGeneration("other".into()),
            transcript_epoch: "e".into(),
            process_identity_json: PROCESS.into(),
        }]),
        // Equal ID text under the validation owner is a different process owner.
        ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "s".into(),
            owner: SessionOwner::ProviderValidationInvocation("g".into()),
            transcript_epoch: "e".into(),
            process_identity_json: PROCESS.into(),
        }]),
        ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "s".into(),
            owner: SessionOwner::RoleGeneration("g".into()),
            transcript_epoch: "e".into(),
            process_identity_json: serde_json::to_string(&mismatched_process).unwrap(),
        }]),
        ProcessSnapshot::Read(vec![LiveInvocation {
            session_id: "other".into(),
            owner: SessionOwner::RoleGeneration("g".into()),
            transcript_epoch: "e".into(),
            process_identity_json: PROCESS.into(),
        }]),
    ] {
        session
            .store
            .record_codex_capacity(&observations[0].0, &observations[0].1, &processes)
            .unwrap();
        assert!(session.capacity_items().is_empty());
        assert!(session
            .store
            .read_codex_capacity(&session.root.join("sessions"), &processes)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn codex_capacity_selection_requires_admitted_explicit_current_turn() {
    for tag in [
        "unknown",
        "quota_exceeded",
        "usage_not_included",
        "rate_limit",
    ] {
        let session = ObservedSession::codex_capacity();
        session.write_capacity_failure(tag);
        assert!(session.read_capacity().is_empty(), "{tag}");
        assert_eq!(
            session.scalar::<i64>("SELECT COUNT(*) FROM provider_failure_holds"),
            0
        );
    }
    for mutation in [
            "UPDATE sessions SET executable_version='codex-cli 0.159.2'",
            "UPDATE sessions SET executable_version='codex-cli 0.160.0'",
            "UPDATE sessions SET executable_version='unknown'",
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.compatibility.synthetic_origin',json('true'))",
            "UPDATE sessions SET launch_config_json=json_remove(launch_config_json,'$.compatibility')",
            "UPDATE sessions SET launch_config_json=json_set(launch_config_json,'$.compatibility.effective_hash','wrong')",
            "UPDATE hook_events SET event_name='UntrustedNativeEvent' WHERE event_name='UserPromptSubmit'",
            "DELETE FROM hook_events WHERE event_name='SessionStart'",
        ] {
            let session = ObservedSession::codex_capacity();
            session.execute(mutation);
            assert!(session.read_capacity().is_empty(), "{mutation}");
        }
    for turn in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!(4),
        serde_json::json!("wrong-turn"),
    ] {
        let session = ObservedSession::codex_capacity();
        session.capacity_hook(
            "UserPromptSubmit",
            serde_json::json!({"turn_id":turn,"prompt_id":CAPACITY_TURN,
                "transcript_path":session.root.join("sessions/day/turn.jsonl")}),
        );
        assert!(session.read_capacity().is_empty());
    }
    for event in ["Stop", "Interrupt", "SessionEnd"] {
        let session = ObservedSession::codex_capacity();
        session.capacity_hook(
            event,
            if event == "SessionEnd" {
                serde_json::json!({})
            } else {
                serde_json::json!({"turn_id":CAPACITY_TURN})
            },
        );
        assert!(
            session.read_capacity().is_empty(),
            "{event} before snapshot"
        );
    }
}

#[test]
fn codex_capacity_projection_retires_current_binding_without_new_audits() {
    for event in [
        "UserPromptSubmit",
        "SessionStart",
        "SessionEnd",
        "Stop",
        "Interrupt",
        "PreToolUse",
        "PostToolUse",
        "PermissionRequest",
        "SubagentStart",
        "SubagentStop",
    ] {
        let session = ObservedSession::codex_capacity();
        session.execute("UPDATE sessions SET launch_state='started';");
        let observations = session.read_capacity();
        session
            .store
            .record_codex_capacity(&observations[0].0, &observations[0].1, &live())
            .unwrap();
        assert_eq!(session.capacity_items().len(), 1);
        let binding =
            crate::workflow::failed_session_stop_binding(&session.store.lock().unwrap(), "s")
                .unwrap()
                .unwrap();
        session.capacity_hook(event, if matches!(event, "SessionStart" | "SessionEnd") { serde_json::json!({}) }
                else { serde_json::json!({"turn_id":if event == "UserPromptSubmit" { "new-turn" } else { CAPACITY_TURN },
                    "transcript_path":session.root.join("sessions/day/turn.jsonl")}) });
        let revision = session.revision();
        assert!(session.capacity_items().is_empty(), "{event}");
        assert!(session.read_capacity().is_empty(), "{event}");
        assert!(
            session
                .store
                .reserve_failed_session_stop(&crate::domain::HumanCommand::StopFailedSession {
                    operation_id: format!("superseded-{event}"),
                    binding,
                })
                .is_err(),
            "{event}"
        );
        assert_eq!(session.revision(), revision);
        assert_eq!(session.scalar::<i64>("SELECT COUNT(*) FROM audit_events WHERE event_code='provider.codex_capacity_observed'"), 1);
    }
    for mutation in ["UPDATE sessions SET status='exited',exit_json='{}'", "UPDATE sessions SET status='recovery_required'",
            "UPDATE sessions SET transcript_epoch='new'", "UPDATE role_generations SET status='replaced'",
            "UPDATE audit_events SET actor_kind='hook' WHERE event_code='provider.codex_capacity_observed'",
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
             VALUES('new-attempt','t','context','implementation','base',1,'running','2999-01-01','2999-01-01')"]
        {
            let session = ObservedSession::codex_capacity();
            let observations = session.read_capacity();
            session.store.record_codex_capacity(&observations[0].0, &observations[0].1, &live()).unwrap();
            session.execute(mutation);
            let revision = session.revision();
            assert!(session.capacity_items().is_empty(), "{mutation}");
            assert!(session.read_capacity().is_empty());
            assert_eq!(session.revision(), revision);
        }
}

#[test]
fn codex_capacity_fifo_does_not_starve_later_candidate_or_h3() {
    use std::os::unix::ffi::OsStrExt;
    let mut session = ObservedSession::codex_capacity();
    let fifo = session.root.join("sessions/day/fifo");
    let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // mkfifo receives a live NUL-terminated path; no writer ever opens it.
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    session.execute("INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,
            transcript_epoch,native_session_id,process_identity_json,created_at,updated_at)
            SELECT 's-bad',role_generation_id,provider,status,launch_config_json,executable_version,
                transcript_epoch,native_session_id,process_identity_json,created_at,updated_at FROM sessions WHERE id='s'");
    let mut context = session.store.role_context("capacity-token").unwrap();
    context.session_id = "s-bad".into();
    for event in ["SessionStart", "UserPromptSubmit"] {
        session.store.save_hook_event(&context, &HookEnvelope { provider: context.provider,
                payload: serde_json::json!({"hook_event_name":event,"session_id":CAPACITY_NATIVE,"cwd":session.root,
                    "turn_id":CAPACITY_TURN,"transcript_path":fifo}) },
                &RolePeerProvenance { peer_pid:4242, peer_process_group_id:4242, peer_start_marker:"peer".into(),
                    managed_root_pid:4242, managed_root_start_marker:"root".into(), state:"managed_process_group_untrusted_payload".into() }).unwrap();
    }
    let processes = ProcessSnapshot::Read(
        ["s-bad", "s"]
            .into_iter()
            .map(|id| LiveInvocation {
                session_id: id.into(),
                owner: SessionOwner::RoleGeneration("g".into()),
                transcript_epoch: "e".into(),
                process_identity_json: PROCESS.into(),
            })
            .collect(),
    );
    session.base = Utc::now();
    let observed = session
        .store
        .read_codex_capacity(&session.root.join("sessions"), &processes)
        .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].0.binding.session_id, "s");
    session
        .store
        .record_codex_capacity(&observed[0].0, &observed[0].1, &processes)
        .unwrap();
    let before = session.business_rows();
    session.observe(1_000, &processes, true);
    session.observe(601_000, &processes, false);
    session.observe(602_000, &processes, false);
    assert!(
        session.scalar::<i64>(
            "SELECT COUNT(*) FROM attention_observations WHERE kind='quiet_turn' AND state='open'"
        ) > 0
    );
    assert_eq!(session.business_rows(), before);
    assert_eq!(session.capacity_items().len(), 1);
}

#[test]
fn codex_capacity_projection_uses_entity_index_beyond_display_windows() {
    let session = ObservedSession::codex_capacity();
    let observed = session.read_capacity();
    session
        .store
        .record_codex_capacity(&observed[0].0, &observed[0].1, &live())
        .unwrap();
    session.execute("WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<205)
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            SELECT 'history-'||n,'g','codex','exited','{}','fixture','old','2999-01-01','2999-01-01' FROM numbers;
            WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<505)
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            SELECT 'noise-'||n,'noise','service','unrelated','session','unrelated','{}','2999-01-01' FROM numbers;");
    let revision = session.revision();
    let state = crate::workflow::state(&session.store).unwrap();
    assert!(state
        .attention
        .iter()
        .any(|item| item.id.starts_with("codex_capacity:")));
    assert!(state.active_sessions.iter().any(|value| value["id"] == "s"
        && value["role_generation_id"] == "g"
        && value["attempt_id"] == "a"
        && value["status"] == "running"));
    assert_eq!(session.revision(), revision);
    let connection = session.store.lock().unwrap();
    let plan = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {CODEX_CAPACITY_AUDITS_SQL}"))
        .unwrap()
        .query_map(params!["s"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("SEARCH audit_events USING INDEX audit_events_entity"),
        "{plan}"
    );
    assert!(!plan.contains("SCAN audit_events"), "{plan}");
}

#[test]
fn dismissed_native_prompt_still_suppresses_raw_wait_observations() {
    let session = ObservedSession::new("dismissed-native-wait");
    session.hook("start", "SessionStart", "{}", 0);
    session.hook(
        "notice",
        "Notification",
        r#"{"notification_type":"agent_needs_input"}"#,
        1_000,
    );
    session.observe(5_000, &live(), true);
    let projected = crate::workflow::state(&session.store).unwrap();
    let binding =
        serde_json::from_value(projected.active_sessions[0]["native_prompt"]["dismissal"].clone())
            .unwrap();
    crate::workflow::execute(
        &session.store,
        &crate::domain::HumanCommand::DismissNativePrompt {
            operation_id: "dismiss".into(),
            binding,
        },
    )
    .unwrap();
    let revision = session.revision();
    session.observe(31_000, &live(), false);
    session.observe(33_000, &live(), false);
    assert_eq!(session.state("process_without_accepted_turn"), "none");
    assert_eq!(session.revision(), revision);
    assert!(
        crate::workflow::native_prompt(&session.store.lock().unwrap(), "s")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        crate::workflow::state(&session.store)
            .unwrap()
            .active_sessions[0]["native_prompt"]["dismissed"],
        true
    );
    assert_eq!(
        session.scalar::<String>("SELECT readiness_state FROM sessions WHERE id='s'"),
        "busy"
    );
}

#[test]
fn an_unaccepted_live_invocation_opens_on_two_spaced_checks_and_ends_on_its_acceptance() {
    let session = ObservedSession::new("unaccepted");
    let kind = "process_without_accepted_turn";
    // Without its own trusted start, absent acceptance proves nothing.
    session.observe(5_000, &live(), true);
    assert_eq!(session.state(kind), "none");
    session.hook("start", "SessionStart", "{}", 0);
    session.observe(10_000, &live(), false);
    assert_eq!(session.state(kind), "candidate:0:0:-");
    let quiet = session.revision();
    // An unreadable process table discards the candidate; it confirms nothing.
    session.observe(31_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "none");
    session.observe(32_000, &live(), false);
    assert_eq!(session.state(kind), "candidate:1:0:-");
    // A second check needs a later, distinct time at least a second on.
    session.observe(32_500, &live(), false);
    session.observe(31_500, &live(), false);
    assert_eq!(session.state(kind), "candidate:1:0:-");
    assert_eq!(
        session.revision(),
        quiet,
        "candidate bookkeeping moved the cursor"
    );
    session.observe(32_500, &live(), false);
    assert_eq!(session.state(kind), "open:2:0:-");
    assert!(session.revision() > quiet);
    // Keyboard control suppresses classification; it is not acceptance.
    session.execute(&format!(
        "INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,
                 process_identity_json,expires_at,revoked_at,created_at,updated_at)
               VALUES('s','hash','human','person','g','{PROCESS}','2999-01-01T00:00:00Z',NULL,
                 '2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');"
    ));
    let gated = session.revision();
    session.observe(36_000, &live(), false);
    assert_eq!(session.state(kind), "open:2:0:-");
    assert_eq!(session.revision(), gated);
    session.execute("DELETE FROM input_leases;");
    // Unknown inventory keeps the open record, marked uncertain.
    session.observe(37_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:2:1:-");
    session.hook("accepted", "UserPromptSubmit", r#"{"prompt":"go"}"#, 38_000);
    session.observe(39_000, &live(), false);
    assert_eq!(session.state(kind), "resolved:2:0:turn_accepted");
    assert_eq!(
        session
            .store
            .lock()
            .unwrap()
            .query_row(
                "SELECT group_concat(event_code) FROM (SELECT event_code FROM audit_events
                       WHERE entity_kind='attention_observation' ORDER BY rowid)",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "attention.observation.opened,attention.observation.resolved"
    );
    // Classification never touched the work it describes.
    assert_eq!(
            session
                .store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT t.attention||':'||t.version||':'||a.status||':'||s.status||':'||s.readiness_state
                     FROM tasks t JOIN attempts a ON a.task_id=t.id
                     JOIN role_generations rg ON rg.attempt_id=a.id JOIN sessions s ON s.role_generation_id=rg.id",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "none:1:running:running:busy"
        );
}

#[test]
fn a_quiet_turn_starts_its_own_clock_and_ends_on_activity() {
    let session = ObservedSession::new("quiet");
    let kind = "quiet_turn";
    session.hook("start", "SessionStart", "{}", -1_200_000);
    session.hook(
        "accepted",
        "UserPromptSubmit",
        r#"{"prompt":"go"}"#,
        -660_000,
    );
    session.execute("UPDATE sessions SET transcript_last_sequence=7;");
    // The old session timestamp is not quiet history; the first look starts the clock.
    session.observe(0, &live(), true);
    assert_eq!(session.state(kind), "candidate:0:0:-");
    session.observe(299_000, &live(), false);
    assert_eq!(session.state(kind), "candidate:0:0:-");
    // A restart forgets the candidate, so its quiet time starts over.
    session.observe(299_500, &live(), true);
    session.observe(300_000, &live(), false);
    assert_eq!(session.state(kind), "candidate:0:0:-");
    session.observe(599_500, &live(), false);
    assert_eq!(session.state(kind), "candidate:1:0:-");
    session.observe(600_500, &live(), false);
    assert_eq!(session.state(kind), "open:2:0:-");
    let opened = session.revision();
    // Lost capture keeps the last confirmed evidence, marked uncertain.
    session.execute("UPDATE sessions SET capture_state='failed';");
    session.observe(601_500, &live(), false);
    assert_eq!(session.state(kind), "open:2:1:-");
    assert!(session.revision() > opened);
    session.execute("UPDATE sessions SET capture_state='capturing';");
    session.observe(602_500, &live(), false);
    assert_eq!(session.state(kind), "open:2:0:-");
    session.execute(&format!("UPDATE sessions SET transcript_last_sequence=8;
            INSERT INTO input_leases(session_id,lease_id_hash,owner_kind,owner_id,role_generation_id,
                process_identity_json,expires_at,created_at,updated_at)
            VALUES('s','quiet-control','human','person','g','{PROCESS}','2999-01-01T00:00:00Z',
                '2026-03-01T00:10:03Z','2026-03-01T00:10:03Z');"));
    session.observe(603_500, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:2:1:-");
    let before = session.business_rows();
    session.observe(604_500, &live(), false);
    assert_eq!(session.state(kind), "resolved:2:0:activity_resumed");
    assert_eq!(session.business_rows(), before);
}

#[test]
fn observation_identity_survives_reopen_and_retirement_has_no_workflow_effects() {
    let session = ObservedSession::new("identity");
    session.hook("start", "SessionStart", "{}", 0);
    let before = session.business_rows();
    session.observe(30_000, &live(), true);
    session.observe(31_000, &live(), false);
    let id: String = session.scalar("SELECT id FROM attention_observations WHERE state='open'");
    let revision = session.revision();
    session.observe(32_000, &live(), false);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.business_rows(), before);
    let reopened = Store::open(&session.root.join("state.sqlite3")).unwrap();
    reopened
        .observe_attention(session.at(33_000), &ProcessSnapshot::Unavailable, true)
        .unwrap();
    assert_eq!(
        session.scalar::<String>("SELECT id FROM attention_observations WHERE state='open'"),
        id
    );
    assert_eq!(session.state("process_without_accepted_turn"), "open:2:1:-");
    session
        .execute("UPDATE tasks SET lifecycle='cancelled'; UPDATE attempts SET status='cancelled';");
    session.observe(34_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        session.state("process_without_accepted_turn"),
        "resolved:2:0:attempt_retired"
    );
    session
        .execute("UPDATE tasks SET lifecycle='in_progress'; UPDATE attempts SET status='running';");
    session.observe(35_000, &live(), false);
    session.observe(36_000, &live(), false);
    assert_eq!(
        session.scalar::<String>("SELECT id FROM attention_observations WHERE state='open'"),
        id
    );
    session.execute("UPDATE sessions SET transcript_epoch='replacement';");
    session.observe(37_000, &live(), false);
    assert_eq!(session.scalar::<String>("SELECT resolution_reason FROM attention_observations WHERE id=(SELECT id FROM attention_observations WHERE entity_key='s:e')"), "invocation_replaced");
}

#[test]
fn quiet_candidates_require_uninterrupted_capture_activity_and_person_gates() {
    let session = ObservedSession::new("quiet-gates");
    let kind = "quiet_turn";
    session.hook("start", "SessionStart", "{}", -1_000_000);
    session.hook(
        "accepted",
        "UserPromptSubmit",
        r#"{"prompt_id":"turn"}"#,
        -700_000,
    );
    session.observe(0, &live(), true);
    session.observe(300_000, &live(), false);
    session.permission();
    session.observe(301_000, &live(), false);
    assert_eq!(session.state(kind), "none");
    session.execute("UPDATE permission_requests SET state='expired';");
    session.observe(302_000, &live(), false);
    session.observe(602_000, &live(), false);
    session.observe(602_500, &ProcessSnapshot::Read(vec![]), false);
    assert_eq!(
        session.state(kind),
        "none",
        "an unattached process is unknown"
    );
    session.observe(603_000, &live(), false);
    session.execute("UPDATE sessions SET transcript_last_sequence=1;");
    session.observe(903_000, &live(), false);
    assert_eq!(session.state(kind), "candidate:0:0:-");
    session.observe(1_203_000, &live(), false);
    session.observe(1_204_000, &live(), false);
    assert_eq!(session.state(kind), "open:2:0:-");
    session.hook("stop", "Stop", r#"{"prompt_id":"turn"}"#, 1_205_000);
    session.observe(1_206_000, &live(), false);
    assert_eq!(session.state(kind), "resolved:2:0:turn_completed");
}

#[test]
fn a_failed_observation_transaction_rolls_back_classification_only() {
    let session = ObservedSession::new("rollback");
    session.hook("start", "SessionStart", "{}", 0);
    session.observe(30_000, &live(), true);
    session.execute("INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,
            created_at,written_at,delivery_session_id,delivery_transcript_epoch)
            VALUES('guidance','a','g','private','written_awaiting_submit','2026-03-01T00:00:00Z',
                '2026-03-01T00:00:00Z','s','e');
            CREATE TRIGGER fail_guidance_observation BEFORE INSERT ON attention_observations
            WHEN NEW.kind='guidance_unaccepted' BEGIN SELECT RAISE(ABORT,'observation write fault'); END;");
    let before = session.business_rows();
    let revision = session.revision();
    let error = session
        .store
        .observe_attention(session.at(31_000), &live(), false)
        .unwrap_err();
    assert!(
        error.to_string().contains("observation write fault"),
        "{error}"
    );
    assert_eq!(
        session.state("process_without_accepted_turn"),
        "candidate:1:0:-"
    );
    assert_eq!(session.state("guidance_unaccepted"), "none");
    assert_eq!(session.business_rows(), before);
    assert_eq!(session.revision(), revision);
    assert_eq!(
        session.scalar::<i64>(
            "SELECT COUNT(*) FROM audit_events WHERE entity_kind='attention_observation'"
        ),
        0
    );
}

#[test]
fn no_session_requires_complete_effective_inventory_and_a_session_required_decision() {
    let session = ObservedSession::new("no-session");
    let kind = "attempt_without_live_session";
    let classify = |milliseconds, processes: &ProcessSnapshot, required| {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        let pass = ObservationPass {
            transaction: &transaction,
            now: session.at(milliseconds),
            now_text: session.at(milliseconds).to_rfc3339(),
            processes,
        };
        let (subject, finding) = pass.attempt_without_live_session("a", required).unwrap();
        pass.advance(subject, finding).unwrap();
        transaction.commit().unwrap();
    };
    classify(0, &ProcessSnapshot::Read(vec![]), true);
    assert_eq!(
        session.state(kind),
        "none",
        "missing handle alone is unknown"
    );
    session.execute("UPDATE sessions SET status='exited',exit_json='{}';");
    classify(0, &ProcessSnapshot::Read(vec![]), true);
    classify(60_000, &ProcessSnapshot::Read(vec![]), true);
    classify(60_500, &ProcessSnapshot::Unavailable, true);
    assert_eq!(session.state(kind), "none");
    classify(61_000, &ProcessSnapshot::Read(vec![]), true);
    classify(121_000, &ProcessSnapshot::Read(vec![]), true);
    classify(122_000, &ProcessSnapshot::Read(vec![]), true);
    assert_eq!(session.state(kind), "open:2:0:-");
    classify(123_000, &live(), true);
    assert_eq!(session.state(kind), "resolved:2:0:effective_session_live");
    classify(124_000, &ProcessSnapshot::Read(vec![]), false);
    assert_eq!(session.state(kind), "resolved:2:0:effective_session_live");
}

#[test]
fn no_session_helper_uses_only_the_four_reviewed_manager_waits() {
    let session = ObservedSession::new("manager-waits");
    {
        let connection = session.store.lock().unwrap();
        connection.execute("INSERT INTO trip_project_state(project_id,readiness,reason,detected_installation,
                detected_json,workflow_id,package_version,upstream_source_hash,overlay_hash,updated_at)
                VALUES('p','ready','fixture','compatible','{}',?1,?2,?3,?4,'2026-01-01T00:00:00Z')",
                params![crate::trip::WORKFLOW_ID, crate::trip::PACKAGE_VERSION, crate::trip::source_hash(), crate::trip::overlay_hash()]).unwrap();
        connection.execute("UPDATE attempts SET workflow_version=?1,workflow_hash=?2,legacy_migration_required=0",
                params![crate::trip::WORKFLOW_ID, crate::workflow_resources::workflow_hash()]).unwrap();
    }
    session.execute(
        "UPDATE role_generations SET role='manager'; UPDATE role_settings SET role='manager';",
    );
    let required = || {
        crate::coordinator::attempts_waiting_on_manager_turn(&session.store.lock().unwrap())
            .unwrap()
            .contains("a")
    };
    for (phase, plan_hash, candidate_hash) in [
        ("planning", None, None),
        ("planning", Some("plan"), None),
        ("implementation", Some("plan"), Some("candidate")),
        ("manager_handoff", Some("plan"), Some("candidate")),
    ] {
        session
            .store
            .lock()
            .unwrap()
            .execute(
                "UPDATE attempts SET phase=?1,plan_hash=?2,candidate_hash=?3",
                params![phase, plan_hash, candidate_hash],
            )
            .unwrap();
        assert!(required(), "{phase} must use the reviewed manager wait");
    }
    session.execute("UPDATE tasks SET attention='paused';");
    assert!(
        !required(),
        "a persisted hold is not a missing-session wait"
    );
    session.execute("UPDATE tasks SET attention='none'; UPDATE sessions SET status='exited',exit_json='{}'; UPDATE role_generations SET status='exited';");
    assert!(
        !required(),
        "dispatching a manager is outside the reviewed set"
    );
}

#[test]
fn guidance_requires_its_delivery_and_exit_or_an_unrelated_prompt_never_clears_it() {
    let session = ObservedSession::new("guidance");
    let kind = "guidance_unaccepted";
    session.execute("INSERT INTO guidance_messages(id,attempt_id,role_generation_id,body,state,
            created_at,written_at,delivery_session_id,delivery_transcript_epoch)
            VALUES('guidance','a','g','private body','written_awaiting_submit','2026-03-01T00:00:00Z',
            '2026-03-01T00:00:00Z','s','e');");
    session.observe(29_999, &ProcessSnapshot::Unavailable, true);
    assert_eq!(session.state(kind), "candidate:0:0:-");
    session.observe(30_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:1:0:-");
    session.hook("start", "SessionStart", "{}", 31_000);
    session.hook(
        "unrelated",
        "UserPromptSubmit",
        r#"{"prompt":"another instruction"}"#,
        32_000,
    );
    session.execute("UPDATE sessions SET status='exited',exit_json='{}';");
    session.observe(33_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:1:0:-");
    session.execute("UPDATE guidance_messages SET state='delivery_unknown';");
    session.observe(33_500, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        session.state(kind),
        "open:1:1:-",
        "uncertain delivery is not disposition"
    );
    session.execute("UPDATE guidance_messages SET state='written_awaiting_submit';");
    assert!(!session
        .scalar::<String>(
            "SELECT evidence_json FROM attention_observations WHERE kind='guidance_unaccepted'"
        )
        .contains("private body"));
    session.execute("UPDATE guidance_messages SET state='submitted';");
    session.observe(34_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "resolved:1:0:guidance_submitted");
}

#[test]
fn permission_observes_only_a_residual_native_unresolved_request_and_never_answers_it() {
    let session = ObservedSession::new("permission");
    let kind = "permission_on_exited_session";
    session.permission();
    session.observe(0, &ProcessSnapshot::Unavailable, true);
    assert_eq!(session.state(kind), "none");
    session.execute("UPDATE sessions SET status='exited';");
    session.observe(1_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        session.state(kind),
        "none",
        "status alone is not matched exit evidence"
    );
    session.execute("UPDATE sessions SET exit_json='{}';");
    let before = session.business_rows();
    session.observe(2_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:1:0:-");
    assert_eq!(session.business_rows(), before);
    session.execute("UPDATE permission_requests SET state='expired';");
    session.observe(3_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "resolved:1:0:request_settled");
}

#[test]
fn resume_contradiction_requires_exact_supersession_and_keeps_independent_holds() {
    let session = ObservedSession::new("resume");
    let kind = "resume_failure_after_acceptance";
    session.execute(r#"UPDATE tasks SET attention='resume_failed'; UPDATE attempts SET status='needs_input';
            UPDATE sessions SET resume_count=1,capability_key='key';
            INSERT INTO resume_invocations(id,session_id,resume_ordinal,transcript_epoch,prior_transcript_epoch,
                launch_config_json,capability_key,capability_identity_json,state,hook_event_boundary_rowid,created_at,updated_at)
            VALUES('resume','s',1,'e','prior','{}','key','{}','running',
                (SELECT COALESCE(MAX(rowid),0) FROM hook_events WHERE session_id='s'),'2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('rejection','rejection','service','session.resume.rejected','session','s',
                '{"attempt_id":"a","role":"implementer","lane_id":"default","role_generation_id":"g","transcript_epoch":"prior","resume_count":0}',
                '2026-02-28T23:59:00Z');"#);
    session.hook("start", "SessionStart", "{}", 0);
    session.hook("accepted", "UserPromptSubmit", "{}", 1_000);
    session.observe(2_000, &ProcessSnapshot::Unavailable, true);
    assert_eq!(
        session.state(kind),
        "none",
        "a submit alone is not supersession"
    );
    session.execute(r#"INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('reconciled','reconciled','hook','session.resume.turn_reconciled','session','s',
            '{"role_generation_id":"g","transcript_epoch":"e","resume_invocation_id":"resume","superseding_hook_event_id":"accepted","rejection_event_ids":["rejection"]}',
            '2026-03-01T00:00:01Z');"#);
    session.hook("gated-failure", "StopFailure", "{}", 2_500);
    session.execute("INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,
            transcript_epoch,accepted_hook_event_id,failure_hook_event_id,failure_kind,attribution,created_at,state)
            VALUES('candidate-hold','a','g','s','e','accepted','gated-failure','authentication_failed',
                'arrival_order','2026-03-01T00:00:02.500Z','active');");
    let before = session.business_rows();
    session.observe(3_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        session.state(kind),
        "none",
        "an H4 hold suppresses a candidate"
    );
    assert_eq!(session.business_rows(), before);
    session.execute("UPDATE provider_failure_holds SET state='human_released',resolved_at='2026-03-01T00:00:03.500Z',
                resolution_kind='human_release',resolution_ref='fixture-release' WHERE id='candidate-hold';
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                VALUES('g-manager','a','manager','claude',1,1,'running','f','2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
                VALUES('s-manager','g-manager','claude','exited','{}','fixture','manager-epoch','2026-03-01T00:00:00Z','2026-03-01T00:00:00Z');
            INSERT INTO hook_events(id,session_id,role_generation_id,provider,event_name,payload_json,peer_pid,peer_process_group_id,
                peer_start_marker,provenance_state,received_at) VALUES('manager-failure','s-manager','g-manager','claude','StopFailure',
                '{}',42,42,'peer','managed_process_group_untrusted_payload','2026-03-01T00:00:03.500Z');
            INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
                failure_hook_event_id,failure_kind,attribution,created_at,state)
                VALUES('unrelated-hold','a','g-manager','s-manager','manager-epoch','manager-failure','authentication_failed',
                    'startup_invocation','2026-03-01T00:00:03.500Z','active');");
    let before = session.business_rows();
    session.observe(4_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "candidate:1:0:-");
    session.observe(5_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:2:0:-");
    assert_eq!(session.business_rows(), before);
    session.hook("standing-failure", "StopFailure", "{}", 5_500);
    session.execute("INSERT INTO provider_failure_holds(id,attempt_id,role_generation_id,session_id,transcript_epoch,
            accepted_hook_event_id,failure_hook_event_id,failure_kind,attribution,created_at,state)
            VALUES('standing-hold','a','g','s','e','accepted','standing-failure','authentication_failed','arrival_order',
                '2026-03-01T00:00:05.500Z','active');");
    let before = session.business_rows();
    session.observe(6_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "resolved:2:0:provider_failure_hold");
    assert_eq!(session.business_rows(), before);
    let projected = crate::workflow::state(&session.store).unwrap();
    assert!(projected
        .attention
        .iter()
        .any(|item| item.id == "provider_failure_hold:standing-hold"));
    assert!(!projected
        .attention
        .iter()
        .any(|item| item.reason.contains("nothing else explains the hold")));
    session.execute("UPDATE provider_failure_holds SET state='human_released',resolved_at='2026-03-01T00:00:06.500Z',
            resolution_kind='human_release',resolution_ref='fixture-release' WHERE id='standing-hold';");
    session.observe(7_000, &ProcessSnapshot::Unavailable, false);
    session.observe(8_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:2:0:-");
    session.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at)
            VALUES('independent',NULL,'a','attention_required','{}','2026-03-01T00:00:09Z','2026-03-01T00:00:09Z');");
    session.observe(9_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "resolved:2:0:independent_hold");
}

#[test]
fn busy_exit_preserves_app_stop_intent_and_excludes_history_and_completed_turns() {
    let session = ObservedSession::new("busy-exit");
    let kind = "busy_after_exit";
    session.hook("start", "SessionStart", "{}", 0);
    session.hook("accepted", "UserPromptSubmit", "{}", 1_000);
    session.execute("UPDATE sessions SET status='exited',exit_json='{}';");
    session.observe(2_000, &ProcessSnapshot::Unavailable, true);
    assert_eq!(session.state(kind), "none");
    session.execute("UPDATE sessions SET status='interrupt_requested',interrupt_requested_at='2026-03-01T00:00:02Z';");
    assert!(session
        .store
        .update_session_exit(
            "s",
            "e",
            PROCESS,
            r#"{"app_stop_requested":false,"observed_at":"2026-03-01T00:00:03Z","exit_code":0}"#
        )
        .unwrap());
    assert_eq!(
        session
            .scalar::<i64>("SELECT json_extract(exit_json,'$.app_stop_requested') FROM sessions"),
        1
    );
    assert_eq!(
        session.scalar::<i64>("SELECT json_extract(exit_json,'$.exit_code') FROM sessions"),
        0
    );
    assert!(session
        .scalar::<Option<String>>("SELECT interrupt_requested_at FROM sessions")
        .is_none());
    session.observe(3_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "none");
    session.execute("UPDATE sessions SET status='running';");
    assert!(session
        .store
        .update_session_exit(
            "s",
            "e",
            PROCESS,
            r#"{"app_stop_requested":true,"observed_at":"2026-03-01T00:00:04Z","exit_code":1}"#
        )
        .unwrap());
    assert_eq!(
        session
            .scalar::<i64>("SELECT json_extract(exit_json,'$.app_stop_requested') FROM sessions"),
        0
    );
    session.observe(4_000, &ProcessSnapshot::Unavailable, false);
    session.observe(5_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "open:2:0:-");
    session.execute("INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,
            summary,evidence_json,metadata_json,created_at) VALUES('report','report','s','g','candidate_ready',
            'completed','[]','{}','2026-03-01T00:00:06Z');");
    session.observe(7_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state(kind), "resolved:2:0:report_recorded");
}

#[test]
fn recurring_episodes_reset_only_at_published_progress_in_source_order() {
    let session = ObservedSession::new("recurrence");
    let consume = |id: &str, milliseconds: i64| {
        let now = session.at(milliseconds).to_rfc3339();
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT OR IGNORE INTO role_results(id,operation_id,session_id,role_generation_id,
                outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
                VALUES(?1,?1,'s','g','blocked','same wording','[]','{}',?2,?2)",
                params![id, now],
            )
            .unwrap();
        transaction
            .execute("UPDATE tasks SET attention='needs_input'", [])
            .unwrap();
        transaction
            .execute(
                "UPDATE attempts SET status='needs_input',updated_at=?1",
                params![now],
            )
            .unwrap();
        transaction.execute("INSERT OR IGNORE INTO audit_events(id,operation_id,actor_kind,event_code,
                entity_kind,entity_id,detail_json,created_at) VALUES(?1,?1,'service','attempt.attention.changed',
                'attempt','a',?2,?3)", params![format!("hold-{id}"), serde_json::json!({"attention":"needs_input",
                "reason":"role_blocked","result_id":id,"role_generation_id":"g"}).to_string(), now]).unwrap();
        count_blocking_episode(&transaction, id, &now).unwrap();
        transaction.commit().unwrap();
    };
    let count = || {
        session.scalar::<i64>(
            "SELECT recurrence_count FROM attention_observations WHERE kind='recurring_block'",
        )
    };
    consume("block-1", 0);
    assert_eq!(count(), 1);
    consume("block-1", 0);
    assert_eq!(count(), 1);
    session.observe(1_000, &ProcessSnapshot::Unavailable, true);
    assert_eq!(session.state("recurring_block"), "candidate:0:0:-");
    consume("block-2", 2_000);
    session.observe(3_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(session.state("recurring_block"), "open:0:0:-");
    let id: String =
        session.scalar("SELECT id FROM attention_observations WHERE kind='recurring_block'");
    consume("block-3", 4_000);
    assert_eq!(count(), 2);
    session.execute("UPDATE tasks SET attention='none'; UPDATE attempts SET status='running';");
    session.hook("start", "SessionStart", "{}", 5_000);
    session.hook("prompt", "UserPromptSubmit", "{}", 6_000);
    session.observe(7_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        count(),
        2,
        "Continue and a new prompt preserve episode history"
    );
    assert_eq!(session.state("recurring_block"), "resolved:0:0:block_ended");
    session.execute("INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,
            summary,evidence_json,metadata_json,created_at,consumed_at) VALUES('retired-progress','retired-progress',
            's','g','candidate_ready','retired','[]','{}','2026-03-01T00:00:08Z','2026-03-01T00:00:08Z');
            UPDATE role_generations SET status='replaced';");
    session.observe(9_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(count(), 2, "switch retirement completed no freeze");
    session.execute("UPDATE role_generations SET status='running';
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at) VALUES('duplicate-progress','duplicate-progress','s','g',
                'candidate_ready','duplicate','[]','{}','2026-03-01T00:00:10Z','2026-03-01T00:00:10Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                VALUES('duplicate-retirement','duplicate-retirement','service','role_result.superseded',
                'role_result','duplicate-progress','{}','2026-03-01T00:00:10Z');");
    session.observe(11_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(count(), 2);
    session.execute("INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at)
            VALUES('reused','a','candidate','base','same-content','{}',1,'2026-01-01T00:00:00Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at) VALUES('published','published','s','g','candidate_ready',
                'published','[]','{}','2026-03-01T00:00:12Z','2026-03-01T00:00:12Z');
            INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,result_snapshot_id,created_at,updated_at)
                VALUES('new-freeze','a','candidate','g','complete','reused','2026-03-01T00:00:12Z','2026-03-01T00:00:12Z');");
    consume("block-after-publication", 13_000);
    assert_eq!(
        count(),
        1,
        "an older publication resets before the later episode, not after it"
    );
    session.observe(14_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(count(), 1);
    assert_eq!(session.scalar::<String>("SELECT json_extract(reset_evidence_json,'$.freeze_intent_id') FROM attention_observations WHERE kind='recurring_block'"), "new-freeze");
    consume("second-after-publication", 15_000);
    session.observe(16_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(count(), 2);
    assert_eq!(
        session
            .scalar::<String>("SELECT id FROM attention_observations WHERE kind='recurring_block'"),
        id
    );
    session.execute("INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,
            authority_generation,created_at,updated_at) VALUES('manager','a','manager','claude',1,1,'exited','f',
            '2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES('manager-session','manager','claude','exited','{}','fixture','manager-epoch','2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
            VALUES('other-role-progress','other-role-progress','manager-session','manager','plan_ready','published','[]','{}',
                '2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');
            INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at)
            VALUES('manager-plan','a','plan','base','plan-content','{}',1,'2026-03-01T00:00:17Z');
            INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,result_snapshot_id,created_at,updated_at)
            VALUES('manager-freeze','a','plan','manager','complete','manager-plan','2026-03-01T00:00:17Z','2026-03-01T00:00:17Z');");
    session.observe(18_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        count(),
        2,
        "another role's published progress does not reset this role"
    );
    session.execute(r#"INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,
                evidence_json,metadata_json,created_at,consumed_at)
            VALUES('pending-before-freeze','pending-before-freeze','s','g','blocked','blocked','[]','{}',
                '2026-03-01T00:00:19Z','2026-03-01T00:00:19Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('pending-hold-before','pending-hold-before','service','attempt.attention.changed','attempt','a',
                '{"attention":"needs_input","reason":"role_blocked","result_id":"pending-before-freeze","role_generation_id":"g"}',
                '2026-03-01T00:00:19Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at)
            VALUES('pending-publication','pending-publication','s','g','candidate_ready','same content','[]','{}',
                '2026-03-01T00:00:22Z','2026-03-01T00:00:22Z');
            INSERT INTO freeze_intents(id,attempt_id,kind,source_role_generation_id,state,result_snapshot_id,created_at,updated_at)
            VALUES('pending-freeze','a','candidate','g','complete','reused','2026-03-01T00:00:22Z','2026-03-01T00:00:22Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,
                metadata_json,created_at,consumed_at)
            VALUES('pending-after-freeze','pending-after-freeze','s','g','blocked','blocked','[]','{}',
                '2026-03-01T00:00:23Z','2026-03-01T00:00:23Z');
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('pending-hold-after','pending-hold-after','service','attempt.attention.changed','attempt','a',
                '{"attention":"needs_input","reason":"role_blocked","result_id":"pending-after-freeze","role_generation_id":"g"}',
                '2026-03-01T00:00:23Z');
            UPDATE attempts SET status='needs_input',updated_at='2026-03-01T00:00:23Z' WHERE id='a';
            UPDATE tasks SET attention='needs_input';"#);
    let before = session.business_rows();
    session.observe(24_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(
        count(),
        1,
        "catch-up orders the publication between pending episodes"
    );
    assert_eq!(session.business_rows(), before);
    assert_eq!(session.scalar::<String>("SELECT last_counted_result_id FROM attention_observations WHERE kind='recurring_block'"), "pending-after-freeze");
    assert_eq!(session.scalar::<String>("SELECT json_extract(reset_evidence_json,'$.freeze_intent_id') FROM attention_observations WHERE kind='recurring_block'"), "pending-freeze");
    session.observe(24_500, &ProcessSnapshot::Unavailable, false);
    assert_eq!(count(), 1);
    consume("second-after-deferred-publication", 25_000);
    session.observe(26_000, &ProcessSnapshot::Unavailable, false);
    assert_eq!(count(), 2);
    session.execute(r#"INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
            VALUES('other-attempt','t','context','implementation','base',1,'running','2026-03-02T00:00:00Z','2026-03-02T00:00:00Z');
            INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,lane_id,created_at,updated_at)
            VALUES('lane-generation','other-attempt','implementer','claude',1,1,'running','f','other-lane','2026-03-02T00:00:00Z','2026-03-02T00:00:00Z');
            INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
            VALUES('lane-session','lane-generation','claude','exited','{}','fixture','lane-epoch','2026-03-02T00:00:00Z','2026-03-02T00:00:00Z');
            INSERT INTO role_results(id,operation_id,session_id,role_generation_id,outcome,summary,evidence_json,metadata_json,created_at,consumed_at)
            VALUES('retired-lane-result','retired-lane-result','lane-session','lane-generation','needs_input','retired','[]','{}','2026-03-02T00:00:01Z','2026-03-02T00:00:01Z'),
                ('lane-result','lane-result','lane-session','lane-generation','needs_input','same wording','[]','{}','2026-03-02T00:00:01Z','2026-03-02T00:00:01Z');
            UPDATE tasks SET attention='needs_input'; UPDATE attempts SET status='needs_input',updated_at='2026-03-02T00:00:01Z' WHERE id='other-attempt';
            INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
            VALUES('lane-hold','lane-hold','service','attempt.attention.changed','attempt','other-attempt',
                '{"attention":"needs_input","reason":"role_needs_input","result_id":"lane-result","role_generation_id":"lane-generation"}',
                '2026-03-02T00:00:01Z');"#);
    {
        let mut connection = session.store.lock().unwrap();
        let transaction = connection.transaction().unwrap();
        count_blocking_episode(&transaction, "lane-result", "2026-03-02T00:00:01Z").unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(
        session.scalar::<i64>(
            "SELECT recurrence_count FROM attention_observations WHERE attempt_id='a'"
        ),
        2
    );
    assert_eq!(session.scalar::<i64>("SELECT recurrence_count FROM attention_observations WHERE attempt_id='other-attempt' AND lane_id='other-lane' AND entity_key LIKE '%:needs_input'"), 1);
}
