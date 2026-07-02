// Caches the signed-in account so UI can synchronously render current-user
// affordances without a request per component mount.
import { getCurrentAccount } from './api.ts'

const ID_KEY = 'eunha:me-id'
const ACCOUNT_KEY = 'eunha:me-account'

export interface MeAccount {
  id: string
  acct: string
}

let cachedId: string | null = localStorage.getItem(ID_KEY)
let cachedAccount: MeAccount | null = readCachedAccount()

function readCachedAccount(): MeAccount | null {
  const raw = localStorage.getItem(ACCOUNT_KEY)
  if (!raw) return null

  try {
    const parsed = JSON.parse(raw) as Partial<MeAccount>
    return parsed.id && parsed.acct ? { id: parsed.id, acct: parsed.acct } : null
  } catch {
    return null
  }
}

export function getMeId(): string | null {
  return cachedId
}

export function getMeAccount(): MeAccount | null {
  return cachedAccount
}

export async function loadMe(token: string): Promise<MeAccount | null> {
  try {
    const me = await getCurrentAccount(token)
    cachedId = me.id
    cachedAccount = { id: me.id, acct: me.acct }
    localStorage.setItem(ID_KEY, me.id)
    localStorage.setItem(ACCOUNT_KEY, JSON.stringify(cachedAccount))
    return cachedAccount
  } catch {
    // ignore — controls just won't render until known
    return cachedAccount
  }
}

export function clearMe(): void {
  cachedId = null
  cachedAccount = null
  localStorage.removeItem(ID_KEY)
  localStorage.removeItem(ACCOUNT_KEY)
}
