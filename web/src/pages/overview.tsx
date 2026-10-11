import { ArrowRightIcon } from 'lucide-react'
import { Pending } from '@/components/pending'
import { Throughput } from '@/components/throughput'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { request } from '@/lib/api'
import { useApiKeys, useConfig, useCredentials } from '@/lib/queries'

function Stat({ label, value, hint }: { label: string; value: string | number; hint: string }) {
  return (
    <Card>
      <CardHeader>
        <CardDescription>{label}</CardDescription>
        <CardTitle className="text-2xl font-semibold tabular-nums">{value}</CardTitle>
      </CardHeader>
      <CardContent className="text-xs text-muted-foreground">{hint}</CardContent>
    </Card>
  )
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-4 py-2 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate font-mono text-xs">{value}</dd>
    </div>
  )
}

export function Overview() {
  const config = useConfig()
  const credentials = useCredentials()
  const apiKeys = useApiKeys()
  const version = useQuery({
    queryKey: ['version'],
    queryFn: async () => (await request('/config')).headers.get('x-cpa-version') ?? 'unknown',
  })
  if (!config.data || !credentials.data || !apiKeys.data) {
    return <Pending error={config.error ?? credentials.error ?? apiKeys.error} />
  }

  const active = credentials.data.filter((credential) => !credential.disabled).length
  const fallback = Object.entries(config.data['model-fallback']).filter(([, chain]) => chain.length > 0)

  return (
    <>
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Stat label="Active credentials" value={active} hint={`${credentials.data.length} auth files loaded`} />
        <Stat label="API keys" value={apiKeys.data.length} hint="accepted on the public listener" />
        <Stat label="Max concurrency" value={config.data['max-concurrency']} hint="requests in flight" />
        <Stat label="Request retries" value={config.data['request-retry']} hint="per failed request" />
      </div>
      <Throughput recording={config.data['usage-statistics-enabled']} />
      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Model fallback</CardTitle>
            <CardDescription>
              When a model's credentials are all cooling down, requests move along its chain.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            {fallback.length === 0 && <p className="text-sm text-muted-foreground">No fallback chains configured.</p>}
            {fallback.map(([model, chain]) => (
              <div key={model} className="flex flex-wrap items-center gap-1.5">
                <Badge className="font-mono">{model === 'default' ? 'any other model' : model}</Badge>
                {chain.map((next) => (
                  <span key={next} className="flex items-center gap-1.5">
                    <ArrowRightIcon className="size-3.5 text-muted-foreground" />
                    <Badge variant="secondary" className="font-mono">
                      {next}
                    </Badge>
                  </span>
                ))}
              </div>
            ))}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Runtime</CardTitle>
            <CardDescription>Read from config.yaml at startup.</CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="divide-y">
              <Fact label="Version" value={version.data ?? '…'} />
              <Fact label="Public listener" value={`${config.data.host}:${config.data.port}`} />
              <Fact label="Admin listener" value={`${config.data['admin-host']}:${config.data['admin-port']}`} />
              <Fact label="Auth directory" value={config.data['auth-dir']} />
              <Fact label="Routing strategy" value={config.data.routing.strategy || 'round-robin'} />
              <Fact
                label="Session affinity"
                value={config.data.routing['session-affinity'] ? config.data.routing['session-affinity-ttl'] : 'off'}
              />
              <Fact label="Credential cooldown" value={config.data['disable-cooling'] ? 'off' : 'on'} />
            </dl>
          </CardContent>
        </Card>
      </div>
    </>
  )
}
