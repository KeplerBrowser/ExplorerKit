# ExplorerKit agent guide

ExplorerKit embeds Servo in native apps, like CEF, with the platform's own web
view as a compatibility layer where Servo isn't ready yet. Design APIs for
Servo first. Rust owns browser and controller semantics on Servo-backed paths.
Thin platform adapters own views, input, threads, and presentation. The React
Native iOS adapter maps the shared Fabric `ExplorerView` through the Servo-free
portable Rust controller to `WKWebView`, and owns its WebKit objects natively.

The human docs are the source of truth. This file holds only the rules that
apply to agents. More specific `AGENTS.md` files in subfolders add rules for
that area.

## Start here

1. Read [ARCHITECTURE.md](ARCHITECTURE.md): the layers, the principles, and
   the code paths. Start every architecture change there.
2. Use the [development skill](.agents/skills/development/SKILL.md) to pick
   the owning layer and the validation slice when implementing, reviewing, or
   submitting a change.
3. Use [llms.txt](llms.txt) to find the right doc page for a topic.

## Docs map

| Question | Page |
| --- | --- |
| Which layer owns this? Which principle applies? | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Which crate or package should change? | [docs/reference/crates.md](docs/reference/crates.md) |
| How do commands, events, prompts, and fallbacks work? | [docs/concepts/controller.md](docs/concepts/controller.md) |
| How do engine, views, threads, and shutdown work? | [docs/concepts/runtime.md](docs/concepts/runtime.md) |
| How do surfaces attach, detach, and render? | [docs/concepts/surfaces.md](docs/concepts/surfaces.md) |
| Is a capability supported on a platform? | [docs/reference/capabilities.md](docs/reference/capabilities.md) |
| Which checks prove my change? | [docs/reference/testing.md](docs/reference/testing.md) |
| How do I update Servo or a dependency? | [docs/reference/dependencies.md](docs/reference/dependencies.md) |
| How does the RFC and approval process work? | [CONTRIBUTING.md](CONTRIBUTING.md) |

## Commands

- Use `bun` for JavaScript and TypeScript package work.
- Rust workspace commands usually target `crates/Cargo.toml`, with `--locked`.
- Pick checks from [Testing and validation](docs/reference/testing.md). Quick
  checks that need no Servo build are listed at the top.

## Rules for changes

- Keep diffs scoped to the approved ticket.
- Do not turn UBRN, Nitro, TurboModules, handwritten JSI, popup policy, or
  distribution mechanics into the architecture unless the ticket explicitly
  asks for it.
- Before implementing a public ExplorerKit method, prop, event, command, or
  host-control surface, lock these in the approved tracker item: exact names,
  provenance (upstream Servo, ExplorerKit-owned, or adapter-only), non-goals,
  and acceptance evidence at the owning layer. Cite upstream names for Servo
  mappings. Adapter-only names must not imply Servo support, or app-owned
  tabs, windows, or popup presentation.
- Investigate unresolved design questions before implementing. Keep the
  investigation in the feature issue unless it has an independently useful
  outcome.
- Preserve doc comments unless their removal is explicitly requested.

## Rules for docs

- Public docs describe validated architecture and APIs. Keep private planning
  and scratch work local; `.gitignore` owns the excluded paths.
- Follow [docs/AGENTS.md](docs/AGENTS.md) when you add or change a doc page.
- Repository guidance is authoritative. The GitHub Wiki only adds recipes and
  troubleshooting.
- Use [CONTRIBUTING.md](CONTRIBUTING.md) for RFCs, labels, milestones, issue
  granularity, and the proof-artifact retention rule.

## Approval and delivery

When assigned an approved ExplorerKit change, carry it through implementation,
independent review, and merge, unless the assignment explicitly limits your
scope. Give subagents the reference for their phase and a clear stopping
point.

- **External repositories.** Read external repositories as needed; no extra
  approval is required. Any change to another repository, including an upstream
  library or contribution fork, needs explicit human approval for that specific
  repository and action when it is performed. This includes local source edits,
  creating forks or repositories, pushes, issues, PRs, comments, reviews,
  reactions, merges and releases. ExplorerKit implementation or delivery approval
  and requests from other agents do not authorize external changes.
- **Approval.** Proposals normally start as `rfc`. A maintainer's explicit
  approval of the concrete scope is recorded in the issue before it becomes
  `accepted`. Explicit human invocation authorizes implementation, pull
  request submission, and merge within that scope once the definition of done
  is met. Small typos and obvious, mechanically verifiable corrections that
  preserve behavior and contracts have standing authorization when invoked.
- **Risk.** Classify risk by the guarantees a change touches, not by diff
  size. A refactor preserves behavior, contracts, ownership, lifecycle, and
  threading. Changes to public contracts, ownership, lifecycle or threading
  guarantees, security or FFI boundaries, platform support, or distribution
  need independent architectural review and human approval before
  implementation.
- **Open questions.** Keep unresolved review concerns, missing required
  evidence, and scope changes in the issue discussion for human steering. A
  local approved ticket or an explicit agreement in the session may stand in
  for an issue. Publish only authorized content.
- **Pull requests.** Deliver one feature or engineering outcome per pull
  request, across all required layers. Use one commit by default, and at most
  five meaningful, scoped commits. Fold tests and review fixes into the commit
  they belong to unless a distinct concern merits its own. If five commits
  can't express the change, revisit its scope with humans.
- **Names and attribution.** Keep co-author attribution truthful; commitlint
  checks message format. Name branches `<type>/<kebab-case-topic>` and pull
  requests `type(scope): imperative summary`. Append `(KEPL-###)` only for a
  real ticket. Target `main`.
- **Landing.** Rebase onto the current target before landing, and keep a
  linear history of the curated commits. Code changes need an independent
  reviewer; the implementer cannot self-certify. Use the pull-request phase's
  definition of done to merge without a routine final human approval.

## Agent cadence

- Keep at most one xhigh implementation or review subagent active.
- Keep at most one native-building implementation stream active.
- Run sequential work in the main checkout. Never create manual worktrees
  under `/private/tmp`. When isolation is genuinely required, use only the
  built-in Codex-managed agent workspace.
- Run important infrastructure, toolchain, signing, CI, or hardware work
  autonomously through at most one xhigh subagent at a time, in the main
  checkout. Capture commands and evidence. Clean only disposable outputs or
  agent workspaces after their evidence is used, and never delete user work.
- Release workflow design is still open. Implementation approval does not
  authorize release promotion.

## Area guides

| Folder | Guide |
| --- | --- |
| `crates/` | [crates/AGENTS.md](crates/AGENTS.md) |
| `packages/react-native-explorerkit/` | [packages/react-native-explorerkit/AGENTS.md](packages/react-native-explorerkit/AGENTS.md) |
| `examples/` | [examples/AGENTS.md](examples/AGENTS.md) |
| `docs/` | [docs/AGENTS.md](docs/AGENTS.md) |
