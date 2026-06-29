// Standard Mastodon OAuth `authorization_code` flow, run entirely client-side
// against eunha's existing endpoints (`POST /api/v1/apps`, `GET /oauth/authorize`,
// `POST /oauth/token`). No backend changes are required.

const SCOPES = 'read write follow push'
const CLIENT_KEY = 'eunha:client'
const TOKEN_KEY = 'eunha:token'

interface ClientCreds {
  client_id: string
  client_secret: string
}

const redirectUri = () => `${window.location.origin}/auth/callback`

export function getToken(): string | null {
  return localStorage.getItem(TOKEN_KEY)
}

export function setToken(token: string) {
  localStorage.setItem(TOKEN_KEY, token)
}

export function logout() {
  localStorage.removeItem(TOKEN_KEY)
}

function storedClient(): ClientCreds | null {
  const raw = localStorage.getItem(CLIENT_KEY)
  return raw ? (JSON.parse(raw) as ClientCreds) : null
}

// Register a first-party OAuth app for this instance once, then reuse it.
async function ensureClient(): Promise<ClientCreds> {
  const existing = storedClient()
  if (existing) return existing

  const res = await fetch('/api/v1/apps', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify({
      client_name: 'eunha web',
      redirect_uris: redirectUri(),
      scopes: SCOPES,
      website: window.location.origin,
    }),
  })
  if (!res.ok) throw new Error(`register app → ${res.status}`)
  const app = (await res.json()) as ClientCreds
  const creds = { client_id: app.client_id, client_secret: app.client_secret }
  localStorage.setItem(CLIENT_KEY, JSON.stringify(creds))
  return creds
}

// Kick off login: register (or reuse) the app, then send the browser to the
// server-rendered authorize page.
export async function beginLogin() {
  const { client_id } = await ensureClient()
  const params = new URLSearchParams({
    client_id,
    redirect_uri: redirectUri(),
    response_type: 'code',
    scope: SCOPES,
  })
  window.location.assign(`/oauth/authorize?${params}`)
}

// Exchange the authorization code for a bearer token.
export async function completeLogin(code: string): Promise<void> {
  const creds = storedClient()
  if (!creds) throw new Error('missing client credentials')

  const res = await fetch('/oauth/token', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify({
      grant_type: 'authorization_code',
      client_id: creds.client_id,
      client_secret: creds.client_secret,
      redirect_uri: redirectUri(),
      code,
      scope: SCOPES,
    }),
  })
  if (!res.ok) throw new Error(`token exchange → ${res.status}`)
  const token = (await res.json()) as { access_token: string }
  setToken(token.access_token)
}
