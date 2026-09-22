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
