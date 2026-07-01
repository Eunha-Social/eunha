// Caches the signed-in account's id so StatusCard can synchronously decide
// whether to show edit/delete controls, without a request per card.
import { getCurrentAccount } from './api.ts'

const KEY = 'eunha:me-id'
let cached: string | null = localStorage.getItem(KEY)

export function getMeId(): string | null {
  return cached
}

export async function loadMe(token: string): Promise<void> {
  try {
    const me = await getCurrentAccount(token)
    cached = me.id
    localStorage.setItem(KEY, me.id)
  } catch {
    // ignore — controls just won't render until known
  }
}

export function clearMe(): void {
  cached = null
  localStorage.removeItem(KEY)
}
