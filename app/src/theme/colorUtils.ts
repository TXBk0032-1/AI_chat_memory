/**
 * Color utility functions adapted and modernized from LX Music Desktop theme engine.
 * Supports linear/log shading, alpha shading, blending, and format normalization.
 */

/**
 * Normalizes hex, rgb, or rgba string to standard 'rgb(r, g, b)' or 'rgba(r, g, b, a)' format.
 */
export function normalizeColor(color: string): string {
  const trimmed = color.trim()
  if (trimmed.startsWith('#')) {
    return hexToRgb(trimmed)
  }
  return trimmed
}

/**
 * Converts a hex color string (#RGB, #RGBA, #RRGGBB, #RRGGBBAA) to rgb/rgba string.
 */
export function hexToRgb(hex: string): string {
  let cleanHex = hex.replace('#', '').trim()
  if (cleanHex.length === 3 || cleanHex.length === 4) {
    cleanHex = cleanHex
      .split('')
      .map((char) => char + char)
      .join('')
  }

  const num = parseInt(cleanHex, 16)
  if (cleanHex.length === 6) {
    const r = (num >> 16) & 255
    const g = (num >> 8) & 255
    const b = num & 255
    return `rgb(${r}, ${g}, ${b})`
  }

  if (cleanHex.length === 8) {
    const r = (num >> 24) & 255
    const g = (num >> 16) & 255
    const b = (num >> 8) & 255
    const a = Math.round(((num & 255) / 255) * 100) / 100
    return `rgba(${r}, ${g}, ${b}, ${a})`
  }

  return 'rgb(0, 0, 0)'
}

/**
 * Converts rgb/rgba string to 6-digit or 8-digit hex.
 */
export function rgbToHex(rgbStr: string): string {
  const normalized = normalizeColor(rgbStr)
  const match = normalized.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/)
  if (!match) return '#000000'

  const r = parseInt(match[1], 10).toString(16).padStart(2, '0')
  const g = parseInt(match[2], 10).toString(16).padStart(2, '0')
  const b = parseInt(match[3], 10).toString(16).padStart(2, '0')

  if (match[4] !== undefined) {
    const alpha = Math.round(parseFloat(match[4]) * 255)
      .toString(16)
      .padStart(2, '0')
    return `#${r}${g}${b}${alpha}`
  }

  return `#${r}${g}${b}`
}

/**
 * Linear color shading: lighten (> 0) or darken (< 0).
 * @param p Percentage between -1.0 and 1.0 (negative = darken towards black, positive = lighten towards white).
 * @param c0 rgb(a) color string.
 */
export function RGB_Linear_Shade(p: number, c0: string): string {
  const norm = normalizeColor(c0)
  const [a, b, c, d] = norm.split(',')
  const isNegative = p < 0
  const t = isNegative ? 0 : 255 * p
  const P = isNegative ? 1 + p : 1 - p

  const rStr = a.includes('a') ? a.slice(5) : a.slice(4)
  const rVal = Math.round(parseInt(rStr, 10) * P + t)
  const gVal = Math.round(parseInt(b, 10) * P + t)
  const bVal = Math.round(parseInt(c, 10) * P + t)

  if (d) {
    return `rgba(${rVal}, ${gVal}, ${bVal},${d}`
  }
  return `rgb(${rVal}, ${gVal}, ${bVal})`
}

/**
 * Logarithmic color shading: preserves perceived luminance more accurately.
 * @param p Percentage between -1.0 and 1.0 (negative = darken towards black, positive = lighten towards white).
 * @param c0 rgb(a) color string.
 */
export function RGB_Log_Shade(p: number, c0: string): string {
  const norm = normalizeColor(c0)
  const [a, b, c, d] = norm.split(',')
  const isNegative = p < 0
  const t = isNegative ? 0 : p * 255 ** 2
  const P = isNegative ? 1 + p : 1 - p

  const rStr = a.includes('a') ? a.slice(5) : a.slice(4)
  const rVal = Math.round((P * parseInt(rStr, 10) ** 2 + t) ** 0.5)
  const gVal = Math.round((P * parseInt(b, 10) ** 2 + t) ** 0.5)
  const bVal = Math.round((P * parseInt(c, 10) ** 2 + t) ** 0.5)

  if (d) {
    return `rgba(${rVal}, ${gVal}, ${bVal},${d}`
  }
  return `rgb(${rVal}, ${gVal}, ${bVal})`
}

/**
 * Modifies alpha channel of a color.
 * @param p Target alpha delta or multiplier (-1.0 to 1.0).
 * @param color rgb(a) color string.
 */
