import { useContext, useState } from 'react'
import { QueryClientContext, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash, CaretDown } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { NativeSelect } from '@/components/shared/NativeSelect'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { cn, parseAmount } from '@/lib/utils'
import type { Category } from '@/types/api'

interface SplitLineDto {
  id: number
  categoryId: number
  categoryName: string
  amount: number
  note: string | null
}

interface DraftRow {
  categoryId: number
  amount: string
  note: string
}

export function TransactionSplitEditor(props: {
  transactionId: number
  transactionAmount: number
  categories: Category[]
}) {
  const qc = useContext(QueryClientContext)
  if (!qc) return null
  return <TransactionSplitEditorInner {...props} />
}

function TransactionSplitEditorInner({
  transactionId,
  transactionAmount,
  categories,
}: {
  transactionId: number
  transactionAmount: number
  categories: Category[]
}) {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [drafts, setDrafts] = useState<DraftRow[] | null>(null)
  const [savedNotice, setSavedNotice] = useState(false)

  const { data: existing = [] } = useQuery({
    queryKey: ['budget', 'splits', transactionId],
    enabled: open,
    queryFn: async () => (await api.get<SplitLineDto[]>(`/budget/transactions/${transactionId}/splits`)).data,
  })

  const rows: DraftRow[] =
    drafts ??
    (existing.length > 0
      ? existing.map((s) => ({
          categoryId: s.categoryId,
          amount: String(Math.abs(s.amount)),
          note: s.note ?? '',
        }))
      : [
          {
            categoryId: categories[0]?.id ?? 0,
            amount: String(Math.abs(transactionAmount)),
            note: '',
          },
        ])

  const targetAbs = Math.abs(transactionAmount)
  const currentSum = rows.reduce((acc, r) => {
    const v = parseAmount(r.amount)
    return acc + (Number.isFinite(v) ? Math.abs(v) : 0)
  }, 0)
  const remaining = Math.round((targetAbs - currentSum) * 100) / 100
  const balanced = Math.abs(remaining) < 0.01

  const saveMutation = useMutation({
    mutationFn: async () => {
      const sign = transactionAmount < 0 ? -1 : 1
      await api.put(`/budget/transactions/${transactionId}/splits`, {
        splits: rows.map((r) => ({
          categoryId: r.categoryId,
          amount: sign * Math.abs(parseAmount(r.amount)),
          note: r.note.trim() || null,
        })),
      })
    },
    onSuccess: () => {
      setSavedNotice(true)
      setDrafts(null)
      void queryClient.invalidateQueries({ queryKey: ['budget', 'splits', transactionId] })
    },
  })

  const clearMutation = useMutation({
    mutationFn: async () => {
      await api.delete(`/budget/transactions/${transactionId}/splits`)
    },
    onSuccess: () => {
      setDrafts(null)
      setSavedNotice(false)
      void queryClient.invalidateQueries({ queryKey: ['budget', 'splits', transactionId] })
    },
  })

  if (categories.length === 0) return null

  return (
    <div className="space-y-2">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="inline-flex w-full items-center justify-between rounded-md border border-border/60 px-3 py-2 text-xs font-medium text-foreground hover:bg-muted/40"
      >
        <span>{t('budgetSplits.title')}</span>
        <CaretDown className={cn('size-3.5 transition-transform', open && 'rotate-180')} aria-hidden="true" />
      </button>

      {open && (
        <div className="space-y-2.5 rounded-lg border border-border/60 p-3">
          {rows.map((row, idx) => (
            <div key={idx} className="flex items-center gap-2">
              <NativeSelect
                className="h-8 flex-1 rounded-md border border-input bg-background px-2 text-xs"
                value={row.categoryId}
                onChange={(e) => {
                  const next = [...rows]
                  next[idx] = { ...row, categoryId: Number(e.target.value) }
                  setDrafts(next)
                }}
              >
                {categories.map((c) => (
                  <option key={c.id} value={c.id}>{c.name}</option>
                ))}
              </NativeSelect>
              <Input
                className="h-8 w-24 text-xs"
                inputMode="decimal"
                value={row.amount}
                onChange={(e) => {
                  const next = [...rows]
                  next[idx] = { ...row, amount: e.target.value }
                  setDrafts(next)
                }}
              />
              {rows.length > 1 && (
                <Button
                  type="button"
                  size="icon-sm"
                  variant="ghost"
                  className="size-7 text-muted-foreground hover:text-destructive"
                  onClick={() => setDrafts(rows.filter((_, i) => i !== idx))}
                  aria-label={t('common.delete')}
                >
                  <Trash className="size-3.5" aria-hidden="true" />
                </Button>
              )}
            </div>
          ))}

          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() =>
                setDrafts([
                  ...rows,
                  {
                    categoryId: categories[0]!.id,
                    amount: remaining > 0 ? String(remaining) : '0',
                    note: '',
                  },
                ])
              }
            >
              <Plus className="size-3.5" aria-hidden="true" />
              {t('budgetSplits.addLine')}
            </Button>
            <span className={cn('tabular-nums', !balanced && 'text-destructive font-medium')}>
              {balanced
                ? t('budgetSplits.balanced')
                : t('budgetSplits.remaining', { amount: remaining })}
            </span>
          </div>

          {savedNotice && (
            <p className="text-xs text-emerald-700 dark:text-emerald-400">{t('budgetSplits.saved')}</p>
          )}

          <div className="flex items-center justify-end gap-2">
            {existing.length > 0 && (
              <Button
                type="button"
                size="sm"
                variant="ghost"
                disabled={clearMutation.isPending}
                onClick={() => clearMutation.mutate()}
              >
                {t('budgetSplits.clear')}
              </Button>
            )}
            <Button
              type="button"
              size="sm"
              disabled={!balanced || rows.length < 2 || saveMutation.isPending}
              onClick={() => saveMutation.mutate()}
            >
              {t('common.save')}
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}
