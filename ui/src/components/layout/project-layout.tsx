import { NavLink, Outlet, useParams, useOutletContext, useNavigate } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { FileCode, Puzzle, Package, Settings, ArrowLeft, RefreshCw, Rocket, GitBranch, Play, Loader2 } from 'lucide-react'
import { getProject, listProjectSpecs, listProjectCompilations, startCompilation } from '@/lib/api'
import type { Project } from '@/lib/api'
import { Button } from '@/components/ui'
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
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const projectQuery = useQuery({
    queryKey: ['project', id],
    queryFn: () => getProject(id!),
    enabled: !!id,
  })

  const specsQuery = useQuery({
    queryKey: ['project-specs', id],
    queryFn: () => listProjectSpecs(id!),
    enabled: !!id,
  })

  const compilationsQuery = useQuery({
    queryKey: ['project-compilations', id],
    queryFn: () => listProjectCompilations(id!),
    enabled: !!id,
  })

  const hasActiveCompilation = (compilationsQuery.data ?? []).some(
    (c) => c.status === 'pending' || c.status === 'compiling'
  )

  const compileMutation = useMutation({
    mutationFn: async () => {
      const specs = specsQuery.data ?? []
      if (specs.length === 0) {
        throw new Error('No specs to compile')
      }
      const [primary, ...rest] = specs
      return startCompilation(primary.id, {
        production: projectQuery.data?.production_mode,
        additional_specs: rest.length > 0 ? rest.map((s) => s.id) : undefined,
      })
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-compilations', id] })
      queryClient.invalidateQueries({ queryKey: ['project-artifacts', id] })
      navigate(`/projects/${id}/builds`)
    },
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
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-xl font-semibold">{project.name}</h1>
            {project.description && (
              <p className="text-sm text-muted-foreground mt-1">
                {project.description}
              </p>
            )}
          </div>
          <Button
            size="sm"
            onClick={() => compileMutation.mutate()}
            disabled={
              (specsQuery.data ?? []).length === 0 ||
              hasActiveCompilation ||
              compileMutation.isPending
            }
          >
            {compileMutation.isPending || hasActiveCompilation ? (
              <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            ) : (
              <Play className="h-4 w-4 mr-2" />
            )}
            {compileMutation.isPending
              ? 'Starting...'
              : hasActiveCompilation
                ? 'Build in progress'
                : 'Compile'}
          </Button>
        </div>
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
