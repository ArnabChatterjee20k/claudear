import { useState, useEffect, useCallback, createContext, useContext } from 'react'

const RouterContext = createContext<{ path: string; navigate: (path: string) => void }>({
  path: '/',
  navigate: () => {},
})

function useHistory() {
  const [path, setPath] = useState(() => window.location.pathname.split('?')[0])

  useEffect(() => {
    const onPopState = () => setPath(window.location.pathname.split('?')[0])
    window.addEventListener('popstate', onPopState)
    return () => window.removeEventListener('popstate', onPopState)
  }, [])

  const navigate = useCallback((newPath: string) => {
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
  return <RouterContext.Provider value={value}>{children}</RouterContext.Provider>
}

export function Router({
  routes,
}: {
  routes: Record<string, () => JSX.Element>
}) {
  const { path } = useRouter()

  const Component = routes[path] ?? routes['/']

  return Component ? <Component /> : null
}
