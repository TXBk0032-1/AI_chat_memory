# 项目梳理

AI Chat Memory 是一个本地优先的 AI 聊天记录管理系统，由采集脚本、本地 API 服务和 Web UI 三部分组成。

## 数据流

1. Tampermonkey 脚本在目标 AI 平台页面加载。
2. 平台适配器获取会话列表和会话详情。
3. 脚本把原始会话数据 POST 到本地服务 `/api/v1/sessions/import`。
4. 后端根据 `platform` 调用对应 normalizer，转成统一会话 / 消息格式。
5. 数据写入 `server/data/chat_memory.db`。
6. Web UI 通过 `/api/v1/sessions/search` 和 `/api/v1/sessions/{id}` 浏览与检索记录。

## 模块职责

| 路径 | 职责 |
|------|------|
| `server/main.py` | FastAPI 应用入口、CORS、路由挂载、静态资源服务 |
| `server/api/` | HTTP API 路由 |
| `server/core/database.py` | SQLite 初始化、会话和消息写入 |
| `server/core/normalizer.py` | 平台原始数据到统一格式的转换 |
| `server/models/` | Pydantic 请求 / 响应模型 |
| `server/static/index.html` | 单文件 Web UI |
| `userscript/dist/ai-chat-memory.user.js` | Tampermonkey 采集脚本和平台同步面板 |

## 当前平台

| 平台 | 后端 normalizer | 脚本适配器 | 备注 |
|------|------------------|------------|------|
| DeepSeek | `normalize_deepseek_session()` | `DeepSeekAdapter` | 支持增量同步 |
| 豆包 | `normalize_doubao_session()` | `DoubaoAdapter` | Cookie 自动携带 |
| Kimi | `normalize_kimi_session()` | `KimiAdapter` | Bearer Token 捕获 |

## 维护约定

- 本地数据库在 `server/data/chat_memory.db`，不提交到仓库。
- 后端暂不使用 ORM，保持 raw SQL + aiosqlite。
- 每次导入会话时会替换该会话消息，不做逐条消息增量更新。
- 新平台需要同时补后端 normalizer 和用户脚本 adapter。
