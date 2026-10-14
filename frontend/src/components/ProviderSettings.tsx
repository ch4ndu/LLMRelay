import { useEffect, useRef, useState } from "react";
import {
  cancelProviderValidation,
  inspectProviderValidation,
  previewProviderValidation,
  reconcileProviderValidationStart,
} from "../api";
import type {
  ProviderCheck,
  ProviderCheckState,
  ProviderInstallation,
  ProviderValidationBatch,
  ProviderValidationPreview,
  ProviderValidationSelection,
  ProviderValidationState,
  ValidationProvider,
} from "../types";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { ModelSelector } from "./ModelSelector";

export interface ProviderSettingsProps {
  /** `AppState.provider_validation`; absent from older services. */
  validation?: ProviderValidationState;
  /** Exact models already used elsewhere, offered as suggestions only. */
  knownModels?: Partial<Record<ValidationProvider, string[]>>;
  /** Requests a fresh authoritative state; never called on mount. */
  onChanged: () => void;
}

type SelectionRow = {
  key: number;
  include: boolean;
  provider: ValidationProvider;
  model: string;
  effort: string;
};

const efforts = ["low", "medium", "high", "xhigh", "max", "ultra"];
const START_STORAGE_KEY = "llmrelay.providers.start";

const providerName = (provider: ValidationProvider) =>
  provider === "claude" ? "Claude" : "Codex";

const when = (value: string | null) =>
  value ? new Date(value).toLocaleString() : "unknown";

const checkStateLabels: Record<ProviderCheckState, string> = {
  pending: "Waiting to run",
  reserved: "Starting",
  running: "Running",
  succeeded: "Model access confirmed",
  failed: "Failed",
  delivery_unknown: "Outcome unknown",
  cancel_requested: "Stopping",
  cancelled: "Cancelled",
  skipped: "Skipped",
};

/** The start request exactly as submitted; identical requests share an ID. */
type PendingStart = {
  id: string;
  preview_hash: string;
  selections: ProviderValidationSelection[];
  authorize_model_calls: boolean;
  max_model_calls: number;
};

const readPendingStart = (): PendingStart | undefined => {
  try {
    const value: unknown = JSON.parse(
      localStorage.getItem(START_STORAGE_KEY) || "null",
    );
    if (typeof value !== "object" || value === null) return undefined;
    const id: unknown = Reflect.get(value, "id");
    const hash: unknown = Reflect.get(value, "preview_hash");
    const selections: unknown = Reflect.get(value, "selections");
    const authorize: unknown = Reflect.get(value, "authorize_model_calls");
    const max: unknown = Reflect.get(value, "max_model_calls");
    if (
      typeof id !== "string" || typeof hash !== "string" ||
      typeof authorize !== "boolean" || typeof max !== "number" ||
      !Array.isArray(selections)
    ) return undefined;
    const parsed = selections.flatMap((item: unknown) => {
      if (typeof item !== "object" || item === null) return [];
      const provider: unknown = Reflect.get(item, "provider");
      const model: unknown = Reflect.get(item, "model");
      const effort: unknown = Reflect.get(item, "effort");
      if (
        (provider !== "codex" && provider !== "claude") ||
        typeof model !== "string" || typeof effort !== "string"
      ) return [];
      const selection: ProviderValidationSelection = { provider, model, effort };
      return [selection];
    });
    return parsed.length === selections.length
      ? {
        id,
        preview_hash: hash,
        selections: parsed,
        authorize_model_calls: authorize,
        max_model_calls: max,
      }
      : undefined;
  } catch {
    return undefined;
  }
};

