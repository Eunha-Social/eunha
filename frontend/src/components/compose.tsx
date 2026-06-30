import { useState } from 'react'

import type { mastodon } from '../masto.ts'
import { postStatus } from '../api.ts'
import { Button } from '@/components/ui/button.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'

export function Compose({
  token,
  replyTo,
  onCancelReply,
  onPosted,
}: {
  token: string
  replyTo: mastodon.v1.Status | null
  onCancelReply: () => void
  onPosted: (status: mastodon.v1.Status) => void
}) {
  const [text, setText] = useState('')
  const [visibility, setVisibility] =
    useState<mastodon.v1.StatusVisibility>('public')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submit = async () => {
    if (!text.trim() || busy) return
    setBusy(true)
    setError(null)
    try {
      const status = await postStatus(token, {
        status: text,
        visibility,
        inReplyToId: replyTo?.id,
      })
      setText('')
      onPosted(status)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Card>
      <CardContent className="space-y-2">
        {replyTo && (
          <div className="text-muted-foreground flex items-center justify-between text-xs">
            <span>Replying to @{replyTo.account.acct}</span>
            <button className="underline" onClick={onCancelReply}>
              cancel
            </button>
          </div>
        )}
        <textarea
          className="bg-background focus-visible:ring-ring w-full resize-y rounded-md border p-2 text-sm outline-none focus-visible:ring-[3px]"
          rows={3}
          placeholder="What's on your mind?"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        {error && <p className="text-destructive text-xs">{error}</p>}
        <div className="flex items-center justify-between">
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
          <Button size="sm" disabled={busy || !text.trim()} onClick={submit}>
            {replyTo ? 'Reply' : 'Post'}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
