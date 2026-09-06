import type { mastodon } from '../masto.ts'

/**
 * Drop direct messages from a list of statuses.
 *
 * Mastodon 5.0 takes messages out of the timeline and out of profiles
 * entirely — they live on the Messages page, where a thread has somewhere to
 * belong, and notifications are unaffected. The server still sends them, so
 * the client is where it happens.
 *
 * Filtered where the list is rendered rather than where it is fetched, on
 * purpose. `useInfiniteFeed` pages from the last item it holds and stops when
 * a page comes back empty, so removing statuses from a page before the feed
 * sees it would end a timeline early for anyone whose next page happened to be
 * all messages. Filtering here leaves the cursor on what the server actually
 * returned.
 */
export function withoutMessages(
  statuses: mastodon.v1.Status[] | null,
): mastodon.v1.Status[] | null {
  if (!statuses) return statuses
  // A boost carries its own visibility, so the underlying status is the one to
  // ask — though a direct status cannot be boosted in the first place.
  return statuses.filter((s) => (s.reblog ?? s).visibility !== 'direct')
}
