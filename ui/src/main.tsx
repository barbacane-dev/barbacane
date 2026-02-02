import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider, Navigate } from 'react-router-dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import './index.css'

import { AuthProvider } from '@/lib/auth'
import { ProtectedRoute } from '@/components/auth'
import { RootLayout, ProjectLayout } from '@/components/layout'
import {
  SpecsPage,
  PluginsPage,
  ArtifactsPage,
  ActivityPage,
  SettingsPage,
  LoginPage,
  InitPage,
  ApiDocsPage,
  ProjectsPage,
  ProjectSpecsPage,
  ProjectPluginsPage,
  ProjectBuildsPage,
  ProjectDeployPage,
  ProjectSettingsPage,
} from '@/pages'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60, // 1 minute
      retry: 1,
    },
  },
})

const router = createBrowserRouter([
  {
    path: '/login',
    element: <LoginPage />,
  },
  {
    path: '/',
    element: (
      <ProtectedRoute>
        <RootLayout />
      </ProtectedRoute>
    ),
    children: [
      // Landing page is now projects
      { index: true, element: <Navigate to="/projects" replace /> },
      // Projects
      { path: 'projects', element: <ProjectsPage /> },
      {
        path: 'projects/:id',
        element: <ProjectLayout />,
        children: [
          { index: true, element: <Navigate to="specs" replace /> },
          { path: 'specs', element: <ProjectSpecsPage /> },
          { path: 'plugins', element: <ProjectPluginsPage /> },
          { path: 'builds', element: <ProjectBuildsPage /> },
          { path: 'deploy', element: <ProjectDeployPage /> },
          { path: 'settings', element: <ProjectSettingsPage /> },
        ],
      },
      // Global pages (backward compatibility + admin)
      { path: 'specs', element: <SpecsPage /> },
      { path: 'plugin-registry', element: <PluginsPage /> },
      { path: 'artifacts', element: <ArtifactsPage /> },
      { path: 'activity', element: <ActivityPage /> },
      { path: 'api-docs', element: <ApiDocsPage /> },
      { path: 'init', element: <InitPage /> },
      { path: 'settings', element: <SettingsPage /> },
      // Legacy route redirect
      { path: 'plugins', element: <Navigate to="/plugin-registry" replace /> },
    ],
  },
])

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <AuthProvider>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </AuthProvider>
  </StrictMode>
)
