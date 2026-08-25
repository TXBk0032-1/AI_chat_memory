<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import {
  X,
  Sparkles,
  Download,
  Upload,
  Trash2,
  Check,
  Sun,
  Moon,
  Eye,
  Sliders,
  Palette,
} from 'lucide-vue-next'
import { translate as t } from '../i18n'
import type { ThemeDefinition } from '../theme/types'
import { createDefaultCustomTheme } from '../theme/presets'
import {
  createThemeColors,
  normalizeColor,
  toHex6,
  isValidColor,
} from '../theme'

const props = defineProps<{
  show: boolean
  themeDef: ThemeDefinition | null
  isNew?: boolean
}>()

const emit = defineEmits<{
  'update:show': [value: boolean]
  save: [theme: ThemeDefinition, activate: boolean]
  delete: [id: string]
}>()

const popularAccentsLight = [
  { name: 'Emerald', hex: '#167961' },
  { name: 'Ocean Blue', hex: '#3498db' },
  { name: 'Indigo', hex: '#6366f1' },
  { name: 'Purple', hex: '#9b59b6' },
  { name: 'Amber', hex: '#f5ab35' },
  { name: 'Crimson', hex: '#d64541' },
  { name: 'Rose', hex: '#f1828d' },
  { name: 'Teal', hex: '#00a896' },
  { name: 'Charcoal', hex: '#4a5568' },
]

const popularAccentsDark = [
  { name: 'Mint Green', hex: '#55c49e' },
  { name: 'Sky Blue', hex: '#60a5fa' },
  { name: 'Soft Indigo', hex: '#818cf8' },
  { name: 'Violet', hex: '#c084fc' },
  { name: 'Warm Amber', hex: '#fbbf24' },
  { name: 'Coral Red', hex: '#f87171' },
  { name: 'Sakura', hex: '#fb7185' },
  { name: 'Cyan', hex: '#22d3ee' },
  { name: 'Platinum', hex: '#cbd5e1' },
]

const id = ref('')
const name = ref('')
const isDark = ref(false)
const isDarkFont = ref(false)
const primaryColor = ref('#167961')
const appBgColor = ref('#f7f9fa')
const mainBgColor = ref('#ffffff')
const fontColor = ref('#212121')
const navFontColor = ref('')
const badgePrimaryColor = ref('')
const badgeSecondaryColor = ref('')
const badgeTertiaryColor = ref('')
const extraExtInfo = ref<Record<string, string>>({})
const showAdvanced = ref(false)
const errorMessage = ref('')
const fileInputRef = ref<HTMLInputElement | null>(null)

function resetFormFromTheme(target: ThemeDefinition | null, isCreatingNew = false) {
  errorMessage.value = ''
  extraExtInfo.value = {}
  if (!target || isCreatingNew) {
    const defaultTheme = createDefaultCustomTheme(target?.isDark ?? isDark.value)
    id.value = defaultTheme.id
    name.value = target?.name ? `${target.name} (Custom)` : ''
    isDark.value = defaultTheme.isDark
    isDarkFont.value = Boolean(defaultTheme.isDarkFont)
    primaryColor.value = toHex6(defaultTheme.config.primary)
    appBgColor.value = toHex6(defaultTheme.config.extInfo?.['--color-app-background'] || (defaultTheme.isDark ? '#171b1e' : '#f7f9fa'))
    mainBgColor.value = toHex6(defaultTheme.config.extInfo?.['--color-main-background'] || (defaultTheme.isDark ? '#1e2428' : '#ffffff'))
    fontColor.value = toHex6(defaultTheme.config.font || (defaultTheme.isDark ? '#e5e5e5' : '#212121'))
    navFontColor.value = ''
    badgePrimaryColor.value = ''
    badgeSecondaryColor.value = ''
    badgeTertiaryColor.value = ''
  } else {
    id.value = target.id
    name.value = target.nameKey ? t(target.nameKey) : target.name
    isDark.value = target.isDark
    isDarkFont.value = Boolean(target.isDarkFont)
    primaryColor.value = toHex6(target.config.primary)
    appBgColor.value = toHex6(target.config.extInfo?.['--color-app-background'] || (target.isDark ? '#171b1e' : '#f7f9fa'))
    mainBgColor.value = toHex6(target.config.extInfo?.['--color-main-background'] || (target.isDark ? '#1e2428' : '#ffffff'))
    fontColor.value = toHex6(target.config.font || (target.isDark ? '#e5e5e5' : '#212121'))
    navFontColor.value = target.config.extInfo?.['--color-nav-font'] ? toHex6(target.config.extInfo['--color-nav-font']) : ''
    badgePrimaryColor.value = target.config.extInfo?.['--color-badge-primary'] ? toHex6(target.config.extInfo['--color-badge-primary']) : ''
    badgeSecondaryColor.value = target.config.extInfo?.['--color-badge-secondary'] ? toHex6(target.config.extInfo['--color-badge-secondary']) : ''
    badgeTertiaryColor.value = target.config.extInfo?.['--color-badge-tertiary'] ? toHex6(target.config.extInfo['--color-badge-tertiary']) : ''

    if (target.config?.extInfo) {
      const knownKeys = new Set([
        '--color-app-background',
        '--color-main-background',
        '--color-nav-font',
        '--color-badge-primary',
        '--color-badge-secondary',
        '--color-badge-tertiary',
      ])
      for (const [k, v] of Object.entries(target.config.extInfo)) {
        if (!knownKeys.has(k)) {
          extraExtInfo.value[k] = v
        }
      }
    }
  }
}

