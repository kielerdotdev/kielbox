import type { QueryClient } from '@tanstack/react-query'
import { createRootRouteWithContext, HeadContent, Link, Outlet, Scripts } from '@tanstack/react-router'
import { Cloud } from 'lucide-react'
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
          <header className="flex items-center justify-between border-b border-line px-6 py-3">
            <Link to="/" className="flex items-center gap-2 text-[15px] font-semibold">
              <Cloud size={18} className="text-accent" aria-hidden />
              Kielbox Cloud
            </Link>
            <span className="text-[12.5px] text-t3" data-testid="identity">
              {me.identity}
            </span>
          </header>
          <main className="mx-auto max-w-[1400px] px-6 py-6">
            <Outlet />
          </main>
        </ToastProvider>
      </TooltipProvider>
    </div>
  )
}
