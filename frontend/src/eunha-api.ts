// eunha-specific APIs with no Mastodon C2S equivalent. Mastodon has no
// invite-tree endpoint, so these are served by eunha's own `/api/eunha/*`
// routes and called with a plain fetch (masto.js only models the C2S surface).

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
  const res = await fetch(`${window.location.origin}/api/eunha/v1/invite_tree`, {
    headers: { Authorization: `Bearer ${token}` },
  })
  if (!res.ok) {
    throw new Error(`invite tree request failed: ${res.status}`)
  }
  return res.json() as Promise<InviteTree>
}
