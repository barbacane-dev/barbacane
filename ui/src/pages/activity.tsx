import { Activity } from 'lucide-react'

export function ActivityPage() {
  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">Activity</h1>
        <p className="text-muted-foreground">
          Compilation jobs and recent activity
        </p>
      </div>
      <div className="flex items-center justify-center rounded-lg border border-dashed border-border p-12">
        <div className="text-center">
          <Activity className="mx-auto h-12 w-12 text-muted-foreground" />
          <h3 className="mt-4 text-lg font-medium">No recent activity</h3>
          <p className="mt-2 text-sm text-muted-foreground">
            Compilation jobs will appear here
          </p>
        </div>
      </div>
    </div>
  )
}
