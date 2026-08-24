<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import type { Message, Reference } from './conversation'
import { renderMarkdown } from './markdown'
import { createMessagePreview, isOversizedMessage } from './message-display'
import { currentLocale, translate as t } from './i18n'


function formatCharacterCount(value: number) {
  return new Intl.NumberFormat(currentLocale()).format(value)
}

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
const contentHtml = computed(() => {
  if (lightweightContent.value) return ''
  const t0 = performance.now()
  const result = renderMarkdown(props.message.content, props.message, props.references, props.query)
  const elapsed = performance.now() - t0
  if (elapsed > 4) {
    console.debug(`[PERF:MARKDOWN] Rendered msg seq=${props.message.seq} (len=${props.message.content.length}) in ${elapsed.toFixed(2)}ms`)
  }
  return result
})
const thinking = computed(() => typeof props.message.metadata?.thinking === 'string' ? props.message.metadata.thinking : '')
const thinkingHtml = computed(() => {
  if (!props.expanded || !thinking.value) return ''
  const t0 = performance.now()
  const result = renderMarkdown(thinking.value, props.message, props.references, props.query)
  const elapsed = performance.now() - t0
  if (elapsed > 4) {
    console.debug(`[PERF:MARKDOWN:THINKING] Rendered thinking seq=${props.message.seq} in ${elapsed.toFixed(2)}ms`)
  }
  return result
})
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

function handleBlockClick(event: MouseEvent) {
  const target = event.target as HTMLElement | null
  const button = target?.closest<HTMLButtonElement>('.code-copy-button')
  if (!button) return
  const wrapper = button.closest('.code-block-wrapper')
  const codeElement = wrapper?.querySelector('pre code')
  if (!codeElement) return
  const text = codeElement.textContent || ''
  void navigator.clipboard.writeText(text).then(() => {
    button.classList.add('copied')
    const copyText = button.querySelector('.copy-text')
    if (copyText) copyText.textContent = t('mcp.copied')
    window.setTimeout(() => {
      button.classList.remove('copied')
      if (copyText) copyText.textContent = t('app.copy')
    }, 2000)
  })
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
  <article :data-message-id="message.id" :class="['message-block', message.role]" @click="handleBlockClick">
    <div class="message-author"><span>{{ roleLabel }}</span><time>{{ formattedDate }}</time></div>
    <section v-if="thinking" :class="['thinking', { open: expanded }]">
      <button class="thinking-toggle" :aria-expanded="expanded" @click="$emit('toggleThinking', message.id)">{{ t('message.showThinking') }}</button>
      <div class="thinking-reveal" :aria-hidden="!expanded"><div><div v-if="expanded" class="markdown" data-search-field="thinking" v-html="thinkingHtml"></div></div></div>
    </section>
    <div v-if="lightweightContent" class="oversized-message">
      <pre class="oversized-message-preview" data-search-field="content">{{ contentPreview.text }}</pre>
      <div class="oversized-message-controls">
        <span class="oversized-message-meta">{{ t('message.originalCharacters', { count: formatCharacterCount(contentPreview.originalLength) }) }}</span>
        <button class="oversized-message-toggle" type="button" @click="showFullContent">{{ t('message.renderFullMarkdown') }}</button>
      </div>
    </div>
    <template v-else>
      <div class="markdown" data-search-field="content" v-html="contentHtml"></div>
      <div v-if="canRestoreLightweight" class="oversized-message-controls full-content-controls">
        <span class="oversized-message-meta">{{ t('message.originalCharacters', { count: formatCharacterCount(message.content.length) }) }}</span>
        <button class="oversized-message-toggle" type="button" @click="restoreLightweightContent">{{ t('message.restoreLightweight') }}</button>
      </div>
    </template>
  </article>
</template>
