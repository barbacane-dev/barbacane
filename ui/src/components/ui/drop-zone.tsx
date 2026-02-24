import { useState, useCallback, useRef } from 'react'
import { Upload } from 'lucide-react'
import { cn } from '@/lib/utils'

interface DropZoneProps extends React.HTMLAttributes<HTMLDivElement> {
  onFileDrop: (file: File) => void
  accept?: string
  icon?: React.ComponentType<{ className?: string }>
  label?: string
  hint?: string
  disabled?: boolean
}

function DropZone({
  onFileDrop,
  accept,
  icon: Icon = Upload,
  label = 'Drop file here or click to browse',
  hint,
  disabled = false,
  className,
  ...props
}: DropZoneProps) {
  const [isDragOver, setIsDragOver] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  const handleDragOver = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault()
      e.stopPropagation()
      if (!disabled) setIsDragOver(true)
    },
    [disabled]
  )

  const handleDragLeave = useCallback((e: React.DragEvent) => {
    e.preventDefault()
    e.stopPropagation()
    setIsDragOver(false)
  }, [])

  const handleDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault()
      e.stopPropagation()
      setIsDragOver(false)
      if (disabled) return

      const file = e.dataTransfer.files[0]
      if (!file) return

      if (accept) {
        const extensions = accept
          .split(',')
          .map((ext) => ext.trim().toLowerCase())
        const fileName = file.name.toLowerCase()
        const isValid = extensions.some((ext) => fileName.endsWith(ext))
        if (!isValid) return
      }

      onFileDrop(file)
    },
    [accept, disabled, onFileDrop]
  )

  const handleClick = () => {
    if (!disabled) inputRef.current?.click()
  }

  const handleInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) onFileDrop(file)
    e.target.value = ''
  }

  return (
    <div
      onClick={handleClick}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
      className={cn(
        'flex cursor-pointer flex-col items-center justify-center rounded-lg border-2 border-dashed p-8 transition-colors',
        isDragOver
          ? 'border-primary bg-primary/5'
          : 'border-border hover:border-primary/50',
        disabled && 'cursor-not-allowed opacity-50',
        className
      )}
      {...props}
    >
      <Icon
        className={cn(
          'h-10 w-10 text-muted-foreground',
          isDragOver && 'text-primary'
        )}
      />
      <p className="mt-3 text-sm font-medium">{label}</p>
      {hint && <p className="mt-1 text-xs text-muted-foreground">{hint}</p>}
      <input
        ref={inputRef}
        type="file"
        accept={accept}
        onChange={handleInputChange}
        className="hidden"
      />
    </div>
  )
}

export { DropZone }
export type { DropZoneProps }
