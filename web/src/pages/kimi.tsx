import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ExternalLinkIcon, MoonStarIcon, PlusIcon, Trash2Icon } from 'lucide-react'
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
import { api, type Credential } from '@/lib/api'
import { useAction } from '@/lib/queries'

type Login = { url: string; state: string; user_code: string }
type KimiAccount = Credential & { expired?: string }

export function Kimi() {
  const client = useQueryClient()
  const accounts = useQuery({ queryKey: ['kimi'], queryFn: async () => (await api<{ files: KimiAccount[] }>('/kimi')).files })
  const [login, setLogin] = useState<Login | null>(null)

  const connect = useAction(async () => setLogin(await api<Login>('/kimi-auth-url')), 'Approve the sign-in on Kimi to finish')
  const remove = useAction(
    (account: KimiAccount) => api(`/kimi?auth_index=${encodeURIComponent(account.auth_index)}`, { method: 'DELETE' }),
    'Kimi account removed',
  )

  // The proxy waits for Kimi's approval in the background; ask it how that is going.
  useEffect(() => {
    if (!login) return
    const timer = setInterval(async () => {
      const result = await api<{ status: string; error?: string }>(`/get-auth-status?state=${encodeURIComponent(login.state)}`).catch(
        () => null,
      )
      if (!result || result.status === 'wait') return
      if (result.status === 'ok') {
        toast.success('Kimi account connected')
        void client.invalidateQueries({ queryKey: ['kimi'] })
      } else {
        toast.error(`Kimi sign-in failed: ${result.error}`)
      }
      setLogin(null)
    }, 3000)
    return () => clearInterval(timer)
  }, [login, client])

  if (!accounts.data) return <Pending error={accounts.error} />

  return (
    <Card>
      <CardHeader>
        <CardTitle>Kimi Code</CardTitle>
        <CardDescription>Signed-in accounts serve the kimi-* models on chat completions.</CardDescription>
        <CardAction>
          <Button disabled={connect.isPending} onClick={() => connect.mutate()}>
            <PlusIcon data-icon="inline-start" />
            Connect account
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        {accounts.data.length === 0 ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <MoonStarIcon />
              </EmptyMedia>
              <EmptyTitle>No Kimi account connected</EmptyTitle>
              <EmptyDescription>Connect one and the Kimi models appear in the model list.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>File</TableHead>
                <TableHead>Auth index</TableHead>
                <TableHead>Token expires</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {accounts.data.map((account) => (
                <TableRow key={account.auth_index}>
                  <TableCell className="font-medium">{account.name}</TableCell>
                  <TableCell className="font-mono text-xs">{account.auth_index}</TableCell>
                  <TableCell className="text-muted-foreground">
                    {account.expired ? new Date(account.expired).toLocaleString() : '—'}
                  </TableCell>
                  <TableCell>
                    <StatusBadge disabled={account.disabled} />
                  </TableCell>
                  <TableCell>
                    <Button variant="ghost" size="icon-sm" aria-label={`Remove ${account.name}`} onClick={() => remove.mutate(account)}>
                      <Trash2Icon />
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
      <Dialog open={login !== null} onOpenChange={(open) => !open && setLogin(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Approve the sign-in on Kimi</DialogTitle>
            <DialogDescription>Check that Kimi shows this code, then approve. This window closes by itself.</DialogDescription>
          </DialogHeader>
          <div className="flex items-center justify-center rounded-lg bg-muted py-6">
            <span className="font-mono text-3xl font-medium tracking-widest">{login?.user_code}</span>
          </div>
          <DialogFooter className="sm:justify-between">
            <span className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner />
              Waiting for approval
            </span>
            <Button asChild>
              <a href={login?.url} target="_blank" rel="noreferrer">
                Open Kimi
                <ExternalLinkIcon data-icon="inline-end" />
              </a>
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
