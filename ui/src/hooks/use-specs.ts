import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  listSpecs,
  getSpec,
  uploadSpec,
  deleteSpec,
  getSpecHistory,
  downloadSpecContent,
  startCompilation,
  listSpecCompilations,
} from '@/lib/api'
import type { SpecType, CompileRequest } from '@/lib/api'

export const specKeys = {
  all: ['specs'] as const,
  lists: () => [...specKeys.all, 'list'] as const,
  list: (filters?: { type?: SpecType; name?: string }) =>
    [...specKeys.lists(), filters] as const,
  details: () => [...specKeys.all, 'detail'] as const,
  detail: (id: string) => [...specKeys.details(), id] as const,
  history: (id: string) => [...specKeys.detail(id), 'history'] as const,
  content: (id: string, revision?: number) =>
    [...specKeys.detail(id), 'content', revision] as const,
  compilations: (id: string) =>
    [...specKeys.detail(id), 'compilations'] as const,
}

export function useSpecs(filters?: { type?: SpecType; name?: string }) {
  return useQuery({
    queryKey: specKeys.list(filters),
    queryFn: () => listSpecs(filters),
  })
}

export function useSpec(id: string) {
  return useQuery({
    queryKey: specKeys.detail(id),
    queryFn: () => getSpec(id),
    enabled: !!id,
  })
}

export function useSpecHistory(id: string) {
  return useQuery({
    queryKey: specKeys.history(id),
    queryFn: () => getSpecHistory(id),
    enabled: !!id,
  })
}

export function useSpecContent(id: string, revision?: number) {
  return useQuery({
    queryKey: specKeys.content(id, revision),
    queryFn: () => downloadSpecContent(id, revision),
    enabled: !!id,
  })
}

export function useSpecCompilations(id: string) {
  return useQuery({
    queryKey: specKeys.compilations(id),
    queryFn: () => listSpecCompilations(id),
    enabled: !!id,
  })
}

export function useUploadSpec() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: uploadSpec,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: specKeys.lists() })
    },
  })
}

export function useDeleteSpec() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: deleteSpec,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: specKeys.lists() })
    },
  })
}

export function useStartCompilation(specId: string) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (options?: CompileRequest) => startCompilation(specId, options),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: specKeys.compilations(specId) })
    },
  })
}
