import type { HTMLAttributes } from 'react'

export function Badge({ className = '', ...props }: HTMLAttributes<HTMLSpanElement>) {
  return (
    <span
      {...props}
      className={`inline-flex items-center rounded-full bg-surface-3 px-2 py-0.5 text-[11px] font-medium text-t2 ${className}`}
    />
  )
}
