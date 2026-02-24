import { useState, useCallback } from 'react'
import { createElement } from 'react'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import type { ReactNode } from 'react'

interface ConfirmOptions {
  title: string
  description: string
  confirmLabel?: string
  cancelLabel?: string
  variant?: 'destructive' | 'warning'
}

interface ConfirmState extends ConfirmOptions {
  resolve: (value: boolean) => void
}

export function useConfirm(): {
  confirm: (options: ConfirmOptions) => Promise<boolean>
  dialog: ReactNode
} {
  const [state, setState] = useState<ConfirmState | null>(null)

  const confirm = useCallback((options: ConfirmOptions): Promise<boolean> => {
    return new Promise<boolean>((resolve) => {
      setState({ ...options, resolve })
    })
  }, [])

  const handleConfirm = useCallback(() => {
    setState((prev) => {
      prev?.resolve(true)
      return null
    })
  }, [])

  const handleCancel = useCallback(() => {
    setState((prev) => {
      prev?.resolve(false)
      return null
    })
  }, [])

  const dialog = state
    ? createElement(ConfirmDialog, {
        open: true,
        onConfirm: handleConfirm,
        onCancel: handleCancel,
        title: state.title,
        description: state.description,
        confirmLabel: state.confirmLabel,
        cancelLabel: state.cancelLabel,
        variant: state.variant,
      })
    : null

  return { confirm, dialog }
}
