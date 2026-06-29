import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider } from 'react-router-dom'
import Home from './pages/Home.tsx'
import Callback from './pages/Callback.tsx'
import './styles.css'

const router = createBrowserRouter([
  { path: '/', element: <Home /> },
  { path: '/auth/callback', element: <Callback /> },
  // Everything else renders Home for now; real routes land in later milestones.
  { path: '*', element: <Home /> },
])

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
)
