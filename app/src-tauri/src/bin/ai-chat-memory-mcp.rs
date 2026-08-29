//! AI Chat Memory 的 MCP stdio 服务器入口（独立 bin target）。
//!
//! 安全模型差异：HTTP 端点（127.0.0.1:19821）依赖 secret + Origin 白名单中间件；
//! stdio 传输没有网络面，不经过任何 HTTP 中间件，信任边界等价于桌面应用——
//! 能启动本进程的本地用户。协议帧走 stdin/stdout，日志仅写文件与 stderr。
//! `mcp_enabled` 关闭时启动即报错退出（退出码 1）。
//!
//! 常见 MCP 客户端配置：
//! `{"command": "<安装目录>/ai-chat-memory-mcp.exe", "args": []}`

#[tokio::main]
async fn main() {
    if let Err(error) = ai_chat_memory_desktop_lib::run_mcp_stdio().await {
        eprintln!("ai-chat-memory-mcp: {error}");
        std::process::exit(1);
    }
}
