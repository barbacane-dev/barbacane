import { Puzzle } from 'lucide-react'

export function PluginsPage() {
  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">Plugins</h1>
        <p className="text-muted-foreground">
          Manage middleware and dispatcher plugins
        </p>
      </div>
      <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
        <div className="text-center">
          <Puzzle className="mx-auto h-12 w-12 text-muted-foreground" />
          <h3 className="mt-4 text-lg font-medium">No plugins registered</h3>
          <p className="mt-2 text-sm text-muted-foreground">
            Register a WASM plugin to extend gateway functionality
          </p>
        </div>
      </div>
    </div>
  )
}
