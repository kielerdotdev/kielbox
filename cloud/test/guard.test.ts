import { describe, expect, it } from 'vitest'

// Source text of every file under src/, via Vite (the Workers test runtime has no filesystem).
const sources = import.meta.glob('/src/**/*.{ts,tsx}', { query: '?raw', import: 'default', eager: true }) as Record<
  string,
  string
>
const files = Object.entries(sources).filter(([path]) => !path.endsWith('routeTree.gen.ts'))

const strip = (code: string) => code.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '')

describe('server function guard', () => {
  it('sees the source tree', () => {
    expect(files.length).toBeGreaterThan(20)
    expect(Object.keys(sources)).toContain('/src/functions/auth.ts')
  })

  it('createServerFn( appears only in functions/auth.ts', () => {
    const offenders = files
      .filter(([path, code]) => strip(code).includes('createServerFn(') && path !== '/src/functions/auth.ts')
      .map(([path]) => path)
    expect(offenders).toEqual([])
  })

  it('every exported server function is built from authedGet or authedPost', () => {
    const fnFiles = files.filter(
      ([path]) => path.startsWith('/src/functions/') && !['auth.ts', 'types.ts'].includes(path.split('/').pop()!),
    )
    expect(fnFiles.map(([p]) => p.split('/').pop()).sort()).toEqual(['items.ts', 'me.ts', 'shares.ts'])
    let count = 0
    for (const [path, code] of fnFiles) {
      const body = strip(code)
      expect(body, path).not.toMatch(/createMiddleware|createServerFn|createIsomorphicFn|\.middleware\(/)
      const decls = body.split(/^export\s+/m).slice(1)
      expect(decls.length, path).toBeGreaterThan(0)
      for (const decl of decls) {
        expect(decl, `${path}: ${decl.slice(0, 40)}`).toMatch(/^const\s+\w+\s*=\s*authed(Get|Post)\b[\s\S]*\.handler\(/)
        count++
      }
    }
    expect(count).toBe(8) // me, listItems, getItem, updateItem, deleteItem, listShares, createShare, revokeShare
  })

  it('mutating functions are POST, reads are GET', () => {
    const mutating = /^(create|update|delete|revoke|set|remove)/
    for (const [path, code] of files.filter(([p]) => p.startsWith('/src/functions/'))) {
      for (const m of strip(code).matchAll(/export const (\w+)\s*=\s*authed(Get|Post)\b/g)) {
        const [, name, method] = m
        expect(method === 'Post', `${path}:${name}`).toBe(mutating.test(name!))
      }
    }
  })

  it('the middleware verifies Access itself', () => {
    const auth = strip(sources['/src/functions/auth.ts']!)
    expect(auth).toMatch(/authenticate\(/)
    expect(auth).toMatch(/\.middleware\(\[authMiddleware\]\)/)
  })
})

describe('client/server boundary', () => {
  it('routes, components and lib only import src/server as types', () => {
    for (const [path, code] of files.filter(([p]) => /^\/src\/(routes|components|lib)\//.test(p) || p === '/src/router.tsx')) {
      const imports = [...strip(code).matchAll(/^import\s+(type\s+)?[^;]*?from\s+['"]([^'"]+)['"]/gms)]
      for (const [, isType, from] of imports) {
        if (from!.includes('/server/') || from!.endsWith('/server')) {
          expect(isType, `${path} imports ${from}`).toBeTruthy()
        }
      }
    }
  })

  it('src/server stays framework-free', () => {
    for (const [path, code] of files.filter(([p]) => p.startsWith('/src/server/'))) {
      expect(strip(code), path).not.toMatch(/from\s+['"](@tanstack|react|react-dom|lucide-react|@base-ui)/)
    }
  })

  it('never uses dangerouslySetInnerHTML', () => {
    const offenders = files.filter(([, code]) => strip(code).includes('dangerouslySetInnerHTML')).map(([p]) => p)
    expect(offenders).toEqual([])
  })
})
