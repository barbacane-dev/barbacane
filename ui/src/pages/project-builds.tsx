import { useParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import {
  Package,
  RefreshCw,
  CheckCircle,
  XCircle,
  Clock,
  Loader2,
  Download,
} from 'lucide-react'
import {
  listProjectCompilations,
  listProjectArtifacts,
  downloadArtifact,
} from '@/lib/api'
import type { Compilation } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { cn } from '@/lib/utils'

export function ProjectBuildsPage() {
  const { id: projectId } = useParams<{ id: string }>()

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

  const isLoading = compilationsQuery.isLoading || artifactsQuery.isLoading
  const compilations = compilationsQuery.data ?? []
  const artifacts = artifactsQuery.data ?? []

  return (
    <div className="p-8">
      <div className="mb-6 flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Builds</h2>
          <p className="text-sm text-muted-foreground">
            Compilation history and downloadable artifacts
          </p>
        </div>
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
      </div>

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
              No artifacts yet. Compile a spec to create an artifact.
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
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleDownload(artifact.id)}
                    >
                      <Download className="h-4 w-4 mr-1" />
                      Download
                    </Button>
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
                        {compilation.errors && compilation.errors.length > 0 && (
                          <div className="mt-2">
                            {compilation.errors.map((err, i) => (
                              <p
                                key={i}
                                className="text-xs text-destructive font-mono"
                              >
                                [{err.code}] {err.message}
                              </p>
                            ))}
                          </div>
                        )}
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
