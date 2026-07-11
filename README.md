# AI Chat Memory

跨平台 AI 聊天记录管理系统。从 Web AI 平台采集对话数据，统一存储、浏览、检索。

本项目包含两个主要部分：

- `server/`：FastAPI + SQLite 本地服务，提供导入、查询、搜索和 Web UI。
- `userscript/dist/ai-chat-memory.user.js`：Tampermonkey 脚本，负责在 AI 平台页面抓取会话并同步到本地服务。

## 支持平台

| 平台 | 采集方式 | 同步模式 |
|------|----------|----------|
| DeepSeek | XHR 拦截 + Bearer Token 自动捕获 | 增量 / 全量 |
| 豆包 | Cookie 自动携带 | 全量 |
| Kimi | XHR/fetch 拦截 + Bearer Token 自动捕获 | 全量 |

## 快速开始

### 1. 安装依赖

```bash
cd server
python -m venv venv
```

Windows PowerShell：

```powershell
.\venv\Scripts\Activate.ps1
pip install -r requirements.txt
```

macOS / Linux：

```bash
source venv/bin/activate
pip install -r requirements.txt
```

### 2. 启动本地服务

Windows PowerShell 可以从项目根目录直接运行：

```powershell
.\start-server.ps1
```

也可以进入 `server/` 手动启动：

```bash
cd server
python main.py
```

服务运行在 http://localhost:19820

### 3. 安装油猴脚本

在 [Tampermonkey](https://www.tampermonkey.net/) 中加载 `userscript/dist/ai-chat-memory.user.js`。

脚本匹配以下站点：

- `https://chat.deepseek.com/*`
- `https://www.doubao.com/*`
- `https://kimi.com/*`
- `https://www.kimi.com/*`

### 4. 同步对话

打开支持的平台页面，右上角会出现 AI Chat Memory 面板：

- **开始同步** — 增量同步（只拉取上次同步后更新的会话）
- **全量同步** — 重新拉取所有会话
- **服务状态** — 自动检测后端连接

DeepSeek 支持基于 `updated_at` 的增量同步；豆包和 Kimi 当前使用全量同步。

### 5. 浏览记录

访问 http://localhost:19820 打开「藏经阁」Web UI，支持：

- 按平台筛选
- 按标题 / 消息内容搜索
- Markdown / LaTeX 渲染
- 思考过程折叠查看

## 项目结构

```
ai-chat-memory/
├── README.md                                # 项目说明
├── CLAUDE.md                                # Claude Code 工作说明
├── docs/PROJECT.md                          # 架构和维护梳理
├── start-server.ps1                         # Windows 快速启动脚本
├── userscript/dist/ai-chat-memory.user.js   # 油猴脚本（采集 + UI 面板）
├── server/
│   ├── main.py                                # FastAPI 入口
│   ├── config.py                              # 配置（端口 19820）
│   ├── api/
│   │   ├── sessions.py                        # 会话 CRUD + 导入 + 同步状态
│   │   └── health.py                          # 健康检查
│   ├── core/
│   │   ├── database.py                        # SQLite (aiosqlite)
│   │   ├── normalizer.py                      # 平台数据标准化
│   │   └── deepseek_export.py                 # DeepSeek ZIP 导出解析器
│   ├── models/__init__.py                     # Pydantic 模型
│   ├── static/index.html                      # Web UI「藏经阁」
│   └── data/chat_memory.db                    # SQLite 数据库（本地运行数据）
```

## 数据位置

默认数据库路径为 `server/data/chat_memory.db`。该目录属于本地运行数据，已在 `.gitignore` 中忽略，避免把聊天记录提交到仓库。

## API

| 端点 | 方法 | 功能 |
|------|------|------|
| `/api/v1/health` | GET | 健康检查 |
| `/api/v1/sessions/import` | POST | 导入会话 |
| `/api/v1/sessions` | GET | 列出会话（?platform=&limit=&offset=） |
| `/api/v1/sessions/search` | GET | 搜索会话（?q=&platform=&date_from=&date_to=&limit=&offset=） |
| `/api/v1/sessions/{id}` | GET | 会话详情（含消息） |
| `/api/v1/sessions/{id}` | DELETE | 删除会话 |
| `/api/v1/sessions/sync-status` | GET | 同步状态（?platform=） |

## 添加新平台

1. 后端：在 `server/core/normalizer.py` 添加 `normalize_<platform>_session()`，并在 `normalize_session()` 中增加分支。
2. 油猴脚本：新增继承 `BaseAdapter` 的适配器，实现 `fetchAllSessions()`、`fetchConversation()`、`getToken()`，并更新平台识别和适配器实例化逻辑。

## 技术栈

- **采集层**: Tampermonkey UserScript, XHR 拦截, Bearer Token 捕获
- **服务端**: Python, FastAPI, SQLite (aiosqlite)
- **展示层**: 原生 HTML/JS, marked.js, KaTeX
