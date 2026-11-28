import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { useMoney } from '@/hooks/use-money'
import { formatPercent, parseAmount } from '@/lib/utils'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import { Badge } from '@/components/ui/badge'

interface TaxonomyCategoryView {
  id: number
  name: string
  color: string
  targetPercent: number | null
  allocatedValueEur: number
  actualPercent: number
  assignedTickers: string[]
}

interface CustomTaxonomyView {
  id: number
  name: string
  description: string | null
  totalClassifiedEur: number
  categories: TaxonomyCategoryView[]
}

export function CustomTaxonomiesSection() {
  const { t } = useTranslation()
  const money = useMoney()
  const queryClient = useQueryClient()

  const { data: taxonomies = [] } = useQuery({
    queryKey: ['wealth', 'taxonomies'],
    queryFn: async () => (await api.get<CustomTaxonomyView[]>('/taxonomies')).data,
  })

  const [newTaxName, setNewTaxName] = useState('')
  const [catDrafts, setCatDrafts] = useState<Record<number, { name: string; target: string }>>({})
  const [assignDrafts, setAssignDrafts] = useState<Record<number, { categoryId: number; ticker: string }>>({})

  const createTax = useMutation({
    mutationFn: async (name: string) => {
      await api.post('/taxonomies', { name })
    },
    onSuccess: () => {
      setNewTaxName('')
      void queryClient.invalidateQueries({ queryKey: ['wealth', 'taxonomies'] })
    },
  })

  const deleteTax = useMutation({
    mutationFn: async (id: number) => {
      await api.delete(`/taxonomies/${id}`)
    },
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['wealth', 'taxonomies'] }),
  })

  const addCategory = useMutation({
    mutationFn: async ({ taxonomyId, name, targetPercent }: { taxonomyId: number; name: string; targetPercent: number | null }) => {
      await api.post(`/taxonomies/${taxonomyId}/categories`, { name, targetPercent, color: '#6366f1' })
    },
    onSuccess: (_res, vars) => {
      setCatDrafts((prev) => ({ ...prev, [vars.taxonomyId]: { name: '', target: '' } }))
      void queryClient.invalidateQueries({ queryKey: ['wealth', 'taxonomies'] })
    },
  })

  const assignHolding = useMutation({
    mutationFn: async ({ taxonomyId, categoryId, ticker }: { taxonomyId: number; categoryId: number; ticker: string }) => {
      await api.post(`/taxonomies/${taxonomyId}/assignments`, {
        categoryId,
        ticker: ticker.trim().toUpperCase(),
        weightPercent: 100,
      })
    },
    onSuccess: (_res, vars) => {
      setAssignDrafts((prev) => ({ ...prev, [vars.taxonomyId]: { categoryId: vars.categoryId, ticker: '' } }))
      void queryClient.invalidateQueries({ queryKey: ['wealth', 'taxonomies'] })
    },
  })

  return (
    <div className="space-y-4">
      <Card className="rounded-xl bg-card">
        <CardHeader>
          <CardTitle className="text-base font-semibold">{t('taxonomies.title')}</CardTitle>
          <CardDescription>{t('taxonomies.description')}</CardDescription>
        </CardHeader>
        <CardContent>
          <form
            className="flex flex-wrap items-center gap-2"
            onSubmit={(e) => {
              e.preventDefault()
              const trimmed = newTaxName.trim()
              if (trimmed) createTax.mutate(trimmed)
            }}
          >
            <Input
              className="max-w-sm"
              value={newTaxName}
              onChange={(e) => setNewTaxName(e.target.value)}
              placeholder={t('taxonomies.newPlaceholder')}
            />
            <Button type="submit" size="sm" disabled={!newTaxName.trim() || createTax.isPending}>
              <Plus className="size-4" aria-hidden="true" />
              {t('taxonomies.create')}
            </Button>
          </form>
        </CardContent>
      </Card>

      {taxonomies.map((tax) => {
        const catDraft = catDrafts[tax.id] ?? { name: '', target: '' }
        const assignDraft = assignDrafts[tax.id] ?? {
          categoryId: tax.categories[0]?.id ?? 0,
          ticker: '',
        }
        return (
          <Card key={tax.id} className="rounded-xl bg-card">
            <CardHeader className="flex flex-row items-center justify-between gap-2 pb-3">
              <div>
                <CardTitle className="text-base font-semibold">{tax.name}</CardTitle>
                <CardDescription>
                  {t('taxonomies.classifiedTotal')}: {money.amount(tax.totalClassifiedEur, 'EUR')}
                </CardDescription>
              </div>
              <Button
                type="button"
                size="icon-sm"
                variant="ghost"
                className="text-muted-foreground hover:text-destructive"
                onClick={() => deleteTax.mutate(tax.id)}
                aria-label={t('common.delete')}
              >
                <Trash className="size-4" aria-hidden="true" />
              </Button>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-2.5">
                {tax.categories.map((cat) => (
                  <div key={cat.id} className="space-y-1.5 rounded-lg border border-border/60 p-3">
                    <div className="flex items-center justify-between gap-2 text-sm">
                      <span className="font-medium text-foreground">{cat.name}</span>
                      <span className="tabular-nums text-xs text-muted-foreground">
                        {money.amount(cat.allocatedValueEur, 'EUR')} ({formatPercent(cat.actualPercent)}
                        {cat.targetPercent != null ? ` / ${formatPercent(cat.targetPercent)}` : ''})
                      </span>
                    </div>
                    <Progress value={Math.min(100, cat.actualPercent)} />
                    {cat.assignedTickers.length > 0 && (
                      <div className="flex flex-wrap gap-1 pt-1">
                        {cat.assignedTickers.map((tk) => (
                          <Badge key={tk} variant="outline" className="text-[11px]">{tk}</Badge>
                        ))}
                      </div>
                    )}
                  </div>
                ))}
              </div>

              <div className="grid gap-3 border-t border-border/60 pt-3 sm:grid-cols-2">
                <form
                  className="flex items-end gap-2"
                  onSubmit={(e) => {
                    e.preventDefault()
                    if (!catDraft.name.trim()) return
                    const target = catDraft.target.trim() ? parseAmount(catDraft.target) : null
                    addCategory.mutate({
                      taxonomyId: tax.id,
                      name: catDraft.name.trim(),
                      targetPercent: target != null && Number.isFinite(target) ? target : null,
                    })
                  }}
                >
                  <div className="flex-1 space-y-1">
                    <Label className="text-xs">{t('taxonomies.addCategory')}</Label>
                    <Input
                      value={catDraft.name}
                      onChange={(e) =>
                        setCatDrafts((prev) => ({
                          ...prev,
                          [tax.id]: { ...catDraft, name: e.target.value },
                        }))
                      }
                      placeholder={t('taxonomies.categoryPlaceholder')}
                    />
                  </div>
                  <div className="w-20 space-y-1">
                    <Label className="text-xs">%</Label>
                    <Input
                      inputMode="decimal"
                      value={catDraft.target}
                      onChange={(e) =>
                        setCatDrafts((prev) => ({
                          ...prev,
                          [tax.id]: { ...catDraft, target: e.target.value },
                        }))
                      }
                      placeholder="30"
                    />
                  </div>
                  <Button type="submit" size="sm" variant="outline" disabled={!catDraft.name.trim()}>
                    <Plus className="size-4" aria-hidden="true" />
                  </Button>
                </form>

                {tax.categories.length > 0 && (
                  <form
                    className="flex items-end gap-2"
                    onSubmit={(e) => {
                      e.preventDefault()
                      const cid = assignDraft.categoryId || tax.categories[0]!.id
                      if (!assignDraft.ticker.trim() || !cid) return
                      assignHolding.mutate({
                        taxonomyId: tax.id,
                        categoryId: cid,
                        ticker: assignDraft.ticker,
                      })
                    }}
                  >
                    <div className="flex-1 space-y-1">
                      <Label className="text-xs">{t('taxonomies.assignTicker')}</Label>
                      <Input
                        value={assignDraft.ticker}
                        onChange={(e) =>
                          setAssignDrafts((prev) => ({
                            ...prev,
                            [tax.id]: { ...assignDraft, ticker: e.target.value },
                          }))
                        }
                        placeholder="CW8.PA"
                      />
                    </div>
                    <Button type="submit" size="sm" variant="outline" disabled={!assignDraft.ticker.trim()}>
                      {t('taxonomies.assign')}
                    </Button>
                  </form>
                )}
              </div>
            </CardContent>
          </Card>
        )
      })}
    </div>
  )
}
