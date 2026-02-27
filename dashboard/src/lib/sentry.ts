import * as Sentry from '@sentry/react'

const dsn = process.env.CLAUDEAR_SENTRY_DSN || ''

export function initSentry() {
  if (!dsn) return

  Sentry.init({
    dsn,
    release: process.env.CLAUDEAR_SENTRY_RELEASE || undefined,
    environment: process.env.CLAUDEAR_SENTRY_ENVIRONMENT || undefined,
    integrations: [
      Sentry.browserTracingIntegration(),
      Sentry.replayIntegration(),
      Sentry.captureConsoleIntegration({ levels: ['error', 'warn'] }),
      Sentry.feedbackIntegration({
        colorScheme: document.documentElement.classList.contains('dark') ? 'dark' : 'light',
        autoInject: true,
        showBranding: false,
        formTitle: 'Report an Issue',
        submitButtonLabel: 'Send Report',
        messagePlaceholder: 'Describe the issue or share feedback...',
        themeLight: {
          background: '#ffffff',
          foreground: '#09090B',
          accentBackground: '#22C55E',
          accentForeground: '#ffffff',
          submitBackground: '#22C55E',
          submitBackgroundHover: '#16a34a',
          submitForeground: '#ffffff',
          submitBorder: '#22C55E',
          inputBackground: '#f4f4f5',
          inputForeground: '#09090B',
          inputBorder: '#e4e4e7',
          inputOutlineFocus: '#22C55E',
          formBorderRadius: '8px',
          formContentBorderRadius: '6px',
        },
        themeDark: {
          background: '#111113',
          foreground: '#FAFAFA',
          accentBackground: '#22C55E',
          accentForeground: '#09090B',
          submitBackground: '#22C55E',
          submitBackgroundHover: '#16a34a',
          submitForeground: '#09090B',
          submitBorder: '#22C55E',
          inputBackground: '#09090B',
          inputForeground: '#FAFAFA',
          inputBorder: '#27272A',
          inputOutlineFocus: '#22C55E',
          formBorderRadius: '8px',
          formContentBorderRadius: '6px',
        },
      }),
      Sentry.httpClientIntegration(),
      Sentry.extraErrorDataIntegration(),
      Sentry.reportingObserverIntegration(),
    ],
    tracesSampleRate: 0.2,
    replaysSessionSampleRate: 0.1,
    replaysOnErrorSampleRate: 1.0,
  })

  Sentry.setTag('app.component', 'claudear-dashboard')
}

export function setSentryColorScheme(dark: boolean) {
  const feedback = Sentry.getFeedback()
  if (!feedback) return
  if (typeof feedback.remove === 'function') feedback.remove()
  if (typeof feedback.createWidget === 'function') {
    feedback.createWidget({ colorScheme: dark ? 'dark' : 'light' })
  }
}

export function hideSentryFeedbackWidget() {
  const feedback = Sentry.getFeedback()
  if (!feedback) return
  feedback.remove()
}

export { Sentry }