watch(
  () => [props.show, props.themeDef, props.isNew] as const,
  ([show, themeDef, isNew]) => {
    if (show) {
      resetFormFromTheme(themeDef, Boolean(isNew))
    }
  },
  { immediate: true },
)

function onModeToggle(mode: 'light' | 'dark') {
  if (isDark.value === (mode === 'dark')) return
  isDark.value = mode === 'dark'
  if (isDark.value) {
    if (appBgColor.value === '#f7f9fa' || appBgColor.value === '#ffffff') appBgColor.value = '#171b1e'
    if (mainBgColor.value === '#ffffff' || mainBgColor.value === '#f7f9fa') mainBgColor.value = '#1e2428'
    if (fontColor.value === '#212121' || fontColor.value === '#333333') fontColor.value = '#e5e5e5'
    if (primaryColor.value === '#167961') primaryColor.value = '#55c49e'
  } else {
    if (appBgColor.value === '#171b1e' || appBgColor.value === '#121212') appBgColor.value = '#f7f9fa'
    if (mainBgColor.value === '#1e2428' || mainBgColor.value === '#1a1a1a') mainBgColor.value = '#ffffff'
    if (fontColor.value === '#e5e5e5' || fontColor.value === '#ffffff') fontColor.value = '#212121'
    if (primaryColor.value === '#55c49e') primaryColor.value = '#167961'
  }
}

function selectAccent(hex: string) {
  primaryColor.value = hex
}

const computedExtInfo = computed(() => {
  const ext: Record<string, string> = {
    ...extraExtInfo.value,
    '--color-app-background': normalizeColor(appBgColor.value),
    '--color-main-background': normalizeColor(mainBgColor.value),
  }
  if (navFontColor.value && isValidColor(navFontColor.value)) {
    ext['--color-nav-font'] = normalizeColor(navFontColor.value)
  }
  if (badgePrimaryColor.value && isValidColor(badgePrimaryColor.value)) {
    ext['--color-badge-primary'] = normalizeColor(badgePrimaryColor.value)
  }
  if (badgeSecondaryColor.value && isValidColor(badgeSecondaryColor.value)) {
    ext['--color-badge-secondary'] = normalizeColor(badgeSecondaryColor.value)
  }
  if (badgeTertiaryColor.value && isValidColor(badgeTertiaryColor.value)) {
    ext['--color-badge-tertiary'] = normalizeColor(badgeTertiaryColor.value)
  }
  return ext
})

const computedPreviewStyles = computed(() => {
  const primary = isValidColor(primaryColor.value) ? primaryColor.value : (isDark.value ? '#55c49e' : '#167961')
  const font = isValidColor(fontColor.value) ? fontColor.value : (isDark.value ? '#e5e5e5' : '#212121')
  const colors = createThemeColors(primary, font, isDark.value, isDarkFont.value, computedExtInfo.value)
  return colors as Record<string, string>
})

