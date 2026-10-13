import { describe, it, expect } from 'vitest'
import { resolveMiddlewareChain } from './middleware-chain'

const auth = { name: 'oidc-auth', config: { audience: 'api' } }
const rateLimit = { name: 'rate-limit', config: { quota: 300 } }

describe('resolveMiddlewareChain', () => {
  it('inherits the global chain when the operation sets none', () => {
    expect(resolveMiddlewareChain([auth, rateLimit], null)).toEqual([auth, rateLimit])
  })

  it('runs nothing for an empty list', () => {
    expect(resolveMiddlewareChain([auth, rateLimit], [])).toEqual([])
  })

  it('removes only the global entry an operation disables', () => {
    const chain = resolveMiddlewareChain([auth, rateLimit], [{ name: 'oidc-auth', enabled: false }])
    expect(chain).toEqual([rateLimit])
  })

  it('removes every stacked instance of a disabled plugin', () => {
    const perSecond = { name: 'rate-limit', config: { policy_name: 'per-second' } }
    const perDay = { name: 'rate-limit', config: { policy_name: 'per-day' } }
    const chain = resolveMiddlewareChain([perSecond, auth, perDay], [{ name: 'rate-limit', enabled: false }])
    expect(chain).toEqual([auth])
  })

  it('matches a global entry by plugin name without its version', () => {
    const override = { name: 'rate-limit', config: { quota: 1000 } }
    const chain = resolveMiddlewareChain([{ name: 'rate-limit@0.1.0', config: { quota: 60 } }], [override])
    expect(chain).toEqual([override])
  })
})
