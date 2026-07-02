import { Link, NavLink } from 'react-router-dom'
import { Bell, Home, LogIn, LogOut, Pencil, Search, Users } from 'lucide-react'

import { beginLogin, getToken, logout } from '../auth.ts'
import { clearMe } from '../me.ts'
import { Button } from '@/components/ui/button.tsx'
import { ModeToggle } from '@/components/mode-toggle.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'
import { cn } from '@/lib/utils.ts'

const navLink =
  'text-muted-foreground hover:text-foreground flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium no-underline'
const activeNavLink = 'bg-muted text-foreground'

function Sidebar({ token }: { token: string | null }) {
  const { openCompose } = useComposeModal()

  return (
    <aside className="fixed top-0 left-0 hidden h-screen w-56 flex-col border-r bg-background px-3 py-4 lg:flex">
      <Link to="/" className="mb-4 px-3 text-lg font-semibold no-underline">
        eunha
      </Link>
      <nav className="flex flex-col gap-1">
        <NavLink
          to="/"
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Home className="size-4" /> Home
        </NavLink>
        <NavLink
          to="/local"
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Users className="size-4" /> Local
        </NavLink>
        <NavLink
          to="/public"
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Users className="size-4" /> Public
        </NavLink>
        <NavLink
          to="/notifications"
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Bell className="size-4" /> Notifications
        </NavLink>
        <NavLink
          to="/search"
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Search className="size-4" /> Search
        </NavLink>
      </nav>
      {token && (
        <Button className="mt-4 w-full" onClick={() => openCompose()}>
          <Pencil /> Post
        </Button>
      )}
      <div className="mt-auto flex items-center justify-between gap-2">
        <ModeToggle />
        {token ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              logout()
              clearMe()
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
      </div>
    </aside>
  )
}

export function TopBar({ title }: { title?: string }) {
  const token = getToken()
  const { openCompose } = useComposeModal()
  return (
    <>
      <Sidebar token={token} />
      <header className="mb-3 flex items-center justify-between border-b pb-2 lg:hidden">
        <Link to="/" className="text-lg font-semibold no-underline">
          {title ?? 'eunha'}
        </Link>
        <div className="flex items-center gap-2">
          {token && (
            <Button size="sm" onClick={() => openCompose()}>
              <Pencil /> Post
            </Button>
          )}
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
                clearMe()
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
    </>
  )
}
