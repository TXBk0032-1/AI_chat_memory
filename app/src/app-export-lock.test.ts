/** @vitest-environment happy-dom */

import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp } from 'vue'
import type { Message, SessionOpen } from './conversation'
import type { SettingsModel } from './desktop-api'
import appSource from './App.vue?raw'
import App from './App.vue'
import { i18n } from './i18n'

describe('App export context locking', () => {
  it('keeps the selected session and branch stable while an export is running', () => {
    expect(appSource).toMatch(/if \(exportBusy\.value\) return\s+if \(!detail\.shouldOpen\(id\)\) return/)
    expect(appSource).toMatch(/async function selectBranch\(branch: BranchNode\) \{\s+if \(exportSelecting\.value \|\| exportBusy\.value\) return/)
  })

  it('does not clear the selected session during an export-driven catalog refresh', () => {
    expect(appSource).toMatch(/if \(exportBusy\.value\) return\s+if \(selected\.value && !visibleIds\.has\(selected\.value\.id\)\)/)
  })

  it('locks the export context before opening the native save dialog', () => {
    const lock = appSource.indexOf('exportBusy.value = true', appSource.indexOf('async function exportSelectedConversation'))
    const saveDialog = appSource.indexOf('const path = await save', appSource.indexOf('async function exportSelectedConversation'))

    expect(lock).toBeGreaterThan(-1)
    expect(lock).toBeLessThan(saveDialog)
  })
})

// ---------------------------------------------------------------------------
// Ready barrier: every export entry — the preview preflight, PNG/JPEG capture
// and PDF printing — must await the ExportDocument's `whenReady()` promise
// (backed by the chunked markdown renderer) before touching the document,
// otherwise Mermaid rendering and capture run against a document that is
// still streaming in frame by frame. ExportDocument is stubbed with the
// current defineExpose shape (getElement + whenReady) where `whenReady()`
// returns a promise the test controls, so each test can hold the document
// "still rendering" and prove nothing runs ahead of the barrier.
// ---------------------------------------------------------------------------

const barrier = vi.hoisted(() => {
  let ready: Promise<void> = Promise.resolve()
  return {
    api: {
      searchSessions: vi.fn(),
      openSession: vi.fn(),
      getSessionMessages: vi.fn(),
      searchSessionHits: vi.fn(),
      getSessionBranches: vi.fn(),
      deleteSession: vi.fn(),
      importHistory: vi.fn(),
      getSettings: vi.fn(),
      saveSettings: vi.fn(),
      rotateSecret: vi.fn(),
      getApiStatus: vi.fn(),
      getSemanticStatus: vi.fn(),
      checkEmbeddingBackend: vi.fn(),
      reindexSemanticSearch: vi.fn(),
      downloadLocalEmbeddingModel: vi.fn(),
      importLocalEmbeddingModel: vi.fn(),
      cancelSemanticWork: vi.fn(),
      moveDataDirectory: vi.fn(),
      confirmCloseBehavior: vi.fn(),
      writeExportFile: vi.fn(),
      printToPdf: vi.fn(),
      getCloudSyncStatus: vi.fn(),
      testCloudSyncConnection: vi.fn(),
      syncNow: vi.fn(),
      rewriteCloudArchive: vi.fn(),
      removeCloudDeviceRecord: vi.fn(),
      setNativeLocale: vi.fn(),
    },
    mermaid: {
      renderMermaidDiagrams: vi.fn(async (): Promise<void> => {}),
      renderExportMermaidDiagrams: vi.fn(async (): Promise<void> => {}),
      reset: vi.fn(),
    },
    images: {
      toPng: vi.fn(async () => 'data:image/png;base64,PNG1'),
      toJpeg: vi.fn(async () => 'data:image/jpeg;base64,JPG1'),
    },
    dialogs: {
      open: vi.fn(async () => null),
      save: vi.fn(async () => 'C:/exports/conversation.png'),
    },
    armReady() {
      let release!: () => void
      ready = new Promise<void>((settle) => { release = settle })
      return release
    },
    whenReady: () => ready,
    reset() {
      ready = Promise.resolve()
    },
  }
})

vi.mock('./desktop-api', () => ({ desktopApi: barrier.api }))
vi.mock('./composables/useMermaidRenderer', () => ({ useMermaidRenderer: () => barrier.mermaid }))
vi.mock('html-to-image', () => barrier.images)
vi.mock('@tauri-apps/plugin-dialog', () => barrier.dialogs)
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn(async () => {}) }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }))
vi.mock('@tauri-apps/api/app', () => ({ setTheme: vi.fn(async () => {}) }))
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    minimize: vi.fn(async () => {}),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async () => () => {}),
  }),
}))

