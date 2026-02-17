import { useState, useMemo } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  RefreshCw,
  ChevronDown,
  ChevronRight,
  Shield,
  Zap,
  GitBranch,
  AlertTriangle,
  Pencil,
  Plus,
  X,
  AlertCircle,
  Settings2,
} from 'lucide-react'
import { getProjectOperations, patchSpecOperations, listPlugins } from '@/lib/api'
import type {
  SpecOperations,
  OperationSummary,
  MiddlewareBinding,
  DispatchBinding,
  Plugin,
} from '@/lib/api'
import { Button, Card, CardContent, Badge } from '@/components/ui'
import { cn } from '@/lib/utils'
import { useJsonSchema, generateSkeletonFromSchema } from '@/hooks'

// ============================================================================
// Constants
// ============================================================================

const METHOD_COLORS: Record<string, string> = {
  GET: 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400',
  POST: 'bg-blue-500/15 text-blue-700 dark:text-blue-400',
  PUT: 'bg-amber-500/15 text-amber-700 dark:text-amber-400',
  PATCH: 'bg-orange-500/15 text-orange-700 dark:text-orange-400',
  DELETE: 'bg-red-500/15 text-red-700 dark:text-red-400',
  SEND: 'bg-violet-500/15 text-violet-700 dark:text-violet-400',
  RECEIVE: 'bg-cyan-500/15 text-cyan-700 dark:text-cyan-400',
}

// ============================================================================
// Edit state types
// ============================================================================

type EditState =
  | null
  | {
      type: 'global-middlewares'
      specId: string
      specName: string
      middlewares: MiddlewareBinding[]
    }
  | {
      type: 'operation'
      specId: string
      specName: string
      path: string
      method: string
      dispatch: DispatchBinding | null
      middlewares: MiddlewareBinding[] | null
    }
  | {
      type: 'config'
      label: string
      configJson: string
      schema: Record<string, unknown> | null
      onSave: (config: Record<string, unknown>) => void
    }

// ============================================================================
// Main page component
// ============================================================================

