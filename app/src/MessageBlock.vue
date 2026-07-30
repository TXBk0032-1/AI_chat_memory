<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import type { Message, Reference } from './conversation'
import { renderMarkdown } from './markdown'
import { createMessagePreview, isOversizedMessage } from './message-display'

const props = defineProps<{
  message: Message
  references: Map<number, Reference>
  query: string
  expanded: boolean
  formattedDate: string
  roleLabel: string
}>()

const emit = defineEmits<{ toggleThinking: [messageId: string]; contentRendered: [] }>()

const fullContentMessageId = ref<string | null>(null)
const oversizedContent = computed(() => isOversizedMessage(props.message.content))
const hasQuery = computed(() => Boolean(props.query.trim()))
const lightweightContent = computed(() => (
  oversizedContent.value
  && !hasQuery.value
  && fullContentMessageId.value !== props.message.id
))
const contentPreview = computed(() => createMessagePreview(props.message.content))
const contentHtml = computed(() => lightweightContent.value
  ? ''
  : renderMarkdown(props.message.content, props.message, props.references, props.query))
const thinking = computed(() => typeof props.message.metadata?.thinking === 'string' ? props.message.metadata.thinking : '')
const thinkingHtml = computed(() => props.expanded && thinking.value ? renderMarkdown(thinking.value, props.message, props.references, props.query) : '')
const canRestoreLightweight = computed(() => (
  oversizedContent.value
  && !hasQuery.value
  && fullContentMessageId.value === props.message.id
))

function showFullContent() {
  fullContentMessageId.value = props.message.id
}

function restoreLightweightContent() {
  fullContentMessageId.value = null
}

function notifyRendered() {
  void nextTick(() => emit('contentRendered'))
}

onMounted(notifyRendered)
watch(() => props.message.id, () => {
  fullContentMessageId.value = null
})
watch([contentHtml, thinkingHtml], notifyRendered)
</script>

<template>
  <article :data-message-id="message.id" :class="['message-block', message.role]">
    <div class="message-author"><span>{{ roleLabel }}</span><time>{{ formattedDate }}</time></div>
    <section v-if="thinking" :class="['thinking', { open: expanded }]">
      <button class="thinking-toggle" :aria-expanded="expanded" @click="$emit('toggleThinking', message.id)">查看思考过程</button>
      <div class="thinking-reveal" :aria-hidden="!expanded"><div><div v-if="expanded" class="markdown" data-search-field="thinking" v-html="thinkingHtml"></div></div></div>
    </section>
    <div v-if="lightweightContent" class="oversized-message">
      <pre class="oversized-message-preview" data-search-field="content">{{ contentPreview.text }}</pre>
      <div class="oversized-message-controls">
        <span class="oversized-message-meta">原始字符数：{{ contentPreview.originalLength.toLocaleString('zh-CN') }}</span>
        <button class="oversized-message-toggle" type="button" @click="showFullContent">渲染完整 Markdown</button>
      </div>
    </div>
    <template v-else>
      <div class="markdown" data-search-field="content" v-html="contentHtml"></div>
      <div v-if="canRestoreLightweight" class="oversized-message-controls full-content-controls">
        <span class="oversized-message-meta">原始字符数：{{ message.content.length.toLocaleString('zh-CN') }}</span>
        <button class="oversized-message-toggle" type="button" @click="restoreLightweightContent">恢复轻量模式</button>
      </div>
    </template>
  </article>
</template>
