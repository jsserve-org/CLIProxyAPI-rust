import { useQuery } from '@tanstack/react-query'
import { useState, type ReactNode } from 'react'
import { Pending } from '@/components/pending'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { api, body } from '@/lib/api'
import { useAction, useConfig } from '@/lib/queries'

const useSave = (path: string) => useAction((value: boolean | number | string) => api(path, body('PUT', { value })), 'Saved')

function Setting({ id, label, description, children }: { id: string; label: string; description: string; children: ReactNode }) {
  return (
    <Field orientation="horizontal">
      <FieldContent>
        <FieldLabel htmlFor={id}>{label}</FieldLabel>
        <FieldDescription>{description}</FieldDescription>
      </FieldContent>
      {children}
    </Field>
  )
}

function Toggle({ path, checked, ...text }: { path: string; label: string; description: string; checked: boolean }) {
  const save = useSave(path)
  return (
    <Setting id={path} {...text}>
      <Switch id={path} checked={checked} disabled={save.isPending} onCheckedChange={(value) => save.mutate(value)} />
    </Setting>
  )
}

/** Saves when you leave the field or press Enter, not on every keystroke. */
function NumberSetting({ path, value, ...text }: { path: string; label: string; description: string; value: number }) {
  const save = useSave(path)
  const [draft, setDraft] = useState(String(value))
  function commit() {
    const next = Number(draft)
    if (draft === '' || !Number.isInteger(next) || next < 0) setDraft(String(value))
    else if (next !== value) save.mutate(next)
  }
  return (
    <Setting id={path} {...text}>
      <Input
        id={path}
        type="number"
        min={0}
        className="w-24 font-mono"
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => event.key === 'Enter' && event.currentTarget.blur()}
      />
    </Setting>
  )
}

function ProxyUrl({ value }: { value: string }) {
  const save = useSave('/proxy-url')
  const clear = useAction(() => api('/proxy-url', { method: 'DELETE' }), 'Proxy removed')
  const [draft, setDraft] = useState(value)
  return (
    <Field>
      <FieldLabel htmlFor="proxy-url">Outbound proxy</FieldLabel>
      <div className="flex gap-2">
        <Input
          id="proxy-url"
          className="font-mono"
          placeholder="socks5://127.0.0.1:1080"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
        <Button variant="outline" disabled={draft === value || !draft} onClick={() => save.mutate(draft)}>
          Save
        </Button>
        <Button
          variant="ghost"
          disabled={!value}
          onClick={() => {
            clear.mutate()
            setDraft('')
          }}
        >
          Remove
        </Button>
      </div>
      <FieldDescription>Upstream requests go through this proxy. Leave empty to connect directly.</FieldDescription>
    </Field>
  )
}

function Strategy() {
  // `/config` reports the raw string; this route normalizes aliases such as "rr".
  const strategy = useQuery({
    queryKey: ['routing-strategy'],
    queryFn: async () => (await api<{ strategy: string }>('/routing/strategy')).strategy,
  })
  const save = useSave('/routing/strategy')
  return (
    <Setting id="strategy" label="Routing strategy" description="How the proxy picks a credential for each request.">
      <Select value={strategy.data ?? ''} onValueChange={(value) => save.mutate(value)}>
        <SelectTrigger id="strategy" className="w-52">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="round-robin">Round robin</SelectItem>
          <SelectItem value="weighted-round-robin">Weighted round robin</SelectItem>
          <SelectItem value="fill-first">Fill first</SelectItem>
        </SelectContent>
      </Select>
    </Setting>
  )
}

function Section({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>
        <FieldGroup>{children}</FieldGroup>
      </CardContent>
    </Card>
  )
}

export function Settings() {
  const { data: config, error } = useConfig()
  if (!config) return <Pending error={error} />

  return (
    <>
      <Section title="Routing" description="Changes apply immediately but are not written to config.yaml.">
        <Strategy />
        <Toggle
          path="/force-model-prefix"
          label="Force model prefix"
          description="Only route a prefixed model name to credentials with that prefix."
          checked={config['force-model-prefix']}
        />
        <ProxyUrl value={config['proxy-url']} />
      </Section>
      <Section title="Retries" description="What happens when an upstream request fails.">
        <NumberSetting
          path="/request-retry"
          label="Request retries"
          description="Times a failed request is retried."
          value={config['request-retry']}
        />
        <NumberSetting
          path="/max-retry-credentials"
          label="Credentials per request"
          description="Most credentials tried for one request."
          value={config['max-retry-credentials']}
        />
        <NumberSetting
          path="/max-retry-interval"
          label="Longest retry wait"
          description="Seconds to wait for a cooling credential before giving up."
          value={config['max-retry-interval']}
        />
      </Section>
      <Section title="Quota" description="What happens when a credential runs out of quota.">
        <Toggle
          path="/quota-exceeded/switch-project"
          label="Switch project"
          description="Move to another project when a quota is exceeded."
          checked={config['quota-exceeded']['switch-project']}
        />
        <Toggle
          path="/quota-exceeded/switch-preview-model"
          label="Switch to preview model"
          description="Fall back to a preview model when a quota is exceeded."
          checked={config['quota-exceeded']['switch-preview-model']}
        />
      </Section>
      <Section title="Logging" description="Diagnostics for the proxy process.">
        <Toggle path="/debug" label="Debug logging" description="Log verbose diagnostics." checked={config.debug} />
        <Toggle
          path="/logging-to-file"
          label="Log to file"
          description="Write logs to files instead of standard output."
          checked={config['logging-to-file']}
        />
        <NumberSetting
          path="/logs-max-total-size-mb"
          label="Log size limit"
          description="Megabytes of log files to keep. 0 means no limit."
          value={config['logs-max-total-size-mb']}
        />
        <NumberSetting
          path="/error-logs-max-files"
          label="Error log files"
          description="Error log files to keep."
          value={config['error-logs-max-files']}
        />
      </Section>
    </>
  )
}
