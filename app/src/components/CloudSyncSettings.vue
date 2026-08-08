<script setup lang="ts">
import { computed, ref } from 'vue'
import { Cloud, RefreshCw, ShieldCheck, Trash2, Wifi } from 'lucide-vue-next'
import type { CloudSyncSettings, CloudSyncState, CloudSyncStatus } from '../desktop-api'
import { cloudSyncProfilesEqual } from '../cloud-sync-profile'
import { translate as t } from '../i18n'

type ConnectionTestState = 'idle' | 'pending' | 'passed' | 'failed'

const stateKeys: Record<CloudSyncState, string> = {
  disabled: 'cloudSync.state.disabled',
  idle: 'cloudSync.state.idle',
  syncing: 'cloudSync.state.syncing',
  offline: 'cloudSync.state.offline',
  needs_unlock: 'cloudSync.state.needsUnlock',
  auth_error: 'cloudSync.state.authError',
  protocol_error: 'cloudSync.state.protocolError',
}

const connectionStateKeys: Record<ConnectionTestState, string> = {
  idle: 'cloudSync.connectionState.idle',
  pending: 'cloudSync.connectionState.pending',
  passed: 'cloudSync.connectionState.passed',
  failed: 'cloudSync.connectionState.failed',
}

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
const statusLabel = computed(() => t(stateKeys[status.state]))
const connectionStateLabel = computed(() => t(connectionStateKeys[connectionTestState]))
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
    <div class="setting-heading"><Cloud :size="18" /><div><h3>{{ t('cloudSync.title') }}</h3><p>{{ t('cloudSync.credentialsStored') }}</p></div></div>
    <div class="setting-row"><span>{{ t('cloudSync.enableAutomatic') }}</span><label class="switch"><input v-model="settings.enabled" type="checkbox" /><span /></label></div>
    <template v-if="settings.enabled">
      <div class="cloud-backend-selector" role="radiogroup" :aria-label="t('cloudSync.backendAria')">
        <button type="button" role="radio" :aria-checked="settings.backend === 'webdav'" :class="{ active: settings.backend === 'webdav' }" @click="settings.backend = 'webdav'">WebDAV</button>
        <button type="button" role="radio" :aria-checked="settings.backend === 's3'" :class="{ active: settings.backend === 's3' }" @click="settings.backend = 's3'">S3</button>
      </div>
      <template v-if="settings.backend === 'webdav'">
        <label><span>{{ t('cloudSync.webdavUrl') }}</span><input v-model="settings.base_url" type="url" autocomplete="url" /></label>
        <label><span>{{ t('cloudSync.remoteDirectory') }}</span><input v-model="settings.root_path" /></label>
        <label><span>{{ t('cloudSync.username') }}</span><input v-model="settings.username" autocomplete="username" /></label>
        <label><span>{{ t('cloudSync.webdavPassword') }}</span><input v-model="password" type="password" autocomplete="current-password" /></label>
      </template>
      <template v-else>
        <label><span>{{ t('cloudSync.endpointOptional') }}</span><input v-model="settings.s3.endpoint_url" type="url" autocomplete="url" /></label>
        <label><span>{{ t('cloudSync.region') }}</span><input v-model="settings.s3.region" /></label>
        <label><span>{{ t('cloudSync.bucket') }}</span><input v-model="settings.s3.bucket" /></label>
        <label><span>{{ t('cloudSync.prefix') }}</span><input v-model="settings.s3.prefix" /></label>
        <label><span>{{ t('cloudSync.accessKeyId') }}</span><input v-model="accessKeyId" autocomplete="username" /></label>
        <label><span>{{ t('cloudSync.secretAccessKey') }}</span><input v-model="secretAccessKey" type="password" autocomplete="current-password" /></label>
        <label><span>{{ t('cloudSync.sessionTokenOptional') }}</span><input v-model="sessionToken" type="password" autocomplete="off" /></label>
        <div class="setting-row"><span>{{ t('cloudSync.forcePathStyle') }}</span><label class="switch"><input v-model="settings.s3.force_path_style" type="checkbox" /><span /></label></div>
      </template>
      <div class="setting-row"><span>{{ t('cloudSync.enableEncryption') }}</span><label class="switch"><input v-model="settings.encryption_enabled" type="checkbox" /><span /></label></div>
      <label v-if="settings.encryption_enabled"><span>{{ t('cloudSync.syncPassword') }}</span><input v-model="syncPassword" type="password" autocomplete="new-password" /></label>
      <p class="path-value"><Wifi :size="14" /> {{ t('cloudSync.statusLine', { state: statusLabel, count: status.pending_mutations }) }}</p>
      <p class="path-value">{{ t('cloudSync.activeProfile', { profile: activeProfileLabel }) }}<template v-if="!profileMatchesActive">{{ t('cloudSync.draftPaused') }}</template></p>
      <p class="path-value" data-connection-test-state>
        {{ t('cloudSync.connectionTest', { state: connectionStateLabel }) }}<template v-if="connectionTestMessage"> · {{ connectionTestMessage }}</template>
      </p>
      <div class="cloud-device-list" :aria-label="t('cloudSync.remoteDevicesAria')">
        <template v-if="status.devices.length">
          <div v-for="device in status.devices" :key="device.device_id" class="cloud-device-row">
            <div><strong>{{ device.display_name }}</strong><span>{{ device.last_seen_at || t('cloudSync.neverOnline') }}</span></div>
            <button class="icon-button" type="button" :aria-label="t('cloudSync.removeDeviceAria', { name: device.display_name })" :disabled="!canMutate" @click="requestDeviceRemoval(device.device_id)"><Trash2 :size="15" /></button>
          </div>
        </template>
        <p v-else class="path-value">{{ t('cloudSync.noDevices') }}</p>
      </div>
      <div class="setting-actions">
        <button class="secondary-button compact" type="button" :disabled="busy || connectionTestState === 'pending' || !canTest" @click="emit('test')"><ShieldCheck :size="14" />{{ t('cloudSync.testConnection') }}</button>
        <button class="secondary-button compact" type="button" :disabled="!canMutate" @click="requestSync"><RefreshCw :size="14" />{{ t('cloudSync.syncNow') }}</button>
        <button class="secondary-button compact" type="button" :disabled="!canMutate" @click="requestRewrite">{{ t('cloudSync.rewriteArchive') }}</button>
      </div>
      <p v-if="status.last_error_message" class="path-value">{{ status.last_error_message }}</p>
      <div v-if="rewriteConfirmationVisible" class="cloud-confirmation" role="dialog" aria-modal="true" :aria-label="t('cloudSync.rewriteDialogAria')">
        <strong>{{ t('cloudSync.rewriteTitle') }}</strong>
        <p>{{ t('cloudSync.rewriteDescription') }}</p>
        <div class="setting-actions">
          <button class="secondary-button compact" type="button" @click="cancelRewrite">{{ t('cloudSync.cancelRewrite') }}</button>
          <button class="primary-button compact" type="button" @click="confirmRewrite">{{ t('cloudSync.confirmRewrite') }}</button>
        </div>
      </div>
      <div v-if="pendingDevice" class="cloud-confirmation" role="dialog" aria-modal="true" :aria-label="t('cloudSync.removeDialogAria')">
        <strong>{{ t('cloudSync.removeTitle') }}</strong>
        <p>{{ pendingDevice.display_name }}</p>
        <div class="setting-actions">
          <button class="secondary-button compact" type="button" @click="cancelDeviceRemoval">{{ t('cloudSync.cancelRemove') }}</button>
          <button class="primary-button compact" type="button" @click="confirmDeviceRemoval">{{ t('cloudSync.confirmRemove') }}</button>
        </div>
      </div>
    </template>
  </section>
</template>
