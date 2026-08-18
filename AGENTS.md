# Repository Agent Instructions

## Repository Shape

- This is one Tauri 2 desktop app, not a workspace. Run npm commands from `app/` and Cargo commands from `app/src-tauri/`.
- `app/src/main.ts` mounts the Vue app; `app/src/desktop-api.ts` is the typed Tauri command boundary; `app/src-tauri/src/lib.rs` wires commands, storage, tray behavior, and the Axum service.
- The local userscript API is fixed at `127.0.0.1:19820`; routes and authorization live in `app/src-tauri/src/http_api.rs`.
- `userscript/dist/ai-chat-memory.user.js` is the committed userscript implementation, despite the `dist` name. Validate it directly after edits; there is no userscript build config.
- The SQLite schema is created in Rust in `app/src-tauri/src/database/connection.rs`; there are no migration files. `legacy/` is reference code, not the active application.
- `app/src-tauri/src/service/tests/` contains dedicated test modules for cloud transition, encryption, and archive import.
- Export pipeline supports PNG, JPEG, PDF, Markdown, and JSON. PDF export uses Windows WebView2 `ICoreWebView2_10::PrintToPdf` via `commands::print_to_pdf` and `@media print` isolation in `ExportDocument.vue`.

## Commands

- Install exact frontend dependencies: `cd app; npm ci`.
- Run the desktop app: `cd app; npm run tauri dev`. Vite must bind port `1420`; the local API must bind port `19820`.
- Frontend test: `cd app; npm test`. One file: `npm test -- src/conversation.test.ts`. One case: `npm test -- src/conversation.test.ts -t "case name"`.
- Rust test: `cd app/src-tauri; cargo test --all-features`. One test: `cargo test --all-features test_name`.
- Repository checks: `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\ci.ps1 check`; add all frontend and Rust tests with stage `test`; stage `release` also builds and copies the Windows MSI/EXE and writes `artifacts/manifest.json`.
- `scripts/ci.ps1` is the executable source of truth for verification order: userscript syntax, Rust format, Clippy with warnings denied, frontend typecheck/build, then frontend and Rust tests.

## Build Constraints

- The toolchain is pinned to Rust `1.97.0` and Node `22` in repository config/CI.
- Every `ci.ps1` stage initializes CUDA and requires the repository machine layout: CUDA 13.3 at `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.3` and Visual Studio 2022 Community MSVC `14.44.35207`. A missing or differently installed toolkit fails even frontend-only pipeline stages.
- `app/src-tauri/Cargo.toml` patches `glib` to `app/src-tauri/vendor/glib`; keep the vendored patch intact when changing dependencies.
- `app/dist/` and `artifacts/` are generated. Do not commit release output.

## Completion Contract

For every task that changes tracked files:

1. Run focused checks appropriate to the change.
2. Create atomic commits with Chinese commit messages. The completion hook rejects any dirty worktree, including untracked files.
3. From the repository root run `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\finish-task.ps1`. It runs the full release pipeline, so do not report completion unless it succeeds.
4. Report the generated MSI, EXE, and `artifacts/manifest.json` paths.

If `ai-chat-memory-desktop.exe` is running, ask the user to close it or close only a development instance you started before rerunning the hook. Never terminate a user-started instance without notice.
