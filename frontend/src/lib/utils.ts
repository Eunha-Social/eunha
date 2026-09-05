import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

// What to put in a toast when a call fails. masto raises `MastoHttpError` with
// the server's own message, which is the useful half of a 422 — "You have
// already pinned the maximum number of statuses" says more than "failed" can.
export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
