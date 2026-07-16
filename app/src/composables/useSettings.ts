import { open } from '@tauri-apps/plugin-dialog'
import { ref, type Ref } from 'vue'
import { desktopApi, type DesktopApi, type SemanticRuntimeStatus, type SettingsModel } from '../desktop-api'

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

  async function openSettings() {
    settings.value = await api.getSettings()
    theme.begin()
    originText.value = settings.value.allowed_origins.join('\n')
    secretCopied.value = false
    showSettings.value = true
    try {
      semanticStatus.value = await api.getSemanticStatus()
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


  async function refreshSemanticStatus() {
    try {
      semanticStatus.value = await api.getSemanticStatus()
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
    try {
      await api.reindexSemanticSearch()
      await refreshSemanticStatus()
    } catch (reason) {
      error.value = String(reason)
    } finally {
      semanticBusy.value = false
    }
  }

  async function downloadLocalModel() {
    semanticBusy.value = true
    try {
      await api.downloadLocalEmbeddingModel()
      await refreshSemanticStatus()
    } catch (reason) {
      error.value = String(reason)
    } finally {
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

  return { showSettings, originText, secretCopied, semanticStatus, semanticBusy, openSettings, closeSettings, saveSettings, rotateSecret, copySecret, changeDataDirectory, checkEmbedding, reindexSemantic, downloadLocalModel, importLocalModel }
}
