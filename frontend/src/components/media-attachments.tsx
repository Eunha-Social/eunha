import { useState } from 'react'

import type { mastodon } from '../masto.ts'
import { cn } from '@/lib/utils.ts'

function MediaItem({ m }: { m: mastodon.v1.MediaAttachment }) {
  const preview = m.previewUrl || m.url || ''
  const full = m.url || m.remoteUrl || preview

  switch (m.type) {
    case 'image':
      return (
        <a href={full ?? undefined} target="_blank" rel="noreferrer" className="block h-full">
          <img
            src={preview}
            alt={m.description ?? ''}
            title={m.description ?? undefined}
            loading="lazy"
            className="h-full w-full object-cover"
          />
        </a>
      )
    case 'gifv':
      return (
        <video
          src={m.url ?? undefined}
          poster={m.previewUrl}
          autoPlay
          loop
          muted
          playsInline
          className="h-full w-full object-cover"
        />
      )
    case 'video':
      return (
        <video
          src={m.url ?? undefined}
          poster={m.previewUrl}
          controls
          preload="none"
          className="h-full w-full object-cover"
        />
      )
    case 'audio':
      return <audio src={m.url ?? undefined} controls className="w-full" />
    default:
      return (
        <a
          href={full ?? undefined}
          target="_blank"
          rel="noreferrer"
          className="text-accent block p-3 text-sm underline"
        >
          {m.description || 'Attachment'}
        </a>
      )
  }
}

export function MediaAttachments({
  attachments,
  sensitive,
}: {
  attachments: mastodon.v1.MediaAttachment[]
  sensitive?: boolean
}) {
  const [revealed, setRevealed] = useState(!sensitive)
  if (attachments.length === 0) return null

  return (
    <div className="relative mt-2">
      <div
        className={cn(
          'grid gap-1 overflow-hidden rounded-xl',
          attachments.length === 1 ? 'grid-cols-1' : 'grid-cols-2',
          !revealed && 'pointer-events-none',
        )}
      >
        {attachments.map((m) => {
          const visual = m.type !== 'audio' && m.type !== 'unknown'
          return (
            <div
              key={m.id}
              className={cn(
                'bg-muted overflow-hidden',
                visual && 'aspect-[16/10] max-h-80',
                !revealed && 'blur-xl',
              )}
            >
              <MediaItem m={m} />
            </div>
          )
        })}
      </div>
      {!revealed && (
        <button
          type="button"
          onClick={() => setRevealed(true)}
          className="absolute inset-0 flex items-center justify-center"
        >
          <span className="bg-background/85 rounded-md px-3 py-1.5 text-sm font-medium">
            Sensitive content — show
          </span>
        </button>
      )}
    </div>
  )
}
