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
      className={`h-7 w-full rounded-control bg-white/3 px-2.5 text-[13px] text-t1 ring-[0.5px] ring-ring placeholder:text-t3 focus:ring-2 focus:ring-accent focus:outline-none ${className}`}
    />
  )
}
