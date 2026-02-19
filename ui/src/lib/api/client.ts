import type {
  AddPluginToProjectRequest,
  ApiKey,
  ApiKeyCreated,
  Artifact,
  Compilation,
  CompileRequest,
  CreateApiKeyRequest,
  CreateProjectRequest,
  DataPlane,
  DeployRequest,
  DeployResponse,
  HealthResponse,
  InitRequest,
  InitResponse,
  PatchSpecOperationsRequest,
  Plugin,
  PluginType,
  ProblemDetails,
  Project,
  ProjectOperationsResponse,
  ProjectPluginConfig,
  Spec,
  SpecOperations,
  SpecRevision,
  SpecType,
  UpdateProjectPluginRequest,
  UpdateProjectRequest,
  UploadResponse,
} from './types'

const API_BASE = '/api'

class ApiError extends Error {
  status: number
  problem: ProblemDetails

  constructor(status: number, problem: ProblemDetails) {
    super(problem.detail ?? problem.title)
    this.name = 'ApiError'
    this.status = status
    this.problem = problem
  }
}

async function request<T>(
  path: string,
  options: RequestInit = {}
): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      'Content-Type': 'application/json',
      ...options.headers,
    },
  })

  if (!response.ok) {
    const problem = (await response.json()) as ProblemDetails
    throw new ApiError(response.status, problem)
  }

  if (response.status === 204) {
    return undefined as T
  }

  return response.json() as Promise<T>
}

// Health
export async function getHealth(): Promise<HealthResponse> {
  return request<HealthResponse>('/health')
}

// Specs
export async function listSpecs(params?: {
  type?: SpecType
  name?: string
}): Promise<Spec[]> {
  const searchParams = new URLSearchParams()
  if (params?.type) searchParams.set('type', params.type)
  if (params?.name) searchParams.set('name', params.name)
  const query = searchParams.toString()
  return request<Spec[]>(`/specs${query ? `?${query}` : ''}`)
}

export async function getSpec(id: string): Promise<Spec> {
  return request<Spec>(`/specs/${id}`)
}

export async function deleteSpec(id: string): Promise<void> {
  return request<void>(`/specs/${id}`, { method: 'DELETE' })
}

export async function uploadSpec(file: File): Promise<UploadResponse> {
  const formData = new FormData()
  formData.append('file', file)

  console.log('Uploading to:', `${API_BASE}/specs`)
  const response = await fetch(`${API_BASE}/specs`, {
    method: 'POST',
    body: formData,
  })

  console.log('Response status:', response.status, response.statusText)

  if (!response.ok) {
    const text = await response.text()
    console.error('Error response:', text)
    try {
      const problem = JSON.parse(text) as ProblemDetails
      throw new ApiError(response.status, problem)
    } catch {
      throw new Error(`Upload failed: ${response.status} ${response.statusText} - ${text}`)
    }
  }

  return response.json() as Promise<UploadResponse>
}

export async function getSpecHistory(id: string): Promise<SpecRevision[]> {
  return request<SpecRevision[]>(`/specs/${id}/history`)
}

export async function downloadSpecContent(
  id: string,
  revision?: number
): Promise<string> {
  const query = revision ? `?revision=${revision}` : ''
  const response = await fetch(`${API_BASE}/specs/${id}/content${query}`)
  if (!response.ok) {
    const problem = (await response.json()) as ProblemDetails
    throw new ApiError(response.status, problem)
  }
  return response.text()
}

// Compilations
export async function startCompilation(
  specId: string,
  options?: CompileRequest
): Promise<Compilation> {
  return request<Compilation>(`/specs/${specId}/compile`, {
    method: 'POST',
    body: JSON.stringify(options ?? {}),
  })
}

export async function listSpecCompilations(specId: string): Promise<Compilation[]> {
  return request<Compilation[]>(`/specs/${specId}/compilations`)
}

