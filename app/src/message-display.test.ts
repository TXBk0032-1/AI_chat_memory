import { describe, expect, it } from 'vitest'
import {
  OVERSIZED_MESSAGE_PREVIEW_LENGTH,
  OVERSIZED_MESSAGE_THRESHOLD,
  createMessagePreview,
  isOversizedMessage,
} from './message-display'

describe('oversized message display rules', () => {
  it('enters lightweight mode only above the content threshold', () => {
    expect(OVERSIZED_MESSAGE_THRESHOLD).toBe(100_000)
    expect(isOversizedMessage('x'.repeat(100_000))).toBe(false)
    expect(isOversizedMessage('x'.repeat(100_001))).toBe(true)
  })

  it('limits the preview while preserving the original UTF-16 length', () => {
    const content = 'x'.repeat(15_000)
    const preview = createMessagePreview(content)

    expect(OVERSIZED_MESSAGE_PREVIEW_LENGTH).toBe(12_000)
    expect(preview.text).toBe('x'.repeat(12_000))
    expect(preview.originalLength).toBe(15_000)
  })

  it('does not split a surrogate pair at the preview boundary', () => {
    const preview = createMessagePreview(`${'x'.repeat(11_999)}😀tail`)

    expect(preview.text.endsWith('😀')).toBe(true)
    expect(preview.text).toHaveLength(12_001)
  })
})
