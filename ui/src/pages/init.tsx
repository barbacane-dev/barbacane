import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { Download, FileCode, Sparkles, ArrowRight, Check } from 'lucide-react'
import { useMutation } from '@tanstack/react-query'
import { initProject } from '@/lib/api'
import type { InitTemplate, InitResponse } from '@/lib/api'
import { Button, Card, CardContent, CardHeader, CardTitle } from '@/components/ui'
import { cn } from '@/lib/utils'

type Step = 'config' | 'preview' | 'download'

export function InitPage() {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>('config')
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [version, setVersion] = useState('1.0.0')
  const [template, setTemplate] = useState<InitTemplate>('basic')
  const [result, setResult] = useState<InitResponse | null>(null)
  const [selectedFile, setSelectedFile] = useState<string | null>(null)

  const initMutation = useMutation({
    mutationFn: initProject,
    onSuccess: (data) => {
      setResult(data)
      setSelectedFile(data.files[0]?.path ?? null)
      setStep('preview')
    },
  })

  const handleGenerate = () => {
    initMutation.mutate({
      name,
      template,
      description: description || undefined,
      version,
    })
  }

  const handleDownload = () => {
    if (!result) return

    // Create a zip-like structure as individual file downloads
    // For simplicity, we'll download as a single JSON file that can be extracted
    const content = JSON.stringify(
      {
        files: result.files,
        instructions: result.next_steps,
      },
      null,
      2
    )

    const blob = new Blob([content], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `${name.toLowerCase().replace(/\s+/g, '-')}-project.json`
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)

    setStep('download')
  }

  const handleDownloadFile = (path: string, content: string) => {
    const blob = new Blob([content], { type: 'text/plain' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = path
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)
  }

  return (
    <div className="p-8 max-w-4xl mx-auto">
      <div className="mb-8">
        <h1 className="text-2xl font-semibold">Create New Project</h1>
        <p className="text-muted-foreground">
          Initialize a new Barbacane API gateway project
        </p>
      </div>

      {/* Steps indicator */}
      <div className="flex items-center gap-4 mb-8">
        {(['config', 'preview', 'download'] as const).map((s, i) => (
          <div key={s} className="flex items-center gap-2">
            <div
              className={cn(
                'flex h-8 w-8 items-center justify-center rounded-full text-sm font-medium',
                step === s
                  ? 'bg-primary text-primary-foreground'
                  : s === 'download' && step === 'download'
                    ? 'bg-green-500 text-white'
                    : 'bg-muted text-muted-foreground'
              )}
            >
              {step === 'download' && s === 'download' ? (
                <Check className="h-4 w-4" />
              ) : (
                i + 1
              )}
            </div>
            <span
              className={cn(
                'text-sm font-medium',
                step === s ? 'text-foreground' : 'text-muted-foreground'
              )}
            >
              {s === 'config' && 'Configure'}
              {s === 'preview' && 'Preview'}
              {s === 'download' && 'Download'}
            </span>
            {i < 2 && <ArrowRight className="h-4 w-4 text-muted-foreground ml-2" />}
          </div>
        ))}
      </div>

      {/* Step 1: Configuration */}
      {step === 'config' && (
        <Card>
          <CardHeader>
            <CardTitle>Project Configuration</CardTitle>
          </CardHeader>
          <CardContent className="space-y-6">
            <div>
              <label className="block text-sm font-medium mb-2">
                Project Name *
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="My API"
                className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-2">
                Description
              </label>
              <input
                type="text"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="A Barbacane-powered API"
                className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-2">Version</label>
              <input
                type="text"
                value={version}
                onChange={(e) => setVersion(e.target.value)}
                placeholder="1.0.0"
                className="w-full rounded-lg border border-input bg-background px-3 py-2 text-foreground placeholder:text-muted-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
              />
            </div>

            <div>
              <label className="block text-sm font-medium mb-3">Template</label>
              <div className="grid grid-cols-2 gap-4">
                <button
                  onClick={() => setTemplate('basic')}
                  className={cn(
                    'flex flex-col items-start rounded-lg border-2 p-4 text-left transition-all',
                    template === 'basic'
                      ? 'border-primary bg-primary/5'
                      : 'border-border hover:border-primary/50'
                  )}
                >
                  <div className="flex items-center gap-2 mb-2">
                    <Sparkles className="h-5 w-5 text-primary" />
                    <span className="font-medium">Basic</span>
                  </div>
                  <p className="text-sm text-muted-foreground">
                    Full example with users CRUD, validation, and schemas
                  </p>
                </button>

                <button
                  onClick={() => setTemplate('minimal')}
                  className={cn(
                    'flex flex-col items-start rounded-lg border-2 p-4 text-left transition-all',
                    template === 'minimal'
                      ? 'border-primary bg-primary/5'
                      : 'border-border hover:border-primary/50'
                  )}
                >
                  <div className="flex items-center gap-2 mb-2">
                    <FileCode className="h-5 w-5 text-secondary" />
                    <span className="font-medium">Minimal</span>
                  </div>
                  <p className="text-sm text-muted-foreground">
                    Bare bones with just a health endpoint
                  </p>
                </button>
              </div>
            </div>

            <div className="flex justify-end pt-4">
              <Button
                onClick={handleGenerate}
                disabled={!name.trim() || initMutation.isPending}
              >
                {initMutation.isPending ? 'Generating...' : 'Generate Project'}
              </Button>
            </div>

            {initMutation.isError && (
              <p className="text-sm text-destructive">
                {initMutation.error instanceof Error
                  ? initMutation.error.message
                  : 'Failed to generate project'}
              </p>
            )}
          </CardContent>
        </Card>
      )}

      {/* Step 2: Preview */}
      {step === 'preview' && result && (
        <div className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>Preview Generated Files</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="flex gap-4">
                {/* File list */}
                <div className="w-48 space-y-1">
                  {result.files.map((file) => (
                    <button
                      key={file.path}
                      onClick={() => setSelectedFile(file.path)}
                      className={cn(
                        'flex w-full items-center gap-2 rounded-lg px-3 py-2 text-sm text-left transition-colors',
                        selectedFile === file.path
                          ? 'bg-primary/10 text-primary'
                          : 'text-muted-foreground hover:bg-muted hover:text-foreground'
                      )}
                    >
                      <FileCode className="h-4 w-4" />
                      {file.path}
                    </button>
                  ))}
                </div>

                {/* File content */}
                <div className="flex-1 rounded-lg border border-border bg-muted/30 p-4">
                  <pre className="text-sm overflow-auto max-h-96">
                    <code>
                      {result.files.find((f) => f.path === selectedFile)?.content}
                    </code>
                  </pre>
                </div>
              </div>
            </CardContent>
          </Card>

          <div className="flex justify-between">
            <Button variant="outline" onClick={() => setStep('config')}>
              Back
            </Button>
            <div className="flex gap-2">
              {result.files.map((file) => (
                <Button
                  key={file.path}
                  variant="outline"
                  size="sm"
                  onClick={() => handleDownloadFile(file.path, file.content)}
                >
                  <Download className="h-4 w-4 mr-1" />
                  {file.path}
                </Button>
              ))}
              <Button onClick={handleDownload}>
                <Download className="h-4 w-4 mr-2" />
                Download All
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* Step 3: Download complete */}
      {step === 'download' && result && (
        <Card>
          <CardContent className="py-12 text-center">
            <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full bg-green-500/10">
              <Check className="h-6 w-6 text-green-500" />
            </div>
            <h3 className="text-lg font-medium mb-2">Project Created!</h3>
            <p className="text-muted-foreground mb-6">
              Your project files have been downloaded.
            </p>

            <div className="text-left max-w-md mx-auto mb-6">
              <h4 className="font-medium mb-2">Next Steps:</h4>
              <ol className="list-decimal list-inside space-y-1 text-sm text-muted-foreground">
                {result.next_steps.map((step, i) => (
                  <li key={i}>{step}</li>
                ))}
              </ol>
            </div>

            <div className="flex justify-center gap-4">
              <Button variant="outline" onClick={() => navigate('/specs')}>
                Go to Specs
              </Button>
              <Button
                onClick={() => {
                  setStep('config')
                  setResult(null)
                  setName('')
                  setDescription('')
                }}
              >
                Create Another
              </Button>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
