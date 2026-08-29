import { ref } from 'vue'
import { desktopApi, type ApiStatus, type DesktopApi } from '../desktop-api'

const statusPollIntervalMs = 3000

export function useApiStatus(api: DesktopApi = desktopApi) {
  const apiStatus = ref<ApiStatus>({ service: { state: 'starting' }, userscript_connected: false, mcp: { state: 'stopped' }, mcp_url: 'http://127.0.0.1:19821/mcp' })
  let statusTimer: number | undefined
  let refreshGeneration = 0

  async function refreshApiStatus() {
    const generation = ++refreshGeneration
    const status = await api.getApiStatus()
    // A slow IPC response from an older refresh must never overwrite a newer
    // status; discard anything that is no longer the latest request.
    if (generation !== refreshGeneration) return
    apiStatus.value = status
  }

  function startStatusPolling() {
    if (statusTimer !== undefined) return
    statusTimer = window.setInterval(() => { void refreshApiStatus().catch(() => {}) }, statusPollIntervalMs)
  }

  function dispose() {
    if (statusTimer !== undefined) {
      window.clearInterval(statusTimer)
      statusTimer = undefined
    }
  }

  return { apiStatus, refreshApiStatus, startStatusPolling, dispose }
}
