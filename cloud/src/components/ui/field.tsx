import { Field as Base } from '@base-ui/react/field'
import { Input as BaseInput } from '@base-ui/react/input'
import type { InputHTMLAttributes, ReactNode } from 'react'

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Base.Root className="flex flex-col gap-1.5">
      <Base.Label className="text-[12px] font-medium text-t3">{label}</Base.Label>
      {children}
    </Base.Root>
  )
}

export function Input({ className = '', ...props }: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <BaseInput
      {...props}
      className={`h-8 w-full rounded-control bg-surface px-3 text-[13px] text-t1 ring-1 ring-line placeholder:text-t3 focus:ring-2 focus:ring-accent focus:outline-none ${className}`}
    />
  )
}
