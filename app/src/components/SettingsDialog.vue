<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  Cable,
  Cloud,
  Check,
  Clipboard,
  FolderOpen,
  Monitor,
  Moon,
  Palette,
  RefreshCw,
  Search,
  ShieldCheck,
  SlidersHorizontal,
  Sun,
  Plus,
  Edit3,
  Copy,
  Trash2,
} from 'lucide-vue-next'
import AppSelect, { type SelectOption } from './AppSelect.vue'
import CloudSyncSettings from './CloudSyncSettings.vue'
import ThemeEditorDialog from './ThemeEditorDialog.vue'
import { cloudSyncProfileSnapshot, cloudSyncProfilesEqual } from '../cloud-sync-profile'
import {
  getCategorizedThemes,
  duplicateThemeAsCustom,
  createDefaultCustomTheme,
  type ThemeDefinition,
} from '../theme'
import type {
  ApiStatus,
  EmbeddingBackendKind,
  LocalEmbeddingDevice,
  LocalEmbeddingDType,
  LanguagePreference,
  ModelDownloadProgress,
  ReindexProgress,
  SearchMode,
  SemanticRuntimeStatus,
  SettingsModel,
  ThemePreference,
  CloudSyncStatus,
  CloudCredentialInput,
  CloudSyncSettings as CloudSyncSettingsModel,
  CloudConnectionTestResult,
} from '../desktop-api'
import { translate as t } from '../i18n'

type ConnectionTestState = 'idle' | 'pending' | 'passed' | 'failed'
type CloudConnectionTestHandler = (
  settings: CloudSyncSettingsModel,
  credentials: CloudCredentialInput,
  requestId: number,
) => Promise<CloudConnectionTestResult>

const props = defineProps<{
  visible: boolean
  secretCopied: boolean
  mcpConfigCopied?: boolean
  apiStatus?: ApiStatus | null
  semanticStatus?: SemanticRuntimeStatus | null
  semanticBusy?: boolean
  downloadProgress?: ModelDownloadProgress | null
  reindexProgress?: ReindexProgress | null
  cloudSyncStatus?: CloudSyncStatus | null
  cloudSyncBusy?: boolean
  activeCloudSyncProfile?: CloudSyncSettingsModel | null
  onCloudSyncTest?: CloudConnectionTestHandler
}>()
const settings = defineModel<SettingsModel>('settings', { required: true })
const originText = defineModel<string>('originText', { required: true })
type SettingsPage = 'general' | 'appearance' | 'semantic' | 'connections' | 'cloud-sync'

const activePage = ref<SettingsPage>('general')
const cloudPassword = ref('')
const syncPassword = ref('')
const s3AccessKeyId = ref('')
const s3SecretAccessKey = ref('')
const s3SessionToken = ref('')
const connectionTestState = ref<ConnectionTestState>('idle')
const connectionTestMessage = ref('')
const testedConnectionFingerprint = ref<string | null>(null)
let latestConnectionTestRequestId = 0

watch(() => props.visible, (visible) => {
  if (visible) {
    activePage.value = 'general'
    connectionTestState.value = 'idle'
    connectionTestMessage.value = ''
    testedConnectionFingerprint.value = null
  } else {
    cloudPassword.value = ''
    syncPassword.value = ''
    pendingDeleteThemeId.value = null
    s3AccessKeyId.value = ''
    s3SecretAccessKey.value = ''
    s3SessionToken.value = ''
    latestConnectionTestRequestId += 1
    connectionTestState.value = 'idle'
    connectionTestMessage.value = ''
    testedConnectionFingerprint.value = null
  }
})

const emit = defineEmits<{
  close: []
  save: [credentials: CloudCredentialInput | null]
  previewTheme: [theme: ThemePreference, lightId?: string, darkId?: string]
  previewThemeId: [id: string, isDark: boolean]
  previewLanguage: [language: LanguagePreference]
  changeDataDirectory: []
  copySecret: []
  rotateSecret: []
  copyMcpConfig: []
  checkEmbedding: []
  reindexSemantic: []
  downloadLocalModel: []
  importLocalModel: []
  cancelSemanticWork: []
  cloudSyncNow: []
  cloudSyncRewrite: []
  cloudSyncRemoveDevice: [deviceId: string]
}>()

function cloudCredentials(): CloudCredentialInput {
  if (settings.value.cloud_sync.backend === 's3') {
    return {
      backend: 's3',
      access_key_id: s3AccessKeyId.value,
      secret_access_key: s3SecretAccessKey.value,
      session_token: s3SessionToken.value || null,
      sync_password: syncPassword.value || null,
    }
  }
  return {
    backend: 'webdav',
    password: cloudPassword.value,
    sync_password: syncPassword.value || null,
  }
}

