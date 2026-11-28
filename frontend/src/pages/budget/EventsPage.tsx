import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash, PencilSimple, CalendarBlank } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { useMoney } from '@/hooks/use-money'
import { formatDate, formatPercent, parseAmount } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import { Badge } from '@/components/ui/badge'
import { EmptyState } from '@/components/shared/EmptyState'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

interface SpendingEventView {
  id: number
  name: string
  description: string | null
  startDate: string | null
  endDate: string | null
  budgetAmount: number | null
  color: string
  spentAmount: number
  transactionCount: number
  usagePercent: number | null
}

export function EventsPage() {
  const { t } = useTranslation()
  const money = useMoney()
  const queryClient = useQueryClient()

  const { data: events = [] } = useQuery({
    queryKey: ['budget', 'events'],
    queryFn: async () => (await api.get<SpendingEventView[]>('/budget/events')).data,
  })

  const [open, setOpen] = useState(false)
  const [editing, setEditing] = useState<SpendingEventView | null>(null)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [startDate, setStartDate] = useState('')
  const [endDate, setEndDate] = useState('')
  const [budgetAmount, setBudgetAmount] = useState('')

  const saveMutation = useMutation({
    mutationFn: async () => {
      const parsedBudget = budgetAmount.trim() ? parseAmount(budgetAmount) : null
      const payload = {
        name: name.trim(),
        description: description.trim() || null,
        startDate: startDate || null,
        endDate: endDate || null,
        budgetAmount: parsedBudget != null && Number.isFinite(parsedBudget) ? parsedBudget : null,
        color: '#3b82f6',
      }
      if (editing) {
        await api.put(`/budget/events/${editing.id}`, payload)
      } else {
        await api.post('/budget/events', payload)
      }
    },
    onSuccess: () => {
      setOpen(false)
      setEditing(null)
      void queryClient.invalidateQueries({ queryKey: ['budget', 'events'] })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: async (id: number) => {
      await api.delete(`/budget/events/${id}`)
    },
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['budget', 'events'] }),
  })

  function openNew() {
    setEditing(null)
    setName('')
    setDescription('')
    setStartDate('')
    setEndDate('')
    setBudgetAmount('')
    setOpen(true)
  }

  function openEdit(ev: SpendingEventView) {
    setEditing(ev)
    setName(ev.name)
    setDescription(ev.description ?? '')
    setStartDate(ev.startDate ?? '')
    setEndDate(ev.endDate ?? '')
    setBudgetAmount(ev.budgetAmount != null ? String(ev.budgetAmount) : '')
    setOpen(true)
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h2 className="text-base font-semibold text-foreground">{t('budgetEvents.title')}</h2>
          <p className="text-sm text-muted-foreground">{t('budgetEvents.description')}</p>
        </div>
        <Button type="button" size="sm" onClick={openNew}>
          <Plus className="size-4" aria-hidden="true" />
          {t('budgetEvents.create')}
        </Button>
      </div>

      {events.length === 0 ? (
        <Card className="rounded-xl bg-card">
          <CardContent>
            <EmptyState
              className="py-8"
              icon={<CalendarBlank className="size-10" />}
              title={t('budgetEvents.emptyTitle')}
              description={t('budgetEvents.emptyDesc')}
            />
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2">
          {events.map((ev) => (
            <Card key={ev.id} className="rounded-xl bg-card">
              <CardHeader className="flex flex-row items-start justify-between gap-2 pb-2">
                <div className="space-y-0.5 min-w-0">
                  <CardTitle className="text-base font-semibold">{ev.name}</CardTitle>
                  {ev.description && <CardDescription>{ev.description}</CardDescription>}
                  {(ev.startDate || ev.endDate) && (
                    <p className="text-xs text-muted-foreground">
                      {ev.startDate ? formatDate(ev.startDate) : ''}
                      {ev.startDate && ev.endDate ? ' / ' : ''}
                      {ev.endDate ? formatDate(ev.endDate) : ''}
                    </p>
                  )}
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  <Button type="button" size="icon-sm" variant="ghost" onClick={() => openEdit(ev)} aria-label={t('common.edit')}>
                    <PencilSimple className="size-4" aria-hidden="true" />
                  </Button>
                  <Button
                    type="button"
                    size="icon-sm"
                    variant="ghost"
                    className="text-muted-foreground hover:text-destructive"
                    onClick={() => deleteMutation.mutate(ev.id)}
                    aria-label={t('common.delete')}
                  >
                    <Trash className="size-4" aria-hidden="true" />
                  </Button>
                </div>
              </CardHeader>
              <CardContent className="space-y-3">
                <div className="flex items-baseline justify-between gap-2 tabular-nums">
                  <span className="text-xl font-bold text-foreground">
                    {money.amount(ev.spentAmount, 'EUR')}
                  </span>
                  {ev.budgetAmount != null && (
                    <span className="text-xs text-muted-foreground">
                      / {money.amount(ev.budgetAmount, 'EUR')}
                    </span>
                  )}
                </div>
                {ev.usagePercent != null && (
                  <Progress value={Math.min(100, ev.usagePercent)} />
                )}
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <Badge variant="outline">
                    {t('budgetEvents.txCount', { count: ev.transactionCount })}
                  </Badge>
                  {ev.usagePercent != null && (
                    <span className="tabular-nums">{formatPercent(ev.usagePercent)}</span>
                  )}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editing ? t('budgetEvents.editTitle') : t('budgetEvents.createTitle')}</DialogTitle>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1">
              <Label htmlFor="ev-name">{t('budgetEvents.fields.name')}</Label>
              <Input id="ev-name" value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="space-y-1">
              <Label htmlFor="ev-desc">{t('budgetEvents.fields.description')}</Label>
              <Input id="ev-desc" value={description} onChange={(e) => setDescription(e.target.value)} />
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-1">
                <Label htmlFor="ev-start">{t('budgetEvents.fields.startDate')}</Label>
                <Input id="ev-start" type="date" value={startDate} onChange={(e) => setStartDate(e.target.value)} />
              </div>
              <div className="space-y-1">
                <Label htmlFor="ev-end">{t('budgetEvents.fields.endDate')}</Label>
                <Input id="ev-end" type="date" value={endDate} onChange={(e) => setEndDate(e.target.value)} />
              </div>
            </div>
            <div className="space-y-1">
              <Label htmlFor="ev-budget">{t('budgetEvents.fields.budget')}</Label>
              <Input id="ev-budget" inputMode="decimal" value={budgetAmount} onChange={(e) => setBudgetAmount(e.target.value)} />
            </div>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button type="button" disabled={!name.trim() || saveMutation.isPending} onClick={() => saveMutation.mutate()}>
              {t('common.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
