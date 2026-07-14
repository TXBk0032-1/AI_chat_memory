# AI Chat Memory

本地优先的跨平台 AI 聊天记录管理器。Tampermonkey userscript 从 DeepSeek、豆包和 Kimi 获取会话，Tauri 桌面端通过本地 API 接收并保存到 SQLite。

## 功能

- 从 DeepSeek、豆包和 Kimi 网页同步聊天记录；
- 按平台、日期、标题和消息正文查找历史对话；
- 查看 DeepSeek 对话分支，并在不同分支间切换；
- 按“一问一答”选择当前分支中的部分内容；
- 导出 PNG、JPEG、Markdown 或 JSON，可选择是否包含思考过程；
- 本地保存数据，支持更改数据目录、同步密钥、主题和托盘行为。

## 安装与使用

当前发布流程生成 Windows x64 安装包。安装前需要系统已具备 WebView2（现代 Windows 通常已预装）。

1. 从 `artifacts/` 运行最新的 `AI Chat Memory_*.msi`。
2. 启动 AI Chat Memory，确认左下角显示“同步服务运行中”。
3. 在 Tampermonkey 中安装 [`userscript/dist/ai-chat-memory.user.js`](userscript/dist/ai-chat-memory.user.js)。
4. 打开并登录 DeepSeek、豆包或 Kimi 网页。
5. 在网页右上方的 AI Chat Memory 面板点击“开始同步”或“全量同步”。
6. 回到桌面端刷新，即可搜索、查看和导出对话。

“开始同步”只处理新增或发生变化的会话；“全量同步”会重新读取平台提供的全部会话。

完整操作、导出格式、安全设置和故障排查请参阅 [用户手册](docs/USER_GUIDE.md)。

## 技术栈

- 桌面端：Tauri 2 + Rust 1.97.0（Edition 2024）
- 前端：Vue 3 + TypeScript + Vite
- 本地服务：Axum，固定监听 `127.0.0.1:19820`
- 数据库：SQLx + SQLite
- 采集端：`userscript/dist/ai-chat-memory.user.js`

## 开发

前置环境：Rust 1.97.0、Node.js 22+、Visual Studio C++ Build Tools、WebView2。

```powershell
cd app
npm install
npm run tauri dev
```

前端生产构建：

```powershell
cd app
npm run build
```

Rust 检查：

```powershell
cd app/src-tauri
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## 数据存储

桌面端数据库默认位于 `%APPDATA%\dev.aichatmemory.desktop\chat_memory.db`，也可在设置中更改数据保存位置。用户数据库目录不会提交到 Git。

更改数据保存位置会迁移当前数据库并重启应用。执行迁移前建议先备份 `chat_memory.db`。

## 本地 API 安全

userscript 请求必须满足：

- Origin 位于桌面端白名单；
- 携带 `X-AI-Chat-Memory-Client: userscript-v1`；
- 若桌面端启用了随机密钥，还需携带 `X-AI-Chat-Memory-Secret`。

桌面端设置页可编辑 Origin 白名单并启用、关闭或轮换随机密钥。userscript 菜单可配置后端地址和对应密钥。

## 归档

已停用的 Python 参考实现和旧项目说明存放在 `archive/`，不属于当前应用架构，也不参与构建、测试或发布。

## 本地 CI/CD

轻量流水线位于 `scripts/ci.ps1`，不依赖 Docker 或外部 CI 服务：

```powershell
# 格式、Clippy、userscript 语法、Vue 类型和前端构建
.\scripts\ci.ps1 check

# check + Rust 测试
.\scripts\ci.ps1 test

# test + Windows MSI/EXE + SHA-256 manifest
.\scripts\ci.ps1 release
```

release 产物输出到 `artifacts/`。使用 `-Clean` 可清理 Rust/前端缓存后执行完全构建。

可选启用 pre-push hook：

```powershell
.\scripts\install-hooks.ps1
```

编码代理在完成并提交任务后应执行统一结束 Hook：

```powershell
.\scripts\finish-task.ps1
```

该命令要求工作区无未提交改动，并运行完整 release 流水线生成最新 MSI、EXE 和校验清单。
