import type { ButtonHTMLAttributes } from 'react'
export function Switch({ checked, ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { checked: boolean }) { return <button type="button" role="switch" aria-checked={checked} className={`switch ${checked ? 'on' : ''}`} {...props}><i /></button> }
