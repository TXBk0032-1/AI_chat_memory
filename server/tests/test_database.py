import tempfile
import unittest
import json
import zipfile
from io import BytesIO
from pathlib import Path
from unittest.mock import patch

from models import ImportRequest
from api.sessions import import_deepseek_export, import_sessions, search_sessions
from starlette.requests import Request
from core import database


class DatabaseTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.data_dir = Path(self.temp_dir.name)
        self.db_path = self.data_dir / "test.db"
        self.data_dir_patch = patch.object(database, "DATA_DIR", self.data_dir)
        self.db_path_patch = patch.object(database, "DB_PATH", self.db_path)
        self.data_dir_patch.start()
        self.db_path_patch.start()
        await database.init_db()

    async def asyncTearDown(self) -> None:
        self.db_path_patch.stop()
        self.data_dir_patch.stop()
        self.temp_dir.cleanup()

    async def test_get_db_closes_connection(self) -> None:
        async with database.get_db() as db:
            await db.execute("SELECT 1")

        with self.assertRaises(ValueError):
            await db.execute("SELECT 1")

    async def test_reimport_preserves_session_id_and_replaces_messages(self) -> None:
        first = {
            "id": "platform-session",
            "title": "First",
            "updated_at": "1",
            "_conversation": {
                "data": {"biz_data": {"chat_messages": [
                    {"role": "user", "fragments": [{"type": "REQUEST", "content": "old"}]}
                ]}}
            },
        }
        second = {
            **first,
            "title": "Second",
            "updated_at": "2",
            "_conversation": {
                "data": {"biz_data": {"chat_messages": [
                    {"role": "assistant", "fragments": [{"type": "RESPONSE", "content": "new"}]}
                ]}}
            },
        }

        await import_sessions(ImportRequest(platform="deepseek", sessions=[first]))
        async with database.get_db() as db:
            original_id = (await (await db.execute("SELECT id FROM sessions")).fetchone())[0]

        await import_sessions(ImportRequest(platform="deepseek", sessions=[second]))
        async with database.get_db() as db:
            session = await (await db.execute("SELECT id, title FROM sessions")).fetchone()
            messages = await (await db.execute("SELECT role, content FROM messages")).fetchall()

        self.assertEqual(session, (original_id, "Second"))
        self.assertEqual(messages, [("assistant", "new")])

    async def test_import_rolls_back_entire_batch(self) -> None:
        valid = {"id": "valid", "title": "Valid", "messages": []}
        invalid = {"id": "invalid", "title": "Invalid", "messages": [], "bad": {1, 2}}

        with self.assertRaises(TypeError):
            await import_sessions(ImportRequest(platform="custom", sessions=[valid, invalid]))

        async with database.get_db() as db:
            count = (await (await db.execute("SELECT COUNT(*) FROM sessions")).fetchone())[0]

        self.assertEqual(count, 0)

    async def test_search_treats_query_as_bound_value(self) -> None:
        raw = {"id": "searchable", "title": "Safe title", "messages": []}
        await import_sessions(ImportRequest(platform="custom", sessions=[raw]))

        result = await search_sessions(q="%' OR 1=1 --")

        self.assertEqual(result["sessions"], [])

    async def test_deepseek_zip_reimport_updates_existing_session(self) -> None:
        conversation = {
            "id": "export-session",
            "title": "Exported",
            "inserted_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:01:00Z",
            "mapping": {
                "node-1": {
                    "id": "node-1",
                    "message": {
                        "inserted_at": "2026-01-01T00:00:00Z",
                        "fragments": [{"type": "REQUEST", "content": "hello"}],
                    },
                }
            },
        }
        body = BytesIO()
        with zipfile.ZipFile(body, "w") as archive:
            archive.writestr("conversations.json", json.dumps([conversation]))
        payload = body.getvalue()

        async def receive() -> dict[str, object]:
            return {"type": "http.request", "body": payload, "more_body": False}

        request = Request({"type": "http", "method": "POST", "path": "/"}, receive)
        await import_deepseek_export(request)
        request = Request({"type": "http", "method": "POST", "path": "/"}, receive)
        await import_deepseek_export(request)

        async with database.get_db() as db:
            session_count = (await (await db.execute("SELECT COUNT(*) FROM sessions")).fetchone())[0]
            message_count = (await (await db.execute("SELECT COUNT(*) FROM messages")).fetchone())[0]

        self.assertEqual((session_count, message_count), (1, 1))


if __name__ == "__main__":
    unittest.main()
