import { useEffect, useState } from 'react'
import { Plus, X } from 'lucide-react'

import { getToken } from '../auth.ts'
import { PANES, paneTitle, readPanes, writePanes, type PaneId } from '../lib/panes.ts'
import { TopBar } from '@/components/top-bar.tsx'
import { ColumnHeader } from '@/components/column-header.tsx'
import { StatusFeed } from '@/components/status-feed.tsx'
import { NotificationsFeed } from '@/pages/Notifications.tsx'
import { MessagesFeed } from '@/pages/Messages.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'
import { Button } from '@/components/ui/button.tsx'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.tsx'

/**
 * Several timelines at once, for people who want them.
 *
 * Each pane scrolls on its own and holds its own stream, which is the point —
 * the single column can only show one feed and makes you navigate to compare.
 * The row scrolls sideways when the panes outgrow the window rather than
 * squeezing them, because a timeline narrower than its posts is not a timeline.
 */
// Each pane is one of three shapes: a status timeline, the notification list,
// or the message list. They keep their own scroll and their own stream, which
// is the point of showing them at once.
function PaneBody({
  id,
  token,
  openCompose,
}: {
  id: PaneId
  token: string | null
  openCompose: ReturnType<typeof useComposeModal>['openCompose']
}) {
  if (id === 'notifications') return <NotificationsFeed />
  if (id === 'messages') return <MessagesFeed />
  return (
    <StatusFeed
      kind={id}
      token={token}
      onReply={(status, prepend) =>
        openCompose({ replyTo: status, onPosted: prepend })
      }
    />
  )
}

export function AdvancedLayout() {
  const token = getToken()
  const { openCompose } = useComposeModal()
  const [panes, setPanes] = useState<PaneId[]>(() => readPanes())

  // Tells the stylesheet to pin the rail: its default `left` assumes a centred
  // reading column, which this layout does not have.
  useEffect(() => {
    document.documentElement.dataset.layout = 'advanced'
    return () => {
      delete document.documentElement.dataset.layout
    }
  }, [])

  const update = (next: PaneId[]) => {
    setPanes(next)
    writePanes(next)
  }
  const remove = (id: PaneId) => update(panes.filter((p) => p !== id))
  const add = (id: PaneId) => update([...panes, id])
  const available = PANES.filter((p) => !panes.includes(p.id))

  return (
    <>
      <TopBar />
      <div className="advanced-frame">
        {panes.map((id) => (
          <div key={id} className="advanced-pane">
            <ColumnHeader title={paneTitle(id)}>
              <Button
                variant="ghost"
                size="icon"
                aria-label={`Close ${paneTitle(id)}`}
                onClick={() => remove(id)}
              >
                <X />
              </Button>
            </ColumnHeader>
            <div className="flex-1 space-y-2 overflow-y-auto p-3">
              <PaneBody id={id} token={token} openCompose={openCompose} />
            </div>
          </div>
        ))}

        {available.length > 0 && (
          <div className="shrink-0 self-start pt-3">
            <DropdownMenu>
              <DropdownMenuTrigger
                render={<Button variant="outline" size="sm" />}
                aria-label="Add a timeline"
              >
                <Plus /> Add
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start">
                {available.map((p) => (
                  <DropdownMenuItem key={p.id} onClick={() => add(p.id)}>
                    {p.title}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )}
      </div>
    </>
  )
}
