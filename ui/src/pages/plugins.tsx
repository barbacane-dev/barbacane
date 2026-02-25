import { useState, useRef } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Puzzle, Upload, Trash2, RefreshCw, Github, ExternalLink, AlertCircle, X, FileJson } from 'lucide-react'
import { listPlugins, registerPlugin, deletePlugin } from '@/lib/api'
import type { Plugin, PluginType } from '@/lib/api'
import { Button, Card, CardContent, Badge, EmptyState, SearchInput, Breadcrumb } from '@/components/ui'
import { useDebounce, useConfirm } from '@/hooks'
import { cn } from '@/lib/utils'

interface GitHubRelease {
  id: number
  tag_name: string
  name: string
  published_at: string
  assets: GitHubAsset[]
}

interface GitHubAsset {
  id: number
  name: string
  browser_download_url: string
  size: number
}

type UploadMode = 'file' | 'github'

export function PluginsPage() {
  const queryClient = useQueryClient()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [showUploadForm, setShowUploadForm] = useState(false)
  const [uploadMode, setUploadMode] = useState<UploadMode>('file')
  const [uploadData, setUploadData] = useState({
    name: '',
    version: '1.0.0',
    type: 'middleware' as PluginType,
    description: '',
    file: null as File | null,
  })

  // Schema viewer state
  const [viewingSchema, setViewingSchema] = useState<Plugin | null>(null)

  // Delete error state
  const [deleteError, setDeleteError] = useState<{ pluginKey: string; message: string } | null>(null)
  const [search, setSearch] = useState('')
  const [typeFilter, setTypeFilter] = useState<PluginType | ''>('')
  const debouncedSearch = useDebounce(search, 300)
  const { confirm, dialog } = useConfirm()

  // GitHub release state
  const [githubRepo, setGithubRepo] = useState('')
  const [releases, setReleases] = useState<GitHubRelease[]>([])
  const [selectedRelease, setSelectedRelease] = useState<GitHubRelease | null>(null)
  const [selectedAsset, setSelectedAsset] = useState<GitHubAsset | null>(null)
  const [fetchingReleases, setFetchingReleases] = useState(false)
  const [fetchError, setFetchError] = useState<string | null>(null)

  const pluginsQuery = useQuery({
    queryKey: ['plugins', { name: debouncedSearch || undefined, type: typeFilter || undefined }],
    queryFn: () => listPlugins({ name: debouncedSearch || undefined, type: typeFilter || undefined }),
  })

  const registerMutation = useMutation({
    mutationFn: (data: typeof uploadData & { file: File }) =>
      registerPlugin({
        name: data.name,
        version: data.version,
        type: data.type,
        description: data.description || undefined,
        file: data.file,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['plugins'] })
      resetForm()
    },
  })

  const deleteMutation = useMutation({
    mutationFn: ({ name, version }: { name: string; version: string }) =>
      deletePlugin(name, version),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['plugins'] })
      setDeleteError(null)
    },
    onError: (error, variables) => {
      const pluginKey = `${variables.name}-${variables.version}`
      const errorMessage = error instanceof Error ? error.message : String(error)

      // Check if this is an "in use" error (plugin referenced by projects)
      let message: string
      if (errorMessage.includes('in use') || errorMessage.includes('cannot be deleted')) {
        message = 'Cannot delete plugin: it is currently in use by one or more projects. Remove it from all projects first.'
      } else {
        message = errorMessage || 'Failed to delete plugin'
      }

      setDeleteError({ pluginKey, message })
    },
  })

  const resetForm = () => {
    setShowUploadForm(false)
    setUploadMode('file')
    setUploadData({
      name: '',
      version: '1.0.0',
      type: 'middleware',
      description: '',
      file: null,
    })
    setGithubRepo('')
    setReleases([])
    setSelectedRelease(null)
    setSelectedAsset(null)
    setFetchError(null)
  }

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) {
      setUploadData((prev) => ({ ...prev, file }))
    }
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!uploadData.file || !uploadData.name) return
    registerMutation.mutate(uploadData as typeof uploadData & { file: File })
  }

  const fetchGitHubReleases = async () => {
    if (!githubRepo.trim()) return

    setFetchingReleases(true)
    setFetchError(null)
    setReleases([])
    setSelectedRelease(null)
    setSelectedAsset(null)

    try {
      // Parse owner/repo format
      const parts = githubRepo.replace('https://github.com/', '').split('/')
      if (parts.length < 2) {
        throw new Error('Invalid repo format. Use owner/repo or full GitHub URL')
      }
      const [owner, repo] = parts

      const response = await fetch(
        `https://api.github.com/repos/${owner}/${repo}/releases`
      )

      if (!response.ok) {
        if (response.status === 404) {
          throw new Error('Repository not found')
        }
        throw new Error(`GitHub API error: ${response.status}`)
      }

      const data = (await response.json()) as GitHubRelease[]

      // Filter releases that have .wasm assets
      const releasesWithWasm = data.filter((release) =>
        release.assets.some((asset) => asset.name.endsWith('.wasm'))
      )

      if (releasesWithWasm.length === 0) {
        throw new Error('No releases with .wasm assets found')
      }

      setReleases(releasesWithWasm)
    } catch (err) {
      setFetchError(err instanceof Error ? err.message : 'Failed to fetch releases')
    } finally {
      setFetchingReleases(false)
    }
  }

  const handleGitHubInstall = async () => {
    if (!selectedAsset || !uploadData.name) return

    try {
      // Download the WASM file
      const response = await fetch(selectedAsset.browser_download_url)
      if (!response.ok) {
        throw new Error('Failed to download plugin')
      }

      const blob = await response.blob()
      const file = new File([blob], selectedAsset.name, {
        type: 'application/wasm',
      })

      registerMutation.mutate({
        ...uploadData,
        file,
      } as typeof uploadData & { file: File })
    } catch (err) {
      console.error('Failed to install from GitHub:', err)
    }
  }

  const handleReleaseSelect = (release: GitHubRelease) => {
    setSelectedRelease(release)
    setSelectedAsset(null)

    // Auto-fill version from tag
    const version = release.tag_name.replace(/^v/, '')
    setUploadData((prev) => ({ ...prev, version }))

    // Auto-select first WASM asset
    const wasmAssets = release.assets.filter((a) => a.name.endsWith('.wasm'))
    if (wasmAssets.length === 1) {
      setSelectedAsset(wasmAssets[0])
      // Try to extract plugin name from filename
      const name = wasmAssets[0].name.replace('.wasm', '').replace(/_/g, '-')
      setUploadData((prev) => ({ ...prev, name }))
    }
  }

  const formatDate = (dateStr: string) => {
    return new Date(dateStr).toLocaleDateString('en-US', {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
    })
  }

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  }

  const plugins = pluginsQuery.data ?? []
  const wasmAssets = selectedRelease?.assets.filter((a) => a.name.endsWith('.wasm')) ?? []

  return (
    <div className="p-8">
      <Breadcrumb
        items={[
          { label: 'Dashboard', href: '/' },
          { label: 'Plugin Registry' },
        ]}
        className="mb-4"
      />
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Plugin Registry</h1>
          <p className="text-muted-foreground">
            Manage middleware and dispatcher plugins
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => pluginsQuery.refetch()}
            disabled={pluginsQuery.isFetching}
          >
            <RefreshCw
              className={cn('h-4 w-4 mr-2', pluginsQuery.isFetching && 'animate-spin')}
            />
            Refresh
          </Button>
          <Button onClick={() => setShowUploadForm(true)}>
            <Upload className="h-4 w-4 mr-2" />
            Register Plugin
          </Button>
        </div>
      </div>

      {/* Upload form */}
      {showUploadForm && (
        <Card className="mb-6">
          <CardContent className="p-6">
            <h3 className="font-medium mb-4">Register New Plugin</h3>

            {/* Mode tabs */}
            <div className="flex gap-2 mb-6">
              <Button
                variant={uploadMode === 'file' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setUploadMode('file')}
              >
                <Upload className="h-4 w-4 mr-2" />
                Upload File
              </Button>
              <Button
                variant={uploadMode === 'github' ? 'default' : 'outline'}
                size="sm"
                onClick={() => setUploadMode('github')}
              >
                <Github className="h-4 w-4 mr-2" />
                From GitHub
              </Button>
            </div>

            {uploadMode === 'file' ? (
              /* File upload form */
              <form onSubmit={handleSubmit} className="space-y-4">
                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="block text-sm font-medium mb-1">Name *</label>
                    <input
                      type="text"
                      value={uploadData.name}
                      onChange={(e) =>
                        setUploadData((prev) => ({ ...prev, name: e.target.value }))
                      }
                      placeholder="my-plugin"
                      className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                      required
                    />
                  </div>
                  <div>
                    <label className="block text-sm font-medium mb-1">Version *</label>
                    <input
                      type="text"
                      value={uploadData.version}
                      onChange={(e) =>
                        setUploadData((prev) => ({ ...prev, version: e.target.value }))
                      }
                      placeholder="1.0.0"
                      className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                      required
                    />
                  </div>
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">Type *</label>
                  <select
                    value={uploadData.type}
                    onChange={(e) =>
                      setUploadData((prev) => ({
                        ...prev,
                        type: e.target.value as PluginType,
                      }))
                    }
                    className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                  >
                    <option value="middleware">Middleware</option>
                    <option value="dispatcher">Dispatcher</option>
                  </select>
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">Description</label>
                  <input
                    type="text"
                    value={uploadData.description}
                    onChange={(e) =>
                      setUploadData((prev) => ({ ...prev, description: e.target.value }))
                    }
                    placeholder="Optional description"
                    className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium mb-1">WASM File *</label>
                  <input
                    ref={fileInputRef}
                    type="file"
                    accept=".wasm"
                    onChange={handleFileChange}
                    className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                    required
                  />
                </div>
                <div className="flex justify-end gap-2">
                  <Button type="button" variant="outline" onClick={resetForm}>
                    Cancel
                  </Button>
                  <Button type="submit" disabled={registerMutation.isPending}>
                    {registerMutation.isPending ? 'Registering...' : 'Register'}
                  </Button>
                </div>
              </form>
            ) : (
              /* GitHub release form */
              <div className="space-y-4">
                {/* Repo input */}
                <div>
                  <label className="block text-sm font-medium mb-1">
                    GitHub Repository
                  </label>
                  <div className="flex gap-2">
                    <input
                      type="text"
                      value={githubRepo}
                      onChange={(e) => setGithubRepo(e.target.value)}
                      placeholder="owner/repo or https://github.com/owner/repo"
                      className="flex-1 rounded-lg border border-input bg-background px-3 py-2 text-sm"
                    />
                    <Button
                      onClick={fetchGitHubReleases}
                      disabled={fetchingReleases || !githubRepo.trim()}
                    >
                      {fetchingReleases ? (
                        <RefreshCw className="h-4 w-4 animate-spin" />
                      ) : (
                        'Fetch Releases'
                      )}
                    </Button>
                  </div>
                  {fetchError && (
                    <p className="mt-1 text-sm text-destructive">{fetchError}</p>
                  )}
                </div>

                {/* Release selector */}
                {releases.length > 0 && (
                  <div>
                    <label className="block text-sm font-medium mb-1">Release</label>
                    <select
                      value={selectedRelease?.id ?? ''}
                      onChange={(e) => {
                        const release = releases.find(
                          (r) => r.id === Number(e.target.value)
                        )
                        if (release) handleReleaseSelect(release)
                      }}
                      className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                    >
                      <option value="">Select a release...</option>
                      {releases.map((release) => (
                        <option key={release.id} value={release.id}>
                          {release.tag_name} - {release.name || 'Untitled'} (
                          {formatDate(release.published_at)})
                        </option>
                      ))}
                    </select>
                  </div>
                )}

                {/* Asset selector */}
                {selectedRelease && wasmAssets.length > 1 && (
                  <div>
                    <label className="block text-sm font-medium mb-1">WASM Asset</label>
                    <select
                      value={selectedAsset?.id ?? ''}
                      onChange={(e) => {
                        const asset = wasmAssets.find(
                          (a) => a.id === Number(e.target.value)
                        )
                        if (asset) {
                          setSelectedAsset(asset)
                          const name = asset.name.replace('.wasm', '').replace(/_/g, '-')
                          setUploadData((prev) => ({ ...prev, name }))
                        }
                      }}
                      className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                    >
                      <option value="">Select an asset...</option>
                      {wasmAssets.map((asset) => (
                        <option key={asset.id} value={asset.id}>
                          {asset.name} ({formatSize(asset.size)})
                        </option>
                      ))}
                    </select>
                  </div>
                )}

                {/* Selected asset info */}
                {selectedAsset && (
                  <div className="rounded-lg border border-border bg-muted/30 p-3">
                    <div className="flex items-center justify-between">
                      <div>
                        <p className="font-medium">{selectedAsset.name}</p>
                        <p className="text-sm text-muted-foreground">
                          {formatSize(selectedAsset.size)}
                        </p>
                      </div>
                      <a
                        href={selectedAsset.browser_download_url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-primary hover:underline"
                      >
                        <ExternalLink className="h-4 w-4" />
                      </a>
                    </div>
                  </div>
                )}

                {/* Plugin details */}
                {selectedAsset && (
                  <>
                    <div className="grid grid-cols-2 gap-4">
                      <div>
                        <label className="block text-sm font-medium mb-1">Name *</label>
                        <input
                          type="text"
                          value={uploadData.name}
                          onChange={(e) =>
                            setUploadData((prev) => ({ ...prev, name: e.target.value }))
                          }
                          placeholder="my-plugin"
                          className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                        />
                      </div>
                      <div>
                        <label className="block text-sm font-medium mb-1">Version</label>
                        <input
                          type="text"
                          value={uploadData.version}
                          onChange={(e) =>
                            setUploadData((prev) => ({
                              ...prev,
                              version: e.target.value,
                            }))
                          }
                          placeholder="1.0.0"
                          className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                        />
                      </div>
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1">Type *</label>
                      <select
                        value={uploadData.type}
                        onChange={(e) =>
                          setUploadData((prev) => ({
                            ...prev,
                            type: e.target.value as PluginType,
                          }))
                        }
                        className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                      >
                        <option value="middleware">Middleware</option>
                        <option value="dispatcher">Dispatcher</option>
                      </select>
                    </div>
                    <div>
                      <label className="block text-sm font-medium mb-1">
                        Description
                      </label>
                      <input
                        type="text"
                        value={uploadData.description}
                        onChange={(e) =>
                          setUploadData((prev) => ({
                            ...prev,
                            description: e.target.value,
                          }))
                        }
                        placeholder="Optional description"
                        className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm"
                      />
                    </div>
                  </>
                )}

                <div className="flex justify-end gap-2">
                  <Button variant="outline" onClick={resetForm}>
                    Cancel
                  </Button>
                  <Button
                    onClick={handleGitHubInstall}
                    disabled={
                      !selectedAsset || !uploadData.name || registerMutation.isPending
                    }
                  >
                    {registerMutation.isPending ? 'Installing...' : 'Install Plugin'}
                  </Button>
                </div>
              </div>
            )}

            {registerMutation.isError && (
              <p className="mt-4 text-sm text-destructive">
                {registerMutation.error instanceof Error
                  ? registerMutation.error.message
                  : 'Failed to register plugin'}
              </p>
            )}
          </CardContent>
        </Card>
      )}

      {/* Delete error banner */}
      {deleteError && (
        <div className="mb-6 rounded-lg border border-destructive/50 bg-destructive/10 p-4">
          <div className="flex items-start justify-between gap-3">
            <div className="flex items-start gap-3">
              <AlertCircle className="h-5 w-5 text-destructive flex-shrink-0 mt-0.5" />
              <div>
                <p className="font-medium text-destructive">Delete failed</p>
                <p className="text-sm text-destructive/80 mt-1">{deleteError.message}</p>
              </div>
            </div>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setDeleteError(null)}
              className="h-8 w-8 p-0 text-destructive hover:text-destructive"
            >
              <X className="h-4 w-4" />
            </Button>
          </div>
        </div>
      )}

      <div className="mb-6 flex items-center gap-3">
        <div className="max-w-sm flex-1">
          <SearchInput
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onClear={() => setSearch('')}
            placeholder="Search plugins..."
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
            variant={typeFilter === 'middleware' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTypeFilter('middleware')}
          >
            Middleware
          </Button>
          <Button
            variant={typeFilter === 'dispatcher' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setTypeFilter('dispatcher')}
          >
            Dispatcher
          </Button>
        </div>
      </div>

      {pluginsQuery.isLoading ? (
        <div className="flex items-center justify-center p-12">
          <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : pluginsQuery.isError ? (
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">Failed to load plugins</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => pluginsQuery.refetch()}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      ) : plugins.length === 0 ? (
        <EmptyState
          icon={Puzzle}
          title="No plugins registered"
          description="Register a WASM plugin to extend gateway functionality"
          action={
            <Button onClick={() => setShowUploadForm(true)}>
              <Upload className="h-4 w-4 mr-2" />
              Register Plugin
            </Button>
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {plugins.map((plugin) => (
            <Card key={`${plugin.name}-${plugin.version}`}>
              <CardContent className="p-4">
                <div className="flex items-start justify-between">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <Puzzle className="h-5 w-5 text-secondary" />
                      <h3 className="font-medium truncate">{plugin.name}</h3>
                    </div>
                    <div className="mt-2 flex flex-wrap gap-2">
                      <Badge
                        variant={
                          plugin.plugin_type === 'middleware' ? 'default' : 'secondary'
                        }
                      >
                        {plugin.plugin_type}
                      </Badge>
                      <Badge variant="outline">v{plugin.version}</Badge>
                    </div>
                    {plugin.description && (
                      <p className="mt-2 text-sm text-muted-foreground truncate">
                        {plugin.description}
                      </p>
                    )}
                    <p className="mt-2 text-xs text-muted-foreground">
                      Registered {formatDate(plugin.registered_at)}
                    </p>
                  </div>
                  <div className="flex gap-1">
                    {plugin.config_schema && Object.keys(plugin.config_schema).length > 0 && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setViewingSchema(plugin)}
                        title="View config schema"
                      >
                        <FileJson className="h-4 w-4 text-muted-foreground" />
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        if (await confirm({ title: 'Delete plugin', description: `Are you sure you want to delete "${plugin.name}" v${plugin.version}?` })) {
                          deleteMutation.mutate({
                            name: plugin.name,
                            version: plugin.version,
                          })
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

      {/* Schema Viewer Modal */}
      {viewingSchema && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="w-full max-w-2xl max-h-[80vh] flex flex-col">
            <CardContent className="p-4 border-b border-border">
              <div className="flex items-center justify-between">
                <div>
                  <h3 className="font-medium">{viewingSchema.name}</h3>
                  <p className="text-sm text-muted-foreground">
                    Configuration Schema — v{viewingSchema.version}
                  </p>
                </div>
                <Button variant="outline" onClick={() => setViewingSchema(null)}>
                  Close
                </Button>
              </div>
            </CardContent>
            <div className="flex-1 overflow-auto p-4">
              <pre className="text-xs font-mono whitespace-pre-wrap bg-muted p-4 rounded-lg">
                {JSON.stringify(viewingSchema.config_schema, null, 2)}
              </pre>
            </div>
          </Card>
        </div>
      )}
      {dialog}
    </div>
  )
}
