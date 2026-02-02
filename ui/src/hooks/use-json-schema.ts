import { useMemo, useCallback } from 'react'
import Ajv, { type ErrorObject } from 'ajv'
import addFormats from 'ajv-formats'

// Create a singleton Ajv instance with formats
const ajv = new Ajv({
  allErrors: true, // Return all errors, not just the first one
  verbose: true, // Include schema and data in errors
  strict: false, // Allow additional keywords
})
addFormats(ajv)

export interface ValidationError {
  path: string
  message: string
  keyword: string
}

export interface ValidationResult {
  valid: boolean
  errors: ValidationError[]
}

/**
 * Format AJV errors into a more user-friendly structure
 */
function formatErrors(errors: ErrorObject[] | null | undefined): ValidationError[] {
  if (!errors) return []

  return errors.map((error) => {
    let path = error.instancePath || '/'
    let message = error.message || 'Invalid value'

    // Make error messages more readable
    switch (error.keyword) {
      case 'required':
        path = `${path}/${error.params?.missingProperty || ''}`
        message = `Required property is missing`
        break
      case 'type':
        message = `Expected ${error.params?.type}`
        break
      case 'enum':
        message = `Must be one of: ${error.params?.allowedValues?.join(', ')}`
        break
      case 'format':
        message = `Invalid format, expected ${error.params?.format}`
        break
      case 'minimum':
        message = `Must be >= ${error.params?.limit}`
        break
      case 'maximum':
        message = `Must be <= ${error.params?.limit}`
        break
      case 'minLength':
        message = `Must be at least ${error.params?.limit} characters`
        break
      case 'maxLength':
        message = `Must be at most ${error.params?.limit} characters`
        break
      case 'pattern':
        message = `Does not match required pattern`
        break
      case 'additionalProperties':
        message = `Unknown property: ${error.params?.additionalProperty}`
        break
    }

    return {
      path: path.replace(/^\//, '') || 'root', // Remove leading slash, default to 'root'
      message,
      keyword: error.keyword,
    }
  })
}

/**
 * Hook for validating data against a JSON Schema
 */
export function useJsonSchema(schema: Record<string, unknown> | null | undefined) {
  // Compile the schema (memoized to avoid recompilation)
  const validate = useMemo(() => {
    if (!schema || Object.keys(schema).length === 0) {
      return null
    }

    try {
      return ajv.compile(schema)
    } catch (error) {
      console.error('Failed to compile JSON Schema:', error)
      return null
    }
  }, [schema])

  // Validation function
  const validateData = useCallback(
    (data: unknown): ValidationResult => {
      // If no schema, always valid
      if (!validate) {
        return { valid: true, errors: [] }
      }

      const valid = validate(data)
      return {
        valid: !!valid,
        errors: formatErrors(validate.errors),
      }
    },
    [validate]
  )

  // Check if schema exists and is valid
  const hasSchema = useMemo(() => {
    return schema && Object.keys(schema).length > 0 && validate !== null
  }, [schema, validate])

  return {
    validate: validateData,
    hasSchema,
  }
}

/**
 * Standalone validation function (for non-hook contexts)
 */
export function validateJsonSchema(
  data: unknown,
  schema: Record<string, unknown>
): ValidationResult {
  if (!schema || Object.keys(schema).length === 0) {
    return { valid: true, errors: [] }
  }

  try {
    const validate = ajv.compile(schema)
    const valid = validate(data)
    return {
      valid: !!valid,
      errors: formatErrors(validate.errors),
    }
  } catch (error) {
    console.error('Failed to compile JSON Schema:', error)
    return {
      valid: false,
      errors: [{ path: 'schema', message: 'Invalid JSON Schema', keyword: 'schema' }],
    }
  }
}

interface SchemaProperty {
  type?: string
  default?: unknown
  enum?: unknown[]
  properties?: Record<string, SchemaProperty>
  items?: SchemaProperty
  required?: string[]
  description?: string
  format?: string
  minimum?: number
  maximum?: number
}

/**
 * Generate a skeleton configuration object from a JSON Schema
 * Includes required fields and uses defaults where available
 */
export function generateSkeletonFromSchema(
  schema: Record<string, unknown> | null | undefined
): Record<string, unknown> {
  if (!schema || Object.keys(schema).length === 0) {
    return {}
  }

  const typedSchema = schema as SchemaProperty
  return generateValueFromSchema(typedSchema) as Record<string, unknown>
}

function generateValueFromSchema(schema: SchemaProperty): unknown {
  // Use default if provided
  if (schema.default !== undefined) {
    return schema.default
  }

  // Use first enum value if available
  if (schema.enum && schema.enum.length > 0) {
    return schema.enum[0]
  }

  // Generate based on type
  switch (schema.type) {
    case 'object':
      return generateObjectFromSchema(schema)

    case 'array':
      // Return empty array, or array with one item if items schema exists
      if (schema.items) {
        return [generateValueFromSchema(schema.items)]
      }
      return []

    case 'string':
      // Generate placeholder based on format
      if (schema.format === 'uri' || schema.format === 'url') {
        return 'https://example.com'
      }
      if (schema.format === 'email') {
        return 'user@example.com'
      }
      if (schema.format === 'date') {
        return new Date().toISOString().split('T')[0]
      }
      if (schema.format === 'date-time') {
        return new Date().toISOString()
      }
      return ''

    case 'number':
    case 'integer':
      // Use minimum if available, otherwise 0
      if (schema.minimum !== undefined) {
        return schema.minimum
      }
      return 0

    case 'boolean':
      return false

    case 'null':
      return null

    default:
      // If no type specified but has properties, treat as object
      if (schema.properties) {
        return generateObjectFromSchema(schema)
      }
      return null
  }
}

function generateObjectFromSchema(schema: SchemaProperty): Record<string, unknown> {
  const result: Record<string, unknown> = {}
  const required = new Set(schema.required || [])
  const properties = schema.properties || {}

  // First, add all required properties
  for (const [key, propSchema] of Object.entries(properties)) {
    if (required.has(key)) {
      result[key] = generateValueFromSchema(propSchema)
    }
  }

  // Then add optional properties that have defaults
  for (const [key, propSchema] of Object.entries(properties)) {
    if (!required.has(key) && propSchema.default !== undefined) {
      result[key] = propSchema.default
    }
  }

  return result
}

/**
 * Generate a skeleton with comments showing all available options
 * Returns a formatted string with inline comments
 */
export function generateSkeletonWithComments(
  schema: Record<string, unknown> | null | undefined
): string {
  if (!schema || Object.keys(schema).length === 0) {
    return '{}'
  }

  const typedSchema = schema as SchemaProperty
  const skeleton = generateSkeletonFromSchema(schema)
  const lines: string[] = ['{']

  const properties = typedSchema.properties || {}
  const required = new Set(typedSchema.required || [])
  const entries = Object.entries(properties)

  entries.forEach(([key, propSchema], index) => {
    const isRequired = required.has(key)
    const value = skeleton[key]
    const hasValue = key in skeleton

    // Build the comment
    const comments: string[] = []
    if (isRequired) comments.push('required')
    if (propSchema.type) comments.push(propSchema.type)
    if (propSchema.format) comments.push(`format: ${propSchema.format}`)
    if (propSchema.enum) comments.push(`options: ${propSchema.enum.join(' | ')}`)
    if (propSchema.minimum !== undefined) comments.push(`min: ${propSchema.minimum}`)
    if (propSchema.maximum !== undefined) comments.push(`max: ${propSchema.maximum}`)

    const commentStr = comments.length > 0 ? ` // ${comments.join(', ')}` : ''

    if (hasValue) {
      const valueStr = JSON.stringify(value)
      const comma = index < entries.length - 1 ? ',' : ''
      lines.push(`  "${key}": ${valueStr}${comma}${commentStr}`)
    } else {
      // Show commented-out optional field
      const defaultValue = generateValueFromSchema(propSchema)
      const valueStr = JSON.stringify(defaultValue)
      lines.push(`  // "${key}": ${valueStr}${commentStr}`)
    }
  })

  lines.push('}')
  return lines.join('\n')
}
