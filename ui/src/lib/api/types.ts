// Types generated from control plane OpenAPI spec

export type SpecType = 'openapi' | 'asyncapi'
export type PluginType = 'middleware' | 'dispatcher'
export type CompilationStatus = 'pending' | 'compiling' | 'succeeded' | 'failed'

export interface HealthResponse {
  status: 'healthy'
  version: string
}

// Projects
export interface Project {
  id: string
  name: string
  description: string | null
  production_mode: boolean
  created_at: string
  updated_at: string
}

export interface CreateProjectRequest {
  name: string
  description?: string
  production_mode?: boolean
}

export interface UpdateProjectRequest {
  name?: string
  description?: string
  production_mode?: boolean
}

// Project plugin configuration
export interface ProjectPluginConfig {
  id: string
  project_id: string
  plugin_name: string
  plugin_version: string
  enabled: boolean
  priority: number
  config: Record<string, unknown>
  created_at: string
  updated_at: string
}

export interface AddPluginToProjectRequest {
  plugin_name: string
  plugin_version: string
  enabled?: boolean
  priority?: number
  config?: Record<string, unknown>
}

export interface UpdateProjectPluginRequest {
  plugin_version?: string
  enabled?: boolean
  priority?: number
  config?: Record<string, unknown>
}

export interface Spec {
  id: string
  project_id: string
  name: string
  current_sha256: string
  spec_type: SpecType
  spec_version: string
  created_at: string
  updated_at: string
}

export interface SpecRevision {
  revision: number
  sha256: string
  filename: string
  created_at: string
}

export interface UploadResponse {
  id: string
  name: string
  revision: number
  sha256: string
}

export interface Plugin {
  name: string
  version: string
  plugin_type: PluginType
  description: string | null
  capabilities: string[]
  config_schema: Record<string, unknown>
  sha256: string
  registered_at: string
}

export interface Artifact {
  id: string
  project_id: string | null
  manifest: Record<string, unknown>
  sha256: string
  size_bytes: number
  compiler_version: string
  compiled_at: string
}

export interface CompileRequest {
  production?: boolean
  additional_specs?: string[]
}

export interface CompilationError {
  code: string
  message: string
}

export interface Compilation {
  id: string
  spec_id: string | null
  project_id: string | null
  status: CompilationStatus
  production: boolean
  additional_specs?: string[]
  artifact_id: string | null
  errors?: CompilationError[]
  warnings?: CompilationError[]
  started_at: string
  completed_at: string | null
}

export interface ProblemDetails {
  type: string
  title: string
  status: number
  detail?: string
  instance?: string
  errors?: ValidationIssue[]
}

export interface ValidationIssue {
  code: string
  message: string
  location?: string
}

// Init types
export type InitTemplate = 'basic' | 'minimal'

export interface InitRequest {
  name: string
  template?: InitTemplate
  description?: string
  version?: string
}

export interface ProjectFile {
  path: string
  content: string
}

export interface InitResponse {
  files: ProjectFile[]
  next_steps: string[]
}

// Data plane types
export type DataPlaneStatus = 'online' | 'offline' | 'deploying'

export interface DataPlane {
  id: string
  project_id: string
  name: string | null
  artifact_id: string | null
  status: DataPlaneStatus
  last_seen: string | null
  connected_at: string | null
  metadata: Record<string, unknown>
  created_at: string
}

// API key types
export interface ApiKey {
  id: string
  project_id: string
  name: string
  key_prefix: string
  scopes: string[]
  expires_at: string | null
  last_used_at: string | null
  created_at: string
  revoked_at: string | null
}

export interface ApiKeyCreated {
  id: string
  name: string
  key: string
  key_prefix: string
  scopes: string[]
  expires_at: string | null
  created_at: string
}

export interface CreateApiKeyRequest {
  name: string
  scopes?: string[]
  expires_at?: string
}

export interface DeployRequest {
  artifact_id?: string
}

export interface DeployResponse {
  artifact_id: string
  data_planes_notified: number
}
