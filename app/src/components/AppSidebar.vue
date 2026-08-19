<script setup lang="ts">
import { computed } from 'vue'
import { Inbox, MessageSquareText, PanelLeftClose, PanelLeftOpen, Settings } from 'lucide-vue-next'
import type { ApiStatus } from '../desktop-api'
import { translate as t } from '../i18n'

const props = defineProps<{
  collapsed: boolean
  total: number
  platform: string
  apiStatus: ApiStatus
}>()
const emit = defineEmits<{
  collapse: [collapsed: boolean]
  selectPlatform: [platform: string]
  openSettings: []
}>()

const sourceIndex = computed(() => ['', 'deepseek', 'doubao', 'kimi'].indexOf(props.platform))
const sourceAccent = computed(() => ({ deepseek: '#4d8fe8', doubao: '#e05c62', kimi: '#39a878' } as Record<string, string>)[props.platform] ?? '#74858d')
const statusLabel = computed(() => props.apiStatus.service.state === 'running' ? t('sidebar.serviceRunning') : props.apiStatus.service.state === 'failed' ? t('sidebar.serviceFailed') : t('sidebar.serviceStarting'))

function handlePlatformPointerDown(platformKey: string, event: PointerEvent) {
  if (event.button !== 0) return
  console.log(`%c[SIDEBAR:POINTER_DOWN_TRIGGER] platform="${platformKey || 'all'}"`, 'color: #2563eb; font-weight: bold')
  emit('selectPlatform', platformKey)
}

function handlePlatformClick(platformKey: string) {
  console.log(`%c[SIDEBAR:PLATFORM_CLICK] platform="${platformKey || 'all'}"`, 'color: #2563eb; font-weight: bold')
  emit('selectPlatform', platformKey)
}
</script>

<template>
  <aside class="sidebar" :aria-label="collapsed ? t('sidebar.collapsedNavigation') : t('sidebar.navigation')">
    <div class="identity">
      <div class="identity-content">
        <div class="identity-mark"><MessageSquareText :size="20" /></div>
        <div class="identity-name"><strong>{{ t('app.title') }}</strong><span>AI Chat Memory</span></div>
        <button class="sidebar-toggle sidebar-toggle-collapse" :title="t('sidebar.collapse')" :aria-label="t('sidebar.collapse')" @click="emit('collapse', true)"><PanelLeftClose :size="17" /></button>
      </div>
      <button class="sidebar-toggle sidebar-toggle-expand" :title="t('sidebar.expand')" :aria-label="t('sidebar.expand')" @click="emit('collapse', false)"><PanelLeftOpen :size="19" /></button>
    </div>
    <nav :aria-label="t('sidebar.primaryNavigation')">
      <button class="nav-item active" :title="t('app.allConversations')" :aria-label="t('app.allConversations')">
        <span class="nav-item-expanded"><Inbox :size="17" /><span>{{ t('app.allConversations') }}</span><em class="nav-item-count">{{ total }}</em></span>
      </button>
    </nav>
    <div class="sidebar-section">
      <p>{{ t('sidebar.sources') }}</p>
      <div class="source-picker">
        <span class="source-highlight" :style="{ transform: `translateY(${sourceIndex * 34}px)`, '--source-accent': sourceAccent }"></span>
        <button :class="['source-item', { active: platform === '' }]" :title="t('sidebar.allSources')" :aria-label="t('sidebar.allSources')" @pointerdown="handlePlatformPointerDown('', $event)" @click="handlePlatformClick('')"><i class="source-glyph all">{{ t('sidebar.allGlyph') }}</i><span>{{ t('sidebar.allSources') }}</span></button>
        <button :class="['source-item', { active: platform === 'deepseek' }]" title="DeepSeek" aria-label="DeepSeek" @pointerdown="handlePlatformPointerDown('deepseek', $event)" @click="handlePlatformClick('deepseek')"><i class="source-glyph deepseek">D</i><span>DeepSeek</span></button>
        <button :class="['source-item', { active: platform === 'doubao' }]" :title="t('app.platformDoubao')" :aria-label="t('app.platformDoubao')" @pointerdown="handlePlatformPointerDown('doubao', $event)" @click="handlePlatformClick('doubao')"><i class="source-glyph doubao">{{ t('sidebar.doubaoGlyph') }}</i><span>{{ t('app.platformDoubao') }}</span></button>
        <button :class="['source-item', { active: platform === 'kimi' }]" title="Kimi" aria-label="Kimi" @pointerdown="handlePlatformPointerDown('kimi', $event)" @click="handlePlatformClick('kimi')"><i class="source-glyph kimi">K</i><span>Kimi</span></button>
      </div>
    </div>
    <div class="sidebar-footer">
      <div class="service-state" :class="apiStatus.service.state">
        <span class="status-dot"></span>
        <div><strong>{{ statusLabel }}</strong><small :class="['connection-label', { connected: apiStatus.userscript_connected }]">{{ apiStatus.userscript_connected ? t('sidebar.userscriptConnected') : t('sidebar.waitingForUserscript') }}</small></div>
      </div>
      <button :class="['icon-button', 'settings-button', { connected: apiStatus.userscript_connected }]" :title="t('sidebar.settings')" :aria-label="t('sidebar.settings')" @click="emit('openSettings')"><Settings :size="18" /></button>
    </div>
  </aside>
</template>
