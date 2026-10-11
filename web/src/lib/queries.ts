import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { api, type Config, type Credential } from './api'

export const useConfig = () => useQuery({ queryKey: ['config'], queryFn: () => api<Config>('/config') })

export const useCredentials = () =>
  useQuery({ queryKey: ['auth-files'], queryFn: async () => (await api<{ files: Credential[] }>('/auth-files')).files })

export const useCopilot = () =>
  useQuery({ queryKey: ['copilot'], queryFn: async () => (await api<{ files: Credential[] }>('/copilot')).files })

export const useApiKeys = () =>
  useQuery({ queryKey: ['api-keys'], queryFn: async () => (await api<{ 'api-keys': string[] }>('/api-keys'))['api-keys'] })

/** A management write: toasts the outcome and refetches everything on success. */
export function useAction<T = void>(run: (input: T) => Promise<unknown>, done: string) {
  const client = useQueryClient()
  return useMutation({
    mutationFn: run,
    onSuccess: () => {
      toast.success(done)
      return client.invalidateQueries()
    },
    onError: (error) => toast.error(error.message),
  })
}
