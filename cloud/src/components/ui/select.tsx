import { Select as Base } from '@base-ui/react/select'
import { Check, ChevronsUpDown } from 'lucide-react'

export interface Option<T extends string> {
  value: T
  label: string
}

export function Select<T extends string>({
  value,
  onValueChange,
  options,
  label,
}: {
  value: T
  onValueChange: (value: T) => void
  options: Option<T>[]
  label: string
}) {
  return (
    <Base.Root
      value={value}
      onValueChange={(v) => {
        if (v !== null) onValueChange(v as T)
      }}
      items={options}
    >
      <Base.Trigger
        aria-label={label}
        className="inline-flex h-8 min-w-36 cursor-pointer items-center justify-between gap-2 rounded-control bg-surface-2 px-3 text-[13px] ring-1 ring-line hover:bg-surface-3"
      >
        <Base.Value />
        <Base.Icon>
          <ChevronsUpDown size={14} className="text-t3" />
        </Base.Icon>
      </Base.Trigger>
      <Base.Portal>
        <Base.Positioner sideOffset={6} alignItemWithTrigger={false}>
          <Base.Popup className="min-w-[var(--anchor-width)] rounded-card bg-surface-2 p-1 shadow-float transition-all data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
            <Base.List>
              {options.map((o) => (
                <Base.Item
                  key={o.value}
                  value={o.value}
                  className="grid cursor-pointer grid-cols-[1rem_1fr] items-center gap-2 rounded-control px-2 py-1.5 text-[13px] outline-none data-[highlighted]:bg-surface-3"
                >
                  <Base.ItemIndicator>
                    <Check size={14} className="text-accent" />
                  </Base.ItemIndicator>
                  <Base.ItemText className="col-start-2">{o.label}</Base.ItemText>
                </Base.Item>
              ))}
            </Base.List>
          </Base.Popup>
        </Base.Positioner>
      </Base.Portal>
    </Base.Root>
  )
}
