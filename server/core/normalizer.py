from uuid import uuid4
from datetime import datetime, timezone

def _iso_to_ts(iso_str):
    """ISO 8601 → Unix 时间戳字符串，失败返回原值"""
    if not iso_str:
        return iso_str
    try:
        dt = datetime.fromisoformat(iso_str.replace('Z', '+00:00'))
        return str(dt.timestamp())
    except Exception:
        return iso_str

def normalize_deepseek_session(raw: dict) -> dict:
    """DeepSeek 原始数据 → 统一格式"""
    messages = []
    if not raw or not isinstance(raw, dict):
        return {'messages': []}
    chat_messages = raw.get('data', {}) or {}
    chat_messages = chat_messages.get('biz_data', {}) or {}
    chat_messages = chat_messages.get('chat_messages', []) or []
    for m in chat_messages:
        fragments = m.get('fragments') or []
        thinking = '\n'.join(f.get('content', '') for f in fragments if f.get('content') and f.get('type') == 'THINK')
        content = '\n'.join(f.get('content', '') for f in fragments if f.get('content') and f.get('type') != 'THINK')
        messages.append({
            'id': m.get('message_id', str(uuid4())),
            'role': m.get('role', 'user'),
            'content': content,
            'created_at': m.get('inserted_at') or m.get('create_time'),
            'metadata': {'model': m.get('model'), 'thinking': thinking if thinking else None}
        })
    return {'messages': messages}

def normalize_doubao_session(raw: dict) -> dict:
    """豆包原始数据 → 统一格式"""
    messages = []
    if not raw or not isinstance(raw, dict):
        return {'messages': []}
    cells = raw.get('downlink_body', {}) or {}
    cells = cells.get('pull_singe_chain_downlink_body', {}) or {}
    cells = cells.get('messages', []) or []
    for m in cells:
        content = m.get('content', {})
        messages.append({
            'id': m.get('message_id', str(uuid4())),
            'role': 'assistant' if m.get('sender_type') == 2 else 'user',
            'content': content.get('text', '') if isinstance(content, dict) else str(content),
            'created_at': m.get('create_time'),
            'metadata': {}
        })
    return {'messages': messages}

def normalize_kimi_session(raw: dict) -> dict:
    """Kimi ListMessages 原始数据 → 统一格式"""
    messages = []
    if not raw or not isinstance(raw, dict):
        return {'messages': []}
    for m in raw.get('messages', []):
        role = m.get('role', 'user')
        if role == 'system':
            continue
        blocks = m.get('blocks', [])
        content = '\n'.join(b.get('text', {}).get('content', '') for b in blocks if b.get('text', {}).get('content'))
        if not content:
            continue
        messages.append({
            'id': m.get('id', str(uuid4())),
            'role': role,
            'content': content,
            'created_at': _iso_to_ts(m.get('createTime')),
            'metadata': {}
        })
    return {'messages': messages}

def normalize_session(platform: str, raw_session: dict, raw_conversation: dict = None) -> dict:
    """统一标准化入口"""
    session_id = str(uuid4())

    if platform == 'deepseek':
        platform_id = raw_session.get('id', '')
        title = raw_session.get('title', '')
        created_at = raw_session.get('created_at')
        updated_at = raw_session.get('updated_at')
        conv_data = normalize_deepseek_session(raw_conversation) if raw_conversation else {'messages': []}
    elif platform == 'doubao':
        conv = raw_session.get('conversation', {}) or {}
        platform_id = conv.get('conversation_id', '')
        title = conv.get('name', '')
        created_at = conv.get('create_time')
        updated_at = conv.get('update_time')
        conv_data = normalize_doubao_session(raw_conversation) if raw_conversation else {'messages': []}
    elif platform == 'kimi':
        platform_id = raw_session.get('id', '')
        title = raw_session.get('name', '')
        created_at = _iso_to_ts(raw_session.get('createTime'))
        updated_at = _iso_to_ts(raw_session.get('updateTime'))
        conv_data = normalize_kimi_session(raw_conversation) if raw_conversation else {'messages': []}
    else:
        platform_id = raw_session.get('id', str(uuid4()))
        title = raw_session.get('title', '')
        created_at = raw_session.get('created_at')
        updated_at = raw_session.get('updated_at')
        conv_data = {'messages': raw_session.get('messages', [])}

    return {
        'id': session_id,
        'platform': platform,
        'platform_session_id': str(platform_id),
        'title': title,
        'created_at': created_at,
        'updated_at': updated_at,
        'imported_at': datetime.now().isoformat(),
        'messages': conv_data['messages'],
        'raw_data': raw_session
    }
