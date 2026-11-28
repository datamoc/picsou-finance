import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { Plus, Trash, CheckCircle, ArrowsLeftRight } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { extractErrorMessage } from '@/lib/errors'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'

interface ProfileEntry {
  id: string
  name: string
  isDefault: boolean
  createdAt: string
}

interface ProfilesResponse {
  activeProfileId: string
  profiles: ProfileEntry[]
}

export function DesktopProfilesCard() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [newName, setNewName] = useState('')
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const { data, isError } = useQuery({
    queryKey: ['desktop', 'profiles'],
    queryFn: async () => (await api.get<ProfilesResponse>('/profiles')).data,
    retry: false,
  })

  const createMutation = useMutation({
    mutationFn: async (name: string) => (await api.post<ProfileEntry>('/profiles', { name })).data,
    onSuccess: () => {
      setNewName('')
      setError(null)
      void queryClient.invalidateQueries({ queryKey: ['desktop', 'profiles'] })
    },
    onError: (err) => setError(extractErrorMessage(err, t('common.error'))),
  })

  const activateMutation = useMutation({
    mutationFn: async (id: string) =>
      (await api.post<{ activeProfileId: string; restartRequired: boolean }>(`/profiles/${id}/activate`)).data,
    onSuccess: (res) => {
      setError(null)
      if (res.restartRequired) {
        setNotice(t('settings.profiles.restartNotice'))
      }
      void queryClient.invalidateQueries({ queryKey: ['desktop', 'profiles'] })
    },
    onError: (err) => setError(extractErrorMessage(err, t('common.error'))),
  })

  const deleteMutation = useMutation({
    mutationFn: async (id: string) => {
      await api.delete(`/profiles/${id}`)
    },
    onSuccess: () => {
      setError(null)
      void queryClient.invalidateQueries({ queryKey: ['desktop', 'profiles'] })
    },
    onError: (err) => setError(extractErrorMessage(err, t('common.error'))),
  })

  if (isError || !data) {
    return (
      <p className="text-sm text-muted-foreground">
        {t('settings.profiles.sqliteOnly')}
      </p>
    )
  }

  return (
    <div className="space-y-4">
      <div className="space-y-2">
        {data.profiles.map((profile) => {
          const active = profile.id === data.activeProfileId
          return (
            <div
              key={profile.id}
              className="flex items-center justify-between gap-3 rounded-lg border border-border/60 px-3.5 py-2.5"
            >
              <div className="flex items-center gap-2 min-w-0">
                <span className="truncate text-sm font-medium text-foreground">
                  {profile.name}
                </span>
                {active && (
                  <Badge variant="secondary">
                    {t('settings.profiles.active')}
                  </Badge>
                )}
              </div>
              <div className="flex items-center gap-2 shrink-0">
                {!active && (
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={activateMutation.isPending}
                    onClick={() => activateMutation.mutate(profile.id)}
                  >
                    <ArrowsLeftRight className="size-3.5" aria-hidden="true" />
                    {t('settings.profiles.switch')}
                  </Button>
                )}
                {!profile.isDefault && !active && (
                  <Button
                    type="button"
                    size="icon-sm"
                    variant="ghost"
                    className="text-muted-foreground hover:text-destructive"
                    disabled={deleteMutation.isPending}
                    onClick={() => deleteMutation.mutate(profile.id)}
                    aria-label={t('common.delete')}
                  >
                    <Trash className="size-4" aria-hidden="true" />
                  </Button>
                )}
              </div>
            </div>
          )
        })}
      </div>

      {notice && (
        <p className="flex items-center gap-1.5 text-xs text-emerald-700 dark:text-emerald-400">
          <CheckCircle className="size-3.5 shrink-0" aria-hidden="true" />
          {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="text-xs text-destructive">{error}</p>
      )}

      <form
        className="flex items-center gap-2"
        onSubmit={(e) => {
          e.preventDefault()
          const trimmed = newName.trim()
          if (trimmed) createMutation.mutate(trimmed)
        }}
      >
        <Input
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          placeholder={t('settings.profiles.newPlaceholder')}
          maxLength={60}
        />
        <Button type="submit" size="sm" disabled={!newName.trim() || createMutation.isPending}>
          <Plus className="size-4" aria-hidden="true" />
          {t('settings.profiles.create')}
        </Button>
      </form>
    </div>
  )
}
