import { useState, useEffect, useCallback, createContext, useContext } from 'react'

const RouterContext = createContext<{ path: string; navigate: (path: string) => void }>({
  path: '/',
  navigate: () => {},
})

function useHash() {
  const [path, setPath] = useState(() => window.location.hash.slice(1) || '/')

  useEffect(() => {
    const onHashChange = () => setPath(window.location.hash.slice(1) || '/')
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

  const navigate = useCallback((newPath: string) => {
    window.location.hash = newPath
  }, [])

  return { path, navigate }
}

export function useRouter() {
  return useContext(RouterContext)
}

export function RouterProvider({ children }: { children: React.ReactNode }) {
  const value = useHash()
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
