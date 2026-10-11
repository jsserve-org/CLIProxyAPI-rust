import { useQuery } from '@tanstack/react-query'
import { CloudIcon, PencilIcon, PlusIcon, Trash2Icon } from 'lucide-react'
import { useState, type FormEvent, type ReactNode } from 'react'
import { Pending } from '@/components/pending'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from '@/components/ui/empty'
import { Field, FieldDescription, FieldGroup, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { Textarea } from '@/components/ui/textarea'
import { api, body, formatModels, parseModels, type ClaudeKey, type ModelAlias, type OpenAiProvider } from '@/lib/api'
import { useAction } from '@/lib/queries'

const PRESETS: Record<string, string> = {
  'Google Gemini': 'https://generativelanguage.googleapis.com/v1beta/openai',
  OpenRouter: 'https://openrouter.ai/api/v1',
  OpenAI: 'https://api.openai.com/v1',
  xAI: 'https://api.x.ai/v1',
}

const mask = (key: string) => (key.length > 8 ? `${key.slice(0, 4)}••••${key.slice(-4)}` : '••••')

/** What the dialog edits: the row being replaced (-1 for a new one) and its fields as text. */
type Draft = { index: number; name: string; baseUrl: string; keys: string; models: string }

function ModelBadges({ models }: { models: ModelAlias[] }) {
  return (
    <div className="flex flex-wrap gap-1">
      {models.map((model) => (
        <Badge key={model.name + model.alias} variant="secondary" className="font-mono">
          {model.alias || model.name}
        </Badge>
      ))}
    </div>
  )
}

function ModelsField({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  return (
    <Field>
      <FieldLabel htmlFor="provider-models">Models</FieldLabel>
      <Textarea id="provider-models" className="font-mono" rows={4} value={value} onChange={(event) => onChange(event.target.value)} />
      <FieldDescription>
        One per line. Write <code className="font-mono">upstream-name = alias</code> to give clients a different name.
      </FieldDescription>
    </Field>
  )
}

function Section(props: { title: string; description: string; onAdd: () => void; empty: string; rows: number; children: ReactNode }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{props.title}</CardTitle>
        <CardDescription>{props.description}</CardDescription>
        <CardAction>
          <Button onClick={props.onAdd}>
            <PlusIcon data-icon="inline-start" />
            Add
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent>
        {props.rows === 0 ? (
          <Empty>
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <CloudIcon />
              </EmptyMedia>
              <EmptyTitle>Nothing configured</EmptyTitle>
              <EmptyDescription>{props.empty}</EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          props.children
        )}
      </CardContent>
    </Card>
  )
}

function RowActions({ label, onEdit, onDelete }: { label: string; onEdit: () => void; onDelete: () => void }) {
  return (
    <div className="flex justify-end gap-1">
      <Button variant="ghost" size="icon-sm" aria-label={`Edit ${label}`} onClick={onEdit}>
        <PencilIcon />
      </Button>
      <Button variant="ghost" size="icon-sm" aria-label={`Delete ${label}`} onClick={onDelete}>
        <Trash2Icon />
      </Button>
    </div>
  )
}

export function Providers() {
  const openai = useQuery({
    queryKey: ['openai-compatibility'],
    queryFn: async () => (await api<{ 'openai-compatibility': OpenAiProvider[] }>('/openai-compatibility'))['openai-compatibility'],
  })
  const claude = useQuery({
    queryKey: ['claude-api-key'],
    queryFn: async () => (await api<{ 'claude-api-key': ClaudeKey[] }>('/claude-api-key'))['claude-api-key'],
  })
  const saveOpenai = useAction((list: OpenAiProvider[]) => api('/openai-compatibility', body('PUT', list)), 'Providers saved')
  const saveClaude = useAction((list: ClaudeKey[]) => api('/claude-api-key', body('PUT', list)), 'Claude keys saved')
  const [provider, setProvider] = useState<Draft | null>(null)
  const [claudeKey, setClaudeKey] = useState<Draft | null>(null)

  if (!openai.data || !claude.data) return <Pending error={openai.error ?? claude.error} />
  const providers = openai.data
  const claudeKeys = claude.data

  /** Replace the drafted row, or append it when it is new. */
  const place = <T,>(list: T[], index: number, item: T) => (index < 0 ? [...list, item] : list.map((old, at) => (at === index ? item : old)))

  function submitProvider(event: FormEvent) {
    event.preventDefault()
    if (!provider) return
    saveOpenai.mutate(
      place(providers, provider.index, {
        name: provider.name.trim(),
        'base-url': provider.baseUrl.trim(),
        'api-key-entries': provider.keys
          .split('\n')
          .map((key) => key.trim())
          .filter(Boolean)
          .map((key) => ({ 'api-key': key })),
        models: parseModels(provider.models),
      }),
    )
    setProvider(null)
  }

  function submitClaudeKey(event: FormEvent) {
    event.preventDefault()
    if (!claudeKey) return
    saveClaude.mutate(
      place(claudeKeys, claudeKey.index, {
        'api-key': claudeKey.keys.trim(),
        'base-url': claudeKey.baseUrl.trim(),
        models: parseModels(claudeKey.models),
      }),
    )
    setClaudeKey(null)
  }

  return (
    <>
      <Section
        title="OpenAI-compatible providers"
        description="Chat completion requests for these models are forwarded with the provider's key. Changes apply immediately but are not written to config.yaml."
        empty="Add Gemini, OpenRouter or any endpoint that speaks the OpenAI chat API."
        rows={providers.length}
        onAdd={() => setProvider({ index: -1, name: '', baseUrl: '', keys: '', models: '' })}
      >
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Provider</TableHead>
              <TableHead>Base URL</TableHead>
              <TableHead>Keys</TableHead>
              <TableHead>Models</TableHead>
              <TableHead className="w-0" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {providers.map((item, index) => (
              <TableRow key={item.name}>
                <TableCell className="font-medium">{item.name}</TableCell>
                <TableCell className="font-mono text-xs">{item['base-url']}</TableCell>
                <TableCell className="tabular-nums">{item['api-key-entries'].length}</TableCell>
                <TableCell>
                  <ModelBadges models={item.models} />
                </TableCell>
                <TableCell>
                  <RowActions
                    label={item.name}
                    onEdit={() =>
                      setProvider({
                        index,
                        name: item.name,
                        baseUrl: item['base-url'],
                        keys: item['api-key-entries'].map((entry) => entry['api-key']).join('\n'),
                        models: formatModels(item.models),
                      })
                    }
                    onDelete={() => saveOpenai.mutate(providers.filter((_, at) => at !== index))}
                  />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Section>

      <Section
        title="Claude API keys"
        description="Messages requests for these models go straight to Anthropic with the key, without translation."
        empty="Add an Anthropic API key and the Claude models it should serve."
        rows={claudeKeys.length}
        onAdd={() => setClaudeKey({ index: -1, name: '', baseUrl: '', keys: '', models: '' })}
      >
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Key</TableHead>
              <TableHead>Base URL</TableHead>
              <TableHead>Models</TableHead>
              <TableHead className="w-0" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {claudeKeys.map((item, index) => (
              <TableRow key={item['api-key']}>
                <TableCell className="font-mono text-xs">{mask(item['api-key'])}</TableCell>
                <TableCell className="font-mono text-xs">{item['base-url'] || 'api.anthropic.com'}</TableCell>
                <TableCell>
                  <ModelBadges models={item.models} />
                </TableCell>
                <TableCell>
                  <RowActions
                    label="Claude key"
                    onEdit={() =>
                      setClaudeKey({ index, name: '', baseUrl: item['base-url'], keys: item['api-key'], models: formatModels(item.models) })
                    }
                    onDelete={() => saveClaude.mutate(claudeKeys.filter((_, at) => at !== index))}
                  />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Section>

      <Dialog open={provider !== null} onOpenChange={(open) => !open && setProvider(null)}>
        <DialogContent>
          <form onSubmit={submitProvider} className="contents">
            <DialogHeader>
              <DialogTitle>{provider && provider.index >= 0 ? 'Edit provider' : 'Add provider'}</DialogTitle>
              <DialogDescription>Any endpoint that accepts OpenAI chat completion requests.</DialogDescription>
            </DialogHeader>
            {provider && (
              <FieldGroup>
                <Field>
                  <FieldLabel htmlFor="provider-preset">Start from</FieldLabel>
                  <Select onValueChange={(name) => setProvider({ ...provider, name: provider.name || name, baseUrl: PRESETS[name] })}>
                    <SelectTrigger id="provider-preset">
                      <SelectValue placeholder="Custom endpoint" />
                    </SelectTrigger>
                    <SelectContent>
                      {Object.keys(PRESETS).map((name) => (
                        <SelectItem key={name} value={name}>
                          {name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
                <Field>
                  <FieldLabel htmlFor="provider-name">Name</FieldLabel>
                  <Input id="provider-name" required value={provider.name} onChange={(event) => setProvider({ ...provider, name: event.target.value })} />
                </Field>
                <Field>
                  <FieldLabel htmlFor="provider-url">Base URL</FieldLabel>
                  <Input
                    id="provider-url"
                    type="url"
                    required
                    className="font-mono"
                    placeholder="https://example.com/v1"
                    value={provider.baseUrl}
                    onChange={(event) => setProvider({ ...provider, baseUrl: event.target.value })}
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor="provider-keys">API keys</FieldLabel>
                  <Textarea
                    id="provider-keys"
                    className="font-mono"
                    rows={2}
                    value={provider.keys}
                    onChange={(event) => setProvider({ ...provider, keys: event.target.value })}
                  />
                  <FieldDescription>One per line. The next key is tried when one fails.</FieldDescription>
                </Field>
                <ModelsField value={provider.models} onChange={(models) => setProvider({ ...provider, models })} />
              </FieldGroup>
            )}
            <DialogFooter>
              <Button type="submit">Save provider</Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <Dialog open={claudeKey !== null} onOpenChange={(open) => !open && setClaudeKey(null)}>
        <DialogContent>
          <form onSubmit={submitClaudeKey} className="contents">
            <DialogHeader>
              <DialogTitle>{claudeKey && claudeKey.index >= 0 ? 'Edit Claude key' : 'Add Claude key'}</DialogTitle>
              <DialogDescription>An Anthropic API key and the models it serves on /v1/messages.</DialogDescription>
            </DialogHeader>
            {claudeKey && (
              <FieldGroup>
                <Field>
                  <FieldLabel htmlFor="claude-key">API key</FieldLabel>
                  <Input
                    id="claude-key"
                    required
                    className="font-mono"
                    value={claudeKey.keys}
                    onChange={(event) => setClaudeKey({ ...claudeKey, keys: event.target.value })}
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor="claude-url">Base URL</FieldLabel>
                  <Input
                    id="claude-url"
                    type="url"
                    className="font-mono"
                    placeholder="https://api.anthropic.com"
                    value={claudeKey.baseUrl}
                    onChange={(event) => setClaudeKey({ ...claudeKey, baseUrl: event.target.value })}
                  />
                  <FieldDescription>Leave empty for Anthropic's own API.</FieldDescription>
                </Field>
                <ModelsField value={claudeKey.models} onChange={(models) => setClaudeKey({ ...claudeKey, models })} />
              </FieldGroup>
            )}
            <DialogFooter>
              <Button type="submit">Save key</Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  )
}
