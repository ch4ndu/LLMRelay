import type { ReactNode } from "react";

export type ErrorGuidance = { summary: string; nextStep: string };

// Backend diagnostics remain unchanged. This is display copy, never retry authority.
export function explainError(error: string): ErrorGuidance {
  const text = error.toLowerCase();
  if (
    text.startsWith("ordinary runtime probe reported observation_unavailable") ||
    (text.startsWith("ordinary runtime probe reported an observed failure") &&
      text.includes("failure category: observation_unavailable"))
  ) {
    const missing = text.startsWith("ordinary runtime probe reported observation_unavailable")
      ? text.split(":", 2)[1]?.trim()
      : undefined;
    const descriptions: Record<string, string> = {
      model_evidence: "The agent could not observe which model was running.",
      session_mode_observed: "The agent could not confirm its conversation was retained or fresh as required.",
      target_data_accessed: "The agent could not confirm whether it accessed the target project.",
      fallback_observed: "The agent could not confirm whether a fallback was used.",
      authentication_observed: "The agent could not confirm its authenticated session.",
    };
    return {
      summary: descriptions[missing ?? ""] ??
        "Agent verification is incomplete, but the agent did not identify which required observation was missing.",
      nextStep:
        "Choose View output and review this agent profile before spending another call. The failed check cannot publish proof. After the session has stopped and the cause is understood, choose Prepare a new verification and review its scope before approving it.",
    };
  }
  if (text.startsWith("runtime model evidence does not match the bound first-invocation provider sessionstart observation")) {
    return {
      summary: "The reported model could not be matched to the native session's recorded model.",
      nextStep:
        "Choose View output and check this agent profile. This verification failed and cannot publish proof. Once the session has stopped and the mismatch is understood, choose Prepare a new verification.",
    };
  }
  if (text.startsWith("ordinary runtime probe reported an observed failure")) {
    return {
      summary: "The agent recorded a failed verification.",
      nextStep:
        "Review the affected role's explanation and choose View output. Correct the reported issue before using Prepare a new verification; the failed check stays in history and a new check needs approval for another agent call.",
    };
  }
  // Matched first: the listed outcomes are free text that can contain other
  // patterns' keywords, such as "timeout".
  if (
    text.startsWith(
      "the failed step may have started an agent action, so it cannot be retried until these have a known outcome:",
    )
  ) {
    return {
      summary:
        "LLMRelay can't retry this step until the outcome of its agent action is known.",
      nextStep:
        "The failed step may have started an agent action. Check the task's Activity tab, then refresh the dashboard. Retry failed step becomes available after LLMRelay records that outcome; to stop the task instead, choose Cancel in Controls. Technical details lists the actions still waiting.",
    };
  }
  if (
    /browser session required|unauthorized|bootstrap.*(expired|invalid|used)/
      .test(text)
  ) {
    return {
      summary: "Your dashboard sign-in has expired.",
      nextStep:
        "Open a fresh dashboard sign-in link from the terminal running LLMRelay. Refreshing this page alone will not sign you in.",
    };
  }
  if (
    /files that new tasks need were changed|policy materialization collision|approved guidance collides/
      .test(text)
  ) {
    return {
      summary:
        "Some project files have uncommitted changes that new tasks would not get.",
      nextStep:
        "Commit or restore the files named in Technical details, then choose Validate and relink in Project settings so new tasks start from the current commit. LLMRelay did not overwrite anything.",
    };
  }
  if (
    /policy file changed after verified materialization|stay fixed for the whole attempt/
      .test(text)
  ) {
    return {
      summary:
        "A workflow or guidance file in the task's workspace was changed during the attempt.",
      nextStep:
        "If the approved plan includes this documentation edit, preserve it and use Review planned documentation update in the task's Overview. Otherwise restore the reviewed copy named in Technical details. Resolve other recovery holds separately; approval never changes protected workflow files.",
    };
  }
  if (/profile change pending/.test(text)) {
    return {
      summary: "New agent settings are waiting to take effect.",
      nextStep:
        "They apply when the role's current session finishes. If Technical details asks for verification, verify the profile in the task's Roles tab.",
    };
  }
  if (/project folder is now at commit/.test(text)) {
    return {
      summary: "The project folder has moved to a different commit.",
      nextStep:
        "Choose Validate and relink in Project settings so new tasks start from the current commit. Relinking is available when no task in this project is running.",
    };
  }
  if (
    /workflow files in the project folder changed after setup|activated manifest drifted|activated package file drifted/
      .test(text)
  ) {
    return {
      summary: "The project's workflow files changed after setup.",
      nextStep:
        "Open Project setup to review the change. LLMRelay will not overwrite it.",
    };
  }
  if (text.includes("setup discovery manager must exit before profile probes are frozen")) {
    return {
      summary: "Discovery has reported, but its agent session has not closed yet.",
      nextStep:
        "Wait for Project setup to show that the discovery session has exited, then authorize the profile probes. If it remains open, choose View output to inspect it and check for a recovery action. Your saved setup information does not need correcting.",
    };
  }
  if (/role capacity|capacity.*full/.test(text)) {
    return {
      summary: "All available agent slots are busy.",
      nextStep:
        "Wait for an active session to finish, then try this action again. An idle but open agent session still uses a slot.",
    };
  }
  if (
    /ambiguous|may have been dispatched|request deadline|connection ended|timed out|timeout/
      .test(text)
  ) {
    return {
      summary: "LLMRelay could not confirm whether this request finished.",
      nextStep:
        "Refresh the dashboard and check the task or session before retrying. If the request is still pending, wait; do not start a second copy.",
    };
  }
  if (
    /failed to fetch|networkerror|network error|service offline|connection refused|load failed/
      .test(text)
  ) {
    return {
      summary: "The dashboard cannot connect to LLMRelay.",
      nextStep:
        "Check that the LLMRelay service is running in its terminal, then choose Retry connection or refresh the dashboard.",
    };
  }
  if (/protocol|service version|response.*(invalid|malformed)/.test(text)) {
    return {
      summary: "The dashboard and service could not understand each other.",
      nextStep:
        "Reload the dashboard. If this continues, use the dashboard supplied by the running LLMRelay version.",
    };
  }
  if (
    /no reviewed contract|no reviewed compatibility|unsupported.*version|unknown.version/
      .test(text)
  ) {
    return {
      summary:
        "This installed agent version has not been verified for this LLMRelay release.",
      nextStep:
        "Update LLMRelay to a release that supports your agent version, or install the reviewed version listed in Technical details. Then run the profile verification again.",
    };
  }
  if (
    /environments\.toml|managed.config|requirements\.toml|codex_home|shared.app.server|credential.store|local compatibility|cloud.config/
      .test(text)
  ) {
    return {
      summary:
        "This agent installation uses settings LLMRelay does not support.",
      nextStep:
        "Review the setting named in Technical details and the supported-installation requirements in the Security guide. Use a supported agent configuration before verifying again; do not remove organization-managed settings.",
    };
  }
  if (
    /\bauth(?:entication)?\b.*(expired|missing|unavailable)|credentials|access.token|signed.in/
      .test(text)
  ) {
    return {
      summary: "LLMRelay could not use the agent's current sign-in.",
      nextStep:
        "Open the agent's own terminal app and check its sign-in. Complete sign-in there, then return to LLMRelay and verify the profile again.",
    };
  }
  if (
    text.includes("ordinary runtime probe terminal reporting invocation exited quiescently without an accepted role report")
  ) {
    return {
      summary: "That verification finished without an accepted report.",
      nextStep:
        "In Project setup, review Technical details, then choose Prepare a new verification. Review and approve its scope before launching. The earlier check stays in history, and the new check uses another agent call.",
    };
  }
  if (
    text.includes("ordinary runtime evidence does not match the exact authenticated native hook commands and count")
  ) {
    return {
      summary: "Agent verification did not match the required check.",
      nextStep:
        "Choose View output to inspect the rejected report. After the session has stopped, choose Prepare a new verification in Project setup and review its scope before approving another agent call.",
    };
  }
  if (
    /attempt is not dispatchable|needs.recovery|ownership.*unresolved|quiescen|process.inventory|descendant/
      .test(text)
  ) {
    return {
      summary:
        "LLMRelay needs to check that the earlier agent session has stopped.",
      nextStep:
        "Open Project setup or the task's Recovery controls. Review the affected session and choose Check recovery and continue. Continue only after the app confirms the check.",
    };
  }
  if (
    /native.*(identity|session)|retained.*(session|resume)|resume.*(spent|unavailable|rejected)|first.*invocation|without an accepted role report/
      .test(text)
  ) {
    return {
      summary:
        "The earlier agent session cannot be continued with the available evidence.",
      nextStep:
        "Open Project setup or the task's session controls and follow the recovery action shown there. If the app offers a corrected verification, review and approve that new check.",
    };
  }
  if (/hook.*(trust|missing|untrusted)|untrusted.*hook/.test(text)) {
    return {
      summary:
        "The agent has not accepted the connection needed to report its progress.",
      nextStep:
        "Choose View output, then Take control to review LLMRelay's listed hooks in the agent's prompt, and Release control when you are done. Waiting alone will not answer the prompt. Then return to Project setup and follow the available retry or recovery action.",
    };
  }
  if (
    /permission|approval.*(pending|required)|denied|not permitted|eacces|eperm/
      .test(text)
  ) {
    return {
      summary: "This action could not get the access it needs.",
      nextStep:
        "Check the permission inbox and Technical details for the requested action. Approve only access you intend to grant; otherwise leave it denied and adjust the task or settings.",
    };
  }
  if (
    /preimage|destination.*changed|source.*changed|file.*changed|hash.*mismatch/
      .test(text)
  ) {
    return {
      summary: "Files changed after they were reviewed.",
      nextStep:
        "Refresh Project setup and review the updated files before approving the installation again.",
    };
  }
  if (
    /capability|profile.*(unverified|validation|required)|runtime.*(proof|evidence|authorized|authority)|compatibility/
      .test(text)
  ) {
    return {
      summary:
        "This agent profile still needs verification before it can be used.",
      nextStep:
        "Open Project setup or the task's Roles tab. Run the offered runtime verification, then publish its proof after it passes and the session exits.",
    };
  }
  if (
    /stale|version.*(conflict|mismatch|changed)|revision.*(conflict|changed)|changed.*version/
      .test(text)
  ) {
    return {
      summary: "This page is showing an older version of the item.",
      nextStep:
        "Refresh the dashboard, review the current values, then apply your change again if it is still needed.",
    };
  }
  if (/cannot change to.*no such file|repository.*does not exist/.test(text)) {
    return {
      summary: "The selected project folder could not be found.",
      nextStep:
        "Enter the path to an existing Git repository in Project settings, then choose Validate and relink.",
    };
  }
  if (
    /repository|git.*(path|directory)|worktree|symlink|folder.*(missing|unavailable)|no such file/
      .test(text)
  ) {
    return {
      summary:
        "LLMRelay could not use the selected project folder or its files.",
      nextStep:
        "Check the folder and file named in Technical details. In Project settings, choose Validate and relink if the repository moved, then return to the blocked action.",
    };
  }
  if (/storage|quota|retain.*request|persist.*draft|save.*draft/.test(text)) {
    return {
      summary:
        "The browser could not save the information needed to keep this request or draft.",
      nextStep:
        "Copy any unsaved text before closing the page. Check that browser storage is available, then reopen the dashboard and check whether the earlier request completed before submitting again.",
    };
  }
  if (
    /required|must |cannot be empty|invalid|at least|too long|maximum|select |enter /
      .test(text)
  ) {
    return {
      summary: "Some information needs to be corrected before continuing.",
      nextStep:
        "Check the highlighted fields and the specific requirement in Technical details, then submit the corrected information.",
    };
  }
  return {
    summary: "LLMRelay could not complete this action.",
    nextStep:
      "Refresh the dashboard and check whether the action completed before trying again. If it remains blocked, open Technical details and Diagnostics and share those details when reporting the problem.",
  };
}

export function ErrorNotice({ error }: { error: string }) {
  const guidance = explainError(error);
  return (
    <div className="error error-notice" role="alert">
      <strong>{guidance.summary}</strong>
      <p>{guidance.nextStep}</p>
      <details>
        <summary>Technical details</summary>
        <pre>{error}</pre>
      </details>
    </div>
  );
}

export function TechnicalDetails(
  { children }: { children: ReactNode },
) {
  return (
    <details className="technical-details">
      <summary>Technical details</summary>
      {children}
    </details>
  );
}
