import aiosqlite
import json
from config import DB_PATH, DATA_DIR

async def init_db():
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    async with aiosqlite.connect(DB_PATH) as db:
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

async def get_db():
    return await aiosqlite.connect(DB_PATH)

async def insert_session(db, session: dict) -> bool:
    try:
        await db.execute(
            "INSERT OR REPLACE INTO sessions (id, platform, platform_session_id, title, created_at, updated_at, imported_at, raw_data) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (session['id'], session['platform'], session['platform_session_id'], session.get('title', ''),
             session.get('created_at'), session.get('updated_at'), session.get('imported_at'),
             json.dumps(session.get('raw_data'), ensure_ascii=False) if session.get('raw_data') else None)
        )
        return True
    except Exception as e:
        print(f"Insert session error: {e}")
        return False

async def insert_messages(db, session_id: str, messages: list):
    await db.execute("DELETE FROM messages WHERE session_id = ?", (session_id,))
    for i, m in enumerate(messages):
        await db.execute(
            "INSERT OR REPLACE INTO messages (id, session_id, role, content, metadata, created_at, seq) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (f"{session_id}_{i}", session_id, m['role'], m.get('content', ''),
             json.dumps(m.get('metadata', {}), ensure_ascii=False), m.get('created_at'), i)
        )
