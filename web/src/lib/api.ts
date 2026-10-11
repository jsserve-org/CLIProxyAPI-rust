const KEY_STORAGE = 'management-key'

export const getKey = () => localStorage.getItem(KEY_STORAGE) ?? ''
export const saveKey = (key: string) => localStorage.setItem(KEY_STORAGE, key)
export const forgetKey = () => localStorage.removeItem(KEY_STORAGE)

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
  }
}

/** Raw management request; throws `ApiError` with the server's message on a non-2xx. */
export async function request(path: string, init: RequestInit = {}, key = getKey()): Promise<Response> {
  const response = await fetch(`/v0/management${path}`, {
    ...init,
    headers: { 'x-management-key': key, ...init.headers },
  })
  if (!response.ok) {
    const body: { error?: { message?: string } | string; message?: string } | null = await response.json().catch(() => null)
    const message = typeof body?.error === 'string' ? (body.message ?? body.error) : body?.error?.message
    throw new ApiError(response.status, message ?? `${response.status} ${response.statusText}`)
  }
  return response
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  return (await request(path, init)).json() as Promise<T>
}

export const body = (method: string, value: unknown): RequestInit => ({
  method,
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify(value),
})

export type Credential = {
  name: string
  auth_index: string
  type: string
  email: string
  account_id?: string
  disabled: boolean
}

export type Config = {
  host: string
  port: number
  'admin-host': string
  'admin-port': number
  'auth-dir': string
  'api-keys': string[]
  'usage-statistics-enabled': boolean
  'redis-usage-queue-retention-seconds': number
  'proxy-url': string
  'request-retry': number
  'max-concurrency': number
  routing: { strategy: string; 'session-affinity': boolean; 'session-affinity-ttl': string }
  debug: boolean
  'logging-to-file': boolean
  'logs-max-total-size-mb': number
  'error-logs-max-files': number
  'max-retry-credentials': number
  'max-retry-interval': number
  'force-model-prefix': boolean
  'disable-cooling': boolean
  'transient-error-cooldown-seconds': number
  'quota-exceeded': { 'switch-project': boolean; 'switch-preview-model': boolean }
  'model-fallback': Record<string, string[]>
}

export type UsageRecord = {
  timestamp: string
  latency_ms: number
  ttft_ms: number
  auth_index: string
  model: string
  endpoint: string
  provider: string
  request_id: string
  failed: boolean
  fail: { status_code: number; body: string }
  tokens: { input_tokens: number; output_tokens: number; cached_tokens: number; total_tokens: number }
}

export type ApiCallResult = { status_code: number; header: Record<string, string[]>; body: string }

/** One Codex rate-limit window from `chatgpt.com/backend-api/wham/usage`. */
export type QuotaWindow = { used_percent: number | null; reset_at: number | null; limit_window_seconds: number | null }
export type RateLimit = { primary_window: QuotaWindow | null; secondary_window: QuotaWindow | null }
export type CodexUsage = {
  plan_type?: string
  rate_limit?: RateLimit | null
  code_review_rate_limit?: RateLimit | null
  additional_rate_limits?: { limit_name?: string; metered_feature?: string; rate_limit?: RateLimit | null }[] | null
}

export type ModelAlias = { name: string; alias: string }
export type OpenAiProvider = { name: string; 'base-url': string; 'api-key-entries': { 'api-key': string }[]; models: ModelAlias[] }
export type ClaudeKey = { 'api-key': string; 'base-url': string; models: ModelAlias[] }
export type Throughput = {
  total_requests: number
  failure_count: number
  total_tokens: number
  minutes: { time: number; requests: number; failed: number; tokens: number }[]
}
export type ErrorLogFile = { name: string; size: number; modified: number }

/** One model per line: `upstream-name`, or `upstream-name = name clients use`. */
export function parseModels(text: string): ModelAlias[] {
  return text
    .split('\n')
    .map((line) => line.split('=').map((part) => part.trim()))
    .filter(([name]) => name)
    .map(([name, alias = '']) => ({ name, alias }))
}
export const formatModels = (models: ModelAlias[]) =>
  models.map((model) => (model.alias ? `${model.name} = ${model.alias}` : model.name)).join('\n')

export type Model = { id: string; owned_by: string }

export type DeviceCode = {
  device_code: string
  user_code: string
  verification_uri: string
  expires_in: number
  interval: number
}

/** Hand a blob to the browser as a file download. */
export function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = name
  anchor.click()
  URL.revokeObjectURL(url)
}