function currentConnectionFingerprint(): string {
  const syncPasswordFingerprint = settings.value.cloud_sync.encryption_enabled
    ? syncPassword.value
    : ''
  const credentials = settings.value.cloud_sync.backend === 's3'
    ? {
        accessKeyId: s3AccessKeyId.value,
        secretAccessKey: s3SecretAccessKey.value,
        sessionToken: s3SessionToken.value,
        syncPassword: syncPasswordFingerprint,
      }
    : {
        webdavPassword: cloudPassword.value,
        syncPassword: syncPasswordFingerprint,
      }
  return JSON.stringify({
    settings: cloudSyncProfileSnapshot(settings.value.cloud_sync),
    credentials,
  })
}

const activeCloudProfileMatches = computed(() => {
  const activeProfile = props.activeCloudSyncProfile
  return !activeProfile || cloudSyncProfilesEqual(settings.value.cloud_sync, activeProfile)
})

const hasDraftCredentials = computed(() => {
  const hasSyncPassword = settings.value.cloud_sync.encryption_enabled && Boolean(syncPassword.value)
  if (settings.value.cloud_sync.backend === 's3') {
    return hasSyncPassword
      || Boolean(s3AccessKeyId.value || s3SecretAccessKey.value || s3SessionToken.value)
  }
  return hasSyncPassword || Boolean(cloudPassword.value)
})

const canSave = computed(() => {
  if (!settings.value.cloud_sync.enabled) return true
  if (connectionTestState.value === 'pending' || connectionTestState.value === 'failed') return false
  if (connectionTestState.value === 'passed') {
    return testedConnectionFingerprint.value === currentConnectionFingerprint()
  }
  return activeCloudProfileMatches.value
    && settings.value.cloud_sync.connection_verified
    && !hasDraftCredentials.value
})

function cloneCloudSettings(value: CloudSyncSettingsModel): CloudSyncSettingsModel {
  return { ...value, s3: { ...value.s3 } }
}

async function testCloudConnection() {
  if (connectionTestState.value === 'pending') return
  const testHandler = props.onCloudSyncTest
  if (!testHandler) {
    connectionTestState.value = 'failed'
    connectionTestMessage.value = t('cloudSync.connectionHandlerMissing')
    testedConnectionFingerprint.value = null
    return
  }

  const requestId = ++latestConnectionTestRequestId
  const requestFingerprint = currentConnectionFingerprint()
  connectionTestState.value = 'pending'
  connectionTestMessage.value = ''
  testedConnectionFingerprint.value = null
  try {
    const result = await testHandler(
      cloneCloudSettings(settings.value.cloud_sync),
      cloudCredentials(),
      requestId,
    )
    if (requestId !== latestConnectionTestRequestId || requestFingerprint !== currentConnectionFingerprint()) return
    if (!result.ok) {
      connectionTestState.value = 'failed'
      connectionTestMessage.value = result.message
      return
    }
    settings.value.cloud_sync = cloneCloudSettings(result.cloud_sync)
    testedConnectionFingerprint.value = currentConnectionFingerprint()
    connectionTestState.value = 'passed'
    connectionTestMessage.value = result.message
  } catch (reason) {
    if (requestId !== latestConnectionTestRequestId || requestFingerprint !== currentConnectionFingerprint()) return
    connectionTestState.value = 'failed'
    connectionTestMessage.value = String(reason)
    testedConnectionFingerprint.value = null
  }
}

watch(currentConnectionFingerprint, (currentFingerprint) => {
  if (connectionTestState.value === 'idle') return
  if (
    connectionTestState.value === 'passed'
    && testedConnectionFingerprint.value === currentFingerprint
  ) return
  latestConnectionTestRequestId += 1
  connectionTestState.value = 'idle'
  connectionTestMessage.value = ''
  testedConnectionFingerprint.value = null
})

function mcpStateLabel(status: ApiStatus | null | undefined): string {
  const state = status?.mcp?.state
  if (state === 'running') return t('settings.stateRunning')
  if (state === 'starting') return t('settings.stateStarting')
  if (state === 'failed') {
    const message = status?.mcp && 'message' in status.mcp ? status.mcp.message : undefined
    return message ? t('settings.stateErrorWithMessage', { message }) : t('settings.stateError')
  }
  return t('settings.stateStopped')
}

function setBackend(backend: EmbeddingBackendKind) {
  settings.value.semantic_search.backend = backend
}

function setMode(mode: SearchMode) {
  settings.value.semantic_search.default_mode = mode
}

if (!settings.value.semantic_search.local.device) {
  settings.value.semantic_search.local.device = 'auto' as LocalEmbeddingDevice
}
if (!settings.value.semantic_search.local.dtype) {
  settings.value.semantic_search.local.dtype = 'auto' as LocalEmbeddingDType
}

const languageOptions = computed<SelectOption<LanguagePreference>[]>(() => [
  { value: 'system', label: t('settings.language.system') },
  { value: 'zh-CN', label: t('settings.language.zhCN') },
  { value: 'en-US', label: t('settings.language.enUS') },
])

