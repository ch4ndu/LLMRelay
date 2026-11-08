use crate::config::InstancePaths;
use crate::control::{self, ControlRequest};
use crate::database::InstanceLock;
use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RECORD_LIMIT: u64 = 64 * 1024;
const EXECUTABLE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OwnedFile {
    path: PathBuf,
    hash: String,
    mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u8,
    uid: u32,
    root: PathBuf,
    generation: String,
    revision: u64,
    desired_running: bool,
    login_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login_pending: Option<LoginOperation>,
    files: Vec<OwnedFile>,
    login_file: OwnedFile,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingInstall {
    previous: Option<Manifest>,
    next: Manifest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginOperation {
    Enable,
    Disable,
}

impl LoginOperation {
    fn enabled(self) -> bool {
        self == Self::Enable
    }

    fn command(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledStopBinding {
    generation: String,
    revision: u64,
    incarnation: Option<String>,
}

struct Layout {
    paths: InstancePaths,
    home: PathBuf,
    directory: PathBuf,
    lifecycle: PathBuf,
    manifest: PathBuf,
    pending: PathBuf,
    login: PathBuf,
    label: String,
    uid: u32,
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub instance_key: String,
    pub data_dir: PathBuf,
    pub diagnostics: PathBuf,
    pub installed: bool,
    pub login_enabled: bool,
    pub login_pending: Option<LoginOperation>,
    pub desired_running: bool,
    pub generation: Option<String>,
    pub pending_installation: bool,
    pub observed: String,
    pub incarnation: Option<String>,
}

// The lifecycle inode lives outside the removable installation. It is never
// unlinked, so concurrent commands cannot acquire locks on different inodes.
struct LifecycleLock(File);
impl Drop for LifecycleLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(crate) fn owner_identity() -> Result<(PathBuf, String, u32)> {
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        bail!("Run LLMRelay as the signed-in user, not root")
    }
    let mut buffer = vec![0_u8; 16 * 1024];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    // getpwuid_r owns no pointers beyond buffer's lifetime; copy both fields.
    let code = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if code != 0 || found.is_null() {
        bail!("Cannot resolve the current user's home directory")
    }
    let entry = unsafe { entry.assume_init() };
    if entry.pw_dir.is_null() || entry.pw_name.is_null() {
        bail!("User identity is incomplete")
    }
    let home = PathBuf::from(unsafe { CStr::from_ptr(entry.pw_dir) }.to_str()?);
    let user = unsafe { CStr::from_ptr(entry.pw_name) }
        .to_str()?
        .to_owned();
    let home = fs::canonicalize(home)?;
    let metadata = fs::symlink_metadata(&home)?;
    if !metadata.is_dir() || metadata.uid() != uid {
        bail!("User home is not owned by the current user")
    }
    Ok((home, user, uid))
}

pub(crate) fn environment() -> Result<Vec<(String, String)>> {
    let (home, user, _) = owner_identity()?;
    Ok(vec![
        (
            "HOME".into(),
            home.to_str().context("home path is not UTF-8")?.into(),
        ),
        ("USER".into(), user.clone()),
        ("LOGNAME".into(), user),
        (
            "PATH".into(),
            format!(
                "/opt/homebrew/bin:/usr/local/bin:{}/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                home.display()
            ),
        ),
        ("TMPDIR".into(), "/private/tmp".into()),
    ])
}

impl Layout {
    fn new(paths: &InstancePaths) -> Result<Self> {
        let (home, _, uid) = owner_identity()?;
        let key = paths.root_key();
        let base = home.join("Library/Application Support/LLMRelay/Services");
        let directory = base.join(&key);
        let label = format!("com.llmrelay.service.{key}");
        Ok(Self {
            paths: paths.clone(),
            uid,
            lifecycle: base.join(format!("{key}.lock")),
            manifest: directory.join("current.json"),
            pending: directory.join("pending.json"),
            login: home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist")),
            directory,
            label,
            home,
        })
    }

    fn lock(&self) -> Result<LifecycleLock> {
        self.lock_mode(false)
    }

    fn lock_mode(&self, wait: bool) -> Result<LifecycleLock> {
        create_owned_directories(
            &self.home,
            self.lifecycle
                .parent()
                .context("lifecycle parent missing")?,
        )?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.lifecycle)?;
        verify_metadata(&file.metadata()?, false, true)?;
        if wait {
            file.lock_exclusive()?;
        } else {
            file.try_lock_exclusive().context(
                "Another service command is in progress; inspect status before retrying",
            )?;
        }
        Ok(LifecycleLock(file))
    }

    fn generation_dir(&self, generation: &str) -> PathBuf {
        self.directory.join(generation)
    }
    fn executable(&self, manifest: &Manifest) -> PathBuf {
        self.generation_dir(&manifest.generation).join("llmrelay")
    }
    fn start_plist(&self, manifest: &Manifest) -> PathBuf {
        self.generation_dir(&manifest.generation)
            .join("start.plist")
    }
    fn target(&self) -> String {
        format!("gui/{}/{}", self.uid, self.label)
    }

    fn dormant_manifest(&self, executable: &[u8]) -> Result<Manifest> {
        let mut manifest = Manifest {
            schema: 1,
            uid: self.uid,
            root: self.paths.root.clone(),
            generation: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            desired_running: false,
            login_enabled: false,
            login_pending: None,
            files: Vec::new(),
            login_file: OwnedFile {
                path: self.login.clone(),
                hash: String::new(),
                mode: 0o600,
            },
        };
        manifest.login_file.hash = hash(&self.plist(&manifest, true)?);
        manifest.files = vec![
            OwnedFile {
                path: self.executable(&manifest),
                hash: hash(executable),
                mode: 0o700,
            },
            OwnedFile {
                path: self.start_plist(&manifest),
                hash: hash(&self.plist(&manifest, false)?),
                mode: 0o600,
            },
        ];
        Ok(manifest)
    }

    fn read_manifest(&self) -> Result<Option<Manifest>> {
        if self.directory.try_exists()? {
            verify_directory_chain(&self.home, &self.directory)?;
            verify_metadata(&fs::symlink_metadata(&self.directory)?, true, true)?;
        }
        let Some(bytes) = read_optional(&self.manifest, RECORD_LIMIT)? else {
            return Ok(None);
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).context("Installed service manifest is malformed")?;
        self.validate_manifest(&manifest)?;
        Ok(Some(manifest))
    }

    fn validate_manifest(&self, manifest: &Manifest) -> Result<()> {
        uuid::Uuid::parse_str(&manifest.generation).context("Invalid installed generation")?;
        if manifest.schema != 1
            || manifest.uid != self.uid
            || manifest.root != self.paths.root
            || manifest.files.len() != 2
            || manifest.files[0].path != self.executable(manifest)
            || manifest.files[0].mode != 0o700
            || manifest.files[1].path != self.start_plist(manifest)
            || manifest.files[1].mode != 0o600
            || manifest.login_file.path != self.login
            || manifest.login_file.mode != 0o600
            || manifest.login_file.hash != hash(&self.plist(manifest, true)?)
        {
            bail!("Installed service ownership does not match this instance")
        }
        for file in &manifest.files {
            if file.hash.len() != 64 || !file.hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("Installed service file identity is malformed")
            }
        }
        if hash(&self.plist(manifest, false)?) != manifest.files[1].hash {
            bail!("Installed service launch configuration has changed")
        }
        Ok(())
    }

    fn validate_files(&self, manifest: &Manifest, missing_ok: bool) -> Result<()> {
        verify_directory_chain(&self.home, &self.directory)?;
        let generation = self.generation_dir(&manifest.generation);
        match fs::symlink_metadata(&generation) {
            Ok(metadata) => verify_metadata(&metadata, true, true)?,
            Err(error) if missing_ok && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(())
            }
            Err(error) => return Err(error.into()),
        }
        for entry in fs::read_dir(&generation)? {
            let path = entry?.path();
            if let Some(file) = manifest
                .files
                .iter()
                .find(|file| publish_path(&file.path).as_ref().ok() == Some(&path))
            {
                if !missing_ok
                    || read_optional(&path, EXECUTABLE_LIMIT)?
                        .is_none_or(|bytes| hash(&bytes) != file.hash)
                    || fs::symlink_metadata(&path)?.mode() & 0o777 != file.mode
                {
                    bail!("Unrecognized interrupted generation publication")
                }
            } else if !manifest.files.iter().any(|file| file.path == path) {
                bail!("Unrecognized content in installed generation; no files were removed")
            }
        }
        for file in &manifest.files {
            let bytes = read_optional(&file.path, EXECUTABLE_LIMIT)?;
            match bytes {
                None if missing_ok => {}
                Some(bytes)
                    if hash(&bytes) == file.hash
                        && fs::symlink_metadata(&file.path)?.mode() & 0o777 == file.mode => {}
                _ => bail!(
                    "Installed file is missing or changed: {}",
                    file.path.display()
                ),
            }
        }
        Ok(())
    }

    fn plist(&self, manifest: &Manifest, login: bool) -> Result<Vec<u8>> {
        let strings = |values: Vec<String>| -> String {
            values
                .iter()
                .map(|value| format!("<string>{}</string>", xml(value)))
                .collect()
        };
        let args = strings(vec![
            self.executable(manifest).display().to_string(),
            "service-run".into(),
            "--data-dir".into(),
            self.paths.root.display().to_string(),
            "--generation".into(),
            manifest.generation.clone(),
        ]);
        let env = environment()?
            .iter()
            .map(|(key, value)| format!("<key>{}</key><string>{}</string>", xml(key), xml(value)))
            .collect::<String>();
        Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{args}</array><key>WorkingDirectory</key><string>{}</string><key>EnvironmentVariables</key><dict>{env}</dict><key>KeepAlive</key><false/><key>RunAtLoad</key><{}/><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>", xml(&self.label), xml(&self.home.display().to_string()), if login {"true"} else {"false"}, xml(&self.paths.logs.join("service.log").display().to_string()), xml(&self.paths.logs.join("service-error.log").display().to_string())).into_bytes())
    }

    fn validate_login(&self, manifest: &Manifest) -> Result<()> {
        if let Some(operation) = self.login_state(manifest)?.1 {
            bail!(
                "Login change is unfinished; repeat service login {}",
                operation.command()
            )
        }
        Ok(())
    }

    fn login_state(&self, manifest: &Manifest) -> Result<(bool, Option<LoginOperation>)> {
        let bytes = self.read_login()?;
        if let Some(bytes) = &bytes {
            if *bytes != self.plist(manifest, true)? {
                bail!("Login file is not owned by this service generation")
            }
        }
        let enabled = bytes.is_some();
        // Older manifests committed their preference last. A disagreement in
        // that format identifies the interrupted operation by its file effect.
        let pending = manifest.login_pending.or_else(|| {
            (enabled != manifest.login_enabled).then_some(if enabled {
                LoginOperation::Enable
            } else {
                LoginOperation::Disable
            })
        });
        Ok((enabled, pending))
    }

    fn read_login(&self) -> Result<Option<Vec<u8>>> {
        let parent = self.login.parent().context("login parent missing")?;
        if parent.try_exists()? {
            verify_directory_chain(&self.home, parent)?;
        }
        let bytes = read_optional(&self.login, RECORD_LIMIT)?;
        if bytes.is_some() && fs::symlink_metadata(&self.login)?.mode() & 0o777 != 0o600 {
            bail!("Login file permissions differ from the owned installation")
        }
        Ok(bytes)
    }

    fn publish(&self, manifest: &Manifest) -> Result<()> {
        self.validate_manifest(manifest)?;
        synced_write(&self.manifest, &serde_json::to_vec(manifest)?, 0o600)
    }

    fn set_desired_running(&self, manifest: &mut Manifest, desired: bool) -> Result<()> {
        if let Some(bytes) = read_optional(&self.pending, RECORD_LIMIT)? {
            let pending: PendingInstall = serde_json::from_slice(&bytes)?;
            self.validate_manifest(&pending.next)?;
            if let Some(previous) = &pending.previous {
                self.validate_manifest(previous)?;
            }
            if desired
                || manifest.desired_running
                || (&*manifest != &pending.next && Some(&*manifest) != pending.previous.as_ref())
            {
                bail!("Reconcile the pending installation before changing service intent")
            }
        }
        // Stop must preserve the exact committed-next identity during install
        // recovery. Start still advances intent even when it was already true.
        if !desired && !manifest.desired_running {
            return Ok(());
        }
        manifest.desired_running = desired;
        manifest.revision = manifest
            .revision
            .checked_add(1)
            .context("Service intent revision exhausted")?;
        self.publish(manifest)
    }

    fn prepare_login(&self, manifest: &mut Manifest, operation: LoginOperation) -> Result<()> {
        if let Some(pending) = self.login_state(manifest)?.1 {
            if pending != operation {
                bail!(
                    "Login change is unfinished; repeat service login {} first",
                    pending.command()
                )
            }
        }
        if manifest.login_pending.is_none() {
            manifest.login_pending = Some(operation);
            manifest.revision = manifest
                .revision
                .checked_add(1)
                .context("Service intent revision exhausted")?;
            self.publish(manifest)?;
        }
        Ok(())
    }

    fn finish_login(&self, manifest: &mut Manifest, operation: LoginOperation) -> Result<()> {
        if manifest.login_pending != Some(operation) {
            bail!("Login operation changed before publication")
        }
        self.login_state(manifest)?;
        if operation.enabled() {
            create_owned_directories(
                &self.home,
                self.login.parent().context("login parent missing")?,
            )?;
            synced_write(&self.login, &self.plist(manifest, true)?, 0o600)?;
        } else {
            remove_if_present(&self.login)?;
        }
        manifest.login_enabled = operation.enabled();
        manifest.login_pending = None;
        manifest.revision = manifest
            .revision
            .checked_add(1)
            .context("Service intent revision exhausted")?;
        self.publish(manifest)
    }

    fn recorded_incarnation(&self) -> Result<Option<String>> {
        let Some(bytes) = read_owned_optional(&self.paths.instance_file, RECORD_LIMIT, false)?
        else {
            return Ok(None);
        };
        let record: crate::server::InstanceRecord = serde_json::from_slice(&bytes)?;
        if record.data_dir != self.paths.root || record.control_socket != self.paths.control_socket
        {
            bail!("Recorded host belongs to another instance")
        }
        uuid::Uuid::parse_str(&record.boot_id).context("Recorded host incarnation is invalid")?;
        Ok(Some(record.boot_id))
    }

    fn prepare_uninstall(&self) -> Result<InstalledStopBinding> {
        if read_optional(&self.pending, RECORD_LIMIT)?.is_some() {
            bail!("Reconcile pending installation before uninstalling")
        }
        let manifest = self
            .read_manifest()?
            .context("This instance is not installed")?;
        self.validate_removal(&manifest)?;
        Ok(InstalledStopBinding {
            generation: manifest.generation,
            revision: manifest.revision,
            incarnation: self.recorded_incarnation()?,
        })
    }

    fn bound_manifest(&self, binding: &InstalledStopBinding) -> Result<Manifest> {
        let manifest = self
            .read_manifest()?
            .context("Installation changed during uninstall")?;
        if manifest.generation != binding.generation
            || manifest.revision != binding.revision
            || self.recorded_incarnation()? != binding.incarnation
            || read_optional(&self.pending, RECORD_LIMIT)?.is_some()
        {
            bail!("Start, installation, or host changed during uninstall; inspect status before retrying")
        }
        Ok(manifest)
    }

    fn clear_bound_intent(
        &self,
        binding: &InstalledStopBinding,
        host: Option<&str>,
    ) -> Result<InstalledStopBinding> {
        if host.is_some() && binding.incarnation.as_deref() != host {
            bail!("The host changed during uninstall; no stop was requested")
        }
        let mut manifest = self.bound_manifest(binding)?;
        self.set_desired_running(&mut manifest, false)?;
        Ok(InstalledStopBinding {
            revision: manifest.revision,
            ..binding.clone()
        })
    }

    fn validate_removal(&self, manifest: &Manifest) -> Result<()> {
        self.validate_files(manifest, true)?;
        // Cleanup may already have removed the plist before the manifest.
        // Validate remaining bytes but require explicit login changes to finish first.
        self.login_state(manifest)?;
        if let Some(operation) = manifest.login_pending {
            bail!(
                "Login change is unfinished; repeat service login {} before uninstalling",
                operation.command()
            )
        }
        // Older manifests can omit login_pending while publication is staged.
        match fs::symlink_metadata(publish_path(&self.login)?) {
            Ok(metadata) => {
                verify_metadata(&metadata, false, true)?;
                bail!("Login publication is unfinished; repeat its original service login command before uninstalling")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.validate_installation_entries(&[manifest], false)
    }

    fn loaded(&self, manifest: &Manifest) -> Result<bool> {
        let (success, output) = launchctl(&["print", &self.target()])?;
        if !success {
            if output.contains("Could not find service") {
                return Ok(false);
            }
            bail!("Cannot inspect the current login session's service registration")
        }
        let field = |key: &str| {
            output
                .lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix(key))
        };
        let executable = self.executable(manifest).display().to_string();
        let start = self.start_plist(manifest).display().to_string();
        let login = self.login.display().to_string();
        let arguments = output
            .split("arguments = {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .map(|value| {
                value
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
            });
        let root = self.paths.root.display().to_string();
        if field("program = ") != Some(executable.as_str())
            || !matches!(field("path = "), Some(path) if path == start || path == login)
            || arguments
                != Some(vec![
                    executable.as_str(),
                    "service-run",
                    "--data-dir",
                    root.as_str(),
                    "--generation",
                    manifest.generation.as_str(),
                ])
        {
            bail!("The loaded service label has an unrecognized executable or arguments; refusing to control it")
        }
        Ok(true)
    }

    fn stopped_owner(&self) -> Result<InstanceLock> {
        let lock = InstanceLock::acquire(&self.paths)?;
        if self.paths.database.try_exists()? {
            crate::database::validate_live_database_family(&self.paths)?;
            let store = crate::store::Store::open_current_readonly(&self.paths.database)?;
            crate::recovery::verify_installed_root_quiescent(&store)?;
        }
        Ok(lock)
    }

    fn remove_generation(&self, manifest: &Manifest) -> Result<()> {
        self.validate_files(manifest, true)?;
        for file in &manifest.files {
            remove_if_present(&file.path)?;
            remove_if_present(&publish_path(&file.path)?)?;
        }
        let directory = self.generation_dir(&manifest.generation);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                verify_metadata(&metadata, true, true)?;
                fs::remove_dir(directory)?;
                sync_dir(&self.directory)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn validate_installation_entries(&self, manifests: &[&Manifest], pending: bool) -> Result<()> {
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path == self.manifest
                || (pending && path == self.pending)
                || manifests
                    .iter()
                    .any(|manifest| path == self.generation_dir(&manifest.generation))
            {
                continue;
            }
            if pending && path == publish_path(&self.manifest)? {
                let bytes = read_optional(&path, RECORD_LIMIT)?
                    .context("Interrupted manifest disappeared")?;
                if manifests.iter().any(|manifest| {
                    serde_json::to_vec(manifest).is_ok_and(|expected| expected == bytes)
                }) {
                    continue;
                }
            }
            bail!("Unrecognized or interrupted installation content was retained; reconcile its original command before installing")
        }
        Ok(())
    }

    fn reconcile_pending(&self, executable: &[u8]) -> Result<()> {
        let Some(bytes) = read_optional(&self.pending, RECORD_LIMIT)? else {
            return Ok(());
        };
        let pending: PendingInstall =
            serde_json::from_slice(&bytes).context("Pending installation is malformed")?;
        self.validate_manifest(&pending.next)?;
        if let Some(previous) = &pending.previous {
            self.validate_manifest(previous)?;
        }
        let current = self.read_manifest()?;
        if current.as_ref() != Some(&pending.next) && current != pending.previous {
            bail!("Pending installation does not match the committed generation")
        }
        let mut manifests = vec![&pending.next];
        manifests.extend(pending.previous.as_ref());
        self.validate_installation_entries(&manifests, true)?;
        if hash(executable) != pending.next.files[0].hash {
            bail!("Repeat install using the exact executable that staged the interrupted installation")
        }
        let _instance = self.stopped_owner()?;
        if let Some(previous) = &pending.previous {
            if previous.desired_running {
                bail!("Stop the installed service before replacing it")
            }
            self.validate_files(previous, current.as_ref() == Some(&pending.next))?;
            if let Some(bytes) = self.read_login()? {
                if bytes != self.plist(previous, true)? {
                    bail!("Login file belongs to another generation")
                }
            }
            if self.loaded(previous)? {
                checked_launchctl(&["bootout", &self.target()])?;
            }
        }
        self.validate_files(&pending.next, true)?;
        let directory = self.generation_dir(&pending.next.generation);
        create_owned_directories(&self.home, &directory)?;
        for (file, bytes) in pending
            .next
            .files
            .iter()
            .zip([executable.to_vec(), self.plist(&pending.next, false)?])
        {
            if read_optional(&file.path, EXECUTABLE_LIMIT)?.is_none() {
                synced_write(&file.path, &bytes, file.mode)?;
            }
        }
        self.validate_files(&pending.next, false)?;
        self.publish(&pending.next)?;
        if let Some(previous) = &pending.previous {
            if self.read_login()?.is_some() {
                remove_if_present(&self.login)?;
            }
            self.remove_generation(previous)?;
        }
        remove_if_present(&self.pending)
    }
}

