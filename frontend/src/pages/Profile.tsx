import { useEffect, useRef, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { ImageUp, Trash2, UserPlus } from 'lucide-react'

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
import { Label } from '@/components/ui/label.tsx'
import { Switch } from '@/components/ui/switch.tsx'

function hasCustomHeader(header: string | null | undefined) {
  return !!header && !header.includes('/headers/original/missing')
}

type ImageKind = 'avatar' | 'header'

interface CropDraft {
  kind: ImageKind
  file: File
  url: string
}

function cropConfig(kind: ImageKind) {
  return kind === 'avatar'
    ? { aspect: 1, width: 400, height: 400 }
    : { aspect: 3, width: 1500, height: 500 }
}

function ProfileImageCropModal({
  draft,
  onCancel,
  onComplete,
}: {
  draft: CropDraft
  onCancel: () => void
  onComplete: (file: File) => void
}) {
  const config = cropConfig(draft.kind)
  const frameRef = useRef<HTMLDivElement>(null)
  const imageRef = useRef<HTMLImageElement>(null)
  const dragRef = useRef<{ pointerId: number; x: number; y: number } | null>(null)
  const [imageSize, setImageSize] = useState({ width: 0, height: 0 })
  const [pan, setPan] = useState({ x: 0, y: 0 })
  const [zoom, setZoom] = useState(1)
  const [busy, setBusy] = useState(false)

  const clampPan = (next: { x: number; y: number }, nextZoom = zoom) => {
    const frame = frameRef.current
    if (!frame || imageSize.width === 0 || imageSize.height === 0) return next

    const frameWidth = frame.clientWidth
    const frameHeight = frame.clientHeight
    const scale = Math.max(
      frameWidth / imageSize.width,
      frameHeight / imageSize.height,
    ) * nextZoom
    const renderedWidth = imageSize.width * scale
    const renderedHeight = imageSize.height * scale
    const maxX = Math.max(0, (renderedWidth - frameWidth) / 2)
    const maxY = Math.max(0, (renderedHeight - frameHeight) / 2)

    return {
      x: Math.min(maxX, Math.max(-maxX, next.x)),
      y: Math.min(maxY, Math.max(-maxY, next.y)),
    }
  }

  useEffect(() => {
    setPan((current) => clampPan(current))
    // Re-clamp when the source image changes size.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imageSize.width, imageSize.height])

  const complete = async () => {
    const frame = frameRef.current
    const image = imageRef.current
    if (!frame || !image || imageSize.width === 0 || imageSize.height === 0) return

    setBusy(true)
    try {
      const frameWidth = frame.clientWidth
      const frameHeight = frame.clientHeight
      const scale = Math.max(
        frameWidth / imageSize.width,
        frameHeight / imageSize.height,
      ) * zoom
      const renderedWidth = imageSize.width * scale
      const renderedHeight = imageSize.height * scale
      const left = (frameWidth - renderedWidth) / 2 + pan.x
      const top = (frameHeight - renderedHeight) / 2 + pan.y
      const sourceX = Math.max(0, -left / scale)
      const sourceY = Math.max(0, -top / scale)
      const sourceWidth = Math.min(imageSize.width - sourceX, frameWidth / scale)
      const sourceHeight = Math.min(imageSize.height - sourceY, frameHeight / scale)

      const canvas = document.createElement('canvas')
      canvas.width = config.width
      canvas.height = config.height
      const ctx = canvas.getContext('2d')
      if (!ctx) throw new Error('Could not prepare image crop')
      ctx.imageSmoothingQuality = 'high'
      ctx.drawImage(
        image,
        sourceX,
        sourceY,
        sourceWidth,
        sourceHeight,
        0,
        0,
        config.width,
        config.height,
      )
      const blob = await new Promise<Blob>((resolve, reject) => {
        canvas.toBlob(
          (result) => {
            if (result) resolve(result)
            else reject(new Error('Could not export image crop'))
          },
          draft.file.type || 'image/png',
          0.92,
        )
      })
      onComplete(new File([blob], draft.file.name, { type: blob.type }))
    } catch {
      setBusy(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 px-3 py-8">
      <div className="bg-background w-full max-w-xl border shadow-lg">
        <div className="flex items-center justify-between border-b px-4 py-3">
          <div className="font-semibold">
            {draft.kind === 'avatar' ? 'Crop avatar' : 'Crop header'}
          </div>
          <Button variant="ghost" size="sm" onClick={onCancel} disabled={busy}>
            Cancel
          </Button>
        </div>
        <div className="space-y-4 p-4">
          <div
            ref={frameRef}
            className="bg-muted relative w-full touch-none overflow-hidden border"
            style={{ aspectRatio: String(config.aspect) }}
            onPointerDown={(event) => {
              event.currentTarget.setPointerCapture(event.pointerId)
              dragRef.current = {
                pointerId: event.pointerId,
                x: event.clientX,
                y: event.clientY,
              }
            }}
            onPointerMove={(event) => {
              const drag = dragRef.current
              if (!drag || drag.pointerId !== event.pointerId) return
              const dx = event.clientX - drag.x
              const dy = event.clientY - drag.y
              dragRef.current = { ...drag, x: event.clientX, y: event.clientY }
              setPan((current) => clampPan({ x: current.x + dx, y: current.y + dy }))
            }}
            onPointerUp={(event) => {
              if (dragRef.current?.pointerId === event.pointerId) dragRef.current = null
            }}
            onPointerCancel={() => {
              dragRef.current = null
            }}
          >
            <img
              ref={imageRef}
              src={draft.url}
              alt=""
              draggable={false}
              className="absolute top-1/2 left-1/2 max-w-none select-none"
              style={{
                width: imageSize.width
                  ? `${imageSize.width * Math.max(
                      (frameRef.current?.clientWidth ?? 1) / imageSize.width,
                      (frameRef.current?.clientHeight ?? 1) / imageSize.height,
                    ) * zoom}px`
                  : undefined,
                transform: `translate(calc(-50% + ${pan.x}px), calc(-50% + ${pan.y}px))`,
              }}
              onLoad={(event) => {
                setImageSize({
                  width: event.currentTarget.naturalWidth,
                  height: event.currentTarget.naturalHeight,
                })
              }}
            />
          </div>
          <label className="grid gap-2 text-sm">
            <span className="text-muted-foreground">Zoom</span>
            <input
              type="range"
              min="1"
              max="3"
              step="0.01"
              value={zoom}
              onChange={(event) => {
                const nextZoom = event.currentTarget.valueAsNumber
                setZoom(nextZoom)
                setPan((current) => clampPan(current, nextZoom))
              }}
            />
          </label>
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={onCancel} disabled={busy}>
              Cancel
            </Button>
            <Button onClick={() => void complete()} disabled={busy || imageSize.width === 0}>
              Save
            </Button>
          </div>
        </div>
      </div>
    </div>
  )
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
  const [cropDraft, setCropDraft] = useState<CropDraft | null>(null)
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
      if (!file) return
      if (file.type === 'image/gif') {
        void updateProfileImage(kind, file)
        return
      }
      setCropDraft({ kind, file, url: URL.createObjectURL(file) })
    }

  const closeCrop = () => {
    if (cropDraft) URL.revokeObjectURL(cropDraft.url)
    setCropDraft(null)
  }

  return (
    <div className="page-frame">
      <TopBar />
      {cropDraft && (
        <ProfileImageCropModal
          draft={cropDraft}
          onCancel={closeCrop}
          onComplete={(file) => {
            const kind = cropDraft.kind
            closeCrop()
            void updateProfileImage(kind, file)
          }}
        />
      )}
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
              <Avatar className="size-16">
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
            {isSelf && (
              <Button
                render={<Link to="/follow-requests" className="no-underline" />}
                size="sm"
                variant="outline"
              >
                <UserPlus /> Follow requests
              </Button>
            )}
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
                  <Label className="text-xs font-normal">
                    <Switch
                      size="sm"
                      checked={rel.showingReblogs}
                      onCheckedChange={toggleReblogs}
                      disabled={relationshipBusy}
                    />
                    Show boosts
                  </Label>
                )}
              </div>
            )}
          </div>
          {imageError && (
            <p className="text-destructive mt-2 text-sm">{imageError}</p>
          )}
          {account.note && (
            <div
              className="mt-3 text-sm [&_a]:font-medium [&_a]:text-primary [&_a]:underline [&_a]:decoration-primary [&_a]:decoration-2 [&_a]:underline-offset-2"
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
