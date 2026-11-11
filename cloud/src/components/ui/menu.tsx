import { Menu as Base } from '@base-ui/react/menu'
import type { ReactElement, ReactNode } from 'react'

export interface MenuEntry {
  label: string
  onSelect: () => void
  icon?: ReactNode
  danger?: boolean
}

export function Menu({ trigger, items }: { trigger: ReactElement; items: MenuEntry[] }) {
  return (
    <Base.Root>
      <Base.Trigger render={trigger} />
      <Base.Portal>
        <Base.Positioner sideOffset={6} align="end">
          <Base.Popup className="min-w-40 origin-top-right rounded-card bg-surface-2 p-1 shadow-float transition-all data-[ending-style]:scale-95 data-[starting-style]:scale-95 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
            {items.map((item) => (
              <Base.Item
                key={item.label}
                onClick={item.onSelect}
                className={`flex cursor-pointer items-center gap-2 rounded-control px-2.5 py-1.5 text-[13px] outline-none data-[highlighted]:bg-surface-3 ${item.danger ? 'text-rec-text' : 'text-t1'}`}
              >
                {item.icon}
                {item.label}
              </Base.Item>
            ))}
          </Base.Popup>
        </Base.Positioner>
      </Base.Portal>
    </Base.Root>
  )
}
