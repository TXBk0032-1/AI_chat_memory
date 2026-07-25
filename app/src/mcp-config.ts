export const DEFAULT_MCP_URL = 'http://127.0.0.1:19821/mcp'

export function buildMcpClientConfig(url: string = DEFAULT_MCP_URL): string {
  return JSON.stringify(
    { mcpServers: { 'ai-chat-memory': { url } } },
    null,
    2,
  )
}