export function ProjectOperationsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const [expandedSpecs, setExpandedSpecs] = useState<Set<string>>(new Set())
  const [expandedOps, setExpandedOps] = useState<Set<string>>(new Set())
  const [editState, setEditState] = useState<EditState>(null)

  const operationsQuery = useQuery({
    queryKey: ['project-operations', projectId],
    queryFn: () => getProjectOperations(projectId!),
    enabled: !!projectId,
  })

  const pluginsQuery = useQuery({
    queryKey: ['plugins'],
    queryFn: () => listPlugins(),
  })

  const middlewarePlugins = useMemo(
    () => (pluginsQuery.data ?? []).filter((p) => p.plugin_type === 'middleware'),
    [pluginsQuery.data]
  )

  const pluginMap = useMemo(
    () => new Map((pluginsQuery.data ?? []).map((p) => [p.name, p])),
    [pluginsQuery.data]
  )

  const patchMutation = useMutation({
    mutationFn: ({
      specId,
      payload,
    }: {
      specId: string
      payload: Parameters<typeof patchSpecOperations>[1]
    }) => patchSpecOperations(specId, payload),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-operations', projectId] })
      queryClient.invalidateQueries({ queryKey: ['project-specs', projectId] })
      setEditState(null)
    },
  })

  const toggleSpec = (specId: string) => {
    setExpandedSpecs((prev) => {
      const next = new Set(prev)
      if (next.has(specId)) next.delete(specId)
      else next.add(specId)
      return next
    })
  }

  const toggleOp = (key: string) => {
    setExpandedOps((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  const handleEditGlobalMiddlewares = (spec: SpecOperations) => {
    setEditState({
      type: 'global-middlewares',
      specId: spec.spec_id,
      specName: spec.spec_name,
      middlewares: spec.global_middlewares.map((mw) => ({ ...mw })),
    })
  }

  const handleEditOperation = (spec: SpecOperations, op: OperationSummary) => {
    setEditState({
      type: 'operation',
      specId: spec.spec_id,
      specName: spec.spec_name,
      path: op.path,
      method: op.method,
      dispatch: op.dispatch ? { ...op.dispatch } : null,
      middlewares: op.middlewares ? op.middlewares.map((mw) => ({ ...mw })) : null,
    })
  }

  const specs = operationsQuery.data?.specs ?? []

  return (
    <div className="p-8">
      <div className="mb-6 flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Operations</h2>
          <p className="text-sm text-muted-foreground">
            View and configure plugin bindings for your API operations
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => operationsQuery.refetch()}
          disabled={operationsQuery.isFetching}
        >
          <RefreshCw
            className={cn('h-4 w-4 mr-2', operationsQuery.isFetching && 'animate-spin')}
          />
          Refresh
        </Button>
      </div>

      {operationsQuery.isLoading ? (
        <div className="flex items-center justify-center p-12">
          <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : operationsQuery.isError ? (
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">Failed to load operations</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => operationsQuery.refetch()}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      ) : specs.length === 0 ? (
        <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
          <div className="text-center">
            <GitBranch className="mx-auto h-12 w-12 text-muted-foreground" />
            <h3 className="mt-4 text-lg font-medium">No operations found</h3>
            <p className="mt-2 text-sm text-muted-foreground">
              Upload an API spec with operations to see plugin bindings here
            </p>
          </div>
        </div>
      ) : (
        <div className="space-y-6">
          {specs.map((spec) => (
            <SpecSection
              key={spec.spec_id}
              spec={spec}
              expanded={!expandedSpecs.has(spec.spec_id)}
              onToggle={() => toggleSpec(spec.spec_id)}
              expandedOps={expandedOps}
              onToggleOp={toggleOp}
              onEditGlobal={() => handleEditGlobalMiddlewares(spec)}
              onEditOperation={(op) => handleEditOperation(spec, op)}
            />
          ))}
        </div>
      )}

      {/* Edit Global Middlewares Dialog */}
      {editState?.type === 'global-middlewares' && (
        <EditMiddlewaresDialog
          title={`Global Middlewares — ${editState.specName}`}
          middlewares={editState.middlewares}
          availablePlugins={middlewarePlugins}
          pluginMap={pluginMap}
          isPending={patchMutation.isPending}
          error={patchMutation.error}
          onSave={(middlewares) => {
            patchMutation.mutate({
              specId: editState.specId,
              payload: { global_middlewares: middlewares },
            })
          }}
          onClose={() => setEditState(null)}
          onEditConfig={(mw, schema, onUpdate) => {
            setEditState({
              type: 'config',
              label: mw.name,
              configJson: JSON.stringify(mw.config ?? {}, null, 2),
              schema,
              onSave: (config) => {
                onUpdate(config)
                // Return to the middleware editing dialog - the caller handles restoring state
              },
            })
          }}
        />
      )}

      {/* Edit Operation Dialog */}
      {editState?.type === 'operation' && (
        <EditOperationDialog
          specName={editState.specName}
          path={editState.path}
          method={editState.method}
          dispatch={editState.dispatch}
          middlewares={editState.middlewares}
          availablePlugins={middlewarePlugins}
          pluginMap={pluginMap}
          isPending={patchMutation.isPending}
          error={patchMutation.error}
          onSave={(middlewares, dispatch) => {
            patchMutation.mutate({
              specId: editState.specId,
              payload: {
                operations: [
                  {
                    path: editState.path,
                    method: editState.method,
                    middlewares,
                    dispatch,
                  },
                ],
              },
            })
          }}
          onClose={() => setEditState(null)}
        />
      )}

      {/* Config Editor Dialog */}
      {editState?.type === 'config' && (
        <ConfigEditorDialog
          label={editState.label}
          initialJson={editState.configJson}
          schema={editState.schema}
          onSave={editState.onSave}
          onClose={() => setEditState(null)}
        />
      )}
    </div>
  )
}

// ============================================================================
// Spec section
// ============================================================================

function SpecSection({
  spec,
  expanded,
  onToggle,
  expandedOps,
  onToggleOp,
  onEditGlobal,
  onEditOperation,
}: {
  spec: SpecOperations
  expanded: boolean
  onToggle: () => void
  expandedOps: Set<string>
  onToggleOp: (key: string) => void
  onEditGlobal: () => void
  onEditOperation: (op: OperationSummary) => void
}) {
  return (
    <Card>
      <CardContent className="p-0">
        <button
          onClick={onToggle}
          className="flex w-full items-center gap-3 p-4 text-left hover:bg-muted/50 transition-colors"
        >
          {expanded ? (
            <ChevronDown className="h-4 w-4 text-muted-foreground shrink-0" />
          ) : (
            <ChevronRight className="h-4 w-4 text-muted-foreground shrink-0" />
          )}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2">
              <span className="font-medium truncate">{spec.spec_name}</span>
              <Badge variant="outline" className="shrink-0">
                {spec.spec_type === 'openapi' ? 'OpenAPI' : 'AsyncAPI'}
              </Badge>
            </div>
            <p className="text-sm text-muted-foreground mt-0.5">
              {spec.operations.length} operation{spec.operations.length !== 1 ? 's' : ''}
              {spec.global_middlewares.length > 0 &&
                ` · ${spec.global_middlewares.length} global middleware${spec.global_middlewares.length !== 1 ? 's' : ''}`}
            </p>
          </div>
        </button>

        {expanded && (
          <div className="border-t border-border">
            {/* Global middlewares */}
            <div className="px-4 py-3 bg-muted/30 border-b border-border">
              <div className="flex items-center justify-between mb-2">
                <span className="text-xs font-medium uppercase tracking-wider text-muted-foreground">
                  Global Middlewares
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 px-2 text-xs"
                  onClick={(e) => {
                    e.stopPropagation()
                    onEditGlobal()
                  }}
                >
                  <Pencil className="h-3 w-3 mr-1" />
                  Edit
                </Button>
              </div>
              {spec.global_middlewares.length > 0 ? (
                <div className="flex flex-wrap gap-2">
                  {spec.global_middlewares.map((mw, i) => (
                    <MiddlewareBadge key={i} binding={mw} />
                  ))}
                </div>
              ) : (
                <p className="text-xs text-muted-foreground italic">
                  No global middlewares configured
                </p>
              )}
            </div>

            {/* Operations list */}
            {spec.operations.length === 0 ? (
              <div className="px-4 py-6 text-center text-sm text-muted-foreground">
                No operations defined in this spec
              </div>
            ) : (
              <div className="divide-y divide-border">
                {spec.operations.map((op) => {
                  const opKey = `${spec.spec_id}:${op.method}:${op.path}`
                  return (
                    <OperationRow
                      key={opKey}
                      operation={op}
                      hasGlobalMiddlewares={spec.global_middlewares.length > 0}
                      expanded={expandedOps.has(opKey)}
                      onToggle={() => onToggleOp(opKey)}
                      onEdit={() => onEditOperation(op)}
                    />
                  )
                })}
              </div>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// ============================================================================
// Operation row
// ============================================================================

function OperationRow({
  operation,
  hasGlobalMiddlewares,
  expanded,
  onToggle,
  onEdit,
}: {
  operation: OperationSummary
  hasGlobalMiddlewares: boolean
  expanded: boolean
  onToggle: () => void
  onEdit: () => void
}) {
  return (
    <div>
      <button
        onClick={onToggle}
        className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-muted/30 transition-colors"
      >
        <div className="flex items-center gap-3 flex-1 min-w-0">
          <span
            className={cn(
              'inline-flex items-center justify-center rounded px-2 py-0.5 text-xs font-bold uppercase shrink-0 w-16 text-center',
              METHOD_COLORS[operation.method.toUpperCase()] ??
                'bg-muted text-muted-foreground'
            )}
          >
            {operation.method}
          </span>
          <span className="font-mono text-sm truncate">{operation.path}</span>
          {operation.deprecated && (
            <AlertTriangle className="h-3.5 w-3.5 text-amber-500 shrink-0" />
          )}
        </div>

        <div className="flex items-center gap-2 shrink-0">
          {operation.dispatch && (
            <span className="inline-flex items-center gap-1 rounded-md bg-violet-500/10 px-2 py-0.5 text-xs text-violet-700 dark:text-violet-400">
              <Zap className="h-3 w-3" />
              {operation.dispatch.name}
            </span>
          )}

          {operation.middlewares === null ? (
            hasGlobalMiddlewares ? (
              <span className="text-xs text-muted-foreground italic">inherits global</span>
            ) : null
          ) : operation.middlewares.length === 0 ? (
            <span className="text-xs text-muted-foreground italic">no middlewares</span>
          ) : (
            <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
              <Shield className="h-3 w-3" />
              {operation.middlewares.length} middleware
              {operation.middlewares.length !== 1 ? 's' : ''}
            </span>
          )}

          <ChevronDown
            className={cn(
              'h-3.5 w-3.5 text-muted-foreground transition-transform',
              !expanded && '-rotate-90'
            )}
          />
        </div>
      </button>

      {expanded && (
        <div className="px-4 pb-3 ml-[calc(1rem+4.5rem)] space-y-2">
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="sm"
              className="h-6 px-2 text-xs"
              onClick={onEdit}
            >
              <Pencil className="h-3 w-3 mr-1" />
              Edit bindings
            </Button>
          </div>

          {operation.dispatch?.config &&
            Object.keys(operation.dispatch.config).length > 0 && (
              <div>
                <div className="text-xs font-medium text-muted-foreground mb-1">
                  Dispatch config
                </div>
                <pre className="text-xs font-mono bg-muted rounded px-3 py-2 overflow-x-auto">
                  {JSON.stringify(operation.dispatch.config, null, 2)}
                </pre>
              </div>
            )}

          {operation.middlewares && operation.middlewares.length > 0 && (
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1">
                Middlewares (override)
              </div>
              <div className="space-y-1">
                {operation.middlewares.map((mw, i) => (
                  <div
                    key={i}
                    className="flex items-start gap-2 text-xs font-mono bg-muted rounded px-3 py-2"
                  >
                    <Shield className="h-3.5 w-3.5 text-blue-500 mt-0.5 shrink-0" />
                    <div className="min-w-0">
                      <span className="font-medium">{mw.name}</span>
                      {mw.config && Object.keys(mw.config).length > 0 && (
                        <pre className="mt-1 text-muted-foreground overflow-x-auto">
                          {JSON.stringify(mw.config, null, 2)}
                        </pre>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {(!operation.dispatch?.config ||
            Object.keys(operation.dispatch.config).length === 0) &&
            (!operation.middlewares || operation.middlewares.length === 0) && (
              <p className="text-xs text-muted-foreground italic">
                No additional configuration details
              </p>
            )}
        </div>
      )}
    </div>
  )
}

// ============================================================================
// Edit Middlewares Dialog (used for global middlewares)
// ============================================================================

function EditMiddlewaresDialog({
  title,
  middlewares: initialMiddlewares,
  availablePlugins,
  pluginMap,
  isPending,
  error,
  onSave,
  onClose,
  onEditConfig,
}: {
  title: string
  middlewares: MiddlewareBinding[]
  availablePlugins: Plugin[]
  pluginMap: Map<string, Plugin>
  isPending: boolean
  error: Error | null
  onSave: (middlewares: MiddlewareBinding[]) => void
  onClose: () => void
  onEditConfig: (
    mw: MiddlewareBinding,
    schema: Record<string, unknown> | null,
    onUpdate: (config: Record<string, unknown>) => void
  ) => void
}) {
  const [middlewares, setMiddlewares] = useState<MiddlewareBinding[]>(initialMiddlewares)
  const [showPicker, setShowPicker] = useState(false)

  const handleAdd = (plugin: Plugin) => {
    const skeleton = generateSkeletonFromSchema(plugin.config_schema)
    setMiddlewares((prev) => [...prev, { name: plugin.name, config: skeleton }])
    setShowPicker(false)
  }

  const handleRemove = (index: number) => {
    setMiddlewares((prev) => prev.filter((_, i) => i !== index))
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <Card className="w-full max-w-lg max-h-[80vh] flex flex-col">
        <CardContent className="p-6 flex flex-col gap-4 overflow-hidden">
          <h2 className="text-lg font-semibold">{title}</h2>

          <div className="flex-1 overflow-auto space-y-2">
            {middlewares.length === 0 ? (
              <p className="text-sm text-muted-foreground py-4 text-center">
                No middlewares. Add one below.
              </p>
            ) : (
              middlewares.map((mw, i) => {
                const plugin = pluginMap.get(mw.name.split('@')[0])
                return (
                  <div
                    key={i}
                    className="flex items-center gap-2 p-3 rounded-lg border border-border"
                  >
                    <Shield className="h-4 w-4 text-blue-500 shrink-0" />
                    <div className="flex-1 min-w-0">
                      <span className="text-sm font-medium">{mw.name}</span>
                      {mw.config && Object.keys(mw.config).length > 0 && (
                        <p className="text-xs text-muted-foreground truncate">
                          {Object.keys(mw.config).join(', ')}
                        </p>
                      )}
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2"
                      onClick={() =>
                        onEditConfig(
                          mw,
                          plugin?.config_schema ?? null,
                          (config) => {
                            setMiddlewares((prev) =>
                              prev.map((m, idx) =>
                                idx === i ? { ...m, config } : m
                              )
                            )
                          }
                        )
                      }
                    >
                      <Settings2 className="h-3.5 w-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-destructive hover:text-destructive"
                      onClick={() => handleRemove(i)}
                    >
                      <X className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                )
              })
            )}

            {showPicker ? (
              <PluginPicker
                plugins={availablePlugins}
                onSelect={handleAdd}
                onCancel={() => setShowPicker(false)}
              />
            ) : (
              <Button
                variant="outline"
                size="sm"
                className="w-full"
                onClick={() => setShowPicker(true)}
              >
                <Plus className="h-4 w-4 mr-2" />
                Add Middleware
              </Button>
            )}
          </div>

          {error && (
            <p className="text-sm text-destructive">
              {error instanceof Error ? error.message : 'Failed to save'}
            </p>
          )}

          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button onClick={() => onSave(middlewares)} disabled={isPending}>
              {isPending ? 'Saving...' : 'Save'}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

// ============================================================================
// Edit Operation Dialog
// ============================================================================

function EditOperationDialog({
  specName,
  path,
  method,
  dispatch: initialDispatch,
  middlewares: initialMiddlewares,
  availablePlugins,
  pluginMap,
  isPending,
  error,
  onSave,
  onClose,
}: {
  specName: string
  path: string
  method: string
  dispatch: DispatchBinding | null
  middlewares: MiddlewareBinding[] | null
  availablePlugins: Plugin[]
  pluginMap: Map<string, Plugin>
  isPending: boolean
  error: Error | null
  onSave: (
    middlewares: MiddlewareBinding[] | null | undefined,
    dispatch: DispatchBinding | null | undefined
  ) => void
  onClose: () => void
}) {
  type MwMode = 'inherit' | 'override' | 'none'
  const initialMode: MwMode =
    initialMiddlewares === null ? 'inherit' : initialMiddlewares.length === 0 ? 'none' : 'override'

  const [mwMode, setMwMode] = useState<MwMode>(initialMode)
  const [middlewares, setMiddlewares] = useState<MiddlewareBinding[]>(initialMiddlewares ?? [])
  const [dispatch, setDispatch] = useState<DispatchBinding | null>(initialDispatch)
  const [showPicker, setShowPicker] = useState(false)
  const [editingConfigIdx, setEditingConfigIdx] = useState<number | null>(null)
  const [editingDispatchConfig, setEditingDispatchConfig] = useState(false)
  const [configJson, setConfigJson] = useState('')

  const editingPlugin = useMemo(() => {
    if (editingConfigIdx !== null) {
      const mw = middlewares[editingConfigIdx]
      return mw ? pluginMap.get(mw.name.split('@')[0]) ?? null : null
    }
    if (editingDispatchConfig && dispatch) {
      return pluginMap.get(dispatch.name.split('@')[0]) ?? null
    }
    return null
  }, [editingConfigIdx, editingDispatchConfig, middlewares, dispatch, pluginMap])

  const { validate, hasSchema } = useJsonSchema(editingPlugin?.config_schema ?? null)

  const [jsonParseError, setJsonParseError] = useState<string | null>(null)
  const [validationErrors, setValidationErrors] = useState<{ path: string; message: string }[]>([])

  const handleAdd = (plugin: Plugin) => {
    const skeleton = generateSkeletonFromSchema(plugin.config_schema)
    setMiddlewares((prev) => [...prev, { name: plugin.name, config: skeleton }])
    setShowPicker(false)
  }

  const handleRemove = (index: number) => {
    setMiddlewares((prev) => prev.filter((_, i) => i !== index))
  }

  const handleOpenConfig = (index: number) => {
    const mw = middlewares[index]
    setConfigJson(JSON.stringify(mw.config ?? {}, null, 2))
    setEditingConfigIdx(index)
    setEditingDispatchConfig(false)
    setJsonParseError(null)
    setValidationErrors([])
  }

  const handleOpenDispatchConfig = () => {
    if (!dispatch) return
    setConfigJson(JSON.stringify(dispatch.config ?? {}, null, 2))
    setEditingDispatchConfig(true)
    setEditingConfigIdx(null)
    setJsonParseError(null)
    setValidationErrors([])
  }

  const handleConfigChange = (value: string) => {
    setConfigJson(value)
    setJsonParseError(null)
    setValidationErrors([])
    try {
      const parsed = JSON.parse(value)
      const result = validate(parsed)
      setValidationErrors(result.errors)
    } catch {
      // Don't show parse error while typing
    }
  }

  const handleSaveConfig = () => {
    let parsed: Record<string, unknown>
    try {
      parsed = JSON.parse(configJson)
      setJsonParseError(null)
    } catch {
      setJsonParseError('Invalid JSON syntax')
      return
    }

    const result = validate(parsed)
    setValidationErrors(result.errors)
    if (!result.valid) return

    if (editingConfigIdx !== null) {
      setMiddlewares((prev) =>
        prev.map((m, i) => (i === editingConfigIdx ? { ...m, config: parsed } : m))
      )
      setEditingConfigIdx(null)
    } else if (editingDispatchConfig && dispatch) {
      setDispatch({ ...dispatch, config: parsed })
      setEditingDispatchConfig(false)
    }
  }

  const handleSave = () => {
    const mwPayload: MiddlewareBinding[] | null | undefined =
      mwMode === 'inherit' ? null : mwMode === 'none' ? [] : middlewares
    // Only send dispatch if it was changed (compare by reference isn't great, so always send)
    onSave(mwPayload, dispatch)
  }

  const isEditingConfig = editingConfigIdx !== null || editingDispatchConfig

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <Card className="w-full max-w-lg max-h-[80vh] flex flex-col">
        <CardContent className="p-6 flex flex-col gap-4 overflow-hidden">
          <div>
            <h2 className="text-lg font-semibold">Edit Operation</h2>
            <div className="flex items-center gap-2 mt-1">
              <span
                className={cn(
                  'inline-flex items-center justify-center rounded px-2 py-0.5 text-xs font-bold uppercase w-16 text-center',
                  METHOD_COLORS[method.toUpperCase()] ?? 'bg-muted text-muted-foreground'
                )}
              >
                {method}
              </span>
              <span className="text-sm font-mono">{path}</span>
              <span className="text-xs text-muted-foreground">— {specName}</span>
            </div>
          </div>

          {isEditingConfig ? (
            /* Inline config editor */
            <div className="flex-1 overflow-auto space-y-3">
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium">
                  Configure{' '}
                  {editingConfigIdx !== null
                    ? middlewares[editingConfigIdx]?.name
                    : dispatch?.name}
                </label>
                {hasSchema && (
                  <Badge variant="outline" className="text-xs">
                    Schema validation
                  </Badge>
                )}
              </div>
              <textarea
                value={configJson}
                onChange={(e) => handleConfigChange(e.target.value)}
                className={cn(
                  'w-full h-40 rounded-lg border bg-background px-3 py-2 font-mono text-sm focus:outline-none focus:ring-1',
                  jsonParseError || validationErrors.length > 0
                    ? 'border-destructive focus:ring-destructive'
                    : 'border-input focus:ring-primary'
                )}
                placeholder="{}"
              />
              {jsonParseError && (
                <div className="p-2 rounded-lg bg-destructive/10 border border-destructive/20 flex items-center gap-2 text-destructive text-sm">
                  <AlertCircle className="h-4 w-4 shrink-0" />
                  {jsonParseError}
                </div>
              )}
              {validationErrors.length > 0 && !jsonParseError && (
                <div className="p-2 rounded-lg bg-destructive/10 border border-destructive/20">
                  <div className="flex items-center gap-2 text-destructive text-sm mb-1">
                    <AlertCircle className="h-4 w-4 shrink-0" />
                    Validation errors ({validationErrors.length})
                  </div>
                  <ul className="space-y-0.5">
                    {validationErrors.map((err, i) => (
                      <li key={i} className="text-xs text-destructive/80">
                        <code className="bg-destructive/10 px-1 rounded">{err.path}</code>{' '}
                        {err.message}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
              <div className="flex justify-end gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setEditingConfigIdx(null)
                    setEditingDispatchConfig(false)
                  }}
                >
                  Back
                </Button>
                <Button
                  size="sm"
                  onClick={handleSaveConfig}
                  disabled={validationErrors.length > 0}
                >
                  Apply
                </Button>
              </div>
            </div>
          ) : (
            /* Main operation editor */
            <div className="flex-1 overflow-auto space-y-4">
              {/* Dispatcher section */}
              {dispatch && (
                <div>
                  <div className="text-xs font-medium uppercase tracking-wider text-muted-foreground mb-2">
                    Dispatcher
                  </div>
                  <div className="flex items-center gap-2 p-3 rounded-lg border border-border">
                    <Zap className="h-4 w-4 text-violet-500 shrink-0" />
                    <span className="text-sm font-medium flex-1">{dispatch.name}</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2"
                      onClick={handleOpenDispatchConfig}
                    >
                      <Settings2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                </div>
              )}

              {/* Middleware mode selector */}
              <div>
                <div className="text-xs font-medium uppercase tracking-wider text-muted-foreground mb-2">
                  Middlewares
                </div>
                <div className="flex gap-2 mb-3">
                  {(['inherit', 'override', 'none'] as MwMode[]).map((mode) => (
                    <button
                      key={mode}
                      onClick={() => setMwMode(mode)}
                      className={cn(
                        'px-3 py-1.5 text-xs rounded-md border transition-colors',
                        mwMode === mode
                          ? 'border-primary bg-primary/10 text-foreground'
                          : 'border-border text-muted-foreground hover:border-primary/50'
                      )}
                    >
                      {mode === 'inherit' && 'Inherit global'}
                      {mode === 'override' && 'Override'}
                      {mode === 'none' && 'No middlewares'}
                    </button>
                  ))}
                </div>

                {mwMode === 'inherit' && (
                  <p className="text-xs text-muted-foreground italic">
                    This operation will use the global middleware chain.
                  </p>
                )}
                {mwMode === 'none' && (
                  <p className="text-xs text-muted-foreground italic">
                    This operation will run with no middlewares (opt-out).
                  </p>
                )}
                {mwMode === 'override' && (
                  <div className="space-y-2">
                    {middlewares.length === 0 && !showPicker && (
                      <p className="text-sm text-muted-foreground text-center py-2">
                        No middlewares. Add one below.
                      </p>
                    )}
                    {middlewares.map((mw, i) => (
                      <div
                        key={i}
                        className="flex items-center gap-2 p-3 rounded-lg border border-border"
                      >
                        <Shield className="h-4 w-4 text-blue-500 shrink-0" />
                        <div className="flex-1 min-w-0">
                          <span className="text-sm font-medium">{mw.name}</span>
                          {mw.config && Object.keys(mw.config).length > 0 && (
                            <p className="text-xs text-muted-foreground truncate">
                              {Object.keys(mw.config).join(', ')}
                            </p>
                          )}
                        </div>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-7 px-2"
                          onClick={() => handleOpenConfig(i)}
                        >
                          <Settings2 className="h-3.5 w-3.5" />
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-7 px-2 text-destructive hover:text-destructive"
                          onClick={() => handleRemove(i)}
                        >
                          <X className="h-3.5 w-3.5" />
                        </Button>
                      </div>
                    ))}

                    {showPicker ? (
                      <PluginPicker
                        plugins={availablePlugins}
                        onSelect={handleAdd}
                        onCancel={() => setShowPicker(false)}
                      />
                    ) : (
                      <Button
                        variant="outline"
                        size="sm"
                        className="w-full"
                        onClick={() => setShowPicker(true)}
                      >
                        <Plus className="h-4 w-4 mr-2" />
                        Add Middleware
                      </Button>
                    )}
                  </div>
                )}
              </div>
            </div>
          )}

          {!isEditingConfig && (
            <>
              {error && (
                <p className="text-sm text-destructive">
                  {error instanceof Error ? error.message : 'Failed to save'}
                </p>
              )}
              <div className="flex justify-end gap-2">
                <Button variant="outline" onClick={onClose}>
                  Cancel
                </Button>
                <Button onClick={handleSave} disabled={isPending}>
                  {isPending ? 'Saving...' : 'Save'}
                </Button>
              </div>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

// ============================================================================
// Config Editor Dialog (standalone, used from global middlewares editing)
// ============================================================================

function ConfigEditorDialog({
  label,
  initialJson,
  schema,
  onSave,
  onClose,
}: {
  label: string
  initialJson: string
  schema: Record<string, unknown> | null
  onSave: (config: Record<string, unknown>) => void
  onClose: () => void
}) {
  const [configJson, setConfigJson] = useState(initialJson)
  const [jsonParseError, setJsonParseError] = useState<string | null>(null)
  const [validationErrors, setValidationErrors] = useState<{ path: string; message: string }[]>([])
  const { validate, hasSchema } = useJsonSchema(schema)

  const handleChange = (value: string) => {
    setConfigJson(value)
    setJsonParseError(null)
    setValidationErrors([])
    try {
      const parsed = JSON.parse(value)
      const result = validate(parsed)
      setValidationErrors(result.errors)
    } catch {
      // parse error shown on save only
    }
  }

  const handleSave = () => {
    let parsed: Record<string, unknown>
    try {
      parsed = JSON.parse(configJson)
      setJsonParseError(null)
    } catch {
      setJsonParseError('Invalid JSON syntax')
      return
    }

    const result = validate(parsed)
    setValidationErrors(result.errors)
    if (!result.valid) return

    onSave(parsed)
    onClose()
  }

  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50">
      <Card className="w-full max-w-lg">
        <CardContent className="p-6 space-y-4">
          <div className="flex items-center justify-between">
            <h2 className="text-lg font-semibold">Configure {label}</h2>
            {hasSchema && (
              <Badge variant="outline" className="text-xs">
                Schema validation
              </Badge>
            )}
          </div>

          <textarea
            value={configJson}
            onChange={(e) => handleChange(e.target.value)}
            className={cn(
              'w-full h-48 rounded-lg border bg-background px-3 py-2 font-mono text-sm focus:outline-none focus:ring-1',
              jsonParseError || validationErrors.length > 0
                ? 'border-destructive focus:ring-destructive'
                : 'border-input focus:ring-primary'
            )}
            placeholder="{}"
          />

          {jsonParseError && (
            <div className="p-2 rounded-lg bg-destructive/10 border border-destructive/20 flex items-center gap-2 text-destructive text-sm">
              <AlertCircle className="h-4 w-4 shrink-0" />
              {jsonParseError}
            </div>
          )}
          {validationErrors.length > 0 && !jsonParseError && (
            <div className="p-2 rounded-lg bg-destructive/10 border border-destructive/20">
              <div className="flex items-center gap-2 text-destructive text-sm mb-1">
                <AlertCircle className="h-4 w-4 shrink-0" />
                Validation errors ({validationErrors.length})
              </div>
              <ul className="space-y-0.5">
                {validationErrors.map((err, i) => (
                  <li key={i} className="text-xs text-destructive/80">
                    <code className="bg-destructive/10 px-1 rounded">{err.path}</code>{' '}
                    {err.message}
                  </li>
                ))}
              </ul>
            </div>
          )}

          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button onClick={handleSave} disabled={validationErrors.length > 0}>
              Apply
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

// ============================================================================
// Plugin Picker
// ============================================================================

function PluginPicker({
  plugins,
  onSelect,
  onCancel,
}: {
  plugins: Plugin[]
  onSelect: (plugin: Plugin) => void
  onCancel: () => void
}) {
  if (plugins.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-border p-4 text-center">
        <p className="text-xs text-muted-foreground">
          No middleware plugins registered. Register plugins in the Plugin Registry first.
        </p>
        <Button variant="ghost" size="sm" className="mt-2" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    )
  }

  return (
    <div className="rounded-lg border border-border overflow-hidden">
      <div className="px-3 py-2 bg-muted/50 border-b border-border flex items-center justify-between">
        <span className="text-xs font-medium text-muted-foreground">Select a middleware</span>
        <Button variant="ghost" size="sm" className="h-6 px-2" onClick={onCancel}>
          <X className="h-3 w-3" />
        </Button>
      </div>
      <div className="max-h-40 overflow-auto divide-y divide-border">
        {plugins.map((plugin) => (
          <button
            key={`${plugin.name}-${plugin.version}`}
            onClick={() => onSelect(plugin)}
            className="w-full px-3 py-2 text-left hover:bg-muted/50 transition-colors"
          >
            <div className="flex items-center gap-2">
              <Shield className="h-3.5 w-3.5 text-blue-500 shrink-0" />
              <span className="text-sm font-medium">{plugin.name}</span>
              <Badge variant="outline" className="text-[10px]">
                v{plugin.version}
              </Badge>
            </div>
            {plugin.description && (
              <p className="text-xs text-muted-foreground mt-0.5 ml-[1.375rem]">
                {plugin.description}
              </p>
            )}
          </button>
        ))}
      </div>
    </div>
  )
}

// ============================================================================
// Middleware badge (read-only display)
// ============================================================================

function MiddlewareBadge({ binding }: { binding: MiddlewareBinding }) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-md bg-blue-500/10 px-2.5 py-1 text-xs font-medium text-blue-700 dark:text-blue-400">
      <Shield className="h-3 w-3" />
      {binding.name}
      {binding.config && Object.keys(binding.config).length > 0 && (
        <span className="text-blue-500/60 font-normal">
          ({Object.keys(binding.config).join(', ')})
        </span>
      )}
    </span>
  )
}
