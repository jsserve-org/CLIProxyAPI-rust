import { useCallback, useEffect, useState } from 'react'
import { Activity, BookOpen, Boxes, ChevronRight, CircleHelp, KeyRound, LayoutDashboard, RefreshCw, Settings2, ShieldCheck, SlidersHorizontal, Terminal } from 'lucide-react'
import { Button } from './components/ui/button'
import { Card } from './components/ui/card'
import { Badge } from './components/ui/badge'
import { Switch } from './components/ui/switch'
import './index.css'

type Config = { 'usage-statistics-enabled'?: boolean; 'max-concurrency'?: number; 'request-retry'?: number; 'routing'?: string; debug?: boolean; 'logging-to-file'?: boolean }
type Credential = { name?: string; auth_index?: string; disabled?: boolean; type?: string }
const initialKey = localStorage.getItem('management-key') ?? ''

function App() {
  const [key, setKey] = useState(initialKey)
  const [config, setConfig] = useState<Config>({})
  const [credentials, setCredentials] = useState<Credential[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [tab, setTab] = useState('Overview')

  const api = useCallback(async (path: string, init?: RequestInit) => {
    const response = await fetch(`/v0/management${path}`, { ...init, headers: { 'x-management-key': key, ...(init?.headers ?? {}) } })
    if (!response.ok) throw new Error(`${response.status} ${response.statusText}`)
    return response.json() as Promise<Record<string, unknown>>
  }, [key])

  const load = useCallback(async () => {
    if (!key) { setLoading(false); return }
    setLoading(true); setError('')
    try {
      const [nextConfig, nextCredentials] = await Promise.all([api('/config'), api('/auth-files')])
      setConfig(nextConfig as Config); setCredentials((nextCredentials.files as Credential[]) ?? [])
      localStorage.setItem('management-key', key)
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Could not reach the management API') }
    finally { setLoading(false) }
  }, [api, key])
  useEffect(() => { void load() }, [load])

  async function toggleUsage(value: boolean) { try { await api('/usage-statistics-enabled', { method: 'PUT', headers: {'content-type':'application/json'}, body: JSON.stringify({ value }) }); setConfig(c => ({ ...c, 'usage-statistics-enabled': value })) } catch (cause) { setError(cause instanceof Error ? cause.message : 'Could not update usage collection') } }

  const active = credentials.filter(c => !c.disabled).length
  const nav = [
    { name: 'Overview', icon: LayoutDashboard }, { name: 'Credentials', icon: KeyRound }, { name: 'Runtime', icon: SlidersHorizontal }, { name: 'API guide', icon: BookOpen },
  ]
  return <div className="console-shell">
    <header className="topbar"><div className="brand"><div className="brand-mark">∿</div><div>Operator console<small>private runtime control</small></div></div><div className="top-actions"><input className="key-input" type="password" aria-label="Management key" placeholder="management key" value={key} onChange={e => setKey(e.target.value)} onKeyDown={e => e.key === 'Enter' && void load()} /><Button onClick={() => void load()}><RefreshCw size={13} /> Connect</Button></div></header>
    <div className="layout"><aside className="sidebar"><div className="nav-label">Workspace</div><nav className="nav">{nav.map(item => { const Icon = item.icon; return <button key={item.name} className={tab === item.name ? 'active' : ''} onClick={() => setTab(item.name)}><Icon size={15} />{item.name}<ChevronRight size={13} style={{marginLeft:'auto', opacity: tab === item.name ? .8 : 0}} /></button> })}</nav><div className="sidebar-note"><strong><ShieldCheck size={13} style={{verticalAlign:'-2px', marginRight:5}} />Private by default</strong>Management traffic stays on the admin listener. API clients keep using the public surface.</div></aside>
      <main className="main"><div className="heading-row"><div><div className="eyebrow">{tab === 'Overview' ? 'System overview' : tab}</div><h1>{tab === 'Overview' ? 'A clear view of your proxy.' : tab}</h1><p className="lede">Control credentials, routing, and observability from one quiet place built for the operator at the keyboard.</p></div><div className="status-pill"><span className="dot" />{loading ? 'syncing' : error ? 'attention' : key ? 'connected' : 'key required'}</div></div>
      {error && <Card className="panel" style={{marginBottom:14, borderColor:'#604035', color:'#e5a995'}}>{error}. Check the management key and admin listener.</Card>}
      {!key && <Card className="panel" style={{marginBottom:14}}><div className="panel-title">Connect to your runtime</div><p className="lede" style={{marginTop:8}}>Enter the management key above, then press Connect to load live data.</p></Card>}
      {tab === 'Overview' && <><div className="grid"><Card className="stat"><div className="stat-label">Active credentials</div><div className="stat-value">{active}</div><div className="stat-hint">{credentials.length} loaded total</div></Card><Card className="stat"><div className="stat-label">Concurrency</div><div className="stat-value">{config['max-concurrency'] ?? '—'}</div><div className="stat-hint">request ceiling</div></Card><Card className="stat"><div className="stat-label">Retry policy</div><div className="stat-value">{config['request-retry'] ?? '—'}</div><div className="stat-hint">attempts per request</div></Card><Card className="stat"><div className="stat-label">Routing</div><div className="stat-value" style={{fontSize:20}}>{config.routing ?? '—'}</div><div className="stat-hint">selection strategy</div></Card></div><div className="section-grid"><Card className="panel"><div className="panel-header"><div className="panel-title">Credentials</div><div className="panel-meta">{credentials.length ? `${credentials.length} records` : 'none yet'}</div></div>{credentials.length === 0 ? <div className="empty">No credentials loaded. Upload auth files through the API or your custom app.</div> : credentials.slice(0,5).map((credential, i) => <div className="credential-row" key={`${credential.name}-${i}`}><div><div className="credential-name">{credential.name ?? 'Unnamed credential'}</div><div className="credential-sub">{credential.auth_index ?? credential.type ?? 'provider credential'}</div></div><Badge tone={credential.disabled ? 'amber' : 'green'}>{credential.disabled ? 'disabled' : 'ready'}</Badge></div>)}</Card><Card className="panel"><div className="panel-header"><div className="panel-title">Runtime controls</div><Activity size={15} color="#79d6c1" /></div><div className="toggle-row"><div><div className="toggle-title">Usage collection</div><div className="toggle-desc">Keep lightweight usage stats for the operator console.</div></div><Switch checked={Boolean(config['usage-statistics-enabled'])} onClick={() => void toggleUsage(!config['usage-statistics-enabled'])} /></div><div className="toggle-row"><div><div className="toggle-title">Debug logging</div><div className="toggle-desc">Verbose diagnostics for local troubleshooting.</div></div><Badge tone={config.debug ? 'amber' : 'green'}>{config.debug ? 'on' : 'off'}</Badge></div><div className="toggle-row"><div><div className="toggle-title">File logging</div><div className="toggle-desc">Persist logs to the configured runtime directory.</div></div><Badge tone={config['logging-to-file'] ? 'green' : 'amber'}>{config['logging-to-file'] ? 'on' : 'off'}</Badge></div></Card></div></>}
      {tab !== 'Overview' && <Card className="panel"><div className="panel-title">{tab === 'Credentials' ? 'Credential inventory' : tab === 'Runtime' ? 'Runtime settings' : 'Build with the API surface'}</div><p className="lede" style={{marginTop:10}}>{tab === 'Credentials' ? 'Credentials remain managed by the existing management endpoints. This view is intentionally focused on status and access.' : tab === 'Runtime' ? 'Use the existing management routes to tune retry, routing, logging, and quota behavior.' : 'Your custom app can use the stable /v0/management endpoints with the x-management-key header. The console is a reference client, not a replacement for that API.'}</p><Button variant="primary" style={{marginTop:22}} onClick={() => setTab('Overview')}>Back to overview</Button></Card>}
      <div style={{display:'flex', gap:16, marginTop:32, color:'#61727d', fontSize:11}}><span><Terminal size={12} style={{verticalAlign:'-2px', marginRight:5}} />Admin listener</span><span><Boxes size={12} style={{verticalAlign:'-2px', marginRight:5}} />API compatible</span><span><CircleHelp size={12} style={{verticalAlign:'-2px', marginRight:5}} />Built for your custom app</span></div>
      </main></div></div>
}


import { createRoot } from 'react-dom/client'
createRoot(document.getElementById('root')!).render(<App />)
