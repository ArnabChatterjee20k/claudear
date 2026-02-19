import { createContext, useContext, useState, useEffect, useCallback } from 'react'
import { getMe, login as apiLogin, logout as apiLogout, updateProfile as apiUpdateProfile, setOnUnauthorized, type AuthUser } from './api'

interface AuthState {
  user: AuthUser | null
  loading: boolean
  login: (email: string, password: string) => Promise<void>
  logout: () => Promise<void>
  updateProfile: (data: { name?: string; password?: string; current_password?: string }) => Promise<void>
  refreshUser: () => Promise<void>
}

const AuthContext = createContext<AuthState>({
  user: null,
  loading: true,
  login: async () => {},
  logout: async () => {},
  updateProfile: async () => {},
  refreshUser: async () => {},
})

export function useAuth() {
  return useContext(AuthContext)
}

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [user, setUser] = useState<AuthUser | null>(null)
  const [loading, setLoading] = useState(true)

  const handleUnauthorized = useCallback(() => {
    setUser(null)
  }, [])

  useEffect(() => {
    setOnUnauthorized(handleUnauthorized)
    getMe()
      .then(setUser)
      .catch(() => setUser(null))
      .finally(() => setLoading(false))
  }, [handleUnauthorized])

  const login = useCallback(async (email: string, password: string) => {
    const res = await apiLogin(email, password)
    setUser(res.user)
  }, [])

  const logout = useCallback(async () => {
    await apiLogout()
    setUser(null)
  }, [])

  const refreshUser = useCallback(async () => {
    const refreshed = await getMe()
    setUser(refreshed)
  }, [])

  const updateProfile = useCallback(async (data: { name?: string; password?: string; current_password?: string }) => {
    await apiUpdateProfile(data)
    await refreshUser()
  }, [refreshUser])

  return (
    <AuthContext.Provider value={{ user, loading, login, logout, updateProfile, refreshUser }}>
      {children}
    </AuthContext.Provider>
  )
}
