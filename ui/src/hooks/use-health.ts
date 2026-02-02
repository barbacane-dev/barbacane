import { useQuery } from '@tanstack/react-query'
import { getHealth } from '@/lib/api'

export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: getHealth,
    refetchInterval: 30000, // Check every 30 seconds
  })
}
