import type { RefObject } from 'react'
import { useTranslation } from 'react-i18next'
import { DownloadSimple } from '@phosphor-icons/react'
import { Button } from '@/components/ui/button'
import { downloadChartSvg } from '@/lib/export-svg'

interface ExportSvgButtonProps {
  containerRef: RefObject<HTMLElement | null>
  filename?: string
  title?: string
  className?: string
}

export function ExportSvgButton({
  containerRef,
  filename = 'picsou-chart.svg',
  title,
  className,
}: ExportSvgButtonProps) {
  const { t } = useTranslation()

  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className={className}
      onClick={() => downloadChartSvg(containerRef.current, filename, title)}
      aria-label={t('common.exportSvg')}
      title={t('common.exportSvg')}
    >
      <DownloadSimple className="size-3.5" aria-hidden="true" />
      <span className="hidden sm:inline">SVG</span>
    </Button>
  )
}
