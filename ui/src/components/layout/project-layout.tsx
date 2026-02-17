import { NavLink, Outlet, useParams, useOutletContext } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { FileCode, Puzzle, Package, Settings, ArrowLeft, RefreshCw, Rocket, GitBranch } from 'lucide-react'
import { getProject } from '@/lib/api'
import type { Project } from '@/lib/api'
import { cn } from '@/lib/utils'

const projectTabs = [
  { name: 'Specs', href: 'specs', icon: FileCode },
  { name: 'Operations', href: 'operations', icon: GitBranch },
  { name: 'Plugins', href: 'plugins', icon: Puzzle },
  { name: 'Builds', href: 'builds', icon: Package },
  { name: 'Deploy', href: 'deploy', icon: Rocket },
  { name: 'Settings', href: 'settings', icon: Settings },
]

export function ProjectLayout() {
  const { id } = useParams<{ id: string }>()

  const projectQuery = useQuery({
    queryKey: ['project', id],
    queryFn: () => getProject(id!),
    enabled: !!id,
  })

  if (projectQuery.isLoading) {
    return (
      <div className="flex items-center justify-center p-12">
        <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (projectQuery.isError || !projectQuery.data) {
    return (
      <div className="p-8">
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">
            Project not found or failed to load
          </p>
          <NavLink
            to="/projects"
            className="mt-4 inline-flex items-center text-sm text-primary hover:underline"
          >
            <ArrowLeft className="h-4 w-4 mr-1" />
            Back to Projects
          </NavLink>
        </div>
      </div>
    )
  }

  const project = projectQuery.data

  return (
    <div className="flex h-full flex-col">
      {/* Project header */}
      <div className="border-b border-border bg-card/50 px-8 py-4">
        <div className="flex items-center gap-2 text-sm text-muted-foreground mb-1">
          <NavLink
            to="/projects"
            className="hover:text-foreground transition-colors"
          >
            Projects
          </NavLink>
          <span>/</span>
          <span className="text-foreground">{project.name}</span>
        </div>
        <h1 className="text-xl font-semibold">{project.name}</h1>
        {project.description && (
          <p className="text-sm text-muted-foreground mt-1">
            {project.description}
          </p>
        )}
      </div>

      {/* Tab navigation */}
      <div className="border-b border-border bg-card/30 px-8">
        <nav className="flex gap-6">
          {projectTabs.map((tab) => (
            <NavLink
              key={tab.href}
              to={tab.href}
              className={({ isActive }) =>
                cn(
                  'flex items-center gap-2 py-3 text-sm font-medium border-b-2 -mb-px transition-colors',
                  isActive
                    ? 'border-primary text-foreground'
                    : 'border-transparent text-muted-foreground hover:text-foreground hover:border-border'
                )
              }
            >
              <tab.icon className="h-4 w-4" />
              {tab.name}
            </NavLink>
          ))}
        </nav>
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-auto">
        <Outlet context={{ project }} />
      </div>
    </div>
  )
}

// Hook to access project data from child routes
export function useProjectContext() {
  return useOutletContext<{ project: Project }>()
}
