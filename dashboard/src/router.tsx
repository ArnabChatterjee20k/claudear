import { useState, useEffect, useCallback, createContext, useContext, type JSX } from 'react'
import { Sentry } from './lib/sentry'

const RouterContext = createContext<{ path: string; navigate: (path: string) => void }>({
  path: '/',
  navigate: () => {},
})

function useHistory() {
  const [path, setPath] = useState(() => window.location.pathname.split('?')[0])

  useEffect(() => {
    const onPopState = () => {
      const newPath = window.location.pathname.split('?')[0]
      Sentry.addBreadcrumb({ category: 'navigation', message: newPath, level: 'info' })
      setPath(newPath)
    }
    window.addEventListener('popstate', onPopState)
    return () => window.removeEventListener('popstate', onPopState)
  }, [])

  const navigate = useCallback((newPath: string) => {
    Sentry.addBreadcrumb({ category: 'navigation', message: newPath, level: 'info' })
    window.history.pushState(null, '', newPath)
    setPath(newPath.split('?')[0])
  }, [])

  return { path, navigate }
}

export function useRouter() {
  return useContext(RouterContext)
}

export function RouterProvider({ children }: { children: React.ReactNode }) {
  const value = useHistory()
  return <RouterContext value={value}>{children}</RouterContext>
}

export function Router({
  routes,
}: {
  routes: Record<string, () => JSX.Element | null>
}) {
  const { path } = useRouter()

  const Component = routes[path] ?? routes['/']

  return Component ? <Component /> : null
}
