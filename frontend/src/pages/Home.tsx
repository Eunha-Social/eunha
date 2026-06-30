import { useEffect, useState } from 'react'

import { getInstance, getHomeTimeline } from '../api.ts'
import type { mastodon } from '../masto.ts'
import { getToken } from '../auth.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { Compose } from '@/components/compose.tsx'
import { StatusCard } from '@/components/status-card.tsx'

export default function Home() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)
  const [statuses, setStatuses] = useState<mastodon.v1.Status[] | null>(null)
  const [replyTo, setReplyTo] = useState<mastodon.v1.Status | null>(null)
  const [error, setError] = useState<string | null>(null)
  const token = getToken()

  useEffect(() => {
    getInstance().then(setInstance).catch((e) => setError(String(e)))
  }, [])

  useEffect(() => {
    if (!token) return
    getHomeTimeline(token)
      .then(setStatuses)
      .catch((e) => setError(String(e)))
  }, [token])

  const handlePosted = (status: mastodon.v1.Status) => {
    setStatuses((prev) => [status, ...(prev ?? [])])
    setReplyTo(null)
  }

  const handleReply = (status: mastodon.v1.Status) => {
    setReplyTo(status)
    window.scrollTo({ top: 0, behavior: 'smooth' })
  }

  return (
    <div className="mx-auto max-w-2xl p-4">
      <TopBar title={instance?.title} />

      {error && <p className="text-destructive mb-4 text-sm">{error}</p>}

      {!token && instance && (
        <section className="space-y-2">
          <h1 className="text-2xl font-bold">{instance.title}</h1>
          <p className="text-foreground/90">{instance.description}</p>
          <p className="text-muted-foreground text-sm">
            {instance.domain} · running eunha {instance.version}
          </p>
        </section>
      )}

      {token && (
        <section className="space-y-3">
          <Compose
            token={token}
            replyTo={replyTo}
            onCancelReply={() => setReplyTo(null)}
            onPosted={handlePosted}
          />
          <h2 className="text-secondary text-lg font-semibold">Home</h2>
          {statuses === null && !error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {statuses?.map((s) => (
            <StatusCard
              key={s.id}
              status={s.reblog ?? s}
              token={token}
              boostedBy={s.reblog ? s.account : undefined}
              onReply={handleReply}
            />
          ))}
          {statuses?.length === 0 && (
            <p className="text-muted-foreground text-sm">Your home timeline is empty.</p>
          )}
        </section>
      )}
    </div>
  )
}
