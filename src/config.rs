use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const DEFAULT_PORT: u16 = 4317;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstancePaths {
    pub root: PathBuf,
    pub state: PathBuf,
    pub logs: PathBuf,
    pub transcripts: PathBuf,
    pub artifacts: PathBuf,
    pub runtime: PathBuf,
    pub socket_dir: PathBuf,
    pub hooks: PathBuf,
    pub database: PathBuf,
    pub instance_file: PathBuf,
    pub lock_file: PathBuf,
    pub control_socket: PathBuf,
    pub role_socket: PathBuf,
}

impl InstancePaths {
    pub fn resolve(override_path: Option<PathBuf>) -> Result<Self> {
        let root = match override_path {
            Some(path) => {
                if !path.is_absolute() {
                    bail!("--data-dir must be an absolute path: {}", path.display());
                }
                canonical_identity(&path)?
            }
            None => {
                let home = env::var_os("HOME").context("HOME is not set; pass --data-dir")?;
                canonical_identity(
                    &PathBuf::from(home)
                        .join("Library")
                        .join("Application Support")
                        .join("AgenticJira"),
                )?
            }
        };
        let state = root.join("state");
        let runtime = root.join("run");
        let socket_dir = short_socket_directory(&root);
        Ok(Self {
            database: state.join("agenticjira.sqlite3"),
            instance_file: runtime.join("instance.json"),
            lock_file: runtime.join("instance.lock"),
            control_socket: socket_dir.join("control.sock"),
            role_socket: socket_dir.join("role.sock"),
            logs: root.join("logs"),
            transcripts: root.join("transcripts"),
            artifacts: root.join("artifacts"),
            hooks: root.join("hooks").join("v1"),
            socket_dir,
            root,
            state,
            runtime,
        })
    }

    pub fn create(&self) -> Result<()> {
        for path in [
            &self.root,
            &self.state,
            &self.logs,
            &self.transcripts,
            &self.artifacts,
            &self.runtime,
            &self.hooks,
        ] {
            fs::create_dir_all(path)
                .with_context(|| format!("create instance directory {}", path.display()))?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                bail!(
                    "instance directory must be owned and must not be a symlink: {}",
                    path.display()
                )
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            validate_private_directory(path)?;
        }
        create_private_socket_directory(&self.socket_dir)?;
        Ok(())
    }
}

fn validate_private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!(
            "instance directory must not be a symlink: {}",
            path.display()
        )
    }
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!(
            "instance directory must be owned by the current user: {}",
            path.display()
        )
    }
    if metadata.mode() & 0o077 != 0 {
        bail!(
            "instance directory must have owner-only permissions: {}",
            path.display()
        )
    }
    Ok(())
}

fn canonical_identity(path: &Path) -> Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::<OsString>::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .context("data directory has no existing ancestor")?;
        missing.push(name.to_owned());
        existing = existing
            .parent()
            .context("data directory has no existing ancestor")?;
    }
    let mut resolved = fs::canonicalize(existing).with_context(|| {
        format!(
            "canonicalize data directory ancestor {}",
            existing.display()
        )
    })?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn create_private_socket_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_socket_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .with_context(|| format!("create private socket directory {}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            let metadata = fs::symlink_metadata(path)?;
            validate_socket_directory(path, &metadata)
        }
        Err(error) => {
            Err(error).with_context(|| format!("inspect socket directory {}", path.display()))
        }
    }
}

fn validate_socket_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        bail!("socket directory must not be a symlink: {}", path.display())
    }
    if !metadata.is_dir() {
        bail!("socket path is not a directory: {}", path.display())
    }
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        bail!(
            "socket directory {} is owned by uid {}, expected {}",
            path.display(),
            metadata.uid(),
            effective_uid
        )
    }
    if metadata.mode() & 0o077 != 0 {
        bail!(
            "socket directory {} has group/world permissions {:03o}; remove it or set mode 0700",
            path.display(),
            metadata.mode() & 0o777
        )
    }
    Ok(())
}

fn short_socket_directory(root: &Path) -> PathBuf {
    let digest = hex::encode(Sha256::digest(root.as_os_str().as_encoded_bytes()));
    #[cfg(target_os = "macos")]
    let base = PathBuf::from("/private/tmp");
    #[cfg(not(target_os = "macos"))]
    let base = std::env::temp_dir();
    base.join(format!("agenticjira-{}", &digest[..20]))
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("file has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temporary, bytes).with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}
