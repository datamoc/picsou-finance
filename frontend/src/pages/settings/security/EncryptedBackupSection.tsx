import { useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { DownloadSimple, UploadSimple, ShieldCheck, CheckCircle, Warning } from '@phosphor-icons/react'
import { api } from '@/lib/api-client'
import { extractErrorMessage } from '@/lib/errors'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'

interface StorageSecurity {
  engine: string
  encryptedBackupSupported: boolean
  vaultMode: string
  vaultPresent: boolean
  osKeystoreMode: string
  databasePath: string | null
}

interface BackupInspection {
  valid: boolean
  uncompressedBytes: number
  integrityOk: boolean
  tableCount: number
  migrationCount: number
}

interface RestoreResponse {
  staged: boolean
  restartRequired: boolean
  inspection: BackupInspection
}

export function EncryptedBackupSection() {
  const { t } = useTranslation()
  const [exportPassphrase, setExportPassphrase] = useState('')
  const [exportConfirm, setExportConfirm] = useState('')
  const [exporting, setExporting] = useState(false)
  const [exportError, setExportError] = useState<string | null>(null)
  const [exportSuccess, setExportSuccess] = useState(false)

  const fileRef = useRef<HTMLInputElement>(null)
  const [selectedFile, setSelectedFile] = useState<File | null>(null)
  const [restorePassphrase, setRestorePassphrase] = useState('')
  const [verifying, setVerifying] = useState(false)
  const [restoring, setRestoring] = useState(false)
  const [inspection, setInspection] = useState<BackupInspection | null>(null)
  const [restoreDone, setRestoreDone] = useState(false)
  const [restoreError, setRestoreError] = useState<string | null>(null)

  const { data: security } = useQuery({
    queryKey: ['backup', 'storage-security'],
    queryFn: async () => (await api.get<StorageSecurity>('/backup/storage-security')).data,
  })

  async function handleExport() {
    setExportError(null)
    setExportSuccess(false)
    if (exportPassphrase.length < 8) {
      setExportError(t('settings.encryptedBackup.passphraseTooShort'))
      return
    }
    if (exportPassphrase !== exportConfirm) {
      setExportError(t('settings.encryptedBackup.passphraseMismatch'))
      return
    }
    setExporting(true)
    try {
      const response = await api.post('/backup/encrypted/export', { passphrase: exportPassphrase }, {
        responseType: 'blob',
      })
      const blob = new Blob([response.data], { type: 'application/octet-stream' })
      const url = URL.createObjectURL(blob)
      const anchor = document.createElement('a')
      anchor.href = url
      const stamp = new Date().toISOString().slice(0, 10)
      anchor.download = `picsou-backup-${stamp}.picsou-backup`
      document.body.appendChild(anchor)
      anchor.click()
      anchor.remove()
      URL.revokeObjectURL(url)
      setExportPassphrase('')
      setExportConfirm('')
      setExportSuccess(true)
    } catch (err) {
      setExportError(extractErrorMessage(err, t('common.error')))
    } finally {
      setExporting(false)
    }
  }

  async function handleVerify() {
    if (!selectedFile || !restorePassphrase) return
    setRestoreError(null)
    setInspection(null)
    setRestoreDone(false)
    setVerifying(true)
    try {
      const form = new FormData()
      form.append('file', selectedFile)
      form.append('passphrase', restorePassphrase)
      const { data } = await api.post<BackupInspection>('/backup/encrypted/verify', form)
      setInspection(data)
    } catch (err) {
      setRestoreError(extractErrorMessage(err, t('common.error')))
    } finally {
      setVerifying(false)
    }
  }

  async function handleRestore() {
    if (!selectedFile || !restorePassphrase) return
    setRestoreError(null)
    setRestoring(true)
    try {
      const form = new FormData()
      form.append('file', selectedFile)
      form.append('passphrase', restorePassphrase)
      const { data } = await api.post<RestoreResponse>('/backup/encrypted/restore', form)
      setInspection(data.inspection)
      setRestoreDone(true)
    } catch (err) {
      setRestoreError(extractErrorMessage(err, t('common.error')))
    } finally {
      setRestoring(false)
    }
  }

  return (
    <div className="space-y-6">
      {security && (
        <div className="flex flex-wrap items-center gap-2 rounded-lg bg-muted/40 px-3 py-2.5 text-xs">
          <ShieldCheck className="size-4 text-primary" aria-hidden="true" />
          <span className="font-medium text-foreground">{t('settings.encryptedBackup.storageStatus')}</span>
          <Badge variant="secondary">{security.engine.toUpperCase()}</Badge>
          <Badge variant="outline">
            {t('settings.encryptedBackup.keystoreLabel')}: {security.osKeystoreMode}
          </Badge>
          <Badge variant="outline">
            {t('settings.encryptedBackup.vaultLabel')}: {security.vaultMode}
          </Badge>
        </div>
      )}

      {security && !security.encryptedBackupSupported ? (
        <p className="text-sm text-muted-foreground">
          {t('settings.encryptedBackup.sqliteOnly')}
        </p>
      ) : (
        <div className="grid gap-6 md:grid-cols-2">
          {/* Export encrypted backup */}
          <div className="space-y-3 rounded-xl border border-border/60 p-4">
            <div className="space-y-1">
              <p className="text-sm font-semibold text-foreground">
                {t('settings.encryptedBackup.exportTitle')}
              </p>
              <p className="text-xs text-muted-foreground">
                {t('settings.encryptedBackup.exportDesc')}
              </p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="backup-export-pass">{t('settings.encryptedBackup.passphrase')}</Label>
              <Input
                id="backup-export-pass"
                type="password"
                value={exportPassphrase}
                onChange={(e) => setExportPassphrase(e.target.value)}
                placeholder={t('settings.encryptedBackup.passphrasePlaceholder')}
                autoComplete="new-password"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="backup-export-confirm">{t('settings.encryptedBackup.confirmPassphrase')}</Label>
              <Input
                id="backup-export-confirm"
                type="password"
                value={exportConfirm}
                onChange={(e) => setExportConfirm(e.target.value)}
                autoComplete="new-password"
              />
            </div>
            {exportError && (
              <p role="alert" className="text-xs text-destructive">{exportError}</p>
            )}
            {exportSuccess && (
              <p className="flex items-center gap-1.5 text-xs text-emerald-700 dark:text-emerald-400">
                <CheckCircle className="size-3.5" aria-hidden="true" />
                {t('settings.encryptedBackup.exportSuccess')}
              </p>
            )}
            <Button
              type="button"
              size="sm"
              disabled={exporting || !exportPassphrase || !exportConfirm}
              onClick={() => void handleExport()}
            >
              <DownloadSimple className="size-4" aria-hidden="true" />
              {t('settings.encryptedBackup.exportButton')}
            </Button>
          </div>

          {/* Verify & restore encrypted backup */}
          <div className="space-y-3 rounded-xl border border-border/60 p-4">
            <div className="space-y-1">
              <p className="text-sm font-semibold text-foreground">
                {t('settings.encryptedBackup.restoreTitle')}
              </p>
              <p className="text-xs text-muted-foreground">
                {t('settings.encryptedBackup.restoreDesc')}
              </p>
            </div>
            <div className="space-y-2">
              <input
                ref={fileRef}
                type="file"
                accept=".picsou-backup,application/octet-stream"
                className="hidden"
                onChange={(e) => {
                  setSelectedFile(e.target.files?.[0] ?? null)
                  setInspection(null)
                  setRestoreDone(false)
                  setRestoreError(null)
                }}
              />
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => fileRef.current?.click()}
              >
                <UploadSimple className="size-4" aria-hidden="true" />
                {selectedFile ? selectedFile.name : t('settings.encryptedBackup.chooseFile')}
              </Button>
            </div>
            <div className="space-y-2">
              <Label htmlFor="backup-restore-pass">{t('settings.encryptedBackup.passphrase')}</Label>
              <Input
                id="backup-restore-pass"
                type="password"
                value={restorePassphrase}
                onChange={(e) => setRestorePassphrase(e.target.value)}
                autoComplete="current-password"
              />
            </div>
            {restoreError && (
              <p role="alert" className="flex items-center gap-1.5 text-xs text-destructive">
                <Warning className="size-3.5 shrink-0" aria-hidden="true" />
                {restoreError}
              </p>
            )}
            {inspection && (
              <div className="rounded-lg bg-muted/40 p-2.5 text-xs text-foreground">
                <p className="font-medium">
                  {inspection.valid
                    ? t('settings.encryptedBackup.verifyOk', { tables: inspection.tableCount })
                    : t('settings.encryptedBackup.verifyFailed')}
                </p>
              </div>
            )}
            {restoreDone && (
              <p className="flex items-center gap-1.5 text-xs text-emerald-700 dark:text-emerald-400">
                <CheckCircle className="size-3.5 shrink-0" aria-hidden="true" />
                {t('settings.encryptedBackup.restoreStaged')}
              </p>
            )}
            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={verifying || !selectedFile || !restorePassphrase}
                onClick={() => void handleVerify()}
              >
                {t('settings.encryptedBackup.verifyButton')}
              </Button>
              <Button
                type="button"
                variant="destructive"
                size="sm"
                disabled={restoring || !selectedFile || !restorePassphrase}
                onClick={() => void handleRestore()}
              >
                {t('settings.encryptedBackup.restoreButton')}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
