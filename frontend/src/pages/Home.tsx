import { useCallback } from 'react'

import { getHomeTimeline } from '../api.ts'
import { withoutMessages } from '../lib/statuses.ts'
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
import PublicTimeline from './PublicTimeline.tsx'

// Signed out there is no home timeline, and what a visitor arrived to look at
// is the instance's own posts — so "/" is the local timeline rather than a
// paragraph about the software running it. What the instance says about itself
// still has a page of its own at /about.
//
// A dispatcher rather than a branch inside the timeline: signing in or out
// reloads the page, so the two never swap under a mounted component, and each
// keeps its own hooks.
export default function Home() {
  return getToken() ? <HomeTimeline /> : <PublicTimeline local />
}

function HomeTimeline() {
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

  const handleReply = (status: mastodon.v1.Status) => {
    openCompose({
      replyTo: status,
      onPosted: (posted) => feed.prepend(posted),
    })
  }

  const statuses = withoutMessages(feed.items)

  return (
    <div className="page-frame">
      <TopBar />
      <TimelineTabs />

      {feed.error && <p className="text-destructive mb-4 text-sm">{feed.error}</p>}

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
                token={token ?? ''}
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
    </div>
  )
}
