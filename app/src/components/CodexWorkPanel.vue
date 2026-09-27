<script setup lang="ts">
import { ref } from 'vue'
import type { WorkItem } from '../conversation'
import { translate as t } from '../i18n'

const props = defineProps<{ items: WorkItem[] }>()
const emit = defineEmits<{ openSubagent: [agentThreadId: string] }>()

// 每个可展开项独立记录开合状态，key 用 work_seq（同一会话内唯一）。
const openKeys = ref(new Set<number>())

function isReasoning(item: WorkItem) { return item.kind === 'reasoning' }
function isSubagent(item: WorkItem) { return item.kind === 'subagent' }
// 加密思考：reasoning 且无正文 → 只读单行。
function isLocked(item: WorkItem) { return isReasoning(item) && !item.body }
function isExpandable(item: WorkItem) { return !isSubagent(item) && item.expandable && !!item.body }

function toggle(item: WorkItem) {
  if (isSubagent(item)) {
    if (item.agent_thread_id) emit('openSubagent', item.agent_thread_id)
    return
  }
  if (!isExpandable(item)) return
  const next = new Set(openKeys.value)
  if (next.has(item.work_seq)) next.delete(item.work_seq)
  else next.add(item.work_seq)
  openKeys.value = next
}

function kindLabel(item: WorkItem) {
  const map: Record<WorkItem['kind'], string> = {
    commentary: t('work.kindCommentary'),
    reasoning: t('work.kindReasoning'),
    command: t('work.kindCommand'),
    mcp_tool: t('work.kindMcpTool'),
    file_change: t('work.kindFileChange'),
    subagent: t('work.kindSubagent'),
    plan: t('work.kindPlan'),
    output: t('work.kindOutput'),
  }
  return map[item.kind] ?? item.kind
}

function headerText(item: WorkItem) {
  if (isLocked(item)) return t('work.reasoningLine', { summary: item.title })
  if (isSubagent(item)) {
    return t('work.subagentLine', { agent: item.agent_label || item.agent_thread_id || '' })
  }
  return item.title
}
</script>

<template>
  <div class="codex-work" role="group" :aria-label="t('work.panelLabel')">
    <div v-for="item in props.items" :key="item.work_seq" :class="['work-item', item.kind, { locked: isLocked(item), open: openKeys.has(item.work_seq) }]">
      <button
        v-if="!isLocked(item)"
        class="work-item-toggle"
        type="button"
        :aria-expanded="isExpandable(item) ? openKeys.has(item.work_seq) : undefined"
        :data-interactive="isExpandable(item) || isSubagent(item) ? 'true' : 'false'"
        @click="toggle(item)"
      >
        <span class="work-item-kind">{{ kindLabel(item) }}</span>
        <span class="work-item-title">{{ headerText(item) }}</span>
      </button>
      <div v-else class="work-item-line">
        <span class="work-item-kind">{{ kindLabel(item) }}</span>
        <span class="work-item-title">{{ headerText(item) }}</span>
      </div>
      <div v-if="isExpandable(item) && openKeys.has(item.work_seq)" class="work-item-reveal">
        <pre class="work-item-body">{{ item.body }}</pre>
        <p v-if="item.truncated" class="work-item-truncated">{{ t('work.truncated') }}</p>
      </div>
    </div>
  </div>
</template>