vi.mock('./components/ExportDocument.vue', async () => {
  const { defineComponent, h, ref } = await import('vue')
  return {
    default: defineComponent({
      name: 'ExportDocument',
      props: ['title', 'time', 'platform', 'messages', 'references', 'includeThinking', 'isPdf', 'compact', 'includeCoverPage'],
      setup(props, { expose }) {
        const root = ref<HTMLElement | null>(null)
        expose({
          getElement: () => root.value,
          whenReady: () => barrier.whenReady(),
        })
        return () => h('article', { class: 'export-document', ref: root },
          (props.messages as Array<{ role: string; content: string }>).map((message) => h('section', { class: 'export-message' }, [
            h('div', { class: 'export-message-role' }, String(message.role).toUpperCase()),
            h('div', { class: 'markdown' }, String(message.content)),
          ])))
      },
    }),
  }
})

function settingsFixture(): SettingsModel {
  return {
    setup_complete: true, secret_enabled: false, allowed_origins: [], close_behavior: 'ask', tray_click_behavior: 'show_menu', theme: 'system', language: 'zh-CN',
    semantic_search: {
      enabled: true, default_mode: 'hybrid', backend: 'local',
      local: { model: 'test', device: 'auto', dtype: 'auto' },
      ollama: { base_url: '', model: 'test' },
      llama_cpp: { base_url: '', model: 'test' },
      openai_compatible: { base_url: '', model: 'test' },
    },
    mcp_enabled: false,
    cloud_sync: {
      backend: 'webdav', enabled: false, connection_verified: false, base_url: '', root_path: '', username: '', encryption_enabled: false,
      s3: { endpoint_url: '', region: 'us-east-1', bucket: '', prefix: '', force_path_style: false },
      remote_id: 'default', vault_id: 'default', generation_id: 'generation-1',
    },
  }
}

function openedSessionFixture(): SessionOpen {
  const messages: Message[] = Array.from({ length: 4 }, (_, index) => ({
    id: `msg-${index}`,
    role: index % 2 ? 'assistant' : 'user',
    content: `Export body ${index}`,
    metadata: {},
    seq: index,
  }))
  return {
    id: 'session-export',
    platform: 'deepseek',
    platform_session_id: 'ds-1',
    title: 'Export conversation',
    created_at: '2026-09-01T08:00:00Z',
    updated_at: '2026-09-01T08:30:00Z',
    message_count: 4,
    has_branches: false,
    start_seq: 0,
    messages,
    references: [],
  }
}

// Drains pending microtask chains (Vue nextTick, mock promises) across a few
// macrotask rounds so an unblocked flow has definitely finished.
async function settle(rounds = 8) {
  for (let index = 0; index < rounds; index += 1) {
    await new Promise((resolve) => { setTimeout(resolve, 0) })
  }
}

async function clickWhenReady(query: () => HTMLButtonElement | null) {
  const button = await vi.waitFor(() => {
    const found = query()
    if (!found || found.disabled) throw new Error('control not ready yet')
    return found
  })
  button.click()
  return button
}

function formatButton(host: HTMLElement, label: string) {
  return [...host.querySelectorAll<HTMLButtonElement>('.export-format-control button')]
    .find((button) => button.textContent?.includes(label) ?? false) ?? null
}

// The preview clears exportImageChecking (and the disabled hint on the image
// formats) only after its final document pass; wait for both signals so the
// export click cannot race the format guard.
async function waitForPreviewSettled(host: HTMLElement) {
  await vi.waitFor(() => {
    const png = formatButton(host, 'PNG')
    if (!png) throw new Error('export format buttons not mounted')
    if (png.getAttribute('aria-disabled') === 'true') throw new Error('preview still checking')
    if (png.getAttribute('title')) throw new Error('image formats still disabled')
  })
  await settle(4)
}

let mountedApp: ReturnType<typeof createApp> | null = null

// Mounts the app, opens the first session and enters export selection,
// leaving the toolbar's "export selected" button armed for the caller (each
// test triggers the preview itself so the ready barrier is armed first).
async function mountToExportToolbar(session: SessionOpen) {
  barrier.reset()
  barrier.dialogs.save.mockResolvedValue('C:/exports/conversation.png')
  const api = barrier.api
  api.searchSessions.mockResolvedValue({
    sessions: [{
      id: session.id,
      platform: session.platform,
      platform_session_id: session.platform_session_id,
      title: session.title,
      created_at: session.created_at,
      updated_at: session.updated_at,
    }],
    total: 1,
    search_mode: 'hybrid',
    semantic_status: 'ready',
  })
  api.openSession.mockResolvedValue(session)
  api.getSessionMessages.mockResolvedValue([])
  api.getSettings.mockResolvedValue(settingsFixture())
  api.getApiStatus.mockResolvedValue({ service: { state: 'running' }, userscript_connected: false, mcp: { state: 'stopped' }, mcp_url: 'http://127.0.0.1:19821/mcp' })
  api.getSemanticStatus.mockResolvedValue({ enabled: false, status: 'disabled', backend: 'local', model_id: '', pending_chunks: 0, ready_chunks: 0, local_model_ready: false })
  api.writeExportFile.mockResolvedValue(undefined)
  api.printToPdf.mockResolvedValue(undefined)

  const host = document.createElement('div')
  document.body.append(host)
  const app = createApp(App, { initialSettings: settingsFixture() }).use(i18n)
  app.mount(host)
  mountedApp = app

  await clickWhenReady(() => host.querySelector<HTMLButtonElement>('button.session-row'))
  await clickWhenReady(() => host.querySelector<HTMLButtonElement>('.detail-header .detail-actions button.icon-button'))
  await clickWhenReady(() => host.querySelector<HTMLButtonElement>('.detail-menu button[role="menuitem"]'))
  await vi.waitFor(() => {
    const button = host.querySelector<HTMLButtonElement>('.export-selection-toolbar button.primary-button')
    if (!button || button.disabled) throw new Error('export toolbar not ready')
  })
  return { app, host }
}

