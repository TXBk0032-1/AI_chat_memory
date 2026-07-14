import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { DesktopApi } from '../desktop-api'
import type { Message, SessionOpen } from '../conversation'
import { useSessionDetail } from './useSessionDetail'

function message(seq: number): Message {
  return { id: `message-${seq}`, role: 'assistant', content: `${seq}`, metadata: {}, seq }
}

function opened(id: string, messages: Message[] = []): SessionOpen {
  return {
    id,
    platform: 'deepseek',
    platform_session_id: id,
    title: id,
    message_count: 100,
    has_branches: false,
    start_seq: 0,
    messages,
    references: [],
  }
}

describe('useSessionDetail', () => {
  beforeEach(() => {
    vi.stubGlobal('localStorage', { getItem: vi.fn(() => null), setItem: vi.fn() })
  })

  it('discards an older open request after switching sessions', async () => {
    let resolveFirst!: (value: SessionOpen) => void
    const api = {
      openSession: vi.fn((id: string) => id === 'first'
        ? new Promise<SessionOpen>((resolve) => { resolveFirst = resolve })
        : Promise.resolve(opened(id))),
    } as unknown as DesktopApi
    const detail = useSessionDetail(api)
    const first = detail.open('first')
    await detail.open('second')
    resolveFirst(opened('first'))
    await first
    expect(detail.selected.value?.id).toBe('second')
  })

  it('merges a requested batch and deduplicates concurrent requests', async () => {
    const api = {
      openSession: vi.fn().mockResolvedValue(opened('session', [message(0)])),
      getSessionMessages: vi.fn().mockResolvedValue([message(50), message(51)]),
    } as unknown as DesktopApi
    const detail = useSessionDetail(api)
    await detail.open('session')
    await Promise.all([detail.ensureMessageLoaded(50), detail.ensureMessageLoaded(51)])
    expect(api.getSessionMessages).toHaveBeenCalledTimes(1)
    expect(detail.messageSlots.value[51]?.id).toBe('message-51')
    expect(detail.loadedMessageCount.value).toBe(3)
  })

  it('loads every batch required by an export selection', async () => {
    const api = {
      openSession: vi.fn().mockResolvedValue(opened('session')),
      getSessionMessages: vi.fn((_id: string, start: number) => Promise.resolve([message(start)])),
    } as unknown as DesktopApi
    const detail = useSessionDetail(api)
    await detail.open('session')
    await detail.ensureMessagesLoaded([0, 50])
    expect(api.getSessionMessages).toHaveBeenCalledTimes(2)
    expect(detail.messageSlots.value[50]?.id).toBe('message-50')
  })
})
