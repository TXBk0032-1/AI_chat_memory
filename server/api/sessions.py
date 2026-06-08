import json
from fastapi import APIRouter, HTTPException
from models import ImportRequest, ImportResponse, Session
from core.database import get_db, insert_session, insert_messages
from core.normalizer import normalize_session

router = APIRouter()

@router.post("/import", response_model=ImportResponse)
async def import_sessions(req: ImportRequest):
    imported, skipped = 0, 0
    db = await get_db()
    try:
        for raw in req.sessions:
            normalized = normalize_session(req.platform, raw, raw.get('_conversation'))
            if await insert_session(db, normalized):
                await insert_messages(db, normalized['id'], normalized.get('messages', []))
                imported += 1
            else:
                skipped += 1
        await db.commit()
    finally:
        await db.close()
    return ImportResponse(imported=imported, skipped=skipped)

@router.get("")
async def list_sessions(platform: str = None, limit: int = 100, offset: int = 0):
    db = await get_db()
    db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
    try:
        if platform:
            cursor = await db.execute(
                "SELECT id, platform, platform_session_id, title, created_at, updated_at, imported_at FROM sessions WHERE platform = ? ORDER BY updated_at DESC LIMIT ? OFFSET ?",
                (platform, limit, offset))
        else:
            cursor = await db.execute(
                "SELECT id, platform, platform_session_id, title, created_at, updated_at, imported_at FROM sessions ORDER BY updated_at DESC LIMIT ? OFFSET ?",
                (limit, offset))
        rows = await cursor.fetchall()
        return {"sessions": rows, "total": len(rows)}
    finally:
        await db.close()

@router.get("/search")
async def search_sessions(q: str = "", platform: str = None, date_from: str = None, date_to: str = None, limit: int = 100, offset: int = 0):
    db = await get_db()
    db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
    try:
        conditions, params = [], []
        if platform:
            conditions.append("s.platform = ?")
            params.append(platform)
        if date_from:
            conditions.append("s.updated_at >= ?")
            params.append(date_from)
        if date_to:
            conditions.append("s.updated_at <= ?")
            params.append(date_to)
        if q:
            conditions.append("(s.title LIKE ? OR EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id AND m.content LIKE ?))")
            params.extend([f"%{q}%", f"%{q}%"])
        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        sql = f"SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at FROM sessions s {where} ORDER BY s.updated_at DESC LIMIT ? OFFSET ?"
        params.extend([limit, offset])
        cursor = await db.execute(sql, params)
        rows = await cursor.fetchall()
        return {"sessions": rows, "total": len(rows)}
    finally:
        await db.close()

@router.get("/sync-status")
async def sync_status(platform: str):
    db = await get_db()
    db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
    try:
        cursor = await db.execute(
            "SELECT MAX(updated_at) as last_updated_at FROM sessions WHERE platform = ?",
            (platform,))
        row = await cursor.fetchone()
        return {"last_updated_at": row["last_updated_at"] if row else None}
    finally:
        await db.close()

@router.get("/{session_id}")
async def get_session(session_id: str):
    db = await get_db()
    db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
    try:
        cursor = await db.execute("SELECT * FROM sessions WHERE id = ?", (session_id,))
        session = await cursor.fetchone()
        if not session:
            raise HTTPException(status_code=404, detail="Session not found")
        cursor = await db.execute("SELECT * FROM messages WHERE session_id = ? ORDER BY seq", (session_id,))
        messages = await cursor.fetchall()
        for m in messages:
            if m.get('metadata'):
                m['metadata'] = json.loads(m['metadata'])
        session['messages'] = messages
        if session.get('raw_data'):
            session['raw_data'] = json.loads(session['raw_data'])
        return session
    finally:
        await db.close()

@router.delete("/{session_id}")
async def delete_session(session_id: str):
    db = await get_db()
    try:
        await db.execute("DELETE FROM messages WHERE session_id = ?", (session_id,))
        await db.execute("DELETE FROM sessions WHERE id = ?", (session_id,))
        await db.commit()
        return {"deleted": session_id}
    finally:
        await db.close()
