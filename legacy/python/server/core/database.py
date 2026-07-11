import aiosqlite
import json
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Any

from config import DB_PATH, DATA_DIR

async def init_db() -> None:
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    async with aiosqlite.connect(DB_PATH) as db:
        await db.execute("PRAGMA foreign_keys = ON")
        await db.executescript("""
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                platform_session_id TEXT NOT NULL,
                title TEXT,
                created_at TEXT,
                updated_at TEXT,
                imported_at TEXT DEFAULT CURRENT_TIMESTAMP,
                raw_data TEXT,
                UNIQUE(platform, platform_session_id)
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT,
                metadata TEXT,
                created_at TEXT,
                seq INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
        """)
        await db.commit()

@asynccontextmanager
async def get_db() -> AsyncIterator[aiosqlite.Connection]:
    db = await aiosqlite.connect(DB_PATH)
    try:
        await db.execute("PRAGMA foreign_keys = ON")
        yield db
    finally:
        await db.close()

async def insert_session(db: aiosqlite.Connection, session: dict[str, Any]) -> str:
    raw_data = json.dumps(session.get('raw_data'), ensure_ascii=False) if session.get('raw_data') else None
    cursor = await db.execute(
        "SELECT id FROM sessions WHERE platform = ? AND platform_session_id = ?",
        (session['platform'], session['platform_session_id'])
    )
    existing = await cursor.fetchone()

    if existing:
        session_id = existing[0]
        await db.execute(
            """
            UPDATE sessions
            SET title = ?, created_at = ?, updated_at = ?, imported_at = ?, raw_data = ?
            WHERE id = ?
            """,
            (session.get('title', ''), session.get('created_at'), session.get('updated_at'),
             session.get('imported_at'), raw_data, session_id)
        )
        return session_id

    await db.execute(
        """
        INSERT INTO sessions
        (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (session['id'], session['platform'], session['platform_session_id'], session.get('title', ''),
         session.get('created_at'), session.get('updated_at'), session.get('imported_at'), raw_data)
    )
    return session['id']

async def insert_messages(
    db: aiosqlite.Connection,
    session_id: str,
    messages: list[dict[str, Any]],
) -> None:
    await db.execute("DELETE FROM messages WHERE session_id = ?", (session_id,))
    for i, m in enumerate(messages):
        await db.execute(
            "INSERT OR REPLACE INTO messages (id, session_id, role, content, metadata, created_at, seq) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (f"{session_id}_{i}", session_id, m['role'], m.get('content', ''),
             json.dumps(m.get('metadata', {}), ensure_ascii=False), m.get('created_at'), i)
        )
