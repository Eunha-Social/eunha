import { useCallback, useEffect, useState } from 'react'

import { getInstance, getHomeTimeline } from '../api.ts'
import type { mastodon } from '../masto.ts'
import { getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { useStatusStreaming } from '../hooks/use-status-streaming.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { TimelineTabs } from '@/components/timeline-tabs.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { TimelineStack } from '@/components/timeline-stack.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'

export default function Home() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)
  const token = getToken()
  const { openCompose } = useComposeModal()

  const feed = useInfiniteFeed<mastodon.v1.Status>(
    (maxId) => (token ? getHomeTimeline(token, maxId) : Promise.resolve([])),
    [token],
  )
  const subscribeHome = useCallback(
    (client: mastodon.streaming.Client) => client.user.subscribe(),
    [],
  )

  useStatusStreaming({
    enabled: !!token,
    token: token ?? undefined,
    subscribe: subscribeHome,
    feed,
  })

  useEffect(() => {
    getInstance().then(setInstance).catch(() => {})
  }, [])

  const handleReply = (status: mastodon.v1.Status) => {
    openCompose({
      replyTo: status,
      onPosted: (posted) => feed.prepend(posted),
    })
  }

  const statuses = feed.items

  return (
    <div className="page-frame">
      <TopBar title={instance?.title} />
      <TimelineTabs />

      {feed.error && <p className="text-destructive mb-4 text-sm">{feed.error}</p>}

      {!token && instance && (
        <section className="space-y-2">
          <h1 className="text-2xl font-bold">{instance.title}</h1>
          <p className="text-foreground/90">{instance.description}</p>
          <p className="text-muted-foreground text-sm">
            {instance.domain} · running eunha {__COMMIT_HASH__}
          </p>
        </section>
      )}

      {token && (
        <section className="space-y-2">
          {statuses === null && !feed.error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {!!statuses?.length && (
            <TimelineStack>
              {statuses.map((s) => (
                <StatusCard
                  key={s.id}
                  status={s.reblog ?? s}
                  token={token}
                  boostedBy={s.reblog ? s.account : undefined}
                  onReply={handleReply}
                />
              ))}
            </TimelineStack>
          )}
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
