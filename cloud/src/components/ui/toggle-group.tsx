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
      className="flex flex-wrap gap-1.5"
    >
      {choices.map((c) => (
        <Toggle
          key={c.value}
          value={c.value}
          className="h-7 cursor-pointer rounded-full bg-surface-2 px-3 text-[12.5px] font-medium text-t2 ring-1 ring-line transition-colors hover:text-t1 data-[pressed]:bg-t1 data-[pressed]:text-deep"
        >
          {c.label}
        </Toggle>
      ))}
    </Base>
  )
}
