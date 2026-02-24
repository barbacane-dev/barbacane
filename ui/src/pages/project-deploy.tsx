import { useState, useMemo, useEffect } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Rocket,
  Key,
  Server,
  RefreshCw,
  Copy,
  Trash2,
  Plus,
  Check,
  AlertCircle,
  Clock,
  Package,
  ChevronDown,
  ChevronRight,
} from 'lucide-react'
import {
  listProjectDataPlanes,
  listProjectApiKeys,
  createProjectApiKey,
  revokeProjectApiKey,
  disconnectProjectDataPlane,
  deployToProjectDataPlanes,
  listProjectArtifacts,
} from '@/lib/api'
import type { ApiKey, ApiKeyCreated } from '@/lib/api'
import { Button, Card, CardContent, CardHeader, CardTitle, Badge, EmptyState } from '@/components/ui'
import { cn, relativeTime, formatUptime } from '@/lib/utils'

export function ProjectDeployPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const [showCreateKeyDialog, setShowCreateKeyDialog] = useState(false)
  const [newKeyName, setNewKeyName] = useState('')
  const [createdKey, setCreatedKey] = useState<ApiKeyCreated | null>(null)
  const [copiedKey, setCopiedKey] = useState(false)
  const [showOffline, setShowOffline] = useState(false)
  const [now, setNow] = useState(() => Date.now())

  // Update clock for health indicators
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 15_000)
    return () => clearInterval(id)
  }, [])

  // Queries
  const dataPlanes = useQuery({
    queryKey: ['project-data-planes', projectId],
    queryFn: () => listProjectDataPlanes(projectId!),
    enabled: !!projectId,
    refetchInterval: 5000, // Poll every 5 seconds for status updates
  })

  const apiKeys = useQuery({
    queryKey: ['project-api-keys', projectId],
    queryFn: () => listProjectApiKeys(projectId!),
    enabled: !!projectId,
  })

  const artifacts = useQuery({
    queryKey: ['project-artifacts', projectId],
    queryFn: () => listProjectArtifacts(projectId!),
    enabled: !!projectId,
  })

  // Mutations
  const createKeyMutation = useMutation({
    mutationFn: (name: string) =>
      createProjectApiKey(projectId!, { name }),
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['project-api-keys', projectId] })
      setCreatedKey(data)
      setNewKeyName('')
    },
  })

  const revokeKeyMutation = useMutation({
    mutationFn: (keyId: string) => revokeProjectApiKey(projectId!, keyId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-api-keys', projectId] })
    },
  })

  const disconnectMutation = useMutation({
    mutationFn: (dataPlaneId: string) =>
      disconnectProjectDataPlane(projectId!, dataPlaneId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-data-planes', projectId] })
    },
  })

  const deployMutation = useMutation({
    mutationFn: (artifactId?: string) =>
      deployToProjectDataPlanes(projectId!, artifactId ? { artifact_id: artifactId } : {}),
    onSuccess: () => {
      // Refetch immediately + staggered to catch async status transitions
      queryClient.invalidateQueries({ queryKey: ['project-data-planes', projectId] })
      setTimeout(() => dataPlanes.refetch(), 1000)
      setTimeout(() => dataPlanes.refetch(), 3000)
    },
  })

  const cleanupMutation = useMutation({
    mutationFn: async (offlineIds: string[]) => {
      await Promise.all(
        offlineIds.map((id) => disconnectProjectDataPlane(projectId!, id))
      )
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project-data-planes', projectId] })
      setShowOffline(false)
    },
  })

  const handleCreateKey = () => {
    if (!newKeyName.trim()) return
    createKeyMutation.mutate(newKeyName.trim())
  }

  const handleCopyKey = async () => {
    if (createdKey) {
      await navigator.clipboard.writeText(createdKey.key)
      setCopiedKey(true)
      setTimeout(() => setCopiedKey(false), 2000)
    }
  }

  const handleCloseKeyDialog = () => {
    setShowCreateKeyDialog(false)
    setCreatedKey(null)
    setNewKeyName('')
  }

  const formatDate = (dateStr: string | null) => {
    if (!dateStr) return 'Never'
    return new Date(dateStr).toLocaleDateString('en-US', {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  const getStatusBadge = (status: string) => {
    switch (status) {
      case 'online':
        return <Badge variant="default" className="bg-green-500">Online</Badge>
      case 'deploying':
        return <Badge variant="default" className="bg-yellow-500">Deploying</Badge>
      default:
        return <Badge variant="secondary">Offline</Badge>
    }
  }

  const getHealthColor = (lastSeen: string | null, status: string) => {
    if (status === 'offline') return 'bg-gray-400'
    if (!lastSeen) return 'bg-gray-400'
    const secondsAgo = (now - new Date(lastSeen).getTime()) / 1000
    if (secondsAgo < 45) return 'bg-green-500'
    if (secondsAgo < 90) return 'bg-yellow-500 animate-pulse'
    return 'bg-red-500'
  }

  const latestArtifact = artifacts.data?.[0]
  const artifactMap = new Map(artifacts.data?.map(a => [a.id, a]) ?? [])

  const onlinePlanes = useMemo(
    () => dataPlanes.data?.filter((dp) => dp.status !== 'offline') ?? [],
    [dataPlanes.data]
  )
  const offlinePlanes = useMemo(
    () => dataPlanes.data?.filter((dp) => dp.status === 'offline') ?? [],
    [dataPlanes.data]
  )
  const onlineCount = onlinePlanes.length

  return (
    <div className="p-8 space-y-6">
      {/* Deploy section */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Rocket className="h-5 w-5" />
            Deploy to Data Planes
          </CardTitle>
        </CardHeader>
        <CardContent>
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm text-muted-foreground">
                {onlineCount} data plane{onlineCount !== 1 ? 's' : ''} connected
              </p>
              {latestArtifact && (
                <p className="text-sm text-muted-foreground mt-1">
                  Latest artifact: {latestArtifact.sha256.slice(0, 12)}...
                </p>
              )}
            </div>
            <Button
              onClick={() => deployMutation.mutate(latestArtifact?.id)}
              disabled={deployMutation.isPending || onlineCount === 0 || !latestArtifact}
            >
              {deployMutation.isPending ? (
                <>
                  <RefreshCw className="h-4 w-4 mr-2 animate-spin" />
                  Deploying...
                </>
              ) : (
                <>
                  <Rocket className="h-4 w-4 mr-2" />
                  Deploy Latest
                </>
              )}
            </Button>
          </div>
          {deployMutation.isSuccess && (
            <p className="text-sm text-green-500 mt-2">
              Deployed to {deployMutation.data.data_planes_notified} data plane(s)
            </p>
          )}
          {deployMutation.isError && (
            <p className="text-sm text-destructive mt-2">
              {deployMutation.error instanceof Error
                ? deployMutation.error.message
                : 'Deployment failed'}
            </p>
          )}
        </CardContent>
      </Card>

      {/* Connected Data Planes */}
      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle className="flex items-center gap-2">
            <Server className="h-5 w-5" />
            Connected Data Planes
          </CardTitle>
          <div className="flex items-center gap-3">
            {dataPlanes.dataUpdatedAt && (
              <span className="text-xs text-muted-foreground">
                Updated {relativeTime(new Date(dataPlanes.dataUpdatedAt).toISOString())}
              </span>
            )}
            <Button
              variant="outline"
              size="sm"
              onClick={() => dataPlanes.refetch()}
              disabled={dataPlanes.isFetching}
            >
              <RefreshCw
                className={cn('h-4 w-4 mr-2', dataPlanes.isFetching && 'animate-spin')}
              />
              Refresh
            </Button>
          </div>
        </CardHeader>
        <CardContent>
          {dataPlanes.isLoading ? (
            <div className="flex items-center justify-center p-8">
              <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          ) : dataPlanes.data?.length === 0 ? (
            <EmptyState
              icon={Server}
              title="No data planes connected"
              description="Start a data plane with --control-plane flag to connect"
              className="border-0 p-8"
            />
          ) : (
            <div className="space-y-3">
              {/* Online / deploying planes */}
              {onlinePlanes.map((dp) => {
                const deployedArtifact = dp.artifact_id ? artifactMap.get(dp.artifact_id) : null
                const uptimeSecs = typeof dp.metadata?.uptime_secs === 'number' ? dp.metadata.uptime_secs : null
                const requestsTotal = typeof dp.metadata?.requests_total === 'number' ? dp.metadata.requests_total : null
                return (
                  <div
                    key={dp.id}
                    className="flex items-center justify-between p-4 rounded-lg border border-border"
                  >
                    <div className="flex items-center gap-4">
                      <div className={cn('h-3 w-3 rounded-full', getHealthColor(dp.last_seen, dp.status))} />
                      <div>
                        <p className="font-medium">{dp.name || dp.id.slice(0, 8)}</p>
                        <div className="flex items-center gap-4 text-sm text-muted-foreground">
                          {getStatusBadge(dp.status)}
                          {dp.last_seen && (
                            <span className="flex items-center gap-1">
                              <Clock className="h-3 w-3" />
                              {relativeTime(dp.last_seen)}
                            </span>
                          )}
                          {uptimeSecs !== null && (
                            <span className="text-xs">
                              Uptime: {formatUptime(uptimeSecs)}
                            </span>
                          )}
                          {requestsTotal !== null && (
                            <span className="text-xs">
                              Requests: {requestsTotal.toLocaleString()}
                            </span>
                          )}
                        </div>
                        {dp.artifact_id && (
                          <div className="flex items-center gap-2 mt-1 text-xs text-muted-foreground">
                            <Package className="h-3 w-3" />
                            <span className="font-mono">{dp.artifact_id.slice(0, 8)}</span>
                            {deployedArtifact && (
                              <>
                                <span>v{deployedArtifact.compiler_version}</span>
                                <span>{formatDate(deployedArtifact.compiled_at)}</span>
                              </>
                            )}
                            {dp.artifact_id === latestArtifact?.id ? (
                              <Badge variant="default" className="bg-green-500/10 text-green-500 text-[10px] px-1.5 py-0">
                                latest
                              </Badge>
                            ) : latestArtifact && (
                              <Badge variant="secondary" className="text-[10px] px-1.5 py-0">
                                outdated
                              </Badge>
                            )}
                          </div>
                        )}
                        {!dp.artifact_id && (
                          <p className="mt-1 text-xs text-muted-foreground italic">
                            No artifact deployed
                          </p>
                        )}
                      </div>
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        if (confirm('Disconnect this data plane?')) {
                          disconnectMutation.mutate(dp.id)
                        }
                      }}
                      disabled={disconnectMutation.isPending}
                    >
                      <Trash2 className="h-4 w-4 text-destructive" />
                    </Button>
                  </div>
                )
              })}

              {/* Offline planes — collapsible */}
              {offlinePlanes.length > 0 && (
                <div className="pt-2">
                  <button
                    type="button"
                    className="flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground transition-colors w-full"
                    onClick={() => setShowOffline((v) => !v)}
                  >
                    {showOffline ? (
                      <ChevronDown className="h-4 w-4" />
                    ) : (
                      <ChevronRight className="h-4 w-4" />
                    )}
                    {offlinePlanes.length} offline data plane{offlinePlanes.length !== 1 ? 's' : ''}
                  </button>

                  {showOffline && (
                    <div className="mt-3 space-y-3">
                      <div className="flex justify-end">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            if (confirm(`Remove all ${offlinePlanes.length} offline data plane(s)?`)) {
                              cleanupMutation.mutate(offlinePlanes.map((dp) => dp.id))
                            }
                          }}
                          disabled={cleanupMutation.isPending}
                        >
                          {cleanupMutation.isPending ? (
                            <>
                              <RefreshCw className="h-4 w-4 mr-2 animate-spin" />
                              Cleaning up...
                            </>
                          ) : (
                            <>
                              <Trash2 className="h-4 w-4 mr-2" />
                              Clean up all
                            </>
                          )}
                        </Button>
                      </div>
                      {offlinePlanes.map((dp) => (
                        <div
                          key={dp.id}
                          className="flex items-center justify-between p-4 rounded-lg border border-border opacity-60"
                        >
                          <div className="flex items-center gap-4">
                            <div className="h-3 w-3 rounded-full bg-gray-400" />
                            <div>
                              <p className="font-medium">{dp.name || dp.id.slice(0, 8)}</p>
                              <div className="flex items-center gap-4 text-sm text-muted-foreground">
                                {getStatusBadge(dp.status)}
                                {dp.last_seen && (
                                  <span className="flex items-center gap-1">
                                    <Clock className="h-3 w-3" />
                                    {relativeTime(dp.last_seen)}
                                  </span>
                                )}
                              </div>
                            </div>
                          </div>
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => {
                              if (confirm('Remove this data plane?')) {
                                disconnectMutation.mutate(dp.id)
                              }
                            }}
                            disabled={disconnectMutation.isPending}
                          >
                            <Trash2 className="h-4 w-4 text-destructive" />
                          </Button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </CardContent>
      </Card>

      {/* API Keys */}
      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle className="flex items-center gap-2">
            <Key className="h-5 w-5" />
            API Keys
          </CardTitle>
          <Button size="sm" onClick={() => setShowCreateKeyDialog(true)}>
            <Plus className="h-4 w-4 mr-2" />
            Create Key
          </Button>
        </CardHeader>
        <CardContent>
          {apiKeys.isLoading ? (
            <div className="flex items-center justify-center p-8">
              <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
            </div>
          ) : apiKeys.data?.length === 0 ? (
            <EmptyState
              icon={Key}
              title="No API keys created"
              description="Create an API key to authenticate data planes"
              className="border-0 p-8"
            />
          ) : (
            <div className="space-y-3">
              {apiKeys.data?.map((key: ApiKey) => (
                <div
                  key={key.id}
                  className="flex items-center justify-between p-4 rounded-lg border border-border"
                >
                  <div>
                    <div className="flex items-center gap-2">
                      <p className="font-medium">{key.name}</p>
                      {key.revoked_at && (
                        <Badge variant="destructive">Revoked</Badge>
                      )}
                    </div>
                    <div className="flex items-center gap-4 text-sm text-muted-foreground mt-1">
                      <code className="bg-muted px-2 py-0.5 rounded">
                        {key.key_prefix}...
                      </code>
                      {key.last_used_at && (
                        <span>Last used: {formatDate(key.last_used_at)}</span>
                      )}
                    </div>
                  </div>
                  {!key.revoked_at && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        if (confirm(`Revoke API key "${key.name}"?`)) {
                          revokeKeyMutation.mutate(key.id)
                        }
                      }}
                      disabled={revokeKeyMutation.isPending}
                    >
                      <Trash2 className="h-4 w-4 text-destructive" />
                    </Button>
                  )}
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      {/* Create Key Dialog */}
      {showCreateKeyDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="w-full max-w-md">
            <CardContent className="p-6">
              {createdKey ? (
                <>
                  <div className="flex items-center gap-2 mb-4">
                    <Check className="h-5 w-5 text-green-500" />
                    <h2 className="text-lg font-semibold">API Key Created</h2>
                  </div>
                  <div className="bg-muted p-4 rounded-lg mb-4">
                    <div className="flex items-center justify-between gap-2">
                      <code className="text-sm break-all">{createdKey.key}</code>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={handleCopyKey}
                      >
                        {copiedKey ? (
                          <Check className="h-4 w-4 text-green-500" />
                        ) : (
                          <Copy className="h-4 w-4" />
                        )}
                      </Button>
                    </div>
                  </div>
                  <div className="flex items-start gap-2 text-sm text-amber-500 mb-4">
                    <AlertCircle className="h-4 w-4 mt-0.5" />
                    <p>
                      Copy this key now. It won't be shown again.
                    </p>
                  </div>
                  <Button className="w-full" onClick={handleCloseKeyDialog}>
                    Done
                  </Button>
                </>
              ) : (
                <>
                  <h2 className="text-lg font-semibold mb-4">Create API Key</h2>
                  <div className="space-y-4">
                    <div>
                      <label className="block text-sm font-medium mb-2">
                        Key Name
                      </label>
                      <input
                        type="text"
                        value={newKeyName}
                        onChange={(e) => setNewKeyName(e.target.value)}
                        placeholder="e.g., production-gateway"
                        className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
                        autoFocus
                      />
                    </div>
                    {createKeyMutation.isError && (
                      <p className="text-sm text-destructive">
                        {createKeyMutation.error instanceof Error
                          ? createKeyMutation.error.message
                          : 'Failed to create API key'}
                      </p>
                    )}
                    <div className="flex justify-end gap-2">
                      <Button variant="outline" onClick={handleCloseKeyDialog}>
                        Cancel
                      </Button>
                      <Button
                        onClick={handleCreateKey}
                        disabled={!newKeyName.trim() || createKeyMutation.isPending}
                      >
                        {createKeyMutation.isPending ? 'Creating...' : 'Create'}
                      </Button>
                    </div>
                  </div>
                </>
              )}
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  )
}