const closeBehaviorOptions = computed<SelectOption<string>[]>(() => [
  { value: 'ask', label: t('settings.closeAsk') },
  { value: 'hide_to_tray', label: t('settings.closeTray') },
  { value: 'exit', label: t('settings.closeExit') },
])

const trayClickOptions = computed<SelectOption<string>[]>(() => [
  { value: 'show_menu', label: t('settings.trayMenu') },
  { value: 'open_window', label: t('settings.trayOpen') },
  { value: 'no_action', label: t('settings.trayNone') },
])

const searchModeOptions = computed<SelectOption<SearchMode>[]>(() => [
  { value: 'hybrid', label: t('searchMode.hybrid') },
  { value: 'semantic', label: t('searchMode.semantic') },
  { value: 'keyword', label: t('searchMode.keyword') },
])

const backendOptions = computed<SelectOption<EmbeddingBackendKind>[]>(() => [
  { value: 'local', label: t('settings.localBackend') },
  { value: 'ollama', label: 'Ollama' },
  { value: 'llama_cpp', label: 'llama.cpp' },
  { value: 'openai_compatible', label: t('settings.openAiCompatible') },
])

const deviceOptions = computed<SelectOption<LocalEmbeddingDevice>[]>(() => [
  { value: 'auto', label: t('settings.automatic') },
  { value: 'cuda', label: 'CUDA' },
  { value: 'cpu', label: 'CPU' },
])

const dtypeOptions = computed<SelectOption<LocalEmbeddingDType>[]>(() => [
  { value: 'auto', label: t('settings.automatic') },
  { value: 'f16', label: 'F16' },
  { value: 'f32', label: 'F32' },
])
const pages: SettingsPage[] = ['general', 'appearance', 'semantic', 'connections', 'cloud-sync']
const activePageIndex = computed(() => pages.indexOf(activePage.value))

const showThemeEditor = ref(false)
const editingTheme = ref<ThemeDefinition | null>(null)
const isCreatingNewTheme = ref(false)

const categorizedThemes = computed(() => getCategorizedThemes(settings.value.custom_themes || []))
const lightThemesList = computed(() => [...categorizedThemes.value.customLight, ...categorizedThemes.value.presetLight])
const darkThemesList = computed(() => [...categorizedThemes.value.customDark, ...categorizedThemes.value.presetDark])

function onThemeModeChange(mode: ThemePreference) {
  settings.value.theme = mode
  emit('previewTheme', mode, settings.value.light_theme_id, settings.value.dark_theme_id)
}

function onSelectTheme(themeDef: ThemeDefinition) {
  if (themeDef.isDark) {
    settings.value.dark_theme_id = themeDef.id
    if (settings.value.theme !== 'dark') {
      settings.value.theme = 'dark'
    }
  } else {
    settings.value.light_theme_id = themeDef.id
    if (settings.value.theme !== 'light') {
      settings.value.theme = 'light'
    }
  }
  emit('previewThemeId', themeDef.id, themeDef.isDark)
}

function openCreateTheme(isDark = false) {
  editingTheme.value = createDefaultCustomTheme(isDark)
  isCreatingNewTheme.value = true
  showThemeEditor.value = true
}

function openEditTheme(themeDef: ThemeDefinition, e?: Event) {
  e?.stopPropagation()
  editingTheme.value = themeDef
  isCreatingNewTheme.value = false
  showThemeEditor.value = true
}

function openDuplicateTheme(themeDef: ThemeDefinition, e?: Event) {
  e?.stopPropagation()
  const cloned = duplicateThemeAsCustom(themeDef)
  editingTheme.value = cloned
  isCreatingNewTheme.value = true
  showThemeEditor.value = true
}

function onSaveCustomTheme(theme: ThemeDefinition, activate: boolean) {
  const currentCustom = [...(settings.value.custom_themes || [])]
  const idx = currentCustom.findIndex((t) => t.id === theme.id)
  if (idx >= 0) {
    currentCustom[idx] = { ...theme, isCustom: true }
  } else {
    currentCustom.unshift({ ...theme, isCustom: true })
  }
  settings.value.custom_themes = currentCustom

  if (activate) {
    onSelectTheme(theme)
  } else {
    const activeId = theme.isDark ? settings.value.dark_theme_id : settings.value.light_theme_id
    if (activeId === theme.id) {
      emit('previewThemeId', theme.id, theme.isDark)
    }
  }
}

// Delete confirmation is a two-step inline state on the card's trash icon:
// the first click arms the pending id, the second click on the same icon
// actually deletes. A stray hover-click can no longer destroy a theme.
const pendingDeleteThemeId = ref<string | null>(null)

function onRequestDeleteCustomTheme(id: string) {
  if (pendingDeleteThemeId.value !== id) {
    pendingDeleteThemeId.value = id
    return
  }
  pendingDeleteThemeId.value = null
  onDeleteCustomTheme(id)
}

