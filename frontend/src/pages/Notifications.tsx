import { useCallback, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { AtSign, Bell, Pencil, Repeat2, Star, UserPlus } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { getNotifications } from '../api.ts'
import { beginLogin, getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { useStreamingSubscription } from '../hooks/use-streaming-subscription.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { TimelineTabs } from '@/components/timeline-tabs.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { FollowRequestActions } from '@/components/follow-request-actions.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'
import { Button } from '@/components/ui/button.tsx'
import { TimelineStack } from '@/components/timeline-stack.tsx'

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
  onResolve,
}: {
  n: mastodon.v1.Notification
  token: string
  // Removes this notification once its follow request is accepted/rejected.
  onResolve: (id: string) => void
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
      <div>
        <div className="px-3 pt-3 sm:px-4">{header}</div>
        <StatusCard
          status={n.status.reblog ?? n.status}
          token={token}
          boostedBy={n.status.reblog ? n.status.account : undefined}
        />
      </div>
    )
  }
  return (
    <Card className="rounded-none border-0 py-3 shadow-none">
      <CardContent className="space-y-2 px-3 sm:px-4">
        {header}
        {n.type === 'follow_request' && (
          <FollowRequestActions
            account={n.account}
            token={token}
            onResolved={() => onResolve(n.id)}
          />
        )}
      </CardContent>
    </Card>
  )
}

export default function Notifications() {
  const token = getToken()
  const feed = useInfiniteFeed<mastodon.v1.Notification>(
    (maxId) => (token ? getNotifications(token, maxId) : Promise.resolve([])),
    [token],
  )
  const { mutate } = feed
  const removeNotification = useCallback(
    (id: string) => mutate((items) => items.filter((item) => item.id !== id)),
    [mutate],
  )
  const subscribeNotifications = useCallback(
    (client: mastodon.streaming.Client) => client.user.notification.subscribe(),
    [],
  )
  const handleStreamingEvent = useCallback(
    (event: mastodon.streaming.Event) => {
      if (event.event === 'notification') {
        mutate((items) =>
          items.some((item) => item.id === event.payload.id)
            ? items
            : [event.payload, ...items],
        )
        return
      }

      if (event.event === 'status.update') {
        mutate((items) =>
          items.map((item) => {
            if (!('status' in item) || item.status?.id !== event.payload.id) {
              return item
            }
            return { ...item, status: event.payload } as mastodon.v1.Notification
          }),
        )
        return
      }

      if (event.event === 'delete') {
        mutate((items) =>
          items.filter(
            (item) => !('status' in item) || item.status?.id !== event.payload,
          ),
        )
      }
    },
    [mutate],
  )

  useStreamingSubscription({
    enabled: !!token,
    token: token ?? undefined,
    subscribe: subscribeNotifications,
    onEvent: handleStreamingEvent,
  })
  const items = feed.items

  return (
    <div className="page-frame">
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
        <div className="space-y-2">
          {feed.error && <p className="text-destructive text-sm">{feed.error}</p>}
          {items === null && !feed.error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {!!items?.length && (
            <TimelineStack>
              {items.map((n) => (
                <NotificationItem
                  key={n.id}
                  n={n}
                  token={token}
                  onResolve={removeNotification}
                />
              ))}
            </TimelineStack>
          )}
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
