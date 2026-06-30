import { useLocation } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import { getPublicTimeline } from '../api.ts'
import { getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { TimelineTabs } from '@/components/timeline-tabs.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'

export default function PublicTimeline() {
  const local = useLocation().pathname === '/local'
  const token = getToken()

  const feed = useInfiniteFeed<mastodon.v1.Status>(
    (maxId) => getPublicTimeline(local, token ?? undefined, maxId),
    [local, token],
  )
  const statuses = feed.items

  return (
    <div className="mx-auto max-w-2xl p-4">
      <TopBar />
      <TimelineTabs />
      {feed.error && <p className="text-destructive text-sm">{feed.error}</p>}
      <div className="space-y-3">
        {statuses === null && !feed.error && (
          <p className="text-muted-foreground text-sm">Loading…</p>
        )}
        {statuses?.map((s) => (
          <StatusCard
            key={s.id}
            status={s.reblog ?? s}
            token={token ?? ''}
            boostedBy={s.reblog ? s.account : undefined}
          />
        ))}
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
