import { useEffect, useState, type ReactNode } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import {
  AtSign,
  Bookmark,
  ExternalLink,
  Globe,
  Lock,
  LockOpen,
  MoreHorizontal,
  Pencil,
  Quote,
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
import { Input } from '@/components/ui/input.tsx'
import { Textarea } from '@/components/ui/textarea.tsx'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.tsx'
import { MediaAttachments } from '@/components/media-attachments.tsx'
import { Poll } from '@/components/poll.tsx'
import { QuotedPost } from '@/components/quoted-post.tsx'
import { RelativeTime } from '@/components/relative-time.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'
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

// A quote that isn't accepted shows a status message in place of the embedded
// post. Copy mirrors Mastodon's web client (status_quoted.tsx): pending quotes
// stay hidden until the original author's server approves them, so the quoted
// content is never revealed early.
const QUOTE_PLACEHOLDER: Record<string, string> = {
  pending: 'Post pending',
  revoked: 'Post removed by author',
  rejected: 'Post unavailable',
  deleted: 'Post unavailable',
  unauthorized: 'Post unavailable',
  blocked_account: "This post is hidden because you've blocked this account.",
  blocked_domain: "This post is hidden because you've blocked this domain.",
  muted_account: "This post is hidden because you've muted this account.",
}

// The post embedded by a quote. Like Mastodon, the quoted post is only rendered
// once the quote is accepted; other states (pending approval, revoked, deleted,
// blocked/muted, …) show a message instead.
function QuotedStatus({
  quote,
}: {
  quote: NonNullable<mastodon.v1.Status['quote']>
}) {
  const quoted = 'quotedStatus' in quote ? quote.quotedStatus : null
  if (quote.state !== 'accepted' || !quoted) {
    return (
      <div className="text-muted-foreground rounded-md border px-3 py-2 text-xs">
        {QUOTE_PLACEHOLDER[quote.state] ?? 'Post unavailable'}
      </div>
    )
  }
  return <QuotedPost status={quoted} />
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
  const { openCompose } = useComposeModal()

  const isOwn = !!token && getMeId() === status.account.id
  // Whether the viewer may quote this post. "manual" still allows quoting (the
  // quote starts pending the author's approval); "unknown" means the server
  // didn't compute a viewer policy for this response (e.g. search-by-URL
  // results), so allow it optimistically — the server validates on create.
  // Only an explicit "denied", or a private/direct post, hides the option.
  const currentUserPolicy = status.quoteApproval?.currentUser
  const canQuote =
    !!token &&
    currentUserPolicy !== 'denied' &&
    status.visibility !== 'private' &&
    status.visibility !== 'direct'
  // A post that came from another instance: `acct` carries the domain only for
  // remote accounts. `url` is then the post's page on that instance, which is
  // where its full context lives — offer it the way Mastodon's web client does.
  const remoteUrl = status.account.acct.includes('@') ? status.url : null
  const hasMenu = isOwn || canQuote || !!remoteUrl

  useEffect(() => {
    setStatus(initial)
    setExpanded(!initial.spoilerText)
  }, [initial])

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
    <Card className="gap-0 rounded-none border-0 bg-transparent py-0 shadow-none ring-0">
      <CardContent className="space-y-1.5 px-3 py-3 sm:px-4">
        {boostedBy && (
          <p className="text-muted-foreground flex items-center gap-1 text-xs">
            <Repeat2 className="size-3.5" />
            {boostedBy.displayName || boostedBy.username} boosted
          </p>
        )}
        <div className="flex items-center gap-2 text-sm">
          <Link to={profilePath}>
            <Avatar className="size-8">
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
              <RelativeTime value={status.createdAt} />
            </Link>
            {status.editedAt && <span title="Edited">(edited)</span>}
            {hasMenu && (
              <DropdownMenu>
                <DropdownMenuTrigger
                  aria-label="More"
                  className="hover:text-foreground ml-1"
                >
                  <MoreHorizontal className="size-4" />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {remoteUrl && (
                    <DropdownMenuItem
                      className="no-underline"
                      render={
                        <a
                          href={remoteUrl}
                          target="_blank"
                          rel="noopener noreferrer"
                        />
                      }
                    >
                      <ExternalLink /> Open original page
                    </DropdownMenuItem>
                  )}
                  {canQuote && (
                    <DropdownMenuItem
                      onClick={() => openCompose({ quoteOf: status })}
                    >
                      <Quote /> Quote
                    </DropdownMenuItem>
                  )}
                  {isOwn && (
                    <>
                      <DropdownMenuItem onClick={startEdit}>
                        <Pencil /> Edit
                      </DropdownMenuItem>
                      <DropdownMenuItem variant="destructive" onClick={onDelete}>
                        <Trash2 /> Delete
                      </DropdownMenuItem>
                    </>
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
            )}
          </div>
        </div>
        {editing ? (
          <div className="space-y-2">
            <Input
              value={editSpoiler}
              onChange={(e) => setEditSpoiler(e.target.value)}
              placeholder="Content warning (optional)"
            />
            <Textarea
              value={editText}
              onChange={(e) => setEditText(e.target.value)}
              rows={4}
              className="resize-y"
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
                  className="text-primary ml-2 text-xs font-medium underline"
                >
                  {expanded ? 'Show less' : 'Show more'}
                </button>
              </div>
            )}
            {expanded && (
              <>
                <div
                  className="text-sm [&_a]:font-medium [&_a]:text-primary [&_a]:underline"
                  dangerouslySetInnerHTML={{ __html: status.content }}
                />
                {status.mediaAttachments.length > 0 && (
                  <MediaAttachments
                    attachments={status.mediaAttachments}
                    sensitive={status.sensitive}
                  />
                )}
                {status.poll && <Poll poll={status.poll} token={token} />}
                {status.quote && <QuotedStatus quote={status.quote} />}
              </>
            )}
          </>
        )}
        <div className="-mb-1 flex items-center gap-1">
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
