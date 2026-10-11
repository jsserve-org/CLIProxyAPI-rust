import { useQuery } from '@tanstack/react-query'
import { DownloadIcon, SearchIcon, Trash2Icon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { Pending } from '@/components/pending'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { api, request, saveBlob, type ErrorLogFile } from '@/lib/api'
import { useAction } from '@/lib/queries'
import { cn } from '@/lib/utils'

function ProcessLog() {
  const [live, setLive] = useState(true)
  const [search, setSearch] = useState('')
  const logs = useQuery({
    queryKey: ['logs'],
    queryFn: async () => (await api<{ lines: string[] }>('/logs')).lines,
    refetchInterval: live ? 2000 : false,
  })
  const clear = useAction(() => api('/logs', { method: 'DELETE' }), 'Logs cleared')

  if (!logs.data) return <Pending error={logs.error} />

  const needle = search.trim().toLowerCase()
  const lines = needle ? logs.data.filter((line) => line.toLowerCase().includes(needle)) : logs.data

  return (
    <Card>
      <CardHeader>
        <CardTitle>Process log</CardTitle>
        <CardDescription>The last {logs.data.length.toLocaleString()} lines, held in memory since the proxy started.</CardDescription>
        <CardAction className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <Switch id="logs-live" checked={live} onCheckedChange={setLive} />
            <Label htmlFor="logs-live">Follow</Label>
          </div>
          <Button
            variant="outline"
            size="icon"
            aria-label="Download log"
            disabled={logs.data.length === 0}
            onClick={() => saveBlob(new Blob([logs.data.join('\n')], { type: 'text/plain' }), 'cliproxyapi.log')}
          >
            <DownloadIcon />
          </Button>
          <Button variant="outline" size="icon" aria-label="Clear log" disabled={clear.isPending} onClick={() => clear.mutate()}>
            <Trash2Icon />
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <InputGroup>
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput aria-label="Search log" placeholder="Search" value={search} onChange={(event) => setSearch(event.target.value)} />
        </InputGroup>
        {/* Newest first, so following the log never needs scrolling. */}
        <div className="max-h-[65vh] overflow-auto rounded-lg bg-muted p-3 font-mono text-xs leading-relaxed">
          {lines.length === 0 && <span className="text-muted-foreground">{needle ? 'No lines match.' : 'Nothing logged yet.'}</span>}
          {lines.toReversed().map((line, index) => (
            <div
              key={lines.length - index}
              className={cn(
                'whitespace-pre-wrap break-all',
                line.includes(' ERROR ') && 'text-destructive',
                line.includes(' WARN ') && 'text-amber-600 dark:text-amber-400',
              )}
            >
              {line}
            </div>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

function FailedRequests() {
  const files = useQuery({
    queryKey: ['request-error-logs'],
    queryFn: async () => (await api<{ files: ErrorLogFile[] }>('/request-error-logs')).files,
  })
  if (!files.data) return <Pending error={files.error} />

  async function download(name: string) {
    try {
      saveBlob(await (await request(`/request-error-logs/${encodeURIComponent(name)}`)).blob(), name)
    } catch (error) {
      toast.error((error as Error).message)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Failed requests</CardTitle>
        <CardDescription>
          The last {files.data.length} requests that failed, with status, model, credential and upstream error. Kept in memory while
          usage recording is on.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {files.data.length === 0 ? (
          <p className="text-sm text-muted-foreground">No request has failed since the proxy started.</p>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>When</TableHead>
                <TableHead>Record</TableHead>
                <TableHead className="text-right">Size</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {files.data.map((file) => (
                <TableRow key={file.name}>
                  <TableCell className="text-muted-foreground">{new Date(file.modified * 1000).toLocaleString()}</TableCell>
                  <TableCell className="font-mono text-xs">{file.name}</TableCell>
                  <TableCell className="text-right tabular-nums">{file.size.toLocaleString()} B</TableCell>
                  <TableCell>
                    <Button variant="ghost" size="icon-sm" aria-label={`Download ${file.name}`} onClick={() => download(file.name)}>
                      <DownloadIcon />
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

export function Logs() {
  return (
    <Tabs defaultValue="process">
      <TabsList>
        <TabsTrigger value="process">Process log</TabsTrigger>
        <TabsTrigger value="failed">Failed requests</TabsTrigger>
      </TabsList>
      <TabsContent value="process">
        <ProcessLog />
      </TabsContent>
      <TabsContent value="failed">
        <FailedRequests />
      </TabsContent>
    </Tabs>
  )
}
