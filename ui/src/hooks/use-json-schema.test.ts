import { describe, it, expect } from 'vitest'
import { validateJsonSchema, generateSkeletonFromSchema, generateSkeletonWithComments } from './use-json-schema'

describe('validateJsonSchema', () => {
  describe('with empty or no schema', () => {
    it('returns valid for any data when schema is empty', () => {
      const result = validateJsonSchema({ foo: 'bar' }, {})
      expect(result.valid).toBe(true)
      expect(result.errors).toHaveLength(0)
    })

    it('returns valid for any data when schema is null-like', () => {
      const result = validateJsonSchema({ foo: 'bar' }, null as unknown as Record<string, unknown>)
      expect(result.valid).toBe(true)
      expect(result.errors).toHaveLength(0)
    })
  })

  describe('with required properties', () => {
    const schema = {
      type: 'object',
      required: ['url'],
      properties: {
        url: { type: 'string' },
      },
    }

    it('validates successfully when required property is present', () => {
      const result = validateJsonSchema({ url: 'https://example.com' }, schema)
      expect(result.valid).toBe(true)
      expect(result.errors).toHaveLength(0)
    })

    it('fails when required property is missing', () => {
      const result = validateJsonSchema({}, schema)
      expect(result.valid).toBe(false)
      expect(result.errors).toHaveLength(1)
      expect(result.errors[0].path).toContain('url')
      expect(result.errors[0].message).toBe('Required property is missing')
      expect(result.errors[0].keyword).toBe('required')
    })
  })

  describe('with type validation', () => {
    const schema = {
      type: 'object',
      properties: {
        timeout: { type: 'number' },
        enabled: { type: 'boolean' },
      },
    }

    it('validates correct types', () => {
      const result = validateJsonSchema({ timeout: 30, enabled: true }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on wrong type for number', () => {
      const result = validateJsonSchema({ timeout: 'thirty' }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toBe('Expected number')
    })

    it('fails on wrong type for boolean', () => {
      const result = validateJsonSchema({ enabled: 'yes' }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toBe('Expected boolean')
    })
  })

  describe('with format validation', () => {
    const schema = {
      type: 'object',
      properties: {
        url: { type: 'string', format: 'uri' },
        email: { type: 'string', format: 'email' },
      },
    }

    it('validates correct URI format', () => {
      const result = validateJsonSchema({ url: 'https://api.example.com/v1' }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on invalid URI format', () => {
      const result = validateJsonSchema({ url: 'not-a-url' }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('format')
    })

    it('validates correct email format', () => {
      const result = validateJsonSchema({ email: 'user@example.com' }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on invalid email format', () => {
      const result = validateJsonSchema({ email: 'not-an-email' }, schema)
      expect(result.valid).toBe(false)
    })
  })

  describe('with enum validation', () => {
    const schema = {
      type: 'object',
      properties: {
        method: { type: 'string', enum: ['GET', 'POST', 'PUT', 'DELETE'] },
      },
    }

    it('validates value in enum', () => {
      const result = validateJsonSchema({ method: 'POST' }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on value not in enum', () => {
      const result = validateJsonSchema({ method: 'PATCH' }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('Must be one of')
    })
  })

  describe('with number constraints', () => {
    const schema = {
      type: 'object',
      properties: {
        timeout: { type: 'number', minimum: 0, maximum: 300 },
        retries: { type: 'integer', minimum: 1 },
      },
    }

    it('validates number within range', () => {
      const result = validateJsonSchema({ timeout: 30, retries: 3 }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on number below minimum', () => {
      const result = validateJsonSchema({ timeout: -5 }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('>= 0')
    })

    it('fails on number above maximum', () => {
      const result = validateJsonSchema({ timeout: 500 }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('<= 300')
    })
  })

  describe('with string constraints', () => {
    const schema = {
      type: 'object',
      properties: {
        apiKey: { type: 'string', minLength: 10, maxLength: 100 },
      },
    }

    it('validates string within length bounds', () => {
      const result = validateJsonSchema({ apiKey: 'abc123xyz789' }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on string too short', () => {
      const result = validateJsonSchema({ apiKey: 'short' }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('at least 10')
    })

    it('fails on string too long', () => {
      const result = validateJsonSchema({ apiKey: 'x'.repeat(150) }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('at most 100')
    })
  })

  describe('with additionalProperties', () => {
    const schema = {
      type: 'object',
      properties: {
        url: { type: 'string' },
      },
      additionalProperties: false,
    }

    it('validates object with only known properties', () => {
      const result = validateJsonSchema({ url: 'https://example.com' }, schema)
      expect(result.valid).toBe(true)
    })

    it('fails on unknown property when additionalProperties is false', () => {
      const result = validateJsonSchema({ url: 'https://example.com', extra: 'field' }, schema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('Unknown property')
      expect(result.errors[0].message).toContain('extra')
    })
  })

  describe('with complex schema (http-upstream example)', () => {
    const httpUpstreamSchema = {
      type: 'object',
      required: ['url'],
      properties: {
        url: {
          type: 'string',
          description: 'Base URL of the upstream',
          format: 'uri',
        },
        path: {
          type: 'string',
          description: 'Path template for the upstream request',
        },
        timeout: {
          type: 'number',
          description: 'Request timeout in seconds',
          default: 30,
          minimum: 0,
        },
      },
      additionalProperties: false,
    }

    it('validates a correct http-upstream config', () => {
      const config = {
        url: 'https://api.example.com',
        path: '/v1/users/{userId}',
        timeout: 60,
      }
      const result = validateJsonSchema(config, httpUpstreamSchema)
      expect(result.valid).toBe(true)
    })

    it('validates with only required fields', () => {
      const config = {
        url: 'https://api.example.com',
      }
      const result = validateJsonSchema(config, httpUpstreamSchema)
      expect(result.valid).toBe(true)
    })

    it('fails when url is missing', () => {
      const config = {
        timeout: 30,
      }
      const result = validateJsonSchema(config, httpUpstreamSchema)
      expect(result.valid).toBe(false)
      expect(result.errors.some((e) => e.path.includes('url'))).toBe(true)
    })

    it('fails when url is not a valid URI', () => {
      const config = {
        url: 'not-a-valid-url',
      }
      const result = validateJsonSchema(config, httpUpstreamSchema)
      expect(result.valid).toBe(false)
    })

    it('fails when timeout is negative', () => {
      const config = {
        url: 'https://api.example.com',
        timeout: -1,
      }
      const result = validateJsonSchema(config, httpUpstreamSchema)
      expect(result.valid).toBe(false)
    })

    it('fails on unknown property', () => {
      const config = {
        url: 'https://api.example.com',
        unknownField: 'value',
      }
      const result = validateJsonSchema(config, httpUpstreamSchema)
      expect(result.valid).toBe(false)
      expect(result.errors[0].message).toContain('unknownField')
    })
  })

  describe('multiple errors', () => {
    const schema = {
      type: 'object',
      required: ['url', 'method'],
      properties: {
        url: { type: 'string', format: 'uri' },
        method: { type: 'string', enum: ['GET', 'POST'] },
        timeout: { type: 'number', minimum: 0 },
      },
    }

    it('returns all errors when multiple validations fail', () => {
      const config = {
        timeout: -5, // Invalid: below minimum
        // Missing: url, method
      }
      const result = validateJsonSchema(config, schema)
      expect(result.valid).toBe(false)
      expect(result.errors.length).toBeGreaterThanOrEqual(2)
    })
  })
})

describe('generateSkeletonFromSchema', () => {
  it('returns empty object for empty schema', () => {
    const result = generateSkeletonFromSchema({})
    expect(result).toEqual({})
  })

  it('returns empty object for null schema', () => {
    const result = generateSkeletonFromSchema(null)
    expect(result).toEqual({})
  })

  it('generates required fields only by default', () => {
    const schema = {
      type: 'object',
      required: ['url'],
      properties: {
        url: { type: 'string', format: 'uri' },
        timeout: { type: 'number' },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({ url: 'https://example.com' })
    expect(result).not.toHaveProperty('timeout')
  })

  it('includes optional fields with defaults', () => {
    const schema = {
      type: 'object',
      required: ['url'],
      properties: {
        url: { type: 'string', format: 'uri' },
        timeout: { type: 'number', default: 30 },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({ url: 'https://example.com', timeout: 30 })
  })

  it('uses first enum value', () => {
    const schema = {
      type: 'object',
      required: ['method'],
      properties: {
        method: { type: 'string', enum: ['GET', 'POST', 'PUT'] },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({ method: 'GET' })
  })

  it('uses minimum for numbers when available', () => {
    const schema = {
      type: 'object',
      required: ['retries'],
      properties: {
        retries: { type: 'integer', minimum: 1 },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({ retries: 1 })
  })

  it('generates correct format placeholders', () => {
    const schema = {
      type: 'object',
      required: ['email', 'website'],
      properties: {
        email: { type: 'string', format: 'email' },
        website: { type: 'string', format: 'uri' },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result.email).toBe('user@example.com')
    expect(result.website).toBe('https://example.com')
  })

  it('generates nested objects', () => {
    const schema = {
      type: 'object',
      required: ['config'],
      properties: {
        config: {
          type: 'object',
          required: ['enabled'],
          properties: {
            enabled: { type: 'boolean' },
            limit: { type: 'number', default: 100 },
          },
        },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({
      config: { enabled: false, limit: 100 },
    })
  })

  it('generates arrays with one item if items schema exists', () => {
    const schema = {
      type: 'object',
      required: ['tags'],
      properties: {
        tags: {
          type: 'array',
          items: { type: 'string' },
        },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({ tags: [''] })
  })

  it('handles http-upstream schema correctly', () => {
    const schema = {
      type: 'object',
      required: ['url'],
      properties: {
        url: { type: 'string', format: 'uri' },
        path: { type: 'string' },
        timeout: { type: 'number', default: 30, minimum: 0 },
      },
    }
    const result = generateSkeletonFromSchema(schema)
    expect(result).toEqual({
      url: 'https://example.com',
      timeout: 30,
    })
  })
})

describe('generateSkeletonWithComments', () => {
  it('returns {} for empty schema', () => {
    const result = generateSkeletonWithComments({})
    expect(result).toBe('{}')
  })

  it('generates formatted output with comments', () => {
    const schema = {
      type: 'object',
      required: ['url'],
      properties: {
        url: { type: 'string', format: 'uri' },
        timeout: { type: 'number', minimum: 0, maximum: 300 },
      },
    }
    const result = generateSkeletonWithComments(schema)
    expect(result).toContain('"url"')
    expect(result).toContain('// required, string, format: uri')
    expect(result).toContain('// "timeout"')
    expect(result).toContain('min: 0')
    expect(result).toContain('max: 300')
  })

  it('shows enum options in comments', () => {
    const schema = {
      type: 'object',
      required: ['method'],
      properties: {
        method: { type: 'string', enum: ['GET', 'POST'] },
      },
    }
    const result = generateSkeletonWithComments(schema)
    expect(result).toContain('options: GET | POST')
  })
})
