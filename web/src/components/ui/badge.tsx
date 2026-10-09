import type { HTMLAttributes } from 'react'
import { cn } from '../../lib/utils'
export function Badge({ className, tone = 'green', ...props }: HTMLAttributes<HTMLSpanElement> & { tone?: 'green' | 'amber' }) { return <span className={cn('badge', tone === 'green' ? 'badge-green' : 'badge-amber', className)} {...props} /> }
