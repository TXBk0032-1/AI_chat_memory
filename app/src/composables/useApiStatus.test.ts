/** @vitest-environment happy-dom */

import { afterEach, describe, expect, it, vi, type Mock } from 'vitest'
import type { ApiStatus, DesktopApi } from '../desktop-api'
import { useApiStatus } from './useApiStatus'

function status(state: string): ApiStatus {
  return { service: { state }, userscript_connected: false, mcp: { state: 'stopped' }, mcp_url: 'http://127.0.0.1:19821/mcp' }
}

function apiMock(): DesktopApi & { getApiStatus: Mock } {
  return { getApiStatus: vi.fn() } as unknown as DesktopApi & { getApiStatus: Mock }
}

describe('useApiStatus', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('applies the latest status after a successful refresh', async () => {
    const api = apiMock()
    api.getApiStatus.mockResolvedValue(status('running'))
    const { apiStatus, refreshApiStatus } = useApiStatus(api)

    await refreshApiStatus()

    expect(apiStatus.value.service.state).toBe('running')
  })

  it('discards a slow stale response that arrives after a newer refresh', async () => {
    const api = apiMock()
    let resolveSlow!: (value: ApiStatus) => void
    const slow = new Promise<ApiStatus>((resolve) => { resolveSlow = resolve })
    api.getApiStatus
      .mockImplementationOnce(() => slow)
      .mockResolvedValueOnce(status('running'))
    const { apiStatus, refreshApiStatus } = useApiStatus(api)

    const slowRefresh = refreshApiStatus()
    await refreshApiStatus()
    expect(apiStatus.value.service.state).toBe('running')

    // The old IPC result finally lands; it must not overwrite the newer status.
    resolveSlow(status('failed'))
    await slowRefresh

    expect(apiStatus.value.service.state).toBe('running')
  })

  it('keeps applying newer statuses again after a stale one was discarded', async () => {
    const api = apiMock()
    let resolveSlow!: (value: ApiStatus) => void
    const slow = new Promise<ApiStatus>((resolve) => { resolveSlow = resolve })
    api.getApiStatus
      .mockImplementationOnce(() => slow)
      .mockResolvedValueOnce(status('running'))
      .mockResolvedValueOnce(status('starting'))
    const { apiStatus, refreshApiStatus } = useApiStatus(api)

    const slowRefresh = refreshApiStatus()
    await refreshApiStatus()
    resolveSlow(status('failed'))
    await slowRefresh
    await refreshApiStatus()

    expect(apiStatus.value.service.state).toBe('starting')
  })

  it('polls on an interval and stops on dispose', () => {
    vi.useFakeTimers()
    const api = apiMock()
    api.getApiStatus.mockResolvedValue(status('running'))
    const { startStatusPolling, dispose } = useApiStatus(api)

    startStatusPolling()
    vi.advanceTimersByTime(3000)
    vi.advanceTimersByTime(3000)
    expect(api.getApiStatus).toHaveBeenCalledTimes(2)

    dispose()
    vi.advanceTimersByTime(3000)
    expect(api.getApiStatus).toHaveBeenCalledTimes(2)
    // Polling start is idempotent.
    startStatusPolling()
    startStatusPolling()
    vi.advanceTimersByTime(3000)
    expect(api.getApiStatus).toHaveBeenCalledTimes(3)
    dispose()
  })
})
