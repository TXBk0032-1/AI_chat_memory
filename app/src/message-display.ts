export const OVERSIZED_MESSAGE_THRESHOLD = 100_000
export const OVERSIZED_MESSAGE_PREVIEW_LENGTH = 12_000

export function isOversizedMessage(content: string) {
  return content.length > OVERSIZED_MESSAGE_THRESHOLD
}

export function createMessagePreview(content: string) {
  let end = Math.min(content.length, OVERSIZED_MESSAGE_PREVIEW_LENGTH)
  const lastCodeUnit = content.charCodeAt(end - 1)
  if (lastCodeUnit >= 0xD800 && lastCodeUnit <= 0xDBFF) end += 1

  return {
    text: content.slice(0, end),
    originalLength: content.length,
  }
}
