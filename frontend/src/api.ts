// Thin typed client for the Mastodon Client-to-Server (C2S) REST API. The same
// API is consumed by third-party mobile apps; this frontend is just one client.

export interface Instance {
  domain: string
  title: string
  description: string
  source_url?: string
  version: string
}

export interface Account {
  id: string
  username: string
  acct: string
  display_name: string
  avatar: string
  url: string
}

export interface Status {
  id: string
  uri: string
  url: string | null
  created_at: string
  content: string
  account: Account
  reblog: Status | null
}

async function request<T>(
  path: string,
  opts: RequestInit & { token?: string } = {},
): Promise<T> {
  const { token, headers, ...rest } = opts
  const res = await fetch(path, {
    ...rest,
    headers: {
      Accept: 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...headers,
    },
  })
  if (!res.ok) {
    throw new Error(`${rest.method ?? 'GET'} ${path} → ${res.status}`)
  }
  return res.json() as Promise<T>
}

export function getInstance(): Promise<Instance> {
  return request<Instance>('/api/v2/instance')
}

export function getHomeTimeline(token: string): Promise<Status[]> {
  return request<Status[]>('/api/v1/timelines/home', { token })
}

export function verifyCredentials(token: string): Promise<Account> {
  return request<Account>('/api/v1/accounts/verify_credentials', { token })
}
