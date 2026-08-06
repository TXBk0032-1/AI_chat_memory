<script setup lang="ts">
import { computed, ref } from 'vue'
import { Cloud, RefreshCw, ShieldCheck, Trash2, Wifi } from 'lucide-vue-next'
import type { CloudSyncSettings, CloudSyncStatus } from '../desktop-api'
import { cloudSyncProfilesEqual } from '../cloud-sync-profile'
type ConnectionTestState = 'idle' | 'pending' | 'passed' | 'failed'

const {
  status,
  busy,
  activeProfile,
  connectionTestState = 'idle',
  connectionTestMessage,
} = defineProps<{
  status: CloudSyncStatus
  busy?: boolean
  activeProfile?: CloudSyncSettings | null
  connectionTestState?: ConnectionTestState
  connectionTestMessage?: string
}>()
const settings = defineModel<CloudSyncSettings>('settings', { required: true })
const password = defineModel<string>('password', { required: true })
const syncPassword = defineModel<string>('syncPassword', { required: true })
const accessKeyId = defineModel<string>('accessKeyId', { required: true })
const secretAccessKey = defineModel<string>('secretAccessKey', { required: true })
const sessionToken = defineModel<string>('sessionToken', { required: true })
const emit = defineEmits<{ test: []; sync: []; rewrite: []; removeDevice: [deviceId: string] }>()
const rewriteConfirmationVisible = ref(false)
const pendingDeviceRemoval = ref<string | null>(null)
const pendingDevice = computed(() => status.devices.find((device) => device.device_id === pendingDeviceRemoval.value))
const profileMatchesActive = computed(() => {
  if (!activeProfile) return true
  return cloudSyncProfilesEqual(settings.value, activeProfile)
})
const canMutate = computed(() => !busy && profileMatchesActive.value)
const activeProfileLabel = computed(() => {
  const profile = activeProfile || settings.value
  if (profile.backend === 'webdav') {
    const root = profile.root_path.trim().replace(/^\/+|\/+$/g, '')
    return `WebDAV · ${profile.base_url.replace(/\/$/, '')}${root ? `/${root}` : ''}`
  }
  const prefix = profile.s3.prefix.trim().replace(/^\/+|\/+$/g, '')
  return `S3 · ${profile.s3.bucket}${prefix ? `/${prefix}` : ''}`
})
const canTest = computed(() => {
  if (settings.value.encryption_enabled && !syncPassword.value) return false
  if (settings.value.backend === 'webdav') {
    return !!settings.value.base_url.trim() && !!settings.value.username.trim() && !!password.value
  }
  return !!settings.value.s3.region.trim()
    && !!settings.value.s3.bucket.trim()
    && !!accessKeyId.value
    && !!secretAccessKey.value
})

function requestRewrite() {
  rewriteConfirmationVisible.value = true
}

function cancelRewrite() {
  rewriteConfirmationVisible.value = false
}

function confirmRewrite() {
  rewriteConfirmationVisible.value = false
  if (canMutate.value) emit('rewrite')
}

function requestSync() {
  if (canMutate.value) emit('sync')
}

function requestDeviceRemoval(deviceId: string) {
  pendingDeviceRemoval.value = deviceId
}

function cancelDeviceRemoval() {
  pendingDeviceRemoval.value = null
}

