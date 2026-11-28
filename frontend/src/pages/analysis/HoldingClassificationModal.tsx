import { useState } from 'react'
import { NativeSelect } from '@/components/shared/NativeSelect'
import { useTranslation } from 'react-i18next'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useClassifyHolding, useHoldingClassification } from '@/features/analysis/hooks'
import { cn, formatPercent, parseAmount, translateCountryName } from '@/lib/utils'
import type {
  HoldingClassificationRequest,
  HoldingClassificationView,
  WealthTier,
} from '@/types/api'

/**
 * The Morningstar sectors, in the order the breakdown legend uses. These are the same keys the
 * ETF look-through already emits, so a hand-classified share lands in the very same bucket as a
 * fund's slice rather than starting a parallel vocabulary.
 */
const SECTOR_KEYS = [
  'technology',
  'communication_services',
  'consumer_cyclical',
  'consumer_defensive',
  'healthcare',
  'industrials',
  'utilities',
  'basic_materials',
  'energy',
  'financial_services',
  'real_estate',
] as const

/**
 * Where a holding no data source covers can go.
 *
 * <p>Offered apart from the real sectors and regions because it is a different kind of answer:
 * an ELTIF or an unquoted fund is not badly classified, it is outside the listed world
 * altogether. Without these it would sit in the "to classify" list forever, dragging coverage
 * down and implying a lookup that will never succeed.
 */
const UNLISTED_SECTOR = 'private_equity'
const UNLISTED_COUNTRY = 'XU'

/** Multi-country geographic regions, using ISO 3166-1 user-assigned 2-letter codes (`X*`). */
const REGION_KEYS = ['XW', 'XD', 'XE', 'XM'] as const

/** The countries the breakdown already knows how to name, ISO 3166-1 alpha-2. */
const COUNTRY_KEYS = [
  'US', 'FR', 'DE', 'GB', 'NL', 'CH', 'IE', 'LU', 'BE', 'ES', 'IT', 'SE', 'DK', 'FI', 'NO',
  'CA', 'JP', 'CN', 'HK', 'KR', 'TW', 'IN', 'SG', 'AU', 'BR',
] as const

const TIER_KEYS: WealthTier[] = [
  'SAFETY_NET',
  'BONDS',
  'REAL_ESTATE',
  'EQUITY',
  'CRYPTO',
  'ALTERNATIVE',
]

function parsePctInput(raw: string): number | null {
  const trimmed = raw.trim()
  if (trimmed === '') return 0
  const n = parseAmount(trimmed)
  if (!Number.isFinite(n) || n < 0 || n > 100) return null
  return Math.round(n * 100) / 100
}

/**
 * The editor body, mounted only once the current classification has loaded and keyed on it, so
 * every field can seed from a lazy `useState` initializer instead of an effect — the shape the
 * React Compiler's `set-state-in-effect` rule requires, and what the sibling modals already do.
 */