export function RGB_Alpha_Shade(p: number, color: string): string {
  const norm = normalizeColor(color)
  const isNegative = p < 0
  const [rRaw, gRaw, bRaw, aRaw] = norm.split(',')
  const rNum = parseInt(rRaw.includes('a') ? rRaw.slice(5) : rRaw.slice(4), 10)
  const gNum = parseInt(gRaw, 10)
  const bNum = parseInt(bRaw, 10)

  let alpha: number
  if (aRaw) {
    const curA = parseFloat(aRaw)
    alpha = curA - (isNegative ? (1 - curA) * p : curA * p)
    alpha = isNegative ? Math.max(0, alpha) : Math.min(1, alpha)
  } else {
    alpha = 1 - p
    alpha = Math.min(1, alpha)
  }

  return `rgba(${rNum}, ${gNum}, ${bNum}, ${alpha.toFixed(2)})`
}

/**
 * Linear blend between two colors.
 * @param p Percentage (0.0 - 1.0).
 * @param c0 rgb(a) color 1.
 * @param c1 rgb(a) color 2.
 */
export function RGB_Linear_Blend(p: number, c0: string, c1: string): string {
  const norm0 = normalizeColor(c0)
  const norm1 = normalizeColor(c1)
  const P = 1 - p

  const [a0, b0, c0Part, d0] = norm0.split(',')
  const [a1, b1, c1Part, d1] = norm1.split(',')

  const r0 = parseInt(a0.includes('a') ? a0.slice(5) : a0.slice(4), 10)
  const r1 = parseInt(a1.includes('a') ? a1.slice(5) : a1.slice(4), 10)
  const g0 = parseInt(b0, 10)
  const g1 = parseInt(b1, 10)
  const b0Val = parseInt(c0Part, 10)
  const b1Val = parseInt(c1Part, 10)

  const r = Math.round(r0 * P + r1 * p)
  const g = Math.round(g0 * P + g1 * p)
  const b = Math.round(b0Val * P + b1Val * p)

  const hasAlpha = Boolean(d0 || d1)
  if (hasAlpha) {
    const a0Val = d0 ? parseFloat(d0) : 1
    const a1Val = d1 ? parseFloat(d1) : 1
    const a = Math.round((a0Val * P + a1Val * p) * 1000) / 1000
    return `rgba(${r}, ${g}, ${b}, ${a})`
  }

  return `rgb(${r}, ${g}, ${b})`
}

/**
 * Converts any valid hex/rgb/rgba string to 6-digit hex format (#RRGGBB).
 */
export function toHex6(color: string): string {
  const hex = rgbToHex(color)
  if (hex.length >= 7) {
    return hex.slice(0, 7)
  }
  return hex.padEnd(7, '0')
}

/**
 * Parses an rgba/rgb/hex string into r, g, b, and alpha (0-1).
 */
export function parseRgba(color: string): { r: number; g: number; b: number; a: number } {
  const norm = normalizeColor(color)
  const match = norm.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/)
  if (match) {
    return {
      r: parseInt(match[1], 10),
      g: parseInt(match[2], 10),
      b: parseInt(match[3], 10),
      a: match[4] !== undefined ? parseFloat(match[4]) : 1,
    }
  }
  return { r: 0, g: 0, b: 0, a: 1 }
}

/**
 * Formats r, g, b, a into a standard CSS color string.
 */
export function formatRgba(r: number, g: number, b: number, a = 1): string {
  const clampedR = Math.max(0, Math.min(255, Math.round(r)))
  const clampedG = Math.max(0, Math.min(255, Math.round(g)))
  const clampedB = Math.max(0, Math.min(255, Math.round(b)))
  const clampedA = Math.max(0, Math.min(1, Math.round(a * 100) / 100))
  if (clampedA < 1) {
    return `rgba(${clampedR}, ${clampedG}, ${clampedB}, ${clampedA})`
  }
  return `rgb(${clampedR}, ${clampedG}, ${clampedB})`
}

/**
 * Validates whether a color string is a valid Hex, RGB, or RGBA color.
 */
export function isValidColor(color: string): boolean {
  if (!color || typeof color !== 'string') return false
  const trimmed = color.trim()
  if (/^#([0-9a-fA-F]{3}|[0-9a-fA-F]{4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.test(trimmed)) {
    return true
  }
  return /^rgba?\(\s*\d+\s*,\s*\d+\s*,\s*\d+\s*(?:,\s*[\d.]+\s*)?\)$/.test(trimmed)
}
