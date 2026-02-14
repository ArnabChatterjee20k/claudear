import { Router } from './router'
import { AppShell } from './components/layout/app-shell'
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

const routes: Record<string, () => JSX.Element> = {
  '/': OverviewPage,
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
}

function App() {
  return (
    <AppShell>
      <Router routes={routes} />
    </AppShell>
  )
}

export default App
