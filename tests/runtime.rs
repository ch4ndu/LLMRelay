use agenticjira::{
    auth,
    config::InstancePaths,
    control::{self, ControlRequest},
    domain::{
        CapabilityStatus, HookEnvelope, HumanCommand, LaunchConfig, Provider, RoleKind,
        RoleOverride, RoleResultReport, ValidationLaunchRequest,
    },
    operations::Application,
    providers::{self, PreparedLaunch},
    store::{json_hash, Store},
    supervisor::Supervisor,
    task_cli::{self, RoleOperation, RoleRequest},
    workflow,
};
use base64::Engine;

fn synthetic_claude_contract() -> agenticjira::provider_compatibility::BundleSet {
    let codex = include_str!("../resources/provider-compatibility/codex.json");
    let mut claude: serde_json::Value = serde_json::from_str(include_str!(
        "../resources/provider-compatibility/claude.json"
    ))
    .unwrap();
    let codex_pack: serde_json::Value = serde_json::from_str(codex).unwrap();
    let mut selector = codex_pack["selectors"][0].clone();
    selector["predicate_id"] = "synthetic-claude-v1".into();
    selector["exact_version"] = "synthetic-claude-v1".into();
    for contract in selector["contracts"].as_array_mut().unwrap() {
        contract["contract_id"] =
            format!("synthetic-claude-{}", contract["role"].as_str().unwrap()).into();
        contract["native_policy_revision"] =
            providers::claude::NATIVE_SANDBOX_POLICY_REVISION.into();
    }
    claude["selectors"] = serde_json::json!([selector]);
    agenticjira::provider_compatibility::BundleSet::synthetic_for_tests(codex, &claude.to_string())
}

fn install_synthetic_codex_home(root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let home = root.join("synthetic-home");
    let codex_home = home.join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n[mcp_servers]\nplain = { command = \"/synthetic/plain\" }\n\"quoted.dot\" = { command = \"/synthetic/dotted\" }\n\"rocket🚀\" = { command = \"/synthetic/unicode\" }\n",
    )
    .unwrap();
    let encode = |value: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    let id_token = format!(
        "{}.{}.synthetic",
        encode(serde_json::json!({"alg":"none"})),
        encode(serde_json::json!({
            "https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}
        }))
    );
    let access_token = format!(
        "{}.{}.synthetic",
        encode(serde_json::json!({"alg":"none"})),
        encode(serde_json::json!({"exp":4102444800_i64}))
    );
    std::fs::write(
        codex_home.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "auth_mode":"chatgpt",
            "OPENAI_API_KEY":null,
            "tokens":{
                "id_token":id_token,
                "access_token":access_token,
                "refresh_token":"synthetic-refresh-token",
                "account_id":"synthetic-account"
            },
            "last_refresh":"2099-01-01T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    std::env::remove_var("CODEX_HOME");
    std::env::set_var("HOME", home);
}

fn environment(name: &str) -> String {
    std::env::var(name).unwrap()
}

fn hook_cli(provider: &str, event: &str, input: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_agenticjira"))
        .args(["hook", "--provider", provider, "--event", event])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

async fn role(operation: RoleOperation) -> anyhow::Result<serde_json::Value> {
    let socket = std::path::PathBuf::from(environment("AGENTICJIRA_ROLE_SOCKET"));
    let response = task_cli::request(
        &socket,
        &RoleRequest {
            credential: environment("AGENTICJIRA_ROLE_TOKEN"),
            operation,
        },
    )
    .await?;
    Ok(response.result)
}

async fn managed_grandchild() {
    let root_pgid: i32 = environment("AJ_T10_ROOT_PGID").parse().unwrap();
    assert_ne!(unsafe { libc::getpgrp() }, root_pgid);
    let session = environment("AGENTICJIRA_SESSION_ID");
    assert!(!role(RoleOperation::Hook { envelope: HookEnvelope {
        provider: Provider::Codex,
        payload: serde_json::json!({"hook_event_name":"SessionStart","session_id":session,"cwd":environment("AJ_T10_CWD")}),
    }}).await.unwrap()["native_identity_authoritative"].as_bool().unwrap());
    assert!(!role(RoleOperation::Report {
        report: RoleResultReport {
            operation_id: "t10-report".into(),
            outcome: "capability_observed".into(),
            summary: "L01 managed-child boundary observed".into(),
            evidence: vec!["managed grandchild retained scoped evidence only".into()],
            metadata: serde_json::json!({
                "claimed_task_id":"another-task",
                "validation_observation":{
                    "cell":"L01",
                    "observed":"managed descendant reported through its bound role socket"
                }
            }),
        }
    })
    .await
    .unwrap()["authoritative_completion"]
        .as_bool()
        .unwrap());
    assert!(role(RoleOperation::ProposeTransition {
        operation_id: "forbidden-transition".into(),
        phase: "implementation".into(),
        evidence: vec!["role cannot approve".into()],
    })
    .await
    .is_err());
    std::env::set_var("AGENTICJIRA_ROLE_TOKEN", "stale-role-token");
    assert!(role(RoleOperation::Context).await.is_err());
    assert!(control::request(
        &std::path::PathBuf::from(environment("AJ_T10_CONTROL_SOCKET")),
        &ControlRequest::Status,
    )
    .await
    .is_err());
    std::fs::write(environment("AJ_T10_DESCENDANT_READY"), b"ready").unwrap();
    for _ in 0..500 {
        if std::path::Path::new(&environment("AJ_T10_DESCENDANT_RELEASE")).exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("T10 descendant release was not observed");
}

fn t10_manual_duration_seconds() -> u64 {
    std::env::var("AJ_T10_MANUAL_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|seconds| (70..=180).contains(seconds))
        .unwrap_or(120)
}

fn t10_manual_timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn t10_manual_terminal_size() -> Option<(u16, u16)> {
    use std::os::fd::AsRawFd;

    let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
    if unsafe { libc::ioctl(std::io::stdout().as_raw_fd(), libc::TIOCGWINSZ, &mut size) } != 0
        || size.ws_row == 0
        || size.ws_col == 0
    {
        return None;
    }
    Some((size.ws_row, size.ws_col))
}

struct T10ManualTerminalInput {
    file: std::fs::File,
    original_flags: libc::c_int,
}

impl T10ManualTerminalInput {
    fn open() -> std::io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};

        // This must be a new open-file description: dup(stdin) would share
        // O_NONBLOCK with the PTY's stdout/stderr in the synthetic fixture.
        let opened = unsafe {
            libc::open(
                b"/dev/tty\0".as_ptr().cast(),
                libc::O_RDWR | libc::O_CLOEXEC,
            )
        };
        if opened < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let file = unsafe { std::fs::File::from_raw_fd(opened) };
        let fd = file.as_raw_fd();
        if unsafe { libc::isatty(fd) } != 1 {
            return Err(std::io::Error::other(
                "manual attachment input is not a controlling terminal",
            ));
        }
        let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if original_flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            file,
            original_flags,
        })
    }
}

