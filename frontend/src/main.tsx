import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider } from 'react-router-dom'
import Home from './pages/Home.tsx'
import Callback from './pages/Callback.tsx'
import Profile from './pages/Profile.tsx'
import StatusThread from './pages/StatusThread.tsx'
import PublicTimeline from './pages/PublicTimeline.tsx'
import Notifications from './pages/Notifications.tsx'
import FollowRequests from './pages/FollowRequests.tsx'
import SearchPage from './pages/Search.tsx'
import TagTimeline from './pages/TagTimeline.tsx'
import AccountList from './pages/AccountList.tsx'
import { ThemeProvider } from './components/theme-provider.tsx'
import { ComposeModalProvider } from './components/compose-modal.tsx'
import { getToken } from './auth.ts'
import { loadMe } from './me.ts'
import './styles.css'

// Warm the current-user id cache for edit/delete controls.
const token = getToken()
if (token) void loadMe(token)

// Static routes outrank dynamic ones in React Router's ranking, so
// `/auth/callback`, `/local`, etc. are matched before `/:acct`. `:acct`
// captures the `@username` (or `@username@domain`) segment of Mastodon-style
// permalinks — bare words like `/local` never collide because profiles carry
// the `@` prefix.
const router = createBrowserRouter([
  { path: '/', element: <Home /> },
  { path: '/auth/callback', element: <Callback /> },
  { path: '/local', element: <PublicTimeline /> },
  { path: '/public', element: <PublicTimeline /> },
  { path: '/notifications', element: <Notifications /> },
  { path: '/follow-requests', element: <FollowRequests /> },
  { path: '/search', element: <SearchPage /> },
  { path: '/tags/:name', element: <TagTimeline /> },
  { path: '/:acct', element: <Profile /> },
  // Static second segments outrank the dynamic `:id` thread route, so these
  // win over `/:acct/:id` (status ids are numeric and never collide).
  { path: '/:acct/followers', element: <AccountList /> },
  { path: '/:acct/following', element: <AccountList /> },
  { path: '/:acct/:id', element: <StatusThread /> },
  { path: '*', element: <Home /> },
])

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider defaultTheme="system" storageKey="eunha-theme">
      <ComposeModalProvider>
        <RouterProvider router={router} />
      </ComposeModalProvider>
    </ThemeProvider>
  </StrictMode>,
)
