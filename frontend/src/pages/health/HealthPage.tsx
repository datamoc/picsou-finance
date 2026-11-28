import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from 'react-router'
import { useTranslation } from 'react-i18next'
import {
  ArrowsClockwise,
  CaretDown,
  CheckCircle,
  Eye,
  EyeSlash,
  Heartbeat,
  WarningCircle,
} from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { PageHeader } from '@/components/shared/PageHeader'
import { ErrorState } from '@/components/shared/ErrorState'
import { EmptyState } from '@/components/shared/EmptyState'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { useMoney } from '@/hooks/use-money'
import { cn, formatPercent } from '@/lib/utils'

interface HealthAffectedItem {
  label: string
  detail: string | null
  valueEur: number
  navigateTo: string | null
}

interface HealthIssue {
  key: string
  category: string
  severity: 'INFO' | 'WARNING' | 'ERROR' | 'CRITICAL'
  titleKey: string
  descriptionKey: string
  affectedCount: number
  impactedValueEur: number
  portfolioSharePct: number
  navigateTo: string
  actionLabelKey: string
  items: HealthAffectedItem[]
  dismissed: boolean
}

interface HealthReport {
  status: 'HEALTHY' | 'ATTENTION' | 'DEGRADED' | 'CRITICAL'
  evaluatedAt: string
  totalPortfolioEur: number
  summary: {
    critical: number
    error: number
    warning: number
    info: number
    dismissed: number
    totalActive: number
    score: number
  }
  issues: HealthIssue[]
  dismissedIssues: HealthIssue[]
}

function severityBadgeVariant(severity: HealthIssue['severity']): 'destructive' | 'secondary' | 'outline' {
  if (severity === 'CRITICAL' || severity === 'ERROR') return 'destructive'
  if (severity === 'WARNING') return 'secondary'
  return 'outline'
}

