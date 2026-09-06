import {
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type KeyboardEvent,
  type SyntheticEvent,
} from 'react'
import { LockOpen, Paperclip, X } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { postStatus, updateMediaDescription, uploadMedia } from '../api.ts'
import { getDefaultVisibility, getMeAccount, getMeId, loadMe } from '../me.ts'
import { useMentionAutocomplete } from '../hooks/use-mention-autocomplete.ts'
import { Button } from '@/components/ui/button.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'
import { Input } from '@/components/ui/input.tsx'
import { Textarea } from '@/components/ui/textarea.tsx'
import type { QuotePolicy } from '../api.ts'
import { QuotedPost } from '@/components/quoted-post.tsx'
import { ComposeAudience } from '@/components/compose-audience.tsx'
import { ComposeHints } from '@/components/compose-hints.tsx'
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

// Mastodon's `privacyPreference`: a reply starts at whichever of the parent's
// visibility and your own default is the more private. Replying to a
// followers-only post with a public post by default is how a private thread
// leaks, and it is not what upstream does.
const PRIVACY_ORDER: mastodon.v1.StatusVisibility[] = [
  'public',
  'unlisted',
  'private',
  'direct',
]

function morePrivate(
  a: mastodon.v1.StatusVisibility | undefined,
  b: mastodon.v1.StatusVisibility,
): mastodon.v1.StatusVisibility {
  if (!a) return b
  return PRIVACY_ORDER.indexOf(a) > PRIVACY_ORDER.indexOf(b) ? a : b
}

export function Compose({
  token,
  replyTo,
  quoteOf,
  messageTo,
  onCancelReply,
  onPosted,
  onTypeChange,
  framed = true,
}: {
  token: string
  replyTo: mastodon.v1.Status | null
  quoteOf?: mastodon.v1.Status | null
  // Opens the composer as a message: direct visibility, and the recipient's
  // handle already typed if one was named. `undefined` means "not a message";
  // `null` means "a message to nobody in particular yet".
  messageTo?: mastodon.v1.Account | null
  onCancelReply?: () => void
  onPosted: (status: mastodon.v1.Status) => void
  // The frame draws the title and the tint, but the mode is decided in here
  // now that the audience menu can switch it.
  onTypeChange?: (type: 'post' | 'reply' | 'message') => void
  framed?: boolean
}) {
  const [text, setText] = useState('')
  const [caret, setCaret] = useState(0)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const [visibility, setVisibility] = useState<mastodon.v1.StatusVisibility>(() =>
    messageTo !== undefined
      ? 'direct'
      : morePrivate(replyTo?.visibility, getDefaultVisibility()),
  )
  const [quotePolicy, setQuotePolicy] = useState<QuotePolicy>('public')
  // Derived, not stored — picking Direct in the audience menu *is* switching to
  // a message, and a reply stays a reply either way.
  const isMessage = !replyTo && visibility === 'direct'
  const [attachments, setAttachments] = useState<mastodon.v1.MediaAttachment[]>([])
  const [busy, setBusy] = useState(false)
  const [uploading, setUploading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)

  // The account's default visibility seeds a post once it loads — but never a
  // message, whose audience is fixed by being a message at all. Without this
  // guard the default arrives a moment after mount and quietly turns a direct
  // message into whatever the account posts by default.
  useEffect(() => {
    if (messageTo !== undefined) return
    let cancelled = false
    loadMe(token)
      .then((me) => {
        // Same trap as message mode: this lands after mount, so it has to
        // respect the parent's visibility rather than overwrite it.
        if (!cancelled && me) {
          setVisibility(morePrivate(replyTo?.visibility, me.defaultVisibility))
        }
      })
      .catch(() => {})

    return () => {
      cancelled = true
    }
  }, [token, messageTo, replyTo?.visibility])

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

  // A message opened against a person starts with their handle, the same way a
  // reply starts with the thread's. Upstream calls this `directCompose`.
  useEffect(() => {
    if (!messageTo) return
    const seed = `@${messageTo.acct} `
    setText(seed)
    setCaret(seed.length)
    requestAnimationFrame(() => {
      const el = textareaRef.current
      if (!el) return
      el.focus()
      el.setSelectionRange(seed.length, seed.length)
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [messageTo?.id])

  const label = replyTo ? 'Reply' : isMessage ? 'Send' : quoteOf ? 'Quote' : 'Publish'

  useEffect(() => {
    onTypeChange?.(replyTo ? 'reply' : isMessage ? 'message' : 'post')
  }, [replyTo, isMessage, onTypeChange])

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
        // A post only its followers can see is one only its author can quote,
        // which is what the menu shows by disabling the toggle.
        quoteApprovalPolicy:
          visibility === 'private' || visibility === 'direct' ? 'nobody' : quotePolicy,
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
    <CardContent className="space-y-2 px-4">
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
        <div className="flex flex-wrap items-center gap-2">
          <ComposeAudience
            visibility={visibility}
            onVisibilityChange={setVisibility}
            quotePolicy={quotePolicy}
            onQuotePolicyChange={setQuotePolicy}
            text={text}
            replyTo={replyTo}
            meAcct={getMeAccount()?.acct ?? null}
            defaultVisibility={getDefaultVisibility()}
            disabled={busy}
          />
        </div>

        {isMessage && (
          <p className="text-muted-foreground flex items-center gap-1.5 text-xs">
            <LockOpen className="size-3.5 shrink-0" />
            Messages are not end-to-end encrypted.
          </p>
        )}

        <div className="relative">
          <Textarea
            ref={textareaRef}
            className="resize-y transition-[border-color,box-shadow] duration-200 motion-reduce:transition-none"
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
                <Input
                  value={a.description ?? ''}
                  onChange={(e) => setAlt(a.id, e.target.value)}
                  onBlur={() =>
                    updateMediaDescription(a.id, a.description ?? '', token).catch(
                      () => {},
                    )
                  }
                  placeholder="Describe for the visually impaired"
                  className="mt-1 h-7 text-xs"
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

        <ComposeHints replyTo={replyTo} attachments={attachments} />

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
            {uploading && (
              <span className="text-muted-foreground motion-safe:animate-pulse text-xs">
                Uploading…
              </span>
            )}
          </div>
          <Button
            size="sm"
            disabled={busy || !canPost}
            onClick={submit}
            title={`${label} (⌘/Ctrl + Enter)`}
          >
            {label}
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
