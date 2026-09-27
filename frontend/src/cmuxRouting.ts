import type {
  CmuxSessionSurface,
  CmuxViewOutcome,
  CmuxViewState,
  TranscriptFrame,
} from "./types";

export const cmuxRouteLabel = (state: CmuxViewOutcome["state"]) =>
  state.replaceAll("_", " ");

const cmuxSurfaceVersion = (surface: CmuxSessionSurface) =>
  [
    surface.id,
    surface.updated_at,
    surface.binding_revision,
    surface.control_revision,
    surface.applied_revision,
    surface.surface_state,
    surface.attachment_state,
    surface.desired_input_state,
    surface.actual_input_state,
    surface.last_error || "",
  ].join(":");

/** Whether a local operation surface predates the current durable projection. */
export const cmuxSurfaceIsOlderThan = (
  candidate: CmuxSessionSurface,
  durable: CmuxSessionSurface,
) => {
  if (candidate.id !== durable.id) {
    return candidate.updated_at <= durable.updated_at;
  }
  for (
    const [candidateValue, durableValue] of [
      [candidate.updated_at, durable.updated_at],
      [candidate.binding_revision, durable.binding_revision],
      [candidate.control_revision, durable.control_revision],
      [candidate.applied_revision, durable.applied_revision],
    ]
  ) {
    if (candidateValue !== durableValue) return candidateValue < durableValue;
  }
  return cmuxSurfaceVersion(candidate) !== cmuxSurfaceVersion(durable);
};

/** Keep a ref's durable projection monotonic when a stale render arrives late. */
export const cmuxNewestSurface = (
  current?: CmuxSessionSurface,
  next?: CmuxSessionSurface | null,
) =>
  !next || (current && cmuxSurfaceIsOlderThan(next, current)) ? current : next;

/**
 * Derive the rendered presentation state exclusively from the latest durable
 * surface projection. The browser's optimistic operation result is temporary;
 * the next refresh always replaces it with this function's result.
 *
 * A newer desired control revision is intentionally `pending` before any
 * stored actual control value. `actual_input_state: control` may describe a
 * prior connection and is never evidence that the newly rendered card owns
 * keyboard authority.
 */
export const cmuxSurfaceViewState = (
  surface?: CmuxSessionSurface | null,
): CmuxViewState => {
  if (!surface) return "failed";
  if (surface.surface_state === "unknown") {
    return surface.attachment_state === "live" ? "unknown_live" : "unknown";
  }
  if (surface.surface_state === "lost") return "lost";
  if (
    ["retired", "failed"].includes(surface.surface_state) ||
    ["ended", "failed"].includes(surface.attachment_state)
  ) return "failed";
  if (
    surface.surface_state === "opening" ||
    surface.attachment_state === "pending" ||
    surface.applied_revision < surface.control_revision
  ) return "pending";
  if (surface.actual_input_state === "control") return "control";
  if (surface.actual_input_state === "blocked") return "blocked";
  return "view_only";
};

/**
 * Derive durable presentation copy and every dashboard action from one exact
 * surface projection. A live terminal-loss row remains a retirement fence,
 * not a historical row eligible for replacement.
 */
export type CmuxSurfacePresentation = {
  state: CmuxViewState;
  viewAvailable: boolean;
  takeAvailable: boolean;
  releaseAvailable: boolean;
  discardAvailable: boolean;
  retryAvailable: boolean;
  viewLabel: string;
  guidance?: string;
  diagnostic?: string;
};

