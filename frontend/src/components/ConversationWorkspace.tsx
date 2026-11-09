import { useEffect, useRef, useState } from "react";
import {
  conversationMutate, conversationMutationIsCurrent,
  conversationRead, operationId, prepareConversationMutation,
} from "../api";
import type {
  ConversationAttentionTarget, ConversationPromotion, ConversationAdmission, ConversationDetail, ConversationEvent, ConversationMessages,
  ConversationMutation, ConversationMutationResult, ConversationPage, ConversationProfile, ConversationReceipt, ConversationSummary,
  ConversationWorkspaceIdentity, ProjectExecution,
} from "../types";
import { withStorageLock } from "../storageCoordination";
import { readPreference, savePreference } from "../storagePreferences";
import { ErrorNotice } from "./ErrorNotice";

interface ConversationWorkspaceProps {
  projectId: string;
  revision: string;
  execution?: ProjectExecution;
  onOpenSetup: (projectId: string) => void;
  onOpenProviders: () => void;
  onPromote: (promotion: ConversationPromotion) => void;
  target?: ConversationAttentionTarget;
  targetCurrent?: boolean;
  onLeaveTarget?: () => void;
  sourceConversationId?: string;
}
type JournalState = "published" | "unknown" | "refused" | "succeeded";
interface Journal {
  id: string;
  body: string;
  hash: string;
  conversation: string | null;
  status: JournalState;
  request: ConversationMutation;
  receipt?: ConversationReceipt;
}
const MAX_INPUT = 12 * 1024;
const bytes = (text: string) => new TextEncoder().encode(text).length;
const errorText = (error: unknown) => error instanceof Error ? error.message : "The conversation could not be updated. Refresh and check its outcome.";
const mutationId = (request: ConversationMutation) => request.action === "create_profile" ? request.input.operation_id : request.operation_id;
const record = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);

// Rust serde_json::Value orders object keys by UTF-8 bytes. These requests contain
// string values and ASCII field names only; default JS key sorting is identical.
function canonical(value: unknown): string {
  if (typeof value === "string" || value === null || typeof value === "boolean") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (record(value)) return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  throw new Error("The retained request contains an unsupported value. Keep it for inspection.");
}
async function requestHash(request: ConversationMutation): Promise<string> {
  const input = request.action === "create_profile" ? request.input : request;
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical(input)));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
function readJournal(key: string): Journal | undefined {
  const text = localStorage.getItem(key);
  if (text === null) return;
  const value: unknown = JSON.parse(text);
  if (!record(value) || typeof value.id !== "string" || typeof value.body !== "string" ||
    typeof value.hash !== "string" || !/^[a-f0-9]{64}$/.test(value.hash) ||
    !(value.conversation === null || typeof value.conversation === "string") ||
    !["published", "unknown", "refused", "succeeded"].includes(String(value.status))) {
    throw new Error("The saved conversation request cannot be read. Keep browser storage and report Technical details; sending is blocked.");
  }
  const request: unknown = JSON.parse(value.body);
  if (!conversationMutationIsCurrent(request) || mutationId(request) !== value.id) {
    throw new Error("The saved request identity is invalid. Sending is blocked; retain browser storage for inspection.");
  }
  // Browser status is only a display hint. Release always fetches fresh receipt
  // evidence; a saved refusal cannot prove nondelivery after a reload.
  return { id: value.id, body: value.body, hash: value.hash, conversation: value.conversation,
    status: value.status === "refused" ? "refused" : value.status === "succeeded" ? "succeeded" : value.status === "published" ? "published" : "unknown", request };
}
const sameRequest = (left: Journal | undefined, right: Journal) => left?.id === right.id && left.body === right.body && left.hash === right.hash;
const settledReceipt = (receipt: ConversationReceipt, request: ConversationMutation) =>
  receipt.found && (receipt.delivery_state === "completed" ||
    (["start", "submit", "steer", "probe"].includes(request.action) && receipt.delivery_state === "bound"));

export function ConversationWorkspace(props: ConversationWorkspaceProps) {
  const [identity, setIdentity] = useState<ConversationWorkspaceIdentity>();
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    let abandoned = false;
    setIdentity(undefined); setError("");
    void conversationRead({ action: "workspace_identity", project_id: props.projectId }).then((result) => {
      if (abandoned) return;
      if (result.project_id !== props.projectId) throw new Error("The service returned another project's workspace identity. Reload before continuing.");
      setIdentity(result);
    }).catch((cause: unknown) => { if (!abandoned) setError(errorText(cause)); });
    return () => { abandoned = true; };
  }, [props.projectId, retry]);
  if (!identity || identity.project_id !== props.projectId) return <section className="panel" data-attention-target={props.target ? `conversation:${props.target.conversation_id}` : undefined} tabIndex={-1}><h1>Conversations</h1>
    {error ? <><ErrorNotice error={error} /><button onClick={() => setRetry((value) => value + 1)}>Retry workspace identity</button></> : <p role="status">Confirming this project's saved request storage…</p>}
  </section>;
  return <ProjectConversationWorkspace key={`${identity.storage_scope}:${identity.project_id}`} {...props} storageScope={identity.storage_scope} />;
}

