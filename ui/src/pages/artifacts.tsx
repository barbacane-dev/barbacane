import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Package, Trash2, RefreshCw, Download } from 'lucide-react'
import { listArtifacts, deleteArtifact, downloadArtifact } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { cn } from '@/lib/utils'

export function ArtifactsPage() {
  const queryClient = useQueryClient()

  const artifactsQuery = useQuery({
    queryKey: ['artifacts'],
    queryFn: () => listArtifacts(),
  })

  const deleteMutation = useMutation({
    mutationFn: deleteArtifact,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['artifacts'] })
    },
  })

  const handleDownload = async (id: string) => {
    try {
      const blob = await downloadArtifact(id)
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `artifact-${id.slice(0, 8)}.bca`
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

  const artifacts = artifactsQuery.data ?? []

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Artifacts</h1>
          <p className="text-muted-foreground">
            Compiled gateway artifacts ready for deployment
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => artifactsQuery.refetch()}
          disabled={artifactsQuery.isFetching}
        >
          <RefreshCw
            className={cn('h-4 w-4 mr-2', artifactsQuery.isFetching && 'animate-spin')}
          />
          Refresh
        </Button>
      </div>

      {artifactsQuery.isLoading ? (
        <div className="flex items-center justify-center p-12">
          <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : artifactsQuery.isError ? (
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">Failed to load artifacts</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => artifactsQuery.refetch()}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      ) : artifacts.length === 0 ? (
        <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
          <div className="text-center">
            <Package className="mx-auto h-12 w-12 text-muted-foreground" />
            <h3 className="mt-4 text-lg font-medium">No artifacts compiled</h3>
            <p className="mt-2 text-sm text-muted-foreground">
              Compile a spec to create a deployable artifact
            </p>
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          {artifacts.map((artifact) => (
            <Card key={artifact.id}>
              <CardContent className="p-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-4">
                    <Package className="h-10 w-10 text-primary" />
                    <div>
                      <div className="flex items-center gap-2">
                        <h3 className="font-medium font-mono text-sm">
                          {artifact.id.slice(0, 8)}
                        </h3>
                        <Badge variant="outline">v{artifact.compiler_version}</Badge>
                      </div>
                      <div className="mt-1 flex items-center gap-4 text-sm text-muted-foreground">
                        <span>{formatSize(artifact.size_bytes)}</span>
                        <span>Compiled {formatDate(artifact.compiled_at)}</span>
                      </div>
                      <p className="mt-1 text-xs text-muted-foreground font-mono">
                        SHA256: {artifact.sha256.slice(0, 16)}...
                      </p>
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
                        if (confirm('Delete this artifact?')) {
                          deleteMutation.mutate(artifact.id)
                        }
                      }}
                      disabled={deleteMutation.isPending}
                    >
                      <Trash2 className="h-4 w-4 text-destructive" />
                    </Button>
                  </div>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}