pub(crate) fn install(paths: &InstancePaths) -> Result<()> {
    require_macos()?;
    let layout = Layout::new(paths)?;
    let _lifecycle = layout.lock()?;
    paths.create()?;
    create_owned_directories(&layout.home, &layout.directory)?;
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let metadata = fs::metadata(&executable)?;
    if !metadata.is_file() || metadata.len() > EXECUTABLE_LIMIT {
        bail!("Installer executable is not a bounded regular file")
    }
    let bytes = fs::read(executable)?;
    let staged_pending = publish_path(&layout.pending)?;
    if let Some(staged) = read_optional(&staged_pending, RECORD_LIMIT)? {
        let pending: PendingInstall = serde_json::from_slice(&staged)
            .context("Interrupted pending record is malformed; installation retained")?;
        layout.validate_manifest(&pending.next)?;
        if let Some(previous) = &pending.previous {
            layout.validate_manifest(previous)?;
        }
        if layout.read_manifest()? != pending.previous || hash(&bytes) != pending.next.files[0].hash
        {
            bail!("Interrupted installation belongs to another generation or executable")
        }
        if read_optional(&layout.pending, RECORD_LIMIT)?
            .is_some_and(|committed| committed != staged)
        {
            bail!("Pending installation records disagree; files were retained")
        }
        synced_write(&layout.pending, &staged, 0o600)?;
    }
    if layout.pending.try_exists()? {
        return layout.reconcile_pending(&bytes);
    }
    let previous = layout.read_manifest()?;
    layout.validate_installation_entries(&previous.iter().collect::<Vec<_>>(), false)?;
    if previous.is_none() && layout.read_login()?.is_some() {
        bail!("A login file exists without its owning installation; it was retained")
    }
    let _instance = layout.stopped_owner()?;
    if let Some(previous) = &previous {
        if previous.desired_running {
            bail!("Stop the installed service before replacing it")
        }
        layout.validate_files(previous, false)?;
        layout.validate_login(previous)?;
    }
    let next = layout.dormant_manifest(&bytes)?;
    synced_write(
        &layout.pending,
        &serde_json::to_vec(&PendingInstall { previous, next })?,
        0o600,
    )?;
    drop(_instance);
    layout.reconcile_pending(&bytes)
}

