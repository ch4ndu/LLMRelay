use crate::config::{InstancePaths, DEFAULT_PORT};
use crate::control::{self, ControlRequest};
use crate::domain::{
    AttachmentBinding, CapabilityProofInput, HookEnvelope, HumanCommand, LaunchHandshake,
    ProcessGenerationAnchor, ProcessIdentity, Provider, RoleKind, RoleResultReport,
    ValidationLaunchRequest,
};
use crate::task_cli::{self, RoleOperation, RoleRequest};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use clap::{Args, Parser, Subcommand};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(name = "llmrelay", version, about = "Local LLMRelay workflow host")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeArgs),
    Status(InstanceArgs),
    Stop(StopArgs),
    Attach(AttachArgs),
    Capability(CapabilityArgs),
    Apply(ApplyArgs),
    State(InstanceArgs),
    Scheduler(SchedulerArgs),
    Snapshot(SnapshotArgs),
    Dispatch(DispatchArgs),
    DispatchSetup(DispatchSetupArgs),
    DeliverGuidance(GuidanceArgs),
    SwitchRole(SwitchRoleArgs),
    Diagnostics(DiagnosticsArgs),
    Database(DatabaseArgs),
    Logs(LogsArgs),
    Import(ImportArgs),
    Check(CheckArgs),
    Role(RoleArgs),
    ResumeWork(ResumeWorkArgs),
    RestartPreview(InstanceArgs),
    #[command(hide = true)]
    Hook(HookArgs),
    #[command(hide = true)]
    InternalLaunch(InternalLaunchArgs),
    #[command(hide = true)]
    UnixConnectProbe(UnixConnectProbeArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    #[arg(
        long,
        help = "Do not open the authenticated dashboard in the default browser"
    )]
    no_open: bool,
}

#[derive(Debug, Args)]
struct InstanceArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct StopArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    drain: bool,
}

#[derive(Debug, Args)]
struct AttachArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    session: String,
    #[arg(
        long,
        help = "Explicitly revoke the active human input lease and take control"
    )]
    takeover: bool,
    #[arg(
        long,
        conflicts_with = "takeover",
        help = "Watch output without acquiring, renewing, or resizing an input lease"
    )]
    view_only: bool,
    #[arg(long)]
    expected_generation: Option<String>,
    #[arg(long)]
    expected_epoch: Option<String>,
    #[arg(long)]
    expected_pid: Option<u32>,
    #[arg(long)]
    expected_process_group: Option<i32>,
    #[arg(long)]
    expected_start_marker: Option<String>,
    #[arg(long)]
    expected_observed_at: Option<String>,
    #[arg(long)]
    cmux_route_id: Option<String>,
    /// Route ID from the persistent mode-neutral cmux table. It is never
    /// interchangeable with the migration-024 diagnostic route argument.
    #[arg(long, conflicts_with = "cmux_route_id")]
    cmux_surface_route_id: Option<String>,
    #[arg(long, requires = "cmux_surface_route_id")]
    cmux_binding_revision: Option<i64>,
}

#[derive(Debug, Args)]
struct ResumeWorkArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    operation_id: Option<String>,
    #[arg(long, conflicts_with = "session")]
    eligible: bool,
    #[arg(long = "session")]
    session: Vec<String>,
}

#[derive(Debug, Args)]
struct CapabilityArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: CapabilityCommand,
}

#[derive(Debug, Subcommand)]
enum CapabilityCommand {
    Launch {
        #[arg(long)]
        cell: String,
        #[arg(long)]
        provider: String,
        #[arg(long, default_value = "plan-reviewer")]
        role: String,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long)]
        effort: String,
        #[arg(long)]
        prompt_file: PathBuf,
        #[arg(long)]
        operation_id: Option<String>,
    },
    WorkflowLaunch {
        #[arg(long)]
        cell: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        role: String,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        task: String,
        #[arg(long)]
        attempt: Option<String>,
        #[arg(long)]
        model: String,
        #[arg(long)]
        effort: String,
        #[arg(long)]
        prompt_file: PathBuf,
        #[arg(long)]
        operation_id: Option<String>,
    },
    List,
    Inspect {
        #[arg(long)]
        session: String,
    },
    Transcript {
        #[arg(long)]
        session: String,
        #[arg(long)]
        after_epoch: Option<String>,
        #[arg(long, default_value_t = 0)]
        after_sequence: u64,
        #[arg(long, default_value_t = 262_144)]
        limit_bytes: usize,
        #[arg(long)]
        raw: bool,
    },
    AcquireInput {
        #[arg(long)]
        session: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value_t = 30)]
        seconds: i64,
    },
    RenewInput {
        #[arg(long)]
        session: String,
        #[arg(long)]
        lease: String,
        #[arg(long, default_value_t = 30)]
        seconds: i64,
    },
    TakeoverInput {
        #[arg(long)]
        session: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value_t = 30)]
        seconds: i64,
    },
    SendInput {
        #[arg(long)]
        session: String,
        #[arg(long)]
        lease: String,
    },
    ReleaseInput {
        #[arg(long)]
        session: String,
        #[arg(long)]
        lease: String,
    },
    Resize {
        #[arg(long)]
        session: String,
        #[arg(long)]
        lease: String,
        #[arg(long)]
        rows: u16,
        #[arg(long)]
        cols: u16,
    },
    Interrupt {
        #[arg(long)]
        session: String,
    },
    Resume {
        #[arg(long)]
        session: String,
        #[arg(long)]
        prompt_file: PathBuf,
    },
    RecordProof {
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(Debug, Args)]
struct ApplyArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SchedulerArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SnapshotArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: SnapshotCommand,
}

#[derive(Debug, Subcommand)]
enum SnapshotCommand {
    Freeze {
        #[arg(long)]
        attempt: String,
        #[arg(long)]
        kind: String,
    },
    Verify {
        #[arg(long)]
        snapshot: String,
    },
}

#[derive(Debug, Args)]
struct DispatchArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    role: String,
    #[arg(long)]
    lane: Option<String>,
    #[arg(long)]
    prompt_file: PathBuf,
}

#[derive(Debug, Args)]
struct DispatchSetupArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    attempt: String,
    #[arg(long)]
    role: String,
}

#[derive(Debug, Args)]
struct GuidanceArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    guidance: String,
}

#[derive(Debug, Args)]
struct SwitchRoleArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: SwitchRoleCommand,
}