impl Drop for T10ManualTerminalInput {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        let _ = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETFL, self.original_flags) };
    }
}

fn t10_manual_transcript_has_final(
    transcripts: &std::path::Path,
    session: &str,
) -> anyhow::Result<bool> {
    const FINAL_MARKER: &[u8] = b"AJ_T10_FINAL ";

    let mut cursor = (None, 0_u64);
    let mut suffix = Vec::new();
    loop {
        let page = agenticjira::transcript::read_frames(
            transcripts,
            session,
            cursor.0.as_deref(),
            cursor.1,
            512 * 1024,
        )?;
        for frame in &page.frames {
            if frame.gap || frame.encoding != "base64" {
                continue;
            }
            suffix.extend(base64::engine::general_purpose::STANDARD.decode(&frame.data)?);
            if suffix
                .windows(FINAL_MARKER.len())
                .any(|window| window == FINAL_MARKER)
            {
                return Ok(true);
            }
            if suffix.len() >= FINAL_MARKER.len() {
                let retained = FINAL_MARKER.len().saturating_sub(1);
                suffix.drain(..suffix.len() - retained);
            }
        }
        let next = (page.next_epoch, page.next_sequence);
        if !page.has_more {
            return Ok(false);
        }
        if next == cursor {
            anyhow::bail!("manual attachment transcript cursor did not advance");
        }
        cursor = next;
    }
}