function confirmDeviceRemoval() {
  const deviceId = pendingDeviceRemoval.value
  pendingDeviceRemoval.value = null
  if (deviceId && canMutate.value) emit('removeDevice', deviceId)
}
</script>
<template>
  <section class="setting-group cloud-sync-settings">
    <div class="setting-heading"><Cloud :size="18" /><div><h3>云同步</h3><p>凭据仅保存在系统凭据存储中。</p></div></div>
    <div class="setting-row"><span>启用自动同步</span><label class="switch"><input v-model="settings.enabled" type="checkbox" /><span /></label></div>
    <template v-if="settings.enabled">
      <div class="cloud-backend-selector" role="radiogroup" aria-label="同步后端">
        <button type="button" role="radio" :aria-checked="settings.backend === 'webdav'" :class="{ active: settings.backend === 'webdav' }" @click="settings.backend = 'webdav'">WebDAV</button>
        <button type="button" role="radio" :aria-checked="settings.backend === 's3'" :class="{ active: settings.backend === 's3' }" @click="settings.backend = 's3'">S3</button>
      </div>
      <template v-if="settings.backend === 'webdav'">
        <label><span>WebDAV 地址</span><input v-model="settings.base_url" type="url" autocomplete="url" /></label>
        <label><span>远端目录</span><input v-model="settings.root_path" /></label>
        <label><span>用户名</span><input v-model="settings.username" autocomplete="username" /></label>
        <label><span>WebDAV 密码</span><input v-model="password" type="password" autocomplete="current-password" /></label>
      </template>
      <template v-else>
        <label><span>Endpoint（AWS 可留空）</span><input v-model="settings.s3.endpoint_url" type="url" autocomplete="url" /></label>
        <label><span>Region</span><input v-model="settings.s3.region" /></label>
        <label><span>Bucket</span><input v-model="settings.s3.bucket" /></label>
        <label><span>对象前缀</span><input v-model="settings.s3.prefix" /></label>
        <label><span>Access Key ID</span><input v-model="accessKeyId" autocomplete="username" /></label>
        <label><span>Secret Access Key</span><input v-model="secretAccessKey" type="password" autocomplete="current-password" /></label>
        <label><span>Session Token（可选）</span><input v-model="sessionToken" type="password" autocomplete="off" /></label>
        <div class="setting-row"><span>使用 Path-style 寻址</span><label class="switch"><input v-model="settings.s3.force_path_style" type="checkbox" /><span /></label></div>
      </template>
      <div class="setting-row"><span>启用包加密</span><label class="switch"><input v-model="settings.encryption_enabled" type="checkbox" /><span /></label></div>
      <label v-if="settings.encryption_enabled"><span>同步密码</span><input v-model="syncPassword" type="password" autocomplete="new-password" /></label>
      <p class="path-value"><Wifi :size="14" /> 状态：{{ status.state }} · 待上传 {{ status.pending_mutations }}</p>
      <p class="path-value">设备记录属于活动后端：{{ activeProfileLabel }}<template v-if="!profileMatchesActive">（当前草稿未保存，操作已暂停）</template></p>
      <p class="path-value" data-connection-test-state>
        连接测试：{{ connectionTestState === 'idle' ? '未测试' : connectionTestState === 'pending' ? '测试中' : connectionTestState === 'passed' ? '已通过' : '失败' }}<template v-if="connectionTestMessage"> · {{ connectionTestMessage }}</template>
      </p>
      <div class="cloud-device-list" aria-label="远端设备">
        <template v-if="status.devices.length">
          <div v-for="device in status.devices" :key="device.device_id" class="cloud-device-row">
            <div><strong>{{ device.display_name }}</strong><span>{{ device.last_seen_at || '尚未在线' }}</span></div>
            <button
              class="icon-button"
              type="button"
              :aria-label="`删除设备记录：${device.display_name}`"
              :disabled="!canMutate"
              @click="requestDeviceRemoval(device.device_id)"
            ><Trash2 :size="15" /></button>
          </div>
        </template>
        <p v-else class="path-value">暂无远端设备记录</p>
      </div>
      <div class="setting-actions">
        <button class="secondary-button compact" type="button" :disabled="busy || connectionTestState === 'pending' || !canTest" @click="emit('test')"><ShieldCheck :size="14" />测试连接</button>
        <button class="secondary-button compact" type="button" :disabled="!canMutate" @click="requestSync"><RefreshCw :size="14" />立即同步</button>
        <button class="secondary-button compact" type="button" :disabled="!canMutate" @click="requestRewrite">重写云端存档</button>
      </div>
      <p v-if="status.last_error_message" class="path-value">{{ status.last_error_message }}</p>
      <div v-if="rewriteConfirmationVisible" class="cloud-confirmation" role="dialog" aria-modal="true" aria-label="确认重写云端存档">
        <strong>确认重写云端存档？</strong>
        <p>旧 generation 保留，直到新 generation 可用。</p>
        <div class="setting-actions">
          <button class="secondary-button compact" type="button" @click="cancelRewrite">取消重写</button>
          <button class="primary-button compact" type="button" @click="confirmRewrite">确认重写</button>
        </div>
      </div>
      <div v-if="pendingDevice" class="cloud-confirmation" role="dialog" aria-modal="true" aria-label="确认删除设备记录">
        <strong>确认删除设备记录？</strong>
        <p>{{ pendingDevice.display_name }}</p>
        <div class="setting-actions">
          <button class="secondary-button compact" type="button" @click="cancelDeviceRemoval">取消删除</button>
          <button class="primary-button compact" type="button" @click="confirmDeviceRemoval">确认删除</button>
        </div>
      </div>
    </template>
  </section>
</template>
