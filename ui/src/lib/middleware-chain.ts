import type { MiddlewareBinding } from '@/lib/api/types'

/** The plugin name without its `@version` suffix: entries match on it. */
export function pluginKey(name: string): string {
  return name.split('@')[0]
}

/**
 * The chain an operation runs, as the compiler resolves it (SPEC-001 §3.1,
 * ADR-0034): `null` inherits the global chain, `[]` runs nothing, and a list
 * keeps the global entries whose plugin it does not name, followed by its own
 * entries except those set to `enabled: false`.
 */
export function resolveMiddlewareChain(
  globalMiddlewares: MiddlewareBinding[],
  operationMiddlewares: MiddlewareBinding[] | null,
): MiddlewareBinding[] {
  if (operationMiddlewares === null) return globalMiddlewares
  if (operationMiddlewares.length === 0) return []
  const opKeys = new Set(operationMiddlewares.map((m) => pluginKey(m.name)))
  const preserved = globalMiddlewares.filter((g) => !opKeys.has(pluginKey(g.name)))
  return [...preserved, ...operationMiddlewares.filter((m) => m.enabled !== false)]
}
