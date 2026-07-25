import { describe, expect, it } from 'vitest'
import { buildMcpClientConfig } from './mcp-config'

describe('buildMcpClientConfig', () => {
  it('includes fixed url', () => {
    const text = buildMcpClientConfig()
    expect(text).toContain('http://127.0.0.1:19821/mcp')
    expect(JSON.parse(text).mcpServers['ai-chat-memory'].url).toBe(
      'http://127.0.0.1:19821/mcp',
    )
  })
})
