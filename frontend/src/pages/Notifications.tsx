import { type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { AtSign, Bell, Pencil, Repeat2, Star, UserPlus } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { getNotifications } from '../api.ts'
import { beginLogin, getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { TimelineTabs } from '@/components/timeline-tabs.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'
import { Button } from '@/components/ui/button.tsx'

function describe(type: string): { icon: ReactNode; verb: string } {
  switch (type) {
    case 'mention':
      return { icon: <AtSign className="size-4" />, verb: 'mentioned you' }
    case 'reblog':
      return { icon: <Repeat2 className="size-4" />, verb: 'boosted your post' }
    case 'quote':
      return { icon: <Repeat2 className="size-4" />, verb: 'quoted your post' }
    case 'favourite':
      return { icon: <Star className="size-4" />, verb: 'favourited your post' }
    case 'follow':
      return { icon: <UserPlus className="size-4" />, verb: 'followed you' }
    case 'follow_request':
      return { icon: <UserPlus className="size-4" />, verb: 'requested to follow you' }
    case 'status':
      return { icon: <Bell className="size-4" />, verb: 'posted' }
    case 'update':
      return { icon: <Pencil className="size-4" />, verb: 'edited a post' }
    case 'poll':
      return { icon: <Bell className="size-4" />, verb: 'ran a poll that ended' }
    default:
      return { icon: <Bell className="size-4" />, verb: type }
  }
}

function NotificationItem({
  n,
  token,
}: {
  n: mastodon.v1.Notification
  token: string
}) {
  const { icon, verb } = describe(n.type)
  const name = n.account.displayName || n.account.username
  const header = (
    <div className="text-muted-foreground flex items-center gap-1.5 text-sm">
      {icon}
      <Link
        to={`/@${n.account.acct}`}
        className="text-foreground font-medium no-underline hover:underline"
      >
        {name}
      </Link>
      <span>{verb}</span>
    </div>
  )

  if (n.status) {
    return (
      <div className="space-y-1">
        {header}
        <StatusCard
          status={n.status.reblog ?? n.status}
          token={token}
          boostedBy={n.status.reblog ? n.status.account : undefined}
        />
      </div>
    )
  }
  return (
    <Card>
      <CardContent>{header}</CardContent>
    </Card>
  )
}

export default function Notifications() {
  const token = getToken()
  const feed = useInfiniteFeed<mastodon.v1.Notification>(
    (maxId) => (token ? getNotifications(token, maxId) : Promise.resolve([])),
    [token],
  )
  const items = feed.items

  return (
    <div className="mx-auto max-w-2xl p-4">
      <TopBar />
      <TimelineTabs />
      {!token ? (
        <div className="space-y-2">
          <p className="text-muted-foreground text-sm">Sign in to see your notifications.</p>
          <Button size="sm" onClick={() => beginLogin()}>
            Sign in
          </Button>
        </div>
      ) : (
        <div className="space-y-3">
          {feed.error && <p className="text-destructive text-sm">{feed.error}</p>}
          {items === null && !feed.error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {items?.map((n) => (
            <NotificationItem key={n.id} n={n} token={token} />
          ))}
          {items?.length === 0 && (
            <p className="text-muted-foreground text-sm">No notifications yet.</p>
          )}
          <InfiniteScroll
            onLoadMore={feed.loadMore}
            loading={feed.loadingMore}
            done={feed.done}
            hasItems={!!items?.length}
          />
        </div>
      )}
    </div>
  )
}
