<script setup lang="ts">
import { Trash2, X } from 'lucide-vue-next'
import type { SessionOpen } from '../conversation'

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
        <header><h2 id="info-title">对话详细信息</h2><button class="icon-button" title="关闭" @click="emit('closeInfo')"><X :size="18" /></button></header>
        <dl><dt>标题</dt><dd>{{ selected.title || '未命名对话' }}</dd><dt>来源</dt><dd>{{ platformName(selected.platform) }}</dd><dt>来源会话 ID</dt><dd class="identifier">{{ selected.platform_session_id }}</dd><dt>创建时间</dt><dd>{{ formatDate(selected.created_at) }}</dd><dt>更新时间</dt><dd>{{ formatDate(selected.updated_at) }}</dd><dt>消息数量</dt><dd>{{ selected.message_count }}</dd></dl>
      </section>
    </div>
  </Transition>

  <Transition name="settings-modal">
    <div v-if="showDelete && selected" class="dialog-backdrop" @click.self="emit('closeDelete')">
      <section class="delete-dialog" role="alertdialog" aria-modal="true" aria-labelledby="delete-title">
        <header><div><h2 id="delete-title">删除对话</h2><p>“{{ selected.title || '未命名对话' }}”及其全部消息将被永久删除，此操作无法撤销。</p></div></header>
        <footer><button class="secondary-button" @click="emit('closeDelete')">取消</button><button class="danger-button" @click="emit('delete')"><Trash2 :size="15" />确认删除</button></footer>
      </section>
    </div>
  </Transition>

  <Transition name="settings-modal">
    <div v-if="showClose" class="dialog-backdrop close-prompt-backdrop">
      <section class="close-prompt" role="alertdialog" aria-modal="true" aria-labelledby="close-prompt-title">
        <header><h2 id="close-prompt-title">关闭对话归档</h2><p>请选择关闭窗口后要执行的操作。你的选择会保存，也可以稍后在设置中修改。</p></header>
        <div class="close-options">
          <label :class="{ selected: pendingCloseBehavior === 'hide_to_tray' }"><input type="checkbox" :checked="pendingCloseBehavior === 'hide_to_tray'" @change="pendingCloseBehavior='hide_to_tray'" /><span><strong>退出到托盘</strong><small>隐藏主窗口，本地同步服务继续运行。</small></span></label>
          <label :class="{ selected: pendingCloseBehavior === 'exit' }"><input type="checkbox" :checked="pendingCloseBehavior === 'exit'" @change="pendingCloseBehavior='exit'" /><span><strong>完全关闭</strong><small>退出应用并停止本地同步服务。</small></span></label>
        </div>
        <footer><button class="secondary-button" @click="emit('cancelClose')">取消</button><button class="primary-button" :disabled="!pendingCloseBehavior" @click="emit('confirmClose')">确认</button></footer>
      </section>
    </div>
  </Transition>
</template>
