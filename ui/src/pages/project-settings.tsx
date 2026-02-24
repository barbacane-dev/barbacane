import { useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Settings, Trash2, Save } from 'lucide-react'
import { getProject, updateProject, deleteProject } from '@/lib/api'
import { Button, Card, CardContent } from '@/components/ui'
import { useConfirm } from '@/hooks'

export function ProjectSettingsPage() {
  const { id: projectId } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { confirm, dialog } = useConfirm()

  const projectQuery = useQuery({
    queryKey: ['project', projectId],
    queryFn: () => getProject(projectId!),
    enabled: !!projectId,
  })

  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [productionMode, setProductionMode] = useState(true)
  const [hasChanges, setHasChanges] = useState(false)

  // Initialize form when project data loads
  if (projectQuery.data && !hasChanges) {
    if (name !== projectQuery.data.name) {
      setName(projectQuery.data.name)
      setDescription(projectQuery.data.description || '')
      setProductionMode(projectQuery.data.production_mode)
    }
  }

  const updateMutation = useMutation({
    mutationFn: () =>
      updateProject(projectId!, {
        name: name !== projectQuery.data?.name ? name : undefined,
        description: description !== (projectQuery.data?.description || '') ? description : undefined,
        production_mode: productionMode !== projectQuery.data?.production_mode ? productionMode : undefined,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['project', projectId] })
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      setHasChanges(false)
    },
  })

  const deleteMutation = useMutation({
    mutationFn: () => deleteProject(projectId!),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['projects'] })
      navigate('/projects')
    },
  })

  const handleFieldChange = (field: 'name' | 'description' | 'production', value: string | boolean) => {
    setHasChanges(true)
    if (field === 'name') setName(value as string)
    else if (field === 'description') setDescription(value as string)
    else setProductionMode(value as boolean)
  }

  if (projectQuery.isLoading) {
    return (
      <div className="p-8">
        <p className="text-muted-foreground">Loading...</p>
      </div>
    )
  }

  if (projectQuery.isError || !projectQuery.data) {
    return (
      <div className="p-8">
        <p className="text-destructive">Failed to load project settings</p>
      </div>
    )
  }

  return (
    <div className="p-8 max-w-2xl">
      <div className="mb-6">
        <h2 className="text-lg font-semibold">Project Settings</h2>
        <p className="text-sm text-muted-foreground">
          Configure project details and preferences
        </p>
      </div>

      <div className="space-y-6">
        {/* General Settings */}
        <Card>
          <CardContent className="p-6">
            <div className="flex items-center gap-3 mb-4">
              <Settings className="h-5 w-5 text-primary" />
              <h3 className="text-md font-medium">General</h3>
            </div>

            <div className="space-y-4">
              <div>
                <label className="block text-sm font-medium mb-2">
                  Project Name
                </label>
                <input
                  type="text"
                  value={name}
                  onChange={(e) => handleFieldChange('name', e.target.value)}
                  className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-2">
                  Description
                </label>
                <textarea
                  value={description}
                  onChange={(e) => handleFieldChange('description', e.target.value)}
                  rows={3}
                  className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
                  placeholder="A brief description of this project"
                />
              </div>

              <div className="flex items-center justify-between">
                <div>
                  <label className="text-sm font-medium">Production Mode</label>
                  <p className="text-xs text-muted-foreground">
                    Enable production optimizations for compiled artifacts
                  </p>
                </div>
                <button
                  type="button"
                  role="switch"
                  aria-checked={productionMode}
                  onClick={() => handleFieldChange('production', !productionMode)}
                  className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
                    productionMode ? 'bg-primary' : 'bg-muted'
                  }`}
                >
                  <span
                    className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                      productionMode ? 'translate-x-6' : 'translate-x-1'
                    }`}
                  />
                </button>
              </div>
            </div>

            {updateMutation.isError && (
              <p className="mt-4 text-sm text-destructive">
                {updateMutation.error instanceof Error
                  ? updateMutation.error.message
                  : 'Failed to update project'}
              </p>
            )}

            <div className="mt-6 flex justify-end">
              <Button
                onClick={() => updateMutation.mutate()}
                disabled={!hasChanges || updateMutation.isPending}
              >
                <Save className="h-4 w-4 mr-2" />
                {updateMutation.isPending ? 'Saving...' : 'Save Changes'}
              </Button>
            </div>
          </CardContent>
        </Card>

        {/* Danger Zone */}
        <Card className="border-destructive/50">
          <CardContent className="p-6">
            <div className="flex items-center gap-3 mb-4">
              <Trash2 className="h-5 w-5 text-destructive" />
              <h3 className="text-md font-medium text-destructive">Danger Zone</h3>
            </div>

            <p className="text-sm text-muted-foreground mb-4">
              Deleting a project will permanently remove all specs, plugin configurations,
              and build history associated with it. This action cannot be undone.
            </p>

            <Button
              variant="destructive"
              onClick={async () => {
                if (
                  await confirm({
                    title: 'Delete project',
                    description: `Are you sure you want to delete "${projectQuery.data.name}"? This cannot be undone.`,
                  })
                ) {
                  deleteMutation.mutate()
                }
              }}
              disabled={deleteMutation.isPending}
            >
              <Trash2 className="h-4 w-4 mr-2" />
              {deleteMutation.isPending ? 'Deleting...' : 'Delete Project'}
            </Button>

            {deleteMutation.isError && (
              <p className="mt-4 text-sm text-destructive">
                {deleteMutation.error instanceof Error
                  ? deleteMutation.error.message
                  : 'Failed to delete project'}
              </p>
            )}
          </CardContent>
        </Card>
      </div>
      {dialog}
    </div>
  )
}
