import { useEffect, useState } from 'react'
import { Link, NavLink, useLocation, useNavigate } from 'react-router-dom'
import {
  Bell,
  Bookmark,
  Compass,
  Home,
  Info,
  LogIn,
  LogOut,
  Pencil,
  Search,
  Settings,
  User,
  UserPlus,
} from 'lucide-react'

import { getInstance } from '../api.ts'
import { beginLogin, getToken, logout } from '../auth.ts'
import { clearMe, getMeAccount, loadMe, type MeAccount } from '../me.ts'
import { Button } from '@/components/ui/button.tsx'
import { ModeToggle } from '@/components/mode-toggle.tsx'
import { useComposeModal } from '@/components/compose-modal.tsx'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from '@/components/ui/sidebar.tsx'
import { cn } from '@/lib/utils.ts'

const navLink =
  'text-muted-foreground hover:text-foreground flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium no-underline'
const activeNavLink = 'bg-muted text-foreground'

type IconType = React.ComponentType<{ className?: string }>
type NavItem = { to: string; end?: boolean; icon: IconType; label: string }

function useNavItems(token: string | null, account: MeAccount | null): NavItem[] {
  // "/" is a timeline for a signed-out visitor too, so About is the only place
  // left that says what this instance is — and this is the only link to it.
  if (!token) {
    return [
      { to: '/explore', icon: Compass, label: 'Explore' },
      { to: '/about', icon: Info, label: 'About' },
    ]
  }
  const items: NavItem[] = [
    { to: '/', end: true, icon: Home, label: 'Home' },
    { to: '/search', icon: Search, label: 'Search' },
    { to: '/explore', icon: Compass, label: 'Explore' },
    { to: '/notifications', icon: Bell, label: 'Notifications' },
    { to: '/bookmarks', icon: Bookmark, label: 'Bookmarks' },
  ]
  if (account) {
    items.push({ to: `/@${account.acct}`, icon: User, label: 'Profile' })
  }
  items.push({ to: '/settings', icon: Settings, label: 'Settings' })
  items.push({ to: '/about', icon: Info, label: 'About' })
  return items
}

function AuthButton({
  token,
  size = 'sm',
}: {
  token: string | null
  size?: React.ComponentProps<typeof Button>['size']
}) {
  if (token) {
    return (
      <Button
        variant="ghost"
        size={size}
        onClick={() => {
          logout()
          clearMe()
          location.assign('/')
        }}
      >
        <LogOut /> Sign out
      </Button>
    )
  }
  return (
    <Button size={size} onClick={() => beginLogin()}>
      <LogIn /> Sign in
    </Button>
  )
}

function SignUpButton({ className }: { className?: string }) {
  const navigate = useNavigate()
  return (
    <Button className={className} onClick={() => navigate('/signup')}>
      <UserPlus /> Create account
    </Button>
  )
}

// The wide-screen rail: a fixed sidebar floating in the left margin of the
// centered column. Shown at `md` and up (see `.sidebar-frame`).
function DesktopRail({
  token,
  title,
  account,
  registrationsOpen,
}: {
  token: string | null
  title: string
  account: MeAccount | null
  registrationsOpen: boolean
}) {
  const { openCompose } = useComposeModal()
  const navItems = useNavItems(token, account)

  return (
    <aside className="sidebar-frame">
      <Link to="/" className="mb-4 px-3 text-lg font-semibold no-underline">
        {title}
      </Link>
      <nav className="flex flex-col gap-1">
        {navItems.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.end}
            className={({ isActive }) => cn(navLink, isActive && activeNavLink)}
          >
            <item.icon className="size-4" /> {item.label}
          </NavLink>
        ))}
      </nav>
      {token && (
        <Button className="mt-4 w-full" onClick={() => openCompose()}>
          <Pencil /> Post
        </Button>
      )}
      {!token && registrationsOpen && <SignUpButton className="mt-4 w-full" />}
      <div className="mt-auto flex items-center justify-between gap-2">
        <ModeToggle />
        <AuthButton token={token} />
      </div>
    </aside>
  )
}

