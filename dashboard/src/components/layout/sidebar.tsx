import { useRouter } from '../../router'
import {
  Activity,
  BarChart3,
  AlertTriangle,
  MessageSquare,
  Shield,
  FlaskConical,
  FolderGit2,
  Brain,
  ScrollText,
  LayoutDashboard,
  ListChecks,
  GitPullRequest,
} from 'lucide-react'

const navItems = [
  { path: '/', label: 'Overview', icon: LayoutDashboard },
  { path: '/attempts', label: 'Attempts', icon: ListChecks },
  { path: '/prs', label: 'PRs', icon: GitPullRequest },
  { path: '/analytics', label: 'Analytics', icon: BarChart3 },
  { path: '/errors', label: 'Errors', icon: AlertTriangle },
  { path: '/feedback', label: 'Feedback', icon: MessageSquare },
  { path: '/regressions', label: 'Regressions', icon: Shield },
  { path: '/experiments', label: 'Experiments', icon: FlaskConical },
  { path: '/repos', label: 'Repos', icon: FolderGit2 },
  { path: '/inference', label: 'Inference', icon: Brain },
  { path: '/activity', label: 'Activity', icon: ScrollText },
] as const

export function Sidebar() {
  const { path, navigate } = useRouter()

  return (
    <aside className="w-56 border-r bg-card flex flex-col">
      <div className="p-4 border-b">
        <h1 className="text-lg font-bold flex items-center gap-2">
          <Activity className="h-5 w-5 text-primary" />
          Claudear
        </h1>
      </div>
      <nav className="flex-1 p-2 space-y-0.5 overflow-y-auto">
        {navItems.map(({ path: itemPath, label, icon: Icon }) => {
          const isActive = path === itemPath
          return (
            <button
              key={itemPath}
              onClick={() => navigate(itemPath)}
              className={`w-full flex items-center gap-2 px-3 py-2 rounded-md text-sm transition-colors ${
                isActive
                  ? 'bg-primary/10 text-primary font-medium'
                  : 'text-muted-foreground hover:bg-muted hover:text-foreground'
              }`}
            >
              <Icon className="h-4 w-4 shrink-0" />
              {label}
            </button>
          )
        })}
      </nav>
    </aside>
  )
}
