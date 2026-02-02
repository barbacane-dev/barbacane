import { useRef, useState } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { FileCode, Upload, Trash2, RefreshCw, Eye, Play } from 'lucide-react'
import {
  listProjectSpecs,
  uploadSpecToProject,
  deleteSpec,
  downloadSpecContent,
  startCompilation,
} from '@/lib/api'
import type { Spec } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { cn } from '@/lib/utils'

export function ProjectSpecsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [viewingSpec, setViewingSpec] = useState<Spec | null>(null)
  const [specContent, setSpecContent] = useState<string>('')

  const specsQuery = useQuery({
    queryKey: ['project-specs', projectId],
    queryFn: () => listProjectSpecs(projectId!),
    enabled: !!projectId,
  })

  const uploadMutation = useMutation({
    mutationFn: (file: File) => uploadSpecToProject(projectId!, file),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-specs', projectId] })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: deleteSpec,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-specs', projectId] })
    },
  })

  const compileMutation = useMutation({
    mutationFn: (specId: string) => startCompilation(specId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-compilations', projectId] })
    },
  })

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) {
      uploadMutation.mutate(file)
    }
    e.target.value = ''
  }

  const handleViewSpec = async (spec: Spec) => {
    try {
      const content = await downloadSpecContent(spec.id)
      setSpecContent(content)
      setViewingSpec(spec)
    } catch (err) {
      console.error('Failed to load spec content:', err)
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

  const specs = specsQuery.data ?? []

  return (
    <div className="p-8">
      <div className="mb-6 flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">API Specifications</h2>
          <p className="text-sm text-muted-foreground">
            Upload and manage OpenAPI and AsyncAPI specs for this project
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => specsQuery.refetch()}
            disabled={specsQuery.isFetching}
          >
            <RefreshCw
              className={cn('h-4 w-4 mr-2', specsQuery.isFetching && 'animate-spin')}
            />
            Refresh
          </Button>
          <Button size="sm" onClick={() => fileInputRef.current?.click()}>
            <Upload className="h-4 w-4 mr-2" />
            Upload Spec
          </Button>
          <input
            ref={fileInputRef}
            type="file"
            accept=".yaml,.yml,.json"
            onChange={handleFileChange}
            className="hidden"
          />
        </div>
      </div>

      {uploadMutation.isError && (
        <div className="mb-4 rounded-lg border border-destructive bg-destructive/10 p-4">
          <p className="text-sm text-destructive">
            {uploadMutation.error instanceof Error
              ? uploadMutation.error.message
              : 'Failed to upload spec'}
          </p>
        </div>
      )}

      {/* Spec Viewer Modal */}
      {viewingSpec && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="w-full max-w-4xl max-h-[80vh] flex flex-col">
            <CardContent className="p-4 border-b border-border">
              <div className="flex items-center justify-between">
                <div>
                  <h3 className="font-medium">{viewingSpec.name}</h3>
                  <p className="text-sm text-muted-foreground">
                    {viewingSpec.spec_type} {viewingSpec.spec_version}
                  </p>
                </div>
                <Button variant="outline" onClick={() => setViewingSpec(null)}>
                  Close
                </Button>
              </div>
            </CardContent>
            <div className="flex-1 overflow-auto p-4">
              <pre className="text-xs font-mono whitespace-pre-wrap bg-muted p-4 rounded-lg">
                {specContent}
              </pre>
            </div>
          </Card>
        </div>
      )}

      {specsQuery.isLoading ? (
        <div className="flex items-center justify-center p-12">
          <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : specsQuery.isError ? (
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">Failed to load specs</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => specsQuery.refetch()}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      ) : specs.length === 0 ? (
        <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
          <div className="text-center">
            <FileCode className="mx-auto h-12 w-12 text-muted-foreground" />
            <h3 className="mt-4 text-lg font-medium">No specs yet</h3>
            <p className="mt-2 text-sm text-muted-foreground">
              Upload an OpenAPI or AsyncAPI specification to get started
            </p>
            <Button
              className="mt-4"
              onClick={() => fileInputRef.current?.click()}
            >
              <Upload className="h-4 w-4 mr-2" />
              Upload Spec
            </Button>
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          {specs.map((spec) => (
            <Card key={spec.id}>
              <CardContent className="p-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-4">
                    <FileCode className="h-10 w-10 text-primary" />
                    <div>
                      <div className="flex items-center gap-2">
                        <h3 className="font-medium">{spec.name}</h3>
                        <Badge variant="outline">
                          {spec.spec_type === 'openapi' ? 'OpenAPI' : 'AsyncAPI'}
                        </Badge>
                        <Badge variant="secondary">v{spec.spec_version}</Badge>
                      </div>
                      <p className="mt-1 text-sm text-muted-foreground">
                        Updated {formatDate(spec.updated_at)}
                      </p>
                    </div>
                  </div>
                  <div className="flex gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleViewSpec(spec)}
                    >
                      <Eye className="h-4 w-4 mr-1" />
                      View
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => compileMutation.mutate(spec.id)}
                      disabled={compileMutation.isPending}
                    >
                      <Play className="h-4 w-4 mr-1" />
                      Compile
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        if (confirm(`Delete spec "${spec.name}"?`)) {
                          deleteMutation.mutate(spec.id)
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
