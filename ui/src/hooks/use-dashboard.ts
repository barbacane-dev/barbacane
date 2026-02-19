import { useQuery } from '@tanstack/react-query'
import {
  listProjects,
  listPlugins,
  getHealth,
  listProjectSpecs,
  listProjectPlugins,
  listProjectDataPlanes,
  listProjectCompilations,
} from '@/lib/api'
import type {
  Project,
  Spec,
  ProjectPluginConfig,
  DataPlane,
  Compilation,
  HealthResponse,
} from '@/lib/api'

export interface ProjectDetails {
  project: Project
  specs: Spec[]
  plugins: ProjectPluginConfig[]
  dataPlanes: DataPlane[]
  compilations: Compilation[]
}

export interface DashboardStats {
  projects: number
  plugins: number
  dataPlanes: { online: number; total: number }
  compilations: number
}

export interface RecentCompilation extends Compilation {
  projectName: string
  projectId: string
}

export function useDashboard() {
  const projectsQuery = useQuery({
    queryKey: ['projects'],
    queryFn: listProjects,
  })

  const pluginsQuery = useQuery({
    queryKey: ['plugins'],
    queryFn: () => listPlugins(),
  })

  const healthQuery = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
  })

  const projects = projectsQuery.data ?? []

  const projectDetailsQuery = useQuery({
    queryKey: ['dashboard', 'project-details', projects.map((p) => p.id)],
    queryFn: async (): Promise<ProjectDetails[]> => {
      const details = await Promise.all(
        projects.map(async (project) => {
          const [specs, plugins, dataPlanes, compilations] = await Promise.all([
            listProjectSpecs(project.id).catch((): Spec[] => []),
            listProjectPlugins(project.id).catch((): ProjectPluginConfig[] => []),
            listProjectDataPlanes(project.id).catch((): DataPlane[] => []),
            listProjectCompilations(project.id).catch((): Compilation[] => []),
          ])
          return { project, specs, plugins, dataPlanes, compilations }
        })
      )
      return details
    },
    enabled: projects.length > 0,
  })

  const details = projectDetailsQuery.data ?? []

  const stats: DashboardStats = {
    projects: projects.length,
    plugins: pluginsQuery.data?.length ?? 0,
    dataPlanes: {
      online: details
        .flatMap((d) => d.dataPlanes)
        .filter((dp) => dp.status === 'online').length,
      total: details.flatMap((d) => d.dataPlanes).length,
    },
    compilations: details.flatMap((d) => d.compilations).length,
  }

  const recentCompilations: RecentCompilation[] = details
    .flatMap((d) =>
      d.compilations.map((c) => ({
        ...c,
        projectName: d.project.name,
        projectId: d.project.id,
      }))
    )
    .sort(
      (a, b) =>
        new Date(b.started_at).getTime() - new Date(a.started_at).getTime()
    )
    .slice(0, 10)

  return {
    projects,
    details,
    stats,
    recentCompilations,
    health: healthQuery.data as HealthResponse | undefined,
    isLoading: projectsQuery.isLoading,
    isError: projectsQuery.isError,
    refetch: () => {
      projectsQuery.refetch()
      pluginsQuery.refetch()
      projectDetailsQuery.refetch()
    },
  }
}
