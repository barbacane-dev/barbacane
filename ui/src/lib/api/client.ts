import type {
  Artifact,
  Compilation,
  CompileRequest,
  HealthResponse,
  InitRequest,
  InitResponse,
  Plugin,
  PluginType,
  ProblemDetails,
  Spec,
  SpecRevision,
  SpecType,
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

  const response = await fetch(`${API_BASE}/specs`, {
    method: 'POST',
    body: formData,
  })

  if (!response.ok) {
    const problem = (await response.json()) as ProblemDetails
    throw new ApiError(response.status, problem)
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

export { ApiError }
