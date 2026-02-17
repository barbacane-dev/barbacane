import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  FolderKanban,
  Server,
  Puzzle,
  Hammer,
  Plus,
  Sparkles,
  FileCode,
  CheckCircle,
  XCircle,
  Clock,
  Loader2,
  ArrowRight,
  RefreshCw,
  Rocket,
  Upload,
  Blocks,
} from 'lucide-react'
import { createProject } from '@/lib/api'
import type { CreateProjectRequest } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { useAuth } from '@/lib/auth'
import { useDashboard } from '@/hooks'
import type { ProjectDetails, RecentCompilation } from '@/hooks/use-dashboard'
import { cn } from '@/lib/utils'

function relativeTime(dateStr: string): string {
  const now = Date.now()
  const then = new Date(dateStr).getTime()
  const seconds = Math.floor((now - then) / 1000)

  if (seconds < 60) return 'just now'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  if (days < 30) return `${days}d ago`
  return new Date(dateStr).toLocaleDateString('en-US', {
    month: 'short',
    day: 'numeric',
  })
}

function compilationStatusBadge(status: string) {
  switch (status) {
    case 'succeeded':
      return (
        <Badge variant="success">
          <CheckCircle className="h-3 w-3 mr-1" />
          Succeeded
        </Badge>
      )
    case 'failed':
      return (
        <Badge variant="destructive">
          <XCircle className="h-3 w-3 mr-1" />
          Failed
        </Badge>
      )
    case 'compiling':
      return (
        <Badge variant="warning">
          <Loader2 className="h-3 w-3 mr-1 animate-spin" />
          Compiling
        </Badge>
      )
    case 'pending':
      return (
        <Badge variant="warning">
          <Clock className="h-3 w-3 mr-1" />
          Pending
        </Badge>
      )
    default:
      return <Badge variant="secondary">Unknown</Badge>
  }
}

// --- Onboarding View ---

const onboardingSteps = [
  {
    number: 1,
    icon: FolderKanban,
    title: 'Create a project',
    description:
      'A project groups your API specs, plugins, and deployments together.',
  },
  {
    number: 2,
    icon: Upload,
    title: 'Upload an API spec',
    description:
      'Add your OpenAPI or AsyncAPI specification file — YAML or JSON.',
  },
  {
    number: 3,
    icon: Blocks,
    title: 'Add middleware plugins',
    description:
      'Enable authentication, rate limiting, logging, and more with WASM plugins.',
  },
  {
    number: 4,
    icon: Rocket,
    title: 'Compile & deploy',
    description:
      'Build an artifact from your spec and push it to your gateway instances.',
  },
]