function InstallationFacts({ installation }: { installation: ProviderInstallation }) {
  return (
    <article className="provider-installation">
      <strong>
        {providerName(installation.provider)} ·{" "}
        {installation.ready ? "installed and signed in" : "local setup needs attention"}
      </strong>
      <small>
        {installation.executable_version
          ? `Version ${installation.executable_version}`
          : "Version not found"}
        {" · "}
        {installation.account_identity
          ? `Account ${installation.account_identity}`
          : "Account identity unavailable"}
        {installation.authentication
          ? ` · Sign-in: ${installation.authentication}`
          : ""}
      </small>
      {installation.guidance && (
        <p className="hint">{installation.guidance}</p>
      )}
      <TechnicalDetails>
        <p>{installation.executable_path ?? "No executable path found"}</p>
        {installation.model_calls_disabled_reason && (
          <p>{installation.model_calls_disabled_reason}</p>
        )}
      </TechnicalDetails>
    </article>
  );
}

function CheckRow({ check }: { check: ProviderCheck }) {
  return (
    <li className={`provider-check state-${check.state}`}>
      <strong>
        {providerName(check.provider)} · {check.model} · {check.effort}
      </strong>
      <span>
        {check.state === "succeeded" && check.reused_proof
          ? "Reused a current confirmation"
          : checkStateLabels[check.state]}
      </span>
      {check.state === "delivery_unknown" && (
        <small>
          LLMRelay cannot confirm this session's outcome. It will not start
          another automatically. Use Refresh this check to review its recorded
          state; a possible running session stays uncertain until LLMRelay
          confirms exit.
        </small>
      )}
      {check.state === "running" && (
        <small>
          At its five-minute limit, LLMRelay requests this session to stop.
          Other checks keep their own limits. A session cannot succeed after
          its deadline.
        </small>
      )}
      {check.state === "cancel_requested" && (
        <small>
          Stop requested. This session stays uncertain until LLMRelay confirms
          exit; no new session is started automatically.
        </small>
      )}
      {check.reason && (check.state === "skipped"
        ? (
          <>
            <small>
              This historical check did not start. Inspect local provider setup
              here; authorize and verify coding access within a project task.
            </small>
            <TechnicalDetails><p>{check.reason}</p></TechnicalDetails>
          </>
        )
        : <small>{check.reason}</small>)}
      {check.proof_expires_at && (
        <small>
          Confirmed {when(check.proof_observed_at)}; valid until{" "}
          {when(check.proof_expires_at)}.
        </small>
      )}
    </li>
  );
}

