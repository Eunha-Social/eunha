import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Network, Ticket } from 'lucide-react'

import { canInviteUsers, getInstance } from '../api.ts'
import { getToken } from '../auth.ts'
import type { mastodon } from '../masto.ts'
import { TopBar } from '@/components/top-bar.tsx'

// Server information, reachable by anyone. Nothing shows it on "/" any more —
// that is a timeline for signed-in and signed-out visitors alike — so this is
// the only place the instance describes itself, reached from the sidebar.
export default function About() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)
  const [canInvite, setCanInvite] = useState(false)
  const token = getToken()

  useEffect(() => {
    getInstance().then(setInstance).catch(() => {})
  }, [])

  // Not every member may invite: the permission can be taken off the everyone
  // role, leaving invites to the instance's staff. Don't offer a page that
  // would only refuse.
  useEffect(() => {
    if (!token) return
    canInviteUsers(token).then(setCanInvite).catch(() => {})
  }, [token])

  return (
    <div className="page-frame">
      <TopBar title={instance?.title} />
      {instance ? (
        <section className="space-y-2">
          <h1 className="text-2xl font-bold">{instance.title}</h1>
          <p className="text-foreground/90">{instance.description}</p>
          <p className="text-muted-foreground text-sm">
            {instance.domain} · running eunha {__COMMIT_HASH__}
          </p>
          {token && (
            <div className="flex flex-col gap-1 pt-1">
              {canInvite && (
                <Link
                  to="/invites"
                  className="text-primary inline-flex items-center gap-2 font-medium"
                >
                  <Ticket className="size-4" /> Invite people
                </Link>
              )}
              <Link
                to="/invite-tree"
                className="text-primary inline-flex items-center gap-2 font-medium"
              >
                <Network className="size-4" /> View the invite tree
              </Link>
            </div>
          )}
        </section>
      ) : (
        <p className="text-muted-foreground text-sm">Loading…</p>
      )}
    </div>
  )
}
