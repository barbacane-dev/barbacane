import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  listPlugins,
  listPluginVersions,
  getPlugin,
  registerPlugin,
  deletePlugin,
} from '@/lib/api'
import type { PluginType } from '@/lib/api'

export const pluginKeys = {
  all: ['plugins'] as const,
  lists: () => [...pluginKeys.all, 'list'] as const,
  list: (filters?: { type?: PluginType; name?: string }) =>
    [...pluginKeys.lists(), filters] as const,
  versions: (name: string) => [...pluginKeys.all, 'versions', name] as const,
  details: () => [...pluginKeys.all, 'detail'] as const,
  detail: (name: string, version: string) =>
    [...pluginKeys.details(), name, version] as const,
}

export function usePlugins(filters?: { type?: PluginType; name?: string }) {
  return useQuery({
    queryKey: pluginKeys.list(filters),
    queryFn: () => listPlugins(filters),
  })
}

export function usePluginVersions(name: string) {
  return useQuery({
    queryKey: pluginKeys.versions(name),
    queryFn: () => listPluginVersions(name),
    enabled: !!name,
  })
}

export function usePlugin(name: string, version: string) {
  return useQuery({
    queryKey: pluginKeys.detail(name, version),
    queryFn: () => getPlugin(name, version),
    enabled: !!name && !!version,
  })
}

export function useRegisterPlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: registerPlugin,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: pluginKeys.lists() })
    },
  })
}

export function useDeletePlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ name, version }: { name: string; version: string }) =>
      deletePlugin(name, version),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: pluginKeys.lists() })
    },
  })
}
