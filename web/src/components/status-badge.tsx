import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'

export function StatusBadge({ disabled }: { disabled: boolean }) {
  return (
    <Badge variant="outline">
      <span className={cn('size-1.5 rounded-full', disabled ? 'bg-amber-500' : 'bg-emerald-500')} />
      {disabled ? 'Disabled' : 'Active'}
    </Badge>
  )
}
