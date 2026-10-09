import type { ButtonHTMLAttributes } from 'react'
import { cn } from '../../lib/utils'
export function Button({ className, variant = 'default', ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'default' | 'primary' }) { return <button className={cn('button', variant === 'primary' && 'button-primary', className)} {...props} /> }
