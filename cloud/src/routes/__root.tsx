import type { QueryClient } from '@tanstack/react-query'
import { createRootRouteWithContext, HeadContent, Link, Outlet, Scripts } from '@tanstack/react-router'
import { Brand } from '../components/Brand'
import type { ReactNode } from 'react'
import { ToastProvider } from '../components/ui/toast'
import { TooltipProvider } from '../components/ui/tooltip'
import { getMe } from '../functions/me'
import appCss from '../styles.css?url'

export const Route = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  head: () => ({
    meta: [
      { charSet: 'utf-8' },
      { name: 'viewport', content: 'width=device-width, initial-scale=1' },
      { name: 'robots', content: 'noindex,nofollow' },
      { title: 'Kielbox Cloud' },
    ],
    links: [{ rel: 'stylesheet', href: appCss }],
  }),
  loader: () => getMe(),
  shellComponent: RootDocument,
  component: RootLayout,
})

function RootDocument({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <head>
        <HeadContent />
      </head>
      <body>
        {children}
        <Scripts />
      </body>
    </html>
  )
}

function RootLayout() {
  const me = Route.useLoaderData()
  return (
    <div id="app-root">
      <TooltipProvider>
        <ToastProvider>
          <header className="app-header">
            <Link to="/" className="brand-link">
              <Brand />
            </Link>
            <span className="app-identity" data-testid="identity">
              {me.identity}
            </span>
          </header>
          <main className="app-main">
            <Outlet />
          </main>
        </ToastProvider>
      </TooltipProvider>
    </div>
  )
}
