import '@testing-library/jest-dom/vitest'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReactNode } from 'react'

const { apiPost } = vi.hoisted(() => ({ apiPost: vi.fn() }))

vi.mock('@/lib/api-client', () => ({
  api: { post: apiPost },
}))

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, values?: { count?: number }) =>
      values?.count === undefined ? key : `${key}:${values.count}`,
  }),
}))

const { LucyaTab } = await import('./LucyaTab')

function renderTab(onConnected?: () => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>
  }
  render(<LucyaTab onConnected={onConnected} />, { wrapper: Wrapper })
}

function signIn() {
  fireEvent.change(screen.getByLabelText('sync.lucya.identifier'), {
    target: { value: ' ab12 ' },
  })
  fireEvent.change(screen.getByLabelText('sync.lucya.password'), {
    target: { value: 'secret' },
  })
  fireEvent.click(screen.getByText('sync.lucya.refresh'))
}

describe('LucyaTab', () => {
  beforeEach(() => {
    apiPost.mockReset()
  })
  afterEach(cleanup)

  it('refreshes by signing in, then forgets the password', async () => {
    apiPost.mockResolvedValue({ data: { contractCount: 2 } })
    renderTab()

    signIn()

    expect(await screen.findByRole('status')).toHaveTextContent('sync.lucya.success:2')
    expect(apiPost).toHaveBeenCalledWith(
      '/lucya/refresh',
      { identifier: 'ab12', password: 'secret' },
      expect.anything(),
    )
    await waitFor(() => expect(screen.getByLabelText('sync.lucya.password')).toHaveValue(''))
  })

  it('tells its host once the contracts are written, never after a refusal', async () => {
    const onConnected = vi.fn()
    apiPost.mockRejectedValueOnce({ response: { status: 422, data: {} } })
    apiPost.mockResolvedValueOnce({ data: { contractCount: 1 } })
    renderTab(onConnected)

    signIn()
    await screen.findByRole('alert')
    expect(onConnected).not.toHaveBeenCalled()

    signIn()
    await waitFor(() => expect(onConnected).toHaveBeenCalledOnce())
  })

  it.each([
    [{ status: 422, data: { code: 'INVALID_CREDENTIALS' } }, 'invalidCredentials'],
    [{ status: 429, data: {} }, 'tooManyAttempts'],
    [{ status: 422, data: { code: 'UPSTREAM_UNAVAILABLE' } }, 'serverError'],
  ])('says why a refresh was refused (%o)', async (response, suffix) => {
    apiPost.mockRejectedValue({ response })
    renderTab()

    signIn()

    expect(await screen.findByRole('alert')).toHaveTextContent(`sync.lucya.errors.${suffix}`)
  })
})
