import { describe, expect, it } from 'vitest'
import appSource from './App.vue?raw'
import mainSource from './main.ts?raw'

describe('App setup initialization order', () => {
  it('creates branch state before computed values and the virtualizer consume it', () => {
    const branchState = appSource.indexOf('const branches = useBranchNavigation')
    const displayedMessages = appSource.indexOf('const displayedMessageSeqs = computed')
    const virtualizer = appSource.indexOf('const messageVirtualizer = useVirtualizer')

    expect(branchState).toBeGreaterThan(-1)
    expect(branchState).toBeLessThan(displayedMessages)
    expect(displayedMessages).toBeLessThan(virtualizer)
  })

  it('delegates Mermaid rendering to its composable', () => {
    expect(appSource).toContain("import { useMermaidRenderer } from './composables/useMermaidRenderer'")
    expect(appSource).toContain('useMermaidRenderer(effectiveTheme)')
    expect(appSource).not.toContain("let mermaidInstance: typeof import('mermaid')['default'] | null = null")
  })

  it('awaits locale initialization before installing i18n and mounting Vue', () => {
    const localeInitialization = mainSource.indexOf('await initializeLocale')
    const pluginInstallation = mainSource.indexOf('.use(i18n)')
    const mount = mainSource.indexOf('.mount(')

    expect(localeInitialization).toBeGreaterThan(-1)
    expect(localeInitialization).toBeLessThan(pluginInstallation)
    expect(pluginInstallation).toBeLessThan(mount)
  })

  it('passes preloaded settings into App so initialization does not fetch them twice', () => {
    expect(mainSource).toContain('createApp(App, { initialSettings })')
    expect(appSource).toContain('initialSettings?: SettingsModel')
    expect(appSource).toContain('props.initialSettings')
  })
  it('connects language preview to both settings lifecycle and the dialog event', () => {
    expect(appSource).toContain('useLocale(')
    expect(appSource).toContain('begin: locale.beginPreview')
    expect(appSource).toContain('@preview-language="previewLanguage"')
  })
})