function OnboardingView() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { user } = useAuth()
  const [showCreateDialog, setShowCreateDialog] = useState(false)
  const [projectName, setProjectName] = useState('')
  const [projectDescription, setProjectDescription] = useState('')

  const createMutation = useMutation({
    mutationFn: (data: CreateProjectRequest) => createProject(data),
    onSuccess: (project) => {
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      setShowCreateDialog(false)
      setProjectName('')
      setProjectDescription('')
      navigate(`/projects/${project.id}/specs`)
    },
  })

  const handleCreate = () => {
    if (!projectName.trim()) return
    createMutation.mutate({
      name: projectName.trim(),
      description: projectDescription.trim() || undefined,
    })
  }

  const firstName = user?.name?.split(' ')[0] ?? 'there'

  return (
    <div className="flex flex-col items-center justify-center px-8 py-16">
      <div className="max-w-3xl text-center">
        <h1 className="text-3xl font-bold">
          Welcome to <span className="text-gradient">Barbacane</span>,{' '}
          {firstName}!
        </h1>
        <p className="mt-4 text-lg text-muted-foreground leading-relaxed">
          Barbacane is your API gateway — it takes your API specification and
          turns it into a running gateway with built-in validation, security, and
          monitoring.
        </p>
      </div>

      <div className="mt-12 grid w-full max-w-3xl gap-4 md:grid-cols-2">
        {onboardingSteps.map((step) => (
          <Card key={step.number}>
            <CardContent className="p-5">
              <div className="flex items-start gap-4">
                <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary font-bold">
                  {step.number}
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <step.icon className="h-4 w-4 text-muted-foreground" />
                    <h3 className="font-medium">{step.title}</h3>
                  </div>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {step.description}
                  </p>
                </div>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      <div className="mt-10 flex gap-3">
        <Button size="lg" onClick={() => setShowCreateDialog(true)}>
          <Plus className="h-4 w-4 mr-2" />
          Create Your First Project
        </Button>
        <Button variant="outline" size="lg" onClick={() => navigate('/init')}>
          <Sparkles className="h-4 w-4 mr-2" />
          Start from Template
        </Button>
      </div>

      {/* Create Project Dialog */}
      {showCreateDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="w-full max-w-md">
            <CardContent className="p-6">
              <h2 className="text-lg font-semibold mb-4">
                Create Your First Project
              </h2>
              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium mb-2">
                    Project Name *
                  </label>
                  <input
                    type="text"
                    value={projectName}
                    onChange={(e) => setProjectName(e.target.value)}
                    placeholder="My API Gateway"
                    className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
                    autoFocus
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') handleCreate()
                    }}
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium mb-2">
                    Description
                  </label>
                  <input
                    type="text"
                    value={projectDescription}
                    onChange={(e) => setProjectDescription(e.target.value)}
                    placeholder="A brief description of this project"
                    className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') handleCreate()
                    }}
                  />
                </div>
                {createMutation.isError && (
                  <p className="text-sm text-destructive">
                    {createMutation.error instanceof Error
                      ? createMutation.error.message
                      : 'Failed to create project'}
                  </p>
                )}
                <div className="flex justify-end gap-2 pt-2">
                  <Button
                    variant="outline"
                    onClick={() => {
                      setShowCreateDialog(false)
                      setProjectName('')
                      setProjectDescription('')
                    }}
                  >
                    Cancel
                  </Button>
                  <Button
                    onClick={handleCreate}
                    disabled={
                      !projectName.trim() || createMutation.isPending
                    }
                  >
                    {createMutation.isPending ? 'Creating...' : 'Create'}
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  )
}

// --- Stat Card ---

