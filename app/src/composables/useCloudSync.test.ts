/** @vitest-environment happy-dom */

import { createApp, defineComponent, h } from 'vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useCloudSync } from './useCloudSync'

const mocks = vi.hoisted(() => ({
  getCloudSyncStatus: vi.fn(),
  testCloudSyncConnection: vi.fn(),
  syncNow: vi.fn(),
  rewriteCloudArchive: vi.fn(),
  removeCloudDeviceRecord: vi.fn(),
}))

vi.mock('../desktop-api', () => ({ desktopApi: mocks }))

function status(state: 'disabled' | 'idle' | 'syncing' = 'idle') {
  return { state, pending_mutations: 0, devices: [] }
}

function mountComposable() {
  document.body.innerHTML = '<div id="app"></div>'
  let exposed!: ReturnType<typeof useCloudSync>
  const Root = defineComponent({
    setup: () => {
      exposed = useCloudSync()
      return () => h('div')
    },
  })
  const app = createApp(Root)
  app.mount('#app')
  return {
    exposed,
    unmount() {
      app.unmount()
      document.body.innerHTML = ''
    },
  }
}

describe('useCloudSync', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    mocks.getCloudSyncStatus.mockReset().mockResolvedValue(status())
    mocks.testCloudSyncConnection.mockReset()
    mocks.syncNow.mockReset()
    mocks.rewriteCloudArchive.mockReset()
    mocks.removeCloudDeviceRecord.mockReset()
  })

  afterEach(() => {
    vi.useRealTimers()
    document.body.innerHTML = ''
  })

  it('polls status after the caller starts it and stops after dispose', async () => {
    const harness = mountComposable()
    try {
      harness.exposed.startPolling()
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(1)

      await vi.advanceTimersByTimeAsync(2_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(2)

      harness.exposed.dispose()
      await vi.advanceTimersByTimeAsync(6_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(2)
    } finally {
      harness.unmount()
    }
  })

  it('restarts polling when the requested interval changes', async () => {
    const harness = mountComposable()
    try {
      harness.exposed.startPolling(15_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(1)

      await vi.advanceTimersByTimeAsync(2_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(1)
      await vi.advanceTimersByTimeAsync(13_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(2)

      harness.exposed.startPolling(2_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(3)
      await vi.advanceTimersByTimeAsync(2_000)
      expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(4)
    } finally {
      harness.unmount()
    }
  })

  it('always clears busy after a failed manual operation', async () => {
    const harness = mountComposable()
    try {
      const failure = new Error('sync failed')
      mocks.syncNow.mockRejectedValueOnce(failure)

      const operation = harness.exposed.syncNow()
      expect(harness.exposed.busy.value).toBe(true)
      await expect(operation).rejects.toBe(failure)
      expect(harness.exposed.busy.value).toBe(false)
    } finally {
      harness.unmount()
    }
  })

  it('updates status and clears busy after removing a device record', async () => {
    const harness = mountComposable()
    try {
      const nextStatus = { state: 'idle' as const, pending_mutations: 0, devices: [] }
      mocks.removeCloudDeviceRecord.mockResolvedValueOnce(nextStatus)

      const operation = harness.exposed.removeDeviceRecord('device-a')
      expect(harness.exposed.busy.value).toBe(true)
      await expect(operation).resolves.toEqual(nextStatus)
      expect(mocks.removeCloudDeviceRecord).toHaveBeenCalledWith('device-a')
      expect(harness.exposed.status.value).toEqual(nextStatus)
      expect(harness.exposed.busy.value).toBe(false)
    } finally {
      harness.unmount()
    }
  })

  it('cleans the polling interval when the composable owner unmounts', async () => {
    const harness = mountComposable()
    harness.exposed.startPolling()
    expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(1)

    harness.unmount()
    await vi.advanceTimersByTimeAsync(4_000)
    expect(mocks.getCloudSyncStatus).toHaveBeenCalledTimes(1)
  })
})
