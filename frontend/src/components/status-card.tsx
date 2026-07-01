import { useState, type ReactNode } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  AtSign,
  Bookmark,
  Globe,
  Lock,
  LockOpen,
  MoreHorizontal,
  Pencil,
  Repeat2,
  Reply,
  Star,
  Trash2,
} from 'lucide-react'

import type { mastodon } from '../masto.ts'
import {
  deleteStatus,
  getStatusSource,
  setBookmark,
  setFavourite,
  setReblog,
  updateStatus,
} from '../api.ts'
import { getMeId } from '../me.ts'
import { Card, CardContent } from '@/components/ui/card.tsx'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx'
import { Button } from '@/components/ui/button.tsx'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.tsx'
import { MediaAttachments } from '@/components/media-attachments.tsx'
import { Poll } from '@/components/poll.tsx'
import { cn } from '@/lib/utils.ts'

const VISIBILITY: Record<
  string,
  { Icon: typeof Globe; label: string }
> = {
  public: { Icon: Globe, label: 'Public' },
  unlisted: { Icon: LockOpen, label: 'Unlisted' },
  private: { Icon: Lock, label: 'Followers only' },
  direct: { Icon: AtSign, label: 'Direct message' },
}

function VisibilityIcon({ v }: { v: mastodon.v1.StatusVisibility }) {
  const { Icon, label } = VISIBILITY[v] ?? VISIBILITY.public
  return (
    <span title={label} className="inline-flex" aria-label={label}>
      <Icon className="size-3.5" />
    </span>
  )
}

function ActionButton({
  icon,
  count,
  active,
  activeClass,
  disabled,
  label,
  onClick,
}: {
  icon: ReactNode
  count?: number
  active?: boolean
  activeClass?: string
  disabled?: boolean
  label: string
  onClick: () => void
}) {
  return (
    <Button
      variant="ghost"
      size="sm"
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
      className={cn('text-muted-foreground gap-1.5', active && activeClass)}
    >
      {icon}
      {count ? <span className="text-xs">{count}</span> : null}
    </Button>
  )
}

