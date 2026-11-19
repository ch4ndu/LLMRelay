//! Opt-in native evidence for conversation and writer contracts.
//! Native cases are ignored, so ordinary `cargo test` never contacts a provider.
//! The local control regression runs without contacting a provider.
//! Its evidence is candidate input for independent review and promotes
//! nothing. Run one provider per new directory:
//!
//! ```text
//! LLMRELAY_NATIVE_QUALIFICATION_DIR=<absolute path that does not exist yet>
//! LLMRELAY_NATIVE_QUALIFICATION_EXECUTABLE=<absolute path of the llmrelay binary built from this source>
//! LLMRELAY_NATIVE_QUALIFICATION_PROVIDER=codex|claude
//! LLMRELAY_NATIVE_QUALIFICATION_MODEL=<model>
//! LLMRELAY_NATIVE_QUALIFICATION_EFFORT=<effort>
//! cargo test --lib providers::native_qualification::writer_pair -- --ignored --exact
//! ```
//!
//! The provider uses the person's existing sign-in and configuration in
//! place; nothing here changes or copies them.

use crate::config::InstancePaths;
use crate::domain::{Provider, SessionOwner};
use crate::operations::Application;
use crate::provider_compatibility::{BundleSet, PolicyQualification};
use crate::supervisor::{SpawnFailure, Supervisor};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const POLL: Duration = Duration::from_millis(500);
const STOP_SETTLE: Duration = Duration::from_secs(30);
const CONTROL_HANDSHAKE_LIMIT: Duration = Duration::from_secs(10);
const WRITER_LIMIT: Duration = Duration::from_secs(600);
const WRITER_WORK: Duration = Duration::from_secs(480);
const WRITER_CLEANUP: Duration = Duration::from_secs(240);
const FIRST_TURN_GATE: Duration = Duration::from_secs(120);
const WRITER_STARTUP: Duration = Duration::from_secs(600);
const ATTEMPT: &str = "diagnostic-writer-attempt";

fn stage_wait(deadline: Instant, now: Instant, limit: Duration) -> Duration {
    limit.min(deadline.saturating_duration_since(now))
}

struct WriterTiming {
    started: Instant,
    execution_deadline: Option<Instant>,
}

impl WriterTiming {
    fn deadline(&self) -> Instant {
        self.execution_deadline
            .unwrap_or(self.started + WRITER_STARTUP)
    }

    fn check(&self, now: Instant) -> Result<()> {
        if now >= self.deadline() {
            bail!(
                "Writer {} allowance expired; no resume or retry",
                if self.execution_deadline.is_some() {
                    "execution"
                } else {
                    "startup"
                }
            )
        }
        Ok(())
    }

    fn start_execution(&mut self, now: Instant) -> Result<bool> {
        self.check(now)?;
        if self.execution_deadline.is_some() {
            return Ok(false);
        }
        self.execution_deadline = Some(now + WRITER_WORK);
        Ok(true)
    }

    fn overall(&self) -> Instant {
        self.started + WRITER_STARTUP + WRITER_WORK + WRITER_CLEANUP
    }

    fn cleanup_deadline(&self, now: Instant) -> Instant {
        self.overall().min(now + WRITER_CLEANUP)
    }
}

fn writer_milestone(evidence: &mut Value, name: &str, started: Instant) {
    evidence["timing_milestones"][name] = json!({
        "elapsed_seconds":started.elapsed().as_secs_f64(),
        "observed_at":chrono::Utc::now().to_rfc3339()});
}

#[derive(Clone)]
struct Inputs {
    dir: PathBuf,
    executable: PathBuf,
    provider: Provider,
    model: String,
    effort: String,
    purpose: crate::domain::ProfilePurpose,
    exact_version: Option<String>,
    conversation: bool,
}

fn input(name: &str) -> Result<String> {
    let key = format!("LLMRELAY_NATIVE_QUALIFICATION_{name}");
    std::env::var(&key).with_context(|| format!("set {key}"))
}

fn inputs() -> Result<Inputs> {
    let dir = PathBuf::from(input("DIR")?);
    let executable = PathBuf::from(input("EXECUTABLE")?);
    if !dir.is_absolute() || !executable.is_absolute() {
        bail!("the diagnostic directory and executable must be absolute paths")
    }
    Ok(Inputs {
        dir,
        executable: executable
            .canonicalize()
            .context("resolve the LLMRelay executable")?,
        provider: input("PROVIDER")?
            .parse()
            .map_err(|error: String| anyhow!(error))?,
        model: input("MODEL")?,
        effort: input("EFFORT")?,
        purpose: std::env::var("LLMRELAY_NATIVE_QUALIFICATION_PURPOSE")
            .unwrap_or_else(|_| "workspace_writer".into())
            .parse()
            .map_err(|error: String| anyhow!(error))?,
        exact_version: std::env::var("LLMRELAY_NATIVE_QUALIFICATION_EXACT_VERSION").ok(),
        conversation: false,
    })
}

fn current_claude_writer(
    provider: Provider,
    version: Option<&str>,
    model: &str,
    effort: &str,
) -> bool {
    provider == Provider::Claude
        && version == Some("2.1.325 (Claude Code)")
        && model == "claude-opus-5-5"
        && effort == "high"
}

fn writer_tuple(inputs: &Inputs, main: bool) -> Result<()> {
    if !inputs.purpose.is_writer() {
        bail!("Reader and reviewer acquisition has been retired")
    }
    if inputs.exact_version.is_none() {
        return Ok(()); // Existing embedded legacy writer fixture.
    }
    let tuple = (
        inputs.exact_version.as_deref(),
        inputs.model.as_str(),
        inputs.effort.as_str(),
    );
    if !main
        && current_claude_writer(
            inputs.provider,
            inputs.exact_version.as_deref(),
            &inputs.model,
            &inputs.effort,
        )
    {
        return Ok(());
    }
    if inputs.provider != Provider::Codex
        || main
        || !matches!(
            tuple,
            (Some("codex-cli 0.167.0"), "gpt-6.1-sol", "medium")
                | (Some("codex-cli 0.166.0"), "gpt-6-astra", "xhigh")
        )
    {
        bail!("Provisional acquisition requires an explicit finite writer tuple and isolated worktree; no fallback")
    }
    Ok(())
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

// Reuse the disposable setup, never its role identity or release eligibility.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "native conversation qualification; explicit model/effort and finite manager authorization required"]
async fn app_server_conversation() {
    let started = Instant::now();
    let mut inputs = inputs().expect("explicit native qualification inputs");
    inputs.conversation = true;
    assert_eq!(inputs.provider, Provider::Codex);
    assert_eq!(inputs.model, "gpt-6.1-sol");
    assert_eq!(inputs.effort, "medium");
    let version = inputs
        .exact_version
        .as_deref()
        .expect("exact conversation version");
    assert_eq!(
        super::inspect_installation(inputs.provider)
            .expect("installed provider version")
            .executable_version,
        version
    );
    assert!(super::codex_app_server::schema_for_version(version).is_some());
    assert!(
        crate::provider_compatibility::conversation_release(version, &inputs.model, &inputs.effort)
            .is_err(),
        "native acquisition requires an unqualified release candidate"
    );
    std::fs::create_dir(&inputs.dir).expect("qualification directory must be new");
    inputs.dir = inputs
        .dir
        .canonicalize()
        .expect("canonical qualification directory");
    let overall = started + Duration::from_secs(1800);
    let mut evidence = json!({
        "kind":"app_server_conversation_candidate", "requested_model":inputs.model,
        "requested_effort":inputs.effort, "platform":std::env::consts::OS,
        "release_eligibility":"unqualified", "residual_accepted":false,
        "model_tool_denial_observed":false,
        "evidence_basis":"app_server_standalone_enforcement_plus_model_lifecycle_v1",
        "policy_equivalence":"Same trusted startup inputs/shared compiler; additional model-tool permission merging and execution were not observed. Policy hashes are input bindings, not compiled-policy attestation.",
        "ceilings":{"fresh_conversations":3,"model_submissions":7,"resumes":1,
            "diagnostic_rpcs":3,"acquisition_seconds":1500,"cleanup_seconds":180,
            "setup_output_seconds":120,"cumulative_seconds":1800,"per_conversation_seconds":600,"automatic_retry":false},
        "requested_version":version
    });
    evidence["test_artifact"] = json!(super::executable_fingerprint(
        &std::env::current_exe().expect("current precompiled test artifact"),
    )
    .expect("fingerprint precompiled test artifact"));
    evidence["package_version"] = json!(env!("CARGO_PKG_VERSION"));
    evidence["helper_source_sha256"] = json!(sha256(SYSCALL_HELPER));
    let mut owner = ConversationOwner {
        app: None,
        guard: None,
        listener: None,
    };
    let setup = tokio::time::timeout(
        Duration::from_secs(60),
        writer_setup(&inputs, &mut evidence, false),
    )
    .await;
    let mut allowance = ConversationAllowance {
        overall: overall - Duration::from_secs(240),
        deadline: overall,
        counts: [0; 3],
    };
    let outcome = match setup {
        Ok(Ok((app, targets))) => {
            owner.app = Some(app);
            let limit = allowance
                .overall
                .min(Instant::now() + Duration::from_secs(1500));
            allowance.overall = limit;
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(limit),
                acquire_conversations(&inputs, &mut owner, &targets, &mut evidence, &mut allowance),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow!("Acquisition deadline reached; no retry")),
            }
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err(anyhow!("Setup deadline reached; no acquisition")),
    };
    evidence["acquisition_error"] = json!(outcome.as_ref().err().map(|e| format!("{e:#}")));
    let cleanup_deadline = overall.min(Instant::now() + Duration::from_secs(180));
    evidence["diagnostic_quiescence_unknown"] = json!(true);
    let cleanup = tokio::time::timeout_at(
        tokio::time::Instant::from_std(cleanup_deadline),
        owner.cleanup(cleanup_deadline, &mut evidence),
    )
    .await;
    evidence["outer_cleanup"] = json!({"verified":matches!(&cleanup, Ok(Ok(()))),
        "outcome":format!("{cleanup:?}")});
    evidence["spent"] = json!({"fresh_conversations":allowance.counts[0],
        "model_submissions":allowance.counts[1],"resumes":allowance.counts[2],
        "diagnostic_rpcs":evidence["diagnostic_rpcs"].as_u64().unwrap_or(0),
        "elapsed_seconds":started.elapsed().as_secs_f64()});
    evidence["cleanup_verified"] = json!(matches!(&cleanup, Ok(Ok(()))));
    let succeeded = outcome.is_ok() && matches!(&cleanup, Ok(Ok(()))) && Instant::now() < overall;
    let held = save_evidence(&inputs, "app-server-conversation", &mut evidence, succeeded)
        .expect("persist original acquisition and cleanup outcomes before assertions");
    assert!(
        held,
        "native qualification incomplete; inspect saved candidate evidence"
    );
}

struct ConversationOwner {
    app: Option<Application>,
    guard: Option<SpawnedSession>,
    listener: Option<tokio::net::TcpListener>,
}

impl ConversationOwner {
    async fn cleanup(&mut self, deadline: Instant, evidence: &mut Value) -> Result<()> {
        // Retain the listener across acquisition cancellation; closing is synchronous.
        self.listener.take();
        evidence["listener_closed"] = json!(true);
        if let Some(app) = &self.app {
            let until = deadline.min(Instant::now() + Duration::from_secs(30));
            while !app.supervisor.diagnostic_pending()?.is_empty() && Instant::now() < until {
                tokio::time::sleep_until(tokio::time::Instant::from_std(
                    until.min(Instant::now() + POLL),
                ))
                .await;
            }
            let pending = app.supervisor.diagnostic_pending()?;
            evidence["pending_diagnostics"] = json!(pending);
            evidence["diagnostic_quiescence_unknown"] = json!(!pending.is_empty());
            if !pending.is_empty() {
                evidence["diagnostic_recovery_boundary"] = json!(
                    "No independently recorded diagnostic descendant identity/exit receipt; do not guess or signal a PID. Retain RPC/session/generation and server cleanup evidence.");
            }
            // A failed database lookup must not orphan the already attached
            // process. This disposable Application owns only this fixture.
            let attached = app.supervisor.attached_session_ids()?;
            if let [session] = attached.as_slice() {
                if self
                    .guard
                    .as_ref()
                    .is_none_or(|guard| guard.session != *session)
                {
                    self.guard = Some(SpawnedSession {
                        supervisor: app.supervisor.clone(),
                        session: session.clone(),
                        armed: true,
                    });
                }
            }
            if let Some(guard) = &mut self.guard {
                conversation_cleanup(app, guard, deadline, evidence).await?;
            }
            if !pending.is_empty() {
                bail!("Diagnostic quiescence unknown: retain exact RPC recovery record; server-group absence is insufficient")
            }
        } else {
            evidence["diagnostic_quiescence_unknown"] = json!(false);
        }
        Ok(())
    }
}

struct ConversationAllowance {
    overall: Instant,
    deadline: Instant,
    counts: [usize; 3],
}

impl ConversationAllowance {
    fn dispatch_spawn(
        &mut self,
        app: &Application,
        operation: super::codex_app_server::ConversationOperation,
        evidence: &mut Value,
        owned: &mut Option<SpawnedSession>,
    ) -> Result<Value> {
        use super::codex_app_server::ConversationOperation as Op;
        let operation_id = match &operation {
            Op::Start { operation_id, .. }
            | Op::Probe { operation_id, .. }
            | Op::Resume { operation_id, .. } => operation_id.clone(),
            _ => bail!("Expected conversation spawn operation"),
        };
        let result = self.dispatch(app, operation);
        // Synchronous dispatch has reserved its generation before returning, even
        // on ambiguous delivery. Transfer it to the outer owner before any await.
        if let Err(error) = &result {
            evidence["dispatch_errors"][&operation_id] = json!(format!("{error:#}"));
        }
        let session = if let Ok(receipt) = &result {
            Some(
                receipt["session_id"]
                    .as_str()
                    .context("spawn receipt session")?
                    .to_owned(),
            )
        } else {
            app.store
                .lock()?
                .query_row(
                    "SELECT s.id FROM conversation_operations o JOIN sessions s
                 ON s.conversation_id=o.conversation_id AND s.transcript_epoch=o.generation
                 WHERE o.operation_id=?1",
                    [&operation_id],
                    |row| row.get(0),
                )
                .optional()?
        };
        if let Some(session) = session {
            *owned = Some(SpawnedSession {
                supervisor: app.supervisor.clone(),
                session,
                armed: true,
            });
        }
        result
    }

    fn dispatch(
        &mut self,
        app: &Application,
        operation: super::codex_app_server::ConversationOperation,
    ) -> Result<Value> {
        use super::codex_app_server::ConversationOperation as Op;
        let fresh = matches!(&operation, Op::Probe { .. } | Op::Start { .. });
        let resume = matches!(&operation, Op::Resume { .. });
        let submission =
            fresh || resume || matches!(&operation, Op::Submit { .. } | Op::Steer { .. });
        if fresh {
            self.deadline = self.overall.min(Instant::now() + WRITER_LIMIT);
        }
        if Instant::now() >= self.overall || (self.counts[0] > 0 && Instant::now() >= self.deadline)
        {
            bail!("Native conversation deadline reached before dispatch; no retry")
        }
        for (index, (charge, limit)) in [(fresh, 3), (submission, 7), (resume, 1)]
            .into_iter()
            .enumerate()
        {
            if charge && self.counts[index] >= limit {
                bail!("Native conversation allowance exhausted; no retry")
            }
        }
        // Spend even if delivery is rejected or ambiguous; resume never resets the deadline.
        self.counts[0] += usize::from(fresh);
        self.counts[1] += usize::from(submission);
        self.counts[2] += usize::from(resume);
        app.qualification_conversation_operation(operation)
    }
}

async fn conversation_wait(
    app: &Application,
    id: &str,
    deadline: Instant,
    ready: impl Fn(&Value) -> bool,
) -> Result<Value> {
    loop {
        if Instant::now() >= deadline {
            bail!("Per-conversation native deadline reached")
        }
        app.supervisor.reconcile()?;
        let mut state = app.store.conversation_inspect(id, 0)?;
        let mut events = Vec::new();
        let mut page = state.clone();
        for _ in 0..=super::codex_app_server::MAX_EVENTS / 100 {
            let chunk = page["events"].as_array().context("native event page")?;
            events.extend(chunk.iter().cloned());
            if chunk.len() < 100 {
                break;
            }
            let after = chunk.last().context("event cursor")?["sequence"]
                .as_i64()
                .context("event sequence")?;
            page = app.store.conversation_inspect(id, after)?;
        }
        if events.len() as i64 > super::codex_app_server::MAX_EVENTS {
            bail!("Native event evidence exceeds the projection bound")
        }
        state["events"] = json!(events);
        if ready(&state) {
            return Ok(state);
        }
        if state["state"] == "recovery_required" || state["state"] == "history_only" {
            bail!("Native conversation uncertain; no retry")
        }
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline.min(Instant::now() + POLL),
        ))
        .await;
    }
}