export const cmuxSurfacePresentation = (
  surface?: CmuxSessionSurface | null,
  pending = false,
): CmuxSurfacePresentation => {
  if (!surface) {
    return {
      state: pending ? "pending" : "failed",
      viewAvailable: !pending,
      takeAvailable: !pending,
      releaseAvailable: false,
      discardAvailable: false,
      retryAvailable: false,
      viewLabel: "View output",
    };
  }

  const durableState = cmuxSurfaceViewState(surface);
  const state = pending ? "pending" : durableState;
  const unknown = surface.surface_state === "unknown";
  const liveRetirement = surface.attachment_state === "live" &&
    ["lost", "retired", "failed"].includes(surface.surface_state);
  const historical = !unknown && !liveRetirement && (
    ["lost", "retired", "failed"].includes(surface.surface_state) ||
    ["ended", "failed"].includes(surface.attachment_state)
  );
  const readyLiveSurface = !pending && surface.surface_state === "open" &&
    surface.attachment_state === "live" &&
    ["view_only", "control", "blocked"].includes(durableState);
  const diagnostic = ["unknown", "unknown_live", "lost", "failed"].includes(
      durableState,
    ) && surface.last_error
    ? surface.last_error
    : undefined;
  const guidance = unknown
    ? surface.attachment_state === "live"
      ? "The exact attachment remains live while cmux presentation is uncertain. View, Take, discard, retry, and recreation stay disabled until that attachment retires."
      : "The cmux reservation remains uncertain. Explicitly discard it before another View can create a fresh surface."
    : liveRetirement
    ? "The exact attachment is still live in the durable retirement interval. View, Take, discard, retry, and recreation stay disabled until durable retirement is observed."
    : historical
    ? "The prior terminal is durably historical. View fresh view-only surface creates a new presentation; the old terminal is never focused or respawned."
    : undefined;
  const viewAvailable = readyLiveSurface || historical;

  return {
    state,
    viewAvailable,
    takeAvailable: readyLiveSurface,
    releaseAvailable: readyLiveSurface &&
      (surface.desired_input_state === "control" ||
        surface.actual_input_state === "control"),
    discardAvailable: !pending && unknown &&
      surface.attachment_state !== "live",
    retryAvailable: viewAvailable,
    viewLabel: historical ? "View fresh view-only surface" : "View output",
    guidance,
    diagnostic,
  };
};

/** Keep an operation response aligned with its durable surface on reload. */
export const cmuxViewOutcomeFromSurface = (
  outcome: CmuxViewOutcome,
): CmuxViewOutcome => {
  if (!outcome.surface) return outcome;
  const presentation = cmuxSurfacePresentation(outcome.surface);
  return {
    ...outcome,
    state: presentation.state,
    retry_available: presentation.retryAvailable,
  };
};

export const cmuxSurfaceOutcome = (
  surface: CmuxSessionSurface,
  message =
    "Current persistent cmux attachment state was refreshed from the durable service projection without issuing a routing action.",
): CmuxViewOutcome => {
  const state = cmuxSurfaceViewState(surface);
  return cmuxViewOutcomeFromSurface({
    state,
    message:
      state === "pending" && surface.applied_revision < surface.control_revision
        ? "The latest dashboard keyboard-control revision is pending on the exact authenticated attachment. The prior durable actual state is not treated as new local input authority."
        : message,
    retry_available: true,
    surface,
  });
};

/**
 * Durable lifecycle and actionability win over local operation state. Pending
 * remains a purely transient affordance only while the durable row is still a
 * normal actionable surface; errors without a newer surface never erase a
 * causal unknown/lost diagnostic.
 */
export const cmuxOutcomeWithDurableSurface = (
  local: CmuxViewOutcome | undefined,
  durable?: CmuxSessionSurface | null,
): CmuxViewOutcome | undefined => {
  if (!durable) return local;
  const durableOutcome = cmuxSurfaceOutcome(durable);
  if (!local) return durableOutcome;
  const durablePresentation = cmuxSurfacePresentation(durable);
  if (
    local.state === "pending" && !local.surface && durablePresentation.viewAvailable &&
    !durablePresentation.diagnostic && !durablePresentation.guidance
  ) {
    return {
      ...durableOutcome,
      state: "pending",
      message: local.message,
      retry_available: false,
    };
  }
  if (
    local.surface && cmuxSurfaceIsOlderThan(durable, local.surface) &&
    !cmuxSurfaceIsOlderThan(local.surface, durable)
  ) {
    return local;
  }
  return durableOutcome;
};

const recordedBoundary = (frame: TranscriptFrame, reason: string) =>
  `\n[recorded output ${reason} at exact epoch ${frame.epoch}, frame ${frame.sequence}]\n`;

class PlainRecordedBytes {
  private state:
    | "text"
    | "escape"
    | "escape_intermediate"
    | "csi"
    | "osc"
    | "osc_escape"
    | "string"
    | "string_escape" = "text";
  private utf8Pending: number[] = [];
  private readonly scalarDecoder = new TextDecoder("utf-8", { fatal: true });

  reset() {
    this.state = "text";
    this.utf8Pending = [];
  }

  pushText(value: string) {
    let result = this.finishUtf8();
    for (const character of value) result += this.pushCharacter(character);
    return result;
  }

  pushBytes(value: Uint8Array) {
    let result = "";
    for (const byte of value) result += this.pushByte(byte);
    return result;
  }

  finish() {
    return this.finishUtf8();
  }

