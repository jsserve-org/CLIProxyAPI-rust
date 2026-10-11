import { useQuery } from '@tanstack/react-query'
import { Pending } from '@/components/pending'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import type { Model } from '@/lib/api'
import { useApiKeys } from '@/lib/queries'

export function Models() {
  const apiKeys = useApiKeys()
  const apiKey = apiKeys.data?.[0]
  // The model list is a client endpoint, so it is read with a client key.
  const models = useQuery({
    queryKey: ['models', apiKey],
    enabled: Boolean(apiKey),
    queryFn: async () => {
      const response = await fetch('/v1/models', { headers: { authorization: `Bearer ${apiKey}` } })
      if (!response.ok) throw new Error(`${response.status} ${response.statusText}`)
      return ((await response.json()) as { data: Model[] }).data
    },
  })

  if (apiKeys.data?.length === 0) return <Pending error={new Error('Add an API key first: the model list is read with one.')} />
  if (!models.data) return <Pending error={apiKeys.error ?? models.error} />

  return (
    <Card>
      <CardHeader>
        <CardTitle>Available models</CardTitle>
        <CardDescription>The {models.data.length} models clients see at /v1/models.</CardDescription>
      </CardHeader>
      <CardContent>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Model</TableHead>
              <TableHead>Provider</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {models.data.map((model) => (
              <TableRow key={model.id}>
                <TableCell className="font-mono text-xs">{model.id}</TableCell>
                <TableCell>
                  <Badge variant="secondary">{model.owned_by}</Badge>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}
