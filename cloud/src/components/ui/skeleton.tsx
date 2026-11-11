import type { HTMLAttributes } from 'react'

export function Skeleton({ className = '', ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div aria-hidden {...props} className={`animate-pulse rounded-control bg-surface-2 ${className}`} />
}