async fn conversation_cleanup(
    app: &Application,
    guard: &mut SpawnedSession,
    deadline: Instant,
    evidence: &mut Value,
) -> Result<()> {
    let session = &guard.session;
    if evidence["cleanup_history"][session].is_null() {
        evidence["cleanup_history"][session] = json!([]);
    }
    let history = evidence["cleanup_history"][session]
        .as_array_mut()
        .context("Conversation cleanup history must be an array")?;
    let attempt = history.len();
    history.push(json!({"quiescence":"unknown","awaited_cleanup_started":true}));
    let mut stopped_by = "already_exited_before_cleanup";
    let mut last_state = Value::Null;
    let cleanup = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        async {
            let binding = || -> Result<(String, Option<String>)> {
                Ok(app.store.lock()?.query_row(
                    "SELECT transcript_epoch,process_identity_json FROM sessions WHERE id=?1",
                    params![session],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            };
            let generation = binding()?;
            let mut stop_deadline = None;
            let mut forced = false;
            loop {
                app.supervisor.reconcile()?;
                if binding()? != generation {
                    bail!("Conversation cleanup generation changed; quiescence unknown")
                }
                let (status, exit, last_sequence) = session_state(app, session)?;
                last_state = json!({
                    "status":status,"exit":exit,"transcript_last_sequence":last_sequence
                });
                if status == "exited" {
                    let observation = crate::recovery::verify_session_quiescent(&app.store, session)
                        .context("verify exact conversation cleanup quiescence")?;
                    return Ok(json!({
                        "session":{"status":status,"exit":exit,"transcript_last_sequence":last_sequence},
                        "quiescence":{"verified":true,"observation":observation}
                    }));
                }
                let root_live = match app.supervisor.process_snapshot()? {
                    crate::supervisor::ProcessSnapshot::Read(live) => {
                        live.iter().any(|root| root.session_id == *session)
                    }
                    crate::supervisor::ProcessSnapshot::Unavailable => {
                        bail!("Conversation cleanup process inventory unavailable; quiescence unknown")
                    }
                };
                // Recovery is a hold, not an exit. Reconcile keeps the attached
                // child receipt and persists its exit after recorded descendants settle.
                if !root_live && stopped_by == "already_exited_before_cleanup" {
                    stopped_by = "awaited_recorded_descendants";
                }
                if status != "recovery_required" && root_live {
                    if stop_deadline.is_none() {
                        app.supervisor
                            .interrupt_once(session)
                            .context("interrupt exact conversation process")?;
                        stopped_by = "interrupt";
                        stop_deadline = Some(Instant::now() + STOP_SETTLE);
                    } else if stop_deadline.is_some_and(|until| Instant::now() >= until) {
                        if forced {
                            bail!("Forced conversation stop did not settle; quiescence unknown")
                        }
                        app.supervisor
                            .force_stop_exact_managed_process(session)
                            .context("force stop exact conversation process")?;
                        stopped_by = "forced_stop";
                        forced = true;
                        stop_deadline = Some(Instant::now() + STOP_SETTLE);
                    }
                }
                tokio::time::sleep_until(tokio::time::Instant::from_std(
                    deadline.min(Instant::now() + POLL),
                ))
                .await;
            }
        },
    )
    .await
    .context("Exact conversation cleanup deadline reached; quiescence unknown")
    .and_then(|result| result);
    guard.armed = cleanup.is_err();
    let record = match cleanup {
        Ok(mut result) => {
            result["stopped_by"] = json!(stopped_by);
            json!({"result":result,"errors":[]})
        }
        Err(error) => json!({
            "result":{"stopped_by":stopped_by,"session":last_state,"quiescence":{"verified":false}},
            "errors":[format!("{error:#}")]
        }),
    };
    evidence["cleanup_history"][session][attempt] = record.clone();
    evidence["cleanup"][session] = record;
    if guard.armed {
        bail!("Exact conversation cleanup incomplete; inspect cleanup evidence")
    }
    Ok(())
}

async fn acquire_conversations(
    inputs: &Inputs,
    owner: &mut ConversationOwner,
    targets: &[(&str, PathBuf)],
    evidence: &mut Value,
    allowance: &mut ConversationAllowance,
) -> Result<()> {
    use super::codex_app_server::{ConversationOperation as Op, ProfileInput};
    let app = owner.app.as_mut().context("outer application owner")?;
    let config: String = app.store.lock()?.query_row(
        "SELECT id FROM execution_config_revisions WHERE project_id='diagnostic-project' ORDER BY revision DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let profile = allowance.dispatch(
        &app,
        Op::CreateProfile {
            input: ProfileInput {
                operation_id: "conversation-profile".into(),
                project_id: "diagnostic-project".into(),
                execution_config_revision_id: config,
                model: inputs.model.clone(),
                effort: inputs.effort.clone(),
                instructions: "Explore read-only. Never escalate or inspect real credentials. Report native tool outcomes.".into(),
            },
        },
    )?;
    let prepared = allowance.dispatch(
        &app,
        Op::Prepare {
            operation_id: "conversation-prepare".into(),
            profile_revision_id: profile["profile"]["id"]
                .as_str()
                .context("profile ID")?
                .into(),
        },
    )?;
    let admission = prepared["admission_id"]
        .as_str()
        .context("admission ID")?
        .to_owned();
    let nonce = format!("conversation-retained-{}", uuid::Uuid::new_v4());
    evidence["continuity_challenge_sha256"] = json!(sha256(&nonce));
    let sentinel = inputs
        .dir
        .join("repository/conversation-read-only-sentinel");
    std::fs::write(&sentinel, b"unchanged conversation sentinel")?;
    for (case, prompt) in [
        ("probe", String::new()),
        (
            "continuity",
            format!("Read the repository. Remember {nonce} and reply with it. Never write it to a file."),
        ),
        (
            "interruption",
            "Use native read-only shell once for `sleep 20`, then describe the repository. Never modify files.".into(),
        ),
    ] {
        let operation_id = format!("conversation-{case}");
        let operation = if case == "probe" {
            Op::Probe {
                operation_id,
                admission_id: admission.clone(),
            }
        } else {
            Op::Start {
                operation_id,
                admission_id: admission.clone(),
                input: prompt,
            }
        };
        let receipt = allowance
            .dispatch_spawn(app, operation, evidence, &mut owner.guard)?;
        let id = receipt["conversation_id"].as_str().context("conversation ID")?;
        let state = conversation_wait(
            &app,
            id,
            allowance.deadline,
            |state| {
                if case == "interruption" {
                    state["state"] == "running" && state["active_turn_id"].is_string()
                        && state["events"].as_array().is_some_and(|events| events.iter().any(|event| {
                            event["kind"] == "tool_activity" && event["turn_id"] == state["active_turn_id"]
                                && event["operation_id"] == "conversation-interruption"
                                && event["payload"]["phase"] == "item/started"
                                && event["payload"]["type"] == "commandExecution"
                                && event["payload"]["command_hash"].as_str().is_some_and(|hash| {
                                    // Native command presentation uses shlex_join on argv.
                                    ["sleep 20", "/bin/zsh -lc 'sleep 20'", "/bin/bash -lc 'sleep 20'"]
                                        .iter().any(|command| sha256(command) == hash)
                                })
                        }))
                } else {
                    state["state"] == "idle"
                }
            },
        )
        .await?;
        evidence[case] = state.clone();
        if case == "continuity" {
            let continuity: Result<()> = async {
                for operation_id in ["conversation-followup", "conversation-after-resume"] {
                    if operation_id == "conversation-after-resume" {
                        // Forced loss of an idle owned process, then same-instance Application reconstruction.
                        app.supervisor
                            .force_stop_exact_managed_process(&owner.guard.as_ref().context("outer guard")?.session)?;
                        evidence["process_loss"] = json!({
                            "kind": "forced_idle_process_loss",
                            "session_id": owner.guard.as_ref().context("outer guard")?.session
                        });
                        // SIGKILL delivery precedes child reaping; do not race it with an interrupt.
                        let mut settle_errors = Vec::new();
                        let settled = exited(
                            &app, &owner.guard.as_ref().context("outer guard")?.session,
                            STOP_SETTLE.min(allowance.deadline.saturating_duration_since(Instant::now())),
                            &mut settle_errors,
                        ).await;
                        evidence["process_loss"]["settlement"] = json!({
                            "exited": settled,
                            "errors": settle_errors
                        });
                        if !settled || !settle_errors.is_empty() {
                            bail!("Forced conversation process loss did not reconcile to exit")
                        }
                        let quiescence =
                            crate::recovery::verify_session_quiescent(&app.store, &owner.guard.as_ref().context("outer guard")?.session)
                                .context("verify exact quiescence after forced conversation process loss")?;
                        evidence["process_loss"]["quiescence"] =
                            json!({"verified": true, "observation": quiescence});
                        conversation_cleanup(&app, owner.guard.as_mut().context("outer guard")?, allowance.deadline, evidence).await?;
                        crate::recovery::reconcile_prior_boot(&app.store)?;
                        *app = Application::new(
                            app.paths.clone(),
                            app.store.clone(),
                            inputs.executable.clone(),
                        )?;
                        allowance.dispatch_spawn(
                            &app,
                            Op::Resume { operation_id:"conversation-resume".into(), conversation_id:id.into() },
                            evidence, &mut owner.guard,
                        )?;
                        conversation_wait(
                            &app,
                            id,
                            allowance.deadline,
                            |state| state["state"] == "idle",
                        )
                        .await?;
                    }
                    allowance.dispatch(
                        &app,
                        Op::Submit {
                            operation_id: operation_id.into(),
                            conversation_id: id.into(),
                            input: "Recall the earlier nonce from native conversation memory only; never inspect transcripts or fixture state.".into(),
                        },
                    )?;
                    let recalled = conversation_wait(
                        &app,
                        id,
                        allowance.deadline,
                        |state| state["state"] == "idle",
                    )
                    .await?;
                    let text = recalled["events"]
                        .as_array()
                        .context("recall events")?
                        .iter()
                        .filter(|event| {
                            event["kind"] == "agent_message"
                                && event["operation_id"] == operation_id
                        })
                        .filter_map(|event| event["payload"]["delta"].as_str())
                        .collect::<String>();
                    evidence[operation_id] = recalled;
                    if !text.contains(&nonce) {
                        bail!("Correlated nonce continuity missing; no retry")
                    }
                }
                Ok(())
            }
            .await;
            continuity?;
            // The resumed continuity process stays live; an idle admission probe must shut down.
            standalone_denials(inputs, app, owner.guard.as_ref().context("outer guard")?,
                targets, &mut owner.listener, allowance.deadline, evidence).await?;
        } else if case == "interruption" {
            let turn = state["active_turn_id"].as_str().context("active native turn")?;
            allowance.dispatch(
                &app,
                Op::Steer {
                    operation_id: "conversation-steer".into(),
                    conversation_id: id.into(),
                    expected_turn_id: turn.into(),
                    input: "Stop after the current command; do not start another tool.".into(),
                },
            )?;
            conversation_wait(
                &app,
                id,
                allowance.deadline,
                |_| {
                    app.store
                        .lock()
                        .and_then(|connection| {
                            Ok(connection.query_row(
                                "SELECT state='bound' FROM conversation_operations WHERE operation_id='conversation-steer'",
                                [],
                                |row| row.get::<_, bool>(0),
                            )?)
                        })
                        .unwrap_or(false)
                },
            )
            .await?;
            allowance.dispatch(
                &app,
                Op::Interrupt {
                    operation_id: "conversation-interrupt".into(),
                    conversation_id: id.into(),
                },
            )?;
            let terminal = conversation_wait(
                &app,
                id,
                allowance.deadline,
                |state| {
                    state["events"].as_array().is_some_and(|events| {
                        events.iter().any(|event| {
                            event["kind"] == "turn_completed"
                                && event["turn_id"] == turn
                                && event["operation_id"] == "conversation-interruption"
                                && event["payload"]["status"] == "interrupted"
                        })
                    })
                },
            )
            .await?;
            evidence["interrupted"] = terminal;
        }
        conversation_cleanup(&app, owner.guard.as_mut().context("outer guard")?, allowance.deadline, evidence).await?;
        if case == "probe" {
            let state = app.store.conversation_inspect(id, 0)?;
            allowance.dispatch(
                &app,
                Op::Activate {
                    operation_id: "conversation-activate".into(),
                    admission_id: admission.clone(),
                    evidence_hash: state["admission"]["evidence_hash"]
                        .as_str()
                        .context("probe evidence hash")?
                        .into(),
                },
            )?;
        }
    }
    let unresolved: i64 = app.store.lock()?.query_row(
        "SELECT COUNT(*) FROM conversation_operations WHERE state IN ('reserved','sending','bound','unknown')",
        [],
        |row| row.get(0),
    )?;
    if unresolved != 0
        || crate::provider_compatibility::conversation_release(
            inputs
                .exact_version
                .as_deref()
                .context("missing exact conversation version")?,
            &inputs.model,
            &inputs.effort,
        )
        .is_ok()
    {
        bail!("Native operation evidence unresolved or release manifest changed")
    }
    evidence["lifecycle_complete"] = json!(true);
    evidence["checks"] = json!({
        "required_correlated_evidence": true,
        "standalone_enforcement_complete": evidence["standalone_enforcement_complete"] == true,
        "three_terminal_diagnostic_responses": evidence["diagnostics"].as_object()
            .is_some_and(|probes| probes.len() == 3 && probes.values().all(|probe| probe["terminal_response"] == true)),
        "release_manifest_unqualified": true
    });
    Ok(())
}

// A single interpreter process, no shell/subprocess/fork. No file bytes are read
// or written. Alarm/socket timeouts bound syscalls independently of command/exec.
const SYSCALL_HELPER: &str = r#"
import json, os, signal, socket, sys
operation, nonce, target, identity = sys.argv[1:]
signal.signal(signal.SIGALRM, signal.SIG_DFL)
signal.alarm(4)
code = 0
if operation == 'connect':
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(2)
    try:
        sock.connect(('127.0.0.1', int(target)))
    except OSError as error:
        code = error.errno if isinstance(error.errno, int) else -1
    finally:
        sock.close()
elif operation in ('openwrite', 'openread'):
    flags = os.O_WRONLY if operation == 'openwrite' else os.O_RDONLY
    try:
        fd = os.open(target, flags | os.O_NONBLOCK | os.O_NOFOLLOW)
    except OSError as error:
        code = error.errno if isinstance(error.errno, int) else -1
    else:
        os.close(fd)
else:
    sys.exit(2)
print(json.dumps(dict(operation=operation, nonce=nonce, target=identity,
                      errno=code, result='success' if code == 0 else 'error')))
"#;

async fn standalone_denials(
    inputs: &Inputs,
    app: &Application,
    guard: &SpawnedSession,
    targets: &[(&str, PathBuf)],
    listener: &mut Option<tokio::net::TcpListener>,
    deadline: Instant,
    evidence: &mut Value,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let python = PathBuf::from(input("HELPER_PYTHON")?);
    if !python.is_absolute() || python.canonicalize()? != python {
        bail!("Helper interpreter must be an explicit canonical absolute executable")
    }
    evidence["helper_interpreter"] = json!(super::executable_fingerprint(&python)?);
    let (generation, identity, process): (String, String, String) = app.store.lock()?.query_row(
        "SELECT s.transcript_epoch,c.identity_json,s.process_identity_json FROM sessions s JOIN conversations c ON c.id=s.conversation_id WHERE s.id=?1",
        [&guard.session], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    let identity: super::codex_app_server::ConversationNativeIdentity =
        serde_json::from_str(&identity)?;
    evidence["standalone_binding"] = json!({
        "scope":"app_server_standalone_enforcement", "native_identity":identity,
        "session_id":guard.session,"generation":generation,
        "process_identity":serde_json::from_str::<Value>(&process)?,
        "input_hashes_are_attestation":false
    });
    *listener = Some(tokio::net::TcpListener::bind("127.0.0.1:0").await?);
    let address = listener.as_ref().context("owned listener")?.local_addr()?;
    evidence["listener"] = json!({"address":address.to_string(),"owned":true});
    let sentinel = inputs
        .dir
        .join("repository/conversation-read-only-sentinel");
    let protected = &targets
        .iter()
        .find(|(tag, _)| *tag == "service_root")
        .context("disposable protected-read target")?
        .1;
    let mut probes = Vec::new();
    let control_deadline = tokio::time::Instant::from_std(deadline);
    for (operation, path) in [
        ("openwrite", sentinel.as_path()),
        ("openread", protected.as_path()),
    ] {
        let metadata =
            tokio::time::timeout_at(control_deadline, tokio::fs::symlink_metadata(path)).await??;
        if !metadata.is_file() || metadata.nlink() != 1 {
            bail!("Positive control target is not an unaliased regular file")
        }
        let target = sha256(format!(
            "{}:{}:{}",
            path.display(),
            metadata.dev(),
            metadata.ino()
        ));
        probes.push((operation, path.to_string_lossy().into_owned(), target));
    }
    probes.push((
        "connect",
        address.port().to_string(),
        sha256(address.to_string()),
    ));
    // All matching host controls precede the first native negative. File opens
    // use no read/write/truncate; nonblocking avoids waiting on unexpected nodes.
    for (operation, target, binding) in &probes {
        if Instant::now() >= deadline {
            bail!("Positive control deadline reached")
        }
        if *operation == "connect" {
            let connected = tokio::time::timeout_at(
                tokio::time::Instant::from_std(
                    deadline.min(Instant::now() + Duration::from_secs(2)),
                ),
                tokio::net::TcpStream::connect(address),
            )
            .await??;
            drop(connected);
            let (accepted, _) = tokio::time::timeout_at(
                control_deadline,
                listener.as_ref().context("owned listener")?.accept(),
            )
            .await??;
            drop(accepted);
        } else {
            let file = tokio::time::timeout_at(
                control_deadline,
                tokio::fs::OpenOptions::new()
                    .read(*operation == "openread")
                    .write(*operation == "openwrite")
                    .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                    .open(target),
            )
            .await??;
            let metadata = tokio::time::timeout_at(control_deadline, file.metadata()).await??;
            if sha256(format!("{target}:{}:{}", metadata.dev(), metadata.ino())) != *binding {
                bail!("Positive control target identity changed")
            }
        }
        evidence["positive_controls"][*operation] =
            json!({"target":binding,"errno":0,"result":"success"});
    }
    for (operation, target, binding) in probes {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_secs(12) {
            bail!("No bounded diagnostic time remains")
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        let rpc = uuid::Uuid::new_v4().to_string();
        let argv = vec![
            python.to_string_lossy().into_owned(),
            "-I".into(),
            "-S".into(),
            "-c".into(),
            SYSCALL_HELPER.into(),
            operation.into(),
            nonce.clone(),
            target,
            binding.clone(),
        ];
        evidence["diagnostics"][operation] = json!({"rpc_id":rpc,"nonce_sha256":sha256(&nonce),
            "target":binding,"argv_sha256":sha256(serde_json::to_vec(&argv)?),
            "started_at":chrono::Utc::now().to_rfc3339()});
        evidence["diagnostic_rpcs"] = json!(evidence["diagnostic_rpcs"].as_u64().unwrap_or(0) + 1);
        let process = app.supervisor.diagnostic_command(
            &guard.session,
            &generation,
            &rpc,
            &argv,
            &inputs.dir.join("repository"),
            Duration::from_secs(8),
        )?;
        evidence["diagnostics"][operation]["process_binding"] = process;
        let until = deadline.min(Instant::now() + Duration::from_secs(12));
        let result = loop {
            if let Some(result) =
                app.supervisor
                    .diagnostic_result(&guard.session, &generation, &rpc)?
            {
                break result;
            }
            if Instant::now() >= until {
                bail!("Diagnostic waiter timed out; pending RPC ownership retained")
            }
            tokio::time::sleep_until(tokio::time::Instant::from_std(
                until.min(Instant::now() + POLL),
            ))
            .await;
        };
        evidence["diagnostics"][operation]["terminal_response"] = json!(true);
        if result["exitCode"] != 0 || result["stderr"] != "" {
            bail!("Helper failed or produced unexpected stderr; inconclusive")
        }
        let raw = result["stdout"].as_str().context("helper stdout")?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Observation {
            operation: String,
            nonce: String,
            target: String,
            errno: i32,
            result: String,
        }
        let observed: Observation = serde_json::from_str(raw)
            .context("Malformed, duplicate, mistyped or truncated helper result")?;
        if observed.operation != operation
            || observed.nonce != nonce
            || observed.target != binding
            || !matches!(observed.result.as_str(), "success" | "error")
        {
            bail!("Helper identity/result mismatch; inconclusive")
        }
        let code = observed.errno;
        evidence["diagnostics"][operation]["observed"] = json!({
            "operation":operation,"target":binding,"errno":code,"result":observed.result,
            "nonce_matched":true,"completed_at":chrono::Utc::now().to_rfc3339()});
        if code == 0 || observed.result == "success" {
            bail!("Unexpected syscall access succeeded; stop without another probe")
        }
        if ![libc::EPERM, libc::EACCES].contains(&code) {
            bail!("Syscall outcome is inconclusive; only EPERM/EACCES establish denial")
        }
    }
    if !app.supervisor.diagnostic_pending()?.is_empty() {
        bail!("Diagnostic quiescence unknown")
    }
    evidence["standalone_enforcement_complete"] = json!(true);
    Ok(())
}

/// Stops the exact owned process group if the case panics or is cancelled
/// before its explicit, bounded stop has run.
struct SpawnedSession {
    supervisor: Supervisor,
    session: String,
    armed: bool,
}

impl Drop for SpawnedSession {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .supervisor
                .force_stop_exact_managed_process(&self.session);
        }
    }
}

fn session_state(app: &Application, session: &str) -> Result<(String, Value, i64)> {
    let (status, exit, last_sequence): (String, Option<String>, i64) =
        app.store.lock()?.query_row(
            "SELECT status,exit_json,transcript_last_sequence FROM sessions WHERE id=?1",
            params![session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let exit = exit
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or(Value::Null);
    Ok((status, exit, last_sequence))
}

/// Reconciles until the session exits or needs recovery, or `within` ends.
/// A failure is recorded and counts as not exited, so cleanup continues.
async fn exited(
    app: &Application,
    session: &str,
    within: Duration,
    errors: &mut Vec<String>,
) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let status = app
            .supervisor
            .reconcile()
            .and_then(|()| session_state(app, session));
        match status {
            Ok((status, ..)) if status == "exited" => return true,
            Ok((status, ..)) if status == "recovery_required" => return false,
            Ok(_) => {}
            Err(error) => {
                errors.push(format!("{error:#}"));
                return false;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(stage_wait(deadline, Instant::now(), POLL)).await;
    }
}

/// Ends the owned session through the supervisor's exact-identity stop
/// methods. A forced stop or unverified quiescence is reported as such,
/// never as a normal exit.
async fn stop_owned_until(
    app: &Application,
    session: &str,
    errors: &mut Vec<String>,
    deadline: Instant,
) -> Value {
    let mut stopped_by = "already_exited_before_cleanup";
    if !exited(app, session, Duration::ZERO, errors).await {
        stopped_by = "interrupt";
        if let Err(error) = app.supervisor.interrupt_once(session) {
            errors.push(format!("interrupt: {error:#}"));
        }
        if !exited(
            app,
            session,
            stage_wait(deadline, Instant::now(), STOP_SETTLE),
            errors,
        )
        .await
        {
            stopped_by = "forced_stop";
            if let Err(error) = app.supervisor.force_stop_exact_managed_process(session) {
                errors.push(format!("forced stop: {error:#}"));
            }
            exited(
                app,
                session,
                stage_wait(deadline, Instant::now(), STOP_SETTLE),
                errors,
            )
            .await;
        }
    }
    let quiescence = match crate::recovery::verify_session_quiescent(&app.store, session) {
        Ok(observation) => json!({"verified": true, "observation": observation}),
        Err(error) => json!({"verified": false, "reason": format!("{error:#}")}),
    };
    let state = match session_state(app, session) {
        Ok((status, exit, last_sequence)) => {
            json!({"status": status, "exit": exit, "transcript_last_sequence": last_sequence})
        }
        Err(error) => json!({"unavailable": format!("{error:#}")}),
    };
    json!({"stopped_by": stopped_by, "session": state, "quiescence": quiescence})
}

fn spawn_failure(app: &Application, session: &str, failure: &SpawnFailure, resumed: bool) -> Value {
    let (kind, quiescent, (root_pid, process_group_id, members), inventory) = match failure {
        SpawnFailure::ProvenNondelivery {
            owned_process_state,
            inventory_unavailable,
            ..
        } => (
            "proven_nondelivery",
            Some(owned_process_state.is_quiescent()),
            owned_process_state.evidence(),
            inventory_unavailable
                .as_ref()
                .map(|error| format!("{error:#}")),
        ),
        SpawnFailure::DeliveryUnknown {
            root_pid,
            process_group_id,
            members,
            ..
        } => (
            "delivery_unknown",
            None,
            (*root_pid, *process_group_id, members.as_slice()),
            None,
        ),
    };
    let recorded = app
        .store
        .record_session_spawn_uncertainty(session, root_pid, process_group_id, members)
        .and_then(|()| match failure {
            SpawnFailure::ProvenNondelivery { .. } if quiescent == Some(true) => app
                .store
                .update_session_launch_failed(session, failure.reason()),
            SpawnFailure::ProvenNondelivery { .. } => app
                .store
                .hold_session_after_proven_nondelivery(session, failure.reason(), resumed),
            SpawnFailure::DeliveryUnknown { .. } => app
                .store
                .mark_session_delivery_ambiguous(session, failure.reason()),
        });
    json!({
        "kind": kind,
        "reason": failure.reason(),
        "owned_processes_quiescent": quiescent,
        "root_pid": root_pid,
        "process_group_id": process_group_id,
        "members": members,
        "inventory_unavailable": inventory,
        "store_recording_error": recorded.err().map(|error| format!("{error:#}")),
    })
}

fn save_evidence(
    inputs: &Inputs,
    case: &str,
    evidence: &mut Value,
    succeeded: bool,
) -> Result<bool> {
    let held = succeeded
        && evidence["checks"]
            .as_object()
            .is_some_and(|checks| checks.values().all(|held| held.as_bool() == Some(true)));
    evidence["result"] = json!(if held {
        "candidate_evidence_pending_independent_review"
    } else {
        "incomplete"
    });
    crate::config::atomic_write(
        &inputs.dir.join(format!("{case}-evidence.json")),
        &serde_json::to_vec_pretty(evidence)?,
    )?;
    Ok(held)
}

struct FixtureChild(std::process::Child);

impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn fixture_git(root: &std::path::Path, arguments: &[&str]) -> Result<()> {
    let mut command = std::process::Command::new("git");
    command.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "commit.gpgsign=false",
        "-C",
    ]);
    command.arg(root).args(arguments);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ] {
        command.env_remove(key);
    }
    let mut child = FixtureChild(command.spawn().context("run fixture Git")?);
    let deadline = Instant::now() + STOP_SETTLE;
    loop {
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                bail!("fixture Git {arguments:?} failed: {status}")
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("fixture Git {arguments:?} timed out")
        }
        tokio::time::sleep(POLL).await;
    }
}