#[derive(Debug, Subcommand)]
enum SwitchRoleCommand {
    Request {
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        attempt: String,
        #[arg(long)]
        role: String,
        #[arg(long)]
        old_generation: String,
        #[arg(long)]
        settings_revision: i64,
        #[arg(long)]
        snapshot: String,
        #[arg(long)]
        handoff_file: PathBuf,
        #[arg(long)]
        expected_task_version: i64,
    },
    Finish {
        #[arg(long)]
        intent: String,
    },
    Resume {
        #[arg(long)]
        session: String,
        #[arg(long)]
        prompt_file: PathBuf,
    },
}

#[derive(Debug, Args)]
struct DiagnosticsArgs {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 200)]
    limit: usize,
    #[command(subcommand)]
    command: Option<DiagnosticsCommand>,
}

#[derive(Debug, Args)]
struct DatabaseArgs {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: DatabaseCommand,
}

#[derive(Debug, Subcommand)]
enum DatabaseCommand {
    Inspect,
    Check,
    Backup {
        #[arg(long)]
        backup_dir: Option<PathBuf>,
    },
    Verify {
        backup: PathBuf,
    },
    Restore {
        backup: PathBuf,
    },
    ReleaseHold,
}

#[derive(Debug, Subcommand)]
enum DiagnosticsCommand {
    Export {
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Args)]
struct LogsArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 200)]
    limit: usize,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    attempt: String,
    #[arg(long, conflicts_with = "check")]
    suite: Option<String>,
    #[arg(long, conflicts_with = "suite")]
    check: Option<String>,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: ImportCommand,
}

#[derive(Debug, Subcommand)]
enum ImportCommand {
    Preview {
        #[arg(long)]
        source: PathBuf,
    },
    Apply {
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        project: String,
        #[arg(long)]
        expected_project_version: i64,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        expected_source_hash: String,
    },
}

#[derive(Debug, Args)]
struct RoleArgs {
    #[command(subcommand)]
    command: RoleCommand,
}

#[derive(Debug, Subcommand)]
enum RoleCommand {
    Context,
    #[command(after_long_help = crate::domain::ROLE_RESULT_REPORT_CONTRACT)]
    Report {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["json", "file"])]
        runtime_v1: bool,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        operation_id: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        status: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        nonce: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        model_evidence: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        sandbox_identity: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        session_mode: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        target_data_accessed: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        fallback_observed: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        authentication_observed: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        failure_category: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        cmux_stderr_hex: Option<String>,
        #[arg(long, require_equals = true, requires = "runtime_v1")]
        actual: Vec<String>,
    },
    ProposeTransition {
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        phase: String,
        #[arg(long = "evidence", required = true)]
        evidence: Vec<String>,
    },
    AcknowledgeGuidance {
        #[arg(long)]
        guidance: String,
    },
    SetupRead {
        #[arg(long)]
        relative_path: String,
    },
    RecordExplorerDecision {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
    },
    ConfigureLanes {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
    },
    RequestIntegration {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
    },
    YieldLane {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
    },
    SelectChecks {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
    },
    SubmitConformance {
        #[arg(long, conflicts_with = "file")]
        json: Option<String>,
        #[arg(long, conflicts_with = "json")]
        file: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct HookArgs {
    #[arg(long)]
    provider: String,
    #[arg(long)]
    event: String,
}

#[derive(Debug, Args)]
struct InternalLaunchArgs {
    #[arg(long)]
    anchor: PathBuf,
    #[arg(long)]
    executable: PathBuf,
    #[arg(last = true, allow_hyphen_values = true)]
    arguments: Vec<String>,
}

#[derive(Debug, Args)]
struct UnixConnectProbeArgs {
    #[arg(long)]
    path: PathBuf,
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve(args) => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
            crate::server::serve(
                InstancePaths::resolve(args.data_dir)?,
                args.port,
                !args.no_open,
            )
            .await
        }
        Command::Status(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::Status,
            )
            .await
        }
        Command::Stop(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::Stop { drain: args.drain },
            )
            .await
        }
        Command::Attach(args) => run_attach(args).await,
        Command::Capability(args) => run_capability(args).await,
        Command::Apply(args) => run_apply(args).await,
        Command::State(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::State,
            )
            .await
        }
        Command::Scheduler(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::SchedulerRunOnce,
            )
            .await
        }
        Command::Snapshot(args) => run_snapshot(args).await,
        Command::Dispatch(args) => run_dispatch(args).await,
        Command::DispatchSetup(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::TripSetupDispatch {
                    attempt_id: args.attempt,
                    role: args.role.parse().map_err(|error: String| anyhow!(error))?,
                },
            )
            .await
        }
        Command::DeliverGuidance(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::GuidanceDeliver {
                    guidance_id: args.guidance,
                },
            )
            .await
        }
        Command::SwitchRole(args) => run_switch_role(args).await,
        Command::Diagnostics(args) => {
            let paths = InstancePaths::resolve(args.data_dir)?;
            match args.command {
                Some(DiagnosticsCommand::Export { output }) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&crate::export::export_sanitized(
                            &paths, &output,
                        )?)?
                    );
                    Ok(())
                }
                None => {
                    print_control(&paths, ControlRequest::Diagnostics { limit: args.limit }).await
                }
            }
        }
        Command::Database(args) => {
            let paths = InstancePaths::resolve(args.data_dir)?;
            let result = match args.command {
                DatabaseCommand::Inspect => crate::database::inspect(&paths),
                DatabaseCommand::Check => crate::database::check(&paths),
                DatabaseCommand::Backup { backup_dir } => {
                    crate::database::backup(&paths, backup_dir.as_deref())
                }
                DatabaseCommand::Verify { backup } => crate::database::verify(&backup),
                DatabaseCommand::Restore { backup } => crate::database::restore(&paths, &backup),
                DatabaseCommand::ReleaseHold => crate::database::release_hold(&paths),
            }?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Command::Logs(args) => {
            let paths = InstancePaths::resolve(args.data_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::export::offline_logs(
                    &paths,
                    args.limit.min(2_000)
                )?)?
            );
            Ok(())
        }
        Command::Import(args) => run_import(args).await,
        Command::Check(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::CheckRun {
                    attempt_id: args.attempt,
                    suite_name: args.suite,
                    check_id: args.check,
                },
            )
            .await
        }
        Command::Role(args) => run_role(args).await,
        Command::ResumeWork(args) => {
            if !args.eligible && args.session.is_empty() {
                bail!("resume-work requires --eligible or one or more --session values")
            }
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::RestartResume {
                    operation_id: args
                        .operation_id
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    session_ids: if args.eligible {
                        None
                    } else {
                        Some(args.session)
                    },
                },
            )
            .await
        }
        Command::RestartPreview(args) => {
            print_control(
                &InstancePaths::resolve(args.data_dir)?,
                ControlRequest::RestartPreview,
            )
            .await
        }
        Command::Hook(args) => run_hook(args).await,
        Command::InternalLaunch(args) => run_internal_launch(args),
        Command::UnixConnectProbe(args) => run_unix_connect_probe(args).await,
    }
}

