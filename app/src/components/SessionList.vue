<script setup lang="ts">
import { Archive, LoaderCircle } from 'lucide-vue-next'
import type { SessionSummary } from '../conversation'
import { escapeTitle } from '../markdown'
import { currentLocale, translate as t } from '../i18n'
import { formatDate as localizedDate } from '../i18n/locale'

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
  return ({ deepseek: 'DeepSeek', doubao: t('app.platformDoubao'), kimi: 'Kimi' } as Record<string, string>)[value] || value
}
function formatDate(value?: string) {
  if (!value) return '-'
  return localizedDate(value, currentLocale(), true)
}

function handleSessionPointerDown(id: string, event: PointerEvent) {
  if (event.button !== 0) return
  emit('select', id)
}

function handleSessionClick(id: string) {
  emit('select', id)
}
</script>

<template>
  <div class="session-pane">
    <div class="table-head"><span>{{ t('session.conversation') }}</span><span>{{ t('session.source') }}</span><span>{{ t('session.updated') }}</span></div>
    <Transition name="session-state" mode="out-in">
      <div v-if="loading && !sessions.length" key="loading" class="loading-state"><LoaderCircle class="spinning" :size="22" /><span>{{ t('session.reading') }}</span></div>
      <div v-else-if="!sessions.length" key="empty" class="empty-state">
        <Archive :size="30" />
        <strong>{{ filtered ? t('session.noMatches') : t('session.noRecords') }}</strong>
        <span>{{ filtered ? t('session.adjustFilters') : t('session.emptyHint') }}</span>
      </div>
      <div v-else key="list" class="session-list-wrapper">
        <TransitionGroup name="session-item" tag="div" class="session-items">
          <button v-for="session in sessions" :key="session.id" :class="['session-row', { selected: selectedId === session.id }]" @pointerdown="handleSessionPointerDown(session.id, $event)" @click="handleSessionClick(session.id)">
            <span class="session-title"><strong v-html="highlightTitle(session.title)"></strong></span>
            <span class="platform-cell"><i :class="session.platform"></i>{{ platformName(session.platform) }}</span>
            <time>{{ formatDate(session.updated_at) }}</time>
          </button>
        </TransitionGroup>
        <button v-if="sessions.length < total" class="load-more" :disabled="loading" @click="emit('loadMore')">{{ loading ? t('session.loading') : t('session.loadMore', { count: total - sessions.length }) }}</button>
      </div>
    </Transition>
  </div>
</template>
