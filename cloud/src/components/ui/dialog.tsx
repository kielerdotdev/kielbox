import { Dialog as Base } from '@base-ui/react/dialog'
import type { ReactNode } from 'react'

export function Dialog({
  open,
  onOpenChange,
  title,
  description,
  children,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: string
  children: ReactNode
}) {
  return (
    <Base.Root open={open} onOpenChange={onOpenChange}>
      <Base.Portal>
        <Base.Backdrop className="fixed inset-0 bg-black/60 transition-opacity data-[ending-style]:opacity-0 data-[starting-style]:opacity-0" />
        <Base.Popup className="fixed left-1/2 top-1/2 w-[min(28rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-card bg-surface-2 p-5 shadow-float transition-all data-[ending-style]:scale-95 data-[starting-style]:scale-95 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
          <Base.Title className="text-[15px] font-semibold">{title}</Base.Title>
          {description ? (
            <Base.Description className="mt-1 text-[12.5px] text-t2">{description}</Base.Description>
          ) : null}
          <div className="mt-4">{children}</div>
        </Base.Popup>
      </Base.Portal>
    </Base.Root>
  )
}
