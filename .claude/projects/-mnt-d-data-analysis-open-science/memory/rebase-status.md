---
name: rebase-status
description: feat/wsl-backend branch rebase onto force-pushed master
metadata:
  type: project
---

Branch `feat/wsl-backend` needs rebase onto `origin/master` (force-pushed from b17b0cf → 9fc9560).

## Our branch (new files upstream doesn't have)
- wsl.rs (524 lines) — WSL detection, path mapping, network probing
- remote.rs (461 lines) — SSH tunnel, remote file ops, host validation  
- i18n.ts + en.json + zh.json — Internationalization infrastructure
- WslBackendCard.tsx + backend-api.ts — Settings UI
- wsl-win-cross.md — memory note

## Upstream master (new files we don't have)
- 6 scientific viewers (AnomalyMap, Band, Dos, Fits, Phase, QCode, Mesh, TableChart)
- 4 skills with Python backends (domain-check, large-file, modal-run, stats-integrity)
- large_file.rs, modal.rs — Rust backend
- ModalCard.tsx, WorkspaceChip.tsx, scrollMemory.ts — UI components
- Rewritten Composer.tsx (file attachments), runtime.ts (Zustand store)
- Linux CI + Release automation
- 30+ new test files

## Overlapping files (need merge)
- runtime.rs — both rewrote sidecar management
- kernel.rs / jupyter.rs — both added features
- artifact_file.rs — both rewrote
- Composer.tsx / runtime.ts — upstream rewrote, we did i18n
- SettingsPage.tsx — upstream added features, we added backend selector + i18n
- ~30 TSX files — upstream minor changes, we did i18n t() replacement
