import { NavLink, useLocation } from 'react-router-dom'

import { getToken } from '../auth.ts'
import { cn } from '@/lib/utils.ts'

const base =
  'border-b-2 border-transparent px-3 py-2 text-sm font-medium text-muted-foreground no-underline hover:text-foreground'
const active = 'border-primary text-foreground'

const cls = ({ isActive }: { isActive: boolean }) => cn(base, isActive && active)

export function TimelineTabs() {
  const token = getToken()
  const { pathname } = useLocation()
  // Signed out, "/" *is* the local timeline, so the tab that owns it says so
  // rather than leaving all three unlit on the page a visitor lands on.
  const localActive = pathname === '/local' || (!token && pathname === '/')

  return (
    <nav className="bg-background sticky top-0 z-30 -mx-3 mb-2 flex gap-1 border-b px-3">
      {token && (
        <NavLink to="/" end className={cls}>
          Home
        </NavLink>
      )}
      <NavLink to="/local" className={cn(base, localActive && active)}>
        Local
      </NavLink>
      <NavLink to="/public" className={cls}>
        Federated
      </NavLink>
    </nav>
  )
}