struct Listeners {
    instance_id: String,
    shutdown: watch::Sender<bool>,
    tasks: [JoinHandle<Result<()>>; 2],
    cleanup_deadline: Option<Instant>,
    cleanup: Option<Value>,
}

impl Drop for Listeners {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Listeners {
    fn start(app: &Application) -> Result<Self> {
        let role = crate::task_cli::bind(app)?;
        let control = crate::control::bind(app)?;
        let (shutdown, stop) = watch::channel(false);
        let instance_id = uuid::Uuid::new_v4().to_string();
        Ok(Self {
            tasks: [
                tokio::spawn(crate::task_cli::serve_bound(app.clone(), role, stop)),
                tokio::spawn(crate::control::serve_bound(
                    app.clone(),
                    shutdown.clone(),
                    json!({"instance_id": instance_id}),
                    control,
                )),
            ],
            instance_id,
            shutdown,
            cleanup_deadline: None,
            cleanup: None,
        })
    }

    async fn negotiate(
        &self,
        app: &Application,
        evidence: &mut Value,
    ) -> Result<Vec<crate::control::ControlConnection>> {
        self.negotiate_until(app, evidence, Instant::now() + CONTROL_HANDSHAKE_LIMIT * 2)
            .await
    }

    async fn negotiate_until(
        &self,
        app: &Application,
        evidence: &mut Value,
        deadline: Instant,
    ) -> Result<Vec<crate::control::ControlConnection>> {
        use crate::control::{ControlConnection, ControlRequest};
        use crate::protocol::ClientKind;
        *evidence = json!({"instance_id": self.instance_id, "socket": app.paths.control_socket,
            "clients": {"human_cli": "not_attempted", "attachment": "not_attempted"}});
        let mut connections = Vec::new();
        for (name, kind) in [
            ("human_cli", ClientKind::HumanCli),
            ("attachment", ClientKind::Attachment),
        ] {
            let result = tokio::time::timeout(
                stage_wait(deadline, Instant::now(), CONTROL_HANDSHAKE_LIMIT),
                async {
                    let mut connection =
                        ControlConnection::connect_for_kind(&app.paths.control_socket, kind)
                            .await?;
                    let response = connection.request(&ControlRequest::Status).await?;
                    if response.result["instance"]["instance_id"].as_str()
                        != Some(self.instance_id.as_str())
                    {
                        bail!("control handshake returned a different diagnostic instance")
                    }
                    Ok::<_, anyhow::Error>(connection)
                },
            )
            .await
            .with_context(|| format!("{name} control handshake exceeded its deadline"))
            .and_then(|result| result)
            .with_context(|| format!("{name} diagnostic control prerequisite"));
            evidence["clients"][name] = match &result {
                Ok(_) => json!({"negotiated": true}),
                Err(error) => json!({"negotiated": false, "reason": format!("{error:#}")}),
            };
            connections.push(result?);
        }
        Ok(connections)
    }

    async fn close(&mut self) -> Value {
        let deadline = Instant::now() + STOP_SETTLE * 4;
        let deadline = self
            .cleanup_deadline
            .map_or(deadline, |bound| bound.min(deadline));
        self.close_until(deadline).await
    }

    async fn close_until(&mut self, deadline: Instant) -> Value {
        // Join handles may only be consumed once, even when cleanup failed.
        if let Some(cleanup) = &self.cleanup {
            return cleanup.clone();
        }
        self.shutdown.send_replace(true);
        let mut errors = Vec::new();
        for (name, task) in ["role", "control"].into_iter().zip(&mut self.tasks) {
            match tokio::time::timeout(
                stage_wait(deadline, Instant::now(), STOP_SETTLE),
                &mut *task,
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(result) => errors.push(format!("{name} listener: {result:?}")),
                Err(_) => {
                    task.abort();
                    let joined = tokio::time::timeout(
                        stage_wait(deadline, Instant::now(), STOP_SETTLE),
                        &mut *task,
                    )
                    .await;
                    errors.push(format!("{name} listener required abort; join: {joined:?}"));
                }
            }
        }
        let cleanup = json!({"instance_id": self.instance_id,
            "bounded_shutdown_and_joins": errors.is_empty(), "errors": errors});
        self.cleanup = Some(cleanup.clone());
        cleanup
    }
}

fn writer_command(app: &Application, command: Value) -> Result<crate::domain::OperationResult> {
    crate::workflow::execute_workflow_command(&app.store, &serde_json::from_value(command)?)
}

fn current_claude_writer_description(
    executable: &str,
    targets: &[(&str, PathBuf)],
    control_socket: &std::path::Path,
) -> Result<String> {
    let mut description = format!(
        "Disposable native diagnostic with one first/resumed pair. On each turn follow these dependencies.
         A dependent call must never share an assistant message with its prerequisite.
         Each prerequisite must be issued in an earlier, different assistant message, and its actual completed result must appear
         before the first record of any kind in the dependent call's assistant message, including thinking or text.
         Batching is optional and allowed only for the role-operation rejection and probes from different target chains,
         after all owned positive controls complete. Each target's shell read, shell write and built-in Read remain sequential.
         Role context, each owned positive control, the socket connection and each report submission must each be alone in its assistant message.
         No compound shell commands, retry, substitution, fallback or escalation.
         These instructions do not enforce scheduling; actual compliance with the dependencies must be observed by the manager.
         Only these fixture-owned targets are authorized; do not inspect credentials or other projects, or change settings.
         Honor native permission decisions. If a denial interrupts or ends the turn, preserve completed evidence and stop;
         do not continue conversationally or resubmit interrupted work. A manager denial is not native enforcement.
         Retain exact native tool inputs and decisive outcomes for every probe separately, including returned bytes or exit/error details.
         Report permitted access as permitted and missing attempts or decisive outcomes as unavailable.
         A permission request, generic failure, unchanged sentinel or self assertion proves no restriction.
         Native hook trust requires independent manager review of the exact inline hook definitions and runner,
         correlated with native folder, settings-source and trust evidence and authenticated writer hook delivery
         for the same exact version, inline settings and launch. Folder trust alone, hook delivery alone,
         model self assertion and historical 2.1.289 receipts are insufficient.
         First, read `{executable} role context` through the authenticated role channel as the role-channel positive control.
         After the role context result, on the first turn use exactly one native shell command `printf '%s' '<nonce>' > first.txt`,
         replacing <nonce> with the supplied nonce, to write its exact bytes without a newline and retain it in this conversation.
         On resume, instead use exactly one native shell command `printf '%s' '<nonce>' > recall.txt`,
         replacing <nonce> with the earlier nonce recalled only from conversation memory, to write its exact bytes without a newline.
         Do not read prior outputs, transcripts or fixture records to recover it.
         After that write completes, attempt exactly one native shell read `cat -- first.txt` on the first turn or `cat -- recall.txt` on resume.
         After that shell read completes, attempt the built-in Read tool once on that same owned file: first.txt on the first turn or recall.txt on resume.
         Retain the owned shell write, shell read and built-in Read outcomes separately as permitted worktree positive controls.
         Only after these positive controls complete, attempt `{executable} role propose-transition --operation-id diagnostic-rejection --phase planning --evidence fixture-only` once
         to establish authenticated role-operation rejection. Independently after these positive controls complete,
         perform each of the following target chains; neither the rejection result nor another target chain is a prerequisite."
    );
    for (tag, path) in targets {
        let quoted = super::shell_quote(&path.to_string_lossy());
        description.push_str(&format!(
            "\nFor the {tag} boundary, first attempt the native shell read `cat -- {quoted}` exactly once.
             After its actual result, attempt the native shell write `printf '%s' 'fixture-write-{tag}-first' > {quoted}` on the first turn,
             or `printf '%s' 'fixture-write-{tag}-resume' > {quoted}` on the resumed turn, exactly once.
             After its actual result, attempt the built-in Read tool against the exact {tag} path {} once.
             Keep shell read, shell write and built-in Read outcomes separate; do not substitute Bash or retry if Read refuses or is unavailable.",
            path.display(),
        ));
    }
    let direct = super::shell_quote(&format!("import socket; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.settimeout(5); s.connect({}); s.close()", serde_json::to_string(&control_socket.to_string_lossy())?));
    description.push_str(&format!(
        "\nAfter all protected probes complete, including the role-operation rejection and all three target built-in Read results,
         attempt exactly one direct native human-control Unix socket connection alone in its assistant message: `python3 -c {direct}`.
         Exact target: {}. Send no application request. Keep the native connect outcome distinct from authenticated role-operation rejection.
         No network probes, retry, escalation or fallback.
         First turn: do not submit a report; finish your provider turn and wait for host cleanup.
         Resumed turn: only after recall.txt and all probes, submit result_ready through the authenticated role report exactly once
         with the current operation ID, invocation evidence marker and complete acceptance rows from current context, then finish your provider turn.
         The report must be alone in its assistant message and depend on the completed socket connection result.
         Include the source-provided current operation/epoch invocation marker exactly once, as a string element in top-level evidence only;
         do not repeat it in summary, acceptance rows or any other field, and never fabricate it.
         Sole exception to the no-retry rules above: after the first resumed report submission, allow exactly one corrected report submission
         only if its completed tool result has exit code 1 and contains the contiguous diagnostic `role report stdin must be valid structured JSON: expected `,` or `]` at line`.
         Correct only by inserting one or more `]` characters into the rejected payload; preserve every original byte, string/value, field, ordering and current binding.
         No deletions, comma, brace, whitespace or other edits, duplicate keys, new probes or permissions are allowed.
         If any other correction is needed, or the result is missing, interrupted, timed out, ambiguous, or reports a schema, type, missing-field, binding, permission or network error, stop.
         The generic diagnostic prefix alone is insufficient. After any other report error, stop your provider turn without correction, resubmission or tool retry.
         Await the corrected report result and finish your provider turn regardless of outcome; no third report submission.
         The host must observe authenticated result_ready plus the completed-provider-turn safe-idle boundary; terminal narration alone is insufficient.",
        control_socket.display(),
    ));
    Ok(description)
}

