import { useState, useEffect, useCallback, createContext, useContext } from 'react'

const RouterContext = createContext<{ path: string; navigate: (path: string) => void }>({
  path: '/',
  navigate: () => {},
})

export function useHash() {
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

export function Router({
  routes,
}: {
  routes: Record<string, () => JSX.Element>
}) {
  const { path, navigate } = useHash()

  const Component = routes[path] ?? routes['/']

  return (
    <RouterContext.Provider value={{ path, navigate }}>
      {Component ? <Component /> : null}
    </RouterContext.Provider>
  )
}
