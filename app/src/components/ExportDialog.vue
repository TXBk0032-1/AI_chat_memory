<script setup lang="ts">
import { Braces, FileImage, FileText, LoaderCircle, X } from 'lucide-vue-next'
import type { ExportFormat } from '../conversation-export'

defineProps<{
  visible: boolean
  selectedCount: number
  busy: boolean
  imageDisabled: boolean
  imageDisabledReason: string
}>()
const format = defineModel<ExportFormat>('format', { required: true })
const includeThinking = defineModel<boolean>('includeThinking', { required: true })
const emit = defineEmits<{ close: []; export: [] }>()

const formats: Array<{ value: ExportFormat; label: string; icon: typeof FileImage }> = [
  { value: 'png', label: 'PNG', icon: FileImage },
  { value: 'jpeg', label: 'JPEG', icon: FileImage },
  { value: 'md', label: 'Markdown', icon: FileText },
  { value: 'json', label: 'JSON', icon: Braces },
]

function isImageFormat(value: ExportFormat) {
  return value === 'png' || value === 'jpeg'
}
</script>

<template>
  <Transition name="settings-modal">
    <div v-if="visible" class="dialog-backdrop" @click.self="!busy && emit('close')">
      <section class="export-dialog" role="dialog" aria-modal="true" aria-labelledby="export-title">
        <header>
          <div><h2 id="export-title">导出聊天记录</h2><p>已选择 {{ selectedCount }} 组问答</p></div>
          <button class="icon-button" title="关闭" :disabled="busy" @click="emit('close')"><X :size="18" /></button>
        </header>
        <div class="export-dialog-body">
          <div class="export-format-control" role="radiogroup" aria-label="导出格式">
            <button
              v-for="item in formats"
              :key="item.value"
              :class="{ active: format === item.value, 'format-disabled': isImageFormat(item.value) && imageDisabled }"
              role="radio"
              :aria-checked="format === item.value"
              :aria-disabled="isImageFormat(item.value) && imageDisabled"
              :title="isImageFormat(item.value) && imageDisabled ? imageDisabledReason : ''"
              :disabled="busy"
              @click="isImageFormat(item.value) && imageDisabled ? undefined : format=item.value"
            >
              <component :is="item.icon" :size="18" /><span>{{ item.label }}</span>
            </button>
          </div>
          <label class="export-thinking-option">
            <input v-model="includeThinking" type="checkbox" :disabled="busy" />
            <span><strong>包含思考过程</strong><small>仅导出所选助手消息中已有的思考内容</small></span>
          </label>
        </div>
        <footer>
          <button class="secondary-button" :disabled="busy" @click="emit('close')">取消</button>
          <button class="primary-button" :disabled="busy" @click="emit('export')"><LoaderCircle v-if="busy" class="spinning" :size="15" />导出</button>
        </footer>
      </section>
    </div>
  </Transition>
</template>
