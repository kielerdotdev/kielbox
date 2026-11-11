import { AlertDialog as Base } from '@base-ui/react/alert-dialog'
import { Button } from './button'

export function AlertDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  onConfirm,
  pending,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: string
  confirmLabel: string
  onConfirm: () => void
  pending?: boolean
}) {
  return (
    <Base.Root open={open} onOpenChange={(next) => { if (!pending) onOpenChange(next) }}>
      <Base.Portal>
        <Base.Backdrop className="fixed inset-0 bg-black/60 transition-opacity data-[ending-style]:opacity-0 data-[starting-style]:opacity-0" />
        <Base.Popup className="fixed left-1/2 top-1/2 w-[min(26rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 rounded-card bg-surface-2 p-5 shadow-float transition-all data-[ending-style]:scale-95 data-[starting-style]:scale-95 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0">
          <Base.Title className="text-[15px] font-semibold">{title}</Base.Title>
          <Base.Description className="mt-1 text-[12.5px] text-t2">{description}</Base.Description>
          <div className="mt-5 flex justify-end gap-2">
            <Base.Close render={<Button variant="ghost" disabled={pending} />}>Cancel</Base.Close>
            <Button variant="danger" disabled={pending} onClick={onConfirm}>
              {confirmLabel}
            </Button>
          </div>
        </Base.Popup>
      </Base.Portal>
    </Base.Root>
  )
}
