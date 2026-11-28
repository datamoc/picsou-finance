import { useMemo, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { CartesianGrid, Line, LineChart, XAxis, YAxis } from 'recharts'
import { api } from '@/lib/api-client'
import { useMoney } from '@/hooks/use-money'
import { signedAmountClass } from '@/lib/money'
import { cn, formatDate, formatPercent } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { ChartContainer, ChartTooltip, type ChartConfig } from '@/components/ui/chart'
import { ExportSvgButton } from '@/components/shared/ExportSvgButton'
import { AXIS_PROPS } from '@/lib/chart-axis'

interface BenchmarkOption {
  id: string
  ticker: string
  label: string
}

interface PerformancePoint {
  date: string
  valueEur: number
  twrIndex: number
  twrPct: number
  benchmarkIndex: number | null
  benchmarkPct: number | null
}

interface PerformanceSummary {
  range: string
  startValueEur: number
  endValueEur: number
  netContributionsEur: number
  totalGainEur: number
  twrPct: number
  annualizedTwrPct: number | null
  mwrPct: number
  maxDrawdownPct: number
  volatilityPct: number | null
  transactionModeAccounts: number
  snapshotModeAccounts: number
  benchmarkId: string | null
  benchmarkLabel: string | null
  benchmarkReturnPct: number | null
  series: PerformancePoint[]
}

const RANGES = ['1M', '3M', 'YTD', '1Y', '3Y', '5Y', 'ALL'] as const

export function PerformanceSection({ portfolioId }: { portfolioId?: number | null }) {
  const { t } = useTranslation()
  const money = useMoney()
  const chartRef = useRef<HTMLDivElement>(null)
  const [range, setRange] = useState<(typeof RANGES)[number]>('1Y')
  const [benchmark, setBenchmark] = useState<string>('MSCI_WORLD')

  const { data: benchmarks = [] } = useQuery({
    queryKey: ['wealth', 'benchmarks'],
    queryFn: async () => (await api.get<BenchmarkOption[]>('/wealth/benchmarks')).data,
  })

  const { data } = useQuery({
    queryKey: ['wealth', 'performance', range, benchmark, portfolioId ?? null],
    queryFn: async () => {
      const params = new URLSearchParams({ range })
      if (benchmark) params.set('benchmark', benchmark)
      if (portfolioId != null) params.set('portfolioId', String(portfolioId))
      return (await api.get<PerformanceSummary>(`/wealth/performance?${params.toString()}`)).data
    },
  })

  const chartConfig = useMemo(
    () =>
      ({
        twrIndex: {
          label: t('performance.portfolioSeries'),
          color: 'var(--chart-1)',
        },
        benchmarkIndex: {
          label: data?.benchmarkLabel ?? t('performance.benchmarkSeries'),
          color: 'var(--chart-5)',
        },
      }) satisfies ChartConfig,
    [t, data?.benchmarkLabel],
  )

  const hasBenchmarkSeries = Boolean(data?.series.some((p) => p.benchmarkIndex != null))

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-1 rounded-lg border border-border/60 bg-background/80 p-1">
          {RANGES.map((r) => (
            <button
              key={r}
              type="button"
              onClick={() => setRange(r)}
              className={cn(
                'h-7 rounded-md px-2.5 text-xs font-medium transition-colors',
                range === r
                  ? 'bg-primary text-primary-foreground'
                  : 'text-muted-foreground hover:text-foreground',
              )}
            >
              {r}
            </button>
          ))}
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <Button
            type="button"
            size="sm"
            variant={benchmark === '' ? 'default' : 'outline'}
            onClick={() => setBenchmark('')}
          >
            {t('performance.noBenchmark')}
          </Button>
          {benchmarks.map((b) => (
            <Button
              key={b.id}
              type="button"
              size="sm"
              variant={benchmark === b.id ? 'default' : 'outline'}
              onClick={() => setBenchmark(b.id)}
            >
              {b.label}
            </Button>
          ))}
        </div>
      </div>

      {data && (
        <>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('performance.twr')}</CardDescription>
                <CardTitle className={cn('text-xl font-bold tabular-nums', signedAmountClass(data.twrPct))}>
                  {formatPercent(data.twrPct, { signed: true })}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground tabular-nums">
                  {data.annualizedTwrPct != null
                    ? t('performance.annualized', { value: formatPercent(data.annualizedTwrPct, { signed: true }) })
                    : t('performance.periodReturn')}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('performance.mwr')}</CardDescription>
                <CardTitle className={cn('text-xl font-bold tabular-nums', signedAmountClass(data.mwrPct))}>
                  {formatPercent(data.mwrPct, { signed: true })}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground">{t('performance.mwrHint')}</p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('performance.totalGain')}</CardDescription>
                <CardTitle className={cn('text-xl font-bold tabular-nums', signedAmountClass(data.totalGainEur))}>
                  {data.totalGainEur > 0 ? '+' : ''}{money.amount(data.totalGainEur, 'EUR')}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground tabular-nums">
                  {t('performance.netFlows')}: {money.amount(data.netContributionsEur, 'EUR')}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('performance.maxDrawdown')}</CardDescription>
                <CardTitle className="text-xl font-bold tabular-nums text-destructive">
                  -{formatPercent(data.maxDrawdownPct)}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground tabular-nums">
                  {data.volatilityPct != null
                    ? t('performance.volatility', { value: formatPercent(data.volatilityPct) })
                    : t('performance.insufficientVol')}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-1">
                <CardDescription>{t('performance.trackingModes')}</CardDescription>
                <CardTitle className="text-base font-semibold tabular-nums">
                  {data.transactionModeAccounts + data.snapshotModeAccounts}
                </CardTitle>
              </CardHeader>
              <CardContent className="flex flex-wrap gap-1.5">
                <Badge variant="outline">
                  {t('performance.txMode', { count: data.transactionModeAccounts })}
                </Badge>
                <Badge variant="secondary">
                  {t('performance.snapshotMode', { count: data.snapshotModeAccounts })}
                </Badge>
              </CardContent>
            </Card>
          </div>

          <Card className="rounded-xl bg-card">
            <CardHeader className="flex flex-row items-center justify-between gap-2">
              <div>
                <CardTitle className="text-base font-semibold">{t('performance.chartTitle')}</CardTitle>
                <CardDescription>{t('performance.chartDesc')}</CardDescription>
              </div>
              <ExportSvgButton
                containerRef={chartRef}
                filename={`picsou-performance-${range.toLowerCase()}.svg`}
                title={t('performance.chartTitle')}
              />
            </CardHeader>
            <CardContent>
              {data.series.length < 2 ? (
                <p className="py-8 text-center text-sm text-muted-foreground">
                  {t('performance.empty')}
                </p>
              ) : (
                <div ref={chartRef}>
                  <ChartContainer config={chartConfig} className="h-[280px] w-full">
                    <LineChart data={data.series} margin={{ top: 10, right: 16, left: 4, bottom: 0 }}>
                      <CartesianGrid strokeDasharray="3 3" vertical={false} />
                      <XAxis
                        dataKey="date"
                        {...AXIS_PROPS}
                        tickFormatter={(d: string) => formatDate(d)}
                      />
                      <YAxis
                        {...AXIS_PROPS}
                        width={56}
                        tickFormatter={(v: number) => `${Math.round(v)}`}
                      />
                      <ChartTooltip />
                      <Line
                        type="monotone"
                        dataKey="twrIndex"
                        stroke="var(--color-twrIndex)"
                        strokeWidth={2}
                        dot={false}
                      />
                      {hasBenchmarkSeries && (
                        <Line
                          type="monotone"
                          dataKey="benchmarkIndex"
                          stroke="var(--color-benchmarkIndex)"
                          strokeWidth={1.75}
                          strokeDasharray="5 4"
                          dot={false}
                        />
                      )}
                    </LineChart>
                  </ChartContainer>
                </div>
              )}
            </CardContent>
          </Card>
        </>
      )}
    </div>
  )
}
