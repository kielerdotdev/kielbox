import { Switch as Base } from '@base-ui/react/switch'

export function Switch({
  checked,
  onCheckedChange,
  label,
}: {
  checked: boolean
  onCheckedChange: (checked: boolean) => void
  label: string
}) {
  return (
    <label className="inline-flex cursor-pointer items-center gap-2 text-[13px] text-t1">
      <Base.Root
        checked={checked}
        onCheckedChange={onCheckedChange}
        className="relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full bg-surface-3 p-0.5 transition-colors data-[checked]:bg-accent"
      >
        <Base.Thumb className="size-4 rounded-full bg-t1 transition-transform data-[checked]:translate-x-4 data-[checked]:bg-ink" />
      </Base.Root>
      {label}
    </label>
  )
}
