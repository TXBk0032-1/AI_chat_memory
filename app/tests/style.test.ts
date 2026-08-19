import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const styleSource = readFileSync(new URL('../src/style.css', import.meta.url), 'utf8')

function ruleFor(selector: string, source = styleSource) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  return source.match(new RegExp(`(?:^|})\\s*${escapedSelector}\\s*\\{([^}]*)\\}`))?.[1]
}

function blockFor(atRule: string) {
  const start = styleSource.indexOf(atRule)
  if (start < 0) return undefined

  const openingBrace = styleSource.indexOf('{', start + atRule.length)
  if (openingBrace < 0) return undefined

  let depth = 1
  for (let index = openingBrace + 1; index < styleSource.length; index += 1) {
    if (styleSource[index] === '{') depth += 1
    if (styleSource[index] === '}') depth -= 1
    if (depth === 0) return styleSource.slice(openingBrace + 1, index)
  }

  return undefined
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

  it('keeps the search stack at content width while allowing it to shrink', () => {
    const searchStack = ruleFor('.search-stack')

    expect(searchStack).toMatch(/\bmin-width:\s*0\s*;/)
    expect(searchStack).toMatch(/\bflex:\s*0\s+1\s+auto\s*;/)
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

describe('print export layout', () => {
  it('removes the application shell from print flow and expands the export document', () => {
    const printBlock = blockFor('@media print')
    expect(printBlock).toBeDefined()

    const rootLayout = ruleFor('html, body, #app, .app-frame', printBlock)
    const appFrame = ruleFor('.app-frame', printBlock)
    const nonExportContent = ruleFor('.app-frame > :not(.export-document-host)', printBlock)
    const exportHost = ruleFor('.export-document-host', printBlock)

    expect(rootLayout).toMatch(/\bmin-width:\s*0\s*!important\s*;/)
    expect(rootLayout).toMatch(/\bmin-height:\s*0\s*!important\s*;/)
    expect(rootLayout).toMatch(/\boverflow:\s*visible\s*!important\s*;/)
    expect(appFrame).toMatch(/\bdisplay:\s*block\s*!important\s*;/)
    expect(nonExportContent).toMatch(/\bdisplay:\s*none\s*!important\s*;/)
    expect(exportHost).toMatch(/\bdisplay:\s*block\s*!important\s*;/)
    expect(exportHost).toMatch(/\bposition:\s*static\s*!important\s*;/)
    expect(exportHost).toMatch(/\bwidth:\s*100%\s*!important\s*;/)
    expect(exportHost).toMatch(/\bmax-width:\s*100%\s*!important\s*;/)
  })

  it('leaves page margins to the native WebView2 print settings', () => {
    const printBlock = blockFor('@media print')
    expect(printBlock).toBeDefined()

    const page = ruleFor('@page', printBlock)
    expect(page).toMatch(/\bsize:\s*A4\s+portrait\s*;/)
    expect(page).not.toMatch(/\bmargin\s*:/)
  })
})
