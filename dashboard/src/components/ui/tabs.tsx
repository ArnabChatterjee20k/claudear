import { useState } from 'react'
import { cn } from '../../lib/utils'

export function Tabs({
  tabs,
  defaultTab,
  children,
}: {
  tabs: { value: string; label: string }[]
  defaultTab?: string
  children: (activeTab: string) => React.ReactNode
}) {
  const [active, setActive] = useState(defaultTab || tabs[0]?.value || '')

  return (
    <div>
      <div className="border-b mb-4">
        <div className="flex gap-0">
          {tabs.map(tab => (
            <button
              key={tab.value}
              onClick={() => setActive(tab.value)}
              className={cn(
                'px-4 py-2 text-sm font-medium border-b-2 transition-colors -mb-px',
                active === tab.value
                  ? 'border-primary text-primary'
                  : 'border-transparent text-muted-foreground hover:text-foreground',
              )}
            >
              {tab.label}
            </button>
          ))}
        </div>
      </div>
      {children(active)}
    </div>
  )
}
