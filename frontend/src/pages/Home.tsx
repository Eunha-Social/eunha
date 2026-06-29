import { useEffect, useState } from 'react'
import { LogIn, LogOut } from 'lucide-react'

import { getInstance, getHomeTimeline } from '../api.ts'
import type { mastodon } from '../masto.ts'
import { beginLogin, getToken, logout } from '../auth.ts'
import { Button } from '@/components/ui/button.tsx'
import { Card, CardContent } from '@/components/ui/card.tsx'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar.tsx'

export default function Home() {
  const [instance, setInstance] = useState<mastodon.v2.Instance | null>(null)
  const [statuses, setStatuses] = useState<mastodon.v1.Status[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const token = getToken()

  useEffect(() => {
    getInstance().then(setInstance).catch((e) => setError(String(e)))
  }, [])

  useEffect(() => {
    if (!token) return
    getHomeTimeline(token)
      .then(setStatuses)
      .catch((e) => setError(String(e)))
  }, [token])

  return (
    <div className="mx-auto max-w-2xl p-4">
      <header className="mb-6 flex items-center justify-between border-b pb-3">
        <span className="text-lg font-semibold">{instance?.title ?? 'eunha'}</span>
        {token ? (
          <Button variant="outline" size="sm" onClick={() => { logout(); location.reload() }}>
            <LogOut /> Sign out
          </Button>
        ) : (
          <Button size="sm" onClick={() => beginLogin()}>
            <LogIn /> Sign in
          </Button>
        )}
      </header>

      {error && <p className="text-destructive mb-4 text-sm">{error}</p>}

      {!token && instance && (
        <section className="space-y-2">
          <h1 className="text-2xl font-bold">{instance.title}</h1>
          <p className="text-foreground/90">{instance.description}</p>
          <p className="text-muted-foreground text-sm">
            {instance.domain} · running eunha {instance.version}
          </p>
        </section>
      )}

      {token && (
        <section className="space-y-3">
          <h2 className="text-secondary text-lg font-semibold">Home</h2>
          {statuses === null && !error && (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
          {statuses?.map((s) => {
            const status = s.reblog ?? s
            return (
              <Card key={s.id}>
                <CardContent className="space-y-2">
                  <div className="flex items-center gap-2 text-sm">
                    <Avatar className="size-9 rounded-lg">
                      <AvatarImage src={status.account.avatar} alt="" />
                      <AvatarFallback>
                        {(status.account.displayName || status.account.username)
                          .slice(0, 1)
                          .toUpperCase()}
                      </AvatarFallback>
                    </Avatar>
                    <span className="font-semibold">
                      {status.account.displayName || status.account.username}
                    </span>
                    <span className="text-muted-foreground">@{status.account.acct}</span>
                    <time className="text-muted-foreground ml-auto text-xs">
                      {new Date(status.createdAt).toLocaleString()}
                    </time>
                  </div>
                  <div
                    className="text-sm [&_a]:text-accent [&_a]:underline"
                    dangerouslySetInnerHTML={{ __html: status.content }}
                  />
                </CardContent>
              </Card>
            )
          })}
          {statuses?.length === 0 && (
            <p className="text-muted-foreground text-sm">Your home timeline is empty.</p>
          )}
        </section>
      )}
    </div>
  )
}
