/** @vitest-environment happy-dom */

import { createApp, defineComponent, h, nextTick, reactive, ref } from 'vue'
import { describe, expect, it, vi } from 'vitest'
import CloudSyncSettings from './components/CloudSyncSettings.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import type { CloudSyncSettings as CloudSettings, SettingsModel } from './desktop-api'

function cloudSettings(): CloudSettings {
  return {
    backend: 'webdav',
    enabled: true,
    connection_verified: false,
    base_url: 'https://dav.example.test',
    root_path: 'archive',
    username: 'alice',
    encryption_enabled: false,
    s3: {
      endpoint_url: '',
      region: 'us-east-1',
      bucket: '',
      prefix: '',
      force_path_style: false,
    },
    remote_id: 'default',
    vault_id: 'default',
    generation_id: 'generation-1',
  }
}

function settingsModel(): SettingsModel {
  return {
    setup_complete: true,
    secret_enabled: false,
    allowed_origins: [],
    close_behavior: 'ask',
    tray_click_behavior: 'show_menu',
    theme: 'system',
    semantic_search: {
      enabled: true,
      default_mode: 'hybrid',
      backend: 'local',
      local: { model: 'test', device: 'auto', dtype: 'auto' },
      ollama: { base_url: 'http://127.0.0.1:11434', model: 'test' },
      llama_cpp: { base_url: 'http://127.0.0.1:8080/v1', model: 'test' },
      openai_compatible: { base_url: 'https://example.test/v1', model: 'test' },
    },
    mcp_enabled: true,
    cloud_sync: cloudSettings(),
  }
}

function button(root: ParentNode, text: string) {
  const found = [...root.querySelectorAll<HTMLButtonElement>('button')]
    .find((candidate) => candidate.textContent?.trim() === text)
  if (!found) throw new Error(`missing button ${text}`)
  return found
}

function inputForLabel(root: ParentNode, text: string) {
  const label = [...root.querySelectorAll<HTMLLabelElement>('label')]
    .find((candidate) => candidate.textContent?.includes(text))
  const input = label?.querySelector<HTMLInputElement>('input')
  if (!input) throw new Error(`missing input ${text}`)
  return input
}

function checkboxForRow(root: ParentNode, text: string) {
  const row = [...root.querySelectorAll<HTMLElement>('.setting-row')]
    .find((candidate) => candidate.textContent?.includes(text))
  const input = row?.querySelector<HTMLInputElement>('input[type="checkbox"]')
  if (!input) throw new Error(`missing checkbox ${text}`)
  return input
}

