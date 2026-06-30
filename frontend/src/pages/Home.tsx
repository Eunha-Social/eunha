import { useEffect, useState } from 'react'

import { getInstance, getHomeTimeline } from '../api.ts'
import type { mastodon } from '../masto.ts'
import { getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { TimelineTabs } from '@/components/timeline-tabs.tsx'
import { Compose } from '@/components/compose.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'

export default function Home() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)
  const [replyTo, setReplyTo] = useState<mastodon.v1.Status | null>(null)
  const token = getToken()

  const feed = useInfiniteFeed<mastodon.v1.Status>(
    (maxId) => (token ? getHomeTimeline(token, maxId) : Promise.resolve([])),
    [token],
  )

  useEffect(() => {
    getInstance().then(setInstance).catch(() => {})
  }, [])

  const handleReply = (status: mastodon.v1.Status) => {
    setReplyTo(status)
    window.scrollTo({ top: 0, behavior: 'smooth' })
  }

  const statuses = feed.items

  return (
    <div className="mx-auto max-w-2xl p-4">
      <TopBar title={instance?.title} />
      <TimelineTabs />

      {feed.error && <p className="text-destructive mb-4 text-sm">{feed.error}</p>}

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
            onPosted={(s) => {
              feed.prepend(s)
              setReplyTo(null)
            }}
          />
          {statuses === null && !feed.error && (
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
          <InfiniteScroll
            onLoadMore={feed.loadMore}
            loading={feed.loadingMore}
            done={feed.done}
            hasItems={!!statuses?.length}
          />
        </section>
      )}
    </div>
  )
}
