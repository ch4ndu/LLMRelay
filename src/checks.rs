use crate::{
    snapshot::SnapshotManifest,
    store::{
        browser_launch_receipt_in, reserve_browser_launch_receipt_in, BrowserLaunchReceipt,
        BrowserLaunchReservation, Store,
    },
    supervisor::Supervisor,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckSpec {
    pub executable: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_version")]
    pub suite_version: i64,
    #[serde(default = "default_cwd")]
    pub cwd: PathBuf,
}
fn default_timeout() -> u64 {
    900
}
fn default_version() -> i64 {
    1
}
fn default_cwd() -> PathBuf {
    PathBuf::from(".")
}

fn browser_check_run_result(result: BrowserCheckRun) -> Result<serde_json::Value> {
    match result {
        BrowserCheckRun::Started(result) | BrowserCheckRun::Existing(result) => Ok(result),
    }
}

struct SelectedCheckBinding {
    check_id: String,
    selected_revision: i64,
    inputs_hash: String,
    acceptance_coverage: String,
    authority: SelectedCheckAuthority,
    exact_command_hash: String,
    scope_hash: String,
}

#[derive(Clone)]
enum SelectedCheckAuthority {
    Exact(String),
    Family {
        id: String,
        revision: i64,
        origin_authorization_id: Option<String>,
    },
}

#[derive(Clone)]
pub struct CheckService {
    store: Store,
    supervisor: Supervisor,
    root: PathBuf,
}
#[derive(Clone)]
struct ProcessInfo {
    pid: u32,
    ppid: u32,
    pgid: i32,
    start: String,
}
struct Captured {
    text: String,
    truncated: bool,
}

enum BrowserCheckRun {
    Started(serde_json::Value),
    Existing(serde_json::Value),
}

enum CheckReservation {
    Reserved {
        workspace: String,
        candidate: String,
        snapshot: String,
    },
    Existing(serde_json::Value),
}

impl CheckService {
    pub fn new(store: Store, supervisor: Supervisor, artifacts: PathBuf) -> Self {
        Self {
            store,
            supervisor,
            root: artifacts.join("checks"),
        }
    }

    pub fn run_configured(&self, _attempt: &str, _suite: &str) -> Result<serde_json::Value> {
        bail!("legacy check suites are history and migration inputs only; current TRIP execution requires an authorized selected check_id")
    }

    pub fn run_selected(&self, attempt: &str, check_id: &str) -> Result<serde_json::Value> {
        self.run_selected_inner(attempt, check_id, false, None)
            .and_then(browser_check_run_result)
    }

    pub fn run_selected_browser(
        &self,
        operation_id: &str,
        attempt: &str,
        check_id: &str,
    ) -> Result<serde_json::Value> {
        let request_hash = crate::store::json_hash(&serde_json::json!({
            "attempt_id":attempt,"check_id":check_id,
        }))?;
        if operation_id.trim().is_empty() {
            bail!("operation_id is required")
        }
        if let Some(existing) =
            self.store
                .browser_launch_receipt(operation_id, "check_run", &request_hash)?
        {
            return Ok(existing);
        }
        let receipt = BrowserLaunchReceipt {
            operation_id,
            operation_kind: "check_run",
            input_hash: &request_hash,
            entity_id: check_id,
        };
        match self.run_selected_inner(attempt, check_id, true, Some(receipt)) {
            Ok(BrowserCheckRun::Started(result)) => {
                self.store.finalize_browser_launch_receipt(
                    receipt.operation_id,
                    receipt.operation_kind,
                    receipt.input_hash,
                    Some(result.clone()),
                    None,
                )?;
                Ok(result)
            }
            Ok(BrowserCheckRun::Existing(existing)) => Ok(existing),
            Err(error) => {
                match self.store.browser_launch_receipt(
                    receipt.operation_id,
                    receipt.operation_kind,
                    receipt.input_hash,
                )? {
                    Some(existing)
                        if existing.get("state").and_then(serde_json::Value::as_str)
                            == Some("reserved") =>
                    {
                        self.store.finalize_browser_launch_receipt(
                            receipt.operation_id,
                            receipt.operation_kind,
                            receipt.input_hash,
                            None,
                            Some(format!("{error:#}")),
                        )?;
                        Err(error)
                    }
                    Some(existing) => Ok(existing),
                    None => {
                        self.store.reject_browser_launch_without_reservation(
                            receipt.operation_id,
                            receipt.operation_kind,
                            receipt.input_hash,
                            receipt.entity_id,
                            &format!("{error:#}"),
                        )?;
                        Err(error)
                    }
                }
            }
        }
    }

    fn run_selected_inner(
        &self,
        attempt: &str,
        check_id: &str,
        background: bool,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<BrowserCheckRun> {
        self.store
            .require_execution_unheld("selected check execution")?;
        let (
            workspace,
            candidate,
            revision,
            kind,
            executable,
            arguments,
            shell,
            cwd,
            timeout,
            acceptance,
            relevant,
        ) = {
            let connection = self.store.lock()?;
            crate::trip::require_attempt_ready(&connection, attempt, None)?;
            connection.query_row(
                "SELECT w.path,a.candidate_hash,a.selected_checks_revision,c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd,c.timeout_seconds,c.acceptance_rows_json,c.relevant_inputs_json
                 FROM attempts a JOIN workspaces w ON w.attempt_id=a.id
                 JOIN trip_selected_checks s ON s.attempt_id=a.id AND s.revision=a.selected_checks_revision AND s.check_id=?2 AND s.required=1
                 JOIN trip_verification_checks c ON c.id=s.check_id AND c.enabled=1
                 WHERE a.id=?1 AND a.phase='checks'",
                params![attempt,check_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<String>>(4)?,row.get::<_,Option<String>>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,String>(7)?,row.get::<_,i64>(8)?,row.get::<_,String>(9)?,row.get::<_,String>(10)?))
            ).optional()?.ok_or_else(||anyhow!("selected check is unavailable for the current selection revision"))?
        };
        let command_identity = serde_json::json!({"kind":&kind,"executable":&executable,"arguments":&arguments,"shell":&shell,"cwd":&cwd});
        let command_hash = crate::store::json_hash(&command_identity)?;
        let scope_hash = crate::store::json_hash(
            &serde_json::json!({"attempt_id":attempt,"candidate_hash":&candidate,"check_id":check_id,"selected_revision":revision,"cwd":&cwd}),
        )?;
        let authority = {
            let connection = self.store.lock()?;
            selected_check_authority(
                &connection,
                attempt,
                check_id,
                revision,
                &command_hash,
                &scope_hash,
                &kind,
                executable.as_deref(),
                &cwd,
            )?
            .ok_or_else(|| {
                anyhow!("selected check is denied or awaiting a service-check permission decision")
            })?
        };
        let cwd_path = PathBuf::from(&cwd);
        if cwd_path.is_absolute()
            || cwd_path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("selected check cwd must remain inside the worktree")
        }
        let workspace_path = PathBuf::from(&workspace);
        let resolved_cwd = crate::trip::contained_path(&workspace_path, &cwd, true)?;
        if !resolved_cwd.is_dir() {
            bail!("selected check working directory must be a contained directory")
        }
        let inputs: Vec<String> = serde_json::from_str(&relevant)?;
        let mut input_hashes = Vec::new();
        for relative in inputs {
            let relative_path = PathBuf::from(&relative);
            if relative_path.is_absolute()
                || relative_path.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                bail!("check relevant input escapes the worktree")
            }
            let path = crate::trip::contained_path(&workspace_path, &relative, true)?;
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("inspect check input {relative}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("check relevant input must be a regular non-symlink file: {relative}")
            }
            input_hashes.push(serde_json::json!({"path":relative,"sha256":hex::encode(sha2::Sha256::digest(std::fs::read(path)?))}));
        }
        let inputs_hash = crate::store::json_hash(&input_hashes)?;
        let spec = match kind.as_str() {
            "structured_argv" => CheckSpec {
                executable: executable
                    .ok_or_else(|| anyhow!("structured check executable is missing"))?
                    .into(),
                arguments: serde_json::from_str(arguments.as_deref().unwrap_or("[]"))?,
                timeout_seconds: timeout as u64,
                suite_version: revision,
                cwd: cwd_path,
            },
            "exact_shell" => CheckSpec {
                executable: PathBuf::from("/bin/sh"),
                arguments: vec![
                    "-lc".into(),
                    shell.ok_or_else(|| anyhow!("exact shell command is missing"))?,
                ],
                timeout_seconds: timeout as u64,
                suite_version: revision,
                cwd: cwd_path,
            },
            _ => bail!("unsupported selected check command kind"),
        };
        self.run(
            attempt,
            check_id,
            &spec,
            Some(SelectedCheckBinding {
                check_id: check_id.to_owned(),
                selected_revision: revision,
                inputs_hash,
                acceptance_coverage: acceptance,
                authority,
                exact_command_hash: command_hash,
                scope_hash,
            }),
            background,
            browser_receipt,
        )
    }

    pub fn selected_authorized(&self, attempt: &str, check_id: &str) -> Result<bool> {
        if crate::database::hold_active(&self.store)? {
            return Ok(false);
        }
        let connection = self.store.lock()?;
        let row: Option<(i64,String,Option<String>,Option<String>,Option<String>,String)> = connection.query_row(
            "SELECT a.selected_checks_revision,c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd
             FROM attempts a JOIN trip_selected_checks s ON s.attempt_id=a.id AND s.revision=a.selected_checks_revision
             JOIN trip_verification_checks c ON c.id=s.check_id AND c.enabled=1
             WHERE a.id=?1 AND s.check_id=?2 AND s.required=1 AND a.phase='checks' AND a.candidate_hash IS NOT NULL",
            params![attempt,check_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))
        ).optional()?;
        let Some((revision, kind, executable, arguments, shell, cwd)) = row else {
            return Ok(false);
        };
        let candidate: String = connection.query_row(
            "SELECT candidate_hash FROM attempts WHERE id=?1",
            params![attempt],
            |row| row.get(0),
        )?;
        let command_hash = crate::store::json_hash(
            &serde_json::json!({"kind":kind,"executable":executable,"arguments":arguments,"shell":shell,"cwd":cwd}),
        )?;
        let scope_hash = crate::store::json_hash(
            &serde_json::json!({"attempt_id":attempt,"candidate_hash":candidate,"check_id":check_id,"selected_revision":revision,"cwd":cwd}),
        )?;
        Ok(selected_check_authority(
            &connection,
            attempt,
            check_id,
            revision,
            &command_hash,
            &scope_hash,
            &kind,
            executable.as_deref(),
            &cwd,
        )?
        .is_some())
    }

    fn run(
        &self,
        attempt: &str,
        suite: &str,
        spec: &CheckSpec,
        selected: Option<SelectedCheckBinding>,
        background: bool,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<BrowserCheckRun> {
        validate_spec(spec)?;
        self.supervisor.reconcile()?;
        let id = uuid::Uuid::new_v4().to_string();
        let reservation = self.reserve(
            attempt,
            suite,
            spec,
            &id,
            selected.as_ref(),
            browser_receipt,
        )?;
        let (workspace, candidate, snapshot) = match reservation {
            CheckReservation::Reserved {
                workspace,
                candidate,
                snapshot,
            } => (workspace, candidate, snapshot),
            CheckReservation::Existing(existing) => return Ok(BrowserCheckRun::Existing(existing)),
        };
        if let Err(error) = self.verify_candidate(&snapshot, &workspace, &candidate) {
            self.finish_failed(&id, "precondition_failed", &format!("{error:#}"))?;
            return Err(error);
        }
        let check_root = self.root.join(&id);
        if let Err(error) = std::fs::create_dir_all(&check_root) {
            self.finish_failed(&id, "launch_failed", &format!("{error:#}"))?;
            return Err(error.into());
        }
        let execution_cwd = match crate::trip::contained_path(
            Path::new(&workspace),
            spec.cwd.to_string_lossy().as_ref(),
            true,
        ) {
            Ok(path) => path,
            Err(error) => {
                self.finish_failed(&id, "precondition_failed", &format!("{error:#}"))?;
                return Err(error);
            }
        };
        if !execution_cwd.is_dir() {
            self.finish_failed(
                &id,
                "precondition_failed",
                "selected check working directory is no longer a contained directory",
            )?;
            bail!("selected check working directory is no longer a contained directory")
        }
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.arguments)
            .current_dir(execution_cwd)
            .env_clear();
        for (key, value) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if matches!(
                name.as_ref(),
                "HOME"
                    | "PATH"
                    | "SHELL"
                    | "TMPDIR"
                    | "USER"
                    | "LOGNAME"
                    | "LANG"
                    | "TERM"
                    | "SSL_CERT_FILE"
                    | "SSL_CERT_DIR"
            ) || name.starts_with("LC_")
            {
                command.env(key, value);
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let boot_identity = crate::supervisor::system_boot_identity()?;
        {
            let connection = self.store.lock()?;
            connection.execute("UPDATE check_runs SET launch_state='spawning',launch_boot_identity=?1 WHERE id=?2 AND status='launch_reserved'",params![boot_identity,id])?;
        }
        let mut child = match command
            .spawn()
            .with_context(|| format!("launch configured check {suite}"))
        {
            Ok(child) => child,
            Err(error) => {
                self.finish_failed(&id, "launch_failed", &format!("{error:#}"))?;
                return Err(error);
            }
        };
        let root = child.id();
        let start = match start_marker(root) {
            Ok(start) => start,
            Err(error) => {
                return self
                    .fail_spawned(&id, attempt, &mut child, root, root as i32, None, error)
                    .map(BrowserCheckRun::Started)
            }
        };
        let pgid = unsafe { libc::getpgid(root as libc::pid_t) };
        if pgid <= 0 {
            return self
                .fail_spawned(
                    &id,
                    attempt,
                    &mut child,
                    root,
                    root as i32,
                    Some(&start),
                    anyhow!("check root has no observable process group"),
                )
                .map(BrowserCheckRun::Started);
        }
        let anchor = crate::domain::ProcessGenerationAnchor {
            pid: root,
            process_group_id: pgid,
            native_start_marker: start.clone(),
            boot_identity: boot_identity.clone(),
        };
        let anchor_recorded = (|| -> Result<usize> {
            let connection = self.store.lock()?;
            Ok(connection.execute(
                "UPDATE check_runs SET recovery_root_pid=?1,recovery_process_group_id=?2,recovery_anchor_json=?3 WHERE id=?4",
                params![i64::from(root),pgid,serde_json::to_string(&anchor)?,id],
            )?)
        })();
        match anchor_recorded {
            Ok(1) => {}
            Ok(_) => {
                return self
                    .fail_spawned(
                        &id,
                        attempt,
                        &mut child,
                        root,
                        pgid,
                        Some(&start),
                        anyhow!("check disappeared before its generation anchor was durable"),
                    )
                    .map(BrowserCheckRun::Started)
            }
            Err(error) => {
                return self
                    .fail_spawned(&id, attempt, &mut child, root, pgid, Some(&start), error)
                    .map(BrowserCheckRun::Started)
            }
        }
        if let Err(error) = self.record_process(&id, root, &start, pgid, std::process::id()) {
            return self
                .fail_spawned(&id, attempt, &mut child, root, pgid, Some(&start), error)
                .map(BrowserCheckRun::Started);
        }
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                return self
                    .fail_spawned(
                        &id,
                        attempt,
                        &mut child,
                        root,
                        pgid,
                        Some(&start),
                        anyhow!("check stdout pipe missing"),
                    )
                    .map(BrowserCheckRun::Started)
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                return self
                    .fail_spawned(
                        &id,
                        attempt,
                        &mut child,
                        root,
                        pgid,
                        Some(&start),
                        anyhow!("check stderr pipe missing"),
                    )
                    .map(BrowserCheckRun::Started)
            }
        };
        let running_recorded = match (|| -> Result<usize> {
            let connection = self.store.lock()?;
            Ok(connection.execute("UPDATE check_runs SET status='running',launch_state='started' WHERE id=?1 AND status='launch_reserved'",params![id])?)
        })() {
            Ok(changed) => changed,
            Err(error) => {
                return self
                    .fail_spawned(&id, attempt, &mut child, root, pgid, Some(&start), error)
                    .map(BrowserCheckRun::Started)
            }
        };
        if running_recorded != 1 {
            return self
                .fail_spawned(
                    &id,
                    attempt,
                    &mut child,
                    root,
                    pgid,
                    Some(&start),
                    anyhow!("check launch reservation became stale after spawn"),
                )
                .map(BrowserCheckRun::Started);
        }
        if background {
            let service = self.clone();
            let worker_id = id.clone();
            let worker_attempt = attempt.to_owned();
            let worker_suite = suite.to_owned();
            let worker_spec = spec.clone();
            let worker_workspace = workspace.clone();
            let worker_candidate = candidate.clone();
            let worker_snapshot = snapshot.clone();
            let worker_root = check_root.clone();
            let worker_start = start.clone();
            std::thread::spawn(move || {
                let _ = service.complete_running_check(
                    worker_id,
                    worker_attempt,
                    worker_suite,
                    worker_spec,
                    worker_workspace,
                    worker_candidate,
                    worker_snapshot,
                    worker_root,
                    root,
                    worker_start,
                    pgid,
                    child,
                    stdout,
                    stderr,
                );
            });
            return Ok(BrowserCheckRun::Started(serde_json::json!({
                "check_id":id,"attempt_id":attempt,"candidate_hash":candidate,
                "state":"running","browser_wait":"reservation_and_spawn_only",
            })));
        }
        let execution_started = Instant::now();
        let out_thread = std::thread::spawn(move || drain_bounded(stdout, 512 * 1024));
        let err_thread = std::thread::spawn(move || drain_bounded(stderr, 512 * 1024));
        let mut known = HashMap::from([(root, start.clone())]);
        let deadline = Instant::now() + Duration::from_secs(spec.timeout_seconds);
        let execution = (|| -> Result<_> {
            loop {
                let inventory = process_inventory()?;
                observe_descendants(pgid, &mut known, &inventory);
                self.record_known(&id, &known, &inventory)?;
                if let Some(status) = child.try_wait()? {
                    break Ok((status, false));
                }
                if Instant::now() >= deadline {
                    signal_known(&known, libc::SIGKILL)?;
                    break Ok((child.wait()?, true));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })();
        let (status, timed_out) = match execution {
            Ok(value) => value,
            Err(error) => {
                return self
                    .fail_spawned(&id, attempt, &mut child, root, pgid, Some(&start), error)
                    .map(BrowserCheckRun::Started)
            }
        };
        let quiescence_deadline = Instant::now() + Duration::from_secs(3);
        let quiescence = (|| -> Result<_> {
            loop {
                let inventory = process_inventory()?;
                observe_descendants(pgid, &mut known, &inventory);
                self.record_known(&id, &known, &inventory)?;
                let live = live_known(&known, &inventory);
                if live.is_empty() {
                    break Ok(false);
                }
                signal_processes(&live, libc::SIGKILL)?;
                if Instant::now() >= quiescence_deadline {
                    break Ok(true);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })();
        let descendants_remain = match quiescence {
            Ok(value) => value,
            Err(error) => {
                return self
                    .fail_spawned(&id, attempt, &mut child, root, pgid, Some(&start), error)
                    .map(BrowserCheckRun::Started)
            }
        };
        let stdout = match out_thread.join() {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                return self
                    .finish_capture_failed(&id, error)
                    .map(BrowserCheckRun::Started)
            }
            Err(_) => {
                return self
                    .finish_capture_failed(&id, anyhow!("stdout drain panicked"))
                    .map(BrowserCheckRun::Started)
            }
        };
        let stderr = match err_thread.join() {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                return self
                    .finish_capture_failed(&id, error)
                    .map(BrowserCheckRun::Started)
            }
            Err(_) => {
                return self
                    .finish_capture_failed(&id, anyhow!("stderr drain panicked"))
                    .map(BrowserCheckRun::Started)
            }
        };
        if let Err(error) =
            crate::config::atomic_write(&check_root.join("stdout.log"), stdout.text.as_bytes())
        {
            return self
                .finish_capture_failed(&id, error)
                .map(BrowserCheckRun::Started);
        }
        if let Err(error) =
            crate::config::atomic_write(&check_root.join("stderr.log"), stderr.text.as_bytes())
        {
            return self
                .finish_capture_failed(&id, error)
                .map(BrowserCheckRun::Started);
        }
        let candidate_validation = self.verify_candidate(&snapshot, &workspace, &candidate);
        let candidate_unchanged = candidate_validation.is_ok();
        let candidate_validation_error =
            candidate_validation.err().map(|error| format!("{error:#}"));
        let evidence = serde_json::json!({"suite":suite,"timed_out":timed_out,"process_group_id":pgid,"tracked_processes":known.len(),"descendants_remained_after_kill":descendants_remain,"candidate_unchanged":candidate_unchanged,"candidate_validation_error":candidate_validation_error,"stdout":stdout.text,"stderr":stderr.text,"output_truncated":stdout.truncated||stderr.truncated});
        let persisted = if timed_out || descendants_remain || !candidate_unchanged {
            None
        } else {
            status.code()
        };
        let connection = self.store.lock()?;
        connection.execute("UPDATE check_runs SET status='finished',launch_state='finished',exit_code=?1,evidence_json=?2,finished_at=?3,elapsed_millis=?4,freshness_state=CASE WHEN ?1=0 THEN 'current' ELSE 'failed' END WHERE id=?5 AND status='running'",params![persisted,evidence.to_string(),Utc::now().to_rfc3339(),execution_started.elapsed().as_millis() as i64,id])?;
        Ok(BrowserCheckRun::Started(serde_json::json!({
            "check_id":id,"attempt_id":attempt,"candidate_hash":candidate,
            "exit_code":status.code(),"timed_out":timed_out,
            "passed":status.success()&&!timed_out&&!descendants_remain&&candidate_unchanged,
            "evidence":evidence,
        })))
    }

    #[allow(clippy::too_many_arguments)]
    fn complete_running_check(
        &self,
        id: String,
        attempt: String,
        suite: String,
        spec: CheckSpec,
        workspace: String,
        candidate: String,
        snapshot: String,
        check_root: PathBuf,
        root: u32,
        start: String,
        pgid: i32,
        mut child: Child,
        stdout: ChildStdout,
        stderr: ChildStderr,
    ) -> Result<serde_json::Value> {
        let execution_started = Instant::now();
        let out_thread = std::thread::spawn(move || drain_bounded(stdout, 512 * 1024));
        let err_thread = std::thread::spawn(move || drain_bounded(stderr, 512 * 1024));
        let mut known = HashMap::from([(root, start.clone())]);
        let deadline = Instant::now() + Duration::from_secs(spec.timeout_seconds);
        let execution = (|| -> Result<_> {
            loop {
                let inventory = process_inventory()?;
                observe_descendants(pgid, &mut known, &inventory);
                self.record_known(&id, &known, &inventory)?;
                if let Some(status) = child.try_wait()? {
                    break Ok((status, false));
                }
                if Instant::now() >= deadline {
                    signal_known(&known, libc::SIGKILL)?;
                    break Ok((child.wait()?, true));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })();
        let (status, timed_out) = match execution {
            Ok(value) => value,
            Err(error) => {
                return self.fail_spawned(
                    &id,
                    &attempt,
                    &mut child,
                    root,
                    pgid,
                    Some(&start),
                    error,
                )
            }
        };
        let quiescence_deadline = Instant::now() + Duration::from_secs(3);
        let quiescence = (|| -> Result<_> {
            loop {
                let inventory = process_inventory()?;
                observe_descendants(pgid, &mut known, &inventory);
                self.record_known(&id, &known, &inventory)?;
                let live = live_known(&known, &inventory);
                if live.is_empty() {
                    break Ok(false);
                }
                signal_processes(&live, libc::SIGKILL)?;
                if Instant::now() >= quiescence_deadline {
                    break Ok(true);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })();
        let descendants_remain = match quiescence {
            Ok(value) => value,
            Err(error) => {
                return self.fail_spawned(
                    &id,
                    &attempt,
                    &mut child,
                    root,
                    pgid,
                    Some(&start),
                    error,
                )
            }
        };
        let stdout = match out_thread.join() {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return self.finish_capture_failed(&id, error),
            Err(_) => return self.finish_capture_failed(&id, anyhow!("stdout drain panicked")),
        };
        let stderr = match err_thread.join() {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return self.finish_capture_failed(&id, error),
            Err(_) => return self.finish_capture_failed(&id, anyhow!("stderr drain panicked")),
        };
        if let Err(error) =
            crate::config::atomic_write(&check_root.join("stdout.log"), stdout.text.as_bytes())
        {
            return self.finish_capture_failed(&id, error);
        }
        if let Err(error) =
            crate::config::atomic_write(&check_root.join("stderr.log"), stderr.text.as_bytes())
        {
            return self.finish_capture_failed(&id, error);
        }
        let candidate_validation = self.verify_candidate(&snapshot, &workspace, &candidate);
        let candidate_unchanged = candidate_validation.is_ok();
        let candidate_validation_error =
            candidate_validation.err().map(|error| format!("{error:#}"));
        let evidence = serde_json::json!({
            "suite":suite,"timed_out":timed_out,"process_group_id":pgid,
            "tracked_processes":known.len(),"descendants_remained_after_kill":descendants_remain,
            "candidate_unchanged":candidate_unchanged,
            "candidate_validation_error":candidate_validation_error,
            "stdout":stdout.text,"stderr":stderr.text,
            "output_truncated":stdout.truncated||stderr.truncated,
        });
        let persisted = if timed_out || descendants_remain || !candidate_unchanged {
            None
        } else {
            status.code()
        };
        let connection = self.store.lock()?;
        connection.execute(
            "UPDATE check_runs SET status='finished',launch_state='finished',exit_code=?1,evidence_json=?2,
                    finished_at=?3,elapsed_millis=?4,
                    freshness_state=CASE WHEN ?1=0 THEN 'current' ELSE 'failed' END
             WHERE id=?5 AND status='running'",
            params![
                persisted,
                evidence.to_string(),
                Utc::now().to_rfc3339(),
                execution_started.elapsed().as_millis() as i64,
                id
            ],
        )?;
        Ok(serde_json::json!({
            "check_id":id,"attempt_id":attempt,"candidate_hash":candidate,
            "exit_code":status.code(),"timed_out":timed_out,
            "passed":status.success()&&!timed_out&&!descendants_remain&&candidate_unchanged,
            "evidence":evidence,
        }))
    }

    fn reserve(
        &self,
        attempt: &str,
        suite: &str,
        spec: &CheckSpec,
        id: &str,
        selected: Option<&SelectedCheckBinding>,
        browser_receipt: Option<BrowserLaunchReceipt<'_>>,
    ) -> Result<CheckReservation> {
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = browser_receipt {
            if let Some(existing) = browser_launch_receipt_in(
                &tx,
                receipt.operation_id,
                receipt.operation_kind,
                receipt.input_hash,
            )? {
                return Ok(CheckReservation::Existing(existing));
            }
        }
        let row:(String,String,String,bool,bool)=tx.query_row(
            "SELECT w.path,a.candidate_hash,s.id,EXISTS(SELECT 1 FROM sessions x JOIN role_generations rg ON rg.id=x.role_generation_id WHERE rg.attempt_id=a.id AND rg.role!='manager' AND x.status NOT IN ('exited','launch_failed')),EXISTS(SELECT 1 FROM check_runs cr WHERE cr.attempt_id=a.id AND cr.status IN ('launch_reserved','running','recovery_required')) FROM attempts a JOIN workspaces w ON w.attempt_id=a.id JOIN snapshots s ON s.attempt_id=a.id AND s.kind='candidate' AND s.manifest_hash=a.candidate_hash WHERE a.id=?1 AND a.phase='checks' AND a.status='running' AND w.state='ready' ORDER BY s.created_at DESC LIMIT 1",
            params![attempt],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))
        ).optional()?.ok_or_else(||anyhow!("running checks phase, workspace, or exact frozen candidate is unavailable"))?;
        if row.3 || row.4 {
            bail!("check ownership conflicts with an active role or check")
        }
        crate::trip::require_attempt_ready(&tx, attempt, None)?;
        let cwd = crate::trip::contained_path(
            Path::new(&row.0),
            spec.cwd.to_string_lossy().as_ref(),
            true,
        )?;
        if !cwd.is_dir() {
            bail!("selected check working directory does not exist")
        }
        if let Some(selected) = selected {
            let (revision,kind,executable,arguments,shell,current_cwd):(i64,String,Option<String>,Option<String>,Option<String>,String)=tx.query_row(
                "SELECT a.selected_checks_revision,c.command_kind,c.executable,c.arguments_json,c.shell_command,c.cwd
                 FROM attempts a JOIN trip_selected_checks s ON s.attempt_id=a.id AND s.revision=a.selected_checks_revision
                 JOIN trip_verification_checks c ON c.id=s.check_id AND c.enabled=1
                 WHERE a.id=?1 AND s.check_id=?2 AND s.required=1",
                params![attempt,selected.check_id],|entry|Ok((entry.get(0)?,entry.get(1)?,entry.get(2)?,entry.get(3)?,entry.get(4)?,entry.get(5)?))
            ).optional()?.ok_or_else(||anyhow!("selected check changed before reservation"))?;
            let current_command = crate::store::json_hash(&serde_json::json!({
                "kind":kind,"executable":executable,"arguments":arguments,"shell":shell,"cwd":current_cwd
            }))?;
            if revision != selected.selected_revision
                || current_command != selected.exact_command_hash
            {
                bail!("selected check command or selection revision changed before reservation")
            }
        }
        tx.execute("INSERT INTO check_runs(id,attempt_id,candidate_hash,executable,arguments_json,cwd,status,evidence_json,created_at,suite_name,check_suite_version,launch_state,check_id,selected_check_revision,inputs_hash,acceptance_coverage_json,freshness_state) VALUES(?1,?2,?3,?4,?5,?6,'launch_reserved',?7,?8,?9,?10,'reserved',?11,?12,?13,?14,'reserved')",params![id,attempt,row.1,spec.executable.to_string_lossy(),serde_json::to_string(&spec.arguments)?,cwd.to_string_lossy(),serde_json::json!({"suite":suite,"ownership":"reserved"}).to_string(),Utc::now().to_rfc3339(),suite,spec.suite_version,selected.map(|value|value.check_id.as_str()),selected.map(|value|value.selected_revision),selected.map(|value|value.inputs_hash.as_str()),selected.map(|value|value.acceptance_coverage.as_str()).unwrap_or("[]")])?;
        if let Some(selected) = selected {
            let current_scope = crate::store::json_hash(&serde_json::json!({
                "attempt_id":attempt,"candidate_hash":row.1,"check_id":selected.check_id,
                "selected_revision":selected.selected_revision,"cwd":spec.cwd.to_string_lossy()
            }))?;
            if current_scope != selected.scope_hash {
                bail!("selected check candidate or working-directory scope changed before reservation")
            }
            match &selected.authority {
                SelectedCheckAuthority::Exact(authorization_id) => {
                    let changed=tx.execute(
                        "UPDATE trip_check_authorizations SET consumed_at=CASE WHEN lifetime='once' THEN ?1 ELSE consumed_at END
                         WHERE id=?2 AND attempt_id=?3 AND check_id=?4 AND selected_revision=?5
                           AND exact_command_hash=?6 AND scope_hash=?7 AND decision='approved'
                           AND (lifetime='reusable' OR (lifetime='once' AND consumed_at IS NULL))
                           AND id=(SELECT latest.id FROM trip_check_authorizations latest
                             WHERE latest.attempt_id=trip_check_authorizations.attempt_id
                               AND latest.check_id=trip_check_authorizations.check_id
                               AND latest.selected_revision=trip_check_authorizations.selected_revision
                               AND latest.exact_command_hash=trip_check_authorizations.exact_command_hash
                               AND latest.scope_hash=trip_check_authorizations.scope_hash
                             ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1)",
                        params![Utc::now().to_rfc3339(),authorization_id,attempt,selected.check_id,
                            selected.selected_revision,selected.exact_command_hash,selected.scope_hash],
                    )?;
                    if changed != 1 {
                        bail!("selected check authorization was consumed concurrently")
                    }
                }
                SelectedCheckAuthority::Family {
                    id,
                    revision,
                    origin_authorization_id,
                } => {
                    if let Some(authorization_id) = origin_authorization_id {
                        let origin_current: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM trip_check_authorizations authorization
                             WHERE authorization.id=?1 AND authorization.attempt_id=?2
                               AND authorization.check_id=?3 AND authorization.selected_revision=?4
                               AND authorization.exact_command_hash=?5 AND authorization.scope_hash=?6
                               AND authorization.decision='approved' AND authorization.lifetime='family'
                               AND authorization.matching_rule_id=?7
                               AND authorization.id=(SELECT latest.id FROM trip_check_authorizations latest
                                 WHERE latest.attempt_id=authorization.attempt_id
                                   AND latest.check_id=authorization.check_id
                                   AND latest.selected_revision=authorization.selected_revision
                                   AND latest.exact_command_hash=authorization.exact_command_hash
                                   AND latest.scope_hash=authorization.scope_hash
                                 ORDER BY latest.created_at DESC,latest.rowid DESC LIMIT 1))",
                            params![authorization_id,attempt,selected.check_id,selected.selected_revision,
                                selected.exact_command_hash,selected.scope_hash,id],
                            |row| row.get(0),
                        )?;
                        if !origin_current {
                            bail!("family-origin selected check authorization changed before reservation")
                        }
                    }
                    let denied: bool = tx.query_row(
                        "SELECT COALESCE((SELECT decision='denied' FROM trip_check_authorizations
                         WHERE attempt_id=?1 AND check_id=?2 AND selected_revision=?3
                           AND exact_command_hash=?4 AND scope_hash=?5
                         ORDER BY created_at DESC,rowid DESC LIMIT 1),0)",
                        params![
                            attempt,
                            selected.check_id,
                            selected.selected_revision,
                            selected.exact_command_hash,
                            selected.scope_hash
                        ],
                        |row| row.get(0),
                    )?;
                    if denied {
                        bail!("selected check was denied before reservation")
                    }
                    let (kind,executable,cwd):(String,Option<String>,String)=tx.query_row(
                        "SELECT c.command_kind,c.executable,c.cwd FROM trip_selected_checks s
                         JOIN trip_verification_checks c ON c.id=s.check_id JOIN attempts a ON a.id=s.attempt_id
                         WHERE s.attempt_id=?1 AND s.check_id=?2 AND s.revision=a.selected_checks_revision",
                        params![attempt,selected.check_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
                    let family = crate::permissions::service_check_family(
                        &tx,
                        attempt,
                        &kind,
                        executable.as_deref(),
                        &cwd,
                    )?;
                    if crate::permissions::matching_service_check_rule(&tx, attempt, &family)?
                        != Some((id.clone(), *revision))
                    {
                        bail!("service-check permission rule changed before reservation")
                    }
                    if tx.execute("UPDATE trip_check_permission_rules SET use_count=use_count+1,last_used_at=?1
                        WHERE id=?2 AND revision=?3 AND revoked_at IS NULL",params![Utc::now().to_rfc3339(),id,revision])? != 1 {
                        bail!("service-check permission rule was revoked before reservation")
                    }
                }
            }
        }
        if let Some(receipt) = browser_receipt {
            let selected_authority = selected.map(|selected| match &selected.authority {
                SelectedCheckAuthority::Exact(id) => serde_json::json!({
                    "kind":"exact",
                    "authorization_id":id,
                }),
                SelectedCheckAuthority::Family {
                    id,
                    revision,
                    origin_authorization_id,
                } => serde_json::json!({
                    "kind":"family",
                    "rule_id":id,
                    "rule_revision":revision,
                    "origin_authorization_id":origin_authorization_id,
                }),
            });
            let authority = serde_json::json!({
                "attempt_id":attempt,
                "check_run_id":id,
                "check_id":selected.map(|selected| selected.check_id.as_str()),
                "candidate_hash":&row.1,
                "snapshot_id":&row.2,
                "workspace":&row.0,
                "suite":suite,
                "selected_revision":selected.map(|selected| selected.selected_revision),
                "inputs_hash":selected.map(|selected| selected.inputs_hash.as_str()),
                "command_hash":selected.map(|selected| selected.exact_command_hash.as_str()),
                "scope_hash":selected.map(|selected| selected.scope_hash.as_str()),
                "selected_authority":selected_authority,
                "spec":spec,
            });
            match reserve_browser_launch_receipt_in(
                &tx,
                receipt,
                authority,
                &Utc::now().to_rfc3339(),
            )? {
                BrowserLaunchReservation::Reserved => {}
                BrowserLaunchReservation::Existing(existing) => {
                    return Ok(CheckReservation::Existing(existing));
                }
            }
        }
        tx.commit()?;
        Ok(CheckReservation::Reserved {
            workspace: row.0,
            candidate: row.1,
            snapshot: row.2,
        })
    }

    fn verify_candidate(&self, snapshot: &str, workspace: &str, hash: &str) -> Result<()> {
        let (manifest, repository) = {
            let connection = self.store.lock()?;
            connection.query_row("SELECT s.manifest_json,p.repository_path FROM snapshots s JOIN attempts a ON a.id=s.attempt_id JOIN tasks t ON t.id=a.task_id JOIN projects p ON p.id=t.project_id WHERE s.id=?1 AND s.manifest_hash=?2",params![snapshot,hash],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?
        };
        let expected: SnapshotManifest = serde_json::from_str(&manifest)?;
        let mut repository = crate::workspace::inspect(Path::new(&repository))?;
        repository.head = if expected.original_base.is_empty() {
            expected.snapshot_base.clone()
        } else {
            expected.original_base.clone()
        };
        let temp = self.root.join(format!("verify-{}", uuid::Uuid::new_v4()));
        let (observed, observed_hash) =
            crate::snapshot::capture(&repository, Path::new(workspace), &temp)?;
        std::fs::remove_dir_all(&temp)?;
        if observed_hash != hash || serde_json::to_vec(&observed)? != serde_json::to_vec(&expected)?
        {
            bail!("workspace bytes drifted from exact frozen candidate")
        }
        Ok(())
    }
    fn finish_failed(&self, id: &str, status: &str, error: &str) -> Result<()> {
        let connection = self.store.lock()?;
        connection.execute(
            "UPDATE check_runs SET status=?1,launch_state='failed',launch_error=?2,evidence_json=?3,finished_at=?4 WHERE id=?5",
            params![
                status,
                error,
                serde_json::json!({"error":error}).to_string(),
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }
    fn fail_spawned(
        &self,
        id: &str,
        attempt: &str,
        child: &mut Child,
        root_pid: u32,
        pgid: i32,
        start: Option<&str>,
        error: anyhow::Error,
    ) -> Result<serde_json::Value> {
        self.fail_spawned_with_inventory(
            id,
            attempt,
            child,
            root_pid,
            pgid,
            start,
            error,
            process_inventory,
        )
    }

    fn fail_spawned_with_inventory<F>(
        &self,
        id: &str,
        attempt: &str,
        child: &mut Child,
        root_pid: u32,
        pgid: i32,
        start: Option<&str>,
        error: anyhow::Error,
        inventory: F,
    ) -> Result<serde_json::Value>
    where
        F: Fn() -> Result<Vec<ProcessInfo>>,
    {
        let mut known = HashMap::new();
        if let Some(start) = start {
            known.insert(root_pid, start.to_owned());
            let _ = self.record_process(id, root_pid, start, pgid, std::process::id());
        }
        let before = inventory();
        if let Ok(inventory) = &before {
            observe_descendants(pgid, &mut known, inventory);
            let _ = self.record_known(id, &known, inventory);
            let _ = signal_processes(&live_known(&known, inventory), libc::SIGKILL);
        }
        let _ = unsafe { libc::killpg(pgid, libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
        let detail = format!("{error:#}");
        let after = inventory();
        let (inventory_state, members) = match after {
            Ok(inventory) => {
                observe_descendants(pgid, &mut known, &inventory);
                let members = inventory
                    .into_iter()
                    .filter(|process| {
                        process.pgid == pgid || known.get(&process.pid) == Some(&process.start)
                    })
                    .collect::<Vec<_>>();
                ("observed", members)
            }
            Err(_) => ("unknown", Vec::new()),
        };
        let recovery_required =
            before.is_err() || inventory_state == "unknown" || !members.is_empty();
        let mut connection = self.store.lock()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = Utc::now().to_rfc3339();
        let launch_boot_identity: Option<String> = tx.query_row(
            "SELECT launch_boot_identity FROM check_runs WHERE id=?1",
            params![id],
            |row| row.get(0),
        )?;
        let recovery_anchor = match (start, launch_boot_identity.as_deref()) {
            (Some(start), Some(boot_identity)) => Some(serde_json::to_string(
                &crate::domain::ProcessGenerationAnchor {
                    pid: root_pid,
                    process_group_id: pgid,
                    native_start_marker: start.to_owned(),
                    boot_identity: boot_identity.to_owned(),
                },
            )?),
            _ => None,
        };
        for process in &members {
            tx.execute(
                "INSERT INTO check_processes(check_id,pid,native_start_marker,process_group_id,parent_pid,last_seen_at)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(check_id,pid,native_start_marker) DO UPDATE SET
                   process_group_id=excluded.process_group_id,parent_pid=excluded.parent_pid,last_seen_at=excluded.last_seen_at",
                params![id,i64::from(process.pid),process.start,process.pgid,i64::from(process.ppid),now],
            )?;
        }
        tx.execute(
            "UPDATE check_runs SET status=?1,launch_state='delivery_unknown',launch_error=?2,evidence_json=?3,
             recovery_root_pid=?4,recovery_process_group_id=?5,recovery_anchor_json=COALESCE(?6,recovery_anchor_json),finished_at=CASE WHEN ?7 THEN NULL ELSE ?8 END WHERE id=?9",
            params![
                if recovery_required { "recovery_required" } else { "launch_ambiguous" },
                &detail,
                serde_json::json!({"error":detail,"root_pid":root_pid,"process_group_id":pgid,"pre_cleanup_inventory":if before.is_ok(){"observed"}else{"unknown"},"post_cleanup_inventory":inventory_state,"known_members":members.len(),"quiescence_verified":!recovery_required,"delivery":"unknown"}).to_string(),
                i64::from(root_pid),
                pgid,
                recovery_anchor,
                recovery_required,
                now,
                id,
            ],
        )?;
        if recovery_required {
            tx.execute(
                "UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id=?2",
                params![now, attempt],
            )?;
            tx.execute("UPDATE tasks SET attention='needs_recovery',version=version+1,updated_at=?1 WHERE id=(SELECT task_id FROM attempts WHERE id=?2)",params![now,attempt])?;
            tx.execute(
                "UPDATE claims SET state='unknown',updated_at=?1 WHERE attempt_id=?2",
                params![now, attempt],
            )?;
            tx.execute("INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at) VALUES(?1,NULL,?2,'attention_required',?3,?4,?4)",params![uuid::Uuid::new_v4().to_string(),attempt,serde_json::json!({"kind":"configured_check_delivery_unknown","check_id":id,"root_pid":root_pid,"process_group_id":pgid,"pre_cleanup_inventory":if before.is_ok(){"observed"}else{"unknown"},"post_cleanup_inventory":inventory_state,"known_members":members.len()}).to_string(),now])?;
        }
        tx.commit()?;
        Err(error)
    }
    fn finish_capture_failed(&self, id: &str, error: anyhow::Error) -> Result<serde_json::Value> {
        let detail = format!("{error:#}");
        let connection = self.store.lock()?;
        connection.execute(
            "UPDATE check_runs SET status='finished',launch_state='capture_failed',launch_error=?1,
             evidence_json=?2,exit_code=NULL,finished_at=?3 WHERE id=?4 AND status='running'",
            params![
                &detail,
                serde_json::json!({"capture_failure":detail,"provider_output_in_database":false})
                    .to_string(),
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        Err(error.context("check process ended but output capture failed"))
    }
    fn record_process(&self, id: &str, pid: u32, start: &str, pgid: i32, ppid: u32) -> Result<()> {
        let connection = self.store.lock()?;
        connection.execute("INSERT INTO check_processes(check_id,pid,native_start_marker,process_group_id,parent_pid,last_seen_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(check_id,pid,native_start_marker) DO UPDATE SET process_group_id=excluded.process_group_id,parent_pid=excluded.parent_pid,last_seen_at=excluded.last_seen_at",params![id,pid as i64,start,pgid,ppid as i64,Utc::now().to_rfc3339()])?;
        Ok(())
    }
    fn record_known(
        &self,
        id: &str,
        known: &HashMap<u32, String>,
        inventory: &[ProcessInfo],
    ) -> Result<()> {
        for process in inventory
            .iter()
            .filter(|p| known.get(&p.pid) == Some(&p.start))
        {
            self.record_process(id, process.pid, &process.start, process.pgid, process.ppid)?;
        }
        Ok(())
    }
    pub fn active_status(&self) -> Result<Vec<serde_json::Value>> {
        let running = {
            let connection = self.store.lock()?;
            let mut statement = connection.prepare(
                "SELECT id FROM check_runs WHERE status IN ('launch_reserved','running','recovery_required')",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if running.is_empty() {
            return Ok(Vec::new());
        }
        let inventory =
            process_inventory().context("running check process inventory is unknown")?;
        let connection = self.store.lock()?;
        let mut result = Vec::new();
        for id in running {
            let mut statement = connection
                .prepare("SELECT pid,native_start_marker FROM check_processes WHERE check_id=?1")?;
            let known = statement
                .query_map(params![id], |row| {
                    Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            let live = live_known(&known, &inventory);
            result.push(serde_json::json!({"check_id":id,"state":if live.is_empty(){"running_process_unknown_or_finishing"}else{"running"},"live_pids":live.iter().map(|p|p.pid).collect::<Vec<_>>() }));
        }
        Ok(result)
    }
    pub fn interrupt_all(&self) -> Result<Vec<String>> {
        let inventory = process_inventory()?;
        let connection = self.store.lock()?;
        let mut statement=connection.prepare("SELECT DISTINCT cp.check_id,cp.pid,cp.native_start_marker,cp.process_group_id,cp.parent_pid FROM check_processes cp JOIN check_runs cr ON cr.id=cp.check_id WHERE cr.status IN ('launch_reserved','running','recovery_required')")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ProcessInfo {
                        pid: row.get::<_, i64>(1)? as u32,
                        start: row.get(2)?,
                        pgid: row.get(3)?,
                        ppid: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u32,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        drop(connection);
        let mut ids = Vec::new();
        for (id, process) in rows {
            if inventory
                .iter()
                .any(|p| p.pid == process.pid && p.start == process.start)
            {
                signal_processes(&[process], libc::SIGINT)?;
                if !ids.contains(&id) {
                    ids.push(id)
                }
            }
        }
        Ok(ids)
    }
}

fn selected_check_authority(
    connection: &rusqlite::Connection,
    attempt: &str,
    check_id: &str,
    revision: i64,
    command_hash: &str,
    scope_hash: &str,
    kind: &str,
    executable: Option<&str>,
    cwd: &str,
) -> Result<Option<SelectedCheckAuthority>> {
    let exact: Option<(String, String, String, Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT id,decision,lifetime,consumed_at,matching_rule_id FROM trip_check_authorizations
             WHERE attempt_id=?1 AND check_id=?2 AND selected_revision=?3
               AND exact_command_hash=?4 AND scope_hash=?5
             ORDER BY created_at DESC,rowid DESC LIMIT 1",
            params![attempt, check_id, revision, command_hash, scope_hash],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    if let Some((id, decision, lifetime, consumed, _)) = exact.as_ref() {
        if decision == "denied" {
            return Ok(None);
        }
        if decision == "approved"
            && (lifetime == "reusable" || (lifetime == "once" && consumed.is_none()))
        {
            return Ok(Some(SelectedCheckAuthority::Exact(id.clone())));
        }
    }
    let family = match crate::permissions::service_check_family(
        connection, attempt, kind, executable, cwd,
    ) {
        Ok(family) => family,
        Err(_) => return Ok(None),
    };
    Ok(
        crate::permissions::matching_service_check_rule(connection, attempt, &family)?.map(
            |(id, revision)| {
                let origin_authorization_id = exact.and_then(
                    |(authorization_id, decision, lifetime, _, matching_rule_id)| {
                        (decision == "approved"
                            && lifetime == "family"
                            && matching_rule_id.as_deref() == Some(id.as_str()))
                        .then_some(authorization_id)
                    },
                );
                SelectedCheckAuthority::Family {
                    id,
                    revision,
                    origin_authorization_id,
                }
            },
        ),
    )
}

fn drain_bounded(mut reader: impl Read, limit: usize) -> Result<Captured> {
    let mut tail = VecDeque::with_capacity(limit);
    let mut buffer = [0u8; 16384];
    let mut total = 0usize;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count);
        for byte in &buffer[..count] {
            if tail.len() == limit {
                tail.pop_front();
            }
            tail.push_back(*byte);
        }
    }
    Ok(Captured {
        text: String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned(),
        truncated: total > limit,
    })
}
fn start_marker(pid: u32) -> Result<String> {
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()?;
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    if !output.status.success() || value.is_empty() {
        bail!("cannot observe check PID start identity")
    }
    Ok(value)
}
fn process_inventory() -> Result<Vec<ProcessInfo>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,pgid=,lstart="])
        .output()?;
    if !output.status.success() {
        bail!("cannot inspect check process inventory")
    }
    let mut values = Vec::new();
    for line in String::from_utf8(output.stdout)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 8 {
            bail!("check process inventory contained a malformed row")
        }
        let (Ok(pid), Ok(ppid), Ok(pgid)) =
            (fields[0].parse(), fields[1].parse(), fields[2].parse())
        else {
            bail!("check process inventory contained an unparseable identity row")
        };
        values.push(ProcessInfo {
            pid,
            ppid,
            pgid,
            start: fields[3..8].join(" "),
        })
    }
    if values.is_empty() {
        bail!("check process inventory was empty")
    }
    Ok(values)
}
fn observe_descendants(pgid: i32, known: &mut HashMap<u32, String>, inventory: &[ProcessInfo]) {
    let by_pid = inventory
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut changed = true;
    while changed {
        changed = false;
        for process in inventory {
            let parent = by_pid
                .get(&process.ppid)
                .and_then(|value| known.get(&value.pid).map(|start| start == &value.start))
                .unwrap_or(false);
            if (process.pgid == pgid || parent) && known.get(&process.pid) != Some(&process.start) {
                known.insert(process.pid, process.start.clone());
                changed = true
            }
        }
    }
}
fn live_known(known: &HashMap<u32, String>, inventory: &[ProcessInfo]) -> Vec<ProcessInfo> {
    inventory
        .iter()
        .filter(|process| known.get(&process.pid) == Some(&process.start))
        .cloned()
        .collect()
}
fn signal_known(known: &HashMap<u32, String>, signal: i32) -> Result<()> {
    signal_processes(&live_known(known, &process_inventory()?), signal)
}
fn signal_processes(processes: &[ProcessInfo], signal: i32) -> Result<()> {
    for process in processes {
        if start_marker(process.pid).ok().as_deref() == Some(&process.start) {
            let result = unsafe { libc::kill(process.pid as libc::pid_t, signal) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
    }
    Ok(())
}
fn validate_spec(spec: &CheckSpec) -> Result<()> {
    if !spec.executable.is_absolute() || !spec.executable.is_file() {
        bail!("configured check executable must be an existing absolute file")
    }
    if spec.arguments.len() > 64 || spec.arguments.iter().any(|value| value.len() > 4096) {
        bail!("configured check arguments exceed bounded limits")
    }
    if !(1..=3600).contains(&spec.timeout_seconds) {
        bail!("check timeout must be between 1 and 3600 seconds")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(name: &str) -> (PathBuf, Cleanup) {
        let root =
            std::env::temp_dir().join(format!("agenticjira-check-{name}-{}", uuid::Uuid::new_v4()));
        (root.clone(), Cleanup(root))
    }

    fn seeded_service(root: &Path, check_id: &str) -> (Store, CheckService) {
        std::fs::create_dir_all(root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        {
            let connection = store.lock().unwrap();
            let now = "2026-01-01T00:00:00Z";
            let root_text = root.to_string_lossy().into_owned();
            let identity_text = root.join("identity").to_string_lossy().into_owned();
            connection.execute(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,queue_paused,created_at,updated_at)
                 VALUES('p','Project',?1,?2,'base',0,?3,?3)",
                params![root_text, identity_text, now],
            ).unwrap();
            connection.execute(
                "INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,created_at,updated_at)
                 VALUES('t','p','Task','check recovery','[]','validation',?1,?1)",
                params![now],
            ).unwrap();
            connection.execute(
                "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,scope_hash,configuration_hash,created_at,updated_at)
                 VALUES('a','t','context','checks','base',1,'running','scope','configuration',?1,?1)",
                params![now],
            ).unwrap();
            connection.execute(
                "INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
                 VALUES('claim','t','a',?1,'running',?2,?2)",
                params![identity_text, now],
            ).unwrap();
            connection.execute(
                "INSERT INTO check_runs(id,attempt_id,candidate_hash,executable,arguments_json,cwd,status,evidence_json,created_at,suite_name,check_suite_version,launch_state,launch_boot_identity)
                 VALUES(?1,'a','candidate','/bin/sh','[]',?2,'launch_reserved','{}',?3,'causal',1,'spawning','fixture-boot')",
                params![check_id, root_text, now],
            ).unwrap();
        }
        let supervisor = Supervisor::new(store.clone(), root.join("transcripts"));
        let service = CheckService::new(store.clone(), supervisor, root.join("artifacts"));
        (store, service)
    }

    fn spawned_owned_child() -> (Child, u32, i32, String) {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command.spawn().unwrap();
        let root_pid = child.id();
        let start = start_marker(root_pid).unwrap();
        let pgid = unsafe { libc::getpgid(root_pid as libc::pid_t) };
        assert!(pgid > 0);
        (child, root_pid, pgid, start)
    }

    fn durable_recovery_state(
        store: &Store,
        check_id: &str,
    ) -> (String, String, String, String, String, i64) {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT cr.status,cr.launch_state,a.status,t.attention,c.state,
                    (SELECT COUNT(*) FROM recovery_records r
                     WHERE r.attempt_id=a.id AND r.state='attention_required')
             FROM check_runs cr JOIN attempts a ON a.id=cr.attempt_id
             JOIN tasks t ON t.id=a.task_id JOIN claims c ON c.attempt_id=a.id
             WHERE cr.id=?1",
                params![check_id],
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
            .unwrap()
    }

    #[test]
    fn fail_spawned_commits_recovery_hold_atomically_when_inventory_is_unknown() {
        let (committed_root, _committed_cleanup) = temp_root("recovery");
        let (committed_store, committed_service) =
            seeded_service(&committed_root, "committed-check");
        let (mut child, root_pid, pgid, start) = spawned_owned_child();
        let error = committed_service
            .fail_spawned_with_inventory(
                "committed-check",
                "a",
                &mut child,
                root_pid,
                pgid,
                Some(&start),
                anyhow!("injected post-spawn failure"),
                || Err(anyhow!("injected inventory failure")),
            )
            .unwrap_err();
        assert!(error.to_string().contains("injected post-spawn failure"));
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(
            durable_recovery_state(&committed_store, "committed-check"),
            (
                "recovery_required".into(),
                "delivery_unknown".into(),
                "needs_recovery".into(),
                "needs_recovery".into(),
                "unknown".into(),
                1,
            )
        );
        let detail: serde_json::Value = serde_json::from_str(
            &committed_store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT detail_json FROM recovery_records WHERE attempt_id='a'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(detail["kind"], "configured_check_delivery_unknown");

        let (rolled_back_root, _rolled_back_cleanup) = temp_root("recovery-rollback");
        let (rolled_back_store, rolled_back_service) =
            seeded_service(&rolled_back_root, "rolled-back-check");
        {
            let connection = rolled_back_store.lock().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_check_attention
                 BEFORE UPDATE OF attention ON tasks
                 BEGIN
                   SELECT RAISE(ABORT, 'injected transaction failure');
                 END;",
                )
                .unwrap();
        }
        let (mut child, root_pid, pgid, start) = spawned_owned_child();
        let error = rolled_back_service
            .fail_spawned_with_inventory(
                "rolled-back-check",
                "a",
                &mut child,
                root_pid,
                pgid,
                Some(&start),
                anyhow!("injected post-spawn failure"),
                || Err(anyhow!("injected inventory failure")),
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("injected transaction failure"));
        assert!(child.try_wait().unwrap().is_some());
        assert_eq!(
            durable_recovery_state(&rolled_back_store, "rolled-back-check"),
            (
                "launch_reserved".into(),
                "spawning".into(),
                "running".into(),
                "none".into(),
                "running".into(),
                0,
            )
        );
    }
}
