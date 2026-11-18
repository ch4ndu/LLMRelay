---
name: workflow-audit
description: Use only when explicitly requested to audit one named LLMRelay user journey or invariant. Trace existing behavior and report findings and coverage gaps in a read-only review.
---

# Workflow audit

1. Define the audit in one sentence with the user action or trigger, the expected observable outcome, and the boundary of the review. Read the relevant repository guides and current code on that path, including unchanged files. The engineering workflow, coding standards, security guide, and applicable TRIP change review gates remain authoritative.
2. Follow the chosen path from the actual entry point to the expected observable outcome, tracing the request, Rust operations, persistence, and observable readback where present on that path. Include reload or restart when it matters to the outcome. Examine one plausible interruption or retry when it could change the result. Check permission or provider differences only where they affect the chosen path.
3. State precisely what the code proves. Distinguish source-proven mismatches, observed runtime behavior, and inference. Mark unexercised paths and outcomes that require a live check as unverified. For a high-impact finding, corroborate it with an independent source or existing artifact, such as checking the schema alongside the write query. If corroboration is unavailable, state the uncertainty and reflect it in the confidence rating.
4. Report a short list ordered by user impact. For each finding, include the trigger, expected and actual behavior, user impact, precise file and line references or artifact evidence, and confidence. Summarize coverage and gaps even when the evidence supports no findings.

Keep the audit read-only and findings-only. Run it directly in the current session. Fixes, provider calls, and live launches require separate authorization. Use a security-focused differential review only for separately requested security work. Reply in the conversation by default. If the user requests a report artifact, save it under `.local/`.

Conceptual reference [pietz/skills code-audit at fea9d30](https://github.com/pietz/skills/blob/fea9d30cf2ee8cf8db5385a9bae5a6dab0b65988/skills/code-audit/SKILL.md).
