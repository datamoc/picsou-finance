import '@testing-library/jest-dom/vitest'
import { describe, it, expect, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import { DistributionPie } from './DistributionPie'
import type { DistributionItem } from './distribution-labels'

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    i18n: { language: 'en', resolvedLanguage: 'en' },
  }),
}))

vi.stubGlobal('ResizeObserver', class {
  observe() {}
  unobserve() {}
  disconnect() {}
})

const LONG_NAME = 'LIVRET DEVELOPPEMENT DURABLE ET SOLIDAIRE DE MADAME ET MONSIEUR'

const data: DistributionItem[] = [
  { accountId: 1, name: LONG_NAME, accountType: 'LDDS', color: '#123456', balanceEur: 600, percentage: 60 },
  { accountId: 2, name: 'PEA', accountType: 'PEA', color: '#654321', balanceEur: 400, percentage: 40 },
]

describe('AllocationTreemap', () => {
  it('keeps a long name inside its tile and exposes it in full on hover', () => {
    render(<DistributionPie data={data} />)
    // Radix tabs switch on mousedown, not click.
    fireEvent.mouseDown(screen.getByRole('tab', { name: 'dashboard.allocation' }), { button: 0 })

    const label = screen.getByText(LONG_NAME)
    expect(label).toHaveClass('truncate', 'max-w-full')
    expect(label.parentElement).toHaveClass('w-full', 'min-w-0', 'overflow-hidden')

    const tile = label.closest('[title]')
    expect(tile).toHaveAttribute('title', LONG_NAME)
  })

  it('does not clip the hover ring or the gap hitbox that sit outside the tile box', () => {
    render(<DistributionPie data={data} />)
    fireEvent.mouseDown(screen.getByRole('tab', { name: 'dashboard.allocation' }), { button: 0 })

    const tile = screen.getByText(LONG_NAME).closest('[title]')
    expect(tile).not.toHaveClass('overflow-hidden')
  })
})

describe('DistributionPie net/gross toggle', () => {
  const leveragedData: DistributionItem[] = [
    {
      accountId: 10,
      name: 'Appartement',
      accountType: 'REAL_ESTATE',
      color: '#a855f7',
      balanceEur: 900_000,
      linkedDebtEur: 500_000,
      percentage: 90,
    },
    {
      accountId: 20,
      name: 'PEA',
      accountType: 'PEA',
      color: '#22c55e',
      balanceEur: 100_000,
      percentage: 10,
    },
  ]

  it('defaults to net equity (deducting linked loans) and switches to gross on click', () => {
    render(<DistributionPie data={leveragedData} />)

    const netBtn = screen.getByRole('button', { name: 'dashboard.distributionBasis.net' })
    const grossBtn = screen.getByRole('button', { name: 'dashboard.distributionBasis.gross' })
    expect(netBtn).toHaveAttribute('aria-pressed', 'true')
    expect(grossBtn).toHaveAttribute('aria-pressed', 'false')

    // Net: 400k / (400k + 100k) = 80.0% and 100k / 500k = 20.0%
    expect(screen.getByText('80.0%')).toBeInTheDocument()
    expect(screen.getByText('20.0%')).toBeInTheDocument()

    // Switch to Gross: 900k / 1000k = 90.0% and 100k / 1000k = 10.0%
    fireEvent.click(grossBtn)
    expect(grossBtn).toHaveAttribute('aria-pressed', 'true')
    expect(netBtn).toHaveAttribute('aria-pressed', 'false')
    expect(screen.getByText('90.0%')).toBeInTheDocument()
    expect(screen.getByText('10.0%')).toBeInTheDocument()
  })

  it('clamps net equity at zero when linked debt exceeds asset value', () => {
    const underwater: DistributionItem[] = [
      {
        accountId: 11,
        name: 'Bien',
        accountType: 'REAL_ESTATE',
        color: '#a855f7',
        balanceEur: 200_000,
        linkedDebtEur: 250_000,
        percentage: 66.7,
      },
      {
        accountId: 21,
        name: 'Livret',
        accountType: 'SAVINGS',
        color: '#22c55e',
        balanceEur: 100_000,
        percentage: 33.3,
      },
    ]

    render(<DistributionPie data={underwater} />)
    expect(screen.getByText('0.0%')).toBeInTheDocument()
    expect(screen.getByText('100.0%')).toBeInTheDocument()
  })
})