// The small-screen drawer: the shadcn Sidebar component, which renders inside a
// portaled Sheet toggled by `SidebarTrigger`. Only rendered below `xl`.
function MobileDrawer({
  token,
  title,
  account,
  registrationsOpen,
}: {
  token: string | null
  title: string
  account: MeAccount | null
  registrationsOpen: boolean
}) {
  const { openCompose } = useComposeModal()
  const { setOpenMobile } = useSidebar()
  const location = useLocation()
  const navItems = useNavItems(token, account)
  const close = () => setOpenMobile(false)
  const isActive = (to: string, end?: boolean) =>
    end
      ? location.pathname === to
      : location.pathname === to || location.pathname.startsWith(`${to}/`)

  return (
    <Sidebar>
      <SidebarHeader>
        <Link
          to="/"
          onClick={close}
          className="px-2 py-1 text-lg font-semibold no-underline"
        >
          {title}
        </Link>
      </SidebarHeader>
      <SidebarContent>
        <SidebarGroup>
          <SidebarMenu>
            {navItems.map((item) => (
              <SidebarMenuItem key={item.to}>
                <SidebarMenuButton
                  isActive={isActive(item.to, item.end)}
                  onClick={close}
                  render={<NavLink to={item.to} end={item.end} />}
                >
                  <item.icon />
                  <span>{item.label}</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            ))}
          </SidebarMenu>
        </SidebarGroup>
      </SidebarContent>
      <SidebarFooter>
        {token && (
          <Button
            className="w-full"
            onClick={() => {
              close()
              openCompose()
            }}
          >
            <Pencil /> Post
          </Button>
        )}
        {!token && registrationsOpen && <SignUpButton className="w-full" />}
        <div className="flex items-center justify-between gap-2">
          <ModeToggle />
          <AuthButton token={token} />
        </div>
      </SidebarFooter>
    </Sidebar>
  )
}

function MobileHeader({
  token,
  title,
  registrationsOpen,
}: {
  token: string | null
  title: string
  registrationsOpen: boolean
}) {
  const { openCompose } = useComposeModal()
  const navigate = useNavigate()

  return (
    <header className="mb-3 flex items-center gap-2 border-b pb-2 md:hidden">
      <SidebarTrigger className="-ml-1" aria-label="Open menu" />
      <Link to="/" className="text-lg font-semibold no-underline">
        {title}
      </Link>
      <div className="ml-auto flex items-center gap-2">
        {token ? (
          <Button size="sm" onClick={() => openCompose()}>
            <Pencil /> Post
          </Button>
        ) : (
          <>
            {registrationsOpen && (
              <Button size="sm" onClick={() => navigate('/signup')}>
                <UserPlus /> Create account
              </Button>
            )}
            <Button
              variant={registrationsOpen ? 'ghost' : 'default'}
              size="sm"
              onClick={() => beginLogin()}
            >
              <LogIn /> Sign in
            </Button>
          </>
        )}
      </div>
    </header>
  )
}

function TopBarInner({
  token,
  title,
  account,
  registrationsOpen,
}: {
  token: string | null
  title: string
  account: MeAccount | null
  registrationsOpen: boolean
}) {
  const { isMobile } = useSidebar()

  return (
    <>
      <DesktopRail
        token={token}
        title={title}
        account={account}
        registrationsOpen={registrationsOpen}
      />
      <MobileHeader
        token={token}
        title={title}
        registrationsOpen={registrationsOpen}
      />
      {/* Below `xl` the Sidebar renders as a Sheet drawer; above it the
          DesktopRail handles navigation, so skip the component's own rail. */}
      {isMobile && (
        <MobileDrawer
          token={token}
          title={title}
          account={account}
          registrationsOpen={registrationsOpen}
        />
      )}
    </>
  )
}

export function TopBar({ title }: { title?: string }) {
  const token = getToken()
  const [instanceTitle, setInstanceTitle] = useState<string | null>(() =>
    document.title === 'eunha' ? null : document.title,
  )
  const [account, setAccount] = useState<MeAccount | null>(() => getMeAccount())
  const [registrationsOpen, setRegistrationsOpen] = useState(false)
  const displayTitle = title ?? instanceTitle ?? ''

  useEffect(() => {
    if (title) return
    getInstance()
      .then((instance) => setInstanceTitle(instance.title))
      .catch(() => {})
  }, [title])

  // Only logged-out users see the sign-up affordance, so only they need the
  // instance's registration status.
  useEffect(() => {
    if (token) return
    let cancelled = false
    getInstance()
      .then((instance) => {
        if (!cancelled) setRegistrationsOpen(instance.registrations.enabled)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [token])

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
    <SidebarProvider className="block min-h-0 w-full">
      <TopBarInner
        token={token}
        title={displayTitle}
        account={account}
        registrationsOpen={registrationsOpen}
      />
    </SidebarProvider>
  )
}
