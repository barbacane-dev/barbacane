// Types generated from control plane OpenAPI spec

export type SpecType = 'openapi' | 'asyncapi'
export type PluginType = 'middleware' | 'dispatcher'
export type CompilationStatus = 'pending' | 'compiling' | 'succeeded' | 'failed'

export interface HealthResponse {
  status: 'healthy'
  version: string
}

export interface Spec {
  id: string
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
  spec_id: string
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