export function HealthPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const money = useMoney()
  const queryClient = useQueryClient()
  const [expanded, setExpanded] = useState<Record<string, boolean>>({})
  const [showDismissed, setShowDismissed] = useState(false)

  const { data, isLoading, isError, isFetching, refetch } = useQuery({
    queryKey: ['health-center'],
    queryFn: async () => (await api.get<HealthReport>('/health-center')).data,
  })

  const dismissMutation = useMutation({
    mutationFn: async (issueKey: string) => {
      await api.post('/health-center/dismiss', { issueKey })
    },
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['health-center'] }),
  })

  const restoreMutation = useMutation({
    mutationFn: async (issueKey: string) => {
      await api.delete(`/health-center/dismiss/${encodeURIComponent(issueKey)}`)
    },
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['health-center'] }),
  })

  const toggleExpand = (key: string) =>
    setExpanded((prev) => ({ ...prev, [key]: !prev[key] }))

  const visibleIssues = showDismissed ? (data?.dismissedIssues ?? []) : (data?.issues ?? [])

  return (
    <div className="space-y-6">
      <PageHeader
        title={t('health.title')}
        actions={
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={isFetching}
            onClick={() => void refetch()}
          >
            <ArrowsClockwise
              className={cn('size-4', isFetching && 'animate-spin')}
              aria-hidden="true"
            />
            {t('health.refresh')}
          </Button>
        }
      />

      {isLoading && (
        <div className="grid gap-4 sm:grid-cols-4">
          <Skeleton className="h-28" />
          <Skeleton className="h-28" />
          <Skeleton className="h-28" />
          <Skeleton className="h-28" />
        </div>
      )}

      {isError && (
        <ErrorState title={t('common.error')} onRetry={() => void refetch()} />
      )}

      {data && (
        <>
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-2">
                <CardDescription>{t('health.score')}</CardDescription>
                <CardTitle className="text-2xl font-bold tabular-nums">
                  {data.summary.score} / 100
                </CardTitle>
              </CardHeader>
              <CardContent>
                <Progress value={data.summary.score} />
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-2">
                <CardDescription>{t('health.criticalAndErrors')}</CardDescription>
                <CardTitle className="text-2xl font-bold tabular-nums text-destructive">
                  {data.summary.critical + data.summary.error}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground">
                  {t('health.criticalCount', { count: data.summary.critical })}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-2">
                <CardDescription>{t('health.warnings')}</CardDescription>
                <CardTitle className="text-2xl font-bold tabular-nums">
                  {data.summary.warning}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <p className="text-xs text-muted-foreground">
                  {t('health.infoCount', { count: data.summary.info })}
                </p>
              </CardContent>
            </Card>

            <Card className="rounded-xl bg-card">
              <CardHeader className="pb-2">
                <CardDescription>{t('health.dismissed')}</CardDescription>
                <CardTitle className="text-2xl font-bold tabular-nums">
                  {data.summary.dismissed}
                </CardTitle>
              </CardHeader>
              <CardContent>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => setShowDismissed((v) => !v)}
                >
                  {showDismissed ? <Eye className="size-3.5" aria-hidden="true" /> : <EyeSlash className="size-3.5" aria-hidden="true" />}
                  {showDismissed ? t('health.showActive') : t('health.showDismissed')}
                </Button>
              </CardContent>
            </Card>
          </div>

          {visibleIssues.length === 0 ? (
            <Card className="rounded-xl bg-card">
              <CardContent>
                <EmptyState
                  className="py-8"
                  icon={showDismissed ? <Heartbeat className="size-10" /> : <CheckCircle className="size-10" />}
                  title={showDismissed ? t('health.emptyDismissedTitle') : t('health.allClearTitle')}
                  description={showDismissed ? t('health.emptyDismissedDesc') : t('health.allClearDesc')}
                />
              </CardContent>
            </Card>
          ) : (
            <div className="space-y-3">
              {visibleIssues.map((issue) => {
                const isOpen = Boolean(expanded[issue.key])
                return (
                  <Card key={issue.key} className="rounded-xl bg-card">
                    <CardHeader className="pb-3">
                      <div className="flex flex-wrap items-start justify-between gap-3">
                        <div className="space-y-1 min-w-0">
                          <div className="flex flex-wrap items-center gap-2">
                            <Badge variant={severityBadgeVariant(issue.severity)}>
                              {t(`health.severity.${issue.severity}`)}
                            </Badge>
                            <span className="text-xs text-muted-foreground tabular-nums">
                              {t('health.affectedItems', { count: issue.affectedCount })}
                              {' · '}
                              {money.amount(issue.impactedValueEur, 'EUR')}
                              {' ('}
                              {formatPercent(issue.portfolioSharePct)}
                              {')'}
                            </span>
                          </div>
                          <CardTitle className="text-base font-semibold">
                            {t(issue.titleKey)}
                          </CardTitle>
                          <CardDescription>{t(issue.descriptionKey)}</CardDescription>
                        </div>

                        <div className="flex flex-wrap items-center gap-2 shrink-0">
                          <Button
                            type="button"
                            size="sm"
                            onClick={() => navigate(issue.navigateTo)}
                          >
                            {t(issue.actionLabelKey)}
                          </Button>
                          {issue.dismissed ? (
                            <Button
                              type="button"
                              size="sm"
                              variant="outline"
                              disabled={restoreMutation.isPending}
                              onClick={() => restoreMutation.mutate(issue.key)}
                            >
                              {t('health.restore')}
                            </Button>
                          ) : (
                            <Button
                              type="button"
                              size="sm"
                              variant="ghost"
                              disabled={dismissMutation.isPending}
                              onClick={() => dismissMutation.mutate(issue.key)}
                            >
                              {t('health.dismiss')}
                            </Button>
                          )}
                        </div>
                      </div>
                    </CardHeader>
                    {issue.items.length > 0 && (
                      <CardContent className="pt-0">
                        <button
                          type="button"
                          onClick={() => toggleExpand(issue.key)}
                          className="inline-flex items-center gap-1.5 text-xs font-medium text-muted-foreground hover:text-foreground"
                        >
                          <CaretDown
                            className={cn('size-3.5 transition-transform', isOpen && 'rotate-180')}
                            aria-hidden="true"
                          />
                          {isOpen ? t('health.hideDetails') : t('health.showDetails')}
                        </button>
                        {isOpen && (
                          <div className="mt-3 divide-y divide-border/60 rounded-lg border border-border/60">
                            {issue.items.map((item, idx) => (
                              <div
                                key={`${issue.key}-${idx}`}
                                className="flex items-center justify-between gap-3 px-3 py-2 text-xs"
                              >
                                <div className="flex items-center gap-2 min-w-0">
                                  <WarningCircle className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
                                  <span className="truncate font-medium text-foreground">{item.label}</span>
                                  {item.detail && (
                                    <span className="truncate text-muted-foreground">{item.detail}</span>
                                  )}
                                </div>
                                <div className="flex items-center gap-2 shrink-0">
                                  <span className="font-mono tabular-nums text-foreground">
                                    {money.amount(item.valueEur, 'EUR')}
                                  </span>
                                  {item.navigateTo && (
                                    <Button
                                      type="button"
                                      size="sm"
                                      variant="ghost"
                                      onClick={() => navigate(item.navigateTo!)}
                                    >
                                      {t('common.open')}
                                    </Button>
                                  )}
                                </div>
                              </div>
                            ))}
                          </div>
                        )}
                      </CardContent>
                    )}
                  </Card>
                )
              })}
            </div>
          )}
        </>
      )}
    </div>
  )
}
