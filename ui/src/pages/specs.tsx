import { FileCode } from 'lucide-react'

export function SpecsPage() {
  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">API Specs</h1>
        <p className="text-muted-foreground">
          Manage your OpenAPI and AsyncAPI specifications
        </p>
      </div>
      <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
        <div className="text-center">
          <FileCode className="mx-auto h-12 w-12 text-muted-foreground" />
          <h3 className="mt-4 text-lg font-medium">No specs yet</h3>
          <p className="mt-2 text-sm text-muted-foreground">
            Upload an OpenAPI or AsyncAPI spec to get started
          </p>
        </div>
      </div>
    </div>
  )
}
