import { useParams, Link } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Package,
  RefreshCw,
  CheckCircle,
  XCircle,
  Clock,
  Loader2,
  Download,
  Play,
  Trash2,
} from 'lucide-react'
import {
  listProjectCompilations,
  listProjectArtifacts,
  listProjectSpecs,
  downloadArtifact,
  deleteArtifact,
  startCompilation,
} from '@/lib/api'
import type { Compilation, CompilationError } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { useProjectContext } from '@/components/layout'
import { cn } from '@/lib/utils'

export function ProjectBuildsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const { project } = useProjectContext()
  const queryClient = useQueryClient()

  const compilationsQuery = useQuery({
    queryKey: ['project-compilations', projectId],
    queryFn: () => listProjectCompilations(projectId!),
    enabled: !!projectId,
    refetchInterval: 5000, // Poll for compilation status updates
  })

  const artifactsQuery = useQuery({
    queryKey: ['project-artifacts', projectId],
    queryFn: () => listProjectArtifacts(projectId!),
    enabled: !!projectId,
  })

  const specsQuery = useQuery({
    queryKey: ['project-specs', projectId],
    queryFn: () => listProjectSpecs(projectId!),
    enabled: !!projectId,
  })

  const specs = specsQuery.data ?? []
  const compilations = compilationsQuery.data ?? []
  const artifacts = artifactsQuery.data ?? []

  const hasActiveCompilation = compilations.some(
    (c) => c.status === 'pending' || c.status === 'compiling'
  )

  const deleteArtifactMutation = useMutation({
    mutationFn: deleteArtifact,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-artifacts', projectId] })
    },
  })

  const compileMutation = useMutation({
    mutationFn: async () => {
      if (specs.length === 0) {
        throw new Error('No specs to compile')
      }
      const [primary, ...rest] = specs
      return startCompilation(primary.id, {
        production: project.production_mode,
        additional_specs: rest.length > 0 ? rest.map((s) => s.id) : undefined,
      })
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: ['project-compilations', projectId],
      })
      queryClient.invalidateQueries({
        queryKey: ['project-artifacts', projectId],
      })
    },
  })

  const handleDownload = async (artifactId: string) => {
    try {
      const blob = await downloadArtifact(artifactId)
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `artifact-${artifactId.slice(0, 8)}.bca`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
    } catch (err) {
      console.error('Failed to download artifact:', err)
    }
  }

  const formatDate = (dateStr: string) => {
    return new Date(dateStr).toLocaleDateString('en-US', {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  }

  const getStatusIcon = (status: Compilation['status']) => {
    switch (status) {
      case 'succeeded':
        return <CheckCircle className="h-5 w-5 text-green-500" />
      case 'failed':
        return <XCircle className="h-5 w-5 text-destructive" />
      case 'compiling':
        return <Loader2 className="h-5 w-5 text-primary animate-spin" />
      case 'pending':
        return <Clock className="h-5 w-5 text-muted-foreground" />
    }
  }

  const getStatusBadge = (status: Compilation['status']) => {
    switch (status) {
      case 'succeeded':
        return <Badge className="bg-green-500/10 text-green-500">Succeeded</Badge>
      case 'failed':
        return <Badge variant="destructive">Failed</Badge>
      case 'compiling':
        return <Badge variant="default">Compiling</Badge>
      case 'pending':
        return <Badge variant="secondary">Pending</Badge>
    }
  }

  const renderCompilationErrors = (errors: CompilationError[]) => {
    return errors.map((err, i) => {
      // Strip code prefix from message if backend didn't (defensive)
      const message = err.message.startsWith(`${err.code}:`)
        ? err.message.slice(err.code.length + 1).trim()
        : err.message

      // E1040: missing plugins — parse plugin names and show structured output
      if (err.code === 'E1040') {
        const pluginNames = [...message.matchAll(/'([^']+)'/g)].map((m) => m[1])

        return (
          <div
            key={i}
            className="mt-2 rounded-md border border-destructive/30 bg-destructive/5 p-3"
          >
            <p className="text-xs font-medium text-destructive">
              <span className="font-mono">[{err.code}]</span> Missing plugin
              {pluginNames.length !== 1 ? 's' : ''}
            </p>
            {pluginNames.length > 0 && (
              <div className="mt-1.5 flex flex-wrap gap-1">
                {pluginNames.map((name) => (
                  <Badge
                    key={name}
                    variant="outline"
                    className="border-destructive/30 text-destructive text-xs"
                  >
                    {name}
                  </Badge>
                ))}
              </div>
            )}
            <p className="mt-1.5 text-xs text-muted-foreground">
              Enable them on the{' '}
              <Link
                to={`/projects/${projectId}/plugins`}
                className="underline text-primary hover:text-primary/80"
              >
                Plugins page
              </Link>{' '}
              before compiling.
            </p>
          </div>
        )
      }

      // Default: generic error display
      return (
        <div
          key={i}
          className="mt-2 rounded-md border border-destructive/30 bg-destructive/5 p-3"
        >
          <p className="text-xs text-destructive">
            <span className="font-mono font-medium">[{err.code}]</span>{' '}
            {message}
          </p>
        </div>
      )
    })
  }

  const isLoading = compilationsQuery.isLoading || artifactsQuery.isLoading

  return (
    <div className="p-8">
      <div className="mb-6 flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Builds</h2>
          <p className="text-sm text-muted-foreground">
            Compilation history and downloadable artifacts
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              compilationsQuery.refetch()
              artifactsQuery.refetch()
            }}
            disabled={isLoading}
          >
            <RefreshCw className={cn('h-4 w-4 mr-2', isLoading && 'animate-spin')} />
            Refresh
          </Button>
          <Button
            size="sm"
            onClick={() => compileMutation.mutate()}
            disabled={
              specs.length === 0 ||
              hasActiveCompilation ||
              compileMutation.isPending
            }
          >
            <Play className="h-4 w-4 mr-2" />
            {compileMutation.isPending
              ? 'Starting...'
              : hasActiveCompilation
                ? 'Build in progress'
                : 'Compile Project'}
          </Button>
        </div>
      </div>

      {compileMutation.isError && (
        <div className="mb-4 rounded-lg border border-destructive bg-destructive/10 p-4">
          <p className="text-sm text-destructive">
            {compileMutation.error instanceof Error
              ? compileMutation.error.message
              : 'Failed to start compilation'}
          </p>
        </div>
      )}

      {/* Artifacts Section */}
      <div className="mb-8">
        <h3 className="text-md font-medium mb-4">Artifacts</h3>
        {artifactsQuery.isLoading ? (
          <div className="flex items-center justify-center p-8">
            <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : artifacts.length === 0 ? (
          <div className="rounded-lg border border-dashed border-border p-8 text-center">
            <Package className="mx-auto h-10 w-10 text-muted-foreground" />
            <p className="mt-2 text-sm text-muted-foreground">
              No artifacts yet. Compile your project to create an artifact.
            </p>
          </div>
        ) : (
          <div className="grid gap-4 md:grid-cols-2">
            {artifacts.map((artifact) => (
              <Card key={artifact.id}>
                <CardContent className="p-4">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      <Package className="h-8 w-8 text-primary" />
                      <div>
                        <p className="font-mono text-sm">
                          {artifact.id.slice(0, 8)}
                        </p>
                        <div className="flex items-center gap-2 mt-1 text-xs text-muted-foreground">
                          <span>{formatSize(artifact.size_bytes)}</span>
                          <span>v{artifact.compiler_version}</span>
                        </div>
                      </div>
                    </div>
                    <div className="flex gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => handleDownload(artifact.id)}
                      >
                        <Download className="h-4 w-4 mr-1" />
                        Download
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => {
                          if (confirm(`Delete artifact ${artifact.id.slice(0, 8)}?`)) {
                            deleteArtifactMutation.mutate(artifact.id)
                          }
                        }}
                        disabled={deleteArtifactMutation.isPending}
                      >
                        <Trash2 className="h-4 w-4 text-destructive" />
                      </Button>
                    </div>
                  </div>
                  <p className="mt-2 text-xs text-muted-foreground">
                    Built {formatDate(artifact.compiled_at)}
                  </p>
                </CardContent>
              </Card>
            ))}
          </div>
        )}
      </div>

      {/* Compilations Section */}
      <div>
        <h3 className="text-md font-medium mb-4">Build History</h3>
        {compilationsQuery.isLoading ? (
          <div className="flex items-center justify-center p-8">
            <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        ) : compilations.length === 0 ? (
          <div className="rounded-lg border border-dashed border-border p-8 text-center">
            <Clock className="mx-auto h-10 w-10 text-muted-foreground" />
            <p className="mt-2 text-sm text-muted-foreground">
              No compilation history yet.
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {compilations.map((compilation) => (
              <Card key={compilation.id}>
                <CardContent className="p-4">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      {getStatusIcon(compilation.status)}
                      <div>
                        <div className="flex items-center gap-2">
                          <span className="font-mono text-sm">
                            {compilation.id.slice(0, 8)}
                          </span>
                          {getStatusBadge(compilation.status)}
                          {compilation.production && (
                            <Badge variant="outline">Production</Badge>
                          )}
                        </div>
                        <p className="mt-1 text-xs text-muted-foreground">
                          Started {formatDate(compilation.started_at)}
                          {compilation.completed_at &&
                            ` | Completed ${formatDate(compilation.completed_at)}`}
                        </p>
                        {compilation.errors && compilation.errors.length > 0 &&
                          renderCompilationErrors(compilation.errors)}
                      </div>
                    </div>
                    {compilation.artifact_id && (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => handleDownload(compilation.artifact_id!)}
                      >
                        <Download className="h-4 w-4 mr-1" />
                        Artifact
                      </Button>
                    )}
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
