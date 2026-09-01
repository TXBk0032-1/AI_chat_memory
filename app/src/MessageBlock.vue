<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { toolCallsFromMetadata, type Message, type Reference } from './conversation'
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

const emit = defineEmits<{ toggleThinking: [messageId: string]; contentRendered: [root: HTMLElement] }>()

const rootElement = ref<HTMLElement | null>(null)

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
const toolCalls = computed(() => toolCallsFromMetadata(props.message.metadata))
const toolCallsOpen = ref(false)

function toggleToolCalls() {
  toolCallsOpen.value = !toolCallsOpen.value
}

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
  const copyText = button.querySelector('.copy-text')
  const resetButton = () => {
    button.classList.remove('copied', 'copy-failed')
    if (copyText) copyText.textContent = t('app.copy')
  }
  navigator.clipboard.writeText(text).then(() => {
    button.classList.add('copied')
    if (copyText) copyText.textContent = t('settings.copied')
    window.setTimeout(resetButton, 2000)
  }, () => {
    // Clipboard access can be denied (window unfocused, permission refused);
    // surface the failure on the button instead of an unhandled rejection.
    button.classList.add('copy-failed')
    if (copyText) copyText.textContent = t('app.copyFailed')
    window.setTimeout(resetButton, 2000)
  })
}

function notifyRendered() {
  void nextTick(() => {
    // Scope the notification to this block so the parent renders Mermaid
    // diagrams inside this message only, instead of scanning the whole
    // document on every mount while the virtual list scrolls.
    if (rootElement.value) emit('contentRendered', rootElement.value)
  })
}

onMounted(notifyRendered)
watch(() => props.message.id, () => {
  fullContentMessageId.value = null
  toolCallsOpen.value = false
})
watch([contentHtml, thinkingHtml], notifyRendered)
</script>

<template>
  <article ref="rootElement" :data-message-id="message.id" :class="['message-block', message.role]" @click="handleBlockClick">
    <div class="message-author"><span>{{ roleLabel }}</span><time>{{ formattedDate }}</time></div>
    <section v-if="thinking" :class="['thinking', { open: expanded }]">
      <button class="thinking-toggle" :aria-expanded="expanded" @click="$emit('toggleThinking', message.id)">{{ t('message.showThinking') }}</button>
      <div class="thinking-reveal" :aria-hidden="!expanded"><div><div v-if="expanded" class="markdown" data-search-field="thinking" v-html="thinkingHtml"></div></div></div>
    </section>
    <section v-if="toolCalls.length" :class="['thinking', 'tool-calls', { open: toolCallsOpen }]">
      <button class="thinking-toggle" :aria-expanded="toolCallsOpen" @click="toggleToolCalls">{{ t('message.showToolCalls', { count: toolCalls.length }) }}</button>
      <div class="thinking-reveal" :aria-hidden="!toolCallsOpen"><div>
        <ul v-if="toolCallsOpen" class="tool-calls-list">
          <li v-for="(call, index) in toolCalls" :key="index" class="tool-call-item">
            <span class="tool-call-name">{{ call.name }}</span>
            <span v-if="call.results_count !== undefined" class="tool-call-meta">{{ t('message.toolCallResultCount', { count: call.results_count }) }}</span>
            <pre v-if="call.result" class="tool-call-result">{{ call.result }}</pre>
          </li>
        </ul>
      </div></div>
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