function cloneCloudSettings(settings: CloudSettings): CloudSettings {
  return {
    ...settings,
    s3: { ...settings.s3 },
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((promiseResolve, promiseReject) => {
    resolve = promiseResolve
    reject = promiseReject
  })
  return { promise, resolve, reject }
}

describe('cloud sync settings', () => {
  it('switches backend fields and validates S3 before testing', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(cloudSettings())
    const test = vi.fn()
    const sync = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(CloudSyncSettings, {
        settings,
        password: 'dav-password',
        syncPassword: '',
        accessKeyId: '',
        secretAccessKey: '',
        sessionToken: '',
        status: { state: 'idle', pending_mutations: 0, devices: [] },
        onTest: test,
        onSync: sync,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      expect(document.body.textContent).toContain('WebDAV 地址')
      expect(document.body.textContent).not.toContain('Access Key ID')

      button(document.body, 'S3').click()
      await nextTick()
      expect(settings.backend).toBe('s3')
      expect(document.body.textContent).toContain('Access Key ID')
      expect(document.body.textContent).not.toContain('WebDAV 地址')
      expect(document.body.querySelector('.cloud-sync-settings label label')).toBeNull()
      expect(button(document.body, '测试连接').disabled).toBe(true)
      expect(button(document.body, '立即同步').disabled).toBe(false)
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('clears every cloud credential whenever the settings dialog closes', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const visible = ref(true)
    const settings = reactive(settingsModel())
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: visible.value,
        secretCopied: false,
        settings,
        originText: '',
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      const password = document.body.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')!
      password.value = 'sensitive'
      password.dispatchEvent(new Event('input'))
      visible.value = false
      await nextTick()
      visible.value = true
      await nextTick()
      button(document.body, '云同步').click()
      await nextTick()
      expect(document.body.querySelector<HTMLInputElement>('input[autocomplete="current-password"]')?.value).toBe('')
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('clears WebDAV, encryption, and every S3 credential on close', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const visible = ref(true)
    const settings = reactive(settingsModel())
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: visible.value,
        secretCopied: false,
        settings,
        originText: '',
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      const webdavPassword = inputForLabel(document.body, 'WebDAV 密码')
      webdavPassword.value = 'webdav-secret'
      webdavPassword.dispatchEvent(new Event('input'))
      checkboxForRow(document.body, '启用包加密').click()
      await nextTick()
      const encryptionPassword = inputForLabel(document.body, '同步密码')
      encryptionPassword.value = 'encryption-secret'
      encryptionPassword.dispatchEvent(new Event('input'))

      button(document.body, 'S3').click()
      await nextTick()
      for (const [label, value] of [
        ['Access Key ID', 'access-key'],
        ['Secret Access Key', 'secret-key'],
        ['Session Token', 'session-token'],
      ] as const) {
        const input = inputForLabel(document.body, label)
        input.value = value
        input.dispatchEvent(new Event('input'))
      }

      visible.value = false
      await nextTick()
      visible.value = true
      await nextTick()
      button(document.body, '云同步').click()
      await nextTick()
      expect(inputForLabel(document.body, 'Access Key ID').value).toBe('')
      expect(inputForLabel(document.body, 'Secret Access Key').value).toBe('')
      expect(inputForLabel(document.body, 'Session Token').value).toBe('')
      expect(inputForLabel(document.body, '同步密码').value).toBe('')
      button(document.body, 'WebDAV').click()
      await nextTick()
      expect(inputForLabel(document.body, 'WebDAV 密码').value).toBe('')
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('requires the sync password before testing an encrypted S3 connection', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(cloudSettings())
    settings.backend = 's3'
    settings.encryption_enabled = true
    settings.s3.bucket = 'archive'
    const syncPassword = ref('')
    const test = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(CloudSyncSettings, {
        settings,
        password: '',
        syncPassword: syncPassword.value,
        accessKeyId: 'AKID',
        secretAccessKey: 'secret-key',
        sessionToken: '',
        status: { state: 'idle', pending_mutations: 0, devices: [] },
        'onUpdate:syncPassword': (value: string) => { syncPassword.value = value },
        onTest: test,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      const testButton = button(document.body, '测试连接')
      expect(testButton.disabled).toBe(true)
      const passwordInput = [...document.body.querySelectorAll<HTMLLabelElement>('label')]
        .find((label) => label.textContent?.includes('同步密码'))
        ?.querySelector<HTMLInputElement>('input')
      if (!passwordInput) throw new Error('missing sync password input')
      passwordInput.value = 'shared password'
      passwordInput.dispatchEvent(new Event('input'))
      await nextTick()

      expect(testButton.disabled).toBe(false)
      testButton.click()
      expect(test).toHaveBeenCalledOnce()
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('emits unsaved S3 settings and the complete credential payload from the dialog', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(settingsModel())
    const test = vi.fn((cloudSync: CloudSettings, _credentials: unknown, _requestId: number) => Promise.resolve({
      ok: true,
      message: 'ok',
      supports_conditional_write: true,
      cloud_sync: { ...cloudSync, connection_verified: true },
    }))
    const save = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: true,
        secretCopied: false,
        settings,
        originText: '',
        onCloudSyncTest: test,
        onSave: save,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      button(document.body, 'S3').click()
      await nextTick()

      const endpoint = inputForLabel(document.body, 'Endpoint')
      endpoint.value = 'https://objects.example.test'
      endpoint.dispatchEvent(new Event('input'))
      const region = inputForLabel(document.body, 'Region')
      region.value = 'eu-west-1'
      region.dispatchEvent(new Event('input'))
      const bucket = inputForLabel(document.body, 'Bucket')
      bucket.value = 'chat-archive'
      bucket.dispatchEvent(new Event('input'))
      const prefix = inputForLabel(document.body, '对象前缀')
      prefix.value = 'production/2026'
      prefix.dispatchEvent(new Event('input'))
      const accessKeyId = inputForLabel(document.body, 'Access Key ID')
      accessKeyId.value = 'AKIA_TEST_ID'
      accessKeyId.dispatchEvent(new Event('input'))
      const secretAccessKey = inputForLabel(document.body, 'Secret Access Key')
      secretAccessKey.value = 'secret-test-value'
      secretAccessKey.dispatchEvent(new Event('input'))
      const sessionToken = inputForLabel(document.body, 'Session Token')
      sessionToken.value = 'session-test-value'
      sessionToken.dispatchEvent(new Event('input'))
      checkboxForRow(document.body, '使用 Path-style 寻址').click()
      checkboxForRow(document.body, '启用包加密').click()
      await nextTick()

      const syncPassword = inputForLabel(document.body, '同步密码')
      syncPassword.value = 'sync-test-password'
      syncPassword.dispatchEvent(new Event('input'))
      await nextTick()

      const testButton = button(document.body, '测试连接')
      expect(testButton.disabled).toBe(false)
      testButton.click()
      await Promise.resolve()
      await nextTick()

      expect(save).not.toHaveBeenCalled()
      expect(settings.cloud_sync).toMatchObject({
        backend: 's3',
        encryption_enabled: true,
        s3: {
          endpoint_url: 'https://objects.example.test',
          region: 'eu-west-1',
          bucket: 'chat-archive',
          prefix: 'production/2026',
          force_path_style: true,
        },
      })
      expect(test).toHaveBeenCalledOnce()
      expect(test.mock.calls[0]?.[0]).toMatchObject({
        backend: 's3',
        encryption_enabled: true,
        s3: settings.cloud_sync.s3,
      })
      expect(test.mock.calls[0]?.[1]).toEqual({
        backend: 's3',
        access_key_id: 'AKIA_TEST_ID',
        secret_access_key: 'secret-test-value',
        session_token: 'session-test-value',
        sync_password: 'sync-test-password',
      })

      button(document.body, '保存设置').click()
      expect(save).toHaveBeenCalledOnce()
      expect(save).toHaveBeenCalledWith({
        backend: 's3',
        access_key_id: 'AKIA_TEST_ID',
        secret_access_key: 'secret-test-value',
        session_token: 'session-test-value',
        sync_password: 'sync-test-password',
      })
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('preserves stored credentials when saving an existing connection without retesting', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(settingsModel())
    settings.cloud_sync.connection_verified = true
    const save = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: true,
        secretCopied: false,
        settings,
        originText: '',
        onSave: save,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '保存设置').click()
      expect(save).toHaveBeenCalledWith(null)
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('requires a second confirmation before rewriting the cloud generation', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(cloudSettings())
    const rewrite = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(CloudSyncSettings, {
        settings,
        password: 'dav-password',
        syncPassword: '',
        accessKeyId: '',
        secretAccessKey: '',
        sessionToken: '',
        status: { state: 'idle', pending_mutations: 0, devices: [] },
        onRewrite: rewrite,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '重写云端存档').click()
      await nextTick()
      expect(rewrite).not.toHaveBeenCalled()
      expect(document.body.querySelector('[role="dialog"][aria-label="确认重写云端存档"]')).not.toBeNull()

      button(document.body, '取消重写').click()
      await nextTick()
      expect(rewrite).not.toHaveBeenCalled()
      expect(document.body.querySelector('[role="dialog"][aria-label="确认重写云端存档"]')).toBeNull()

      button(document.body, '重写云端存档').click()
      await nextTick()
      button(document.body, '确认重写').click()
      expect(rewrite).toHaveBeenCalledOnce()
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('requires a second confirmation before deleting each remote device record', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(cloudSettings())
    const removeDevice = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(CloudSyncSettings, {
        settings,
        password: 'dav-password',
        syncPassword: '',
        accessKeyId: '',
        secretAccessKey: '',
        sessionToken: '',
        status: {
          state: 'idle',
          pending_mutations: 0,
          devices: [
            { device_id: 'device-a', display_name: '笔记本', last_seen_at: '2026-08-01T12:00:00Z' },
            { device_id: 'device-b', display_name: '台式机', last_seen_at: null },
          ],
        },
        onRemoveDevice: removeDevice,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      expect(document.body.textContent).toContain('笔记本')
      expect(document.body.textContent).toContain('台式机')

      const deleteButton = [...document.body.querySelectorAll<HTMLButtonElement>('button')]
        .find((candidate) => candidate.getAttribute('aria-label') === '删除设备记录：笔记本')
      if (!deleteButton) throw new Error('missing device delete button')
      deleteButton.click()
      await nextTick()
      expect(removeDevice).not.toHaveBeenCalled()
      expect(document.body.querySelector('[role="dialog"][aria-label="确认删除设备记录"]')).not.toBeNull()

      button(document.body, '取消删除').click()
      expect(removeDevice).not.toHaveBeenCalled()
      deleteButton.click()
      await nextTick()
      button(document.body, '确认删除').click()
      expect(removeDevice).toHaveBeenCalledWith('device-a')
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('blocks mutation actions for an unsaved profile and labels devices as belonging to the active profile', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const activeProfile = cloudSettings()
    const settings = reactive(cloneCloudSettings(activeProfile))
    settings.backend = 's3'
    settings.s3.region = 'eu-west-1'
    settings.s3.bucket = 'draft-archive'
    const sync = vi.fn()
    const rewrite = vi.fn()
    const removeDevice = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(CloudSyncSettings, {
        settings,
        activeProfile,
        password: 'dav-password',
        syncPassword: '',
        accessKeyId: 'AKID',
        secretAccessKey: 'secret-key',
        sessionToken: '',
        status: {
          state: 'idle',
          pending_mutations: 0,
          devices: [{ device_id: 'device-a', display_name: '笔记本', last_seen_at: null }],
        },
        onSync: sync,
        onRewrite: rewrite,
        onRemoveDevice: removeDevice,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      await nextTick()
      expect(document.body.textContent).toContain('设备记录属于活动后端')
      expect(document.body.textContent).toContain('WebDAV · https://dav.example.test/archive')
      expect(button(document.body, '立即同步').disabled).toBe(true)
      expect(button(document.body, '重写云端存档').disabled).toBe(true)

      const deleteButton = document.body.querySelector<HTMLButtonElement>('[aria-label="删除设备记录：笔记本"]')
      expect(deleteButton?.disabled).toBe(true)
      button(document.body, '立即同步').click()
      button(document.body, '重写云端存档').click()
      deleteButton?.click()
      expect(sync).not.toHaveBeenCalled()
      expect(rewrite).not.toHaveBeenCalled()
      expect(removeDevice).not.toHaveBeenCalled()

      Object.assign(settings, cloneCloudSettings(activeProfile))
      await nextTick()
      expect(button(document.body, '立即同步').disabled).toBe(false)
      expect(button(document.body, '重写云端存档').disabled).toBe(false)
      expect(deleteButton?.disabled).toBe(false)
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('tracks asynchronous connection tests and invalidates a passed result when credentials change', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(settingsModel())
    settings.cloud_sync.connection_verified = false
    const testResult = deferred<{ ok: boolean; message: string; supports_conditional_write: boolean; cloud_sync: CloudSettings }>()
    const test = vi.fn(() => testResult.promise)
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: true,
        secretCopied: false,
        settings,
        originText: '',
        onCloudSyncTest: test,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      const password = inputForLabel(document.body, 'WebDAV 密码')
      password.value = 'dav-password'
      password.dispatchEvent(new Event('input'))
      await nextTick()

      const testButton = button(document.body, '测试连接')
      testButton.click()
      await nextTick()
      expect(test).toHaveBeenCalledOnce()
      expect(document.body.textContent).toContain('连接测试：测试中')
      expect(button(document.body, '保存设置').disabled).toBe(true)
      testButton.click()
      expect(test).toHaveBeenCalledOnce()

      testResult.resolve({ ok: true, message: 'ok', supports_conditional_write: true, cloud_sync: { ...settings.cloud_sync, connection_verified: true } })
      await nextTick()
      await Promise.resolve()
      await nextTick()
      expect(document.body.textContent).toContain('连接测试：已通过')
      expect(button(document.body, '保存设置').disabled).toBe(false)

      password.value = 'changed-password'
      password.dispatchEvent(new Event('input'))
      await nextTick()
      expect(document.body.textContent).toContain('连接测试：未测试')
      expect(button(document.body, '保存设置').disabled).toBe(true)
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('keeps a successful connection test passed when the backend normalizes S3 settings', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(settingsModel())
    settings.cloud_sync.backend = 's3'
    settings.cloud_sync.connection_verified = false
    settings.cloud_sync.s3.endpoint_url = 'https://s3.example.test/'
    settings.cloud_sync.s3.bucket = 'archive'
    settings.cloud_sync.s3.prefix = '/backups/'
    const test = vi.fn(async (draft: CloudSettings) => ({
      ok: true,
      message: 'ok',
      supports_conditional_write: true,
      cloud_sync: {
        ...cloneCloudSettings(draft),
        connection_verified: true,
        s3: {
          ...draft.s3,
          endpoint_url: 'https://s3.example.test',
          prefix: 'backups',
        },
      },
    }))
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: true,
        secretCopied: false,
        settings,
        originText: '',
        onCloudSyncTest: test,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      for (const [label, value] of [
        ['Access Key ID', 'AKID'],
        ['Secret Access Key', 'secret-key'],
      ] as const) {
        const input = inputForLabel(document.body, label)
        input.value = value
        input.dispatchEvent(new Event('input'))
      }
      await nextTick()

      button(document.body, '测试连接').click()
      await Promise.resolve()
      await nextTick()
      await nextTick()

      expect(settings.cloud_sync.s3.endpoint_url).toBe('https://s3.example.test')
      expect(settings.cloud_sync.s3.prefix).toBe('backups')
      expect(document.body.textContent).toContain('连接测试：已通过')
      expect(button(document.body, '保存设置').disabled).toBe(false)
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('does not let a late connection result overwrite a newer draft request and rejects failed saves', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(settingsModel())
    settings.cloud_sync.connection_verified = false
    const first = deferred<{ ok: boolean; message: string; supports_conditional_write: boolean; cloud_sync: CloudSettings }>()
    const second = deferred<{ ok: boolean; message: string; supports_conditional_write: boolean; cloud_sync: CloudSettings }>()
    const test = vi.fn()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise)
    const save = vi.fn()
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: true,
        secretCopied: false,
        settings,
        originText: '',
        onCloudSyncTest: test,
        onSave: save,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      const password = inputForLabel(document.body, 'WebDAV 密码')
      password.value = 'first-password'
      password.dispatchEvent(new Event('input'))
      await nextTick()
      const testButton = button(document.body, '测试连接')
      testButton.click()
      await nextTick()
      const firstRequestId = test.mock.calls[0]?.[2]

      password.value = 'second-password'
      password.dispatchEvent(new Event('input'))
      await nextTick()
      expect(document.body.textContent).toContain('连接测试：未测试')
      testButton.click()
      await nextTick()
      const secondRequestId = test.mock.calls[1]?.[2]
      expect(secondRequestId).toBeGreaterThan(firstRequestId)

      first.resolve({ ok: true, message: 'stale', supports_conditional_write: true, cloud_sync: { ...settings.cloud_sync, connection_verified: true } })
      await Promise.resolve()
      await nextTick()
      expect(document.body.textContent).toContain('连接测试：测试中')
      expect(document.body.textContent).not.toContain('连接测试：已通过')
      expect(save).not.toHaveBeenCalled()

      second.resolve({ ok: false, message: '认证失败', supports_conditional_write: false, cloud_sync: settings.cloud_sync })
      await Promise.resolve()
      await nextTick()
      expect(document.body.textContent).toContain('连接测试：失败 · 认证失败')
      expect(button(document.body, '保存设置').disabled).toBe(true)
      button(document.body, '保存设置').click()
      expect(save).not.toHaveBeenCalled()
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

  it('ignores abandoned hidden backend fields and credentials after returning to the active backend', async () => {
    document.body.innerHTML = '<div id="app"></div>'
    const settings = reactive(settingsModel())
    settings.cloud_sync.connection_verified = true
    const activeProfile = cloneCloudSettings(settings.cloud_sync)
    const Root = defineComponent({
      setup: () => () => h(SettingsDialog, {
        visible: true,
        secretCopied: false,
        settings,
        originText: '',
        activeCloudSyncProfile: activeProfile,
      }),
    })
    const app = createApp(Root)
    app.mount('#app')
    try {
      button(document.body, '云同步').click()
      await nextTick()
      expect(button(document.body, '保存设置').disabled).toBe(false)
      expect(button(document.body, '立即同步').disabled).toBe(false)

      button(document.body, 'S3').click()
      await nextTick()
      for (const [label, value] of [
        ['Bucket', 'draft-bucket'],
        ['对象前缀', 'draft-prefix'],
        ['Access Key ID', 'draft-access-key'],
        ['Secret Access Key', 'draft-secret-key'],
      ] as const) {
        const input = inputForLabel(document.body, label)
        input.value = value
        input.dispatchEvent(new Event('input'))
      }
      await nextTick()

      button(document.body, 'WebDAV').click()
      await nextTick()

      expect(button(document.body, '保存设置').disabled).toBe(false)
      expect(button(document.body, '立即同步').disabled).toBe(false)
    } finally {
      app.unmount()
      document.body.innerHTML = ''
    }
  })

})
