import { EnvelopeSimple } from '@phosphor-icons/react'
import { useQuery } from '@tanstack/react-query'
import { SidecarSessionPanel } from "@/components/sync/SidecarSessionPanel"
import { natixisApi } from '@/features/sync/api'
import { syncKeys } from '@/features/sync/keys'

interface NatixisPanelProps {
  onConnected?: () => void
}

/**
 * Natixis Interépargne employee savings; the code arrives by SMS, e-mail or voice call, never an
 * app push. Picsou Desktop on Windows or Linux can sign in with a passkey instead of a password and
 * code.
 */
export function NatixisPanel({ onConnected }: NatixisPanelProps = {}) {
  const { data: passkey } = useQuery({
    queryKey: syncKeys.natixisPasskey(),
    queryFn: natixisApi.getPasskeyAvailability,
    staleTime: Infinity,
  })

  return (
    <SidecarSessionPanel
      provider="natixis"
      translationPrefix="sync.natixis"
      fieldIdPrefix="natixis"
      loginIcon={EnvelopeSimple}
      otpRetryable
      passkey={passkey?.available ? { connect: natixisApi.connectWithPasskey } : undefined}
      onConnected={onConnected}
    />
  )
}
