import { useEffect, useState } from 'react'
import { Link, NavLink } from 'react-router-dom'
import { Bell, Home, LogIn, LogOut, Pencil, Search, User, UserPlus } from 'lucide-react'

import { getInstance } from '../api.ts'
import { beginLogin, getToken, logout } from '../auth.ts'
import { clearMe, getMeAccount, loadMe, type MeAccount } from '../me.ts'
import { Button } from '@/components/ui/button.tsx'
import { ModeToggle } from '@/components/mode-toggle.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'
import { cn } from '@/lib/utils.ts'

const navLink =
  'text-muted-foreground hover:text-foreground flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium no-underline'
const activeNavLink = 'bg-muted text-foreground'

function Sidebar({
  token,
  title,
  account,
}: {
  token: string | null
  title: string
  account: MeAccount | null
}) {
  const { openCompose } = useComposeModal()

  return (
    <aside className="sidebar-frame">
      <Link to="/" className="mb-4 px-3 text-lg font-semibold no-underline">
        {title}
      </Link>
      <nav className="flex flex-col gap-1">
        <NavLink
          to="/"
          end
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Home className="size-4" /> Home
        </NavLink>
        {token && account && (
          <NavLink
            to={`/@${account.acct}`}
            className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
          >
            <User className="size-4" /> Profile
          </NavLink>
        )}
        <NavLink
          to="/notifications"
          className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
        >
          <Bell className="size-4" /> Notifications
        </NavLink>
        {token && (
          <NavLink
            to="/follow-requests"
            className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
          >
            <UserPlus className="size-4" /> Follow requests
          </NavLink>
        )}
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
  const [instanceTitle, setInstanceTitle] = useState<string | null>(() =>
    document.title === 'eunha' ? null : document.title,
  )
  const [account, setAccount] = useState<MeAccount | null>(() => getMeAccount())
  const displayTitle = title ?? instanceTitle ?? ''

  useEffect(() => {
    if (title) return
    getInstance()
      .then((instance) => setInstanceTitle(instance.title))
      .catch(() => {})
  }, [title])

  useEffect(() => {
    if (!token) {
      setAccount(null)
      return
    }

    let cancelled = false
    loadMe(token)
      .then((me) => {
        if (!cancelled) setAccount(me)
      })
      .catch(() => {
        if (!cancelled) setAccount(null)
      })

    return () => {
      cancelled = true
    }
  }, [token])

  useEffect(() => {
    if (!displayTitle) return
    document.title = displayTitle
  }, [displayTitle])

  return (
    <>
      <Sidebar token={token} title={displayTitle} account={account} />
      <header className="mb-3 flex items-center justify-between border-b pb-2 xl:hidden">
        <Link to="/" className="text-lg font-semibold no-underline">
          {displayTitle}
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
