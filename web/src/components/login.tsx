import { useState, type FormEvent } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from '@/components/ui/field'
import { Input } from '@/components/ui/input'
import { Spinner } from '@/components/ui/spinner'
import { ApiError, request, saveKey } from '@/lib/api'

export function Login({ onConnect }: { onConnect: (key: string) => void }) {
  const [key, setKey] = useState('')
  const [error, setError] = useState('')
  const [checking, setChecking] = useState(false)

  async function connect(event: FormEvent) {
    event.preventDefault()
    setChecking(true)
    setError('')
    try {
      await request('/config', {}, key)
      saveKey(key)
      onConnect(key)
    } catch (cause) {
      const rejected = cause instanceof ApiError && cause.status === 401
      setError(rejected ? 'The admin listener rejected that key.' : `Couldn't reach the admin listener: ${(cause as Error).message}`)
    } finally {
      setChecking(false)
    }
  }

  return (
    <main className="flex min-h-svh items-center justify-center bg-sidebar p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>Operator console</CardTitle>
          <CardDescription>Connect to this proxy's admin listener.</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={connect}>
            <FieldGroup>
              <Field data-invalid={Boolean(error)}>
                <FieldLabel htmlFor="management-key">Management key</FieldLabel>
                <Input
                  id="management-key"
                  type="password"
                  autoFocus
                  autoComplete="current-password"
                  aria-invalid={Boolean(error)}
                  value={key}
                  onChange={(event) => setKey(event.target.value)}
                />
                {error ? (
                  <FieldError>{error}</FieldError>
                ) : (
                  <FieldDescription>
                    The <code className="font-mono">remote-management.secret-key</code> from config.yaml. It is kept in this
                    browser only.
                  </FieldDescription>
                )}
              </Field>
              <Button type="submit" disabled={!key || checking}>
                {checking && <Spinner data-icon="inline-start" />}
                Connect
              </Button>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
    </main>
  )
}
