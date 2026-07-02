import { useEffect, useState } from 'react'
import { Link, useParams } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import {
  getAccountStatuses,
  getCurrentAccount,
  getRelationship,
  lookupAccount,
  setFollow,
} from '../api.ts'
import { getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { TimelineStack } from '@/components/timeline-stack.tsx'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx'
import { Button } from '@/components/ui/button.tsx'

function hasCustomHeader(header: string | null | undefined) {
  return !!header && !header.includes('/headers/original/missing')
}

export default function Profile() {
  const { acct = '' } = useParams()
  const handle = acct.replace(/^@/, '')
  const token = getToken()

  const [account, setAccount] = useState<mastodon.v1.Account | null>(null)
  const [rel, setRel] = useState<mastodon.v1.Relationship | null>(null)
  const [selfId, setSelfId] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [relationshipBusy, setRelationshipBusy] = useState(false)

  const feed = useInfiniteFeed<mastodon.v1.Status>(
    (maxId) =>
      account ? getAccountStatuses(account.id, token ?? undefined, maxId) : Promise.resolve([]),
    [account?.id, token],
  )
  const statuses = feed.items

  useEffect(() => {
    setAccount(null)
    setRel(null)
    setError(null)
    lookupAccount(handle, token ?? undefined)
      .then((acc) => {
        setAccount(acc)
        if (token) {
          getRelationship(acc.id, token).then((r) => setRel(r ?? null)).catch(() => {})
        }
      })
      .catch((e) => setError(String(e)))
    if (token) {
      getCurrentAccount(token).then((me) => setSelfId(me.id)).catch(() => {})
    }
  }, [handle, token])

  const toggleFollow = async () => {
    if (!account || !token || !rel || relationshipBusy) return
    setRelationshipBusy(true)
    try {
      setRel(await setFollow(account.id, token, !rel.following))
    } finally {
      setRelationshipBusy(false)
    }
  }

  const toggleReblogs = async () => {
    if (!account || !token || !rel?.following || relationshipBusy) return
    setRelationshipBusy(true)
    try {
      setRel(
        await setFollow(account.id, token, true, {
          reblogs: !rel.showingReblogs,
        }),
      )
    } finally {
      setRelationshipBusy(false)
    }
  }

  const isSelf = account != null && account.id === selfId

  return (
    <div className="page-frame">
      <TopBar />
      {error && <p className="text-destructive text-sm">{error}</p>}
      {account && (
        <>
          {hasCustomHeader(account.header) && (
            <img
              src={account.header}
              alt=""
              className="h-32 w-full rounded-xl object-cover"
            />
          )}
          <div className="mt-3 flex items-start gap-3">
            <Avatar className="size-16 rounded-xl">
              <AvatarImage src={account.avatar} alt="" />
              <AvatarFallback>
                {(account.displayName || account.username).slice(0, 1).toUpperCase()}
              </AvatarFallback>
            </Avatar>
            <div className="flex-1">
              <div className="text-xl font-bold">
                {account.displayName || account.username}
              </div>
              <div className="text-muted-foreground">@{account.acct}</div>
            </div>
            {token && rel && !isSelf && (
              <div className="flex flex-col items-end gap-2">
                <Button
                  size="sm"
                  variant={rel.following || rel.requested ? 'outline' : 'default'}
                  onClick={toggleFollow}
                  disabled={relationshipBusy}
                >
                  {rel.following ? 'Following' : rel.requested ? 'Requested' : 'Follow'}
                </Button>
                {rel.following && (
                  <label className="text-muted-foreground flex items-center gap-2 text-xs">
                    <input
                      type="checkbox"
                      checked={rel.showingReblogs}
                      onChange={toggleReblogs}
                      disabled={relationshipBusy}
                      className="accent-primary size-3.5"
                    />
                    Show boosts
                  </label>
                )}
              </div>
            )}
          </div>
          {account.note && (
            <div
              className="mt-3 text-sm [&_a]:text-accent [&_a]:underline"
              dangerouslySetInnerHTML={{ __html: account.note }}
            />
          )}
          <div className="text-muted-foreground mt-3 mb-4 flex gap-4 text-sm">
            <span>
              <b className="text-foreground">{account.statusesCount}</b> posts
            </span>
            <Link
              to={`/@${account.acct}/following`}
              className="no-underline hover:underline"
            >
              <b className="text-foreground">{account.followingCount}</b> following
            </Link>
            <Link
              to={`/@${account.acct}/followers`}
              className="no-underline hover:underline"
            >
              <b className="text-foreground">{account.followersCount}</b> followers
            </Link>
          </div>

          <div className="space-y-2">
            {statuses === null && !error && (
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
              <p className="text-muted-foreground text-sm">No posts yet.</p>
            )}
            <InfiniteScroll
              onLoadMore={feed.loadMore}
              loading={feed.loadingMore}
              done={feed.done}
              hasItems={!!statuses?.length}
            />
          </div>
        </>
      )}
    </div>
  )
}
