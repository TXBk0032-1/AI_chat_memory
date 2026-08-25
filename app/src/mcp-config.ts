export const DEFAULT_MCP_URL = 'http://127.0.0.1:19821/mcp'

export function buildMcpClientConfig(url: string = DEFAULT_MCP_URL, secret?: string): string {
  const serverConfig: Record<string, unknown> = { url }
  if (secret) {
    serverConfig.headers = {
      'x-ai-chat-memory-secret': secret,
    }
  }
  return JSON.stringify(
    { mcpServers: { 'ai-chat-memory': serverConfig } },
    null,
    2,
  )
}
