import { useState, useEffect, useCallback } from 'react'
import useSWR from 'swr'
import { fetchConfig, saveConfig, type ConfigResponse } from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { Badge } from '../components/ui/badge'
import {
  Settings, Save, RotateCcw, FileText, AlertTriangle,
  CheckCircle2, Server, GitBranch, Bell, Shield,
  Cpu, Bot, RefreshCw, Eye, Pencil, X,
} from 'lucide-react'

interface TomlSection {
  key: string
  label: string
  icon: React.ReactNode
  description: string
  lines: string[]
  startLine: number
}

interface ParsedField {
  key: string
  value: string
  type: 'string' | 'number' | 'boolean' | 'array' | 'unknown'
  comment?: string
  isSecret: boolean
  rawLine: string
}

const SECTION_META: Record<string, { label: string; icon: React.ReactNode; description: string }> = {
  _top: { label: 'Core', icon: <Server className="h-4 w-4 text-blue-500" />, description: 'Working directory, orgs, and discovery paths' },
  retry: { label: 'Retry Strategy', icon: <RefreshCw className="h-4 w-4 text-orange-500" />, description: 'Retry timing and limits for failed fixes' },
  claude: { label: 'Claude', icon: <Bot className="h-4 w-4 text-violet-500" />, description: 'Model, instructions, and permissions' },
  discord: { label: 'Discord', icon: <Bell className="h-4 w-4 text-indigo-500" />, description: 'Webhook and bot notification settings' },
  ask: { label: 'Human Q&A', icon: <Eye className="h-4 w-4 text-cyan-500" />, description: 'Ask-loop timeouts and semantic reuse' },
  linear: { label: 'Linear', icon: <GitBranch className="h-4 w-4 text-purple-500" />, description: 'Issue source triggers and filters' },
  sentry: { label: 'Sentry', icon: <AlertTriangle className="h-4 w-4 text-red-500" />, description: 'Error tracking source settings' },
  github: { label: 'GitHub', icon: <GitBranch className="h-4 w-4 text-gray-700" />, description: 'Token, PR monitoring, and webhooks' },
  'github.app': { label: 'GitHub App', icon: <Shield className="h-4 w-4 text-gray-500" />, description: 'App-based authentication config' },
  email: { label: 'Email', icon: <Bell className="h-4 w-4 text-green-500" />, description: 'SMTP and IMAP notification settings' },
  sms: { label: 'SMS', icon: <Bell className="h-4 w-4 text-yellow-500" />, description: 'Twilio SMS notification settings' },
  push: { label: 'Push', icon: <Bell className="h-4 w-4 text-pink-500" />, description: 'Pushover notification settings' },
  regression: { label: 'Regression', icon: <Shield className="h-4 w-4 text-red-400" />, description: 'Post-fix regression monitoring' },
  cascade: { label: 'Cascade', icon: <RefreshCw className="h-4 w-4 text-teal-500" />, description: 'Multi-repo cascade chaining' },
  learning: { label: 'Learning', icon: <Cpu className="h-4 w-4 text-emerald-500" />, description: 'Continuous learning and knowledge extraction' },
}

const SECRET_KEYS = ['token', 'api_key', 'secret', 'password', 'webhook_url', 'private_key', 'auth_token', 'account_sid', 'api_token']

function isSecretKey(key: string): boolean {
  const lower = key.toLowerCase()
  return SECRET_KEYS.some(s => lower.includes(s))
}

function parseFieldType(value: string): 'string' | 'number' | 'boolean' | 'array' | 'unknown' {
  if (value === 'true' || value === 'false') return 'boolean'
  if (value.startsWith('[')) return 'array'
  if (value.startsWith('"') || value.startsWith("'")) return 'string'
  if (/^-?\d+(\.\d+)?$/.test(value)) return 'number'
  return 'unknown'
}

