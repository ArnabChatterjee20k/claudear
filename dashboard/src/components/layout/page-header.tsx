import { useEffect } from 'react'

export function PageHeader({ title, description }: { title: string; description: string }) {
  useEffect(() => {
    document.title = `${title} - Claudear`
  }, [title])

  return (
    <div className="mb-6">
      <h2 className="text-2xl font-bold font-mono tracking-tight">{title}</h2>
      <p className="text-muted-foreground">{description}</p>
    </div>
  )
}
