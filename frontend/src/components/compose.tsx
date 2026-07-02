import { useRef, useState, type ChangeEvent } from 'react'
import { Paperclip, X } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { postStatus, updateMediaDescription, uploadMedia } from '../api.ts'
import { Button } from '@/components/ui/button.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'

const MAX_ATTACHMENTS = 4

export function Compose({
  token,
  replyTo,
  onCancelReply,
  onPosted,
  framed = true,
}: {
  token: string
  replyTo: mastodon.v1.Status | null
  onCancelReply?: () => void
  onPosted: (status: mastodon.v1.Status) => void
  framed?: boolean
}) {
  const [text, setText] = useState('')
  const [visibility, setVisibility] =
    useState<mastodon.v1.StatusVisibility>('public')
  const [attachments, setAttachments] = useState<mastodon.v1.MediaAttachment[]>([])
  const [busy, setBusy] = useState(false)
  const [uploading, setUploading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)

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
    if ((!text.trim() && attachments.length === 0) || busy || uploading) return
    setBusy(true)
    setError(null)
    try {
      const status = await postStatus(token, {
        status: text,
        visibility,
        inReplyToId: replyTo?.id,
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

  const canPost = (text.trim().length > 0 || attachments.length > 0) && !uploading

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
        <textarea
          className="bg-background focus-visible:ring-ring w-full resize-y rounded-md border p-2 text-sm outline-none focus-visible:ring-[3px]"
          rows={3}
          placeholder="What's on your mind?"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />

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
          <Button size="sm" disabled={busy || !canPost} onClick={submit}>
            {replyTo ? 'Reply' : 'Post'}
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
