import { useEffect, useRef } from 'react'
import { decode } from 'blurhash'

// Decode a BlurHash to a small canvas, scaled up via CSS as an image placeholder.
export function Blurhash({ hash, className }: { hash: string; className?: string }) {
  const ref = useRef<HTMLCanvasElement>(null)

  useEffect(() => {
    const canvas = ref.current
    if (!canvas) return
    try {
      const pixels = decode(hash, 32, 32)
      const ctx = canvas.getContext('2d')
      if (!ctx) return
      const image = ctx.createImageData(32, 32)
      image.data.set(pixels)
      ctx.putImageData(image, 0, 0)
    } catch {
      // Invalid hash — leave the canvas blank.
    }
  }, [hash])

  return <canvas ref={ref} width={32} height={32} className={className} aria-hidden />
}
