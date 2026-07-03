import {
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type KeyboardEvent,
  type SyntheticEvent,
} from 'react'
import { Paperclip, X } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { postStatus, updateMediaDescription, uploadMedia } from '../api.ts'
import { getDefaultVisibility, getMeId, loadMe } from '../me.ts'
import { useMentionAutocomplete } from '../hooks/use-mention-autocomplete.ts'
import { Button } from '@/components/ui/button.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'
import { QuotedPost } from '@/components/quoted-post.tsx'
import { cn } from '@/lib/utils.ts'

const MAX_ATTACHMENTS = 4

// Seed a reply's text with the handles of everyone in the conversation, the way
// Mastodon's web client does (reducers/compose.js `statusToTextMentions`): the
// replied-to author first, then the status's other mentions, excluding oneself
// and de-duplicated. Without these handles in the body the server — which
// derives mentions by parsing @handles from the text — creates no mention row
// for the author, so they never get a reply/mention notification.
function statusToTextMentions(
  status: mastodon.v1.Status,
  meId: string | null,
): string {
  const handles: string[] = []
  const seen = new Set<string>()
  const add = (acct: string) => {
    if (seen.has(acct)) return
    seen.add(acct)
    handles.push(`@${acct}`)
  }
  if (status.account.id !== meId) add(status.account.acct)
  for (const mention of status.mentions) {
    if (mention.id !== meId) add(mention.acct)
  }
  return handles.length > 0 ? `${handles.join(' ')} ` : ''
}

