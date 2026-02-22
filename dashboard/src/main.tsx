import React from 'react'
import ReactDOM from 'react-dom/client'
import { SWRConfig } from 'swr'
import { initSentry } from './lib/sentry'
import App from './App'

initSentry()

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <SWRConfig
      value={{
        dedupingInterval: 2_000,
        revalidateOnFocus: false,
        keepPreviousData: true,
        errorRetryCount: 2,
      }}
    >
      <App />
    </SWRConfig>
  </React.StrictMode>,
)
