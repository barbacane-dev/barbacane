import { useEffect, useState, useMemo } from 'react'
import { createHighlighter, type Highlighter } from 'shiki'
import { cn } from '@/lib/utils'

interface CodeBlockProps {
  code: string
  language?: 'yaml' | 'json'
  className?: string
}

let highlighterPromise: Promise<Highlighter> | null = null

function getHighlighter() {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({
      themes: ['github-dark', 'github-light'],
      langs: ['yaml', 'json'],
    })
  }
  return highlighterPromise
}

function detectLanguage(code: string): 'yaml' | 'json' {
  const trimmed = code.trimStart()
  return trimmed.startsWith('{') || trimmed.startsWith('[') ? 'json' : 'yaml'
}

export function CodeBlock({ code, language, className }: CodeBlockProps) {
  const [highlighter, setHighlighter] = useState<Highlighter | null>(null)
  const lang = language ?? detectLanguage(code)

  useEffect(() => {
    getHighlighter().then(setHighlighter)
  }, [])

  const html = useMemo(() => {
    if (!highlighter) return null
    const isDark = !document.documentElement.classList.contains('light')
    return highlighter.codeToHtml(code, {
      lang,
      theme: isDark ? 'github-dark' : 'github-light',
    })
  }, [highlighter, code, lang])

  if (!html) {
    return (
      <pre className={cn('text-xs font-mono whitespace-pre-wrap', className)}>
        {code}
      </pre>
    )
  }

  return (
    <div
      className={cn('[&_pre]:!bg-transparent [&_pre]:!p-0 [&_code]:text-xs [&_code]:font-mono [&_pre]:whitespace-pre-wrap', className)}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  )
}
