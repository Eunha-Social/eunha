import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { createPortal } from 'react-dom'
import { LockOpen, X } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { getToken } from '../auth.ts'
import { Compose } from '@/components/compose.tsx'
import { Button } from '@/components/ui/button.tsx'
import { cn } from '@/lib/utils.ts'

type ComposeOptions = {
  replyTo?: mastodon.v1.Status | null
  quoteOf?: mastodon.v1.Status | null
  // Open as a message. Pass an account to address it, or `null` to open an
  // unaddressed one — the distinction Mastodon 5.0 draws between messaging a
  // person from their profile and starting one from the Messages page.
  messageTo?: mastodon.v1.Account | null
  onPosted?: (status: mastodon.v1.Status) => void
}

type ComposeModalContextValue = {
  openCompose: (options?: ComposeOptions) => void
}

const ComposeModalContext = createContext<ComposeModalContextValue | null>(null)

export function ComposeModalProvider({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false)
  const [replyTo, setReplyTo] = useState<mastodon.v1.Status | null>(null)
  const [quoteOf, setQuoteOf] = useState<mastodon.v1.Status | null>(null)
  const [messageTo, setMessageTo] = useState<mastodon.v1.Account | null | undefined>(
    undefined,
  )
  const [onPosted, setOnPosted] =
    useState<((status: mastodon.v1.Status) => void) | null>(null)

  const close = useCallback(() => {
    setOpen(false)
    setReplyTo(null)
    setQuoteOf(null)
    setMessageTo(undefined)
    setOnPosted(null)
  }, [])

  const openCompose = useCallback((options?: ComposeOptions) => {
    setReplyTo(options?.replyTo ?? null)
    setQuoteOf(options?.quoteOf ?? null)
    setMessageTo('messageTo' in (options ?? {}) ? (options?.messageTo ?? null) : undefined)
    setOnPosted(() => options?.onPosted ?? null)
    setOpen(true)
  }, [])

  const value = useMemo(() => ({ openCompose }), [openCompose])
  const token = getToken()
  // A reply is a reply even when it is private, matching upstream's
  // `selectComposeType` — reply wins over message.
  const isMessage = messageTo !== undefined && !replyTo

  return (
    <ComposeModalContext.Provider value={value}>
      {children}
      {open && token
        ? createPortal(
            <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/45 px-3 py-10 sm:py-16">
              <div
                className={cn(
                  'text-card-foreground w-full max-w-xl rounded-md border shadow-lg',
                  // The ground changes in message mode, which is the whole
                  // point of it being a mode: you can see what you are writing
                  // into without reading a label.
                  isMessage ? 'bg-secondary' : 'bg-card',
                )}
              >
                <div className="flex items-center justify-between border-b px-4 py-2">
                  <h2 className="text-sm font-semibold">
                    {replyTo
                      ? 'Reply'
                      : isMessage
                        ? 'New message'
                        : quoteOf
                          ? 'Quote post'
                          : 'Post'}
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
                {isMessage && (
                  <p className="text-muted-foreground flex items-center gap-1.5 border-b px-4 py-2 text-xs">
                    <LockOpen className="size-3.5 shrink-0" />
                    Messages are not end-to-end encrypted.
                  </p>
                )}
                <Compose
                  token={token}
                  replyTo={replyTo}
                  quoteOf={quoteOf}
                  messageTo={messageTo}
                  onCancelReply={replyTo || quoteOf ? close : undefined}
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
