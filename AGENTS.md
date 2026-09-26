# LLMRelay Project Instructions

LLMRelay is a local Rust workflow host with a React/TypeScript dashboard and SQLite state.
Read the current implementation and only the guides relevant to the task.

## Contract Routing

| Area | Guide |
| --- | --- |
| Scope, completion verification, Git identity/authority, completion-driven waits | [Engineering workflow](docs/ENGINEERING_WORKFLOW.md) |
| Rust implementation/tests, general code quality, TypeScript rules, deslop | [Coding standards](docs/CODING_STANDARDS.md) |
| Product architecture and design decisions | [Research and plan](RESEARCH_AND_PLAN.md) |
| Application TRIP workflow, roles and review gates | [Workflows](docs/WORKFLOWS.md) |
| Project initialization and configuration | [Project setup](docs/PROJECT_SETUP.md) |
| Sessions, recovery, startup and diagnostics | [Operations](docs/OPERATIONS.md) |
| Authentication, permissions and isolation | [Security](docs/SECURITY.md) |
| Build, test commands and local packaging | [Building](docs/BUILDING.md) |
| Roadmap and current milestone plan | [Milestones](docs/MILESTONES.md), [Next milestone](docs/NEXT_MILESTONE_PLAN.md) |

## Required Gates

All engineering tasks follow [Engineering workflow](docs/ENGINEERING_WORKFLOW.md)
and the applicable [Coding standards](docs/CODING_STANDARDS.md).
Completion requires verifying the actual requested behavior, separately from build success.
When TRIP Explorer is invoked, follow the [installed skill](.agents/skills/trip-explorer-workflow/SKILL.md)
and its configured roles, approval boundaries and review gates.
