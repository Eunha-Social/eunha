import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { Network } from 'lucide-react'

import { getInstance } from '../api.ts'
import { getToken } from '../auth.ts'
import type { mastodon } from '../masto.ts'
import { TopBar } from '@/components/top-bar.tsx'

// Server information, reachable by anyone (authenticated users see the home
// timeline instead of this on "/", so they get here via the sidebar link).
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
            <p>
              <Link
                to="/invite-tree"
                className="text-primary inline-flex items-center gap-2 font-medium"
              >
                <Network className="size-4" /> View the invite tree
              </Link>
            </p>
          )}
        </section>
      ) : (
        <p className="text-muted-foreground text-sm">Loading…</p>
      )}
    </div>
  )
}