pub(crate) fn start(paths: &InstancePaths) -> Result<()> {
    require_macos()?;
    let layout = Layout::new(paths)?;
    let _lifecycle = layout.lock()?;
    if layout.pending.try_exists()? {
        bail!("Installation is pending; repeat service install before Start")
    }
    let mut manifest = layout
        .read_manifest()?
        .context("Install this instance before Start")?;
    layout.validate_files(&manifest, false)?;
    // Also refuse a foreground owner: Start never replaces an active instance.
    let _instance = InstanceLock::acquire(paths)?;
    let loaded = layout.loaded(&manifest)?;
    layout.set_desired_running(&mut manifest, true)?;
    if !loaded {
        checked_launchctl(&[
            "bootstrap",
            &format!("gui/{}", layout.uid),
            &layout.start_plist(&manifest).display().to_string(),
        ])?;
    }
    // service-run must acquire these same locks after kickstart returns.
    checked_launchctl(&["kickstart", &layout.target()])
}

pub(crate) fn login(paths: &InstancePaths, enabled: bool) -> Result<()> {
    require_macos()?;
    let layout = Layout::new(paths)?;
    let _lifecycle = layout.lock()?;
    if layout.pending.try_exists()? {
        bail!("Installation is pending; repeat service install")
    }
    let mut manifest = layout
        .read_manifest()?
        .context("Install this instance before changing login behavior")?;
    layout.validate_files(&manifest, false)?;
    let operation = if enabled {
        LoginOperation::Enable
    } else {
        LoginOperation::Disable
    };
    layout.prepare_login(&mut manifest, operation)?;
    layout.finish_login(&mut manifest, operation)
}

