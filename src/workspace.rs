use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryInfo {
    pub root: PathBuf,
    pub common_directory: PathBuf,
    pub identity: String,
    pub head: String,
    pub branch: Option<String>,
    pub dirty: bool,
}

pub fn inspect(path: &Path) -> Result<RepositoryInfo> {
    if !path.is_absolute() {
        bail!("repository path must be absolute")
    }
    let root = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?).canonicalize()?;
    let common = PathBuf::from(git(&root, &["rev-parse", "--git-common-dir"])?);
    let common_directory = (if common.is_absolute() {
        common
    } else {
        root.join(common)
    })
    .canonicalize()?;
    let head = git(&root, &["rev-parse", "HEAD"])?;
    let branch = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let dirty = !git(
        &root,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )?
    .is_empty();
    let identity = format!("git:{}", common_directory.to_string_lossy());
    Ok(RepositoryInfo {
        root,
        common_directory,
        identity,
        head,
        branch,
        dirty,
    })
}

pub fn create_detached_worktree(
    repository: &RepositoryInfo,
    destination: &Path,
    base: &str,
) -> Result<()> {
    if destination.exists() {
        bail!(
            "worktree destination already exists: {}",
            destination.display()
        )
    }
    let parent = destination
        .parent()
        .context("worktree destination has no parent")?;
    std::fs::create_dir_all(parent)?;
    git(
        &repository.root,
        &["cat-file", "-e", &format!("{base}^{{commit}}")],
    )?;
    command(
        &repository.root,
        &[
            "worktree",
            "add",
            "--detach",
            &destination.to_string_lossy(),
            base,
        ],
    )?;
    let created = inspect(destination)?;
    if created.identity != repository.identity || created.head != base {
        bail!("created worktree identity/base does not match reservation")
    }
    Ok(())
}

pub(crate) fn policy_mode(policy: &serde_json::Value) -> Result<crate::domain::WorkspaceMode> {
    match policy.get("workspace_mode") {
        None => Ok(crate::domain::WorkspaceMode::IsolatedWorktree),
        Some(value) => serde_json::from_value(value.clone()).context("invalid workspace mode"),
    }
}

pub(crate) fn checkout_identity(repository: &RepositoryInfo) -> Result<serde_json::Value> {
    use std::os::unix::fs::MetadataExt;
    let root = repository.root.metadata()?;
    let common = repository.common_directory.metadata()?;
    Ok(
        serde_json::json!({"root":{"device":root.dev(),"inode":root.ino()},"common":{"device":common.dev(),"inode":common.ino()}}),
    )
}

pub(crate) fn require_checkout_environment() -> Result<()> {
    for selector in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        if std::env::var_os(selector).is_some() {
            bail!("Clear inherited {selector} in the service environment before Main checkout preparation; Git selectors must not replace the registered checkout or starting index")
        }
    }
    Ok(())
}

pub(crate) struct CheckedCheckoutResult {
    policy: String,
    snapshot: String,
    manifest_hash: String,
}

impl CheckedCheckoutResult {
    pub(crate) fn recheck(&self, connection: &rusqlite::Connection, attempt: &str) -> Result<()> {
        let current: bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM workspaces w JOIN attempts a ON a.id=w.attempt_id
            JOIN claims c ON c.attempt_id=a.id AND c.state='running' AND c.repository_identity=w.repository_identity
            JOIN snapshots s ON s.id=?2 AND s.attempt_id=a.id AND s.complete=1
            WHERE a.id=?1 AND w.policy_json=?3 AND s.manifest_hash=?4 AND a.candidate_hash=s.manifest_hash)",
            rusqlite::params![attempt,self.snapshot,self.policy,self.manifest_hash], |row|row.get(0))?;
        let active: bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sessions s JOIN role_generations g ON g.id=s.role_generation_id
            WHERE g.attempt_id=?1 AND (s.status NOT IN ('exited','launch_failed') OR (s.status='exited' AND COALESCE(json_extract(s.exit_json,'$.process_group_quiescent'),0)!=1)))
            OR EXISTS(SELECT 1 FROM check_runs WHERE attempt_id=?1 AND status IN ('launch_reserved','running','recovery_required','launch_ambiguous'))",
            rusqlite::params![attempt], |row|row.get(0))?;
        if !current || active {
            bail!("The reviewed checkout or its process ownership changed before acceptance; reload the result")
        }
        let task: String = connection.query_row(
            "SELECT task_id FROM attempts WHERE id=?1",
            rusqlite::params![attempt],
            |row| row.get(0),
        )?;
        crate::workflow::validate_workspace_checkout(
            connection,
            &task,
            &serde_json::from_str(&self.policy)?,
        )
    }
}