function parseFields(lines: string[]): ParsedField[] {
  const fields: ParsedField[] = []
  let pendingComment = ''

  for (const line of lines) {
    const trimmed = line.trim()
    if (trimmed === '' || trimmed.startsWith('# ==')) {
      pendingComment = ''
      continue
    }
    if (trimmed.startsWith('#') && !trimmed.includes('=')) {
      pendingComment = trimmed.replace(/^#\s*/, '')
      continue
    }
    if (trimmed.startsWith('[')) {
      pendingComment = ''
      continue
    }

    const match = trimmed.match(/^([^=]+?)\s*=\s*(.+)$/)
    if (match) {
      const key = match[1].trim()
      const value = match[2].trim()
      fields.push({
        key,
        value,
        type: parseFieldType(value),
        comment: pendingComment || undefined,
        isSecret: isSecretKey(key),
        rawLine: line,
      })
      pendingComment = ''
    }
  }
  return fields
}

function parseArrayValue(value: string): string[] {
  const inner = value.replace(/^\[/, '').replace(/\]$/, '').trim()
  if (!inner) return []
  return inner.split(',').map(s => s.trim().replace(/^["']|["']$/g, '')).filter(Boolean)
}

function serializeArrayValue(items: string[]): string {
  return `[${items.map(i => `"${i}"`).join(', ')}]`
}

function FieldInput({
  field,
  onChange,
  readOnly,
}: {
  field: ParsedField
  onChange: (key: string, newValue: string) => void
  readOnly?: boolean
}) {
  if (field.type === 'boolean') {
    const checked = field.value === 'true'
    return (
      <button
        type="button"
        onClick={() => !readOnly && onChange(field.key, checked ? 'false' : 'true')}
        disabled={readOnly}
        className={`relative w-10 h-5 rounded-full transition-colors ${
          checked ? 'bg-primary' : 'bg-muted-foreground/30'
        } ${readOnly ? 'opacity-70 cursor-default' : 'cursor-pointer'}`}
      >
        <span
          className={`absolute top-0.5 left-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
            checked ? 'translate-x-5' : ''
          }`}
        />
      </button>
    )
  }

  if (field.type === 'number') {
    return (
      <input
        type="number"
        value={field.value}
        onChange={e => onChange(field.key, e.target.value)}
        readOnly={readOnly}
        tabIndex={readOnly ? -1 : undefined}
        className={`w-full px-3 py-1.5 text-sm border rounded-md bg-background ${
          readOnly ? 'text-muted-foreground cursor-default' : 'focus:outline-none focus:ring-2 focus:ring-primary/30'
        }`}
      />
    )
  }

  if (field.type === 'array') {
    const items = parseArrayValue(field.value)
    const [newItem, setNewItem] = useState('')

    return (
      <div className="space-y-2">
        <div className="flex flex-wrap gap-1">
          {items.length === 0 && readOnly && (
            <span className="text-xs text-muted-foreground italic">Empty</span>
          )}
          {items.map((item, i) => (
            <span
              key={i}
              className="inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-xs bg-muted border"
            >
              {item}
              {!readOnly && (
                <button
                  onClick={() => {
                    const next = items.filter((_, j) => j !== i)
                    onChange(field.key, serializeArrayValue(next))
                  }}
                  className="text-muted-foreground hover:text-foreground"
                >
                  <X className="h-3 w-3" />
                </button>
              )}
            </span>
          ))}
        </div>
        {!readOnly && (
          <div className="flex gap-1">
            <input
              type="text"
              value={newItem}
              onChange={e => setNewItem(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter' && newItem.trim()) {
                  e.preventDefault()
                  onChange(field.key, serializeArrayValue([...items, newItem.trim()]))
                  setNewItem('')
                }
              }}
              placeholder="Add item..."
              className="flex-1 px-2 py-1 text-xs border rounded-md bg-background focus:outline-none focus:ring-2 focus:ring-primary/30"
            />
            <button
              onClick={() => {
                if (newItem.trim()) {
                  onChange(field.key, serializeArrayValue([...items, newItem.trim()]))
                  setNewItem('')
                }
              }}
              className="px-2 py-1 text-xs border rounded-md hover:bg-muted"
            >
              Add
            </button>
          </div>
        )}
      </div>
    )
  }

  // String / unknown
  const displayValue = field.value.replace(/^["']|["']$/g, '')
  return (
    <input
      type={field.isSecret ? 'password' : 'text'}
      value={displayValue}
      onChange={e => onChange(field.key, `"${e.target.value}"`)}
      readOnly={readOnly}
      tabIndex={readOnly ? -1 : undefined}
      className={`w-full px-3 py-1.5 text-sm border rounded-md bg-background ${
        readOnly ? 'text-muted-foreground cursor-default' : 'focus:outline-none focus:ring-2 focus:ring-primary/30'
      }`}
    />
  )
}

function parseSections(content: string): TomlSection[] {
  const lines = content.split('\n')
  const sections: TomlSection[] = []
  let currentKey = '_top'
  let currentLines: string[] = []
  let startLine = 0

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]
    const sectionMatch = line.match(/^\[([^\]]+)\]/)
    if (sectionMatch) {
      if (currentLines.length > 0) {
        sections.push({ key: currentKey, lines: currentLines, startLine, ...getMeta(currentKey) })
      }
      currentKey = sectionMatch[1]
      currentLines = [line]
      startLine = i
    } else {
      currentLines.push(line)
    }
  }
  if (currentLines.length > 0) {
    sections.push({ key: currentKey, lines: currentLines, startLine, ...getMeta(currentKey) })
  }

  return sections
}

function getMeta(key: string) {
  const meta = SECTION_META[key]
  if (meta) return meta
  return { label: key, icon: <Settings className="h-4 w-4 text-gray-400" />, description: '' }
}

function SectionFormCard({
  section,
  editing,
  onChange,
}: {
  section: TomlSection
  editing: boolean
  onChange: (newLines: string[]) => void
}) {
  const fields = parseFields(section.lines)

  const handleFieldChange = (key: string, newValue: string) => {
    const newLines = section.lines.map(line => {
      const match = line.match(/^(\s*)([^=]+?)\s*=\s*(.+)$/)
      if (match && match[2].trim() === key) {
        return `${match[1]}${key} = ${newValue}`
      }
      return line
    })
    onChange(newLines)
  }

  return (
    <Card className={editing ? 'ring-2 ring-primary/20' : ''}>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            {section.icon}
            <CardTitle className="text-base">{section.label}</CardTitle>
          </div>
          <Badge variant="outline" className="text-xs font-mono">
            {section.key === '_top' ? 'root' : `[${section.key}]`}
          </Badge>
        </div>
        {section.description && (
          <CardDescription className="text-xs">{section.description}</CardDescription>
        )}
      </CardHeader>
      <CardContent>
        {fields.length > 0 ? (
          <div className="space-y-3">
            {fields.map(field => (
              <div key={field.key}>
                <div className="flex items-center gap-2 mb-1">
                  <label className="text-xs font-medium font-mono">{field.key}</label>
                  {field.isSecret && (
                    <span className="text-[10px] text-muted-foreground bg-muted px-1.5 py-0.5 rounded">secret</span>
                  )}
                </div>
                {field.comment && (
                  <p className="text-[11px] text-muted-foreground mb-1">{field.comment}</p>
                )}
                <FieldInput field={field} onChange={handleFieldChange} readOnly={!editing} />
              </div>
            ))}
          </div>
        ) : editing ? (
          <textarea
            value={section.lines.join('\n')}
            onChange={e => onChange(e.target.value.split('\n'))}
            className="w-full font-mono text-xs bg-muted/50 border rounded-md p-3 min-h-[120px] resize-y focus:outline-none focus:ring-2 focus:ring-primary/30"
            spellCheck={false}
          />
        ) : (
          <p className="text-xs text-muted-foreground italic">Empty section</p>
        )}
      </CardContent>
    </Card>
  )
}

export default function ConfigPage() {
  const { data, isLoading, error, mutate } = useSWR<ConfigResponse>('config', fetchConfig)
  const [editContent, setEditContent] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [saveResult, setSaveResult] = useState<{ ok: boolean; message: string } | null>(null)
  const [mode, setMode] = useState<'sections' | 'raw'>('sections')

  const isEditing = editContent !== null
  const content = isEditing ? editContent : (data?.content ?? '')
  const sections = parseSections(content)

  useEffect(() => {
    if (saveResult) {
      const t = setTimeout(() => setSaveResult(null), 5000)
      return () => clearTimeout(t)
    }
  }, [saveResult])

  const handleSave = useCallback(async () => {
    if (!editContent) return
    setSaving(true)
    setSaveResult(null)
    try {
      const result = await saveConfig(editContent)
      setSaveResult(result)
      setEditContent(null)
      mutate()
    } catch (e: any) {
      const msg = e?.message || 'Unknown error'
      let detail = msg
      try {
        if (msg.includes('Failed to put')) {
          detail = msg
        }
      } catch {}
      setSaveResult({ ok: false, message: detail })
    } finally {
      setSaving(false)
    }
  }, [editContent, mutate])

  const handleSectionChange = useCallback((sectionIdx: number, newLines: string[]) => {
    const allSections = parseSections(editContent ?? '')
    allSections[sectionIdx] = { ...allSections[sectionIdx], lines: newLines }
    const rebuilt = allSections.map(s => s.lines.join('\n')).join('\n')
    setEditContent(rebuilt)
  }, [editContent])

  return (
    <div className="space-y-6">
      <PageHeader title="Configuration" description="View and edit your claudear.toml config file" />

      {/* Toolbar */}
      <div className="flex items-center gap-3 flex-wrap">
        {data?.path && (
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground bg-muted/50 px-3 py-1.5 rounded-md">
            <FileText className="h-3.5 w-3.5" />
            <span className="font-mono">{data.path}</span>
          </div>
        )}

        <div className="flex-1" />

        {/* View mode toggle */}
        <div className="flex items-center border rounded-md overflow-hidden text-xs">
          <button
            onClick={() => setMode('sections')}
            className={`px-3 py-1.5 transition-colors ${mode === 'sections' ? 'bg-primary text-primary-foreground' : 'bg-card hover:bg-muted'}`}
          >
            Sections
          </button>
          <button
            onClick={() => setMode('raw')}
            className={`px-3 py-1.5 transition-colors ${mode === 'raw' ? 'bg-primary text-primary-foreground' : 'bg-card hover:bg-muted'}`}
          >
            Raw
          </button>
        </div>

        {!isEditing ? (
          <button
            onClick={() => setEditContent(data?.content ?? '')}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
          >
            <Pencil className="h-3.5 w-3.5" />
            Edit
          </button>
        ) : (
          <>
            <button
              onClick={() => { setEditContent(null); setSaveResult(null) }}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium border hover:bg-muted transition-colors"
            >
              <RotateCcw className="h-3.5 w-3.5" />
              Discard
            </button>
            <button
              onClick={handleSave}
              disabled={saving}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium bg-green-600 text-white hover:bg-green-700 disabled:opacity-50 transition-colors"
            >
              <Save className="h-3.5 w-3.5" />
              {saving ? 'Saving...' : 'Save'}
            </button>
          </>
        )}
      </div>

      {/* Save result toast */}
      {saveResult && (
        <div className={`flex items-center gap-2 px-4 py-3 rounded-md text-sm ${
          saveResult.ok
            ? 'bg-green-50 border border-green-200 text-green-800'
            : 'bg-red-50 border border-red-200 text-red-800'
        }`}>
          {saveResult.ok ? <CheckCircle2 className="h-4 w-4" /> : <AlertTriangle className="h-4 w-4" />}
          {saveResult.message}
        </div>
      )}

      {/* Loading */}
      {isLoading && (
        <div className="space-y-4">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-32" />
          ))}
        </div>
      )}

      {/* Error */}
      {error && (
        <Card>
          <CardContent className="p-6">
            <div className="flex items-center gap-2 text-red-600">
              <AlertTriangle className="h-5 w-5" />
              <span className="text-sm">Failed to load config: {error.message}</span>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Section view */}
      {!isLoading && !error && mode === 'sections' && (
        <div className="grid gap-4 md:grid-cols-2">
          {sections.map((section, idx) => (
            <SectionFormCard
              key={section.key + '-' + idx}
              section={section}
              editing={isEditing}
              onChange={(newLines) => handleSectionChange(idx, newLines)}
            />
          ))}
        </div>
      )}

      {/* Raw view */}
      {!isLoading && !error && mode === 'raw' && (
        <Card>
          <CardHeader className="pb-3">
            <div className="flex items-center gap-2">
              <FileText className="h-4 w-4 text-muted-foreground" />
              <CardTitle className="text-base">claudear.toml</CardTitle>
            </div>
          </CardHeader>
          <CardContent>
            {isEditing ? (
              <textarea
                value={editContent}
                onChange={e => setEditContent(e.target.value)}
                className="w-full font-mono text-xs bg-muted/50 border rounded-md p-4 min-h-[600px] resize-y focus:outline-none focus:ring-2 focus:ring-primary/30"
                spellCheck={false}
              />
            ) : (
              <pre className="text-xs font-mono text-muted-foreground bg-muted/30 rounded-md p-4 overflow-x-auto whitespace-pre max-h-[700px] overflow-y-auto">
                {content || 'No content'}
              </pre>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}
