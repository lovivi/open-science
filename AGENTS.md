# Open Science (desktop)

Brand name: **Open Science** — "An open AI workbench for scientists. Your research
partner for rigorous science." (Bundle identifier stays `com.ai4s.workbench` and
internal `@ai4s/*` package names are unchanged — display branding only.)

Project rules and working context for AI agents (Claude Code, Cursor, Codex, etc.).
`CLAUDE.md` is a symlink to this file — edit only `AGENTS.md`.

## Design principles

Keep it **simple, explicit, clear, complete**.

- **Simple** — no over-engineering; if not necessary, do not add entities.
- **Explicit** — no ambiguity; no bugs.
- **Clear** — understandable at a glance.
- **Complete** — cover the key points; prioritize safety.

## Common commands

```bash
# Prerequisites: Node >=20, pnpm 9, Rust toolchain, macOS or Windows
pnpm install                           # install all workspace dependencies

# Development
pnpm dev                               # start Vite dev server (apps/desktop/)
pnpm --filter @ai4s/desktop tauri dev  # launch Tauri desktop app with hot reload
pnpm --filter @ai4s/desktop tauri build # build installer (.dmg / .msi)

# Code quality
pnpm test                              # Vitest (all packages, jsdom env, globals)
pnpm test:watch                        # Vitest watch mode
pnpm typecheck                         # tsc --noEmit (strict mode)
pnpm lint                              # ESLint (TypeScript + React hooks)

# Before first dev — fetch bundled sidecars (kept out of git):
bash scripts/dev/fetch-opencode.sh     # OpenCode agent runtime binary
bash scripts/dev/fetch-uv.sh           # uv for isolated Python/Jupyter
bash scripts/dev/fetch-skills.sh       # ai4s-skills pack

# Run a single test file
pnpm --filter @ai4s/desktop vitest run src/lib/artifacts.test.ts
```

## What this project is

An open-source, local-first, model-agnostic, reproducible AI research workbench
for macOS and Windows. See `README.md`, `docs/PRD.md`, and `docs/TECHNICAL_DESIGN.md`.

Recommended stack: **Tauri 2 + React + TypeScript + Vite**, Tailwind + Radix UI,
**OpenCode** as the agent runtime (bundled single-binary sidecar; HTTP + SSE API),
local workspace + SQLite + JSONL provenance.

## Frontend architecture

### `apps/desktop/src/` — React frontend

```
src/
├── app/               # Route shells, providers, layout
│   ├── router.tsx     # React Router v6 routes
│   ├── layout/        # AppShell — sidebar + thread + inspector layout
│   └── providers/     # ThemeProvider (dark/light)
├── components/        # Presentational UI
│   ├── ui/            # Primitives (button, input, dialog, etc.)
│   ├── sidebar/       # Session list, file explorer
│   ├── thread/        # Chat thread, message bubbles
│   ├── inspector/     # Right-panel artifact viewer (ArtifactInspector)
│   ├── tool-call-card/# Tool call UI (write, run, etc.)
│   ├── command-palette/ # ⌘K command palette
│   ├── artifact-viewer/ # Renders figures, tables, PDFs, notebooks, etc.
│   └── ...
├── features/          # Domain-specific state & logic
│   ├── chat/          # Conversation state (Zustand store)
│   ├── artifacts/     # Artifact derivation & history
│   ├── provenance/    # provenance.jsonl tracking
│   ├── review/        # Traceability reviewer (citation audit)
│   ├── agent-runtime/ # OpenCode lifecycle management
│   ├── projects/      # Workspace project handling
│   ├── settings/      # Settings pages (model provider, keychain)
│   ├── skills/        # Skills library UI
│   ├── literature/    # Literature search & survey
│   └── onboarding/    # First-launch walkthrough
├── lib/               # Pure logic: no React, testable
│   ├── store.ts       # Zustand stores (session, artifacts, settings)
│   ├── artifacts.ts   # Artifact derivation from tool calls
│   ├── provenance.ts  # Provenance log CRUD
│   ├── review.ts      # Citation & number traceability checks
│   ├── runtime.ts     # Agent runtime lifecycle
│   ├── kernel.ts      # Jupyter kernel state
│   ├── csv.ts         # CSV/table helpers
│   ├── genome.ts      # Genome viewer parsing
│   ├── molecule.ts    # Molecular structure helpers
│   └── chartPalette.ts # Unified chart color design system
└── test/              # Test utilities
    ├── setup.ts       # Vitest setup (jsdom, testing-library matchers)
    ├── render.tsx     # Custom render wrapper
    └── opencode-client.node.test.ts # Node-level SDK integration tests
```

### Key patterns

- **SDK isolation layer** — `packages/sdk/` wraps OpenCode's HTTP+SSE API as `OpenCodeClient`. Frontend code never calls OpenCode directly; it imports from `@ai4s/sdk`.
- **State management** — Zustand stores in `src/lib/store.ts`, split into slices (session, artifacts, settings, runtime). UI components subscribe to slices directly.
- **Path aliases** — `@/` → `src/`, `@ai4s/sdk`, `@ai4s/shared` (configured in both `tsconfig.json` and `vite.config.ts`).
- **Tests** — Vitest with `globals: true`, `environment: jsdom`, custom setup file. Test files live side-by-side with source (`*.test.ts`/`*.test.tsx`). Main test utilities: `render.tsx` (wrapped provider), `setup.ts` (jest-dom matchers).
- **Artifact provenance** — Every agent `write` tool call is tracked in `.openscience/provenance.jsonl`; the `provenance.ts` lib manages CRUD, and `ArtifactInspector` shows version history.
- **Chart design system** — `chartPalette.ts` exports a shared palette validated for light/dark; used by both native UI charts and agent-generated matplotlib figures. See `packages/shared/` for the full spec.

## Repository map

- `apps/desktop/` — Tauri + React desktop shell (`src/` frontend, `src-tauri/` Rust).
- `packages/` — `ui`, `shared`, `sdk` (the `OpenCodeClient` wrapper).
- `runtime/` — `manager`, `opencode-profile`, `mcp`, `skills`.
- `docs/` — product and technical specs.
- `examples/bci-trends/` — the built-in demo project.
- `scripts/` — release and dev scripts.

## Architecture guardrails

- The UI never calls OpenCode directly — it goes through `packages/sdk` (`OpenCodeClient`).
  Pin the OpenCode version (see `OPENCODE_VERSION`) and bundle it as a sidecar.
- Keep the frontend, desktop shell, and agent runtime decoupled.
- Skills, MCP servers, and model providers must stay pluggable.
- Keep the artifact schema and workflow templates stable and versioned.

## Safety defaults (non-negotiable for the desktop)

- The agent may only access the current workspace.
- Command execution, file deletion, dependency install, and remote connections
  require approval (manual approval mode by default — never ship `off`).
- API keys go to the OS keychain / credential manager; never into provenance,
  logs, crash reports, git, or exported projects.

## Working conventions

- Default working language for discussion is Chinese; **all project files and
  code are in English** (this is a pure-English project).
- One progress file: `PROGRESS.md`. Append one line per real milestone,
  `YYYY-MM-DD HH:MM` + a one-sentence conclusion, newest on top. Results and
  blockers only.
- Avoid adding new Markdown docs unless requested — too many docs become debt.
- Prefer minimal, verifiable changes; every step should produce a checkable result.
- Do not write inferences as verified facts; tie conclusions to code or data.
