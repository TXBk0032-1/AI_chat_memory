<script setup lang="ts">
import { computed } from 'vue'
import { Inbox, MessageSquareText, PanelLeftClose, PanelLeftOpen, Settings } from 'lucide-vue-next'
import type { ApiStatus } from '../desktop-api'

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
const statusLabel = computed(() => props.apiStatus.service.state === 'running' ? '同步服务运行中' : props.apiStatus.service.state === 'failed' ? '同步服务异常' : '同步服务启动中')
</script>

<template>
  <aside class="sidebar" :aria-label="collapsed ? '已折叠的来源导航' : '来源导航'">
    <div class="identity">
      <div class="identity-content">
        <div class="identity-mark"><MessageSquareText :size="20" /></div>
        <div class="identity-name"><strong>对话归档</strong><span>AI Chat Memory</span></div>
        <button class="sidebar-toggle sidebar-toggle-collapse" title="折叠侧边栏" aria-label="折叠侧边栏" @click="emit('collapse', true)"><PanelLeftClose :size="17" /></button>
      </div>
      <button class="sidebar-toggle sidebar-toggle-expand" title="展开侧边栏" aria-label="展开侧边栏" @click="emit('collapse', false)"><PanelLeftOpen :size="19" /></button>
    </div>
    <nav aria-label="主要导航">
      <button class="nav-item active" title="全部对话" aria-label="全部对话">
        <span class="nav-item-expanded"><Inbox :size="17" /><span>全部对话</span><em>{{ total }}</em></span>
        <em class="nav-item-collapsed" aria-hidden="true">{{ total }}</em>
      </button>
    </nav>
    <div class="sidebar-section">
      <p>来源</p>
      <div class="source-picker">
        <span class="source-highlight" :style="{ transform: `translateY(${sourceIndex * 34}px)`, '--source-accent': sourceAccent }"></span>
        <button :class="['source-item', { active: platform === '' }]" title="全部来源" aria-label="全部来源" @click="emit('selectPlatform', '')"><i class="source-glyph all">全</i><span>全部来源</span></button>
        <button :class="['source-item', { active: platform === 'deepseek' }]" title="DeepSeek" aria-label="DeepSeek" @click="emit('selectPlatform', 'deepseek')"><i class="source-glyph deepseek">D</i><span>DeepSeek</span></button>
        <button :class="['source-item', { active: platform === 'doubao' }]" title="豆包" aria-label="豆包" @click="emit('selectPlatform', 'doubao')"><i class="source-glyph doubao">豆</i><span>豆包</span></button>
        <button :class="['source-item', { active: platform === 'kimi' }]" title="Kimi" aria-label="Kimi" @click="emit('selectPlatform', 'kimi')"><i class="source-glyph kimi">K</i><span>Kimi</span></button>
      </div>
    </div>
    <div class="sidebar-footer">
      <div class="service-state" :class="apiStatus.service.state">
        <span class="status-dot"></span>
        <div><strong>{{ statusLabel }}</strong><small :class="['connection-label', { connected: apiStatus.userscript_connected }]">{{ apiStatus.userscript_connected ? '网页脚本已连接' : '等待网页脚本连接' }}</small></div>
      </div>
      <button :class="['icon-button', 'settings-button', { connected: apiStatus.userscript_connected }]" title="设置" aria-label="设置" @click="emit('openSettings')"><Settings :size="18" /></button>
    </div>
  </aside>
</template>
