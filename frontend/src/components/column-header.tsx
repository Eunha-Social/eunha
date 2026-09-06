import type { ReactNode } from 'react'

import { cn } from '@/lib/utils.ts'

/**
 * The heading at the top of a column.
 *
 * Mastodon 5.0 standardised these — the announcement counts "at least 12
 * different headers" before it — and gives each column one bar with the same
 * shape: a title that scrolls the column back to the top when clicked, and
 * room on the right for whatever that page can do.
 *
 * The title is not always the navigation's word for the page. Home is labelled
 * "Following", because that is what the feed *is*; the rail already says where
 * you are.
 */
export function ColumnHeader({
  title,
  children,
  className,
}: {
  title: string
  /** Controls for this column, aligned right. */
  children?: ReactNode
  className?: string
}) {
  return (
    <header
      className={cn(
        'bg-card/85 sticky top-0 z-30 flex items-center gap-2 rounded-t-lg border-b px-3 py-2 backdrop-blur',
        className,
      )}
    >
      <button
        type="button"
        className="min-w-0 flex-1 truncate text-left text-sm font-semibold"
        onClick={() => window.scrollTo({ top: 0, behavior: 'smooth' })}
      >
        {title}
      </button>
      {children && <div className="flex shrink-0 items-center gap-1">{children}</div>}
    </header>
  )
}
