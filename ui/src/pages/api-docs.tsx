import { useEffect } from 'react'

export function ApiDocsPage() {
  useEffect(() => {
    // Redirect to the backend-served Scalar API documentation
    window.location.href = '/api/docs'
  }, [])

  return (
    <div className="flex items-center justify-center h-full">
      <p className="text-muted-foreground">Redirecting to API documentation...</p>
    </div>
  )
}