export async function getCompilation(id: string): Promise<Compilation> {
  return request<Compilation>(`/compilations/${id}`)
}

export async function deleteCompilation(id: string): Promise<void> {
  return request<void>(`/compilations/${id}`, { method: 'DELETE' })
}

// Plugins
export async function listPlugins(params?: {
  type?: PluginType
  name?: string
}): Promise<Plugin[]> {
  const searchParams = new URLSearchParams()
  if (params?.type) searchParams.set('type', params.type)
  if (params?.name) searchParams.set('name', params.name)
  const query = searchParams.toString()
  return request<Plugin[]>(`/plugins${query ? `?${query}` : ''}`)
}

export async function listPluginVersions(name: string): Promise<Plugin[]> {
  return request<Plugin[]>(`/plugins/${name}`)
}

export async function getPlugin(name: string, version: string): Promise<Plugin> {
  return request<Plugin>(`/plugins/${name}/${version}`)
}

export async function deletePlugin(name: string, version: string): Promise<void> {
  return request<void>(`/plugins/${name}/${version}`, { method: 'DELETE' })
}

export async function registerPlugin(data: {
  name: string
  version: string
  type: PluginType
  description?: string
  capabilities?: string[]
  config_schema?: Record<string, unknown>
  file: File
}): Promise<Plugin> {
  const formData = new FormData()
  formData.append('name', data.name)
  formData.append('version', data.version)
  formData.append('type', data.type)
  if (data.description) formData.append('description', data.description)
  if (data.capabilities) {
    formData.append('capabilities', JSON.stringify(data.capabilities))
  }
  if (data.config_schema) {
    formData.append('config_schema', JSON.stringify(data.config_schema))
  }
  formData.append('file', data.file)

  const response = await fetch(`${API_BASE}/plugins`, {
    method: 'POST',
    body: formData,
  })

  if (!response.ok) {
    const problem = (await response.json()) as ProblemDetails
    throw new ApiError(response.status, problem)
  }

  return response.json() as Promise<Plugin>
}

// Artifacts
export async function listArtifacts(): Promise<Artifact[]> {
  return request<Artifact[]>('/artifacts')
}

export async function getArtifact(id: string): Promise<Artifact> {
  return request<Artifact>(`/artifacts/${id}`)
}

export async function deleteArtifact(id: string): Promise<void> {
  return request<void>(`/artifacts/${id}`, { method: 'DELETE' })
}

export async function downloadArtifact(id: string): Promise<Blob> {
  const response = await fetch(`${API_BASE}/artifacts/${id}/download`)
  if (!response.ok) {
    const problem = (await response.json()) as ProblemDetails
    throw new ApiError(response.status, problem)
  }
  return response.blob()
}

// Init
export async function initProject(data: InitRequest): Promise<InitResponse> {
  return request<InitResponse>('/init', {
    method: 'POST',
    body: JSON.stringify(data),
  })
}

// Projects
export async function listProjects(): Promise<Project[]> {
  return request<Project[]>('/projects')
}

export async function createProject(data: CreateProjectRequest): Promise<Project> {
  return request<Project>('/projects', {
    method: 'POST',
    body: JSON.stringify(data),
  })
}

export async function getProject(id: string): Promise<Project> {
  return request<Project>(`/projects/${id}`)
}

export async function updateProject(
  id: string,
  data: UpdateProjectRequest
): Promise<Project> {
  return request<Project>(`/projects/${id}`, {
    method: 'PUT',
    body: JSON.stringify(data),
  })
}

export async function deleteProject(id: string): Promise<void> {
  return request<void>(`/projects/${id}`, { method: 'DELETE' })
}

// Project specs
export async function listProjectSpecs(projectId: string): Promise<Spec[]> {
  return request<Spec[]>(`/projects/${projectId}/specs`)
}

