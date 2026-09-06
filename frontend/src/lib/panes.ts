/**
 * The advanced layout: several timelines side by side.
 *
 * Off by default, and deliberately so. Mastodon keeps its multi-column view as
 * an opt-in for the same reason — it suits people who read a lot of feeds at
 * once and is clutter for everyone else. eunha's default stays one column.
 *
 * Stored per browser rather than on the server: it is a property of the screen
 * you are sitting at, not of the account. The same person on a phone wants the
 * single column even when their desktop shows four.
 */
const ENABLED_KEY = 'eunha:panes'
const LIST_KEY = 'eunha:panes:list'

// The status timelines only. Notifications and Messages are columns upstream
// can pin too, but they are lists of a different shape and reading them side
// by side is not what the layout is for — that is left undone rather than
// half-built.
export type PaneId = 'home' | 'local' | 'public'

export const PANES: { id: PaneId; title: string }[] = [
  { id: 'home', title: 'Following' },
  { id: 'local', title: 'Local' },
  { id: 'public', title: 'Federated' },
]

export function paneTitle(id: PaneId): string {
  return PANES.find((p) => p.id === id)?.title ?? id
}

export function isAdvancedLayout(): boolean {
  try {
    return localStorage.getItem(ENABLED_KEY) === 'on'
  } catch {
    // Private windows and blocked site data throw on access, and a layout
    // preference is not worth failing a render over.
    return false
  }
}

export function setAdvancedLayout(on: boolean) {
  try {
    localStorage.setItem(ENABLED_KEY, on ? 'on' : 'off')
  } catch {
    // ignore
  }
}

export function readPanes(): PaneId[] {
  try {
    const raw = localStorage.getItem(LIST_KEY)
    if (!raw) return ['home', 'local', 'public']
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return ['home', 'local', 'public']
    const known = parsed.filter((id): id is PaneId =>
      PANES.some((p) => p.id === id),
    )
    return known.length > 0 ? known : ['home']
  } catch {
    return ['home', 'local', 'public']
  }
}

export function writePanes(ids: PaneId[]) {
  try {
    localStorage.setItem(LIST_KEY, JSON.stringify(ids))
  } catch {
    // ignore
  }
}
