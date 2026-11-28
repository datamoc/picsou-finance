import { useContext, useState } from 'react'
import { QueryClientContext, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash, PencilSimple } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { useAccounts } from '@/features/accounts/hooks'
import { useMoney } from '@/hooks/use-money'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Progress } from '@/components/ui/progress'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { cn, formatPercent, parseAmount } from '@/lib/utils'

interface LimitPreset {
  envelopeKind: string
  label: string
  limitAmount: number
  periodKind: 'ANNUAL' | 'LIFETIME'
  matchingAccountTypes: string[]
}

interface ContributionLimitView {
  id: number
  name: string
  envelopeKind: string
  limitAmount: number
  periodKind: 'ANNUAL' | 'LIFETIME'
  periodYear: number | null
  currency: string
  contributedAmount: number
  remainingAmount: number
  usagePercent: number
  status: 'OK' | 'NEAR_LIMIT' | 'EXCEEDED'
  accounts: Array<{ id: number; name: string; accountType: string; contributedAmount: number }>
}

export function ContributionLimitsSection() {
  const queryClient = useContext(QueryClientContext)
  if (!queryClient) return null
  return <ContributionLimitsSectionInner />
}

function ContributionLimitsSectionInner() {
  const { t } = useTranslation()
  const money = useMoney()
  const queryClient = useQueryClient()
  const { data: accounts = [] } = useAccounts()

  const { data: limits = [] } = useQuery({
    queryKey: ['contribution-limits'],
    queryFn: async () => (await api.get<ContributionLimitView[]>('/contribution-limits')).data,
  })

  const { data: presets = [] } = useQuery({
    queryKey: ['contribution-limits', 'presets'],
    queryFn: async () => (await api.get<LimitPreset[]>('/contribution-limits/presets')).data,
  })

  const [open, setOpen] = useState(false)
  const [editing, setEditing] = useState<ContributionLimitView | null>(null)
  const [name, setName] = useState('')
  const [envelopeKind, setEnvelopeKind] = useState('CUSTOM')
  const [limitAmount, setLimitAmount] = useState('')
  const [periodKind, setPeriodKind] = useState<'ANNUAL' | 'LIFETIME'>('LIFETIME')
  const [selectedAccountIds, setSelectedAccountIds] = useState<number[]>([])

  const saveMutation = useMutation({
    mutationFn: async () => {
      const payload = {
        name: name.trim(),
        envelopeKind,
        limitAmount: parseAmount(limitAmount),
        periodKind,
        periodYear: periodKind === 'ANNUAL' ? new Date().getFullYear() : null,
        currency: 'EUR',
        accountIds: selectedAccountIds,
      }
      if (editing) {
        await api.put(`/contribution-limits/${editing.id}`, payload)
      } else {
        await api.post('/contribution-limits', payload)
      }
    },
    onSuccess: () => {
      setOpen(false)
      setEditing(null)
      void queryClient.invalidateQueries({ queryKey: ['contribution-limits'] })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: async (id: number) => {
      await api.delete(`/contribution-limits/${id}`)
    },
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['contribution-limits'] }),
  })

  function openNew(preset?: LimitPreset) {
    setEditing(null)
    if (preset) {
      setName(preset.label)
      setEnvelopeKind(preset.envelopeKind)
      setLimitAmount(String(preset.limitAmount))
      setPeriodKind(preset.periodKind)
      const matched = accounts
        .filter((a) => preset.matchingAccountTypes.includes(a.type))
        .map((a) => a.id)
      setSelectedAccountIds(matched)
    } else {
      setName('')
      setEnvelopeKind('CUSTOM')
      setLimitAmount('')
      setPeriodKind('LIFETIME')
      setSelectedAccountIds([])
    }
    setOpen(true)
  }

  function openEdit(item: ContributionLimitView) {
    setEditing(item)
    setName(item.name)
    setEnvelopeKind(item.envelopeKind)
    setLimitAmount(String(item.limitAmount))
    setPeriodKind(item.periodKind)
    setSelectedAccountIds(item.accounts.map((a) => a.id))
    setOpen(true)
  }

  function toggleAccount(id: number) {
    setSelectedAccountIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    )
  }

  const parsedLimit = parseAmount(limitAmount)
  const canSubmit = name.trim().length > 0 && Number.isFinite(parsedLimit) && parsedLimit > 0

  return (
    <Card className="rounded-xl bg-card">
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-3">
        <div className="space-y-1">
          <CardTitle className="text-base font-semibold">{t('limits.title')}</CardTitle>
          <CardDescription>{t('limits.description')}</CardDescription>
        </div>
        <Button type="button" size="sm" variant="outline" onClick={() => openNew()}>
          <Plus className="size-4" aria-hidden="true" />
          {t('limits.add')}
        </Button>
      </CardHeader>
      <CardContent className="space-y-4">
        {presets.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-xs text-muted-foreground mr-1">{t('limits.presetsLabel')}</span>
            {presets.map((p) => (
              <Button
                key={p.envelopeKind}
                type="button"
                size="sm"
                variant="outline"
                onClick={() => openNew(p)}
              >
                {p.label}
              </Button>
            ))}
          </div>
        )}

        {limits.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('limits.empty')}</p>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2">
            {limits.map((item) => (
              <div
                key={item.id}
                className="space-y-2.5 rounded-xl border border-border/60 p-4"
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="space-y-0.5 min-w-0">
                    <div className="flex items-center gap-2">
                      <p className="truncate text-sm font-semibold text-foreground">{item.name}</p>
                      <Badge
                        variant={item.status === 'EXCEEDED' ? 'destructive' : item.status === 'NEAR_LIMIT' ? 'secondary' : 'outline'}
                      >
                        {t(`limits.status.${item.status}`)}
                      </Badge>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      {item.periodKind === 'ANNUAL'
                        ? t('limits.periodAnnual', { year: item.periodYear ?? new Date().getFullYear() })
                        : t('limits.periodLifetime')}
                    </p>
                  </div>
                  <div className="flex items-center gap-1 shrink-0">
                    <Button
                      type="button"
                      size="icon-sm"
                      variant="ghost"
                      onClick={() => openEdit(item)}
                      aria-label={t('common.edit')}
                    >
                      <PencilSimple className="size-4" aria-hidden="true" />
                    </Button>
                    <Button
                      type="button"
                      size="icon-sm"
                      variant="ghost"
                      className="text-muted-foreground hover:text-destructive"
                      onClick={() => deleteMutation.mutate(item.id)}
                      aria-label={t('common.delete')}
                    >
                      <Trash className="size-4" aria-hidden="true" />
                    </Button>
                  </div>
                </div>

                <Progress value={Math.min(100, item.usagePercent)} />

                <div className="flex items-center justify-between text-xs tabular-nums">
                  <span className="font-medium text-foreground">
                    {money.amount(item.contributedAmount, item.currency)} / {money.amount(item.limitAmount, item.currency)}
                  </span>
                  <span className="text-muted-foreground">
                    {formatPercent(item.usagePercent)}
                  </span>
                </div>

                <p className="text-xs text-muted-foreground tabular-nums">
                  {t('limits.remaining')}:{' '}
                  <span className="font-medium text-foreground">
                    {money.amount(item.remainingAmount, item.currency)}
                  </span>
                </p>
              </div>
            ))}
          </div>
        )}
      </CardContent>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editing ? t('limits.editTitle') : t('limits.createTitle')}</DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="limit-name">{t('limits.fields.name')}</Label>
              <Input id="limit-name" value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="limit-amount">{t('limits.fields.amount')}</Label>
              <Input
                id="limit-amount"
                inputMode="decimal"
                value={limitAmount}
                onChange={(e) => setLimitAmount(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label>{t('limits.fields.period')}</Label>
              <div className="flex gap-2">
                {(['LIFETIME', 'ANNUAL'] as const).map((pk) => (
                  <Button
                    key={pk}
                    type="button"
                    size="sm"
                    variant={periodKind === pk ? 'default' : 'outline'}
                    onClick={() => setPeriodKind(pk)}
                  >
                    {pk === 'ANNUAL' ? t('limits.annual') : t('limits.lifetime')}
                  </Button>
                ))}
              </div>
            </div>
            <div className="space-y-1.5">
              <Label>{t('limits.fields.accounts')}</Label>
              <div className="max-h-44 space-y-1.5 overflow-y-auto rounded-lg border border-border/60 p-2">
                {accounts.map((acc) => {
                  const checked = selectedAccountIds.includes(acc.id)
                  return (
                    <button
                      key={acc.id}
                      type="button"
                      onClick={() => toggleAccount(acc.id)}
                      className={cn(
                        'flex w-full items-center justify-between rounded-md px-2.5 py-1.5 text-left text-xs transition-colors',
                        checked
                          ? 'bg-primary/10 text-foreground font-medium'
                          : 'text-muted-foreground hover:bg-muted/60',
                      )}
                    >
                      <span className="truncate">{acc.name}</span>
                      <span className="ml-2 shrink-0 text-[11px]">{acc.type}</span>
                    </button>
                  )
                })}
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button
              type="button"
              disabled={!canSubmit || saveMutation.isPending}
              onClick={() => saveMutation.mutate()}
            >
              {t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
