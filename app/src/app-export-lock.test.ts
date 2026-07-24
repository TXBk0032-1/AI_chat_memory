import { describe, expect, it } from 'vitest'
import appSource from './App.vue?raw'

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
