import { readdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { describe, expect, it } from 'vitest'

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name)
    return entry.isDirectory() ? sourceFiles(path) : [path]
  })
}

describe('desktop copy policy', () => {
  it('keeps Han-script user-facing literals inside locale resources', () => {
    const root = resolve(process.cwd(), 'src')
    const localeRoot = resolve(root, 'i18n', 'locales')
    const offenders = sourceFiles(root)
      .filter((path) => /\.(?:ts|vue)$/.test(path))
      .filter((path) => !path.endsWith('.test.ts'))
      .filter((path) => !path.startsWith(localeRoot))
      .flatMap((path) => readFileSync(path, 'utf8')
        .split(/\r?\n/)
        .flatMap((line, index) => {
          if (!/[\p{Script=Han}]{2,}/u.test(line)) return []
          if (/^\s*(?:\/\/|\/\*|\*|<!--)/.test(line)) return []
          return [`${path.slice(root.length + 1)}:${index + 1}: ${line.trim()}`]
        }))
    expect(offenders).toEqual([])
  })
})
