use crate::domain::{
    AttachmentBinding, ExecutableFingerprint, LaunchHandshake, ObservedProcessIdentity,
    PeerProcessIdentity, ProcessGenerationAnchor, ProcessIdentity, Provider, RolePeerProvenance,
};
use crate::providers::PreparedLaunch;
use crate::store::Store;
use crate::transcript::{self, TranscriptSink};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, Utc};
use portable_pty::{native_pty_system, MasterPty, PtySize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::sync::{Arc, Mutex, RwLock};

const SERVICE_STOP_SETTLING_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
pub(crate) const GRACEFUL_STOP_SECONDS: i64 = 30;

#[derive(Clone)]
pub struct Supervisor {
    store: Store,
    transcripts_root: std::path::PathBuf,
    sessions: Arc<RwLock<HashMap<String, Arc<SessionHandle>>>>,
    #[cfg(test)]
    process_inventory_failure: Arc<RwLock<Option<String>>>,
    #[cfg(test)]
    synthetic_attachments: Arc<RwLock<HashMap<String, AttachmentBinding>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptOutcome {
    Requested,
    AlreadyRequested,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SetupRetainedFirstTurnStopTimeoutOutcome {
    Quiescent,
    TimedOut,
    Stale,
}

struct SessionHandle {
    session_id: String,
    role_generation_id: String,
    transcript_epoch: String,
    process: ProcessIdentity,
    group_leader: ProcessGenerationAnchor,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    input: Mutex<Box<dyn Write + Send>>,
    io_boundary: Mutex<()>,
    known_members: Mutex<HashMap<u32, String>>,
    first_stop_requested_at: Mutex<Option<std::time::Instant>>,
    codex_helper_image: Option<Mutex<Option<ExecutableFingerprint>>>,
    codex_helper_identity: Mutex<Option<NativeHelperIdentity>>,
    _master: Mutex<Box<dyn MasterPty + Send>>,
}

fn setup_stop_timeout_receipt_matches_handle(
    receipt: &crate::store::SetupRetainedFirstTurnStopTimeoutReceipt,
    handle: &SessionHandle,
) -> bool {
    handle.session_id == receipt.session_id
        && handle.role_generation_id == receipt.generation_id
        && handle.transcript_epoch == receipt.transcript_epoch
        && serde_json::to_string(&handle.process).ok().as_deref()
            == Some(receipt.process_identity_json.as_str())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeHelperIdentity {
    pid: u32,
    native_start_marker: String,
}

#[derive(Clone, Debug)]
struct ProcessInfo {
    pid: u32,
    ppid: u32,
    pgid: i32,
    start: String,
    command: String,
}

fn peer_matches_managed_identity_or_descendant(
    peer: &PeerProcessIdentity,
    inventory: &[ProcessInfo],
    managed_members: &HashMap<u32, String>,
) -> Result<bool> {
    let by_pid = inventory
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let observed = by_pid.get(&peer.pid).ok_or_else(|| {
        anyhow!(
            "process inventory cannot establish identity for control peer PID {}",
            peer.pid
        )
    })?;
    if observed.start != peer.native_start_marker {
        bail!("control peer changed before ancestry validation")
    }
    let mut current = peer.pid;
    let mut visited = HashSet::new();
    while current > 1 && visited.insert(current) {
        let info = by_pid.get(&current).ok_or_else(|| {
            anyhow!("process inventory cannot establish ancestry for PID {current}")
        })?;
        if managed_members
            .get(&current)
            .is_some_and(|start| start == &info.start)
        {
            return Ok(true);
        }
        current = info.ppid;
    }
    Ok(false)
}

/// A positive proof captured once while the foreground host starts.  Merely
/// being able to reach cmux later is not a routing authority: the same exact
/// ancestor executable and start identity must still exist before an automatic
/// create, health, or focus call is allowed.
#[derive(Clone, Debug)]
pub(crate) struct CmuxHostAncestry {
    pid: u32,
    native_start_marker: String,
    executable: std::path::PathBuf,
}

impl CmuxHostAncestry {
    pub(crate) fn remains_current(&self) -> bool {
        let Ok(inventory) = process_inventory() else {
            return false;
        };
        let by_pid = inventory
            .iter()
            .map(|process| (process.pid, process))
            .collect::<HashMap<_, _>>();
        let mut current = std::process::id();
        let mut visited = HashSet::new();
        while current > 1 && visited.insert(current) {
            let Some(process) = by_pid.get(&current) else {
                return false;
            };
            if current == self.pid {
                return process.start == self.native_start_marker
                    && process_executable_path(self.pid)
                        .map(|path| path == self.executable)
                        .unwrap_or(false);
            }
            current = process.ppid;
        }
        false
    }

    pub(crate) fn verified_executable(&self) -> Option<std::path::PathBuf> {
        (self.executable.is_absolute() && self.remains_current()).then(|| self.executable.clone())
    }
}

pub(crate) fn capture_cmux_host_ancestry() -> Option<CmuxHostAncestry> {
    let inventory = process_inventory().ok()?;
    let by_pid = inventory
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut current = std::process::id();
    let mut visited = HashSet::new();
    while current > 1 && visited.insert(current) {
        let process = by_pid.get(&current)?;
        if current != std::process::id() && !process.start.trim().is_empty() {
            if let Ok(executable) = process_executable_path(current) {
                if executable.file_name().and_then(|name| name.to_str()) == Some("cmux") {
                    return Some(CmuxHostAncestry {
                        pid: current,
                        native_start_marker: process.start.clone(),
                        executable,
                    });
                }
            }
        }
        current = process.ppid;
    }
    None
}

#[cfg(test)]
pub(crate) fn current_process_cmux_ancestry_for_tests() -> Result<CmuxHostAncestry> {
    let pid = std::process::id();
    Ok(CmuxHostAncestry {
        pid,
        native_start_marker: native_start_marker(pid)?,
        executable: process_executable_path(pid)?,
    })
}

impl From<ProcessInfo> for ObservedProcessIdentity {
    fn from(process: ProcessInfo) -> Self {
        Self {
            pid: process.pid,
            parent_pid: process.ppid,
            process_group_id: process.pgid,
            native_start_marker: process.start,
        }
    }
}

#[derive(Debug)]
pub enum SpawnFailure {
    ProvenNondelivery {
        reason: String,
        owned_process_state: OwnedProcessState,
    },
    DeliveryUnknown {
        reason: String,
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
        members: Vec<ObservedProcessIdentity>,
    },
}

#[derive(Debug)]
pub enum OwnedProcessState {
    Quiescent {
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
        members: Vec<ObservedProcessIdentity>,
    },
    Uncertain {
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
        members: Vec<ObservedProcessIdentity>,
    },
}

impl OwnedProcessState {
    pub fn is_quiescent(&self) -> bool {
        matches!(self, Self::Quiescent { .. })
    }

    pub fn evidence(&self) -> (Option<u32>, Option<i32>, &[ObservedProcessIdentity]) {
        match self {
            Self::Quiescent {
                root_pid,
                process_group_id,
                members,
            }
            | Self::Uncertain {
                root_pid,
                process_group_id,
                members,
            } => (*root_pid, *process_group_id, members),
        }
    }
}

impl SpawnFailure {
    fn proven(error: impl std::fmt::Display) -> Self {
        Self::ProvenNondelivery {
            reason: error.to_string(),
            owned_process_state: OwnedProcessState::Quiescent {
                root_pid: None,
                process_group_id: None,
                members: Vec::new(),
            },
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::ProvenNondelivery { reason, .. } | Self::DeliveryUnknown { reason, .. } => reason,
        }
    }
}

impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for SpawnFailure {}

impl Supervisor {
    pub fn new(store: Store, transcripts_root: std::path::PathBuf) -> Self {
        Self {
            store,
            transcripts_root,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(test)]
            process_inventory_failure: Arc::new(RwLock::new(None)),
            #[cfg(test)]
            synthetic_attachments: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn process_inventory(&self) -> Result<Vec<ProcessInfo>> {
        #[cfg(test)]
        if let Some(reason) = self
            .process_inventory_failure
            .read()
            .map_err(|_| anyhow!("process inventory test seam lock poisoned"))?
            .clone()
        {
            bail!("{reason}")
        }
        process_inventory()
    }

    #[cfg(test)]
    fn fail_process_inventory_for_tests(&self, reason: Option<&str>) {
        *self.process_inventory_failure.write().unwrap() = reason.map(str::to_owned);
    }

    /// Installs an explicitly synthetic live attachment only for an in-process
    /// test. Production attachment establishment always verifies the managed
    /// OS process before returning a binding.
    #[cfg(test)]
    pub(crate) fn install_synthetic_attachment_for_tests(
        &self,
        binding: AttachmentBinding,
    ) -> Result<()> {
        self.store.verify_attachment_binding(&binding)?;
        self.synthetic_attachments
            .write()
            .map_err(|_| anyhow!("synthetic attachment test seam lock poisoned"))?
            .insert(binding.session_id.clone(), binding);
        Ok(())
    }

    pub fn spawn(
        &self,
        session_id: &str,
        role_generation_id: &str,
        transcript_epoch: &str,
        launch: &PreparedLaunch,
    ) -> std::result::Result<ProcessIdentity, SpawnFailure> {
        let transcript =
            TranscriptSink::create(&self.transcripts_root, session_id, transcript_epoch)
                .map_err(SpawnFailure::proven)?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(SpawnFailure::proven)?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(SpawnFailure::proven)?;
        let writer = pair.master.take_writer().map_err(SpawnFailure::proven)?;
        let boot_identity = system_boot_identity().map_err(SpawnFailure::proven)?;
        let anchor_root = self.transcripts_root.join(".launch-anchors");
        std::fs::create_dir_all(&anchor_root).map_err(SpawnFailure::proven)?;
        let anchor_path = anchor_root.join(format!("{session_id}-{transcript_epoch}.json"));
        match std::fs::remove_file(&anchor_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(SpawnFailure::proven(error)),
        }
        self.store
            .mark_session_spawning(session_id, transcript_epoch, &boot_identity)
            .context("persist native spawn intent")
            .map_err(SpawnFailure::proven)?;
        let spawn_baseline = process_inventory();
        let mut child = pair
            .slave
            .spawn_command(launch.command(&anchor_path))
            .with_context(|| format!("launch {} in PTY", launch.executable.display()))
            .map_err(SpawnFailure::proven)?;
        drop(pair.slave);
        let child_pid = child.process_id();
        let wrapper_group = child_pid
            .map(|pid| unsafe { libc::getpgid(pid as libc::pid_t) })
            .filter(|group| *group > 0);
        if let Some(wrapper_pid) = child_pid {
            let Some(wrapper_group) = wrapper_group else {
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    child_pid,
                    None,
                    "launch wrapper has no observable process group",
                ));
            };
            if wrapper_group == unsafe { libc::getpgrp() } {
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    child_pid,
                    None,
                    "launch wrapper shares the service process group",
                ));
            }
            let wrapper_start = match native_start_marker(wrapper_pid) {
                Ok(start) => start,
                Err(error) => {
                    return Err(self.classify_spawn_failure(
                        child.as_mut(),
                        child_pid,
                        Some(wrapper_group),
                        error,
                    ))
                }
            };
            let wrapper_anchor = ProcessGenerationAnchor {
                pid: wrapper_pid,
                process_group_id: wrapper_group,
                native_start_marker: wrapper_start,
                boot_identity: boot_identity.clone(),
            };
            if let Err(error) =
                self.store
                    .record_session_anchor(session_id, transcript_epoch, &wrapper_anchor)
            {
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    child_pid,
                    Some(wrapper_group),
                    error.context("persist launch wrapper generation anchor"),
                ));
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (leader_anchor, provider_anchor) = loop {
            match std::fs::read(&anchor_path) {
                Ok(bytes) => {
                    match serde_json::from_slice::<LaunchHandshake>(&bytes) {
                        Ok(LaunchHandshake::ProviderSpawned { leader, provider })
                            if leader.boot_identity == boot_identity
                                && provider.boot_identity == boot_identity =>
                        {
                            break (leader, provider)
                        }
                        Ok(LaunchHandshake::PreProviderSpawnFailed { reason }) => {
                            return Err(self.classify_acknowledged_nondelivery(
                                child.as_mut(),
                                child_pid,
                                wrapper_group,
                                reason,
                            ));
                        }
                        Ok(_) => return Err(self.classify_spawn_failure(
                            child.as_mut(),
                            child_pid,
                            wrapper_group,
                            "launch wrapper reported a different operating-system boot identity",
                        )),
                        Err(error) if std::time::Instant::now() < deadline => {
                            let _ = error;
                        }
                        Err(error) => {
                            return Err(self.classify_spawn_failure(
                                child.as_mut(),
                                child_pid,
                                wrapper_group,
                                format!("launch wrapper anchor is malformed: {error}"),
                            ))
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(self.classify_spawn_failure(
                        child.as_mut(),
                        child_pid,
                        wrapper_group,
                        format!("read launch wrapper anchor: {error}"),
                    ))
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let reason = format!(
                        "launch wrapper exited before provider spawn handshake: {status:?}"
                    );
                    if let Ok(bytes) = std::fs::read(&anchor_path) {
                        if let Ok(LaunchHandshake::PreProviderSpawnFailed { reason }) =
                            serde_json::from_slice::<LaunchHandshake>(&bytes)
                        {
                            return Err(self.classify_acknowledged_nondelivery(
                                child.as_mut(),
                                child_pid,
                                wrapper_group,
                                reason,
                            ));
                        }
                    }
                    return Err(if child_pid.is_some() {
                        self.classify_spawn_failure(
                            child.as_mut(),
                            child_pid,
                            wrapper_group,
                            reason,
                        )
                    } else {
                        self.classify_pidless_spawn_failure(child.as_mut(), spawn_baseline, reason)
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(self.classify_spawn_failure(
                        child.as_mut(),
                        child_pid,
                        wrapper_group,
                        format!("inspect launch wrapper before provider exec: {error}"),
                    ))
                }
            }
            if std::time::Instant::now() >= deadline {
                if child_pid.is_none() {
                    return Err(self.classify_pidless_spawn_failure(
                        child.as_mut(),
                        spawn_baseline,
                        "launch wrapper exposed neither its durable anchor nor a portable PID",
                    ));
                }
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    child_pid,
                    wrapper_group,
                    "launch wrapper did not publish its durable anchor before timeout",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let leader_pid = leader_anchor.pid;
        let pid = provider_anchor.pid;
        if leader_anchor.process_group_id <= 0
            || leader_anchor.process_group_id == unsafe { libc::getpgrp() }
            || leader_anchor.pid as i32 != leader_anchor.process_group_id
            || provider_anchor.process_group_id != leader_anchor.process_group_id
            || child_pid.is_some_and(|wrapper| wrapper != leader_anchor.pid)
        {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(leader_anchor.process_group_id),
                "launch wrapper reported an invalid leader/provider process-group relationship",
            ));
        }
        let process_group_id = unsafe { libc::getpgid(pid as libc::pid_t) };
        if process_group_id <= 0 || process_group_id != provider_anchor.process_group_id {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(leader_anchor.process_group_id),
                format!(
                    "provider child {pid} no longer matches anchored process group {}",
                    provider_anchor.process_group_id
                ),
            ));
        }
        if let Err(error) =
            self.store
                .record_session_anchor(session_id, transcript_epoch, &leader_anchor)
        {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(process_group_id),
                error.context("persist process-group leader generation anchor"),
            ));
        }
        let inventory = match process_inventory() {
            Ok(inventory) => inventory,
            Err(error) => {
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    Some(leader_pid),
                    Some(process_group_id),
                    error,
                ))
            }
        };
        let provider_parent = inventory
            .iter()
            .find(|process| process.pid == pid)
            .map(|process| process.ppid);
        if provider_parent != Some(leader_anchor.pid) {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(process_group_id),
                format!(
                    "provider child {pid} is not rooted at launch wrapper {:?}",
                    leader_anchor.pid
                ),
            ));
        }
        let leader_start = match native_start_marker(leader_anchor.pid) {
            Ok(marker) => marker,
            Err(error) => {
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    Some(leader_pid),
                    Some(process_group_id),
                    error,
                ));
            }
        };
        if leader_start != leader_anchor.native_start_marker {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(process_group_id),
                "process-group leader no longer matches the launch handshake generation",
            ));
        }
        let native_start_marker = match native_start_marker(pid) {
            Ok(marker) => marker,
            Err(error) => {
                return Err(self.classify_spawn_failure(
                    child.as_mut(),
                    Some(leader_pid),
                    Some(process_group_id),
                    error,
                ));
            }
        };
        if native_start_marker != provider_anchor.native_start_marker {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(process_group_id),
                "provider PID no longer matches the launch wrapper generation anchor",
            ));
        }
        let process = ProcessIdentity {
            pid,
            process_group_id,
            native_start_marker: native_start_marker.clone(),
            observed_started_at: Utc::now().to_rfc3339(),
        };
        let process_json = serde_json::to_string(&process).map_err(SpawnFailure::proven)?;
        // cmux may expose Codex through a script shim.  By this point the
        // handshake has bound the final provider PID, so use its OS-backed
        // executable path as the fallback source for the exact helper image.
        let codex_helper_image = (launch.config.provider == Provider::Codex)
            .then(|| Mutex::new(codex_helper_image(launch, pid)));
        if let Err(error) = self.store.record_session_process(
            session_id,
            transcript_epoch,
            &process_json,
            pid,
            &native_start_marker,
            process_group_id,
            leader_anchor.pid,
        ) {
            return Err(self.classify_spawn_failure(
                child.as_mut(),
                Some(leader_pid),
                Some(process_group_id),
                error.context("persist provider process identity"),
            ));
        }
        let _ = std::fs::remove_file(&anchor_path);
        let mut known_members = HashMap::new();
        known_members.insert(leader_pid, leader_anchor.native_start_marker.clone());
        known_members.insert(pid, native_start_marker);
        let handle = Arc::new(SessionHandle {
            session_id: session_id.to_owned(),
            role_generation_id: role_generation_id.to_owned(),
            transcript_epoch: transcript_epoch.to_owned(),
            process: process.clone(),
            group_leader: leader_anchor.clone(),
            child: Mutex::new(child),
            input: Mutex::new(writer),
            io_boundary: Mutex::new(()),
            known_members: Mutex::new(known_members),
            first_stop_requested_at: Mutex::new(None),
            codex_helper_image,
            codex_helper_identity: Mutex::new(None),
            _master: Mutex::new(pair.master),
        });
        match self.sessions.write() {
            Ok(mut sessions) => {
                sessions.insert(session_id.to_owned(), handle.clone());
            }
            Err(_) => {
                let failure = handle.child.lock().map_or_else(
                    |_| SpawnFailure::DeliveryUnknown {
                        reason: "supervisor session and child maps are poisoned".to_owned(),
                        root_pid: Some(leader_pid),
                        process_group_id: Some(process_group_id),
                        members: vec![
                            ObservedProcessIdentity {
                                pid: leader_pid,
                                parent_pid: std::process::id(),
                                process_group_id,
                                native_start_marker: leader_anchor.native_start_marker.clone(),
                            },
                            ObservedProcessIdentity {
                                pid,
                                parent_pid: leader_pid,
                                process_group_id,
                                native_start_marker: process.native_start_marker.clone(),
                            },
                        ],
                    },
                    |mut child| {
                        self.classify_spawn_failure(
                            &mut **child,
                            Some(leader_pid),
                            Some(process_group_id),
                            "supervisor session map poisoned",
                        )
                    },
                );
                return Err(failure);
            }
        }
        let store = self.store.clone();
        let tracked_session = session_id.to_owned();
        let tracked_epoch = transcript_epoch.to_owned();
        let tracked_process = process_json.clone();
        let transcript_root = self.transcripts_root.clone();
        if let Err(error) = std::thread::Builder::new().name(format!("pty-{session_id}")).spawn(move || {
            let mut buffer = vec![0_u8; 16 * 1024];
            let mut sink_failed = false;
            let mut capture_complete = false;
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        capture_complete = true;
                        break;
                    }
                    Ok(count) if !sink_failed => match transcript.append(&buffer[..count]) {
                        Ok(Some(frame)) => {
                            if let Err(error) = store.set_transcript_sequence(&tracked_session, &tracked_epoch, frame.sequence) {
                                tracing::warn!(session_id = %tracked_session, error = %error, "transcript cursor persistence failed");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            sink_failed = true;
                            if let Err(store_error) = store.record_capture_failure(&tracked_session, &tracked_epoch, &format!("transcript sink failure: {error:#}")) {
                                tracing::error!(session_id = %tracked_session, error = %store_error, "capture failure persistence also failed");
                            }
                        }
                    },
                    Ok(_) => {}
                    Err(error) => {
                        if let Err(store_error) = store.record_capture_failure(&tracked_session, &tracked_epoch, &format!("PTY read failure: {error}")) {
                            tracing::error!(session_id = %tracked_session, error = %store_error, "capture failure persistence also failed");
                        }
                        break;
                    }
                }
            }
            if capture_complete && !sink_failed {
                let output_tail = crate::transcript::recent_output_summary(
                    &transcript_root,
                    &tracked_session,
                    &tracked_epoch,
                    2048,
                )
                .ok()
                .flatten();
                match store.mark_capture_complete(
                    &tracked_session,
                    &tracked_epoch,
                    &tracked_process,
                    output_tail.as_deref(),
                ) {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!(session_id = %tracked_session, "PTY capture completion no longer matches the active session binding"),
                    Err(error) => tracing::error!(session_id = %tracked_session, error = %error, "capture completion persistence failed"),
                }
            }
        }) {
            if let Ok(mut sessions) = self.sessions.write() {
                sessions.remove(session_id);
            }
            if let Ok(mut child) = handle.child.lock() {
                return Err(self.classify_spawn_failure(
                    &mut **child,
                    Some(leader_pid),
                    Some(process_group_id),
                    error,
                ));
            }
            return Err(SpawnFailure::DeliveryUnknown {
                reason: format!("PTY reader thread failed and child lock is poisoned: {error}"),
                root_pid: Some(leader_pid),
                process_group_id: Some(process_group_id),
                members: vec![
                    ObservedProcessIdentity {
                        pid: leader_pid,
                        parent_pid: std::process::id(),
                        process_group_id,
                        native_start_marker: leader_anchor.native_start_marker.clone(),
                    },
                    ObservedProcessIdentity {
                        pid,
                        parent_pid: leader_pid,
                        process_group_id,
                        native_start_marker: process.native_start_marker.clone(),
                    },
                ],
            });
        }
        Ok(process)
    }

    fn classify_spawn_failure(
        &self,
        child: &mut dyn portable_pty::Child,
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
        error: impl std::fmt::Display,
    ) -> SpawnFailure {
        let reason = error.to_string();
        let owned_process_state = self.cleanup_owned_processes(child, root_pid, process_group_id);
        let (root_pid, process_group_id, members) = owned_process_state.evidence();
        SpawnFailure::DeliveryUnknown {
            reason,
            root_pid,
            process_group_id,
            members: members.to_vec(),
        }
    }

    fn cleanup_owned_processes(
        &self,
        child: &mut dyn portable_pty::Child,
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
    ) -> OwnedProcessState {
        let safe_group =
            process_group_id.filter(|group| *group > 0 && *group != unsafe { libc::getpgrp() });
        let before = process_inventory();
        if let Ok(inventory) = &before {
            for process in discover_processes(root_pid, safe_group, inventory, &[]) {
                if native_start_marker(process.pid).ok().as_deref() == Some(process.start.as_str())
                    && process.pgid != unsafe { libc::getpgrp() }
                {
                    let _ = unsafe { libc::kill(process.pid as libc::pid_t, libc::SIGKILL) };
                }
            }
        }
        if let Some(group) = safe_group {
            let _ = unsafe { libc::killpg(group, libc::SIGKILL) };
        }
        let _ = child.kill();
        let _ = child.wait();
        if root_pid.is_none() && safe_group.is_none() {
            return OwnedProcessState::Uncertain {
                root_pid,
                process_group_id: safe_group,
                members: Vec::new(),
            };
        }
        let known = before
            .as_ref()
            .map(|inventory| discover_processes(root_pid, safe_group, inventory, &[]))
            .unwrap_or_default();
        let after = process_inventory();
        let surviving = after
            .as_ref()
            .map(|inventory| discover_processes(root_pid, safe_group, inventory, &known))
            .unwrap_or_default();
        let mut members = known;
        for process in &surviving {
            if !members
                .iter()
                .any(|member| member.pid == process.pid && member.start == process.start)
            {
                members.push(process.clone());
            }
        }
        let evidence = members
            .into_iter()
            .map(ObservedProcessIdentity::from)
            .collect();
        if before.is_ok() && after.is_ok() && surviving.is_empty() {
            OwnedProcessState::Quiescent {
                root_pid,
                process_group_id: safe_group,
                members: evidence,
            }
        } else {
            OwnedProcessState::Uncertain {
                root_pid,
                process_group_id: safe_group,
                members: evidence,
            }
        }
    }

    fn classify_acknowledged_nondelivery(
        &self,
        child: &mut dyn portable_pty::Child,
        root_pid: Option<u32>,
        process_group_id: Option<i32>,
        reason: impl std::fmt::Display,
    ) -> SpawnFailure {
        let reason = reason.to_string();
        let owned_process_state = self.cleanup_owned_processes(child, root_pid, process_group_id);
        SpawnFailure::ProvenNondelivery {
            reason,
            owned_process_state,
        }
    }

    fn classify_pidless_spawn_failure(
        &self,
        child: &mut dyn portable_pty::Child,
        baseline: Result<Vec<ProcessInfo>>,
        error: impl std::fmt::Display,
    ) -> SpawnFailure {
        let reason = error.to_string();
        let before = process_inventory();
        let mut observed = match (&baseline, &before) {
            (Ok(baseline), Ok(inventory)) => newly_spawned_service_descendants(baseline, inventory),
            _ => Vec::new(),
        };
        for process in &observed {
            if native_start_marker(process.pid).ok().as_deref() == Some(process.start.as_str()) {
                let _ = unsafe { libc::kill(process.pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        if let Ok(inventory) = process_inventory() {
            for process in discover_processes(None, None, &inventory, &observed) {
                if !observed
                    .iter()
                    .any(|member| member.pid == process.pid && member.start == process.start)
                {
                    observed.push(process);
                }
            }
        }
        let root_pid = observed
            .iter()
            .find(|process| process.ppid == std::process::id())
            .map(|process| process.pid);
        let groups = observed
            .iter()
            .map(|process| process.pgid)
            .collect::<HashSet<_>>();
        SpawnFailure::DeliveryUnknown {
            reason,
            root_pid,
            process_group_id: (groups.len() == 1).then(|| *groups.iter().next().unwrap()),
            members: observed
                .into_iter()
                .map(ObservedProcessIdentity::from)
                .collect(),
        }
    }

    pub fn reconcile(&self) -> Result<()> {
        self.store.reconcile_exited_runtime_probes()?;
        for handle in self.handles()? {
            let child_exit = handle
                .child
                .lock()
                .map_err(|_| anyhow!("provider child lock poisoned"))?
                .try_wait()?;
            let inventory = match self.process_inventory() {
                Ok(inventory) => inventory,
                Err(error) => {
                    let Some(status) = child_exit.as_ref() else {
                        return Err(error);
                    };
                    if service_stop_is_settling(
                        *handle
                            .first_stop_requested_at
                            .lock()
                            .map_err(|_| anyhow!("session stop-state lock poisoned"))?,
                        std::time::Instant::now(),
                    ) {
                        continue;
                    }
                    if self.store.session_json(&handle.session_id)?["status"].as_str()
                        != Some("recovery_required")
                    {
                        self.store.mark_session_delivery_ambiguous(
                            &handle.session_id,
                            &format!(
                                "provider root exited with status {status:?} while exact descendant inventory was unavailable: {error:#}"
                            ),
                        )?;
                    }
                    continue;
                }
            };
            self.observe_members(&handle, &inventory)?;
            if let Some(status) = child_exit {
                let remaining = self.remaining_members(&handle, &inventory)?;
                if remaining.is_empty() {
                    let _boundary = handle
                        .io_boundary
                        .lock()
                        .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
                    let process_json = serde_json::to_string(&handle.process)?;
                    let output_tail = (!status.success())
                        .then(|| {
                            transcript::recent_output_summary(
                                &self.transcripts_root,
                                &handle.session_id,
                                &handle.transcript_epoch,
                                2048,
                            )
                            .ok()
                            .flatten()
                        })
                        .flatten();
                    self.store.update_session_exit(
                        &handle.session_id,
                        &handle.transcript_epoch,
                        &process_json,
                        &serde_json::json!({
                            "status": format!("{status:?}"), "observed_at": Utc::now().to_rfc3339(),
                            "success": status.success(), "code": status.exit_code(),
                            "output_tail": output_tail,
                            "structured_result_required": true, "process_group_quiescent": true
                        })
                        .to_string(),
                    )?;
                } else if service_stop_is_settling(
                    *handle
                        .first_stop_requested_at
                        .lock()
                        .map_err(|_| anyhow!("session stop-state lock poisoned"))?,
                    std::time::Instant::now(),
                ) {
                    continue;
                } else if self.store.session_json(&handle.session_id)?["status"].as_str()
                    != Some("recovery_required")
                {
                    self.store.mark_session_delivery_ambiguous(
                        &handle.session_id,
                        &format!(
                            "provider root exited with status {status:?} while {} exact recorded descendant generation(s) remained live",
                            remaining.len()
                        ),
                    )?;
                }
            }
        }
        self.reconcile_interrupt_deadlines()?;
        Ok(())
    }

    fn reconcile_interrupt_deadlines(&self) -> Result<()> {
        for receipt in self.store.interrupt_deadline_candidates()? {
            let attached = self
                .sessions
                .read()
                .map_err(|_| anyhow!("supervisor session map poisoned"))?
                .get(&receipt.session_id)
                .cloned();
            if let Some(handle) = attached {
                if self.verify_process(&handle).is_ok() {
                    self.store.mark_interrupt_deadline_recovery(
                        &receipt,
                        &serde_json::json!({
                            "source":"attached_child_exact_process_live",
                            "pid":handle.process.pid,
                            "process_group_id":handle.process.process_group_id,
                            "native_start_marker":handle.process.native_start_marker,
                        }),
                        true,
                    )?;
                    continue;
                }
            }
            match crate::recovery::observe_session_processes(&self.store, &receipt.session_id)? {
                crate::recovery::SessionProcessObservation::Quiescent(observation) => {
                    self.store.update_session_exit(
                        &receipt.session_id,
                        &receipt.transcript_epoch,
                        &receipt.process_identity_json,
                        &serde_json::json!({
                            "success":true,
                            "process_group_quiescent":true,
                            "source":"graceful_stop_deadline_reconciliation",
                            "verification":observation,
                            "observed_at":Utc::now().to_rfc3339(),
                        })
                        .to_string(),
                    )?;
                }
                crate::recovery::SessionProcessObservation::Live(observation) => {
                    self.store
                        .mark_interrupt_deadline_recovery(&receipt, &observation, true)?;
                }
                crate::recovery::SessionProcessObservation::Uncertain(reason) => {
                    self.store.mark_interrupt_deadline_recovery(
                        &receipt,
                        &serde_json::json!({
                            "source":"operating_system_identity_verification_uncertain",
                            "reason":reason,
                        }),
                        false,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn reconcile_setup_retained_first_turn_stop_timeout(
        &self,
        receipt: &crate::store::SetupRetainedFirstTurnStopTimeoutReceipt,
    ) -> Result<SetupRetainedFirstTurnStopTimeoutOutcome> {
        let attached = self
            .sessions
            .read()
            .map_err(|_| anyhow!("supervisor session map poisoned"))?
            .get(&receipt.session_id)
            .cloned();
        if let Some(handle) =
            attached.filter(|handle| setup_stop_timeout_receipt_matches_handle(receipt, handle))
        {
            let child_exit = handle
                .child
                .lock()
                .map_err(|_| anyhow!("provider child lock poisoned"))?
                .try_wait()?;
            let inventory = match self.process_inventory() {
                Ok(inventory) => inventory,
                Err(error) => {
                    let observation = serde_json::json!({
                        "source":"attached_child_with_unavailable_process_inventory",
                        "child_exit_observed":child_exit.is_some(),
                        "error":format!("{error:#}")
                    });
                    return Ok(
                        if self.store.mark_setup_retained_first_turn_stop_timed_out(
                            receipt,
                            &observation,
                            false,
                        )? {
                            SetupRetainedFirstTurnStopTimeoutOutcome::TimedOut
                        } else {
                            SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                        },
                    );
                }
            };
            self.observe_members(&handle, &inventory)?;
            if let Some(status) = child_exit {
                let remaining = self.remaining_members(&handle, &inventory)?;
                if remaining.is_empty() {
                    let observation = serde_json::json!({
                        "source":"attached_child_exit",
                        "status":format!("{status:?}"),
                        "success":status.success(),
                        "code":status.exit_code(),
                        "remaining_exact_members":0
                    });
                    return Ok(
                        if self
                            .store
                            .mark_setup_retained_first_turn_stop_quiescent(receipt, &observation)?
                        {
                            SetupRetainedFirstTurnStopTimeoutOutcome::Quiescent
                        } else {
                            SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                        },
                    );
                }
                let observation = serde_json::json!({
                    "source":"attached_child_exit_with_live_exact_descendants",
                    "status":format!("{status:?}"),
                    "live_processes":remaining.iter().map(|process| serde_json::json!({
                        "pid":process.pid,
                        "process_group_id":process.pgid,
                        "native_start_marker":process.start
                    })).collect::<Vec<_>>()
                });
                return Ok(
                    if self.store.mark_setup_retained_first_turn_stop_timed_out(
                        receipt,
                        &observation,
                        true,
                    )? {
                        SetupRetainedFirstTurnStopTimeoutOutcome::TimedOut
                    } else {
                        SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                    },
                );
            }
            if self.verify_process(&handle).is_ok() {
                let observation = serde_json::json!({
                    "source":"attached_child_exact_process_live",
                    "pid":handle.process.pid,
                    "process_group_id":handle.process.process_group_id,
                    "native_start_marker":handle.process.native_start_marker
                });
                return Ok(
                    if self.store.mark_setup_retained_first_turn_stop_timed_out(
                        receipt,
                        &observation,
                        true,
                    )? {
                        SetupRetainedFirstTurnStopTimeoutOutcome::TimedOut
                    } else {
                        SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                    },
                );
            }
            // The process may have exited between try_wait and identity
            // verification. Fall through to a fresh durable OS observation.
        }
        match crate::recovery::observe_session_processes(&self.store, &receipt.session_id)? {
            crate::recovery::SessionProcessObservation::Quiescent(observation) => Ok(
                if self
                    .store
                    .mark_setup_retained_first_turn_stop_quiescent(receipt, &observation)?
                {
                    SetupRetainedFirstTurnStopTimeoutOutcome::Quiescent
                } else {
                    SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                },
            ),
            crate::recovery::SessionProcessObservation::Live(observation) => Ok(
                if self.store.mark_setup_retained_first_turn_stop_timed_out(
                    receipt,
                    &observation,
                    true,
                )? {
                    SetupRetainedFirstTurnStopTimeoutOutcome::TimedOut
                } else {
                    SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                },
            ),
            crate::recovery::SessionProcessObservation::Uncertain(reason) => {
                let observation = serde_json::json!({
                    "source":"operating_system_identity_verification_uncertain",
                    "reason":reason
                });
                Ok(
                    if self.store.mark_setup_retained_first_turn_stop_timed_out(
                        receipt,
                        &observation,
                        false,
                    )? {
                        SetupRetainedFirstTurnStopTimeoutOutcome::TimedOut
                    } else {
                        SetupRetainedFirstTurnStopTimeoutOutcome::Stale
                    },
                )
            }
        }
    }

    pub fn acquire_input(
        &self,
        session_id: &str,
        owner_id: &str,
        seconds: i64,
    ) -> Result<(String, String)> {
        if !(1..=300).contains(&seconds) {
            bail!("input lease duration must be between 1 and 300 seconds")
        }
        let handle = self.handle(session_id)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        let binding = self.live_attachment_binding_locked(&handle)?;
        let secret = crate::auth::issue_secret();
        let process_json = serde_json::to_string(&binding.process)?;
        let expires_at = (Utc::now() + Duration::seconds(seconds)).to_rfc3339();
        self.store.acquire_input_lease(
            session_id,
            &secret,
            owner_id,
            &process_json,
            &binding.role_generation_id,
            &expires_at,
        )?;
        Ok((secret, expires_at))
    }

    pub fn establish_attachment(&self, session_id: &str) -> Result<AttachmentBinding> {
        #[cfg(test)]
        if let Some(binding) = self
            .synthetic_attachments
            .read()
            .map_err(|_| anyhow!("synthetic attachment test seam lock poisoned"))?
            .get(session_id)
            .cloned()
        {
            self.store.verify_attachment_binding(&binding)?;
            return Ok(binding);
        }
        let handle = self.handle(session_id)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        self.live_attachment_binding_locked(&handle)
    }

    pub fn renew_input(
        &self,
        binding: &AttachmentBinding,
        lease_secret: &str,
        seconds: i64,
    ) -> Result<String> {
        if !(1..=300).contains(&seconds) {
            bail!("input lease duration must be between 1 and 300 seconds")
        }
        let handle = self.attachment_handle(binding)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        self.verify_attachment_binding_locked(&handle, binding)?;
        let expires_at = (Utc::now() + Duration::seconds(seconds)).to_rfc3339();
        self.store.renew_input_lease(
            &binding.session_id,
            lease_secret,
            &serde_json::to_string(&binding.process)?,
            &binding.role_generation_id,
            &expires_at,
        )?;
        Ok(expires_at)
    }

    pub fn takeover_input(
        &self,
        binding: &AttachmentBinding,
        owner_id: &str,
        seconds: i64,
    ) -> Result<(String, String)> {
        if !(1..=300).contains(&seconds) {
            bail!("input lease duration must be between 1 and 300 seconds")
        }
        let handle = self.attachment_handle(binding)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        self.verify_attachment_binding_locked(&handle, binding)?;
        let secret = crate::auth::issue_secret();
        let expires_at = (Utc::now() + Duration::seconds(seconds)).to_rfc3339();
        self.store.takeover_input_lease(
            &binding.session_id,
            &secret,
            owner_id,
            &serde_json::to_string(&binding.process)?,
            &binding.role_generation_id,
            &expires_at,
            &uuid::Uuid::new_v4().to_string(),
        )?;
        Ok((secret, expires_at))
    }

    pub fn attachment_transcript(
        &self,
        binding: &AttachmentBinding,
        after_epoch: Option<&str>,
        after_sequence: u64,
        limit_bytes: usize,
    ) -> Result<crate::domain::TranscriptPage> {
        let handle = self.attachment_handle(binding)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        let mut access = self.store.attachment_transcript_access(binding)?;
        if matches!(access, crate::store::AttachmentTranscriptAccess::Live) {
            if let Err(live_error) = self.verify_process(&handle) {
                for attempt in 0..5 {
                    self.reconcile_attachment_exit_locked(&handle)?;
                    access = self.store.attachment_transcript_access(binding)?;
                    if !matches!(access, crate::store::AttachmentTranscriptAccess::Live) {
                        break;
                    }
                    if attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
                if matches!(access, crate::store::AttachmentTranscriptAccess::Live) {
                    return Err(live_error);
                }
            }
        }
        match access {
            crate::store::AttachmentTranscriptAccess::Live => {}
            // The process is already dead in this state. Keep the immutable
            // binding check, but permit captured bytes to drain only.
            crate::store::AttachmentTranscriptAccess::CaptureDraining => {}
            crate::store::AttachmentTranscriptAccess::CaptureComplete { sequence }
                if after_sequence == sequence
                    && (after_epoch == Some(binding.transcript_epoch.as_str())
                        || (after_epoch.is_none() && sequence == 0)) =>
            {
                return Ok(crate::domain::TranscriptPage {
                    frames: Vec::new(),
                    next_epoch: Some(binding.transcript_epoch.clone()),
                    next_sequence: sequence,
                    has_more: false,
                });
            }
            crate::store::AttachmentTranscriptAccess::CaptureComplete { .. } => {}
        }
        transcript::read_attachment_frames(
            &self.transcripts_root,
            &binding.session_id,
            &binding.transcript_epoch,
            after_epoch,
            after_sequence,
            limit_bytes,
        )
    }

    pub fn write_input(&self, session_id: &str, lease_secret: &str, bytes: &[u8]) -> Result<()> {
        let binding = self.establish_attachment(session_id)?;
        self.write_attachment_input(&binding, lease_secret, bytes)
    }

    pub fn write_attachment_input(
        &self,
        binding: &AttachmentBinding,
        lease_secret: &str,
        bytes: &[u8],
    ) -> Result<()> {
        if bytes.len() > 64 * 1024 {
            bail!("one input write is limited to 64 KiB")
        }
        let handle = self.attachment_handle(binding)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        self.verify_attachment_binding_locked(&handle, binding)?;
        let process_json = serde_json::to_string(&binding.process)?;
        self.store.verify_input_lease(
            &binding.session_id,
            lease_secret,
            &process_json,
            &binding.role_generation_id,
        )?;
        let mut input = handle
            .input
            .lock()
            .map_err(|_| anyhow!("PTY input lock poisoned"))?;
        input.write_all(bytes)?;
        input.flush()?;
        Ok(())
    }

    pub fn release_input(&self, session_id: &str, lease_secret: &str) -> Result<()> {
        let binding = self.establish_attachment(session_id)?;
        self.release_attachment_input(&binding, lease_secret)
    }

    pub fn release_attachment_input(
        &self,
        binding: &AttachmentBinding,
        lease_secret: &str,
    ) -> Result<()> {
        let handle = self.attachment_handle(binding)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        self.verify_attachment_binding_locked(&handle, binding)?;
        self.store
            .release_input_lease(&binding.session_id, lease_secret)
    }

    pub fn resize(&self, session_id: &str, lease_secret: &str, rows: u16, cols: u16) -> Result<()> {
        let binding = self.establish_attachment(session_id)?;
        self.resize_attachment(&binding, lease_secret, rows, cols)
    }

    pub fn resize_attachment(
        &self,
        binding: &AttachmentBinding,
        lease_secret: &str,
        rows: u16,
        cols: u16,
    ) -> Result<()> {
        if !(10..=300).contains(&rows) || !(20..=500).contains(&cols) {
            bail!("terminal size must be 10..300 rows and 20..500 columns")
        }
        let handle = self.attachment_handle(binding)?;
        let _boundary = handle
            .io_boundary
            .lock()
            .map_err(|_| anyhow!("session I/O boundary poisoned"))?;
        self.verify_attachment_binding_locked(&handle, binding)?;
        self.store.verify_input_lease(
            &binding.session_id,
            lease_secret,
            &serde_json::to_string(&binding.process)?,
            &binding.role_generation_id,
        )?;
        handle
            ._master
            .lock()
            .map_err(|_| anyhow!("PTY master lock poisoned"))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        Ok(())
    }

    pub fn native_idle_ready(&self, session_id: &str) -> Result<bool> {
        let handle = self.handle(session_id)?;
        self.verify_process(&handle)?;
        let inventory = process_inventory()?;
        self.observe_members(&handle, &inventory)?;
        let observed_provider = process_executable_path(handle.process.pid).ok();
        let helper_image = handle
            .codex_helper_image
            .as_ref()
            .map(|image| latched_codex_helper_image(image, observed_provider.as_deref()))
            .transpose()?
            .flatten();
        native_members_idle(
            handle.process.pid,
            handle.group_leader.pid,
            helper_image.as_ref(),
            &handle.codex_helper_identity,
            &self.remaining_members(&handle, &inventory)?,
            |pid| {
                Ok((
                    native_start_marker(pid)?,
                    executable_fingerprint(&process_executable_path(pid)?)?,
                ))
            },
        )
    }

    pub fn interrupt(&self, session_id: &str) -> Result<()> {
        self.interrupt_once(session_id).map(|_| ())
    }

    pub fn interrupt_once(&self, session_id: &str) -> Result<InterruptOutcome> {
        let handle = self.handle(session_id)?;
        let mut first_stop = handle
            .first_stop_requested_at
            .lock()
            .map_err(|_| anyhow!("session stop-state lock poisoned"))?;
        if first_stop.is_some() {
            return Ok(InterruptOutcome::AlreadyRequested);
        }
        self.verify_process(&handle)?;
        self.store.mark_interrupt_requested(session_id)?;
        if unsafe { libc::kill(-handle.process.process_group_id, libc::SIGINT) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("signal verified provider process group");
        }
        // A failed kernel delivery is not an interrupt receipt. Leave the
        // marker clear so the setup-manager retry path can re-verify this
        // exact handle before it makes one explicit new signal attempt.
        *first_stop = Some(std::time::Instant::now());
        Ok(InterruptOutcome::Requested)
    }

    pub(crate) fn retry_graceful_stop_exact(&self, session_id: &str) -> Result<()> {
        self.store.mark_interrupt_requested(session_id)?;
        if let Ok(handle) = self.handle(session_id) {
            self.verify_process(&handle)?;
            if unsafe { libc::kill(-handle.process.process_group_id, libc::SIGINT) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            self.store.mark_session_delivery_ambiguous(
                session_id,
                &format!("human-authorized graceful-stop retry delivery failed: {error}"),
            )?;
            return Err(error).context("retry signal verified provider process group");
        }
        self.signal_recorded_exact_process(session_id, libc::SIGINT)
    }

    pub(crate) fn force_stop_exact_managed_process(&self, session_id: &str) -> Result<()> {
        if let Ok(handle) = self.handle(session_id) {
            self.verify_process(&handle)?;
            if unsafe { libc::kill(-handle.process.process_group_id, libc::SIGKILL) } == 0 {
                return Ok(());
            }
            return Err(std::io::Error::last_os_error())
                .context("force stop verified provider process group");
        }
        self.signal_recorded_exact_process(session_id, libc::SIGKILL)
    }

    fn signal_recorded_exact_process(&self, session_id: &str, signal: libc::c_int) -> Result<()> {
        let record = self.store.session_json(session_id)?;
        let process: ProcessIdentity = serde_json::from_value(
            record
                .get("process_identity")
                .cloned()
                .ok_or_else(|| anyhow!("session has no durable managed process identity"))?,
        )
        .context("parse durable managed process identity")?;
        let observed = native_start_marker(process.pid)?;
        if observed != process.native_start_marker {
            bail!(
                "PID {} no longer matches the durable managed process start identity",
                process.pid
            )
        }
        let group = unsafe { libc::getpgid(process.pid as libc::pid_t) };
        if group != process.process_group_id {
            bail!(
                "PID {} no longer belongs to the durable managed process group",
                process.pid
            )
        }
        if unsafe { libc::kill(-process.process_group_id, signal) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("signal durable exact managed process group");
        }
        Ok(())
    }

    pub(crate) fn retry_setup_manager_interrupt_once(
        &self,
        session_id: &str,
    ) -> Result<InterruptOutcome> {
        let handle = self.handle(session_id)?;
        let mut first_stop = handle
            .first_stop_requested_at
            .lock()
            .map_err(|_| anyhow!("session stop-state lock poisoned"))?;
        if first_stop.is_some() {
            return Ok(InterruptOutcome::AlreadyRequested);
        }
        // This verifies the immutable session PID, process group, and native
        // start marker after the durable failed-delivery hold matched the
        // same stored identity. It is intentionally not a generic retry API.
        self.verify_process(&handle)?;
        // Record that this particular human-authorized retry is being
        // delivered before signalling.  finish_setup_manager_stop restores a
        // truthful recovery_required state if the kernel delivery fails.
        self.store.mark_interrupt_requested(session_id)?;
        if unsafe { libc::kill(-handle.process.process_group_id, libc::SIGINT) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("retry signal verified setup manager process group");
        }
        *first_stop = Some(std::time::Instant::now());
        Ok(InterruptOutcome::Requested)
    }

    pub(crate) fn request_manager_service_stop(
        &self,
        attempt_id: &str,
        phase: &str,
        receipt: &crate::store::ManagerServiceStopReceipt,
    ) -> Result<InterruptOutcome> {
        let handle = self.handle(&receipt.session_id)?;
        if handle.role_generation_id != receipt.generation_id
            || handle.transcript_epoch != receipt.transcript_epoch
        {
            return Ok(InterruptOutcome::Stale);
        }
        let mut first_stop = handle
            .first_stop_requested_at
            .lock()
            .map_err(|_| anyhow!("session stop-state lock poisoned"))?;
        if first_stop.is_some() {
            return Ok(InterruptOutcome::AlreadyRequested);
        }
        self.verify_process(&handle)?;
        match self
            .store
            .claim_manager_service_stop(attempt_id, phase, receipt)?
        {
            crate::store::ManagerServiceStopClaim::Stale => Ok(InterruptOutcome::Stale),
            crate::store::ManagerServiceStopClaim::AlreadyRequested => {
                Ok(InterruptOutcome::AlreadyRequested)
            }
            crate::store::ManagerServiceStopClaim::Claimed => {
                *first_stop = Some(std::time::Instant::now());
                if unsafe { libc::kill(-handle.process.process_group_id, libc::SIGINT) } != 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("signal verified provider process group");
                }
                Ok(InterruptOutcome::Requested)
            }
        }
    }

    pub(crate) fn request_setup_retained_first_turn_stop(
        &self,
        receipt: &crate::store::SetupRetainedFirstTurnStopReceipt,
    ) -> Result<InterruptOutcome> {
        let handle = self.handle(&receipt.session_id)?;
        if handle.role_generation_id != receipt.generation_id
            || handle.transcript_epoch != receipt.transcript_epoch
        {
            return Ok(InterruptOutcome::Stale);
        }
        let mut first_stop = handle
            .first_stop_requested_at
            .lock()
            .map_err(|_| anyhow!("session stop-state lock poisoned"))?;
        if first_stop.is_some() {
            return Ok(InterruptOutcome::AlreadyRequested);
        }
        self.verify_process(&handle)?;
        match self.store.claim_setup_retained_first_turn_stop(receipt)? {
            crate::store::ManagerServiceStopClaim::Stale => Ok(InterruptOutcome::Stale),
            crate::store::ManagerServiceStopClaim::AlreadyRequested => {
                Ok(InterruptOutcome::AlreadyRequested)
            }
            crate::store::ManagerServiceStopClaim::Claimed => {
                *first_stop = Some(std::time::Instant::now());
                deliver_setup_retained_first_turn_signal(
                    &self.store,
                    &receipt.session_id,
                    handle.process.process_group_id,
                    |process_group_id| {
                        if unsafe { libc::kill(-process_group_id, libc::SIGINT) } == 0 {
                            Ok(())
                        } else {
                            Err(std::io::Error::last_os_error())
                        }
                    },
                )?;
                Ok(InterruptOutcome::Requested)
            }
        }
    }

    pub fn active_session_ids(&self) -> Result<Vec<String>> {
        self.reconcile()?;
        let inventory = process_inventory()?;
        let mut active = Vec::new();
        for handle in self.handles()? {
            self.observe_members(&handle, &inventory)?;
            if !self.remaining_members(&handle, &inventory)?.is_empty() {
                active.push(handle.session_id.clone());
            }
        }
        Ok(active)
    }

    pub fn peer_is_managed_or_descendant(&self, peer: &PeerProcessIdentity) -> Result<bool> {
        let inventory = process_inventory()?;
        let mut managed_members = HashMap::new();
        for handle in self.handles()? {
            managed_members
                .entry(handle.process.pid)
                .or_insert_with(|| handle.process.native_start_marker.clone());
            let known_members = handle
                .known_members
                .lock()
                .map_err(|_| anyhow!("known process member lock poisoned"))?;
            for (pid, start) in known_members.iter() {
                managed_members.entry(*pid).or_insert_with(|| start.clone());
            }
        }
        peer_matches_managed_identity_or_descendant(peer, &inventory, &managed_members)
    }

    pub async fn await_role_attachment(
        &self,
        session_id: &str,
        role_generation_id: &str,
    ) -> Result<()> {
        const ATTACHMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
        const ATTACHMENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

        match self.attachment_generation_matches(session_id, role_generation_id)? {
            Some(true) => return Ok(()),
            Some(false) => {
                bail!("session {session_id} is attached to a different role generation")
            }
            None => {}
        }
        let boot_identity = tokio::task::spawn_blocking(system_boot_identity)
            .await
            .context("join operating-system boot identity lookup")??;
        if !self.store.session_is_spawning_for_attachment(
            session_id,
            role_generation_id,
            &boot_identity,
        )? {
            return Ok(());
        }

        let deadline = std::time::Instant::now() + ATTACHMENT_TIMEOUT;
        loop {
            match self.attachment_generation_matches(session_id, role_generation_id)? {
                Some(true) => return Ok(()),
                Some(false) => {
                    bail!("session {session_id} attached to a different role generation while awaiting startup")
                }
                None => {}
            }
            if !self.store.session_is_spawning_for_attachment(
                session_id,
                role_generation_id,
                &boot_identity,
            )? {
                match self.attachment_generation_matches(session_id, role_generation_id)? {
                    Some(true) => return Ok(()),
                    Some(false) => {
                        bail!("session {session_id} attached to a different role generation while awaiting startup")
                    }
                    None => {
                        bail!("session {session_id} stopped awaiting same-generation supervisor attachment")
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "session {session_id} timed out awaiting same-generation supervisor attachment"
                )
            }
            tokio::time::sleep(ATTACHMENT_POLL_INTERVAL).await;
        }
    }

    pub fn validate_role_peer(
        &self,
        session_id: &str,
        role_generation_id: &str,
        peer_pid: u32,
    ) -> Result<RolePeerProvenance> {
        let handle = self.handle(session_id)?;
        if handle.role_generation_id != role_generation_id {
            bail!("session {session_id} is attached to a different role generation")
        }
        let inventory = process_inventory()?;
        self.observe_members(&handle, &inventory)?;
        let by_pid = inventory
            .iter()
            .map(|process| (process.pid, process))
            .collect::<HashMap<_, _>>();
        let peer = by_pid
            .get(&peer_pid)
            .copied()
            .ok_or_else(|| anyhow!("role peer PID {peer_pid} is absent from process inventory"))?;
        let mut current = peer_pid;
        let mut visited = HashSet::new();
        let mut rooted = false;
        while current > 1 && visited.insert(current) {
            let process = by_pid
                .get(&current)
                .ok_or_else(|| anyhow!("role peer ancestry became unknown at PID {current}"))?;
            if process.pid == handle.process.pid
                && process.start == handle.process.native_start_marker
            {
                rooted = true;
                break;
            }
            current = process.ppid;
        }
        if !rooted {
            bail!("role peer PID {peer_pid} is not a verified descendant of session {session_id}")
        }
        self.verify_process(&handle)?;
        Ok(RolePeerProvenance {
            peer_pid,
            peer_process_group_id: peer.pgid,
            peer_start_marker: peer.start.clone(),
            managed_root_pid: handle.process.pid,
            managed_root_start_marker: handle.process.native_start_marker.clone(),
            state: "managed_process_group_untrusted_payload".to_owned(),
        })
    }

    pub fn process_group_members(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        let handle = self.handle(session_id)?;
        let inventory = process_inventory()?;
        self.observe_members(&handle, &inventory)?;
        Ok(self
            .remaining_members(&handle, &inventory)?
            .into_iter()
            .map(|member| {
                serde_json::json!({
                    "pid": member.pid, "ppid": member.ppid, "pgid": member.pgid,
                    "start": member.start, "command": member.command
                })
            })
            .collect())
    }

    fn handles(&self) -> Result<Vec<Arc<SessionHandle>>> {
        Ok(self
            .sessions
            .read()
            .map_err(|_| anyhow!("supervisor session map poisoned"))?
            .values()
            .cloned()
            .collect())
    }

    fn handle(&self, session_id: &str) -> Result<Arc<SessionHandle>> {
        self.sessions
            .read()
            .map_err(|_| anyhow!("supervisor session map poisoned"))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow!("session {session_id} is not attached to this service boot"))
    }

    fn attachment_handle(&self, binding: &AttachmentBinding) -> Result<Arc<SessionHandle>> {
        let handle = self.handle(&binding.session_id)?;
        if handle.role_generation_id != binding.role_generation_id
            || handle.transcript_epoch != binding.transcript_epoch
            || handle.process != binding.process
        {
            bail!("terminal attachment binding no longer matches this service session")
        }
        Ok(handle)
    }

    fn live_attachment_binding_locked(&self, handle: &SessionHandle) -> Result<AttachmentBinding> {
        self.verify_process(handle)?;
        let binding = AttachmentBinding {
            session_id: handle.session_id.clone(),
            role_generation_id: handle.role_generation_id.clone(),
            transcript_epoch: handle.transcript_epoch.clone(),
            process: handle.process.clone(),
        };
        self.store.verify_attachment_binding(&binding)?;
        Ok(binding)
    }

    fn verify_attachment_binding_locked(
        &self,
        handle: &SessionHandle,
        binding: &AttachmentBinding,
    ) -> Result<()> {
        if handle.session_id != binding.session_id
            || handle.role_generation_id != binding.role_generation_id
            || handle.transcript_epoch != binding.transcript_epoch
            || handle.process != binding.process
        {
            bail!("terminal attachment binding no longer matches the live generation")
        }
        self.verify_process(handle)?;
        self.store.verify_attachment_binding(binding)
    }

    /// Resolves the small window between a provider's exit and the periodic
    /// reconciler recording that exact exit. This is deliberately available
    /// only from the transcript path while the caller holds the session I/O
    /// boundary: input, lease, and resize operations remain live-only.
    fn reconcile_attachment_exit_locked(&self, handle: &SessionHandle) -> Result<()> {
        let status = handle
            .child
            .lock()
            .map_err(|_| anyhow!("provider child lock poisoned"))?
            .try_wait()?;
        let Some(status) = status else {
            return Ok(());
        };
        let inventory = process_inventory()
            .context("cannot verify attachment process-group quiescence after provider exit")?;
        self.observe_members(handle, &inventory)?;
        if !self.remaining_members(handle, &inventory)?.is_empty() {
            return Ok(());
        }
        let process_json = serde_json::to_string(&handle.process)?;
        let output_tail = (!status.success())
            .then(|| {
                transcript::recent_output_summary(
                    &self.transcripts_root,
                    &handle.session_id,
                    &handle.transcript_epoch,
                    2048,
                )
                .ok()
                .flatten()
            })
            .flatten();
        self.store.update_session_exit(
            &handle.session_id,
            &handle.transcript_epoch,
            &process_json,
            &serde_json::json!({
                "status": format!("{status:?}"),
                "observed_at": Utc::now().to_rfc3339(),
                "success": status.success(),
                "code": status.exit_code(),
                "output_tail": output_tail,
                "structured_result_required": true,
                "process_group_quiescent": true,
                "source": "attachment_transcript_reconciliation"
            })
            .to_string(),
        )?;
        Ok(())
    }

    fn attachment_generation_matches(
        &self,
        session_id: &str,
        role_generation_id: &str,
    ) -> Result<Option<bool>> {
        Ok(self
            .sessions
            .read()
            .map_err(|_| anyhow!("supervisor session map poisoned"))?
            .get(session_id)
            .map(|handle| handle.role_generation_id == role_generation_id))
    }

    fn verify_process(&self, handle: &SessionHandle) -> Result<()> {
        let observed = native_start_marker(handle.process.pid)?;
        if observed != handle.process.native_start_marker {
            bail!(
                "PID {} no longer matches recorded process start identity",
                handle.process.pid
            )
        }
        let pgid = unsafe { libc::getpgid(handle.process.pid as libc::pid_t) };
        if pgid != handle.process.process_group_id {
            bail!(
                "PID {} process group changed from {} to {}",
                handle.process.pid,
                handle.process.process_group_id,
                pgid
            )
        }
        Ok(())
    }

    fn observe_members(&self, handle: &SessionHandle, inventory: &[ProcessInfo]) -> Result<()> {
        let mut known = handle
            .known_members
            .lock()
            .map_err(|_| anyhow!("known process member lock poisoned"))?;
        let by_pid = inventory
            .iter()
            .map(|process| (process.pid, process))
            .collect::<HashMap<_, _>>();
        let mut changed = true;
        while changed {
            changed = false;
            for process in inventory {
                let in_group = process.pgid == handle.process.process_group_id;
                let parent_known = by_pid
                    .get(&process.ppid)
                    .and_then(|parent| known.get(&parent.pid).map(|start| start == &parent.start))
                    .unwrap_or(false);
                if (in_group || parent_known) && known.get(&process.pid) != Some(&process.start) {
                    known.insert(process.pid, process.start.clone());
                    changed = true;
                }
            }
        }
        let observed = inventory
            .iter()
            .filter(|process| known.get(&process.pid) == Some(&process.start))
            .cloned()
            .collect::<Vec<_>>();
        drop(known);
        let invocation_process_json = serde_json::to_string(&handle.process)?;
        for process in observed {
            self.store.record_session_process(
                &handle.session_id,
                &handle.transcript_epoch,
                &invocation_process_json,
                process.pid,
                &process.start,
                process.pgid,
                process.ppid,
            )?;
        }
        Ok(())
    }

    fn remaining_members(
        &self,
        handle: &SessionHandle,
        inventory: &[ProcessInfo],
    ) -> Result<Vec<ProcessInfo>> {
        let known = handle
            .known_members
            .lock()
            .map_err(|_| anyhow!("known process member lock poisoned"))?;
        Ok(inventory
            .iter()
            .filter(|process| {
                process.pgid == handle.process.process_group_id
                    || known
                        .get(&process.pid)
                        .is_some_and(|start| start == &process.start)
            })
            .cloned()
            .collect())
    }
}

fn deliver_setup_retained_first_turn_signal<F>(
    store: &Store,
    session_id: &str,
    process_group_id: i32,
    signal: F,
) -> Result<()>
where
    F: FnOnce(i32) -> std::io::Result<()>,
{
    if let Err(signal_error) = signal(process_group_id) {
        store.mark_session_delivery_ambiguous(
            session_id,
            &format!(
                "retained setup first-turn completion signal delivery is unknown: {signal_error}"
            ),
        )?;
        return Err(signal_error).context("signal verified retained setup provider process group");
    }
    Ok(())
}

fn service_stop_is_settling(
    first_stop_requested_at: Option<std::time::Instant>,
    observed_at: std::time::Instant,
) -> bool {
    first_stop_requested_at.is_some_and(|requested_at| {
        observed_at
            .checked_duration_since(requested_at)
            .is_some_and(|elapsed| elapsed < SERVICE_STOP_SETTLING_GRACE)
    })
}

fn codex_helper_image(launch: &PreparedLaunch, provider_pid: u32) -> Option<ExecutableFingerprint> {
    if launch.config.provider != Provider::Codex {
        return None;
    }
    codex_helper_image_from_paths(
        &launch.executable,
        process_executable_path(provider_pid).ok().as_deref(),
    )
}

fn codex_helper_image_from_paths(
    configured_executable: &std::path::Path,
    observed_provider_executable: Option<&std::path::Path>,
) -> Option<ExecutableFingerprint> {
    codex_helper_image_beside(configured_executable)
        .or_else(|| observed_provider_executable.and_then(codex_helper_image_beside))
}

fn codex_helper_image_beside(executable: &std::path::Path) -> Option<ExecutableFingerprint> {
    let executable = executable.canonicalize().ok()?;
    let executable_parent = executable.parent()?;
    let helper = executable_parent.join("codex-code-mode-host");
    let link_metadata = std::fs::symlink_metadata(&helper).ok()?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return None;
    }
    let canonical_helper = helper.canonicalize().ok()?;
    if canonical_helper != helper || canonical_helper.parent() != Some(executable_parent) {
        return None;
    }
    executable_fingerprint(&canonical_helper).ok()
}

fn latched_codex_helper_image(
    image: &Mutex<Option<ExecutableFingerprint>>,
    observed_provider_executable: Option<&std::path::Path>,
) -> Result<Option<ExecutableFingerprint>> {
    let mut image = image
        .lock()
        .map_err(|_| anyhow!("Codex helper image lock poisoned"))?;
    if image.is_none() {
        *image = observed_provider_executable.and_then(codex_helper_image_beside);
    }
    Ok(image.clone())
}

fn executable_fingerprint(path: &std::path::Path) -> Result<ExecutableFingerprint> {
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("resolve executable image {}", path.display()))?;
    let metadata = canonical_path
        .metadata()
        .with_context(|| format!("inspect executable image {}", canonical_path.display()))?;
    if !metadata.is_file() {
        bail!(
            "executable image {} is not a regular file",
            canonical_path.display()
        )
    }
    Ok(ExecutableFingerprint {
        canonical_path,
        device: metadata.dev(),
        inode: metadata.ino(),
        bytes: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
    })
}

fn same_executable_image(
    expected: &ExecutableFingerprint,
    observed: &ExecutableFingerprint,
) -> bool {
    expected.canonical_path == observed.canonical_path
        && expected.device == observed.device
        && expected.inode == observed.inode
        && expected.bytes == observed.bytes
        && expected.modified_seconds == observed.modified_seconds
        && expected.modified_nanos == observed.modified_nanos
}

fn native_members_idle<F>(
    provider_pid: u32,
    wrapper_pid: u32,
    codex_helper_image: Option<&ExecutableFingerprint>,
    codex_helper_identity: &Mutex<Option<NativeHelperIdentity>>,
    members: &[ProcessInfo],
    inspect_helper: F,
) -> Result<bool>
where
    F: FnOnce(u32) -> Result<(String, ExecutableFingerprint)>,
{
    let additional = members
        .iter()
        .filter(|process| process.pid != provider_pid && process.pid != wrapper_pid)
        .collect::<Vec<_>>();
    if additional.is_empty() {
        return Ok(true);
    }
    let (Some(expected_image), [helper]) = (codex_helper_image, additional.as_slice()) else {
        return Ok(false);
    };
    if helper.ppid != provider_pid || helper.start.is_empty() {
        return Ok(false);
    }
    let Ok((current_start, current_image)) = inspect_helper(helper.pid) else {
        return Ok(false);
    };
    if current_start != helper.start || !same_executable_image(expected_image, &current_image) {
        return Ok(false);
    }
    let mut identity = codex_helper_identity
        .lock()
        .map_err(|_| anyhow!("Codex helper identity lock poisoned"))?;
    let observed = NativeHelperIdentity {
        pid: helper.pid,
        native_start_marker: helper.start.clone(),
    };
    match identity.as_ref() {
        Some(latched) => Ok(latched == &observed),
        None => {
            *identity = Some(observed);
            Ok(true)
        }
    }
}

#[cfg(target_os = "macos")]
fn process_executable_path(pid: u32) -> Result<std::path::PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        bail!("could not inspect executable path for PID {pid}")
    }
    buffer.truncate(length as usize);
    Ok(std::path::PathBuf::from(OsString::from_vec(buffer)))
}

#[cfg(target_os = "linux")]
fn process_executable_path(pid: u32) -> Result<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .with_context(|| format!("inspect executable path for PID {pid}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_executable_path(pid: u32) -> Result<std::path::PathBuf> {
    bail!("executable-path inspection is unsupported for PID {pid} on this platform")
}

pub(crate) fn native_start_marker(pid: u32) -> Result<String> {
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()?;
    if !output.status.success() {
        bail!("could not inspect start identity for PID {pid}")
    }
    let marker = String::from_utf8(output.stdout)?.trim().to_owned();
    if marker.is_empty() {
        bail!("PID {pid} has no observable start identity")
    }
    Ok(marker)
}

fn process_inventory() -> Result<Vec<ProcessInfo>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,pgid=,lstart=,comm="])
        .output()?;
    if !output.status.success() {
        bail!("could not inspect process inventory")
    }
    let mut processes = Vec::new();
    for line in String::from_utf8(output.stdout)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 9 {
            bail!("process inventory contained a malformed row")
        }
        let (Ok(pid), Ok(ppid), Ok(pgid)) =
            (fields[0].parse(), fields[1].parse(), fields[2].parse())
        else {
            bail!("process inventory contained an unparseable identity row")
        };
        processes.push(ProcessInfo {
            pid,
            ppid,
            pgid,
            start: fields[3..8].join(" "),
            command: fields[8..].join(" "),
        });
    }
    if processes.is_empty() {
        bail!("process inventory returned no parseable processes")
    }
    Ok(processes)
}

pub fn system_boot_identity() -> Result<String> {
    let linux = std::path::Path::new("/proc/sys/kernel/random/boot_id");
    if linux.is_file() {
        let value = std::fs::read_to_string(linux)?;
        let value =
            uuid::Uuid::parse_str(value.trim()).context("Linux boot identity is not a UUID")?;
        if value.is_nil() {
            bail!("Linux boot identity is invalid")
        }
        return Ok(format!("linux:{value}"));
    }
    let output = Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()
        .context("read operating-system boot identity")?;
    let value = String::from_utf8(output.stdout)?.trim().to_owned();
    if !output.status.success() || value.is_empty() {
        bail!("operating-system boot identity is unavailable")
    }
    let value =
        uuid::Uuid::parse_str(&value).context("macOS boot session identity is not a UUID")?;
    if value.is_nil() {
        bail!("macOS boot session identity is invalid")
    }
    Ok(format!("darwin:bootsessionuuid:{value}"))
}

pub fn boot_identity_proves_reboot(recorded: &str, current: &str) -> bool {
    fn comparable(value: &str) -> Option<(&str, uuid::Uuid)> {
        let (platform, raw) = if let Some(raw) = value.strip_prefix("linux:") {
            ("linux", raw)
        } else {
            ("darwin", value.strip_prefix("darwin:bootsessionuuid:")?)
        };
        let identity = uuid::Uuid::parse_str(raw).ok()?;
        (!identity.is_nil()).then_some((platform, identity))
    }
    matches!((comparable(recorded), comparable(current)),
        (Some((recorded_platform, recorded_id)), Some((current_platform, current_id)))
            if recorded_platform == current_platform && recorded_id != current_id)
}

fn discover_processes(
    root_pid: Option<u32>,
    process_group_id: Option<i32>,
    inventory: &[ProcessInfo],
    previously_observed: &[ProcessInfo],
) -> Vec<ProcessInfo> {
    let by_pid = inventory
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut known = previously_observed
        .iter()
        .map(|process| (process.pid, process.start.clone()))
        .collect::<HashMap<_, _>>();
    if let Some(root) = root_pid {
        if let Some(process) = by_pid.get(&root) {
            known.insert(process.pid, process.start.clone());
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for process in inventory {
            let in_group = process_group_id == Some(process.pgid);
            let parent_known = by_pid
                .get(&process.ppid)
                .and_then(|parent| known.get(&parent.pid).map(|start| start == &parent.start))
                .unwrap_or(false);
            if (in_group || parent_known) && known.get(&process.pid) != Some(&process.start) {
                known.insert(process.pid, process.start.clone());
                changed = true;
            }
        }
    }
    inventory
        .iter()
        .filter(|process| known.get(&process.pid) == Some(&process.start))
        .cloned()
        .collect()
}

fn newly_spawned_service_descendants(
    baseline: &[ProcessInfo],
    inventory: &[ProcessInfo],
) -> Vec<ProcessInfo> {
    let baseline = baseline
        .iter()
        .map(|process| (process.pid, process.start.clone()))
        .collect::<HashMap<_, _>>();
    let by_pid = inventory
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut observed = inventory
        .iter()
        .filter(|process| {
            process.ppid == std::process::id() && baseline.get(&process.pid) != Some(&process.start)
        })
        .map(|process| (process.pid, process.start.clone()))
        .collect::<HashMap<_, _>>();
    let mut changed = true;
    while changed {
        changed = false;
        for process in inventory {
            let parent_observed = by_pid
                .get(&process.ppid)
                .and_then(|parent| {
                    observed
                        .get(&parent.pid)
                        .map(|start| start == &parent.start)
                })
                .unwrap_or(false);
            if parent_observed && observed.get(&process.pid) != Some(&process.start) {
                observed.insert(process.pid, process.start.clone());
                changed = true;
            }
        }
    }
    inventory
        .iter()
        .filter(|process| observed.get(&process.pid) == Some(&process.start))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct RunningChild {
        kill_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl portable_pty::ChildKiller for RunningChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.kill_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(RunningChild {
                kill_count: self.kill_count.clone(),
            })
        }
    }

    impl portable_pty::Child for RunningChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            Some(42)
        }
    }

    fn image(path: &str) -> ExecutableFingerprint {
        ExecutableFingerprint {
            canonical_path: path.into(),
            device: 1,
            inode: 2,
            bytes: 3,
            modified_seconds: 4,
            modified_nanos: 5,
        }
    }

    fn process(pid: u32, ppid: u32, start: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            pgid: 10,
            start: start.into(),
            command: "/package/codex-code-mode-host".into(),
        }
    }

    fn members(helper: ProcessInfo) -> Vec<ProcessInfo> {
        vec![
            process(10, 1, "wrapper"),
            process(20, 10, "provider"),
            helper,
        ]
    }

    #[test]
    fn control_peer_rejects_a_pid_when_its_captured_start_identity_differs() {
        let root =
            std::env::temp_dir().join(format!("agenticjira-peer-g14-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        let supervisor = Supervisor::new(store, root.join("transcripts"));
        let pid = std::process::id();
        assert_ne!(native_start_marker(pid).unwrap(), "g14-reused-pid-start");
        let error = supervisor
            .peer_is_managed_or_descendant(
                &PeerProcessIdentity::new(pid, "g14-reused-pid-start".into()).unwrap(),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("control peer changed before ancestry validation"));
        drop(supervisor);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn control_peer_is_denied_when_an_exact_retained_member_is_reparented() {
        let inventory = vec![process(10, 1, "root-start"), process(20, 1, "member-start")];
        let managed_members = HashMap::from([
            (10, "root-start".to_owned()),
            (20, "member-start".to_owned()),
        ]);

        assert!(peer_matches_managed_identity_or_descendant(
            &PeerProcessIdentity::new(20, "member-start".into()).unwrap(),
            &inventory,
            &managed_members,
        )
        .unwrap());
    }

    #[test]
    fn exact_codex_helper_image_is_latched_by_pid_and_start() {
        let expected = image("/package/codex-code-mode-host");
        let identity = Mutex::new(None);
        let observed = members(process(30, 20, "helper-start"));
        assert!(
            native_members_idle(20, 10, Some(&expected), &identity, &observed, |_| Ok((
                "helper-start".into(),
                expected.clone()
            )),)
            .unwrap()
        );
        assert_eq!(
            *identity.lock().unwrap(),
            Some(NativeHelperIdentity {
                pid: 30,
                native_start_marker: "helper-start".into()
            })
        );
        assert!(!native_members_idle(
            20,
            10,
            Some(&expected),
            &identity,
            &members(process(31, 20, "replacement-start")),
            |_| Ok(("replacement-start".into(), expected.clone())),
        )
        .unwrap());
    }

    #[test]
    fn codex_helper_image_falls_back_from_a_shim_to_the_observed_provider() {
        let root = std::env::temp_dir().join(format!(
            "llmrelay-codex-helper-paths-{}",
            uuid::Uuid::new_v4()
        ));
        let shim_root = root.join("cmux-cli-shims");
        let provider_root = root.join("provider");
        std::fs::create_dir_all(&shim_root).unwrap();
        std::fs::create_dir_all(&provider_root).unwrap();
        let shim = shim_root.join("codex");
        let provider = provider_root.join("codex");
        let helper = provider_root.join("codex-code-mode-host");
        std::fs::write(&shim, b"shim").unwrap();
        std::fs::write(&provider, b"provider").unwrap();
        std::fs::write(&helper, b"helper").unwrap();

        let observed = codex_helper_image_from_paths(&shim, Some(&provider)).unwrap();
        assert_eq!(observed.canonical_path, helper.canonicalize().unwrap());
        let delayed = Mutex::new(None);
        assert_eq!(
            latched_codex_helper_image(&delayed, Some(&provider))
                .unwrap()
                .unwrap()
                .canonical_path,
            helper.canonicalize().unwrap()
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_helper_classification_rejects_every_unverified_or_extra_member() {
        let expected = image("/package/codex-code-mode-host");
        let candidate = process(30, 20, "helper-start");
        let check = |expected_image: Option<&ExecutableFingerprint>,
                     members: Vec<ProcessInfo>,
                     current_start: &str,
                     current_image: ExecutableFingerprint| {
            native_members_idle(20, 10, expected_image, &Mutex::new(None), &members, |_| {
                Ok((current_start.into(), current_image))
            })
            .unwrap()
        };

        assert!(!check(
            None,
            members(candidate.clone()),
            "helper-start",
            expected.clone()
        ));
        assert!(!check(
            Some(&expected),
            members(process(30, 21, "helper-start")),
            "helper-start",
            expected.clone()
        ));
        assert!(!check(
            Some(&expected),
            members(candidate.clone()),
            "helper-start",
            image("/impostor/codex-code-mode-host")
        ));
        let mut drifted = expected.clone();
        drifted.bytes += 1;
        assert!(!check(
            Some(&expected),
            members(candidate.clone()),
            "helper-start",
            drifted
        ));
        assert!(!check(
            Some(&expected),
            members(candidate.clone()),
            "different-start",
            expected.clone()
        ));
        assert!(!native_members_idle(
            20,
            10,
            Some(&expected),
            &Mutex::new(None),
            &members(candidate.clone()),
            |_| bail!("OS lookup failed"),
        )
        .unwrap());
        let mut extra = members(candidate);
        extra.push(process(40, 30, "unknown-child"));
        assert!(!check(
            Some(&expected),
            extra,
            "helper-start",
            expected.clone()
        ));
    }

    #[test]
    fn retained_setup_signal_failure_enters_explicit_recovery() {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-retained-setup-signal-failure-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        {
            let connection = store.lock().unwrap();
            connection.execute_batch(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                   VALUES('p','Project','/tmp/project','identity','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,attention,created_at,updated_at)
                   VALUES('t','p','Task','Task','[]','validation','paused','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                   VALUES('a','t','context','planning','base',1,'held','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                   VALUES('g','a','manager','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
                   VALUES('s','g','codex','interrupt_requested','{}','fixture','epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            ).unwrap();
        }
        let error = deliver_setup_retained_first_turn_signal(&store, "s", 42, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("signal verified retained setup provider process group"));
        let connection = store.lock().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT status FROM sessions WHERE id='s'", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "recovery_required"
        );
        assert_eq!(
            connection.query_row(
                "SELECT COUNT(*) FROM recovery_records WHERE session_id='s' AND state='attention_required'",
                [],
                |row| row.get::<_, i64>(0),
            ).unwrap(),
            1
        );
        assert!(connection
            .query_row(
                "SELECT launch_error FROM sessions WHERE id='s'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .contains("signal delivery is unknown"));
        drop(connection);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn expired_attached_setup_stop_precedes_unrelated_inventory_failure() {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-retained-setup-inventory-failure-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        let process = ProcessIdentity {
            pid: 42,
            process_group_id: 42,
            native_start_marker: "fixture-start".into(),
            observed_started_at: "2026-01-01T00:00:00Z".into(),
        };
        let process_json = serde_json::to_string(&process).unwrap();
        {
            let connection = store.lock().unwrap();
            connection.execute_batch(
                "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                   VALUES('p','Project','/tmp/project','identity','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,attention,created_at,updated_at)
                   VALUES('t','p','Task','Task','[]','validation','none','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                   VALUES('a','t','context','planning','base',1,'held','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                   VALUES('g','a','manager','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                 INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,
                   transcript_epoch,native_session_id,readiness_state,created_at,updated_at)
                   VALUES('s','g','codex','interrupt_requested','{}','fixture','epoch','native','idle_candidate',
                   '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
            connection
                .execute(
                    "UPDATE sessions SET process_identity_json=?1 WHERE id='s'",
                    rusqlite::params![process_json],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO audit_events(id,operation_id,actor_kind,event_code,entity_kind,entity_id,detail_json,created_at)
                     VALUES('audit','operation','service','setup.retained_first_turn.stop_claimed','session','s',?1,'2026-01-01T00:00:00Z')",
                    rusqlite::params![serde_json::json!({
                        "role_generation_id":"g",
                        "transcript_epoch":"epoch",
                        "native_session_id":"native",
                        "process_identity_json":process_json,
                        "stop_event_rowid":7,
                        "timeout_at":"2999-01-01T00:00:00Z"
                    }).to_string()],
                )
                .unwrap();
        }
        let supervisor = Supervisor::new(store.clone(), root.join("transcripts"));
        let kill_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pty = native_pty_system().openpty(PtySize::default()).unwrap();
        supervisor.sessions.write().unwrap().insert(
            "s".into(),
            Arc::new(SessionHandle {
                session_id: "s".into(),
                role_generation_id: "g".into(),
                transcript_epoch: "epoch".into(),
                process: process.clone(),
                group_leader: ProcessGenerationAnchor {
                    pid: 42,
                    process_group_id: 42,
                    native_start_marker: "fixture-start".into(),
                    boot_identity: "linux:00000000-0000-0000-0000-000000000001".into(),
                },
                child: Mutex::new(Box::new(RunningChild {
                    kill_count: kill_count.clone(),
                })),
                input: Mutex::new(Box::new(std::io::sink())),
                io_boundary: Mutex::new(()),
                known_members: Mutex::new(HashMap::from([(42, "fixture-start".into())])),
                first_stop_requested_at: Mutex::new(Some(
                    std::time::Instant::now() - std::time::Duration::from_secs(31),
                )),
                codex_helper_image: None,
                codex_helper_identity: Mutex::new(None),
                _master: Mutex::new(pty.master),
            }),
        );
        let unrelated_pty = native_pty_system().openpty(PtySize::default()).unwrap();
        supervisor.sessions.write().unwrap().insert(
            "unrelated".into(),
            Arc::new(SessionHandle {
                session_id: "unrelated".into(),
                role_generation_id: "unrelated-generation".into(),
                transcript_epoch: "unrelated-epoch".into(),
                process: ProcessIdentity {
                    pid: 43,
                    process_group_id: 43,
                    native_start_marker: "unrelated-start".into(),
                    observed_started_at: "2026-01-01T00:00:00Z".into(),
                },
                group_leader: ProcessGenerationAnchor {
                    pid: 43,
                    process_group_id: 43,
                    native_start_marker: "unrelated-start".into(),
                    boot_identity: "linux:00000000-0000-0000-0000-000000000001".into(),
                },
                child: Mutex::new(Box::new(RunningChild {
                    kill_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                })),
                input: Mutex::new(Box::new(std::io::sink())),
                io_boundary: Mutex::new(()),
                known_members: Mutex::new(HashMap::from([(43, "unrelated-start".into())])),
                first_stop_requested_at: Mutex::new(None),
                codex_helper_image: None,
                codex_helper_identity: Mutex::new(None),
                _master: Mutex::new(unrelated_pty.master),
            }),
        );
        supervisor.fail_process_inventory_for_tests(Some("fixture inventory unavailable"));

        assert!(
            crate::coordinator::reconcile_one_setup_retained_first_turn_stop_timeout(
                &store,
                &supervisor,
            )
            .unwrap()
            .is_none()
        );
        let nonexpired = supervisor.reconcile().unwrap_err().to_string();
        assert!(nonexpired.contains("fixture inventory unavailable"));
        {
            let connection = store.lock().unwrap();
            connection
                .execute(
                    "UPDATE audit_events SET detail_json=json_set(detail_json,'$.timeout_at','2000-01-01T00:00:00Z')
                     WHERE id='audit'",
                    [],
                )
                .unwrap();
        }
        let visible = crate::coordinator::reconcile_one_setup_retained_first_turn_stop_timeout(
            &store,
            &supervisor,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            visible["action"],
            "setup_retained_first_turn_stop_timed_out"
        );
        assert_eq!(visible["session_id"], "s");
        assert_eq!(visible["automatic_resignal"], false);
        let connection = store.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM recovery_records WHERE session_id='s' AND state='attention_required'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT json_extract(detail_json,'$.exact_process_live') FROM recovery_records WHERE session_id='s'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        drop(connection);
        assert!(
            crate::coordinator::reconcile_one_setup_retained_first_turn_stop_timeout(
                &store,
                &supervisor,
            )
            .unwrap()
            .is_none()
        );
        assert!(supervisor
            .reconcile()
            .unwrap_err()
            .to_string()
            .contains("fixture inventory unavailable"));
        let connection = store.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM recovery_records WHERE session_id='s'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(kill_count.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(connection);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn graceful_stop_deadline_stays_durable_across_an_explicit_safe_retry() {
        let root = std::env::temp_dir().join(format!(
            "agenticjira-graceful-stop-deadline-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(&root.join("state.sqlite3")).unwrap();
        use std::os::unix::process::CommandExt;
        struct ProcessGroupCleanup(i32);
        impl Drop for ProcessGroupCleanup {
            fn drop(&mut self) {
                let _ = unsafe { libc::kill(-self.0, libc::SIGKILL) };
            }
        }
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "trap '' INT; while :; do sleep 1; done"]);
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let process_group_id = (0..20)
            .find_map(|_| {
                let group = unsafe { libc::getpgid(pid as libc::pid_t) };
                if group == pid as i32 {
                    Some(group)
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    None
                }
            })
            .expect("fixture child must own an isolated process group");
        let _cleanup = ProcessGroupCleanup(process_group_id);
        let identity = ProcessIdentity {
            pid,
            process_group_id,
            native_start_marker: native_start_marker(pid).unwrap(),
            observed_started_at: "2026-01-01T00:00:00Z".into(),
        };
        let identity_json = serde_json::to_string(&identity).unwrap();
        let anchor = ProcessGenerationAnchor {
            pid,
            process_group_id,
            native_start_marker: identity.native_start_marker.clone(),
            boot_identity: system_boot_identity().unwrap(),
        };
        let requested_at = (Utc::now() - Duration::seconds(GRACEFUL_STOP_SECONDS + 1)).to_rfc3339();
        {
            let connection = store.lock().unwrap();
            connection
                .execute_batch(
                    "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                       VALUES('p','Project','/tmp/project','identity','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,created_at,updated_at)
                       VALUES('t','p','Task','Task','[]','in_progress','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                       VALUES('a','t','context','implementation','base',1,'running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
                       VALUES('claim','t','a','identity','running','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                       VALUES('g','a','implementer','codex',1,1,'running','authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,transcript_epoch,created_at,updated_at)
                       VALUES('s','g','codex','interrupt_requested','{}','fixture','epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
                       VALUES('setting','t','implementer',1,'{}','g','2026-01-01T00:00:00Z');",
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE sessions SET process_identity_json=?1,interrupt_requested_at=?2,
                            recovery_root_pid=?3,recovery_process_group_id=?4,
                            recovery_anchor_json=?5,launch_boot_identity=?6 WHERE id='s'",
                    rusqlite::params![
                        identity_json,
                        requested_at,
                        pid,
                        process_group_id,
                        serde_json::to_string(&anchor).unwrap(),
                        anchor.boot_identity,
                    ],
                )
                .unwrap();
        }
        assert!(crate::recovery::reconcile_prior_boot(&store)
            .unwrap()
            .is_empty());
        {
            let connection = store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT status FROM sessions WHERE id='s'", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .unwrap(),
                "interrupt_requested"
            );
        }
        let supervisor = Supervisor::new(store.clone(), root.join("transcripts"));
        supervisor.reconcile().unwrap();
        {
            let connection = store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT s.status,s.interrupt_requested_at,
                                json_extract(r.detail_json,'$.automatic_signal'),
                                json_extract(r.detail_json,'$.automatic_force_stop')
                           FROM sessions s
                           JOIN recovery_records r ON r.session_id=s.id
                          WHERE s.id='s' AND json_extract(r.detail_json,'$.kind')='graceful_stop_deadline'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                            ))
                        },
                    )
                    .unwrap(),
                ("recovery_required".into(), requested_at.clone(), 0, 0)
            );
        }
        supervisor.retry_graceful_stop_exact("s").unwrap();
        {
            let connection = store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT interrupt_requested_at FROM sessions WHERE id='s'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                requested_at
            );
        }
        supervisor.force_stop_exact_managed_process("s").unwrap();
        child.wait().unwrap();
        supervisor.reconcile().unwrap();
        let connection = store.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT s.status,s.interrupt_requested_at,
                            json_extract(r.detail_json,'$.interrupt_requested_at'),
                            (SELECT COUNT(*) FROM recovery_records pending
                              WHERE pending.session_id=s.id AND pending.state='attention_required'
                                AND json_extract(pending.detail_json,'$.kind')='graceful_stop_deadline'),
                            r.state,json_extract(r.detail_json,'$.next_disposition'),
                            claim.state,a.status,t.attention,
                            json_extract(rc.result_json,'$.native_resume_forbidden')
                       FROM sessions s
                       JOIN role_generations g ON g.id=s.role_generation_id
                       JOIN attempts a ON a.id=g.attempt_id
                       JOIN tasks t ON t.id=a.task_id
                       JOIN claims claim ON claim.attempt_id=a.id
                       JOIN recovery_records r ON r.session_id=s.id
                       JOIN restart_candidates rc ON rc.session_id=s.id
                      WHERE s.id='s' AND json_extract(r.detail_json,'$.kind')='graceful_stop_deadline'",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, i64>(9)?,
                        ))
                    },
                )
                .unwrap(),
            (
                "exited".into(),
                None,
                requested_at.clone(),
                0,
                "resolved_graceful_stop_quiescent".into(),
                "fresh_dispatch_hold_available".into(),
                "running".into(),
                "restart_parked".into(),
                "restart_parked".into(),
                1,
            )
        );
        drop(connection);
        let projection = crate::workflow::state(&store).unwrap();
        let continuation = projection
            .continuation_actions
            .iter()
            .find(|action| action.operation == "continue" && action.binding["session_id"] == "s")
            .unwrap();
        assert!(matches!(
            continuation.kind,
            crate::domain::ContinuationActionKind::ContinueFreshDispatch
        ));
        assert!(projection.continuation_actions.iter().all(|action| {
            action.binding["session_id"] != "s"
                || !matches!(
                    action.operation.as_str(),
                    "retry_graceful_stop"
                        | "force_stop_exact_process"
                        | "restart_resume"
                        | "role_resume"
                )
        }));
        let version: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT version FROM tasks WHERE id='t'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            crate::workflow::execute(
                &store,
                &crate::domain::HumanCommand::Control {
                    operation_id: "graceful-stop-next-current-control".into(),
                    task_id: "t".into(),
                    expected_version: version,
                    action: "continue".into(),
                    payload: serde_json::json!({}),
                },
            )
            .unwrap()
            .state,
            "control_requested"
        );
        let continue_paths =
            crate::config::InstancePaths::resolve(Some(root.join("graceful-stop-continue")))
                .unwrap();
        continue_paths.create().unwrap();
        let continue_app = crate::operations::Application::new(
            continue_paths,
            store.clone(),
            std::env::current_exe().unwrap(),
        )
        .unwrap();
        let continue_state = |control_id: &str| {
            let connection = store.lock().unwrap();
            connection
                .query_row(
                    "SELECT rc.state,t.attention,a.status,c.state,claim.state
                       FROM restart_candidates rc
                       JOIN attempts a ON a.id=rc.attempt_id
                       JOIN tasks t ON t.id=a.task_id
                       JOIN claims claim ON claim.attempt_id=a.id
                       JOIN controls c ON c.attempt_id=a.id AND c.requested_operation_id=?1
                      WHERE rc.session_id='s'",
                    [control_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .unwrap()
        };
        {
            let connection = store.lock().unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER graceful_stop_continue_ownership_boundary
                     AFTER UPDATE OF state ON restart_candidates
                     WHEN NEW.session_id='s' AND NEW.state='released_fresh_dispatch'
                     BEGIN
                       UPDATE claims SET state='unknown' WHERE id='claim';
                     END;",
                )
                .unwrap();
        }
        let parked_continue = continue_state("graceful-stop-next-current-control");
        let rejected_continue = continue_app.coordinator_tick().unwrap();
        assert_eq!(rejected_continue["action"], "control_rejected");
        assert_eq!(
            continue_state("graceful-stop-next-current-control"),
            (
                parked_continue.0,
                parked_continue.1,
                parked_continue.2,
                "rejected".into(),
                parked_continue.4,
            )
        );
        store
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER graceful_stop_continue_ownership_boundary;")
            .unwrap();
        let version: i64 = store
            .lock()
            .unwrap()
            .query_row("SELECT version FROM tasks WHERE id='t'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            crate::workflow::execute(
                &store,
                &crate::domain::HumanCommand::Control {
                    operation_id: "graceful-stop-atomic-continue".into(),
                    task_id: "t".into(),
                    expected_version: version,
                    action: "continue".into(),
                    payload: serde_json::json!({}),
                },
            )
            .unwrap()
            .state,
            "control_requested"
        );
        let continued = continue_app.coordinator_tick().unwrap();
        assert_eq!(continued["action"], "continued");
        assert_eq!(
            continue_state("graceful-stop-atomic-continue"),
            (
                "released_fresh_dispatch".into(),
                "none".into(),
                "running".into(),
                "finished".into(),
                "running".into(),
            )
        );
        let manager_store = Store::open(&root.join("manager-stop-switch.sqlite3")).unwrap();
        let manager_process = serde_json::json!({
            "pid": 101,
            "process_group_id": 101,
            "native_start_marker": "manager-stop",
            "observed_started_at": "2026-01-01T00:00:00Z",
        })
        .to_string();
        {
            let connection = manager_store.lock().unwrap();
            connection
                .execute_batch(
                    "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
                       VALUES('manager-project','Manager project','/tmp/manager-project','manager-identity','base','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,attention,created_at,updated_at)
                       VALUES('manager-task','manager-project','Manager stop','Manager stop','[]','in_progress','needs_recovery','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
                       VALUES('manager-attempt','manager-task','context','implementation','base',1,'needs_recovery','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO claims(id,task_id,attempt_id,repository_identity,state,created_at,updated_at)
                       VALUES('manager-claim','manager-task','manager-attempt','manager-identity','unknown','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO role_generations(id,attempt_id,role,provider,generation,config_revision,status,authority_generation,created_at,updated_at)
                       VALUES('manager-generation','manager-attempt','manager','codex',1,1,'recovery_required','manager-authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
                             ('live-implementer-generation','manager-attempt','implementer','codex',1,1,'running','implementer-authority','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO sessions(id,role_generation_id,provider,status,launch_config_json,executable_version,native_session_id,transcript_epoch,created_at,updated_at)
                       VALUES('manager-session','manager-generation','codex','recovery_required','{}','fixture','manager-native','manager-stop-epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z'),
                             ('live-implementer-session','live-implementer-generation','codex','running','{}','fixture','live-native','live-epoch','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO role_settings(id,task_id,role,revision,config_json,effective_generation_id,created_at)
                       VALUES('manager-setting','manager-task','manager',1,'{}','manager-generation','2026-01-01T00:00:00Z'),
                             ('live-implementer-setting','manager-task','implementer',1,'{}','live-implementer-generation','2026-01-01T00:00:00Z');
                     INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
                       VALUES('manager-stop-control','manager-attempt','manager_stop','held',1,'{\"manager_session_id\":\"manager-session\",\"quiescent\":false}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                     INSERT INTO capabilities(id,provider,executable_version,role,mode,config_hash,status,evidence_reference,gaps_json,checked_at,proof_json)
                       VALUES('manager-capability','codex','fixture','manager','interactive_pty','manager-capability-key','supported','fixture','[]','2026-01-01T00:00:00Z','{\"fixture\":true}');",
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE sessions SET process_identity_json=?1,capability_key='manager-capability-key'
                     WHERE id='manager-session'",
                    rusqlite::params![manager_process],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
                     VALUES('manager-stop-recovery','manager-session','manager-attempt','attention_required',?1,
                       '{\"kind\":\"graceful_stop_deadline\",\"role_generation_id\":\"manager-generation\",\"transcript_epoch\":\"manager-stop-epoch\"}',
                       '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
                    rusqlite::params![manager_process],
                )
                .unwrap();
        }
        assert!(manager_store
            .update_session_exit(
                "manager-session",
                "manager-stop-epoch",
                &manager_process,
                &serde_json::json!({"process_group_quiescent": true}).to_string(),
            )
            .unwrap());
        {
            let connection = manager_store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT t.attention,a.status,c.state,claim.state,
                                json_extract(c.payload_json,'$.native_resume_forbidden'),
                                json_extract(c.payload_json,'$.resume_fence_generation_id'),
                                (SELECT COUNT(*) FROM restart_candidates WHERE attempt_id=a.id),
                                r.state
                         FROM tasks t JOIN attempts a ON a.task_id=t.id
                         JOIN claims claim ON claim.attempt_id=a.id
                         JOIN controls c ON c.attempt_id=a.id
                         JOIN recovery_records r ON r.id='manager-stop-recovery'
                         WHERE a.id='manager-attempt' AND c.id='manager-stop-control'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, String>(7)?,
                            ))
                        },
                    )
                    .unwrap(),
                (
                    "none".into(),
                    "running".into(),
                    "held".into(),
                    "running".into(),
                    1,
                    "manager-generation".into(),
                    0,
                    "resolved_graceful_stop_quiescent".into(),
                )
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT status FROM sessions WHERE id='live-implementer-session'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "running"
            );
        }
        {
            let connection = manager_store.lock().unwrap();
            connection
                .execute(
                    "UPDATE controls
                     SET payload_json=json_remove(payload_json,
                       '$.native_resume_forbidden','$.resume_fence_generation_id')
                     WHERE id='manager-stop-control'",
                    [],
                )
                .unwrap();
        }
        let stale_launch = crate::domain::LaunchConfig {
            provider: crate::domain::Provider::Codex,
            role: crate::domain::RoleKind::Manager,
            executable: std::path::PathBuf::from("/fixture/not-used-before-fence"),
            executable_version: "fixture".into(),
            model: "fixture".into(),
            effort: "fixture".into(),
            cwd: root.clone(),
            argv: Vec::new(),
            environment_keys: Vec::new(),
            permission_policy: "fixture".into(),
            security_policy: serde_json::json!({}),
            hook_revision: "fixture".into(),
            capability_status: crate::domain::CapabilityStatus::Unverified,
        };
        let manager_projection = crate::workflow::state(&manager_store).unwrap();
        assert!(manager_projection
            .continuation_actions
            .iter()
            .all(|action| {
                action.binding["session_id"] != "manager-session"
                    || !matches!(action.operation.as_str(), "role_resume" | "restart_resume")
            }));
        assert!(manager_store
            .reserve_role_resume(
                "manager-session",
                "manager-stale-resume",
                &stale_launch,
                "manager-stale-token",
            )
            .unwrap_err()
            .to_string()
            .contains("settled manager-stop intent"));
        let manager_version: i64 = manager_store
            .lock()
            .unwrap()
            .query_row(
                "SELECT version FROM tasks WHERE id='manager-task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            crate::workflow::execute(
                &manager_store,
                &crate::domain::HumanCommand::Control {
                    operation_id: "manager-stop-continue".into(),
                    task_id: "manager-task".into(),
                    expected_version: manager_version,
                    action: "continue_manager".into(),
                    payload: serde_json::json!({}),
                },
            )
            .unwrap()
            .state,
            "manager_stop_released"
        );
        {
            let connection = manager_store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT t.attention,a.status,claim.state,c.state,
                                (SELECT COUNT(*) FROM restart_candidates WHERE attempt_id=a.id),r.state
                         FROM tasks t JOIN attempts a ON a.task_id=t.id
                         JOIN claims claim ON claim.attempt_id=a.id
                         JOIN controls c ON c.id='manager-stop-control'
                         JOIN recovery_records r ON r.id='manager-stop-recovery'
                         WHERE a.id='manager-attempt'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                            ))
                        },
                    )
                    .unwrap(),
                (
                    "none".into(),
                    "running".into(),
                    "running".into(),
                    "cancelled".into(),
                    0,
                    "resolved_graceful_stop_quiescent".into(),
                )
            );
        }

        let switch_process = serde_json::json!({
            "pid": 102,
            "process_group_id": 102,
            "native_start_marker": "switch-stop",
            "observed_started_at": "2026-01-01T00:01:00Z",
        })
        .to_string();
        {
            let connection = manager_store.lock().unwrap();
            connection
                .execute_batch(
                    "UPDATE tasks SET attention='needs_recovery' WHERE id='manager-task';
                     UPDATE attempts SET status='needs_recovery' WHERE id='manager-attempt';
                     UPDATE claims SET state='unknown' WHERE id='manager-claim';
                     UPDATE role_generations SET status='stopping' WHERE id='manager-generation';
                     INSERT INTO switch_intents(id,attempt_id,role,old_generation_id,requested_settings_revision,handoff_json,state,created_at,updated_at)
                       VALUES('recovered-switch','manager-attempt','manager','manager-generation',2,'{}','stopping_old','2026-01-01T00:01:00Z','2026-01-01T00:01:00Z');
                     INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
                       VALUES('recovered-switch-control','manager-attempt','manager_change','switching',2,'{\"old_generation_id\":\"manager-generation\"}','2026-01-01T00:01:00Z','2026-01-01T00:01:00Z');",
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE sessions SET status='recovery_required',transcript_epoch='switch-stop-epoch',
                         process_identity_json=?1,exit_json=NULL WHERE id='manager-session'",
                    rusqlite::params![switch_process],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO recovery_records(id,session_id,attempt_id,state,process_identity_json,detail_json,created_at,updated_at)
                     VALUES('switch-stop-recovery','manager-session','manager-attempt','attention_required',?1,
                       '{\"kind\":\"graceful_stop_deadline\",\"role_generation_id\":\"manager-generation\",\"transcript_epoch\":\"switch-stop-epoch\"}',
                       '2026-01-01T00:01:00Z','2026-01-01T00:01:00Z')",
                    rusqlite::params![switch_process],
                )
                .unwrap();
        }
        assert!(manager_store
            .update_session_exit(
                "manager-session",
                "switch-stop-epoch",
                &switch_process,
                &serde_json::json!({"process_group_quiescent": true}).to_string(),
            )
            .unwrap());
        {
            let connection = manager_store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT t.attention,a.status,claim.state,switch.state,control.state,
                                json_extract(switch.handoff_json,'$.native_resume_forbidden'),
                                json_extract(control.payload_json,'$.resume_fence_generation_id'),
                                (SELECT COUNT(*) FROM restart_candidates WHERE attempt_id=a.id),recovery.state
                         FROM tasks t JOIN attempts a ON a.task_id=t.id
                         JOIN claims claim ON claim.attempt_id=a.id
                         JOIN switch_intents switch ON switch.id='recovered-switch'
                         JOIN controls control ON control.id='recovered-switch-control'
                         JOIN recovery_records recovery ON recovery.id='switch-stop-recovery'
                         WHERE a.id='manager-attempt'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, i64>(5)?,
                                row.get::<_, String>(6)?,
                                row.get::<_, i64>(7)?,
                                row.get::<_, String>(8)?,
                            ))
                        },
                    )
                    .unwrap(),
                (
                    "none".into(),
                    "running".into(),
                    "running".into(),
                    "rejected".into(),
                    "failed".into(),
                    1,
                    "manager-generation".into(),
                    0,
                    "resolved_graceful_stop_quiescent".into(),
                )
            );
        }
        let switch_projection = crate::workflow::state(&manager_store).unwrap();
        assert!(switch_projection.continuation_actions.iter().all(|action| {
            action.binding["session_id"] != "manager-session"
                || !matches!(action.operation.as_str(), "role_resume" | "restart_resume")
        }));
        assert!(switch_projection.continuation_actions.iter().any(|action| {
            action.operation == "refresh_and_reconcile"
                && action.binding["switch_intent_id"] == "recovered-switch"
        }));
        assert!(manager_store
            .reserve_role_resume(
                "manager-session",
                "switch-stale-resume",
                &stale_launch,
                "switch-stale-token",
            )
            .unwrap_err()
            .to_string()
            .contains("rejected exact-generation switch"));
        {
            let connection = manager_store.lock().unwrap();
            connection
                .execute(
                    "UPDATE switch_intents SET state='stopping_old' WHERE id='recovered-switch'",
                    [],
                )
                .unwrap();
        }
        let active_switch_projection = crate::workflow::state(&manager_store).unwrap();
        assert!(active_switch_projection
            .continuation_actions
            .iter()
            .all(|action| {
                action.binding["session_id"] != "manager-session"
                    || !matches!(action.operation.as_str(), "role_resume" | "restart_resume")
            }));
        assert!(manager_store
            .reserve_role_resume(
                "manager-session",
                "active-switch-stale-resume",
                &stale_launch,
                "active-switch-stale-token",
            )
            .unwrap_err()
            .to_string()
            .contains("active exact-generation switch"));

        {
            let connection = manager_store.lock().unwrap();
            connection
                .execute_batch(
                    "UPDATE tasks SET attention='needs_recovery' WHERE id='manager-task';
                     UPDATE attempts SET status='needs_recovery' WHERE id='manager-attempt';
                     UPDATE claims SET state='unknown' WHERE id='manager-claim';
                     INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
                       VALUES('unknown-claim-continue','manager-attempt','continue','requested',3,'{}','2026-01-01T00:02:00Z','2026-01-01T00:02:00Z');",
                )
                .unwrap();
        }
        let generic_continue_paths =
            crate::config::InstancePaths::resolve(Some(root.join("manager-stop-generic-continue")))
                .unwrap();
        generic_continue_paths.create().unwrap();
        let generic_continue_app = crate::operations::Application::new(
            generic_continue_paths,
            manager_store.clone(),
            std::env::current_exe().unwrap(),
        )
        .unwrap();
        let generic_continue = generic_continue_app.coordinator_tick().unwrap();
        assert_eq!(generic_continue["action"], "control_rejected");
        assert!(generic_continue["reason"].as_str().unwrap().contains(
            "cannot normalize task or attempt while repository claim ownership is unknown"
        ));
        {
            let connection = manager_store.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT t.attention,a.status,claim.state,c.state
                         FROM tasks t JOIN attempts a ON a.task_id=t.id
                         JOIN claims claim ON claim.attempt_id=a.id
                         JOIN controls c ON c.id='unknown-claim-continue'
                         WHERE a.id='manager-attempt'",
                        [],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                            ))
                        },
                    )
                    .unwrap(),
                (
                    "needs_recovery".into(),
                    "needs_recovery".into(),
                    "unknown".into(),
                    "rejected".into(),
                )
            );
        }
    }

    #[test]
    fn current_process_executable_path_is_os_backed_and_fingerprintable() {
        let from_pid = executable_fingerprint(
            &process_executable_path(std::process::id()).expect("current process executable path"),
        )
        .expect("current process executable fingerprint");
        let from_runtime =
            executable_fingerprint(&std::env::current_exe().expect("current test executable path"))
                .expect("current test executable fingerprint");
        assert!(same_executable_image(&from_runtime, &from_pid));
    }

    #[test]
    fn cmux_authority_returns_only_its_current_absolute_executable() {
        let ancestry = current_process_cmux_ancestry_for_tests().unwrap();
        let executable = ancestry.verified_executable().unwrap();
        assert!(executable.is_absolute());
        assert!(same_executable_image(
            &executable_fingerprint(&executable).unwrap(),
            &executable_fingerprint(&std::env::current_exe().unwrap()).unwrap(),
        ));
    }
}