async fn t10_manual_attachment_root() {
    use std::io::{Read, Write};

    let stop = std::path::PathBuf::from(environment("AJ_T10_MANUAL_STOP"));
    let deadline_millis: u128 = environment("AJ_T10_MANUAL_DEADLINE_MILLIS")
        .parse()
        .unwrap();
    let mut input_terminal = T10ManualTerminalInput::open().unwrap();
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "AJ_T10_SYNTHETIC_READY pid={} stream_bytes_per_second=16384 initial_idle_seconds=80",
        std::process::id()
    )
    .unwrap();
    stdout.flush().unwrap();
    let mut input = [0_u8; 4096];
    let mut stream_sequence = 0_u64;
    let mut observation_sequence = 0_u64;
    let mut last_size = None;
    let mut next_stream = std::time::Instant::now() + std::time::Duration::from_secs(80);
    let reason = loop {
        let now_millis = t10_manual_timestamp_millis();
        if stop.exists() {
            break "sentinel";
        }
        if now_millis >= deadline_millis {
            break "deadline";
        }
        if std::time::Instant::now() >= next_stream {
            stream_sequence += 1;
            let prefix = format!(
                "AJ_T10_STREAM sequence={stream_sequence} emitted_unix_ms={now_millis} bytes=2048 "
            );
            let mut frame = prefix.into_bytes();
            frame.resize(2047, b'.');
            frame.push(b'\n');
            stdout.write_all(&frame).unwrap();
            stdout.flush().unwrap();
            next_stream += std::time::Duration::from_millis(125);
        }
        for _ in 0..4 {
            match input_terminal.file.read(&mut input) {
                Ok(0) => break,
                Ok(count) => {
                    observation_sequence += 1;
                    writeln!(
                        stdout,
                        "AJ_T10_INPUT_ECHO sequence={observation_sequence} observed_unix_ms={} data_base64={}",
                        t10_manual_timestamp_millis(),
                        base64::engine::general_purpose::STANDARD.encode(&input[..count])
                    )
                    .unwrap();
                    stdout.flush().unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("T10 manual terminal input read failed: {error}"),
            }
        }
        let size = t10_manual_terminal_size();
        if size != last_size {
            last_size = size;
            if let Some((rows, cols)) = size {
                observation_sequence += 1;
                writeln!(
                    stdout,
                    "AJ_T10_GEOMETRY sequence={observation_sequence} observed_unix_ms={} rows={rows} cols={cols}",
                    t10_manual_timestamp_millis()
                )
                .unwrap();
                stdout.flush().unwrap();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    writeln!(
        stdout,
        "AJ_T10_FINAL sequence={} observed_unix_ms={} reason={reason}",
        stream_sequence + 1,
        t10_manual_timestamp_millis()
    )
    .unwrap();
    stdout.flush().unwrap();
}

async fn t10_manual_attachment_fixture(
    app: &Application,
    paths: &InstancePaths,
    store: &Store,
    session: &str,
    provider_pid: u32,
    duration_seconds: u64,
) {
    let stop = paths.runtime.join("t10-manual-stop");
    let cleanup = paths.runtime.join("t10-manual-cleanup");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(duration_seconds);
    println!(
        "AJ_T10_MANUAL_READY data_dir={} session_id={session} pid={provider_pid} stop_marker=run/t10-manual-stop",
        paths.root.display()
    );
    let mut stop_requested = false;
    let mut next_reconciliation = std::time::Instant::now();
    loop {
        let now = std::time::Instant::now();
        if now >= next_reconciliation {
            app.supervisor.reconcile().unwrap();
            next_reconciliation = now + std::time::Duration::from_secs(1);
            let evidence = store.session_json(session).unwrap();
            if evidence["status"].as_str() == Some("exited")
                && evidence["capture_state"].as_str() == Some("complete")
            {
                let final_marker =
                    t10_manual_transcript_has_final(&paths.transcripts, session).unwrap();
                let clean_exit = evidence["exit"]["success"].as_bool() == Some(true);
                if !final_marker || !clean_exit {
                    println!(
                        "AJ_T10_MANUAL_FINAL_DRAIN_REJECTED session_id={session} final_marker={final_marker} clean_exit={clean_exit} data_dir={}",
                        paths.root.display()
                    );
                    panic!("T10 manual attachment fixture rejected a non-final or non-clean provider exit");
                }
                println!("AJ_T10_MANUAL_FINAL_DRAIN_READY session_id={session}");
                let grace = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while !cleanup.exists() && std::time::Instant::now() < grace {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                return;
            }
        }
        if !stop_requested && (stop.exists() || std::time::Instant::now() >= deadline) {
            std::fs::write(&stop, b"stop").unwrap();
            stop_requested = true;
            println!("AJ_T10_MANUAL_STOP_REQUESTED session_id={session}");
        }
        if stop_requested
            && std::time::Instant::now() >= deadline + std::time::Duration::from_secs(20)
        {
            panic!("T10 manual attachment fixture did not reach final-drain synchronization");
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn service_requested_stop_crosses_settling_deadline() {
    let root = std::env::temp_dir().join(format!(
        "agenticjira-settling-deadline-{}",
        uuid::Uuid::new_v4()
    ));
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let paths = InstancePaths::resolve(Some(root.join("instance"))).unwrap();
    paths.create().unwrap();
    let store = Store::open(&paths.database).unwrap();
    let database = rusqlite::Connection::open(&paths.database).unwrap();
    database.execute_batch(
        "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
           VALUES('p','p','/tmp/p','settling-deadline-identity','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
           VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
           VALUES('a','t','c','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
           VALUES('g','a','manager','codex',1,1,'launch_reserved','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
           VALUES('s','g','codex','launch_reserved','{}','fixture','epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
           VALUES('claim','t','a','settling-deadline-identity','running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
    ).unwrap();
    database.execute(
        "INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
         VALUES('credential','g',?1,'[\"report_result\"]','2026-01-01T00:00:00Z')",
        rusqlite::params![auth::hash_secret("settling-deadline-token")],
    ).unwrap();
    let supervisor = Supervisor::new(store.clone(), paths.transcripts.clone());
    let executable = std::env::current_exe().unwrap();
    let launch = PreparedLaunch {
        config: LaunchConfig {
            compatibility: None,
            provider: Provider::Codex,
            role: RoleKind::Manager,
            executable: executable.clone(),
            executable_version: "fixture".into(),
            model: "inert-local".into(),
            effort: "none".into(),
            cwd: project,
            argv: vec![],
            environment_keys: vec!["AJ_SETTLING_ROOT".into()],
            permission_policy: "fixture".into(),
            security_policy: serde_json::json!({}),
            hook_revision: "fixture".into(),
            capability_status: CapabilityStatus::Unverified,
        },
        executable,
        arguments: vec![
            "--exact".into(),
            "service_requested_stop_allows_exact_descendant_to_settle".into(),
            "--nocapture".into(),
        ],
        environment: vec![
            ("AJ_SETTLING_ROOT".into(), "1".into()),
            (
                "AJ_SETTLING_READY".into(),
                root.join("ready").to_string_lossy().into_owned(),
            ),
            (
                "AJ_SETTLING_RELEASE".into(),
                root.join("release").to_string_lossy().into_owned(),
            ),
        ],
        supervision_executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_agenticjira")),
    };
    let identity = supervisor.spawn("s", "g", "epoch", &launch).unwrap();
    store
        .update_session_running("s", "epoch", &serde_json::to_string(&identity).unwrap())
        .unwrap();
    for _ in 0..200 {
        if root.join("ready").exists() && supervisor.process_group_members("s").unwrap().len() > 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    supervisor.interrupt("s").unwrap();
    for _ in 0..100 {
        supervisor.reconcile().unwrap();
        if supervisor
            .process_group_members("s")
            .unwrap()
            .iter()
            .all(|member| member["pid"] != identity.pid)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!supervisor.process_group_members("s").unwrap().is_empty());
    assert!(store.role_context("settling-deadline-token").is_err());
    let before_deadline: (String, String, i64, String, bool, bool) = database
        .query_row(
            "SELECT s.status,t.attention,
                (SELECT COUNT(*) FROM recovery_records WHERE session_id=s.id),c.state,
                s.exit_json IS NULL,rc.revoked_at IS NOT NULL
         FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
         JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN claims c ON c.attempt_id=a.id
         JOIN role_credentials rc ON rc.role_generation_id=g.id WHERE s.id='s'",
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
        before_deadline,
        (
            "interrupt_requested".into(),
            "none".into(),
            0,
            "running".into(),
            true,
            true
        )
    );
    let capacity = agenticjira::workflow::state(&store).unwrap().resources["capacity"].clone();
    assert_eq!(capacity["occupied_global"], 1);
    assert_eq!(capacity["occupied_by_provider"]["codex"], 1);
    assert_eq!(capacity["occupied_managers_by_provider"]["codex"], 1);
    for _ in 0..400 {
        supervisor.reconcile().unwrap();
        if store.session_json("s").unwrap()["status"] == "recovery_required" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let recovered: (String, String, i64, String, bool, String, String) = database.query_row(
        "SELECT s.status,t.attention,
                (SELECT COUNT(*) FROM recovery_records WHERE session_id=s.id AND state='attention_required'),
                c.state,s.exit_json IS NULL,s.launch_state,s.readiness_state
         FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
         JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id
         JOIN claims c ON c.attempt_id=a.id WHERE s.id='s'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).unwrap();
    assert_eq!(
        recovered,
        (
            "recovery_required".into(),
            "needs_recovery".into(),
            1,
            "unknown".into(),
            true,
            "delivery_unknown".into(),
            "unknown".into()
        )
    );
    assert!(!supervisor.process_group_members("s").unwrap().is_empty());
    let detail: serde_json::Value = serde_json::from_str(&database.query_row(
        "SELECT detail_json FROM recovery_records WHERE session_id='s' AND state='attention_required'",
        [],
        |row| row.get::<_, String>(0),
    ).unwrap()).unwrap();
    assert_eq!(detail["kind"], "provider_delivery_unknown");
    assert!(detail["reason"]
        .as_str()
        .unwrap()
        .contains("exact recorded descendant generation"));
    std::fs::write(root.join("release"), b"release").unwrap();
    for _ in 0..200 {
        supervisor.reconcile().unwrap();
        if supervisor.process_group_members("s").unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(supervisor.process_group_members("s").unwrap().is_empty());
    drop(supervisor);
    drop(store);
    drop(database);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_requested_stop_allows_exact_descendant_to_settle() {
    if std::env::var("AJ_SETTLING_DESCENDANT").as_deref() == Ok("1") {
        std::fs::write(environment("AJ_SETTLING_READY"), b"ready").unwrap();
        for _ in 0..500 {
            if std::path::Path::new(&environment("AJ_SETTLING_RELEASE")).exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("settling release was not observed");
    }
    if std::env::var("AJ_SETTLING_ROOT").as_deref() == Ok("1") {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "service_requested_stop_allows_exact_descendant_to_settle",
                "--nocapture",
            ])
            .env("AJ_SETTLING_DESCENDANT", "1");
        unsafe {
            command.pre_exec(|| {
                (libc::setpgid(0, 0) == 0)
                    .then_some(())
                    .ok_or_else(std::io::Error::last_os_error)
            });
        }
        let _child = command.spawn().unwrap();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    let root = std::env::temp_dir().join(format!("agenticjira-settling-{}", uuid::Uuid::new_v4()));
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let paths = InstancePaths::resolve(Some(root.join("instance"))).unwrap();
    paths.create().unwrap();
    let store = Store::open(&paths.database).unwrap();
    let database = rusqlite::Connection::open(&paths.database).unwrap();
    database.execute_batch(
        "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
           VALUES('p','p','/tmp/p','settling-identity','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO tasks(id,project_id,title,lifecycle,created_at,updated_at)
           VALUES('t','p','t','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
           VALUES('a','t','c','planning','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
           VALUES('g','a','manager','codex',1,1,'launch_reserved','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
           VALUES('s','g','codex','launch_reserved','{}','fixture','epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
         INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
           VALUES('claim','t','a','settling-identity','running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
    ).unwrap();
    database.execute(
        "INSERT INTO role_credentials(id,role_generation_id,token_hash,permissions_json,created_at)
         VALUES('credential','g',?1,'[\"report_result\"]','2026-01-01T00:00:00Z')",
        rusqlite::params![auth::hash_secret("settling-token")],
    ).unwrap();
    let supervisor = Supervisor::new(store.clone(), paths.transcripts.clone());
    let executable = std::env::current_exe().unwrap();
    let launch = PreparedLaunch {
        config: LaunchConfig {
            compatibility: None,
            provider: Provider::Codex,
            role: RoleKind::Manager,
            executable: executable.clone(),
            executable_version: "fixture".into(),
            model: "inert-local".into(),
            effort: "none".into(),
            cwd: project,
            argv: vec![],
            environment_keys: vec!["AJ_SETTLING_ROOT".into()],
            permission_policy: "fixture".into(),
            security_policy: serde_json::json!({}),
            hook_revision: "fixture".into(),
            capability_status: CapabilityStatus::Unverified,
        },
        executable,
        arguments: vec![
            "--exact".into(),
            "service_requested_stop_allows_exact_descendant_to_settle".into(),
            "--nocapture".into(),
        ],
        environment: vec![
            ("AJ_SETTLING_ROOT".into(), "1".into()),
            (
                "AJ_SETTLING_READY".into(),
                root.join("ready").to_string_lossy().into_owned(),
            ),
            (
                "AJ_SETTLING_RELEASE".into(),
                root.join("release").to_string_lossy().into_owned(),
            ),
        ],
        supervision_executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_agenticjira")),
    };
    let identity = supervisor.spawn("s", "g", "epoch", &launch).unwrap();
    store
        .update_session_running("s", "epoch", &serde_json::to_string(&identity).unwrap())
        .unwrap();
    for _ in 0..200 {
        if root.join("ready").exists() && supervisor.process_group_members("s").unwrap().len() > 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    supervisor.interrupt("s").unwrap();
    for _ in 0..100 {
        supervisor.reconcile().unwrap();
        if supervisor
            .process_group_members("s")
            .unwrap()
            .iter()
            .all(|member| member["pid"] != identity.pid)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let remaining = supervisor.process_group_members("s").unwrap();
    assert!(!remaining.is_empty());
    assert!(remaining.iter().all(|member| member["pid"] != identity.pid));
    assert!(store.role_context("settling-token").is_err());
    let settling: (String, String, i64, String) = database.query_row(
        "SELECT s.status,t.attention,(SELECT COUNT(*) FROM recovery_records WHERE session_id=s.id),c.state FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id JOIN attempts a ON a.id=g.attempt_id JOIN tasks t ON t.id=a.task_id JOIN claims c ON c.attempt_id=a.id WHERE s.id='s'", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(
        settling,
        (
            "interrupt_requested".into(),
            "none".into(),
            0,
            "running".into()
        )
    );
    std::fs::write(root.join("release"), b"release").unwrap();
    for _ in 0..200 {
        supervisor.reconcile().unwrap();
        if store.session_json("s").unwrap()["status"] == "exited" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let exited = store.session_json("s").unwrap();
    assert_eq!(exited["status"], "exited");
    assert_eq!(exited["exit"]["process_group_quiescent"], true);
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM recovery_records WHERE session_id='s'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(supervisor);
    drop(store);
    drop(database);
    std::fs::remove_dir_all(root).unwrap();
    service_requested_stop_crosses_settling_deadline().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t10_role_auth_hook_and_human_boundary() {
    if std::env::var("AJ_T10_GRANDCHILD").as_deref() == Ok("1") {
        managed_grandchild().await;
        return;
    }
    if std::env::var("AJ_T10_ROOT").as_deref() == Ok("1") {
        if std::env::var("AJ_T10_MANUAL_ATTACH").as_deref() == Ok("1") {
            t10_manual_attachment_root().await;
            return;
        }
        use std::os::unix::process::CommandExt;
        let root_pgid = unsafe { libc::getpgrp() };
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "t10_role_auth_hook_and_human_boundary",
                "--nocapture",
            ])
            .env("AJ_T10_GRANDCHILD", "1")
            .env("AJ_T10_ROOT_PGID", root_pgid.to_string());
        unsafe {
            command.pre_exec(|| {
                (libc::setpgid(0, 0) == 0)
                    .then_some(())
                    .ok_or_else(std::io::Error::last_os_error)
            });
        }
        let mut child = command.spawn().unwrap();
        for _ in 0..500 {
            if std::path::Path::new(&environment("AJ_T10_DESCENDANT_READY")).exists() {
                for _ in 0..500 {
                    if std::path::Path::new(&environment("AJ_T10_ROOT_RELEASE")).exists() {
                        drop(child);
                        return;
                    }
                    if let Some(status) = child.try_wait().unwrap() {
                        panic!("T10 descendant fixture exited before root release: {status}");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                panic!("T10 root release was not observed");
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("T10 descendant fixture exited before becoming ready: {status}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("T10 descendant fixture did not become ready");
    }

    let manual_attachment = std::env::var("AJ_T10_MANUAL_ATTACH").as_deref() == Ok("1");
    let root = std::env::temp_dir().join(format!(
        "agenticjira-t10{}-{}",
        manual_attachment.then_some("-manual").unwrap_or_default(),
        uuid::Uuid::new_v4()
    ));
    let original_home = std::env::var_os("HOME");
    let original_codex_home = std::env::var_os("CODEX_HOME");
    install_synthetic_codex_home(&root);
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = std::fs::canonicalize(project).unwrap();
    let paths = InstancePaths::resolve(Some(root.join("instance"))).unwrap();
    paths.create().unwrap();
    let store = Store::open(&paths.database).unwrap();
    let executable = std::env::current_exe().unwrap();
    let app = Application::new(paths.clone(), store.clone(), executable.clone()).unwrap();
    let claude_reviewer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&app.hooks.claude_settings).unwrap()).unwrap();
    let claude_implementer: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&app.hooks.claude_implementer_settings).unwrap())
            .unwrap();
    for settings in [&claude_reviewer, &claude_implementer] {
        assert_eq!(settings["env"]["DISABLE_AUTOUPDATER"], "1");
        assert_eq!(settings["env"]["DISABLE_UPDATES"], "1");
    }
    let claude_contract = synthetic_claude_contract();
    for claude_role in [RoleKind::PlanReviewer, RoleKind::Implementer] {
        let launch = providers::prepare_role_launch_with_bundles(
            Provider::Claude,
            claude_role,
            "local-child",
            "none",
            &project,
            "managed update policy contract",
            &paths.role_socket,
            "claude-contract-token",
            "claude-contract-generation",
            "claude-contract-session",
            None,
            &app.hooks,
            &executable,
            &claude_contract,
        )
        .unwrap();
        for key in ["DISABLE_AUTOUPDATER", "DISABLE_UPDATES"] {
            assert_eq!(
                launch
                    .environment
                    .iter()
                    .filter(|(name, value)| name == key && value == "1")
                    .count(),
                1
            );
            assert!(launch
                .config
                .environment_keys
                .iter()
                .any(|name| name == key));
            assert_eq!(
                launch.config.security_policy["update_environment"][key],
                "1"
            );
        }
        assert!(launch.config.permission_policy.contains(
            "managed child and settings pin DISABLE_AUTOUPDATER=1 and DISABLE_UPDATES=1"
        ));
        let identity = providers::capability_identity(&launch.config).unwrap();
        let identity_key = providers::capability_identity_key(&identity).unwrap();
        let mut omitted = launch.config.clone();
        omitted
            .environment_keys
            .retain(|key| key != "DISABLE_AUTOUPDATER");
        assert_ne!(providers::capability_key(&omitted).unwrap(), identity_key);
        let mut changed = launch.config.clone();
        changed.security_policy["update_environment"]["DISABLE_UPDATES"] = serde_json::json!("0");
        assert_ne!(providers::capability_key(&changed).unwrap(), identity_key);
    }
    let configured_events = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PermissionDenied",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopFailure",
        "SubagentStart",
        "SubagentStop",
        "SessionEnd",
    ];
    for event in configured_events {
        let expected = format!(
            "{} --event {}",
            providers::shell_quote(&app.hooks.runner.to_string_lossy()),
            providers::shell_quote(event),
        );
        assert_eq!(
            claude_reviewer["hooks"][event][0]["hooks"][0]["command"],
            expected
        );
        assert_eq!(
            claude_implementer["hooks"][event][0]["hooks"][0]["command"],
            expected
        );
    }
    let codex_launch = providers::codex::prepare(
        RoleKind::Implementer,
        "local-child",
        "none",
        &project,
        "hook command contract only",
        &paths.role_socket,
        "hook-contract-token",
        "hook-contract-generation",
        "hook-contract-session",
        None,
        &app.hooks,
        &executable,
        &[],
    )
    .unwrap();
    assert_eq!(
        codex_launch
            .arguments
            .windows(2)
            .filter(|pair| { pair[0] == "--config" && pair[1] == "approvals_reviewer=\"user\"" })
            .count(),
        1
    );
    assert_eq!(
        codex_launch.config.security_policy["approval_reviewer"],
        "user"
    );
    assert_eq!(
        codex_launch.config.security_policy["native_command_policy"]["status"],
        "unknown_unattested"
    );
    assert_eq!(
        codex_launch.config.capability_status,
        agenticjira::domain::CapabilityStatus::Unverified
    );
    let codex_identity = providers::capability_identity(&codex_launch.config).unwrap();
    assert_eq!(
        codex_identity.capability_status,
        Some(agenticjira::domain::CapabilityStatus::Unverified)
    );
    assert_eq!(
        codex_identity.security_policy["native_approval_ownership"],
        serde_json::json!({
            "revision":providers::codex::APPROVAL_OWNERSHIP_REVISION,
            "native_approvals":"honored",
            "new_native_requests":"existing_approval_inbox",
            "revocation_scope":"agenticjira_rules_only",
        })
    );
    let codex_manager = providers::codex::prepare(
        RoleKind::Manager,
        "local-child",
        "none",
        &project,
        "read-only tuple contract",
        &paths.role_socket,
        "manager-contract-token",
        "manager-contract-generation",
        "manager-contract-session",
        None,
        &app.hooks,
        &executable,
        &[],
    )
    .unwrap();
    assert_eq!(
        codex_manager
            .arguments
            .windows(2)
            .filter(|pair| { pair[0] == "--config" && pair[1] == "approvals_reviewer=\"user\"" })
            .count(),
        1
    );
    assert_eq!(
        codex_manager.config.security_policy["approval_reviewer"],
        "user"
    );
    assert_eq!(
        codex_manager.config.capability_status,
        agenticjira::domain::CapabilityStatus::Unverified
    );
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "Stop",
        "Interrupt",
        "SubagentStart",
        "SubagentStop",
        "SessionEnd",
    ] {
        let hook = codex_launch
            .arguments
            .iter()
            .find(|argument| argument.starts_with(&format!("hooks.{event}=")))
            .unwrap_or_else(|| panic!("Codex launch omitted {event} hook"));
        assert!(
            hook.contains(&format!("--event {}", providers::shell_quote(event))),
            "Codex {event} hook omitted its exact trusted event: {hook}"
        );
    }
    for (provider, input) in [
        (
            "unsupported-provider",
            r#"{"hook_event_name":"PermissionRequest"}"#,
        ),
        ("codex", "{"),
        ("codex", r#"{"hook_event_name":"SessionStart"}"#),
    ] {
        let output = hook_cli(provider, "PermissionRequest", input);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(
            response["hookSpecificOutput"]["decision"]["behavior"],
            "deny"
        );
    }
    let ordinary_error = hook_cli("codex", "SessionStart", "{");
    assert!(!ordinary_error.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&ordinary_error.stdout)
            .ok()
            .is_none_or(|value| value.get("hookSpecificOutput").is_none())
    );
    let token = auth::issue_secret();
    let session = uuid::Uuid::new_v4().to_string();
    let codex_plan_reviewer = providers::codex::prepare(
        RoleKind::PlanReviewer,
        "local-child",
        "none",
        &project,
        "no inference",
        &paths.role_socket,
        "t10-credential",
        "0d9a4cfe-dc25-47ac-8229-ade497a2d321",
        &session,
        None,
        &app.hooks,
        &executable,
        &[],
    )
    .unwrap();
    let request = ValidationLaunchRequest {
        operation_id: "t10-launch".into(),
        cell: "L01".into(),
        provider: Provider::Codex,
        role: RoleKind::PlanReviewer,
        project_path: project.clone(),
        model: "local-child".into(),
        effort: "none".into(),
        prompt: "no inference".into(),
    };
    let config = codex_plan_reviewer.config;
    store
        .reserve_validation(
            &request,
            &json_hash(&request).unwrap(),
            "t10-project",
            &project.to_string_lossy(),
            &project.to_string_lossy(),
            "t10-base",
            "t10-task",
            "t10-attempt",
            "t10-context",
            "t10-config",
            "0d9a4cfe-dc25-47ac-8229-ade497a2d321",
            "t10-credential",
            &auth::hash_secret(&token),
            &session,
            "9c53a030-f42e-4b95-a2bc-e5b709d29b25",
            &config,
        )
        .unwrap();

    let control_listener = control::bind(&app).unwrap();
    let role_listener = task_cli::bind(&app).unwrap();
    let (shutdown, _) = tokio::sync::watch::channel(false);
    let control_task = tokio::spawn(control::serve_bound(
        app.clone(),
        shutdown.clone(),
        serde_json::json!({}),
        control_listener,
    ));
    let role_task = tokio::spawn(task_cli::serve_bound(
        app.clone(),
        role_listener,
        shutdown.subscribe(),
    ));
    let manual_duration_seconds = if manual_attachment {
        t10_manual_duration_seconds()
    } else {
        0
    };
    let manual_stop = paths.runtime.join("t10-manual-stop");
    let manual_deadline_millis =
        t10_manual_timestamp_millis() + u128::from(manual_duration_seconds) * 1000;
    let child = PreparedLaunch {
        config,
        executable: executable.clone(),
        arguments: vec![
            "--exact".into(),
            "t10_role_auth_hook_and_human_boundary".into(),
            "--nocapture".into(),
        ],
        environment: vec![
            ("AJ_T10_ROOT".into(), "1".into()),
            ("AJ_T10_CWD".into(), project.to_string_lossy().into_owned()),
            (
                "AJ_T10_CONTROL_SOCKET".into(),
                paths.control_socket.to_string_lossy().into_owned(),
            ),
            (
                "AJ_T10_DESCENDANT_READY".into(),
                root.join("descendant-ready").to_string_lossy().into_owned(),
            ),
            (
                "AJ_T10_DESCENDANT_RELEASE".into(),
                root.join("descendant-release")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "AJ_T10_ROOT_RELEASE".into(),
                root.join("root-release").to_string_lossy().into_owned(),
            ),
            (
                "AGENTICJIRA_ROLE_SOCKET".into(),
                paths.role_socket.to_string_lossy().into_owned(),
            ),
            ("AGENTICJIRA_ROLE_TOKEN".into(), token),
            ("AGENTICJIRA_SESSION_ID".into(), session.clone()),
            (
                "AJ_T10_MANUAL_ATTACH".into(),
                manual_attachment.then_some("1").unwrap_or_default().into(),
            ),
            (
                "AJ_T10_MANUAL_STOP".into(),
                manual_stop.to_string_lossy().into_owned(),
            ),
            (
                "AJ_T10_MANUAL_DEADLINE_MILLIS".into(),
                manual_deadline_millis.to_string(),
            ),
        ],
        supervision_executable: std::path::PathBuf::from(env!("CARGO_BIN_EXE_agenticjira")),
    };
    let identity = app
        .supervisor
        .spawn(
            &session,
            "0d9a4cfe-dc25-47ac-8229-ade497a2d321",
            "9c53a030-f42e-4b95-a2bc-e5b709d29b25",
            &child,
        )
        .unwrap();
    assert_ne!(identity.process_group_id, unsafe { libc::getpgrp() });
    let (anchor_json, provider_pid): (String, i64) = {
        let connection = rusqlite::Connection::open_with_flags(
            &paths.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        connection.query_row(
            "SELECT s.recovery_anchor_json,sp.pid FROM sessions s JOIN session_processes sp ON sp.session_id=s.id WHERE s.id=?1 AND sp.native_start_marker=?2",
            rusqlite::params![session, identity.native_start_marker],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    let leader: agenticjira::domain::ProcessGenerationAnchor =
        serde_json::from_str(&anchor_json).unwrap();
    assert_eq!(leader.pid as i32, identity.process_group_id);
    assert_ne!(leader.pid, identity.pid);
    assert_eq!(provider_pid as u32, identity.pid);
    store
        .update_session_running(
            &session,
            "9c53a030-f42e-4b95-a2bc-e5b709d29b25",
            &serde_json::to_string(&identity).unwrap(),
        )
        .unwrap();

    if manual_attachment {
        t10_manual_attachment_fixture(
            &app,
            &paths,
            &store,
            &session,
            identity.pid,
            manual_duration_seconds,
        )
        .await;
    } else {
        for _ in 0..100 {
            let evidence = store.session_evidence_json(&session).unwrap();
            let diagnostics = app.diagnostics.read_sanitized(100).unwrap();
            if evidence["role_results"] == 1
                && diagnostics
                    .iter()
                    .any(|event| event["event_code"] == "role.ipc.rejected")
                && diagnostics
                    .iter()
                    .any(|event| event["event_code"] == "control.ipc.rejected")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let evidence = store.session_evidence_json(&session).unwrap();
        assert_eq!(
            (
                evidence["hook_events"].as_i64().unwrap(),
                evidence["role_results"].as_i64().unwrap()
            ),
            (1, 1)
        );
        assert!(!evidence["authoritative_completion"].as_bool().unwrap());
        let state = workflow::state(&store).unwrap();
        assert_ne!(state.tasks[0].lifecycle, "done");
        let human = app
            .execute_human_command(&HumanCommand::SetRoleSettings {
                operation_id: "human-separate".into(),
                task_id: "t10-task".into(),
                role: RoleKind::PlanReviewer,
                expected_version: 1,
                config: RoleOverride {
                    provider: Provider::Codex,
                    model: "human-setting".into(),
                    effort: "none".into(),
                },
            })
            .unwrap();
        assert_eq!(human.state, "settings_pending_next_invocation");
        let observation = rusqlite::Connection::open_with_flags(
            &paths.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        std::fs::write(root.join("root-release"), b"release").unwrap();

        for _ in 0..500 {
            app.supervisor.reconcile().unwrap();
            let status: String = observation
                .query_row(
                    "SELECT status FROM sessions WHERE id=?1",
                    rusqlite::params![session],
                    |row| row.get(0),
                )
                .unwrap();
            if status == "recovery_required" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (recovery_state, live_descendants, recovery_records, recovery_detail): (
        String,
        i64,
        i64,
        String,
    ) = observation.query_row(
            "SELECT s.status,
                    (SELECT COUNT(*) FROM session_processes sp WHERE sp.session_id=s.id AND sp.pid!=?2),
                    (SELECT COUNT(*) FROM recovery_records r WHERE r.session_id=s.id AND r.state='attention_required'),
                    (SELECT detail_json FROM recovery_records r WHERE r.session_id=s.id AND r.state='attention_required' LIMIT 1)
             FROM sessions s WHERE s.id=?1",
            rusqlite::params![session, identity.pid],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
        assert_eq!(recovery_state, "recovery_required");
        assert!(live_descendants > 0);
        assert_eq!(recovery_records, 1);
        assert!(recovery_detail.contains("provider root exited"));
        assert!(app
            .supervisor
            .process_group_members(&session)
            .unwrap()
            .iter()
            .any(|member| member["pid"].as_u64() != Some(u64::from(identity.pid))));
        assert!(app.supervisor.interrupt(&session).is_err());
        let cancel_version: i64 = observation
            .query_row("SELECT version FROM tasks WHERE id='t10-task'", [], |row| {
                row.get(0)
            })
            .unwrap();
        app.execute_human_command(&HumanCommand::Control {
            operation_id: "t10-cancel-with-surviving-descendant".into(),
            task_id: "t10-task".into(),
            expected_version: cancel_version,
            action: "cancel".into(),
            payload: serde_json::json!({}),
        })
        .unwrap();
        let pending_cancel = app.coordinator_tick().unwrap();
        assert_eq!(pending_cancel["action"], "recovery_required_for_cancel");
        assert_eq!(
            pending_cancel["public_resolution"],
            "resolve_recovery_cancel"
        );
        std::fs::write(root.join("descendant-release"), b"release").unwrap();
        let mut verified_quiescent = false;
        for _ in 0..500 {
            if agenticjira::recovery::verify_attempt_quiescent(&store, "t10-attempt").is_ok() {
                verified_quiescent = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(verified_quiescent);
        assert!(app
            .supervisor
            .process_group_members(&session)
            .unwrap()
            .is_empty());
        let version: i64 = observation
            .query_row("SELECT version FROM tasks WHERE id='t10-task'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let recovered = app
            .execute_human_command(&HumanCommand::ResolveRecovery {
                operation_id: "t10-root-gone-descendant-cancel".into(),
                task_id: "t10-task".into(),
                attempt_id: "t10-attempt".into(),
                recovery_id: observation.query_row("SELECT id FROM recovery_records WHERE attempt_id='t10-attempt' AND session_id=?1 AND state='attention_required' ORDER BY created_at DESC, rowid DESC LIMIT 1", rusqlite::params![session], |row| row.get::<_, String>(0)).unwrap(),
                session_id: Some(session.clone()),
                expected_version: version,
                decision: "cancel".into(),
                evidence: "exact recorded descendant generation exited before cancellation".into(),
            })
            .unwrap();
        assert_eq!(recovered.state, "recovery_cancelled");
        assert_eq!(
        observation.query_row(
                "SELECT state FROM controls WHERE requested_operation_id='t10-cancel-with-surviving-descendant'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "cancelled"
    );

        for _ in 0..100 {
            if !app
                .supervisor
                .active_session_ids()
                .unwrap()
                .contains(&session)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!app
            .supervisor
            .active_session_ids()
            .unwrap()
            .contains(&session));
    }
    let _ = shutdown.send(true);
    control_task.await.unwrap().unwrap();
    role_task.await.unwrap().unwrap();
    let socket_dir = paths.socket_dir.clone();
    drop(app);
    drop(store);
    match original_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    match original_codex_home {
        Some(value) => std::env::set_var("CODEX_HOME", value),
        None => std::env::remove_var("CODEX_HOME"),
    }
    if manual_attachment {
        println!(
            "AJ_T10_MANUAL_EVIDENCE_RETAINED data_dir={}",
            paths.root.display()
        );
    } else {
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(socket_dir).unwrap();
    }
}
