import { useEffect, useState } from 'react'
import {
  getInstance,
  getHomeTimeline,
  type Instance,
  type Status,
} from '../api.ts'
import { beginLogin, getToken, logout } from '../auth.ts'

export default function Home() {
  const [instance, setInstance] = useState<Instance | null>(null)
  const [statuses, setStatuses] = useState<Status[] | null>(null)
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
    <div className="app">
      <header className="topbar">
        <strong>{instance?.title ?? 'eunha'}</strong>
        {token ? (
          <button onClick={() => { logout(); location.reload() }}>Sign out</button>
        ) : (
          <button onClick={() => beginLogin()}>Sign in</button>
        )}
      </header>

      {error && <p className="error">{error}</p>}

      {!token && instance && (
        <section className="intro">
          <h1>{instance.title}</h1>
          <p>{instance.description}</p>
          <p className="muted">
            {instance.domain} · running eunha {instance.version}
          </p>
        </section>
      )}

      {token && (
        <section className="timeline">
          <h2>Home</h2>
          {statuses === null && !error && <p className="muted">Loading…</p>}
          {statuses?.map((s) => {
            const status = s.reblog ?? s
            return (
              <article key={s.id} className="status">
                <div className="status-head">
                  <img className="avatar" src={status.account.avatar} alt="" />
                  <span className="name">
                    {status.account.display_name || status.account.username}
                  </span>
                  <span className="acct muted">@{status.account.acct}</span>
                  <time className="muted">
                    {new Date(status.created_at).toLocaleString()}
                  </time>
                </div>
                <div
                  className="status-body"
                  dangerouslySetInnerHTML={{ __html: status.content }}
                />
              </article>
            )
          })}
          {statuses?.length === 0 && <p className="muted">Your home timeline is empty.</p>}
        </section>
      )}
    </div>
  )
}
