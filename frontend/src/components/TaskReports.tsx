import { useEffect, useState } from "react";
import { SaveKnowledgeSource } from "./Knowledge";
import { ErrorNotice, TechnicalDetails } from "./ErrorNotice";
import { MarkdownContent } from "./MarkdownContent";
import {
  type CurrentInputSource,
  type CurrentInputUnavailableReason,
  roleLabel,
  type TaskContent,
} from "../types";

export const outcomeLabel = (outcome?: string) => {
  switch (outcome) {
    case "plan_ready":
      return "Plan";
    case "approved":
      return "Approved";
    case "request_changes":
      return "Changes requested";
    case "needs_rework":
      return "Needs a different approach";
    case "candidate_ready":
      return "Work finished";
    case "needs_input":
      return "Needs your input";
    case "blocked":
      return "Blocked";
    case "evidence_ready":
      return "Findings";
    case "handoff_ready":
      return "Ready for final verification";
    default:
      return outcome ? outcome.replaceAll("_", " ") : "Report";
  }
};

export function ReportList(
  { content, error, started, projectId, taskId, attemptId }: {
    projectId?: string; taskId?: string; attemptId?: string;
    content?: TaskContent;
    error: string;
    /** Whether the task has an attempt; without one nothing is fetched. */
    started: boolean;
  },
) {
  if (!started) {
    return (
      <p className="empty">
        No agent has worked on this task yet, so there are no reports. Plans,
        reviews and results appear here once it starts.
      </p>
    );
  }
  if (error) return <ErrorNotice error={error} />;
  if (!content) return <p className="hint">Loading reports…</p>;
  const records = [...content.records].reverse();
  if (!records.length) {
    return (
      <p className="empty">
        No reports yet. Agents' plans, reviews and results appear here.
      </p>
    );
  }
  return (
    <div className="report-list">
      {content.truncated && (
        <p className="hint">Only the most recent reports are shown.</p>
      )}
      {records.map((record) => (
        <article className="report" key={record.id}>
          <header>
            <strong>
              {record.kind === "rework_request"
                ? "Your rework request"
                : `${record.role ? roleLabel(record.role) : "Agent"} · ${
                  outcomeLabel(record.outcome)
                }`}
            </strong>
            <time dateTime={record.created_at}>
              {new Date(record.created_at).toLocaleString()}
            </time>
          </header>
          {record.superseded_by_native_turn && (
            <small className="hint">
              Superseded: the agent accepted a later turn on{" "}
              {new Date(record.superseded_by_native_turn.superseded_at)
                .toLocaleString()}, so this report no longer holds the task.
            </small>
          )}
          <MarkdownContent text={record.summary} />
          {projectId && taskId && attemptId && content.task_id === taskId && content.attempt_id === attemptId && record.kind === "report" && <SaveKnowledgeSource projectId={projectId} source={{ kind: "historical", source: { task_id: taskId, attempt_id: attemptId, kind: "report", id: record.id } }} title={record.summary.slice(0, 100)} />}
          {record.plan && record.plan !== record.summary && (
            <details>
              <summary>Plan</summary>
              <MarkdownContent text={record.plan} />
            </details>
          )}
        </article>
      ))}
    </div>
  );
}

const unavailableInput: Record<CurrentInputUnavailableReason, string> = {
  not_recorded:
    "This request was recorded before LLMRelay linked requests to the report that raised them, so its exact question cannot be shown.",
  missing:
    "The report that raised this request could not be found, so its question cannot be shown.",
  mismatched:
    "The report recorded for this request does not match it, so it is not shown.",
  superseded:
    "The agent continued working after it raised this request, so that report no longer applies.",
  generation_replaced:
    "The agent session that raised this request has since been replaced, so its report no longer applies.",
};

export const inputAuthor = (source: CurrentInputSource) =>
  source.role === null
    ? "An agent"
    : source.role === "single_agent"
    ? "The agent"
    : `The ${roleLabel(source.role).toLowerCase()}`;

/**
 * The exact report behind the current attempt's input hold. When it is
 * unavailable, nothing stands in for it: the person is told why and can
 * refresh or read every report in Activity.
 */
