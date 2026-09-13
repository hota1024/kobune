import { useMemo } from 'react'
import QRCode from 'qrcode'

/**
 * A QR code, drawn as one rect per module.
 *
 * Square and crisp at any size — `shapeRendering="crispEdges"` on an
 * integer grid, scaled by the viewBox rather than resampled. A phone is
 * the reason this is on screen at all (`kobune url --qr`), so it has to
 * survive being pointed at from across a desk.
 */
export function Qr({ value, size, label }: { value: string; size: number; label?: string }) {
  const modules = useMemo(() => {
    try {
      const code = QRCode.create(value, { errorCorrectionLevel: 'M' })
      const count = code.modules.size
      const data = code.modules.data
      const rects: { x: number; y: number }[] = []

      for (let y = 0; y < count; y++) {
        for (let x = 0; x < count; x++) {
          if (data[y * count + x]) rects.push({ x, y })
        }
      }

      return { count, rects }
    } catch {
      return null
    }
  }, [value])

  if (!modules) return null

  return (
    <div className="flex shrink-0 flex-col items-center gap-1.5">
      <div className="bg-white p-1.5">
        <svg
          width={size}
          height={size}
          viewBox={`0 0 ${modules.count} ${modules.count}`}
          shapeRendering="crispEdges"
          className="block"
          role="img"
          aria-label={`QR code for ${value}`}
        >
          {modules.rects.map((rect) => (
            <rect key={`${rect.x}:${rect.y}`} x={rect.x} y={rect.y} width={1} height={1} />
          ))}
        </svg>
      </div>
      {label && (
        <div className="font-mono text-[9.5px] tracking-[.12em] text-shell-muted">{label}</div>
      )}
    </div>
  )
}
