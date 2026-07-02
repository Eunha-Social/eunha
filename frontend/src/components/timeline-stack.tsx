import type { ReactNode } from 'react'

export function TimelineStack({ children }: { children: ReactNode }) {
  return (
    <div className="[&>*+*]:border-t [&>*+*]:border-border">
      {children}
    </div>
  )
}
