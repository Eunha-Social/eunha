import { Link } from 'react-router-dom'
import { LogIn, LogOut, Search } from 'lucide-react'

import { beginLogin, getToken, logout } from '../auth.ts'
import { Button } from '@/components/ui/button.tsx'
import { ModeToggle } from '@/components/mode-toggle.tsx'

export function TopBar({ title }: { title?: string }) {
  const token = getToken()
  return (
    <header className="mb-6 flex items-center justify-between border-b pb-3">
      <Link to="/" className="text-lg font-semibold no-underline">
        {title ?? 'eunha'}
      </Link>
      <div className="flex items-center gap-2">
        <Button asChild variant="ghost" size="icon" aria-label="Search">
          <Link to="/search">
            <Search />
          </Link>
        </Button>
        {token ? (
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              logout()
              location.assign('/')
            }}
          >
            <LogOut /> Sign out
          </Button>
        ) : (
          <Button size="sm" onClick={() => beginLogin()}>
            <LogIn /> Sign in
          </Button>
        )}
        <ModeToggle />
      </div>
    </header>
  )
}
