from datetime import datetime
from typing import Any, Literal, Optional
from uuid import uuid4
from pydantic import BaseModel, Field

class Message(BaseModel):
    id: str = Field(default_factory=lambda: str(uuid4()))
    session_id: str = ""
    role: Literal['user', 'assistant', 'system', 'tool']
    content: str
    metadata: dict[str, Any] = Field(default_factory=dict)
    created_at: Optional[str] = None
    seq: int = 0

class Session(BaseModel):
    id: str = Field(default_factory=lambda: str(uuid4()))
    platform: str
    platform_session_id: str
    title: str = ""
    messages: list[Message] = Field(default_factory=list)
    created_at: Optional[str] = None
    updated_at: Optional[str] = None
    imported_at: str = Field(default_factory=lambda: datetime.now().isoformat())
    raw_data: Optional[dict] = None

class ImportRequest(BaseModel):
    platform: str
    sessions: list[dict]

class ImportResponse(BaseModel):
    imported: int
    skipped: int
