import { useEffect, useRef } from 'react'
import { AlertTriangle } from 'lucide-react'
import { Button, Card, CardContent } from '@/components/ui'
import { cn } from '@/lib/utils'

export interface ConfirmDialogProps {
  open: boolean
  onConfirm: () => void
  onCancel: () => void
  title: string
  description: string
  confirmLabel?: string
  cancelLabel?: string
  variant?: 'destructive' | 'warning'
  isPending?: boolean
}

export function ConfirmDialog({
  open,
  onConfirm,
  onCancel,
  title,
  description,
  confirmLabel = 'Delete',
  cancelLabel = 'Cancel',
  variant = 'destructive',
  isPending = false,
}: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (open) {
      cancelRef.current?.focus()
    }
  }, [open])

  useEffect(() => {
    if (!open) return
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel()
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [open, onCancel])

  if (!open) return null

  const isDestructive = variant === 'destructive'

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={(e) => {
        if (e.target === e.currentTarget) onCancel()
      }}
    >
      <Card className="w-full max-w-sm">
        <CardContent className="p-6">
          <div className="flex items-start gap-3">
            <div
              className={cn(
                'flex h-9 w-9 shrink-0 items-center justify-center rounded-full',
                isDestructive ? 'bg-destructive/10' : 'bg-amber-500/10'
              )}
            >
              <AlertTriangle
                className={cn(
                  'h-5 w-5',
                  isDestructive ? 'text-destructive' : 'text-amber-500'
                )}
              />
            </div>
            <div>
              <h3 className="font-medium">{title}</h3>
              <p className="mt-1 text-sm text-muted-foreground">{description}</p>
            </div>
          </div>

          <div className="mt-6 flex justify-end gap-2">
            <Button
              ref={cancelRef}
              variant="outline"
              size="sm"
              onClick={onCancel}
              disabled={isPending}
            >
              {cancelLabel}
            </Button>
            <Button
              variant={isDestructive ? 'destructive' : 'default'}
              size="sm"
              onClick={onConfirm}
              disabled={isPending}
            >
              {isPending ? 'Please wait...' : confirmLabel}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}