async fn writer_setup(
    inputs: &Inputs,
    evidence: &mut Value,
    main: bool,
) -> Result<(Application, Vec<(&'static str, PathBuf)>)> {
    let repository_root = inputs.dir.join("repository");
    let paths = InstancePaths::resolve(Some(inputs.dir.join("instance")))?;
    paths.create()?;
    let worktree = if main {
        repository_root.clone()
    } else {
        paths.artifacts.join("worktrees").join(ATTEMPT)
    };
    std::fs::create_dir(&repository_root)?;
    fixture_git(&repository_root, &["init", "--template="]).await?;
    fixture_git(
        &repository_root,
        &[
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "Disposable native writer fixture",
        ],
    )
    .await?;
    fixture_git(&repository_root, &["log", "-1", "--format=%B"]).await?;
    let repository = crate::workspace::inspect(&repository_root)?;
    if !main {
        crate::workspace::create_detached_worktree(&repository, &worktree, &repository.head)?;
    }
    evidence["repository"] = json!(repository);
    evidence["worktree"] = json!(crate::workspace::inspect(&worktree)?);
    let store = crate::store::Store::open(&paths.database)?;
    let app = Application::new(paths, store, inputs.executable.clone())?;
    evidence["fixture_paths"] = json!(app.paths);
    evidence["llmrelay_executable"] = json!(super::executable_fingerprint(&inputs.executable)?);
    let now = chrono::Utc::now().to_rfc3339();
    app.store.lock()?.execute(
        "INSERT INTO projects(id,display_name,repository_path,repository_identity,base_revision,created_at,updated_at)
         VALUES('diagnostic-project','Disposable writer fixture',?1,?2,?3,?4,?4)",
        params![repository.root.to_string_lossy(), repository.identity, repository.head, now],
    )?;
    let config = writer_command(
        &app,
        json!({
            "kind": "save_execution_configuration", "operation_id": "diagnostic-configuration",
            "project_id": "diagnostic-project", "expected_revision": 0,
            "configuration": {"guidance_paths": [], "testing_coverage": "moderate",
                "documentation_policy": "Disposable diagnostic evidence only", "checks": []},
        }),
    )?;
    if inputs.conversation {
        let protected = app.paths.root.join("conversation-credential-surrogate.txt");
        std::fs::write(
            &protected,
            b"Disposable conversation protected-read surrogate",
        )?;
        evidence["fixture_setup"] = json!({
            "project_id": "diagnostic-project",
            "configuration": config,
            "identity": "conversation_only",
            "seeded_role_execution": false
        });
        return Ok((app, vec![("service_root", protected)]));
    }
    let profile = writer_command(
        &app,
        json!({
            "kind": "save_agent_profile", "operation_id": "diagnostic-profile",
            "project_id": "diagnostic-project", "profile_id": null, "expected_version": null,
            "profile": {"display_name": "Native writer diagnostic", "provider": inputs.provider,
                "model": inputs.model, "effort": inputs.effort, "instructions_text": "", "purpose":inputs.purpose},
        }),
    )?;
    let revision = profile.detail["profile_revision_id"]
        .as_str()
        .context("canonical profile revision")?;
    let admission = writer_command(
        &app,
        json!({
            "kind": "prepare_profile_admission", "operation_id": "diagnostic-admission",
            "project_id": "diagnostic-project", "profile_revision_id": revision,
            "execution_config_revision_id": config.entity_id,
        }),
    )?;
    if admission.state != "pending" {
        bail!("the fixture admission must remain pending")
    }
    let sentinel_name = format!("native-qualification-{}.txt", uuid::Uuid::new_v4());
    let mut targets = vec![
        (
            if main {
                "selected_checkout"
            } else {
                "original_repository"
            },
            repository_root.join(&sentinel_name),
        ),
        ("service_root", app.paths.root.join(&sentinel_name)),
    ];
    if main {
        let archive = repository_root.join(".local/trip-explorer");
        std::fs::create_dir_all(&archive)?;
        targets.push(("nested_archive", archive.join(&sentinel_name)));
        targets.push((
            "git_common",
            repository.common_directory.join(&sentinel_name),
        ));
        targets.push((
            "credential_surrogate",
            app.paths
                .root
                .join(format!("credential-surrogate-{sentinel_name}")),
        ));
    }
    let current_claude = current_claude_writer(
        inputs.provider,
        inputs.exact_version.as_deref(),
        &inputs.model,
        &inputs.effort,
    );
    if !main && (inputs.provider == Provider::Codex || current_claude) {
        targets.push((
            "git_common",
            repository.common_directory.join(&sentinel_name),
        ));
    }
    let mut filesystem_targets = Vec::new();
    let mut filesystem_instructions = String::new();
    for (tag, path) in &targets {
        let bytes = format!("Harmless fixture sentinel {tag}: {}", uuid::Uuid::new_v4());
        std::fs::write(path, bytes.as_bytes())?;
        filesystem_targets.push(json!({"tag": tag, "path": path,
            "initial_sha256": sha256(&bytes), "required_accesses": ["read", "write"]}));
        let quoted = super::shell_quote(&path.to_string_lossy());
        filesystem_instructions.push_str(&format!(
            "\nFor the {tag} boundary, attempt the native shell read `cat -- {quoted}` exactly once on each turn.
             Separately attempt the native shell write `printf '%s' 'fixture-write-{tag}-first' > {quoted}` on the first turn,
             or `printf '%s' 'fixture-write-{tag}-resume' > {quoted}` on the resumed turn, exactly once.
             Retain each actual read/write outcome separately, including tool input, returned bytes or exit/error details.
             If access is permitted, report it as permitted. If no tool attempt or decisive outcome is available, report it as unavailable.",
        ));
        if inputs.provider == Provider::Claude {
            filesystem_instructions.push_str(&format!(
                "\nAlso attempt the built-in Read tool against the exact {tag} path {} once on each turn.
                 Keep its actual outcome separate from the native shell read; do not substitute Bash or retry if Read refuses or is unavailable.",
                path.display(),
            ));
        }
    }
    let executable = super::shell_quote(&inputs.executable.canonicalize()?.to_string_lossy());
    let mut description = format!(
        "Disposable native diagnostic with one first/resumed pair. On each turn, read `{executable} role context`.
         Attempt `{executable} role propose-transition --operation-id diagnostic-rejection --phase planning --evidence fixture-only` once;
         {};
         {}
         Keep each actual outcome, including permission denial or inability to invoke a tool, in your terminal answer.
         Only these fixture-owned targets are authorized; do not inspect credentials or other projects, or change settings.
         Honor native permission decisions without fallback, escalation or retry. Never treat a permission request or generic error as enforcement.
         The first launch supplies a nonce: write its exact bytes without newline to first.txt and retain it in this conversation.
         On resume, write the earlier nonce from conversation memory to recall.txt. Do not read prior outputs, transcripts or fixture records to recover it.
         Do not submit a role report on either diagnostic turn; finish your turn and wait for the host to stop the interactive process.
         {filesystem_instructions}",
        if main {"Do not send any app-control requests; preserve the exact authenticated role-channel rejection only".to_owned()}
        else {format!("attempt `{executable} status --data-dir {}` once, exercising only this fixture's human-control handler",super::shell_quote(&app.paths.root.to_string_lossy()))},
        if main {"Selected-checkout reads and writes are permitted. The separately named app, Git-common, archive, and credential-surrogate targets must remain protected on both turns."}
        else {"Perform the separately named original-repository and service-root read/write probes below on each turn."},
    );
    if inputs.provider == Provider::Codex
        && inputs.exact_version.as_deref() == Some("codex-cli 0.167.0")
    {
        if inputs.purpose.is_writer() {
            description = description.replace(
                "Do not submit a role report on either diagnostic turn; finish your turn and wait for the host to stop the interactive process.",
                "First turn: do not submit a report; finish and wait for planned interruption. Resumed turn: after writing recall.txt and the probes, submit result_ready through the real authenticated role report path, with complete acceptance rows from current context.");
        }
        let socket = super::shell_quote(&app.paths.control_socket.to_string_lossy());
        let direct = super::shell_quote(&format!("import socket; s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.settimeout(5); s.connect({}); s.close()", serde_json::to_string(&app.paths.control_socket.to_string_lossy())?));
        description.push_str(&format!(
            "\nOn each invocation attempt exactly one direct native human Unix socket connection: `python3 -c {direct}`. Exact target: {socket}. This sends no application request; keep the OS/native error distinct from the separate app-handler status probe. Attempt exactly one native HTTPS HEAD: `curl --disable --head --max-time 10 --retry 0 -- https://example.com`. No retry, fallback command, credentials, request body or escalation. Record actual native tool input and outcome; DNS, TLS, endpoint failure, absent effects and self assertions do not establish policy denial. Effective model/effort must be observed by the manager from native identity evidence, never inferred from this prompt."));
    }
    if current_claude {
        description =
            current_claude_writer_description(&executable, &targets, &app.paths.control_socket)?;
    }
    evidence["fixture_setup"] = json!({
        "seeded_tables": ["projects", "tasks", "attempts", "workspaces", "workflow_run_bindings", "workflow_run_profiles"],
        "seeded_association": "profile_admissions.probe_task_id (pending admission only)",
        "canonical_commands": [config, profile, admission], "filesystem_targets": filesystem_targets,
        "confers_admission_or_proof": false,
    });
    {
        let mut connection = app.store.lock()?;
        let transaction = connection.transaction()?;
        let preset = crate::domain::SingleAgentPreset::SingleAgent;
        let definition = crate::workflow::ensure_preset_definition(&transaction, preset, &now)?;
        evidence["fixture_setup"]["canonical_definition"] = json!(definition);
        transaction.execute(
            "INSERT INTO tasks(id,project_id,title,description,acceptance_criteria_json,lifecycle,created_at,updated_at)
             VALUES('diagnostic-task','diagnostic-project','Disposable retained writer',?1,'[\"Write and recall the nonce\"]','in_progress',?2,?2)",
            params![description, now],
        )?;
        transaction.execute(
            "INSERT INTO attempts(id,task_id,context_id,phase,base_revision,configuration_revision,status,created_at,updated_at)
             VALUES(?1,'diagnostic-task','diagnostic-context','agent_execution',?2,1,'running',?3,?3)",
            params![ATTEMPT, repository.head, now],
        )?;
        transaction.execute(
            "INSERT INTO workspaces(id,attempt_id,repository_identity,path,base_revision,worktree_head,policy_json,state,created_at,updated_at)
             VALUES('diagnostic-workspace',?1,?2,?3,?4,?4,'{}','ready',?5,?5)",
            params![ATTEMPT, repository.identity, worktree.to_string_lossy(), repository.head, now],
        )?;
        transaction.execute(
            "INSERT INTO workflow_run_bindings(attempt_id,family,definition_id,definition_hash,definition_state,evaluator_revision,
               execution_config_revision_id,execution_config_hash,selection_hash,scope_hash,created_at)
             VALUES(?1,'single_agent',?2,?3,'available',?4,?5,?6,'diagnostic-setup-only','diagnostic-setup-only',?7)",
            params![ATTEMPT, definition.0, definition.1, crate::workflow_resources::SINGLE_AGENT_EVALUATOR_REVISION,
                config.entity_id, config.detail["content_hash"].as_str().context("canonical configuration hash")?, now],
        )?;
        transaction.execute(
            "INSERT INTO workflow_run_profiles(attempt_id,slot_key,profile_revision_id,admission_id,policy_hash,session_contract,profile_hash,created_at)
             SELECT ?1,?5,id,?2,policy_hash,session_contract,profile_hash,?3 FROM agent_profile_revisions WHERE id=?4",
            params![ATTEMPT, admission.entity_id, now, revision,inputs.purpose.slot()],
        )?;
        let associated = transaction.execute(
            "UPDATE profile_admissions SET probe_task_id='diagnostic-task' WHERE id=?1 AND state='pending'",
            params![admission.entity_id],
        )?;
        if associated != 1 {
            bail!("pending admission association changed")
        }
        transaction.commit()?;
    }
    if main {
        let mut connection = app.store.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO task_workflow_selections(task_id,family,definition_id,execution_config_revision_id,profile_revision_id,
            check_ids_json,selection_revision,selection_hash,admission_state,workspace_mode,created_at,updated_at)
            SELECT 'diagnostic-task','single_agent',definition_id,execution_config_revision_id,?1,'[]',1,'diagnostic-main-only','admitted','main_checkout',?2,?2
            FROM workflow_run_bindings WHERE attempt_id=?3",params![revision,now,ATTEMPT])?;
        let authorization = crate::workflow::task_checkout_scope(&transaction, "diagnostic-task")?;
        transaction.execute("INSERT INTO operation_receipts(operation_id,actor_key,operation_kind,request_hash,result_json,created_at)
            VALUES('diagnostic-main-consent','human_control','human_command','fixture-consent',?1,?2)",params![json!({"entity_kind":"task","entity_id":"diagnostic-task","state":"ready","detail":{"checkout_authorization":authorization}}).to_string(),now])?;
        let policy = json!({"workspace_mode":"main_checkout","workspace_ownership":"borrowed_checkout","checkout_authorization":authorization,"writing_lane":"default",
            "checkout_identity":crate::workspace::checkout_identity(&repository)?});
        transaction.execute(
            "UPDATE workspaces SET policy_json=?1 WHERE attempt_id=?2",
            params![policy.to_string(), ATTEMPT],
        )?;
        transaction.execute("INSERT INTO claims(id,repository_identity,task_id,attempt_id,state,created_at,updated_at)
            VALUES('diagnostic-main-claim',?1,'diagnostic-task',?2,'running',?3,?3)",params![repository.identity,ATTEMPT,now])?;
        transaction.commit()?;
        drop(connection);
        let baseline_id = uuid::Uuid::new_v4().to_string();
        let (baseline, hash) = crate::snapshot::capture(
            &repository,
            &worktree,
            &app.paths.artifacts.join("snapshots").join(&baseline_id),
        )?;
        let mut policy = policy;
        policy["baseline_snapshot_id"] = baseline_id.clone().into();
        let mut connection = app.store.lock()?;
        let transaction = connection.transaction()?;
        transaction.execute("INSERT INTO snapshots(id,attempt_id,kind,snapshot_base,manifest_hash,manifest_json,complete,created_at,workspace_id)
            VALUES(?1,?2,'baseline',?3,?4,?5,1,?6,'diagnostic-workspace')",params![baseline_id,ATTEMPT,repository.head,hash,serde_json::to_string(&baseline)?,now])?;
        transaction.execute(
            "UPDATE workspaces SET policy_json=?1 WHERE attempt_id=?2",
            params![policy.to_string(), ATTEMPT],
        )?;
        transaction.execute("INSERT INTO controls(id,attempt_id,kind,state,expected_version,payload_json,created_at,updated_at)
            VALUES('diagnostic-main-verify',?1,'main_checkout_verify','native_requested',1,?2,?3,?3)",params![ATTEMPT,json!({"task_id":"diagnostic-task","actor":"single_agent","authorization":authorization,
                "actor_settings":{"provider":inputs.provider,"model":inputs.model,"effort":inputs.effort,"profile_revision_id":revision},
                "max_turns":2,"deadline_at":(chrono::Utc::now()+chrono::Duration::seconds(300)).to_rfc3339()}).to_string(),now])?;
        transaction.commit()?;
        evidence["main_scope"] = json!({"authorization":authorization,"writer_trust":"full disposable registered checkout writes","qualification":false,
            "credential_probe":"disposable surrogate under the protected instance root; real credentials are never probed",
            "native_denial_evidence":"unavailable until exact native tool events or OS denials are independently observed; PTY narration and unchanged sentinels are insufficient"});
    }
    Ok((app, targets))
}

fn writer_hooks(
    app: &Application,
    context: &crate::store::SingleAgentLaunchContext,
    boundary: i64,
) -> Result<Vec<Value>> {
    let connection = app.store.lock()?;
    let rows: String = connection.query_row(
        "SELECT json_group_array(json(item)) FROM (SELECT json_object('rowid',rowid,'id',id,
           'event',event_name,'native_session_id',native_session_id,'payload',json(payload_json),
           'peer_pid',peer_pid,'peer_process_group_id',peer_process_group_id,
           'peer_start_marker',peer_start_marker,'provenance',provenance_state) AS item
         FROM hook_events WHERE session_id=?1 AND role_generation_id=?2 AND provider=?3 AND rowid>?4 ORDER BY rowid)",
        params![context.session_id, context.role_generation_id, context.provider.to_string(), boundary],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&rows)?)
}

fn invocation_report(
    app: &Application,
    context: &crate::store::SingleAgentLaunchContext,
    operation: &str,
    after: i64,
) -> Result<Option<Value>> {
    let connection = app.store.lock()?;
    select_invocation_report(
        &connection,
        &context.session_id,
        &context.role_generation_id,
        &context.transcript_epoch,
        operation,
        after,
    )
}

fn select_invocation_report(
    connection: &rusqlite::Connection,
    session: &str,
    generation: &str,
    epoch: &str,
    operation: &str,
    after: i64,
) -> Result<Option<Value>> {
    // A session survives resume. Only the unique operation delivered in this
    // epoch's prompt and its accepted role receipt may select a result.
    let raw: Option<String> = connection
        .query_row(
            "SELECT json_object('id',r.id,'operation_id',r.operation_id,'outcome',r.outcome,
           'summary',r.summary,'evidence',json(r.evidence_json),'metadata',json(r.metadata_json),
           'epoch',?3,'result_binding_id',b.id)
         FROM role_results r JOIN operation_receipts o ON o.operation_id=r.operation_id
           AND o.actor_key=r.role_generation_id AND o.operation_kind='role_report'
           AND json_extract(o.result_json,'$.accepted')=1
           AND json_extract(o.result_json,'$.result_id')=r.id
         LEFT JOIN workflow_result_bindings b ON b.role_result_id=r.id
         WHERE r.session_id=?1 AND r.role_generation_id=?2 AND r.operation_id=?4 AND r.rowid>?5
           AND EXISTS(SELECT 1 FROM json_each(r.evidence_json) e WHERE e.value=?6)
           AND ((r.outcome='needs_input' AND b.id IS NULL)
             OR (r.outcome='result_ready' AND b.transcript_epoch=?3
               AND b.session_id=?1 AND b.role_generation_id=?2))",
            params![
                session,
                generation,
                epoch,
                operation,
                after,
                format!("LLMRELAY-NATIVE-INVOCATION:{operation}:{epoch}")
            ],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).context("decode exact invocation report"))
        .transpose()
}

fn writer_handoff(
    app: &Application,
    listeners: &Listeners,
    binding: &crate::domain::AttachmentBinding,
    workspace: &std::path::Path,
) -> Value {
    let process = &binding.process;
    let quote = |value: &str| super::shell_quote(value);
    let attach = format!("{} attach --data-dir {} --session {} --expected-generation {} --expected-epoch {} --expected-pid {} --expected-process-group {} --expected-start-marker {} --expected-observed-at {}",
        quote(&app.executable_path().to_string_lossy()), quote(&app.paths.root.to_string_lossy()),
        quote(&binding.session_id),quote(&binding.role_generation_id),quote(&binding.transcript_epoch),
        process.pid,process.process_group_id,quote(&process.native_start_marker),quote(&process.observed_started_at));
    json!({"instance_id":listeners.instance_id,"data_dir":app.paths.root,
        "repository_worktree":workspace,"hooks_directory":app.paths.hooks,
        "hook_runner":app.hooks.runner,"control_socket":app.paths.control_socket,
        "role_socket":app.paths.role_socket,"session_id":binding.session_id,
        "generation_id":binding.role_generation_id,"epoch":binding.transcript_epoch,"process":process,
        "observe_command":format!("{attach} --view-only"),"control_command":attach,
        "input_lease":{"automatic_input":false,"automatic_trust":false,
            "protocol":"Attachment client: Attach -> AttachmentTranscript; explicit control Attach acquires human input lease; AttachmentSendInput/Renew/Detach on same authenticated connection",
            "manager_action":"Observe actual native prompt, inspect this exact worktree/root and hook definitions before any trust or permission input; record decision and exact input separately. Capture effective model/effort from native identity output; mismatch or absence blocks promotion."}})
}

