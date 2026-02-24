export const projects = [
  {
    id: '11111111-1111-1111-1111-111111111111',
    name: 'Pet Store API',
    description: 'Sample pet store project',
    production_mode: false,
    created_at: '2026-02-20T10:00:00Z',
    updated_at: '2026-02-20T10:00:00Z',
  },
]

export const specs = [
  {
    id: '22222222-2222-2222-2222-222222222222',
    name: 'petstore.yaml',
    spec_type: 'openapi',
    spec_version: '3.0.3',
    project_id: '11111111-1111-1111-1111-111111111111',
    created_at: '2026-02-20T10:00:00Z',
    updated_at: '2026-02-20T10:00:00Z',
  },
]

export const plugins = [
  {
    name: 'rate-limit',
    version: '0.1.0',
    plugin_type: 'middleware',
    description: 'Rate limiting middleware',
    config_schema: null,
    created_at: '2026-02-20T10:00:00Z',
    updated_at: '2026-02-20T10:00:00Z',
  },
  {
    name: 'http-upstream',
    version: '0.1.0',
    plugin_type: 'dispatcher',
    description: 'HTTP reverse proxy dispatcher',
    config_schema: null,
    created_at: '2026-02-20T10:00:00Z',
    updated_at: '2026-02-20T10:00:00Z',
  },
]

export const specContent = `openapi: "3.0.3"
info:
  title: Pet Store
  version: "1.0.0"
paths:
  /pets:
    get:
      operationId: listPets
      summary: List all pets
      responses:
        "200":
          description: A list of pets
`

export const health = {
  status: 'healthy',
  version: '0.1.2',
}