pub(crate) fn request_bound_drain(
    app: &crate::operations::Application,
    binding: &InstalledStopBinding,
) -> Result<InstalledStopBinding> {
    let layout = Layout::new(&app.paths)?;
    let _lifecycle = layout.lock()?;
    let stopped = layout.clear_bound_intent(binding, Some(app.service_boot_id()))?;
    // The host continuously owns InstanceLock. Close dispatch under the same
    // lifecycle ownership as the bound intent write; Start cannot intervene.
    app.request_drain();
    Ok(stopped)
}

pub(crate) fn clear_desired_running(paths: &InstancePaths) -> Result<()> {
    let layout = Layout::new(paths)?;
    // Foreground-only instances never create service records or locks.
    if !layout.directory.try_exists()? {
        return Ok(());
    }
    let _lifecycle = layout.lock()?;
    if let Some(mut manifest) = layout.read_manifest()? {
        layout.set_desired_running(&mut manifest, false)?;
    }
    Ok(())
}

pub(crate) async fn stop(paths: &InstancePaths) -> Result<serde_json::Value> {
    let owned = paths.clone();
    tokio::task::spawn_blocking(move || clear_desired_running(&owned)).await??;
    // Never send IPC while owning the lifecycle lock: Stop publishes intent too.
    match control::request(&paths.control_socket, &ControlRequest::Stop { drain: true }).await {
        Ok(response) if response.ok => Ok(response.result),
        Ok(response) => bail!("Stop was refused: {}", response.error.unwrap_or_default()),
        Err(error) => {
            let owned = paths.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let layout = Layout::new(&owned)?;
                let _lifecycle = layout.lock()?;
                if let Some(mut manifest) = layout.read_manifest()? {
                    layout.set_desired_running(&mut manifest, false)?;
                }
                let _instance = layout.stopped_owner()?;
                Ok(())
            })
            .await??;
            Ok(
                serde_json::json!({"draining":false,"quiescent":true,"observed":"offline","message":format!("No host is listening: {error}")}),
            )
        }
    }
}

