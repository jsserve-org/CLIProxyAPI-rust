import { TriangleAlertIcon } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'

/** Placeholder for a page whose data has not arrived (or failed to). */
export function Pending({ error }: { error: Error | null }) {
  if (error) {
    return (
      <Alert variant="destructive">
        <TriangleAlertIcon />
        <AlertTitle>Couldn't load this page</AlertTitle>
        <AlertDescription>{error.message}</AlertDescription>
      </Alert>
    )
  }
  return (
    <div className="flex flex-col gap-4">
      <Skeleton className="h-24" />
      <Skeleton className="h-64" />
    </div>
  )
}
