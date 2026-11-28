import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash, PencilSimple, ChartLineUp } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { useAccounts } from '@/features/accounts/hooks'
import { useMoney } from '@/hooks/use-money'
import { cn } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

interface VirtualPortfolioView {
  id: number
  name: string
  description: string | null
  color: string
  totalValueEur: number
  accountCount: number
  accounts: Array<{ id: number; name: string; accountType: string; balanceEur: number }>
}

export function VirtualPortfoliosSection({
  onInspectPerformance,
}: {
  onInspectPerformance?: (portfolioId: number) => void
}) {
  const { t } = useTranslation()
  const money = useMoney()
  const queryClient = useQueryClient()
  const { data: accounts = [] } = useAccounts()

  const { data: portfolios = [] } = useQuery({
    queryKey: ['wealth', 'portfolios'],
    queryFn: async () => (await api.get<VirtualPortfolioView[]>('/portfolios')).data,
  })

  const [open, setOpen] = useState(false)
  const [editing, setEditing] = useState<VirtualPortfolioView | null>(null)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [accountIds, setAccountIds] = useState<number[]>([])

  const saveMutation = useMutation({
    mutationFn: async () => {
      const payload = {
        name: name.trim(),
        description: description.trim() || null,
        color: '#3b82f6',
        accountIds,
      }
      if (editing) {
        await api.put(`/portfolios/${editing.id}`, payload)
      } else {
        await api.post('/portfolios', payload)
      }
    },
    onSuccess: () => {
      setOpen(false)
      setEditing(null)
      void queryClient.invalidateQueries({ queryKey: ['wealth', 'portfolios'] })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: async (id: number) => {
      await api.delete(`/portfolios/${id}`)
    },
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['wealth', 'portfolios'] }),
  })

  function openNew() {
    setEditing(null)
    setName('')
    setDescription('')
    setAccountIds([])
    setOpen(true)
  }

  function openEdit(vp: VirtualPortfolioView) {
    setEditing(vp)
    setName(vp.name)
    setDescription(vp.description ?? '')
    setAccountIds(vp.accounts.map((a) => a.id))
    setOpen(true)
  }

  function toggleAccount(id: number) {
    setAccountIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]))
  }

  return (
    <Card className="rounded-xl bg-card">
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-3">
        <div>
          <CardTitle className="text-base font-semibold">{t('portfolios.title')}</CardTitle>
          <CardDescription>{t('portfolios.description')}</CardDescription>
        </div>
        <Button type="button" size="sm" variant="outline" onClick={openNew}>
          <Plus className="size-4" aria-hidden="true" />
          {t('portfolios.create')}
        </Button>
      </CardHeader>
      <CardContent>
        {portfolios.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('portfolios.empty')}</p>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {portfolios.map((vp) => (
              <div
                key={vp.id}
                className="flex flex-col justify-between space-y-3 rounded-xl border border-border/60 p-4"
              >
                <div className="space-y-1">
                  <div className="flex items-start justify-between gap-2">
                    <p className="font-semibold text-foreground">{vp.name}</p>
                    <div className="flex items-center gap-1 shrink-0">
                      <Button
                        type="button"
                        size="icon-sm"
                        variant="ghost"
                        onClick={() => openEdit(vp)}
                        aria-label={t('common.edit')}
                      >
                        <PencilSimple className="size-4" aria-hidden="true" />
                      </Button>
                      <Button
                        type="button"
                        size="icon-sm"
                        variant="ghost"
                        className="text-muted-foreground hover:text-destructive"
                        onClick={() => deleteMutation.mutate(vp.id)}
                        aria-label={t('common.delete')}
                      >
                        <Trash className="size-4" aria-hidden="true" />
                      </Button>
                    </div>
                  </div>
                  {vp.description && (
                    <p className="text-xs text-muted-foreground">{vp.description}</p>
                  )}
                  <p className="text-xl font-bold tabular-nums text-foreground">
                    {money.amount(vp.totalValueEur, 'EUR')}
                  </p>
                </div>

                <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border/60 pt-2.5">
                  <Badge variant="outline">
                    {t('portfolios.accountCount', { count: vp.accountCount })}
                  </Badge>
                  {onInspectPerformance && (
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      onClick={() => onInspectPerformance(vp.id)}
                    >
                      <ChartLineUp className="size-3.5" aria-hidden="true" />
                      {t('portfolios.viewPerformance')}
                    </Button>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </CardContent>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editing ? t('portfolios.editTitle') : t('portfolios.createTitle')}</DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="vp-name">{t('portfolios.fields.name')}</Label>
              <Input id="vp-name" value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="vp-desc">{t('portfolios.fields.description')}</Label>
              <Input id="vp-desc" value={description} onChange={(e) => setDescription(e.target.value)} />
            </div>
            <div className="space-y-1.5">
              <Label>{t('portfolios.fields.accounts')}</Label>
              <div className="max-h-48 space-y-1.5 overflow-y-auto rounded-lg border border-border/60 p-2">
                {accounts.map((acc) => {
                  const checked = accountIds.includes(acc.id)
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
              disabled={!name.trim() || saveMutation.isPending}
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