function ClassificationForm({
  current,
  saving,
  onSubmit,
  onClose,
}: {
  current: HoldingClassificationView
  saving: boolean
  onSubmit: (body: HoldingClassificationRequest) => void
  onClose: () => void
}) {
  const { t, i18n } = useTranslation()
  const language = i18n?.resolvedLanguage ?? i18n?.language
  const hadMix =
    current.equityPct != null || current.bondsPct != null || current.cashPct != null

  const [sector, setSector] = useState(() => current.sectorKey ?? '')
  const [country, setCountry] = useState(() => current.countryKey ?? '')
  const [tier, setTier] = useState<string>(() => current.wealthTier ?? '')
  const [complex, setComplex] = useState(() => hadMix)
  const [equityInput, setEquityInput] = useState(() => String(current.equityPct ?? 100))
  const [bondsInput, setBondsInput] = useState(() => String(current.bondsPct ?? 0))
  const [cashInput, setCashInput] = useState(() => String(current.cashPct ?? 0))

  const eqVal = parsePctInput(equityInput)
  const bondsVal = parsePctInput(bondsInput)
  const cashVal = parsePctInput(cashInput)
  const mixPartsValid = eqVal != null && bondsVal != null && cashVal != null
  const mixTotal = mixPartsValid ? Math.round((eqVal + bondsVal + cashVal) * 100) / 100 : null
  const mixValid = !complex || (mixTotal != null && Math.abs(mixTotal - 100) < 0.005)

  function submit() {
    if (!mixValid) return
    const base: HoldingClassificationRequest = {
      sectorKey: sector === '' ? null : sector,
      countryKey: country === '' ? null : country,
      wealthTier: complex ? null : tier === '' ? null : (tier as WealthTier),
    }
    if (complex && eqVal != null && bondsVal != null && cashVal != null) {
      onSubmit({
        ...base,
        equityPct: eqVal,
        bondsPct: bondsVal,
        cashPct: cashVal,
      })
    } else if (hadMix) {
      onSubmit({
        ...base,
        equityPct: null,
        bondsPct: null,
        cashPct: null,
      })
    } else {
      onSubmit(base)
    }
  }

  const inferredSector = current.inferredSectorKey
  const inferredCountry = current.inferredCountryKey

  return (
    <>
      <div className="space-y-6">
        <div className="space-y-2">
          <Label htmlFor="classification-sector">
            {t('analysis.classification.sector')}
          </Label>
          <NativeSelect
            id="classification-sector"
            value={sector}
            onChange={(e) => setSector(e.target.value)}
          >
            <option value="">{t('analysis.classification.inherit')}</option>
            {SECTOR_KEYS.map((key) => (
              <option key={key} value={key}>
                {t(`holdings.insight.sectorNames.${key}`, key)}
              </option>
            ))}
            <option disabled>──────────</option>
            <option value={UNLISTED_SECTOR}>
              {t(`holdings.insight.sectorNames.${UNLISTED_SECTOR}`)}
            </option>
          </NativeSelect>
          {/* Shown, never pre-selected: adopting a guess by default would turn it into a permanent
              hand-made override that stops tracking the provider. */}
          {inferredSector && (
            <p className="text-sm text-muted-foreground">
              {t('analysis.classification.inferred', {
                value: t(`holdings.insight.sectorNames.${inferredSector}`, inferredSector),
              })}
            </p>
          )}
        </div>

        <div className="space-y-2">
          <Label htmlFor="classification-country">
            {t('analysis.classification.country')}
          </Label>
          <NativeSelect
            id="classification-country"
            value={country}
            onChange={(e) => setCountry(e.target.value)}
          >
            <option value="">{t('analysis.classification.inherit')}</option>
            {REGION_KEYS.map((key) => (
              <option key={key} value={key}>
                {translateCountryName(t, key, language)}
              </option>
            ))}
            <option disabled>──────────</option>
            {COUNTRY_KEYS.map((key) => (
              <option key={key} value={key}>
                {translateCountryName(t, key, language)}
              </option>
            ))}
            <option disabled>──────────</option>
            <option value={UNLISTED_COUNTRY}>
              {t(`holdings.insight.countryNames.${UNLISTED_COUNTRY}`)}
            </option>
          </NativeSelect>
          {inferredCountry && (
            <p className="text-sm text-muted-foreground">
              {t('analysis.classification.inferred', {
                value: translateCountryName(t, inferredCountry, language),
              })}
            </p>
          )}
        </div>

        <div className="space-y-3 rounded-lg border border-border/60 p-3">
          <div className="flex items-center justify-between gap-3">
            <div className="space-y-0.5">
              <Label htmlFor="classification-complex">
                {t('analysis.classification.complexProduct')}
              </Label>
              <p className="text-xs text-muted-foreground">
                {t('analysis.classification.complexProductHint')}
              </p>
            </div>
            <Switch
              id="classification-complex"
              checked={complex}
              onCheckedChange={setComplex}
            />
          </div>

          {complex && (
            <div className="space-y-2 pt-1">
              <div className="grid grid-cols-3 gap-3">
                <div className="space-y-1">
                  <Label htmlFor="classification-equity-pct" className="text-xs">
                    {t('analysis.classification.equityPct')}
                  </Label>
                  <Input
                    id="classification-equity-pct"
                    inputMode="decimal"
                    value={equityInput}
                    onChange={(e) => setEquityInput(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="classification-bonds-pct" className="text-xs">
                    {t('analysis.classification.bondsPct')}
                  </Label>
                  <Input
                    id="classification-bonds-pct"
                    inputMode="decimal"
                    value={bondsInput}
                    onChange={(e) => setBondsInput(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="classification-cash-pct" className="text-xs">
                    {t('analysis.classification.cashPct')}
                  </Label>
                  <Input
                    id="classification-cash-pct"
                    inputMode="decimal"
                    value={cashInput}
                    onChange={(e) => setCashInput(e.target.value)}
                  />
                </div>
              </div>
              <div className="flex items-center justify-between text-xs tabular-nums">
                <span className={cn(mixValid ? 'text-muted-foreground' : 'font-medium text-destructive')}>
                  {mixTotal != null
                    ? t('analysis.classification.mixTotal', {
                        total: formatPercent(mixTotal, { fractionDigits: mixTotal % 1 === 0 ? 0 : 1 }, language),
                      })
                    : t('analysis.classification.mixMustEqual100')}
                </span>
                {!mixValid && (
                  <span className="text-destructive">
                    {t('analysis.classification.mixMustEqual100')}
                  </span>
                )}
              </div>
            </div>
          )}
        </div>

        {!complex && (
          <div className="space-y-2">
            <Label htmlFor="classification-tier">{t('analysis.classification.tier')}</Label>
            <NativeSelect
              id="classification-tier"
              value={tier}
              onChange={(e) => setTier(e.target.value)}
            >
              <option value="">{t('analysis.classification.inherit')}</option>
              {TIER_KEYS.map((key) => (
                <option key={key} value={key}>
                  {t(`analysis.tiers.${key}`, key)}
                </option>
              ))}
            </NativeSelect>
            <p className="text-sm text-muted-foreground">
              {t('analysis.classification.tierHint')}
            </p>
          </div>
        )}
      </div>

      <DialogFooter>
        <Button type="button" variant="outline" onClick={onClose}>
          {t('common.cancel')}
        </Button>
        <Button type="button" onClick={submit} disabled={saving || !mixValid}>
          {t('common.save')}
        </Button>
      </DialogFooter>
    </>
  )
}

export function HoldingClassificationModal({
  open,
  accountId,
  ticker,
  name,
  onClose,
}: {
  open: boolean
  accountId: number | null
  ticker: string | null
  name?: string | null
  onClose: () => void
}) {
  const { t } = useTranslation()
  const current = useHoldingClassification(accountId, ticker, open)
  // Deliberately here and not inside the form.
  //
  // Saving invalidates the whole ['analysis'] namespace, which includes the per-ticker query this
  // modal reads. The refetch returns the values just saved, the form's remount key changes, and
  // React unmounts it — and TanStack Query drops the callbacks passed to mutate() when the
  // component that called it is gone (mutationObserver checks hasListeners()). So `onSuccess:
  // onClose` never fired: the row was written and the dialog just sat there.
  //
  // This component survives that remount, so its observer is still listening when the mutation
  // resolves. The keyed remount stays, because docs/conventions/frontend.md forbids re-seeding
  // form state from an effect.
  const classify = useClassifyHolding()

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('analysis.classification.title')}</DialogTitle>
          <DialogDescription>
            {name ? `${name} · ${ticker}` : ticker}
          </DialogDescription>
        </DialogHeader>

        {current.isPending || !current.data || accountId == null || !ticker ? (
          <div className="space-y-4">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
          </div>
        ) : (
          <ClassificationForm
            // Remounts when the loaded classification changes, so the lazy initializers re-seed.
            key={`${current.data.ticker}-${current.data.sectorKey}-${current.data.countryKey}-${current.data.wealthTier}-${current.data.equityPct}-${current.data.bondsPct}-${current.data.cashPct}`}
            current={current.data}
            saving={classify.isPending}
            onSubmit={(body) =>
              classify.mutate({ accountId, ticker, body }, { onSuccess: onClose })
            }
            onClose={onClose}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}
