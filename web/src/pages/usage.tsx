import { useMutation } from '@tanstack/react-query'
import { ActivityIcon, DownloadIcon } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { Pending } from '@/components/pending'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { Field, FieldContent, FieldDescription, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { api, body, saveBlob, type UsageRecord } from '@/lib/api'
import { useAction, useConfig } from '@/lib/queries'

// Fetching removes records from the server's queue, so keep them across page switches.
let fetched: UsageRecord[] = []

const number = new Intl.NumberFormat()

export function Usage() {
  const { data: config, error } = useConfig()
  const [records, setRecords] = useState(fetched)
  const [count, setCount] = useState('100')

  const toggle = useAction((value: boolean) => api('/usage-statistics-enabled', body('PUT', { value })), 'Saved')
  const fetchRecords = useMutation({
    mutationFn: () => api<UsageRecord[]>(`/usage-queue?count=${encodeURIComponent(count)}`),
    onSuccess: (batch) => {
      fetched = [...batch.reverse(), ...fetched]
      setRecords(fetched)
      toast.success(batch.length ? `Fetched ${number.format(batch.length)} records` : 'The queue is empty')
    },
    onError: (cause) => toast.error(cause.message),
  })

  if (!config) return <Pending error={error} />

  const tokens = records.reduce((sum, record) => sum + record.tokens.total_tokens, 0)
  const failed = records.filter((record) => record.failed).length

  return (
    <>
      <Card>
        <CardContent>
          <Field orientation="horizontal">
            <FieldContent>
              <FieldLabel htmlFor="usage-enabled">Record usage</FieldLabel>
              <FieldDescription>
                Queue one record per request, kept in memory for {number.format(config['redis-usage-queue-retention-seconds'])} seconds.
              </FieldDescription>
            </FieldContent>
            <Switch
              id="usage-enabled"
              checked={config['usage-statistics-enabled']}
              disabled={toggle.isPending}
              onCheckedChange={(value) => toggle.mutate(value)}
            />
          </Field>
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>Request records</CardTitle>
          <CardDescription>
            {records.length
              ? `${number.format(records.length)} requests · ${number.format(tokens)} tokens · ${number.format(failed)} failed`
              : 'Fetching takes records off the proxy queue. Download them if you need to keep them.'}
          </CardDescription>
          <CardAction className="flex gap-2">
            <Input
              aria-label="Records to fetch"
              type="number"
              min={1}
              className="w-20 font-mono"
              value={count}
              onChange={(event) => setCount(event.target.value)}
            />
            <Button disabled={fetchRecords.isPending} onClick={() => fetchRecords.mutate()}>
              Fetch records
            </Button>
            <Button
              variant="outline"
              size="icon"
              aria-label="Download records as JSON"
              disabled={records.length === 0}
              onClick={() => saveBlob(new Blob([JSON.stringify(records, null, 2)], { type: 'application/json' }), 'usage-records.json')}
            >
              <DownloadIcon />
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent>
          {records.length === 0 ? (
            <Empty>
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <ActivityIcon />
                </EmptyMedia>
                <EmptyTitle>No records fetched</EmptyTitle>
                <EmptyDescription>Fetch records to see the latest requests the proxy handled.</EmptyDescription>
              </EmptyHeader>
            </Empty>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Time</TableHead>
                  <TableHead>Model</TableHead>
                  <TableHead>Endpoint</TableHead>
                  <TableHead>Credential</TableHead>
                  <TableHead className="text-right">In</TableHead>
                  <TableHead className="text-right">Out</TableHead>
                  <TableHead className="text-right">Latency</TableHead>
                  <TableHead>Result</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {records.map((record, index) => (
                  <TableRow key={record.request_id || index}>
                    <TableCell className="text-muted-foreground">{new Date(record.timestamp).toLocaleString()}</TableCell>
                    <TableCell className="font-mono text-xs">{record.model}</TableCell>
                    <TableCell className="font-mono text-xs">{record.endpoint}</TableCell>
                    <TableCell className="text-muted-foreground">{record.auth_index || '—'}</TableCell>
                    <TableCell className="text-right tabular-nums">{number.format(record.tokens.input_tokens)}</TableCell>
                    <TableCell className="text-right tabular-nums">{number.format(record.tokens.output_tokens)}</TableCell>
                    <TableCell className="text-right tabular-nums">{number.format(record.latency_ms)} ms</TableCell>
                    <TableCell>
                      {record.failed ? (
                        <Badge variant="destructive">Failed {record.fail.status_code || ''}</Badge>
                      ) : (
                        <Badge variant="outline">OK</Badge>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </>
  )
}