/** A provider confirmation grants no project or task admission. */
export function ProviderSettings(
  { validation, knownModels = {}, onChanged }: ProviderSettingsProps,
) {
  const nextKey = useRef(2);
  const [rows, setRows] = useState<SelectionRow[]>(() =>
    (["codex", "claude"] as const).map((provider, index) => {
      const proof = validation?.proofs.filter((item) =>
        item.provider === provider
      ).sort((left, right) => right.observed_at.localeCompare(left.observed_at))[0];
      return {
        key: index,
        include: !!proof,
        provider,
        model: proof?.model ?? "",
        effort: proof?.effort ?? "medium",
      };
    })
  );
  const selections: ProviderValidationSelection[] = rows.filter((row) =>
    row.include && row.model.trim()
  ).map(({ provider, model, effort }) => ({
    provider,
    model: model.trim(),
    effort,
  }));
  const selectionKey = JSON.stringify(selections);
  const [preview, setPreview] = useState<
    { selectionKey: string; result: ProviderValidationPreview } | undefined
  >(undefined);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState("");
  const previewRequest = useRef(0);
  const [startBusy, setStartBusy] = useState(false);
  const [startError, setStartError] = useState("");
  const [startNotice, setStartNotice] = useState("");
  const [pendingStart, setPendingStart] = useState(readPendingStart);
  const [recoveredId, setRecoveredId] = useState<string>();
  const [inspected, setInspected] = useState<
    Record<string, ProviderValidationBatch>
  >({});
  const [batchBusy, setBatchBusy] = useState<Record<string, boolean>>({});
  const [batchErrors, setBatchErrors] = useState<Record<string, string>>({});

  const currentPreview = preview?.selectionKey === selectionKey
    ? preview.result
    : undefined;
  const resolvedStart = pendingStart &&
    validation?.batches.find((batch) => batch.operation_id === pendingStart.id);
  // Only the service's own batch list settles an unknown start; a batch
  // missing from it proves nothing.
  useEffect(() => {
    if (!resolvedStart) return;
    localStorage.removeItem(START_STORAGE_KEY);
    setPendingStart(undefined);
    setPreview(undefined);
    setStartError("");
  }, [resolvedStart]);

  const updateRow = (key: number, change: Partial<SelectionRow>) =>
    setRows((current) =>
      current.map((row) => row.key === key ? { ...row, ...change } : row)
    );
  const requestPreview = async () => {
    const request = ++previewRequest.current;
    const requestedKey = selectionKey;
    setPreviewBusy(true);
    setPreviewError("");
    try {
      const result = await previewProviderValidation(selections);
      if (request !== previewRequest.current) return;
      setPreview({ selectionKey: requestedKey, result });
    } catch (cause) {
      if (request !== previewRequest.current) return;
      setPreviewError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (request === previewRequest.current) setPreviewBusy(false);
    }
  };
  const storedPreview = (stored: PendingStart): ProviderValidationPreview => ({
    preview_hash: stored.preview_hash,
    installations: [],
    checks: [],
    model_calls_required: stored.max_model_calls,
  });
  const storedApproval = (stored: PendingStart) => ({
    authorizeModelCalls: stored.authorize_model_calls,
    maxModelCalls: stored.max_model_calls,
  });
  // Settles the stored request without running anything. Only a typed reply
  // for this exact ID clears it, and only while it is still the stored one;
  // any failure keeps the identity for another attempt.
  const reconcile = async (stored: PendingStart) => {
    setStartBusy(true);
    setStartError("");
    setStartNotice("");
    try {
      const outcome = await reconcileProviderValidationStart(
        storedPreview(stored),
        stored.selections,
        storedApproval(stored),
        stored.id,
      );
      if (readPendingStart()?.id === stored.id) {
        localStorage.removeItem(START_STORAGE_KEY);
        setPreview(undefined);
      }
      setPendingStart((current) => current?.id === stored.id ? undefined : current);
      if (outcome.outcome === "started") {
        const batch = outcome.batch;
        setInspected((current) => ({ ...current, [batch.id]: batch }));
        setRecoveredId(batch.id);
        setStartNotice(
          "The earlier request had started. Its checks are shown below; nothing was run again.",
        );
      } else {
        setStartNotice(
          "The earlier request never started, and LLMRelay closed it so it never will. No model was contacted. Coding access is authorized and verified within a project task.",
        );
      }
      onChanged();
    } catch (cause) {
      setStartError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setStartBusy(false);
    }
  };
  const batchAction = async (
    batchId: string,
    action: (id: string) => Promise<ProviderValidationBatch>,
  ) => {
    setBatchBusy((current) => ({ ...current, [batchId]: true }));
    setBatchErrors((current) => ({ ...current, [batchId]: "" }));
    try {
      const batch = await action(batchId);
      setInspected((current) => {
        const known = current[batch.id];
        return known && known.updated_at > batch.updated_at
          ? current
          : { ...current, [batch.id]: batch };
      });
      onChanged();
    } catch (cause) {
      setBatchErrors((current) => ({
        ...current,
        [batchId]: cause instanceof Error ? cause.message : String(cause),
      }));
    } finally {
      setBatchBusy((current) => ({ ...current, [batchId]: false }));
    }
  };

  // The newer of the shared state and this page's own inspection wins.
  const batches = [
    ...(validation?.batches ?? []).map((batch) => {
      const local = inspected[batch.id];
      return local && local.updated_at > batch.updated_at ? local : batch;
    }),
    ...Object.values(inspected).filter((batch) =>
      !validation?.batches.some((item) => item.id === batch.id)
    ),
  ].sort((left, right) => right.created_at.localeCompare(left.created_at))
    .slice(0, 5);
  // A recovered request may be older than the recent checks; it stays shown.
  const recovered = recoveredId ? inspected[recoveredId] : undefined;
  const shownBatches = recovered &&
      !batches.some((batch) => batch.id === recovered.id)
    ? [...batches, recovered]
    : batches;
  return (
    <section className="panel provider-settings" aria-labelledby="providers-title">
      <header>
        <h2 id="providers-title">Providers</h2>
      </header>
      <p className="hint">
        Inspect installed providers, versions and sign-in, and refresh local model
        catalogs. Inspection starts no agent session and does not confirm model
        access or project permissions. Authorize and verify coding access from a
        task's Overview after configuring its Roles. Provider settings and hooks
        are never approved automatically here.
      </p>
      {!validation && (
        <p className="warning">
          This LLMRelay service does not report local provider inspection and history. Update the
          service to use this page.
        </p>
      )}
      <fieldset className="provider-selection" disabled={startBusy}>
        <legend>Local inspection</legend>
        {rows.map((row) => (
          <div className="provider-row" key={row.key}>
            <label className="toggle">
              <input
                type="checkbox"
                checked={row.include}
                onChange={(event) =>
                  updateRow(row.key, { include: event.target.checked })}
              />
              Include
            </label>
            <label>
              Provider
              <select
                value={row.provider}
                onChange={(event) =>
                  updateRow(row.key, {
                    provider: event.target.value === "claude"
                      ? "claude"
                      : "codex",
                  })}
              >
                <option value="codex">Codex</option>
                <option value="claude">Claude</option>
              </select>
            </label>
            <ModelSelector
              provider={row.provider}
              value={row.model}
              onChange={(model) => updateRow(row.key, { model })}
              label={`${providerName(row.provider)} model catalog entry`}
              knownExactModels={knownModels[row.provider] ?? []}
            />
            <label>
              Effort
              <select
                value={row.effort}
                onChange={(event) =>
                  updateRow(row.key, { effort: event.target.value })}
              >
                {!efforts.includes(row.effort) && (
                  <option value={row.effort}>{row.effort}</option>
                )}
                {efforts.map((effort) => (
                  <option key={effort} value={effort}>{effort}</option>
                ))}
              </select>
            </label>
            <button
              type="button"
              onClick={() =>
                setRows((current) => current.filter((item) => item.key !== row.key))}
            >
              Remove
            </button>
          </div>
        ))}
        <div className="button-row">
          <button
            type="button"
            onClick={() => {
              const key = nextKey.current++;
              setRows((current) => [...current, {
                key,
                include: true,
                provider: "codex",
                model: "",
                effort: "medium",
              }]);
            }}
          >
            Add a model
          </button>
          <button
            type="button"
            className="primary"
            disabled={!validation || !selections.length || previewBusy}
            onClick={() => void requestPreview()}
          >
            {previewBusy ? "Checking installations…" : "Inspect providers"}
          </button>
        </div>
        <small>
          Inspection uses local installation and account facts. It starts no
          coding session and makes no model request.
        </small>
      </fieldset>
      {previewError && <ErrorNotice error={previewError} />}
      {preview && !currentPreview && (
        <p className="hint">
          Your selection changed after the last inspection. Choose Inspect providers
          again to see current local facts.
        </p>
      )}
      {currentPreview && (
        <section className="provider-preview" aria-label="Local provider inspection">
          <h3>Local facts</h3>
          {currentPreview.installations.map((installation) => (
            <InstallationFacts key={installation.provider} installation={installation} />
          ))}
          <ul className="provider-plan">
            {currentPreview.checks.map((check) => (
              <li key={check.ordinal}>
                <strong>{providerName(check.provider)} · {check.model} · {check.effort}</strong>
                <small>{check.model_listed === true
                  ? "Listed in the local model catalog; account access is not tested."
                  : check.model_listed === false
                  ? "Not listed locally. An exact model ID can still be configured; access is not tested."
                  : "Local catalog availability is unknown; access is not tested."}</small>
                <TechnicalDetails><p>{check.reason}</p></TechnicalDetails>
              </li>
            ))}
          </ul>
          <p className="hint">
            No new app-wide coding checks are available. Continue configuring providers,
            then open a project task to select its workspace, choose Make Ready and verify
            its scoped access. Local facts cannot replace that native permission evidence.
          </p>
          <TechnicalDetails><p>Inspection {currentPreview.preview_hash}</p></TechnicalDetails>
        </section>
      )}
      {pendingStart && !resolvedStart && !startBusy && (
        <div className="warning" role="status">
          <p>
            An earlier check request has an unknown outcome. Check or close it:
            LLMRelay shows its historical batch if recorded, or closes it if it never
            arrived. This starts no session and makes no model request.
          </p>
          <button type="button" onClick={() => void reconcile(pendingStart)}>
            Check or close the earlier request
          </button>
        </div>
      )}
      {startError && <ErrorNotice error={startError} />}
      {startNotice && <p className="hint" role="status">{startNotice}</p>}
      <section className="provider-batches" aria-label="Recent provider checks">
        <h3>Historical checks</h3>
        {!shownBatches.length && (
          <p className="empty">No provider checks have run.</p>
        )}
        {shownBatches.map((batch) => (
          <article className="provider-batch" key={batch.id}>
            <header>
              <strong>
                Started {when(batch.created_at)} ·{" "}
                {batch.state === "running"
                  ? "Running"
                  : batch.state === "cancel_requested"
                  ? "Stopping"
                  : batch.state === "cancelled"
                  ? "Cancelled"
                  : "Finished"}
              </strong>
              <small>
                {batch.model_calls_authorized
                  ? `Up to ${batch.max_model_calls} new coding session${
                    batch.max_model_calls === 1 ? "" : "s"
                  } allowed`
                  : "No new coding sessions allowed"}
              </small>
            </header>
            <ul>
              {[...batch.checks].sort((left, right) =>
                left.ordinal - right.ordinal
              ).map((check) => <CheckRow key={check.id} check={check} />)}
            </ul>
            <div className="button-row">
              <button
                type="button"
                disabled={batchBusy[batch.id]}
                onClick={() =>
                  void batchAction(batch.id, inspectProviderValidation)}
              >
                Refresh this check
              </button>
              {batch.state === "running" && (
                <button
                  type="button"
                  className="danger"
                  disabled={batchBusy[batch.id]}
                  onClick={() =>
                    void batchAction(batch.id, cancelProviderValidation)}
                >
                  Cancel remaining checks
                </button>
              )}
            </div>
            {batch.state === "cancel_requested" && (
              <small>
                Cancellation is requested. A session already running stays
                uncertain until LLMRelay confirms exit. No replacement session
                is started automatically.
              </small>
            )}
            {batchErrors[batch.id] && <ErrorNotice error={batchErrors[batch.id]} />}
            <TechnicalDetails>
              <p>Batch {batch.id} · request {batch.operation_id}</p>
            </TechnicalDetails>
          </article>
        ))}
      </section>
      {validation && validation.proofs.length > 0 && (
        <section className="provider-proofs" aria-label="Confirmed model access">
          <h3>Historical model access</h3>
          <ul>
            {validation.proofs.map((proof) => (
              <li
                key={`${proof.provider}:${proof.model}:${proof.effort}:${proof.observed_at}`}
              >
                <strong>
                  {providerName(proof.provider)} · {proof.model} · {proof.effort}
                </strong>
                <span>
                  {proof.expired
                    ? "Expired historical confirmation"
                    : `Valid until ${when(proof.expires_at)}`}
                </span>
                <small>
                  Confirmed {when(proof.observed_at)}
                  {proof.executable_version
                    ? ` with version ${proof.executable_version}`
                    : ""}
                  {proof.account_identity
                    ? ` for account ${proof.account_identity}`
                    : "; account identity unavailable, so it is not reused across projects"}
                  .
                </small>
              </li>
            ))}
          </ul>
          <p className="hint">
            These historical confirmations show account access when checked.
            They do not authorize a project checkout or qualify a current task's
            native permissions.
          </p>
        </section>
      )}
    </section>
  );
}
