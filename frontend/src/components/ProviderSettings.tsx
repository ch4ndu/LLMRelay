import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  cancelProviderValidation,
  inspectProviderValidation,
  operationId,
  previewProviderValidation,
  reconcileProviderValidationStart,
  startProviderValidation,
} from "../api";
import type {
  ProviderCheck,
  ProviderCheckPreview,
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

const planLabels: Record<ProviderCheckPreview["plan"], string> = {
  reuse_proof: "Reuses a current confirmation; no model call",
  model_call: "Makes one model call",
  unavailable: "Cannot run",
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

const startBody = (start: Omit<PendingStart, "id">) =>
  JSON.stringify([
    start.preview_hash,
    start.selections.map(({ provider, model, effort }) => [provider, model, effort]),
    start.authorize_model_calls,
    start.max_model_calls,
  ]);

function InstallationFacts({ installation }: { installation: ProviderInstallation }) {
  return (
    <article className="provider-installation">
      <strong>
        {providerName(installation.provider)} ·{" "}
        {installation.ready ? "installed and usable" : "not ready"}
      </strong>
      <small>
        {installation.executable_version
          ? `Version ${installation.executable_version}`
          : "Version not found"}
        {" · "}
        {installation.account_identity
          ? `Account ${installation.account_identity}`
          : "Account identity unavailable, so results are not reused across projects"}
        {installation.authentication
          ? ` · Sign-in: ${installation.authentication}`
          : ""}
      </small>
      {installation.model_calls_disabled_reason && (
        <p className="hint">
          Model calls are turned off for this provider:{" "}
          {installation.model_calls_disabled_reason}
        </p>
      )}
      {installation.guidance && <p className="hint">{installation.guidance}</p>}
      <TechnicalDetails>
        <p>{installation.executable_path ?? "No executable path found"}</p>
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
          LLMRelay cannot tell whether this model call ran. It is not repeated
          automatically; check the provider before running it again.
        </small>
      )}
      {check.reason && <small>{check.reason}</small>}
      {check.proof_expires_at && (
        <small>
          Confirmed {when(check.proof_observed_at)}; valid until{" "}
          {when(check.proof_expires_at)}.
        </small>
      )}
    </li>
  );
}

