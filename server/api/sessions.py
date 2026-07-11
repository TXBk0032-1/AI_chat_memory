import json
import logging
from typing import Any

from fastapi import APIRouter, HTTPException, Request
from models import ImportRequest, ImportResponse
from core.database import get_db, insert_session, insert_messages
from core.deepseek_export import DeepSeekExportError, load_deepseek_export_conversations
from core.normalizer import normalize_deepseek_export_session, normalize_session

router = APIRouter()
logger = logging.getLogger(__name__)

@router.post("/import", response_model=ImportResponse)
async def import_sessions(req: ImportRequest) -> ImportResponse:
    imported = 0
    async with get_db() as db:
        try:
            for raw in req.sessions:
                normalized = normalize_session(req.platform, raw, raw.get('_conversation'))
                session_id = await insert_session(db, normalized)
                await insert_messages(db, session_id, normalized.get('messages', []))
                imported += 1
            await db.commit()
        except Exception:
            await db.rollback()
            logger.exception("Session import failed; rolled back batch")
            raise
    return ImportResponse(imported=imported, skipped=0)

@router.post("/import/deepseek-export", response_model=ImportResponse)
async def import_deepseek_export(request: Request) -> ImportResponse:
    try:
        conversations = load_deepseek_export_conversations(await request.body())
    except DeepSeekExportError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    imported = 0
    async with get_db() as db:
        try:
            for raw in conversations:
                normalized = normalize_deepseek_export_session(raw)
                session_id = await insert_session(db, normalized)
                await insert_messages(db, session_id, normalized.get('messages', []))
                imported += 1
            await db.commit()
        except ValueError as exc:
            await db.rollback()
            raise HTTPException(status_code=400, detail=str(exc)) from exc
        except Exception:
            await db.rollback()
            logger.exception("DeepSeek export import failed; rolled back batch")
            raise
    return ImportResponse(imported=imported, skipped=0)

@router.get("")
async def list_sessions(platform: str | None = None, limit: int = 100, offset: int = 0) -> dict[str, Any]:
    async with get_db() as db:
        db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
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

@router.get("/search")
async def search_sessions(q: str = "", platform: str | None = None, date_from: str | None = None, date_to: str | None = None, limit: int = 100, offset: int = 0) -> dict[str, Any]:
    async with get_db() as db:
        db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
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
        # Only fixed condition fragments are interpolated; all user values remain bound parameters.
        where = ("WHERE " + " AND ".join(conditions)) if conditions else ""
        sql = f"SELECT s.id, s.platform, s.platform_session_id, s.title, s.created_at, s.updated_at, s.imported_at FROM sessions s {where} ORDER BY s.updated_at DESC LIMIT ? OFFSET ?"
        params.extend([limit, offset])
        cursor = await db.execute(sql, params)
        rows = await cursor.fetchall()
        return {"sessions": rows, "total": len(rows)}

@router.get("/sync-status")
async def sync_status(platform: str) -> dict[str, str | None]:
    async with get_db() as db:
        db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
        cursor = await db.execute(
            "SELECT MAX(updated_at) as last_updated_at FROM sessions WHERE platform = ?",
            (platform,))
        row = await cursor.fetchone()
        return {"last_updated_at": row["last_updated_at"] if row else None}

@router.get("/{session_id}")
async def get_session(session_id: str) -> dict[str, Any]:
    async with get_db() as db:
        db.row_factory = lambda c, r: dict(zip([col[0] for col in c.description], r))
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

@router.delete("/{session_id}")
async def delete_session(session_id: str) -> dict[str, str]:
    async with get_db() as db:
        try:
            await db.execute("DELETE FROM messages WHERE session_id = ?", (session_id,))
            await db.execute("DELETE FROM sessions WHERE id = ?", (session_id,))
            await db.commit()
        except Exception:
            await db.rollback()
            logger.exception("Session deletion failed; rolled back transaction")
            raise
    return {"deleted": session_id}
