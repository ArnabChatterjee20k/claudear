import { useState, useRef, useCallback, useEffect } from 'react'
import { getWsBase } from './api'

export interface ChatMessage {
  id?: number
  role: 'user' | 'assistant'
  content: string
  sources?: ChatSource[]
  created_at?: string
}

export interface ChatSource {
  file_path: string
  start_line: number
  end_line: number
  symbol_name?: string
  similarity: number
}

export interface ChatSession {
  id: string
  repo_id?: number
  created_at: string
  updated_at: string
  messages?: ChatMessage[]
}

interface ChatChunk {
  delta: string
  done: boolean
  sources?: ChatSource[]
  session_id?: string
  error?: string
  stopped?: boolean
}

interface ChatRequest {
  type: 'chat'
  message: string
  session_id?: string
  repo_id?: number
  max_context_chunks?: number
}

interface UseChatReturn {
  messages: ChatMessage[]
  sendMessage: (message: string) => void
  stopStreaming: () => void
  isStreaming: boolean
  isConnected: boolean
  error: string | null
  sessionId: string | null
  clearMessages: () => void
}

export function useChat(repoId?: number): UseChatReturn {
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [isStreaming, setIsStreaming] = useState(false)
  const [isConnected, setIsConnected] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [sessionId, setSessionId] = useState<string | null>(null)
  const wsRef = useRef<WebSocket | null>(null)
  const reconnectTimer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const mountedRef = useRef(true)
  const streamingContentRef = useRef('')

  const connect = useCallback(() => {
    if (!mountedRef.current || wsRef.current?.readyState === WebSocket.OPEN) return

    const ws = new WebSocket(`${getWsBase()}/api/chat/ws`)
    wsRef.current = ws

    ws.onopen = () => {
      if (mountedRef.current) {
        setIsConnected(true)
        setError(null)
      }
    }

    ws.onmessage = (event) => {
      try {
        const chunk: ChatChunk = JSON.parse(event.data)

        if (chunk.error) {
          setError(chunk.error)
          setIsStreaming(false)
          return
        }

        if (chunk.session_id && !sessionId) {
          setSessionId(chunk.session_id)
        }

        // Accumulate streaming content
        streamingContentRef.current += chunk.delta

        // Update the last assistant message with accumulated content
        setMessages(prev => {
          const updated = [...prev]
          const lastIdx = updated.length - 1
          if (lastIdx >= 0 && updated[lastIdx].role === 'assistant') {
            updated[lastIdx] = {
              ...updated[lastIdx],
              content: streamingContentRef.current,
              sources: chunk.done ? chunk.sources : updated[lastIdx].sources,
            }
          }
          return updated
        })

        if (chunk.done) {
          setIsStreaming(false)
          streamingContentRef.current = ''
        }
      } catch { /* ignore malformed messages */ }
    }

    ws.onclose = () => {
      wsRef.current = null
      if (mountedRef.current) {
        setIsConnected(false)
        reconnectTimer.current = setTimeout(connect, 3000)
      }
    }

    ws.onerror = () => ws.close()
  }, [sessionId])

  const sendMessage = useCallback((message: string) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) {
      setError('Not connected')
      return
    }

    setError(null)
    setIsStreaming(true)
    streamingContentRef.current = ''

    // Add user message immediately
    const userMsg: ChatMessage = { role: 'user', content: message }
    // Add empty assistant message that will be filled by streaming
    const assistantMsg: ChatMessage = { role: 'assistant', content: '', sources: [] }

    setMessages(prev => [...prev, userMsg, assistantMsg])

    const request: ChatRequest = {
      type: 'chat',
      message,
      session_id: sessionId ?? undefined,
      repo_id: repoId,
    }

    wsRef.current.send(JSON.stringify(request))
  }, [sessionId, repoId])

  const stopStreaming = useCallback(() => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return
    wsRef.current.send(JSON.stringify({ type: 'stop' }))
  }, [])

  const clearMessages = useCallback(() => {
    setMessages([])
    setSessionId(null)
    streamingContentRef.current = ''
  }, [])

  useEffect(() => {
    mountedRef.current = true
    connect()
    return () => {
      mountedRef.current = false
      if (reconnectTimer.current) clearTimeout(reconnectTimer.current)
      wsRef.current?.close()
    }
  }, [connect])

  return { messages, sendMessage, stopStreaming, isStreaming, isConnected, error, sessionId, clearMessages }
}

// REST API helpers for session management
const API_BASE = '/api'

export async function fetchChatSessions(): Promise<ChatSession[]> {
  const res = await fetch(`${API_BASE}/chat/sessions`)
  if (!res.ok) throw new Error(`Failed to fetch sessions: ${res.statusText}`)
  return res.json()
}

export async function fetchChatSession(id: string): Promise<ChatSession> {
  const res = await fetch(`${API_BASE}/chat/sessions/${id}`)
  if (!res.ok) throw new Error(`Failed to fetch session: ${res.statusText}`)
  return res.json()
}

export async function deleteChatSession(id: string): Promise<void> {
  const res = await fetch(`${API_BASE}/chat/sessions/${id}`, { method: 'DELETE' })
  if (!res.ok) throw new Error(`Failed to delete session: ${res.statusText}`)
}

export async function fetchChatModels(): Promise<{ models: Array<{ name: string; status: string; context_length: number }> }> {
  const res = await fetch(`${API_BASE}/chat/models`)
  if (!res.ok) throw new Error(`Failed to fetch models: ${res.statusText}`)
  return res.json()
}
