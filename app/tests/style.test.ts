import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const styleSource = readFileSync(new URL('../src/style.css', import.meta.url), 'utf8')

describe('search toolbar layout', () => {
  it('top-aligns toolbar controls when the search summary appears', () => {
    // Given the real application stylesheet
    const controlBar = styleSource.match(/\.control-bar\s*\{([^}]*)\}/)?.[1]
    const resultCount = styleSource.match(/\.result-count\s*\{([^}]*)\}/)?.[1]

    // When the toolbar alignment contract is inspected
    expect(controlBar).toBeDefined()

    // Then controls share the first row instead of centering against the summary
    expect(controlBar).toMatch(/\balign-items:\s*flex-start\s*;/)
    expect(resultCount).toMatch(/\bline-height:\s*36px\s*;/)
  })
})
