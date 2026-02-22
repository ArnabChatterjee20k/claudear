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
      Sentry.feedbackIntegration({
        colorScheme: 'system',
        autoInject: true,
        showBranding: false,
        formTitle: 'Report an Issue',
        submitButtonLabel: 'Send Report',
        messagePlaceholder: 'Describe the issue or share feedback...',
      }),
    ],
    tracesSampleRate: 0.2,
    replaysSessionSampleRate: 0.1,
    replaysOnErrorSampleRate: 1.0,
  })

  Sentry.setTag('app.component', 'claudear-dashboard')
}

export { Sentry }
