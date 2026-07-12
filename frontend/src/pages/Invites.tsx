import { useEffect, useState } from 'react'
import { Copy, Trash2 } from 'lucide-react'
import { toast } from 'sonner'

import {
  createInvite,
  deleteInvite,
  getInvites,
  type Invite,
} from '../eunha-api.ts'
import { beginLogin, getToken } from '../auth.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { Button } from '@/components/ui/button.tsx'
import { Input } from '@/components/ui/input.tsx'
import { Label } from '@/components/ui/label.tsx'
import { Switch } from '@/components/ui/switch.tsx'
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select.tsx'

// Mirrors Mastodon's MAX_USES_COUNTS / EXPIRATION_DURATIONS. "0" means
// unlimited / never and is omitted from the request.
const MAX_USES: Record<string, string> = {
  '0': 'Unlimited uses',
  '1': '1 use',
  '5': '5 uses',
  '10': '10 uses',
  '25': '25 uses',
  '50': '50 uses',
  '100': '100 uses',
}
const EXPIRES_IN: Record<string, string> = {
  '0': 'Never expires',
  '1800': '30 minutes',
  '3600': '1 hour',
  '21600': '6 hours',
  '43200': '12 hours',
  '86400': '1 day',
  '604800': '1 week',
}

// The API serializes timestamps as naive UTC (no offset); tag them as UTC so the
// browser doesn't read them as local time.
function asUtc(s: string): string {
  return /[Z+]/.test(s) ? s : `${s}Z`
}

function expiryLabel(invite: Invite): string {
  if (!invite.expires_at) return 'Never expires'
  const when = new Date(asUtc(invite.expires_at))
  return when.getTime() < Date.now()
    ? 'Expired'
    : `Expires ${when.toLocaleString()}`
}

function InviteRow({
  invite,
  onRevoke,
}: {
  invite: Invite
  onRevoke: (id: string) => void
}) {
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(invite.url)
      toast.success('Invite link copied')
    } catch {
      toast.error('Could not copy link')
    }
  }

  return (
    <div className="space-y-2 rounded-lg border p-3">
      <div className="flex items-center gap-2">
        <Input readOnly value={invite.url} className="font-mono text-xs" />
        <Button size="sm" variant="secondary" onClick={copy}>
          <Copy /> Copy
        </Button>
        <Button
          size="sm"
          variant="ghost"
          aria-label="Revoke invite"
          onClick={() => onRevoke(invite.id)}
        >
          <Trash2 />
        </Button>
      </div>
      <div className="text-muted-foreground flex flex-wrap gap-x-3 text-xs">
        <span>
          {invite.uses}
          {invite.max_uses != null ? ` / ${invite.max_uses}` : ''} used
        </span>
        <span>{expiryLabel(invite)}</span>
        {invite.autofollow && <span>auto-follow</span>}
        {invite.comment && <span>“{invite.comment}”</span>}
      </div>
    </div>
  )
}

export default function Invites() {
  const token = getToken()
  const [invites, setInvites] = useState<Invite[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [maxUses, setMaxUses] = useState('0')
  const [expiresIn, setExpiresIn] = useState('0')
  const [autofollow, setAutofollow] = useState(false)
  const [comment, setComment] = useState('')
  const [creating, setCreating] = useState(false)

  useEffect(() => {
    if (!token) return
    getInvites(token)
      .then(setInvites)
      .catch((e) => setError(String(e)))
  }, [token])

  const create = async () => {
    if (!token) return
    setCreating(true)
    try {
      const invite = await createInvite(token, {
        max_uses: maxUses === '0' ? undefined : Number(maxUses),
        expires_in: expiresIn === '0' ? undefined : Number(expiresIn),
        autofollow,
        comment: comment.trim() || undefined,
      })
      setInvites((prev) => [invite, ...(prev ?? [])])
      setComment('')
      toast.success('Invite link created')
    } catch {
      toast.error('Could not create invite')
    } finally {
      setCreating(false)
    }
  }

  const revoke = async (id: string) => {
    if (!token) return
    const prev = invites
    setInvites((cur) => cur?.filter((i) => i.id !== id) ?? null)
    try {
      await deleteInvite(token, id)
      toast.success('Invite revoked')
    } catch {
      setInvites(prev ?? null)
      toast.error('Could not revoke invite')
    }
  }

  return (
    <div className="page-frame">
      <TopBar />
      <h1 className="mb-1 text-lg font-bold">Invites</h1>
      <p className="text-muted-foreground mb-4 text-sm">
        Create a link to invite people to this instance.
      </p>

      {!token ? (
        <div className="space-y-2">
          <p className="text-muted-foreground text-sm">
            Sign in to create invite links.
          </p>
          <Button size="sm" onClick={() => beginLogin()}>
            Sign in
          </Button>
        </div>
      ) : (
        <>
          <div className="mb-6 space-y-3 rounded-lg border p-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="space-y-1">
                <Label>Uses</Label>
                <Select
                  items={MAX_USES}
                  value={maxUses}
                  onValueChange={(v) => setMaxUses(v ?? '0')}
                >
                  <SelectTrigger className="w-full" aria-label="Maximum uses">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      {Object.entries(MAX_USES).map(([value, label]) => (
                        <SelectItem key={value} value={value}>
                          {label}
                        </SelectItem>
                      ))}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1">
                <Label>Expiry</Label>
                <Select
                  items={EXPIRES_IN}
                  value={expiresIn}
                  onValueChange={(v) => setExpiresIn(v ?? '0')}
                >
                  <SelectTrigger className="w-full" aria-label="Expiry">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      {Object.entries(EXPIRES_IN).map(([value, label]) => (
                        <SelectItem key={value} value={value}>
                          {label}
                        </SelectItem>
                      ))}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="space-y-1">
              <Label htmlFor="invite-comment">Note (optional)</Label>
              <Input
                id="invite-comment"
                value={comment}
                maxLength={420}
                placeholder="What is this invite for?"
                onChange={(e) => setComment(e.target.value)}
              />
            </div>
            <label className="flex items-center gap-2 text-sm">
              <Switch checked={autofollow} onCheckedChange={setAutofollow} />
              New members auto-follow me
            </label>
            <Button onClick={create} disabled={creating}>
              {creating ? 'Creating…' : 'Create invite link'}
            </Button>
          </div>

          {error && <p className="text-destructive text-sm">{error}</p>}
          {invites === null && !error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {invites?.length === 0 && (
            <p className="text-muted-foreground text-sm">No invites yet.</p>
          )}
          <div className="space-y-2">
            {invites?.map((invite) => (
              <InviteRow key={invite.id} invite={invite} onRevoke={revoke} />
            ))}
          </div>
        </>
      )}
    </div>
  )
}
