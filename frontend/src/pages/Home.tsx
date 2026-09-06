import { getToken } from '../auth.ts'
import { isAdvancedLayout } from '../lib/panes.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { StatusFeed } from '@/components/status-feed.tsx'
import { ColumnHeader } from '@/components/column-header.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'
import { AdvancedLayout } from '@/components/advanced-layout.tsx'
import PublicTimeline from './PublicTimeline.tsx'

// Signed out there is no home timeline, and what a visitor arrived to look at
// is the instance's own posts — so "/" is the local timeline rather than a
// paragraph about the software running it. What the instance says about itself
// still has a page of its own at /about.
//
// A dispatcher rather than a branch inside the timeline: signing in or out
// reloads the page, so the two never swap under a mounted component, and each
// keeps its own hooks. The advanced layout joins it on the same terms.
export default function Home() {
  if (!getToken()) return <PublicTimeline local />
  return isAdvancedLayout() ? <AdvancedLayout /> : <HomeTimeline />
}

function HomeTimeline() {
  const token = getToken()
  const { openCompose } = useComposeModal()

  return (
    <>
      <TopBar />
      <div className="column-frame">
        {/* "Following", not "Home": the rail already says where you are, and
            this names what the feed actually is. Upstream keeps "Home" as the
            column's accessible label for the same reason. */}
        <ColumnHeader title="Following" />
        <section aria-label="Home" className="space-y-2 p-3">
          <StatusFeed
            kind="home"
            token={token}
            onReply={(status, prepend) =>
              openCompose({ replyTo: status, onPosted: prepend })
            }
          />
        </section>
      </div>
    </>
  )
}
