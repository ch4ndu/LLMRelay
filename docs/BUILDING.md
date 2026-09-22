# Building LLMRelay

[Back to README](../README.md)

These instructions are for contributors and anyone building a local package. To use an already-built package, follow [the README](../README.md#start-llmrelay); Rust and Deno are not runtime requirements. Run the commands below from the repository root unless a command changes directory.

## Requirements and verification

Requirements:

- macOS with Git.
- Rust and Cargo 1.86.0.
- Deno 2.9.5.
- Installed and authenticated Codex and/or Claude Code CLIs for the profiles you choose. Setup validates those selected profiles; an unrelated second CLI is not a requirement.

Clone or copy this repository, then fetch the exact locked dependencies and verify the integrated source:

```sh
cargo fetch --locked
cd frontend
deno install --frozen
cd ..
./scripts/verify.sh
```

`scripts/verify.sh` compiles the locked Rust application, runs the Rust causal cases, type-checks and tests the dashboard, and builds the frontend assets. `build.rs` registers the complete Vite output in the executable; the server serves only those registered paths with fixed MIME types, loopback origin checks, and a restrictive content-security policy. Verification does not launch a provider or alter global provider configuration.

To create a local ZIP after verification:

```sh
./scripts/package.sh
```

The archive is written beneath `.local/dist`. Packaging does not install, sign, notarize, publish, or upload it. See [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) before distributing a package outside this machine.

To run the package built in this checkout, without installing it or adding anything to PATH:

```sh
.local/dist/llmrelay-0.1.0-macos/bin/llmrelay serve --port 0
```

To use the ZIP elsewhere, extract the `llmrelay-0.1.0-macos` folder and run its primary `bin/llmrelay` executable from Terminal. The legacy `bin/agenticjira` executable remains included for existing invocations. The dashboard is embedded in each executable; Rust and Deno are needed only to build from source. The runtime still needs Git and the installed, authenticated provider CLIs described below. Keeping the extracted folder in a location you choose is sufficient. Adding its `bin` directory to PATH is optional; no login service is registered.

## Frontend development

The dashboard source is in `frontend/src`. From `frontend/`, the existing tasks are:

```sh
deno task typecheck
deno task test
deno task build
```

The DOM suite runs without launching provider sessions. `deno task build` produces `frontend/dist`; the Rust build embeds those assets, so rebuild the executable after changing the dashboard. `deno task dev` starts Vite for frontend development, but it is not a replacement for the authenticated Rust service or its session controls.

## Package contents

The package includes `bin/llmrelay`, the compatibility executable `bin/agenticjira`, and documentation under `share/llmrelay/`. The `docs/` subdirectory keeps the same relative layout as this source tree so the README links work after extraction. Packaging also includes the research plan and third-party notices. Local packaging does not grant release or publication authority.