pub(crate) async fn uninstall(paths: &InstancePaths) -> Result<()> {
    let initial = paths.clone();
    let before = tokio::task::spawn_blocking(move || -> Result<InstalledStopBinding> {
        let layout = Layout::new(&initial)?;
        let _lifecycle = layout.lock()?;
        layout.prepare_uninstall()
    })
    .await??;
    // No IPC while lifecycle is held. A newly appearing host receives the
    // original incarnation binding and must refuse before touching its intent.
    let request = ControlRequest::StopInstalled {
        binding: before.clone(),
    };
    let stopped = match control::request(&paths.control_socket, &request).await {
        Ok(response) if response.ok => {
            serde_json::from_value::<InstalledStopBinding>(response.result["binding"].clone())?
        }
        Ok(response) => bail!(
            "Uninstall stop was refused: {}",
            response.error.unwrap_or_default()
        ),
        Err(_) => {
            let owned = paths.clone();
            let binding = before.clone();
            tokio::task::spawn_blocking(move || -> Result<InstalledStopBinding> {
                let layout = Layout::new(&owned)?;
                let _lifecycle = layout.lock()?;
                let _instance = layout.stopped_owner()?;
                layout.clear_bound_intent(&binding, None)
            })
            .await??
        }
    };
    if stopped.generation != before.generation
        || stopped.incarnation != before.incarnation
        || !(stopped.revision == before.revision
            || Some(stopped.revision) == before.revision.checked_add(1))
    {
        bail!("Uninstall stop returned a different installation binding")
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let waiting = paths.clone();
        match tokio::task::spawn_blocking(move || InstanceLock::acquire(&waiting).map(drop)).await?
        {
            Ok(()) => break,
            Err(error) if tokio::time::Instant::now() >= deadline => {
                return Err(error).context(
                    "Drain has not released instance ownership; the installation was retained",
                )
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    let owned = paths.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let layout = Layout::new(&owned)?;
        let _lifecycle = layout.lock()?;
        let _instance = layout.stopped_owner().context(
            "Drain is unfinished or prior process ownership is unknown; keep the installation and retry after recovery",
        )?;
        let manifest = layout.bound_manifest(&stopped)?;
        if manifest.desired_running {
            bail!("Start changed during uninstall; the installation was retained")
        }
        layout.validate_removal(&manifest)?;
        if layout.loaded(&manifest)? {
            checked_launchctl(&["bootout", &layout.target()])?;
        }
        layout.remove_generation(&manifest)?;
        remove_if_present(&layout.login)?;
        remove_if_present(&layout.manifest)?;
        // No recursive deletion: state, logs, history, recovery and repositories stay.
        fs::remove_dir(&layout.directory).context("Unrecognized installation files were retained")?;
        sync_dir(layout.directory.parent().context("installation parent missing")?)
    })
    .await?
}

pub(crate) async fn status(paths: &InstancePaths) -> Result<ServiceStatus> {
    let owned = paths.clone();
    let mut status = tokio::task::spawn_blocking(move || -> Result<ServiceStatus> {
        let layout = Layout::new(&owned)?;
        let manifest = layout.read_manifest()?;
        let pending = layout.pending.try_exists()?;
        let mut login = (false, None);
        if let Some(manifest) = &manifest {
            layout.validate_files(manifest, false)?;
            login = layout.login_state(manifest)?;
        }
        Ok(ServiceStatus {
            instance_key: owned.root_key(),
            data_dir: owned.root.clone(),
            diagnostics: owned.logs,
            installed: manifest.is_some(),
            login_enabled: login.0,
            login_pending: login.1,
            desired_running: manifest.as_ref().is_some_and(|m| m.desired_running),
            generation: manifest.map(|m| m.generation),
            pending_installation: pending,
            observed: "offline_or_unavailable".into(),
            incarnation: None,
        })
    })
    .await??;
    if let Ok(response) = control::request(&paths.control_socket, &ControlRequest::Status).await {
        if response.ok && response.result["instance"]["data_dir"].as_str() == paths.root.to_str() {
            status.observed = if response.result["coordinator"]["draining"] == true {
                "draining"
            } else {
                "running"
            }
            .into();
            status.incarnation = response.result["instance"]["boot_id"]
                .as_str()
                .map(str::to_owned);
        }
    }
    Ok(status)
}

pub(crate) fn acquire_startup(
    paths: &InstancePaths,
    generation: &str,
) -> Result<Arc<InstanceLock>> {
    require_macos()?;
    let layout = Layout::new(paths)?;
    // A kickstarted child may arrive before Start releases its lifecycle lock.
    // Wait for that publication, then acquire InstanceLock without waiting.
    let _lifecycle = layout.lock_mode(true)?;
    let lock = Arc::new(InstanceLock::acquire(paths)?);
    // Check after instance ownership; Stop/Start cannot change intent between
    // this check and acquiring the continuously retained server lock.
    let manifest = layout
        .read_manifest()?
        .context("Service installation is missing")?;
    if layout.pending.try_exists()?
        || manifest.generation != generation
        || !manifest.desired_running
    {
        bail!("Service generation is pending, replaced, or deliberately stopped")
    }
    layout.validate_files(&manifest, false)?;
    if fs::canonicalize(std::env::current_exe()?)? != layout.executable(&manifest) {
        bail!("service-run requires the exact installed executable")
    }
    Ok(lock)
}

pub(crate) fn enter_clean_environment(paths: &InstancePaths, generation: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let expected = environment()?
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let current = std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                key.into_string()
                    .map_err(|_| anyhow!("Invalid environment key"))?,
                value
                    .into_string()
                    .map_err(|_| anyhow!("Invalid environment value"))?,
            ))
        })
        .collect::<Result<std::collections::BTreeMap<_, _>>>();
    if current.as_ref().is_ok_and(|current| *current == expected) {
        return Ok(());
    }
    // launchd may inherit variables beyond EnvironmentVariables. Re-exec before
    // acquiring ownership or opening the store, with only the owned allowlist.
    let error = Command::new(std::env::current_exe()?)
        .args(["service-run", "--data-dir"])
        .arg(&paths.root)
        .args(["--generation", generation])
        .env_clear()
        .envs(expected)
        .exec();
    Err(error).context("Cannot enter the deterministic background environment")
}

