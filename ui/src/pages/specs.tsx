import { useState, useRef } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { FileCode, Upload, Trash2, RefreshCw, Eye } from 'lucide-react'
import { listSpecs, uploadSpec, deleteSpec, downloadSpecContent } from '@/lib/api'
import type { Spec } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { cn } from '@/lib/utils'

export function SpecsPage() {
  const queryClient = useQueryClient()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [selectedSpec, setSelectedSpec] = useState<Spec | null>(null)
  const [specContent, setSpecContent] = useState<string | null>(null)

  const specsQuery = useQuery({
    queryKey: ['specs'],
    queryFn: () => listSpecs(),
  })

  const uploadMutation = useMutation({
    mutationFn: uploadSpec,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['specs'] })
      if (fileInputRef.current) {
        fileInputRef.current.value = ''
      }
    },
  })

  const deleteMutation = useMutation({
    mutationFn: deleteSpec,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['specs'] })
      setSelectedSpec(null)
      setSpecContent(null)
    },
  })

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) {
      uploadMutation.mutate(file)
    }
  }

  const handleViewSpec = async (spec: Spec) => {
    setSelectedSpec(spec)
    try {
      const content = await downloadSpecContent(spec.id)
      setSpecContent(content)
    } catch (err) {
      console.error('Failed to load spec content:', err)
      setSpecContent('Failed to load spec content')
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
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">API Specs</h1>
          <p className="text-muted-foreground">
            Manage your OpenAPI and AsyncAPI specifications
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
          <input
            ref={fileInputRef}
            type="file"
            accept=".yaml,.yml,.json"
            onChange={handleFileChange}
            className="hidden"
          />
          <Button
            onClick={() => fileInputRef.current?.click()}
            disabled={uploadMutation.isPending}
          >
            <Upload className="h-4 w-4 mr-2" />
            {uploadMutation.isPending ? 'Uploading...' : 'Upload Spec'}
          </Button>
        </div>
      </div>

      {uploadMutation.isError && (
        <div className="mb-4 rounded-lg border border-destructive bg-destructive/10 p-4 text-sm text-destructive">
          {uploadMutation.error instanceof Error
            ? uploadMutation.error.message
            : 'Failed to upload spec'}
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
              Upload an OpenAPI or AsyncAPI spec to get started
            </p>
            <Button
              onClick={() => fileInputRef.current?.click()}
              className="mt-4"
            >
              <Upload className="h-4 w-4 mr-2" />
              Upload Spec
            </Button>
          </div>
        </div>
      ) : (
        <div className="grid gap-4 lg:grid-cols-2">
          {/* Specs list */}
          <div className="space-y-3">
            {specs.map((spec) => (
              <Card
                key={spec.id}
                className={cn(
                  'cursor-pointer transition-all hover:border-primary/50',
                  selectedSpec?.id === spec.id && 'border-primary'
                )}
                onClick={() => handleViewSpec(spec)}
              >
                <CardContent className="p-4">
                  <div className="flex items-start justify-between">
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <FileCode className="h-5 w-5 text-primary" />
                        <h3 className="font-medium truncate">{spec.name}</h3>
                      </div>
                      <div className="mt-2 flex flex-wrap gap-2">
                        <Badge variant="secondary">{spec.spec_type}</Badge>
                        <Badge variant="outline">v{spec.spec_version}</Badge>
                      </div>
                      <p className="mt-2 text-xs text-muted-foreground">
                        Updated {formatDate(spec.updated_at)}
                      </p>
                    </div>
                    <div className="flex gap-1">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={(e) => {
                          e.stopPropagation()
                          handleViewSpec(spec)
                        }}
                      >
                        <Eye className="h-4 w-4" />
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={(e) => {
                          e.stopPropagation()
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

          {/* Spec preview */}
          {selectedSpec && (
            <Card>
              <CardContent className="p-4">
                <div className="mb-4 flex items-center justify-between">
                  <h3 className="font-medium">{selectedSpec.name}</h3>
                  <Badge variant="secondary">{selectedSpec.spec_type}</Badge>
                </div>
                <div className="rounded-lg border border-border bg-muted/30 p-4 max-h-[600px] overflow-auto">
                  <pre className="text-sm">
                    <code>{specContent ?? 'Loading...'}</code>
                  </pre>
                </div>
              </CardContent>
            </Card>
          )}
        </div>
      )}
    </div>
  )
}
