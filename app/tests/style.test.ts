import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const styleSource = readFileSync(new URL('../src/style.css', import.meta.url), 'utf8')

function ruleFor(selector: string) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return styleSource.match(new RegExp(`${escapedSelector}\\s*\\{([^}]*)\\}`))?.[1]
}

describe('search toolbar layout', () => {
  it('top-aligns toolbar controls and preserves the direct filter button', () => {
    const controlBar = ruleFor('.control-bar')
    const filterButton = ruleFor('.control-bar > .filter-button')

    expect(controlBar).toBeDefined()
    expect(controlBar).toMatch(/\balign-items:\s*flex-start\s*;/)
    expect(filterButton).toMatch(/\bflex:\s*0\s+0\s+auto\s*;/)
  })

  it('keeps the result count at control height without synthetic line height', () => {
    const resultCount = ruleFor('.result-count')

    expect(resultCount).toMatch(/(?:^|;)\s*height:\s*36px\s*;/)
    expect(resultCount).toMatch(/\bdisplay:\s*inline-flex\s*;/)
    expect(resultCount).toMatch(/\balign-items:\s*center\s*;/)
    expect(resultCount).toMatch(/\bwhite-space:\s*nowrap\s*;/)
    expect(resultCount).toMatch(/\bflex:\s*0\s+0\s+auto\s*;/)
    expect(resultCount).toMatch(/(?:^|;)\s*line-height:\s*normal\s*;/)
    expect(resultCount).not.toMatch(/(?:^|;)\s*line-height:\s*36px\s*;/)
  })

  it('lets the search stack shrink while consuming remaining toolbar space', () => {
    const searchStack = ruleFor('.search-stack')

    expect(searchStack).toMatch(/\bmin-width:\s*0\s*;/)
    expect(searchStack).toMatch(/\bflex:\s*1\s+1\s+auto\s*;/)
  })

  it('allows the search row to shrink', () => {
    const searchRow = ruleFor('.search-row')

    expect(searchRow).toMatch(/\bmin-width:\s*0\s*;/)
  })

  it('allows the search field to shrink while retaining its desktop width cap', () => {
    const searchField = ruleFor('.search-field')

    expect(searchField).toMatch(/\bmin-width:\s*0\s*;/)
    expect(searchField).toMatch(/\bwidth:\s*min\(460px,\s*48vw\)\s*;/)
  })
})
