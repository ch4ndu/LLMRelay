use super::Application;
use crate::providers;
use crate::store::json_hash;
use anyhow::{anyhow, bail, Context, Result};
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

fn conversation_toml(value: &serde_json::Value) -> Result<String> {
    Ok(match value {
        serde_json::Value::String(text) => providers::toml_string(text)?,
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(conversation_toml)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Ok(format!(
                    "{}={}",
                    providers::toml_string(key)?,
                    conversation_toml(value)?
                )))
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        serde_json::Value::Null => bail!("Trusted conversation configuration cannot contain null"),
    })
}

fn conversation_guidance(
    root: &Path,
    base_revision: &str,
    saved: &str,
) -> Result<(String, String)> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    #[derive(serde::Deserialize)]
    struct SavedConfiguration {
        schema: u64,
        base_revision: String,
        guidance: Vec<SavedGuidance>,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SavedGuidance {
        path: String,
        sha256: String,
        bytes: usize,
    }
    let configured: SavedConfiguration =
        serde_json::from_str(saved).context("Read approved conversation configuration")?;
    if configured.schema != 1 {
        bail!("Unsupported conversation execution configuration schema")
    }
    if configured.base_revision != base_revision {
        bail!("The project's registered commit changed; review and save the execution configuration again")
    }
    if configured.guidance.len() > 32 {
        bail!("An execution configuration lists at most 32 guidance files")
    }
    root.to_str()
        .context("Conversation repository must be a UTF-8 path")?;
    let mut guidance = Vec::new();
    let mut content = String::new();
    let mut paths = std::collections::BTreeSet::new();
    for ancestor in root.ancestors() {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            paths.insert(ancestor.join(name));
        }
    }
    let mut committed = std::collections::BTreeMap::new();
    for record in &configured.guidance {
        let relative = &record.path;
        if relative.is_empty()
            || relative.len() > 512
            || relative.trim() != relative
            || relative.starts_with('/')
            || relative.contains('\\')
            || relative.chars().any(char::is_control)
            || relative
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || relative.split('/').next() == Some(".git")
        {
            bail!("Approved conversation guidance must be a plain repository-relative path")
        }
        let path = root.join(relative);
        if committed.insert(path.clone(), record).is_some() {
            bail!("Approved conversation guidance lists {relative} twice")
        }
    }
    let repository = if committed.is_empty() {
        None
    } else {
        Some(crate::workspace::inspect(root)?)
    };
    // Ancestor instructions are native working-tree inputs; selected guidance
    // is separately bound to approved committed bytes, even at the same path.
    let sources = paths.into_iter().map(|path| (path, None)).chain(
        committed
            .into_iter()
            .map(|(path, record)| (path, Some(record))),
    );
    for (path, record) in sources {
        let bytes = if let Some(record) = record {
            let repository = repository
                .as_ref()
                .context("Resolve approved guidance repository")?;
            let regular = crate::workspace::tree_entry(repository, base_revision, &record.path)?
                .is_some_and(|entry| entry.starts_with("100644 ") || entry.starts_with("100755 "));
            if !regular || record.bytes > 256 * 1024 {
                bail!(
                    "Approved guidance {} is not a bounded committed regular file",
                    record.path
                )
            }
            let mut files = crate::workspace::committed_files_within(
                root,
                base_revision,
                &[record.path.as_str()],
                256 * 1024 + 1024,
            )?
            .context("Approved conversation guidance exceeds its size bound")?;
            let bytes = files
                .pop()
                .flatten()
                .context("Approved conversation guidance is missing")?;
            if bytes.len() != record.bytes
                || hex::encode(sha2::Sha256::digest(&bytes)) != record.sha256
            {
                bail!("Approved guidance {} changed; review and save the execution configuration again", record.path)
            }
            bytes
        } else {
            match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
            {
                Ok(file) => {
                    let metadata = file.metadata()?;
                    if !metadata.is_file() || metadata.len() > 256 * 1024 {
                        bail!(
                            "Conversation guidance is not a bounded regular file: {}",
                            path.display()
                        )
                    }
                    let mut bytes = Vec::new();
                    file.take(48 * 1024 + 1).read_to_end(&mut bytes)?;
                    bytes
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("Read conversation guidance identity"),
            }
        };
        if content.len() + bytes.len() > 48 * 1024 {
            bail!("Conversation guidance exceeds 48 KiB; shorten the selected guidance files")
        }
        let source_revision = record.map(|_| base_revision);
        content.push_str(&format!(
            "\nGuidance from {}{}:\n",
            path.display(),
            source_revision
                .map(|revision| format!(" at {revision}"))
                .unwrap_or_default()
        ));
        content
            .push_str(std::str::from_utf8(&bytes).context("Conversation guidance must be UTF-8")?);
        guidance.push(serde_json::json!({
            "path": path, "hash": hex::encode(sha2::Sha256::digest(bytes)),
            "source_revision": source_revision,
        }));
    }
    Ok((json_hash(&serde_json::json!(guidance))?, content))
}