export async function uploadSpecToProject(
  projectId: string,
  file: File
): Promise<UploadResponse> {
  const formData = new FormData()
  formData.append('file', file)

  const response = await fetch(`${API_BASE}/projects/${projectId}/specs`, {
    method: 'POST',
    body: formData,
  })

  if (!response.ok) {
    const problem = (await response.json()) as ProblemDetails
    throw new ApiError(response.status, problem)
  }

  return response.json() as Promise<UploadResponse>
}

// Project plugins
export async function listProjectPlugins(
  projectId: string
): Promise<ProjectPluginConfig[]> {
  return request<ProjectPluginConfig[]>(`/projects/${projectId}/plugins`)
}

export async function addPluginToProject(
  projectId: string,
  data: AddPluginToProjectRequest
): Promise<ProjectPluginConfig> {
  return request<ProjectPluginConfig>(`/projects/${projectId}/plugins`, {
    method: 'POST',
    body: JSON.stringify(data),
  })
}

export async function updateProjectPlugin(
  projectId: string,
  pluginName: string,
  data: UpdateProjectPluginRequest
): Promise<ProjectPluginConfig> {
  return request<ProjectPluginConfig>(
    `/projects/${projectId}/plugins/${pluginName}`,
    {
      method: 'PUT',
      body: JSON.stringify(data),
    }
  )
}

export async function removePluginFromProject(
  projectId: string,
  pluginName: string
): Promise<void> {
  return request<void>(`/projects/${projectId}/plugins/${pluginName}`, {
    method: 'DELETE',
  })
}

// Project compilations and artifacts
export async function listProjectCompilations(
  projectId: string
): Promise<Compilation[]> {
  return request<Compilation[]>(`/projects/${projectId}/compilations`)
}

export async function listProjectArtifacts(projectId: string): Promise<Artifact[]> {
  return request<Artifact[]>(`/projects/${projectId}/artifacts`)
}

// Project API keys
export async function listProjectApiKeys(projectId: string): Promise<ApiKey[]> {
  return request<ApiKey[]>(`/projects/${projectId}/api-keys`)
}

export async function createProjectApiKey(
  projectId: string,
  data: CreateApiKeyRequest
): Promise<ApiKeyCreated> {
  return request<ApiKeyCreated>(`/projects/${projectId}/api-keys`, {
    method: 'POST',
    body: JSON.stringify(data),
  })
}

export async function revokeProjectApiKey(
  projectId: string,
  keyId: string
): Promise<void> {
  return request<void>(`/projects/${projectId}/api-keys/${keyId}`, {
    method: 'DELETE',
  })
}

// Project data planes
export async function listProjectDataPlanes(projectId: string): Promise<DataPlane[]> {
  return request<DataPlane[]>(`/projects/${projectId}/data-planes`)
}

export async function getProjectDataPlane(
  projectId: string,
  dataPlaneId: string
): Promise<DataPlane> {
  return request<DataPlane>(`/projects/${projectId}/data-planes/${dataPlaneId}`)
}

export async function disconnectProjectDataPlane(
  projectId: string,
  dataPlaneId: string
): Promise<void> {
  return request<void>(`/projects/${projectId}/data-planes/${dataPlaneId}`, {
    method: 'DELETE',
  })
}

export async function deployToProjectDataPlanes(
  projectId: string,
  data?: DeployRequest
): Promise<DeployResponse> {
  return request<DeployResponse>(`/projects/${projectId}/deploy`, {
    method: 'POST',
    body: JSON.stringify(data ?? {}),
  })
}

// Project operations (plugin bindings)
export async function getProjectOperations(
  projectId: string
): Promise<ProjectOperationsResponse> {
  return request<ProjectOperationsResponse>(`/projects/${projectId}/operations`)
}

export async function patchSpecOperations(
  specId: string,
  data: PatchSpecOperationsRequest
): Promise<SpecOperations> {
  return request<SpecOperations>(`/specs/${specId}/operations`, {
    method: 'PATCH',
    body: JSON.stringify(data),
  })
}

export { ApiError }
