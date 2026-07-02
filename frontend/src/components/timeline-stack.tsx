import type { ReactNode } from 'react'

export function TimelineStack({ children }: { children: ReactNode }) {
  return (
    <div className="bg-card overflow-hidden rounded-md border shadow-sm divide-y">
      {children}
    </div>
  )
}