function buildThemeObject(): ThemeDefinition {
  return {
    id: id.value || `custom_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
    name: name.value.trim() || (isDark.value ? 'Custom Dark' : 'Custom Light'),
    nameKey: '',
    isDark: isDark.value,
    isDarkFont: isDarkFont.value,
    isCustom: true,
    config: {
      primary: normalizeColor(primaryColor.value),
      font: normalizeColor(fontColor.value),
      extInfo: computedExtInfo.value,
    },
  }
}

function handleSave(activate = false) {
  if (!name.value.trim()) {
    errorMessage.value = t('settings.themeEditor.nameRequired')
    return
  }
  const theme = buildThemeObject()
  emit('save', theme, activate)
  emit('update:show', false)
}

function handleDelete() {
  if (confirm(t('settings.themeEditor.deleteConfirm'))) {
    emit('delete', id.value)
    emit('update:show', false)
  }
}

function handleExport() {
  const theme = buildThemeObject()
  const dataStr = 'data:text/json;charset=utf-8,' + encodeURIComponent(JSON.stringify(theme, null, 2))
  const downloadAnchor = document.createElement('a')
  downloadAnchor.setAttribute('href', dataStr)
  downloadAnchor.setAttribute('download', `${theme.name.replace(/\s+/g, '_').toLowerCase()}_theme.json`)
  document.body.appendChild(downloadAnchor)
  downloadAnchor.click()
  downloadAnchor.remove()
}

function triggerImport() {
  fileInputRef.value?.click()
}

function handleImportFile(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  if (!file) return

  const reader = new FileReader()
  reader.onload = (e) => {
    try {
      const parsed = JSON.parse(e.target?.result as string) as ThemeDefinition
      if (!parsed || !parsed.config || !parsed.config.primary) {
        errorMessage.value = t('settings.themeEditor.importInvalid')
        return
      }
      resetFormFromTheme(parsed, false)
      name.value = parsed.name || 'Imported Theme'
      id.value = `custom_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`
      errorMessage.value = ''
    } catch {
      errorMessage.value = t('settings.themeEditor.importInvalid')
    }
  }
  reader.readAsText(file)
  input.value = ''
}

function closeDialog() {
  emit('update:show', false)
}
</script>

<template>
  <Teleport to="body">
    <Transition name="theme-dialog-fade">
      <div v-if="show" class="theme-editor-backdrop" @click.self="closeDialog">
        <div
          class="theme-editor-dialog"
          role="dialog"
          aria-modal="true"
          :aria-labelledby="'theme-editor-title'"
        >
          <!-- Header -->
          <div class="theme-editor-header">
            <div class="header-title-group">
              <div class="header-icon-wrap">
                <Palette :size="20" />
              </div>
              <div>
                <h2 id="theme-editor-title">
                  {{ isNew ? t('settings.themeEditor.createTitle') : t('settings.themeEditor.editTitle') }}
                </h2>
              </div>
            </div>
            <div class="header-actions">
              <button
                type="button"
                class="header-btn"
                :title="t('settings.exportTheme')"
                @click="handleExport"
              >
                <Download :size="16" />
                <span>{{ t('settings.exportTheme') }}</span>
              </button>
              <button
                type="button"
                class="header-btn"
                :title="t('settings.importThemeButton')"
                @click="triggerImport"
              >
                <Upload :size="16" />
                <span>{{ t('settings.importThemeButton') }}</span>
              </button>
              <input
                ref="fileInputRef"
                type="file"
                accept=".json"
                class="hidden-file-input"
                @change="handleImportFile"
              />
              <button
                type="button"
                class="close-btn"
                aria-label="Close"
                @click="closeDialog"
              >
                <X :size="18" />
              </button>
            </div>
          </div>

          <!-- Body split into Form & Live Preview -->
          <div class="theme-editor-body">
            <!-- Left: Form Controls -->
            <div class="theme-editor-form">
              <div v-if="errorMessage" class="theme-error-banner">
                {{ errorMessage }}
              </div>

              <!-- Name & Mode -->
              <div class="form-row">
                <div class="form-field flex-1">
                  <label for="theme-name-input">{{ t('settings.themeEditor.nameLabel') }}</label>
                  <input
                    id="theme-name-input"
                    v-model="name"
                    type="text"
                    class="theme-input"
                    :placeholder="t('settings.themeEditor.namePlaceholder')"
                  />
                </div>
                <div class="form-field">
                  <label>{{ t('settings.themeEditor.modeLabel') }}</label>
                  <div class="mode-toggle-group">
                    <button
                      type="button"
                      class="mode-btn"
                      :class="{ active: !isDark }"
                      @click="onModeToggle('light')"
                    >
                      <Sun :size="14" />
                      <span>{{ t('settings.themeEditor.modeLight') }}</span>
                    </button>
                    <button
                      type="button"
                      class="mode-btn"
                      :class="{ active: isDark }"
                      @click="onModeToggle('dark')"
                    >
                      <Moon :size="14" />
                      <span>{{ t('settings.themeEditor.modeDark') }}</span>
                    </button>
                  </div>
                </div>
              </div>

              <!-- Primary Color & Swatches -->
              <div class="form-section">
                <div class="section-title">
                  <Sparkles :size="15" />
                  <span>{{ t('settings.themeEditor.primaryColor') }}</span>
                </div>
                <div class="color-picker-row">
                  <div class="color-input-wrap">
                    <input
                      v-model="primaryColor"
                      type="color"
                      class="color-picker-native"
                    />
                    <span class="color-preview-badge" :style="{ backgroundColor: primaryColor }"></span>
                  </div>
                  <input
                    v-model="primaryColor"
                    type="text"
                    class="theme-input font-mono flex-1"
                    placeholder="#167961"
                  />
                </div>
                <!-- Popular Swatches -->
                <div class="swatches-grid">
                  <button
                    v-for="swatch in (isDark ? popularAccentsDark : popularAccentsLight)"
                    :key="swatch.hex"
                    type="button"
                    class="swatch-btn"
                    :class="{ active: primaryColor.toLowerCase() === swatch.hex.toLowerCase() }"
                    :style="{ backgroundColor: swatch.hex }"
                    :title="swatch.name"
                    @click="selectAccent(swatch.hex)"
                  >
                    <Check v-if="primaryColor.toLowerCase() === swatch.hex.toLowerCase()" :size="12" />
                  </button>
                </div>
              </div>

              <!-- Base Backgrounds & Font Color -->
              <div class="form-section">
                <div class="section-title">
                  <Sliders :size="15" />
                  <span>{{ t('settings.themeEditor.colorsTitle') }}</span>
                </div>

                <div class="form-grid-2">
                  <div class="form-field">
                    <label>{{ t('settings.themeEditor.appBgColor') }}</label>
                    <div class="color-picker-row">
                      <div class="color-input-wrap">
                        <input v-model="appBgColor" type="color" class="color-picker-native" />
                        <span class="color-preview-badge" :style="{ backgroundColor: appBgColor }"></span>
                      </div>
                      <input v-model="appBgColor" type="text" class="theme-input font-mono flex-1" />
                    </div>
                  </div>

                  <div class="form-field">
                    <label>{{ t('settings.themeEditor.mainBgColor') }}</label>
                    <div class="color-picker-row">
                      <div class="color-input-wrap">
                        <input v-model="mainBgColor" type="color" class="color-picker-native" />
                        <span class="color-preview-badge" :style="{ backgroundColor: mainBgColor }"></span>
                      </div>
                      <input v-model="mainBgColor" type="text" class="theme-input font-mono flex-1" />
                    </div>
                  </div>
                </div>

                <div class="form-field mt-3">
                  <label>{{ t('settings.themeEditor.fontColor') }}</label>
                  <div class="color-picker-row">
                    <div class="color-input-wrap">
                      <input v-model="fontColor" type="color" class="color-picker-native" />
                      <span class="color-preview-badge" :style="{ backgroundColor: fontColor }"></span>
                    </div>
                    <input v-model="fontColor" type="text" class="theme-input font-mono flex-1" />
                  </div>
                </div>
              </div>

              <!-- Advanced Detail Accordion -->
              <div class="advanced-section">
                <button
                  type="button"
                  class="advanced-toggle"
                  @click="showAdvanced = !showAdvanced"
                >
                  <span>{{ t('settings.themeEditor.advancedTitle') }}</span>
                  <span class="toggle-arrow" :class="{ open: showAdvanced }">▼</span>
                </button>
                <div v-show="showAdvanced" class="advanced-content">
                  <div class="form-grid-2">
                    <div class="form-field">
                      <label>{{ t('settings.themeEditor.navFontColor') }}</label>
                      <div class="color-picker-row">
                        <div class="color-input-wrap">
                          <input v-model="navFontColor" type="color" class="color-picker-native" />
                          <span class="color-preview-badge" :style="{ backgroundColor: navFontColor || primaryColor }"></span>
                        </div>
                        <input v-model="navFontColor" type="text" class="theme-input font-mono flex-1" placeholder="Auto" />
                      </div>
                    </div>
                    <div class="form-field">
                      <label>{{ t('settings.themeEditor.badgePrimary') }}</label>
                      <div class="color-picker-row">
                        <div class="color-input-wrap">
                          <input v-model="badgePrimaryColor" type="color" class="color-picker-native" />
                          <span class="color-preview-badge" :style="{ backgroundColor: badgePrimaryColor || primaryColor }"></span>
                        </div>
                        <input v-model="badgePrimaryColor" type="text" class="theme-input font-mono flex-1" placeholder="Auto" />
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </div>

            <!-- Right: Mini Live Interactive Preview -->
            <div class="theme-editor-preview-container">
              <div class="preview-header-label">
                <Eye :size="15" />
                <span>{{ t('settings.themeEditor.previewTitle') }}</span>
              </div>

              <div class="mini-window" :style="computedPreviewStyles">
                <!-- Mini Title Bar -->
                <div class="mini-titlebar">
                  <div class="mini-traffic-lights">
                    <span class="light red"></span>
                    <span class="light yellow"></span>
                    <span class="light green"></span>
                  </div>
                  <span class="mini-app-title">AI Chat Memory</span>
                  <span class="mini-badge">{{ isDark ? 'Dark' : 'Light' }}</span>
                </div>

                <!-- Mini Window Layout -->
                <div class="mini-content-layout">
                  <!-- Mini Sidebar -->
                  <div class="mini-sidebar">
                    <div class="mini-nav-item active">
                      <span class="mini-nav-dot"></span>
                      <span>Chats</span>
                    </div>
                    <div class="mini-nav-item">
                      <span class="mini-nav-dot dim"></span>
                      <span>Search</span>
                    </div>
                    <div class="mini-nav-item">
                      <span class="mini-nav-dot dim"></span>
                      <span>Settings</span>
                    </div>
                  </div>

                  <!-- Mini Main View -->
                  <div class="mini-main">
                    <!-- Mini Search Bar -->
                    <div class="mini-search-box">
                      <span>🔍</span>
                      <span class="mini-search-placeholder">{{ t('settings.themeEditor.previewSearch') }}</span>
                    </div>

                    <!-- Mini Chat Messages -->
                    <div class="mini-chat-thread">
                      <!-- User Bubble -->
                      <div class="mini-message mini-message-user">
                        <div class="mini-bubble user-bubble">
                          {{ t('settings.themeEditor.previewUserMsg') }}
                        </div>
                      </div>

                      <!-- AI Bubble -->
                      <div class="mini-message mini-message-ai">
                        <div class="mini-avatar">AI</div>
                        <div class="mini-bubble ai-bubble">
                          <p>{{ t('settings.themeEditor.previewAiMsg') }}</p>
                          <div class="mini-tags">
                            <span class="mini-tag tag-primary">{{ t('settings.themeEditor.previewBadge') }}</span>
                            <span class="mini-tag tag-secondary">v0.1.0</span>
                          </div>
                        </div>
                      </div>
                    </div>

                    <!-- Mini Action Footer -->
                    <div class="mini-footer">
                      <button type="button" class="mini-btn-primary">
                        {{ t('settings.themeEditor.previewButton') }}
                      </button>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <!-- Footer Actions -->
          <div class="theme-editor-footer">
            <div class="footer-left">
              <button
                v-if="!isNew && themeDef?.isCustom"
                type="button"
                class="btn-danger"
                @click="handleDelete"
              >
                <Trash2 :size="15" />
                <span>{{ t('settings.deleteTheme') }}</span>
              </button>
            </div>
            <div class="footer-right">
              <button type="button" class="btn-secondary" @click="closeDialog">
                {{ t('settings.themeEditor.cancel') }}
              </button>
              <button type="button" class="btn-primary-outline" @click="handleSave(false)">
                {{ t('settings.themeEditor.saveTheme') }}
              </button>
              <button type="button" class="btn-primary" @click="handleSave(true)">
                <Check :size="16" />
                <span>{{ t('settings.themeEditor.applyAndSave') }}</span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.theme-editor-backdrop {
  position: fixed;
  inset: 0;
  background-color: rgba(0, 0, 0, 0.55);
  backdrop-filter: blur(4px);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1050;
  padding: 20px;
}

.theme-editor-dialog {
  background-color: var(--color-main-background, #ffffff);
  color: var(--color-1000, #212121);
  border-radius: 12px;
  width: 920px;
  max-width: 96vw;
  max-height: 90vh;
  display: flex;
  flex-direction: column;
  box-shadow: 0 20px 45px rgba(0, 0, 0, 0.25), 0 0 0 1px rgba(0, 0, 0, 0.08);
  overflow: hidden;
  animation: dialog-pop 0.22s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes dialog-pop {
  from {
    transform: scale(0.96);
    opacity: 0;
  }
  to {
    transform: scale(1);
    opacity: 1;
  }
}

.theme-editor-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 24px;
  border-bottom: 1px solid var(--color-primary-alpha-800, rgba(0, 0, 0, 0.08));
}

.header-title-group {
  display: flex;
  align-items: center;
  gap: 12px;
}

.header-icon-wrap {
  width: 36px;
  height: 36px;
  border-radius: 8px;
  background: var(--color-primary-subtle, rgba(22, 121, 97, 0.1));
  color: var(--color-primary, #167961);
  display: flex;
  align-items: center;
  justify-content: center;
}

.header-title-group h2 {
  margin: 0;
  font-size: 1.15rem;
  font-weight: 600;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.header-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  font-size: 0.85rem;
  border-radius: 6px;
  border: 1px solid var(--color-primary-border, #e2e8f0);
  background: transparent;
  color: var(--color-1000, #333);
  cursor: pointer;
  transition: all 0.15s ease;
}

.header-btn:hover {
  background: var(--color-primary-subtle, rgba(22, 121, 97, 0.08));
  border-color: var(--color-primary, #167961);
}

.close-btn {
  background: transparent;
  border: none;
  color: var(--color-600, #718096);
  cursor: pointer;
  padding: 6px;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.15s;
}

.close-btn:hover {
  background: rgba(0, 0, 0, 0.06);
  color: var(--color-1000, #000);
}

.theme-editor-body {
  display: grid;
  grid-template-columns: 1.15fr 0.95fr;
  overflow-y: auto;
  padding: 24px;
  gap: 24px;
  flex: 1;
}

@media (max-width: 768px) {
  .theme-editor-body {
    grid-template-columns: 1fr;
  }
}

.theme-editor-form {
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.theme-error-banner {
  background: #fee2e2;
  color: #b91c1c;
  padding: 8px 14px;
  border-radius: 6px;
  font-size: 0.85rem;
  border-left: 3px solid #ef4444;
}

.form-row {
  display: flex;
  gap: 16px;
  align-items: flex-end;
}

.flex-1 {
  flex: 1;
}

.form-field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.form-field label {
  font-size: 0.82rem;
  font-weight: 500;
  color: var(--color-700, #4b5563);
}

.theme-input {
  height: 36px;
  padding: 0 12px;
  border-radius: 6px;
  border: 1px solid var(--color-primary-border, #cbd5e1);
  background-color: var(--color-app-background, #f8fafc);
  color: var(--color-1000, #1e293b);
  font-size: 0.9rem;
  outline: none;
  transition: border-color 0.15s, box-shadow 0.15s;
}

.theme-input:focus {
  border-color: var(--color-primary, #167961);
  box-shadow: 0 0 0 2px var(--color-primary-subtle, rgba(22, 121, 97, 0.15));
}

.font-mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
  font-size: 0.85rem;
}

.mode-toggle-group {
  display: flex;
  height: 36px;
  background: var(--color-app-background, #f1f5f9);
  padding: 3px;
  border-radius: 8px;
  border: 1px solid var(--color-primary-border, #e2e8f0);
}

.mode-btn {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 12px;
  border: none;
  background: transparent;
  color: var(--color-700, #64748b);
  font-size: 0.82rem;
  font-weight: 500;
  border-radius: 6px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.mode-btn.active {
  background: var(--color-main-background, #ffffff);
  color: var(--color-primary, #167961);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.1);
}

.form-section {
  background: var(--color-app-background, #f8fafc);
  padding: 16px;
  border-radius: 8px;
  border: 1px solid var(--color-primary-alpha-900, #e2e8f0);
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.section-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 0.88rem;
  font-weight: 600;
  color: var(--color-1000, #1e293b);
}

.color-picker-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.color-input-wrap {
  position: relative;
  width: 36px;
  height: 36px;
  border-radius: 6px;
  overflow: hidden;
  cursor: pointer;
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.15);
}

.color-picker-native {
  position: absolute;
  top: -10px;
  left: -10px;
  width: 60px;
  height: 60px;
  opacity: 0;
  cursor: pointer;
}

.color-preview-badge {
  display: block;
  width: 100%;
  height: 100%;
}

.swatches-grid {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 4px;
}

.swatch-btn {
  width: 28px;
  height: 28px;
  border-radius: 6px;
  border: none;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #ffffff;
  transition: transform 0.15s, box-shadow 0.15s;
  box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.1);
}

.swatch-btn:hover {
  transform: scale(1.12);
}

.swatch-btn.active {
  box-shadow: 0 0 0 2px var(--color-main-background, #ffffff), 0 0 0 4px var(--color-primary, #167961);
}

.form-grid-2 {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 12px;
}

.mt-3 {
  margin-top: 12px;
}

.advanced-section {
  border: 1px solid var(--color-primary-alpha-900, #e2e8f0);
  border-radius: 8px;
  overflow: hidden;
}

.advanced-toggle {
  width: 100%;
  padding: 10px 16px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  background: var(--color-app-background, #f8fafc);
  border: none;
  font-size: 0.85rem;
  font-weight: 500;
  color: var(--color-700, #64748b);
  cursor: pointer;
}

.toggle-arrow {
  font-size: 0.75rem;
  transition: transform 0.2s;
}

.toggle-arrow.open {
  transform: rotate(180deg);
}

.advanced-content {
  padding: 16px;
  background: var(--color-main-background, #ffffff);
  border-top: 1px solid var(--color-primary-alpha-900, #e2e8f0);
}

/* Mini Preview Styles */
.theme-editor-preview-container {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.preview-header-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 0.85rem;
  font-weight: 600;
  color: var(--color-700, #64748b);
}

.mini-window {
  flex: 1;
  min-height: 420px;
  border-radius: 10px;
  background: var(--color-app-background, #f7f9fa);
  color: var(--color-1000, #212121);
  border: 1px solid var(--color-primary-border, rgba(0, 0, 0, 0.1));
  box-shadow: 0 10px 25px rgba(0, 0, 0, 0.12);
  display: flex;
  flex-direction: column;
  overflow: hidden;
  transition: all 0.2s ease;
}

.mini-titlebar {
  height: 32px;
  background: var(--color-app-background, #f7f9fa);
  border-bottom: 1px solid var(--color-primary-alpha-800, rgba(0, 0, 0, 0.06));
  display: flex;
  align-items: center;
  padding: 0 12px;
  gap: 8px;
}

.mini-traffic-lights {
  display: flex;
  gap: 6px;
}

.mini-traffic-lights .light {
  width: 9px;
  height: 9px;
  border-radius: 50%;
}

.mini-traffic-lights .red { background: var(--color-btn-close, #fab4a0); }
.mini-traffic-lights .yellow { background: var(--color-btn-min, #85c43b); }
.mini-traffic-lights .green { background: var(--color-btn-hide, #3bc2b2); }

.mini-app-title {
  font-size: 0.72rem;
  font-weight: 600;
  color: var(--color-nav-font, var(--color-primary, #167961));
  flex: 1;
  text-align: center;
}

.mini-badge {
  font-size: 0.65rem;
  padding: 1px 6px;
  border-radius: 4px;
  background: var(--color-primary-subtle, rgba(22, 121, 97, 0.1));
  color: var(--color-primary, #167961);
}

.mini-content-layout {
  display: flex;
  flex: 1;
  overflow: hidden;
}

.mini-sidebar {
  width: 90px;
  background: var(--color-app-background, #f7f9fa);
  border-right: 1px solid var(--color-primary-alpha-800, rgba(0, 0, 0, 0.06));
  padding: 10px 6px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.mini-nav-item {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px;
  border-radius: 6px;
  font-size: 0.72rem;
  color: var(--color-700, #64748b);
}

.mini-nav-item.active {
  background: var(--color-primary-subtle, rgba(22, 121, 97, 0.12));
  color: var(--color-primary, #167961);
  font-weight: 600;
}

.mini-nav-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--color-primary, #167961);
}

.mini-nav-dot.dim {
  background: var(--color-primary-alpha-600, #cbd5e1);
}

.mini-main {
  flex: 1;
  background: var(--color-main-background, #ffffff);
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  overflow-y: auto;
}

.mini-search-box {
  background: var(--color-app-background, #f1f5f9);
  border: 1px solid var(--color-primary-border, #e2e8f0);
  border-radius: 6px;
  padding: 6px 10px;
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 0.72rem;
  color: var(--color-600, #94a3b8);
}

.mini-chat-thread {
  display: flex;
  flex-direction: column;
  gap: 10px;
  flex: 1;
}

.mini-message {
  display: flex;
  gap: 8px;
}

.mini-message-user {
  justify-content: flex-end;
}

.mini-bubble {
  max-width: 82%;
  padding: 8px 12px;
  border-radius: 8px;
  font-size: 0.74rem;
  line-height: 1.4;
}

.user-bubble {
  background: var(--color-primary, #167961);
  color: #ffffff;
  border-bottom-right-radius: 2px;
}

.mini-message-ai {
  align-items: flex-start;
}

.mini-avatar {
  width: 24px;
  height: 24px;
  border-radius: 6px;
  background: var(--color-primary-subtle, rgba(22, 121, 97, 0.15));
  color: var(--color-primary, #167961);
  font-size: 0.65rem;
  font-weight: 700;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.ai-bubble {
  background: var(--color-app-background, #f8fafc);
  color: var(--color-1000, #1e293b);
  border: 1px solid var(--color-primary-border, #e2e8f0);
  border-bottom-left-radius: 2px;
}

.ai-bubble p {
  margin: 0;
}

.mini-tags {
  display: flex;
  gap: 6px;
  margin-top: 6px;
}

.mini-tag {
  font-size: 0.62rem;
  padding: 2px 6px;
  border-radius: 4px;
  font-weight: 500;
}

.tag-primary {
  background: var(--color-badge-primary, var(--color-primary, #167961));
  color: #ffffff;
}

.tag-secondary {
  background: var(--color-badge-secondary, #4baed5);
  color: #ffffff;
}

.mini-footer {
  display: flex;
  justify-content: flex-end;
  border-top: 1px solid var(--color-primary-alpha-800, #f1f5f9);
  padding-top: 8px;
}

.mini-btn-primary {
  background: var(--color-primary, #167961);
  color: #ffffff;
  border: none;
  padding: 4px 10px;
  font-size: 0.72rem;
  font-weight: 500;
  border-radius: 4px;
  cursor: default;
}

/* Dialog Footer */
.theme-editor-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 24px;
  border-top: 1px solid var(--color-primary-alpha-800, rgba(0, 0, 0, 0.08));
  background: var(--color-app-background, #f8fafc);
}

.footer-right {
  display: flex;
  align-items: center;
  gap: 10px;
}

.btn-secondary {
  padding: 8px 16px;
  font-size: 0.88rem;
  border-radius: 6px;
  border: 1px solid var(--color-primary-border, #cbd5e1);
  background: var(--color-main-background, #ffffff);
  color: var(--color-1000, #334155);
  cursor: pointer;
  transition: all 0.15s;
}

.btn-secondary:hover {
  background: rgba(0, 0, 0, 0.04);
}

.btn-danger {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 14px;
  font-size: 0.88rem;
  border-radius: 6px;
  border: 1px solid #fca5a5;
  background: transparent;
  color: #dc2626;
  cursor: pointer;
  transition: all 0.15s;
}

.btn-danger:hover {
  background: #fee2e2;
}

.btn-primary-outline {
  padding: 8px 16px;
  font-size: 0.88rem;
  border-radius: 6px;
  border: 1px solid var(--color-primary, #167961);
  background: transparent;
  color: var(--color-primary, #167961);
  cursor: pointer;
  font-weight: 500;
  transition: all 0.15s;
}

.btn-primary-outline:hover {
  background: var(--color-primary-subtle, rgba(22, 121, 97, 0.08));
}

.btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 8px 18px;
  font-size: 0.88rem;
  font-weight: 500;
  border-radius: 6px;
  border: none;
  background: var(--color-primary, #167961);
  color: #ffffff;
  cursor: pointer;
  box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
  transition: all 0.15s;
}

.btn-primary:hover {
  background: var(--color-primary-hover, #126350);
}

.hidden-file-input {
  display: none;
}

.theme-dialog-fade-enter-active,
.theme-dialog-fade-leave-active {
  transition: opacity 0.2s ease;
}

.theme-dialog-fade-enter-from,
.theme-dialog-fade-leave-to {
  opacity: 0;
}
</style>