impl Application {
    pub fn conversation_operation(
        &self,
        operation: crate::providers::codex_app_server::ConversationOperation,
    ) -> Result<serde_json::Value> {
        self.conversation_operation_inner(operation, false)
    }

    fn conversation_operation_inner(
        &self,
        operation: crate::providers::codex_app_server::ConversationOperation,
        provisional: bool,
    ) -> Result<serde_json::Value> {
        use crate::providers::codex_app_server::ConversationOperation as Op;
        operation.validate()?;
        match &operation {
            Op::WorkspaceIdentity { project_id } => {
                self.store
                    .require_conversation_workspace_project(project_id)?;
                return Ok(serde_json::to_value(
                    crate::providers::codex_app_server::ConversationWorkspaceIdentity {
                        project_id: project_id.clone(),
                        storage_scope: self.paths.root_key(),
                    },
                )?);
            }
            Op::TaskSources {
                project_id,
                task_id,
            } => {
                return Ok(serde_json::to_value(
                    self.store.task_sources(project_id, task_id)?,
                )?)
            }
            Op::List { .. } | Op::Profiles { .. } | Op::Admissions { .. } => {
                return self.store.conversation_discover(&operation)
            }
            Op::Detail {
                project_id,
                conversation_id,
                after,
            } => {
                return Ok(serde_json::to_value(self.store.conversation_detail(
                    project_id,
                    conversation_id,
                    *after,
                )?)?)
            }
            Op::Messages {
                project_id,
                conversation_id,
            } => {
                return Ok(serde_json::to_value(
                    self.store
                        .conversation_messages(project_id, conversation_id)?,
                )?)
            }
            Op::Receipt {
                project_id,
                operation_id,
                original_request,
                request_hash,
            } => {
                let _intake = self
                    .intake_lock
                    .lock()
                    .map_err(|_| anyhow!("Conversation intake is unavailable"))?;
                return Ok(serde_json::to_value(
                    self.store.conversation_lookup_receipt(
                        project_id,
                        operation_id,
                        original_request,
                        request_hash,
                    )?,
                )?);
            }
            _ => {}
        }
        if let Op::Inspect {
            conversation_id,
            after,
        } = &operation
        {
            return self.store.conversation_inspect(conversation_id, *after);
        }
        let _intake = self
            .intake_lock
            .lock()
            .map_err(|_| anyhow!("Conversation intake is unavailable"))?;
        let request = serde_json::to_value(&operation)?;
        let receipt_request = match &operation {
            Op::CreateProfile { input } => serde_json::to_value(input)?,
            _ => request.clone(),
        };
        // Every branch below reserves a durable receipt before any provider
        // delivery. Intake remains held through refusal recording, and the
        // store rechecks absence in an immediate transaction. An existing
        // reservation (including an unknown outcome) can never become refused.
        // Both conversation spawn paths also hold intake through handle registration,
        // so no new stdout reader can appear between a recovery check and its write.
        let result = (|| -> Result<serde_json::Value> {
            match operation {
            Op::CreateProfile { input } => self.store.conversation_create_profile(&input),
            Op::Prepare {
                operation_id,
                profile_revision_id,
            } => {
                if let Some(receipt) = self.store.conversation_receipt(&operation_id, &request)? {
                    return Ok(receipt);
                }
                let profile = self.store.conversation_profile(&profile_revision_id)?;
                let prepared = self.prepare_conversation(profile, provisional)?;
                self.store.conversation_prepare_admission(
                    &operation_id,
                    &request,
                    &prepared.identity,
                )
            }
            Op::Activate {
                operation_id,
                admission_id,
                evidence_hash,
            } => {
                if let Some(receipt) = self.store.conversation_receipt(&operation_id, &request)? {
                    return Ok(receipt);
                }
                let (identity, _) = self.store.conversation_admission(&admission_id)?;
                let current = self.prepare_conversation(identity.profile.clone(), provisional)?;
                let hash = json_hash(&serde_json::to_value(&current.identity)?)?;
                self.store.conversation_activate(
                    &operation_id,
                    &request,
                    &admission_id,
                    &hash,
                    &evidence_hash,
                )
            }
            Op::WithKnowledge { operation_id, operation, .. } => match *operation {
                Op::Start { admission_id, input, .. } => self.start_conversation(
                    &operation_id, &request, &admission_id, &input, false, provisional),
                Op::Submit { conversation_id, input, .. } => self.submit_conversation(
                    &operation_id, &request, &conversation_id, &input, None, provisional),
                Op::Steer { conversation_id, expected_turn_id, input, .. } => self.submit_conversation(
                    &operation_id, &request, &conversation_id, &input, Some(&expected_turn_id), provisional),
                _ => bail!("Select Start, Submit or Steer before attaching knowledge"),
            },
            Op::Start {
                operation_id,
                admission_id,
                input,
            } => self.start_conversation(
                &operation_id,
                &request,
                &admission_id,
                &input,
                false,
                provisional,
            ),
            Op::Probe {
                operation_id,
                admission_id,
            } => self.start_conversation(
                &operation_id,
                &request,
                &admission_id,
                "Reply with a brief description of this repository. Do not edit files, request elevated permissions, or use network tools.",
                true,
                provisional,
            ),
            Op::Submit {
                operation_id,
                conversation_id,
                input,
            } => self.submit_conversation(
                &operation_id,
                &request,
                &conversation_id,
                &input,
                None,
                provisional,
            ),
            Op::Steer {
                operation_id,
                conversation_id,
                expected_turn_id,
                input,
            } => self.submit_conversation(
                &operation_id,
                &request,
                &conversation_id,
                &input,
                Some(&expected_turn_id),
                provisional,
            ),
            Op::InterruptTurn {
                operation_id,
                conversation_id,
                expected_session_id,
                expected_generation,
                expected_turn_id,
            } => {
                let (result, fresh) = self.store.conversation_reserve_interrupt_turn(
                    &operation_id, &request, &conversation_id, &expected_session_id,
                    &expected_generation, &expected_turn_id,
                )?;
                if fresh && self.supervisor.interrupt_conversation(
                    &expected_session_id, &conversation_id, &expected_generation, &operation_id,
                ).is_err() {
                    self.store.conversation_hold_generation(
                        &conversation_id, &expected_generation,
                        "Turn interrupt delivery is unknown. Refresh and inspect its outcome before sending another message; it will not be replayed.",
                    )?;
                }
                Ok(self.store.conversation_receipt(&operation_id, &request)?.unwrap_or(result))
            }
            Op::DisconnectIdle {
                operation_id,
                conversation_id,
                expected_session_id,
                expected_generation,
            } => {
                if let Some(receipt) = self.store.conversation_receipt(&operation_id, &request)? {
                    return Ok(receipt);
                }
                self.supervisor.disconnect_idle_conversation(
                    &operation_id, &request, &conversation_id, &expected_session_id,
                    &expected_generation,
                )
            }
            Op::Interrupt {
                operation_id,
                conversation_id,
            } => {
                let (result, fresh) = self.store.conversation_interrupt_receipt(
                    &operation_id,
                    &request,
                    &conversation_id,
                )?;
                if fresh {
                    let (session, generation) = self.store.conversation_session(&conversation_id)?;
                    let delivery = self.supervisor.interrupt_conversation(
                        &session,
                        &conversation_id,
                        &generation,
                        &operation_id,
                    );
                    // An active native turn must report its terminal outcome before
                    // process cleanup. Idle transports have no turn to interrupt.
                    let stopped = match delivery {
                        Ok(true) => Ok(()),
                        Ok(false) => self.supervisor.interrupt_once(&session).map(|_| ()),
                        Err(error) => Err(error),
                    };
                    if stopped.is_err() {
                        self.store.conversation_hold(
                            &conversation_id,
                            "Stop delivery is unknown. Ownership remains held until exact process quiescence is observed.",
                        )?;
                    }
                }
                Ok(result)
            }
            Op::Deny {
                operation_id,
                conversation_id,
                generation,
                thread_id,
                turn_id,
                request_id,
            } => self.store.conversation_acknowledge_denial(
                &operation_id,
                &request,
                &conversation_id,
                &generation,
                &thread_id,
                &turn_id,
                &request_id,
            ),
            Op::Reconcile {
                operation_id,
                conversation_id,
            } => {
                if let Some(receipt) = self.store.conversation_receipt(&operation_id, &request)? {
                    return Ok(receipt);
                }
                self.supervisor.require_conversation_reader_settled(&conversation_id)?;
                self.store.conversation_reconcile_quiescence(
                    &operation_id,
                    &request,
                    &conversation_id,
                )
            }
            Op::Resume {
                operation_id,
                conversation_id,
            } => {
                if let Some(receipt) = self.store.conversation_receipt(&operation_id, &request)? {
                    return Ok(receipt);
                }
                self.supervisor.require_conversation_reader_settled(&conversation_id)?;
                self.require_conversation_execution()?;
                let identity = self.store.conversation_identity(&conversation_id)?;
                let prepared = self.prepare_conversation(identity.profile.clone(), provisional)?;
                if json_hash(&serde_json::to_value(&identity)?)?
                    != json_hash(&serde_json::to_value(&prepared.identity)?)?
                {
                    bail!("Conversation identity changed. Create a new conversation under fresh admission.")
                }
                let (result, thread) = self.store.conversation_reserve_resume(
                    &operation_id,
                    &request,
                    &conversation_id,
                    &identity,
                )?;
                let session = result["session_id"]
                    .as_str()
                    .context("Reserved conversation session is missing")?;
                let generation = result["generation"]
                    .as_str()
                    .context("Reserved conversation generation is missing")?;
                if let Err(error) = self.supervisor.spawn_conversation(
                    session,
                    &conversation_id,
                    generation,
                    &operation_id,
                    prepared,
                    &self.executable,
                    Some(thread),
                ) {
                    self.store.conversation_hold(
                        &conversation_id,
                        "Conversation resume outcome is unknown; it will not be retried automatically.",
                    )?;
                    return Err(error);
                }
                Ok(result)
            }
            Op::WorkspaceIdentity { .. } | Op::Inspect { .. } | Op::List { .. } | Op::Profiles { .. } | Op::Admissions { .. }
            | Op::Detail { .. } | Op::Messages { .. } | Op::TaskSources { .. } | Op::Receipt { .. } =>
                bail!("Conversation inspection routing changed unexpectedly"),
        }
        })();
        if let Err(error) = &result {
            if let Err(receipt_error) = self
                .store
                .conversation_record_pre_delivery_refusal(&receipt_request, &error.to_string())
            {
                tracing::warn!(error = %receipt_error, "Conversation refusal receipt unavailable; original command error retained");
            }
        }
        result
    }

