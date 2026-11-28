import { useMemo, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from 'recharts'
import { api } from '@/lib/api-client'
import { useMoney } from '@/hooks/use-money'
import { signedAmountClass } from '@/lib/money'
import { cn, formatPercent } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { ChartContainer, ChartTooltip, type ChartConfig } from '@/components/ui/chart'
import { ExportSvgButton } from '@/components/shared/ExportSvgButton'
import { AXIS_PROPS } from '@/lib/chart-axis'

interface IncomeMonthlyBucket {
  month: string
  dividendsEur: number
  interestEur: number
  scpiEur: number
  totalEur: number
}

interface IncomeByCurrency {
  currency: string
  nativeAmount: number
  amountEur: number
  converted: boolean
}

interface IncomeByAsset {
  label: string
  ticker: string | null
  kind: string
  totalEur: number
  paymentCount: number
}

interface IncomeSummaryResponse {
  year: number
  totalEur: number
  previousYearTotalEur: number
  yoyChangePct: number | null
  monthlyAverageEur: number
  monthly: IncomeMonthlyBucket[]
  byCurrency: IncomeByCurrency[]
  byAsset: IncomeByAsset[]
  unconvertedCurrencies: string[]
}

export function IncomeSection() {
  const { t } = useTranslation()
  const money = useMoney()
  const chartRef = useRef<HTMLDivElement>(null)
  const currentYear = new Date().getFullYear()
  const [year, setYear] = useState(currentYear)

  const { data } = useQuery({
    queryKey: ['wealth', 'income', year],
    queryFn: async () => (await api.get<IncomeSummaryResponse>(`/wealth/income?year=${year}`)).data,
  })

  const chartConfig = useMemo(
    () =>
      ({
        dividendsEur: { label: t('income.kinds.dividends'), color: 'var(--chart-1)' },
        interestEur: { label: t('income.kinds.interest'), color: 'var(--chart-2)' },
        scpiEur: { label: t('income.kinds.scpi'), color: 'var(--chart-3)' },
      }) satisfies ChartConfig,
    [t],
  )

  const years = [currentYear, currentYear - 1, currentYear - 2]

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-1 rounded-lg border border-border/60 bg-background/80 p-1">
          {years.map((y) => (
            <button
              key={y}
              type="button"
              onClick={() => setYear(y)}
              className={cn(
                'h-7 rounded-md px-3 text-xs font-medium tabular-nums transition-colors',
                year === y
                  ? 'bg-primary text-primary-foreground'
                  : 'text-muted-foreground hover:text-foreground',
              )}
            >
              {y}
            </button>
          ))}
        </div>
      </div>

      {data && (
        <>
          {data.unconvertedCurrencies.length > 0 && (
            <div className="rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-xs text-destructive">
              {t('income.unconvertedNotice', { currencies: data.unconvertedCurrencies.join(', ') })}
            </div>
          )}

          <div className="grid gap-3 sm:grid-cols-3">
            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('income.totalYear', { year: data.year })}</CardDescription>
                <CardTitle className="text-2xl font-bold tabular-nums">
                  {money.amount(data.totalEur, 'EUR')}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground tabular-nums">
                  {t('income.monthlyAvg')}: {money.amount(data.monthlyAverageEur, 'EUR')}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('income.yoyChange')}</CardDescription>
                <CardTitle className={cn('text-2xl font-bold tabular-nums', signedAmountClass(data.yoyChangePct))}>
                  {data.yoyChangePct != null ? formatPercent(data.yoyChangePct, { signed: true }) : '—'}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground tabular-nums">
                  {data.year - 1}: {money.amount(data.previousYearTotalEur, 'EUR')}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('income.currencies')}</CardDescription>
                <CardTitle className="text-base font-semibold">
                  {data.byCurrency.length}
                </CardTitle>
              </CardHeader>
              <CardContent className="flex flex-wrap gap-1.5">
                {data.byCurrency.map((c) => (
                  <Badge key={c.currency} variant={c.converted ? 'secondary' : 'destructive'}>
                    {c.currency}: {money.amount(c.amountEur, 'EUR')}
                  </Badge>
                ))}
              </CardContent>
            </Card>
          </div>

          <Card className="rounded-xl bg-card">
            <CardHeader className="flex flex-row items-center justify-between gap-2">
              <div>
                <CardTitle className="text-base font-semibold">{t('income.chartTitle')}</CardTitle>
                <CardDescription>{t('income.chartDesc')}</CardDescription>
              </div>
              <ExportSvgButton
                containerRef={chartRef}
                filename={`picsou-income-${year}.svg`}
                title={t('income.chartTitle')}
              />
            </CardHeader>
            <CardContent>
              <div ref={chartRef}>
                <ChartContainer config={chartConfig} className="h-[260px] w-full">
                  <BarChart data={data.monthly} margin={{ top: 10, right: 16, left: 4, bottom: 0 }}>
                    <CartesianGrid strokeDasharray="3 3" vertical={false} />
                    <XAxis dataKey="month" {...AXIS_PROPS} />
                    <YAxis {...AXIS_PROPS} tickFormatter={money.tick} width={84} />
                    <ChartTooltip />
                    <Bar dataKey="dividendsEur" stackId="a" fill="var(--color-dividendsEur)" radius={[0, 0, 0, 0]} />
                    <Bar dataKey="interestEur" stackId="a" fill="var(--color-interestEur)" radius={[0, 0, 0, 0]} />
                    <Bar dataKey="scpiEur" stackId="a" fill="var(--color-scpiEur)" radius={[4, 4, 0, 0]} />
                  </BarChart>
                </ChartContainer>
              </div>
            </CardContent>
          </Card>

          {data.byAsset.length > 0 && (
            <Card className="rounded-xl bg-card">
              <CardHeader>
                <CardTitle className="text-base font-semibold">{t('income.topPayers')}</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="divide-y divide-border/60 rounded-xl border border-border/60">
                  {data.byAsset.slice(0, 15).map((item, idx) => (
                    <div key={`${item.label}-${idx}`} className="flex items-center justify-between gap-3 px-3.5 py-2.5 text-sm">
                      <div className="min-w-0">
                        <p className="truncate font-medium text-foreground">{item.label}</p>
                        <p className="text-xs text-muted-foreground">
                          {item.kind} · {t('income.payments', { count: item.paymentCount })}
                        </p>
                      </div>
                      <span className="font-mono font-semibold tabular-nums text-foreground">
                        {money.amount(item.totalEur, 'EUR')}
                      </span>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          )}
        </>
      )}
    </div>
  )
}
