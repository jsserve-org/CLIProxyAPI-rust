import { useQueries } from '@tanstack/react-query'
import { GaugeIcon, RefreshCwIcon } from 'lucide-react'
import { Pending } from '@/components/pending'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { api, body, type ApiCallResult, type CodexUsage, type Credential, type RateLimit } from '@/lib/api'
import { useCredentials } from '@/lib/queries'

/** Ask ChatGPT for a credential's usage, through the proxy so the token never reaches the browser. */
async function fetchUsage(credential: Credential): Promise<CodexUsage> {
  const result = await api<ApiCallResult>(
    '/api-call',
    body('POST', {
      auth_index: credential.auth_index,
      method: 'GET',
      url: 'https://chatgpt.com/backend-api/wham/usage',
      header: {
        Authorization: 'Bearer $TOKEN$',
        'User-Agent': 'codex-cli/1.0.0',
        ...(credential.account_id && { 'Chatgpt-Account-Id': credential.account_id }),
      },
    }),
  )
  if (result.status_code !== 200) throw new Error(`ChatGPT answered ${result.status_code}`)
  return JSON.parse(result.body) as CodexUsage
}

function windowName(seconds: number | null) {
  if (!seconds) return 'Window'
  const hours = Math.round(seconds / 3600)
  return hours >= 24 ? `${Math.round(hours / 24)}-day window` : `${hours}-hour window`
}

function Windows({ label, limit }: { label: string; limit?: RateLimit | null }) {
  const windows = [limit?.primary_window, limit?.secondary_window].filter((window) => window?.used_percent != null)
  return windows.map((window, index) => (
    <div key={`${label}-${index}`} className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-4 text-sm">
        <span>
          {label} {windowName(window!.limit_window_seconds).toLowerCase()}
        </span>
        <span className="text-xs text-muted-foreground tabular-nums">
          {Math.round(window!.used_percent!)}% used
          {window!.reset_at ? ` · resets ${new Date(window!.reset_at * 1000).toLocaleString()}` : ''}
        </span>
      </div>
      <Progress value={Math.min(100, window!.used_percent!)} />
    </div>
  ))
}

export function Quota() {
  const credentials = useCredentials()
  const usage = useQueries({
    queries: (credentials.data ?? []).map((credential) => ({
      queryKey: ['quota', credential.auth_index],
      queryFn: () => fetchUsage(credential),
      staleTime: 60_000,
    })),
  })

  if (!credentials.data) return <Pending error={credentials.error} />
  if (credentials.data.length === 0) {
    return (
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <GaugeIcon />
          </EmptyMedia>
          <EmptyTitle>No credentials to check</EmptyTitle>
          <EmptyDescription>Add a Codex credential and its quota shows up here.</EmptyDescription>
        </EmptyHeader>
      </Empty>
    )
  }

  return credentials.data.map((credential, index) => {
    const { data, error, isFetching, refetch } = usage[index]
    return (
      <Card key={credential.auth_index}>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            {credential.email || credential.name}
            {data?.plan_type && <Badge variant="secondary">{data.plan_type}</Badge>}
            {credential.disabled && <Badge variant="outline">Disabled</Badge>}
          </CardTitle>
          <CardDescription>{credential.name}</CardDescription>
          <CardAction>
            <Button variant="ghost" size="icon-sm" aria-label={`Refresh quota for ${credential.name}`} disabled={isFetching} onClick={() => refetch()}>
              <RefreshCwIcon className={isFetching ? 'animate-spin' : undefined} />
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          {error ? (
            <p className="text-sm text-destructive">Couldn't read the quota: {error.message}</p>
          ) : !data ? (
            <Skeleton className="h-10" />
          ) : (
            <>
              <Windows label="Codex" limit={data.rate_limit} />
              <Windows label="Code review" limit={data.code_review_rate_limit} />
              {data.additional_rate_limits?.map((extra, extraIndex) => (
                <Windows key={extraIndex} label={extra.limit_name || extra.metered_feature || 'Additional'} limit={extra.rate_limit} />
              ))}
            </>
          )}
        </CardContent>
      </Card>
    )
  })
}
