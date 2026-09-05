import { useEffect, useState } from 'react'
import { toast } from 'sonner'

import type { mastodon } from '../masto.ts'
import { fileReport } from '../api.ts'
import { errorMessage } from '@/lib/utils.ts'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog.tsx'
import { Label } from '@/components/ui/label.tsx'
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select.tsx'
import { Switch } from '@/components/ui/switch.tsx'
import { Textarea } from '@/components/ui/textarea.tsx'

type Category = 'spam' | 'violation' | 'other'

// The three the server stores. Upstream offers a rules-based option too, which
// forces the category to `violation` — eunha serves no rules, so there is
// nothing to pick and the plain category is the whole choice.
const CATEGORIES: Record<Category, string> = {
  spam: 'Spam or scam',
  violation: 'Breaks a server rule',
  other: 'Something else',
}

// Mastodon's Report::COMMENT_SIZE_LIMIT, which the server enforces too.
const COMMENT_LIMIT = 1000

export function ReportDialog({
  account,
  status,
  open,
  onOpenChange,
  token,
}: {
  account: mastodon.v1.Account
  // The post that prompted the report, if it started from one. It is sent for
  // context; a report is always against the account.
  status?: mastodon.v1.Status
  open: boolean
  onOpenChange: (open: boolean) => void
  token: string
}) {
  const [category, setCategory] = useState<Category>('other')
  const [comment, setComment] = useState('')
  const [forward, setForward] = useState(false)
  const [sending, setSending] = useState(false)

  // A fresh dialog each time it opens — a half-written report from the last
  // account is not a draft worth keeping.
  useEffect(() => {
    if (open) {
      setCategory('other')
      setComment('')
      setForward(false)
    }
  }, [open])

  // `acct` carries a domain only for remote accounts, and forwarding is only
  // meaningful for those: there is no other server to tell.
  const domain = account.acct.includes('@') ? account.acct.split('@')[1] : null

  const submit = async () => {
    if (sending) return
    setSending(true)
    try {
      await fileReport(token, {
        accountId: account.id,
        statusIds: status ? [status.id] : undefined,
        comment: comment.trim() || undefined,
        forward: domain ? forward : undefined,
        category,
      })
      toast.success(`Reported @${account.acct}. Moderators will take a look.`)
      onOpenChange(false)
    } catch (e) {
      toast.error(errorMessage(e))
    } finally {
      setSending(false)
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Report @{account.acct}?</AlertDialogTitle>
          <AlertDialogDescription>
            {status
              ? 'This post is sent with the report so moderators can see what prompted it.'
              : 'Moderators on this server will see the report.'}
          </AlertDialogDescription>
        </AlertDialogHeader>

        <div className="space-y-3">
          <div className="space-y-1">
            <Label>Reason</Label>
            <Select
              items={CATEGORIES}
              value={category}
              onValueChange={(v) => setCategory((v as Category) ?? 'other')}
            >
              <SelectTrigger className="w-full" aria-label="Reason">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {(Object.keys(CATEGORIES) as Category[]).map((key) => (
                    <SelectItem key={key} value={key}>
                      {CATEGORIES[key]}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-1">
            <Label htmlFor="report-comment">Anything else? (optional)</Label>
            <Textarea
              id="report-comment"
              value={comment}
              maxLength={COMMENT_LIMIT}
              onChange={(e) => setComment(e.target.value)}
              rows={3}
              className="resize-y"
              placeholder="What should a moderator know?"
            />
            <p className="text-muted-foreground text-xs">
              {comment.length}/{COMMENT_LIMIT}
            </p>
          </div>

          {domain && (
            <Label className="text-sm font-normal">
              <Switch size="sm" checked={forward} onCheckedChange={setForward} />
              Also send this to {domain}
            </Label>
          )}
        </div>

        <AlertDialogFooter>
          <AlertDialogCancel disabled={sending}>Cancel</AlertDialogCancel>
          <AlertDialogAction variant="destructive" disabled={sending} onClick={submit}>
            {sending ? 'Reporting…' : 'Report'}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
