import { open } from '@tauri-apps/plugin-dialog'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { ref, type Ref } from 'vue'
import {
  desktopApi,
  type DesktopApi,
  type ModelDownloadProgress,
  type ReindexProgress,
  type SemanticRuntimeStatus,
  type SettingsModel,
} from '../desktop-api'

type ThemePreview = { begin(): void; accept(): void; cancel(): void }

export function useSettings(
  settings: Ref<SettingsModel>,
  error: Ref<string>,
  theme: ThemePreview,
  api: DesktopApi = desktopApi,
) {
  const showSettings = ref(false)
  const originText = ref('')
  const secretCopied = ref(false)
  const semanticStatus = ref<SemanticRuntimeStatus | null>(null)
  const semanticBusy = ref(false)
  const downloadProgress = ref<ModelDownloadProgress | null>(null)
  const reindexProgress = ref<ReindexProgress | null>(null)
  let unlistenDownload: UnlistenFn | undefined
  let unlistenReindex: UnlistenFn | undefined
  let reindexPollTimer: number | undefined

  async function openSettings() {
    settings.value = await api.getSettings()
    theme.begin()
    originText.value = settings.value.allowed_origins.join('\n')
    secretCopied.value = false
    showSettings.value = true
    try {
      await refreshSemanticStatus()
      if (semanticStatus.value?.reindex && semanticStatus.value.reindex.stage !== 'done') {
        startReindexPolling()
      }
    } catch {
      semanticStatus.value = null
    }
  }

  function closeSettings(save = false) {
    if (!save) theme.cancel()
    showSettings.value = false
  }

  async function saveSettings() {
    settings.value.allowed_origins = originText.value.split('\n').map((value) => value.trim()).filter(Boolean)
    settings.value.setup_complete = true
    try {
      settings.value = await api.saveSettings(settings.value)
      theme.accept()
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
    const path = await open({ directory: true, multiple: false, title: '选择数据保存目录' })
    if (typeof path !== 'string') return
    if (!confirm('应用将把当前数据库复制到新目录并立即重启。是否继续？')) return
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
        if (status.reindex.stage === 'done' || status.reindex.stage === 'error') {
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
          message: status.message || `正在向量化（就绪 ${status.ready_chunks}/${total}，剩余 ${status.pending_chunks}）`,
        }
      } else if (status.pending_chunks === 0 && reindexProgress.value && reindexProgress.value.stage !== 'done') {
        reindexProgress.value = {
          ...reindexProgress.value,
          stage: 'done',
          ready_chunks: status.ready_chunks,
          pending_chunks: 0,
          total_chunks: status.ready_chunks,
          fraction: 1,
          message: status.message || `重建索引完成（就绪 ${status.ready_chunks}）`,
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
      message: '正在启动重建索引…',
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
            message: '重建索引完成',
          }
        }
      }
    } catch (reason) {
      error.value = String(reason)
      reindexProgress.value = {
        stage: 'error',
        total_sessions: reindexProgress.value?.total_sessions ?? 0,
        processed_sessions: reindexProgress.value?.processed_sessions ?? 0,
        total_chunks: reindexProgress.value?.total_chunks ?? 0,
        ready_chunks: reindexProgress.value?.ready_chunks ?? 0,
        pending_chunks: reindexProgress.value?.pending_chunks ?? 0,
        fraction: reindexProgress.value?.fraction ?? 0,
        message: String(reason),
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
      message: '准备下载本地模型…',
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
          message: '下载完成',
        }
      }
    } catch (reason) {
      error.value = String(reason)
      downloadProgress.value = {
        stage: 'error',
        file_index: downloadProgress.value?.file_index ?? 0,
        file_count: downloadProgress.value?.file_count ?? 3,
        downloaded_bytes: downloadProgress.value?.downloaded_bytes ?? 0,
        total_bytes: downloadProgress.value?.total_bytes,
        fraction: downloadProgress.value?.fraction ?? 0,
        message: String(reason),
      }
    } finally {
      unlistenDownload?.()
      unlistenDownload = undefined
      semanticBusy.value = false
    }
  }

  async function importLocalModel() {
    const path = await open({ directory: true, multiple: false, title: '选择本地 embedding 模型目录' })
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

  return {
    showSettings, originText, secretCopied, semanticStatus, semanticBusy, downloadProgress, reindexProgress,
    openSettings, closeSettings, saveSettings, rotateSecret, copySecret, changeDataDirectory,
    checkEmbedding, reindexSemantic, downloadLocalModel, importLocalModel,
  }
}