function ProjectConversationWorkspace({ projectId, storageScope, revision, execution, onOpenSetup, onOpenProviders, onPromote, target, targetCurrent = true, onLeaveTarget, sourceConversationId }: ConversationWorkspaceProps & { storageScope: string }) {
  const scope = `${encodeURIComponent(storageScope)}.${encodeURIComponent(projectId)}`;
  const preference = `agenticjira.conversation.${scope}`;
  // One project-wide slot also fences unknown Starts and setup actions which do
  // not yet have a conversation ID. The exact conversation remains in its body.
  const journalKey = `llmrelay.conversation.request.${scope}`;
  const [selected, setSelected] = useState(() => target?.conversation_id || sourceConversationId || readPreference(preference) || "");
  const [sourceSelection, setSourceSelection] = useState<Record<string, string>>({});
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [profiles, setProfiles] = useState<ConversationProfile[]>([]);
  const [admissions, setAdmissions] = useState<ConversationAdmission[]>([]);
  const [cursors, setCursors] = useState<{ list: string | null; profiles: string | null; admissions: string | null }>({ list: null, profiles: null, admissions: null });
  const [detail, setDetail] = useState<ConversationDetail>();
  const [messages, setMessages] = useState<ConversationMessages>();
  const [journal, setJournal] = useState<Journal>();
  const [recoveryJournal, setRecoveryJournal] = useState<Journal>();
  const [retained, setRetained] = useState<Journal[]>([]);
  const retainedPrefix = `${journalKey}.uncertainty.`;
  const [journalError, setJournalError] = useState("");
  const [journalReady, setJournalReady] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [admissionId, setAdmissionId] = useState("");
  const [configId, setConfigId] = useState("");
  const [instructions, setInstructions] = useState("");
  const [input, setInput] = useState("");
  const mounted = useRef(true);
  const lifetime = useRef(new AbortController());
  const restoredInput = useRef(false);
  const currentSelection = useRef(selected);
  currentSelection.current = selected;
  const cache = useRef<{ id: string; events: ConversationEvent[]; after: number }>({ id: "", events: [], after: 0 });
  const loadedPages = useRef({ list: 1, profiles: 1, admissions: 1 });
  const slot = (request: ConversationMutation) => request.action === "reconcile" ? `${journalKey}.reconcile` : journalKey;
  const choose = (id: string) => { setSourceSelection({}); setMessages(undefined); setSelected(id); savePreference(preference, id || undefined); setError(""); setNotice(""); };
  useEffect(() => {
    const id = target?.conversation_id || sourceConversationId;
    if (id) choose(id);
  }, [target, sourceConversationId]);
  const requireNoLegacyJournal = () => {
    const prefix = "llmrelay.conversation.request.";
    const suffix = `.${encodeURIComponent(projectId)}`;
    for (let index = 0; index < localStorage.length; index += 1) {
      const key = localStorage.key(index);
      if (!key?.startsWith(prefix)) continue;
      const base = key.endsWith(".reconcile") ? key.slice(0, -".reconcile".length) : key;
      if (!base.endsWith(suffix)) continue;
      const oldScope = base.slice(prefix.length, -suffix.length);
      if (/^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i.test(oldScope)) {
        throw new Error(`A saved request from an earlier service session needs inspection before sending. Its storage ownership cannot be confirmed automatically. Keep browser storage and report Technical details. Retained key: ${key}`);
      }
    }
  };
  const syncJournal = () => {
    try {
      const saved = readJournal(journalKey);
      setJournal(saved); setRecoveryJournal(readJournal(`${journalKey}.reconcile`)); setJournalError("");
      requireNoLegacyJournal();
      const archived: Journal[] = [];
      for (let index = 0; index < localStorage.length; index += 1) {
        const key = localStorage.key(index);
        if (!key?.startsWith(retainedPrefix)) continue;
        const item = readJournal(key);
        if (!item || key !== `${retainedPrefix}${encodeURIComponent(item.id)}` || item.status !== "unknown") {
          throw new Error("A retained uncertain request cannot be read safely. Keep browser storage for inspection.");
        }
        archived.push(item);
      }
      setRetained(archived);
      if (!restoredInput.current) {
        restoredInput.current = true;
        if (saved && "input" in saved.request) {
          if (typeof saved.request.input === "string") setInput(saved.request.input);
          else { setInstructions(saved.request.input.instructions); setConfigId(saved.request.input.execution_config_revision_id); }
        }
        if (saved?.request.action === "start") setAdmissionId(saved.request.admission_id);
      }
    }
    catch (cause) { setJournalError(errorText(cause)); }
    finally { setJournalReady(true); }
  };
  useEffect(() => {
    mounted.current = true;
    lifetime.current = new AbortController();
    syncJournal();
    // Restart recovery only reads the exact retained request's receipt.
    for (const key of [journalKey, `${journalKey}.reconcile`]) {
      try {
        const saved = readJournal(key);
        if (saved) void checkOutcome(saved).catch((cause: unknown) => { if (mounted.current) setError(errorText(cause)); });
      } catch (cause) { setJournalError(errorText(cause)); }
    }
    const changed = (event: StorageEvent) => { if (event.key?.startsWith("llmrelay.conversation.request.") || event.key === null) syncJournal(); };
    window.addEventListener("storage", changed);
    return () => { mounted.current = false; lifetime.current.abort(); window.removeEventListener("storage", changed); };
  }, [journalKey]);

  useEffect(() => {
    let abandoned = false;
    void Promise.all([
      readDiscoveryPages((cursor) => conversationRead({ action: "list", project_id: projectId, cursor }), loadedPages.current.list, () => abandoned),
      readDiscoveryPages((cursor) => conversationRead({ action: "profiles", project_id: projectId, cursor }), loadedPages.current.profiles, () => abandoned),
      readDiscoveryPages((cursor) => conversationRead({ action: "admissions", project_id: projectId, cursor }), loadedPages.current.admissions, () => abandoned),
    ]).then(([list, saved, recorded]) => {
      if (abandoned) return;
      if (list.items.some((item) => item.project_id !== projectId) || saved.items.some((item) => item.project_id !== projectId)) throw new Error("The service returned a conversation from another project. Reload before continuing.");
      setConversations((previous) => merge(previous, list.items));
      setProfiles((previous) => merge(previous, saved.items));
      setAdmissions((previous) => merge(previous, recorded.items));
      setCursors({ list: list.next_cursor, profiles: saved.next_cursor, admissions: recorded.next_cursor });
    }).catch((cause: unknown) => { if (!abandoned) setError(errorText(cause)); });
    return () => { abandoned = true; };
  }, [projectId, revision, refresh]);

  useEffect(() => {
    let abandoned = false;
    if (!selected) { setDetail(undefined); setMessages(undefined); setLoading(false); cache.current = { id: "", events: [], after: 0 }; return; }
    setLoading(true);
    setDetail((previous) => previous?.conversation.id === selected ? previous : undefined);
    void (async () => {
      const assembled = await conversationRead({ action: "messages", project_id: projectId, conversation_id: selected });
      if (abandoned) return;
      if (assembled.messages.some((message) => message.source.conversation_id !== selected)) throw new Error("The recorded messages belong to another conversation. Sending is blocked until ownership is confirmed.");
      let saved = cache.current.id === selected ? cache.current : { id: selected, events: [], after: 0 };
      let latest: ConversationDetail | undefined;
      // Capture the Messages watermark once. 82 pages cover the backend's 4096
      // event cap; new events cannot keep this refresh running indefinitely.
      for (let page = 0; page < 82; page += 1) {
        latest = await conversationRead({ action: "detail", project_id: projectId, conversation_id: selected, after: saved.after });
        if (abandoned) return;
        if (latest.conversation.project_id !== projectId || latest.conversation.id !== selected) throw new Error("This conversation does not belong to the selected project. Choose a conversation explicitly.");
        saved = { id: selected, events: Array.from(new Map([...saved.events, ...latest.events].map((event) => [event.sequence, event])).values()).sort((a, b) => a.sequence - b.sequence), after: latest.next_after };
        if (!latest.has_more || saved.after >= assembled.through_sequence) break;
      }
      if (!abandoned && latest) {
        cache.current = saved;
        setDetail({ ...latest, events: saved.events }); setMessages(assembled);
        const latestAdmission = latest.admission;
        setAdmissions((previous) => merge(previous, [latestAdmission]));
      }
    })().catch((cause: unknown) => { if (!abandoned) { setDetail(undefined); setError(errorText(cause)); } })
      .finally(() => { if (!abandoned) setLoading(false); });
    return () => { abandoned = true; };
  }, [projectId, selected, revision, refresh]);

  async function more(kind: "list" | "profiles" | "admissions") {
    const cursor = cursors[kind];
    if (!cursor) return;
    setBusy(true);
    try {
      if (kind === "list") {
        const page = await conversationRead({ action: "list", project_id: projectId, cursor });
        if (mounted.current) { if (page.items.some((item) => item.project_id !== projectId)) throw new Error("Conversation project mismatch."); setConversations((previous) => merge(previous, page.items)); setCursors((previous) => ({ ...previous, list: page.next_cursor })); }
      } else if (kind === "profiles") {
        const page = await conversationRead({ action: "profiles", project_id: projectId, cursor });
        if (mounted.current) { if (page.items.some((item) => item.project_id !== projectId)) throw new Error("Profile project mismatch."); setProfiles((previous) => merge(previous, page.items)); setCursors((previous) => ({ ...previous, profiles: page.next_cursor })); }
      } else {
        const page = await conversationRead({ action: "admissions", project_id: projectId, cursor });
        if (mounted.current) { setAdmissions((previous) => merge(previous, page.items)); setCursors((previous) => ({ ...previous, admissions: page.next_cursor })); }
      }
      if (mounted.current) loadedPages.current[kind] += 1;
    } catch (cause) { if (mounted.current) setError(errorText(cause)); }
    finally { if (mounted.current) setBusy(false); }
  }

  async function updateJournal(original: Journal, update: (saved: Journal) => Journal | undefined) {
    await withStorageLock(journalKey, () => {
      const key = slot(original.request);
      const saved = readJournal(key);
      if (!sameRequest(saved, original) || !saved) throw new Error("Another window changed the saved request. Refresh and check its outcome.");
      const next = update(saved);
      if (next) localStorage.setItem(key, JSON.stringify(next));
      else localStorage.removeItem(key);
    });
    if (mounted.current) syncJournal();
  }
  async function receiptFor(original: Journal): Promise<ConversationReceipt> {
    if (await requestHash(original.request) !== original.hash) throw new Error("The saved request hash does not match. Retain the original request for inspection.");
    if (original.request.action === "create_profile" && original.request.input.project_id !== projectId) throw new Error("The retained profile belongs to another project.");
    const receipt = await conversationRead({ action: "receipt", project_id: projectId, operation_id: original.id, original_request: original.request, request_hash: original.hash });
    if (receipt.operation_id !== original.id || receipt.request_hash !== original.hash) throw new Error("The returned receipt does not match this request.");
    return receipt;
  }
  async function checkOutcome(original: Journal) {
    const receipt = await receiptFor(original);
    const success = settledReceipt(receipt, original.request);
    // Only a backend receipt with failed/refused state proves nondelivery.
    // Browser status and other failed/unknown states cannot release the fence.
    const definitiveRefusal = receipt.found && receipt.state === "refused" && receipt.delivery_state === "failed";
    await updateJournal(original, (saved) => ({ ...saved, receipt, status: success ? "succeeded" : definitiveRefusal ? "refused" : "unknown", conversation: receipt.conversation_id || saved.conversation }));
    if (mounted.current) {
      const update = (saved: Journal | undefined): Journal | undefined => saved && sameRequest(saved, original) ? { ...saved, receipt, status: success ? "succeeded" : definitiveRefusal ? "refused" : "unknown" } : saved;
      if (original.request.action === "reconcile") setRecoveryJournal(update);
      else setJournal(update);
      setNotice(success ? "The service recorded this request. Review the conversation, then continue when ready." : definitiveRefusal ? "The service refused this request before delivery. Your text is retained." : "The outcome is still unresolved. A missing receipt does not prove the request was never delivered. Keep this request and check again; no new message can replace it.");
      setRefresh((value) => value + 1);
    }
    return receipt;
  }
  async function deliver(original: Journal) {
    let result: ConversationMutationResult;
    try {
      result = await conversationMutate(original.request);
    } catch (cause) {
      // A backend error can occur after reservation. Its receipt takes precedence
      // over HTTP status; a network error never proves nondelivery.
      const receipt = await receiptFor(original).catch(() => undefined);
      if (receipt?.found) { await checkOutcome(original); throw cause; }
      await updateJournal(original, (saved) => ({ ...saved, status: "unknown" }));
      throw cause;
    }
    await updateJournal(original, (saved) => ({ ...saved, status: "unknown", conversation: result.conversation_id || saved.conversation }));
    const savedProfile = result.profile;
    if (mounted.current && savedProfile?.project_id === projectId) setProfiles((previous) => merge(previous, [savedProfile]));
    await checkOutcome(original);
  }
  async function send(request: ConversationMutation) {
    const selectionAtSend = selected;
    setBusy(true); setError(""); setNotice("");
    try {
      await prepareConversationMutation(request);
      const original: Journal = { id: mutationId(request), body: JSON.stringify(request), hash: await requestHash(request), conversation: "conversation_id" in request ? request.conversation_id : null, status: "published", request };
      if (!mounted.current) return;
      const fresh = await withStorageLock(journalKey, () => {
        requireNoLegacyJournal();
        const key = slot(request);
        // Quiescence reconciliation gets a separate retained slot under the
        // same lock. It may observe a held request, never replace or replay it.
        if (readJournal(key) || (request.action !== "reconcile" && readJournal(`${journalKey}.reconcile`))) return false;
        localStorage.setItem(key, JSON.stringify(original));
        if (!sameRequest(readJournal(key), original)) throw new Error("The browser could not save this request. Nothing was sent.");
        return true;
      }, lifetime.current.signal);
      if (!fresh) throw new Error("A saved request already owns this project. Refresh and check its outcome before continuing.");
      if (mounted.current) syncJournal();
      if (!mounted.current) return;
      await deliver(original);
    } catch (cause) { if (mounted.current && currentSelection.current === selectionAtSend) setError(errorText(cause)); }
    finally { if (mounted.current) { setBusy(false); syncJournal(); setRefresh((value) => value + 1); } }
  }
  async function inspectPending(original: Journal) {
    setBusy(true); setError("");
    try { await checkOutcome(original); }
    catch (cause) { if (mounted.current) setError(errorText(cause)); }
    finally { if (mounted.current) setBusy(false); }
  }
  async function retainUncertainty(original: Journal) {
    setBusy(true); setError("");
    try {
      // Saved browser evidence never authorizes moving the active fence.
      const receipt = await receiptFor(original);
      if (!receipt.found || receipt.delivery_state !== "unknown" || !receipt.conversation_id ||
        !receipt.generation || !receipt.retained_uncertainty_session_id ||
        (original.conversation && original.conversation !== receipt.conversation_id)) {
        throw new Error("The service has not confirmed safe separation for this exact uncertain request. Reconcile process state and check again.");
      }
      const owned = await conversationRead({ action: "detail", project_id: projectId, conversation_id: receipt.conversation_id, after: 0 });
      const current = owned.conversation;
      if (current.project_id !== projectId || current.id !== receipt.conversation_id ||
        current.generation !== receipt.generation || current.session_id !== receipt.retained_uncertainty_session_id ||
        current.state !== "history_only" || current.binding_valid || current.reservation_state !== "released") {
        throw new Error("The conversation's recovery evidence changed. Keep the original request and refresh.");
      }
      if (!mounted.current) return;
      const moved = await withStorageLock(journalKey, () => {
        const saved = readJournal(journalKey);
        if (!saved || !sameRequest(saved, original) || readJournal(`${journalKey}.reconcile`)) {
          throw new Error("The saved request changed or reconciliation is still pending. Refresh before continuing.");
        }
        const key = `${retainedPrefix}${encodeURIComponent(original.id)}`;
        if (!sameRequest(readJournal(journalKey), original) || localStorage.getItem(key) !== null ||
          readJournal(`${journalKey}.reconcile`)) throw new Error("Another window changed saved requests. Nothing was removed.");
        const archive = JSON.stringify({ ...saved, status: "unknown", conversation: receipt.conversation_id, receipt });
        localStorage.setItem(key, archive);
        if (localStorage.getItem(key) !== archive || !sameRequest(readJournal(key), original)) {
          throw new Error("The browser could not retain the uncertain request. The active request is still held.");
        }
        if (!sameRequest(readJournal(journalKey), original)) throw new Error("Another window changed the active request. Its request remains held.");
        localStorage.removeItem(journalKey);
        if (localStorage.getItem(journalKey) !== null) throw new Error("The active request could not be released. Keep both records for inspection.");
        return true;
      }, lifetime.current.signal);
      if (moved && mounted.current) {
        setInput("");
        setNotice("The original request remains uncertain and retained for inspection. Its conversation is history only. Choose New conversation to deliberately start separate work.");
        setRefresh((value) => value + 1);
      }
    } catch (cause) { if (mounted.current) setError(errorText(cause)); }
    finally { if (mounted.current) { syncJournal(); setBusy(false); } }
  }
  async function releasePending(original: Journal) {
    setBusy(true);
    try {
      const receipt = await receiptFor(original);
      const refused = receipt.found && receipt.state === "refused" && receipt.delivery_state === "failed";
      if (!refused && !settledReceipt(receipt, original.request)) throw new Error("The request is not safely resolved. Keep it and check its outcome.");
      if (receipt?.conversation_id) {
        const owned = await conversationRead({ action: "detail", project_id: projectId, conversation_id: receipt.conversation_id, after: 0 });
        if (!mounted.current) return;
        if (owned.conversation.project_id !== projectId || owned.conversation.id !== receipt.conversation_id) throw new Error("Conversation ownership could not be confirmed.");
        // Persist the eventual Start mapping before removing the project fence.
        savePreference(preference, receipt.conversation_id);
        if (localStorage.getItem(preference) !== receipt.conversation_id) throw new Error("The browser could not retain the conversation selection. Keep the saved request.");
      }
      await updateJournal(original, () => undefined);
      if (mounted.current) {
        if (receipt?.conversation_id) choose(receipt.conversation_id);
        if (receipt?.admission_id) setAdmissionId(receipt.admission_id);
        if (original.status === "succeeded" && "input" in original.request && typeof original.request.input === "string" && input === original.request.input) setInput("");
        setNotice(refused ? "The refused request was released. Review your text before sending a new request." : "The recorded request was resolved. Backend session state determines which actions are available.");
        setRefresh((value) => value + 1);
      }
    } catch (cause) { if (mounted.current) setError(errorText(cause)); }
    finally { if (mounted.current) setBusy(false); }
  }
  async function replaySetup() {
    if (!journal || !["create_profile", "prepare", "activate"].includes(journal.request.action)) return;
    setBusy(true);
    try {
      await prepareConversationMutation(journal.request);
      if (await requestHash(journal.request) !== journal.hash) throw new Error("The original request hash does not match.");
      const claimed = await withStorageLock(journalKey, () => {
        requireNoLegacyJournal();
        const saved = readJournal(journalKey);
        if (!sameRequest(saved, journal) || !saved || saved.status !== "unknown" || readJournal(`${journalKey}.reconcile`)) return false;
        localStorage.setItem(journalKey, JSON.stringify({ ...saved, status: "published" }));
        return true;
      }, lifetime.current.signal);
      if (!claimed) throw new Error("Another window owns this request. Check its outcome.");
      if (!mounted.current) return;
      await deliver(journal);
    } catch (cause) { if (mounted.current) setError(errorText(cause)); }
    finally { if (mounted.current) { setBusy(false); syncJournal(); } }
  }
  const current = detail?.conversation.id === selected ? detail.conversation : undefined;
  const staleTarget = !!target && (!targetCurrent || !current || current.project_id !== target.project_id || current.id !== target.conversation_id ||
    current.generation !== target.generation || (target.turn_id !== null && current.active_turn_id !== target.turn_id) ||
    (target.request_id !== null && !detail?.requests.some((request) => request.generation === target.generation && request.request_id === target.request_id)) ||
    (target.operation_id !== null && detail?.unresolved_operation_id !== target.operation_id));
  const selectedMessages = (messages?.messages || []).filter((message) => sourceSelection[message.id] === message.content_hash && message.selectable);
  const selectedBytes = selectedMessages.reduce((total, message) => total + bytes(message.text), 0);
  const selectionChanged = Object.keys(sourceSelection).length !== selectedMessages.length;
  const blocked = staleTarget || busy || loading || !journalReady || (!!selected && !current) || !!journal || !!recoveryJournal || !!journalError;
  const idle = current?.state === "idle" && current.binding_valid && !detail?.unresolved_operation && !current.active_turn_id && !current.is_probe;
  const liveIdle = idle && current?.reservation_state === "running";
  const running = current?.state === "running" && !!current.active_turn_id && current.binding_valid && !current.is_probe && !detail?.unresolved_operation;
  const inputValid = input.trim().length > 0 && bytes(input) <= MAX_INPUT;
  const activeAdmissions = admissions.filter((item) => item.recorded_state === "active" && profiles.some((profile) => profile.id === item.profile_revision_id && qualifiedProfile(profile)));
  const selectedAdmission = activeAdmissions.find((item) => item.id === admissionId);
  const config = execution?.configurations.find((item) => item.id === configId);
  return <section className="conversation-workspace" data-attention-target={`conversation:${target?.conversation_id || selected}`} tabIndex={-1} aria-labelledby="conversation-heading">
    <header className="page-heading"><div><span className="eyebrow">Project conversations</span><h1 id="conversation-heading">Conversations</h1>
      <p>Experimental read-only repository conversations. Qualified scope: macOS, codex-cli 0.167.0, gpt-6.1-sol, medium effort. Standalone enforcement and model lifecycle evidence are separate; model tool-denial was not observed. Each project still needs its own admission.</p>
    </div><button disabled={busy} onClick={() => { onLeaveTarget?.(); choose(""); }}>New conversation</button></header>
    <div className="button-row"><button onClick={() => { setRefresh((value) => value + 1); syncJournal(); }}>Refresh history</button><button onClick={() => onOpenSetup(projectId)}>Project setup</button><button onClick={onOpenProviders}>Providers</button></div>
    {(error || journalError) && <div role="alert"><p>Review Project setup and Providers for configuration problems. If a request may have been sent, use Refresh / check outcome before trying again. Your input is retained.</p><ErrorNotice error={journalError || error} /></div>}
    {notice && <p role="status">{notice}</p>}
    {staleTarget && <p role="alert">This attention target changed or is unavailable. Conversation controls are held. Refresh attention and open the current exact item; no replacement conversation was selected.</p>}
    {retained.map((item) => <section key={item.id} className="panel" aria-label="Retained uncertain request">
      <h2>Retained uncertain request: {item.request.action.replaceAll("_", " ")}</h2>
      <p>This request's outcome remains unknown. It will never be resent here. Its conversation remains history only.</p>
      <details><summary>Inspect retained request</summary><p>Operation: {item.id}</p><p>Hash: {item.hash}</p><pre>{item.body}</pre></details>
      {item.conversation && <button onClick={() => choose(item.conversation || "")}>Open history only conversation</button>}
    </section>)}
    {[journal, recoveryJournal].filter((item): item is Journal => !!item).map((pending) => <section key={pending.id} className="panel" aria-label="Saved request outcome"><h2>Saved request: {pending.request.action.replaceAll("_", " ")}</h2>
      <p>{pending.status === "published" || pending.status === "unknown" ? "This request may have reached the service. Reload never sends it again. Check its outcome before continuing." : pending.status === "refused" ? "The service refused this request. Your original input is retained." : "The service recorded this request. Review its outcome before releasing it."}</p>
      <details><summary>Original request and technical details</summary><pre>{pending.body}</pre>{pending.receipt && <pre>{JSON.stringify(pending.receipt, null, 2)}</pre>}</details>
      <div className="button-row"><button disabled={busy} onClick={() => void inspectPending(pending)}>Refresh / check outcome</button>
        {pending.conversation && <button disabled={busy} onClick={() => choose(pending.conversation || "")}>Open recorded conversation</button>}
        {pending === journal && pending.status === "unknown" && pending.receipt?.retained_uncertainty_session_id && <button disabled={busy || !!recoveryJournal || !!journalError} onClick={() => void retainUncertainty(pending)}>Retain uncertainty and allow separate work</button>}
        {(pending.status === "succeeded" || pending.status === "refused") && <button disabled={busy} onClick={() => void releasePending(pending)}>Continue after confirmed outcome</button>}
        {pending === journal && pending.status === "unknown" && ["create_profile", "prepare", "activate"].includes(pending.request.action) && <button disabled={busy || !!recoveryJournal || !!journalError} onClick={() => void replaySetup()}>Retry exact saved setup request</button>}</div>
      <p>Setup retry reuses the original ID and body. Message, probe, resume, interrupt and disconnect requests are never replayed here.</p>
    </section>)}
    <div className="conversation-columns"><aside className="panel" aria-label="Saved conversations"><h2>Saved conversations</h2>
      {!conversations.length && <p>No conversations loaded for this project.</p>}
      <ul>{conversations.map((item) => <li key={item.id}><button disabled={busy} aria-current={selected === item.id ? "true" : undefined} onClick={() => { onLeaveTarget?.(); choose(item.id); }}>{item.is_probe ? "Probe" : "Conversation"} · {new Date(item.created_at).toLocaleString()} · {item.state.replaceAll("_", " ")}</button></li>)}</ul>
      {cursors.list && <button disabled={busy} onClick={() => void more("list")}>Load more conversations</button>}
    </aside><div className="conversation-main">
      {selected ? <section className="panel" aria-label="Selected conversation">
        {loading && <p role="status">Loading recorded conversation…</p>}
        {!loading && !detail && <p>The selected conversation is unavailable in this project. Refresh or choose a conversation explicitly; another conversation will not be substituted.</p>}
        {detail && current && <><h2>{current.is_probe ? "Probe conversation" : "Conversation"}</h2><p>State: {current.state.replaceAll("_", " ")} · {detail.profile.model} · {detail.profile.effort}</p>
          <p>Partial recorded conversation, not a complete native transcript. Tool entries contain safe summaries only; raw tool output is unavailable.</p>
          <details><summary>Session and admission details</summary><p>Generation: {current.generation} · Session: {current.session_id || "none"} · Active turn: {current.active_turn_id || "none"} · Reservation: {current.reservation_state || "none"} · Binding: {current.binding_valid ? "valid" : "history only"}</p><p>Admission recorded as {detail.admission.recorded_state}; this is not a live readiness guarantee.</p></details>
          {(!current.binding_valid || detail.unresolved_operation || current.state === "recovery_required") && <p role="status">Recovery is required or a control is unresolved. Check the saved request and refresh. Reconcile only checks recorded process quiescence; Resume requires unchanged identity and confirmed history. A history-only conversation cannot resume.</p>}
          <ol className="conversation-history" aria-label="Recorded messages and tool summaries">
            {historyEntries(messages, detail.events).map((entry) => <li key={entry.id}><strong>{entry.label}</strong><p>{entry.text}</p>{entry.incomplete && <small>Visible history; unavailable as a completed source message.</small>}</li>)}
          </ol>
          {!current.is_probe && <section className="conversation-promotion" aria-label="Select task source messages">
            <h3>Create task from conversation</h3>
            <p>Select recorded messages to freeze as task reference material. Excerpts cannot be edited here. Server validation is authoritative.</p>
            {(messages?.messages || []).filter((message) => message.selectable).map((message) => <label key={message.id} className="conversation-source-choice">
              <input type="checkbox" checked={sourceSelection[message.id] === message.content_hash} disabled={loading || busy || staleTarget}
                onChange={(event) => setSourceSelection((previous) => { const next = { ...previous }; if (event.target.checked) next[message.id] = message.content_hash; else delete next[message.id]; return next; })} />
              <span><strong>{message.source.kind === "user" ? "You" : "Assistant"}</strong><span className="conversation-excerpt">{message.text}</span><small>{message.id} · {message.coverage} · {message.content_hash}</small></span>
            </label>)}
            <p>{selectedMessages.length ? `${selectedMessages.length} / 16 messages · ${selectedBytes} / 24576 UTF-8 bytes.` : "No source messages selected; the source conversation link will still be saved."}</p>
            {selectionChanged && <p role="alert">A selected record changed or is no longer available. Clear the selection and select current records before continuing.</p>}
            {(selectedMessages.length > 16 || selectedBytes > 24576) && <p role="alert">Select at most 16 messages and 24 KiB of reference text. Remove messages to continue.</p>}
            <div className="button-row"><button disabled={loading || busy || !messages || staleTarget || selectionChanged || selectedMessages.length > 16 || selectedBytes > 24576}
              onClick={() => onPromote({ project_id: projectId, source: { conversation_id: current.id, messages: selectedMessages.map((message) => ({ message_id: message.id, content_hash: message.content_hash })) }, previews: selectedMessages,
                original_request: messages?.messages.find((message) => message.source.kind === "user" && message.selectable)?.text || "" })}>Create task from conversation</button>
              <button disabled={busy} onClick={() => setSourceSelection({})}>Clear source selection</button></div>
          </section>}
          {detail.requests.length > 0 && <section aria-label="Native request history"><h3>Denied or unsupported native requests</h3><ul>{detail.requests.map((request) => <li key={`${request.generation}:${request.request_id}`}>{request.method} · {request.state}. This is recorded history and grants no permission. Native questions cannot be answered here.</li>)}</ul></section>}
          <div className="button-row">
            <button disabled={blocked || !running || !current.session_id} onClick={() => current.session_id && current.active_turn_id && void send({ action: "interrupt_turn", operation_id: operationId(), conversation_id: current.id, expected_session_id: current.session_id, expected_generation: current.generation, expected_turn_id: current.active_turn_id })}>Interrupt turn</button>
            <button disabled={blocked || !liveIdle || !current.session_id} onClick={() => current.session_id && void send({ action: "disconnect_idle", operation_id: operationId(), conversation_id: current.id, expected_session_id: current.session_id, expected_generation: current.generation })}>Disconnect idle session</button>
            <button disabled={staleTarget || busy || loading || !!recoveryJournal || !!journalError || !current.session_id || !["recovery_required", "history_only", "idle", "stopping"].includes(current.state) || current.reservation_state === "running"} onClick={() => void send({ action: "reconcile", operation_id: operationId(), conversation_id: current.id })}>Reconcile process state</button>
            <button disabled={blocked || !idle || current.reservation_state !== "released"} onClick={() => void send({ action: "resume", operation_id: operationId(), conversation_id: current.id })}>Resume conversation</button>
          </div><p>Disconnect releases capacity only after the exact process has exited. Ready tasks may then start if pickup is enabled. Interrupt waits for the exact turn outcome and acknowledgement; it keeps the session reserved.</p>
        </>}
      </section> : <section className="panel"><h2>New conversation</h2><label>Admitted conversation profile<select value={admissionId} onChange={(event) => setAdmissionId(event.target.value)}><option value="">Choose a recorded active admission</option>{activeAdmissions.map((item) => { const profile = profiles.find((saved) => saved.id === item.profile_revision_id); return <option key={item.id} value={item.id}>{profile?.model} · {profile?.effort} · profile {profile?.id}</option>; })}</select></label><p>Active is a historical recorded state. Starting rechecks current eligibility and may refuse. Selecting a profile does not start a provider.</p>{!activeAdmissions.length && <p>No admitted profile is loaded. Use the setup steps below, or load more profiles and admissions.</p>}</section>}
      <section className="panel"><label htmlFor="conversation-input">{selected ? "Follow-up or steering message" : "First message"}</label><textarea id="conversation-input" rows={5} value={input} onChange={(event) => setInput(event.target.value)} aria-describedby="conversation-input-limit" /><small id="conversation-input-limit">{bytes(input)} / {MAX_INPUT} UTF-8 bytes. Ordinary textual questions can be answered with a follow-up.</small>
        <div className="button-row"><button className="primary" disabled={blocked || !inputValid || (selected ? !liveIdle : !selectedAdmission)} onClick={() => void send(selected ? { action: "submit", operation_id: operationId(), conversation_id: selected, input } : { action: "start", operation_id: operationId(), admission_id: admissionId, input })}>{selected ? "Send follow-up" : "Ask / start conversation"}</button>
          {selected && <button disabled={blocked || !running || !inputValid} onClick={() => current?.active_turn_id && void send({ action: "steer", operation_id: operationId(), conversation_id: current.id, expected_turn_id: current.active_turn_id, input })}>Steer active turn</button>}</div>
      </section>
    </div></div>
    <details className="panel"><summary>Conversation setup: save profile → Prepare → Probe → inspect evidence → Activate</summary>
      <p>Use a saved execution configuration from Project setup. Profiles are immutable. Prepare validates the exact local configuration. Probe explicitly starts a provider; activation is a separate decision after reviewing its recorded evidence.</p>
      <label>Saved execution configuration<select value={configId} onChange={(event) => setConfigId(event.target.value)}><option value="">Choose saved configuration</option>{execution?.configurations.map((item) => <option key={item.id} value={item.id}>Revision {item.revision} · {item.id}</option>)}</select></label>
      {config && <details><summary>Inspect saved execution configuration</summary><pre>{JSON.stringify(config.configuration, null, 2)}</pre></details>}
      {!execution?.configurations.length && <p>Save an execution configuration in Project setup first.</p>}
      <p>Supported qualified model: gpt-6.1-sol. Effort: medium. No model or provider substitution.</p>
      <label>Profile instructions<textarea rows={3} value={instructions} onChange={(event) => setInstructions(event.target.value)} /></label>
      <button disabled={blocked || !config || !instructions.trim() || bytes(instructions) > MAX_INPUT} onClick={() => void send({ action: "create_profile", input: { operation_id: operationId(), project_id: projectId, execution_config_revision_id: configId, model: "gpt-6.1-sol", effort: "medium", instructions } })}>Save immutable conversation profile</button>
      {profiles.map((profile) => <section key={profile.id} className="conversation-profile"><h3>{profile.model} · {profile.effort}</h3>{!qualifiedProfile(profile) && <p>This historical profile is outside the current qualified model and effort. Save an exact supported profile to prepare new admission.</p>}<details><summary>Inspect immutable profile</summary><p>Profile: {profile.id} · Execution configuration: {profile.execution_config_revision_id}</p><pre>{profile.instructions}</pre><p>Profile hash: {profile.profile_hash}</p></details><button disabled={blocked || !qualifiedProfile(profile)} onClick={() => void send({ action: "prepare", operation_id: operationId(), profile_revision_id: profile.id })}>Prepare this profile</button>
        {admissions.filter((item) => item.profile_revision_id === profile.id).map((item) => <div key={item.id}><p>Recorded admission: {item.recorded_state}. Live eligibility has not been checked by this listing.</p>
          <button disabled={blocked || !qualifiedProfile(profile) || item.recorded_state !== "prepared"} onClick={() => void send({ action: "probe", operation_id: operationId(), admission_id: item.id })}>Run provider Probe</button>
          {item.probe_invocation_id && <button disabled={busy} onClick={() => choose(item.probe_invocation_id || "")}>Inspect Probe conversation</button>}
          <details><summary>Inspect recorded admission evidence</summary><p>Evidence hash: {item.evidence_hash || "Not ready; open the Probe conversation and refresh after it finishes."}</p><p>Probe: {item.probe_invocation_id || "Not started"}</p></details>
          <button disabled={blocked || !qualifiedProfile(profile) || item.recorded_state !== "proven" || !item.evidence_hash} onClick={() => item.evidence_hash && void send({ action: "activate", operation_id: operationId(), admission_id: item.id, evidence_hash: item.evidence_hash })}>Activate reviewed evidence</button>
        </div>)}
      </section>)}
      <div className="button-row">{cursors.profiles && <button disabled={busy} onClick={() => void more("profiles")}>Load more profiles</button>}{cursors.admissions && <button disabled={busy} onClick={() => void more("admissions")}>Load more admissions</button>}</div>
    </details>
  </section>;
}

