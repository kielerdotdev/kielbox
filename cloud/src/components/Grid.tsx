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
    <div className="capture-groups">
      {groups.map((g) => (
        <section key={g.label}>
          <h2 className="month-heading">{g.label}</h2>
          <div className="capture-grid">
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
    <div className="capture-grid" aria-busy>
      {Array.from({ length: count }, (_, i) => (
        <Skeleton key={i} className="aspect-video rounded-control" />
      ))}
    </div>
  )
}
