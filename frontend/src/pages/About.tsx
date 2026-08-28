import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Network, Ticket } from 'lucide-react'

import { getInstance } from '../api.ts'
import { getToken } from '../auth.ts'
import type { mastodon } from '../masto.ts'
import { TopBar } from '@/components/top-bar.tsx'

// Server information, reachable by anyone. Nothing shows it on "/" any more —
// that is a timeline for signed-in and signed-out visitors alike — so this is
// the only place the instance describes itself, reached from the sidebar.
export default function About() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)
  const token = getToken()

  useEffect(() => {
    getInstance().then(setInstance).catch(() => {})
  }, [])

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
              {/* Shown to every member: not everyone may create an invite,
                  but anyone can be handed one, and this is where they read it. */}
              <Link
                to="/invites"
                className="text-primary inline-flex items-center gap-2 font-medium"
              >
                <Ticket className="size-4" /> Invites
              </Link>
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
