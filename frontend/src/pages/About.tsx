import { useEffect, useState } from 'react'

import { getInstance } from '../api.ts'
import type { mastodon } from '../masto.ts'
import { TopBar } from '@/components/top-bar.tsx'

// Server information, reachable by anyone (authenticated users see the home
// timeline instead of this on "/", so they get here via the sidebar link).
export default function About() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)

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
        </section>
      ) : (
        <p className="text-muted-foreground text-sm">Loading…</p>
      )}
    </div>
  )
}
