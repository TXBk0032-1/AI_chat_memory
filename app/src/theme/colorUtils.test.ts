import { describe, expect, it } from 'vitest'
import {
  RGB_Alpha_Shade,
  RGB_Linear_Blend,
  RGB_Linear_Shade,
  RGB_Log_Shade,
  hexToRgb,
  normalizeColor,
  rgbToHex,
} from './colorUtils'

describe('theme colorUtils', () => {
  it('normalizes hex and rgb color strings', () => {
    expect(normalizeColor('#167961')).toBe('rgb(22, 121, 97)')
    expect(normalizeColor('#fff')).toBe('rgb(255, 255, 255)')
    expect(normalizeColor('rgb(77, 175, 124)')).toBe('rgb(77, 175, 124)')
    expect(normalizeColor('rgba(77, 175, 124, 0.5)')).toBe('rgba(77, 175, 124, 0.5)')
  })

  it('converts hex to rgb and rgba accurately', () => {
    expect(hexToRgb('#167961')).toBe('rgb(22, 121, 97)')
    expect(hexToRgb('#12345680')).toBe('rgba(18, 52, 86, 0.5)')
  })

  it('converts rgb/rgba to hex accurately', () => {
    expect(rgbToHex('rgb(22, 121, 97)')).toBe('#167961')
    expect(rgbToHex('#167961')).toBe('#167961')
  })

  it('calculates linear shading for light and dark steps', () => {
    const lightStep = RGB_Linear_Shade(0.2, 'rgb(77, 175, 124)')
    expect(lightStep).toContain('rgb(')
    const [r, g, b] = lightStep.replace('rgb(', '').replace(')', '').split(',').map((x) => parseInt(x, 10))
    expect(r).toBeGreaterThan(77)
    expect(g).toBeGreaterThan(175)
    expect(b).toBeGreaterThan(124)

    const darkStep = RGB_Linear_Shade(-0.2, 'rgb(77, 175, 124)')
    const [dr, dg, db] = darkStep.replace('rgb(', '').replace(')', '').split(',').map((x) => parseInt(x, 10))
    expect(dr).toBeLessThan(77)
    expect(dg).toBeLessThan(175)
    expect(db).toBeLessThan(124)
  })

  it('calculates log shading', () => {
    const shaded = RGB_Log_Shade(0.2, 'rgb(100, 100, 100)')
    expect(shaded).toContain('rgb(')
  })

  it('calculates alpha shading accurately', () => {
    const alphaColor = RGB_Alpha_Shade(0.3, 'rgb(77, 175, 124)')
    expect(alphaColor).toBe('rgba(77, 175, 124, 0.70)')
  })

  it('blends two colors linearly', () => {
    const blended = RGB_Linear_Blend(0.5, 'rgb(0, 0, 0)', 'rgb(200, 200, 200)')
    expect(blended).toBe('rgb(100, 100, 100)')
  })
})
