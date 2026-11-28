import { useMemo } from "react"
import { PiggyBank } from '@phosphor-icons/react'
import { SidecarSessionPanel } from "@/components/sync/SidecarSessionPanel"
import { linxeaApi } from "@/features/sync/api"

interface LinxeaPanelProps {
  onConnected?: () => void
}

/**
 * Linxea's client space: native OS WebView login on Picsou Desktop (Edge WebView2 on Windows),
 * or e-mail + password + 6-digit e-mail code on server installs.
 */
export function LinxeaPanel({ onConnected }: LinxeaPanelProps = {}) {
  const webview = useMemo(
    () => ({
      start: () => linxeaApi.startWebview(),
      getStatus: (processId: string) => linxeaApi.getWebviewStatus(processId),
      cancel: (processId: string) => linxeaApi.cancelWebview(processId),
    }),
    [],
  )
  return (
    <SidecarSessionPanel
      provider="linxea"
      translationPrefix="sync.linxea"
      fieldIdPrefix="linxea"
      loginIcon={PiggyBank}
      webview={webview}
      onConnected={onConnected}
    />
  )
}
