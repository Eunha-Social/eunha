import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider } from 'react-router-dom'
import Home from './pages/Home.tsx'
import Callback from './pages/Callback.tsx'
import Profile from './pages/Profile.tsx'
import StatusThread from './pages/StatusThread.tsx'
import PublicTimeline from './pages/PublicTimeline.tsx'
import Notifications from './pages/Notifications.tsx'
import { ThemeProvider } from './components/theme-provider.tsx'
import './styles.css'

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
  { path: '/:acct', element: <Profile /> },
  { path: '/:acct/:id', element: <StatusThread /> },
  { path: '*', element: <Home /> },
])

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider defaultTheme="system" storageKey="eunha-theme">
      <RouterProvider router={router} />
    </ThemeProvider>
  </StrictMode>,
)
