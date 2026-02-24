import { useQuery } from '@tanstack/react-query'
import { Activity, RefreshCw, CheckCircle, XCircle, Clock, Loader2 } from 'lucide-react'
import { listSpecs, listSpecCompilations } from '@/lib/api'
import type { Compilation } from '@/lib/api'
import { Button, Card, CardContent, Badge, EmptyState, Breadcrumb } from '@/components/ui'
import { cn } from '@/lib/utils'

interface CompilationWithSpec extends Compilation {
  spec_name: string
}

export function ActivityPage() {
  // First fetch all specs
  const specsQuery = useQuery({
    queryKey: ['specs'],
    queryFn: () => listSpecs(),
  })

  // Then fetch compilations for each spec
  const compilationsQuery = useQuery({
    queryKey: ['all-compilations', specsQuery.data?.map((s) => s.id)],
    queryFn: async () => {
      const specs = specsQuery.data ?? []
      const allCompilations: CompilationWithSpec[] = []

      for (const spec of specs) {
        try {
          const compilations = await listSpecCompilations(spec.id)
          for (const comp of compilations) {
            allCompilations.push({ ...comp, spec_name: spec.name })
          }
        } catch {
          // Ignore errors for individual specs
        }
      }

      // Sort by started_at descending (most recent first)
      return allCompilations.sort(
        (a, b) => new Date(b.started_at).getTime() - new Date(a.started_at).getTime()
      )
    },
    enabled: !!specsQuery.data,
  })

  const formatDate = (dateStr: string) => {
    return new Date(dateStr).toLocaleDateString('en-US', {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    })
  }

  const getStatusIcon = (status: Compilation['status']) => {
    switch (status) {
      case 'succeeded':
        return <CheckCircle className="h-5 w-5 text-green-500" />
      case 'failed':
        return <XCircle className="h-5 w-5 text-destructive" />
      case 'compiling':
        return <Loader2 className="h-5 w-5 text-primary animate-spin" />
      case 'pending':
        return <Clock className="h-5 w-5 text-muted-foreground" />
    }
  }

  const getStatusBadge = (status: Compilation['status']) => {
    switch (status) {
      case 'succeeded':
        return <Badge className="bg-green-500/10 text-green-500">Succeeded</Badge>
      case 'failed':
        return <Badge variant="destructive">Failed</Badge>
      case 'compiling':
        return <Badge variant="default">Compiling</Badge>
      case 'pending':
        return <Badge variant="secondary">Pending</Badge>
    }
  }

  const isLoading = specsQuery.isLoading || compilationsQuery.isLoading
  const compilations = compilationsQuery.data ?? []

  return (
    <div className="p-8">
      <Breadcrumb
        items={[
          { label: 'Dashboard', href: '/' },
          { label: 'Activity' },
        ]}
        className="mb-4"
      />
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Activity</h1>
          <p className="text-muted-foreground">Compilation jobs and recent activity</p>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            specsQuery.refetch()
            compilationsQuery.refetch()
          }}
          disabled={isLoading}
        >
          <RefreshCw className={cn('h-4 w-4 mr-2', isLoading && 'animate-spin')} />
          Refresh
        </Button>
      </div>

      {isLoading ? (
        <div className="flex items-center justify-center p-12">
          <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
        </div>
      ) : specsQuery.isError || compilationsQuery.isError ? (
        <div className="rounded-lg border border-destructive bg-destructive/10 p-8 text-center">
          <p className="text-destructive">Failed to load activity</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              specsQuery.refetch()
              compilationsQuery.refetch()
            }}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      ) : compilations.length === 0 ? (
        <EmptyState
          icon={Activity}
          title="No recent activity"
          description="Compilation jobs will appear here"
        />
      ) : (
        <div className="space-y-4">
          {compilations.map((compilation) => (
            <Card key={compilation.id}>
              <CardContent className="p-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-4">
                    {getStatusIcon(compilation.status)}
                    <div>
                      <div className="flex items-center gap-2">
                        <h3 className="font-medium">{compilation.spec_name}</h3>
                        {getStatusBadge(compilation.status)}
                        {compilation.production && (
                          <Badge variant="outline">Production</Badge>
                        )}
                      </div>
                      <div className="mt-1 flex items-center gap-4 text-sm text-muted-foreground">
                        <span>Started {formatDate(compilation.started_at)}</span>
                        {compilation.completed_at && (
                          <span>Completed {formatDate(compilation.completed_at)}</span>
                        )}
                      </div>
                      {compilation.errors && compilation.errors.length > 0 && (
                        <div className="mt-2">
                          {compilation.errors.map((err, i) => (
                            <p
                              key={i}
                              className="text-sm text-destructive font-mono"
                            >
                              [{err.code}] {err.message}
                            </p>
                          ))}
                        </div>
                      )}
                      {compilation.artifact_id && (
                        <p className="mt-1 text-xs text-muted-foreground font-mono">
                          Artifact: {compilation.artifact_id.slice(0, 8)}...
                        </p>
                      )}
                    </div>
                  </div>
                  <div className="text-right text-xs text-muted-foreground font-mono">
                    {compilation.id.slice(0, 8)}
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
