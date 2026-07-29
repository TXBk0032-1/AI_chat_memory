import { onBeforeUnmount, ref, type Ref } from 'vue'
import { desktopApi, type CloudConnectionTestResult, type CloudCredentialInput, type CloudSyncStatus } from '../desktop-api'

export function useCloudSync(enabled?: Ref<boolean>) {
  const status = ref<CloudSyncStatus>({ state: 'disabled', pending_mutations: 0, devices: [] })
  const busy = ref(false)
  const password = ref('')
  const syncPassword = ref('')
  let timer: ReturnType<typeof setInterval> | undefined
  async function refreshStatus() { status.value = await desktopApi.getCloudSyncStatus(); return status.value }
  async function testConnection(): Promise<CloudConnectionTestResult> { return desktopApi.testCloudSyncConnection() }
  async function saveCredentials() {
    const input: CloudCredentialInput = { webdav_password: password.value, sync_password: syncPassword.value || null }
    await desktopApi.saveCloudSyncCredentials(input)
    password.value = ''; syncPassword.value = ''
  }
  async function syncNow() { busy.value = true; try { status.value = await desktopApi.syncNow(); return status.value } finally { busy.value = false } }
  async function rewriteArchive() { busy.value = true; try { status.value = await desktopApi.rewriteCloudArchive(); return status.value } finally { busy.value = false } }
  function startPolling() { if (timer) return; void refreshStatus(); timer = setInterval(() => void refreshStatus(), 2_000) }
  function dispose() { if (timer) clearInterval(timer); timer = undefined }
  if (enabled?.value) startPolling()
  onBeforeUnmount(dispose)
  return { status, busy, password, syncPassword, testConnection, saveCredentials, syncNow, rewriteArchive, refreshStatus, startPolling, dispose }
}
