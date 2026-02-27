import { User, Bot, ChevronDown, ChevronRight, Copy, Check } from 'lucide-react'
import { useState } from 'react'
import type { ChatMessage as ChatMessageType, ChatSource } from '../../lib/chat'

interface ChatMessageProps {
  message: ChatMessageType
  isStreaming?: boolean
}

export function ChatMessage({ message, isStreaming }: ChatMessageProps) {
  const isUser = message.role === 'user'

  return (
    <div className={`flex gap-3 ${isUser ? 'flex-row-reverse' : ''}`}>
      <div className={`shrink-0 h-8 w-8 rounded-full flex items-center justify-center ${
        isUser
          ? 'bg-primary/10 text-primary'
          : 'bg-muted text-muted-foreground'
      }`}>
        {isUser ? <User className="h-4 w-4" /> : <Bot className="h-4 w-4" />}
      </div>
      <div className={`flex-1 min-w-0 max-w-[80%] ${isUser ? 'text-right' : ''}`}>
        <div className={`inline-block rounded-lg px-4 py-2.5 text-sm ${
          isUser
            ? 'bg-primary text-primary-foreground'
            : 'bg-muted'
        }`}>
          <MessageContent content={message.content} isStreaming={isStreaming} />
        </div>
        {!isUser && message.sources && message.sources.length > 0 && (
          <SourcesPanel sources={message.sources} />
        )}
      </div>
    </div>
  )
}

function MessageContent({ content, isStreaming }: { content: string; isStreaming?: boolean }) {
  if (!content && isStreaming) {
    return (
      <span className="inline-flex gap-1">
        <span className="animate-pulse">.</span>
        <span className="animate-pulse delay-100">.</span>
        <span className="animate-pulse delay-200">.</span>
      </span>
    )
  }

  // Simple markdown-like rendering for code blocks
  const parts = content.split(/(```[\s\S]*?```)/g)

  return (
    <div className="whitespace-pre-wrap break-words">
      {parts.map((part, i) => {
        if (part.startsWith('```') && part.endsWith('```')) {
          const lines = part.slice(3, -3).split('\n')
          const lang = lines[0]?.trim() || ''
          const code = (lang ? lines.slice(1) : lines).join('\n').trim()
          return <CodeBlock key={i} code={code} language={lang} />
        }
        // Handle inline code
        return <InlineText key={i} text={part} />
      })}
      {isStreaming && <span className="animate-pulse ml-0.5">|</span>}
    </div>
  )
}

function InlineText({ text }: { text: string }) {
  const parts = text.split(/(`[^`]+`)/g)
  return (
    <>
      {parts.map((part, i) => {
        if (part.startsWith('`') && part.endsWith('`')) {
          return (
            <code key={i} className="px-1 py-0.5 rounded bg-background/50 text-xs font-mono">
              {part.slice(1, -1)}
            </code>
          )
        }
        return <span key={i}>{part}</span>
      })}
    </>
  )
}

function CodeBlock({ code, language }: { code: string; language: string }) {
  const [copied, setCopied] = useState(false)

  const handleCopy = () => {
    navigator.clipboard.writeText(code)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  return (
    <div className="my-2 rounded-md border bg-background overflow-hidden">
      <div className="flex items-center justify-between px-3 py-1.5 border-b bg-muted/50">
        <span className="text-xs text-muted-foreground font-mono">{language || 'code'}</span>
        <button
          onClick={handleCopy}
          className="text-xs text-muted-foreground hover:text-foreground transition-colors"
          title="Copy code"
        >
          {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
        </button>
      </div>
      <pre className="p-3 overflow-x-auto text-xs font-mono">
        <code>{code}</code>
      </pre>
    </div>
  )
}

function SourcesPanel({ sources }: { sources: ChatSource[] }) {
  const [expanded, setExpanded] = useState(false)

  return (
    <div className="mt-1.5">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
      >
        {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
        {sources.length} source{sources.length !== 1 ? 's' : ''}
      </button>
      {expanded && (
        <div className="mt-1 space-y-0.5 ml-4">
          {sources.map((src, i) => (
            <div key={i} className="text-xs text-muted-foreground font-mono">
              <span className="text-foreground">{src.file_path}</span>
              :{src.start_line}-{src.end_line}
              {src.symbol_name && (
                <span className="ml-1 text-primary">({src.symbol_name})</span>
              )}
              <span className="ml-1 opacity-60">{Math.round(src.similarity * 100)}%</span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
