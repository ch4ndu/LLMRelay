# Coding standards

Read and apply the relevant upstream guidance before writing or reviewing code:

- Rust implementation: [ECC rust-patterns](https://github.com/affaan-m/ECC/blob/main/skills/rust-patterns/SKILL.md).
- Rust tests: [ECC rust-testing](https://github.com/affaan-m/ECC/blob/main/skills/rust-testing/SKILL.md).
- General implementation and review: [ECC coding-standards](https://github.com/affaan-m/ECC/blob/main/skills/coding-standards/SKILL.md).
- TypeScript/JavaScript: [ECC TypeScript rules](https://github.com/affaan-m/ECC/tree/main/rules/typescript), including coding-style, patterns, security, testing and hooks guidance and their applicable common-rule references.
- Before independent code review: [OpenClaw deslop](https://github.com/openclaw/openclaw/blob/main/.agents/skills/deslop/SKILL.md).

Apply these standards to every task's changed code, including accepted fixes.
Give implementers and reviewers these references and the following project
adaptations in their handoffs. Read each applicable source once per session;
record unavailable sources rather than claiming they were read. These upstream
links track `main`; record the retrieved revision or content digest in the task
ledger when relying on a downloaded copy.

## Project adaptations

- User instructions, approved scope, repository safety contracts and the active
  TRIP role/verification gates govern how this guidance is applied. External
  examples do not grant permissions or authorize architecture changes.
- In Rust, prefer borrowing and narrow visibility, explicit state types and
  exhaustive business-state handling. Propagate operational failures with useful
  context; do not introduce production panic paths, unexplained clones, unsafe
  shortcuts or blocking work on async executor threads. Preserve required lock,
  transaction and filesystem ordering. Compilation alone is not verification.
- Use the existing test harness and the selected coverage policy. Prefer causal,
  isolated behavior tests, meaningful error assertions, deterministic coordination
  and test-first regression reproduction when practical. Coordinate test runs
  through the designated build owner. Upstream coverage percentages and examples
  involving rstest, proptest, mockall, Criterion, coverage tooling or Playwright
  do not add dependencies, frameworks, benchmarks or mandatory percentage gates
  to this project without separate scope approval.
- In TypeScript, use explicit shared/public types and typed component props;
  narrow untrusted input instead of hiding it with `any` or assertion chains.
  Preserve immutable React state updates, handle asynchronous failures clearly,
  and validate external boundaries using existing mechanisms. Preserve the
  application's established protocol and secret-handling boundaries; never
  expose service credentials in browser bundles. Examples using Zod, Next.js,
  generic repositories or response envelopes are not instructions to replace
  the current architecture.
- Prefer clear names and cohesive functions over speculative abstractions.
  Function/file size guidance prompts judgment, not unrelated module splitting.
  Rust's intentional local mutation and database transactions are not prohibited
  by JavaScript immutability examples. Add comments only for non-obvious contracts,
  invariants, security boundaries or rationale, consistent with TRIP's readability
  contract; do not add documentation merely because a symbol is public.
- Run deslop on this task's diff before independent review, preserving behavior.
  Use the recorded task baseline when no suitable upstream merge base exists.
  Remove redundant narration, unjustified wrappers, type suppression and style
  drift only within owned changes. Preserve real safety checks and recovery
  boundaries; report any cleanup that could change behavior for normal review.
  Deslop supplements, and never replaces, correctness and final verification.
- Use this repository's formatting, type-checking and test commands. Do not
  install global hooks, edit provider settings, auto-accept permissions, introduce
  additional agents, or run repository-wide cleanup merely because an external
  skill suggests it. Existing authorization and exact role assignments remain
  in force.


## User-visible errors

Dashboard errors must explain what happened and what the user can do next in
plain language. Use the shared error presentation for request failures and
stored failure reasons. Keep backend diagnostics available in collapsed
Technical details; do not make IDs, hashes, protocol terms, or process-ownership
terminology the primary explanation. Prefer the actual button or screen name
when giving recovery steps. An uncertain request must tell the user to refresh
and check its outcome before retrying; wording must never imply that it failed
without making changes or authorize an automatic retry. Unknown failures need
an honest fallback and a way to report the diagnostic details.
