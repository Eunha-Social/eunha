import { useEffect, useState } from 'react'
import { Link, NavLink } from 'react-router-dom'
import { Bell, LogIn, LogOut, Pencil, Search, User } from 'lucide-react'

import type { mastodon } from '../masto.ts'
import { getCurrentAccount, getInstance } from '../api.ts'
import { beginLogin, getToken, logout } from '../auth.ts'
import { clearMe } from '../me.ts'
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
  account: mastodon.v1.AccountCredentials | null
}) {
  const { openCompose } = useComposeModal()

  return (
    <aside className="sidebar-frame">
      <Link to="/" className="mb-4 px-3 text-lg font-semibold no-underline">
        {title}
      </Link>
      <nav className="flex flex-col gap-1">
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
  const [account, setAccount] = useState<mastodon.v1.AccountCredentials | null>(null)
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
    getCurrentAccount(token)
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
