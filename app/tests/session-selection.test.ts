import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const appSource = readFileSync(new URL('../src/App.vue', import.meta.url), 'utf8')

describe('session selection', () => {
  it('ignores the open session before resetting or loading detail state', () => {
    const start = appSource.indexOf('async function selectSession(id: string)')
    const end = appSource.indexOf('\nfunction retryBackgroundLoad()', start)
    const selectSession = appSource.slice(start, end)

    const sameSessionGuard = selectSession.indexOf('if (!detail.shouldOpen(id)) return')
    const firstSideEffect = selectSession.indexOf('persistReadingPosition()')
    const detailLoad = selectSession.indexOf('detail.open(id)')

    expect(start).toBeGreaterThanOrEqual(0)
    expect(end).toBeGreaterThan(start)
    expect(sameSessionGuard).toBeGreaterThanOrEqual(0)
    expect(sameSessionGuard).toBeLessThan(firstSideEffect)
    expect(sameSessionGuard).toBeLessThan(detailLoad)
  })
})
