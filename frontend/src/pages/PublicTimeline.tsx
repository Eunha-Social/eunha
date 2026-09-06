import { useLocation } from 'react-router-dom'

import { getToken } from '../auth.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { ColumnHeader } from '@/components/column-header.tsx'
import { StatusFeed } from '@/components/status-feed.tsx'

// `/local` and `/public` are the same page over two feeds, chosen by the path.
// `local` is also accepted as a prop, because "/" renders this page for a
// signed-out visitor and there is no path to read it from there.
export default function PublicTimeline({ local }: { local?: boolean } = {}) {
  const pathname = useLocation().pathname
  const isLocal = local ?? pathname === '/local'
  const token = getToken()

  return (
    <>
      <TopBar />
      <div className="column-frame">
        <ColumnHeader title={isLocal ? 'Local' : 'Federated'} />
        <div className="space-y-2 p-3">
          <StatusFeed kind={isLocal ? 'local' : 'public'} token={token} />
        </div>
      </div>
    </>
  )
}