function StatCard({
  icon: Icon,
  label,
  value,
  subtitle,
  onClick,
}: {
  icon: React.ComponentType<{ className?: string }>
  label: string
  value: string | number
  subtitle?: string
  onClick?: () => void
}) {
  return (
    <Card
      className={onClick ? 'cursor-pointer hover:border-primary/50 transition-colors' : undefined}
      onClick={onClick}
    >
      <CardContent className="p-5">
        <div className="flex items-center gap-3">
          <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary/10">
            <Icon className="h-5 w-5 text-primary" />
          </div>
          <div>
            <p className="text-sm text-muted-foreground">{label}</p>
            <p className="text-xl font-semibold">{value}</p>
            {subtitle && (
              <p className="text-xs text-muted-foreground">{subtitle}</p>
            )}
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

// --- Project Health Card ---

function ProjectHealthCard({ detail }: { detail: ProjectDetails }) {
  const navigate = useNavigate()
  const { project, specs, plugins, dataPlanes, compilations } = detail
  const onlineCount = dataPlanes.filter((dp) => dp.status === 'online').length
  const latestCompilation = compilations.sort(
    (a, b) =>
      new Date(b.started_at).getTime() - new Date(a.started_at).getTime()
  )[0]

  return (
    <Card
      className="cursor-pointer hover:border-primary/50 transition-colors"
      onClick={() => navigate(`/projects/${project.id}/specs`)}
    >
      <CardContent className="p-4">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-primary/10">
              <FolderKanban className="h-4 w-4 text-primary" />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <h3 className="font-medium">{project.name}</h3>
                {project.production_mode && (
                  <Badge variant="outline" className="text-xs">
                    Production
                  </Badge>
                )}
              </div>
            </div>
          </div>
          <ArrowRight className="h-4 w-4 text-muted-foreground" />
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
          <span className="flex items-center gap-1">
            <FileCode className="h-3.5 w-3.5" />
            {specs.length} spec{specs.length !== 1 ? 's' : ''}
          </span>
          <span className="flex items-center gap-1">
            <Puzzle className="h-3.5 w-3.5" />
            {plugins.length} plugin{plugins.length !== 1 ? 's' : ''}
          </span>
          <span className="flex items-center gap-1">
            <span
              className={cn(
                'inline-block h-2 w-2 rounded-full',
                onlineCount > 0 ? 'bg-green-500' : 'bg-gray-400'
              )}
            />
            {onlineCount}/{dataPlanes.length} data plane
            {dataPlanes.length !== 1 ? 's' : ''}
          </span>
        </div>

        {latestCompilation && (
          <div className="mt-2">
            {compilationStatusBadge(latestCompilation.status)}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- Recent Activity ---

function RecentActivityRow({
  compilation,
}: {
  compilation: RecentCompilation
}) {
  const navigate = useNavigate()

  return (
    <button
      className="flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm transition-colors hover:bg-accent/50"
      onClick={() => navigate(`/projects/${compilation.projectId}/builds`)}
    >
      <span className="flex-1 truncate font-medium">
        {compilation.projectName}
      </span>
      {compilationStatusBadge(compilation.status)}
      <span className="shrink-0 text-xs text-muted-foreground">
        {relativeTime(compilation.started_at)}
      </span>
    </button>
  )
}

// --- Dashboard View ---

function DashboardView() {
  const navigate = useNavigate()
  const { stats, details, recentCompilations, refetch, isLoading } =
    useDashboard()

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Dashboard</h1>
          <p className="text-muted-foreground">
            Overview of your API gateway projects
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            disabled={isLoading}
          >
            <RefreshCw
              className={cn('h-4 w-4 mr-2', isLoading && 'animate-spin')}
            />
            Refresh
          </Button>
          <Button size="sm" onClick={() => navigate('/projects')}>
            <Plus className="h-4 w-4 mr-2" />
            New Project
          </Button>
        </div>
      </div>

      {/* Stats */}
      <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
        <StatCard
          icon={FolderKanban}
          label="Projects"
          value={stats.projects}
          onClick={() => navigate('/projects')}
        />
        <StatCard
          icon={Server}
          label="Data Planes"
          value={
            stats.dataPlanes.total > 0
              ? `${stats.dataPlanes.online} / ${stats.dataPlanes.total}`
              : '0'
          }
          subtitle={
            stats.dataPlanes.total > 0
              ? `${stats.dataPlanes.online} online`
              : undefined
          }
        />
        <StatCard
          icon={Hammer}
          label="Compilations"
          value={stats.compilations}
          onClick={() => navigate('/activity')}
        />
        <StatCard
          icon={Puzzle}
          label="Plugins"
          value={stats.plugins}
          subtitle="registered"
          onClick={() => navigate('/plugin-registry')}
        />
      </div>

      {/* Project Health */}
      <div className="mt-8">
        <h2 className="text-lg font-semibold mb-4">Project Health</h2>
        <div className="grid gap-4 md:grid-cols-2">
          {details.map((detail) => (
            <ProjectHealthCard key={detail.project.id} detail={detail} />
          ))}
        </div>
      </div>

      {/* Recent Activity */}
      {recentCompilations.length > 0 && (
        <div className="mt-8">
          <div className="flex items-center justify-between mb-4">
            <h2 className="text-lg font-semibold">Recent Activity</h2>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => navigate('/activity')}
            >
              View all
              <ArrowRight className="h-4 w-4 ml-1" />
            </Button>
          </div>
          <Card>
            <CardContent className="p-2">
              {recentCompilations.map((compilation) => (
                <RecentActivityRow
                  key={compilation.id}
                  compilation={compilation}
                />
              ))}
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  )
}

// --- Main Export ---

export function DashboardPage() {
  const { projects, isLoading, isError, refetch } = useDashboard()

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-12">
        <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (isError) {
    return (
      <div className="flex flex-col items-center justify-center px-8 py-16">
        <div className="max-w-md text-center">
          <Server className="mx-auto h-12 w-12 text-muted-foreground" />
          <h2 className="mt-4 text-lg font-semibold">
            Cannot reach the control plane
          </h2>
          <p className="mt-2 text-sm text-muted-foreground">
            Make sure <code className="rounded bg-muted px-1.5 py-0.5 text-xs">barbacane-control</code> is
            running and the database is up.
          </p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            className="mt-6"
          >
            <RefreshCw className="h-4 w-4 mr-2" />
            Retry
          </Button>
        </div>
      </div>
    )
  }

  if (projects.length === 0) {
    return <OnboardingView />
  }

  return <DashboardView />
}