async fn run_unix_connect_probe(args: UnixConnectProbeArgs) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = args.path;
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute() || bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
        bail!("unix-connect path_error path must be a bounded absolute Unix socket path")
    }
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::UnixStream::connect(&path),
    )
    .await
    {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(())
        }
        Ok(Err(error)) => {
            let errno = error.raw_os_error();
            let class = match errno {
                Some(libc::EPERM) => "EPERM",
                Some(libc::EACCES) => "EACCES",
                _ => "OTHER",
            };
            bail!(
                "unix-connect os_error errno={} class={class}",
                errno.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
            )
        }
        Err(_) => bail!("unix-connect timeout after 2 seconds"),
    }
}

fn run_internal_launch(args: InternalLaunchArgs) -> Result<()> {
    let pid = std::process::id();
    if unsafe { libc::getpgrp() } != pid as i32 && unsafe { libc::setpgid(0, 0) } != 0 {
        let error = std::io::Error::last_os_error();
        publish_pre_provider_failure(
            &args.anchor,
            &format!("isolate provider launch process group: {error}"),
        );
        return Err(error).context("isolate provider launch process group");
    }
    let process_group_id = unsafe { libc::getpgrp() };
    let boot_identity = match crate::supervisor::system_boot_identity() {
        Ok(identity) => identity,
        Err(error) => {
            publish_pre_provider_failure(&args.anchor, &format!("{error:#}"));
            return Err(error);
        }
    };
    let leader_start = match crate::supervisor::native_start_marker(pid) {
        Ok(start) => start,
        Err(error) => {
            publish_pre_provider_failure(&args.anchor, &format!("{error:#}"));
            return Err(error);
        }
    };
    let leader = ProcessGenerationAnchor {
        pid,
        process_group_id,
        native_start_marker: leader_start,
        boot_identity: boot_identity.clone(),
    };
    let mut child = match std::process::Command::new(&args.executable)
        .args(&args.arguments)
        .spawn()
        .with_context(|| format!("spawn provider {}", args.executable.display()))
    {
        Ok(child) => child,
        Err(error) => {
            publish_pre_provider_failure(&args.anchor, &format!("{error:#}"));
            return Err(error);
        }
    };
    let child_pid = child.id();
    let provider_group = unsafe { libc::getpgid(child_pid as libc::pid_t) };
    if provider_group <= 0 || provider_group != process_group_id {
        let _ = child.kill();
        let _ = child.wait();
        bail!("provider child did not inherit the supervised process group")
    }
    let publish = (|| -> Result<()> {
        let provider = ProcessGenerationAnchor {
            pid: child_pid,
            process_group_id: provider_group,
            native_start_marker: crate::supervisor::native_start_marker(child_pid)?,
            boot_identity,
        };
        let handshake = LaunchHandshake::ProviderSpawned { leader, provider };
        crate::config::atomic_write(&args.anchor, &serde_json::to_vec(&handshake)?)?;
        Ok(())
    })();
    if let Err(error) = publish {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error).context("publish provider generation anchor");
    }
    let status = child.wait().context("wait for supervised provider")?;
    std::process::exit(status.code().unwrap_or(128));
}

fn publish_pre_provider_failure(path: &std::path::Path, reason: &str) {
    let acknowledgement = LaunchHandshake::PreProviderSpawnFailed {
        reason: reason.to_owned(),
    };
    if let Ok(bytes) = serde_json::to_vec(&acknowledgement) {
        let _ = crate::config::atomic_write(path, &bytes);
    }
}

async fn run_capability(args: CapabilityArgs) -> Result<()> {
    let paths = InstancePaths::resolve(args.data_dir)?;
    let request = match args.command {
        CapabilityCommand::Launch {
            cell,
            provider,
            role,
            project,
            model,
            effort,
            prompt_file,
            operation_id,
        } => {
            let prompt = std::fs::read_to_string(&prompt_file)
                .with_context(|| format!("read {}", prompt_file.display()))?;
            ControlRequest::CapabilityLaunch {
                request: ValidationLaunchRequest {
                    operation_id: operation_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    cell,
                    provider: Provider::from_str(&provider).map_err(|error| anyhow!(error))?,
                    role: crate::domain::RoleKind::from_str(&role)
                        .map_err(|error| anyhow!(error))?,
                    project_path: project,
                    model,
                    effort,
                    prompt,
                },
            }
        }
        CapabilityCommand::WorkflowLaunch {
            cell,
            provider,
            role,
            project,
            task,
            attempt,
            model,
            effort,
            prompt_file,
            operation_id,
        } => {
            let prompt = std::fs::read_to_string(&prompt_file)
                .with_context(|| format!("read {}", prompt_file.display()))?;
            ControlRequest::CapabilityWorkflowLaunch {
                request: crate::domain::WorkflowValidationRequest {
                    launch: ValidationLaunchRequest {
                        operation_id: operation_id
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        cell,
                        provider: Provider::from_str(&provider).map_err(|error| anyhow!(error))?,
                        role: crate::domain::RoleKind::from_str(&role)
                            .map_err(|error| anyhow!(error))?,
                        project_path: project,
                        model,
                        effort,
                        prompt,
                    },
                    task_id: task,
                    attempt_id: attempt,
                },
            }
        }
        CapabilityCommand::List => ControlRequest::CapabilityList,
        CapabilityCommand::Inspect { session } => ControlRequest::CapabilityInspect {
            session_id: session,
        },
        CapabilityCommand::Transcript {
            session,
            after_epoch,
            after_sequence,
            limit_bytes,
            raw,
        } => {
            let response = control::request(
                &paths.control_socket,
                &ControlRequest::CapabilityTranscript {
                    session_id: session,
                    after_epoch,
                    after_sequence,
                    limit_bytes,
                },
            )
            .await?;
            if raw {
                let page: crate::domain::TranscriptPage = serde_json::from_value(response.result)?;
                for frame in page.frames {
                    if frame.encoding == "base64" {
                        std::io::Write::write_all(
                            &mut std::io::stdout(),
                            &base64::engine::general_purpose::STANDARD.decode(frame.data)?,
                        )?;
                    } else {
                        println!("\n[LLMRelay capture gap: {}]", frame.data);
                    }
                }
            } else {
                println!("{}", serde_json::to_string_pretty(&response.result)?);
            }
            return Ok(());
        }
        CapabilityCommand::AcquireInput {
            session,
            owner,
            seconds,
        } => ControlRequest::CapabilityAcquireInput {
            session_id: session,
            owner_id: owner,
            seconds,
        },
        CapabilityCommand::RenewInput {
            session,
            lease,
            seconds,
        } => ControlRequest::CapabilityRenewInput {
            session_id: session,
            lease,
            seconds,
        },
        CapabilityCommand::TakeoverInput {
            session,
            owner,
            seconds,
        } => ControlRequest::CapabilityTakeoverInput {
            session_id: session,
            owner_id: owner,
            seconds,
        },
        CapabilityCommand::SendInput { session, lease } => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes)?;
            ControlRequest::CapabilitySendInput {
                session_id: session,
                lease,
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            }
        }
        CapabilityCommand::ReleaseInput { session, lease } => {
            ControlRequest::CapabilityReleaseInput {
                session_id: session,
                lease,
            }
        }
        CapabilityCommand::Resize {
            session,
            lease,
            rows,
            cols,
        } => ControlRequest::CapabilityResize {
            session_id: session,
            lease,
            rows,
            cols,
        },
        CapabilityCommand::Interrupt { session } => ControlRequest::CapabilityInterrupt {
            session_id: session,
        },
        CapabilityCommand::Resume {
            session,
            prompt_file,
        } => ControlRequest::CapabilityResume {
            session_id: session,
            prompt: std::fs::read_to_string(&prompt_file)
                .with_context(|| format!("read {}", prompt_file.display()))?,
        },
        CapabilityCommand::RecordProof { file } => {
            let proof: CapabilityProofInput = serde_json::from_slice(
                &std::fs::read(&file).with_context(|| format!("read {}", file.display()))?,
            )
            .context("capability proof file must be valid JSON")?;
            ControlRequest::CapabilityRecordProof { proof }
        }
    };
    print_control(&paths, request).await
}

