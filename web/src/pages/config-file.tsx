import { useQuery } from '@tanstack/react-query'
import { diffLines } from 'diff'
import { useEffect, useState } from 'react'
import { Pending } from '@/components/pending'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Textarea } from '@/components/ui/textarea'
import { api, request } from '@/lib/api'
import { useAction } from '@/lib/queries'
import { cn } from '@/lib/utils'

export function ConfigFile() {
  const file = useQuery({ queryKey: ['config.yaml'], queryFn: async () => (await request('/config.yaml')).text() })
  const [draft, setDraft] = useState('')
  const [reviewing, setReviewing] = useState(false)
  useEffect(() => setDraft(file.data ?? ''), [file.data])

  const save = useAction(
    () => api('/config.yaml', { method: 'PUT', headers: { 'content-type': 'application/yaml' }, body: draft }),
    'Config file saved',
  )

  if (file.data === undefined) return <Pending error={file.error} />
  const changed = draft !== file.data

  return (
    <Card>
      <CardHeader>
        <CardTitle>config.yaml</CardTitle>
        <CardDescription>
          Saving validates the file, writes it to disk and applies it. Listener addresses, the auth directory and the outbound
          proxy only change after a restart.
        </CardDescription>
        <CardAction className="flex gap-2">
          <Button variant="outline" disabled={!changed} onClick={() => setDraft(file.data)}>
            Discard changes
          </Button>
          <Button disabled={!changed} onClick={() => setReviewing(true)}>
            Review and save
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        <Textarea
          aria-label="config.yaml"
          spellCheck={false}
          className="min-h-[60vh] font-mono text-xs leading-relaxed"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
      </CardContent>
      <Dialog open={reviewing} onOpenChange={setReviewing}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Save these changes?</DialogTitle>
            <DialogDescription>Lines you removed are red, lines you added are green.</DialogDescription>
          </DialogHeader>
          <pre className="max-h-[50vh] overflow-auto rounded-lg bg-muted p-3 font-mono text-xs">
            {diffLines(file.data, draft)
              .filter((part) => part.added || part.removed)
              .map((part, index) => (
                <span
                  key={index}
                  className={cn('block whitespace-pre-wrap', part.added ? 'text-emerald-600 dark:text-emerald-400' : 'text-destructive')}
                >
                  {part.value
                    .replace(/\n$/, '')
                    .split('\n')
                    .map((line) => `${part.added ? '+' : '-'} ${line}`)
                    .join('\n')}
                </span>
              ))}
          </pre>
          <DialogFooter>
            <Button variant="outline" onClick={() => setReviewing(false)}>
              Keep editing
            </Button>
            <Button
              disabled={save.isPending}
              onClick={() => {
                save.mutate()
                setReviewing(false)
              }}
            >
              Save config file
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  )
}
