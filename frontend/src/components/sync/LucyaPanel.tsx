import { useState } from 'react'
import { ArrowsClockwise, Lock, User, Warning } from '@phosphor-icons/react'
import { useTranslation } from 'react-i18next'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useLucyaRefresh } from '@/features/sync/hooks'
import { getErrorCode, getErrorStatus } from '@/lib/errors'

/** Backend error code -> `sync.lucya.errors.<suffix>`; any other failure reads as `serverError`. */
const ERROR_MESSAGE_SUFFIXES: Record<string, string> = {
  INVALID_CREDENTIALS: 'invalidCredentials',
  CAPTCHA_BLOCKED: 'captchaBlocked',
  MFA_TYPE_UNSUPPORTED: 'mfaTypeUnsupported',
  RATE_LIMITED: 'tooManyAttempts',
  PORTFOLIO_INCOMPLETE: 'portfolioIncomplete',
  UPSTREAM_FORMAT_CHANGED: 'formatChanged',
  INVALID_DATA: 'invalidData',
}

function errorSuffix(error: unknown): string {
  if (getErrorStatus(error) === 429) return 'tooManyAttempts'
  return ERROR_MESSAGE_SUFFIXES[getErrorCode(error) ?? ''] ?? 'serverError'
}

interface LucyaPanelProps {
  /** Called once the contracts are written, so a host dialog can close. */
  onConnected?: () => void
}

/**
 * Lucya keeps nothing between two visits: signing in is how its contracts are refreshed, so there
 * is no session status, sync button or disconnection.
 */
export function LucyaPanel({ onConnected }: LucyaPanelProps = {}) {
  const { t } = useTranslation()
  const [identifier, setIdentifier] = useState('')
  const [password, setPassword] = useState('')
  const refresh = useLucyaRefresh()

  return (
    <div className="space-y-6">
      <Card size="sm">
        <CardContent>
          <p className="text-sm text-muted-foreground">{t('sync.lucya.scope')}</p>
          {refresh.data && (
            <p className="mt-2 text-sm text-emerald-600 dark:text-emerald-400" role="status">
              {t('sync.lucya.success', { count: refresh.data.contractCount })}
            </p>
          )}
        </CardContent>
      </Card>

      {refresh.error && (
        <Card size="sm" className="border-destructive/30">
          <CardContent className="flex items-center gap-3">
            <Warning className="size-5 shrink-0 text-destructive" />
            <p className="text-sm text-destructive" role="alert">
              {t(`sync.lucya.errors.${errorSuffix(refresh.error)}`)}
            </p>
          </CardContent>
        </Card>
      )}

      <form
        onSubmit={(event) => {
          event.preventDefault()
          refresh.mutate(
            { identifier: identifier.trim(), password },
            { onSuccess: () => onConnected?.(), onSettled: () => setPassword('') },
          )
        }}
      >
        <Card size="sm">
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="lucya-identifier">
                <User className="mr-1 inline-block size-4" />
                {t('sync.lucya.identifier')}
              </Label>
              <Input
                id="lucya-identifier"
                autoComplete="username"
                value={identifier}
                onChange={(event) => setIdentifier(event.target.value)}
                required
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="lucya-password">
                <Lock className="mr-1 inline-block size-4" />
                {t('sync.lucya.password')}
              </Label>
              <Input
                id="lucya-password"
                type="password"
                autoComplete="current-password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                required
              />
            </div>
            <Button type="submit" disabled={refresh.isPending}>
              <ArrowsClockwise className={refresh.isPending ? 'animate-spin' : undefined} />
              {t(refresh.isPending ? 'sync.lucya.refreshing' : 'sync.lucya.refresh')}
            </Button>
          </CardContent>
        </Card>
      </form>
    </div>
  )
}
