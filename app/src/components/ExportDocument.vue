<script setup lang="ts">
import { computed, ref } from 'vue'
import type { Message, Reference } from '../conversation'
import { renderMarkdown } from '../markdown'

const props = defineProps<{
  title: string
  time: string
  platform: string
  messages: Message[]
  references: Map<number, Reference>
  includeThinking: boolean
}>()

const root = ref<HTMLElement | null>(null)
const rendered = computed(() => props.messages.map((message) => ({
  message,
  content: renderMarkdown(message.content, message, props.references, ''),
  thinking: props.includeThinking && typeof message.metadata?.thinking === 'string'
    ? renderMarkdown(message.metadata.thinking, message, props.references, '')
    : '',
})))

function roleLabel(role: string) {
  return role.toUpperCase()
}

defineExpose({ getElement: () => root.value })
</script>

<template>
  <article ref="root" class="export-document">
    <header class="export-document-header">
      <span>{{ platform }}</span>
      <h1>{{ title || '未命名对话' }}</h1>
      <time>{{ time }}</time>
    </header>
    <section v-for="item in rendered" :key="item.message.id" class="export-message">
      <div class="export-message-role">{{ roleLabel(item.message.role) }}</div>
      <div v-if="item.thinking" class="export-thinking">
        <strong>{{ roleLabel(item.message.role) }} 思考过程</strong>
        <div class="markdown" v-html="item.thinking"></div>
      </div>
      <div class="markdown" v-html="item.content"></div>
    </section>
  </article>
</template>
