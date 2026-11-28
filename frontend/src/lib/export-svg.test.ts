import { describe, it, expect } from 'vitest'
import { serializeChartSvg } from './export-svg'

describe('serializeChartSvg', () => {
  it('returns null when no SVG exists inside the container', () => {
    const div = document.createElement('div')
    expect(serializeChartSvg(div)).toBeNull()
  })

  it('resolves CSS custom properties and adds an opaque background rect', () => {
    const host = document.createElement('div')
    host.style.setProperty('--chart-1', '#2563eb')
    host.style.setProperty('--card', '#ffffff')
    host.innerHTML = `<svg width="400" height="200"><path d="M0 0 L100 100" stroke="var(--chart-1)" fill="none" /></svg>`
    document.body.appendChild(host)

    try {
      const markup = serializeChartSvg(host, { title: 'Net worth' })
      expect(markup).not.toBeNull()
      expect(markup).toContain('<?xml version="1.0" encoding="UTF-8"?>')
      expect(markup).toContain('<title>Net worth</title>')
      expect(markup).toContain('stroke="#2563eb"')
      expect(markup).toContain('width="100%"')
      expect(markup).toContain('xmlns="http://www.w3.org/2000/svg"')
    } finally {
      host.remove()
    }
  })
})
