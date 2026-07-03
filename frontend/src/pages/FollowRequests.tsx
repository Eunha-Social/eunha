import { useCallback } from 'react'

import type { mastodon } from '../masto.ts'
import { getFollowRequests } from '../api.ts'
import { beginLogin, getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { AccountRow } from '@/components/account-row.tsx'
import { FollowRequestActions } from '@/components/follow-request-actions.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { Button } from '@/components/ui/button.tsx'

export default function FollowRequests() {
  const token = getToken()
  const feed = useInfiniteFeed<mastodon.v1.Account>(
    (maxId) => (token ? getFollowRequests(token, maxId) : Promise.resolve([])),
    [token],
  )
  const { mutate } = feed
  const remove = useCallback(
    (id: string) => mutate((items) => items.filter((a) => a.id !== id)),
    [mutate],
  )
  const accounts = feed.items

  return (
    <div className="page-frame">
      <TopBar />
      <h1 className="mb-3 text-lg font-bold">Follow requests</h1>
      {!token ? (
        <div className="space-y-2">
          <p className="text-muted-foreground text-sm">
            Sign in to review your follow requests.
          </p>
          <Button size="sm" onClick={() => beginLogin()}>
            Sign in
          </Button>
        </div>
      ) : (
        <div className="space-y-1">
          {feed.error && <p className="text-destructive text-sm">{feed.error}</p>}
          {accounts === null && !feed.error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {accounts?.map((a) => (
            <AccountRow
              key={a.id}
              account={a}
              action={
                <FollowRequestActions
                  account={a}
                  token={token}
                  onResolved={remove}
                />
              }
            />
          ))}
          {accounts?.length === 0 && (
            <p className="text-muted-foreground text-sm">No pending follow requests.</p>
          )}
          <InfiniteScroll
            onLoadMore={feed.loadMore}
            loading={feed.loadingMore}
            done={feed.done}
            hasItems={!!accounts?.length}
          />
        </div>
      )}
    </div>
  )
}
