import { useState, useRef } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { FileCode, Upload, Trash2, RefreshCw, Eye, AlertTriangle, X } from 'lucide-react'
import { listSpecs, uploadSpec, deleteSpec, downloadSpecContent, getSpecCompliance } from '@/lib/api'
import type { Spec, ComplianceWarning } from '@/lib/api'
import { Button, Card, CardContent, Badge, SearchInput, Breadcrumb, DropZone } from '@/components/ui'
import { useDebounce } from '@/hooks'
import type { SpecType } from '@/lib/api'
import { cn } from '@/lib/utils'

export function SpecsPage() {
  const queryClient = useQueryClient()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [selectedSpec, setSelectedSpec] = useState<Spec | null>(null)
  const [specContent, setSpecContent] = useState<string | null>(null)
  const [search, setSearch] = useState('')
  const [typeFilter, setTypeFilter] = useState<SpecType | ''>('')
  const debouncedSearch = useDebounce(search, 300)
  const [complianceWarnings, setComplianceWarnings] = useState<ComplianceWarning[]>([])
  const [checkingCompliance, setCheckingCompliance] = useState<string | null>(null)
  const [complianceChecked, setComplianceChecked] = useState(false)

  const specsQuery = useQuery({
    queryKey: ['specs', { name: debouncedSearch || undefined, type: typeFilter || undefined }],
    queryFn: () => listSpecs({ name: debouncedSearch || undefined, type: typeFilter || undefined }),
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

  const handleCheckCompliance = async (spec: Spec) => {
    setCheckingCompliance(spec.id)
    try {
      const warnings = await getSpecCompliance(spec.id)
      setComplianceWarnings(warnings)
      setComplianceChecked(true)
    } catch (err) {
      console.error('Failed to check compliance:', err)
    } finally {
      setCheckingCompliance(null)
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
      <Breadcrumb
        items={[
          { label: 'Dashboard', href: '/' },
          { label: 'API Specs' },
        ]}
        className="mb-4"
      />
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

      <div className="mb-6 flex items-center gap-3">
        <div className="max-w-sm flex-1">
          <SearchInput
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onClear={() => setSearch('')}
            placeholder="Search specs..."
          />
        </div>
        <div className="flex gap-1">
          <Button
            variant={typeFilter === '' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTypeFilter('')}
          >
            All
          </Button>
          <Button
            variant={typeFilter === 'openapi' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTypeFilter('openapi')}
          >
            OpenAPI
          </Button>
          <Button
            variant={typeFilter === 'asyncapi' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTypeFilter('asyncapi')}
          >
            AsyncAPI
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

      {complianceWarnings.length > 0 && (
        <div className="mb-4 rounded-lg border border-yellow-500/50 bg-yellow-500/10 p-4">
          <div className="flex items-start justify-between">
            <div className="flex items-start gap-2">
              <AlertTriangle className="h-4 w-4 mt-0.5 text-yellow-600 dark:text-yellow-500 shrink-0" />
              <div>
                <p className="text-sm font-medium text-yellow-800 dark:text-yellow-400">
                  {complianceWarnings.length} compliance {complianceWarnings.length === 1 ? 'warning' : 'warnings'}
                </p>
                <ul className="mt-1 space-y-1">
                  {complianceWarnings.map((w, i) => (
                    <li key={i} className="text-sm text-yellow-700 dark:text-yellow-400/80">
                      <span className="font-mono text-xs">{w.code}</span>{' '}
                      {w.message}
                      {w.location && (
                        <span className="text-yellow-600/70 dark:text-yellow-500/60"> — {w.location}</span>
                      )}
                    </li>
                  ))}
                </ul>
              </div>
            </div>
            <button
              onClick={() => { setComplianceWarnings([]); setComplianceChecked(false) }}
              className="text-yellow-600 hover:text-yellow-800 dark:text-yellow-500 dark:hover:text-yellow-300"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
        </div>
      )}

      {complianceChecked && complianceWarnings.length === 0 && (
        <div className="mb-4 rounded-lg border border-green-500/50 bg-green-500/10 p-4">
          <div className="flex items-center justify-between">
            <p className="text-sm font-medium text-green-800 dark:text-green-400">
              No compliance warnings found
            </p>
            <button
              onClick={() => setComplianceChecked(false)}
              className="text-green-600 hover:text-green-800 dark:text-green-500 dark:hover:text-green-300"
            >
              <X className="h-4 w-4" />
            </button>
          </div>
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
        <DropZone
          onFileDrop={(file) => uploadMutation.mutate(file)}
          accept=".yaml,.yml,.json"
          icon={FileCode}
          label="Drop your API spec here or click to browse"
          hint="OpenAPI or AsyncAPI specification (YAML or JSON)"
          disabled={uploadMutation.isPending}
        />
      ) : (
        <div className="space-y-4">
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
                          handleCheckCompliance(spec)
                        }}
                        disabled={checkingCompliance === spec.id}
                      >
                        <AlertTriangle className={cn('h-4 w-4', checkingCompliance === spec.id && 'animate-spin')} />
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
        <DropZone
          onFileDrop={(file) => uploadMutation.mutate(file)}
          accept=".yaml,.yml,.json"
          icon={Upload}
          label="Drop another spec here or click to browse"
          hint="OpenAPI or AsyncAPI specification (YAML or JSON)"
          disabled={uploadMutation.isPending}
          className="p-4"
        />
        </div>
      )}
    </div>
  )
}
