import { useQueryClient } from '@tanstack/react-query'
import { BotIcon, CopyIcon, ExternalLinkIcon, PlusIcon, RefreshCwIcon, Trash2Icon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Pending } from '@/components/pending'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { Spinner } from '@/components/ui/spinner'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { api, body, request, type Credential, type DeviceCode } from '@/lib/api'
import { useAction, useCopilot } from '@/lib/queries'

export function Copilot() {
  const copilot = useCopilot()
  const client = useQueryClient()
  const [device, setDevice] = useState<DeviceCode | null>(null)

  const connect = useAction(
    async () => setDevice(await api<DeviceCode>('/copilot/device-code', { method: 'POST' })),
    'Enter the code on GitHub to finish',
  )
  const refresh = useAction(
    (credential: Credential) => api('/copilot/refresh', body('POST', { auth_index: credential.auth_index })),
    'Copilot token refreshed',
  )
  const remove = useAction(
    (credential: Credential) => api(`/copilot?auth_index=${encodeURIComponent(credential.auth_index)}`, { method: 'DELETE' }),
    'Copilot account removed',
  )

  // GitHub approves the device out of band, so poll until the token lands.
  useEffect(() => {
    if (!device) return
    const timer = setInterval(async () => {
      try {
        const response = await request('/copilot/device-token', body('POST', { device_code: device.device_code }))
        if (response.status !== 200) return
        toast.success('Copilot account connected')
        void client.invalidateQueries({ queryKey: ['copilot'] })
      } catch (error) {
        toast.error(`GitHub sign-in failed: ${(error as Error).message}`)
      }
      setDevice(null)
    }, (device.interval + 1) * 1000)
    return () => clearInterval(timer)
  }, [device, client])

  if (!copilot.data) return <Pending error={copilot.error} />

  return (
    <Card>
      <CardHeader>
        <CardTitle>GitHub Copilot</CardTitle>
        <CardDescription>
          Accounts used for the models listed under <code className="font-mono">copilot.models</code>.
        </CardDescription>
        <CardAction>
          <Button disabled={connect.isPending} onClick={() => connect.mutate()}>
            <PlusIcon data-icon="inline-start" />
            Connect account
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        {copilot.data.length === 0 ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <BotIcon />
              </EmptyMedia>
              <EmptyTitle>No Copilot account connected</EmptyTitle>
              <EmptyDescription>
                Connect one with a GitHub device code. Copilot must be enabled in config.yaml first.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Account</TableHead>
                <TableHead>File</TableHead>
                <TableHead>Auth index</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {copilot.data.map((credential) => (
                <TableRow key={credential.auth_index}>
                  <TableCell className="font-medium">{credential.email || '—'}</TableCell>
                  <TableCell className="text-muted-foreground">{credential.name}</TableCell>
                  <TableCell className="font-mono text-xs">{credential.auth_index}</TableCell>
                  <TableCell>
                    <StatusBadge disabled={credential.disabled} />
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-1">
                      <Button variant="outline" size="sm" disabled={refresh.isPending} onClick={() => refresh.mutate(credential)}>
                        <RefreshCwIcon data-icon="inline-start" />
                        Refresh token
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={`Remove ${credential.name}`}
                        onClick={() => remove.mutate(credential)}
                      >
                        <Trash2Icon />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
      <Dialog open={device !== null} onOpenChange={(open) => !open && setDevice(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Enter this code on GitHub</DialogTitle>
            <DialogDescription>This window closes by itself once GitHub approves the device.</DialogDescription>
          </DialogHeader>
          <div className="flex items-center justify-center gap-2 rounded-lg bg-muted py-6">
            <span className="font-mono text-3xl font-medium tracking-widest">{device?.user_code}</span>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Copy code"
              onClick={() => device && navigator.clipboard.writeText(device.user_code).then(() => toast.success('Code copied'))}
            >
              <CopyIcon />
            </Button>
          </div>
          <DialogFooter className="sm:justify-between">
            <span className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner />
              Waiting for approval
            </span>
            <Button asChild>
              <a href={device?.verification_uri} target="_blank" rel="noreferrer">
                Open GitHub
                <ExternalLinkIcon data-icon="inline-end" />
              </a>
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
