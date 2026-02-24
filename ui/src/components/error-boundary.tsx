import { useRouteError, isRouteErrorResponse, useNavigate } from 'react-router-dom'
import { AlertTriangle, Home, RotateCcw, ArrowLeft } from 'lucide-react'
import { Button } from '@/components/ui'

export function RouteErrorBoundary() {
  const error = useRouteError()
  const navigate = useNavigate()

  let title = 'Something went wrong'
  let message = 'An unexpected error occurred.'

  if (isRouteErrorResponse(error)) {
    if (error.status === 404) {
      title = 'Page not found'
      message = 'The page you are looking for does not exist.'
    } else {
      title = `Error ${error.status}`
      message = error.statusText || message
    }
  } else if (error instanceof Error) {
    message = error.message
  }

  return (
    <div className="flex min-h-[50vh] items-center justify-center p-8">
      <div className="max-w-md text-center">
        <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full bg-destructive/10">
          <AlertTriangle className="h-6 w-6 text-destructive" />
        </div>
        <h1 className="text-xl font-semibold">{title}</h1>
        <p className="mt-2 text-sm text-muted-foreground">{message}</p>

        {import.meta.env.DEV && error instanceof Error && error.stack && (
          <pre className="mt-4 max-h-48 overflow-auto rounded-lg border border-border bg-muted p-3 text-left text-xs font-mono text-muted-foreground">
            {error.stack}
          </pre>
        )}

        <div className="mt-6 flex justify-center gap-3">
          <Button variant="outline" size="sm" onClick={() => navigate(-1)}>
            <ArrowLeft className="h-4 w-4 mr-2" />
            Go back
          </Button>
          <Button variant="outline" size="sm" onClick={() => window.location.reload()}>
            <RotateCcw className="h-4 w-4 mr-2" />
            Try again
          </Button>
          <Button size="sm" onClick={() => navigate('/')}>
            <Home className="h-4 w-4 mr-2" />
            Dashboard
          </Button>
        </div>
      </div>
    </div>
  )
}
