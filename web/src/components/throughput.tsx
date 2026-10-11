import { useQuery } from '@tanstack/react-query'
import { Bar, BarChart, CartesianGrid, XAxis } from 'recharts'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { ChartContainer, ChartTooltip, ChartTooltipContent, type ChartConfig } from '@/components/ui/chart'
import { api, type Throughput as Stats } from '@/lib/api'

const chartConfig = {
  ok: { label: 'Succeeded', color: 'var(--primary)' },
  failed: { label: 'Failed', color: 'var(--destructive)' },
} satisfies ChartConfig

const MINUTES = 60

/** Requests per minute over the last hour, refreshed while the page is open. */
export function Throughput({ recording }: { recording: boolean }) {
  const stats = useQuery({
    queryKey: ['usage'],
    queryFn: async () => (await api<{ usage: Stats }>('/usage')).usage,
    refetchInterval: 10_000,
  })

  // The server only reports minutes that saw traffic; fill the quiet ones with zeroes.
  const seen = new Map(stats.data?.minutes.map((minute) => [minute.time, minute]))
  const now = Math.floor(Date.now() / 60_000) * 60
  const points = Array.from({ length: MINUTES }, (_, index) => {
    const time = now - (MINUTES - 1 - index) * 60
    const minute = seen.get(time)
    return { time, ok: minute ? minute.requests - minute.failed : 0, failed: minute?.failed ?? 0 }
  })
  const clock = (time: number) => new Date(time * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

  return (
    <Card>
      <CardHeader>
        <CardTitle>Requests in the last hour</CardTitle>
        <CardDescription>
          {recording
            ? `${(stats.data?.total_requests ?? 0).toLocaleString()} requests · ${(stats.data?.failure_count ?? 0).toLocaleString()} failed · ${(stats.data?.total_tokens ?? 0).toLocaleString()} tokens`
            : 'Usage recording is off, so nothing is counted. Turn it on under Usage.'}
        </CardDescription>
      </CardHeader>
      <CardContent>
        <ChartContainer config={chartConfig} className="aspect-auto h-48 w-full">
          <BarChart data={points}>
            <CartesianGrid vertical={false} />
            <XAxis dataKey="time" tickFormatter={clock} tickLine={false} axisLine={false} minTickGap={48} />
            <ChartTooltip content={<ChartTooltipContent labelFormatter={(_, items) => clock(items[0]?.payload.time)} />} />
            <Bar dataKey="ok" stackId="requests" fill="var(--color-ok)" />
            <Bar dataKey="failed" stackId="requests" fill="var(--color-failed)" />
          </BarChart>
        </ChartContainer>
      </CardContent>
    </Card>
  )
}
