import { useEffect, useState, type FormEvent } from 'react'
import { Link, useSearchParams } from 'react-router-dom'

import type { mastodon } from '../masto.ts'
import { search } from '../api.ts'
import { getToken } from '../auth.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { StatusCard } from '@/components/status-card.tsx'
import { AccountRow } from '@/components/account-row.tsx'

export default function Search() {
  const [params, setParams] = useSearchParams()
  const q = params.get('q') ?? ''
  const token = getToken()

  const [input, setInput] = useState(q)
  const [results, setResults] = useState<mastodon.v2.Search | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => setInput(q), [q])

  useEffect(() => {
    if (!q.trim()) {
      setResults(null)
      return
    }
    let cancelled = false
    setLoading(true)
    setError(null)
    search(q, token ?? undefined)
      .then((r) => !cancelled && setResults(r))
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false))
    return () => {
      cancelled = true
    }
  }, [q, token])

  const submit = (e: FormEvent) => {
    e.preventDefault()
    setParams(input.trim() ? { q: input.trim() } : {})
  }

  const empty =
    results != null &&
    results.accounts.length === 0 &&
    results.hashtags.length === 0 &&
    results.statuses.length === 0

  return (
    <div className="mx-auto max-w-2xl p-4">
      <TopBar />
      <form onSubmit={submit} className="mb-4">
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="Search posts, people, and hashtags"
          autoFocus
          className="bg-background focus-visible:ring-ring w-full rounded-md border px-3 py-2 text-sm outline-none focus-visible:ring-[3px]"
        />
      </form>

      {error && <p className="text-destructive text-sm">{error}</p>}
      {loading && <p className="text-muted-foreground text-sm">Searching…</p>}

      {results && (
        <div className="space-y-6">
          {results.accounts.length > 0 && (
            <section>
              <h2 className="text-secondary mb-2 text-sm font-semibold">People</h2>
              <div className="space-y-1">
                {results.accounts.map((a) => (
                  <AccountRow key={a.id} account={a} />
                ))}
              </div>
            </section>
          )}

          {results.hashtags.length > 0 && (
            <section>
              <h2 className="text-secondary mb-2 text-sm font-semibold">Hashtags</h2>
              <div className="space-y-1">
                {results.hashtags.map((t) => (
                  <Link
                    key={t.name}
                    to={`/tags/${t.name}`}
                    className="hover:bg-muted/50 block rounded-lg p-2 font-medium no-underline"
                  >
                    #{t.name}
                  </Link>
                ))}
              </div>
            </section>
          )}

          {results.statuses.length > 0 && (
            <section>
              <h2 className="text-secondary mb-2 text-sm font-semibold">Posts</h2>
              <div className="space-y-3">
                {results.statuses.map((s) => (
                  <StatusCard
                    key={s.id}
                    status={s.reblog ?? s}
                    token={token ?? ''}
                    boostedBy={s.reblog ? s.account : undefined}
                  />
                ))}
              </div>
            </section>
          )}

          {empty && (
            <p className="text-muted-foreground text-sm">No results for “{q}”.</p>
          )}
        </div>
      )}
    </div>
  )
}
