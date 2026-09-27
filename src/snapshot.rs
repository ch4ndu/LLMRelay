use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const FILE_LIMIT: u64 = 10 * 1024 * 1024;
const SNAPSHOT_LIMIT: u64 = 100 * 1024 * 1024;
const METADATA_DIR: &str = ".agenticjira-snapshot-meta";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub schema: u8,
    /// The immutable project revision from which this attempt started.
    #[serde(default)]
    pub original_base: String,
    /// The candidate worktree HEAD at capture time. Dirty bytes are represented by entries.
    #[serde(default)]
    pub candidate_head: String,
    /// Compatibility name for schema-1 snapshots; equals candidate_head in schema 2.
    pub snapshot_base: String,
    pub repository_identity: String,
    pub entries: Vec<SnapshotEntry>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub path: String,
    pub kind: String,
    pub hash: Option<String>,
    pub mode: u32,
    pub bytes: u64,
    pub symlink_target: Option<String>,
    pub deleted: bool,
}

pub fn capture(
    repository: &crate::workspace::RepositoryInfo,
    worktree: &Path,
    destination: &Path,
) -> Result<(SnapshotManifest, String)> {
    let observed = crate::workspace::inspect(worktree)?;
    if observed.identity != repository.identity {
        bail!("snapshot worktree belongs to another physical repository")
    }
    if !crate::workspace::commit_exists(repository, &observed.head)?
        || !crate::workspace::is_ancestor(repository, &repository.head, &observed.head)?
    {
        bail!("registered project base is not an ancestor of the candidate worktree head")
    }
    let mut paths = snapshot_paths(worktree)?;
    let base_output = Command::new("git")
        .arg("-C")
        .arg(&repository.root)
        .args(["ls-tree", "-r", "-z", "--name-only", &repository.head])
        .output()?;
    if !base_output.status.success() {
        bail!("git ls-tree failed while enumerating the original attempt base")
    }
    std::fs::create_dir_all(destination)?;
    let mut entries = Vec::new();
    let mut total = 0_u64;
    let base_paths = base_output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|raw| String::from_utf8(raw.to_vec()))
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    paths.extend(base_paths);
    for relative in paths {
        validate_relative(&relative)?;
        if Path::new(&relative)
            .components()
            .next()
            .is_some_and(|part| part.as_os_str() == METADATA_DIR)
        {
            bail!("snapshot path uses reserved metadata directory")
        }
        let source = worktree.join(&relative);
        let target = destination.join(&relative);
        let Some(metadata) = leaf_metadata(worktree, Path::new(&relative))? else {
            entries.push(SnapshotEntry {
                path: relative,
                kind: "deleted".into(),
                hash: None,
                mode: 0,
                bytes: 0,
                symlink_target: None,
                deleted: true,
            });
            continue;
        };
        if metadata.file_type().is_symlink() {
            let link = std::fs::read_link(&source)?;
            if link.is_absolute() || link.components().any(|part| part == Component::ParentDir) {
                bail!("snapshot rejects external or parent-traversing symlink {relative}")
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(&link, &target)?;
            entries.push(SnapshotEntry {
                path: relative,
                kind: "symlink".into(),
                hash: None,
                mode: metadata.mode(),
                bytes: 0,
                symlink_target: Some(link.to_string_lossy().into()),
                deleted: false,
            });
        } else if metadata.is_file() {
            if metadata.len() > FILE_LIMIT {
                bail!("snapshot file exceeds 10 MiB: {relative}")
            }
            total += metadata.len();
            if total > SNAPSHOT_LIMIT {
                bail!("snapshot exceeds 100 MiB bound")
            }
            let bytes = std::fs::read(&source)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &bytes)?;
            std::fs::set_permissions(
                &target,
                std::fs::Permissions::from_mode(metadata.mode() & 0o777),
            )?;
            entries.push(SnapshotEntry {
                path: relative,
                kind: "file".into(),
                hash: Some(hex::encode(Sha256::digest(&bytes))),
                mode: metadata.mode(),
                bytes: metadata.len(),
                symlink_target: None,
                deleted: false,
            });
        } else {
            bail!("snapshot rejects unsupported filesystem object {relative}")
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = SnapshotManifest {
        schema: 2,
        original_base: repository.head.clone(),
        candidate_head: observed.head.clone(),
        snapshot_base: observed.head,
        repository_identity: repository.identity.clone(),
        entries,
        total_bytes: total,
    };
    let hash = hex::encode(Sha256::digest(serde_json::to_vec(&manifest)?));
    crate::config::atomic_write(
        &destination.join(METADATA_DIR).join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok((manifest, hash))
}

pub fn materialize(
    manifest: &SnapshotManifest,
    snapshot_root: &Path,
    worktree: &Path,
) -> Result<()> {
    // Remove old leaves before creating replacements. A deleted Git leaf may
    // now be a directory, or be beneath a new file, without deleting that replacement.
    for entry in manifest.entries.iter().filter(|entry| entry.deleted) {
        validate_relative(&entry.path)?;
        if leaf_metadata(worktree, Path::new(&entry.path))?.is_some() {
            std::fs::remove_file(worktree.join(&entry.path))?;
        }
    }
    for entry in manifest.entries.iter().filter(|entry| !entry.deleted) {
        validate_relative(&entry.path)?;
        prepare_owned_ancestors(worktree, Path::new(&entry.path))?;
        let destination = worktree.join(&entry.path);
        let source = snapshot_root.join(&entry.path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::symlink_metadata(&destination).is_ok() {
            let metadata = std::fs::symlink_metadata(&destination)?;
            if metadata.is_dir() {
                std::fs::remove_dir_all(&destination)?
            } else {
                std::fs::remove_file(&destination)?
            }
        }
        match entry.kind.as_str() {
            "file" => {
                let bytes = std::fs::read(&source)?;
                if Some(hex::encode(Sha256::digest(&bytes))) != entry.hash {
                    bail!("snapshot content hash mismatch for {}", entry.path)
                }
                std::fs::write(&destination, bytes)?;
                std::fs::set_permissions(
                    &destination,
                    std::fs::Permissions::from_mode(entry.mode & 0o777),
                )?;
            }
            "symlink" => std::os::unix::fs::symlink(
                entry
                    .symlink_target
                    .as_deref()
                    .context("missing symlink target")?,
                destination,
            )?,
            other => bail!("unsupported manifest kind {other}"),
        }
    }
    if !verify_materialized(manifest, worktree)? {
        bail!("materialized snapshot inventory, content, or identity did not verify")
    }
    Ok(())
}

pub fn verify_integration(
    repository: &crate::workspace::RepositoryInfo,
    manifest: &SnapshotManifest,
    commit: &str,
) -> Result<Vec<String>> {
    let original_base = original_base(manifest);
    let candidate_head = candidate_head(manifest);
    if !crate::workspace::commit_exists(repository, original_base)?
        || !crate::workspace::commit_exists(repository, candidate_head)?
        || !crate::workspace::commit_exists(repository, commit)?
    {
        bail!("snapshot base, candidate head, or integration commit is unreachable")
    }
    if !crate::workspace::is_ancestor(repository, original_base, candidate_head)?
        || !crate::workspace::is_ancestor(repository, original_base, commit)?
    {
        bail!("candidate or integration commit does not descend from the original attempt base")
    }
    let mut changed = Vec::new();
    for entry in &manifest.entries {
        let base = commit_entry(repository, original_base, &entry.path)?;
        if entry_matches_commit(entry, base.as_ref()) {
            continue;
        }
        let integrated = commit_entry(repository, commit, &entry.path)?;
        if !entry_matches_commit(entry, integrated.as_ref()) {
            bail!("integration content/mode/type mismatch for {}", entry.path)
        }
        changed.push(entry.path.clone());
    }
    Ok(changed)
}

pub fn clean_candidate_commit(
    repository: &crate::workspace::RepositoryInfo,
    manifest: &SnapshotManifest,
) -> Result<Option<String>> {
    let head = candidate_head(manifest);
    if !crate::workspace::commit_exists(repository, head)? {
        return Ok(None);
    }
    for entry in &manifest.entries {
        let observed = commit_entry(repository, head, &entry.path)?;
        if !entry_matches_commit(entry, observed.as_ref()) {
            return Ok(None);
        }
    }
    Ok(Some(head.to_owned()))
}

pub fn verify_materialized(manifest: &SnapshotManifest, worktree: &Path) -> Result<bool> {
    let observed = crate::workspace::inspect(worktree)?;
    if observed.identity != manifest.repository_identity
        || observed.head != candidate_head(manifest)
    {
        return Ok(false);
    }
    for entry in &manifest.entries {
        validate_relative(&entry.path)?;
        if !entry_matches_path(entry, worktree)? {
            return Ok(false);
        }
    }
    let mut actual_paths = BTreeSet::new();
    for path in snapshot_paths(worktree)? {
        validate_relative(&path)?;
        if leaf_metadata(worktree, Path::new(&path))?.is_some() {
            actual_paths.insert(path);
        }
    }
    let expected_paths = manifest
        .entries
        .iter()
        .filter(|entry| !entry.deleted)
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    Ok(actual_paths == expected_paths)
}

#[derive(Clone)]
struct CommitEntry {
    kind: String,
    hash: Option<String>,
    mode: u32,
    symlink_target: Option<String>,
}

fn commit_entry(
    repository: &crate::workspace::RepositoryInfo,
    commit: &str,
    path: &str,
) -> Result<Option<CommitEntry>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&repository.root)
        .args(["ls-tree", commit, "--", path])
        .output()?;
    if !output.status.success() {
        bail!("git ls-tree failed for {path}")
    }
    let line = String::from_utf8(output.stdout)?.trim().to_owned();
    if line.is_empty() {
        return Ok(None);
    }
    let metadata = line
        .split('\t')
        .next()
        .context("malformed git tree entry")?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 {
        bail!("malformed git tree entry")
    }
    // Git directories are containers, not snapshot leaves. A former file at
    // this path is deleted even when new files now live below the same name.
    if fields[1] == "tree" {
        return Ok(None);
    }
    let mode = u32::from_str_radix(fields[0], 8)?;
    let blob = Command::new("git")
        .arg("-C")
        .arg(&repository.root)
        .args(["show", &format!("{commit}:{path}")])
        .output()?;
    if !blob.status.success() {
        bail!("read git blob failed for {path}")
    }
    if blob.stdout.len() as u64 > FILE_LIMIT {
        bail!("integration blob exceeds 10 MiB: {path}")
    }
    if fields[0] == "120000" {
        Ok(Some(CommitEntry {
            kind: "symlink".into(),
            hash: None,
            mode,
            symlink_target: Some(String::from_utf8(blob.stdout)?),
        }))
    } else {
        Ok(Some(CommitEntry {
            kind: "file".into(),
            hash: Some(hex::encode(Sha256::digest(blob.stdout))),
            mode,
            symlink_target: None,
        }))
    }
}

fn entry_matches_commit(entry: &SnapshotEntry, observed: Option<&CommitEntry>) -> bool {
    if entry.deleted {
        return observed.is_none();
    }
    let Some(observed) = observed else {
        return false;
    };
    entry.kind == observed.kind
        && entry.hash == observed.hash
        && entry.symlink_target == observed.symlink_target
        && expected_git_mode(entry) == observed.mode
}

fn entry_matches_path(entry: &SnapshotEntry, root: &Path) -> Result<bool> {
    let path = root.join(&entry.path);
    let Some(metadata) = leaf_metadata(root, Path::new(&entry.path))? else {
        return Ok(entry.deleted);
    };
    if entry.deleted {
        return Ok(false);
    }
    if metadata.file_type().is_symlink() {
        return Ok(entry.kind == "symlink"
            && Some(std::fs::read_link(path)?.to_string_lossy().into_owned())
                == entry.symlink_target
            && (metadata.mode() & 0o777) == (entry.mode & 0o777));
    }
    if !metadata.is_file() || entry.kind != "file" {
        return Ok(false);
    }
    let bytes = std::fs::read(path)?;
    Ok(Some(hex::encode(Sha256::digest(bytes))) == entry.hash
        && (metadata.mode() & 0o777) == (entry.mode & 0o777))
}

fn original_base(manifest: &SnapshotManifest) -> &str {
    if manifest.original_base.is_empty() {
        &manifest.snapshot_base
    } else {
        &manifest.original_base
    }
}

fn candidate_head(manifest: &SnapshotManifest) -> &str {
    if manifest.candidate_head.is_empty() {
        &manifest.snapshot_base
    } else {
        &manifest.candidate_head
    }
}

fn expected_git_mode(entry: &SnapshotEntry) -> u32 {
    match entry.kind.as_str() {
        "symlink" => 0o120000,
        "file" if entry.mode & 0o111 != 0 => 0o100755,
        "file" => 0o100644,
        _ => 0,
    }
}

fn snapshot_paths(worktree: &Path) -> Result<BTreeSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    if !output.status.success() {
        bail!("git ls-files failed while enumerating snapshot paths")
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|raw| String::from_utf8(raw.to_vec()).map_err(Into::into))
        .collect()
}

/// Resolve Git leaves without following symlink ancestors. A file replacing a
/// directory makes its old descendants absent; a directory replacing a file
/// makes that old leaf absent. Other I/O failures must not look like deletion.
fn leaf_metadata(root: &Path, relative: &Path) -> Result<Option<std::fs::Metadata>> {
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for component in relative.components().take(count.saturating_sub(1)) {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.is_dir() => return Ok(None),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    match std::fs::symlink_metadata(root.join(relative)) {
        Ok(metadata) if metadata.is_dir() => Ok(None),
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn prepare_owned_ancestors(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for component in relative.components().take(count.saturating_sub(1)) {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
                std::fs::remove_file(&current)?;
                std::fs::create_dir(&current)?
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!("unsupported ancestor object {}", current.display())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_relative(path: &str) -> Result<()> {
    let path = PathBuf::from(path);
    if path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("unsafe snapshot path {}", path.display())
    }
    Ok(())
}
