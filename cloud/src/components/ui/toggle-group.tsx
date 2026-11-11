import { Toggle } from '@base-ui/react/toggle'
import { ToggleGroup as Base } from '@base-ui/react/toggle-group'

export interface Choice<T extends string> {
  value: T
  label: string
}

/** Single-select chip group; re-selecting the active chip keeps it selected. */
export function ToggleGroup<T extends string>({
  value,
  onValueChange,
  choices,
  label,
}: {
  value: T
  onValueChange: (value: T) => void
  choices: Choice<T>[]
  label: string
}) {
  return (
    <Base
      aria-label={label}
      value={[value]}
      onValueChange={(next) => {
        const picked = next.find((v) => v !== value) ?? next[0]
        if (picked) onValueChange(picked as T)
      }}
      className="filter-chips flex flex-wrap gap-0.5"
    >
      {choices.map((c) => (
        <Toggle
          key={c.value}
          value={c.value}
          className="filter-chip"
        >
          {c.label}
        </Toggle>
      ))}
    </Base>
  )
}
