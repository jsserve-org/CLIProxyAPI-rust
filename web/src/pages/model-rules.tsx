import { useQuery } from '@tanstack/react-query'
import { useEffect, useState } from 'react'
import { Pending } from '@/components/pending'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from '@/components/ui/card'
import { Textarea } from '@/components/ui/textarea'
import { api, body, formatModels, parseModels, type ModelAlias } from '@/lib/api'
import { useAction } from '@/lib/queries'

function Rule(props: { title: string; description: string; placeholder: string; saved: string; pending: boolean; onSave: (text: string) => void }) {
  const [draft, setDraft] = useState(props.saved)
  useEffect(() => setDraft(props.saved), [props.saved])
  return (
    <Card>
      <CardHeader>
        <CardTitle>{props.title}</CardTitle>
        <CardDescription>{props.description}</CardDescription>
      </CardHeader>
      <CardContent>
        <Textarea
          aria-label={props.title}
          className="font-mono"
          rows={6}
          placeholder={props.placeholder}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
        />
      </CardContent>
      <CardFooter>
        <Button disabled={draft === props.saved || props.pending} onClick={() => props.onSave(draft)}>
          Save
        </Button>
      </CardFooter>
    </Card>
  )
}

export function ModelRules() {
  const aliases = useQuery({
    queryKey: ['oauth-model-alias'],
    queryFn: async () => (await api<{ 'oauth-model-alias': Record<string, ModelAlias[]> }>('/oauth-model-alias'))['oauth-model-alias'],
  })
  const excluded = useQuery({
    queryKey: ['oauth-excluded-models'],
    queryFn: async () => (await api<{ 'oauth-excluded-models': Record<string, string[]> }>('/oauth-excluded-models'))['oauth-excluded-models'],
  })
  // Other providers' entries are kept as they are; the proxy only applies `codex`.
  const saveAliases = useAction(
    (text: string) => api('/oauth-model-alias', body('PUT', { ...aliases.data, codex: parseModels(text).filter((model) => model.alias) })),
    'Aliases saved',
  )
  const saveExcluded = useAction(
    (text: string) =>
      api(
        '/oauth-excluded-models',
        body('PUT', {
          ...excluded.data,
          codex: text
            .split('\n')
            .map((line) => line.trim())
            .filter(Boolean),
        }),
      ),
    'Exclusions saved',
  )

  if (!aliases.data || !excluded.data) return <Pending error={aliases.error ?? excluded.error} />

  return (
    <>
      <Rule
        title="Codex model aliases"
        description="Give a Codex model another name for clients. One per line, as real-model = alias. Not written to config.yaml."
        placeholder="gpt-5.6-sol = best"
        saved={formatModels(aliases.data.codex ?? [])}
        pending={saveAliases.isPending}
        onSave={(text) => saveAliases.mutate(text)}
      />
      <Rule
        title="Excluded Codex models"
        description="Models clients can't see or use. One pattern per line; * matches anything. Not written to config.yaml."
        placeholder="*-spark"
        saved={(excluded.data.codex ?? []).join('\n')}
        pending={saveExcluded.isPending}
        onSave={(text) => saveExcluded.mutate(text)}
      />
    </>
  )
}
