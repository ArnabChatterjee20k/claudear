import { Plus, Trash2, MessageSquare } from 'lucide-react'
import type { ChatSession } from '../../lib/chat'

interface ChatSessionsProps {
  sessions: ChatSession[]
  activeSessionId: string | null
  onSelectSession: (id: string) => void
  onNewSession: () => void
  onDeleteSession: (id: string) => void
}

export function ChatSessions({
  sessions,
  activeSessionId,
  onSelectSession,
  onNewSession,
  onDeleteSession,
}: ChatSessionsProps) {
  return (
    <div className="flex flex-col h-full">
      <div className="p-3 border-b">
        <button
          onClick={onNewSession}
          className="w-full flex items-center justify-center gap-2 px-3 py-2 rounded-md text-sm
            border border-dashed hover:bg-muted transition-colors text-muted-foreground
            hover:text-foreground"
        >
          <Plus className="h-4 w-4" />
          New Chat
        </button>
      </div>
      <div className="flex-1 overflow-y-auto p-2 space-y-0.5">
        {sessions.length === 0 ? (
          <div className="px-3 py-8 text-center text-xs text-muted-foreground">
            No conversations yet
          </div>
        ) : (
          sessions.map((session) => {
            const isActive = session.id === activeSessionId
            const date = new Date(session.updated_at)
            const timeStr = date.toLocaleDateString(undefined, {
              month: 'short',
              day: 'numeric',
              hour: '2-digit',
              minute: '2-digit',
            })

            return (
              <div
                key={session.id}
                className={`group flex items-center gap-2 px-3 py-2 rounded-md text-sm cursor-pointer transition-colors ${
                  isActive
                    ? 'bg-primary/10 text-primary'
                    : 'text-muted-foreground hover:bg-muted hover:text-foreground'
                }`}
                onClick={() => onSelectSession(session.id)}
              >
                <MessageSquare className="h-4 w-4 shrink-0" />
                <div className="flex-1 min-w-0">
                  <div className="truncate text-xs">
                    {session.id.slice(0, 8)}...
                  </div>
                  <div className="text-[10px] opacity-60">{timeStr}</div>
                </div>
                <button
                  onClick={(e) => {
                    e.stopPropagation()
                    onDeleteSession(session.id)
                  }}
                  className="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-destructive/10
                    text-muted-foreground hover:text-destructive transition-all"
                  title="Delete session"
                >
                  <Trash2 className="h-3 w-3" />
                </button>
              </div>
            )
          })
        )}
      </div>
    </div>
  )
}
