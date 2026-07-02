import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { createPortal } from 'react-dom'
import { X } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { getToken } from '../auth.ts'
import { Compose } from '@/components/compose.tsx'
import { Button } from '@/components/ui/button.tsx'

type ComposeOptions = {
  replyTo?: mastodon.v1.Status | null
  onPosted?: (status: mastodon.v1.Status) => void
}

type ComposeModalContextValue = {
  openCompose: (options?: ComposeOptions) => void
}

const ComposeModalContext = createContext<ComposeModalContextValue | null>(null)

export function ComposeModalProvider({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false)
  const [replyTo, setReplyTo] = useState<mastodon.v1.Status | null>(null)
  const [onPosted, setOnPosted] =
    useState<((status: mastodon.v1.Status) => void) | null>(null)

  const close = useCallback(() => {
    setOpen(false)
    setReplyTo(null)
    setOnPosted(null)
  }, [])

  const openCompose = useCallback((options?: ComposeOptions) => {
    setReplyTo(options?.replyTo ?? null)
    setOnPosted(() => options?.onPosted ?? null)
    setOpen(true)
  }, [])

  const value = useMemo(() => ({ openCompose }), [openCompose])
  const token = getToken()

  return (
    <ComposeModalContext.Provider value={value}>
      {children}
      {open && token
        ? createPortal(
            <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/45 px-3 py-10 sm:py-16">
              <div className="bg-card text-card-foreground w-full max-w-xl rounded-md border shadow-lg">
                <div className="flex items-center justify-between border-b px-4 py-2">
                  <h2 className="text-sm font-semibold">
                    {replyTo ? 'Reply' : 'Post'}
                  </h2>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Close"
                    onClick={close}
                  >
                    <X />
                  </Button>
                </div>
                <Compose
                  token={token}
                  replyTo={replyTo}
                  onCancelReply={replyTo ? close : undefined}
                  onPosted={(status) => {
                    onPosted?.(status)
                    close()
                  }}
                  framed={false}
                />
              </div>
            </div>,
            document.body,
          )
        : null}
    </ComposeModalContext.Provider>
  )
}

export function useComposeModal() {
  const context = useContext(ComposeModalContext)
  if (!context) throw new Error('useComposeModal must be used inside ComposeModalProvider')
  return context
}
