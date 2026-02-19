import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider, Navigate } from 'react-router-dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import './index.css'

import { AuthProvider } from '@/lib/auth'
import { ProtectedRoute } from '@/components/auth'
import { RootLayout, ProjectLayout } from '@/components/layout'
import {
  DashboardPage,
  SpecsPage,
  PluginsPage,
  ArtifactsPage,
  ActivityPage,
  SettingsPage,
  LoginPage,
  InitPage,
  ProjectsPage,
  ProjectSpecsPage,
  ProjectPluginsPage,
  ProjectBuildsPage,
  ProjectOperationsPage,
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
      { index: true, element: <DashboardPage /> },
      // Projects
      { path: 'projects', element: <ProjectsPage /> },
      {
        path: 'projects/:id',
        element: <ProjectLayout />,
        children: [
          { index: true, element: <Navigate to="specs" replace /> },
          { path: 'specs', element: <ProjectSpecsPage /> },
          { path: 'operations', element: <ProjectOperationsPage /> },
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