function merge<T extends { id: string }>(previous: T[], incoming: T[]): T[] {
  return Array.from(new Map([...previous, ...incoming].map((item) => [item.id, item])).values());
}
const qualifiedProfile = (profile: ConversationProfile) => profile.model === "gpt-6.1-sol" && profile.effort === "medium";
async function readDiscoveryPages<T>(read: (cursor: string | null) => Promise<ConversationPage<T>>, pageBudget: number, abandoned: () => boolean): Promise<ConversationPage<T>> {
  const items: T[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < pageBudget; page += 1) {
    if (abandoned()) break;
    const result = await read(cursor);
    items.push(...result.items);
    cursor = result.next_cursor;
    if (!cursor) break;
  }
  return { items, next_cursor: cursor };
}
function historyEntries(messages: ConversationMessages | undefined, events: ConversationEvent[]) {
  const entries = (messages?.messages || []).map((message) => ({ id: message.id, sequence: message.first_sequence, label: message.source.kind === "user" ? "You" : "Assistant", text: message.text, incomplete: !message.selectable }));
  for (const event of events) {
    if (event.kind === "tool_activity") {
      const payload = event.payload;
      entries.push({ id: `event:${event.sequence}`, sequence: event.sequence, label: "Tool summary", text: `${typeof payload.type === "string" ? payload.type : "Tool"} · ${typeof payload.phase === "string" ? payload.phase : "recorded activity"}${typeof payload.exit_code === "number" ? ` · exit ${payload.exit_code}` : ""}${payload.native_permission_error === true ? " · permission denied" : ""}. Raw output is unavailable.`, incomplete: false });
    } else if (event.kind === "turn_completed" || event.kind === "error") {
      entries.push({ id: `event:${event.sequence}`, sequence: event.sequence, label: event.kind === "error" ? "Conversation error" : "Turn outcome", text: event.kind === "error" ? "The native conversation reported an error. Check its outcome before continuing." : typeof event.payload.status === "string" ? event.payload.status : "Recorded terminal event", incomplete: false });
    }
  }
  return entries.sort((left, right) => left.sequence - right.sequence);
}
