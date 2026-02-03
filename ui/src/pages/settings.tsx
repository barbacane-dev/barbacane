import { useQuery } from '@tanstack/react-query'
import { RefreshCw, CheckCircle, XCircle, Server, Database, Palette } from 'lucide-react'
import { getHealth } from '@/lib/api'
import { useTheme } from '@/hooks'
import { useAuth } from '@/lib/auth'
import { Button, Card, CardContent } from '@/components/ui'
import { cn } from '@/lib/utils'

export function SettingsPage() {
  const { theme, setTheme } = useTheme()
  const { user } = useAuth()

  const healthQuery = useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
    refetchInterval: 30000, // Check every 30 seconds
  })

  const isHealthy = healthQuery.data?.status === 'healthy'

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">Settings</h1>
        <p className="text-muted-foreground">Control plane configuration</p>
      </div>

      <div className="space-y-6 max-w-2xl">
        {/* API Connection Status */}
        <Card>
          <CardContent className="p-6">
            <div className="flex items-center gap-3 mb-4">
              <Server className="h-5 w-5 text-primary" />
              <h3 className="text-lg font-medium">API Connection</h3>
            </div>

            {healthQuery.isLoading ? (
              <div className="flex items-center gap-2">
                <RefreshCw className="h-4 w-4 animate-spin text-muted-foreground" />
                <span className="text-sm text-muted-foreground">Checking connection...</span>
              </div>
            ) : healthQuery.isError ? (
              <div className="flex items-center gap-2">
                <XCircle className="h-5 w-5 text-destructive" />
                <span className="text-sm text-destructive">Connection failed</span>
              </div>
            ) : (
              <div className="space-y-3">
                <div className="flex items-center gap-2">
                  {isHealthy ? (
                    <CheckCircle className="h-5 w-5 text-green-500" />
                  ) : (
                    <XCircle className="h-5 w-5 text-destructive" />
                  )}
                  <span className="text-sm">
                    {isHealthy ? 'Connected to control plane' : 'Control plane unhealthy'}
                  </span>
                </div>
                {healthQuery.data && (
                  <p className="text-sm text-muted-foreground">
                    Control plane version: {healthQuery.data.version}
                  </p>
                )}
              </div>
            )}

            <Button
              variant="outline"
              size="sm"
              onClick={() => healthQuery.refetch()}
              disabled={healthQuery.isFetching}
              className="mt-4"
            >
              <RefreshCw
                className={cn('h-4 w-4 mr-2', healthQuery.isFetching && 'animate-spin')}
              />
              Check Connection
            </Button>
          </CardContent>
        </Card>

        {/* Theme Settings */}
        <Card>
          <CardContent className="p-6">
            <div className="flex items-center gap-3 mb-4">
              <Palette className="h-5 w-5 text-secondary" />
              <h3 className="text-lg font-medium">Appearance</h3>
            </div>

            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium mb-2">Theme</label>
                <div className="flex gap-2">
                  <Button
                    variant={theme === 'light' ? 'default' : 'outline'}
                    size="sm"
                    onClick={() => setTheme('light')}
                  >
                    Light
                  </Button>
                  <Button
                    variant={theme === 'dark' ? 'default' : 'outline'}
                    size="sm"
                    onClick={() => setTheme('dark')}
                  >
                    Dark
                  </Button>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* User Info */}
        <Card>
          <CardContent className="p-6">
            <div className="flex items-center gap-3 mb-4">
              <Database className="h-5 w-5 text-accent" />
              <h3 className="text-lg font-medium">Account</h3>
            </div>

            <div className="space-y-2 text-sm">
              <div className="flex justify-between">
                <span className="text-muted-foreground">Name</span>
                <span>{user?.name}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Email</span>
                <span>{user?.email}</span>
              </div>
            </div>

            <p className="mt-4 text-xs text-muted-foreground">
              Authentication is currently in mock mode. Configure OIDC for production use.
            </p>
          </CardContent>
        </Card>

        {/* API Endpoint */}
        <Card>
          <CardContent className="p-6">
            <div className="flex items-center gap-3 mb-4">
              <Server className="h-5 w-5 text-muted-foreground" />
              <h3 className="text-lg font-medium">API Configuration</h3>
            </div>

            <div className="space-y-2 text-sm">
              <div className="flex justify-between">
                <span className="text-muted-foreground">API Endpoint</span>
                <span className="font-mono">/api</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">Proxy Target</span>
                <span className="font-mono">http://localhost:9090</span>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
