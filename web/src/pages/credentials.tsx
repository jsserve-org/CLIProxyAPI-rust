import {
  DownloadIcon,
  EllipsisIcon,
  ExternalLinkIcon,
  FileKeyIcon,
  LogInIcon,
  RotateCcwIcon,
  SearchIcon,
  TimerResetIcon,
  Trash2Icon,
  UploadIcon,
} from 'lucide-react'
import { useRef, useState } from 'react'
import { toast } from 'sonner'
import { Pending } from '@/components/pending'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, FieldDescription, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { Switch } from '@/components/ui/switch'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { api, body, request, saveBlob, type Credential } from '@/lib/api'
import { useAction, useCredentials } from '@/lib/queries'

export function Credentials() {
  const credentials = useCredentials()
  const fileInput = useRef<HTMLInputElement>(null)
  const [deleting, setDeleting] = useState<Credential[]>([])
  const [search, setSearch] = useState('')
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set())
  // A Codex sign-in in progress: the page to open and the redirect URL pasted back.
  const [signIn, setSignIn] = useState<{ url: string; redirect: string } | null>(null)

  const startSignIn = useAction(
    async () => setSignIn({ url: (await api<{ url: string }>('/codex-auth-url')).url, redirect: '' }),
    'Sign-in page ready',
  )
  const finishSignIn = useAction(async (redirect: string) => {
    await api('/oauth-callback', body('POST', { provider: 'codex', redirect_url: redirect }))
    setSignIn(null)
  }, 'Codex account added')

  const upload = useAction(async (files: File[]) => {
    for (const file of files) {
      const form = new FormData()
      form.append('file', file)
      await api('/auth-files', { method: 'POST', body: form })
    }
  }, 'Credential uploaded')
  const reload = useAction(() => api('/auth-files/refresh', { method: 'POST' }), 'Reloaded credentials from disk')
  const setDisabled = useAction(async (change: { credentials: Credential[]; disabled: boolean }) => {
    for (const credential of change.credentials) {
      await api('/auth-files/status', body('PATCH', { name: credential.name, auth_index: credential.auth_index, disabled: change.disabled }))
    }
  }, 'Credentials updated')
  const resetQuota = useAction(
    (credential: Credential) => api('/reset-quota', body('POST', { auth_index: credential.auth_index })),
    'Quota cooldown reset',
  )
  const remove = useAction(async (credentials: Credential[]) => {
    for (const credential of credentials) {
      await api(`/auth-files?name=${encodeURIComponent(credential.name)}&auth_index=${encodeURIComponent(credential.auth_index)}`, {
        method: 'DELETE',
      })
    }
    setSelected(new Set())
  }, 'Credentials deleted')

  async function download(credential: Credential) {
    try {
      const response = await request(`/auth-files/download?name=${encodeURIComponent(credential.name)}`)
      saveBlob(await response.blob(), credential.name)
    } catch (error) {
      toast.error((error as Error).message)
    }
  }

  if (!credentials.data) return <Pending error={credentials.error} />

  const needle = search.trim().toLowerCase()
  const visible = credentials.data.filter((credential) => `${credential.name} ${credential.email}`.toLowerCase().includes(needle))
  const picked = credentials.data.filter((credential) => selected.has(credential.auth_index))
  const allPicked = visible.length > 0 && visible.every((credential) => selected.has(credential.auth_index))

  function pick(credentials: Credential[], on: boolean) {
    const next = new Set(selected)
    for (const credential of credentials) {
      if (on) next.add(credential.auth_index)
      else next.delete(credential.auth_index)
    }
    setSelected(next)
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Auth files</CardTitle>
        <CardDescription>Codex credentials the proxy routes requests through.</CardDescription>
        <CardAction className="flex gap-2">
          <Button variant="outline" disabled={reload.isPending} onClick={() => reload.mutate()}>
            <RotateCcwIcon data-icon="inline-start" />
            Reload from disk
          </Button>
          <Button variant="outline" disabled={upload.isPending} onClick={() => fileInput.current?.click()}>
            <UploadIcon data-icon="inline-start" />
            Upload
          </Button>
          <Button disabled={startSignIn.isPending} onClick={() => startSignIn.mutate()}>
            <LogInIcon data-icon="inline-start" />
            Sign in with Codex
          </Button>
          <input
            ref={fileInput}
            type="file"
            accept=".json,application/json"
            multiple
            hidden
            onChange={(event) => {
              upload.mutate([...(event.target.files ?? [])])
              event.target.value = ''
            }}
          />
        </CardAction>
      </CardHeader>
      <CardContent>
        {credentials.data.length === 0 ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <FileKeyIcon />
              </EmptyMedia>
              <EmptyTitle>No auth files yet</EmptyTitle>
              <EmptyDescription>Upload a Codex auth JSON file to start routing requests.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <div className="flex flex-col gap-3">
            <div className="flex flex-wrap items-center gap-2">
              <InputGroup className="max-w-xs">
                <InputGroupAddon>
                  <SearchIcon />
                </InputGroupAddon>
                <InputGroupInput
                  aria-label="Search credentials"
                  placeholder="Search by file or account"
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                />
              </InputGroup>
              {picked.length > 0 && (
                <>
                  <span className="ml-auto text-sm text-muted-foreground">{picked.length} selected</span>
                  <Button variant="outline" size="sm" onClick={() => setDisabled.mutate({ credentials: picked, disabled: false })}>
                    Enable
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setDisabled.mutate({ credentials: picked, disabled: true })}>
                    Disable
                  </Button>
                  <Button variant="destructive" size="sm" onClick={() => setDeleting(picked)}>
                    Delete
                  </Button>
                </>
              )}
            </div>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-0">
                  <Checkbox aria-label="Select all" checked={allPicked} onCheckedChange={(on) => pick(visible, on === true)} />
                </TableHead>
                <TableHead>File</TableHead>
                <TableHead>Account</TableHead>
                <TableHead>Auth index</TableHead>
                <TableHead>Enabled</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {visible.map((credential) => (
                <TableRow key={credential.auth_index}>
                  <TableCell>
                    <Checkbox
                      aria-label={`Select ${credential.name}`}
                      checked={selected.has(credential.auth_index)}
                      onCheckedChange={(on) => pick([credential], on === true)}
                    />
                  </TableCell>
                  <TableCell className="font-medium">{credential.name}</TableCell>
                  <TableCell className="text-muted-foreground">{credential.email || credential.account_id || '—'}</TableCell>
                  <TableCell className="font-mono text-xs">{credential.auth_index}</TableCell>
                  <TableCell>
                    <Switch
                      aria-label={`Enable ${credential.name}`}
                      checked={!credential.disabled}
                      onCheckedChange={(on) => setDisabled.mutate({ credentials: [credential], disabled: !on })}
                    />
                  </TableCell>
                  <TableCell>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${credential.name}`}>
                          <EllipsisIcon />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem onSelect={() => download(credential)}>
                          <DownloadIcon />
                          Download
                        </DropdownMenuItem>
                        <DropdownMenuItem onSelect={() => resetQuota.mutate(credential)}>
                          <TimerResetIcon />
                          Reset quota cooldown
                        </DropdownMenuItem>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem variant="destructive" onSelect={() => setDeleting([credential])}>
                          <Trash2Icon />
                          Delete
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          </div>
        )}
      </CardContent>
      <AlertDialog open={deleting.length > 0} onOpenChange={(open) => !open && setDeleting([])}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {deleting.length === 1 ? deleting[0].name : `${deleting.length} auth files`}?</AlertDialogTitle>
            <AlertDialogDescription>
              They are removed from the auth directory and the proxy stops using them. Download them first if you need a copy.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={() => remove.mutate(deleting)}>
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Dialog open={signIn !== null} onOpenChange={(open) => !open && setSignIn(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Sign in with Codex</DialogTitle>
            <DialogDescription>
              Sign in on OpenAI's page. Your browser then lands on a localhost:1455 address that may fail to load. Copy that
              address and paste it below.
            </DialogDescription>
          </DialogHeader>
          <Button variant="outline" asChild>
            <a href={signIn?.url} target="_blank" rel="noreferrer">
              Open the sign-in page
              <ExternalLinkIcon data-icon="inline-end" />
            </a>
          </Button>
          <Field>
            <FieldLabel htmlFor="codex-redirect">Address you were sent to</FieldLabel>
            <Input
              id="codex-redirect"
              className="font-mono"
              placeholder="http://localhost:1455/auth/callback?code=…"
              value={signIn?.redirect ?? ''}
              onChange={(event) => signIn && setSignIn({ ...signIn, redirect: event.target.value })}
            />
            <FieldDescription>The sign-in expires after 10 minutes.</FieldDescription>
          </Field>
          <DialogFooter>
            <Button
              disabled={!signIn?.redirect.trim() || finishSignIn.isPending}
              onClick={() => signIn && finishSignIn.mutate(signIn.redirect.trim())}
            >
              Add account
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
