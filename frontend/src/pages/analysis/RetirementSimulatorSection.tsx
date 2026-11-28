import { useMemo, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Area, AreaChart, CartesianGrid, ReferenceLine, XAxis, YAxis } from 'recharts'
import { api } from '@/lib/api-client'
import { useMoney } from '@/hooks/use-money'
import { formatPercent, parseAmount } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'
import { ChartContainer, ChartTooltip, type ChartConfig } from '@/components/ui/chart'
import { ExportSvgButton } from '@/components/shared/ExportSvgButton'
import { AXIS_PROPS } from '@/lib/chart-axis'

interface RetirementYearPoint {
  age: number
  year: number
  phase: 'ACCUMULATION' | 'RETIREMENT'
  balanceNominalEur: number
  balanceRealEur: number
  annualContributionEur: number
  annualWithdrawalEur: number
}

interface RetirementSimulationResponse {
  fiNumberEur: number
  coastFireNumberEur: number
  projectedAtRetirementEur: number
  projectedRealAtRetirementEur: number
  yearsToFi: number | null
  fiAge: number | null
  sustainableMonthlyWithdrawalEur: number
  depletionAge: number | null
  fundedRatioPct: number
  points: RetirementYearPoint[]
}

interface SaveUpResponse {
  monthsRequired: number | null
  yearsRequired: number | null
  totalContributedEur: number
  totalGrowthEur: number
  finalBalanceEur: number
}

function num(raw: string, fallback: number): number {
  const v = parseAmount(raw)
  return Number.isFinite(v) ? v : fallback
}

