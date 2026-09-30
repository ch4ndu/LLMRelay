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
    child
        .stdout
        .take()
        .context("git cat-file stdout")?
        .read_to_end(&mut output)?;
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
        let content = output
            .get(cursor..cursor + size)
            .context("git cat-file object was truncated")?;
        files.push((*kind == "blob").then(|| content.to_vec()));
        cursor += size + 1;
    }
    Ok(files)
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
