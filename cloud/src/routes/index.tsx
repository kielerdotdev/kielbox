import { useInfiniteQuery } from '@tanstack/react-query'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { Search, Library as LibraryIcon, CircleAlert } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { z } from 'zod'
import { FilterChips, filterOf, searchOf } from '../components/FilterChips'
import { Grid, SkeletonGrid } from '../components/Grid'
import { Button } from '../components/ui/button'
import { Input } from '../components/ui/field'
import { listItems } from '../functions/items'
import { KINDS } from '../lib/format'

const searchSchema = z.object({
  q: z.string().optional(),
  kind: z.enum(KINDS).optional(),
  favorite: z.boolean().optional(),
})

export const Route = createFileRoute('/')({
  validateSearch: searchSchema,
  loaderDeps: ({ search }) => ({ q: search.q, kind: search.kind, favorite: search.favorite }),
  // Prefetch the first page so it is server-rendered.
  loader: ({ deps }) => listItems({ data: deps }),
  component: Library,
  pendingComponent: () => <div className="library-page"><div className="library-toolbar"><h1>Library</h1></div><SkeletonGrid /></div>,
  errorComponent: ({ reset }) => <div className="library-empty" role="alert"><CircleAlert size={28} aria-hidden /><h1>Couldn't load your library</h1><p>The server didn't respond. Your captures are safe; check your connection and try again.</p><Button onClick={reset}>Try again</Button></div>,
})

function Library() {
  const search = Route.useSearch()
  const first = Route.useLoaderData()
  const navigate = useNavigate({ from: Route.fullPath })

  const [text, setText] = useState(search.q ?? '')
  useEffect(() => setText(search.q ?? ''), [search.q])
  useEffect(() => {
    const trimmed = text.trim()
    if (trimmed === (search.q ?? '')) return
    const t = setTimeout(
      () => void navigate({ search: (prev) => ({ ...prev, q: trimmed || undefined }), replace: true }),
      250,
    )
    return () => clearTimeout(t)
  }, [text, search.q, navigate])

  const query = useInfiniteQuery({
    queryKey: ['items', search.q ?? '', search.kind ?? '', search.favorite ?? false],
    queryFn: ({ pageParam }) =>
      listItems({ data: { q: search.q, kind: search.kind, favorite: search.favorite, cursor: pageParam } }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    initialData: { pages: [first], pageParams: [undefined as string | undefined] },
  })

  const { hasNextPage, isFetchingNextPage, fetchNextPage } = query
  const sentinel = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = sentinel.current
    if (!el || !hasNextPage) return
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting) && !isFetchingNextPage) void fetchNextPage()
    })
    io.observe(el)
    return () => io.disconnect()
  }, [hasNextPage, isFetchingNextPage, fetchNextPage])

  const items = query.data.pages.flatMap((p) => p.items)

  return (
    <div className="library-page">
      <div className="library-toolbar">
        <h1>Library</h1>
        <FilterChips
          value={filterOf(search.kind, search.favorite)}
          onChange={(f) =>
            void navigate({ search: (prev) => ({ q: prev.q, kind: undefined, favorite: undefined, ...searchOf(f) }) })
          }
        />
        <div className="library-search relative">
          <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-t3" aria-hidden />
          <Input
            type="search"
            aria-label="Search library"
            placeholder="Search captures"
            className="pl-9"
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        </div>
      </div>

      {query.isError ? (
        <div className="library-empty" role="alert"><CircleAlert size={28} aria-hidden /><h2>Couldn't load your library</h2><p>Your captures are safe. Check your connection and try again.</p><Button onClick={() => void query.refetch()}>Try again</Button></div>
      ) : query.isPending ? (
        <SkeletonGrid />
      ) : items.length === 0 ? (
        <div className="library-empty">
          {search.q || search.kind || search.favorite ? <Search size={28} aria-hidden /> : <LibraryIcon size={28} aria-hidden />}
          <h2>{search.q || search.kind || search.favorite ? 'Nothing matches' : 'No captures yet'}</h2>
          <p>{search.q || search.kind || search.favorite ? 'Try another filter or search.' : 'Captures you upload from the Kielbox desktop app land here, newest first. Turn it on under Settings, then Cloud.'}</p>
          <div className="flex flex-wrap justify-center gap-2">
            {search.kind || search.favorite ? <Button onClick={() => void navigate({ search: { q: search.q } })}>Search all kinds</Button> : null}
            {search.q ? <Button variant="ghost" onClick={() => { setText(''); void navigate({ search: (prev) => ({ ...prev, q: undefined }) }) }}>Clear search</Button> : null}
          </div>
        </div>
      ) : (
        <Grid items={items} loadingMore={isFetchingNextPage} />
      )}
      <div ref={sentinel} aria-hidden className="h-px" />
    </div>
  )
}
