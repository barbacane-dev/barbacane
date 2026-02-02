import { useState, useMemo } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Puzzle, Plus, Trash2, RefreshCw, Settings2, AlertCircle } from 'lucide-react'
import {
  listProjectPlugins,
  listPlugins,
  addPluginToProject,
  updateProjectPlugin,
  removePluginFromProject,
} from '@/lib/api'
import type { AddPluginToProjectRequest, Plugin, ProjectPluginConfig } from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { cn } from '@/lib/utils'
import { useJsonSchema, type ValidationError } from '@/hooks'

export function ProjectPluginsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const [showAddDialog, setShowAddDialog] = useState(false)
  const [selectedPlugin, setSelectedPlugin] = useState<Plugin | null>(null)
  const [editingConfig, setEditingConfig] = useState<ProjectPluginConfig | null>(null)
  const [configJson, setConfigJson] = useState('')
  const [validationErrors, setValidationErrors] = useState<ValidationError[]>([])
  const [jsonParseError, setJsonParseError] = useState<string | null>(null)

  const configsQuery = useQuery({
    queryKey: ['project-plugins', projectId],
    queryFn: () => listProjectPlugins(projectId!),
    enabled: !!projectId,
  })

  // Fetch all plugins (for schemas and add dialog)
  const availablePluginsQuery = useQuery({
    queryKey: ['plugins'],
    queryFn: () => listPlugins(),
  })

  // Find the schema for the currently editing plugin
  const editingPluginSchema = useMemo(() => {
    if (!editingConfig || !availablePluginsQuery.data) return null
    const plugin = availablePluginsQuery.data.find(
      (p) => p.name === editingConfig.plugin_name
    )
    return plugin?.config_schema ?? null
  }, [editingConfig, availablePluginsQuery.data])

  // Use JSON Schema validation hook
  const { validate, hasSchema } = useJsonSchema(editingPluginSchema)

  const addMutation = useMutation({
    mutationFn: (data: AddPluginToProjectRequest) =>
      addPluginToProject(projectId!, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-plugins', projectId] })
      setShowAddDialog(false)
      setSelectedPlugin(null)
    },
  })

  const updateMutation = useMutation({
    mutationFn: ({ pluginName, enabled, config }: {
      pluginName: string
      enabled?: boolean
      config?: Record<string, unknown>
    }) => updateProjectPlugin(projectId!, pluginName, { enabled, config }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-plugins', projectId] })
      setEditingConfig(null)
    },
  })

  const removeMutation = useMutation({
    mutationFn: (pluginName: string) =>
      removePluginFromProject(projectId!, pluginName),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-plugins', projectId] })
    },
  })

  const handleAddPlugin = () => {
    if (!selectedPlugin) return
    addMutation.mutate({
      plugin_name: selectedPlugin.name,
      plugin_version: selectedPlugin.version,
    })
  }

  const handleConfigChange = (value: string) => {
    setConfigJson(value)
    setJsonParseError(null)
    setValidationErrors([])

    // Try to parse and validate
    try {
      const config = JSON.parse(value)
      const result = validate(config)
      setValidationErrors(result.errors)
    } catch {
      // Don't set parse error while typing - only on save
    }
  }

  const handleSaveConfig = () => {
    if (!editingConfig) return

    // First check JSON syntax
    let config: Record<string, unknown>
    try {
      config = JSON.parse(configJson)
      setJsonParseError(null)
    } catch (e) {
      setJsonParseError('Invalid JSON syntax')
      return
    }

    // Then validate against schema
    const result = validate(config)
    setValidationErrors(result.errors)

    if (!result.valid) {
      return // Don't save if validation fails
    }

    updateMutation.mutate({
      pluginName: editingConfig.plugin_name,
      config,
    })
  }

  const configs = configsQuery.data ?? []
  const availablePlugins = availablePluginsQuery.data ?? []

  // Filter out plugins already added to the project
  const addedPluginNames = new Set(configs.map((c) => c.plugin_name))
  const pluginsToAdd = availablePlugins.filter(
    (p) => !addedPluginNames.has(p.name)
  )

  return (
    <div className="p-8">
      <div className="mb-6 flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Plugin Configuration</h2>
          <p className="text-sm text-muted-foreground">
            Configure plugins for this project
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => configsQuery.refetch()}
            disabled={configsQuery.isFetching}
          >
            <RefreshCw
              className={cn('h-4 w-4 mr-2', configsQuery.isFetching && 'animate-spin')}
            />
            Refresh
          </Button>
          <Button size="sm" onClick={() => setShowAddDialog(true)}>
            <Plus className="h-4 w-4 mr-2" />
            Add Plugin
          </Button>
        </div>
      </div>

      {/* Add Plugin Dialog */}
      {showAddDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="w-full max-w-md">
            <CardContent className="p-6">
              <h2 className="text-lg font-semibold mb-4">Add Plugin</h2>
              {availablePluginsQuery.isLoading ? (
                <div className="flex items-center justify-center py-8">
                  <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
                </div>
              ) : pluginsToAdd.length === 0 ? (
                <p className="text-sm text-muted-foreground py-4">
                  No plugins available. Register plugins in the Plugin Registry first.
                </p>
              ) : (
                <div className="space-y-2 max-h-60 overflow-auto">
                  {pluginsToAdd.map((plugin) => (
                    <div
                      key={`${plugin.name}-${plugin.version}`}
                      className={cn(
                        'p-3 rounded-lg border cursor-pointer transition-colors',
                        selectedPlugin?.name === plugin.name
                          ? 'border-primary bg-primary/5'
                          : 'border-border hover:border-primary/50'
                      )}
                      onClick={() => setSelectedPlugin(plugin)}
                    >
                      <div className="flex items-center justify-between">
                        <span className="font-medium">{plugin.name}</span>
                        <Badge variant="outline">v{plugin.version}</Badge>
                      </div>
                      {plugin.description && (
                        <p className="text-sm text-muted-foreground mt-1">
                          {plugin.description}
                        </p>
                      )}
                    </div>
                  ))}
                </div>
              )}
              {addMutation.isError && (
                <p className="text-sm text-destructive mt-4">
                  {addMutation.error instanceof Error
                    ? addMutation.error.message
                    : 'Failed to add plugin'}
                </p>
              )}
              <div className="flex justify-end gap-2 mt-4">
                <Button
                  variant="outline"
                  onClick={() => {
                    setShowAddDialog(false)
                    setSelectedPlugin(null)
                  }}
                >
                  Cancel
                </Button>
                <Button
                  onClick={handleAddPlugin}
                  disabled={!selectedPlugin || addMutation.isPending}
                >
                  {addMutation.isPending ? 'Adding...' : 'Add'}
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}

      {/* Edit Config Dialog */}
      {editingConfig && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="w-full max-w-lg">
            <CardContent className="p-6">
              <h2 className="text-lg font-semibold mb-4">
                Configure {editingConfig.plugin_name}
              </h2>
              <div>
                <div className="flex items-center justify-between mb-2">
                  <label className="block text-sm font-medium">
                    Configuration (JSON)
                  </label>
                  {hasSchema && (
                    <Badge variant="outline" className="text-xs">
                      Schema validation enabled
                    </Badge>
                  )}
                </div>
                <textarea
                  value={configJson}
                  onChange={(e) => handleConfigChange(e.target.value)}
                  className={cn(
                    'w-full h-48 rounded-lg border bg-background px-3 py-2 font-mono text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1',
                    jsonParseError || validationErrors.length > 0
                      ? 'border-destructive focus:border-destructive focus:ring-destructive'
                      : 'border-input focus:border-primary focus:ring-primary'
                  )}
                  placeholder="{}"
                />
              </div>

              {/* JSON Parse Error */}
              {jsonParseError && (
                <div className="mt-3 p-3 rounded-lg bg-destructive/10 border border-destructive/20">
                  <div className="flex items-center gap-2 text-destructive">
                    <AlertCircle className="h-4 w-4" />
                    <span className="text-sm font-medium">{jsonParseError}</span>
                  </div>
                </div>
              )}

              {/* Validation Errors */}
              {validationErrors.length > 0 && !jsonParseError && (
                <div className="mt-3 p-3 rounded-lg bg-destructive/10 border border-destructive/20">
                  <div className="flex items-center gap-2 text-destructive mb-2">
                    <AlertCircle className="h-4 w-4" />
                    <span className="text-sm font-medium">
                      Validation errors ({validationErrors.length})
                    </span>
                  </div>
                  <ul className="space-y-1">
                    {validationErrors.map((error, idx) => (
                      <li key={idx} className="text-sm text-destructive/80">
                        <code className="text-xs bg-destructive/10 px-1 py-0.5 rounded">
                          {error.path}
                        </code>{' '}
                        {error.message}
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              {updateMutation.isError && (
                <p className="text-sm text-destructive mt-4">
                  {updateMutation.error instanceof Error
                    ? updateMutation.error.message
                    : 'Failed to update configuration'}
                </p>
              )}
              <div className="flex justify-end gap-2 mt-4">
                <Button
                  variant="outline"
                  onClick={() => {
                    setEditingConfig(null)
                    setValidationErrors([])
                    setJsonParseError(null)
                  }}
                >
                  Cancel
                </Button>
                <Button
                  onClick={handleSaveConfig}
                  disabled={updateMutation.isPending || validationErrors.length > 0}
                >
                  {updateMutation.isPending ? 'Saving...' : 'Save'}
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}

      {configsQuery.isLoading ? (
        <div className="flex items-center justify-center p-12">
          <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : configsQuery.isError ? (
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">Failed to load plugin configurations</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => configsQuery.refetch()}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      ) : configs.length === 0 ? (
        <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
          <div className="text-center">
            <Puzzle className="mx-auto h-12 w-12 text-muted-foreground" />
            <h3 className="mt-4 text-lg font-medium">No plugins configured</h3>
            <p className="mt-2 text-sm text-muted-foreground">
              Add plugins to enhance your API gateway
            </p>
            <Button className="mt-4" onClick={() => setShowAddDialog(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Plugin
            </Button>
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          {configs.map((config) => (
            <Card key={config.id}>
              <CardContent className="p-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-4">
                    <Puzzle className="h-10 w-10 text-secondary" />
                    <div>
                      <div className="flex items-center gap-2">
                        <h3 className="font-medium">{config.plugin_name}</h3>
                        <Badge variant="outline">v{config.plugin_version}</Badge>
                        {config.enabled ? (
                          <Badge className="bg-green-500/10 text-green-500">
                            Enabled
                          </Badge>
                        ) : (
                          <Badge variant="secondary">Disabled</Badge>
                        )}
                      </div>
                      <p className="mt-1 text-sm text-muted-foreground">
                        Priority: {config.priority}
                      </p>
                    </div>
                  </div>
                  <div className="flex gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        setEditingConfig(config)
                        setConfigJson(JSON.stringify(config.config, null, 2))
                        setValidationErrors([])
                        setJsonParseError(null)
                      }}
                    >
                      <Settings2 className="h-4 w-4 mr-1" />
                      Configure
                    </Button>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        updateMutation.mutate({
                          pluginName: config.plugin_name,
                          enabled: !config.enabled,
                        })
                      }
                      disabled={updateMutation.isPending}
                    >
                      {config.enabled ? 'Disable' : 'Enable'}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        if (confirm(`Remove "${config.plugin_name}" from project?`)) {
                          removeMutation.mutate(config.plugin_name)
                        }
                      }}
                      disabled={removeMutation.isPending}
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