struct RawTerminal {
    input: OwnedFd,
    original: libc::termios,
    original_flags: libc::c_int,
}

impl RawTerminal {
    fn enter() -> Result<Self> {
        if unsafe { libc::isatty(std::io::stdin().as_raw_fd()) } != 1 {
            bail!("llmrelay attach requires a terminal on standard input")
        }
        // `dup` would retain stdin's open-file description, including its
        // status flags. Open the controlling terminal instead so making input
        // nonblocking cannot also make a shared stdout/stderr descriptor
        // nonblocking.
        // Darwin's kqueue cannot register the /dev/tty alias. Reopen the
        // actual stdin TTY to obtain independent flags and a pollable device.
        let mut tty_path = [0 as libc::c_char; 1024];
        let named = unsafe {
            libc::ttyname_r(
                std::io::stdin().as_raw_fd(),
                tty_path.as_mut_ptr(),
                tty_path.len(),
            )
        };
        if named != 0 {
            return Err(std::io::Error::from_raw_os_error(named))
                .context("resolve attach input terminal");
        }
        let opened = unsafe {
            libc::open(
                tty_path.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        };
        if opened < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open controlling terminal for attach input");
        }
        let input = unsafe { OwnedFd::from_raw_fd(opened) };
        let fd = input.as_raw_fd();
        if unsafe { libc::isatty(fd) } != 1 {
            bail!("controlling terminal for attach input is not a terminal")
        }
        let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error()).context("read terminal settings");
        }
        let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if original_flags < 0 {
            return Err(std::io::Error::last_os_error()).context("read terminal flags");
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error()).context("enter raw terminal mode");
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } != 0 {
            let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
            return Err(std::io::Error::last_os_error()).context("set nonblocking terminal input");
        }
        Ok(Self {
            input,
            original,
            original_flags,
        })
    }

    fn fd(&self) -> libc::c_int {
        self.input.as_raw_fd()
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let fd = self.input.as_raw_fd();
        let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, self.original_flags) };
        let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &self.original) };
    }
}

struct TerminalInput(libc::c_int);

