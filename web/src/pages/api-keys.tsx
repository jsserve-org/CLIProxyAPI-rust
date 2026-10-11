import { CopyIcon, EyeIcon, EyeOffIcon, KeyRoundIcon, PencilIcon, PlusIcon, SparklesIcon, Trash2Icon } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { toast } from 'sonner'
import { Pending } from '@/components/pending'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupInput } from '@/components/ui/input-group'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { api, body } from '@/lib/api'
import { useAction, useApiKeys } from '@/lib/queries'

const mask = (key: string) => (key.length > 8 ? `${key.slice(0, 4)}${'•'.repeat(12)}${key.slice(-4)}` : '•'.repeat(12))

function generateKey() {
  const bytes = crypto.getRandomValues(new Uint8Array(24))
  return `sk-${[...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('')}`
}

export function ApiKeys() {
  const apiKeys = useApiKeys()
  const [revealed, setRevealed] = useState(false)
  // `old` is the key being replaced; an empty `old` means a new key.
  const [editing, setEditing] = useState<{ old: string; value: string } | null>(null)

  const save = useAction(
    (edit: { old: string; value: string }) => api('/api-keys', body('PATCH', { old: edit.old, new: edit.value })),
    'API key saved',
  )
  const remove = useAction(
    (key: string) => api(`/api-keys?value=${encodeURIComponent(key)}`, { method: 'DELETE' }),
    'API key deleted',
  )

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!editing) return
    save.mutate({ old: editing.old, value: editing.value.trim() })
    setEditing(null)
  }

  if (!apiKeys.data) return <Pending error={apiKeys.error} />

  return (
    <Card>
      <CardHeader>
        <CardTitle>API keys</CardTitle>
        <CardDescription>
          Clients send one of these as a bearer token. Changes apply immediately but are not written to config.yaml.
        </CardDescription>
        <CardAction className="flex gap-2">
          <Button variant="outline" onClick={() => setRevealed(!revealed)}>
            {revealed ? <EyeOffIcon data-icon="inline-start" /> : <EyeIcon data-icon="inline-start" />}
            {revealed ? 'Hide keys' : 'Show keys'}
          </Button>
          <Button onClick={() => setEditing({ old: '', value: '' })}>
            <PlusIcon data-icon="inline-start" />
            Add key
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        {apiKeys.data.length === 0 ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <KeyRoundIcon />
              </EmptyMedia>
              <EmptyTitle>No API keys</EmptyTitle>
              <EmptyDescription>Every request to the public listener is rejected until you add one.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Key</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {apiKeys.data.map((key) => (
                <TableRow key={key}>
                  <TableCell className="font-mono text-xs">{revealed ? key : mask(key)}</TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label="Copy key"
                        onClick={() => navigator.clipboard.writeText(key).then(() => toast.success('API key copied'))}
                      >
                        <CopyIcon />
                      </Button>
                      <Button variant="ghost" size="icon-sm" aria-label="Edit key" onClick={() => setEditing({ old: key, value: key })}>
                        <PencilIcon />
                      </Button>
                      <Button variant="ghost" size="icon-sm" aria-label="Delete key" onClick={() => remove.mutate(key)}>
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
      <Dialog open={editing !== null} onOpenChange={(open) => !open && setEditing(null)}>
        <DialogContent>
          <form onSubmit={submit} className="contents">
            <DialogHeader>
              <DialogTitle>{editing?.old ? 'Edit API key' : 'Add API key'}</DialogTitle>
              <DialogDescription>
                {editing?.old ? 'Clients using the old value stop working as soon as you save.' : 'Paste a key or generate a random one.'}
              </DialogDescription>
            </DialogHeader>
            <InputGroup>
              <InputGroupInput
                aria-label="API key"
                className="font-mono"
                autoFocus
                value={editing?.value ?? ''}
                onChange={(event) => editing && setEditing({ ...editing, value: event.target.value })}
              />
              <InputGroupAddon align="inline-end">
                <InputGroupButton onClick={() => editing && setEditing({ ...editing, value: generateKey() })}>
                  <SparklesIcon />
                  Generate
                </InputGroupButton>
              </InputGroupAddon>
            </InputGroup>
            <DialogFooter>
              <Button type="submit" disabled={!editing?.value.trim()}>
                Save key
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
