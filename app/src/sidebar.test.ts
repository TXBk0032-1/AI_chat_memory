import { beforeEach, describe, expect, it, vi } from 'vitest'
import { loadSidebarCollapsed, saveSidebarCollapsed } from './sidebar'

describe('sidebar preference', () => {
  let values: Map<string, string>

  beforeEach(() => {
    values = new Map<string, string>()
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    })
  })

  it('defaults to expanded and restores a collapsed sidebar', () => {
    expect(loadSidebarCollapsed()).toBe(false)
    saveSidebarCollapsed(true)
    expect(loadSidebarCollapsed()).toBe(true)
  })

  it('falls back to expanded for invalid stored data', () => {
    values.set('ai-chat-memory.sidebar-collapsed', '{invalid')
    expect(loadSidebarCollapsed()).toBe(false)
  })
})
