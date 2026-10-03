use crate::config::InstancePaths;
use crate::domain::{
    DecisionEvidenceState, DecisionOwner, DecisionPrerequisite, RestartCandidateResult,
};
use crate::store::{
    require_current_schema, require_maintenance_schema, Store, CURRENT_SCHEMA_VERSION,
    SERVICE_UPGRADABLE_SCHEMA_VERSIONS, STATE_DATABASE_BUSY_TIMEOUT,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use rusqlite::{backup::Backup, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MANIFEST_SCHEMA: u32 = 1;
const JOURNAL_SCHEMA: u32 = 1;
const HOLD_ID: &str = "database-restore-hold";
const MAX_BACKUPS: usize = 10;
const MAX_BACKUP_AGE_DAYS: i64 = 30;
const MAX_BACKUP_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MIGRATION_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static TEST_AVAILABLE_CAPACITY: std::cell::RefCell<std::collections::VecDeque<u64>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    pub fn acquire(paths: &InstancePaths) -> Result<Self> {
        fs::create_dir_all(&paths.runtime)
            .with_context(|| format!("create runtime directory {}", paths.runtime.display()))?;
        private_directory(&paths.runtime)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&paths.lock_file)
            .with_context(|| format!("open instance lock {}", paths.lock_file.display()))?;
        file.try_lock_exclusive().map_err(|error| {
            anyhow!(
                "another LLMRelay instance owns {}: {error}",
                paths.root.display()
            )
        })?;
        validate_lock_file(&file, &paths.lock_file)?;
        Ok(Self { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn validate_lock_file(file: &File, path: &Path) -> Result<()> {
    let descriptor = file
        .metadata()
        .with_context(|| format!("inspect instance lock descriptor {}", path.display()))?;
    let path_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect instance lock path {}", path.display()))?;
    if !descriptor.is_file()
        || descriptor.uid() != unsafe { libc::geteuid() }
        || descriptor.mode() & 0o077 != 0
        || path_metadata.file_type().is_symlink()
        || path_metadata.dev() != descriptor.dev()
        || path_metadata.ino() != descriptor.ino()
    {
        bail!(
            "instance lock must be the current regular, owner-owned, private file: {}",
            path.display()
        )
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupManifest {
    schema: u32,
    operation_id: String,
    created_at: String,
    application_version: String,
    database_file: String,
    database_bytes: u64,
    database_sha256: String,
    sqlite_schema_version: i64,
    integrity_check: String,
    foreign_key_violations: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum RestorePhase {
    Verified,
    Displacing,
    Installing,
    HoldPending,
    Completed,
    Releasing,
    Released,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalMove {
    source: PathBuf,
    quarantine: PathBuf,
    disposition: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
struct JournalFileBinding {
    path: PathBuf,
    device: u64,
    inode: u64,
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum DisplacedInventory {
    Available {
        evidence: serde_json::Value,
        recorded_boot: Option<String>,
    },
    Unavailable {
        error: String,
        recorded_boot: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RestoreJournal {
    schema: u32,
    operation_id: String,
    backup_manifest_sha256: String,
    backup_database_sha256: String,
    backup_database: PathBuf,
    staged_database: PathBuf,
    quarantine: PathBuf,
    phase: RestorePhase,
    moves: Vec<JournalMove>,
    staged_binding: JournalFileBinding,
    live_family_before: Vec<JournalFileBinding>,
    installed: bool,
    inventory: DisplacedInventory,
    release_evidence: Option<serde_json::Value>,
    created_at: String,
    updated_at: String,
}

pub fn inspect(paths: &InstancePaths) -> Result<serde_json::Value> {
    validate_live_database_family(paths)?;
    let store = Store::open_current_readonly(&paths.database)?;
    let connection = store.lock()?;
    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    Ok(serde_json::json!({
        "database":paths.database,
        "schema_version":CURRENT_SCHEMA_VERSION,
        "application_version":crate::VERSION,
        "page_count":page_count,
        "page_size":page_size,
        "bytes":page_count.saturating_mul(page_size),
        "journal_mode":journal_mode,
        "read_only":true,
    }))
}

pub fn check(paths: &InstancePaths) -> Result<serde_json::Value> {
    validate_live_database_family(paths)?;
    let store = Store::open_current_readonly(&paths.database)?;
    let connection = store.lock()?;
    let (integrity, foreign_keys) = sqlite_checks(&connection)?;
    if integrity != "ok" {
        bail!("database integrity check failed: {integrity}")
    }
    if foreign_keys != 0 {
        bail!("database foreign-key check found {foreign_keys} violation(s)")
    }
    Ok(serde_json::json!({
        "database":paths.database,
        "schema_version":CURRENT_SCHEMA_VERSION,
        "integrity":"ok",
        "foreign_key_violations":0,
        "read_only":true,
    }))
}

pub(crate) fn open_service_locked(paths: &InstancePaths, lock: &InstanceLock) -> Result<Store> {
    validate_lock_file(&lock.file, &paths.lock_file)?;
    validate_live_database_family(paths)?;
    if !paths.database.exists() {
        return Store::open_service(&paths.database);
    }
    let source = Store::open_maintenance_readonly(&paths.database)
        .context("inspect service database before upgrade")?;
    let connection = source.lock()?;
    let transaction = connection
        .unchecked_transaction()
        .context("begin pre-upgrade source read")?;
    let version = require_maintenance_schema(&transaction)?;
    if version == CURRENT_SCHEMA_VERSION {
        drop(transaction);
        drop(connection);
        drop(source);
        return Store::open_service(&paths.database);
    }
    let (integrity, foreign_keys) =
        sqlite_checks(&transaction).context("check source database before upgrade")?;
    if integrity != "ok" || foreign_keys != 0 {
        bail!("source database cannot be upgraded: integrity={integrity}, foreign_key_violations={foreign_keys}")
    }
    let destination = backup_root(paths, None)?;
    validate_backup_destination(paths, &destination)?;
    let (backup_budget, migration_budget) = source_storage_budgets(paths, &transaction)?;
    require_upgrade_capacity(&destination, &paths.state, backup_budget, migration_budget)?;
    create_private_directory(&destination)?;
    let snapshot = publish_backup_locked(&destination, &transaction)
        .context("publish verified pre-upgrade restore point")?;
    drop(transaction);
    drop(connection);
    drop(source);
    let backup = snapshot["backup"]
        .as_str()
        .context("published backup path missing")?;
    // No writable source open is permitted until publication and this second space check.
    require_capacity(&paths.state, migration_budget)
        .with_context(|| format!("migration capacity after verified restore point {backup}"))?;
    Store::open_service(&paths.database).with_context(|| {
        format!(
            "database upgrade from schema {version} failed; verified restore point: {backup}; offline recovery uses `llmrelay database --data-dir <instance> restore <backup>` with this point"
        )
    })
}

pub fn backup(paths: &InstancePaths, configured: Option<&Path>) -> Result<serde_json::Value> {
    paths.create()?;
    let _lock = InstanceLock::acquire(paths)?;
    recover_interrupted_restore_locked(paths)?;
    let destination = backup_root(paths, configured)?;
    validate_live_database_family(paths)?;
    validate_backup_destination(paths, &destination)?;
    let source = Store::open_current_readonly(&paths.database)?;
    let connection = source.lock()?;
    let transaction = connection.unchecked_transaction()?;
    require_current_schema(&transaction)?;
    let (backup_budget, _) = source_storage_budgets(paths, &transaction)?;
    require_capacity(&destination, backup_budget).context("backup destination capacity")?;
    create_private_directory(&destination)?;
    publish_backup_locked(&destination, &transaction)
}

fn publish_backup_locked(destination: &Path, source: &Connection) -> Result<serde_json::Value> {
    let operation_id = uuid::Uuid::new_v4().to_string();
    let staging = destination.join(format!(".{operation_id}.staging"));
    let published = destination.join(format!(
        "{}-{operation_id}",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    create_private_directory(&staging)?;
    let database = staging.join("database.sqlite3");
    let mut target = Connection::open(&database)
        .with_context(|| format!("create backup database {}", database.display()))?;
    {
        let backup = Backup::new(source, &mut target)?;
        backup.run_to_completion(128, Duration::from_millis(5), None)?;
    }
    target.pragma_update(None, "query_only", "ON")?;
    let sqlite_schema_version = require_maintenance_schema(&target)?;
    let (integrity, foreign_keys) = sqlite_checks(&target)?;
    if integrity != "ok" || foreign_keys != 0 {
        bail!(
            "backup verification failed: integrity={integrity}, foreign_key_violations={foreign_keys}"
        )
    }
    drop(target);
    set_private_file(&database)?;
    sync_file(&database)?;
    let database_bytes = fs::metadata(&database)?.len();
    let database_sha256 = sha256_file(&database)?;
    let manifest = BackupManifest {
        schema: MANIFEST_SCHEMA,
        operation_id: operation_id.clone(),
        created_at: Utc::now().to_rfc3339(),
        application_version: crate::VERSION.to_owned(),
        database_file: "database.sqlite3".to_owned(),
        database_bytes,
        database_sha256,
        sqlite_schema_version,
        integrity_check: integrity,
        foreign_key_violations: foreign_keys,
    };
    write_private_json(&staging.join("manifest.json"), &manifest)?;
    sync_directory(&staging)?;
    verify(&staging).context("reread staged backup before publication")?;
    #[cfg(test)]
    if TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH.with(|armed| armed.replace(false)) {
        bail!("injected interruption before backup publication")
    }
    fs::rename(&staging, &published)
        .with_context(|| format!("publish backup {}", published.display()))?;
    sync_directory(destination)?;
    prune_backups(destination, &published)?;
    Ok(serde_json::json!({
        "operation_id":operation_id,
        "backup":published,
        "database_bytes":database_bytes,
        "database_sha256":manifest.database_sha256,
        "verified":true,
    }))
}

pub fn verify(backup: &Path) -> Result<serde_json::Value> {
    let (manifest_path, manifest, database) = load_manifest(backup)?;
    validate_private_backup(&manifest_path, &database)?;
    let actual_bytes = fs::metadata(&database)?.len();
    if actual_bytes != manifest.database_bytes {
        bail!(
            "backup length mismatch: manifest={}, actual={actual_bytes}",
            manifest.database_bytes
        )
    }
    let actual_hash = sha256_file(&database)?;
    if actual_hash != manifest.database_sha256 {
        bail!("backup SHA-256 mismatch")
    }
    let connection = open_sqlite_for_verification(&database)?;
    connection.pragma_update(None, "query_only", "ON")?;
    let actual_schema = require_maintenance_schema(&connection)?;
    if actual_schema != manifest.sqlite_schema_version {
        bail!(
            "backup manifest schema {} does not match actual SQLite schema {actual_schema}",
            manifest.sqlite_schema_version
        )
    }
    let (integrity, foreign_keys) = sqlite_checks(&connection)?;
    if integrity != "ok" || foreign_keys != 0 {
        bail!(
            "backup SQLite verification failed: integrity={integrity}, foreign_key_violations={foreign_keys}"
        )
    }
    Ok(serde_json::json!({
        "backup":manifest_path.parent(),
        "operation_id":manifest.operation_id,
        "database_bytes":actual_bytes,
        "database_sha256":actual_hash,
        "schema_version":manifest.sqlite_schema_version,
        "integrity":"ok",
        "foreign_key_violations":0,
        "authenticity":"not_claimed",
        "verified":true,
    }))
}

pub fn restore(paths: &InstancePaths, backup: &Path) -> Result<serde_json::Value> {
    paths.create()?;
    let _lock = InstanceLock::acquire(paths)?;
    if let Some(previous) = read_journal(paths)? {
        if previous.phase == RestorePhase::Verified {
            return resume_verified_restore(paths, backup, previous);
        }
        recover_interrupted_restore_locked(paths)?;
        let recovered = read_journal(paths)?
            .ok_or_else(|| anyhow!("database restore journal disappeared during recovery"))?;
        if recovered.phase != RestorePhase::Released {
            bail!("a database restore hold already exists; release it before another restore")
        }
        let archived = paths.state.join(format!(
            "database-restore-history-{}.json",
            recovered.operation_id
        ));
        fs::rename(journal_path(paths), &archived).context("archive released restore journal")?;
        sync_directory(&paths.state)?;
    }
    validate_live_database_family(paths)?;
    let verified = verify(backup)?;
    let (_, manifest, backup_database) = load_manifest(backup)?;
    validate_restore_source(paths, &backup_database)?;
    let backup_database = fs::canonicalize(&backup_database)?;
    require_capacity(&paths.state, manifest.database_bytes.saturating_mul(2))?;

    let operation_id = uuid::Uuid::new_v4().to_string();
    let staged_database = paths.state.join(format!(".restore-{operation_id}.sqlite3"));
    copy_private(&backup_database, &staged_database)?;
    if sha256_file(&staged_database)? != manifest.database_sha256 {
        bail!("staged restore database failed SHA-256 verification")
    }
    verify_sqlite_file(&staged_database)?;
    let staged_binding = capture_file_binding(&staged_database)?;
    let inventory = displaced_inventory(paths)?;
    let live_family_before = capture_live_family_bindings(paths)?;
    let quarantine = paths.state.join("database-quarantine").join(&operation_id);
    create_private_directory(&quarantine)?;
    let moves = database_family(paths)
        .into_iter()
        .map(|source| {
            let name = source
                .file_name()
                .context("database family path has no file name")?;
            Ok(JournalMove {
                quarantine: quarantine.join(name),
                source,
                disposition: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let now = Utc::now().to_rfc3339();
    let mut journal = RestoreJournal {
        schema: JOURNAL_SCHEMA,
        operation_id: operation_id.clone(),
        backup_manifest_sha256: sha256_file(&manifest_path(backup)?)?,
        backup_database_sha256: manifest.database_sha256.clone(),
        backup_database,
        staged_database,
        quarantine,
        phase: RestorePhase::Verified,
        moves,
        staged_binding,
        live_family_before,
        installed: false,
        inventory,
        release_evidence: None,
        created_at: now.clone(),
        updated_at: now,
    };
    persist_journal(paths, &journal)?;
    complete_replacement(paths, &mut journal)?;
    establish_restore_hold(paths, &journal)?;
    journal.phase = RestorePhase::Completed;
    journal.updated_at = Utc::now().to_rfc3339();
    persist_journal(paths, &journal)?;
    Ok(restore_result(&journal, &verified))
}

fn resume_verified_restore(
    paths: &InstancePaths,
    backup: &Path,
    mut journal: RestoreJournal,
) -> Result<serde_json::Value> {
    validate_verified_retry(paths, backup, &journal)?;
    let verified = verify(backup)?;
    complete_replacement(paths, &mut journal)?;
    establish_restore_hold(paths, &journal)?;
    journal.phase = RestorePhase::Completed;
    journal.updated_at = Utc::now().to_rfc3339();
    persist_journal(paths, &journal)?;
    Ok(restore_result(&journal, &verified))
}

fn restore_result(journal: &RestoreJournal, verified: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "operation_id":journal.operation_id,
        "backup":verified["backup"],
        "quarantine":journal.quarantine,
        "restore_hold":true,
        "automatic_resume":false,
        "next_action":"reconcile displaced and restored external state, then run `llmrelay database release-hold` offline",
    })
}

pub fn recover_interrupted_restore_locked(paths: &InstancePaths) -> Result<()> {
    let Some(mut journal) = read_journal(paths)? else {
        return Ok(());
    };
    match journal.phase {
        RestorePhase::Verified => bail!(
            "database restore {} stopped before displacement; rerun restore after preserving the staged evidence at {}",
            journal.operation_id,
            journal.staged_database.display()
        ),
        RestorePhase::Displacing | RestorePhase::Installing => {
            complete_replacement(paths, &mut journal)?;
            establish_restore_hold(paths, &journal)?;
            journal.phase = RestorePhase::Completed;
            journal.updated_at = Utc::now().to_rfc3339();
            persist_journal(paths, &journal)
        }
        RestorePhase::HoldPending => {
            establish_restore_hold(paths, &journal)?;
            journal.phase = RestorePhase::Completed;
            journal.updated_at = Utc::now().to_rfc3339();
            persist_journal(paths, &journal)
        }
        RestorePhase::Completed => establish_restore_hold(paths, &journal),
        RestorePhase::Releasing => finish_release(paths, &mut journal),
        RestorePhase::Released => Ok(()),
    }
}

pub fn release_hold(paths: &InstancePaths) -> Result<serde_json::Value> {
    let _lock = InstanceLock::acquire(paths)?;
    recover_interrupted_restore_locked(paths)?;
    let mut journal =
        read_journal(paths)?.ok_or_else(|| anyhow!("no database restore hold exists"))?;
    if journal.phase != RestorePhase::Completed {
        bail!("database restore {} is not complete", journal.operation_id)
    }
    let store = Store::open_maintenance_writable(&paths.database)?;
    let hold = store
        .restore_hold()?
        .ok_or_else(|| anyhow!("restore journal exists but its durable hold is missing"))?;
    if hold["operation_id"].as_str() != Some(journal.operation_id.as_str()) {
        bail!("restore hold does not match the current restore journal")
    }
    if let DisplacedInventory::Unavailable { recorded_boot, .. } = &journal.inventory {
        let current_boot = crate::supervisor::system_boot_identity()
            .context("release requires a current valid OS boot identity")?;
        if !crate::supervisor::boot_identity_proves_reboot(recorded_boot, &current_boot) {
            bail!("displaced database inventory was unavailable; release requires a verified OS reboot after restore plus the remaining reconciliation checks (the recorded and current boot are the same or incomparable)")
        }
    } else if let DisplacedInventory::Available {
        evidence,
        recorded_boot,
    } = &journal.inventory
    {
        verify_displaced_inventory(evidence, recorded_boot.as_deref())?;
    }
    let connection = store.lock()?;
    let prerequisites = restore_prerequisites(&connection, true)?;
    let blockers: Vec<_> = prerequisites
        .iter()
        .filter(|item| item.state != DecisionEvidenceState::Satisfied)
        .collect();
    if !blockers.is_empty() {
        bail!(
            "restore hold cannot be released: {}",
            blockers
                .iter()
                .map(|item| item.message.as_deref().unwrap_or(&item.code))
                .collect::<Vec<_>>()
                .join("; ")
        )
    }
    validate_external_references(&connection)?;
    drop(connection);
    journal.release_evidence = Some(serde_json::json!({
        "verified_at":Utc::now().to_rfc3339(),
        "current_boot":crate::supervisor::system_boot_identity()?,
        "process_claim_check_recovery_prerequisites":0,
        "external_references":"available",
    }));
    journal.phase = RestorePhase::Releasing;
    journal.updated_at = Utc::now().to_rfc3339();
    persist_journal(paths, &journal)?;
    finish_release(paths, &mut journal)?;
    Ok(serde_json::json!({
        "operation_id":journal.operation_id,
        "restore_hold":false,
        "automatic_resume":false,
        "work_state":"paused",
        "message":"restore execution fence released; projects and tasks remain paused for explicit normal recovery",
    }))
}

pub(crate) fn restore_prerequisites(
    connection: &Connection,
    observe_external: bool,
) -> Result<Vec<DecisionPrerequisite>> {
    let mut prerequisites = Vec::new();
    let mut statement = connection.prepare(
        "SELECT 'session',s.id,s.status,a.task_id FROM sessions s
           JOIN role_generations rg ON rg.id=s.role_generation_id JOIN attempts a ON a.id=rg.attempt_id
           WHERE s.status IN ('launch_reserved','launching','running','recovery_required')
         UNION ALL SELECT 'claim',id,state,task_id FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping')
         UNION ALL SELECT 'check',c.id,c.status,a.task_id FROM check_runs c JOIN attempts a ON a.id=c.attempt_id
           WHERE c.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')
         UNION ALL SELECT 'freeze',f.id,f.state,a.task_id FROM freeze_intents f JOIN attempts a ON a.id=f.attempt_id WHERE f.state='recovery_required'
         UNION ALL SELECT 'recovery',r.id,r.state,a.task_id FROM recovery_records r LEFT JOIN attempts a ON a.id=r.attempt_id
           WHERE r.id!='database-restore-hold' AND r.state='attention_required'
         ORDER BY 1,2",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (kind, id, state, task_id) = row?;
        prerequisites.push(DecisionPrerequisite {
            code: format!("restore.{kind}_unresolved"),
            state: DecisionEvidenceState::Missing,
            owner: DecisionOwner::Human,
            evidence: serde_json::json!({"kind":kind,"id":id,"state":state,"task_id":task_id}),
            message: Some(format!("{kind} {id} remains {state}")),
        });
    }
    let mut tasks = connection.prepare("SELECT id,lifecycle,attention FROM tasks WHERE lifecycle NOT IN ('done','cancelled') ORDER BY id")?;
    for task in tasks.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (task_id, lifecycle, attention) = task?;
        prerequisites.push(DecisionPrerequisite {
            code: "restore.affected_task".into(), state: DecisionEvidenceState::Satisfied,
            owner: DecisionOwner::Service,
            evidence: serde_json::json!({"task_id":task_id,"lifecycle":lifecycle,"attention":attention}),
            message: Some(format!("task {task_id} remains paused for explicit normal recovery after hold release")),
        });
    }
    let mut paths = connection
        .prepare("SELECT repository_path FROM projects UNION SELECT path FROM workspaces")?;
    for path in paths.query_map([], |row| row.get::<_, String>(0))? {
        let path = path?;
        let state = if observe_external {
            if Path::new(&path).is_dir() {
                DecisionEvidenceState::Satisfied
            } else {
                DecisionEvidenceState::Missing
            }
        } else {
            DecisionEvidenceState::Unknown
        };
        prerequisites.push(DecisionPrerequisite {
            code: "restore.external_reference".into(), state, owner: DecisionOwner::External,
            evidence: serde_json::json!({"path":path}),
            message: Some(format!("registered repository or workspace path {path} requires offline availability verification")),
        });
    }
    if !observe_external {
        for code in [
            "restore.journal_identity",
            "restore.completed_phase",
            "restore.displaced_inventory",
            "restore.boot_identity",
        ] {
            prerequisites.push(DecisionPrerequisite {
                code: code.into(),
                state: DecisionEvidenceState::Unknown,
                owner: DecisionOwner::External,
                evidence: serde_json::Value::Null,
                message: Some(format!(
                    "{} requires offline verification",
                    code.replace('_', " ")
                )),
            });
        }
    }
    Ok(prerequisites)
}

fn finish_release(paths: &InstancePaths, journal: &mut RestoreJournal) -> Result<()> {
    if journal.release_evidence.is_none() {
        bail!("restore release journal is missing verified reconciliation evidence")
    }
    validate_restore_file_bindings(paths, journal)?;
    let store = Store::open_maintenance_writable(&paths.database)?;
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    tx.execute("UPDATE projects SET queue_paused=1,updated_at=?1", [&now])?;
    tx.execute("UPDATE tasks SET attention='paused',updated_at=?1 WHERE lifecycle NOT IN ('done','cancelled')", [&now])?;
    tx.execute(
        "UPDATE sessions SET desired_running=0 WHERE desired_running!=0",
        [],
    )?;
    tx.execute("UPDATE instance_settings SET auto_resume_eligible=0,version=version+1,updated_at=?1 WHERE singleton=1 AND auto_resume_eligible!=0", [&now])?;
    tx.execute(
        "UPDATE recovery_records SET state='resolved_quiescent',resolved_at=?1,updated_at=?1
         WHERE id=?2 AND state='attention_required'",
        rusqlite::params![now, HOLD_ID],
    )?;
    tx.commit()?;
    drop(connection);
    journal.phase = RestorePhase::Released;
    journal.updated_at = Utc::now().to_rfc3339();
    persist_journal(paths, journal)
}

pub(crate) fn hold_active(store: &Store) -> Result<bool> {
    Ok(store.restore_hold()?.is_some())
}

fn complete_replacement(paths: &InstancePaths, journal: &mut RestoreJournal) -> Result<()> {
    validate_restore_journal(paths, journal)?;
    validate_restore_file_bindings(paths, journal)?;
    if journal.phase == RestorePhase::Verified {
        journal.phase = RestorePhase::Displacing;
        journal.updated_at = Utc::now().to_rfc3339();
        persist_journal(paths, journal)?;
    }
    if journal.phase == RestorePhase::Displacing {
        for index in 0..journal.moves.len() {
            validate_restore_file_bindings(paths, journal)?;
            if journal.moves[index].disposition.is_some() {
                continue;
            }
            let source = &journal.moves[index].source;
            let quarantine = &journal.moves[index].quarantine;
            if source.exists() {
                fs::rename(source, quarantine)
                    .with_context(|| format!("quarantine database file {}", source.display()))?;
                sync_directory(&journal.quarantine)?;
                sync_directory(&paths.state)?;
                journal.moves[index].disposition = Some("moved".to_owned());
            } else if quarantine.exists() {
                journal.moves[index].disposition = Some("moved".to_owned());
            } else {
                journal.moves[index].disposition = Some("absent".to_owned());
            }
            journal.updated_at = Utc::now().to_rfc3339();
            persist_journal(paths, journal)?;
        }
        validate_restore_file_bindings(paths, journal)?;
        journal.phase = RestorePhase::Installing;
        journal.updated_at = Utc::now().to_rfc3339();
        persist_journal(paths, journal)?;
    }
    if journal.phase == RestorePhase::Installing {
        validate_restore_file_bindings(paths, journal)?;
        if !journal.installed {
            if journal.staged_database.exists() {
                fs::rename(&journal.staged_database, &paths.database)
                    .context("install restored database")?;
                sync_directory(&paths.state)?;
            } else if !paths.database.exists() {
                bail!("restore install is ambiguous: both staged and live databases are missing")
            }
            validate_restore_file_bindings(paths, journal)?;
            verify_sqlite_file(&paths.database)?;
            if sha256_file(&paths.database)? != journal.backup_database_sha256 {
                bail!("installed restore database does not match the verified backup")
            }
            validate_restore_file_bindings(paths, journal)?;
            journal.installed = true;
            journal.updated_at = Utc::now().to_rfc3339();
            persist_journal(paths, journal)?;
        }
        validate_restore_file_bindings(paths, journal)?;
        journal.phase = RestorePhase::HoldPending;
        journal.updated_at = Utc::now().to_rfc3339();
        persist_journal(paths, journal)?;
    }
    Ok(())
}

fn establish_restore_hold(paths: &InstancePaths, journal: &RestoreJournal) -> Result<()> {
    validate_restore_file_bindings(paths, journal)?;
    let store = Store::open_maintenance_writable(&paths.database)?;
    let mut connection = store.lock()?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = Utc::now().to_rfc3339();
    let detail = serde_json::json!({
        "kind":"database_restore_hold",
        "operation_id":journal.operation_id.as_str(),
        "backup_database_sha256":journal.backup_database_sha256.as_str(),
        "quarantine":&journal.quarantine,
        "inventory":&journal.inventory,
        "automatic_resume":false,
        "release":"offline_only",
    });
    tx.execute(
        "INSERT INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at)
         VALUES(?1,NULL,NULL,'attention_required',?2,?3,?3)
         ON CONFLICT(id) DO UPDATE SET session_id=NULL,attempt_id=NULL,state='attention_required',
           process_identity_json=NULL,detail_json=excluded.detail_json,resolved_at=NULL,
           created_at=CASE
             WHEN json_extract(recovery_records.detail_json,'$.operation_id')=
                  json_extract(excluded.detail_json,'$.operation_id')
             THEN recovery_records.created_at ELSE excluded.created_at END,
           updated_at=excluded.updated_at",
        rusqlite::params![HOLD_ID, detail.to_string(), now],
    )?;
    tx.execute(
        "UPDATE recovery_records SET state='resolved_superseded',resolved_at=?1,updated_at=?1
         WHERE id!=?2 AND state='attention_required'
           AND json_extract(detail_json,'$.kind') IN (
             'database_restore_session','database_restore_check',
             'database_restore_claim','database_restore_freeze')
           AND COALESCE(json_extract(detail_json,'$.operation_id'),'')!=?3",
        rusqlite::params![now, HOLD_ID, journal.operation_id],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
         SELECT ?1 || ':session:' || s.id,s.id,rg.attempt_id,'attention_required',s.process_identity_json,
                json_object('kind','database_restore_session','operation_id',?1,'automatic_signal',0),?2,?2
         FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
         WHERE s.status IN ('launch_reserved','launching','running','recovery_required')",
        rusqlite::params![journal.operation_id, now],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at)
         SELECT ?1 || ':check:' || c.id,NULL,c.attempt_id,'attention_required',
                json_object('kind','database_restore_check','operation_id',?1,'check_id',c.id,'automatic_signal',0),?2,?2
         FROM check_runs c
         WHERE c.status IN ('launch_reserved','running','recovery_required','launch_ambiguous')",
        rusqlite::params![journal.operation_id, now],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
         SELECT ?1 || ':claim:' || c.id,NULL,c.attempt_id,'attention_required',c.process_identity_json,
                json_object('kind','database_restore_claim','operation_id',?1,'claim_id',c.id,
                  'prior_state',c.state,'automatic_signal',0),?2,?2
         FROM claims c
         WHERE c.state IN ('reserved','launching','running','unknown','stopping')
           AND NOT EXISTS(
             SELECT 1 FROM sessions s JOIN role_generations rg ON rg.id=s.role_generation_id
             WHERE rg.attempt_id=c.attempt_id
               AND s.status IN ('launch_reserved','launching','running','recovery_required'))
           AND NOT EXISTS(
             SELECT 1 FROM check_runs check_run WHERE check_run.attempt_id=c.attempt_id
               AND check_run.status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))",
        rusqlite::params![journal.operation_id, now],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO recovery_records(id,session_id,attempt_id,state,detail_json,created_at,updated_at)
         SELECT ?1 || ':freeze:' || freeze.id,NULL,freeze.attempt_id,'attention_required',
                json_object('kind','database_restore_freeze','operation_id',?1,'freeze_id',freeze.id,'automatic_signal',0),?2,?2
         FROM freeze_intents freeze WHERE freeze.state IN ('reserved','capturing','recovery_required')",
        rusqlite::params![journal.operation_id, now],
    )?;
    tx.execute("UPDATE sessions SET status='recovery_required',desired_running=0,updated_at=?1 WHERE status IN ('launch_reserved','launching','running')", [&now])?;
    tx.execute("UPDATE check_runs SET status='recovery_required' WHERE status IN ('launch_reserved','running','launch_ambiguous')", [])?;
    tx.execute("UPDATE claims SET state='unknown',updated_at=?1 WHERE state IN ('reserved','launching','running','stopping')", [&now])?;
    tx.execute("UPDATE attempts SET status='needs_recovery',updated_at=?1 WHERE id IN (SELECT attempt_id FROM recovery_records WHERE state='attention_required' AND attempt_id IS NOT NULL)", [&now])?;
    tx.execute("UPDATE tasks SET attention='needs_recovery',updated_at=?1 WHERE id IN (SELECT task_id FROM attempts WHERE status='needs_recovery')", [&now])?;
    tx.execute(
        "UPDATE role_credentials SET revoked_at=COALESCE(revoked_at,?1)",
        [&now],
    )?;
    tx.execute("UPDATE permission_rules SET revoked_at=COALESCE(revoked_at,?1),revoked_by=COALESCE(revoked_by,'database_restore'),revoke_reason=COALESCE(revoke_reason,'database restore invalidated prior authority')", [&now])?;
    tx.execute("UPDATE trip_check_permission_rules SET revoked_at=COALESCE(revoked_at,?1),revoked_by=COALESCE(revoked_by,'database_restore'),revoke_reason=COALESCE(revoke_reason,'database restore invalidated prior authority')", [&now])?;
    tx.execute(
        "UPDATE trip_check_authorizations SET consumed_at=COALESCE(consumed_at,?1)",
        [&now],
    )?;
    tx.execute("UPDATE permission_requests SET state='expired',decision_kind=COALESCE(decision_kind,'expired'),decision_actor=COALESCE(decision_actor,'database_restore'),decision_reason=COALESCE(decision_reason,'database restore invalidated pending authority'),decided_at=COALESCE(decided_at,?1),revision=revision+1,updated_at=?1 WHERE state IN ('pending','approved_once','approved_rule')", [&now])?;
    tx.execute(
        "UPDATE input_leases SET revoked_at=COALESCE(revoked_at,?1),updated_at=?1",
        [&now],
    )?;
    tx.execute("UPDATE launch_permits SET state='released',consumed_at=COALESCE(consumed_at,?1) WHERE state IN ('issued','reserved')", [&now])?;
    tx.execute("UPDATE instance_settings SET auto_resume_eligible=0,version=version+1,updated_at=?1 WHERE singleton=1 AND auto_resume_eligible!=0", [&now])?;
    tx.execute("UPDATE projects SET queue_paused=1,updated_at=?1", [&now])?;
    tx.execute("UPDATE tasks SET attention=CASE WHEN attention='none' THEN 'pause_requested' ELSE attention END,updated_at=?1 WHERE lifecycle NOT IN ('done','cancelled')", [&now])?;
    let restart_candidates = {
        let mut statement = tx.prepare(
            "SELECT session_id,state,result_json FROM restart_candidates
             WHERE state NOT IN ('resumed','released_fresh_dispatch','cancelled')",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (session_id, state, result_json) in restart_candidates {
        let mut result = RestartCandidateResult::parse(&result_json)?;
        result.validate_candidate_state(&state, &session_id)?;
        result.restart.terminalize_batch();
        result.set(
            "restore_cancellation",
            serde_json::json!({"operation_id":journal.operation_id.as_str()}),
        );
        tx.execute(
            "UPDATE restart_candidates SET state='cancelled',
                    reason='database restore requires explicit normal recovery',result_json=?1,updated_at=?2
             WHERE session_id=?3 AND state NOT IN ('resumed','released_fresh_dispatch','cancelled')",
            rusqlite::params![result.encode()?, now, session_id],
        )?;
    }
    tx.execute("UPDATE guidance_messages SET state='abandoned',reason='database restore invalidated ambiguous or in-flight delivery' WHERE state IN ('delivery_reserved','written_awaiting_submit','delivery_unknown')", [])?;
    tx.execute("UPDATE review_requests SET delivery_state='abandoned',updated_at=?1 WHERE delivery_state IN ('reserved','launching','delivered','ambiguous')", [&now])?;
    tx.execute(
        "UPDATE controls SET state='cancelled',updated_at=?1 WHERE state='requested'",
        [&now],
    )?;
    tx.execute("UPDATE switch_intents SET state='cancelled',updated_at=?1 WHERE state NOT IN ('finished','cancelled','superseded','rejected')", [&now])?;
    tx.execute("UPDATE rework_intents SET state='failed',result_json=json_set(result_json,'$.restore_hold','database restore invalidated prior transition authority'),updated_at=?1 WHERE state NOT IN ('completed','failed','cancelled')", [&now])?;
    tx.execute("UPDATE trip_setup_permits SET state='revoked',consumed_at=COALESCE(consumed_at,?1) WHERE state='issued'", [&now])?;
    tx.execute("UPDATE trip_runtime_admissions SET state='failed',failure_reason='database restore invalidated prior runtime authority',updated_at=?1 WHERE state IN ('pending_approval','authorized','running','awaiting_publication')", [&now])?;
    tx.execute("UPDATE trip_runtime_probes SET state='failed',failure_reason='database restore invalidated prior runtime authority',updated_at=?1 WHERE state IN ('pending_approval','authorized','running','awaiting_resume','evidence_recorded')", [&now])?;
    tx.execute("UPDATE freeze_intents SET state='recovery_required',error='database restore requires explicit recovery',updated_at=?1 WHERE state IN ('reserved','capturing')", [&now])?;
    tx.execute("UPDATE cmux_session_surfaces SET surface_state='lost',attachment_state='ended',actual_input_state='lost',last_error='database restore invalidated prior presentation binding',updated_at=?1 WHERE surface_state IN ('opening','open','unknown')", [&now])?;
    tx.execute("UPDATE cmux_task_workspaces SET state='retired',last_error='database restore invalidated prior workspace binding',updated_at=?1 WHERE state IN ('opening','open','unknown')", [&now])?;
    tx.commit()?;
    Ok(())
}

fn open_displaced_readonly(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open displaced database read-only {}", path.display()))?;
    connection.busy_timeout(STATE_DATABASE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "query_only", "ON")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    // SQLite opens lazily; this read separates unreadable data from incompatible inventory SQL.
    connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    Ok(connection)
}

fn displaced_data_is_unreadable(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(failure.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                | rusqlite::ErrorCode::SystemIoFailure | rusqlite::ErrorCode::CannotOpen
                | rusqlite::ErrorCode::PermissionDenied)
    )
}

fn displaced_inventory(paths: &InstancePaths) -> Result<DisplacedInventory> {
    if !paths.database.exists() {
        return Ok(DisplacedInventory::Available {
            evidence: serde_json::json!({
                "database":"absent",
                "sessions":[],
                "checks":[],
                "claims":[],
                "worktrees":[],
                "cmux":[],
                "cmux_workspaces":[],
            }),
            recorded_boot: None,
        });
    }
    let connection = match open_displaced_readonly(&paths.database) {
        Ok(connection) => connection,
        Err(error) if displaced_data_is_unreadable(&error) => {
            let recorded_boot = crate::supervisor::system_boot_identity().context(
                "displaced database is unreadable and a valid OS boot identity could not be recorded before displacement",
            )?;
            return Ok(DisplacedInventory::Unavailable {
                error: format!("{error:#}"),
                recorded_boot,
            });
        }
        Err(error) => return Err(error).context("read displaced database before restore"),
    };
    require_maintenance_schema(&connection)?;
    Ok(DisplacedInventory::Available {
        evidence: capture_inventory(&connection)
            .context("capture inventory from readable displaced database")?,
        recorded_boot: crate::supervisor::system_boot_identity().ok(),
    })
}

fn capture_inventory(connection: &Connection) -> Result<serde_json::Value> {
    let sessions = collect_json_rows(
        connection,
        "SELECT json_object('kind','session','id',id,'status',status,'process_identity',process_identity_json,'recovery_process_group_id',recovery_process_group_id,'recovery_anchor',recovery_anchor_json,'launch_boot_identity',launch_boot_identity,'native_session_id',native_session_id) FROM sessions WHERE status IN ('launch_reserved','launching','running','recovery_required')",
    )?;
    let checks = collect_json_rows(
        connection,
        "SELECT json_object('kind','check','id',id,'status',status,'recovery_root_pid',recovery_root_pid,'recovery_process_group_id',recovery_process_group_id,'recovery_anchor',recovery_anchor_json,'launch_boot_identity',launch_boot_identity,'cwd',cwd) FROM check_runs WHERE status IN ('launch_reserved','running','recovery_required','launch_ambiguous')",
    )?;
    let claims = collect_json_rows(
        connection,
        "SELECT json_object('kind','claim','id',id,'state',state,'repository_identity',repository_identity,'process_identity',process_identity_json) FROM claims WHERE state IN ('reserved','launching','running','unknown','stopping')",
    )?;
    let worktrees = collect_json_rows(
        connection,
        "SELECT json_object('kind','worktree','id',id,'path',path,'state',state) FROM workspaces",
    )?;
    let cmux = collect_json_rows(
        connection,
        "SELECT json_object('kind','cmux','id',id,'workspace_id',workspace_id,'surface_id',surface_id,'process_identity',process_identity_json) FROM cmux_session_surfaces WHERE surface_state IN ('opening','open','unknown')",
    )?;
    let cmux_workspaces = collect_json_rows(
        connection,
        "SELECT json_object('kind','cmux_workspace','id',id,'task_id',task_id,'workspace_id',workspace_id,'opening_surface_id',opening_surface_id,'state',state) FROM cmux_task_workspaces WHERE state IN ('opening','open','unknown')",
    )?;
    Ok(serde_json::json!({
        "sessions":sessions,
        "checks":checks,
        "claims":claims,
        "worktrees":worktrees,
        "cmux":cmux,
        "cmux_workspaces":cmux_workspaces,
    }))
}

fn collect_json_rows(connection: &Connection, sql: &str) -> Result<Vec<serde_json::Value>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| {
        let value = row?;
        serde_json::from_str(&value).map_err(Into::into)
    })
    .collect()
}

fn validate_external_references(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare("SELECT repository_path FROM projects UNION SELECT path FROM workspaces")?;
    let paths = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let missing: Vec<_> = paths
        .into_iter()
        .filter(|path| !Path::new(path).is_dir())
        .collect();
    if !missing.is_empty() {
        bail!(
            "restore hold cannot be released: registered repository paths remain unavailable: {}",
            missing.join(", ")
        )
    }
    Ok(())
}

fn verify_displaced_inventory(
    evidence: &serde_json::Value,
    recorded_boot: Option<&str>,
) -> Result<()> {
    let current_boot = crate::supervisor::system_boot_identity()?;
    let inventory = crate::recovery::process_inventory()?;
    for category in ["sessions", "checks", "claims", "cmux"] {
        let entries = evidence
            .get(category)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("displaced inventory is missing {category}"))?;
        for entry in entries {
            let kind = entry["kind"].as_str().unwrap_or(category);
            let id = entry["id"].as_str().unwrap_or("unknown");
            let process = entry["process_identity"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(serde_json::from_str::<crate::domain::ProcessIdentity>)
                .transpose()
                .with_context(|| format!("parse displaced {kind} process identity {id}"))?;
            let anchor = entry["recovery_anchor"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(serde_json::from_str::<crate::domain::ProcessGenerationAnchor>)
                .transpose()
                .with_context(|| format!("parse displaced {kind} generation anchor {id}"))?;
            let recorded = process
                .as_ref()
                .map(|process| {
                    vec![(
                        process.pid,
                        process.native_start_marker.clone(),
                        process.process_group_id,
                    )]
                })
                .unwrap_or_default();
            let group = entry["recovery_process_group_id"]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .or_else(|| process.as_ref().map(|value| value.process_group_id));
            let verification = crate::recovery::verify_generation_absent_evidence(
                kind,
                id,
                &recorded,
                group,
                anchor.as_ref(),
                entry["launch_boot_identity"].as_str(),
                &current_boot,
                &inventory,
            );
            if let Err(error) = verification {
                if !recorded_boot.is_some_and(|boot| {
                    crate::supervisor::boot_identity_proves_reboot(boot, &current_boot)
                }) {
                    return Err(error).with_context(|| {
                        format!("displaced {kind} {id} is not reconciled; a verified later OS boot can establish process quiescence")
                    });
                }
            }
        }
    }
    let worktrees = evidence
        .get("worktrees")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("displaced inventory is missing worktrees"))?;
    let unavailable = worktrees
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .filter(|path| !Path::new(path).is_dir())
        .collect::<Vec<_>>();
    if !unavailable.is_empty() {
        bail!(
            "displaced worktree references remain unavailable: {}",
            unavailable.join(", ")
        )
    }
    Ok(())
}

fn sqlite_checks(connection: &Connection) -> Result<(String, u64)> {
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    let mut violations = 0_u64;
    while rows.next()?.is_some() {
        violations += 1;
    }
    Ok((integrity, violations))
}

fn open_sqlite_for_verification(path: &Path) -> Result<Connection> {
    let mut uri = Vec::with_capacity(path.as_os_str().as_bytes().len() + 32);
    uri.extend_from_slice(b"file:");
    for byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(*byte);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            uri.extend_from_slice(&[
                b'%',
                HEX[(*byte >> 4) as usize],
                HEX[(*byte & 0x0f) as usize],
            ]);
        }
    }
    uri.extend_from_slice(b"?immutable=1");
    // Verification must inspect only the bound main file, never create or consume sidecars.
    Connection::open_with_flags(
        PathBuf::from(OsString::from_vec(uri)),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open SQLite database for verification {}", path.display()))
}

fn verify_sqlite_file(path: &Path) -> Result<()> {
    let connection = open_sqlite_for_verification(path)?;
    connection.pragma_update(None, "query_only", "ON")?;
    require_maintenance_schema(&connection)?;
    let (integrity, foreign_keys) = sqlite_checks(&connection)?;
    if integrity != "ok" || foreign_keys != 0 {
        bail!(
            "SQLite verification failed for {}: integrity={integrity}, foreign_key_violations={foreign_keys}",
            path.display()
        )
    }
    Ok(())
}

fn load_manifest(backup: &Path) -> Result<(PathBuf, BackupManifest, PathBuf)> {
    let manifest_path = manifest_path(backup)?;
    let bytes = fs::read(&manifest_path)
        .with_context(|| format!("read backup manifest {}", manifest_path.display()))?;
    let manifest: BackupManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse backup manifest {}", manifest_path.display()))?;
    if manifest.schema != MANIFEST_SCHEMA
        || (manifest.sqlite_schema_version != CURRENT_SCHEMA_VERSION
            && !SERVICE_UPGRADABLE_SCHEMA_VERSIONS.contains(&manifest.sqlite_schema_version))
        || manifest.database_file != "database.sqlite3"
        || manifest.integrity_check != "ok"
        || manifest.foreign_key_violations != 0
    {
        bail!("backup manifest is unsupported or was not recorded as verified")
    }
    let parent = manifest_path
        .parent()
        .context("backup manifest has no parent")?;
    let database = parent.join(&manifest.database_file);
    Ok((manifest_path, manifest, database))
}

fn manifest_path(backup: &Path) -> Result<PathBuf> {
    if backup.is_dir() {
        Ok(backup.join("manifest.json"))
    } else if backup
        .file_name()
        .is_some_and(|name| name == "manifest.json")
    {
        Ok(backup.to_owned())
    } else {
        bail!("backup must name a snapshot directory or manifest.json")
    }
}

fn backup_root(paths: &InstancePaths, configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(configured) = configured {
        if !configured.is_absolute() {
            bail!("--backup-dir must be an absolute path")
        }
        return resolve_missing(configured);
    }
    let parent = paths.root.parent().context("instance root has no parent")?;
    let name = paths
        .root
        .file_name()
        .context("instance root has no directory name")?;
    resolve_missing(&parent.join(format!("{}.backups", name.to_string_lossy())))
}

fn validate_backup_destination(paths: &InstancePaths, destination: &Path) -> Result<()> {
    let root = resolve_missing(&paths.root)?;
    if destination.starts_with(&root) || root.starts_with(destination) {
        bail!("backup destination must not overlap the live instance tree")
    }
    if paths.database.exists() {
        let store = Store::open_maintenance_readonly(&paths.database)?;
        let connection = store.lock()?;
        let mut statement = connection.prepare("SELECT repository_path FROM projects")?;
        let repositories = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for repository in repositories {
            let repository = resolve_missing(Path::new(&repository))?;
            if destination.starts_with(&repository) || repository.starts_with(destination) {
                bail!(
                    "backup destination overlaps registered repository {}",
                    repository.display()
                )
            }
        }
    }
    Ok(())
}

fn validate_restore_source(paths: &InstancePaths, database: &Path) -> Result<()> {
    let source = fs::canonicalize(database)?;
    let root = resolve_missing(&paths.root)?;
    if source.starts_with(&root) {
        bail!("restore source must not be inside the live instance tree")
    }
    if paths.database.exists() {
        let connection = match open_displaced_readonly(&paths.database) {
            Ok(connection) => connection,
            Err(error) if displaced_data_is_unreadable(&error) => return Ok(()),
            Err(error) => return Err(error).context("inspect restore source repository overlap"),
        };
        require_maintenance_schema(&connection).map_err(|error| {
            anyhow!("{error}; restore will not migrate or replace the existing database")
        })?;
        let mut statement = connection.prepare("SELECT repository_path FROM projects")?;
        let repositories = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for repository in repositories {
            if source.starts_with(resolve_missing(Path::new(&repository))?) {
                bail!("restore source must not be inside a registered repository")
            }
        }
    }
    Ok(())
}

fn validate_private_backup(manifest: &Path, database: &Path) -> Result<()> {
    let manifest_parent = manifest.parent().context("manifest has no parent")?;
    validate_private_path(manifest_parent, true)?;
    validate_private_path(manifest, false)?;
    validate_private_path(database, false)
}

fn validate_private_path(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect private backup path {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("backup paths must not be symlinks: {}", path.display())
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "backup path is not owned by the current user: {}",
            path.display()
        )
    }
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_type || metadata.mode() & 0o077 != 0 {
        bail!(
            "backup path is not private or has the wrong type: {}",
            path.display()
        )
    }
    Ok(())
}

fn database_family(paths: &InstancePaths) -> [PathBuf; 3] {
    [
        paths.database.clone(),
        PathBuf::from(format!("{}-wal", paths.database.display())),
        PathBuf::from(format!("{}-shm", paths.database.display())),
    ]
}

fn capture_live_family_bindings(paths: &InstancePaths) -> Result<Vec<JournalFileBinding>> {
    validate_live_database_family(paths)?;
    database_family(paths)
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| capture_file_binding(&path))
        .collect()
}

fn capture_file_binding(path: &Path) -> Result<JournalFileBinding> {
    let metadata = fs::metadata(path)?;
    Ok(JournalFileBinding {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        sha256: sha256_file(path)?,
    })
}

pub(crate) fn validate_live_database_family(paths: &InstancePaths) -> Result<()> {
    for path in database_family(paths) {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect live database file {}", path.display()))
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            bail!(
                "live database files must be regular, owner-owned, and not symlinks: {}",
                path.display()
            )
        }
    }
    Ok(())
}

fn validate_verified_retry(
    paths: &InstancePaths,
    backup: &Path,
    journal: &RestoreJournal,
) -> Result<()> {
    if journal.installed || journal.moves.iter().any(|item| item.disposition.is_some()) {
        bail!("verified restore retry is not at an untouched pre-displacement boundary")
    }
    let (manifest_path, manifest, backup_database) = load_manifest(backup)?;
    validate_private_backup(&manifest_path, &backup_database)?;
    let backup_database = fs::canonicalize(backup_database)?;
    if backup_database != journal.backup_database
        || sha256_file(&manifest_path)? != journal.backup_manifest_sha256
        || manifest.database_sha256 != journal.backup_database_sha256
        || sha256_file(&backup_database)? != journal.backup_database_sha256
    {
        bail!("verified restore retry must use the exact unchanged backup and manifest")
    }
    validate_private_path(&journal.staged_database, false)?;
    verify_sqlite_file(&journal.staged_database)?;
    if capture_file_binding(&journal.staged_database)? != journal.staged_binding
        || journal.staged_binding.sha256 != journal.backup_database_sha256
    {
        bail!("verified restore retry staged database no longer matches the journal")
    }
    let current_bindings = capture_live_family_bindings(paths)?;
    if current_bindings != journal.live_family_before {
        bail!("verified restore retry refused because the live database family changed")
    }
    Ok(())
}

fn validate_restore_journal(paths: &InstancePaths, journal: &RestoreJournal) -> Result<()> {
    let operation_id = uuid::Uuid::parse_str(&journal.operation_id)
        .context("restore journal operation ID is not a UUID")?;
    if operation_id.to_string() != journal.operation_id {
        bail!("restore journal operation ID is not canonical")
    }
    let expected_staged = paths
        .state
        .join(format!(".restore-{}.sqlite3", journal.operation_id));
    let expected_quarantine = paths
        .state
        .join("database-quarantine")
        .join(&journal.operation_id);
    if journal.staged_database != expected_staged || journal.quarantine != expected_quarantine {
        bail!("restore journal paths do not belong to the current instance and operation")
    }
    if journal.staged_binding.path != expected_staged
        || journal.staged_binding.sha256 != journal.backup_database_sha256
        || !valid_sha256(&journal.backup_manifest_sha256)
        || !valid_sha256(&journal.backup_database_sha256)
    {
        bail!("restore journal staged database binding is invalid")
    }
    private_directory(&journal.quarantine)?;
    match fs::symlink_metadata(&journal.staged_database) {
        Ok(metadata) => validate_owned_regular_file(
            &journal.staged_database,
            &metadata,
            "staged restore database",
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect staged restore database"),
    }
    let expected_family = database_family(paths);
    if journal.moves.len() != expected_family.len() {
        bail!("restore journal must contain the exact live SQLite file family")
    }
    for (item, source) in journal.moves.iter().zip(expected_family.iter()) {
        let name = source
            .file_name()
            .context("live database family path has no file name")?;
        if item.source != *source || item.quarantine != expected_quarantine.join(name) {
            bail!("restore journal move paths do not match the current live SQLite family")
        }
        if !matches!(
            item.disposition.as_deref(),
            None | Some("moved") | Some("absent")
        ) {
            bail!("restore journal contains an unsupported move disposition")
        }
        match fs::symlink_metadata(&item.quarantine) {
            Ok(metadata) => validate_owned_regular_file(
                &item.quarantine,
                &metadata,
                "quarantined database file",
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect quarantined database file"),
        }
    }
    let mut bound_paths = journal
        .live_family_before
        .iter()
        .map(|binding| binding.path.clone())
        .collect::<Vec<_>>();
    bound_paths.sort();
    bound_paths.dedup();
    if bound_paths.len() != journal.live_family_before.len()
        || bound_paths
            .iter()
            .any(|path| !expected_family.iter().any(|expected| expected == path))
        || journal
            .live_family_before
            .iter()
            .any(|binding| !valid_sha256(&binding.sha256))
    {
        bail!("restore journal live-family bindings are invalid")
    }
    Ok(())
}

fn validate_restore_file_bindings(paths: &InstancePaths, journal: &RestoreJournal) -> Result<()> {
    validate_live_database_family(paths)?;
    match &journal.phase {
        RestorePhase::Verified | RestorePhase::Displacing => {
            if journal.installed
                || (journal.phase == RestorePhase::Verified
                    && journal.moves.iter().any(|item| item.disposition.is_some()))
            {
                bail!("restore journal phase does not match its displacement state")
            }
            require_exact_file_binding(
                &journal.staged_database,
                &journal.staged_binding,
                "staged restore database",
            )?;
            for item in &journal.moves {
                validate_journal_move(journal, item, false)?;
            }
            Ok(())
        }
        RestorePhase::Installing => validate_installing_bindings(paths, journal),
        RestorePhase::HoldPending => {
            validate_installed_bindings(paths, journal, true, &["attention_required"])
        }
        RestorePhase::Completed => {
            validate_installed_bindings(paths, journal, false, &["attention_required"])
        }
        RestorePhase::Releasing => validate_installed_bindings(
            paths,
            journal,
            false,
            &["attention_required", "resolved_quiescent"],
        ),
        RestorePhase::Released => Ok(()),
    }
}

fn validate_installing_bindings(paths: &InstancePaths, journal: &RestoreJournal) -> Result<()> {
    if journal.moves.iter().any(|item| item.disposition.is_none()) {
        bail!("restore installation began before displacement was fully recorded")
    }
    let staged = observed_file_binding(&journal.staged_database, "staged restore database")?;
    let live = observed_file_binding(&paths.database, "installed restore database")?;
    let staged_waiting = staged
        .as_ref()
        .is_some_and(|actual| file_binding_matches(actual, &journal.staged_binding));
    let renamed_to_live = live
        .as_ref()
        .is_some_and(|actual| file_binding_matches(actual, &journal.staged_binding));
    if (staged_waiting && (live.is_some() || journal.installed))
        || (renamed_to_live && staged.is_some())
        || (!staged_waiting && !renamed_to_live)
    {
        bail!("restore installation no longer matches the recorded staged-file binding")
    }
    for item in &journal.moves {
        validate_journal_move(
            journal,
            item,
            renamed_to_live && item.source.as_path() == paths.database.as_path(),
        )?;
    }
    Ok(())
}

fn validate_installed_bindings(
    paths: &InstancePaths,
    journal: &RestoreJournal,
    allow_pre_hold: bool,
    accepted_hold_states: &[&str],
) -> Result<()> {
    if !journal.installed || journal.moves.iter().any(|item| item.disposition.is_none()) {
        bail!("restore hold phase does not have a fully installed database")
    }
    if observed_file_binding(&journal.staged_database, "staged restore database")?.is_some() {
        bail!("restore hold phase still has a staged database")
    }
    let live = observed_file_binding(&paths.database, "installed restore database")?
        .ok_or_else(|| anyhow!("restore hold phase is missing the installed database"))?;
    if !file_identity_matches(&live, &journal.staged_binding) {
        bail!("installed restore database no longer has the recorded staged-file identity")
    }
    let family = database_family(paths);
    let live_sidecars_before_hold_read = family
        .iter()
        .skip(1)
        .map(|path| observed_file_binding(path, "live SQLite sidecar"))
        .collect::<Result<Vec<_>>>()?;
    let exact_pre_hold = file_binding_matches(&live, &journal.staged_binding)
        && live_sidecars_before_hold_read
            .iter()
            .all(|sidecar| sidecar.is_none());
    let hold_state = if allow_pre_hold && exact_pre_hold {
        None
    } else {
        // Quarantine evidence must be exact before SQLite reads candidate
        // current-operation sidecars.
        for item in &journal.moves {
            validate_journal_move(journal, item, true)?;
        }
        validate_live_sidecars_against_displaced(journal, &live_sidecars_before_hold_read)?;
        let hold_state = matching_restore_hold_state(paths, journal);
        let live_sidecars_after_hold_read = family
            .iter()
            .skip(1)
            .map(|path| observed_file_binding(path, "live SQLite sidecar"))
            .collect::<Result<Vec<_>>>()?;
        validate_live_sidecars_against_displaced(journal, &live_sidecars_after_hold_read)?;
        hold_state?
    };
    let committed_current_hold = hold_state
        .as_deref()
        .is_some_and(|state| accepted_hold_states.contains(&state));
    if !committed_current_hold && !(allow_pre_hold && exact_pre_hold) {
        bail!("installed restore database lacks a committed hold for the current operation")
    }
    for item in &journal.moves {
        let live_source_allowed =
            item.source.as_path() == paths.database.as_path() || committed_current_hold;
        validate_journal_move(journal, item, live_source_allowed)?;
    }
    Ok(())
}

fn validate_live_sidecars_against_displaced(
    journal: &RestoreJournal,
    live_sidecars: &[Option<JournalFileBinding>],
) -> Result<()> {
    let checkpoint_bookkeeping_is_nondiscriminating = match (
        journal
            .moves
            .get(1)
            .and_then(|item| recorded_live_binding(journal, &item.source)),
        live_sidecars.first().and_then(Option::as_ref),
    ) {
        (Some(recorded), Some(actual)) => {
            recorded.bytes == 0 && actual.bytes == 0 && !file_identity_matches(actual, recorded)
        }
        _ => false,
    };
    for (item, actual) in journal.moves.iter().skip(1).zip(live_sidecars) {
        if let (Some(actual), Some(recorded)) =
            (actual, recorded_live_binding(journal, &item.source))
        {
            // A distinct empty WAL has no displaced frames; its paired SHM is
            // checkpoint bookkeeping.
            if file_identity_matches(actual, recorded)
                || (!checkpoint_bookkeeping_is_nondiscriminating
                    && file_contents_match(actual, recorded))
            {
                bail!(
                    "pre-restore SQLite sidecar reappeared at {}",
                    item.source.display()
                )
            }
        }
    }
    Ok(())
}

fn validate_journal_move(
    journal: &RestoreJournal,
    item: &JournalMove,
    live_source_allowed: bool,
) -> Result<()> {
    let recorded = recorded_live_binding(journal, &item.source);
    let source = observed_file_binding(&item.source, "live database file")?;
    let quarantine = observed_file_binding(&item.quarantine, "quarantined database file")?;
    if !live_source_allowed && item.disposition.is_some() && source.is_some() {
        bail!(
            "live database file reappeared after displacement: {}",
            item.source.display()
        )
    }
    let valid = match item.disposition.as_deref() {
        None => match recorded {
            Some(recorded) => {
                (source
                    .as_ref()
                    .is_some_and(|actual| file_binding_matches(actual, recorded))
                    && quarantine.is_none())
                    || (source.is_none()
                        && quarantine
                            .as_ref()
                            .is_some_and(|actual| file_binding_matches(actual, recorded)))
            }
            None => source.is_none() && quarantine.is_none(),
        },
        Some("moved") => {
            recorded.is_some_and(|recorded| {
                quarantine
                    .as_ref()
                    .is_some_and(|actual| file_binding_matches(actual, recorded))
            }) && (live_source_allowed || source.is_none())
        }
        Some("absent") => {
            recorded.is_none() && quarantine.is_none() && (live_source_allowed || source.is_none())
        }
        Some(_) => false,
    };
    if !valid {
        bail!(
            "restore move evidence no longer matches the recorded binding for {}",
            item.source.display()
        )
    }
    Ok(())
}

fn matching_restore_hold_state(
    paths: &InstancePaths,
    journal: &RestoreJournal,
) -> Result<Option<String>> {
    let store = Store::open_maintenance_readonly(&paths.database)
        .context("open installed database to verify restore hold operation binding")?;
    let connection = store.lock()?;
    let quarantine_text = journal.quarantine.to_string_lossy();
    let quarantine: &str = quarantine_text.as_ref();
    connection
        .query_row(
            "SELECT state FROM recovery_records
             WHERE id=?1
               AND json_extract(detail_json,'$.operation_id')=?2
               AND json_extract(detail_json,'$.backup_database_sha256')=?3
               AND json_extract(detail_json,'$.quarantine')=?4",
            rusqlite::params![
                HOLD_ID,
                journal.operation_id.as_str(),
                journal.backup_database_sha256.as_str(),
                quarantine
            ],
            |row| row.get(0),
        )
        .optional()
        .context("read restore hold operation binding")
}

fn recorded_live_binding<'a>(
    journal: &'a RestoreJournal,
    path: &Path,
) -> Option<&'a JournalFileBinding> {
    journal
        .live_family_before
        .iter()
        .find(|binding| binding.path.as_path() == path)
}

fn observed_file_binding(path: &Path, description: &str) -> Result<Option<JournalFileBinding>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {description} {}", path.display()))
        }
    };
    validate_owned_regular_file(path, &metadata, description)?;
    Ok(Some(JournalFileBinding {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.len(),
        sha256: sha256_file(path)
            .with_context(|| format!("hash {description} {}", path.display()))?,
    }))
}

fn require_exact_file_binding(
    path: &Path,
    recorded: &JournalFileBinding,
    description: &str,
) -> Result<()> {
    let actual = observed_file_binding(path, description)?
        .ok_or_else(|| anyhow!("{description} is missing: {}", path.display()))?;
    if !file_binding_matches(&actual, recorded) {
        bail!(
            "{description} no longer matches its recorded binding: {}",
            path.display()
        )
    }
    Ok(())
}

fn file_identity_matches(actual: &JournalFileBinding, recorded: &JournalFileBinding) -> bool {
    actual.device == recorded.device && actual.inode == recorded.inode
}

fn file_binding_matches(actual: &JournalFileBinding, recorded: &JournalFileBinding) -> bool {
    file_identity_matches(actual, recorded) && file_contents_match(actual, recorded)
}

fn file_contents_match(actual: &JournalFileBinding, recorded: &JournalFileBinding) -> bool {
    actual.bytes == recorded.bytes && actual.sha256 == recorded.sha256
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_owned_regular_file(
    path: &Path,
    metadata: &fs::Metadata,
    description: &str,
) -> Result<()> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        bail!(
            "{description} must be regular, owner-owned, and not a symlink: {}",
            path.display()
        )
    }
    Ok(())
}

fn journal_path(paths: &InstancePaths) -> PathBuf {
    paths.state.join("database-restore-journal.json")
}

fn read_journal(paths: &InstancePaths) -> Result<Option<RestoreJournal>> {
    let path = journal_path(paths);
    match fs::symlink_metadata(&path) {
        Ok(_) => validate_private_path(&path, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let journal: RestoreJournal = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse restore journal {}", path.display()))?;
    if journal.schema != JOURNAL_SCHEMA {
        bail!("unsupported database restore journal schema")
    }
    validate_restore_journal(paths, &journal)?;
    Ok(Some(journal))
}

fn persist_journal(paths: &InstancePaths, journal: &RestoreJournal) -> Result<()> {
    write_private_json(&journal_path(paths), journal)
}

fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("private JSON path has no parent")?;
    create_private_directory(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_directory(parent)
}

fn copy_private(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    sync_directory(
        destination
            .parent()
            .context("restore staging has no parent")?,
    )
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        bail!(
            "directory must be owned and not a symlink: {}",
            path.display()
        )
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    private_directory(path)
}

fn private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        bail!(
            "directory must be private, owned, and not a symlink: {}",
            path.display()
        )
    }
    Ok(())
}

fn set_private_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("file must be owned and not a symlink: {}", path.display())
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn resolve_missing(path: &Path) -> Result<PathBuf> {
    let mut current = path;
    let mut missing = Vec::new();
    while !current.exists() {
        missing.push(
            current
                .file_name()
                .context("path has no existing ancestor")?
                .to_owned(),
        );
        current = current.parent().context("path has no existing ancestor")?;
    }
    let mut resolved = fs::canonicalize(current)?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn source_storage_budgets(paths: &InstancePaths, source: &Connection) -> Result<(u64, u64)> {
    let pages: i64 = source.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = source.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    validate_live_database_family(paths)?;
    let main_bytes = fs::metadata(&paths.database)
        .context("read main database size for capacity preflight")?
        .len();
    let wal = database_family(paths)[1].clone();
    let wal_bytes = match fs::symlink_metadata(&wal) {
        Ok(metadata) => {
            validate_owned_regular_file(&wal, &metadata, "live WAL for capacity sizing")?;
            metadata.len()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error).context("read live WAL size for capacity preflight"),
    };
    storage_budgets(pages, page_size, main_bytes, wal_bytes)
}

fn storage_budgets(
    pages: i64,
    page_size: i64,
    main_bytes: u64,
    wal_bytes: u64,
) -> Result<(u64, u64)> {
    if pages <= 0 || page_size <= 0 {
        bail!("database capacity sizing requires positive page count and page size")
    }
    let logical_bytes = (pages as u64)
        .checked_mul(page_size as u64)
        .context("logical database size overflow")?;
    let backup_budget = logical_bytes
        .max(main_bytes)
        .checked_mul(2)
        .context("backup capacity budget overflow")?;
    let migration_budget = backup_budget
        .checked_add(wal_bytes)
        .and_then(|bytes| bytes.checked_add(MIGRATION_RESERVE_BYTES))
        .context("migration capacity budget overflow")?;
    Ok((backup_budget, migration_budget))
}

fn existing_capacity_ancestor(path: &Path) -> Result<PathBuf> {
    let resolved = resolve_missing(path)?;
    resolved
        .ancestors()
        .find(|item| item.exists())
        .map(Path::to_owned)
        .context("capacity path has no existing ancestor")
}

fn require_upgrade_capacity(
    destination: &Path,
    state: &Path,
    backup_budget: u64,
    migration_budget: u64,
) -> Result<()> {
    let backup_device = fs::metadata(existing_capacity_ancestor(destination)?)?.dev();
    let state_device = fs::metadata(existing_capacity_ancestor(state)?)?.dev();
    if backup_device == state_device {
        let combined = backup_budget
            .checked_add(migration_budget)
            .context("combined upgrade capacity budget overflow")?;
        require_capacity(state, combined).context("shared filesystem backup and migration capacity")
    } else {
        require_capacity(destination, backup_budget)
            .context("backup destination capacity before upgrade")?;
        require_capacity(state, migration_budget)
            .context("state filesystem migration capacity before backup")
    }
}

fn available_capacity(path: &Path) -> Result<u64> {
    #[cfg(test)]
    if let Some(available) = TEST_AVAILABLE_CAPACITY.with(|values| values.borrow_mut().pop_front())
    {
        return Ok(available);
    }
    let existing = existing_capacity_ancestor(path)?;
    let c_path = std::ffi::CString::new(existing.as_os_str().as_encoded_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs initializes the output only on success; the C path lives through the call.
    if unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("inspect available database capacity");
    }
    let stats = unsafe { stats.assume_init() };
    (stats.f_bavail as u64)
        .checked_mul(stats.f_frsize as u64)
        .context("available database capacity overflow")
}

fn require_capacity(path: &Path, required: u64) -> Result<()> {
    let available = available_capacity(path)
        .with_context(|| format!("inspect capacity at {}", path.display()))?;
    if available < required {
        bail!(
            "insufficient capacity at {}: {available} bytes available, {required} required",
            path.display()
        )
    }
    Ok(())
}

fn prune_backups(root: &Path, newest: &Path) -> Result<()> {
    let now = Utc::now();
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if !path.is_dir()
            || path
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"."))
        {
            continue;
        }
        let Ok((_, manifest, _)) = load_manifest(&path) else {
            continue;
        };
        if !app_owned_snapshot(&path)? || verify(&path).is_err() {
            continue;
        }
        let Ok(created) = DateTime::parse_from_rfc3339(&manifest.created_at) else {
            continue;
        };
        snapshots.push((
            path,
            created.with_timezone(&Utc),
            manifest.database_bytes,
            manifest.sqlite_schema_version,
        ));
    }
    snapshots.sort_by(|left, right| (&left.1, &left.0).cmp(&(&right.1, &right.0)));
    let mut anchors = std::collections::BTreeMap::new();
    for (path, _, _, version) in &snapshots {
        if SERVICE_UPGRADABLE_SCHEMA_VERSIONS.contains(version) {
            anchors.insert(*version, path.clone());
        }
    }
    let mut total = snapshots.iter().try_fold(0_u64, |sum, (_, _, size, _)| {
        sum.checked_add(*size)
            .context("retention snapshot size overflow")
    })?;
    let mut count = snapshots.len();
    for (path, created, size, version) in snapshots {
        // Schema anchors survive the ordinary limits so intermediate upgrade retries retain recovery.
        if path == newest || anchors.get(&version) == Some(&path) {
            continue;
        }
        let expired = now.signed_duration_since(created).num_days() > MAX_BACKUP_AGE_DAYS;
        if count > MAX_BACKUPS || total > MAX_BACKUP_BYTES || expired {
            fs::remove_dir_all(&path)
                .with_context(|| format!("prune verified application backup {}", path.display()))?;
            count -= 1;
            total = total.saturating_sub(size);
        }
    }
    sync_directory(root)
}

fn app_owned_snapshot(path: &Path) -> Result<bool> {
    let mut names = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names
        == vec![
            std::ffi::OsString::from("database.sqlite3"),
            std::ffi::OsString::from("manifest.json"),
        ])
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::InstancePaths;

    #[test]
    fn capacity_refusal_is_explicit() {
        let path =
            std::env::temp_dir().join(format!("agenticjira-capacity-{}", uuid::Uuid::new_v4()));
        create_private_directory(&path).unwrap();
        let error = require_capacity(&path, u64::MAX).unwrap_err().to_string();
        assert!(error.contains("insufficient capacity"));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn restorepoint_capacity_counts_committed_wal_growth_and_rejects_overflow() {
        let root = std::env::temp_dir().join(format!("llmrelay-sizing-{}", uuid::Uuid::new_v4()));
        let paths = InstancePaths::resolve(Some(root)).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        let connection = store.lock().unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        connection.execute(
            "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
             VALUES('sizing','sizing','service','fixture','fixture','fixture',json_object('payload',?1),'2026-01-01T00:00:00Z')",
            ["x".repeat(1024 * 1024)],
        ).unwrap();
        let pages: u64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        let page_size: u64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        let logical = pages * page_size;
        let main = fs::metadata(&paths.database).unwrap().len();
        let wal = fs::metadata(&database_family(&paths)[1]).unwrap().len();
        assert!(logical > main);
        assert!(wal > 0);
        assert_eq!(
            source_storage_budgets(&paths, &connection).unwrap(),
            (2 * logical, 2 * logical + wal + 64 * 1024 * 1024)
        );
        assert_eq!(
            storage_budgets(1, 4096, 8192, 123).unwrap(),
            (16384, 16384 + 123 + MIGRATION_RESERVE_BYTES)
        );
        for (pages, size, main, wal) in [
            (0, 4096, 1, 0),
            (1, -1, 1, 0),
            (i64::MAX, i64::MAX, 0, 0),
            (1, 1, u64::MAX, 0),
            (1, 1, 0, u64::MAX),
        ] {
            assert!(storage_budgets(pages, size, main, wal).is_err());
        }
        drop(connection);
        drop(store);
        fs::remove_dir_all(paths.root).unwrap();
    }

    #[test]
    fn restorepoint_capacity_combines_shared_filesystems_and_separates_other_devices() {
        let root = std::env::temp_dir().join(format!("llmrelay-capacity-{}", uuid::Uuid::new_v4()));
        create_private_directory(&root).unwrap();
        let destination = root.join("missing-backups");
        TEST_AVAILABLE_CAPACITY.with(|values| *values.borrow_mut() = [79].into());
        let error = require_upgrade_capacity(&destination, &root, 30, 50).unwrap_err();
        assert!(format!("{error:#}").contains("80 required"));
        assert!(!destination.exists());
        TEST_AVAILABLE_CAPACITY.with(|values| *values.borrow_mut() = [80].into());
        require_upgrade_capacity(&destination, &root, 30, 50).unwrap();
        assert!(require_upgrade_capacity(&destination, &root, u64::MAX, 1)
            .unwrap_err()
            .to_string()
            .contains("overflow"));
        let other_device = Path::new("/dev");
        if fs::metadata(other_device).unwrap().dev() != fs::metadata(&root).unwrap().dev() {
            for available in [[29, 50], [30, 49], [30, 50]] {
                TEST_AVAILABLE_CAPACITY.with(|values| *values.borrow_mut() = available.into());
                let result = require_upgrade_capacity(other_device, &root, 30, 50);
                match available {
                    [29, 50] => assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("backup destination capacity")),
                    [30, 49] => assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("state filesystem migration capacity")),
                    _ => assert!(result.is_ok()),
                }
                TEST_AVAILABLE_CAPACITY.with(|values| values.borrow_mut().clear());
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_staged_backup_does_not_publish_or_replace_verified_backup() {
        let root = std::env::temp_dir().join(format!("llmrelay-backup-{}", uuid::Uuid::new_v4()));
        let destination = root.with_extension("backups");
        let paths = InstancePaths::resolve(Some(root.clone())).unwrap();
        paths.create().unwrap();
        let store = Store::open(&paths.database).unwrap();
        let first = backup(&paths, Some(&destination)).unwrap();
        let first_path = PathBuf::from(first["backup"].as_str().unwrap());
        let first_hash = first["database_sha256"].clone();

        TEST_INTERRUPT_BACKUP_BEFORE_PUBLISH.with(|armed| armed.set(true));
        let error = backup(&paths, Some(&destination)).unwrap_err();
        assert!(error.to_string().contains("injected interruption"));
        assert!(destination.join(first_path.file_name().unwrap()).exists());
        assert_eq!(verify(&first_path).unwrap()["database_sha256"], first_hash);
        let entries: Vec<_> = fs::read_dir(&destination)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "one published backup and one staging directory"
        );
        assert!(entries
            .iter()
            .any(|name| name.to_string_lossy().ends_with(".staging")));
        assert!(entries
            .iter()
            .any(|name| name == first_path.file_name().unwrap()));

        drop(store);
        let reopened = Store::open_current_readonly(&paths.database).unwrap();
        require_current_schema(&reopened.lock().unwrap()).unwrap();
        drop(reopened);
        let next = backup(&paths, Some(&destination)).unwrap();
        assert_ne!(next["backup"], first["backup"]);
        assert!(verify(&first_path).is_ok());
        assert!(verify(Path::new(next["backup"].as_str().unwrap())).is_ok());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&destination).unwrap();
    }
}
