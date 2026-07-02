import { useCallback, useEffect, useState } from 'react'
import { useParams } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import { getStatus, getStatusContext } from '../api.ts'
import { getToken } from '../auth.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { Compose } from '@/components/compose.tsx'
import { TimelineStack } from '@/components/timeline-stack.tsx'

export default function StatusThread() {
  const { id = '' } = useParams()
  const token = getToken()

  const [status, setStatus] = useState<mastodon.v1.Status | null>(null)
  const [context, setContext] = useState<mastodon.v1.Context | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(() => {
    getStatus(id, token ?? undefined).then(setStatus).catch((e) => setError(String(e)))
    getStatusContext(id, token ?? undefined).then(setContext).catch(() => {})
  }, [id, token])

  useEffect(() => {
    setStatus(null)
    setContext(null)
    setError(null)
    load()
  }, [load])

  const render = (s: mastodon.v1.Status) => (
    <StatusCard
      key={s.id}
      status={s.reblog ?? s}
      token={token ?? ''}
      boostedBy={s.reblog ? s.account : undefined}
    />
  )

  return (
    <div className="mx-auto max-w-2xl p-3">
      <TopBar />
      {error && <p className="text-destructive text-sm">{error}</p>}

      <div className="space-y-2">
        {!!context?.ancestors.length && (
          <TimelineStack>{context.ancestors.map(render)}</TimelineStack>
        )}

        {status && (
          <div className="ring-primary/40 overflow-hidden rounded-md border bg-card ring-2">
            {render(status)}
          </div>
        )}

        {token && status && (
          <Compose token={token} replyTo={status} onPosted={() => load()} />
        )}

        {!!context?.descendants.length && (
          <TimelineStack>{context.descendants.map(render)}</TimelineStack>
        )}
      </div>
    </div>
  )
}
