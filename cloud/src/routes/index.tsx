import { useInfiniteQuery } from '@tanstack/react-query'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { Search } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { z } from 'zod'
import { FilterChips, filterOf, searchOf } from '../components/FilterChips'
import { Grid, SkeletonGrid } from '../components/Grid'
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
    <div className="flex flex-col gap-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <FilterChips
          value={filterOf(search.kind, search.favorite)}
          onChange={(f) =>
            void navigate({ search: (prev) => ({ q: prev.q, kind: undefined, favorite: undefined, ...searchOf(f) }) })
          }
        />
        <div className="relative w-full max-w-xs">
          <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-t3" aria-hidden />
          <Input
            type="search"
            aria-label="Search library"
            placeholder="Search title or filename"
            className="pl-9"
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        </div>
      </div>

      {query.isPending ? (
        <SkeletonGrid />
      ) : items.length === 0 ? (
        <p className="py-16 text-center text-t3">
          {search.q || search.kind || search.favorite ? 'Nothing matches.' : 'No captures uploaded yet.'}
        </p>
      ) : (
        <Grid items={items} loadingMore={isFetchingNextPage} />
      )}
      <div ref={sentinel} aria-hidden className="h-px" />
    </div>
  )
}
