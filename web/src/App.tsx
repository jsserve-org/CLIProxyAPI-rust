import { useIsFetching, useQueryClient } from '@tanstack/react-query'
import {
  ActivityIcon,
  BoxesIcon,
  CloudIcon,
  ShuffleIcon,
  FileCodeIcon,
  GaugeIcon,
  ScrollTextIcon,
  BotIcon,
  FileKeyIcon,
  KeyRoundIcon,
  LayoutDashboardIcon,
  LogOutIcon,
  MoonStarIcon,
  MoonIcon,
  RefreshCwIcon,
  SendIcon,
  SlidersHorizontalIcon,
  SunIcon,
  WaypointsIcon,
} from 'lucide-react'
import { useTheme } from 'next-themes'
import { useEffect, useState } from 'react'
import { Login } from '@/components/login'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarRail,
  SidebarTrigger,
} from '@/components/ui/sidebar'
import { ApiError, forgetKey, getKey } from '@/lib/api'
import { useConfig } from '@/lib/queries'
import { cn } from '@/lib/utils'
import { ApiCall } from '@/pages/api-call'
import { ApiKeys } from '@/pages/api-keys'
import { ConfigFile } from '@/pages/config-file'
import { Copilot } from '@/pages/copilot'
import { Credentials } from '@/pages/credentials'
import { Kimi } from '@/pages/kimi'
import { Logs } from '@/pages/logs'
import { ModelRules } from '@/pages/model-rules'
import { Models } from '@/pages/models'
import { Overview } from '@/pages/overview'
import { Providers } from '@/pages/providers'
import { Quota } from '@/pages/quota'
import { Settings } from '@/pages/settings'
import { Usage } from '@/pages/usage'

const groups = [
  {
    label: 'Proxy',
    pages: [
      { id: 'overview', title: 'Overview', icon: LayoutDashboardIcon, Page: Overview },
      { id: 'settings', title: 'Settings', icon: SlidersHorizontalIcon, Page: Settings },
      { id: 'config-file', title: 'Config file', icon: FileCodeIcon, Page: ConfigFile },
      { id: 'models', title: 'Models', icon: BoxesIcon, Page: Models },
      { id: 'model-rules', title: 'Model rules', icon: ShuffleIcon, Page: ModelRules },
    ],
  },
  {
    label: 'Access',
    pages: [
      { id: 'credentials', title: 'Credentials', icon: FileKeyIcon, Page: Credentials },
      { id: 'quota', title: 'Quota', icon: GaugeIcon, Page: Quota },
      { id: 'providers', title: 'Providers', icon: CloudIcon, Page: Providers },
      { id: 'copilot', title: 'Copilot', icon: BotIcon, Page: Copilot },
      { id: 'kimi', title: 'Kimi', icon: MoonStarIcon, Page: Kimi },
      { id: 'api-keys', title: 'API keys', icon: KeyRoundIcon, Page: ApiKeys },
    ],
  },
  {
    label: 'Tools',
    pages: [
      { id: 'usage', title: 'Usage', icon: ActivityIcon, Page: Usage },
      { id: 'logs', title: 'Logs', icon: ScrollTextIcon, Page: Logs },
      { id: 'api-call', title: 'API call', icon: SendIcon, Page: ApiCall },
    ],
  },
]
const pages = groups.flatMap((group) => group.pages)

function useHash() {
  const [hash, setHash] = useState(location.hash.slice(1))
  useEffect(() => {
    const update = () => setHash(location.hash.slice(1))
    addEventListener('hashchange', update)
    return () => removeEventListener('hashchange', update)
  }, [])
  return hash
}

export function App() {
  const [key, setKey] = useState(getKey)
  const client = useQueryClient()
  if (!key) return <Login onConnect={setKey} />
  return (
    <Console
      onSignOut={() => {
        forgetKey()
        client.clear()
        setKey('')
      }}
    />
  )
}

function Console({ onSignOut }: { onSignOut: () => void }) {
  const hash = useHash()
  const current = pages.find((page) => page.id === hash) ?? pages[0]
  const client = useQueryClient()
  const fetching = useIsFetching() > 0
  const { resolvedTheme, setTheme } = useTheme()

  // A key that stopped working (rotated, or a stale one in storage) sends you back to the login.
  const { error } = useConfig()
  const rejected = error instanceof ApiError && error.status === 401
  useEffect(() => {
    if (rejected) onSignOut()
  }, [rejected, onSignOut])

  return (
    <SidebarProvider>
      <Sidebar collapsible="icon">
        <SidebarHeader>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton size="lg" asChild>
                <a href="#overview">
                  <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
                    <WaypointsIcon className="size-4" />
                  </div>
                  <div className="grid flex-1 text-left leading-tight">
                    <span className="truncate font-medium">CLIProxyAPI</span>
                    <span className="truncate font-mono text-xs text-muted-foreground">{location.host}</span>
                  </div>
                </a>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarHeader>
        <SidebarContent>
          {groups.map((group) => (
            <SidebarGroup key={group.label}>
              <SidebarGroupLabel>{group.label}</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {group.pages.map((page) => (
                    <SidebarMenuItem key={page.id}>
                      <SidebarMenuButton asChild isActive={page === current} tooltip={page.title}>
                        <a href={`#${page.id}`}>
                          <page.icon />
                          <span>{page.title}</span>
                        </a>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          ))}
        </SidebarContent>
        <SidebarFooter>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton tooltip="Sign out" onClick={onSignOut}>
                <LogOutIcon />
                <span>Sign out</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarFooter>
        <SidebarRail />
      </Sidebar>
      <SidebarInset>
        <header className="sticky top-0 z-10 flex h-12 shrink-0 items-center gap-2 border-b bg-background px-4">
          <SidebarTrigger className="-ml-1" />
          <Separator orientation="vertical" className="mr-1 data-[orientation=vertical]:h-4 data-[orientation=vertical]:self-center" />
          <h1 className="text-sm font-medium">{current.title}</h1>
          <div className="ml-auto flex items-center gap-1">
            <Button variant="ghost" size="icon-sm" aria-label="Reload data" onClick={() => client.invalidateQueries()}>
              <RefreshCwIcon className={cn(fetching && 'animate-spin')} />
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Switch theme"
              onClick={() => setTheme(resolvedTheme === 'dark' ? 'light' : 'dark')}
            >
              {resolvedTheme === 'dark' ? <SunIcon /> : <MoonIcon />}
            </Button>
          </div>
        </header>
        <div className="mx-auto flex w-full max-w-5xl flex-col gap-4 p-4 md:p-6">
          <current.Page />
        </div>
      </SidebarInset>
    </SidebarProvider>
  )
}