impl AsRawFd for TerminalInput {
    fn as_raw_fd(&self) -> libc::c_int {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttachmentInputAction {
    Detach,
    Forward,
    Ignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistentCmuxInputAuthority {
    ViewOnly,
    Control,
    Hold,
}

/// Persistent cmux authority is a connection-local capability, not a direct
/// reflection of durable `actual_input_state`. In particular, an
/// `unknown_live` sync deliberately preserves the durable actual state while
/// putting this client into Hold so it cannot renew, resize, write, or infer
/// input authority from an old durable control acknowledgement.
fn persistent_cmux_input_authority(result: &serde_json::Value) -> PersistentCmuxInputAuthority {
    match result["state"].as_str() {
        Some("unknown_live") => PersistentCmuxInputAuthority::Hold,
        Some("control") if result["actual_input_state"].as_str() == Some("control") => {
            PersistentCmuxInputAuthority::Control
        }
        _ => PersistentCmuxInputAuthority::ViewOnly,
    }
}

fn attachment_input_action(bytes: &[u8], owns_input: bool) -> AttachmentInputAction {
    if bytes.is_empty() || bytes.contains(&0x1d) {
        AttachmentInputAction::Detach
    } else if owns_input {
        AttachmentInputAction::Forward
    } else {
        // Read-only watches still receive terminal-generated replies and focus
        // sequences.  They are neither user commands nor provider input and
        // should not be echoed into the provider output surface.
        AttachmentInputAction::Ignore
    }
}

fn attachment_notice(message: &str) {
    let mut error = std::io::stderr().lock();
    let _ = write!(error, "\r\n{message}\r\n");
    let _ = error.flush();
}

async fn read_terminal_input(
    input: &tokio::io::unix::AsyncFd<TerminalInput>,
    bytes: &mut [u8],
) -> Result<usize> {
    loop {
        let mut ready = input.readable().await?;
        let read = unsafe {
            libc::read(
                input.get_ref().as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        ready.clear_ready();
        if read >= 0 {
            return Ok(read as usize);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
    if unsafe { libc::ioctl(std::io::stdout().as_raw_fd(), libc::TIOCGWINSZ, &mut size) } != 0
        || size.ws_row == 0
        || size.ws_col == 0
    {
        return None;
    }
    Some((size.ws_row, size.ws_col))
}

const ATTACHMENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

async fn attachment_request(
    connection: &mut control::ControlConnection,
    request: &ControlRequest,
) -> Result<control::ControlResponse> {
    connection
        .request_with_timeout(request, ATTACHMENT_REQUEST_TIMEOUT)
        .await
}

async fn catch_up_attachment(
    connection: &mut control::ControlConnection,
    cursor: &mut (Option<String>, u64),
    output: &mut std::io::StdoutLock<'_>,
) -> Result<bool> {
    // Exactly one bounded page per scheduling turn: a continuous producer
    // cannot keep the control connection away from input, signals, or renewals.
    let response = attachment_request(
        connection,
        &ControlRequest::AttachmentTranscript {
            after_epoch: cursor.0.clone(),
            after_sequence: cursor.1,
            limit_bytes: 256 * 1024,
        },
    )
    .await?;
    let page: crate::domain::TranscriptPage = serde_json::from_value(response.result)?;
    if page.frames.is_empty() {
        return Ok(false);
    }
    for frame in &page.frames {
        if frame.gap {
            output
                .write_all(format!("\r\n[LLMRelay capture gap: {}]\r\n", frame.data).as_bytes())?;
        } else if frame.encoding == "base64" {
            output.write_all(&base64::engine::general_purpose::STANDARD.decode(&frame.data)?)?;
        } else {
            bail!("unsupported transcript encoding {}", frame.encoding)
        }
    }
    output.flush()?;
    // A cursor is never advanced before the page has been emitted successfully.
    cursor.0 = page.next_epoch;
    cursor.1 = page.next_sequence;
    Ok(page.has_more)
}

fn expected_attachment_binding(args: &AttachArgs) -> Result<Option<AttachmentBinding>> {
    uuid::Uuid::parse_str(&args.session).context("attach session must be a UUID")?;
    let values_present = [
        args.expected_generation.is_some(),
        args.expected_epoch.is_some(),
        args.expected_pid.is_some(),
        args.expected_process_group.is_some(),
        args.expected_start_marker.is_some(),
        args.expected_observed_at.is_some(),
    ];
    if values_present.iter().all(|present| !present) {
        if let Some(route_id) = args
            .cmux_route_id
            .as_deref()
            .or(args.cmux_surface_route_id.as_deref())
        {
            uuid::Uuid::parse_str(route_id).context("cmux attachment route must be a UUID")?;
            bail!("a cmux attachment route requires every expected binding field")
        }
        return Ok(None);
    }
    if values_present.iter().any(|present| !present) {
        bail!(
            "expected attachment binding requires generation, epoch, and complete process identity"
        )
    }
    if let Some(route_id) = args.cmux_route_id.as_deref() {
        uuid::Uuid::parse_str(route_id).context("cmux attachment route must be a UUID")?;
    }
    if let Some(surface_route_id) = args.cmux_surface_route_id.as_deref() {
        uuid::Uuid::parse_str(surface_route_id)
            .context("persistent cmux session surface route must be a UUID")?;
        if args.cmux_binding_revision.unwrap_or_default() <= 0 {
            bail!("persistent cmux session surface requires a positive binding revision")
        }
    }
    let binding = AttachmentBinding {
        session_id: args.session.clone(),
        role_generation_id: args.expected_generation.clone().expect("checked above"),
        transcript_epoch: args.expected_epoch.clone().expect("checked above"),
        process: ProcessIdentity {
            pid: args.expected_pid.expect("checked above"),
            process_group_id: args.expected_process_group.expect("checked above"),
            native_start_marker: args.expected_start_marker.clone().expect("checked above"),
            observed_started_at: args.expected_observed_at.clone().expect("checked above"),
        },
    };
    binding
        .validate_route_identifiers()
        .map_err(|error| anyhow!(error))?;
    Ok(Some(binding))
}

async fn run_attach(args: AttachArgs) -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let expected_binding = expected_attachment_binding(&args)?;
    let persistent_cmux_surface = args.cmux_surface_route_id.is_some();
    if persistent_cmux_surface && (!args.view_only || args.takeover) {
        bail!("a persistent cmux attachment must begin with --view-only and cannot use --takeover")
    }
    let paths = InstancePaths::resolve(args.data_dir)?;
    let mut connection = control::ControlConnection::connect_for_kind(
        &paths.control_socket,
        crate::protocol::ClientKind::Attachment,
    )
    .await?;
    let owner_id = format!("terminal-{}", uuid::Uuid::new_v4());
    let attach_request = ControlRequest::Attach {
        session_id: args.session.clone(),
        owner_id,
        seconds: 30,
        takeover: args.takeover,
        view_only: args.view_only,
        expected_binding: expected_binding.clone(),
        cmux_route_id: args.cmux_route_id.clone(),
        cmux_surface_route_id: args.cmux_surface_route_id.clone(),
        cmux_binding_revision: args.cmux_binding_revision,
    };
    let attached = match attachment_request(&mut connection, &attach_request).await {
        Ok(response) => response,
        Err(error) => {
            // This is the only failure report a cmux-created client emits.
            // It records no provider action and lets a later dashboard retry
            // create a fresh owned surface instead of using this one.
            if let (Some(route_id), Some(binding), Some(binding_revision)) = (
                args.cmux_surface_route_id.as_deref(),
                expected_binding.as_ref(),
                args.cmux_binding_revision,
            ) {
                let _ = attachment_request(
                    &mut connection,
                    &ControlRequest::CmuxSessionSurfaceFailed {
                        surface_route_id: route_id.to_owned(),
                        binding: binding.clone(),
                        binding_revision,
                    },
                )
                .await;
            } else if let (Some(route_id), Some(binding)) =
                (args.cmux_route_id.as_deref(), expected_binding.as_ref())
            {
                let _ = attachment_request(
                    &mut connection,
                    &ControlRequest::CmuxAttachmentFailed {
                        route_id: route_id.to_owned(),
                        binding: binding.clone(),
                    },
                )
                .await;
            }
            return Err(error);
        }
    };
    let mut persistent_authority = if persistent_cmux_surface {
        persistent_cmux_input_authority(&attached.result)
    } else {
        PersistentCmuxInputAuthority::ViewOnly
    };
    let mut owns_input = if persistent_cmux_surface {
        persistent_authority == PersistentCmuxInputAuthority::Control
    } else {
        attached.result["mode"].as_str() == Some("owner")
    };
    if owns_input && persistent_authority != PersistentCmuxInputAuthority::Hold {
        attachment_notice(
            "attached with input ownership; Ctrl-] detaches without stopping the provider",
        );
    } else if persistent_cmux_surface {
        attachment_notice("attached view-only to the persistent cmux surface; dashboard Take or Release changes a revision on this same authenticated connection. Ctrl-] detaches without stopping the provider");
    } else if args.view_only {
        attachment_notice("attached read-only; output remains connected without an input lease. Ctrl-] detaches, and dashboard Take keyboard control opens a separate explicit control attachment");
    } else {
        attachment_notice(
            "attached view-only because another human owns input; rerun with --takeover to take control",
        );
    }

    let terminal = RawTerminal::enter()?;
    let input = tokio::io::unix::AsyncFd::new(TerminalInput(terminal.fd()))?;
    let mut window_changed = signal(SignalKind::window_change())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut cursor = (None, 0_u64);
    let mut ticker = tokio::time::interval(Duration::from_millis(50));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut immediate_catch_up = false;
    let mut last_renewal = Instant::now();
    let mut last_control_sync = Instant::now() - Duration::from_millis(250);
    let mut input_bytes = [0_u8; 16 * 1024];
    let mut output = std::io::stdout().lock();
    if owns_input {
        if let Some((rows, cols)) = terminal_size() {
            attachment_request(
                &mut connection,
                &ControlRequest::AttachmentResize { rows, cols },
            )
            .await?;
        }
    }
    loop {
        let mut poll_output = false;
        tokio::select! {
            _ = ticker.tick(), if !immediate_catch_up => {
                poll_output = true;
            }
            // Yield before scheduling the next bounded page so ready terminal
            // input and signals retain fair access to this connection.
            _ = tokio::task::yield_now(), if immediate_catch_up => {
                poll_output = true;
            }
            read = read_terminal_input(&input, &mut input_bytes) => {
                let count = read?;
                match attachment_input_action(&input_bytes[..count], owns_input) {
                    AttachmentInputAction::Detach => {
                        let _ = attachment_request(&mut connection, &ControlRequest::AttachmentDetach).await;
                        return Ok(());
                    }
                    AttachmentInputAction::Forward => {
                        attachment_request(
                            &mut connection,
                            &ControlRequest::AttachmentSendInput {
                                data_base64: base64::engine::general_purpose::STANDARD.encode(&input_bytes[..count]),
                            },
                        )
                        .await?;
                    }
                    AttachmentInputAction::Ignore => {}
                }
            }
            _ = window_changed.recv(), if owns_input && persistent_authority != PersistentCmuxInputAuthority::Hold => {
                if let Some((rows, cols)) = terminal_size() {
                    attachment_request(
                        &mut connection,
                        &ControlRequest::AttachmentResize { rows, cols },
                    )
                    .await?;
                }
            }
            _ = terminate.recv() => {
                let _ = attachment_request(&mut connection, &ControlRequest::AttachmentDetach).await;
                return Ok(());
            }
            _ = interrupt.recv() => {
                let _ = attachment_request(&mut connection, &ControlRequest::AttachmentDetach).await;
                return Ok(());
            }
            _ = hangup.recv() => {
                let _ = attachment_request(&mut connection, &ControlRequest::AttachmentDetach).await;
                return Ok(());
            }
        }
        if poll_output {
            if persistent_cmux_surface && last_control_sync.elapsed() >= Duration::from_millis(250)
            {
                last_control_sync = Instant::now();
                match attachment_request(&mut connection, &ControlRequest::AttachmentControlSync)
                    .await
                {
                    Ok(response) => {
                        let state = response.result["state"].as_str().unwrap_or("view_only");
                        if state == "retired" {
                            attachment_notice(response.result["message"].as_str().unwrap_or(
                                "persistent cmux attachment retired; detaching without stopping the provider",
                            ));
                            // Retire has already cleared any in-memory lease.
                            // Explicitly detach so the exact durable attachment
                            // records ended before a later View can reserve a
                            // replacement.
                            let _ = attachment_request(
                                &mut connection,
                                &ControlRequest::AttachmentDetach,
                            )
                            .await;
                            return Ok(());
                        }
                        let next_authority = persistent_cmux_input_authority(&response.result);
                        let next_owns_input =
                            next_authority == PersistentCmuxInputAuthority::Control;
                        if next_authority == PersistentCmuxInputAuthority::Hold {
                            if persistent_authority != PersistentCmuxInputAuthority::Hold {
                                attachment_notice(response.result["message"].as_str().unwrap_or(
                                    "cmux presentation is uncertain; this attachment remains connected for output but holds all input authority until a later authenticated sync resolves the exact surface",
                                ));
                            }
                        } else if next_owns_input && !owns_input {
                            attachment_notice("dashboard keyboard-control intent is active on this authenticated attachment; Ctrl-] detaches and releases it without stopping the provider");
                            if let Some((rows, cols)) = terminal_size() {
                                attachment_request(
                                    &mut connection,
                                    &ControlRequest::AttachmentResize { rows, cols },
                                )
                                .await?;
                            }
                            last_renewal = Instant::now();
                        } else if !next_owns_input && owns_input {
                            attachment_notice(response.result["message"].as_str().unwrap_or(
                                "keyboard control returned to view-only; submit a new explicit dashboard Take request to try again",
                            ));
                        }
                        persistent_authority = next_authority;
                        owns_input = next_owns_input;
                    }
                    Err(error) => {
                        if connection.is_poisoned() {
                            return Err(error);
                        }
                        attachment_notice(&format!(
                            "persistent keyboard-control sync was not applied ({error:#}); this terminal remains view-only until the next authenticated sync",
                        ));
                        persistent_authority = PersistentCmuxInputAuthority::ViewOnly;
                        owns_input = false;
                    }
                }
            }
            if owns_input
                && persistent_authority != PersistentCmuxInputAuthority::Hold
                && last_renewal.elapsed() >= Duration::from_secs(10)
            {
                match attachment_request(
                    &mut connection,
                    &ControlRequest::AttachmentRenew { seconds: 30 },
                )
                .await
                {
                    Ok(_) => last_renewal = Instant::now(),
                    Err(error) => {
                        owns_input = false;
                        if connection.is_poisoned() {
                            return Err(error);
                        }
                        attachment_notice(&format!("input ownership ended ({error:#}); this attachment is now view-only; submit a new explicit dashboard Take request to try again"));
                    }
                }
            }
            immediate_catch_up =
                catch_up_attachment(&mut connection, &mut cursor, &mut output).await?;
        }
    }
}

async fn run_apply(args: ApplyArgs) -> Result<()> {
    let mut input = String::new();
    if let Some(path) = args.file {
        input =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    } else {
        std::io::stdin().read_to_string(&mut input)?;
    }
    let command: HumanCommand =
        serde_json::from_str(&input).context("command must be a valid HumanCommand JSON object")?;
    print_control(
        &InstancePaths::resolve(args.data_dir)?,
        ControlRequest::HumanCommand { command },
    )
    .await
}

async fn run_snapshot(args: SnapshotArgs) -> Result<()> {
    let request = match args.command {
        SnapshotCommand::Freeze { attempt, kind } => ControlRequest::SnapshotFreeze {
            attempt_id: attempt,
            snapshot_kind: kind,
        },
        SnapshotCommand::Verify { snapshot } => ControlRequest::SnapshotVerify {
            snapshot_id: snapshot,
        },
    };
    print_control(&InstancePaths::resolve(args.data_dir)?, request).await
}

async fn run_dispatch(args: DispatchArgs) -> Result<()> {
    let role = RoleKind::from_str(&args.role).map_err(|error| anyhow!(error))?;
    let prompt = std::fs::read_to_string(&args.prompt_file)
        .with_context(|| format!("read {}", args.prompt_file.display()))?;
    print_control(
        &InstancePaths::resolve(args.data_dir)?,
        ControlRequest::RoleDispatch {
            attempt_id: args.attempt,
            role,
            lane: args.lane,
            prompt,
        },
    )
    .await
}

async fn run_switch_role(args: SwitchRoleArgs) -> Result<()> {
    let request = match args.command {
        SwitchRoleCommand::Request {
            operation_id,
            attempt,
            role,
            old_generation,
            settings_revision,
            snapshot,
            handoff_file,
            expected_task_version,
        } => {
            let handoff: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&handoff_file)
                    .with_context(|| format!("read {}", handoff_file.display()))?,
            )?;
            ControlRequest::RoleSwitchRequest {
                operation_id,
                attempt_id: attempt,
                role,
                old_generation_id: old_generation,
                settings_revision,
                snapshot_id: snapshot,
                handoff,
                expected_task_version,
            }
        }
        SwitchRoleCommand::Finish { intent } => {
            ControlRequest::RoleSwitchFinish { intent_id: intent }
        }
        SwitchRoleCommand::Resume {
            session,
            prompt_file,
        } => ControlRequest::RoleResume {
            session_id: session,
            prompt: std::fs::read_to_string(&prompt_file)
                .with_context(|| format!("read {}", prompt_file.display()))?,
        },
    };
    print_control(&InstancePaths::resolve(args.data_dir)?, request).await
}

async fn run_import(args: ImportArgs) -> Result<()> {
    let paths = InstancePaths::resolve(args.data_dir)?;
    let request = match args.command {
        ImportCommand::Preview { source } => ControlRequest::LegacyPreview { source },
        ImportCommand::Apply {
            operation_id,
            project,
            expected_project_version,
            source,
            expected_source_hash,
        } => ControlRequest::LegacyImport {
            operation_id,
            project_id: project,
            expected_project_version,
            source,
            expected_source_hash,
        },
    };
    print_control(&paths, request).await
}

async fn print_control(paths: &InstancePaths, request: ControlRequest) -> Result<()> {
    let response = control::request(&paths.control_socket, &request).await?;
    println!("{}", serde_json::to_string_pretty(&response.result)?);
    Ok(())
}

async fn run_role(args: RoleArgs) -> Result<()> {
    let socket = env_path("AGENTICJIRA_ROLE_SOCKET")?;
    let credential =
        std::env::var("AGENTICJIRA_ROLE_TOKEN").context("AGENTICJIRA_ROLE_TOKEN is not set")?;
    let operation = match args.command {
        RoleCommand::Context => RoleOperation::Context,
        RoleCommand::Report {
            json,
            file,
            runtime_v1,
            operation_id,
            status,
            nonce,
            model_evidence,
            sandbox_identity,
            session_mode,
            target_data_accessed,
            fallback_observed,
            authentication_observed,
            failure_category,
            cmux_stderr_hex,
            actual,
        } => {
            if runtime_v1 {
                let mut arguments = vec!["--runtime-v1".to_owned()];
                for (name, value) in [
                    ("operation-id", operation_id),
                    ("status", status),
                    ("nonce", nonce),
                    ("model-evidence", model_evidence),
                    ("sandbox-identity", sandbox_identity),
                    ("session-mode", session_mode),
                    ("target-data-accessed", target_data_accessed),
                    ("fallback-observed", fallback_observed),
                    ("authentication-observed", authentication_observed),
                    ("failure-category", failure_category),
                    ("cmux-stderr-hex", cmux_stderr_hex),
                ] {
                    if let Some(value) = value {
                        arguments.push(format!("--{name}={value}"));
                    }
                }
                arguments.extend(actual.into_iter().map(|value| format!("--actual={value}")));
                let report = crate::domain::parse_runtime_role_report_args(&arguments)
                    .map_err(anyhow::Error::msg)?;
                RoleOperation::Report { report }
            } else {
                let mut input = match (json, file) {
                    (Some(json), None) => json,
                    (None, Some(path)) => std::fs::read_to_string(&path)
                        .with_context(|| format!("read {}", path.display()))?,
                    (None, None) => String::new(),
                    (Some(_), Some(_)) => unreachable!("clap enforces argument conflicts"),
                };
                if input.is_empty() {
                    std::io::stdin().read_to_string(&mut input)?;
                }
                let report: RoleResultReport = serde_json::from_str(&input)
                    .context("role report stdin must be valid structured JSON")?;
                RoleOperation::Report { report }
            }
        }
        RoleCommand::ProposeTransition {
            operation_id,
            phase,
            evidence,
        } => RoleOperation::ProposeTransition {
            operation_id,
            phase,
            evidence,
        },
        RoleCommand::AcknowledgeGuidance { guidance } => RoleOperation::AcknowledgeGuidance {
            guidance_id: guidance,
        },
        RoleCommand::SetupRead { relative_path } => RoleOperation::SetupRead { relative_path },
        RoleCommand::RecordExplorerDecision { json, file } => {
            RoleOperation::RecordExplorerDecision {
                input: role_json_input(json, file)?,
            }
        }
        RoleCommand::ConfigureLanes { json, file } => RoleOperation::ConfigureLanes {
            input: role_json_input(json, file)?,
        },
        RoleCommand::RequestIntegration { json, file } => RoleOperation::RequestIntegration {
            input: role_json_input(json, file)?,
        },
        RoleCommand::YieldLane { json, file } => RoleOperation::YieldLane {
            input: role_json_input(json, file)?,
        },
        RoleCommand::SelectChecks { json, file } => RoleOperation::SelectChecks {
            input: role_json_input(json, file)?,
        },
        RoleCommand::SubmitConformance { json, file } => RoleOperation::SubmitConformance {
            input: role_json_input(json, file)?,
        },
    };
    let response = task_cli::request(
        &socket,
        &RoleRequest {
            credential,
            operation,
        },
    )
    .await?;
    println!("{}", serde_json::to_string(&response.result)?);
    Ok(())
}

fn role_json_input(json: Option<String>, file: Option<PathBuf>) -> Result<serde_json::Value> {
    let mut input = match (json, file) {
        (Some(json), None) => json,
        (None, Some(path)) => {
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
        }
        (None, None) => String::new(),
        (Some(_), Some(_)) => unreachable!("clap enforces argument conflicts"),
    };
    if input.is_empty() {
        std::io::stdin().read_to_string(&mut input)?;
    }
    let value: serde_json::Value = serde_json::from_str(&input)
        .context("role operation input must be valid structured JSON")?;
    if !value.is_object() {
        bail!("role operation input must be a JSON object")
    }
    Ok(value)
}

async fn run_hook(args: HookArgs) -> Result<()> {
    if args.event == "PermissionRequest" {
        return run_permission_hook(args).await;
    }
    let provider = Provider::from_str(&args.provider).map_err(|error| anyhow!(error))?;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let payload: serde_json::Value =
        serde_json::from_str(&input).context("hook stdin must be JSON")?;
    if payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)
        != Some(args.event.as_str())
    {
        bail!("hook envelope event does not match the trusted configured event")
    }
    run_hook_bridge(provider, payload, false).await
}

async fn run_permission_hook(args: HookArgs) -> Result<()> {
    let prepared = (|| -> Result<(Provider, serde_json::Value)> {
        let provider = Provider::from_str(&args.provider).map_err(|error| anyhow!(error))?;
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .context("read PermissionRequest hook stdin")?;
        let payload: serde_json::Value =
            serde_json::from_str(&input).context("PermissionRequest hook stdin must be JSON")?;
        if !payload.is_object()
            || payload
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str)
                != Some("PermissionRequest")
        {
            bail!("PermissionRequest hook envelope is missing or contradicts its trusted event")
        }
        Ok((provider, payload))
    })();
    let (provider, payload) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let diagnostic = bounded_hook_diagnostic(&format!("{error:#}"));
            println!(
                "{}",
                serde_json::to_string(&crate::permissions::provider_response(
                    false,
                    Some(&format!(
                        "LLMRelay denied malformed PermissionRequest input ({diagnostic}); no action was authorized"
                    ))
                ))?
            );
            return Ok(());
        }
    };
    run_hook_bridge(provider, payload, true).await
}

