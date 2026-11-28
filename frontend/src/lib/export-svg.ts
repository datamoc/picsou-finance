/**
 * Serializes a Recharts (or generic) SVG inside a container into a self-contained SVG string
 * with resolved CSS custom properties and an explicit background fill so the downloaded file
 * renders faithfully outside the app in both light and dark themes.
 */

const CSS_VAR_RE = /var\(\s*(--[a-zA-Z0-9_-]+)\s*(?:,\s*([^)]+))?\)/g

function resolveCssVariables(
  value: string,
  lookup: (name: string) => string,
  depth = 0,
): string {
  if (!value.includes('var(') || depth > 5) return value
  const replaced = value.replace(CSS_VAR_RE, (_m, varName: string, fallback?: string) => {
    const resolved = lookup(varName).trim()
    if (resolved) return resolved
    return fallback ? fallback.trim() : ''
  })
  return replaced === value ? value : resolveCssVariables(replaced, lookup, depth + 1)
}

function buildVarLookup(sourceEl: Element): (name: string) => string {
  const chain: CSSStyleDeclaration[] = []
  let el: Element | null = sourceEl
  while (el) {
    chain.push(window.getComputedStyle(el))
    el = el.parentElement
  }
  const cache = new Map<string, string>()
  return (name: string) => {
    const cached = cache.get(name)
    if (cached !== undefined) return cached
    for (const style of chain) {
      const val = style.getPropertyValue(name).trim()
      if (val) {
        cache.set(name, val)
        return val
      }
    }
    cache.set(name, '')
    return ''
  }
}

const RESOLVABLE_ATTRS = ['stroke', 'fill', 'stop-color', 'color', 'style'] as const

export function serializeChartSvg(
  container: HTMLElement,
  options?: { title?: string },
): string | null {
  const sourceSvg = container.querySelector('svg')
  if (!sourceSvg) return null

  const clone = sourceSvg.cloneNode(true) as SVGSVGElement
  const rect = sourceSvg.getBoundingClientRect()
  const width = Math.max(1, Math.round(rect.width || Number(sourceSvg.getAttribute('width')) || 800))
  const height = Math.max(1, Math.round(rect.height || Number(sourceSvg.getAttribute('height')) || 300))

  clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg')
  clone.setAttribute('width', String(width))
  clone.setAttribute('height', String(height))
  if (!clone.getAttribute('viewBox')) {
    clone.setAttribute('viewBox', `0 0 ${width} ${height}`)
  }

  const lookup = buildVarLookup(sourceSvg)
  const bg = lookup('--card') || lookup('--background') || '#ffffff'
  const fg = lookup('--foreground') || '#111111'
  const fontFamily = window.getComputedStyle(document.documentElement).fontFamily || 'sans-serif'

  clone.setAttribute('style', `font-family: ${fontFamily}; color: ${fg};`)

  // Resolve CSS variables on every element in the cloned SVG
  const allNodes = [clone, ...Array.from(clone.querySelectorAll('*'))]
  for (const node of allNodes) {
    for (const attr of RESOLVABLE_ATTRS) {
      const raw = node.getAttribute(attr)
      if (raw && raw.includes('var(')) {
        node.setAttribute(attr, resolveCssVariables(raw, lookup))
      }
    }
  }

  // Insert an opaque background rectangle as first visual element
  const ns = 'http://www.w3.org/2000/svg'
  const bgRect = document.createElementNS(ns, 'rect')
  bgRect.setAttribute('x', '0')
  bgRect.setAttribute('y', '0')
  bgRect.setAttribute('width', '100%')
  bgRect.setAttribute('height', '100%')
  bgRect.setAttribute('fill', resolveCssVariables(bg, lookup) || '#ffffff')
  clone.insertBefore(bgRect, clone.firstChild)

  if (options?.title) {
    const titleEl = document.createElementNS(ns, 'title')
    titleEl.textContent = options.title
    clone.insertBefore(titleEl, clone.firstChild)
  }

  const serializer = new XMLSerializer()
  return `<?xml version="1.0" encoding="UTF-8"?>\n${serializer.serializeToString(clone)}`
}

export function downloadChartSvg(
  container: HTMLElement | null,
  filename = 'picsou-chart.svg',
  title?: string,
): boolean {
  if (!container) return false
  const svgMarkup = serializeChartSvg(container, { title })
  if (!svgMarkup) return false
  const blob = new Blob([svgMarkup], { type: 'image/svg+xml;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename.endsWith('.svg') ? filename : `${filename}.svg`
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
  URL.revokeObjectURL(url)
  return true
}