export function Compose({
  token,
  replyTo,
  quoteOf,
  onCancelReply,
  onPosted,
  framed = true,
}: {
  token: string
  replyTo: mastodon.v1.Status | null
  quoteOf?: mastodon.v1.Status | null
  onCancelReply?: () => void
  onPosted: (status: mastodon.v1.Status) => void
  framed?: boolean
}) {
  const [text, setText] = useState('')
  const [caret, setCaret] = useState(0)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const [visibility, setVisibility] =
    useState<mastodon.v1.StatusVisibility>(() => getDefaultVisibility())
  const [attachments, setAttachments] = useState<mastodon.v1.MediaAttachment[]>([])
  const [busy, setBusy] = useState(false)
  const [uploading, setUploading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    let cancelled = false
    loadMe(token)
      .then((me) => {
        if (!cancelled && me) setVisibility(me.defaultVisibility)
      })
      .catch(() => {})

    return () => {
      cancelled = true
    }
  }, [token])

  // When a reply is opened, prefill the composer with the conversation's
  // handles (like Mastodon's COMPOSE_REPLY) and park the caret after them so
  // the user types their message after the mentions.
  useEffect(() => {
    if (!replyTo) return
    const seed = statusToTextMentions(replyTo, getMeId())
    setText(seed)
    setCaret(seed.length)
    requestAnimationFrame(() => {
      const el = textareaRef.current
      if (!el) return
      el.focus()
      el.setSelectionRange(seed.length, seed.length)
    })
    // Reseed only when the reply target changes, never on each keystroke.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [replyTo?.id])

  const onFiles = async (e: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? [])
    e.target.value = ''
    if (files.length === 0) return
    setUploading(true)
    setError(null)
    try {
      for (const file of files) {
        if (attachments.length >= MAX_ATTACHMENTS) break
        const media = await uploadMedia(file, token)
        setAttachments((prev) =>
          prev.length < MAX_ATTACHMENTS ? [...prev, media] : prev,
        )
      }
    } catch (err) {
      setError(String(err))
    } finally {
      setUploading(false)
    }
  }

  const removeAttachment = (id: string) =>
    setAttachments((prev) => prev.filter((a) => a.id !== id))

  const setAlt = (id: string, description: string) =>
    setAttachments((prev) =>
      prev.map((a) => (a.id === id ? { ...a, description } : a)),
    )

  const submit = async () => {
    if (
      (!text.trim() && attachments.length === 0 && !quoteOf) ||
      busy ||
      uploading
    )
      return
    setBusy(true)
    setError(null)
    try {
      const status = await postStatus(token, {
        status: text,
        visibility,
        inReplyToId: replyTo?.id,
        quotedStatusId: quoteOf?.id,
        mediaIds: attachments.map((a) => a.id),
      })
      setText('')
      setAttachments([])
      onPosted(status)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const mentions = useMentionAutocomplete({
    token,
    text,
    setText,
    caret,
    setCaret,
    textareaRef,
  })

  // Keep the tracked caret in sync as it moves (arrows, clicks, selection).
  const syncCaret = (e: SyntheticEvent<HTMLTextAreaElement>) =>
    setCaret(e.currentTarget.selectionStart ?? 0)

  // Cmd+Enter (macOS) / Ctrl+Enter posts, including replies. Checked before the
  // mention handler so the shortcut fires even while the autocomplete popup is
  // open; plain Enter still selects a suggestion there. submit() guards against
  // empty/in-flight posts, so calling it unconditionally is safe.
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      void submit()
      return
    }
    mentions.onKeyDown(e)
  }

  const canPost =
    (text.trim().length > 0 || attachments.length > 0 || !!quoteOf) && !uploading

  const content = (
    <CardContent className="space-y-2">
        {replyTo && (
          <div className="text-muted-foreground flex items-center justify-between text-xs">
            <span>Replying to @{replyTo.account.acct}</span>
            {onCancelReply && (
              <button className="underline" onClick={onCancelReply}>
                cancel
              </button>
            )}
          </div>
        )}
        {quoteOf && (
          <div className="space-y-1">
            <div className="text-muted-foreground flex items-center justify-between text-xs">
              <span>Quoting this post</span>
              {onCancelReply && (
                <button className="underline" onClick={onCancelReply}>
                  cancel
                </button>
              )}
            </div>
            <QuotedPost status={quoteOf} linked={false} />
          </div>
        )}
        <div className="relative">
          <textarea
            ref={textareaRef}
            className="bg-background focus-visible:ring-ring w-full resize-y rounded-md border p-2 text-sm outline-none focus-visible:ring-[3px]"
            rows={3}
            placeholder="What's on your mind?"
            value={text}
            onChange={(e) => {
              setText(e.target.value)
              setCaret(e.target.selectionStart ?? 0)
            }}
            onSelect={syncCaret}
            onKeyDown={onKeyDown}
          />
          {mentions.open && (
            <ul
              className="bg-popover absolute top-full right-0 left-0 z-50 mt-1 max-h-56 overflow-auto rounded-md border py-1 shadow-md"
              role="listbox"
            >
              {mentions.suggestions.map((a, i) => (
                <li key={a.id} role="option" aria-selected={i === mentions.active}>
                  <button
                    type="button"
                    // mousedown (not click) so the textarea keeps focus/caret.
                    onMouseDown={(e) => {
                      e.preventDefault()
                      mentions.select(a)
                    }}
                    onMouseEnter={() => mentions.setActive(i)}
                    className={cn(
                      'flex w-full items-center gap-2 px-2 py-1.5 text-left text-sm',
                      i === mentions.active && 'bg-accent',
                    )}
                  >
                    <img
                      src={a.avatar}
                      alt=""
                      className="size-6 shrink-0 rounded"
                    />
                    <span className="truncate font-medium">
                      {a.displayName || a.username}
                    </span>
                    <span className="text-muted-foreground truncate text-xs">
                      @{a.acct}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        {attachments.length > 0 && (
          <div className="grid grid-cols-2 gap-2">
            {attachments.map((a) => (
              <div key={a.id} className="relative rounded-md border p-1">
                <button
                  type="button"
                  onClick={() => removeAttachment(a.id)}
                  aria-label="Remove attachment"
                  className="bg-background/80 absolute top-1 right-1 z-10 p-0.5"
                >
                  <X className="size-4" />
                </button>
                {a.type === 'image' || a.type === 'gifv' || a.type === 'video' ? (
                  <img
                    src={a.previewUrl}
                    alt=""
                    className="h-24 w-full object-cover"
                  />
                ) : (
                  <div className="text-muted-foreground flex h-24 items-center justify-center text-xs">
                    {a.type}
                  </div>
                )}
                <input
                  value={a.description ?? ''}
                  onChange={(e) => setAlt(a.id, e.target.value)}
                  onBlur={() =>
                    updateMediaDescription(a.id, a.description ?? '', token).catch(
                      () => {},
                    )
                  }
                  placeholder="Describe for the visually impaired"
                  className="bg-background mt-1 w-full border px-1.5 py-1 text-xs"
                />
              </div>
            ))}
          </div>
        )}

        {error && <p className="text-destructive text-xs">{error}</p>}

        <input
          ref={fileRef}
          type="file"
          accept="image/*,video/*,audio/*"
          multiple
          hidden
          onChange={onFiles}
        />

        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label="Add media"
              disabled={uploading || attachments.length >= MAX_ATTACHMENTS}
              onClick={() => fileRef.current?.click()}
            >
              <Paperclip />
            </Button>
            <select
              className="bg-background rounded-md border px-2 py-1 text-xs"
              value={visibility}
              onChange={(e) =>
                setVisibility(e.target.value as mastodon.v1.StatusVisibility)
              }
            >
              <option value="public">Public</option>
              <option value="unlisted">Unlisted</option>
              <option value="private">Followers</option>
              <option value="direct">Direct</option>
            </select>
            {uploading && (
              <span className="text-muted-foreground text-xs">Uploading…</span>
            )}
          </div>
          <Button
            size="sm"
            disabled={busy || !canPost}
            onClick={submit}
            title={`${replyTo ? 'Reply' : quoteOf ? 'Quote' : 'Post'} (⌘/Ctrl + Enter)`}
          >
            {replyTo ? 'Reply' : quoteOf ? 'Quote' : 'Post'}
          </Button>
        </div>
    </CardContent>
  )

  if (!framed) return <div className="py-4">{content}</div>

  return (
    <Card>
      {content}
    </Card>
  )
}
