export function SettingsPage() {
  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">Settings</h1>
        <p className="text-muted-foreground">
          Control plane configuration
        </p>
      </div>
      <div className="space-y-6">
        <div className="rounded-lg border border-border p-6">
          <h3 className="text-lg font-medium">API Connection</h3>
          <p className="mt-1 text-sm text-muted-foreground">
            Connected to control plane API
          </p>
          <div className="mt-4 flex items-center gap-2">
            <span className="h-2 w-2 rounded-full bg-green-500" />
            <span className="text-sm">Healthy</span>
          </div>
        </div>
      </div>
    </div>
  )
}