/**
 * App-wide provider and model checks, outside any project. Opening this page
 * makes no request. Refreshing model suggestions reads only the local model
 * catalog, a preview inspects the local installation only, and model calls
 * happen solely through an explicitly authorized start. A confirmation here
 * never grants a project permission to run agents.
 */
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
  const [authorize, setAuthorize] = useState(false);
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
    setAuthorize(false);
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
      setAuthorize(false);
    } catch (cause) {
      if (request !== previewRequest.current) return;
      setPreviewError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (request === previewRequest.current) setPreviewBusy(false);
    }
  };
  // A start whose outcome is unknown stays stored until the service returns
  // its batch. Only that exact request, under its original ID, is ever sent
  // again; the service then returns the recorded batch instead of a second one.
  // Both operations send only the preview's hash with the exact request.
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
  const send = async (stored: PendingStart, replay: boolean) => {
    localStorage.setItem(START_STORAGE_KEY, JSON.stringify(stored));
    setPendingStart(stored);
    setStartBusy(true);
    setStartError("");
    setStartNotice("");
    try {
      const batch = await startProviderValidation(
        storedPreview(stored),
        stored.selections,
        storedApproval(stored),
        stored.id,
      );
      localStorage.removeItem(START_STORAGE_KEY);
      setPendingStart(undefined);
      setInspected((current) => ({ ...current, [batch.id]: batch }));
      setPreview(undefined);
      setAuthorize(false);
      onChanged();
    } catch (cause) {
      // A refused replay cannot prove the first request never ran, because
      // the service may refuse before it looks the operation up.
      if (!replay && !(cause instanceof ApiError && cause.ambiguous)) {
        localStorage.removeItem(START_STORAGE_KEY);
        setPendingStart(undefined);
      }
      setStartError(cause instanceof Error ? cause.message : String(cause));
      onChanged();
    } finally {
      setStartBusy(false);
    }
  };
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
      // As after an ordinary start, a settled request leaves no preview or
      // call approval behind: the next start needs a new preview and approval.
      if (readPendingStart()?.id === stored.id) {
        localStorage.removeItem(START_STORAGE_KEY);
        setPreview(undefined);
        setAuthorize(false);
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
          "The earlier request never started, and LLMRelay closed it so it never will. No model was contacted. Preview the checks again and approve a new request to run them.",
        );
      }
      onChanged();
    } catch (cause) {
      setStartError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setStartBusy(false);
    }
  };
  const start = () => {
    if (!currentPreview || startBusy) return;
    const calls = currentPreview.model_calls_required;
    const request = {
      preview_hash: currentPreview.preview_hash,
      selections,
      authorize_model_calls: calls > 0 && authorize,
      max_model_calls: calls,
    };
    if (pendingStart && startBody(pendingStart) !== startBody(request)) return;
    void send({ ...request, id: pendingStart?.id ?? operationId() }, !!pendingStart);
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
  const otherStartUnknown = !!pendingStart && !!currentPreview &&
    startBody(pendingStart) !==
      startBody({
        preview_hash: currentPreview.preview_hash,
        selections,
        authorize_model_calls: currentPreview.model_calls_required > 0 &&
          authorize,
        max_model_calls: currentPreview.model_calls_required,
      });
  const callsNeeded = currentPreview?.model_calls_required ?? 0;

  return (
    <section className="panel provider-settings" aria-labelledby="providers-title">
      <header>
        <h2 id="providers-title">Providers</h2>
      </header>
      <p className="hint">
        Check that each selected provider is installed and that your account
        can use the chosen model. Results apply to the whole app, but they never
        give a project permission to run agents: each project still needs its
        own setup, hooks, approvals and verified agent sessions. LLMRelay does
        not change any provider's own settings or hooks.
      </p>
      {!validation && (
        <p className="warning">
          This LLMRelay service does not report provider checks. Update the
          service to use this page.
        </p>
      )}
      <fieldset className="provider-selection" disabled={startBusy}>
        <legend>What to check</legend>
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
              label={`${providerName(row.provider)} model to check`}
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
            {previewBusy ? "Checking installations…" : "Preview checks"}
          </button>
        </div>
        <small>
          Preview only inspects what is installed on this computer. It makes no
          model call.
        </small>
      </fieldset>
      {previewError && <ErrorNotice error={previewError} />}
      {preview && !currentPreview && (
        <p className="hint">
          Your selection changed after the last preview. Choose Preview checks
          again before starting.
        </p>
      )}
      {currentPreview && (
        <section className="provider-preview" aria-label="Provider check preview">
          <h3>Before you start</h3>
          {currentPreview.installations.map((installation) => (
            <InstallationFacts
              key={installation.provider}
              installation={installation}
            />
          ))}
          <ul className="provider-plan">
            {currentPreview.checks.map((check) => (
              <li key={check.ordinal}>
                <strong>
                  {providerName(check.provider)} · {check.model} · {check.effort}
                </strong>
                <span>{planLabels[check.plan]}</span>
                <small>{check.reason}</small>
                {check.plan === "reuse_proof" && check.proof_expires_at && (
                  <small>
                    Confirmed {when(check.proof_observed_at)}; valid until{" "}
                    {when(check.proof_expires_at)}.
                  </small>
                )}
                {check.model_listed !== null && (
                  <small>
                    {check.model_listed
                      ? "The provider lists this model. A listing alone does not prove your account can use it."
                      : "The provider does not list this model locally. Only a model call can show whether your account can use it."}
                  </small>
                )}
              </li>
            ))}
          </ul>
          {callsNeeded > 0
            ? (
              <label className="toggle">
                <input
                  type="checkbox"
                  checked={authorize}
                  disabled={startBusy}
                  onChange={(event) => setAuthorize(event.target.checked)}
                />
                Allow up to {callsNeeded} new model call
                {callsNeeded === 1 ? "" : "s"} for these checks. Each call is a
                short request with no project files and no tools. Calls may use
                paid quota and are never retried automatically.
              </label>
            )
            : <p className="hint">These checks need no new model call.</p>}
          {otherStartUnknown && (
            <p className="warning">
              An earlier start with different choices has an unknown outcome.
              Check for its saved result before starting other checks.
            </p>
          )}
          <button
            type="button"
            className="primary"
            disabled={startBusy || otherStartUnknown ||
              (callsNeeded > 0 && !authorize)}
            onClick={start}
          >
            {startBusy ? "Starting…" : "Start these checks"}
          </button>
          {callsNeeded > 0 && !authorize && !otherStartUnknown && (
            <small className="hint">
              Start these checks becomes available after you allow the model
              calls above.
            </small>
          )}
        </section>
      )}
      {pendingStart && !resolvedStart && !startBusy && (
        <div className="warning" role="status">
          <p>
            LLMRelay could not confirm whether the last start reached it, so
            other checks cannot start until its outcome is known. Choose Check
            or close the earlier request: LLMRelay shows its checks if it was
            recorded, or, if it never arrived, closes it so it can never start.
            This makes no model call. Or send the identical request again: it
            reuses the original request, so it is never started twice.
          </p>
          <div className="button-row">
            <button type="button" onClick={() => void reconcile(pendingStart)}>
              Check or close the earlier request
            </button>
            <button
              type="button"
              onClick={() => void send(pendingStart, true)}
            >
              Send the identical request again
            </button>
          </div>
        </div>
      )}
      {startError && <ErrorNotice error={startError} />}
      {startNotice && <p className="hint" role="status">{startNotice}</p>}
      <section className="provider-batches" aria-label="Recent provider checks">
        <h3>Recent checks</h3>
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
                  ? `Up to ${batch.max_model_calls} model call${
                    batch.max_model_calls === 1 ? "" : "s"
                  } allowed`
                  : "No model calls allowed"}
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
                Cancellation is requested. A call already running stays
                uncertain until LLMRelay confirms its process has stopped.
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
          <h3>Confirmed model access</h3>
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
                    ? "Expired; check again to reuse it"
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
            A confirmation shows only that this account could call this model
            when checked. A change of provider version, account or settings
            makes it out of date.
          </p>
        </section>
      )}
    </section>
  );
}
