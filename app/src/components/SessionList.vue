<script setup lang="ts">
import { Archive, LoaderCircle } from 'lucide-vue-next'
import type { SessionSummary } from '../conversation'
import { escapeTitle } from '../markdown'

const props = defineProps<{
  sessions: SessionSummary[]
  total: number
  loading: boolean
  selectedId?: string
  filtered: boolean
  query: string
}>()
const emit = defineEmits<{ select: [id: string]; loadMore: [] }>()

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
function highlightTitle(value: string) {
  const html = escapeTitle(value)
  if (!props.query) return html
  const pattern = new RegExp(escapeRegExp(props.query), 'gi')
  return html.replace(pattern, (match) => `<mark class="search-hit">${match}</mark>`)
}
function platformName(value: string) {
  return ({ deepseek: 'DeepSeek', doubao: '豆包', kimi: 'Kimi' } as Record<string, string>)[value] || value
}
function formatDate(value?: string) {
  if (!value) return '-'
  const date = new Date(Number(value) * 1000)
  if (Number.isNaN(date.getTime())) return value
  return new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(date)
}
</script>

<template>
  <div class="session-pane">
    <div class="table-head"><span>对话</span><span>来源</span><span>更新时间</span></div>
    <div v-if="loading && !sessions.length" class="loading-state"><LoaderCircle class="spinning" :size="22" /><span>正在读取对话</span></div>
    <button v-for="session in sessions" :key="session.id" :class="['session-row', { selected: selectedId === session.id }]" @click="emit('select', session.id)">
      <span class="session-title"><strong v-html="highlightTitle(session.title)"></strong></span>
      <span class="platform-cell"><i :class="session.platform"></i>{{ platformName(session.platform) }}</span>
      <time>{{ formatDate(session.updated_at) }}</time>
    </button>
    <div v-if="!loading && !sessions.length" class="empty-state">
      <Archive :size="30" />
      <strong>{{ filtered ? '没有匹配的对话' : '还没有对话记录' }}</strong>
      <span>{{ filtered ? '调整搜索或筛选条件后重试' : '同步 userscript 或导入 DeepSeek ZIP 后会显示在这里' }}</span>
    </div>
    <button v-if="sessions.length < total" class="load-more" :disabled="loading" @click="emit('loadMore')">{{ loading ? '加载中' : `加载更多（剩余 ${total-sessions.length} 条）` }}</button>
  </div>
</template>
