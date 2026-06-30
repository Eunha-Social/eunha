import { NavLink } from 'react-router-dom'

import { getToken } from '../auth.ts'
import { cn } from '@/lib/utils.ts'

const base =
  'border-b-2 border-transparent px-3 py-2 text-sm font-medium text-muted-foreground no-underline hover:text-foreground'
const active = 'border-primary text-foreground'

const cls = ({ isActive }: { isActive: boolean }) => cn(base, isActive && active)

export function TimelineTabs() {
  const token = getToken()
  return (
    <nav className="mb-4 flex gap-1 border-b">
      {token && (
        <NavLink to="/" end className={cls}>
          Home
        </NavLink>
      )}
      {token && (
        <NavLink to="/notifications" className={cls}>
          Notifications
        </NavLink>
      )}
      <NavLink to="/local" className={cls}>
        Local
      </NavLink>
      <NavLink to="/public" className={cls}>
        Federated
      </NavLink>
    </nav>
  )
}
