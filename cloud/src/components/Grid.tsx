import type { ItemJson } from '../functions/types'
import { monthLabel } from '../lib/format'
import { Card } from './Card'
import { Skeleton } from './ui/skeleton'

/** Items are already newest first; group consecutive items by month. */
export function Grid({ items, loadingMore }: { items: ItemJson[]; loadingMore?: boolean }) {
  const groups: { label: string; items: ItemJson[] }[] = []
  for (const item of items) {
    const label = monthLabel(item.captured_at)
    const last = groups[groups.length - 1]
    if (last && last.label === label) last.items.push(item)
    else groups.push({ label, items: [item] })
  }
  return (
    <div className="flex flex-col gap-8">
      {groups.map((g) => (
        <section key={g.label}>
          <h2 className="mb-3 text-[12px] font-medium uppercase tracking-wide text-t3">{g.label}</h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-4">
            {g.items.map((item) => (
              <Card key={item.id} item={item} />
            ))}
          </div>
        </section>
      ))}
      {loadingMore ? <SkeletonGrid count={4} /> : null}
    </div>
  )
}

export function SkeletonGrid({ count = 8 }: { count?: number }) {
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-4" aria-busy>
      {Array.from({ length: count }, (_, i) => (
        <Skeleton key={i} className="aspect-[4/3] rounded-card" />
      ))}
    </div>
  )
}
