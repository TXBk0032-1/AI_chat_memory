<script setup lang="ts">
import { computed, nextTick, onMounted, watch } from 'vue'
import type { Message, Reference } from './conversation'
import { renderMarkdown } from './markdown'
import { translate as t } from './i18n'

const props = defineProps<{
  message: Message
  references: Map<number, Reference>
  query: string
  expanded: boolean
  formattedDate: string
  roleLabel: string
}>()

const emit = defineEmits<{ toggleThinking: [messageId: string]; contentRendered: [] }>()

const contentHtml = computed(() => renderMarkdown(props.message.content, props.message, props.references, props.query))
const thinking = computed(() => typeof props.message.metadata?.thinking === 'string' ? props.message.metadata.thinking : '')
const thinkingHtml = computed(() => props.expanded ? renderMarkdown(thinking.value, props.message, props.references, props.query) : '')

function notifyRendered() {
  void nextTick(() => emit('contentRendered'))
}

onMounted(notifyRendered)
watch([contentHtml, thinkingHtml], notifyRendered)
</script>

<template>
  <article :data-message-id="message.id" :class="['message-block', message.role]">
    <div class="message-author"><span>{{ roleLabel }}</span><time>{{ formattedDate }}</time></div>
    <section v-if="thinking" :class="['thinking', { open: expanded }]">
      <button class="thinking-toggle" :aria-expanded="expanded" @click="$emit('toggleThinking', message.id)">{{ t('message.showThinking') }}</button>
      <div class="thinking-reveal" :aria-hidden="!expanded"><div><div v-if="expanded" class="markdown" data-search-field="thinking" v-html="thinkingHtml"></div></div></div>
    </section>
    <div class="markdown" data-search-field="content" v-html="contentHtml"></div>
  </article>
</template>
