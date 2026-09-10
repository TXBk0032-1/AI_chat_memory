<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'
import type { Message, Reference } from '../conversation'
import { translate as t } from '../i18n'
import { useChunkedExportRenderer } from '../composables/useChunkedExportRenderer'

const props = defineProps<{
  title: string
  time: string
  platform: string
  messages: Message[]
  references: Map<number, Reference>
  includeThinking: boolean
  isPdf?: boolean
  compact?: boolean
  includeCoverPage?: boolean
}>()

const root = ref<HTMLElement | null>(null)

// Rendering every message's markdown synchronously blocks the UI thread for
// large exports; the chunked renderer spreads it across animation frames and
// lets export flows await `whenReady()` before touching the document.
const { rendered, restart, whenReady, cancel } = useChunkedExportRenderer(
  () => ({ messages: props.messages, references: props.references, includeThinking: props.includeThinking }),
)

watch(() => [props.messages, props.references, props.includeThinking], restart, { immediate: true, deep: false })

onBeforeUnmount(cancel)

function roleLabel(role: string) {
  return role.toUpperCase()
}

defineExpose({
  getElement: () => root.value,
  whenReady,
})
</script>

<template>
  <article
    ref="root"
    class="export-document"
    :class="{
      'export-document--compact': compact,
      'export-document--pdf': isPdf,
    }"
  >
    <section v-if="isPdf && includeCoverPage" class="export-cover-page">
      <div class="export-cover-platform">{{ platform }}</div>
      <h1 class="export-cover-title">{{ title || t('app.untitledConversation') }}</h1>
      <div class="export-cover-meta">
        <p><span class="meta-label">{{ t('export.timeLabel') }}</span><span class="meta-value">{{ time }}</span></p>
        <p><span class="meta-label">{{ t('export.totalMessages') }}</span><span class="meta-value">{{ messages.length }}</span></p>
        <p><span class="meta-label">{{ t('export.generatedBy') }}</span><span class="meta-value">AI Chat Memory</span></p>
      </div>
    </section>
    <header class="export-document-header">
      <span>{{ platform }}</span>
      <h1>{{ title || t('app.untitledConversation') }}</h1>
      <time>{{ time }}</time>
    </header>
    <section v-for="item in rendered" :key="item.message.id" class="export-message">
      <div class="export-message-role">{{ roleLabel(item.message.role) }}</div>
      <div v-if="item.thinking" class="export-thinking">
        <strong>{{ t('export.thinkingLabel', { role: roleLabel(item.message.role) }) }}</strong>
        <div class="markdown" v-html="item.thinking"></div>
      </div>
      <div class="markdown" v-html="item.content"></div>
    </section>
  </article>
</template>
