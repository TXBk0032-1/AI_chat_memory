import { onBeforeUnmount, ref, type Ref } from 'vue'
import { desktopApi, type CloudConnectionTestResult, type CloudCredentialInput, type CloudSyncSettings, type CloudSyncStatus } from '../desktop-api'

export function useCloudSync(enabled?: Ref<boolean>) {
  const status = ref<CloudSyncStatus>({ state: 'disabled', pending_mutations: 0, devices: [] })
  const busy = ref(false)
  let timer: ReturnType<typeof setInterval> | undefined
  let pollingIntervalMs: number | undefined
  async function refreshStatus() { status.value = await desktopApi.getCloudSyncStatus(); return status.value }
  async function testConnection(settings: CloudSyncSettings, credentials: CloudCredentialInput): Promise<CloudConnectionTestResult> { return desktopApi.testCloudSyncConnection(settings, credentials) }
  async function syncNow() { busy.value = true; try { status.value = await desktopApi.syncNow(); return status.value } finally { busy.value = false } }
  async function rewriteArchive() { busy.value = true; try { status.value = await desktopApi.rewriteCloudArchive(); return status.value } finally { busy.value = false } }
  async function removeDeviceRecord(deviceId: string) {
    busy.value = true
    try {
      status.value = await desktopApi.removeCloudDeviceRecord(deviceId)
      return status.value
    } finally {
      busy.value = false
    }
  }
  function pollStatus() { void refreshStatus().catch(() => undefined) }
  function startPolling(intervalMs = 2_000) {
    if (timer && pollingIntervalMs === intervalMs) return
    if (timer) clearInterval(timer)
    pollingIntervalMs = intervalMs
    pollStatus()
    timer = setInterval(pollStatus, intervalMs)
  }
  function dispose() {
    if (timer) clearInterval(timer)
    timer = undefined
    pollingIntervalMs = undefined
  }
  if (enabled?.value) startPolling()
  onBeforeUnmount(dispose)
  return { status, busy, testConnection, syncNow, rewriteArchive, removeDeviceRecord, refreshStatus, startPolling, dispose }
}
