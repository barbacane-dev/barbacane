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
