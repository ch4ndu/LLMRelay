# Third-party notices

LLMRelay uses the pinned Rust crates in `Cargo.lock` and the pinned dashboard packages in `frontend/deno.lock`. The packaged binaries and dashboard are built locally from those locks.

The TRIP Explorer skills, references, and optional helper scripts bundled under `resources/trip-explorer/0.9.0/` come from the selected local `trip-explorer-workflow` v0.9.0 checkout. That checkout contained local changes, so the bundled content hashes identify the snapshot; the upstream Git commit alone does not. These resources are preserved verbatim. LLMRelay's transport and enforcement overlay is maintained separately in `resources/prompts/trip-overlay.md` and `resources/workflows/trip-explorer-0.9.0-llmrelay-1.json`. The selected source checkout did not include a license file; this notice does not assign a license to its contents.

The direct Rust dependency `toml` 0.8.23 is dual-licensed under MIT or Apache-2.0. The additional resolved crates are equivalent 1.0.2, hashbrown 0.17.1, indexmap 2.14.2, serde_spanned 0.6.9, toml_datetime 0.6.11, toml_edit 0.22.27, toml_write 0.1.2, and winnow 0.7.15. These use MIT or Apache-2.0 licensing, except winnow, which uses MIT. The exact inventory is recorded in `Cargo.lock`.

The direct dashboard dependencies are React 19.1.1 and React DOM 19.1.1. Direct build and test dependencies are TypeScript 5.9.2, Vite 7.1.5, the Vite React plugin 5.0.2, React type declarations 19.1.12 and 19.1.9, and happy-dom 18.0.1.

The exact resolved frontend inventory and its license metadata are recorded in `frontend/deno.lock`. Before distributing a package outside this machine, regenerate the dependency inventory from the exact lockfiles and include the corresponding license texts. `scripts/package.sh` includes this notice but does not publish, install, sign, notarize, or upload anything.
