// `cn` is shadcn's engine for the same job `twMerge(clsx(...))` did — same API,
// no dependencies. It arrives with any component added through the shadcn CLI,
// which writes `import { cn } from "cn"` into what it generates. Re-exporting it
// here means the components that predate that import the same function rather
// than a second implementation of it.
export { cn, type ClassValue } from 'cn'

// What to put in a toast when a call fails. masto raises `MastoHttpError` with
// the server's own message, which is the useful half of a 422 — "You have
// already pinned the maximum number of statuses" says more than "failed" can.
export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