pub(crate) fn check_reviewed_checkout(
    store: &crate::store::Store,
    attempt: &str,
) -> Result<Option<CheckedCheckoutResult>> {
    let (task, path, policy, snapshot, hash, manifest): (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = {
        let connection = store.lock()?;
        connection.query_row("SELECT a.task_id,w.path,w.policy_json,s.id,s.manifest_hash,s.manifest_json FROM attempts a
            LEFT JOIN workspaces w ON w.attempt_id=a.id LEFT JOIN snapshots s ON s.id=COALESCE(a.accepted_snapshot_id,
              (SELECT snapshot_id FROM workflow_result_bindings WHERE attempt_id=a.id AND candidate_hash=a.candidate_hash ORDER BY created_at DESC LIMIT 1))
            WHERE a.id=?1",rusqlite::params![attempt], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)))?
    };
    let Some(policy) = policy else {
        if crate::workflow::task_workspace_mode(&*store.lock()?, &task)?
            == crate::domain::WorkspaceMode::MainCheckout
        {
            bail!("Main acceptance requires its prepared borrowed checkout and reviewed snapshot")
        }
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&policy)?;
    if policy_mode(&value)? != crate::domain::WorkspaceMode::MainCheckout {
        return Ok(None);
    }
    require_checkout_environment()?;
    crate::workflow::validate_workspace_checkout(&*store.lock()?, &task, &value)?;
    let authorization: crate::domain::TaskCheckoutScope =
        serde_json::from_value(value["checkout_authorization"].clone())?;
    let path = path.context("Main acceptance has no recorded checkout path")?;
    let root = Path::new(&path).canonicalize()?;
    let repository = inspect(&root)?;
    if value["checkout_identity"] != checkout_identity(&repository)? {
        bail!("The borrowed checkout's physical identity changed after preparation; cancel or relink before accepting")
    }
    let manifest: crate::snapshot::SnapshotManifest = serde_json::from_str(
        &manifest.context("Main acceptance has no complete reviewed snapshot")?,
    )?;
    if root.to_str() != Some(authorization.checkout_root.as_str())
        || repository.root != root
        || repository.identity != authorization.repository_identity
        || !crate::snapshot::verify_materialized(&manifest, &root)?
    {
        let mut connection = store.lock()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed=tx.execute("UPDATE attempts SET status='held' WHERE id=?1 AND status IN ('running','held') AND candidate_hash=?2
            AND EXISTS(SELECT 1 FROM workspaces WHERE attempt_id=?1 AND policy_json=?3)",rusqlite::params![attempt,hash,policy])?;
        if changed == 1 {
            tx.execute("UPDATE tasks SET attention='needs_input',version=version+1,updated_at=?1 WHERE id=?2 AND lifecycle='awaiting_review' AND attention!='needs_input'",
                rusqlite::params![chrono::Utc::now().to_rfc3339(),task])?;
            tx.execute("UPDATE workflow_result_bindings SET state_reason='The live Main checkout changed after review; restore reviewed bytes or cancel and authorize a new task to preserve the new edits'
                WHERE attempt_id=?1 AND candidate_hash=?2 AND state='awaiting_human'",rusqlite::params![attempt,hash])?;
        }
        tx.commit()?;
        bail!("The Main checkout changed after review. Restore the reviewed bytes, or cancel this task and authorize the current checkout for a new task; user files were left unchanged.")
    }
    let checked = CheckedCheckoutResult {
        policy,
        snapshot: snapshot.context("Missing reviewed snapshot")?,
        manifest_hash: hash.context("Missing reviewed snapshot hash")?,
    };
    checked.recheck(&*store.lock()?, attempt)?;
    Ok(Some(checked))
}

pub(crate) fn prepare(
    repository: &RepositoryInfo,
    destination: &Path,
    base: &str,
    policy: &serde_json::Value,
) -> Result<()> {
    if policy_mode(policy)? == crate::domain::WorkspaceMode::IsolatedWorktree {
        return create_detached_worktree(repository, destination, base);
    }
    require_checkout_environment()?;
    if policy["workspace_ownership"].as_str() != Some("borrowed_checkout") {
        bail!("Main checkout preparation requires explicit borrowed workspace ownership")
    }
    let scope: crate::domain::TaskCheckoutScope =
        serde_json::from_value(policy["checkout_authorization"].clone())
            .context("Main checkout preparation requires its frozen authorization")?;
    let registered = inspect(&repository.root)?;
    if policy
        .get("checkout_identity")
        .is_some_and(|frozen| checkout_identity(&registered).ok().as_ref() != Some(frozen))
    {
        bail!("The Main checkout's physical root or Git metadata changed after preparation; recover the borrowed workspace before continuing")
    }
    let selected = destination.canonicalize()
        .context("The authorized checkout is unavailable. Relink the project and authorize it with Make Ready.")?;
    if scope.workspace_mode != crate::domain::WorkspaceMode::MainCheckout
        || selected != registered.root
        || selected != Path::new(&scope.checkout_root)
        || registered.root != repository.root
        || registered.common_directory != repository.common_directory
        || registered.identity != repository.identity
        || registered.identity != scope.repository_identity
        || registered.head != base
    {
        bail!("The authorized checkout root, repository or commit changed. Relink the project and authorize the displayed checkout with Make Ready; no checkout files were changed.")
    }
    Ok(())
}

pub fn resolve_commit(repository: &RepositoryInfo, git_ref: &str) -> Result<String> {
    if git_ref.trim().is_empty() {
        bail!("integration ref cannot be blank")
    }
    git(
        &repository.root,
        &["rev-parse", &format!("{git_ref}^{{commit}}")],
    )
}

pub fn is_ancestor(repository: &RepositoryInfo, ancestor: &str, descendant: &str) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(&repository.root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("git merge-base failed"),
    }
}

pub fn tree_entry(repository: &RepositoryInfo, commit: &str, path: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repository.root)
        .args(["ls-tree", commit, "--", path])
        .output()?;
    if !output.status.success() {
        bail!("git ls-tree failed for {path}")
    }
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

/// The commit currently checked out at `root`.
pub fn head(root: &Path) -> Result<String> {
    git(root, &["rev-parse", "HEAD"])
}

/// The bytes of each path in `revision`, or `None` when the commit has no blob
/// there. One batched `git cat-file` process reads every requested path.
pub fn committed_files(
    root: &Path,
    revision: &str,
    paths: &[&str],
) -> Result<Vec<Option<Vec<u8>>>> {
    read_committed_files(root, revision, paths, None)?
        .context("an unbounded git cat-file read cannot exceed a bound")
}

/// Reads one extra byte to detect overflow, then kills and reaps the exact
/// child before returning `None` instead of buffering a large historical blob.
pub(crate) fn committed_files_within(
    root: &Path,
    revision: &str,
    paths: &[&str],
    max_output_bytes: usize,
) -> Result<Option<Vec<Option<Vec<u8>>>>> {
    read_committed_files(root, revision, paths, Some(max_output_bytes))
}

fn read_committed_files(
    root: &Path,
    revision: &str,
    paths: &[&str],
    max_output_bytes: Option<usize>,
) -> Result<Option<Vec<Option<Vec<u8>>>>> {
    use std::io::{Read, Write};
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("run git cat-file in {}", root.display()))?;
    {
        // The request list is small enough for the pipe buffer, so it is
        // written completely before any output is read.
        let mut stdin = child.stdin.take().context("git cat-file stdin")?;
        for path in paths {
            if path.contains('\n') {
                bail!("committed path contains a newline")
            }
            writeln!(stdin, "{revision}:{path}")?;
        }
    }
    let mut output = Vec::new();
    let mut stdout = child.stdout.take().context("git cat-file stdout")?;
    match max_output_bytes {
        None => {
            stdout.read_to_end(&mut output)?;
        }
        Some(limit) => {
            let limit_u64 = u64::try_from(limit).context("git cat-file read bound")?;
            (&mut stdout)
                .take(limit_u64.saturating_add(1))
                .read_to_end(&mut output)?;
            if output.len() > limit {
                drop(stdout);
                let killed = child.kill();
                child.wait()?;
                killed.context("stop git cat-file at its read bound")?;
                return Ok(None);
            }
        }
    }
    drop(stdout);
    if !child.wait()?.success() {
        bail!("git cat-file failed in {}", root.display())
    }
    let mut files = Vec::with_capacity(paths.len());
    let mut cursor = 0;
    for _ in paths {
        let end = output[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| cursor + offset)
            .context("git cat-file output ended early")?;
        let header = std::str::from_utf8(&output[cursor..end])?;
        cursor = end + 1;
        // Absent objects echo the request, which may itself contain spaces.
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            files.push(None);
            continue;
        }
        let fields = header.split(' ').collect::<Vec<_>>();
        let [_, kind, size] = fields.as_slice() else {
            bail!("unexpected git cat-file header")
        };
        let size: usize = size.parse()?;
        let end = cursor
            .checked_add(size)
            .context("git cat-file object size is out of range")?;
        let content = output
            .get(cursor..end)
            .context("git cat-file object was truncated")?;
        files.push((*kind == "blob").then(|| content.to_vec()));
        cursor = end + 1;
    }
    Ok(Some(files))
}

pub fn commit_exists(repository: &RepositoryInfo, commit: &str) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(&repository.root)
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .status()?;
    Ok(status.success())
}

