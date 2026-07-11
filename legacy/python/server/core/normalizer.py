from uuid import uuid4
from datetime import datetime
from typing import Any

DEEPSEEK_EXPORT_CONTENT_TYPES = {
    'REQUEST': 'user',
    'RESPONSE': 'assistant',
}

def _iso_to_ts(iso_str: str | None) -> str | None:
    """ISO 8601 → Unix 时间戳字符串，失败返回原值"""
    if not iso_str:
        return iso_str
    try:
        dt = datetime.fromisoformat(iso_str.replace('Z', '+00:00'))
        return str(dt.timestamp())
    except Exception:
        return iso_str

def normalize_deepseek_session(raw: dict[str, Any]) -> dict[str, Any]:
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

def normalize_deepseek_export_session(raw: dict[str, Any]) -> dict[str, Any]:
    """DeepSeek 官方导出 conversation → 统一格式，保留 mapping 分支关系"""
    if not raw or not isinstance(raw, dict):
        raise ValueError('DeepSeek export conversation must be an object')
    mapping = raw.get('mapping')
    if not isinstance(mapping, dict):
        raise ValueError('DeepSeek export conversation missing mapping')

    messages = []
    for node_id, node in mapping.items():
        if not isinstance(node, dict):
            continue
        message = node.get('message')
        if not isinstance(message, dict):
            continue

        fragments = message.get('fragments') or []
        if not isinstance(fragments, list):
            fragments = []

        fragment_types = []
        content_by_role = {'user': [], 'assistant': []}
        thinking = []
        tool_types = []
        search_result_count = 0

        for fragment in fragments:
            if not isinstance(fragment, dict):
                continue
            fragment_type = fragment.get('type')
            if fragment_type:
                fragment_types.append(fragment_type)

            content = fragment.get('content')
            if fragment_type in DEEPSEEK_EXPORT_CONTENT_TYPES and content:
                content_by_role[DEEPSEEK_EXPORT_CONTENT_TYPES[fragment_type]].append(str(content))
            elif fragment_type == 'THINK' and content:
                thinking.append(str(content))
            elif fragment_type:
                tool_types.append(fragment_type)

            results = fragment.get('results')
            if isinstance(results, list):
                search_result_count += len(results)

        role = None
        content = ''
        if content_by_role['user']:
            role = 'user'
            content = '\n'.join(content_by_role['user'])
        elif content_by_role['assistant']:
            role = 'assistant'
            content = '\n'.join(content_by_role['assistant'])

        if not role:
            continue

        metadata = {
            'source': 'deepseek_export',
            'node_id': node.get('id') or node_id,
            'parent_node_id': node.get('parent'),
            'children_node_ids': node.get('children') or [],
            'fragment_types': sorted(set(t for t in fragment_types if t)),
            'tool_types': sorted(set(t for t in tool_types if t)),
            'search_result_count': search_result_count,
            'model': message.get('model'),
            'files': message.get('files') or [],
        }
        if thinking:
            metadata['thinking'] = '\n'.join(thinking)

        messages.append({
            'id': message.get('message_id') or node.get('id') or node_id or str(uuid4()),
            'role': role,
            'content': content,
            'created_at': message.get('inserted_at') or raw.get('updated_at') or raw.get('inserted_at'),
            'metadata': metadata
        })

    messages.sort(key=lambda m: m.get('created_at') or '')
    return {
        'id': str(uuid4()),
        'platform': 'deepseek',
        'platform_session_id': str(raw.get('id', '')),
        'title': raw.get('title', ''),
        'created_at': raw.get('inserted_at'),
        'updated_at': raw.get('updated_at') or raw.get('inserted_at'),
        'imported_at': datetime.now().isoformat(),
        'messages': messages,
        'raw_data': raw
    }

def normalize_doubao_session(raw: dict[str, Any]) -> dict[str, Any]:
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

def normalize_kimi_session(raw: dict[str, Any]) -> dict[str, Any]:
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

def normalize_session(
    platform: str,
    raw_session: dict[str, Any],
    raw_conversation: dict[str, Any] | None = None,
) -> dict[str, Any]:
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