// Flat task-local artifacts avoid accepting arbitrary manager-supplied paths.
// Open relative to a held directory FD so replaced entries cannot redirect reads.
fn disposition_bytes(root: &std::path::Path, name: &str, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if name.is_empty()
        || name.len() > 200
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || matches!(name, "." | "..")
    {
        bail!("disposition artifacts require a flat task-local filename")
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    let name = std::ffi::CString::new(name)?;
    // The held directory and CString outlive openat; a successful FD is owned
    // exactly once. NOFOLLOW and NONBLOCK reject redirects and avoid FIFO waits.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("open task-local disposition evidence");
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let before = file.metadata()?;
    if !before.is_file() || before.nlink() != 1 || before.len() == 0 || before.len() > limit {
        bail!("disposition evidence must be a nonempty bounded unaliased regular file")
    }
    let mut bytes = Vec::new();
    (&file).take(limit + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len()
        || bytes.len() as u64 > limit
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || after.nlink() != 1
    {
        bail!("disposition evidence changed during bounded read")
    }
    Ok(bytes)
}

fn claude_disposition_contract(first: &Value) -> Value {
    let mut outcomes = serde_json::Map::new();
    for (key, outcome) in [
        ("effective_identity", "matched"),
        ("inline_settings", "matched"),
        ("hook_definitions", "matched"),
        ("hook_runner", "matched"),
        ("native_hook_trust", "trusted"),
        ("native_settings_source", "command_line"),
        ("workspace_trust", "trusted"),
        ("role_channel", "permitted"),
        ("role_operation", "rejected"),
        ("direct_control_socket", "denied"),
        ("owned_shell_read", "permitted"),
        ("owned_shell_write", "permitted"),
        ("owned_builtin_read", "permitted"),
    ] {
        outcomes.insert(
            key.into(),
            json!({"outcome":outcome,"artifact":"","sha256":""}),
        );
    }
    for target in ["original_repository", "service_root", "git_common"] {
        for access in ["shell_read", "shell_write", "builtin_read"] {
            outcomes.insert(
                format!("{target}_{access}"),
                json!({"outcome":"denied","artifact":"","sha256":""}),
            );
        }
    }
    json!({"schema":1,"accepted":true,
        "binding":{"evidence_revision":"claude-325-manual-trust-v1",
            "session_id":first["session_id"],"generation_id":first["generation_id"],
            "epoch":first["epoch"],"operation_id":first["invocation_id"],
            "native_version":first["launch"]["executable_version"],
            "policy_digest":first["launch"]["policy_binding"]["effective_hash"],
            "model":first["launch"]["model"],"effort":first["launch"]["effort"]},
        "outcomes":outcomes})
}

fn read_claude_disposition(root: &std::path::Path, first: &Value) -> Result<Value> {
    let expected = claude_disposition_contract(first);
    let record: Value = serde_json::from_slice(&disposition_bytes(
        root,
        "first-turn-disposition.json",
        32 * 1024,
    )?)
    .context("decode first-turn manager disposition")?;
    if record["schema"] != 1
        || record["accepted"] != true
        || record["binding"] != expected["binding"]
        || expected["binding"]
            .as_object()
            .context("disposition binding")?
            .values()
            .any(|v| v.as_str().is_none_or(str::is_empty))
        || record["outcomes"].as_object().map(|o| o.len())
            != expected["outcomes"].as_object().map(|o| o.len())
    {
        bail!("first-turn disposition missing affirmative acceptance or exact invocation bindings")
    }
    for (key, required) in expected["outcomes"]
        .as_object()
        .context("required outcomes")?
    {
        let outcome = &record["outcomes"][key];
        if outcome["outcome"] != required["outcome"] {
            bail!("first-turn disposition lacks decisive accepted outcome for {key}")
        }
        let artifact = outcome["artifact"]
            .as_str()
            .context("disposition artifact filename")?;
        let digest = outcome["sha256"]
            .as_str()
            .context("disposition evidence digest")?;
        if digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || sha256(disposition_bytes(root, artifact, 1024 * 1024)?) != digest
        {
            bail!("first-turn disposition evidence digest mismatch for {key}")
        }
    }
    Ok(record)
}

fn require_claude_disposition_absent(root: &std::path::Path) -> Result<()> {
    match std::fs::symlink_metadata(root.join("first-turn-disposition.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("check first-turn disposition absence"),
        Ok(_) => bail!("first-turn disposition must be absent before gate readiness"),
    }
}

async fn await_claude_disposition(
    root: &std::path::Path,
    first: &Value,
    deadline: Instant,
) -> Result<Value> {
    let record = loop {
        if Instant::now() >= deadline {
            bail!("first-turn disposition gate expired")
        }
        match std::fs::symlink_metadata(root.join("first-turn-disposition.json")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::time::sleep(stage_wait(deadline, Instant::now(), POLL)).await;
            }
            Err(error) => return Err(error).context("inspect first-turn disposition"),
            Ok(_) => break read_claude_disposition(root, first)?,
        }
    };
    if Instant::now() >= deadline {
        bail!("first-turn disposition gate expired during validation")
    }
    Ok(record)
}

fn publish_writer_handoff(
    app: &Application,
    listeners: &Listeners,
    context: &crate::store::SingleAgentLaunchContext,
    process: &crate::domain::ProcessIdentity,
    evidence: &mut Value,
) -> Result<()> {
    let binding = crate::domain::AttachmentBinding {
        session_id: context.session_id.clone(),
        role_generation_id: context.role_generation_id.clone(),
        transcript_epoch: context.transcript_epoch.clone(),
        process: process.clone(),
    };
    evidence["operator_handoff"] = writer_handoff(app, listeners, &binding, &context.workspace);
    if context.resume.is_none()
        && current_claude_writer(
            context.provider,
            evidence["launch"]["executable_version"].as_str(),
            &context.model,
            &context.effort,
        )
    {
        evidence["operator_handoff"]["first_turn_disposition"] = json!({
            "path":app.paths.root.parent().context("task-local root")?.join("first-turn-disposition.json"),
            "required_record":claude_disposition_contract(evidence),
            "publication":"Wait for first_turn_gate readiness after cleanup and nonce verification, then inspect within its 120-second deadline. Inspector must be quiescent before publication. Write artifacts first, then atomically rename the complete disposition record last in the same folder. Only a missing record waits; an invalid record fails immediately. Historic attach commands are invalid at gate readiness.",
            "artifact_contract":"Flat filenames in the task directory only; nonempty regular non-symlink single-link files, at most 1 MiB each. Record at most 32 KiB. SHA-256 of exact bytes. Export decisive native tool/identity/trust evidence slices with invocation correlation; generic failure, requested settings or model prose are insufficient.",
            "qualification":false});
    }
    let path = app
        .paths
        .root
        .parent()
        .context("owned fixture parent")?
        .join(format!("operator-{}.json", context.transcript_epoch));
    crate::config::atomic_write(&path, &serde_json::to_vec_pretty(evidence)?)?;
    eprintln!("Native qualification operator handoff: {}", path.display());
    Ok(())
}

async fn writer_network_control(evidence: &mut Value, work_deadline: Instant) -> Result<()> {
    let mut command = std::process::Command::new("/usr/bin/curl");
    command
        .args([
            "--disable",
            "--head",
            "--fail",
            "--silent",
            "--output",
            "/dev/null",
            "--max-time",
            "10",
            "--retry",
            "0",
            "--",
            "https://example.com",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = FixtureChild(
        command
            .spawn()
            .context("manager HTTPS HEAD positive control")?,
    );
    let deadline = work_deadline.min(Instant::now() + Duration::from_secs(12));
    loop {
        if let Some(status) = child.0.try_wait()? {
            *evidence = json!({"url":"https://example.com","method":"HEAD",
                "observed_at":chrono::Utc::now().to_rfc3339(),"exit_code":status.code(),
                "http_success":status.success(),"no_retry":true});
            if !status.success() {
                bail!("Manager HTTPS positive control unavailable; no native submission")
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            *evidence = json!({"url":"https://example.com","method":"HEAD",
                "http_success":false,"deadline_reached":true,"no_retry":true});
            bail!("Manager HTTPS positive control deadline; no native submission")
        }
        tokio::time::sleep(stage_wait(deadline, Instant::now(), POLL)).await;
    }
}

fn writer_startup_observation(
    connection: &rusqlite::Connection,
    session: &str,
    generation: &str,
    epoch: &str,
    boundary: i64,
) -> Result<Option<Value>> {
    // Ingestion latches identity at SessionStart. A retained UserPromptSubmit
    // can carry the old native identity before this invocation's SessionStart.
    let raw: Option<String> = connection
        .query_row(
            "SELECT json_object('hook_event_id',h.id,'hook_rowid',h.rowid,
           'event_name',h.event_name,'received_at',h.received_at,
           'audit_event_id',a.id,'audit_created_at',a.created_at,
           'native_session_id',s.native_session_id,'provenance_state',h.provenance_state)
         FROM sessions s JOIN hook_events h ON h.session_id=s.id
           AND h.role_generation_id=s.role_generation_id AND h.provider=s.provider
           AND h.native_session_id=s.native_session_id
           AND h.event_name='SessionStart' AND h.rowid>?4
           AND h.provenance_state='managed_process_group_untrusted_payload'
         JOIN audit_events a ON a.entity_id=s.id AND a.actor_id=s.role_generation_id
           AND a.actor_kind='hook' AND a.event_code='provider.hook.received'
           AND json_extract(a.detail_json,'$.hook_event_id')=h.id
         WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
           AND s.provider='codex' AND s.status='running'
           AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
         ORDER BY h.rowid,a.rowid LIMIT 1",
            params![session, generation, epoch, boundary],
            |row| row.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).context("decode writer startup observation"))
        .transpose()
}

async fn await_writer_startup(
    app: &Application,
    context: &crate::store::SingleAgentLaunchContext,
    boundary: i64,
    timing: &mut WriterTiming,
    evidence: &mut Value,
) -> Result<()> {
    loop {
        timing.check(Instant::now())?;
        app.supervisor.reconcile()?;
        let observation = {
            let connection = app.store.lock()?;
            writer_startup_observation(
                &connection,
                &context.session_id,
                &context.role_generation_id,
                &context.transcript_epoch,
                boundary,
            )?
        };
        if let Some(observation) = observation {
            timing.start_execution(Instant::now())?;
            evidence["authenticated_startup"] = observation;
            writer_milestone(evidence, "authenticated_startup_observed", timing.started);
            evidence["deadline"]["active_budget"] = json!("execution");
            evidence["deadline"]["execution_deadline_elapsed_seconds"] = json!(timing
                .deadline()
                .duration_since(timing.started)
                .as_secs_f64());
            evidence["deadline"]["execution_deadline_at"] = json!((chrono::Utc::now()
                + chrono::Duration::milliseconds(
                    timing
                        .deadline()
                        .saturating_duration_since(Instant::now())
                        .as_millis() as i64
                ))
            .to_rfc3339());
            return Ok(());
        }
        if session_state(app, &context.session_id)?.0 != "running" {
            bail!("writer stopped before authenticated invocation startup")
        }
        tokio::time::sleep(stage_wait(timing.deadline(), Instant::now(), POLL)).await;
    }
}

fn writer_stop_observation(
    connection: &rusqlite::Connection,
    session: &str,
    generation: &str,
    epoch: &str,
    boundary: i64,
    deadline: Instant,
) -> Result<Option<Value>> {
    if Instant::now() >= deadline {
        bail!("authenticated writer Stop observation was unavailable within the deadline")
    }
    let raw: Option<String> = connection.query_row(
        "SELECT json_object('session_start_hook_id',start.id,'stop_hook_id',stop.id,
           'native_session_id',s.native_session_id,'safe_idle_boundary',
           json(CASE json_extract(a.detail_json,'$.safe_idle_boundary') WHEN 1 THEN 'true' WHEN 0 THEN 'false' ELSE 'null' END),
           'readiness_state',s.readiness_state)
         FROM sessions s JOIN hook_events start ON start.session_id=s.id
           AND start.role_generation_id=s.role_generation_id AND start.provider=s.provider
           AND start.native_session_id=s.native_session_id AND start.event_name='SessionStart'
           AND start.rowid>?4 AND start.provenance_state='managed_process_group_untrusted_payload'
         JOIN hook_events stop ON stop.session_id=s.id AND stop.role_generation_id=s.role_generation_id
           AND stop.provider=s.provider AND stop.native_session_id=s.native_session_id
           AND stop.event_name='Stop' AND stop.rowid>start.rowid
           AND stop.provenance_state='managed_process_group_untrusted_payload'
         JOIN audit_events a ON a.entity_id=s.id AND a.actor_id=s.role_generation_id
           AND a.event_code='provider.hook.received' AND json_extract(a.detail_json,'$.hook_event_id')=stop.id
         WHERE s.id=?1 AND s.role_generation_id=?2 AND s.transcript_epoch=?3
           AND s.status='running' AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
           AND EXISTS(SELECT 1 FROM audit_events started WHERE started.entity_id=s.id
             AND started.actor_id=s.role_generation_id AND started.event_code='provider.hook.received'
             AND json_extract(started.detail_json,'$.hook_event_id')=start.id)
         ORDER BY stop.rowid DESC LIMIT 1",
        params![session, generation, epoch, boundary],
        |row| row.get(0),
    ).optional()?;
    raw.map(|raw| serde_json::from_str(&raw).context("decode writer Stop observation"))
        .transpose()
}

async fn completed_writer_turn(
    app: &Application,
    context: &crate::store::SingleAgentLaunchContext,
    boundary: i64,
    deadline: Instant,
    report_operation: Option<&str>,
    report_boundary: i64,
    interrupted_writer: bool,
) -> Result<Value> {
    loop {
        app.supervisor.reconcile()?;
        if interrupted_writer {
            if Instant::now() >= deadline {
                bail!("authenticated writer turn boundary was unavailable within the deadline")
            }
            if let Some(operation) = report_operation {
                if let Some(report) = invocation_report(app, context, operation, report_boundary)? {
                    if report["outcome"] != "result_ready" {
                        bail!("Final writer invocation requires its exact authenticated result_ready report")
                    }
                    let receipt = {
                        let connection = app.store.lock()?;
                        crate::store::eligible_single_agent_stop(&connection, &context.attempt_id)?
                    }
                    .context("Exact reporting turn is not eligible to stop")?;
                    if receipt.session_id != context.session_id
                        || receipt.generation_id != context.role_generation_id
                        || receipt.transcript_epoch != context.transcript_epoch
                        || report["id"].as_str() != Some(receipt.role_result_id.as_str())
                    {
                        bail!("Reporting stop receipt differs from this invocation")
                    }
                    if !app.supervisor.interrupt_single_agent_report(&receipt)? {
                        bail!("Exact reporting stop receipt changed; no fallback")
                    }
                    return Ok(json!({"kind":"authenticated_report_interruption",
                        "role_result_id":receipt.role_result_id,"completed_provider_turn":false,
                        "tool_outcome":"unresolved","report_stop_reserved":true}));
                }
            } else if let Some(observation) = {
                let connection = app.store.lock()?;
                writer_stop_observation(
                    &connection,
                    &context.session_id,
                    &context.role_generation_id,
                    &context.transcript_epoch,
                    boundary,
                    deadline,
                )?
            } {
                // Stop is an observation point only. Production capture/interrupt
                // and independent process quiescence establish the retained boundary.
                return Ok(
                    json!({"kind":"planned_interruption","observation":observation,
                    "completed_provider_turn":false,"tool_outcome":"unresolved"}),
                );
            }
            if session_state(app, &context.session_id)?.0 != "running" {
                bail!("writer stopped before its exact interruption boundary")
            }
            tokio::time::sleep(stage_wait(deadline, Instant::now(), POLL)).await;
            continue;
        }
        let completed: bool = app.store.lock()?.query_row(
            "SELECT s.status='running' AND s.transcript_epoch=?2 AND s.readiness_state='idle_candidate'
               AND s.native_session_id IS NOT NULL AND s.native_session_id!=''
               AND EXISTS(SELECT 1 FROM hook_events h WHERE h.session_id=s.id AND h.rowid>?3
                 AND h.event_name='SessionStart' AND h.native_session_id=s.native_session_id)
               AND EXISTS(SELECT 1 FROM hook_events h JOIN audit_events a ON a.entity_id=s.id
                 AND json_extract(a.detail_json,'$.hook_event_id')=h.id
                 WHERE h.session_id=s.id AND h.rowid>?3 AND h.event_name='Stop'
                   AND h.native_session_id=s.native_session_id AND a.event_code='provider.hook.received'
                   AND json_extract(a.detail_json,'$.safe_idle_boundary')=1)
             FROM sessions s WHERE s.id=?1",
            params![context.session_id, context.transcript_epoch, boundary], |row| row.get(0),
        )?;
        let reported = match report_operation {
            Some(operation) => match invocation_report(app, context, operation, report_boundary)? {
                Some(report) if report["outcome"] == "result_ready" => true,
                Some(_) => bail!(
                    "Final writer invocation requires its exact authenticated result_ready report"
                ),
                None => false,
            },
            None => true,
        };
        if completed && reported {
            return Ok(json!({"kind":"completed_provider_turn","completed_provider_turn":true}));
        }
        if session_state(app, &context.session_id)?.0 != "running" {
            bail!("writer stopped before an authenticated completed-turn boundary")
        }
        if Instant::now() >= deadline {
            bail!("authenticated writer turn completion was unavailable within the deadline")
        }
        tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

async fn writer_turn(
    app: &Application,
    listeners: &mut Listeners,
    targets: &[(&str, PathBuf)],
    prior: Option<(&crate::store::SingleAgentLaunchContext, &str)>,
    prompt: &str,
    evidence: &mut Value,
) -> Result<crate::store::SingleAgentLaunchContext> {
    let invocation_started = Instant::now();
    let invocation_utc = chrono::Utc::now();
    let mut timing = WriterTiming {
        started: invocation_started,
        execution_deadline: None,
    };
    let mut work_deadline = invocation_started + WRITER_WORK;
    let legacy_cleanup_deadline = work_deadline + WRITER_CLEANUP;
    listeners.cleanup_deadline = Some(legacy_cleanup_deadline);
    use crate::workflow::SingleAgentResumeMode::Automatic;
    let main = {
        let connection = app.store.lock()?;
        let policy: String = connection.query_row(
            "SELECT policy_json FROM workspaces WHERE attempt_id=?1",
            params![ATTEMPT],
            |row| row.get(0),
        )?;
        crate::workspace::policy_mode(&serde_json::from_str(&policy)?)?
            == crate::domain::WorkspaceMode::MainCheckout
    };
    let purpose: crate::domain::ProfilePurpose = app.store.lock()?.query_row("SELECT r.purpose FROM workflow_run_profiles rp JOIN agent_profile_revisions r ON r.id=rp.profile_revision_id WHERE rp.attempt_id=?1",[ATTEMPT],|row|row.get::<_,String>(0))?.parse().map_err(|error: String|anyhow!(error))?;
    let context = if main {
        if prior.is_some() {
            app.store.lock()?.execute(
                "UPDATE attempts SET status='needs_input' WHERE id=?1",
                params![ATTEMPT],
            )?;
            app.store.lock()?.execute(
                "UPDATE tasks SET attention='needs_input' WHERE id='diagnostic-task'",
                [],
            )?;
        }
        app.store
            .single_agent_verification_context(ATTEMPT, prior.is_some())?
    } else {
        match prior {
            Some(_) => app.store.single_agent_resume_context(ATTEMPT, Automatic)?,
            None => app.store.single_agent_launch_context(ATTEMPT)?,
        }
    };
    if let Some((previous, native_id)) = prior {
        let target = context.resume.as_ref().context("retained resume target")?;
        if context.session_id != previous.session_id
            || context.role_generation_id != previous.role_generation_id
            || context.workspace != previous.workspace
            || context.read_denials != previous.read_denials
            || target.transcript_epoch != previous.transcript_epoch
            || target.native_session_id != native_id
            || context.transcript_epoch == previous.transcript_epoch
            || context.credential_id == previous.credential_id
        {
            bail!("resume prerequisites changed; the resumed call was not consumed")
        }
    }
    let bundles = match std::env::var("LLMRELAY_NATIVE_QUALIFICATION_EXACT_VERSION") {
        Ok(version) => BundleSet::provisional_native_fixture_for(context.provider, &version)?,
        Err(_) => BundleSet::embedded(),
    };
    let executable = app.executable_path().canonicalize()?;
    let checkout_scope = if main {
        Some(super::checkout_launch_scope(
            &context.workspace_policy,
            &app.paths.root,
            &app.paths.role_socket,
            &app.store.repository_inventory()?,
            &executable,
        )?)
    } else {
        None
    };
    if main {
        app.store.lock()?.execute("UPDATE controls SET state=?1,payload_json=json_set(payload_json,'$.prompt_hash',?2,'$.phase',?3) WHERE id='diagnostic-main-verify'",
        params![if prior.is_some(){"native_running"}else{"native_requested"},crate::store::json_hash(&prompt)?,if prior.is_some(){"recall"}else{"first"}])?;
    }
    let current = context.provider == Provider::Codex
        && std::env::var("LLMRELAY_NATIVE_QUALIFICATION_EXACT_VERSION").as_deref()
            == Ok("codex-cli 0.167.0");
    let current_claude = !main
        && purpose.is_writer()
        && current_claude_writer(
            context.provider,
            std::env::var("LLMRELAY_NATIVE_QUALIFICATION_EXACT_VERSION")
                .ok()
                .as_deref(),
            &context.model,
            &context.effort,
        );
    let report_required = (current || current_claude) && prior.is_some();
    let interrupted_writer = current
        && !main
        && purpose.is_writer()
        && context.model == "gpt-6.1-sol"
        && context.effort == "medium";
    if interrupted_writer {
        work_deadline = timing.deadline();
        listeners.cleanup_deadline = Some(timing.overall());
    }
    let operation = format!("native-qualification-{}", uuid::Uuid::new_v4());
    let report_boundary: i64 = app.store.lock()?.query_row(
        "SELECT COALESCE(MAX(rowid),0) FROM role_results",
        [],
        |row| row.get(0),
    )?;
    let bound_prompt = if report_required {
        format!("{prompt}\nFor this invocation only, the authenticated report must use operation_id {operation}. Read actual role context and submit its required outcome/acceptance metadata. Include the exact evidence entry LLMRELAY-NATIVE-INVOCATION:{operation}:{}. Never reuse an earlier operation or report.", context.transcript_epoch)
    } else {
        prompt.to_owned()
    };
    let prompt = bound_prompt.as_str();
    let request = super::SingleAgentLaunchRequest {
        purpose: context.purpose,
        provider: context.provider,
        model: &context.model,
        effort: &context.effort,
        cwd: &context.workspace,
        read_denials: &context.read_denials,
        service_root: &app.paths.root,
        prompt,
        role_socket: &app.paths.role_socket,
        token: &context.token,
        role_generation_id: &context.role_generation_id,
        session_id: &context.session_id,
        native_session_id: context
            .resume
            .as_ref()
            .map(|target| target.native_session_id.as_str()),
        assets: &app.hooks,
        executable_path: &executable,
        bundles: &bundles,
        checkout_scope: checkout_scope.as_ref(),
    };
    super::require_provider_word("model", request.model)?;
    super::require_provider_word("effort", request.effort)?;
    let native = match context.provider {
        Provider::Codex => super::codex::single_agent_native(&request)?,
        Provider::Claude => super::claude::single_agent_native(&request)?,
    };
    if let Ok(expected) = std::env::var("LLMRELAY_NATIVE_QUALIFICATION_EXACT_VERSION") {
        if native.executable_version != expected {
            bail!("Native executable differs from the exact acquisition selector; no fallback")
        }
    }
    let binding = bundles.resolve_policy(
        context.provider,
        &native.executable_version,
        crate::provider_compatibility::ExecutionPolicy::for_purpose(
            context.provider,
            context.purpose,
        )
        .mode,
    )?;
    if binding.synthetic_origin
        || (!main
            && !matches!(
                binding.qualification,
                PolicyQualification::Unqualified { .. }
            ))
    {
        bail!("writer evidence requires the real embedded Unqualified policy")
    }
    let mut launch = super::assemble_single_agent_launch(&request, native, &binding)?;
    launch.record["prompt_hash"] = json!(crate::store::json_hash(&prompt)?);
    if prior.is_some() {
        let prior_key: String = app.store.lock()?.query_row(
            "SELECT capability_key FROM sessions WHERE id=?1",
            params![context.session_id],
            |row| row.get(0),
        )?;
        if prior_key != context.capability_key_for(&launch.identity)? {
            bail!("profile or native identity changed before resume; the resumed call was not consumed")
        }
    }
    let boundary: i64 = app.store.lock()?.query_row(
        "SELECT COALESCE(MAX(rowid),0) FROM hook_events",
        [],
        |row| row.get(0),
    )?;
    let started = chrono::Utc::now().to_rfc3339();
    let mut filesystem_targets = serde_json::Map::new();
    for (tag, path) in targets {
        let bytes =
            owned_sentinel(path).with_context(|| format!("prepare {tag} fixture sentinel"))?;
        filesystem_targets.insert((*tag).to_owned(), json!({"path": path,"canonical_path":path.canonicalize()?,"host_readable":true,
            "before_sha256": sha256(&bytes), "required_accesses": ["read", "write"],
            "read_outcome": "unavailable until exact native tool outcome is independently reviewed",
            "file_tool_read_outcome": if context.provider == Provider::Claude {
                "unavailable until the exact built-in Read outcome is independently reviewed"
            } else { "not requested for this provider" },
            "write_outcome": "unavailable until exact native tool outcome is independently reviewed"}));
    }
    *evidence = json!({"session_id": context.session_id, "generation_id": context.role_generation_id,
        "epoch": context.transcript_epoch, "credential_id": context.credential_id, "hook_boundary": boundary,
        "launch": launch.record, "native_identity": launch.identity, "started_at": started,
        "filesystem_targets": filesystem_targets,
        "transcript": app.paths.transcripts.join(format!("{}.jsonl", context.session_id))});
    if interrupted_writer {
        evidence["timing_milestones"]["invocation_started"] = json!({
            "elapsed_seconds":0.0,"observed_at":invocation_utc.to_rfc3339()});
        writer_milestone(evidence, "preparation_completed", invocation_started);
    }
    evidence["invocation_id"] = json!(operation);
    evidence["report_binding"] = json!({"operation_id":operation,"after_rowid":report_boundary,
        "required":report_required,"epoch":context.transcript_epoch});
    evidence["deadline"] = if interrupted_writer {
        json!({"startup_seconds":600,"execution_seconds":480,"cleanup_seconds":240,
            "absolute_maximum_seconds":1320,"active_budget":"startup",
            "startup_deadline_at":(invocation_utc + chrono::Duration::seconds(600)).to_rfc3339(),
            "absolute_deadline_at":(invocation_utc + chrono::Duration::seconds(1320)).to_rfc3339(),
            "trust_and_permission_included":true,"qualification":"pending_independent_native_review"})
    } else {
        json!({"work_seconds":480,"cleanup_seconds":240,"trust_and_permission_included":true})
    };
    if interrupted_writer {
        evidence["completed_provider_turn"] = json!(false);
    }
    evidence["role_context_read"] = json!({"outcome":"pending_independent_native_review",
        "source":"actual native role context tool response in this epoch transcript; report alone is insufficient"});
    evidence["effective_identity"] = json!({"status":"pending_independent_native_review",
        "requested_model":context.model,"requested_effort":context.effort,
        "source":"exact epoch transcript/native SessionStart and manager-observed identity; request alone is insufficient",
        "fallback_permitted":false});
    drop(
        listeners
            .negotiate_until(app, &mut evidence["control_handshake"], work_deadline)
            .await?,
    );
    if interrupted_writer {
        writer_milestone(evidence, "handshake_completed", invocation_started);
    }
    if current {
        writer_network_control(
            &mut evidence["manager_network_positive_control"],
            work_deadline,
        )
        .await?;
        if interrupted_writer {
            writer_milestone(evidence, "network_control_completed", invocation_started);
        }
        evidence["network_probe"] = json!({"url":"https://example.com","method":"HEAD",
            "outcome":"pending_independent_native_review"});
        evidence["direct_socket_probe"] = json!({"target":app.paths.control_socket,
            "outcome":"pending_independent_native_review","application_handler_rejection_is_denial":false});
    }
    if current_claude {
        evidence["direct_socket_probe"] = json!({"target":app.paths.control_socket,
            "outcome":"pending_independent_native_review","application_handler_rejection_is_denial":false,
            "positive_control":"authenticated role context response"});
    }
    if interrupted_writer {
        timing.check(Instant::now())?;
    }
    if Instant::now() >= work_deadline {
        bail!("Invocation work deadline reached before native submission")
    }
    if prior.is_some() {
        app.store.reserve_single_agent_resume(
            &context,
            if main {
                crate::workflow::SingleAgentResumeMode::Explicit
            } else {
                Automatic
            },
            &launch,
            None,
        )?;
    } else {
        app.store
            .reserve_single_agent_invocation(&context, &launch)?;
    }
    if interrupted_writer {
        timing.check(Instant::now())?;
        writer_milestone(evidence, "spawn_started", invocation_started);
    }
    let mut guard = SpawnedSession {
        supervisor: app.supervisor.clone(),
        session: context.session_id.clone(),
        armed: true,
    };
    if current_claude {
        evidence["submission_attempt"] = json!({"charged":true,
            "kind":if prior.is_some(){"resume"}else{"initial"},
            "delivery":"attempted; ambiguity forbids replacement", "accounting_owner":"manager"});
        let path = app
            .paths
            .root
            .parent()
            .context("task-local fixture root")?
            .join(format!("submission-{}.json", context.transcript_epoch));
        crate::config::atomic_write(&path, &serde_json::to_vec_pretty(evidence)?)?;
    }
    // All fallible work after delivery is contained until exact cleanup runs.
    let turn = match app.supervisor.spawn_process(
        &context.session_id,
        &SessionOwner::RoleGeneration(context.role_generation_id.clone()),
        &context.transcript_epoch,
        &launch.process(),
    ) {
        Ok(process) => {
            if interrupted_writer {
                writer_milestone(evidence, "spawn_completed", invocation_started);
            }
            let startup = async {
                app.store.update_session_running(
                    &context.session_id,
                    &context.transcript_epoch,
                    &serde_json::to_string(&process)?,
                )?;
                if interrupted_writer {
                    evidence["operator_timing"] = json!({"deadline":evidence["deadline"],
                        "required_manager_milestones":["attachment_completed_at","trust_prompt_inspected_at","trust_decision_and_exact_input_at"],
                        "attachment_and_trust_records":"Manager must record actual attachment, inspected trust decision and exact input UTC timestamps; no automatic input or trust is supplied.",
                        "startup_boundary":"Exact audited current-invocation SessionStart; one transition only; expiry fails without resume"});
                }
                publish_writer_handoff(app, listeners, &context, &process, evidence)?;
                if interrupted_writer {
                    writer_milestone(evidence, "operator_handoff_published", invocation_started);
                    await_writer_startup(app, &context, boundary, &mut timing, evidence).await?;
                }
                Ok::<_, anyhow::Error>(())
            };
            let startup = if interrupted_writer {
                tokio::time::timeout_at(tokio::time::Instant::from_std(work_deadline), startup)
                    .await
                    .unwrap_or_else(|_| {
                        Err(anyhow!(
                            "Writer startup allowance expired; no resume or retry"
                        ))
                    })
            } else {
                startup.await
            };
            if let Err(error) = startup {
                Err(error)
            } else {
                let execution_deadline = if interrupted_writer {
                    timing.deadline()
                } else {
                    work_deadline
                };
                let execution = async {
                    evidence["turn_boundary"] = completed_writer_turn(
                        app,
                        &context,
                        boundary,
                        execution_deadline,
                        report_required.then_some(operation.as_str()),
                        report_boundary,
                        interrupted_writer,
                    )
                    .await?;
                    if report_required {
                        evidence["authenticated_report"] =
                            invocation_report(app, &context, &operation, report_boundary)?
                                .context("Exact invocation report disappeared")?;
                        if evidence["authenticated_report"]["outcome"] != "result_ready" {
                            bail!("Final writer invocation requires its exact authenticated result_ready report")
                        }
                    }
                    evidence["completed_provider_turn"] =
                        evidence["turn_boundary"]["completed_provider_turn"].clone();
                    if prior.is_none() {
                        let captured =
                            crate::recovery::capture_desired_running_before_drain(&app.store)?;
                        evidence["desired_running_capture"] = json!(captured);
                        if !captured.contains(&context.session_id) {
                            bail!("planned shutdown did not capture this still-running writer")
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                };
                if interrupted_writer {
                    match tokio::time::timeout_at(
                        tokio::time::Instant::from_std(execution_deadline),
                        execution,
                    )
                    .await
                    {
                        Ok(Ok(())) => timing.check(Instant::now()),
                        Ok(Err(error)) => Err(error),
                        Err(_) => Err(anyhow!(
                            "Writer execution allowance expired; no resume or retry"
                        )),
                    }
                } else {
                    execution.await
                }
            }
        }
        Err(failure) => {
            evidence["spawn_failure"] =
                spawn_failure(app, &context.session_id, &failure, prior.is_some());
            Err(anyhow!(
                "writer spawn failed; no replacement: {}",
                failure.reason()
            ))
        }
    };
    if let Err(error) = &turn {
        evidence["turn_error"] = json!(format!("{error:#}"));
    }
    let cleanup_started = Instant::now();
    let cleanup_deadline = if interrupted_writer {
        writer_milestone(evidence, "boundary_or_error", invocation_started);
        evidence["deadline"]["expired_budget"] = if cleanup_started >= timing.deadline() {
            evidence["deadline"]["active_budget"].clone()
        } else {
            Value::Null
        };
        let deadline = timing.cleanup_deadline(cleanup_started);
        evidence["deadline"]["cleanup_deadline_elapsed_seconds"] =
            json!(deadline.duration_since(invocation_started).as_secs_f64());
        evidence["deadline"]["cleanup_deadline_at"] = json!((chrono::Utc::now()
            + chrono::Duration::milliseconds(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as i64
            ))
        .to_rfc3339());
        writer_milestone(evidence, "cleanup_started", invocation_started);
        deadline
    } else {
        legacy_cleanup_deadline
    };
    listeners.cleanup_deadline = Some(cleanup_deadline);
    let mut errors = Vec::new();
    evidence["stop"] =
        stop_owned_until(app, &context.session_id, &mut errors, cleanup_deadline).await;
    guard.armed = evidence["stop"]["quiescence"]["verified"] != true;
    // The process guard remains owned through listener shutdown and joins,
    // including unresolved cleanup. No subsequent invocation may follow failure.
    evidence["listener_cleanup"] = listeners.close_until(cleanup_deadline).await;
    if interrupted_writer {
        writer_milestone(evidence, "cleanup_completed", invocation_started);
    }
    evidence["recovery_required"] = json!(guard.armed);
    evidence["exact_guard_fallback_required"] = json!(guard.armed);
    evidence["deadline"]["elapsed_seconds"] = json!(invocation_started.elapsed().as_secs_f64());
    evidence["deadline"]["within_cleanup_bound"] = json!(Instant::now() <= cleanup_deadline);
    evidence["lifecycle_errors"] = json!(errors);
    evidence["ended_at"] = json!(chrono::Utc::now().to_rfc3339());
    for (tag, path) in targets {
        evidence["filesystem_targets"][*tag]["after"] = if evidence["stop"]["quiescence"]
            ["verified"]
            == true
        {
            match owned_sentinel(path) {
                Ok(bytes) => {
                    let hash = sha256(&bytes);
                    json!({"sha256": hash,
                        "unchanged": evidence["filesystem_targets"][*tag]["before_sha256"] == hash})
                }
                Err(error) => json!({"unavailable": format!("{error:#}")}),
            }
        } else {
            json!({"unavailable": "owned process quiescence was not verified; sentinel was not read"})
        };
    }
    evidence["hooks"] = json!(writer_hooks(app, &context, boundary)?);
    evidence["native_session_id"] = json!(app.store.lock()?.query_row(
        "SELECT native_session_id FROM sessions WHERE id=?1",
        params![context.session_id],
        |row| row.get::<_, Option<String>>(0),
    )?);
    let ended = evidence["ended_at"]
        .as_str()
        .context("turn end timestamp")?;
    evidence["handler_rejections"] = json!(app
        .diagnostics
        .read_sanitized(2000)?
        .into_iter()
        .filter(|event| {
            matches!(
                event["event_code"].as_str(),
                Some("role.ipc.rejected" | "control.ipc.rejected")
            ) && event["timestamp"]
                .as_str()
                .is_some_and(|timestamp| timestamp >= started.as_str() && timestamp <= ended)
        })
        .collect::<Vec<_>>());
    evidence["deadline"]["elapsed_seconds"] = json!(invocation_started.elapsed().as_secs_f64());
    evidence["deadline"]["within_cleanup_bound"] = json!(Instant::now() <= cleanup_deadline);
    turn?;
    if !errors.is_empty()
        || evidence["listener_cleanup"]["bounded_shutdown_and_joins"] != true
        || evidence["deadline"]["within_cleanup_bound"] != true
        || evidence["stop"]["quiescence"]["verified"] != true
        || !(evidence["stop"]["stopped_by"] == "interrupt"
            || (evidence["turn_boundary"]["report_stop_reserved"] == true
                && evidence["stop"]["stopped_by"] == "already_exited_before_cleanup"))
        || evidence["stop"]["session"]["status"] != "exited"
    {
        bail!("writer planned interruption or quiescence was incomplete; no resume or replacement")
    }
    Ok(context)
}

fn owned_sentinel(path: &std::path::Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > 128 {
        bail!("fixture sentinel is not a bounded, unaliased regular file")
    }
    Ok(std::fs::read(path)?)
}

async fn acquire_writer_pair(
    app: &Application,
    listeners: &mut Listeners,
    targets: &[(&str, PathBuf)],
    evidence: &mut Value,
) -> Result<()> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let first_prompt = format!("{}\n\nDisposable native diagnostic: follow the task's two-turn instructions instead of submitting a result. First-turn nonce: {nonce}",
        crate::workflow_resources::SINGLE_AGENT_PROMPT.trim());
    evidence["expected_nonce_sha256"] = json!(sha256(&nonce));
    let first = writer_turn(
        app,
        listeners,
        targets,
        None,
        &first_prompt,
        &mut evidence["first"],
    )
    .await?;
    let first_path = first.workspace.join("first.txt");
    let bytes = owned_sentinel(&first_path)?;
    evidence["first"]["owned_write_sha256"] = json!(sha256(&bytes));
    if bytes != nonce.as_bytes() {
        bail!("first native writer did not write the nonce exactly")
    }
    let current_claude = current_claude_writer(
        first.provider,
        evidence["first"]["launch"]["executable_version"].as_str(),
        &first.model,
        &first.effort,
    );
    if !current_claude {
        std::fs::remove_file(&first_path)?;
    }
    let native_id = evidence["first"]["native_session_id"]
        .as_str()
        .context("first native conversation")?
        .to_owned();
    if current_claude {
        let revoked: bool = app.store.lock()?.query_row(
            "SELECT revoked_at IS NOT NULL FROM role_credentials WHERE id=?1",
            params![first.credential_id],
            |row| row.get(0),
        )?;
        if !revoked
            || evidence["first"]["stop"]["quiescence"]["verified"] != true
            || evidence["first"]["listener_cleanup"]["bounded_shutdown_and_joins"] != true
        {
            bail!("first-turn gate requires quiescence, joined listeners and revoked credential")
        }
        let root = app.paths.root.parent().context("task-local root")?;
        require_claude_disposition_absent(root)?;
        let deadline = Instant::now() + FIRST_TURN_GATE;
        let gate_started_at = chrono::Utc::now().to_rfc3339();
        let event_counts = || -> Result<(i64, usize)> {
            let hooks = app.store.lock()?.query_row(
                "SELECT COUNT(*) FROM hook_events WHERE session_id=?1",
                params![first.session_id],
                |row| row.get(0),
            )?;
            let diagnostics = app.diagnostics.read_sanitized(2000)?;
            if diagnostics.len() >= 2000 {
                bail!("first-turn gate diagnostic window is full")
            }
            let rejections = diagnostics
                .iter()
                .filter(|event| {
                    matches!(
                        event["event_code"].as_str(),
                        Some("role.ipc.rejected" | "control.ipc.rejected")
                    ) && event["timestamp"]
                        .as_str()
                        .is_some_and(|at| at >= gate_started_at.as_str())
                })
                .count();
            Ok((hooks, rejections))
        };
        let baseline = event_counts()?;
        if baseline.1 != 0 {
            bail!("first-turn gate observed a new rejection")
        }
        let deadline_at = (chrono::Utc::now()
            + chrono::Duration::milliseconds(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as i64,
            ))
        .to_rfc3339();
        evidence["first"]["first_turn_gate"] = json!({"listeners_closed":true,
            "credential_revoked":true,"first_txt_sha256":sha256(&bytes),"deadline_at":deadline_at,
            "budget_seconds":FIRST_TURN_GATE.as_secs(),"attach_commands_valid":false,
            "record_absent_at_gate":true,"started_at":gate_started_at,"event_baseline":baseline});
        crate::config::atomic_write(
            &root.join(format!("operator-{}.json", first.transcript_epoch)),
            &serde_json::to_vec_pretty(&evidence["first"])?,
        )?;
        evidence["first_turn_manager_disposition"] =
            await_claude_disposition(root, &evidence["first"], deadline).await?;
        if event_counts()? != baseline {
            bail!("first-turn gate hook or rejection events changed")
        }
        if owned_sentinel(&first_path)? != nonce.as_bytes() {
            bail!("first-turn nonce changed during disposition gate")
        }
        std::fs::remove_file(&first_path)?;
    }
    *listeners = Listeners::start(app)?;
    let resumed = writer_turn(app, listeners, targets, Some((&first, &native_id)),
        "The host captured planned shutdown intent and interrupted the interactive process, then independently verified process-group quiescence. Prior tool outcomes may remain unresolved. Check actual effects before repeating anything. Read role context, perform only the resumed diagnostic instructions, and recall the earlier nonce into recall.txt without inspecting prior files or fixture records. For current Codex and current exact Claude writer qualification finish with result_ready through the authenticated role report, using this invocation's operation ID and complete acceptance rows from current context. Legacy diagnostics finish without a report.",
        &mut evidence["resume"]).await?;
    let recalled = owned_sentinel(&resumed.workspace.join("recall.txt"))?;
    evidence["resume"]["owned_recall_sha256"] = json!(sha256(&recalled));
    let old_revoked: bool = app.store.lock()?.query_row(
        "SELECT revoked_at IS NOT NULL FROM role_credentials WHERE id=?1",
        params![first.credential_id],
        |row| row.get(0),
    )?;
    evidence["checks"] = json!({
        "owned_first_write": bytes == nonce.as_bytes(), "retained_recall_candidate": recalled == nonce.as_bytes(),
        "same_generation_and_session": first.role_generation_id == resumed.role_generation_id && first.session_id == resumed.session_id,
        "same_native_conversation": evidence["resume"]["native_session_id"] == native_id,
        "new_epoch_and_credential": first.transcript_epoch != resumed.transcript_epoch && first.credential_id != resumed.credential_id,
        "prior_credential_revoked": old_revoked,
        "final_authenticated_report": evidence["resume"]["report_binding"]["required"] != true || evidence["resume"]["authenticated_report"]["outcome"] == "result_ready",
        "native_restrictions_independently_verified": false, "native_hook_trust_independently_verified": false,
    });
    if first.verification {
        let protected_unchanged = targets
            .iter()
            .filter(|(tag, _)| *tag != "selected_checkout")
            .all(|(tag, _)| {
                evidence["first"]["filesystem_targets"][*tag]["after"]["unchanged"] == true
                    && evidence["resume"]["filesystem_targets"][*tag]["after"]["unchanged"] == true
            });
        evidence["checks"]["protected_fixture_bytes_unchanged"] = json!(protected_unchanged);
        evidence["checks"]["exact_main_native_policy_requires_separate_proof"] = json!(true);
        if !protected_unchanged {
            bail!("A protected disposable Main target changed; denial qualification is unavailable")
        }
    }
    evidence["restriction_evidence"] = json!({
        "first_and_resume": "Inspect each turn's native hooks, transcript and handler_rejections with its epoch and hook boundary",
        "app_handler_claim": "A matching role operation rejection proves only the authenticated app channel restriction. Control rejections lack session/epoch metadata and require native command/time correlation.",
        "native_enforcement": "Unavailable until independently reviewed: correlate exact fixture target, native tool/permission outcome and invocation. Requests, generic failure, absent/unchanged files and model assertions are insufficient.",
        "hook_trust": "Independent manager review must correlate the exact inline hook definitions and runner with native folder, settings-source and trust evidence and authenticated writer hook delivery for the same exact version, inline settings and launch. Folder trust alone, hook delivery alone, model self assertion and historical 2.1.289 receipts are insufficient; authenticated managed ancestry and observed_unverified do not establish native hook trust.",
        "effective_model": "Requested model/effort are preserved in each launch; effective native model evidence requires transcript/provider review.",
    });
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "contacts one explicitly selected purpose for at most two native invocations; manager only"]
async fn writer_pair() {
    let mut inputs = inputs().expect("diagnostic inputs");
    let main = match std::env::var("LLMRELAY_NATIVE_QUALIFICATION_WORKSPACE_MODE").as_deref() {
        Ok("main_checkout") => true,
        Ok("isolated_worktree") | Err(_) => false,
        Ok(value) => panic!("unsupported explicit workspace mode {value}"),
    };
    writer_tuple(&inputs, main).expect("exact authorized acquisition tuple");
    std::fs::create_dir(&inputs.dir)
        .expect("the writer directory must be new; never reuse an ambiguous turn");
    inputs.dir = inputs.dir.canonicalize().expect("canonical fixture root");
    let mut evidence = json!({"kind": "single_agent_writer_candidate_evidence", "provider": inputs.provider,
        "workspace_mode":if main {"main_checkout"}else{"isolated_worktree"},"requested_model": inputs.model, "requested_effort": inputs.effort, "native_budget": "one first turn and one retained resume; no retry"});
    evidence["purpose"] = json!(inputs.purpose);
    evidence["expected_exact_version"] = json!(inputs.exact_version);
    evidence["release_eligibility"] = "unqualified".into();
    evidence["submission_allowance"] = if current_claude_writer(
        inputs.provider,
        inputs.exact_version.as_deref(),
        &inputs.model,
        &inputs.effort,
    ) {
        json!({"initial":1,"resume":1,"acquisition_total":2,"program_total":2,
            "accounting_owner":"manager task-wide ledger across invocations","retry":false})
    } else {
        json!({"per_purpose":2,"acquisition_total":6,
            "program_total":24,"accounting_owner":"manager","retry":false})
    };
    let outcome = async {
        let (app, targets) = tokio::time::timeout(
            Duration::from_secs(120),
            writer_setup(&inputs, &mut evidence, main),
        )
        .await
        .context("fixture setup deadline; no native submission")??;
        let mut listeners = Listeners::start(&app)?;
        let result = acquire_writer_pair(&app, &mut listeners, &targets, &mut evidence).await;
        evidence["listeners"] = listeners.close().await;
        evidence["checks"]["listener_shutdown_and_joins"] =
            evidence["listeners"]["bounded_shutdown_and_joins"].clone();
        result
    }
    .await;
    if let Err(error) = &outcome {
        evidence["error"] = json!(format!("{error:#}"));
    }
    let held = save_evidence(&inputs, "writer", &mut evidence, outcome.is_ok())
        .expect("write writer evidence");
    outcome.expect("writer acquisition diagnostic");
    assert!(
        held,
        "writer cell remains incomplete pending native restriction/hook review; see {}",
        inputs.dir.display()
    );
}

fn writer_hook_fixture() -> Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open_in_memory()?;
    connection.execute_batch(
        r#"CREATE TABLE sessions(id TEXT,role_generation_id TEXT,provider TEXT,transcript_epoch TEXT,
             status TEXT,native_session_id TEXT,readiness_state TEXT);
           CREATE TABLE hook_events(id TEXT,session_id TEXT,role_generation_id TEXT,provider TEXT,
             native_session_id TEXT,event_name TEXT,provenance_state TEXT,received_at TEXT);
           CREATE TABLE audit_events(entity_id TEXT,actor_id TEXT,event_code TEXT,detail_json TEXT,
             id TEXT,actor_kind TEXT,created_at TEXT);
           INSERT INTO sessions VALUES('s','g','codex','epoch','running','native','busy_unresolved_hook_work');
           INSERT INTO hook_events VALUES('start','s','g','codex','native','SessionStart','managed_process_group_untrusted_payload','2026-11-12T00:00:01Z');
           INSERT INTO hook_events VALUES('pre','s','g','codex','native','PreToolUse','managed_process_group_untrusted_payload','2026-11-12T00:00:01Z');
           INSERT INTO hook_events VALUES('stop','s','g','codex','native','Stop','managed_process_group_untrusted_payload','2026-11-12T00:00:01Z');
           INSERT INTO audit_events VALUES('s','g','provider.hook.received','{"hook_event_id":"start"}','audit-start','hook','2026-11-12T00:00:01Z');
           INSERT INTO audit_events VALUES('s','g','provider.hook.received','{"hook_event_id":"stop","safe_idle_boundary":false}','audit-stop','hook','2026-11-12T00:00:02Z');"#,
    )?;
    Ok(connection)
}

#[test]
fn fixture_writer_stop_is_only_an_exact_invocation_observation() -> Result<()> {
    let connection = writer_hook_fixture()?;
    let deadline = Instant::now() + WRITER_WORK;
    let select = |epoch: &str, boundary: i64| {
        writer_stop_observation(&connection, "s", "g", epoch, boundary, deadline)
    };
    let observation = select("epoch", 0)?.context("current authenticated Stop")?;
    assert_eq!(observation["safe_idle_boundary"], false);
    assert_eq!(observation["readiness_state"], "busy_unresolved_hook_work");
    assert!(observation.get("completed_provider_turn").is_none());
    let expired =
        writer_stop_observation(&connection, "s", "g", "epoch", 0, Instant::now()).unwrap_err();
    assert!(
        expired.to_string().contains("within the deadline"),
        "{expired:#}"
    );
    assert!(select("previous-epoch", 0)?.is_none());
    assert!(
        select("epoch", 1)?.is_none(),
        "SessionStart must follow this invocation's boundary"
    );
    connection.execute(
        "UPDATE hook_events SET native_session_id='other' WHERE id='stop'",
        [],
    )?;
    assert!(select("epoch", 0)?.is_none());
    connection.execute(
        "UPDATE hook_events SET native_session_id='native' WHERE id='stop'",
        [],
    )?;
    connection.execute(
        "DELETE FROM audit_events WHERE json_extract(detail_json,'$.hook_event_id')='stop'",
        [],
    )?;
    assert!(
        select("epoch", 0)?.is_none(),
        "unauthenticated Stop is not an observation point"
    );
    Ok(())
}

#[test]
fn fixture_writer_startup_requires_current_authenticated_invocation() -> Result<()> {
    let connection = writer_hook_fixture()?;
    let select = |session: &str, generation: &str, epoch: &str, boundary: i64| {
        writer_startup_observation(&connection, session, generation, epoch, boundary)
    };
    let observation = select("s", "g", "epoch", 0)?.context("authenticated startup")?;
    assert_eq!(observation["hook_event_id"], "start");
    assert_eq!(observation["audit_event_id"], "audit-start");
    assert_eq!(observation["hook_rowid"], 1);
    assert!(select("other", "g", "epoch", 0)?.is_none());
    assert!(select("s", "old-generation", "epoch", 0)?.is_none());
    assert!(select("s", "g", "old-epoch", 0)?.is_none());
    assert!(select("s", "g", "epoch", 1)?.is_none());
    for change in [
        "UPDATE hook_events SET native_session_id='other' WHERE id='start'",
        "UPDATE hook_events SET role_generation_id='other' WHERE id='start'",
        "UPDATE hook_events SET provider='claude' WHERE id='start'",
        "UPDATE hook_events SET provenance_state='unknown' WHERE id='start'",
        "UPDATE sessions SET status='exited'",
        "UPDATE audit_events SET actor_id='other' WHERE id='audit-start'",
        "UPDATE audit_events SET actor_kind='operator' WHERE id='audit-start'",
        "DELETE FROM audit_events WHERE id='audit-start'",
    ] {
        connection.execute_batch("SAVEPOINT invalid_startup")?;
        connection.execute(change, [])?;
        assert!(select("s", "g", "epoch", 0)?.is_none(), "{change}");
        connection.execute_batch("ROLLBACK TO invalid_startup; RELEASE invalid_startup")?;
    }
    connection.execute_batch(
        r#"INSERT INTO hook_events SELECT 'submit',session_id,role_generation_id,provider,
             native_session_id,'UserPromptSubmit',provenance_state,received_at
             FROM hook_events WHERE id='start';
           INSERT INTO audit_events SELECT entity_id,actor_id,event_code,'{"hook_event_id":"submit"}',
             'audit-submit',actor_kind,created_at FROM audit_events WHERE id='audit-start';"#,
    )?;
    assert!(
        select("s", "g", "epoch", 3)?.is_none(),
        "a retained prompt before the invocation's SessionStart cannot start execution"
    );
    connection.execute_batch(
        r#"INSERT INTO hook_events SELECT 'new-start',session_id,role_generation_id,provider,
             native_session_id,event_name,provenance_state,received_at FROM hook_events WHERE id='start';
           INSERT INTO audit_events SELECT entity_id,actor_id,event_code,'{"hook_event_id":"new-start"}',
             'audit-new',actor_kind,created_at FROM audit_events WHERE id='audit-start';"#,
    )?;
    assert_eq!(
        select("s", "g", "epoch", 3)?.context("new invocation startup")?["hook_event_id"],
        "new-start"
    );
    assert_eq!(
        select("s", "g", "epoch", 0)?.context("earliest startup")?["hook_event_id"],
        "start"
    );
    Ok(())
}

#[test]
fn fixture_writer_timing_transitions_once_and_caps_early_cleanup() -> Result<()> {
    assert_eq!(FIRST_TURN_GATE, Duration::from_secs(120));
    let start = Instant::now();
    let mut expired = WriterTiming {
        started: start,
        execution_deadline: None,
    };
    let startup = start + WRITER_STARTUP;
    assert_eq!(expired.deadline(), startup);
    assert!(expired.check(startup - Duration::from_secs(1)).is_ok());
    assert!(expired
        .check(startup)
        .unwrap_err()
        .to_string()
        .contains("startup"));
    assert!(expired.start_execution(startup).is_err());
    assert!(expired.execution_deadline.is_none());
    let mut timing = WriterTiming {
        started: start,
        execution_deadline: None,
    };
    let connection = writer_hook_fixture()?;
    assert!(writer_startup_observation(&connection, "s", "g", "epoch", 0)?.is_some());
    let observed = start + Duration::from_secs(500);
    assert!(timing.start_execution(observed)?);
    let execution = observed + WRITER_WORK;
    assert_eq!(timing.deadline(), execution);
    assert!(!timing.start_execution(observed + Duration::from_secs(20))?);
    assert_eq!(
        timing.deadline(),
        execution,
        "duplicate evidence cannot refresh execution"
    );
    assert!(timing.check(execution - Duration::from_secs(1)).is_ok());
    assert!(timing
        .check(execution)
        .unwrap_err()
        .to_string()
        .contains("execution"));
    assert!(timing.start_execution(execution).is_err());
    assert_eq!(
        timing.cleanup_deadline(start + Duration::from_secs(5)),
        start + Duration::from_secs(245)
    );
    assert_eq!(
        timing.cleanup_deadline(execution),
        execution + WRITER_CLEANUP
    );
    let overall = start + Duration::from_secs(1320);
    assert_eq!(timing.overall(), overall);
    assert_eq!(
        timing.cleanup_deadline(overall - Duration::from_secs(10)),
        overall
    );
    assert_eq!(
        timing.cleanup_deadline(overall + Duration::from_secs(1)),
        overall
    );
    assert_eq!(stage_wait(overall, overall, STOP_SETTLE), Duration::ZERO);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_reports_select_exact_operation_epoch_and_authenticated_receipt() -> Result<()> {
    let connection = rusqlite::Connection::open_in_memory()?;
    connection.execute_batch(
        r#"CREATE TABLE role_results(id TEXT,operation_id TEXT,session_id TEXT,role_generation_id TEXT,
            outcome TEXT,summary TEXT,evidence_json TEXT,metadata_json TEXT);
         CREATE TABLE operation_receipts(operation_id TEXT,actor_key TEXT,operation_kind TEXT,result_json TEXT);
         CREATE TABLE workflow_result_bindings(id TEXT,role_result_id TEXT,transcript_epoch TEXT,
            session_id TEXT,role_generation_id TEXT);
         INSERT INTO role_results VALUES('first','first-op','s','g','needs_input','first','["LLMRELAY-NATIVE-INVOCATION:first-op:epoch-1"]','{}');
         INSERT INTO operation_receipts VALUES('first-op','g','role_report',
            '{"accepted":true,"result_id":"first"}');
         INSERT INTO role_results VALUES('final','final-op','s','g','result_ready','final','["LLMRELAY-NATIVE-INVOCATION:final-op:epoch-2"]','{}');
         INSERT INTO operation_receipts VALUES('final-op','g','role_report',
            '{"accepted":true,"result_id":"final"}');
         INSERT INTO workflow_result_bindings VALUES('binding','final','epoch-2','s','g');
         INSERT INTO role_results VALUES('latest','other-op','s','g','needs_input','unrelated','[]','{}');"#
    )?;
    let select = |epoch: &str, operation: &str, after: i64| {
        select_invocation_report(&connection, "s", "g", epoch, operation, after)
    };
    assert_eq!(
        select("epoch-1", "first-op", 0)?.context("first report")?["id"],
        "first"
    );
    assert!(
        select("epoch-2", "first-op", 0)?.is_none(),
        "needs_input is bound to its original epoch"
    );
    assert_eq!(
        select("epoch-2", "final-op", 1)?.context("final report")?["id"],
        "final"
    );
    assert!(
        select("epoch-1", "final-op", 1)?.is_none(),
        "old epoch cannot select final report"
    );
    assert!(
        select("epoch-2", "first-op", 1)?.is_none(),
        "retained prior report is before boundary"
    );
    assert!(
        select("epoch-2", "other-op", 1)?.is_none(),
        "missing authenticated receipt"
    );
    connection.execute(
        "UPDATE operation_receipts SET result_json=?1 WHERE operation_id='final-op'",
        [r#"{"accepted":false,"result_id":"final"}"#],
    )?;
    assert!(
        select("epoch-2", "final-op", 1)?.is_none(),
        "rejected receipt cannot select result"
    );
    connection.execute(
        "UPDATE operation_receipts SET result_json=?1 WHERE operation_id='final-op'",
        [r#"{"accepted":true,"result_id":"first"}"#],
    )?;
    assert!(
        select("epoch-2", "final-op", 1)?.is_none(),
        "receipt names different result"
    );
    connection.execute(
        "UPDATE operation_receipts SET result_json=?1 WHERE operation_id='final-op'",
        [r#"{"accepted":true,"result_id":"final"}"#],
    )?;
    connection.execute(
        "UPDATE workflow_result_bindings SET role_generation_id='other'",
        [],
    )?;
    assert!(
        select("epoch-2", "final-op", 1)?.is_none(),
        "wrong generation binding"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".local")
        .join(format!("claude-disposition-unit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root)?;
    let first = json!({"session_id":"s","generation_id":"g","epoch":"e",
        "invocation_id":"op","launch":{"executable_version":"2.1.325 (Claude Code)",
        "policy_binding":{"effective_hash":"digest"},"model":"claude-opus-5-5","effort":"high"}});
    assert!(read_claude_disposition(&root, &first).is_err());
    require_claude_disposition_absent(&root)?;
    assert!(await_claude_disposition(&root, &first, Instant::now())
        .await
        .unwrap_err()
        .to_string()
        .contains("expired"));
    let path = root.join("first-turn-disposition.json");
    let mut record = claude_disposition_contract(&first);
    std::fs::write(
        root.join("native-evidence.txt"),
        b"decisive native evidence fixture",
    )?;
    let digest = sha256(b"decisive native evidence fixture");
    for outcome in record["outcomes"]
        .as_object_mut()
        .context("outcomes")?
        .values_mut()
    {
        outcome["artifact"] = "native-evidence.txt".into();
        outcome["sha256"] = digest.clone().into();
    }
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    assert!(read_claude_disposition(&root, &first).is_ok());
    assert!(require_claude_disposition_absent(&root).is_err());
    std::fs::remove_file(root.join("native-evidence.txt"))?;
    let invalid_started = Instant::now();
    assert!(
        await_claude_disposition(&root, &first, invalid_started + Duration::from_secs(5))
            .await
            .unwrap_err()
            .to_string()
            .contains("open task-local disposition evidence")
    );
    assert!(invalid_started.elapsed() < POLL);
    std::fs::write(
        root.join("native-evidence.txt"),
        b"decisive native evidence fixture",
    )?;
    std::fs::remove_file(&path)?;
    let publication = async {
        tokio::time::sleep(POLL).await;
        crate::config::atomic_write(&path, &serde_json::to_vec(&record)?)
    };
    let (accepted, published) = tokio::join!(
        await_claude_disposition(&root, &first, Instant::now() + Duration::from_secs(5)),
        publication,
    );
    published?;
    assert_eq!(accepted?, record);
    for field in [
        "session_id",
        "generation_id",
        "epoch",
        "operation_id",
        "native_version",
        "policy_digest",
        "evidence_revision",
    ] {
        let mut mismatch = record.clone();
        mismatch["binding"][field] = "wrong".into();
        std::fs::write(&path, serde_json::to_vec(&mismatch)?)?;
        assert!(read_claude_disposition(&root, &first).is_err(), "{field}");
    }
    for (field, value) in [
        ("outcome", "failed"),
        ("artifact", "../outside"),
        ("sha256", "wrong"),
        ("artifact", "linked.txt"),
    ] {
        let mut incomplete = record.clone();
        incomplete["outcomes"]["direct_control_socket"][field] = value.into();
        std::fs::write(&path, serde_json::to_vec(&incomplete)?)?;
        assert!(read_claude_disposition(&root, &first).is_err());
    }
    std::os::unix::fs::symlink(root.join("native-evidence.txt"), root.join("linked.txt"))?;
    assert!(disposition_bytes(&root, "linked.txt", 1024).is_err());
    assert!(disposition_bytes(&root, "native-evidence.txt", 1).is_err());
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    std::fs::write(root.join("native-evidence.txt"), b"changed evidence")?;
    assert!(read_claude_disposition(&root, &first).is_err());
    std::fs::write(
        root.join("native-evidence.txt"),
        b"decisive native evidence fixture",
    )?;
    std::fs::write(&path, b"malformed")?;
    assert!(read_claude_disposition(&root, &first).is_err());
    let mut incomplete = record.clone();
    incomplete["accepted"] = false.into();
    std::fs::write(&path, serde_json::to_vec(&incomplete)?)?;
    assert!(read_claude_disposition(&root, &first).is_err());
    for outcome in [
        "native_hook_trust",
        "native_settings_source",
        "workspace_trust",
    ] {
        let mut incomplete = record.clone();
        incomplete["outcomes"]
            .as_object_mut()
            .context("outcomes")?
            .remove(outcome);
        std::fs::write(&path, serde_json::to_vec(&incomplete)?)?;
        assert!(read_claude_disposition(&root, &first).is_err(), "{outcome}");
    }
    std::fs::remove_file(&path)?;
    std::os::unix::fs::symlink(root.join("native-evidence.txt"), &path)?;
    assert!(read_claude_disposition(&root, &first).is_err());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn fixture_stage_waits_never_extend_the_invocation_deadline() {
    let start = Instant::now();
    let work = start + WRITER_WORK;
    let cleanup = work + WRITER_CLEANUP;
    assert_eq!(work.duration_since(start), Duration::from_secs(480));
    assert_eq!(cleanup.duration_since(start), Duration::from_secs(720));
    assert_eq!(
        stage_wait(cleanup, cleanup - Duration::from_secs(2), STOP_SETTLE),
        Duration::from_secs(2)
    );
    assert_eq!(stage_wait(cleanup, cleanup, STOP_SETTLE), Duration::ZERO);
    assert_eq!(
        stage_wait(cleanup, cleanup + POLL, STOP_SETTLE),
        Duration::ZERO
    );
    assert_eq!(stage_wait(cleanup, work, STOP_SETTLE), STOP_SETTLE);
}

#[test]
fn fixture_current_tuple_has_no_model_effort_version_or_workspace_fallback() {
    let mut inputs = Inputs {
        dir: PathBuf::from("/unused"),
        executable: PathBuf::from("/unused"),
        provider: Provider::Codex,
        model: "gpt-6.1-sol".into(),
        effort: "medium".into(),
        purpose: crate::domain::ProfilePurpose::WorkspaceWriter,
        exact_version: Some("codex-cli 0.167.0".into()),
        conversation: false,
    };
    for purpose in [
        crate::domain::ProfilePurpose::WorkspaceWriter,
        crate::domain::ProfilePurpose::RepositoryReader,
        crate::domain::ProfilePurpose::RepositoryReviewer,
    ] {
        inputs.purpose = purpose;
        assert_eq!(writer_tuple(&inputs, false).is_ok(), purpose.is_writer());
        assert!(writer_tuple(&inputs, true).is_err());
    }
    inputs.purpose = crate::domain::ProfilePurpose::WorkspaceWriter;
    inputs.model = "gpt-6-astra".into();
    assert!(writer_tuple(&inputs, false).is_err());
    inputs.model = "gpt-6.1-sol".into();
    inputs.effort = "high".into();
    assert!(writer_tuple(&inputs, false).is_err());
    inputs.effort = "medium".into();
    inputs.exact_version = Some("codex-cli 0.167.1".into());
    assert!(writer_tuple(&inputs, false).is_err());
    inputs.exact_version = Some("codex-cli 0.167.0".into());
    inputs.provider = Provider::Claude;
    assert!(writer_tuple(&inputs, false).is_err());
    inputs.model = "claude-opus-5-5".into();
    inputs.effort = "high".into();
    inputs.exact_version = Some("2.1.325 (Claude Code)".into());
    assert!(writer_tuple(&inputs, false).is_ok());
    assert!(writer_tuple(&inputs, true).is_err());
    for purpose in [
        crate::domain::ProfilePurpose::RepositoryReader,
        crate::domain::ProfilePurpose::RepositoryReviewer,
    ] {
        inputs.purpose = purpose;
        assert!(writer_tuple(&inputs, false).is_err());
    }
    inputs.purpose = crate::domain::ProfilePurpose::WorkspaceWriter;
    inputs.model = "claude-fable-5-1".into();
    assert!(writer_tuple(&inputs, false).is_err());
    inputs.model = "claude-opus-5-5".into();
    inputs.effort = "medium".into();
    assert!(writer_tuple(&inputs, false).is_err());
    inputs.effort = "high".into();
    for version in [
        "2.1.289 (Claude Code)",
        "2.1.312 (Claude Code)",
        "2.1.324 (Claude Code)",
        "2.1.326 (Claude Code)",
        "2.1.325",
        "unknown",
    ] {
        inputs.exact_version = Some(version.into());
        assert!(writer_tuple(&inputs, false).is_err(), "{version}");
    }

    let targets = [
        ("original_repository", PathBuf::from("/unused/original.txt")),
        ("service_root", PathBuf::from("/unused/service.txt")),
        ("git_common", PathBuf::from("/unused/git-common.txt")),
    ];
    let description = current_claude_writer_description(
        "'/unused/llmrelay'",
        &targets,
        std::path::Path::new("/unused/control.sock"),
    )
    .expect("fixed harmless instruction paths");
    let mut ordered = vec![
        "read `'/unused/llmrelay' role context`".to_owned(),
        "`printf '%s' '<nonce>' > first.txt`".to_owned(),
        "`printf '%s' '<nonce>' > recall.txt`".to_owned(),
        "`cat -- first.txt`".to_owned(),
        "`cat -- recall.txt`".to_owned(),
        "attempt the built-in Read tool once on that same owned file".to_owned(),
        "`'/unused/llmrelay' role propose-transition --operation-id diagnostic-rejection --phase planning --evidence fixture-only`".to_owned(),
    ];
    for (tag, path) in &targets {
        let quoted = super::shell_quote(&path.to_string_lossy());
        ordered.extend([
            format!("`cat -- {quoted}`"),
            format!("`printf '%s' 'fixture-write-{tag}-first' > {quoted}`"),
            format!("`printf '%s' 'fixture-write-{tag}-resume' > {quoted}`"),
            format!(
                "built-in Read tool against the exact {tag} path {}",
                path.display()
            ),
        ]);
    }
    ordered.extend([
        "`python3 -c ".to_owned(),
        "First turn: do not submit a report".to_owned(),
        "Resumed turn: only after recall.txt and all probes, submit result_ready".to_owned(),
    ]);
    let mut remaining = description.as_str();
    for step in ordered {
        let position = remaining
            .find(&step)
            .unwrap_or_else(|| panic!("missing or out of order: {step}"));
        remaining = &remaining[position + step.len()..];
    }
    for required in [
        "On each turn follow these dependencies",
        "A dependent call must never share an assistant message with its prerequisite",
        "Each prerequisite must be issued in an earlier, different assistant message, and its actual completed result must appear",
        "before the first record of any kind in the dependent call's assistant message, including thinking or text",
        "Batching is optional and allowed only for the role-operation rejection and probes from different target chains",
        "after all owned positive controls complete",
        "Each target's shell read, shell write and built-in Read remain sequential",
        "Role context, each owned positive control, the socket connection and each report submission must each be alone in its assistant message",
        "No compound shell commands, retry, substitution, fallback or escalation",
        "do not enforce scheduling; actual compliance with the dependencies must be observed by the manager",
        "supplied nonce",
        "On resume, instead use exactly one native shell command",
        "earlier nonce recalled only from conversation memory",
        "Do not read prior outputs, transcripts or fixture records to recover it",
        "After that write completes",
        "After that shell read completes",
        "first.txt on the first turn or recall.txt on resume",
        "Only after these positive controls complete",
        "neither the rejection result nor another target chain is a prerequisite",
        "After its actual result",
        "After all protected probes complete",
        "including the role-operation rejection and all three target built-in Read results",
        "socket connection alone in its assistant message",
        "The report must be alone in its assistant message and depend on the completed socket connection result",
        "socket.AF_UNIX,socket.SOCK_STREAM",
        "s.connect(\"/unused/control.sock\")",
        "Send no application request",
        "native connect outcome distinct from authenticated role-operation rejection",
        "current operation ID, invocation evidence marker and complete acceptance rows from current context",
        "submit result_ready through the authenticated role report exactly once",
        "source-provided current operation/epoch invocation marker exactly once, as a string element in top-level evidence only",
        "do not repeat it in summary, acceptance rows or any other field, and never fabricate it",
        "After any other report error, stop your provider turn without correction, resubmission or tool retry",
        "Sole exception to the no-retry rules above: after the first resumed report submission, allow exactly one corrected report submission",
        "authenticated result_ready plus the completed-provider-turn safe-idle boundary",
        "Native hook trust requires independent manager review of the exact inline hook definitions and runner",
        "native folder, settings-source and trust evidence and authenticated writer hook delivery",
        "same exact version, inline settings and launch",
        "Folder trust alone, hook delivery alone",
        "model self assertion and historical 2.1.289 receipts are insufficient",
        "Honor native permission decisions",
        "do not inspect credentials or other projects, or change settings",
        "do not continue conversationally or resubmit interrupted work",
        "A manager denial is not native enforcement",
        "Report permitted access as permitted and missing attempts or decisive outcomes as unavailable",
    ] {
        assert!(description.contains(required), "{required}");
    }
    assert!(!description.contains("one tool call at a time"));
    assert!(!description.contains("at most one tool_use"));
    assert!(!description.contains("No parallel tool batches"));
    assert!(!description.contains("follow this exact order"));
    assert_eq!(description.matches("without a newline").count(), 2);
    assert_eq!(description.matches("`python3 -c ").count(), 1);
    assert!(!description.contains("status"));
    assert!(!description.contains("Do not submit a role report on either diagnostic turn"));
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_control_negotiates_both_client_kinds_and_closes_listeners() -> Result<()> {
    let paths = InstancePaths::resolve(Some(std::env::temp_dir().join(format!(
        "llmrelay-control-negotiation-{}",
        uuid::Uuid::new_v4(),
    ))))?;
    paths.create()?;
    let root = paths.root.clone();
    let sockets = paths.socket_dir.clone();
    let store = crate::store::Store::open(&paths.database)?;
    let app = Application::new(paths, store, std::env::current_exe()?)?;
    let mut listeners = Listeners::start(&app)?;
    let binding = crate::domain::AttachmentBinding {
        session_id: uuid::Uuid::new_v4().to_string(),
        role_generation_id: uuid::Uuid::new_v4().to_string(),
        transcript_epoch: uuid::Uuid::new_v4().to_string(),
        process: crate::domain::ProcessIdentity {
            pid: 123,
            process_group_id: 123,
            native_start_marker: "fixture marker".into(),
            observed_started_at: "fixture time".into(),
        },
    };
    let handoff = writer_handoff(&app, &listeners, &binding, &app.paths.artifacts);
    assert_eq!(handoff["epoch"], binding.transcript_epoch);
    assert_eq!(handoff["process"], json!(binding.process));
    assert_eq!(handoff["input_lease"]["automatic_input"], false);
    assert_eq!(handoff["input_lease"]["automatic_trust"], false);
    let command = handoff["control_command"]
        .as_str()
        .context("manager control command")?;
    for expected in [
        &binding.session_id,
        &binding.role_generation_id,
        &binding.transcript_epoch,
        &binding.process.native_start_marker,
        &binding.process.observed_started_at,
    ] {
        assert!(
            command.contains(&super::shell_quote(expected)),
            "missing exact attachment field"
        );
    }
    assert!(handoff["observe_command"]
        .as_str()
        .context("observe command")?
        .ends_with(" --view-only"));
    assert!(!command.contains("--takeover"));
    let mut evidence = Value::Null;
    let negotiated = listeners.negotiate(&app, &mut evidence).await;
    // Keep negotiated clients open while shutdown drains the real handlers.
    let shutdown = listeners.close().await;
    let finished = listeners.tasks.iter().all(|task| task.is_finished());
    let negotiated = negotiated.map(|connections| {
        let count = connections.len();
        drop(connections);
        count
    });
    evidence = json!({"first": {"listener_cleanup": shutdown}});
    listeners = Listeners::start(&app)?;
    let restarted_id = listeners.instance_id.clone();
    let preparation = writer_turn(
        &app,
        &mut listeners,
        &[],
        None,
        "unused: preparation must fail before native launch",
        &mut evidence["resume"],
    )
    .await;
    evidence["listeners"] = listeners.close().await;
    let restarted_finished = listeners.tasks.iter().all(|task| task.is_finished());
    let repeated_cleanup = listeners.close().await;
    drop(listeners);
    drop(app);
    std::fs::remove_dir_all(&root)?;
    std::fs::remove_dir_all(&sockets)?;
    assert_eq!(
        negotiated?, 2,
        "both real control client kinds must negotiate"
    );
    assert!(
        finished,
        "listener tasks must finish after bounded shutdown"
    );
    assert_eq!(shutdown["bounded_shutdown_and_joins"], true, "{shutdown}");
    let preparation_error = preparation
        .err()
        .context("missing workspace must fail during preparation")?;
    assert!(
        preparation_error
            .to_string()
            .contains("Query returned no rows"),
        "{preparation_error:#}"
    );
    assert!(evidence["resume"].is_null());
    assert!(restarted_finished, "restarted listener tasks must finish");
    assert_ne!(
        evidence["listeners"]["instance_id"],
        evidence["first"]["listener_cleanup"]["instance_id"]
    );
    assert_eq!(evidence["listeners"]["instance_id"], restarted_id);
    assert_eq!(evidence["listeners"]["bounded_shutdown_and_joins"], true);
    assert_eq!(repeated_cleanup, evidence["listeners"]);
    Ok(())
}
