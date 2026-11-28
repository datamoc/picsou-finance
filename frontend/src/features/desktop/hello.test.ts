import { beforeEach, describe, expect, it, vi } from 'vitest'
import { HELLO_CANCELLED, helloAvailable, helloUnlock } from './hello'

const invoke = vi.fn()

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}))

function shellMode(on: boolean) {
  Object.defineProperty(window, 'isTauri', { value: on, configurable: true })
}

beforeEach(() => {
  invoke.mockReset()
  shellMode(false)
})

describe('windows hello availability', () => {
  it('is unavailable outside the desktop shell without calling it', async () => {
    await expect(helloAvailable()).resolves.toBe(false)
    expect(invoke).not.toHaveBeenCalled()
  })

  it('returns the shell answer inside the desktop shell', async () => {
    shellMode(true)
    invoke.mockResolvedValue(true)
    await expect(helloAvailable()).resolves.toBe(true)
    expect(invoke).toHaveBeenCalledWith('hello_available')
  })

  it('is unavailable when the shell call throws', async () => {
    shellMode(true)
    invoke.mockRejectedValue(new Error('no bridge'))
    await expect(helloAvailable()).resolves.toBe(false)
  })
})

describe('windows hello unlock', () => {
  it('walks the window through on success', async () => {
    invoke.mockResolvedValue(undefined)
    await expect(helloUnlock()).resolves.toBeUndefined()
    expect(invoke).toHaveBeenCalledWith('hello_unlock')
  })

  it('propagates the shell rejection, cancellation included', async () => {
    invoke.mockRejectedValue(HELLO_CANCELLED)
    await expect(helloUnlock()).rejects.toBe(HELLO_CANCELLED)
  })
})