fn require_macos() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Per-user service installation is supported on macOS only")
    }
    Ok(())
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn verify_metadata(metadata: &fs::Metadata, directory: bool, private: bool) -> Result<()> {
    if metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        })
        || metadata.mode() & (if private { 0o077 } else { 0o022 }) != 0
    {
        bail!(
            "Service path must be an owned, regular, non-symlink {}",
            if directory { "directory" } else { "file" }
        )
    }
    Ok(())
}
fn verify_directory_chain(home: &Path, directory: &Path) -> Result<()> {
    let mut current = home.to_path_buf();
    for component in directory.strip_prefix(home)?.components() {
        current.push(component);
        verify_metadata(&fs::symlink_metadata(&current)?, true, false)?;
    }
    Ok(())
}
fn create_owned_directories(home: &Path, directory: &Path) -> Result<()> {
    let mut current = home.to_path_buf();
    for component in directory.strip_prefix(home)?.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => verify_metadata(&metadata, true, false)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?;
                sync_dir(current.parent().context("directory parent missing")?)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    read_owned_optional(path, limit, true)
}
fn read_owned_optional(path: &Path, limit: u64, private: bool) -> Result<Option<Vec<u8>>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_metadata(&file.metadata()?, false, private)?;
    if file.metadata()?.len() > limit {
        bail!("Service file exceeds its size bound")
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("Service file grew beyond its size bound")
    }
    Ok(Some(bytes))
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all().map_err(Into::into)
}
fn publish_path(path: &Path) -> Result<PathBuf> {
    Ok(path
        .parent()
        .context("service file parent missing")?
        .join(format!(
            ".publish-{}",
            path.file_name()
                .context("service file name missing")?
                .to_string_lossy()
        )))
}
fn synced_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        verify_metadata(&metadata, false, true)?;
    }
    let parent = path.parent().context("service file parent missing")?;
    let temporary = publish_path(path)?;
    if let Some(pending) = read_optional(&temporary, EXECUTABLE_LIMIT)? {
        if pending != bytes {
            bail!("An interrupted publication has different bytes; repeat its original service command before changing intent")
        }
        if fs::symlink_metadata(&temporary)?.mode() & 0o777 != mode {
            bail!("Interrupted publication permissions changed")
        }
        File::open(&temporary)?.sync_all()?;
        fs::rename(&temporary, path)?;
        return sync_dir(parent);
    }
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_dir(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn remove_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            verify_metadata(&metadata, false, true)?;
            fs::remove_file(path)?;
            sync_dir(path.parent().context("file parent missing")?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn checked_launchctl(args: &[&str]) -> Result<()> {
    let (success, _) = launchctl(args)?;
    if !success {
        bail!("launchctl refused the service operation; inspect service status before retrying")
    }
    Ok(())
}
fn launchctl(args: &[&str]) -> Result<(bool, String)> {
    let mut child = Command::new("/bin/launchctl")
        .args(args)
        .env_clear()
        .envs(environment()?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().context("launchctl stdout missing")?;
    let mut stderr = child.stderr.take().context("launchctl stderr missing")?;
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Cannot bound launchctl output")
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let result = (|| -> Result<(bool, String)> {
        let mut finished = None;
        loop {
            for reader in [&mut stdout as &mut dyn Read, &mut stderr as &mut dyn Read] {
                let mut buffer = [0_u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            output.extend_from_slice(&buffer[..count]);
                            if output.len() > 64 * 1024 {
                                bail!("launchctl output exceeded its bound")
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            if let Some(status) = finished {
                return Ok((status, String::from_utf8(output)?));
            }
            if let Some(status) = child.try_wait()? {
                finished = Some(status.success());
                continue;
            }
            if Instant::now() >= deadline {
                bail!("launchctl did not finish; inspect status before retrying")
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_lifecycle_intent_and_prior_boot_unknown_refuse_cleanup() {
        let root = std::env::temp_dir().join(format!("llmrelay-service-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = InstancePaths::resolve(Some(root.join("instance"))).unwrap();
        paths.create().unwrap();
        let directory = root.join("installed");
        let layout = Layout {
            paths: paths.clone(),
            home: root.clone(),
            uid: unsafe { libc::geteuid() },
            lifecycle: root.join("instance-service.lock"),
            manifest: directory.join("current.json"),
            pending: directory.join("pending.json"),
            login: root.join("login.plist"),
            label: "com.llmrelay.fixture".into(),
            directory,
        };
        let lifecycle = layout.lock().unwrap();
        assert!(layout.lock().is_err());
        let instance = InstanceLock::acquire(&paths).unwrap();
        assert!(layout.stopped_owner().is_err());
        drop(instance);
        let mut manifest = layout.dormant_manifest(b"fixture executable").unwrap();
        create_owned_directories(&root, &layout.generation_dir(&manifest.generation)).unwrap();
        synced_write(&manifest.files[0].path, b"fixture executable", 0o700).unwrap();
        synced_write(
            &manifest.files[1].path,
            &layout.plist(&manifest, false).unwrap(),
            0o600,
        )
        .unwrap();
        layout.publish(&manifest).unwrap();
        assert!(!layout.read_manifest().unwrap().unwrap().desired_running);
        layout.set_desired_running(&mut manifest, true).unwrap();
        layout.set_desired_running(&mut manifest, false).unwrap();
        assert_eq!(layout.read_manifest().unwrap(), Some(manifest.clone()));
        assert!(!layout.login.exists());
        let pending = serde_json::to_vec(&PendingInstall {
            previous: None,
            next: manifest.clone(),
        })
        .unwrap();
        synced_write(&layout.pending, &pending, 0o600).unwrap();
        let stopped_revision = manifest.revision;
        for _ in 0..2 {
            layout.set_desired_running(&mut manifest, false).unwrap();
        }
        assert_eq!(manifest.revision, stopped_revision);
        assert_eq!(fs::read(&layout.pending).unwrap(), pending);
        let mut mismatched = manifest.clone();
        mismatched.revision += 1;
        assert!(layout.set_desired_running(&mut mismatched, false).is_err());
        layout.reconcile_pending(b"fixture executable").unwrap();
        assert_eq!(layout.read_manifest().unwrap(), Some(manifest.clone()));
        assert!(!layout.pending.exists());

        let assert_refused = |binding: &InstalledStopBinding, host: Option<&str>| {
            let before = fs::read(&layout.manifest).unwrap();
            let error = layout.clear_bound_intent(binding, host).unwrap_err();
            assert!(error.to_string().contains("changed"), "{error:#}");
            assert_eq!(fs::read(&layout.manifest).unwrap(), before);
        };
        layout.set_desired_running(&mut manifest, true).unwrap();
        let stale = layout.prepare_uninstall().unwrap();
        layout.set_desired_running(&mut manifest, true).unwrap();
        assert_refused(&stale, None);
        let current = layout.prepare_uninstall().unwrap();
        let instance = layout.stopped_owner().unwrap();
        let stopped = layout.clear_bound_intent(&current, None).unwrap();
        assert_eq!(stopped.revision, current.revision + 1);
        drop(instance);
        manifest = layout.read_manifest().unwrap().unwrap();
        assert!(!manifest.desired_running);
        assert!(layout.bound_manifest(&stale).is_err());
        let stale = layout.prepare_uninstall().unwrap();
        let replacement = layout.dormant_manifest(b"replacement").unwrap();
        layout.publish(&replacement).unwrap();
        assert_refused(&stale, None);
        layout.publish(&manifest).unwrap();

        let record_host = |boot: &str| {
            let record = crate::server::InstanceRecord {
                instance_id: uuid::Uuid::new_v4().to_string(),
                boot_id: boot.into(),
                pid: 1,
                address: "http://127.0.0.1:1".into(),
                data_dir: paths.root.clone(),
                control_socket: paths.control_socket.clone(),
                role_socket: paths.role_socket.clone(),
                started_at: "fixture".into(),
                status: "stopped".into(),
            };
            let bytes = serde_json::to_vec(&record).unwrap();
            crate::config::atomic_write(&paths.instance_file, &bytes).unwrap();
        };
        let first = uuid::Uuid::new_v4().to_string();
        record_host(&first);
        assert_refused(&stale, Some(&first));
        assert_refused(&stale, None);
        let stale = layout.prepare_uninstall().unwrap();
        let second = uuid::Uuid::new_v4().to_string();
        assert_refused(&stale, Some(&second));
        record_host(&second);
        assert_refused(&stale, None);
        let current = layout.prepare_uninstall().unwrap();
        let instance = layout.stopped_owner().unwrap();
        let stopped = layout.clear_bound_intent(&current, None).unwrap();
        assert_eq!(stopped.revision, current.revision);
        assert!(!layout.bound_manifest(&stopped).unwrap().desired_running);
        drop(instance);

        let stage = publish_path(&layout.login).unwrap();
        for operation in [LoginOperation::Enable, LoginOperation::Disable] {
            layout.prepare_login(&mut manifest, operation).unwrap();
            assert_eq!(
                layout.login_state(&manifest).unwrap(),
                (!operation.enabled(), Some(operation))
            );
            let error = layout.validate_login(&manifest).unwrap_err();
            assert!(error.to_string().contains(operation.command()));
            let error = layout.validate_removal(&manifest).unwrap_err();
            assert!(error.to_string().contains(operation.command()));
            if operation.enabled() {
                let plist = layout.plist(&manifest, true).unwrap();
                synced_write(&layout.login, &plist, 0o600).unwrap();
                synced_write(&stage, &plist, 0o600).unwrap();
                let mut legacy = manifest.clone();
                legacy.login_pending = None;
                layout.publish(&legacy).unwrap();
                let before = fs::read(&layout.manifest).unwrap();
                assert!(layout.prepare_uninstall().is_err());
                assert_eq!(fs::read(&stage).unwrap(), plist);
                assert_eq!(fs::read(&layout.manifest).unwrap(), before);
                layout.publish(&manifest).unwrap();
            } else {
                remove_if_present(&layout.login).unwrap();
            }
            manifest = layout.read_manifest().unwrap().unwrap();
            assert_eq!(
                layout.login_state(&manifest).unwrap(),
                (operation.enabled(), Some(operation))
            );
            let opposite = if operation.enabled() {
                LoginOperation::Disable
            } else {
                LoginOperation::Enable
            };
            assert!(layout.prepare_login(&mut manifest, opposite).is_err());
            let mut legacy = manifest.clone();
            legacy.login_pending = None;
            assert_eq!(layout.login_state(&legacy).unwrap().1, Some(operation));
            layout.prepare_login(&mut manifest, operation).unwrap();
            layout.finish_login(&mut manifest, operation).unwrap();
            assert!(!stage.exists());
            assert_eq!(
                layout.login_state(&manifest).unwrap(),
                (operation.enabled(), None)
            );
            assert!(!manifest.desired_running);
        }
        layout
            .prepare_login(&mut manifest, LoginOperation::Enable)
            .unwrap();
        layout
            .finish_login(&mut manifest, LoginOperation::Enable)
            .unwrap();
        let generation = layout.generation_dir(&manifest.generation);
        let saved_generation = root.join("retained-generation");
        fs::rename(&generation, &saved_generation).unwrap();
        std::os::unix::fs::symlink(root.join("absent-generation"), &generation).unwrap();
        let records = || {
            (
                fs::read(&layout.manifest).unwrap(),
                fs::read(&layout.login).unwrap(),
            )
        };
        let before = records();
        let error = layout.validate_removal(&manifest).unwrap_err();
        assert!(error.to_string().contains("non-symlink"));
        assert!(layout.remove_generation(&manifest).is_err());
        assert_eq!(records(), before);
        fs::remove_file(&generation).unwrap();
        fs::rename(&saved_generation, &generation).unwrap();
        let store = crate::store::Store::open(&paths.database).unwrap();
        store.lock().unwrap().execute_batch(
            "INSERT INTO provider_validation_batches(id,operation_id,request_hash,preview_hash,selection_json,
               max_fresh_calls,native_calls_authorized,state,created_at,updated_at)
               VALUES('prior-batch','prior-operation','h','p','[]',1,1,'running',
                 '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO provider_validation_invocations(id,batch_id,ordinal,provider,model,effort,evidence_key,
               state,created_at,updated_at)
               VALUES('prior-invocation','prior-batch',0,'codex','inert-local','none','prior-key',
                 'delivery_unknown','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
             INSERT INTO sessions(id,provider_validation_invocation_id,provider,status,launch_state,
               launch_config_json,executable_version,transcript_epoch,launch_boot_identity,created_at,updated_at)
               VALUES('prior-unknown','prior-invocation','codex','recovery_required','spawning',
                 '{}','fixture','prior-epoch','unversioned-unknown',
                 '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
        ).unwrap();
        let refusal = format!("{:#}", layout.stopped_owner().err().unwrap());
        assert!(refusal.contains("quiescence is unknown"), "{refusal}");
        assert!(layout.executable(&manifest).exists());
        let readonly = crate::store::Store::open_current_readonly(&paths.database).unwrap();
        assert!(readonly
            .lock()
            .unwrap()
            .execute("DELETE FROM sessions", [])
            .is_err());
        drop(readonly);
        synced_write(&layout.executable(&manifest), b"changed executable", 0o700).unwrap();
        assert!(layout.validate_files(&manifest, false).is_err());
        assert!(layout.executable(&manifest).exists());
        synced_write(&layout.executable(&manifest), b"fixture executable", 0o700).unwrap();
        // Retire only the synthetic unknown row after asserting its refusal.
        let retire_unknown = "DELETE FROM sessions WHERE id='prior-unknown'";
        store.lock().unwrap().execute(retire_unknown, []).unwrap();
        drop(store);
        let instance = layout.stopped_owner().unwrap();
        assert!(manifest.login_enabled && manifest.login_pending.is_none());
        layout.remove_generation(&manifest).unwrap();
        remove_if_present(&layout.login).unwrap();
        assert_eq!(layout.read_manifest().unwrap(), Some(manifest.clone()));
        let binding = layout.prepare_uninstall().unwrap();
        let stopped = layout.clear_bound_intent(&binding, None).unwrap();
        assert_eq!(layout.bound_manifest(&stopped).unwrap(), manifest);
        layout.validate_removal(&manifest).unwrap();
        assert!(!generation.exists() && !layout.login.exists());
        layout.remove_generation(&manifest).unwrap();
        remove_if_present(&layout.login).unwrap();
        remove_if_present(&layout.manifest).unwrap();
        fs::remove_dir(&layout.directory).unwrap();
        sync_dir(layout.directory.parent().unwrap()).unwrap();
        drop((instance, lifecycle));
        fs::remove_dir_all(root).unwrap();
    }
}