export function CurrentInputReport(
  { content, error, titleId, onRefresh, onOpenReports }: {
    content?: TaskContent;
    error: string;
    titleId: string;
    onRefresh: () => void;
    onOpenReports: () => void;
  },
) {
  const source = content?.current_input;
  const unavailable = (title: string, message: string) => (
    <>
      <h3 id={titleId}>{title}</h3>
      <p>{message}</p>
      <p className="hint">
        No other report is shown in its place. Refresh to check the current
        state, or read every report for this attempt in Activity.
      </p>
      <div className="button-row">
        <button type="button" onClick={onRefresh}>Refresh</button>
        <button type="button" onClick={onOpenReports}>Open Activity</button>
      </div>
      {source?.state === "unavailable" && (
        <TechnicalDetails>
          <p>
            {source.kind} · {source.unavailable_reason} · report{" "}
            {source.result_id ?? "not recorded"} · generation{" "}
            {source.role_generation_id ?? "not recorded"} · attempt{" "}
            {source.attempt_id}
          </p>
        </TechnicalDetails>
      )}
    </>
  );
  if (error) {
    return (
      <>
        <h3 id={titleId}>The agent's request could not be loaded</h3>
        <ErrorNotice error={error} />
        <div className="button-row">
          <button type="button" onClick={onRefresh}>Refresh</button>
        </div>
      </>
    );
  }
  if (!content) {
    return (
      <>
        <h3 id={titleId}>Waiting for your input</h3>
        <p className="hint">Loading the agent's report…</p>
      </>
    );
  }
  if (source === undefined) {
    return unavailable(
      "The agent's question is not available",
      "This LLMRelay service does not identify which report raised this request, so no question is shown here.",
    );
  }
  if (source === null) {
    return unavailable(
      "No question is linked to this wait",
      "LLMRelay has no agent question linked to this wait. Why it is waiting and the available actions show what the task needs.",
    );
  }
  if (source.state === "unavailable") {
    return unavailable(
      "The agent's question is not available",
      unavailableInput[source.unavailable_reason],
    );
  }
  const author = inputAuthor(source);
  return (
    <>
      <h3 id={titleId}>
        {source.kind === "question"
          ? `${author} needs a decision`
          : `${author} is blocked`}
      </h3>
      <article className="report current-input">
        <header>
          <small className="hint">
            {source.kind === "question"
              ? "Its question, in its own words:"
              : "What is blocking it, in its own words:"}
          </small>
          <time dateTime={source.report.created_at}>
            {new Date(source.report.created_at).toLocaleString()}
          </time>
        </header>
        <MarkdownContent text={source.report.summary} />
      </article>
      <TechnicalDetails>
        <p>
          Report {source.report.id} · generation {source.role_generation_id}
          {" "}
          · attempt {source.attempt_id}
        </p>
      </TechnicalDetails>
    </>
  );
}

export function FindingKnowledgeSource({ projectId, parent, field, index, text, title }: {
  projectId: string;
  parent: Extract<import("../types").KnowledgeSourceSelector, { kind: "finding" }>["parent"];
  field: "summary" | "evidence" | "findings";
  index: number | null;
  text: string;
  title: string;
}) {
  const [hash, setHash] = useState(""); const [error, setError] = useState("");
  useEffect(() => {
    let abandoned = false; setHash(""); setError("");
    if (new TextEncoder().encode(text).length > 65536) { setError("This complete finding exceeds 64 KiB. Select a smaller source; its preview will not be hashed as complete evidence."); return; }
    void crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)).then((digest) => { if (!abandoned) setHash(Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("")); }).catch((cause: unknown) => { if (!abandoned) setError(String(cause)); });
    return () => { abandoned = true; };
  }, [text]);
  return <>{hash && <SaveKnowledgeSource key={`${JSON.stringify(parent)}:${field}:${index}:${hash}`} projectId={projectId} source={{ kind: "finding", parent, field, index, content_hash: hash }} title={title} statement={new TextEncoder().encode(text).length <= 8192 ? text : ""} />}{error && <ErrorNotice error={error} />}</>;
}


export function knowledgeFindingText(value: unknown): string | undefined {
  if (typeof value === "string") return value;
  function encode(item: unknown): string | undefined {
    if (item === null || typeof item === "boolean" || typeof item === "string" || (typeof item === "number" && Number.isFinite(item))) return JSON.stringify(item);
    if (Array.isArray(item)) { const values = item.map(encode); return values.every((value) => value !== undefined) ? `[${values.join(",")}]` : undefined; }
    if (typeof item === "object" && item !== null) {
      const values = Object.entries(item).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0).map(([key, value]) => { const encoded = encode(value); return encoded === undefined ? undefined : `${JSON.stringify(key)}:${encoded}`; });
      return values.every((value) => value !== undefined) ? `{${values.join(",")}}` : undefined;
    }
    return undefined;
  }
  return value === null ? undefined : encode(value);
}
