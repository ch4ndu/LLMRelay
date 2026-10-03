import { ErrorNotice } from "./ErrorNotice";
import { useEffect, useState } from "react";
import { command, operation, operationId } from "../api";
import type {
  CmuxSessionSurface,
  CmuxViewOutcome,
  Project,
  TripSetupState,
  TripVerificationCheck,
} from "../types";
import { ProjectSetup } from "./ProjectSetup";

export function ProjectSettings(
  {
    project,
    setup,
    tripChecks = [],
    cmuxSurfaces = {},
    onChanged,
    onViewSession = async () => {
      throw new Error("cmux attachment routing is not available");
    },
  }: {
    project: Project;
    setup?: TripSetupState;
    tripChecks?: TripVerificationCheck[];
    cmuxSurfaces?: Record<string, CmuxSessionSurface>;
    onChanged: () => Promise<void> | void;
    onViewSession?: (
      sessionId: string,
    ) => Promise<CmuxViewOutcome>;
  },
) {
  const [path, setPath] = useState(project.repository_path);
  const [legacyPath, setLegacyPath] = useState("");
  const [legacyPreview, setLegacyPreview] = useState<Record<string, unknown>>();
  const [error, setError] = useState("");
  const [reportReminders, setReportReminders] = useState(
    project.settings.role_report_reminders === true,
  );
  const [savingReminders, setSavingReminders] = useState(false);
  useEffect(() => setPath(project.repository_path), [project.repository_path]);
  useEffect(() => {
    setReportReminders(project.settings.role_report_reminders === true);
  }, [project.id, project.version, project.settings.role_report_reminders]);

  const saveReportReminders = async () => {
    setSavingReminders(true);
    try {
      await command({
        kind: "update_project_settings",
        operation_id: operationId(),
        project_id: project.id,
        expected_version: project.version,
        settings: { ...project.settings, role_report_reminders: reportReminders },
      });
      setError("");
      await onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSavingReminders(false);
    }
  };

  const apply = async (body: Record<string, unknown>) => {
    try {
      await command({ ...body, operation_id: operationId() } as never);
      setError("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const previewLegacy = async () => {
    try {
      setLegacyPreview(
        await operation({ kind: "legacy_preview", source: legacyPath }),
      );
      setError("");
    } catch (cause) {
      setLegacyPreview(undefined);
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  const importLegacy = async () => {
    try {
      await operation({
        kind: "legacy_import",
        operation_id: operationId(),
        project_id: project.id,
        expected_project_version: project.version,
        source: legacyPath,
        expected_source_hash: legacyPreview?.source_hash,
      });
      setLegacyPreview(undefined);
      setLegacyPath("");
      onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };

  return (
    <section className="panel project-settings">
      <header>
        <div>
          <h3>{project.display_name}</h3>
          <span>{project.repository_identity}</span>
        </div>
        <span
          className={project.queue_paused ? "badge waiting" : "badge supported"}
        >
          {project.queue_paused ? "Pickup paused" : "Pickup on"}
        </span>
      </header>
      <label>
        Repository path<div className="compact-fields">
          <input
            value={path}
            onChange={(event) => setPath(event.target.value)}
          />
          <button
            disabled={!path}
            onClick={() =>
              apply({
                kind: "relink_project",
                project_id: project.id,
                path,
                expected_version: project.version,
              })}
          >
            Validate and relink
          </button>
        </div>
        <small className="hint">
          Relinking checks the folder and makes new tasks start from its
          current commit. Use it after moving the folder or committing changes.
          It is available when no task in this project is running.
        </small>
      </label>
      <ProjectSetup
        project={project}
        setup={setup}
        cmuxSurfaces={cmuxSurfaces}
        onChanged={onChanged}
        onViewSession={onViewSession}
      />
      <div>
        <label>
          <input
            type="checkbox"
            checked={reportReminders}
            disabled={savingReminders}
            onChange={(event) => setReportReminders(event.target.checked)}
          />
          Remind agents to submit reports
        </label>
        <p className="hint">
          Off by default. After an eligible agent finishes a turn without its
          required report, LLMRelay may send an additional prompt and use provider
          quota. Final verification is excluded. Each agent generation has two
          reminder reservations for its entire lifetime, including resumed
          sessions. A reservation stays spent even if it is cancelled or delivery
          is uncertain. Turning this off prevents future writes; prompts already
          written cannot be retracted, and turning it back on does not reset the
          allowance.
        </p>
        <button disabled={savingReminders} onClick={saveReportReminders}>
          {savingReminders ? "Saving reminders…" : "Save report reminders"}
        </button>
      </div>
      <details>
        <summary>Verification checks ({tripChecks.length})</summary>
        <p className="hint">
          These checks come from the project's setup. For each task the manager
          selects the ones that apply, and each check still needs your approval
          for the exact result it runs on.
        </p>
        <div className="check-suite-list">
          {tripChecks.map((check) => (
            <article key={check.id}>
              <div>
                <strong>{check.check_key} · {check.category}</strong>
                <small>
                  {check.original_text} · {check.cwd} · {check.timeout_seconds}s
                </small>
              </div>
              <span>
                {check.enabled ? "Inherited" : "Disabled in revision"}
              </span>
              <small>
                Inputs: {check.relevant_inputs.join(", ") ||
                  "declared by manager at selection"}
              </small>
            </article>
          ))}
        </div>
        {!tripChecks.length && (
          <p className="empty">
            No inherited verification commands are activated. Verified
            completion remains blocked when applicable evidence is required.
          </p>
        )}
      </details>
      <details>
        <summary>Import legacy tasks (optional)</summary>
        <p className="hint">
          Import supported task Markdown. This does not register or change a Git
          project. Preview validates the exact source without changing it;
          import is idempotent for this project and preserves the original
          source, frontmatter, sections, runs, bugs, and unknown fields.
        </p>
        <div className="compact-fields">
          <input
            aria-label="Legacy task source"
            value={legacyPath}
            onChange={(event) => {
              setLegacyPath(event.target.value);
              setLegacyPreview(undefined);
            }}
            placeholder="Absolute .md path"
          />
          <button
            disabled={!legacyPath.startsWith("/")}
            onClick={previewLegacy}
          >
            Preview
          </button>
        </div>
        {legacyPreview && (
          <div className="preview">
            <strong>
              {String(
                (legacyPreview.frontmatter as Record<string, unknown>)?.title ||
                  "Validated legacy task",
              )}
            </strong>
            <small>
              {String(legacyPreview.source_hash)} · {String(
                (legacyPreview.validation_runs as unknown[])?.length || 0,
              )} runs · {String(
                (legacyPreview.validation_bugs as unknown[])?.length || 0,
              )} bugs
            </small>
            <button className="primary" onClick={importLegacy}>
              Import unchanged source
            </button>
          </div>
        )}
      </details>
      {error && <ErrorNotice error={error} />}
    </section>
  );
}
