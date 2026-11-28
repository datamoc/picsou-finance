import { useContext, useState } from 'react'
import { QueryClientContext, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'
import { cn, formatDate, parseAmount } from '@/lib/utils'

type CostBasisMethod = 'AVERAGE' | 'FIFO' | 'LIFO' | 'HIFO'

interface StockSplitDto {
  id: number
  ticker: string
  splitDate: string
  ratioNumerator: number
  ratioDenominator: number
  note: string | null
}

const METHODS: CostBasisMethod[] = ['AVERAGE', 'FIFO', 'LIFO', 'HIFO']

export function CostBasisCard(props: {
  accountId: number
  initialMethod?: CostBasisMethod
}) {
  const qc = useContext(QueryClientContext)
  if (!qc) return null
  return <CostBasisCardInner {...props} />
}

function CostBasisCardInner({
  accountId,
  initialMethod = 'AVERAGE',
}: {
  accountId: number
  initialMethod?: CostBasisMethod
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [method, setMethod] = useState<CostBasisMethod>(initialMethod)
  const [ticker, setTicker] = useState('')
  const [splitDate, setSplitDate] = useState('')
  const [ratioNum, setRatioNum] = useState('2')
  const [ratioDen, setRatioDen] = useState('1')

  const { data: splits = [] } = useQuery({
    queryKey: ['stock-splits'],
    queryFn: async () => (await api.get<StockSplitDto[]>('/stock-splits')).data,
  })

  const updateMethod = useMutation({
    mutationFn: async (next: CostBasisMethod) => {
      await api.patch(`/accounts/${accountId}/cost-basis-method`, { method: next })
      return next
    },
    onSuccess: (next) => {
      setMethod(next)
      void queryClient.invalidateQueries({ queryKey: ['accounts', accountId] })
    },
  })

  const addSplit = useMutation({
    mutationFn: async () => {
      await api.post('/stock-splits', {
        ticker: ticker.trim().toUpperCase(),
        splitDate,
        ratioNumerator: parseAmount(ratioNum),
        ratioDenominator: parseAmount(ratioDen),
      })
    },
    onSuccess: () => {
      setTicker('')
      setSplitDate('')
      setRatioNum('2')
      setRatioDen('1')
      void queryClient.invalidateQueries({ queryKey: ['stock-splits'] })
      void queryClient.invalidateQueries({ queryKey: ['accounts', accountId] })
    },
  })

  const deleteSplit = useMutation({
    mutationFn: async (id: number) => {
      await api.delete(`/stock-splits/${id}`)
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['stock-splits'] })
      void queryClient.invalidateQueries({ queryKey: ['accounts', accountId] })
    },
  })

  return (
    <Card className="rounded-xl bg-card">
      <CardHeader className="pb-3">
        <CardTitle className="text-base font-semibold">{t('costBasis.title')}</CardTitle>
        <CardDescription>{t('costBasis.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <div className="space-y-2">
          <Label>{t('costBasis.methodLabel')}</Label>
          <div className="flex flex-wrap gap-1.5">
            {METHODS.map((m) => (
              <button
                key={m}
                type="button"
                disabled={updateMethod.isPending}
                onClick={() => updateMethod.mutate(m)}
                className={cn(
                  'h-8 rounded-md border px-3 text-xs font-medium transition-colors',
                  method === m
                    ? 'border-primary bg-primary text-primary-foreground'
                    : 'border-border/60 bg-background text-muted-foreground hover:text-foreground',
                )}
              >
                {t(`costBasis.methods.${m}`)}
              </button>
            ))}
          </div>
        </div>

        <div className="space-y-3 border-t border-border/60 pt-4">
          <p className="text-sm font-medium text-foreground">{t('costBasis.splitsTitle')}</p>
          {splits.length > 0 && (
            <div className="space-y-1.5">
              {splits.map((s) => (
                <div
                  key={s.id}
                  className="flex items-center justify-between gap-2 rounded-lg border border-border/60 px-3 py-2 text-xs"
                >
                  <div className="flex items-center gap-2">
                    <Badge variant="outline">{s.ticker}</Badge>
                    <span className="tabular-nums font-medium">
                      {s.ratioNumerator}:{s.ratioDenominator}
                    </span>
                    <span className="text-muted-foreground">{formatDate(s.splitDate)}</span>
                  </div>
                  <Button
                    type="button"
                    size="icon-sm"
                    variant="ghost"
                    className="size-7 text-muted-foreground hover:text-destructive"
                    onClick={() => deleteSplit.mutate(s.id)}
                    aria-label={t('common.delete')}
                  >
                    <Trash className="size-3.5" aria-hidden="true" />
                  </Button>
                </div>
              ))}
            </div>
          )}

          <form
            className="grid gap-2 sm:grid-cols-5"
            onSubmit={(e) => {
              e.preventDefault()
              if (ticker.trim() && splitDate) addSplit.mutate()
            }}
          >
            <Input
              value={ticker}
              onChange={(e) => setTicker(e.target.value)}
              placeholder="NVDA"
              className="h-8 text-xs"
            />
            <Input
              type="date"
              value={splitDate}
              onChange={(e) => setSplitDate(e.target.value)}
              className="h-8 text-xs"
            />
            <Input
              inputMode="decimal"
              value={ratioNum}
              onChange={(e) => setRatioNum(e.target.value)}
              placeholder="10"
              className="h-8 text-xs"
            />
            <Input
              inputMode="decimal"
              value={ratioDen}
              onChange={(e) => setRatioDen(e.target.value)}
              placeholder="1"
              className="h-8 text-xs"
            />
            <Button
              type="submit"
              size="sm"
              variant="outline"
              disabled={!ticker.trim() || !splitDate || addSplit.isPending}
            >
              <Plus className="size-3.5" aria-hidden="true" />
              {t('costBasis.addSplit')}
            </Button>
          </form>
        </div>
      </CardContent>
    </Card>
  )
}
