import { useCallback } from 'react'
import { useLocation } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import { getPublicTimeline } from '../api.ts'
import { getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { useStatusStreaming } from '../hooks/use-status-streaming.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { TimelineStack } from '@/components/timeline-stack.tsx'

// `/local` and `/public` are the same page over two feeds, chosen by the path.
// `local` is also accepted as a prop, because "/" renders this page for a
// signed-out visitor and there is no path to read it from there.
export default function PublicTimeline({ local }: { local?: boolean } = {}) {
  const pathname = useLocation().pathname
  const isLocal = local ?? pathname === '/local'
  const token = getToken()

  const feed = useInfiniteFeed<mastodon.v1.Status>(
    (maxId) => getPublicTimeline(isLocal, token ?? undefined, maxId),
    [isLocal, token],
  )
  const subscribePublic = useCallback(
    (client: mastodon.streaming.Client) =>
      isLocal ? client.public.local.subscribe() : client.public.subscribe(),
    [isLocal],
  )

  useStatusStreaming({
    token: token ?? undefined,
    subscribe: subscribePublic,
    feed,
  })
  const statuses = feed.items

  return (
    <div className="page-frame">
      <TopBar />
      {feed.error && <p className="text-destructive text-sm">{feed.error}</p>}
      <div className="space-y-2">
        {statuses === null && !feed.error && (
          <p className="text-muted-foreground text-sm">Loading…</p>
        )}
        {!!statuses?.length && (
          <TimelineStack>
            {statuses.map((s) => (
              <StatusCard
                key={s.id}
                status={s.reblog ?? s}
                token={token ?? ''}
                boostedBy={s.reblog ? s.account : undefined}
              />
            ))}
          </TimelineStack>
        )}
        {statuses?.length === 0 && (
          <p className="text-muted-foreground text-sm">Nothing here yet.</p>
        )}
        <InfiniteScroll
          onLoadMore={feed.loadMore}
          loading={feed.loadingMore}
          done={feed.done}
          hasItems={!!statuses?.length}
        />
      </div>
    </div>
  )
}
