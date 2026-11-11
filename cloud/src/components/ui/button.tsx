import type { ButtonHTMLAttributes } from 'react'

type Variant = 'primary' | 'secondary' | 'ghost' | 'danger'
const variants: Record<Variant, string> = {
  primary: 'bg-accent text-ink hover:bg-accent-hover font-semibold',
  secondary: 'bg-surface-2 text-t1 hover:bg-surface-3 ring-1 ring-line font-medium',
  ghost: 'text-t2 hover:text-t1 hover:bg-surface-2 font-medium',
  danger: 'bg-rec text-white hover:brightness-110 font-semibold',
}

export function Button({
  variant = 'secondary',
  className = '',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      type="button"
      {...props}
      className={`inline-flex h-8 cursor-pointer items-center justify-center gap-1.5 rounded-control px-3 text-[13px] transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${variants[variant]} ${className}`}
    />
  )
}
