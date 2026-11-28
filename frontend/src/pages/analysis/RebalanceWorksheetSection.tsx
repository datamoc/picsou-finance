import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { DownloadSimple } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { useMoney } from '@/hooks/use-money'
import { signedAmountClass } from '@/lib/money'
import { cn, formatPercent, parseAmount } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'

interface RebalanceRow {
  tier: string
  currentEur: number
  currentPct: number
  targetPct: number
  driftPct: number
  inBand: boolean
  isDriftDriver: boolean
  action: 'BUY' | 'SELL' | 'HOLD'
  tradeEur: number
  postEur: number
  postPct: number
  postDriftPct: number
}

interface RebalancePlanResponse {
  currentTotalEur: number
  postTotalEur: number
  cashContributionEur: number
  driftBandPct: number
  cashOnly: boolean
  maxAbsDriftBefore: number
  maxAbsDriftAfter: number
  cashNeededToZeroDriftWithoutSelling: number
  rows: RebalanceRow[]
}

export function RebalanceWorksheetSection() {
  const { t } = useTranslation()
  const money = useMoney()
  const [cashInput, setCashInput] = useState('1000')
  const [driftBandInput, setDriftBandInput] = useState('5')
  const [cashOnly, setCashOnly] = useState(true)

  const cashContributionEur = Math.max(0, Number.isFinite(parseAmount(cashInput)) ? parseAmount(cashInput) : 0)
  const driftBandPct = Math.min(50, Math.max(0, Number.isFinite(parseAmount(driftBandInput)) ? parseAmount(driftBandInput) : 5))

  const { data } = useQuery({
    queryKey: ['analysis', 'rebalance', cashContributionEur, driftBandPct, cashOnly],
    queryFn: async () =>
      (
        await api.post<RebalancePlanResponse>('/analysis/rebalance-plan', {
          cashContributionEur,
          driftBandPct,
          cashOnly,
        })
      ).data,
  })

  function exportCsv() {
    if (!data) return
    const header = 'Tier,CurrentEUR,CurrentPct,TargetPct,DriftPct,Action,TradeEUR,PostEUR,PostPct,PostDriftPct'
    const lines = data.rows.map((r) =>
      [
        r.tier,
        r.currentEur,
        r.currentPct,
        r.targetPct,
        r.driftPct,
        r.action,
        r.tradeEur,
        r.postEur,
        r.postPct,
        r.postDriftPct,
      ].join(','),
    )
    const blob = new Blob([[header, ...lines].join('\n')], { type: 'text/csv;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = 'picsou-rebalance-plan.csv'
    document.body.appendChild(a)
    a.click()
    a.remove()
    URL.revokeObjectURL(url)
  }

  return (
    <div className="space-y-4">
      <Card className="rounded-xl bg-card">
        <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-3">
          <div className="space-y-1">
            <CardTitle className="text-base font-semibold">{t('rebalance.title')}</CardTitle>
            <CardDescription>{t('rebalance.description')}</CardDescription>
          </div>
          <Button type="button" size="sm" variant="outline" disabled={!data} onClick={exportCsv}>
            <DownloadSimple className="size-4" aria-hidden="true" />
            {t('rebalance.exportCsv')}
          </Button>
        </CardHeader>
        <CardContent className="space-y-5">
          <div className="grid gap-4 sm:grid-cols-3">
            <div className="space-y-1.5">
              <Label htmlFor="rebalance-cash">{t('rebalance.cashContribution')}</Label>
              <Input
                id="rebalance-cash"
                inputMode="decimal"
                value={cashInput}
                onChange={(e) => setCashInput(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="rebalance-band">{t('rebalance.driftBand')}</Label>
              <Input
                id="rebalance-band"
                inputMode="decimal"
                value={driftBandInput}
                onChange={(e) => setDriftBandInput(e.target.value)}
              />
            </div>
            <div className="flex items-center justify-between gap-3 rounded-lg border border-border/60 px-3.5 py-2">
              <div className="space-y-0.5">
                <Label htmlFor="rebalance-cash-only" className="cursor-pointer text-sm font-medium">
                  {t('rebalance.cashOnly')}
                </Label>
                <p className="text-xs text-muted-foreground">{t('rebalance.cashOnlyHint')}</p>
              </div>
              <Switch
                id="rebalance-cash-only"
                checked={cashOnly}
                onCheckedChange={setCashOnly}
              />
            </div>
          </div>

          {data && (
            <>
              <div className="grid gap-3 sm:grid-cols-3">
                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('rebalance.maxDriftBefore')}</p>
                  <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                    {formatPercent(data.maxAbsDriftBefore)}
                  </p>
                </div>
                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('rebalance.maxDriftAfter')}</p>
                  <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                    {formatPercent(data.maxAbsDriftAfter)}
                  </p>
                </div>
                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('rebalance.cashForZeroDrift')}</p>
                  <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                    {money.amount(data.cashNeededToZeroDriftWithoutSelling, 'EUR')}
                  </p>
                </div>
              </div>

              <div className="overflow-x-auto rounded-xl border border-border/60">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-border/60 bg-muted/30 text-left text-xs text-muted-foreground">
                      <th className="px-3 py-2 font-medium">{t('rebalance.columns.tier')}</th>
                      <th className="px-3 py-2 text-right font-medium">{t('rebalance.columns.current')}</th>
                      <th className="px-3 py-2 text-right font-medium">{t('rebalance.columns.target')}</th>
                      <th className="px-3 py-2 text-right font-medium">{t('rebalance.columns.drift')}</th>
                      <th className="px-3 py-2 text-right font-medium">{t('rebalance.columns.order')}</th>
                      <th className="px-3 py-2 text-right font-medium">{t('rebalance.columns.post')}</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-border/60">
                    {data.rows.map((row) => (
                      <tr key={row.tier}>
                        <td className="px-3 py-2.5">
                          <div className="flex items-center gap-2">
                            <span className="font-medium text-foreground">
                              {t(`analysis.tiers.${row.tier}`, row.tier)}
                            </span>
                            {row.isDriftDriver && (
                              <Badge variant="destructive">{t('rebalance.driftDriver')}</Badge>
                            )}
                          </div>
                        </td>
                        <td className="px-3 py-2.5 text-right tabular-nums">
                          <div>{money.amount(row.currentEur, 'EUR')}</div>
                          <div className="text-xs text-muted-foreground">{formatPercent(row.currentPct)}</div>
                        </td>
                        <td className="px-3 py-2.5 text-right tabular-nums">
                          {formatPercent(row.targetPct)}
                        </td>
                        <td className={cn('px-3 py-2.5 text-right tabular-nums', signedAmountClass(row.driftPct))}>
                          {formatPercent(row.driftPct, { signed: true })}
                        </td>
                        <td className="px-3 py-2.5 text-right tabular-nums">
                          <Badge
                            variant={row.action === 'BUY' ? 'default' : row.action === 'SELL' ? 'destructive' : 'secondary'}
                            className="mr-2"
                          >
                            {t(`rebalance.action.${row.action}`)}
                          </Badge>
                          <span className={cn('font-medium', signedAmountClass(row.tradeEur))}>
                            {row.tradeEur > 0 ? '+' : ''}{money.amount(row.tradeEur, 'EUR')}
                          </span>
                        </td>
                        <td className="px-3 py-2.5 text-right tabular-nums">
                          <div>{money.amount(row.postEur, 'EUR')}</div>
                          <div className="text-xs text-muted-foreground">{formatPercent(row.postPct)}</div>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
