import { useCallback, useEffect, useRef, useState, type DependencyList } from 'react'

/**
 * Feed backed by a masto paginator, for endpoints whose cursor lives only in
 * the response's `Link` header.
 *
 * `useInfiniteFeed` pages by the last item's own id, which works when the items
 * *are* what is paginated. `favourited_by` and `reblogged_by` return accounts
 * but paginate by favourite id and reblog id, so the ids in the body are the
 * wrong cursor entirely — passing one as `max_id` would filter against an
 * unrelated sequence. masto's paginator follows the `Link` header instead, and
 * this walks it one page per `loadMore`.
 *
 * The paginator is reopened whenever `deps` change; a run that is superseded
 * (or unmounted) stops writing state.
 */
export function useInfinitePaginator<T>(
  open: () => AsyncIterable<T[]>,
  deps: DependencyList,
) {
  const [items, setItems] = useState<T[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loadingMore, setLoadingMore] = useState(false)
  const [done, setDone] = useState(false)

  const openRef = useRef(open)
  openRef.current = open
  const iterRef = useRef<AsyncIterator<T[]> | null>(null)
  const busyRef = useRef(false)
  // Bumped on every reset so a page still in flight from a previous run is
  // discarded rather than appended to the new list.
  const runRef = useRef(0)

  useEffect(() => {
    const run = ++runRef.current
    setItems(null)
    setError(null)
    setDone(false)
    setLoadingMore(false)
    busyRef.current = true
    const iter = openRef.current()[Symbol.asyncIterator]()
    iterRef.current = iter
    iter
      .next()
      .then(({ value, done: exhausted }) => {
        if (run !== runRef.current) return
        const page = exhausted ? [] : (value ?? [])
        setItems(page)
        setDone(exhausted === true || page.length === 0)
      })
      .catch((e) => {
        if (run === runRef.current) setError(String(e))
      })
      .finally(() => {
        if (run === runRef.current) busyRef.current = false
      })
    return () => {
      // Abandon this run: a late page resolves into a stale `run` and is
      // dropped by the guards above.
      runRef.current++
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)

  const loadMore = useCallback(async () => {
    const iter = iterRef.current
    if (busyRef.current || done || !iter) return
    const run = runRef.current
    busyRef.current = true
    setLoadingMore(true)
    try {
      const { value, done: exhausted } = await iter.next()
      if (run !== runRef.current) return
      const page = exhausted ? [] : (value ?? [])
      // The server stops sending a `next` link once a page comes back empty,
      // so an empty page and an exhausted iterator both mean the end.
      if (page.length === 0) setDone(true)
      else setItems((prev) => [...(prev ?? []), ...page])
    } catch (e) {
      if (run === runRef.current) setError(String(e))
    } finally {
      if (run === runRef.current) {
        busyRef.current = false
        setLoadingMore(false)
      }
    }
  }, [done])

  // Lets a caller drop or replace rows without refetching — undoing a block
  // should take that account out of the list it was just acted on in.
  const mutate = useCallback((fn: (items: T[]) => T[]) => {
    setItems((prev) => (prev ? fn(prev) : prev))
  }, [])

  return { items, error, loadingMore, done, loadMore, mutate }
}
