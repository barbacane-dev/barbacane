import { useRef, useState } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { FileCode, Upload, Trash2, RefreshCw, Eye, Download, AlertTriangle, X } from 'lucide-react'
import {
  listProjectSpecs,
  uploadSpecToProject,
  deleteSpec,
  downloadSpecContent,
  getSpecCompliance,
} from '@/lib/api'
import type { Spec, ComplianceWarning } from '@/lib/api'
import { Button, Card, CardContent, Badge, DropZone, CodeBlock } from '@/components/ui'
import { useConfirm } from '@/hooks'
import { cn } from '@/lib/utils'

export function ProjectSpecsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [viewingSpec, setViewingSpec] = useState<Spec | null>(null)
  const [specContent, setSpecContent] = useState<string>('')
  const [complianceWarnings, setComplianceWarnings] = useState<ComplianceWarning[]>([])
  const [checkingCompliance, setCheckingCompliance] = useState<string | null>(null)
  const [complianceChecked, setComplianceChecked] = useState(false)
  const { confirm, dialog } = useConfirm()

  const specsQuery = useQuery({
    queryKey: ['project-specs', projectId],
    queryFn: () => listProjectSpecs(projectId!),
    enabled: !!projectId,
  })

  const uploadMutation = useMutation({
    mutationFn: (file: File) => uploadSpecToProject(projectId!, file),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['project-specs', projectId] })
      queryClient.invalidateQueries({ queryKey: ['project-operations', projectId] })
      setComplianceWarnings(data.warnings ?? [])
    },
  })

  const deleteMutation = useMutation({
    mutationFn: deleteSpec,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-specs', projectId] })
      queryClient.invalidateQueries({ queryKey: ['project-operations', projectId] })
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

  const handleDownloadSpec = async (spec: Spec) => {
    try {
      const content = await downloadSpecContent(spec.id)
      const blob = new Blob([content], { type: 'application/yaml' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = spec.name.endsWith('.yaml') || spec.name.endsWith('.yml') || spec.name.endsWith('.json')
        ? spec.name
        : `${spec.name}.yaml`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
    } catch (err) {
      console.error('Failed to download spec:', err)
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
              <CodeBlock code={specContent} className="bg-muted p-4 rounded-lg" />
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
                      onClick={() => handleDownloadSpec(spec)}
                    >
                      <Download className="h-4 w-4 mr-1" />
                      Download
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => handleCheckCompliance(spec)}
                      disabled={checkingCompliance === spec.id}
                    >
                      <AlertTriangle className={cn('h-4 w-4 mr-1', checkingCompliance === spec.id && 'animate-spin')} />
                      Check
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        if (await confirm({ title: 'Delete spec', description: `Are you sure you want to delete "${spec.name}"?` })) {
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
      {dialog}
    </div>
  )
}
