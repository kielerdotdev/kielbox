import type { ReactNode } from 'react'
import { Switch as Base } from '@base-ui/react/switch'

export function Switch({
  checked,
  onCheckedChange,
  label,
}: {
  checked: boolean
  onCheckedChange: (checked: boolean) => void
  label: ReactNode
}) {
  return (
    <label className="flex w-full cursor-pointer flex-row-reverse items-center justify-between gap-4 text-[13px] text-t1">
      <Base.Root
        checked={checked}
        onCheckedChange={onCheckedChange}
        className="relative inline-flex h-[18px] w-[30px] shrink-0 cursor-pointer items-center rounded-full bg-surface-3 p-0.5 transition-colors data-[checked]:bg-accent"
      >
        <Base.Thumb className="size-[14px] rounded-full bg-t1 transition-transform data-[checked]:translate-x-3 data-[checked]:bg-white" />
      </Base.Root>
      {label}
    </label>
  )
}
