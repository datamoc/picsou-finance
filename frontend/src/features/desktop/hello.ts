import { isInsideDesktopShell } from './shell'

/** The shell's rejection when the person closes the Hello prompt: stay silent, not an error. */
export const HELLO_CANCELLED = 'cancelled'

/** Whether the running shell can ask for Windows Hello right now. False everywhere else. */
export async function helloAvailable(): Promise<boolean> {
  if (!isInsideDesktopShell()) return false
  try {
    const { invoke } = await import('@tauri-apps/api/core')
    return await invoke<boolean>('hello_available')
  } catch {
    return false
  }
}

/**
 * Verify Windows Hello and unlock through it. Resolves once the shell has walked the
 * window through the unlock; rejects with `HELLO_CANCELLED` when the person closes the
 * prompt, or with a message otherwise.
 */
export async function helloUnlock(): Promise<void> {
  const { invoke } = await import('@tauri-apps/api/core')
  await invoke('hello_unlock')
}
