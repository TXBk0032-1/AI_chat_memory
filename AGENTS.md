# Repository Agent Instructions

## Task Completion Hook

For every task that changes tracked source files:

1. Run the checks appropriate for the change.
2. Create one or more atomic Git commits. Commit messages must be written in Chinese.
3. After the commits succeed, run the task completion hook from the repository root:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\finish-task.ps1
   ```

4. Do not report the task as complete unless the hook succeeds.
5. Report the generated MSI, EXE, and manifest paths in the final response.

The hook performs the full local release pipeline, including checks, tests, the Tauri release build, MSI packaging, and SHA-256 manifest generation. Build artifacts under `artifacts/` are generated output and must not be committed.

If `ai-chat-memory-desktop.exe` is running, ask the user to close it or close a development instance started during the task, then rerun the hook. Do not terminate a user-started application without notice.
