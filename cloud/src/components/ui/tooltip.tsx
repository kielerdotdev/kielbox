import { Tooltip as Base } from '@base-ui/react/tooltip'
import type { ReactElement } from 'react'

export const TooltipProvider = Base.Provider

export function Tooltip({ label, children }: { label: string; children: ReactElement }) {
  return (
    <Base.Root>
      <Base.Trigger render={children} />
      <Base.Portal>
        <Base.Positioner sideOffset={6}>
          <Base.Popup className="rounded-control bg-surface-3 px-2 py-1 text-[12px] text-t1 shadow-float transition-opacity data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
            {label}
          </Base.Popup>
        </Base.Positioner>
      </Base.Portal>
    </Base.Root>
  )
}
