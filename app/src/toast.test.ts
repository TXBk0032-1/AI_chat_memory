import { afterEach, describe, expect, it, vi } from 'vitest'
import { useToastQueue } from './composables/useToastQueue'

describe('toast queue', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('stacks new notices and expires each one on its own schedule', () => {
    vi.useFakeTimers()
    const queue = useToastQueue()

    queue.showToast('first', 2000)
    vi.advanceTimersByTime(500)
    queue.showToast('second', 2000)

    expect(queue.toasts.value.map((toast) => toast.message)).toEqual(['first', 'second'])
    vi.advanceTimersByTime(1500)
    expect(queue.toasts.value.map((toast) => toast.message)).toEqual(['second'])
    vi.advanceTimersByTime(500)
    expect(queue.toasts.value).toEqual([])
  })

  it('clears pending notices when disposed', () => {
    vi.useFakeTimers()
    const queue = useToastQueue()
    queue.showToast('export complete')

    queue.disposeToasts()
    vi.runAllTimers()

    expect(queue.toasts.value).toEqual([])
  })
})
