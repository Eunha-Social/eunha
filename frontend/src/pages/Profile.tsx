import { useEffect, useRef, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { ImageUp, Trash2 } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import {
  deleteProfileAvatar,
  deleteProfileHeader,
  getAccountStatuses,
  getCurrentAccount,
  getRelationship,
  lookupAccount,
  setFollow,
  updateAccountImages,
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
  const [imageBusy, setImageBusy] = useState<'avatar' | 'header' | null>(null)
  const [imageError, setImageError] = useState<string | null>(null)
  const avatarInputRef = useRef<HTMLInputElement>(null)
  const headerInputRef = useRef<HTMLInputElement>(null)

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
    setImageError(null)
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

  const applyUpdatedAccount = (updated: mastodon.v1.Account) => {
    setAccount((current) =>
      current
        ? {
            ...current,
            avatar: updated.avatar,
            avatarStatic: updated.avatarStatic,
            header: updated.header,
            headerStatic: updated.headerStatic,
          }
        : updated,
    )
  }

  const updateProfileImage = async (
    kind: 'avatar' | 'header',
    file: File | null,
  ) => {
    if (!token || !isSelf || imageBusy) return
    setImageBusy(kind)
    setImageError(null)
    try {
      const updated = file
        ? await updateAccountImages(token, { [kind]: file })
        : kind === 'avatar'
          ? await deleteProfileAvatar(token)
          : await deleteProfileHeader(token)
      applyUpdatedAccount(updated)
    } catch (e) {
      setImageError(e instanceof Error ? e.message : String(e))
    } finally {
      setImageBusy(null)
    }
  }

  const onImageSelected =
    (kind: 'avatar' | 'header') => (event: React.ChangeEvent<HTMLInputElement>) => {
      const file = event.currentTarget.files?.[0] ?? null
      event.currentTarget.value = ''
      if (file) void updateProfileImage(kind, file)
    }

  return (
    <div className="page-frame">
      <TopBar />
      {error && <p className="text-destructive text-sm">{error}</p>}
      {account && (
        <>
          {isSelf && (
            <>
              <input
                ref={avatarInputRef}
                type="file"
                accept="image/*"
                className="hidden"
                onChange={onImageSelected('avatar')}
              />
              <input
                ref={headerInputRef}
                type="file"
                accept="image/*"
                className="hidden"
                onChange={onImageSelected('header')}
              />
            </>
          )}
          {hasCustomHeader(account.header) && (
            <div className="relative">
              <img
                src={account.header}
                alt=""
                className="h-32 w-full rounded-xl object-cover"
              />
              {isSelf && (
                <div className="absolute top-2 right-2 flex gap-1">
                  <Button
                    size="icon"
                    variant="outline"
                    className="bg-background/90 size-8"
                    aria-label="Upload header image"
                    disabled={imageBusy !== null}
                    onClick={() => headerInputRef.current?.click()}
                  >
                    <ImageUp />
                  </Button>
                  <Button
                    size="icon"
                    variant="outline"
                    className="bg-background/90 size-8"
                    aria-label="Remove header image"
                    disabled={imageBusy !== null}
                    onClick={() => void updateProfileImage('header', null)}
                  >
                    <Trash2 />
                  </Button>
                </div>
              )}
            </div>
          )}
          {!hasCustomHeader(account.header) && isSelf && (
            <div className="mb-2 flex justify-end">
              <Button
                size="sm"
                variant="outline"
                disabled={imageBusy !== null}
                onClick={() => headerInputRef.current?.click()}
              >
                <ImageUp /> Header
              </Button>
            </div>
          )}
          <div className="mt-3 flex items-start gap-3">
            <div className="flex flex-col items-center gap-1">
              <Avatar className="size-16 rounded-xl">
                <AvatarImage src={account.avatar} alt="" />
                <AvatarFallback>
                  {(account.displayName || account.username).slice(0, 1).toUpperCase()}
                </AvatarFallback>
              </Avatar>
              {isSelf && (
                <div className="flex gap-1">
                  <Button
                    size="icon"
                    variant="ghost"
                    className="size-7"
                    aria-label="Upload avatar image"
                    disabled={imageBusy !== null}
                    onClick={() => avatarInputRef.current?.click()}
                  >
                    <ImageUp />
                  </Button>
                  <Button
                    size="icon"
                    variant="ghost"
                    className="size-7"
                    aria-label="Remove avatar image"
                    disabled={imageBusy !== null}
                    onClick={() => void updateProfileImage('avatar', null)}
                  >
                    <Trash2 />
                  </Button>
                </div>
              )}
            </div>
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
          {imageError && (
            <p className="text-destructive mt-2 text-sm">{imageError}</p>
          )}
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
