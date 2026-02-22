import * as Sentry from '@sentry/react'

const dsn = process.env.SENTRY_DSN || ''

export function initSentry() {
  if (!dsn) return

  Sentry.init({
    dsn,
    release: process.env.SENTRY_RELEASE || undefined,
    environment: process.env.SENTRY_ENVIRONMENT || undefined,
    integrations: [
      Sentry.browserTracingIntegration(),
      Sentry.replayIntegration(),
      Sentry.captureConsoleIntegration({ levels: ['error', 'warn'] }),
    ],
    tracesSampleRate: 0.2,
    replaysSessionSampleRate: 0.1,
    replaysOnErrorSampleRate: 1.0,
  })
}

export { Sentry }
