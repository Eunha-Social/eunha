import { useCallback } from 'react'

import type { mastodon } from '../masto.ts'
import { useStreamingSubscription } from './use-streaming-subscription.ts'

type StatusFeed = {
  mutate: (fn: (items: mastodon.v1.Status[]) => mastodon.v1.Status[]) => void
}

function hasStatusId(status: mastodon.v1.Status, id: string) {
  return status.id === id || status.reblog?.id === id
}

function mergeStatus(status: mastodon.v1.Status, updated: mastodon.v1.Status) {
  if (status.reblog?.id === updated.id) return { ...status, reblog: updated }
  if (status.id === updated.id) return updated
  return status
}

export function useStatusStreaming({
  enabled = true,
  token,
  subscribe,
  feed,
}: {
  enabled?: boolean
  token?: string
  subscribe: (client: mastodon.streaming.Client) => mastodon.streaming.Subscription
  feed: StatusFeed
}) {
  const { mutate } = feed
  const onEvent = useCallback(
    (event: mastodon.streaming.Event) => {
      if (event.event === 'update') {
        mutate((items) =>
          items.some((item) => hasStatusId(item, event.payload.id))
            ? items.map((item) => mergeStatus(item, event.payload))
            : [event.payload, ...items],
        )
        return
      }

      if (event.event === 'status.update') {
        mutate((items) =>
          items.map((item) => mergeStatus(item, event.payload)),
        )
        return
      }

      if (event.event === 'delete') {
        mutate((items) =>
          items.filter((item) => !hasStatusId(item, event.payload)),
        )
      }
    },
    [mutate],
  )

  useStreamingSubscription({ enabled, token, subscribe, onEvent })
}
