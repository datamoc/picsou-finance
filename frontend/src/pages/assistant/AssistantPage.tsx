import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { PaperPlaneRight, Plus, Trash, Sparkle } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { extractErrorMessage } from '@/lib/errors'
import { PageHeader } from '@/components/shared/PageHeader'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'

interface ChatSession {
  id: number
  title: string
  createdAt: string
  updatedAt: string
}

interface ChatMessage {
  id: number
  role: 'USER' | 'ASSISTANT'
  content: string
  provider: string | null
  model: string | null
  createdAt: string
}

interface ChatSessionDetail {
  session: ChatSession
  messages: ChatMessage[]
}

export function AssistantPage() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const [activeId, setActiveId] = useState<number | null>(null)
  const [prompt, setPrompt] = useState('')
  const [error, setError] = useState<string | null>(null)

  const { data: sessions = [] } = useQuery({
    queryKey: ['ai', 'chat', 'sessions'],
    queryFn: async () => (await api.get<ChatSession[]>('/ai/chat/sessions')).data,
  })

  const currentId = activeId ?? sessions[0]?.id ?? null

  const { data: detail } = useQuery({
    queryKey: ['ai', 'chat', 'session', currentId],
    enabled: currentId != null,
    queryFn: async () => (await api.get<ChatSessionDetail>(`/ai/chat/sessions/${currentId}`)).data,
  })

  const createSession = useMutation({
    mutationFn: async (title?: string) =>
      (await api.post<ChatSession>('/ai/chat/sessions', { title: title || t('assistant.newChatTitle') })).data,
    onSuccess: (created) => {
      setActiveId(created.id)
      void queryClient.invalidateQueries({ queryKey: ['ai', 'chat', 'sessions'] })
    },
  })

  const deleteSession = useMutation({
    mutationFn: async (id: number) => {
      await api.delete(`/ai/chat/sessions/${id}`)
    },
    onSuccess: (_res, id) => {
      if (activeId === id) setActiveId(null)
      void queryClient.invalidateQueries({ queryKey: ['ai', 'chat', 'sessions'] })
    },
  })

  const sendMessage = useMutation({
    mutationFn: async (content: string) => {
      let sid = currentId
      if (sid == null) {
        const created = (
          await api.post<ChatSession>('/ai/chat/sessions', {
            title: content.slice(0, 60),
          })
        ).data
        sid = created.id
        setActiveId(sid)
      }
      return (await api.post<ChatSessionDetail>(`/ai/chat/sessions/${sid}/messages`, { content })).data
    },
    onSuccess: (updated) => {
      setPrompt('')
      setError(null)
      setActiveId(updated.session.id)
      void queryClient.invalidateQueries({ queryKey: ['ai', 'chat', 'sessions'] })
      void queryClient.invalidateQueries({ queryKey: ['ai', 'chat', 'session', updated.session.id] })
    },
    onError: (err) => setError(extractErrorMessage(err, t('common.error'))),
  })

  const suggestedPrompts = [
    t('assistant.suggestions.summary'),
    t('assistant.suggestions.concentration'),
    t('assistant.suggestions.feesAndCash'),
  ]

  return (
    <div className="space-y-6">
      <PageHeader
        title={t('assistant.title')}
        actions={
          <Button type="button" size="sm" variant="outline" onClick={() => createSession.mutate(undefined)}>
            <Plus className="size-4" aria-hidden="true" />
            {t('assistant.newChat')}
          </Button>
        }
      />

      <div className="grid gap-4 lg:grid-cols-4">
        <Card className="rounded-xl bg-card lg:col-span-1">
          <CardHeader className="pb-3">
            <CardTitle className="text-sm font-semibold">{t('assistant.conversations')}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-1.5">
            {sessions.length === 0 ? (
              <p className="text-xs text-muted-foreground">{t('assistant.noSessions')}</p>
            ) : (
              sessions.map((s) => (
                <div
                  key={s.id}
                  className={cn(
                    'flex items-center justify-between gap-2 rounded-lg px-2.5 py-2 text-xs transition-colors',
                    s.id === currentId
                      ? 'bg-primary/10 font-medium text-foreground'
                      : 'text-muted-foreground hover:bg-muted/60',
                  )}
                >
                  <button
                    type="button"
                    className="min-w-0 flex-1 truncate text-left"
                    onClick={() => setActiveId(s.id)}
                  >
                    {s.title}
                  </button>
                  <Button
                    type="button"
                    size="icon-sm"
                    variant="ghost"
                    className="size-6 shrink-0 text-muted-foreground hover:text-destructive"
                    onClick={() => deleteSession.mutate(s.id)}
                    aria-label={t('common.delete')}
                  >
                    <Trash className="size-3.5" aria-hidden="true" />
                  </Button>
                </div>
              ))
            )}
          </CardContent>
        </Card>

        <Card className="rounded-xl bg-card lg:col-span-3">
          <CardHeader className="pb-3">
            <CardTitle className="text-base font-semibold">
              {detail?.session.title ?? t('assistant.welcomeTitle')}
            </CardTitle>
            <CardDescription>{t('assistant.welcomeDesc')}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {(!detail || detail.messages.length === 0) && (
              <div className="space-y-3 rounded-xl bg-muted/30 p-4">
                <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
                  <Sparkle className="size-4 text-primary" aria-hidden="true" />
                  <span>{t('assistant.tryAsking')}</span>
                </div>
                <div className="flex flex-wrap gap-2">
                  {suggestedPrompts.map((q) => (
                    <Button
                      key={q}
                      type="button"
                      size="sm"
                      variant="outline"
                      disabled={sendMessage.isPending}
                      onClick={() => sendMessage.mutate(q)}
                    >
                      {q}
                    </Button>
                  ))}
                </div>
              </div>
            )}

            {detail && detail.messages.length > 0 && (
              <div className="max-h-[420px] space-y-3 overflow-y-auto pr-1">
                {detail.messages.map((msg) => (
                  <div
                    key={msg.id}
                    className={cn(
                      'rounded-xl px-4 py-3 text-sm whitespace-pre-wrap',
                      msg.role === 'USER'
                        ? 'ml-8 bg-primary text-primary-foreground'
                        : 'mr-8 border border-border/60 bg-muted/30 text-foreground',
                    )}
                  >
                    <p>{msg.content}</p>
                    {msg.role === 'ASSISTANT' && (msg.provider || msg.model) && (
                      <div className="mt-2 flex items-center gap-1.5">
                        <Badge variant="outline" className="text-[10px]">
                          {[msg.provider, msg.model].filter(Boolean).join(' · ')}
                        </Badge>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}

            {error && (
              <p role="alert" className="text-xs text-destructive">{error}</p>
            )}

            <form
              className="flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault()
                const trimmed = prompt.trim()
                if (trimmed && !sendMessage.isPending) sendMessage.mutate(trimmed)
              }}
            >
              <Input
                value={prompt}
                onChange={(e) => setPrompt(e.target.value)}
                placeholder={t('assistant.inputPlaceholder')}
                disabled={sendMessage.isPending}
              />
              <Button type="submit" disabled={!prompt.trim() || sendMessage.isPending}>
                <PaperPlaneRight className="size-4" aria-hidden="true" />
                {t('assistant.send')}
              </Button>
            </form>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