  private finishUtf8() {
    if (!this.utf8Pending.length) return "";
    this.utf8Pending = [];
    return this.pushCharacter("\ufffd");
  }

  private pushByte(byte: number): string {
    if (this.utf8Pending.length) {
      if (byte >= 0x80 && byte <= 0xbf) {
        this.utf8Pending.push(byte);
        const lead = this.utf8Pending[0];
        const expected = lead <= 0xdf ? 2 : lead <= 0xef ? 3 : 4;
        if (this.utf8Pending.length < expected) return "";
        const pending = this.utf8Pending;
        this.utf8Pending = [];
        try {
          return this.pushText(
            this.scalarDecoder.decode(new Uint8Array(pending)),
          );
        } catch {
          return this.pushCharacter("\ufffd");
        }
      }
      this.utf8Pending = [];
      return this.pushCharacter("\ufffd") + this.pushByte(byte);
    }
    if (byte <= 0x9f) return this.pushCharacter(String.fromCodePoint(byte));
    if (byte >= 0xc2 && byte <= 0xf4) {
      this.utf8Pending = [byte];
      return "";
    }
    return this.pushCharacter("\ufffd");
  }

  private pushCharacter(character: string) {
    let result = "";
    switch (this.state) {
      case "text":
        if (character === "\u001b") this.state = "escape";
        else if (character === "\u009b") this.state = "csi";
        else if (character === "\u009d") this.state = "osc";
        else if (["\u0090", "\u0098", "\u009e", "\u009f"].includes(character)) {
          this.state = "string";
        } else if (character === "\r") result = "\n";
        else if (
          character === "\n" ||
          character === "\t" ||
          (character.codePointAt(0)! >= 0x20 &&
            character.codePointAt(0)! !== 0x7f &&
            !(character.codePointAt(0)! >= 0x80 &&
              character.codePointAt(0)! <= 0x9f))
        ) {
          result = character;
        }
        break;
      case "escape":
        this.state = character === "\u001b"
          ? "escape"
          : character === "["
          ? "csi"
          : character === "]"
          ? "osc"
          : ["P", "X", "^", "_"].includes(character)
          ? "string"
          : character >= " " && character <= "/"
          ? "escape_intermediate"
          : "text";
        break;
      case "escape_intermediate":
        this.state = character === "\u001b"
          ? "escape"
          : character >= " " && character <= "/"
          ? "escape_intermediate"
          : "text";
        break;
      case "csi":
        if (character >= "@" && character <= "~") this.state = "text";
        break;
      case "osc":
        if (character === "\u0007" || character === "\u009c") {
          this.state = "text";
        } else if (character === "\u001b") this.state = "osc_escape";
        break;
      case "osc_escape":
        this.state = character === "\\" || character === "\u009c"
          ? "text"
          : character === "\u001b"
          ? "osc_escape"
          : "osc";
        break;
      case "string":
        if (character === "\u009c") this.state = "text";
        else if (character === "\u001b") this.state = "string_escape";
        break;
      case "string_escape":
        this.state = character === "\\" || character === "\u009c"
          ? "text"
          : character === "\u001b"
          ? "string_escape"
          : "string";
        break;
    }
    return result;
  }
}

const base64Bytes = (value: string) =>
  Uint8Array.from(atob(value), (character) => character.charCodeAt(0));

/**
 * This is intentionally a bounded plain-text transcript reader, not a
 * terminal renderer. It keeps UTF-8 and escape-sequence state across adjacent
 * retained frames, then resets that state only at an explicit retention gap or
 * epoch boundary.
 */
export const recordedOutputText = (outcome?: CmuxViewOutcome) => {
  const frames = outcome?.recorded_output?.frames || [];
  const plain = new PlainRecordedBytes();
  let epoch: string | undefined;
  let output = "";
  const reset = () => {
    output += plain.finish();
    plain.reset();
  };

  for (const frame of frames) {
    if (epoch && frame.epoch !== epoch) {
      reset();
      output += recordedBoundary(frame, "changed");
    }
    if (frame.gap) {
      reset();
      output += recordedBoundary(frame, "gap");
    }
    epoch = frame.epoch;
    if (frame.encoding !== "base64") {
      output += plain.pushText(frame.data);
      continue;
    }
    try {
      output += plain.pushBytes(base64Bytes(frame.data));
    } catch {
      reset();
      output += "[recorded output frame could not be decoded]\n";
    }
  }
  output += plain.finish();
  return output;
};
