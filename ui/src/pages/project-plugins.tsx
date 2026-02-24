import { useState, useMemo } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Puzzle, Plus, Trash2, RefreshCw, Settings2, AlertCircle, Shield, Zap, X, Search, Check } from 'lucide-react'
import {
  listProjectPlugins,
  listPlugins,
  addPluginToProject,
  updateProjectPlugin,
  removePluginFromProject,
} from '@/lib/api'
import type { Plugin, ProjectPluginConfig } from '@/lib/api'
import { Button, Card, CardContent, Badge, EmptyState } from '@/components/ui'
import { cn } from '@/lib/utils'
import { useJsonSchema, generateSkeletonFromSchema, type ValidationError } from '@/hooks'

export function ProjectPluginsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const [showAddSection, setShowAddSection] = useState(false)
  const [selectedPluginNames, setSelectedPluginNames] = useState<Set<string>>(new Set())
  const [addingPlugins, setAddingPlugins] = useState(false)
  const [pluginSearch, setPluginSearch] = useState('')
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

  // Use JSON Schema validation hook for edit dialog
  const { validate, hasSchema } = useJsonSchema(editingPluginSchema)

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

  const togglePluginSelection = (plugin: Plugin) => {
    setSelectedPluginNames((prev) => {
      const next = new Set(prev)
      if (next.has(plugin.name)) {
        next.delete(plugin.name)
      } else {
        next.add(plugin.name)
      }
      return next
    })
  }

  const handleAddSelectedPlugins = async () => {
    if (selectedPluginNames.size === 0) return
    setAddingPlugins(true)

    try {
      for (const name of selectedPluginNames) {
        const plugin = pluginsToAdd.find((p) => p.name === name)
        if (!plugin) continue

        const skeleton = generateSkeletonFromSchema(plugin.config_schema)
        await addPluginToProject(projectId!, {
          plugin_name: plugin.name,
          plugin_version: plugin.version,
          config: skeleton,
        })
      }
      queryClient.invalidateQueries({ queryKey: ['project-plugins', projectId] })
      setShowAddSection(false)
      setSelectedPluginNames(new Set())
      setPluginSearch('')
    } finally {
      setAddingPlugins(false)
    }
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
    } catch {
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

  // Map plugin name → Plugin for quick lookup
  const pluginMap = useMemo(
    () => new Map((availablePluginsQuery.data ?? []).map((p) => [p.name, p])),
    [availablePluginsQuery.data]
  )

  // Filter out plugins already added to the project, then by search
  const addedPluginNames = new Set(configs.map((c) => c.plugin_name))
  const pluginsToAdd = availablePlugins.filter(
    (p) => !addedPluginNames.has(p.name)
  )
  const filteredPluginsToAdd = pluginsToAdd.filter(
    (p) =>
      !pluginSearch ||
      p.name.toLowerCase().includes(pluginSearch.toLowerCase()) ||
      p.description?.toLowerCase().includes(pluginSearch.toLowerCase()) ||
      p.plugin_type.toLowerCase().includes(pluginSearch.toLowerCase())
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
          {!showAddSection && (
            <Button size="sm" onClick={() => setShowAddSection(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Plugin
            </Button>
          )}
        </div>
      </div>

      {/* Add Plugin Section (inline) */}
      {showAddSection && (
        <Card className="mb-6">
          <CardContent className="p-6">
            <div className="flex items-center justify-between mb-4">
              <div>
                <h3 className="text-md font-semibold">Add Plugins</h3>
                <p className="text-xs text-muted-foreground mt-0.5">
                  Select plugins to add, then configure them individually.
                </p>
              </div>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setShowAddSection(false)
                  setSelectedPluginNames(new Set())
                  setPluginSearch('')
                }}
              >
                <X className="h-4 w-4" />
              </Button>
            </div>

            {availablePluginsQuery.isLoading ? (
              <div className="flex items-center justify-center py-8">
                <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
              </div>
            ) : pluginsToAdd.length === 0 ? (
              <p className="text-sm text-muted-foreground py-4">
                No plugins available. Register plugins in the Plugin Registry first.
              </p>
            ) : (
              <>
                {/* Search + Select All */}
                <div className="flex items-center gap-3 mb-4">
                  <div className="relative flex-1">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                    <input
                      type="text"
                      value={pluginSearch}
                      onChange={(e) => setPluginSearch(e.target.value)}
                      placeholder="Search plugins..."
                      className="w-full rounded-lg border border-input bg-background pl-9 pr-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:border-primary focus:ring-primary"
                    />
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      const allVisible = new Set(filteredPluginsToAdd.map((p) => p.name))
                      const allSelected = filteredPluginsToAdd.every((p) => selectedPluginNames.has(p.name))
                      if (allSelected) {
                        setSelectedPluginNames((prev) => {
                          const next = new Set(prev)
                          allVisible.forEach((n) => next.delete(n))
                          return next
                        })
                      } else {
                        setSelectedPluginNames((prev) => new Set([...prev, ...allVisible]))
                      }
                    }}
                  >
                    {filteredPluginsToAdd.every((p) => selectedPluginNames.has(p.name)) && filteredPluginsToAdd.length > 0
                      ? 'Deselect All'
                      : 'Select All'}
                  </Button>
                </div>

                {/* Plugin grid */}
                <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                  {filteredPluginsToAdd.map((plugin) => {
                    const isSelected = selectedPluginNames.has(plugin.name)
                    return (
                      <div
                        key={`${plugin.name}-${plugin.version}`}
                        className={cn(
                          'p-3 rounded-lg border cursor-pointer transition-colors relative',
                          isSelected
                            ? 'border-primary bg-primary/5 ring-1 ring-primary'
                            : 'border-border hover:border-primary/50'
                        )}
                        onClick={() => togglePluginSelection(plugin)}
                      >
                        {isSelected && (
                          <div className="absolute top-2 right-2 h-5 w-5 rounded-full bg-primary flex items-center justify-center">
                            <Check className="h-3 w-3 text-primary-foreground" />
                          </div>
                        )}
                        <div className="flex items-center gap-2 pr-6">
                          {plugin.plugin_type === 'dispatcher' ? (
                            <Zap className="h-5 w-5 text-secondary shrink-0" />
                          ) : (
                            <Shield className="h-5 w-5 text-secondary shrink-0" />
                          )}
                          <span className="font-medium text-sm truncate">{plugin.name}</span>
                          <Badge variant="outline" className="text-[10px] ml-auto shrink-0">
                            v{plugin.version}
                          </Badge>
                        </div>
                        {plugin.description && (
                          <p className="text-xs text-muted-foreground mt-1.5 line-clamp-2">
                            {plugin.description}
                          </p>
                        )}
                        <Badge variant="secondary" className="text-[10px] mt-1.5">
                          {plugin.plugin_type}
                        </Badge>
                      </div>
                    )
                  })}
                </div>

                {filteredPluginsToAdd.length === 0 && pluginSearch && (
                  <p className="text-sm text-muted-foreground text-center py-4">
                    No plugins matching "{pluginSearch}"
                  </p>
                )}

                {/* Action bar */}
                {selectedPluginNames.size > 0 && (
                  <div className="mt-4 pt-4 border-t flex items-center justify-between">
                    <p className="text-sm text-muted-foreground">
                      {selectedPluginNames.size} plugin{selectedPluginNames.size !== 1 ? 's' : ''} selected
                    </p>
                    <div className="flex gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setSelectedPluginNames(new Set())}
                      >
                        Clear
                      </Button>
                      <Button
                        size="sm"
                        onClick={handleAddSelectedPlugins}
                        disabled={addingPlugins}
                      >
                        {addingPlugins
                          ? 'Adding...'
                          : `Add ${selectedPluginNames.size} Plugin${selectedPluginNames.size !== 1 ? 's' : ''}`}
                      </Button>
                    </div>
                  </div>
                )}
              </>
            )}
          </CardContent>
        </Card>
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
        <EmptyState
          icon={Puzzle}
          title="No plugins configured"
          description="Add plugins to enhance your API gateway"
          action={
            <Button onClick={() => setShowAddSection(true)}>
              <Plus className="h-4 w-4 mr-2" />
              Add Plugin
            </Button>
          }
        />
      ) : (
        <div className="space-y-4">
          {configs.map((config) => {
            const pluginInfo = pluginMap.get(config.plugin_name)
            return (
              <Card key={config.id}>
                <CardContent className="p-4">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-4">
                      {pluginInfo?.plugin_type === 'dispatcher' ? (
                        <Zap className="h-10 w-10 text-secondary shrink-0" />
                      ) : (
                        <Shield className="h-10 w-10 text-secondary shrink-0" />
                      )}
                      <div>
                        <div className="flex items-center gap-2">
                          <h3 className="font-medium">{config.plugin_name}</h3>
                          <Badge variant="outline">v{config.plugin_version}</Badge>
                          {pluginInfo && (
                            <Badge variant="outline" className="text-xs">
                              {pluginInfo.plugin_type}
                            </Badge>
                          )}
                          {config.enabled ? (
                            <Badge className="bg-green-500/10 text-green-500">
                              Enabled
                            </Badge>
                          ) : (
                            <Badge variant="secondary">Disabled</Badge>
                          )}
                        </div>
                        {pluginInfo?.description && (
                          <p className="mt-1 text-sm text-muted-foreground">
                            {pluginInfo.description}
                          </p>
                        )}
                        <div className="mt-1 flex items-center gap-2 flex-wrap">
                          <span className="text-xs text-muted-foreground">
                            Priority: {config.priority}
                          </span>
                          {pluginInfo?.capabilities && pluginInfo.capabilities.length > 0 && (
                            <>
                              <span className="text-muted-foreground">·</span>
                              {pluginInfo.capabilities.map((cap) => (
                                <Badge
                                  key={cap}
                                  variant="secondary"
                                  className="text-[10px] px-1.5 py-0"
                                >
                                  {cap}
                                </Badge>
                              ))}
                            </>
                          )}
                        </div>
                      </div>
                    </div>
                    <div className="flex gap-2 shrink-0">
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
            )
          })}
        </div>
      )}
    </div>
  )
}
