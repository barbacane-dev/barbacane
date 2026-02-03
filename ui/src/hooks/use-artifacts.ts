import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  listArtifacts,
  getArtifact,
  deleteArtifact,
  downloadArtifact,
} from '@/lib/api'

export const artifactKeys = {
  all: ['artifacts'] as const,
  lists: () => [...artifactKeys.all, 'list'] as const,
  details: () => [...artifactKeys.all, 'detail'] as const,
  detail: (id: string) => [...artifactKeys.details(), id] as const,
}

export function useArtifacts() {
  return useQuery({
    queryKey: artifactKeys.lists(),
    queryFn: () => listArtifacts(),
  })
}

export function useArtifact(id: string) {
  return useQuery({
    queryKey: artifactKeys.detail(id),
    queryFn: () => getArtifact(id),
    enabled: !!id,
  })
}

export function useDeleteArtifact() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: deleteArtifact,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: artifactKeys.lists() })
    },
  })
}

export function useDownloadArtifact() {
  return useMutation({
    mutationFn: async (id: string) => {
      const blob = await downloadArtifact(id)
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `${id}.bca`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
    },
  })
}
