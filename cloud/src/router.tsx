import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { createRouter, type ErrorComponentProps } from '@tanstack/react-router'
import { routeTree } from './routeTree.gen'

/** A 401 from a server function means the Access session ended: reload so Access can re-auth. */
function onError(error: unknown) {
  if (typeof window !== 'undefined' && error instanceof Error && error.message === 'unauthorized') {
    window.location.reload()
  }
}

export function getRouter() {
  const queryClient = new QueryClient({
    queryCache: new QueryCache({ onError }),
    mutationCache: new MutationCache({ onError }),
    defaultOptions: { queries: { staleTime: 30_000, retry: false } },
  })
  return createRouter({
    routeTree,
    scrollRestoration: true,
    defaultPreload: 'intent',
    defaultPreloadStaleTime: 0,
    context: { queryClient },
    Wrap: ({ children }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>,
    defaultErrorComponent: ({ error }: ErrorComponentProps) => (
      <div className="mx-auto max-w-xl p-8">
        <h1 className="text-xl font-semibold">Something went wrong</h1>
        <p className="mt-2 text-t2 [overflow-wrap:anywhere]">{error instanceof Error ? error.message : String(error)}</p>
      </div>
    ),
    defaultNotFoundComponent: () => (
      <div className="mx-auto max-w-xl p-8">
        <h1 className="text-xl font-semibold">Not found</h1>
        <p className="mt-2 text-t2">That page does not exist.</p>
      </div>
    ),
  })
}

declare module '@tanstack/react-router' {
  interface Register {
    router: ReturnType<typeof getRouter>
  }
}
