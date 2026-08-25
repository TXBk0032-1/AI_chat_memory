import { open } from '@tauri-apps/plugin-dialog'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { ref, type Ref } from 'vue'
import {
  desktopApi,
  type ApiStatus,
  type CloudCredentialInput,
  type DesktopApi,
  type ModelDownloadProgress,
  type ReindexProgress,
  type SemanticRuntimeStatus,
  type SettingsModel,
} from '../desktop-api'
import { buildMcpClientConfig } from '../mcp-config'
import { translate as t } from '../i18n'

type ThemePreview = { begin(): void; accept(): void; cancel(): void }
type LocalePreview = { begin(): void; accept(): void | Promise<unknown>; cancel(): void | Promise<unknown> }

export function useSettings(
  settings: Ref<SettingsModel>,
  error: Ref<string>,
  theme: ThemePreview,
  locale: LocalePreview,
  api: DesktopApi = desktopApi,
) {
  const showSettings = ref(false)
  const originText = ref('')
  const secretCopied = ref(false)
  const mcpConfigCopied = ref(false)
  const settingsApiStatus = ref<ApiStatus | null>(null)
  const semanticStatus = ref<SemanticRuntimeStatus | null>(null)
  const semanticBusy = ref(false)
  const downloadProgress = ref<ModelDownloadProgress | null>(null)
  const reindexProgress = ref<ReindexProgress | null>(null)
  let unlistenDownload: UnlistenFn | undefined
  let unlistenReindex: UnlistenFn | undefined
  let reindexPollTimer: number | undefined
  let mcpConfigCopiedTimer: number | undefined

  function clearMcpConfigCopiedTimer() {
    if (mcpConfigCopiedTimer !== undefined) {
      window.clearTimeout(mcpConfigCopiedTimer)
      mcpConfigCopiedTimer = undefined
    }
  }

  async function openSettings() {
    settings.value = await api.getSettings()
    theme.begin()
    locale.begin()
    originText.value = settings.value.allowed_origins.join('\n')
    secretCopied.value = false
    mcpConfigCopied.value = false
    clearMcpConfigCopiedTimer()
    showSettings.value = true
    try {
      settingsApiStatus.value = await api.getApiStatus()
      await refreshSemanticStatus()
      if (semanticStatus.value?.reindex && !['done', 'error', 'cancelled'].includes(semanticStatus.value.reindex.stage)) {
        startReindexPolling()
      }
    } catch {
      semanticStatus.value = null
    }
  }

  function closeSettings(save = false) {
    if (!save) {
      theme.cancel()
      void locale.cancel()
    }
    clearMcpConfigCopiedTimer()
    showSettings.value = false
  }

  async function saveSettings(cloudSyncCredentials: CloudCredentialInput | null = null) {
    settings.value.allowed_origins = originText.value.split('\n').map((value) => value.trim()).filter(Boolean)
    settings.value.setup_complete = true
    for (const backend of ['ollama', 'llama_cpp', 'openai_compatible'] as const) {
      const value = settings.value.semantic_search[backend].dimensions
      if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
        delete settings.value.semantic_search[backend].dimensions
      }
    }
    try {
      settings.value = await api.saveSettings(settings.value, cloudSyncCredentials)
      theme.accept()
      await locale.accept()
      closeSettings(true)
    } catch (reason) {
      error.value = String(reason)
    }
  }

  async function rotateSecret() {
    settings.value = await api.rotateSecret()
    secretCopied.value = false
  }

  async function copySecret() {
    if (!settings.value.secret) return
    await navigator.clipboard.writeText(settings.value.secret)
    secretCopied.value = true
  }

  async function changeDataDirectory() {
    const path = await open({ directory: true, multiple: false, title: t('settings.selectDataDirectory') })
    if (typeof path !== 'string') return
    if (!confirm(t('settings.moveDataConfirmation'))) return
    try {
      await api.moveDataDirectory(path)
    } catch (reason) {
      error.value = String(reason)
    }
  }

  function stopReindexPolling() {
    if (reindexPollTimer !== undefined) {
      window.clearInterval(reindexPollTimer)
      reindexPollTimer = undefined
    }
  }

  function startReindexPolling() {
    stopReindexPolling()
    reindexPollTimer = window.setInterval(() => {
      void refreshSemanticStatus()
    }, 1000)
  }

  async function refreshSemanticStatus() {
    try {
      const status = await api.getSemanticStatus()
      semanticStatus.value = status
      if (status.reindex) {
        reindexProgress.value = status.reindex
        if (status.reindex.stage === 'done' || status.reindex.stage === 'error' || status.reindex.stage === 'cancelled') {
          stopReindexPolling()
          semanticBusy.value = false
        }
      } else if (status.pending_chunks > 0 && reindexProgress.value && reindexProgress.value.stage !== 'done') {
        // Keep a live bar while the background worker continues embedding.
        const total = status.ready_chunks + status.pending_chunks
        reindexProgress.value = {
          stage: 'embedding',
          total_sessions: reindexProgress.value.total_sessions,
          processed_sessions: reindexProgress.value.processed_sessions,
          total_chunks: total,
          ready_chunks: status.ready_chunks,
          pending_chunks: status.pending_chunks,
          fraction: total > 0 ? 0.35 + (status.ready_chunks / total) * 0.65 : reindexProgress.value.fraction,
          message: status.message || t('progress.vectorizing', { ready: status.ready_chunks, total, pending: status.pending_chunks }),
        }
      } else if (status.pending_chunks === 0 && reindexProgress.value && reindexProgress.value.stage !== 'done') {
        reindexProgress.value = {
          ...reindexProgress.value,
          stage: 'done',
          ready_chunks: status.ready_chunks,
          pending_chunks: 0,
          total_chunks: status.ready_chunks,
          fraction: 1,
          message: status.message || t('progress.reindexCompleteReady', { ready: status.ready_chunks }),
        }
        stopReindexPolling()
        semanticBusy.value = false
      }
    } catch {
      semanticStatus.value = null
    }
  }

  async function checkEmbedding() {
    semanticBusy.value = true
    try {
      await api.checkEmbeddingBackend()
      await refreshSemanticStatus()
    } catch (reason) {
      error.value = String(reason)
    } finally {
      semanticBusy.value = false
    }
  }

  async function reindexSemantic() {
    semanticBusy.value = true
    reindexProgress.value = {
      stage: 'starting',
      total_sessions: 0,
      processed_sessions: 0,
      total_chunks: 0,
      ready_chunks: 0,
      pending_chunks: 0,
      fraction: 0,
      message: t('progress.startingReindex'),
    }
    try {
      unlistenReindex?.()
      unlistenReindex = await listen<ReindexProgress>('semantic-reindex-progress', (event) => {
        reindexProgress.value = event.payload
      })
      startReindexPolling()
      await api.reindexSemanticSearch()
      await refreshSemanticStatus()
      // Queueing finished; keep polling while embeddings continue in the worker.
      if ((semanticStatus.value?.pending_chunks ?? 0) > 0) {
        semanticBusy.value = true
        startReindexPolling()
      } else {
        stopReindexPolling()
        semanticBusy.value = false
        if (reindexProgress.value?.stage !== 'done') {
          reindexProgress.value = {
            stage: 'done',
            total_sessions: reindexProgress.value?.total_sessions ?? 0,
            processed_sessions: reindexProgress.value?.processed_sessions ?? 0,
            total_chunks: semanticStatus.value?.ready_chunks ?? 0,
            ready_chunks: semanticStatus.value?.ready_chunks ?? 0,
            pending_chunks: 0,
            fraction: 1,
            message: t('progress.reindexComplete'),
          }
        }
      }
    } catch (reason) {
      const message = String(reason)
      const cancelled = message.includes(t('progress.cancelMarker')) || message.toLowerCase().includes('cancel')
      if (!cancelled) error.value = message
      reindexProgress.value = {
        stage: cancelled ? 'cancelled' : 'error',
        total_sessions: reindexProgress.value?.total_sessions ?? 0,
        processed_sessions: reindexProgress.value?.processed_sessions ?? 0,
        total_chunks: reindexProgress.value?.total_chunks ?? 0,
        ready_chunks: reindexProgress.value?.ready_chunks ?? 0,
        pending_chunks: reindexProgress.value?.pending_chunks ?? 0,
        fraction: reindexProgress.value?.fraction ?? 0,
        message: cancelled ? t('progress.indexCancelled') : message,
      }
      stopReindexPolling()
      semanticBusy.value = false
    } finally {
      unlistenReindex?.()
      unlistenReindex = undefined
    }
  }

  async function downloadLocalModel() {
    semanticBusy.value = true
    downloadProgress.value = {
      stage: 'starting',
      file_index: 0,
      file_count: 3,
      downloaded_bytes: 0,
      fraction: 0,
      message: t('progress.preparingDownload'),
    }
    try {
      unlistenDownload?.()
      unlistenDownload = await listen<ModelDownloadProgress>('local-model-download-progress', (event) => {
        downloadProgress.value = event.payload
      })
      await api.downloadLocalEmbeddingModel()
      await refreshSemanticStatus()
      if (downloadProgress.value?.stage !== 'done') {
        downloadProgress.value = {
          stage: 'done',
          file_index: downloadProgress.value?.file_count ?? 3,
          file_count: downloadProgress.value?.file_count ?? 3,
          downloaded_bytes: downloadProgress.value?.downloaded_bytes ?? 0,
          total_bytes: downloadProgress.value?.total_bytes,
          fraction: 1,
          message: t('progress.downloadComplete'),
        }
      }
    } catch (reason) {
      const message = String(reason)
      const cancelled = message.includes(t('progress.cancelMarker')) || message.toLowerCase().includes('cancel')
      if (!cancelled) error.value = message
      downloadProgress.value = {
        stage: cancelled ? 'cancelled' : 'error',
        file_index: downloadProgress.value?.file_index ?? 0,
        file_count: downloadProgress.value?.file_count ?? 3,
        downloaded_bytes: downloadProgress.value?.downloaded_bytes ?? 0,
        total_bytes: downloadProgress.value?.total_bytes,
        fraction: downloadProgress.value?.fraction ?? 0,
        message: cancelled ? t('progress.downloadCancelled') : message,
      }
    } finally {
      unlistenDownload?.()
      unlistenDownload = undefined
      semanticBusy.value = false
    }
  }

  async function importLocalModel() {
    const path = await open({ directory: true, multiple: false, title: t('settings.selectModelDirectory') })
    if (typeof path !== 'string') return
    semanticBusy.value = true
    try {
      await api.importLocalEmbeddingModel(path)
      settings.value = await api.getSettings()
      await refreshSemanticStatus()
    } catch (reason) {
      error.value = String(reason)
    } finally {
      semanticBusy.value = false
    }
  }


  async function cancelSemanticWork() {
    try {
      await api.cancelSemanticWork()
      if (downloadProgress.value && downloadProgress.value.stage !== 'done') {
        downloadProgress.value = {
          stage: 'cancelled',
          file_index: downloadProgress.value.file_index ?? 0,
          file_count: downloadProgress.value.file_count ?? 3,
          downloaded_bytes: downloadProgress.value.downloaded_bytes ?? 0,
          total_bytes: downloadProgress.value.total_bytes,
          fraction: downloadProgress.value.fraction ?? 0,
          message: t('progress.downloadCancelled'),
        }
      }
      if (reindexProgress.value && reindexProgress.value.stage !== 'done') {
        reindexProgress.value = {
          stage: 'cancelled',
          total_sessions: reindexProgress.value.total_sessions ?? 0,
          processed_sessions: reindexProgress.value.processed_sessions ?? 0,
          total_chunks: reindexProgress.value.total_chunks ?? 0,
          ready_chunks: reindexProgress.value.ready_chunks ?? 0,
          pending_chunks: reindexProgress.value.pending_chunks ?? 0,
          fraction: reindexProgress.value.fraction ?? 0,
          message: t('progress.indexCancelled'),
        }
      }
      stopReindexPolling()
      await refreshSemanticStatus()
    } catch (reason) {
      error.value = String(reason)
    } finally {
      semanticBusy.value = false
      unlistenDownload?.()
      unlistenDownload = undefined
      unlistenReindex?.()
      unlistenReindex = undefined
    }
  }


  async function copyMcpConfig() {
    clearMcpConfigCopiedTimer()
    const url = settingsApiStatus.value?.mcp_url
    const secret = settings.value.secret_enabled ? settings.value.secret : undefined
    await navigator.clipboard.writeText(url ? buildMcpClientConfig(url, secret) : buildMcpClientConfig(undefined, secret))
    mcpConfigCopied.value = true
    mcpConfigCopiedTimer = window.setTimeout(() => {
      mcpConfigCopied.value = false
      mcpConfigCopiedTimer = undefined
    }, 2200)
  }

  function dispose() {
    clearMcpConfigCopiedTimer()
    stopReindexPolling()
    unlistenDownload?.()
    unlistenDownload = undefined
    unlistenReindex?.()
    unlistenReindex = undefined
  }

  return {
    showSettings, originText, secretCopied, mcpConfigCopied, settingsApiStatus, semanticStatus, semanticBusy, downloadProgress, reindexProgress,
    openSettings, closeSettings, saveSettings, rotateSecret, copySecret, copyMcpConfig, changeDataDirectory,
    checkEmbedding, reindexSemantic, downloadLocalModel, importLocalModel, cancelSemanticWork, dispose,
  }
}