    fn require_conversation_execution(&self) -> Result<()> {
        self.store
            .require_execution_unheld("conversation execution")?;
        if !self.dispatch_enabled()
            || self.draining.load(Ordering::SeqCst)
            || self.synthetic_dispatch_for_tests
            || self.store.compatibility_bundles.is_synthetic()
        {
            bail!("Conversation execution is unavailable while dispatch is stopped, draining, or synthetic.")
        }
        Ok(())
    }

    fn prepare_conversation(
        &self,
        profile: crate::providers::codex_app_server::ConversationProfileRevision,
        provisional: bool,
    ) -> Result<crate::providers::codex_app_server::PreparedConversation> {
        use crate::providers::codex_app_server as adapter;
        if self.synthetic_dispatch_for_tests || self.store.compatibility_bundles.is_synthetic() {
            bail!("Synthetic contexts cannot prepare native conversation execution")
        }
        let installation = providers::inspect_installation(crate::domain::Provider::Codex)?;
        let release = if provisional {
            #[cfg(test)]
            {
                crate::provider_compatibility::provisional_conversation_release(
                    &installation.executable_version,
                )?
            }
            #[cfg(not(test))]
            {
                bail!("Provisional conversation qualification is unavailable in production")
            }
        } else {
            crate::provider_compatibility::conversation_release(&installation.executable_version)?
        };
        let (path, repository_identity, config_hash, epoch, configuration, base_revision): (
            String,
            String,
            String,
            String,
            String,
            String,
        ) = {
            let connection = self.store.lock()?;
            connection
                .query_row(
                    "SELECT p.repository_path,p.repository_identity,e.content_hash,
                         r.restore_epoch,e.config_json,p.base_revision FROM projects p
                     JOIN execution_config_revisions e ON e.project_id=p.id
                     CROSS JOIN conversation_runtime r
                     WHERE p.id=?1 AND e.id=?2 AND r.singleton=1
                     AND e.revision=(SELECT MAX(revision) FROM execution_config_revisions
                         WHERE project_id=p.id)",
                    rusqlite::params![profile.project_id, profile.execution_config_revision_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .context("The project configuration changed or is unavailable. Save a new conversation profile and prepare its admission.")?
        };
        let repository = crate::workspace::inspect(Path::new(&path))?;
        if repository.identity != repository_identity {
            bail!("The conversation repository has moved; register and prepare it again")
        }
        let (disabled_mcp, preflight) = providers::codex::inspect_conversation_preflight(
            &repository.root,
            &installation.executable_version,
        )?;
        let sockets = self
            .paths
            .socket_dir
            .canonicalize()
            .context("Resolve protected conversation socket directory")?;
        let mut denied = vec![
            self.paths.root.clone(),
            sockets.join("control.sock"),
            sockets.join("role.sock"),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            denied.extend([
                home.join(".aws"),
                home.join(".ssh"),
                home.join(".codex/auth.json"),
            ]);
        }
        let mut config = adapter::trusted_config(&repository.root, &denied, &disabled_mcp)?;
        config.insert(
            "model_reasoning_effort".into(),
            serde_json::json!(profile.effort),
        );
        let (guidance_hash, guidance) =
            conversation_guidance(&repository.root, &base_revision, &configuration)?;
        let policy_hash = json_hash(&serde_json::to_value(&config)?)?;
        let identity = adapter::ConversationNativeIdentity {
            executable: installation.executable,
            executable_version: installation.executable_version,
            schema_hash: adapter::SCHEMA_HASH.into(),
            adapter_revision: adapter::ADAPTER_REVISION.into(),
            adapter_hash: json_hash(&serde_json::json!({
                "protocol_source": hex::encode(sha2::Sha256::digest(include_bytes!(
                    "../providers/codex_app_server.rs"
                ))),
                "host_executable": serde_json::to_value(providers::executable_fingerprint(
                    &self.executable
                )?)?
            }))?,
            profile,
            repository_identity,
            repository_root: repository.root,
            guidance_hash,
            execution_config_hash: config_hash,
            policy_hash,
            credential_contract: providers::codex::CREDENTIAL_CONTRACT_REVISION.into(),
            configuration_hash: json_hash(&preflight)?,
            release_binding: release,
            instance_id: self.paths.root_key(),
            restore_epoch: epoch,
        };
        let mut arguments = vec![
            "app-server".into(),
            "--listen".into(),
            "stdio://".into(),
            "--strict-config".into(),
        ];
        // Only app-owned keys reach CLI or thread config. JSON objects are
        // converted to TOML values here, never supplied by an HTTP caller.
        for (key, value) in &config {
            arguments.extend([
                "--config".into(),
                format!("{key}={}", conversation_toml(value)?),
            ]);
        }
        Ok(adapter::PreparedConversation {
            identity,
            arguments,
            config,
            guidance,
        })
    }

    fn start_conversation(
        &self,
        operation: &str,
        request: &serde_json::Value,
        admission: &str,
        input: &str,
        probe: bool,
        provisional: bool,
    ) -> Result<serde_json::Value> {
        if let Some(receipt) = self.store.conversation_receipt(operation, request)? {
            return Ok(receipt);
        }
        self.require_conversation_execution()?;
        let (identity, _) = self.store.conversation_admission(admission)?;
        let prepared = self.prepare_conversation(identity.profile.clone(), provisional)?;
        if json_hash(&serde_json::to_value(&identity)?)?
            != json_hash(&serde_json::to_value(&prepared.identity)?)?
        {
            bail!("Conversation admission is stale; prepare the exact profile again")
        }
        let (result, fresh) = self
            .store
            .conversation_reserve_start(operation, request, admission, &identity, input, probe)?;
        if fresh {
            let conversation = result["conversation_id"]
                .as_str()
                .context("Reserved conversation ID missing")?;
            let session = result["session_id"]
                .as_str()
                .context("Reserved session ID missing")?;
            let generation = result["generation"]
                .as_str()
                .context("Reserved generation missing")?;
            if let Err(error) = self.supervisor.spawn_conversation(
                session,
                conversation,
                generation,
                operation,
                prepared,
                &self.executable,
                None,
            ) {
                self.store.conversation_hold(
                    conversation,
                    "Conversation launch is unresolved; inspect it before creating new work.",
                )?;
                return Err(error);
            }
        }
        Ok(result)
    }

    fn submit_conversation(
        &self,
        operation: &str,
        request: &serde_json::Value,
        id: &str,
        input: &str,
        steer: Option<&str>,
        provisional: bool,
    ) -> Result<serde_json::Value> {
        if let Some(receipt) = self.store.conversation_receipt(operation, request)? {
            return Ok(receipt);
        }
        self.require_conversation_execution()?;
        let identity = self.store.conversation_identity(id)?;
        let current = self.prepare_conversation(identity.profile.clone(), provisional)?;
        if json_hash(&serde_json::to_value(&identity)?)?
            != json_hash(&serde_json::to_value(&current.identity)?)?
        {
            bail!("Conversation configuration changed; start a new conversation under fresh admission")
        }
        let (session, generation) = self.store.conversation_session(id)?;
        let (result, fresh) = self
            .store
            .conversation_reserve_input(operation, request, id, input, steer)?;
        if fresh {
            self.supervisor.send_conversation(
                &session,
                id,
                &generation,
                operation,
                &identity.profile.effort,
            )?;
        }
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) fn qualification_conversation_operation(
        &self,
        operation: crate::providers::codex_app_server::ConversationOperation,
    ) -> Result<serde_json::Value> {
        self.conversation_operation_inner(operation, true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::git;
    use super::conversation_guidance;
    use crate::domain::HumanCommand;
    use crate::store::Store;
    use anyhow::{Context, Result};
    use std::path::Path;

    #[test]
    fn conversation_guidance_consumes_saved_configuration_and_committed_identity() -> Result<()> {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let configured_identity = |key: &str| -> Result<String> {
            let value = git(project_root, &["config", "--get", key])
                .with_context(|| format!("guidance fixture requires configured Git {key}"))?;
            anyhow::ensure!(
                !value.is_empty(),
                "guidance fixture requires nonempty Git {key}"
            );
            Ok(value)
        };
        let name = configured_identity("user.name")?;
        let email = configured_identity("user.email")?;
        let root = project_root
            .join(".local")
            .join(format!("llmrelay-guidance-{}", uuid::Uuid::new_v4()));
        let repository = root.join("repository");
        std::fs::create_dir_all(&repository)?;
        let repository = repository.canonicalize()?;
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", &name],
            vec!["config", "user.email", &email],
            vec!["config", "commit.gpgsign", "false"],
            vec!["config", "core.hooksPath", "/dev/null"],
        ] {
            git(&repository, &args)?;
        }
        std::fs::write(repository.join("GUIDE.md"), "approved selected guidance\n")?;
        std::fs::write(repository.join("AGENTS.md"), "approved ancestor guidance\n")?;
        git(&repository, &["add", "."])?;
        let check_message = |message: &str| {
            let normalized = message.to_ascii_lowercase();
            for prohibited in [
                "co-authored-by",
                "generated-by",
                "generated with",
                "claude-session",
                "codex",
                "openai",
                "claude",
                "anthropic",
            ] {
                assert!(
                    !normalized.contains(prohibited),
                    "fixture commit contains prohibited attribution: {message}"
                );
            }
        };
        let message = "guidance fixture";
        check_message(message);
        git(&repository, &["commit", "-qm", message])?;
        let committed_message = git(&repository, &["log", "-1", "--format=%B"])?;
        check_message(&committed_message);
        assert_eq!(committed_message, message);
        let store = Store::open(&root.join("state.sqlite3"))?;
        let project = crate::workflow::execute(
            &store,
            &HumanCommand::AddProject {
                operation_id: "guidance-project".into(),
                path: repository.clone(),
                display_name: "Guidance".into(),
            },
        )?
        .entity_id;
        let base = crate::workspace::head(&repository)?;
        let mut empty_hash = String::new();
        for (revision, paths) in [vec![], vec!["GUIDE.md", "AGENTS.md"]]
            .into_iter()
            .enumerate()
        {
            let command = serde_json::from_value(serde_json::json!({
                "kind":"save_execution_configuration", "operation_id":format!("guidance-{revision}"),
                "project_id":project, "expected_revision":revision,
                "configuration":{"guidance_paths":paths,"testing_coverage":"minimal",
                    "documentation_policy":"none","checks":[]}
            }))?;
            let saved = crate::workflow::execute_workflow_command(&store, &command)?;
            let json: String = store.lock()?.query_row(
                "SELECT config_json FROM execution_config_revisions WHERE id=?1",
                [&saved.entity_id],
                |row| row.get(0),
            )?;
            let (hash, text) = conversation_guidance(&repository, &base, &json)?;
            if revision == 0 {
                assert!(!text.contains("approved selected guidance"));
                empty_hash = hash;
                continue;
            }
            assert_ne!(hash, empty_hash);
            assert!(text.contains("approved selected guidance"));
            assert!(text.contains(&format!(" at {base}:")));
            std::fs::write(
                repository.join("GUIDE.md"),
                "unapproved working-tree guidance",
            )?;
            assert_eq!(
                conversation_guidance(&repository, &base, &json)?,
                (hash.clone(), text)
            );
            std::fs::write(
                repository.join("AGENTS.md"),
                "changed native ancestor guidance",
            )?;
            let (changed_hash, changed_text) = conversation_guidance(&repository, &base, &json)?;
            assert_ne!(changed_hash, hash);
            assert!(changed_text.contains("approved ancestor guidance"));
            assert!(changed_text.contains("changed native ancestor guidance"));
            let config: serde_json::Value = serde_json::from_str(&json)?;
            for (pointer, value) in [
                ("/schema", serde_json::json!(2)),
                ("/guidance", serde_json::Value::Null),
                ("/guidance/0/sha256", serde_json::json!("0".repeat(64))),
                ("/guidance/0/bytes", serde_json::json!(1)),
                ("/guidance/0/path", serde_json::json!("missing.md")),
                ("/guidance/0/path", serde_json::json!("../outside.md")),
            ] {
                let mut malformed = config.clone();
                *malformed.pointer_mut(pointer).unwrap() = value;
                assert!(conversation_guidance(&repository, &base, &malformed.to_string()).is_err());
            }
            let stale = conversation_guidance(&repository, "changed-base", &json).unwrap_err();
            assert!(stale.to_string().contains("registered commit changed"));
        }
        drop(store);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
