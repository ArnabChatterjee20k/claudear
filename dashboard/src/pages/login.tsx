import { useEffect, useState } from 'react'
import { useAuth } from '../lib/auth'
import { hideSentryFeedbackWidget } from '../lib/sentry'

export default function LoginPage() {
  const { login } = useAuth()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError('')
    setLoading(true)
    try {
      await login(email, password)
    } catch {
      setError('Invalid email or password')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    hideSentryFeedbackWidget()
  }, [])

  return (
    <div className="relative min-h-screen overflow-hidden bg-background">
      <div className="pointer-events-none absolute inset-0">
        <div className="absolute left-1/2 top-24 h-72 w-72 -translate-x-1/2 rounded-full bg-primary/12 blur-3xl" />
        <div className="absolute inset-x-0 top-0 h-40 bg-gradient-to-b from-primary/5 to-transparent" />
        <div className="absolute bottom-0 left-1/4 h-40 w-40 rounded-full bg-primary/5 blur-2xl" />
      </div>

      <div className="relative z-10 flex min-h-screen items-center justify-center px-6 py-10">
        <section className="w-full max-w-lg">
          <div className="relative overflow-hidden rounded-2xl border border-border/80 bg-card/95 shadow-2xl shadow-black/15 backdrop-blur">
            <div className="absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-primary/70 to-transparent" />

            <div className="border-b border-border/80 bg-background/60 px-4 py-3">
              <div className="relative flex items-center justify-center">
                <div className="absolute left-0 flex items-center gap-2">
                  <span className="h-2.5 w-2.5 rounded-full bg-rose-400/70" />
                  <span className="h-2.5 w-2.5 rounded-full bg-amber-400/70" />
                  <span className="h-2.5 w-2.5 rounded-full bg-primary/80" />
                </div>
                <div className="flex items-center gap-0 font-mono text-sm text-muted-foreground">
                  claudear
                  <span className="ml-[0.1em] inline-block h-[1em] w-[0.5em] rounded-[0.05em] bg-primary" />
                </div>
              </div>
            </div>

            <div className="p-6 sm:p-8">
              <div className="relative mb-6 overflow-hidden rounded-xl border border-border/70 bg-background/70 p-4">
                <div className="pointer-events-none absolute inset-0 bg-gradient-to-br from-primary/8 via-transparent to-transparent" />
                <div className="relative min-w-0">
                  <div className="min-w-0">
                    <h1 className="text-2xl font-semibold tracking-tight text-foreground">
                      Sign in
                    </h1>
                    <p className="mt-1 text-sm text-muted-foreground">
                      Dashboard access
                    </p>
                  </div>
                </div>
              </div>

              <form onSubmit={handleSubmit} className="space-y-4">
                {error && (
                  <div
                    role="alert"
                    className="rounded-lg border border-destructive/20 bg-destructive/10 px-3 py-2.5 text-sm text-destructive"
                  >
                    {error}
                  </div>
                )}

                <div className="space-y-2">
                  <label
                    htmlFor="email"
                    className="text-xs font-semibold uppercase tracking-[0.12em] text-muted-foreground"
                  >
                    Email
                  </label>
                  <input
                    id="email"
                    type="email"
                    value={email}
                    onChange={(e) => setEmail(e.target.value)}
                    required
                    autoFocus
                    autoComplete="email"
                    className="w-full rounded-lg border border-input/90 bg-background/85 px-3 py-2.5 text-sm shadow-inner shadow-black/5 outline-none transition placeholder:text-muted-foreground/70 hover:border-border focus:border-primary/40 focus:ring-2 focus:ring-primary/20"
                    placeholder="admin@example.com"
                  />
                </div>

                <div className="space-y-2">
                  <label
                    htmlFor="password"
                    className="text-xs font-semibold uppercase tracking-[0.12em] text-muted-foreground"
                  >
                    Password
                  </label>
                  <input
                    id="password"
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    required
                    autoComplete="current-password"
                    className="w-full rounded-lg border border-input/90 bg-background/85 px-3 py-2.5 text-sm shadow-inner shadow-black/5 outline-none transition placeholder:text-muted-foreground/70 hover:border-border focus:border-primary/40 focus:ring-2 focus:ring-primary/20"
                    placeholder="Enter your password"
                  />
                </div>

                <button
                  type="submit"
                  disabled={loading}
                  className="group relative w-full overflow-hidden rounded-lg border border-primary/25 bg-primary px-4 py-2.5 text-sm font-semibold text-primary-foreground shadow-lg shadow-primary/20 transition hover:bg-primary/90 disabled:cursor-not-allowed disabled:opacity-60"
                >
                  <span className="absolute inset-0 opacity-0 transition group-hover:opacity-100">
                    <span className="absolute inset-y-0 left-[-30%] w-1/2 rotate-12 bg-white/20 blur-md" />
                  </span>
                  <span className="relative">{loading ? 'Signing in...' : 'Sign in'}</span>
                </button>
              </form>
            </div>
          </div>
        </section>
      </div>
    </div>
  )
}
