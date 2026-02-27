import { useState, useRef, useCallback } from 'react'
import { Send, Square } from 'lucide-react'

interface ChatInputProps {
  onSend: (message: string) => void
  onStop?: () => void
  isStreaming?: boolean
  disabled?: boolean
  placeholder?: string
}

export function ChatInput({ onSend, onStop, isStreaming, disabled, placeholder }: ChatInputProps) {
  const [value, setValue] = useState('')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  const handleSubmit = useCallback(() => {
    const trimmed = value.trim()
    if (!trimmed || disabled) return
    onSend(trimmed)
    setValue('')
    // Reset textarea height
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto'
    }
  }, [value, disabled, onSend])

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      if (isStreaming) return
      handleSubmit()
    }
  }

  const handleInput = () => {
    const textarea = textareaRef.current
    if (textarea) {
      textarea.style.height = 'auto'
      textarea.style.height = `${Math.min(textarea.scrollHeight, 200)}px`
    }
  }

  return (
    <div className="flex items-end gap-2 p-4 border-t bg-card">
      <textarea
        ref={textareaRef}
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={handleKeyDown}
        onInput={handleInput}
        placeholder={placeholder ?? 'Ask about your code...'}
        disabled={disabled}
        rows={1}
        className="flex-1 resize-none rounded-lg border bg-background px-3 py-2 text-sm
          placeholder:text-muted-foreground focus:outline-none focus:ring-2 focus:ring-ring
          disabled:opacity-50 disabled:cursor-not-allowed min-h-[38px] max-h-[200px]"
      />
      {isStreaming ? (
        <button
          onClick={onStop}
          className="shrink-0 h-[38px] w-[38px] rounded-lg bg-destructive text-destructive-foreground
            flex items-center justify-center hover:bg-destructive/90 transition-colors"
          title="Stop generation"
        >
          <Square className="h-4 w-4" />
        </button>
      ) : (
        <button
          onClick={handleSubmit}
          disabled={disabled || !value.trim()}
          className="shrink-0 h-[38px] w-[38px] rounded-lg bg-primary text-primary-foreground
            flex items-center justify-center hover:bg-primary/90 transition-colors
            disabled:opacity-50 disabled:cursor-not-allowed"
          title="Send message"
        >
          <Send className="h-4 w-4" />
        </button>
      )}
    </div>
  )
}
