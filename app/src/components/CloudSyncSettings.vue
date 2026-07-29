<script setup lang="ts">
import { Cloud, RefreshCw, ShieldCheck, Wifi } from 'lucide-vue-next'
import type { CloudSyncSettings, CloudSyncStatus } from '../desktop-api'
const { status, busy } = defineProps<{ status: CloudSyncStatus; busy?: boolean }>()
const settings = defineModel<CloudSyncSettings>('settings', { required: true })
const password = defineModel<string>('password', { required: true })
const syncPassword = defineModel<string>('syncPassword', { required: true })
const emit = defineEmits<{ test: []; sync: []; rewrite: [] }>()
</script>
<template>
  <section class="setting-group cloud-sync-settings">
    <div class="setting-heading"><Cloud :size="18" /><div><h3>云同步</h3><p>仅使用 WebDAV；本地凭据不写入设置文件。</p></div></div>
    <div class="setting-row"><span>启用自动同步</span><label class="switch"><input v-model="settings.enabled" type="checkbox" /><span /></label></div>
    <template v-if="settings.enabled">
      <label><span>WebDAV 地址</span><input v-model="settings.base_url" type="url" autocomplete="url" /></label>
      <label><span>远端目录</span><input v-model="settings.root_path" /></label>
      <label><span>用户名</span><input v-model="settings.username" autocomplete="username" /></label>
      <label><span>WebDAV 密码</span><input v-model="password" type="password" autocomplete="current-password" /></label>
      <label class="setting-row"><span>启用包加密</span><label class="switch"><input v-model="settings.encryption_enabled" type="checkbox" /><span /></label></label>
      <label v-if="settings.encryption_enabled"><span>同步密码</span><input v-model="syncPassword" type="password" autocomplete="new-password" /></label>
      <p class="path-value"><Wifi :size="14" /> 状态：{{ status.state }} · 待上传 {{ status.pending_mutations }}</p>
      <div class="setting-actions">
        <button class="secondary-button compact" type="button" :disabled="busy" @click="emit('test')"><ShieldCheck :size="14" />测试连接</button>
        <button class="secondary-button compact" type="button" :disabled="busy" @click="emit('sync')"><RefreshCw :size="14" />立即同步</button>
        <button class="secondary-button compact" type="button" :disabled="busy" @click="emit('rewrite')">重写云端存档</button>
      </div>
      <p v-if="status.last_error_message" class="path-value">{{ status.last_error_message }}</p>
    </template>
  </section>
</template>
