<script setup lang="ts">
import { Braces, FileImage, FileText, LoaderCircle, X } from 'lucide-vue-next'
import type { ExportFormat } from '../conversation-export'
import { translate as t } from '../i18n'

defineProps<{
  visible: boolean
  selectedCount: number
  busy: boolean
  imageDisabled: boolean
  imageDisabledReason: string
}>()
const format = defineModel<ExportFormat>('format', { required: true })
const includeThinking = defineModel<boolean>('includeThinking', { required: true })
const compact = defineModel<boolean>('compact', { default: false })
const includeCoverPage = defineModel<boolean>('includeCoverPage', { default: false })
const emit = defineEmits<{ close: []; export: [] }>()

const formats: Array<{ value: ExportFormat; label: string; icon: typeof FileImage }> = [
  { value: 'png', label: 'PNG', icon: FileImage },
  { value: 'jpeg', label: 'JPEG', icon: FileImage },
  { value: 'pdf', label: 'PDF', icon: FileText },
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
          <div><h2 id="export-title">{{ t('export.dialogTitle') }}</h2><p>{{ t('export.selectedCount', { count: selectedCount }) }}</p></div>
          <button class="icon-button" :title="t('app.close')" :disabled="busy" @click="emit('close')"><X :size="18" /></button>
        </header>
        <div class="export-dialog-body">
          <div class="export-format-control" role="radiogroup" :aria-label="t('export.format')">
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
          <Transition name="pdf-options">
            <div v-if="format === 'pdf'" class="export-pdf-options">
              <label class="export-thinking-option">
                <input v-model="compact" type="checkbox" :disabled="busy" />
                <span><strong>{{ t('export.compactLayout') }}</strong><small>{{ t('export.compactLayoutHint') }}</small></span>
              </label>
              <label class="export-thinking-option">
                <input v-model="includeCoverPage" type="checkbox" :disabled="busy" />
                <span><strong>{{ t('export.includeCoverPage') }}</strong><small>{{ t('export.includeCoverPageHint') }}</small></span>
              </label>
            </div>
          </Transition>
          <label class="export-thinking-option">
            <input v-model="includeThinking" type="checkbox" :disabled="busy" />
            <span><strong>{{ t('export.includeThinking') }}</strong><small>{{ t('export.includeThinkingHint') }}</small></span>
          </label>
        </div>
        <footer>
          <button class="secondary-button" :disabled="busy" @click="emit('close')">{{ t('app.cancel') }}</button>
          <button class="primary-button" :disabled="busy" @click="emit('export')"><LoaderCircle v-if="busy" class="spinning" :size="15" />{{ t('export.action') }}</button>
        </footer>
      </section>
    </div>
  </Transition>
</template>