// Arms the ready barrier, clicks the toolbar's "export selected" button and
// lets the flow settle against the pending barrier.
async function openExportDialogWithBarrier(host: HTMLElement) {
  const release = barrier.armReady()
  const button = host.querySelector<HTMLButtonElement>('.export-selection-toolbar button.primary-button')!
  button.click()
  await settle()
  return release
}

describe('App export ready barrier', () => {
  afterEach(() => {
    mountedApp?.unmount()
    mountedApp = null
    document.body.innerHTML = ''
    vi.clearAllMocks()
  })

  it('prepares the export preview only after the document reports ready', async () => {
    const { host } = await mountToExportToolbar(openedSessionFixture())
    const resolveReady = await openExportDialogWithBarrier(host)

    // While the document is still rendering frame by frame, nothing may
    // touch it.
    expect(barrier.mermaid.renderExportMermaidDiagrams).not.toHaveBeenCalled()

    resolveReady()
    await vi.waitFor(() => { expect(barrier.mermaid.renderExportMermaidDiagrams).toHaveBeenCalledTimes(1) })
    const root = host.querySelector('.export-document-host .export-document')
    expect(barrier.mermaid.renderExportMermaidDiagrams).toHaveBeenCalledWith(root)
  })

  it('captures a PNG export only after the document reports ready', async () => {
    const { host } = await mountToExportToolbar(openedSessionFixture())
    const resolvePreview = await openExportDialogWithBarrier(host)
    resolvePreview()
    await vi.waitFor(() => { expect(barrier.mermaid.renderExportMermaidDiagrams).toHaveBeenCalled() })
    await waitForPreviewSettled(host)

    const resolveExport = barrier.armReady()
    await clickWhenReady(() => host.querySelector<HTMLButtonElement>('.export-dialog footer button.primary-button'))
    await settle()

    expect(barrier.images.toPng).not.toHaveBeenCalled()

    resolveExport()
    await vi.waitFor(() => { expect(barrier.images.toPng).toHaveBeenCalledTimes(1) })
    expect(barrier.api.writeExportFile).toHaveBeenCalledWith('C:/exports/conversation.png', { encoding: 'base64', data: 'PNG1' })
  })

  it('captures a JPEG export only after the document reports ready', async () => {
    const { host } = await mountToExportToolbar(openedSessionFixture())
    const resolvePreview = await openExportDialogWithBarrier(host)
    resolvePreview()
    await vi.waitFor(() => { expect(barrier.mermaid.renderExportMermaidDiagrams).toHaveBeenCalled() })
    await waitForPreviewSettled(host)

    await clickWhenReady(() => formatButton(host, 'JPEG'))
    barrier.dialogs.save.mockResolvedValue('C:/exports/conversation.jpeg')
    const resolveExport = barrier.armReady()
    await clickWhenReady(() => host.querySelector<HTMLButtonElement>('.export-dialog footer button.primary-button'))
    await settle()

    expect(barrier.images.toJpeg).not.toHaveBeenCalled()
    expect(barrier.images.toPng).not.toHaveBeenCalled()

    resolveExport()
    await vi.waitFor(() => { expect(barrier.images.toJpeg).toHaveBeenCalledTimes(1) })
    expect(barrier.api.writeExportFile).toHaveBeenCalledWith('C:/exports/conversation.jpeg', { encoding: 'base64', data: 'JPG1' })
  })

  it('prints a PDF export only after the document reports ready', async () => {
    const { host } = await mountToExportToolbar(openedSessionFixture())
    const resolvePreview = await openExportDialogWithBarrier(host)
    resolvePreview()
    await vi.waitFor(() => { expect(barrier.mermaid.renderExportMermaidDiagrams).toHaveBeenCalled() })
    await waitForPreviewSettled(host)

    await clickWhenReady(() => formatButton(host, 'PDF'))
    barrier.dialogs.save.mockResolvedValue('C:/exports/conversation.pdf')
    const resolveExport = barrier.armReady()
    await clickWhenReady(() => host.querySelector<HTMLButtonElement>('.export-dialog footer button.primary-button'))
    await settle()

    expect(barrier.api.printToPdf).not.toHaveBeenCalled()

    resolveExport()
    await vi.waitFor(() => { expect(barrier.api.printToPdf).toHaveBeenCalledTimes(1) })
    expect(barrier.api.printToPdf).toHaveBeenCalledWith('C:/exports/conversation.pdf', { compact: false })
    expect(barrier.api.writeExportFile).not.toHaveBeenCalled()
  })
})
