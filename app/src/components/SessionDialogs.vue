<script setup lang="ts">
import { Trash2, X } from 'lucide-vue-next'
import type { SessionOpen } from '../conversation'
import { translate as t } from '../i18n'

defineProps<{
  selected: SessionOpen | null
  showInfo: boolean
  showDelete: boolean
  showClose: boolean
  formatDate: (value?: string) => string
  platformName: (value: string) => string
}>()
const pendingCloseBehavior = defineModel<'hide_to_tray' | 'exit' | null>('pendingCloseBehavior', { required: true })
const emit = defineEmits<{
  closeInfo: []
  closeDelete: []
  delete: []
  cancelClose: []
  confirmClose: []
}>()
</script>

<template>
  <Transition name="settings-modal">
    <div v-if="showInfo && selected" class="dialog-backdrop" @click.self="emit('closeInfo')">
      <section class="info-dialog" role="dialog" aria-modal="true" aria-labelledby="info-title">
        <header><h2 id="info-title">{{ t('dialogs.infoTitle') }}</h2><button class="icon-button" :title="t('app.close')" @click="emit('closeInfo')"><X :size="18" /></button></header>
        <dl><dt>{{ t('dialogs.fieldTitle') }}</dt><dd>{{ selected.title || t('app.untitledConversation') }}</dd><dt>{{ t('dialogs.fieldSource') }}</dt><dd>{{ platformName(selected.platform) }}</dd><dt>{{ t('dialogs.fieldSessionId') }}</dt><dd class="identifier">{{ selected.platform_session_id }}</dd><dt>{{ t('dialogs.fieldCreated') }}</dt><dd>{{ formatDate(selected.created_at) }}</dd><dt>{{ t('dialogs.fieldUpdated') }}</dt><dd>{{ formatDate(selected.updated_at) }}</dd><dt>{{ t('dialogs.fieldMessages') }}</dt><dd>{{ selected.message_count }}</dd></dl>
      </section>
    </div>
  </Transition>

  <Transition name="settings-modal">
    <div v-if="showDelete && selected" class="dialog-backdrop" @click.self="emit('closeDelete')">
      <section class="delete-dialog" role="alertdialog" aria-modal="true" aria-labelledby="delete-title">
        <header><div><h2 id="delete-title">{{ t('dialogs.deleteTitle') }}</h2><p>{{ t('dialogs.deleteDescription', { title: selected.title || t('app.untitledConversation') }) }}</p></div></header>
        <footer><button class="secondary-button" @click="emit('closeDelete')">{{ t('app.cancel') }}</button><button class="danger-button" @click="emit('delete')"><Trash2 :size="15" />{{ t('dialogs.confirmDelete') }}</button></footer>
      </section>
    </div>
  </Transition>

  <Transition name="settings-modal">
    <div v-if="showClose" class="dialog-backdrop close-prompt-backdrop">
      <section class="close-prompt" role="alertdialog" aria-modal="true" aria-labelledby="close-prompt-title">
        <header><h2 id="close-prompt-title">{{ t('dialogs.closeTitle') }}</h2><p>{{ t('dialogs.closeDescription') }}</p></header>
        <div class="close-options">
          <label :class="{ selected: pendingCloseBehavior === 'hide_to_tray' }"><input type="checkbox" :checked="pendingCloseBehavior === 'hide_to_tray'" @change="pendingCloseBehavior='hide_to_tray'" /><span><strong>{{ t('dialogs.hideToTray') }}</strong><small>{{ t('dialogs.hideToTrayHint') }}</small></span></label>
          <label :class="{ selected: pendingCloseBehavior === 'exit' }"><input type="checkbox" :checked="pendingCloseBehavior === 'exit'" @change="pendingCloseBehavior='exit'" /><span><strong>{{ t('dialogs.exit') }}</strong><small>{{ t('dialogs.exitHint') }}</small></span></label>
        </div>
        <footer><button class="secondary-button" @click="emit('cancelClose')">{{ t('app.cancel') }}</button><button class="primary-button" :disabled="!pendingCloseBehavior" @click="emit('confirmClose')">{{ t('dialogs.confirm') }}</button></footer>
      </section>
    </div>
  </Transition>
</template>
