import type { Kind } from '../lib/format'
import { ToggleGroup } from './ui/toggle-group'

export type Filter = 'all' | 'replay' | 'recording' | 'screenshot' | 'favorites'

export function filterOf(kind: Kind | undefined, favorite: boolean | undefined): Filter {
  if (favorite) return 'favorites'
  if (kind === 'replay' || kind === 'recording' || kind === 'screenshot') return kind
  return 'all'
}

export function searchOf(filter: Filter): { kind?: Kind; favorite?: boolean } {
  if (filter === 'all') return {}
  if (filter === 'favorites') return { favorite: true }
  return { kind: filter }
}

export function FilterChips({ value, onChange }: { value: Filter; onChange: (f: Filter) => void }) {
  return (
    <ToggleGroup<Filter>
      label="Filter library"
      value={value}
      onValueChange={onChange}
      choices={[
        { value: 'all', label: 'All' },
        { value: 'replay', label: 'Replays' },
        { value: 'recording', label: 'Recordings' },
        { value: 'screenshot', label: 'Screenshots' },
        { value: 'favorites', label: 'Favorites' },
      ]}
    />
  )
}
