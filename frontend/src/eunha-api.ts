// eunha-specific APIs with no Mastodon C2S equivalent. Mastodon has no
// invite-tree endpoint nor a REST API for invite CRUD, so these are served by
// eunha's own routes and called with a plain fetch (masto.js only models the
// C2S surface).

// Carries the HTTP status so callers can tell "you typed the wrong password"
// (401) from "something broke", which the message alone doesn't say.
export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

async function eunhaFetch(
  path: string,
  token: string,
  init?: RequestInit,
): Promise<Response> {
  const res = await fetch(`${window.location.origin}${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${token}`,
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
      ...init?.headers,
    },
  })
  if (!res.ok) {
    throw new ApiError(res.status, `${path} failed: ${res.status}`)
  }
  return res
}

export interface InviteTreeAccount {
  id: string
  username: string
  acct: string
  display_name: string
  avatar: string
  invited_at: string
}

export interface InviteNode extends InviteTreeAccount {
  children: InviteNode[]
}

export interface InviteTree {
  roots: InviteNode[]
  total: number
}

export async function getInviteTree(token: string): Promise<InviteTree> {
  const res = await eunhaFetch('/api/eunha/v1/invite_tree', token)
  return res.json() as Promise<InviteTree>
}

// ── Invites ────────────────────────────────────────────────────────────────
// Served by eunha's /api/v1/invites (a non-standard extension: Mastodon exposes
// invite CRUD only through its web UI, never the REST API).

export interface Invite {
  id: string
  code: string
  url: string
  max_uses: number | null
  uses: number
  expires_at: string | null
  autofollow: boolean
  comment: string | null
  created_at: string
}

export interface CreateInviteParams {
  max_uses?: number
  /** Seconds until expiry; omit for never. */
  expires_in?: number
  autofollow?: boolean
  comment?: string
}

export async function getInvites(token: string): Promise<Invite[]> {
  const res = await eunhaFetch('/api/v1/invites', token)
  return res.json() as Promise<Invite[]>
}

export async function createInvite(
  token: string,
  params: CreateInviteParams,
): Promise<Invite> {
  const res = await eunhaFetch('/api/v1/invites', token, {
    method: 'POST',
    body: JSON.stringify(params),
  })
  return res.json() as Promise<Invite>
}

export interface GrantInvitesParams {
  /** Whose account to mint them into; omit for every local member. */
  account_id?: string
  count: number
  /** Uses per code; 1 by default. */
  max_uses?: number
  /** Seconds until expiry; omit for never. */
  expires_in?: number
  comment?: string
}

export interface GrantInvitesResult {
  granted: number
  accounts: number
}

/**
 * Mint invites into other members' accounts (admin only).
 *
 * Under `/api/eunha/` because Mastodon has no such action at all: there an
 * invite is made by whoever hands it out. The codes belong to the member they
 * are minted for, so a signup through one lands under them in the invite tree.
 */
export async function grantInvites(
  token: string,
  params: GrantInvitesParams,
): Promise<GrantInvitesResult> {
  const res = await eunhaFetch('/api/eunha/v1/invite_grants', token, {
    method: 'POST',
    body: JSON.stringify(params),
  })
  return res.json() as Promise<GrantInvitesResult>
}

export async function deleteInvite(token: string, id: string): Promise<void> {
  await eunhaFetch(`/api/v1/invites/${id}`, token, { method: 'DELETE' })
}

// ── Sign up ─────────────────────────────────────────────────────────────────
// POST /api/v1/accounts is unauthenticated and always requires email
// confirmation, so it returns a placeholder token; we just need success/failure.

export interface SignUpParams {
  username: string
  email: string
  password: string
  locale?: string
  invite_code?: string
  reason?: string
}

export async function signUp(params: SignUpParams): Promise<void> {
  const res = await fetch(`${window.location.origin}/api/v1/accounts`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(params),
  })
  if (!res.ok) {
    let message = `Sign up failed (${res.status})`
    try {
      const body = (await res.json()) as { error?: string }
      if (body.error) message = body.error
    } catch {
      // keep the status-based fallback
    }
    throw new Error(message)
  }
}

// ── Account deletion ───────────────────────────────────────────────────────
// Mastodon deletes accounts through a web form (`/settings/delete`) and has no
// REST equivalent, so `DELETE /api/v1/accounts` is eunha's own. It runs the
// same challenge as that form: the current password, or — for accounts with no
// password — the username.

export interface DeleteAccountChallenge {
  password?: string
  username?: string
}

// Suspends the account immediately and purges it in the background. The
// caller's token stops working as soon as this returns.
export async function deleteAccount(
  token: string,
  challenge: DeleteAccountChallenge,
): Promise<void> {
  await eunhaFetch('/api/v1/accounts', token, {
    method: 'DELETE',
    body: JSON.stringify(challenge),
  })
}
