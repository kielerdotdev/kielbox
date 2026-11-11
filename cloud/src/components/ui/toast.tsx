import { Toast as Base } from '@base-ui/react/toast'
import { X } from 'lucide-react'
import type { ReactNode } from 'react'

export const useToast = Base.useToastManager

function Toasts() {
  const { toasts } = Base.useToastManager()
  return (
    <Base.Portal>
      <Base.Viewport className="fixed bottom-4 right-4 z-50 flex w-80 flex-col gap-2">
        {toasts.map((toast) => (
          <Base.Root
            key={toast.id}
            toast={toast}
            className="flex items-start justify-between gap-3 rounded-card bg-surface-2 p-3 shadow-float transition-all data-[ending-style]:translate-y-2 data-[starting-style]:translate-y-2 data-[ending-style]:opacity-0 data-[starting-style]:opacity-0"
          >
            <div className="min-w-0">
              <Base.Title className="text-[13px] font-semibold" />
              <Base.Description className="text-[12.5px] text-t2 [overflow-wrap:anywhere]" />
            </div>
            <Base.Close aria-label="Dismiss" className="cursor-pointer text-t3 hover:text-t1">
              <X size={14} />
            </Base.Close>
          </Base.Root>
        ))}
      </Base.Viewport>
    </Base.Portal>
  )
}

/** One provider for the whole app (mounted in __root.tsx). */
export function ToastProvider({ children }: { children: ReactNode }) {
  return (
    <Base.Provider>
      {children}
      <Toasts />
    </Base.Provider>
  )
}