async fn run_hook_bridge(
    provider: Provider,
    payload: serde_json::Value,
    permission_request: bool,
) -> Result<()> {
    let duration = if permission_request {
        std::time::Duration::from_secs(crate::permissions::LOCAL_PERMISSION_TIMEOUT_SECONDS)
    } else {
        std::time::Duration::from_secs(2)
    };
    let operation = if permission_request {
        RoleOperation::PermissionRequest {
            envelope: HookEnvelope { provider, payload },
        }
    } else {
        RoleOperation::Hook {
            envelope: HookEnvelope { provider, payload },
        }
    };
    let bridge = async {
        let socket = env_path("AGENTICJIRA_ROLE_SOCKET")?;
        let credential =
            std::env::var("AGENTICJIRA_ROLE_TOKEN").context("AGENTICJIRA_ROLE_TOKEN is not set")?;
        task_cli::request(
            &socket,
            &RoleRequest {
                credential,
                operation,
            },
        )
        .await
    };
    match tokio::time::timeout(duration, bridge).await {
        Ok(Ok(response)) if permission_request => {
            println!("{}", serde_json::to_string(&response.result)?);
            Ok(())
        }
        Ok(Ok(_)) => Ok(()),
        outcome if permission_request => {
            let message = match outcome {
                Ok(Err(_)) => {
                    "LLMRelay could not bind this request to live permission authority; use the faithful native terminal prompt"
                }
                Err(_) => {
                    "LLMRelay permission decision timed out; no action was authorized and native input is required"
                }
                Ok(Ok(_)) => unreachable!(),
            };
            println!(
                "{}",
                serde_json::to_string(&crate::permissions::provider_response(
                    false,
                    Some(message)
                ))?
            );
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(_) => bail!("LLMRelay hook acknowledgment timed out after two seconds"),
    }
}

fn bounded_hook_diagnostic(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(240)
        .collect::<String>();
    if value.chars().count() > 240 {
        output.push_str("...");
    }
    output
}

fn env_path(name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os(name).with_context(|| format!("{name} is not set"))?,
    ))
}