export function RetirementSimulatorSection() {
  const { t } = useTranslation()
  const money = useMoney()
  const chartRef = useRef<HTMLDivElement>(null)

  const [currentAge, setCurrentAge] = useState('35')
  const [retirementAge, setRetirementAge] = useState('60')
  const [lifeExpectancy, setLifeExpectancy] = useState('90')
  const [currentSavings, setCurrentSavings] = useState('80000')
  const [monthlyContribution, setMonthlyContribution] = useState('1200')
  const [annualReturn, setAnnualReturn] = useState('6.5')
  const [inflationRate, setInflationRate] = useState('2.0')
  const [monthlyRetirementSpend, setMonthlyRetirementSpend] = useState('2800')
  const [swr, setSwr] = useState('4.0')

  // Save-up calculator inputs
  const [saveTarget, setSaveTarget] = useState('50000')
  const [saveCurrent, setSaveCurrent] = useState('10000')
  const [saveMonthly, setSaveMonthly] = useState('600')
  const [saveReturn, setSaveReturn] = useState('4.5')

  const retirementPayload = useMemo(
    () => ({
      currentAge: Math.max(18, Math.min(95, Math.round(num(currentAge, 35)))),
      retirementAge: Math.max(25, Math.min(100, Math.round(num(retirementAge, 60)))),
      lifeExpectancy: Math.max(50, Math.min(115, Math.round(num(lifeExpectancy, 90)))),
      currentSavingsEur: Math.max(0, num(currentSavings, 80000)),
      monthlyContributionEur: Math.max(0, num(monthlyContribution, 1200)),
      expectedAnnualReturnPct: Math.max(-5, Math.min(25, num(annualReturn, 6.5))),
      inflationPct: Math.max(0, Math.min(15, num(inflationRate, 2.0))),
      monthlyRetirementSpendEur: Math.max(100, num(monthlyRetirementSpend, 2800)),
      safeWithdrawalRatePct: Math.max(1, Math.min(10, num(swr, 4.0))),
    }),
    [
      currentAge,
      retirementAge,
      lifeExpectancy,
      currentSavings,
      monthlyContribution,
      annualReturn,
      inflationRate,
      monthlyRetirementSpend,
      swr,
    ],
  )

  const { data: sim } = useQuery({
    queryKey: ['wealth', 'planning', 'retirement', retirementPayload],
    queryFn: async () =>
      (await api.post<RetirementSimulationResponse>('/wealth/planning/retirement', retirementPayload)).data,
  })

  const saveUpPayload = useMemo(
    () => ({
      targetAmountEur: Math.max(1, num(saveTarget, 50000)),
      currentSavingsEur: Math.max(0, num(saveCurrent, 10000)),
      monthlyContributionEur: Math.max(0, num(saveMonthly, 600)),
      expectedAnnualReturnPct: Math.max(0, Math.min(25, num(saveReturn, 4.5))),
    }),
    [saveTarget, saveCurrent, saveMonthly, saveReturn],
  )

  const { data: saveUp } = useQuery({
    queryKey: ['wealth', 'planning', 'save-up', saveUpPayload],
    queryFn: async () =>
      (await api.post<SaveUpResponse>('/wealth/planning/save-up', saveUpPayload)).data,
  })

  const chartConfig = useMemo(
    () =>
      ({
        balanceRealEur: {
          label: t('planning.realBalance'),
          color: 'var(--chart-1)',
        },
        balanceNominalEur: {
          label: t('planning.nominalBalance'),
          color: 'var(--chart-5)',
        },
      }) satisfies ChartConfig,
    [t],
  )

  return (
    <div className="space-y-6">
      <Card className="rounded-xl bg-card">
        <CardHeader>
          <CardTitle className="text-base font-semibold">{t('planning.fireTitle')}</CardTitle>
          <CardDescription>{t('planning.fireDesc')}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-5">
          <div className="grid gap-3 sm:grid-cols-3 lg:grid-cols-5">
            <div className="space-y-1">
              <Label htmlFor="fire-age">{t('planning.fields.currentAge')}</Label>
              <Input id="fire-age" inputMode="numeric" value={currentAge} onChange={(e) => setCurrentAge(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-ret-age">{t('planning.fields.retirementAge')}</Label>
              <Input id="fire-ret-age" inputMode="numeric" value={retirementAge} onChange={(e) => setRetirementAge(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-life">{t('planning.fields.lifeExpectancy')}</Label>
              <Input id="fire-life" inputMode="numeric" value={lifeExpectancy} onChange={(e) => setLifeExpectancy(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-savings">{t('planning.fields.currentSavings')}</Label>
              <Input id="fire-savings" inputMode="decimal" value={currentSavings} onChange={(e) => setCurrentSavings(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-monthly">{t('planning.fields.monthlyContribution')}</Label>
              <Input id="fire-monthly" inputMode="decimal" value={monthlyContribution} onChange={(e) => setMonthlyContribution(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-return">{t('planning.fields.expectedReturn')}</Label>
              <Input id="fire-return" inputMode="decimal" value={annualReturn} onChange={(e) => setAnnualReturn(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-inflation">{t('planning.fields.inflation')}</Label>
              <Input id="fire-inflation" inputMode="decimal" value={inflationRate} onChange={(e) => setInflationRate(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-spend">{t('planning.fields.monthlySpend')}</Label>
              <Input id="fire-spend" inputMode="decimal" value={monthlyRetirementSpend} onChange={(e) => setMonthlyRetirementSpend(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="fire-swr">{t('planning.fields.swr')}</Label>
              <Input id="fire-swr" inputMode="decimal" value={swr} onChange={(e) => setSwr(e.target.value)} />
            </div>
          </div>

          {sim && (
            <>
              <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('planning.kpi.fiNumber')}</p>
                  <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                    {money.amount(sim.fiNumberEur, 'EUR', { maximumFractionDigits: 0 })}
                  </p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {sim.fiAge != null
                      ? t('planning.kpi.fiAge', { age: sim.fiAge, years: sim.yearsToFi })
                      : t('planning.kpi.fiNotReached')}
                  </p>
                </div>

                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('planning.kpi.coastFire')}</p>
                  <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                    {money.amount(sim.coastFireNumberEur, 'EUR', { maximumFractionDigits: 0 })}
                  </p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {t('planning.kpi.fundedRatio', { pct: formatPercent(sim.fundedRatioPct) })}
                  </p>
                </div>

                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('planning.kpi.atRetirement')}</p>
                  <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                    {money.amount(sim.projectedRealAtRetirementEur, 'EUR', { maximumFractionDigits: 0 })}
                  </p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {t('planning.kpi.sustainableMonthly')}:{' '}
                    {money.amount(sim.sustainableMonthlyWithdrawalEur, 'EUR', { maximumFractionDigits: 0 })}
                  </p>
                </div>

                <div className="rounded-xl bg-muted/40 px-4 py-3">
                  <p className="text-xs text-muted-foreground">{t('planning.kpi.longevity')}</p>
                  <div className="mt-1 flex items-center gap-2">
                    <Badge variant={sim.depletionAge == null ? 'default' : 'destructive'}>
                      {sim.depletionAge == null
                        ? t('planning.kpi.perpetual')
                        : t('planning.kpi.depletedAt', { age: sim.depletionAge })}
                    </Badge>
                  </div>
                </div>
              </div>

              <div className="flex items-center justify-end">
                <ExportSvgButton
                  containerRef={chartRef}
                  filename="picsou-fire-simulation.svg"
                  title={t('planning.fireTitle')}
                />
              </div>

              <div ref={chartRef}>
                <ChartContainer config={chartConfig} className="h-[280px] w-full">
                  <AreaChart data={sim.points} margin={{ top: 10, right: 16, left: 4, bottom: 0 }}>
                    <CartesianGrid strokeDasharray="3 3" vertical={false} />
                    <XAxis dataKey="age" {...AXIS_PROPS} />
                    <YAxis {...AXIS_PROPS} tickFormatter={money.tick} width={84} />
                    <ChartTooltip />
                    <ReferenceLine
                      x={retirementPayload.retirementAge}
                      stroke="var(--muted-foreground)"
                      strokeDasharray="4 4"
                    />
                    <Area
                      type="monotone"
                      dataKey="balanceRealEur"
                      stroke="var(--color-balanceRealEur)"
                      fill="var(--color-balanceRealEur)"
                      fillOpacity={0.14}
                      strokeWidth={2}
                      dot={false}
                    />
                  </AreaChart>
                </ChartContainer>
              </div>
            </>
          )}
        </CardContent>
      </Card>

      <Card className="rounded-xl bg-card">
        <CardHeader>
          <CardTitle className="text-base font-semibold">{t('planning.saveUpTitle')}</CardTitle>
          <CardDescription>{t('planning.saveUpDesc')}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid gap-3 sm:grid-cols-4">
            <div className="space-y-1">
              <Label htmlFor="saveup-target">{t('planning.saveUp.target')}</Label>
              <Input id="saveup-target" inputMode="decimal" value={saveTarget} onChange={(e) => setSaveTarget(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="saveup-current">{t('planning.saveUp.current')}</Label>
              <Input id="saveup-current" inputMode="decimal" value={saveCurrent} onChange={(e) => setSaveCurrent(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="saveup-monthly">{t('planning.saveUp.monthly')}</Label>
              <Input id="saveup-monthly" inputMode="decimal" value={saveMonthly} onChange={(e) => setSaveMonthly(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="saveup-return">{t('planning.saveUp.returnPct')}</Label>
              <Input id="saveup-return" inputMode="decimal" value={saveReturn} onChange={(e) => setSaveReturn(e.target.value)} />
            </div>
          </div>

          {saveUp && (
            <div className="grid gap-3 sm:grid-cols-3">
              <div className="rounded-xl bg-muted/40 px-4 py-3">
                <p className="text-xs text-muted-foreground">{t('planning.saveUp.horizon')}</p>
                <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                  {saveUp.monthsRequired != null
                    ? t('planning.saveUp.monthsValue', { months: saveUp.monthsRequired, years: saveUp.yearsRequired })
                    : t('planning.saveUp.unreachable')}
                </p>
              </div>
              <div className="rounded-xl bg-muted/40 px-4 py-3">
                <p className="text-xs text-muted-foreground">{t('planning.saveUp.contributed')}</p>
                <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                  {money.amount(saveUp.totalContributedEur, 'EUR', { maximumFractionDigits: 0 })}
                </p>
              </div>
              <div className="rounded-xl bg-muted/40 px-4 py-3">
                <p className="text-xs text-muted-foreground">{t('planning.saveUp.growth')}</p>
                <p className="mt-1 text-lg font-bold tabular-nums text-foreground">
                  {money.amount(saveUp.totalGrowthEur, 'EUR', { maximumFractionDigits: 0 })}
                </p>
              </div>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
