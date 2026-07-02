import type { ReactNode } from 'react'

export function TimelineStack({ children }: { children: ReactNode }) {
  return (
    <div className="border-x [&>*+*]:border-t [&>*+*]:border-border">
      {children}
    </div>
  )
}