#[cfg(test)]
mod attach_input_tests {
    use super::{
        attachment_input_action, persistent_cmux_input_authority, AttachmentInputAction,
        PersistentCmuxInputAuthority,
    };

    #[test]
    fn read_only_attachment_ignores_terminal_input_except_detach() {
        assert_eq!(
            attachment_input_action(b"ordinary keys", false),
            AttachmentInputAction::Ignore
        );
        assert_eq!(
            attachment_input_action(b"\x1b[?1;2c", false),
            AttachmentInputAction::Ignore
        );
        assert_eq!(
            attachment_input_action(&[0x1d], false),
            AttachmentInputAction::Detach
        );
        assert_eq!(
            attachment_input_action(b"ordinary keys", true),
            AttachmentInputAction::Forward
        );
    }

    #[test]
    fn unknown_live_keeps_output_connected_but_never_turns_durable_control_into_local_authority() {
        let unknown_live = serde_json::json!({
            "state": "unknown_live",
            "actual_input_state": "control",
            "desired_input_state": "control",
            "control_revision": 7,
            "applied_revision": 7,
        });
        let authority = persistent_cmux_input_authority(&unknown_live);
        assert_eq!(authority, PersistentCmuxInputAuthority::Hold);
        assert_eq!(
            attachment_input_action(
                b"ordinary keys",
                authority == PersistentCmuxInputAuthority::Control
            ),
            AttachmentInputAction::Ignore,
            "Hold sends no input during this renewal interval"
        );
        assert_eq!(
            persistent_cmux_input_authority(&unknown_live),
            PersistentCmuxInputAuthority::Hold,
            "a later unknown_live sync remains Hold rather than reacquiring from durable actual state"
        );
        assert_eq!(
            persistent_cmux_input_authority(&serde_json::json!({
                "state": "control",
                "actual_input_state": "control",
            })),
            PersistentCmuxInputAuthority::Control,
            "only an authenticated control sync can restore local input authority"
        );
    }
}