fn git(cwd: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .with_context(|| format!("run git {} in {}", arguments.join(" "), cwd.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn command(cwd: &Path, arguments: &[&str]) -> Result<()> {
    git(cwd, arguments).map(|_| ())
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn borrowed_checkout_preserves_dirty_baseline_and_refuses_scope_drift() {
        let root = std::env::temp_dir().join(format!("llmrelay-checkout-{}", uuid::Uuid::new_v4()));
        let checkout = root.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        command(&checkout, &["init"]).unwrap();
        std::fs::write(checkout.join("tracked.txt"), b"committed\n").unwrap();
        std::fs::write(
            checkout.join(".gitignore"),
            b"ignored.txt\nignored-snapshots/\n",
        )
        .unwrap();
        command(&checkout, &["add", "."]).unwrap();
        command(
            &checkout,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        let repository = inspect(&checkout).unwrap();
        crate::snapshot::require_artifacts_excluded(
            &repository.root,
            &repository.root.join("ignored-snapshots"),
        )
        .unwrap();
        assert!(crate::snapshot::require_artifacts_excluded(
            &repository.root,
            &repository.root.join("eligible-snapshots")
        )
        .is_err());
        let scope = crate::domain::TaskCheckoutScope {
            task_id: "task".into(),
            project_id: "project".into(),
            checkout_root: repository.root.to_str().unwrap().into(),
            repository_identity: repository.identity.clone(),
            workspace_mode: crate::domain::WorkspaceMode::MainCheckout,
            selection_revision: 1,
            selection_hash: "selection".into(),
            writer_policy_hash: "writer".into(),
            configuration_hash: "configuration".into(),
            scope_hash: "scope".into(),
        };
        let policy = serde_json::json!({"workspace_mode":"main_checkout",
            "workspace_ownership":"borrowed_checkout","checkout_authorization":scope});
        std::fs::write(checkout.join("tracked.txt"), b"preexisting tracked\n").unwrap();
        std::fs::write(checkout.join("new.txt"), b"preexisting untracked\0bytes").unwrap();
        std::fs::set_permissions(
            checkout.join("new.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(checkout.join("ignored.txt"), b"outside snapshot coverage").unwrap();
        prepare(&repository, &checkout, &repository.head, &policy).unwrap();
        assert!(!root.join("worktrees").exists());
        let baseline = repository.root.join("ignored-snapshots/baseline");
        let (first, first_hash) =
            crate::snapshot::capture(&repository, &checkout, &baseline).unwrap();
        crate::snapshot::require_artifacts_excluded(
            &repository.root,
            &repository.root.join("ignored-snapshots"),
        )
        .unwrap();
        let (second, second_hash) =
            crate::snapshot::capture(&repository, &checkout, &root.join("verification")).unwrap();
        assert!(crate::snapshot::task_changed_paths(&first, &second)
            .unwrap()
            .is_empty());
        crate::snapshot::require_stable_preparation(&first, &second).unwrap();
        assert_eq!(first_hash, second_hash);
        assert_eq!(first.original_base, repository.head);
        assert_eq!(first.repository_identity, repository.identity);
        std::fs::write(checkout.join("task.txt"), b"agent change\n").unwrap();
        let (changed, _) =
            crate::snapshot::capture(&repository, &checkout, &root.join("candidate")).unwrap();
        assert_eq!(
            crate::snapshot::task_changed_paths(&first, &changed).unwrap(),
            vec!["task.txt"]
        );
        assert!(!crate::snapshot::verify_materialized(&first, &checkout).unwrap());
        std::fs::remove_file(checkout.join("task.txt")).unwrap();
        assert_eq!(
            std::fs::read(baseline.join("tracked.txt")).unwrap(),
            b"preexisting tracked\n"
        );
        assert_eq!(
            std::fs::read(baseline.join("new.txt")).unwrap(),
            b"preexisting untracked\0bytes"
        );
        assert_eq!(
            std::fs::metadata(baseline.join("new.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(!first
            .entries
            .iter()
            .any(|entry| entry.path == "ignored.txt"));
        for cause in ["mode", "content"] {
            std::fs::set_permissions(
                checkout.join("new.txt"),
                std::fs::Permissions::from_mode(if cause == "mode" { 0o644 } else { 0o755 }),
            )
            .unwrap();
            if cause == "content" {
                std::fs::write(checkout.join("new.txt"), b"external change").unwrap();
            }
            let (changed, _) =
                crate::snapshot::capture(&repository, &checkout, &root.join(cause)).unwrap();
            let error = crate::snapshot::require_stable_preparation(&first, &changed).unwrap_err();
            assert!(
                error.to_string().contains("checkout changed"),
                "{cause}: {error}"
            );
            assert!(!crate::snapshot::verify_materialized(&first, &checkout).unwrap());
        }
        let before = std::fs::read(checkout.join("new.txt")).unwrap();
        let mut unowned = policy.clone();
        unowned
            .as_object_mut()
            .unwrap()
            .remove("workspace_ownership");
        assert!(prepare(&repository, &checkout, &repository.head, &unowned).is_err());
        assert!(prepare(&repository, &checkout, "different-head", &policy).is_err());
        let isolated = root.join("isolated");
        prepare(
            &repository,
            &isolated,
            &repository.head,
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(isolated.join("tracked.txt")).unwrap(),
            b"committed\n"
        );
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&checkout, &alias).unwrap();
        prepare(&repository, &alias, &repository.head, &policy).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&isolated, &alias).unwrap();
        assert_eq!(inspect(&alias).unwrap().identity, repository.identity);
        let error = prepare(&repository, &alias, &repository.head, &policy).unwrap_err();
        assert!(
            error.to_string().contains("authorized checkout root"),
            "{error}"
        );
        assert_eq!(std::fs::read(checkout.join("new.txt")).unwrap(), before);
        assert_eq!(
            std::fs::read(checkout.join("tracked.txt")).unwrap(),
            b"preexisting tracked\n"
        );
        assert!(checkout.join(".git").exists());
        command(
            &checkout,
            &["worktree", "remove", isolated.to_str().unwrap()],
        )
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
