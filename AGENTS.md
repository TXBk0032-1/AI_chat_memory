# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project Overview

Cross-platform AI chat history management system (跨平台 AI 聊天记录管理系统). Captures conversations from web AI platforms via a Tampermonkey userscript, stores them in a local SQLite database, and provides a web UI ("藏经阁") for browsing and searching. The project is primarily in Chinese.

## Architecture

Two main components:

1. **server/** — Python FastAPI backend (port 19820)
   - `main.py` — App entry, CORS, router mounting, static file serving
   - `config.py` — HOST (127.0.0.1), PORT (19820), DATA_DIR, DB_PATH
   - `api/sessions.py` — Session CRUD, import, search, sync-status endpoints
   - `api/health.py` — Health check
   - `core/database.py` — aiosqlite init, session/message insert helpers
   - `core/normalizer.py` — Platform-specific raw data → unified format; dispatcher `normalize_session()` routes by platform string
   - `models/__init__.py` — Pydantic models (Message, Session, ImportRequest, ImportResponse)
   - `static/index.html` — Single-file web UI with marked.js + KaTeX

2. **userscript/dist/ai-chat-memory.user.js** — Single bundled Tampermonkey userscript
   - Platform adapters (DeepSeek, Doubao, Kimi) each with `fetchAllSessions()` / `fetchConversation()` / `getToken()`
   - DeepSeek/Kimi: XHR/fetch monkey-patching to capture Bearer tokens
   - Doubao: Cookie-based (no token capture needed)
   - UI panel injected into target sites with sync controls
   - Incremental sync for DeepSeek (uses `updated_at` cursor); others always full-fetch

## Running the Server

```bash
cd server
source venv/bin/activate   # or: python -m venv venv && pip install -r requirements.txt
python main.py
```

Server runs at `http://localhost:19820`. No separate build/lint/test commands configured.

## Key Design Decisions

- **No ORM**: Raw aiosqlite SQL with manual `get_db()` / `db.close()` per request (no dependency injection)
- **Sessions dedup**: `UNIQUE(platform, platform_session_id)` with `INSERT OR REPLACE`
- **Messages**: Deleted and re-inserted on every import for a session (no incremental message updates)
- **Normalizer pattern**: Each platform has a `normalize_<platform>_session()` function; `normalize_session()` dispatches by platform string and extracts session metadata differently per platform
- **Search**: Simple LIKE queries across session titles and message content; no full-text search index
- **Userscript**: Monolithic single file, runs at `document-start`, uses `unsafeWindow` for token interception

## Adding a New Platform

1. Server side: Add `normalize_<platform>_session()` in `core/normalizer.py`, add a branch in `normalize_session()`
2. Userscript side: Add adapter class extending `BaseAdapter`, add to the platform detection `PLATFORM` const and adapter instantiation

## API Prefix

All API routes are under `/api/v1`. Sessions routes under `/api/v1/sessions`.
