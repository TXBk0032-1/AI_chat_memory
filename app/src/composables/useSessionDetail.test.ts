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

  it('does not reopen the selected or currently opening session', async () => {
    let resolveOpen!: (value: SessionOpen) => void
    const api = {
      openSession: vi.fn(() => new Promise<SessionOpen>((resolve) => { resolveOpen = resolve })),
    } as unknown as DesktopApi
    const detail = useSessionDetail(api)

    const first = detail.open('session')
    expect(detail.shouldOpen('session')).toBe(false)
    expect(detail.shouldOpen('other')).toBe(true)
    expect(await detail.open('session')).toBeNull()
    expect(api.openSession).toHaveBeenCalledTimes(1)

    resolveOpen(opened('session'))
    await first
    expect(detail.shouldOpen('session')).toBe(false)
    expect(detail.shouldOpen('other')).toBe(true)
  })

  it('allows restoring the selected session after another session fails to open', async () => {
    let rejectOpen!: (reason: unknown) => void
    const api = {
      openSession: vi.fn((id: string) => id === 'second'
        ? new Promise<SessionOpen>((_resolve, reject) => { rejectOpen = reject })
        : Promise.resolve(opened(id))),
    } as unknown as DesktopApi
    const detail = useSessionDetail(api)

    await detail.open('first')
    const second = detail.open('second')
    expect(detail.shouldOpen('second')).toBe(false)
    expect(detail.shouldOpen('first')).toBe(true)

    rejectOpen(new Error('open failed'))
    await second
    expect(detail.selected.value?.id).toBe('first')
    expect(detail.shouldOpen('first')).toBe(true)
  })

  it('does not reopen the selected session after a background batch fails', async () => {
    vi.useFakeTimers()
    try {
      const api = {
        openSession: vi.fn().mockResolvedValue(opened('session', [message(0)])),
        getSessionMessages: vi.fn().mockRejectedValue(new Error('batch failed')),
      } as unknown as DesktopApi
      const detail = useSessionDetail(api)

      const result = await detail.open('session')
      detail.scheduleBackgroundLoad(result!.generation)
      await vi.runOnlyPendingTimersAsync()

      expect(detail.backgroundLoadFailed.value).toBe(true)
      expect(detail.shouldOpen('session')).toBe(false)
    } finally {
      vi.useRealTimers()
    }
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