function onDeleteCustomTheme(id: string) {
  const currentCustom = (settings.value.custom_themes || []).filter((t) => t.id !== id)
  settings.value.custom_themes = currentCustom

  if (settings.value.light_theme_id === id) {
    settings.value.light_theme_id = 'green'
  }
  if (settings.value.dark_theme_id === id) {
    settings.value.dark_theme_id = 'black'
  }
  emit('previewTheme', settings.value.theme, settings.value.light_theme_id, settings.value.dark_theme_id)
}
</script>

<template>
  <Transition name="settings-modal">
    <div v-if="visible" class="dialog-backdrop settings-dialog-backdrop" @click.self="emit('close')">
      <section class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header><div><h2 id="settings-title">{{ t('settings.title') }}</h2><p>{{ t('settings.subtitle') }}</p></div></header>
        <div class="settings-layout">
          <nav class="settings-navigation" :aria-label="t('settings.categories')">
            <span class="settings-nav-highlight" :style="{ '--nav-index': activePageIndex }" aria-hidden="true"></span>
            <button
              id="settings-navigation-general"
              type="button"
              class="settings-navigation__button"
              :class="{ active: activePage === 'general' }"
              :aria-current="activePage === 'general' ? 'page' : undefined"
              aria-controls="settings-page-general"
              @click="activePage = 'general'"
            >
              <SlidersHorizontal :size="16" aria-hidden="true" />
              <span>{{ t('settings.general') }}</span>
            </button>
            <button
              id="settings-navigation-appearance"
              type="button"
              class="settings-navigation__button"
              :class="{ active: activePage === 'appearance' }"
              :aria-current="activePage === 'appearance' ? 'page' : undefined"
              aria-controls="settings-page-appearance"
              @click="activePage = 'appearance'"
            >
              <Palette :size="16" aria-hidden="true" />
              <span>{{ t('settings.appearance') }}</span>
            </button>
            <button
              id="settings-navigation-semantic"
              type="button"
              class="settings-navigation__button"
              :class="{ active: activePage === 'semantic' }"
              :aria-current="activePage === 'semantic' ? 'page' : undefined"
              aria-controls="settings-page-semantic"
              @click="activePage = 'semantic'"
            >
              <Search :size="16" aria-hidden="true" />
              <span>{{ t('settings.semantic') }}</span>
            </button>
            <button
              id="settings-navigation-connections"
              type="button"
              class="settings-navigation__button"
              :class="{ active: activePage === 'connections' }"
              :aria-current="activePage === 'connections' ? 'page' : undefined"
              aria-controls="settings-page-connections"
              @click="activePage = 'connections'"
            >
              <Cable :size="16" aria-hidden="true" />
              <span>{{ t('settings.connections') }}</span>
            </button>
            <button
              id="settings-navigation-cloud-sync"
              type="button"
              class="settings-navigation__button"
              :class="{ active: activePage === 'cloud-sync' }"
              :aria-current="activePage === 'cloud-sync' ? 'page' : undefined"
              aria-controls="settings-page-cloud-sync"
              @click="activePage = 'cloud-sync'"
            ><Cloud :size="16" aria-hidden="true" /><span>{{ t('cloudSync.title') }}</span></button>
          </nav>
          <div class="settings-pages">
            <section
              v-show="activePage === 'general'"
              id="settings-page-general"
              class="settings-content settings-page"
              role="region"
              aria-labelledby="settings-navigation-general"
            >
              <section class="setting-group behavior-settings">
                <div><h3>{{ t('settings.language.title') }}</h3><p>{{ t('settings.language.description') }}</p></div>
                <label>
                  <span>{{ t('settings.language.title') }}</span>
                  <AppSelect
                    v-model="settings.language"
                    :options="languageOptions"
                    block
                    @update:model-value="emit('previewLanguage', settings.language)"
                  />
                </label>
              </section>
              <section class="setting-group">
                <div class="setting-row"><div><h3>{{ t('settings.dataLocation') }}</h3><p class="path-value">{{ settings.data_directory || t('settings.defaultDataLocation') }}</p></div><button class="secondary-button" @click="emit('changeDataDirectory')">{{ t('settings.changeLocation') }}</button></div>
              </section>
              <section class="setting-group behavior-settings">
                <label><span>{{ t('settings.closeBehavior') }}</span><AppSelect v-model="settings.close_behavior" :options="closeBehaviorOptions" block /></label>
                <label><span>{{ t('settings.trayClick') }}</span><AppSelect v-model="settings.tray_click_behavior" :options="trayClickOptions" block /></label>
              </section>
            </section>

            <section
              v-show="activePage === 'appearance'"
              id="settings-page-appearance"
              class="settings-content settings-page"
              role="region"
              aria-labelledby="settings-navigation-appearance"
            >
              <section class="setting-group theme-setting">
                <div><h3>{{ t('settings.appearanceTitle') }}</h3><p>{{ t('settings.appearanceDescription') }}</p></div>
                <div class="theme-options" role="radiogroup" :aria-label="t('settings.themeGroup')">
                  <button :class="{ active: settings.theme === 'system' }" role="radio" :aria-checked="settings.theme === 'system'" @click="onThemeModeChange('system')"><Monitor :size="16" /><span>{{ t('settings.themeSystem') }}</span></button>
                  <button :class="{ active: settings.theme === 'light' }" role="radio" :aria-checked="settings.theme === 'light'" @click="onThemeModeChange('light')"><Sun :size="16" /><span>{{ t('settings.themeLight') }}</span></button>
                  <button :class="{ active: settings.theme === 'dark' }" role="radio" :aria-checked="settings.theme === 'dark'" @click="onThemeModeChange('dark')"><Moon :size="16" /><span>{{ t('settings.themeDark') }}</span></button>
                </div>
              </section>

              <section v-show="settings.theme !== 'dark'" class="setting-group theme-palette-group">
                <div class="setting-heading theme-section-heading">
                  <div>
                    <h3>{{ t('settings.lightThemesTitle') }}</h3>
                    <p>{{ t('settings.lightThemesDescription') }}</p>
                  </div>
                  <div class="theme-heading-actions">
                    <button
                      type="button"
                      class="theme-btn-action"
                      @click="openCreateTheme(false)"
                    >
                      <Plus :size="13" />
                      <span>{{ t('settings.newThemeButton') }}</span>
                    </button>
                  </div>
                </div>
                <div class="theme-card-grid" role="radiogroup" :aria-label="t('settings.lightThemesTitle')">
                  <div
                    v-for="item in lightThemesList"
                    :key="item.id"
                    class="theme-card"
                    :class="{ active: (settings.light_theme_id || 'green') === item.id }"
                    :aria-checked="(settings.light_theme_id || 'green') === item.id"
                    role="radio"
                    tabindex="0"
                    @click="onSelectTheme(item)"
                    @keydown.enter.space.prevent="onSelectTheme(item)"
                  >
                    <!-- Custom Badge -->
                    <span v-if="item.isCustom" class="theme-card-custom-badge">
                      {{ t('settings.customThemeBadge') }}
                    </span>

                    <!-- Action buttons -->
                    <div class="theme-card-actions" @click.stop>
                      <button
                        v-if="item.isCustom"
                        type="button"
                        class="theme-card-icon-btn"
                        :title="t('settings.editTheme')"
                        @click="openEditTheme(item, $event)"
                      >
                        <Edit3 :size="12" />
                      </button>
                      <button
                        type="button"
                        class="theme-card-icon-btn"
                        :title="t('settings.duplicateTheme')"
                        @click="openDuplicateTheme(item, $event)"
                      >
                        <Copy :size="12" />
                      </button>
                      <button
                        v-if="item.isCustom"
                        type="button"
                        :class="['theme-card-icon-btn', 'is-danger', { 'is-confirming': pendingDeleteThemeId === item.id }]"
                        :title="pendingDeleteThemeId === item.id ? t('settings.deleteThemeConfirm') : t('settings.deleteTheme')"
                        @click="onRequestDeleteCustomTheme(item.id)"
                      >
                        <Trash2 :size="12" />
                      </button>
                    </div>

                    <div class="theme-card-preview" :style="{ '--preview-color': item.config.primary, '--preview-bg': item.config.extInfo?.['--color-app-background'] || '#f7f9fa' }">
                      <span class="theme-card-swatch"></span>
                      <span class="theme-card-accent"></span>
                    </div>
                    <div class="theme-card-info">
                      <strong>{{ item.nameKey ? t(item.nameKey) : item.name }}</strong>
                      <span v-if="(settings.light_theme_id || 'green') === item.id" class="theme-card-badge">{{ t('settings.selectedTheme') }}</span>
                    </div>
                  </div>
                </div>
              </section>

              <section v-show="settings.theme !== 'light'" class="setting-group theme-palette-group">
                <div class="setting-heading theme-section-heading">
                  <div>
                    <h3>{{ t('settings.darkThemesTitle') }}</h3>
                    <p>{{ t('settings.darkThemesDescription') }}</p>
                  </div>
                  <div class="theme-heading-actions">
                    <button
                      type="button"
                      class="theme-btn-action"
                      @click="openCreateTheme(true)"
                    >
                      <Plus :size="13" />
                      <span>{{ t('settings.newThemeButton') }}</span>
                    </button>
                  </div>
                </div>
                <div class="theme-card-grid" role="radiogroup" :aria-label="t('settings.darkThemesTitle')">
                  <div
                    v-for="item in darkThemesList"
                    :key="item.id"
                    class="theme-card is-dark-card"
                    :class="{ active: (settings.dark_theme_id || 'black') === item.id }"
                    :aria-checked="(settings.dark_theme_id || 'black') === item.id"
                    role="radio"
                    tabindex="0"
                    @click="onSelectTheme(item)"
                    @keydown.enter.space.prevent="onSelectTheme(item)"
                  >
                    <!-- Custom Badge -->
                    <span v-if="item.isCustom" class="theme-card-custom-badge">
                      {{ t('settings.customThemeBadge') }}
                    </span>

                    <!-- Action buttons -->
                    <div class="theme-card-actions" @click.stop>
                      <button
                        v-if="item.isCustom"
                        type="button"
                        class="theme-card-icon-btn"
                        :title="t('settings.editTheme')"
                        @click="openEditTheme(item, $event)"
                      >
                        <Edit3 :size="12" />
                      </button>
                      <button
                        type="button"
                        class="theme-card-icon-btn"
                        :title="t('settings.duplicateTheme')"
                        @click="openDuplicateTheme(item, $event)"
                      >
                        <Copy :size="12" />
                      </button>
                      <button
                        v-if="item.isCustom"
                        type="button"
                        :class="['theme-card-icon-btn', 'is-danger', { 'is-confirming': pendingDeleteThemeId === item.id }]"
                        :title="pendingDeleteThemeId === item.id ? t('settings.deleteThemeConfirm') : t('settings.deleteTheme')"
                        @click="onRequestDeleteCustomTheme(item.id)"
                      >
                        <Trash2 :size="12" />
                      </button>
                    </div>

                    <div class="theme-card-preview" :style="{ '--preview-color': item.config.primary, '--preview-bg': item.config.extInfo?.['--color-app-background'] || '#171b1e' }">
                      <span class="theme-card-swatch"></span>
                      <span class="theme-card-accent"></span>
                    </div>
                    <div class="theme-card-info">
                      <strong>{{ item.nameKey ? t(item.nameKey) : item.name }}</strong>
                      <span v-if="(settings.dark_theme_id || 'black') === item.id" class="theme-card-badge">{{ t('settings.selectedTheme') }}</span>
                    </div>
                  </div>
                </div>
              </section>
            </section>

            <section
              v-show="activePage === 'semantic'"
              id="settings-page-semantic"
              class="settings-content settings-page"
              role="region"
              aria-labelledby="settings-navigation-semantic"
            >
          <section class="setting-group">
            <div class="setting-row">
              <div>
                <h3>{{ t('settings.semantic') }}</h3>
                <p>{{ t('settings.semanticDescription') }}</p>
              </div>
              <label class="switch"><input v-model="settings.semantic_search.enabled" type="checkbox" /><span></span></label>
            </div>
            <Transition name="setting-expand">
              <div class="semantic-settings" v-if="settings.semantic_search.enabled">
                <label>
                  <span>{{ t('settings.defaultMode') }}</span>
                  <AppSelect
                    :model-value="settings.semantic_search.default_mode"
                    :options="searchModeOptions"
                    block
                    @update:model-value="setMode($event as SearchMode)"
                  />
                </label>
                <label>
                  <span>{{ t('settings.embeddingBackend') }}</span>
                  <AppSelect
                    :model-value="settings.semantic_search.backend"
                    :options="backendOptions"
                    block
                    @update:model-value="setBackend($event as EmbeddingBackendKind)"
                  />
                </label>
                <template v-if="settings.semantic_search.backend === 'local'">
                  <label><span>{{ t('settings.modelId') }}</span><input v-model="settings.semantic_search.local.model" /></label>
                  <label>
                    <span>{{ t('settings.device') }}</span>
                    <AppSelect
                      v-model="settings.semantic_search.local.device"
                      :options="deviceOptions"
                      block
                    />
                  </label>
                  <label>
                    <span>{{ t('settings.precision') }}</span>
                    <AppSelect
                      v-model="settings.semantic_search.local.dtype"
                      :options="dtypeOptions"
                      block
                    />
                  </label>
                  <p class="path-value">{{ t('settings.localDirectory', { path: semanticStatus?.local_model_path || settings.semantic_search.local.model_path || t('settings.notPrepared') }) }}</p>
                  <p class="path-value">{{ t('settings.downloadSource', { model: settings.semantic_search.local.model }) }}</p>
                  <p class="path-value">{{ t('settings.currentDevice', { device: semanticStatus?.device || t('settings.notLoaded'), dtype: semanticStatus?.dtype || t('settings.notLoaded') }) }}</p>
                  <div class="setting-actions">
                    <button class="secondary-button compact" :disabled="semanticBusy && !!downloadProgress && downloadProgress.stage !== 'done' && downloadProgress.stage !== 'error' && downloadProgress.stage !== 'cancelled'" @click="emit('downloadLocalModel')">{{ semanticBusy && downloadProgress && downloadProgress.stage !== 'done' && downloadProgress.stage !== 'error' && downloadProgress.stage !== 'cancelled' ? t('settings.downloading') : t('settings.downloadModel') }}</button>
                    <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('importLocalModel')"><FolderOpen :size="14" />{{ t('settings.importModel') }}</button>
                    <button v-if="semanticBusy && downloadProgress && downloadProgress.stage !== 'done' && downloadProgress.stage !== 'error' && downloadProgress.stage !== 'cancelled'" class="secondary-button compact" @click="emit('cancelSemanticWork')">{{ t('settings.cancelDownload') }}</button>
                  </div>
                  <Transition name="setting-expand">
                    <div v-if="downloadProgress" class="download-progress" :data-stage="downloadProgress.stage">
                      <div class="download-progress-meta">
                        <span>{{ downloadProgress.message }}</span>
                        <strong>{{ Math.round((downloadProgress.fraction || 0) * 100) }}%</strong>
                      </div>
                      <div class="download-progress-track" aria-hidden="true">
                        <i :style="{ width: `${Math.max(2, Math.round((downloadProgress.fraction || 0) * 100))}%` }"></i>
                      </div>
                      <p v-if="downloadProgress.file" class="path-value">{{ t('settings.fileProgress', { index: downloadProgress.file_index + 1, count: downloadProgress.file_count, file: downloadProgress.file }) }}</p>
                    </div>
                  </Transition>
                </template>
                <template v-else-if="settings.semantic_search.backend === 'ollama'">
                  <label><span>Base URL</span><input v-model="settings.semantic_search.ollama.base_url" /></label>
                  <label><span>{{ t('settings.model') }}</span><input v-model="settings.semantic_search.ollama.model" /></label>
                  <label><span>{{ t('settings.dimensions') }}</span><input v-model.number="settings.semantic_search.ollama.dimensions" type="number" min="0" :placeholder="t('settings.automatic')" /></label>
                </template>
                <template v-else-if="settings.semantic_search.backend === 'llama_cpp'">
                  <label><span>Base URL</span><input v-model="settings.semantic_search.llama_cpp.base_url" /></label>
                  <label><span>{{ t('settings.model') }}</span><input v-model="settings.semantic_search.llama_cpp.model" /></label>
                  <label><span>API Key</span><input v-model="settings.semantic_search.llama_cpp.api_key" /></label>
                  <label><span>{{ t('settings.dimensions') }}</span><input v-model.number="settings.semantic_search.llama_cpp.dimensions" type="number" min="0" :placeholder="t('settings.automatic')" /></label>
                </template>
                <template v-else>
                  <label><span>Base URL</span><input v-model="settings.semantic_search.openai_compatible.base_url" /></label>
                  <label><span>{{ t('settings.model') }}</span><input v-model="settings.semantic_search.openai_compatible.model" /></label>
                  <label><span>API Key</span><input v-model="settings.semantic_search.openai_compatible.api_key" /></label>
                  <label><span>{{ t('settings.dimensions') }}</span><input v-model.number="settings.semantic_search.openai_compatible.dimensions" type="number" min="0" :placeholder="t('settings.automatic')" /></label>
                </template>
                <p class="path-value">
                  {{ t('settings.status', { status: semanticStatus?.status || 'unknown' }) }}
                  · {{ t('settings.ready', { count: semanticStatus?.ready_chunks ?? 0 }) }}
                  · {{ t('settings.pendingIndex', { count: semanticStatus?.pending_chunks ?? 0 }) }}
                  <template v-if="semanticStatus?.message"> · {{ semanticStatus.message }}</template>
                </p>
                <div class="setting-actions">
                  <button class="secondary-button compact" :disabled="semanticBusy" @click="emit('checkEmbedding')">{{ t('settings.testBackend') }}</button>
                  <button class="secondary-button compact" :disabled="!!(semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled')" @click="emit('reindexSemantic')">
                    <RefreshCw :size="14" :class="{ spinning: semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled' }" />
                    {{ semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled' ? t('settings.rebuilding') : t('settings.rebuildIndex') }}
                  </button>
                  <button v-if="semanticBusy && reindexProgress && reindexProgress.stage !== 'done' && reindexProgress.stage !== 'error' && reindexProgress.stage !== 'cancelled'" class="secondary-button compact" @click="emit('cancelSemanticWork')">{{ t('settings.cancelEncoding') }}</button>
                </div>
                <Transition name="setting-expand">
                  <div v-if="reindexProgress" class="download-progress" :data-stage="reindexProgress.stage">
                    <div class="download-progress-meta">
                      <span>{{ reindexProgress.message }}</span>
                      <strong>{{ Math.round((reindexProgress.fraction || 0) * 100) }}%</strong>
                    </div>
                    <div class="download-progress-track" aria-hidden="true">
                      <i :style="{ width: `${Math.max(2, Math.round((reindexProgress.fraction || 0) * 100))}%` }"></i>
                    </div>
                    <p class="path-value">
                      {{ t('settings.sessionProgress', { processed: reindexProgress.processed_sessions, total: reindexProgress.total_sessions }) }}
                      · {{ t('settings.ready', { count: reindexProgress.ready_chunks }) }}
                      · {{ t('settings.pending', { count: reindexProgress.pending_chunks }) }}
                    </p>
                  </div>
                </Transition>
              </div>
            </Transition>
          </section>
            </section>
            <section
              v-show="activePage === 'connections'"
              id="settings-page-connections"
              class="settings-content settings-page"
              role="region"
              aria-labelledby="settings-navigation-connections"
            >
          <section class="setting-group">
            <div class="setting-heading"><ShieldCheck :size="18" /><div><h3>{{ t('settings.originsTitle') }}</h3><p>{{ t('settings.originsDescription') }}</p></div></div>
            <textarea v-model="originText" spellcheck="false" :aria-label="t('settings.originsAria')"></textarea>
          </section>
          <section class="setting-group">
            <div class="setting-row"><div><h3>{{ t('settings.secretTitle') }}</h3><p>{{ t('settings.secretDescription') }}</p></div><label class="switch"><input v-model="settings.secret_enabled" type="checkbox" /><span></span></label></div>
            <Transition name="setting-expand">
              <div v-if="settings.secret_enabled" class="secret-field"><code>{{ settings.secret || t('settings.secretAfterSave') }}</code><button class="icon-button" :title="secretCopied ? t('settings.copied') : t('settings.copySecret')" :disabled="!settings.secret" @click="emit('copySecret')"><Check v-if="secretCopied" :size="17" /><Clipboard v-else :size="17" /></button><button class="secondary-button compact" @click="emit('rotateSecret')">{{ t('settings.rotateSecret') }}</button></div>
            </Transition>
          </section>
          <section class="setting-group">
            <div class="setting-row">
              <div>
                <h3>MCP</h3>
                <p>{{ t('settings.mcpDescription') }}</p>
              </div>
              <label class="switch"><input v-model="settings.mcp_enabled" type="checkbox" /><span></span></label>
            </div>
            <p class="path-value">{{ t('settings.address', { address: apiStatus?.mcp_url || 'http://127.0.0.1:19821/mcp' }) }}</p>
            <p class="path-value">{{ t('settings.mcpStatus', { status: mcpStateLabel(apiStatus) }) }}</p>
            <div class="setting-actions">
              <button class="secondary-button compact mcp-copy-button" :class="{ copied: mcpConfigCopied }" type="button" @click="emit('copyMcpConfig')">
                <span class="mcp-copy-button__icon" aria-hidden="true">
                  <Clipboard class="mcp-copy-button__clipboard" :size="14" />
                  <Check class="mcp-copy-button__check" :size="14" />
                </span>
                <span class="mcp-copy-button__label">
                  <span class="mcp-copy-button__label-default">{{ t('settings.copyClientConfig') }}</span>
                  <span class="mcp-copy-button__label-success">{{ t('settings.copiedClientConfig') }}</span>
                </span>
              </button>
            </div>
          </section>
            </section>
            <section
              v-show="activePage === 'cloud-sync'"
              id="settings-page-cloud-sync"
              class="settings-content settings-page"
              role="region"
              aria-labelledby="settings-navigation-cloud-sync"
            >
              <CloudSyncSettings
                v-model:settings="settings.cloud_sync"
                v-model:password="cloudPassword"
                v-model:sync-password="syncPassword"
                v-model:access-key-id="s3AccessKeyId"
                v-model:secret-access-key="s3SecretAccessKey"
                v-model:session-token="s3SessionToken"
                :status="cloudSyncStatus || { state: 'disabled', pending_mutations: 0, devices: [] }"
                :busy="cloudSyncBusy"
                :active-profile="activeCloudSyncProfile"
                :connection-test-state="connectionTestState"
                :connection-test-message="connectionTestMessage"
                @test="testCloudConnection"
                @sync="emit('cloudSyncNow')"
                @rewrite="emit('cloudSyncRewrite')"
                @remove-device="emit('cloudSyncRemoveDevice', $event)"
              />
            </section>
          </div>
        </div>
        <footer><button class="secondary-button" @click="emit('close')">{{ t('app.cancel') }}</button><button class="primary-button" :disabled="!canSave || cloudSyncBusy" @click="emit('save', connectionTestState === 'passed' ? cloudCredentials() : null)">{{ t('settings.save') }}</button></footer>
      </section>
    </div>
  </Transition>

  <ThemeEditorDialog
    v-model:show="showThemeEditor"
    :theme-def="editingTheme"
    :is-new="isCreatingNewTheme"
    @save="onSaveCustomTheme"
    @delete="onDeleteCustomTheme"
  />
</template>
