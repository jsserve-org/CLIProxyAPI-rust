import { useMutation } from '@tanstack/react-query'
import { SendIcon } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field, FieldDescription, FieldGroup, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Spinner } from '@/components/ui/spinner'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'
import { api, body, type ApiCallResult } from '@/lib/api'
import { useCredentials } from '@/lib/queries'

const NO_CREDENTIAL = 'none'

function pretty(text: string) {
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return text
  }
}

/** One `Name: value` header per line. */
function parseHeaders(text: string) {
  const headers: Record<string, string> = {}
  for (const line of text.split('\n')) {
    const colon = line.indexOf(':')
    if (colon > 0) headers[line.slice(0, colon).trim()] = line.slice(colon + 1).trim()
  }
  return headers
}

export function ApiCall() {
  const credentials = useCredentials()
  const [authIndex, setAuthIndex] = useState(NO_CREDENTIAL)
  const [method, setMethod] = useState('GET')
  const [url, setUrl] = useState('')
  const [headers, setHeaders] = useState('Authorization: Bearer $TOKEN$')
  const [data, setData] = useState('')

  const send = useMutation({
    mutationFn: () =>
      api<ApiCallResult>(
        '/api-call',
        body('POST', {
          auth_index: authIndex === NO_CREDENTIAL ? undefined : authIndex,
          method,
          url,
          header: parseHeaders(headers),
          data,
        }),
      ),
    onError: (error) => toast.error(error.message),
  })

  function submit(event: FormEvent) {
    event.preventDefault()
    send.mutate()
  }

  return (
    <>
      <Card>
        <CardHeader>
          <CardTitle>Call an upstream API</CardTitle>
          <CardDescription>
            Send a request from the proxy with a stored credential. Only HTTPS hosts listed under{' '}
            <code className="font-mono">management-allowed-hosts</code> are accepted.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit}>
            <FieldGroup>
              <div className="flex gap-2">
                <Select value={method} onValueChange={setMethod}>
                  <SelectTrigger aria-label="Method" className="w-28 font-mono">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].map((name) => (
                      <SelectItem key={name} value={name}>
                        {name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Input
                  aria-label="URL"
                  type="url"
                  required
                  className="font-mono"
                  placeholder="https://api.openai.com/v1/models"
                  value={url}
                  onChange={(event) => setUrl(event.target.value)}
                />
                <Button type="submit" disabled={send.isPending}>
                  {send.isPending ? <Spinner data-icon="inline-start" /> : <SendIcon data-icon="inline-start" />}
                  Send
                </Button>
              </div>
              <Field>
                <FieldLabel htmlFor="call-credential">Credential</FieldLabel>
                <Select value={authIndex} onValueChange={setAuthIndex}>
                  <SelectTrigger id="call-credential">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={NO_CREDENTIAL}>No credential</SelectItem>
                    {credentials.data?.map((credential) => (
                      <SelectItem key={credential.auth_index} value={credential.auth_index}>
                        {credential.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <FieldDescription>
                  <code className="font-mono">$TOKEN$</code> in a header is replaced with this credential's access token.
                </FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="call-headers">Headers</FieldLabel>
                <Textarea
                  id="call-headers"
                  className="font-mono"
                  rows={3}
                  value={headers}
                  onChange={(event) => setHeaders(event.target.value)}
                />
                <FieldDescription>One per line, as Name: value.</FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="call-body">Body</FieldLabel>
                <Textarea id="call-body" className="font-mono" rows={5} value={data} onChange={(event) => setData(event.target.value)} />
              </Field>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
      {send.data && (
        <Card>
          <CardHeader>
            <CardTitle>Response</CardTitle>
            <CardAction>
              <Badge variant={send.data.status_code < 400 ? 'secondary' : 'destructive'} className="font-mono">
                {send.data.status_code}
              </Badge>
            </CardAction>
          </CardHeader>
          <CardContent>
            <Tabs defaultValue="body">
              <TabsList>
                <TabsTrigger value="body">Body</TabsTrigger>
                <TabsTrigger value="headers">Headers</TabsTrigger>
              </TabsList>
              <TabsContent value="body">
                <pre className="max-h-96 overflow-auto rounded-lg bg-muted p-3 font-mono text-xs">{pretty(send.data.body)}</pre>
              </TabsContent>
              <TabsContent value="headers">
                <pre className="max-h-96 overflow-auto rounded-lg bg-muted p-3 font-mono text-xs">
                  {Object.entries(send.data.header)
                    .map(([name, values]) => `${name}: ${values.join(', ')}`)
                    .join('\n')}
                </pre>
              </TabsContent>
            </Tabs>
          </CardContent>
        </Card>
      )}
    </>
  )
}
