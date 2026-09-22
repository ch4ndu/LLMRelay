import { useEffect, useMemo, useRef, useState } from "react";
import { getModelCatalog } from "../api";
import type { ModelCatalog, Provider } from "../types";

type ModelSelectorProps = {
  provider: Provider | "";
  value: string;
  onChange: (value: string) => void;
  label?: string;
  knownExactModels?: string[];
  disabled?: boolean;
};

const uniqueExactModels = (models: string[]) =>
  [...new Set(models.map((model) => model.trim()).filter(Boolean))].sort(
    (left, right) => left.localeCompare(right),
  );

/**
 * An advisory picker, not an entitlement or launch control. The editable input
 * remains the canonical stored exact model string; selecting a catalog value is
 * always a deliberate user edit.
 */
export function ModelSelector(
  {
    provider,
    value,
    onChange,
    label = "Exact model",
    knownExactModels = [],
    disabled = false,
  }: ModelSelectorProps,
) {
  const providerRef = useRef(provider);
  const catalogRequestGenerationRef = useRef(0);
  providerRef.current = provider;
  const [catalogState, setCatalogState] = useState<{
    provider: Provider | "";
    catalog?: ModelCatalog;
    loading: boolean;
    error: string;
  }>({ provider, loading: false, error: "" });
  const [selectedSuggestion, setSelectedSuggestion] = useState("");
  useEffect(() => {
    // Invalidate a request even when a provider eventually cycles back to the
    // same value (Codex -> Claude -> Codex). Provider equality alone cannot
    // distinguish the older Codex response from the later refresh.
    catalogRequestGenerationRef.current += 1;
    setCatalogState({ provider, loading: false, error: "" });
    setSelectedSuggestion("");
  }, [provider]);
  const catalog = catalogState.provider === provider
    ? catalogState.catalog
    : undefined;
  const loading = catalogState.provider === provider && catalogState.loading;
  const catalogError = catalogState.provider === provider
    ? catalogState.error
    : "";
  const known = useMemo(() => uniqueExactModels(knownExactModels), [
    knownExactModels,
  ]);
  const models = catalog?.state === "available" ? catalog.models : [];
  const selectedCatalogModel = models.find((model) => model.slug === value.trim());
  const aliasMatch = models.find((model) =>
    model.display_name?.toLocaleLowerCase() === value.trim().toLocaleLowerCase() &&
    model.slug !== value.trim()
  );
  const refresh = async () => {
    if (!provider || loading) return;
    const requestedProvider = provider;
    const requestGeneration = ++catalogRequestGenerationRef.current;
    setCatalogState({ provider: requestedProvider, loading: true, error: "" });
    try {
      const next = await getModelCatalog(requestedProvider);
      if (
        providerRef.current === requestedProvider &&
        catalogRequestGenerationRef.current === requestGeneration
      ) {
        setCatalogState({
          provider: requestedProvider,
          catalog: next,
          loading: false,
          error: "",
        });
      }
    } catch (cause) {
      if (
        providerRef.current === requestedProvider &&
        catalogRequestGenerationRef.current === requestGeneration
      ) {
        setCatalogState({
          provider: requestedProvider,
          loading: false,
          error: cause instanceof Error ? cause.message : String(cause),
        });
      }
    }
  };
  const choose = (next: string) => {
    setSelectedSuggestion("");
    if (next) onChange(next);
  };

  return (
    <div className="model-selector">
      <label>
        {label}
        <input
          aria-label={label}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        />
      </label>
      <div className="model-selector-actions">
        <button
          type="button"
          disabled={disabled || !provider || loading}
          onClick={() => void refresh()}
        >
          {loading ? "Loading local suggestions…" : "Refresh local suggestions"}
        </button>
        <select
          aria-label={`${label} advisory suggestions`}
          disabled={disabled || (!known.length && !models.length)}
          value={selectedSuggestion}
          onChange={(event) => choose(event.target.value)}
        >
          <option value="">Choose an advisory exact model</option>
          {known.map((model) => (
            <option key={`known:${model}`} value={model}>
              Existing exact profile · {model}
            </option>
          ))}
          {models.filter((model) => !known.includes(model.slug)).map((model) => (
            <option key={`catalog:${model.slug}`} value={model.slug}>
              {model.display_name || model.slug} · {model.slug}
              {model.visibility ? ` · ${model.visibility}` : ""}
            </option>
          ))}
        </select>
      </div>
      {aliasMatch && (
        <small className="warning">
          The stored exact string matches the catalog display name for
          {" "}<code>{aliasMatch.slug}</code>. It remains unchanged; choose
          that suggested slug explicitly only if you intend to correct it.
        </small>
      )}
      {catalog?.state === "available" && (
        <small className="hint">
          Local Codex metadata fetched {catalog.fetched_at || "at an unknown time"}
          {catalog.stale ? " and is stale" : ""}. Suggestions are advisory;
          they do not establish account eligibility, capability evidence, or
          provider-launch authority.
        </small>
      )}
      {selectedCatalogModel?.efforts.length ? (
        <small className="hint">
          The advisory catalog lists reasoning efforts for this model:
          {" "}{selectedCatalogModel.efforts.join(", ")}. The stored effort is
          unchanged until you select it separately.
        </small>
      ) : null}
      {catalog?.state === "unavailable" && (
        <small className="hint">{catalog.reason}</small>
      )}
      {catalogError && <small className="error">{catalogError}</small>}
      {provider === "claude" && !catalog && (
        <small className="hint">
          Claude has no trusted local catalog here. Existing exact profiles and
          the manual exact entry remain available.
        </small>
      )}
    </div>
  );
}
