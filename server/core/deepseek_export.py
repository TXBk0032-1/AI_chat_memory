import json
import zipfile
from io import BytesIO
from typing import Any


class DeepSeekExportError(ValueError):
    pass


def load_deepseek_export_conversations(zip_bytes: bytes) -> list[dict[str, Any]]:
    if not zip_bytes:
        raise DeepSeekExportError("ZIP 文件为空")

    try:
        with zipfile.ZipFile(BytesIO(zip_bytes)) as archive:
            if "conversations.json" not in archive.namelist():
                raise DeepSeekExportError("ZIP 中缺少 conversations.json")
            try:
                conversations = json.loads(archive.read("conversations.json").decode("utf-8"))
            except UnicodeDecodeError as exc:
                raise DeepSeekExportError(f"conversations.json 不是 UTF-8: {exc}") from exc
            except json.JSONDecodeError as exc:
                raise DeepSeekExportError(f"conversations.json 不是有效 JSON: {exc}") from exc
    except zipfile.BadZipFile as exc:
        raise DeepSeekExportError("不是有效 ZIP 文件") from exc

    if not isinstance(conversations, list):
        raise DeepSeekExportError("conversations.json 须为会话数组")

    for index, conversation in enumerate(conversations):
        if not isinstance(conversation, dict):
            raise DeepSeekExportError(f"第 {index + 1} 个会话不是对象")
        missing = [key for key in ("id", "title", "inserted_at", "updated_at", "mapping") if key not in conversation]
        if missing:
            raise DeepSeekExportError(f"第 {index + 1} 个会话缺少字段: {', '.join(missing)}")
        if not isinstance(conversation.get("mapping"), dict):
            raise DeepSeekExportError(f"第 {index + 1} 个会话 mapping 不是对象")

    return conversations