export function StatusCard({
  status: initial,
  token,
  boostedBy,
  onReply,
}: {
  status: mastodon.v1.Status
  token: string
  boostedBy?: mastodon.v1.Account
  onReply?: (status: mastodon.v1.Status) => void
}) {
  const [status, setStatus] = useState(initial)
  const [busy, setBusy] = useState(false)
  const [expanded, setExpanded] = useState(!initial.spoilerText)
  const [deleted, setDeleted] = useState(false)
  const [editing, setEditing] = useState(false)
  const [editText, setEditText] = useState('')
  const [editSpoiler, setEditSpoiler] = useState('')
  const [saving, setSaving] = useState(false)
  const navigate = useNavigate()

  const isOwn = !!token && getMeId() === status.account.id

  const startEdit = async () => {
    try {
      const src = await getStatusSource(status.id, token)
      setEditText(src.text)
      setEditSpoiler(src.spoilerText)
      setEditing(true)
    } catch {
      // ignore
    }
  }

  const saveEdit = async () => {
    if (saving) return
    setSaving(true)
    try {
      const updated = await updateStatus(
        status.id,
        { status: editText, spoilerText: editSpoiler },
        token,
      )
      setStatus(updated)
      setExpanded(!updated.spoilerText)
      setEditing(false)
    } catch {
      // ignore
    } finally {
      setSaving(false)
    }
  }

  const onDelete = async () => {
    if (!window.confirm('Delete this post?')) return
    try {
      await deleteStatus(status.id, token)
      setDeleted(true)
    } catch {
      // ignore
    }
  }

  // Boosting a status returns a reblog wrapper around the original; normalize
  // back to the underlying status so counts/flags stay on the displayed entity.
  const act = async (fn: () => Promise<mastodon.v1.Status>) => {
    if (busy || !token) return
    setBusy(true)
    try {
      const res = await fn()
      setStatus(res.reblog ?? res)
    } finally {
      setBusy(false)
    }
  }

  const name = status.account.displayName || status.account.username
  const profilePath = `/@${status.account.acct}`
  const threadPath = `/@${status.account.acct}/${status.id}`
  // Private and direct posts can't be boosted (matches Mastodon's web UI).
  const notBoostable =
    status.visibility === 'private' || status.visibility === 'direct'

  if (deleted) return null

  return (
    <Card>
      <CardContent className="space-y-2">
        {boostedBy && (
          <p className="text-muted-foreground flex items-center gap-1 text-xs">
            <Repeat2 className="size-3.5" />
            {boostedBy.displayName || boostedBy.username} boosted
          </p>
        )}
        <div className="flex items-center gap-2 text-sm">
          <Link to={profilePath}>
            <Avatar className="size-9 rounded-lg">
              <AvatarImage src={status.account.avatar} alt="" />
              <AvatarFallback>{name.slice(0, 1).toUpperCase()}</AvatarFallback>
            </Avatar>
          </Link>
          <Link to={profilePath} className="font-semibold no-underline hover:underline">
            {name}
          </Link>
          <span className="text-muted-foreground">@{status.account.acct}</span>
          <div className="text-muted-foreground ml-auto flex items-center gap-1 text-xs">
            <VisibilityIcon v={status.visibility} />
            <Link to={threadPath} className="no-underline hover:underline">
              {new Date(status.createdAt).toLocaleString()}
            </Link>
            {status.editedAt && <span title="Edited">(edited)</span>}
            {isOwn && (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    aria-label="More"
                    className="hover:text-foreground ml-1"
                  >
                    <MoreHorizontal className="size-4" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem onClick={startEdit}>
                    <Pencil /> Edit
                  </DropdownMenuItem>
                  <DropdownMenuItem variant="destructive" onClick={onDelete}>
                    <Trash2 /> Delete
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            )}
          </div>
        </div>
        {editing ? (
          <div className="space-y-2">
            <input
              value={editSpoiler}
              onChange={(e) => setEditSpoiler(e.target.value)}
              placeholder="Content warning (optional)"
              className="bg-background w-full rounded-md border px-2 py-1 text-sm outline-none"
            />
            <textarea
              value={editText}
              onChange={(e) => setEditText(e.target.value)}
              rows={4}
              className="bg-background focus-visible:ring-ring w-full resize-y rounded-md border p-2 text-sm outline-none focus-visible:ring-[3px]"
            />
            <div className="flex gap-2">
              <Button size="sm" disabled={saving || !editText.trim()} onClick={saveEdit}>
                Save
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setEditing(false)}>
                Cancel
              </Button>
            </div>
          </div>
        ) : (
          <>
            {status.spoilerText && (
              <div className="text-sm">
                <span>{status.spoilerText}</span>
                <button
                  type="button"
                  onClick={() => setExpanded((e) => !e)}
                  className="text-accent ml-2 text-xs underline"
                >
                  {expanded ? 'Show less' : 'Show more'}
                </button>
              </div>
            )}
            {expanded && (
              <>
                <div
                  className="text-sm [&_a]:text-accent [&_a]:underline"
                  dangerouslySetInnerHTML={{ __html: status.content }}
                />
                {status.mediaAttachments.length > 0 && (
                  <MediaAttachments
                    attachments={status.mediaAttachments}
                    sensitive={status.sensitive}
                  />
                )}
                {status.poll && <Poll poll={status.poll} token={token} />}
              </>
            )}
          </>
        )}
        <div className="flex items-center gap-1">
          <ActionButton
            icon={<Reply />}
            count={status.repliesCount}
            label="Reply"
            onClick={() => (onReply ? onReply(status) : navigate(threadPath))}
          />
          <ActionButton
            icon={notBoostable ? <Lock /> : <Repeat2 />}
            count={status.reblogsCount}
            active={status.reblogged ?? false}
            activeClass="text-primary"
            disabled={busy || !token || notBoostable}
            label={notBoostable ? 'Boosting not allowed' : 'Boost'}
            onClick={() => act(() => setReblog(token, status.id, !status.reblogged))}
          />
          <ActionButton
            icon={<Star />}
            count={status.favouritesCount}
            active={status.favourited ?? false}
            activeClass="text-yellow-500"
            disabled={busy || !token}
            label="Favourite"
            onClick={() => act(() => setFavourite(token, status.id, !status.favourited))}
          />
          <ActionButton
            icon={<Bookmark />}
            active={status.bookmarked ?? false}
            activeClass="text-secondary"
            disabled={busy || !token}
            label="Bookmark"
            onClick={() => act(() => setBookmark(token, status.id, !status.bookmarked))}
          />
        </div>
      </CardContent>
    </Card>
  )
}
