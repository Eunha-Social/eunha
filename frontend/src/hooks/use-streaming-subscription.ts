import { useEffect } from 'react'

import { streamingClient, type mastodon } from '../masto.ts'

export function useStreamingSubscription({
  enabled = true,
  token,
  subscribe,
  onEvent,
}: {
  enabled?: boolean
  token?: string
  subscribe: (client: mastodon.streaming.Client) => mastodon.streaming.Subscription
  onEvent: (event: mastodon.streaming.Event) => void
}) {
  useEffect(() => {
    if (!enabled) return

    let cancelled = false
    const client = streamingClient(token)
    const subscription = subscribe(client)

    void (async () => {
      try {
        for await (const event of subscription) {
          if (cancelled) break
          onEvent(event)
        }
      } catch (e) {
        if (!cancelled) console.warn('streaming disconnected', e)
      }
    })()

    return () => {
      cancelled = true
      subscription.unsubscribe()
      client.close()
    }
  }, [enabled, token, subscribe, onEvent])
}
