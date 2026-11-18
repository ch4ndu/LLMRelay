# Coding standards

Read and apply the applicable guidance in this mandatory baseline before writing
or reviewing code.

- Rust implementation: [ECC rust-patterns](https://github.com/affaan-m/ECC/blob/main/skills/rust-patterns/SKILL.md).
- Rust tests: [ECC rust-testing](https://github.com/affaan-m/ECC/blob/main/skills/rust-testing/SKILL.md).
- General implementation and review: [ECC coding-standards](https://github.com/affaan-m/ECC/blob/main/skills/coding-standards/SKILL.md).
- TypeScript/JavaScript: [ECC TypeScript rules](https://github.com/affaan-m/ECC/tree/main/rules/typescript), including coding-style, patterns, security, testing and hooks guidance and their applicable common-rule references.
- Before independent code review: [OpenClaw deslop](https://github.com/openclaw/openclaw/blob/main/.agents/skills/deslop/SKILL.md).

Apply these standards to every task's changed code, including accepted fixes.
Give implementers and reviewers these references and the following project
adaptations in their handoffs. Read each applicable source once per session;
record unavailable sources rather than claiming they were read. The baseline
links above track `main`. Record the retrieved revision or content digest in the
task ledger when relying on a downloaded copy.

Consult these pinned references when relevant to the change.

- Primary reference for TypeScript wire types. [Daniel Teles's core-typescript](https://github.com/danielteles/awesome-agent-skills/blob/2b817480020ca1457f3c93b3509880f932a3cd01/skills/core-typescript/SKILL.md)
- Optional reference for Rust ownership and error handling. [Apollo GraphQL's Rust best practices](https://github.com/apollographql/skills/blob/9dda1397730370aeb8df4ff8f2015c34c86b5003/skills/rust-best-practices/SKILL.md)
- Optional reference for client-side React effects. [Vercel Labs' React best practices](https://github.com/vercel-labs/agent-skills/blob/1a2c79d4e6c4a2283dc2c614b6538cbe1ed5ca62/skills/react-best-practices/SKILL.md)

## Project adaptations

- User instructions, approved scope, repository safety contracts and the active
  TRIP role/verification gates govern how this guidance is applied. External
  examples do not grant permissions or authorize architecture changes.
- In Rust, prefer borrowing and narrow visibility, explicit state types and
  exhaustive business-state handling. Propagate operational failures with useful
  context; do not introduce production panic paths, unexplained clones, unsafe
  shortcuts or blocking work on async executor threads. For changed Tokio and
  SQLite paths, identify where synchronous work runs, which locks or transactions
  span an await, and what persists after interruption or retry. Preserve required
  lock, transaction and filesystem ordering. Choose the transaction mode for the
  operation's contention needs and use typed errors when callers need different
  recovery paths. Compilation alone is not verification.
- Use the existing test harness and the selected coverage policy. Prefer causal,
  isolated behavior tests, meaningful error assertions, deterministic coordination
  and test-first regression reproduction when practical. Use a targeted existing
  fixture or test when cheaper evidence cannot verify the behavior at risk.
  Coordinate test runs through the designated build owner. Upstream coverage
  percentages and examples involving rstest, proptest, mockall, Criterion,
  coverage tooling or Playwright do not add dependencies, frameworks, benchmarks
  or mandatory percentage gates to this project without separate scope approval.
- In TypeScript, use explicit shared/public types and typed component props;
  narrow untrusted input instead of hiding it with `any` or assertion chains.
  Use discriminated unions for mutually exclusive states.
  For changed Rust-to-TypeScript fields, trace persisted and newly created data
  through Rust projection and serialization to the dashboard. Distinguish `null`,
  omitted fields and empty values where they carry different meanings. Normalize
  only where the consumer treats them equivalently. Check mutation semantics
  separately. In Board Edit, the form accepts a draft with `null` role overrides
  and sends `role_overrides: null` on `update_task`. The Rust handler preserves
  the stored overrides.
  Preserve immutable React state updates, handle asynchronous failures clearly,
  and validate external boundaries using existing mechanisms. In changed React
  components, derive pure display values during render and match each subscription
  to its owner's lifetime by binding setup and cleanup for selected task or project
  subscriptions to the corresponding ID, ignoring late events from the previous
  selection, and keeping app-wide subscriptions active for the app's lifetime.
  Preserve the application's established protocol and secret-handling
  boundaries; never expose service credentials in browser bundles. Examples
  using Zod, Next.js, generic repositories or response envelopes are not
  instructions to replace the current architecture.
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
