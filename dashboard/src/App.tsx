import { Router, RouterProvider } from './router'
import { AppShell } from './components/layout/app-shell'
import { AuthProvider, useAuth } from './lib/auth'
import LoginPage from './pages/login'
import OverviewPage from './pages/overview'
import AttemptsPage from './pages/attempts'
import PrsPage from './pages/prs'
import AnalyticsPage from './pages/analytics'
import ErrorsPage from './pages/errors'
import FeedbackPage from './pages/feedback'
import RegressionsPage from './pages/regressions'
import ExperimentsPage from './pages/experiments'
import ReposPage from './pages/repos'
import InferencePage from './pages/inference'
import ActivityPage from './pages/activity'
import UsersPage from './pages/users'
import TelemetryPage from './pages/telemetry'
import ConfigPage from './pages/config'
import IssuesPage from './pages/issues'

const routes: Record<string, () => JSX.Element> = {
  '/': OverviewPage,
  '/issues': IssuesPage,
  '/attempts': AttemptsPage,
  '/prs': PrsPage,
  '/analytics': AnalyticsPage,
  '/errors': ErrorsPage,
  '/feedback': FeedbackPage,
  '/regressions': RegressionsPage,
  '/experiments': ExperimentsPage,
  '/repos': ReposPage,
  '/inference': InferencePage,
  '/activity': ActivityPage,
  '/telemetry': TelemetryPage,
  '/config': ConfigPage,
  '/users': UsersPage,
}

function AuthenticatedApp() {
  const { user, loading } = useAuth()

  if (loading) {
    return (
      <div className="min-h-screen bg-background flex items-center justify-center">
        <div className="text-muted-foreground text-sm">Loading...</div>
      </div>
    )
  }

  if (!user) {
    return <LoginPage />
  }

  return (
    <RouterProvider>
      <AppShell>
        <Router routes={routes} />
      </AppShell>
    </RouterProvider>
  )
}

function App() {
  return (
    <AuthProvider>
      <AuthenticatedApp />
    </AuthProvider>
  )
}

export default App
