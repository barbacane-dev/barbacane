import { Package } from 'lucide-react'

export function ArtifactsPage() {
  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">Artifacts</h1>
        <p className="text-muted-foreground">
          Compiled gateway artifacts ready for deployment
        </p>
      </div>
      <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
        <div className="text-center">
          <Package className="mx-auto h-12 w-12 text-muted-foreground" />
          <h3 className="mt-4 text-lg font-medium">No artifacts compiled</h3>
          <p className="mt-2 text-sm text-muted-foreground">
            Compile a spec to create a deployable artifact
          </p>
        </div>
      </div>
    </div>
  )
}
