import { useEffect, useState } from 'react'
import { NavLink, useLocation, useParams } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import { getFollowers, getFollowing, lookupAccount } from '../api.ts'
import { getToken } from '../auth.ts'
import { useInfiniteFeed } from '../hooks/use-infinite-feed.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { AccountRow } from '@/components/account-row.tsx'
import { InfiniteScroll } from '@/components/infinite-scroll.tsx'
import { cn } from '@/lib/utils.ts'

const tab =
  'border-b-2 border-transparent px-3 py-2 text-sm font-medium text-muted-foreground no-underline hover:text-foreground'
const cls = ({ isActive }: { isActive: boolean }) =>
  cn(tab, isActive && 'border-primary text-foreground')

export default function AccountList() {
  const { acct = '' } = useParams()
  const handle = acct.replace(/^@/, '')
  const following = useLocation().pathname.endsWith('/following')
  const token = getToken()

  const [account, setAccount] = useState<mastodon.v1.Account | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setAccount(null)
    setError(null)
    lookupAccount(handle, token ?? undefined)
      .then(setAccount)
      .catch((e) => setError(String(e)))
  }, [handle, token])

  const feed = useInfiniteFeed<mastodon.v1.Account>(
    (maxId) =>
      account
        ? following
          ? getFollowing(account.id, token ?? undefined, maxId)
          : getFollowers(account.id, token ?? undefined, maxId)
        : Promise.resolve([]),
    [account?.id, following, token],
  )
  const accounts = feed.items

  return (
    <div className="page-frame">
      <TopBar />
      <h1 className="mb-2 text-lg font-bold">@{handle}</h1>
      <nav className="mb-2 flex gap-1 border-b">
        <NavLink to={`/@${handle}/followers`} className={cls}>
          Followers
        </NavLink>
        <NavLink to={`/@${handle}/following`} className={cls}>
          Following
        </NavLink>
      </nav>
      {error && <p className="text-destructive text-sm">{error}</p>}
      <div className="space-y-1">
        {accounts === null && !error && (
          <p className="text-muted-foreground text-sm">Loading…</p>
        )}
        {accounts?.map((a) => (
          <AccountRow key={a.id} account={a} />
        ))}
        {accounts?.length === 0 && (
          <p className="text-muted-foreground text-sm">Nobody here yet.</p>
        )}
        <InfiniteScroll
          onLoadMore={feed.loadMore}
          loading={feed.loadingMore}
          done={feed.done}
          hasItems={!!accounts?.length}
        />
      </div>
    </div>
  )
}
