import { useState, type FormEvent } from 'react'
import useSWR from 'swr'
import { Plus, X } from 'lucide-react'
import {
  createExperiment,
  fetchExperiments,
  updateExperiment,
  type PromptExperiment,
} from '../lib/api'
import { useAuth } from '../lib/auth'
import { PageHeader } from '../components/layout/page-header'
import { CardStackSkeleton } from '../components/shared/page-skeletons'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { EmptyState } from '../components/shared/empty-state'
import { Modal } from '../components/shared/modal'

export default function ExperimentsPage() {
  const { user } = useAuth()
  const isAdmin = user?.role === 'admin'
  const { data, error, isLoading, mutate } = useSWR<PromptExperiment[]>(
    'experiments',
    fetchExperiments,
    { refreshInterval: 30000 },
  )
  const [showCreateForm, setShowCreateForm] = useState(false)
  const [experimentName, setExperimentName] = useState('')
  const [variant, setVariant] = useState('control')
  const [promptTemplate, setPromptTemplate] = useState('')
  const [createError, setCreateError] = useState('')
  const [creating, setCreating] = useState(false)

  const [actionError, setActionError] = useState('')
  const [editingExperiment, setEditingExperiment] = useState<PromptExperiment | null>(null)
  const [editExperimentName, setEditExperimentName] = useState('')
  const [editVariant, setEditVariant] = useState('')
  const [editPromptTemplate, setEditPromptTemplate] = useState('')
  const [editError, setEditError] = useState('')
  const [savingEdit, setSavingEdit] = useState(false)
  const [deactivatingId, setDeactivatingId] = useState<number | null>(null)

  const experiments = Array.isArray(data) ? data : []
  const groups: Record<string, PromptExperiment[]> = {}
  for (const exp of experiments) {
    if (!groups[exp.experiment_name]) {
      groups[exp.experiment_name] = []
    }
    groups[exp.experiment_name].push(exp)
  }

  const groupNames = Object.keys(groups).sort()

  const resetCreateForm = () => {
    setExperimentName('')
    setVariant('control')
    setPromptTemplate('')
    setCreateError('')
    setShowCreateForm(false)
  }

  const closeEditModal = () => {
    setEditingExperiment(null)
    setEditExperimentName('')
    setEditVariant('')
    setEditPromptTemplate('')
    setEditError('')
  }

  const openEditModal = (exp: PromptExperiment) => {
    setActionError('')
    setEditError('')
    setEditingExperiment(exp)
    setEditExperimentName(exp.experiment_name)
    setEditVariant(exp.variant)
    setEditPromptTemplate(exp.prompt_template)
  }

  const handleCreate = async (e: FormEvent) => {
    e.preventDefault()
    setCreateError('')
    setActionError('')

    if (!experimentName.trim() || !variant.trim() || !promptTemplate.trim()) {
      setCreateError('Experiment name, variant, and prompt template are required.')
      return
    }

    setCreating(true)
    try {
      await createExperiment({
        experiment_name: experimentName.trim(),
        variant: variant.trim(),
        prompt_template: promptTemplate,
        active: true,
      })
      resetCreateForm()
      void mutate()
    } catch {
      setCreateError('Failed to create experiment.')
    } finally {
      setCreating(false)
    }
  }

  const handleEditSave = async (e: FormEvent) => {
    e.preventDefault()
    if (!editingExperiment) return

    setEditError('')
    setActionError('')

    if (!editExperimentName.trim() || !editVariant.trim() || !editPromptTemplate.trim()) {
      setEditError('Experiment name, variant, and prompt template are required.')
      return
    }

    setSavingEdit(true)
    try {
      await updateExperiment(editingExperiment.id, {
        experiment_name: editExperimentName.trim(),
        variant: editVariant.trim(),
        prompt_template: editPromptTemplate,
        active: editingExperiment.active,
      })
      closeEditModal()
      void mutate()
    } catch {
      setEditError('Failed to update experiment.')
    } finally {
      setSavingEdit(false)
    }
  }

  const handleDeactivate = async (exp: PromptExperiment) => {
    if (!confirm(`Deactivate "${exp.experiment_name}" / "${exp.variant}"?`)) return

    setActionError('')
    setEditError('')
    setDeactivatingId(exp.id)
    try {
      await updateExperiment(exp.id, {
        experiment_name: exp.experiment_name,
        variant: exp.variant,
        prompt_template: exp.prompt_template,
        active: false,
      })
      if (editingExperiment?.id === exp.id) {
        closeEditModal()
      }
      void mutate()
    } catch {
      setActionError('Failed to deactivate experiment.')
      if (editingExperiment?.id === exp.id) {
        setEditError('Failed to deactivate experiment.')
      }
    } finally {
      setDeactivatingId(current => (current === exp.id ? null : current))
    }
  }

  return (
    <div className="space-y-6">
      <PageHeader title="Experiments" description="Prompt A/B testing results" />

      {isAdmin && (
        <Card>
          <CardHeader className="pb-3">
            <div className="flex items-center justify-between gap-3">
              <div>
                <CardTitle>Create Experiment</CardTitle>
                <CardDescription>
                  Add a new prompt variant to start tracking results.
                </CardDescription>
              </div>
              {!showCreateForm ? (
                <button
                  type="button"
                  onClick={() => {
                    setCreateError('')
                    setShowCreateForm(true)
                  }}
                  className="inline-flex items-center gap-2 px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm font-medium hover:bg-primary/90"
                >
                  <Plus className="h-4 w-4" />
                  Create
                </button>
              ) : (
                <button
                  type="button"
                  onClick={resetCreateForm}
                  className="inline-flex items-center gap-2 px-3 py-2 border rounded-md text-sm hover:bg-muted"
                >
                  <X className="h-4 w-4" />
                  Cancel
                </button>
              )}
            </div>
          </CardHeader>
          {showCreateForm && (
            <CardContent>
              {createError && (
                <div className="mb-4 rounded-md bg-destructive/10 text-destructive text-sm p-3">
                  {createError}
                </div>
              )}
              <form onSubmit={handleCreate} className="space-y-4">
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <div className="space-y-1">
                    <label className="text-sm font-medium">Experiment Name</label>
                    <input
                      value={experimentName}
                      onChange={e => setExperimentName(e.target.value)}
                      placeholder="e.g. prompt-quality-v1"
                      required
                      className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                    />
                  </div>
                  <div className="space-y-1">
                    <label className="text-sm font-medium">Variant</label>
                    <input
                      value={variant}
                      onChange={e => setVariant(e.target.value)}
                      placeholder="control"
                      required
                      className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                    />
                  </div>
                </div>

                <div className="space-y-1">
                  <label className="text-sm font-medium">Prompt Template</label>
                  <textarea
                    value={promptTemplate}
                    onChange={e => setPromptTemplate(e.target.value)}
                    placeholder="Write the prompt template for this variant..."
                    rows={6}
                    required
                    className="w-full px-3 py-2 border rounded-md bg-background text-sm font-mono focus:outline-none focus:ring-2 focus:ring-primary resize-y"
                  />
                </div>

                <div className="flex justify-end gap-2">
                  <button
                    type="button"
                    onClick={resetCreateForm}
                    className="px-3 py-2 border rounded-md text-sm hover:bg-muted"
                  >
                    Cancel
                  </button>
                  <button
                    type="submit"
                    disabled={creating}
                    className="px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
                  >
                    {creating ? 'Creating...' : 'Create Experiment'}
                  </button>
                </div>
              </form>
            </CardContent>
          )}
        </Card>
      )}

      {actionError && (
        <div className="rounded-md bg-destructive/10 text-destructive text-sm p-3">
          {actionError}
        </div>
      )}

      {error && (
        <div className="text-destructive text-sm">Failed to load experiments.</div>
      )}

      {isLoading && <CardStackSkeleton count={3} itemClassName="h-48 w-full" />}

      {Array.isArray(data) && groupNames.length === 0 && (
        <EmptyState message="No experiments recorded yet" />
      )}

      {groupNames.map(name => {
        const variants = groups[name]

        return (
          <Card key={name}>
            <CardHeader>
              <CardTitle>{name}</CardTitle>
              <CardDescription>
                {variants.length} variant{variants.length !== 1 ? 's' : ''}
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b">
                      <th className="text-left py-2 font-medium">Variant</th>
                      <th className="text-right py-2 font-medium">Success</th>
                      <th className="text-right py-2 font-medium">Failure</th>
                      <th className="text-right py-2 font-medium">Success Rate</th>
                      <th className="text-right py-2 font-medium">Avg Merge Time</th>
                      <th className="text-right py-2 font-medium">Avg Review Score</th>
                      {isAdmin && <th className="text-right py-2 font-medium">Actions</th>}
                    </tr>
                  </thead>
                  <tbody>
                    {variants.map(v => {
                      const total = v.success_count + v.failure_count
                      const rate = total > 0 ? (v.success_count / total) * 100 : 0
                      const isDeactivating = deactivatingId === v.id

                      return (
                        <tr key={v.id} className="border-b last:border-0">
                          <td className="py-2 font-medium">{v.variant}</td>
                          <td className="py-2 text-right text-green-600">
                            {v.success_count}
                          </td>
                          <td className="py-2 text-right text-red-600">
                            {v.failure_count}
                          </td>
                          <td className="py-2 text-right font-medium">
                            {rate.toFixed(1)}%
                          </td>
                          <td className="py-2 text-right">
                            {v.avg_time_to_merge != null
                              ? `${v.avg_time_to_merge.toFixed(1)}m`
                              : '--'}
                          </td>
                          <td className="py-2 text-right">
                            {v.avg_review_score != null
                              ? v.avg_review_score.toFixed(2)
                              : '--'}
                          </td>
                          {isAdmin && (
                            <td className="py-2 text-right">
                              <div className="inline-flex items-center gap-2">
                                <button
                                  type="button"
                                  onClick={() => openEditModal(v)}
                                  className="px-2 py-1 border rounded-md text-xs hover:bg-muted"
                                >
                                  Edit
                                </button>
                                <button
                                  type="button"
                                  onClick={() => void handleDeactivate(v)}
                                  disabled={isDeactivating}
                                  className="px-2 py-1 border rounded-md text-xs text-destructive border-destructive/30 hover:bg-destructive/10 disabled:opacity-50"
                                >
                                  {isDeactivating ? 'Deactivating...' : 'Deactivate'}
                                </button>
                              </div>
                            </td>
                          )}
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
              </div>
            </CardContent>
          </Card>
        )
      })}

      <Modal
        open={!!editingExperiment}
        onClose={() => {
          if (!savingEdit) closeEditModal()
        }}
        title={editingExperiment ? `Edit Variant: ${editingExperiment.variant}` : 'Edit Variant'}
      >
        {editError && (
          <div className="mb-4 rounded-md bg-destructive/10 text-destructive text-sm p-3">
            {editError}
          </div>
        )}

        <form onSubmit={handleEditSave} className="space-y-4">
          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            <div className="space-y-1">
              <label className="text-sm font-medium">Experiment Name</label>
              <input
                value={editExperimentName}
                onChange={e => setEditExperimentName(e.target.value)}
                required
                className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              />
            </div>
            <div className="space-y-1">
              <label className="text-sm font-medium">Variant</label>
              <input
                value={editVariant}
                onChange={e => setEditVariant(e.target.value)}
                required
                className="w-full px-3 py-2 border rounded-md bg-background text-sm focus:outline-none focus:ring-2 focus:ring-primary"
              />
            </div>
          </div>

          <div className="space-y-1">
            <label className="text-sm font-medium">Prompt Template</label>
            <textarea
              value={editPromptTemplate}
              onChange={e => setEditPromptTemplate(e.target.value)}
              rows={8}
              required
              className="w-full px-3 py-2 border rounded-md bg-background text-sm font-mono focus:outline-none focus:ring-2 focus:ring-primary resize-y"
            />
          </div>

          <div className="flex flex-col-reverse sm:flex-row sm:items-center sm:justify-between gap-2">
            <button
              type="button"
              onClick={() => {
                if (editingExperiment) void handleDeactivate(editingExperiment)
              }}
              disabled={!editingExperiment || deactivatingId === editingExperiment?.id}
              className="px-3 py-2 border rounded-md text-sm text-destructive border-destructive/30 hover:bg-destructive/10 disabled:opacity-50"
            >
              {deactivatingId === editingExperiment?.id ? 'Deactivating...' : 'Deactivate Variant'}
            </button>

            <div className="flex justify-end gap-2">
              <button
                type="button"
                onClick={closeEditModal}
                className="px-3 py-2 border rounded-md text-sm hover:bg-muted"
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={savingEdit}
                className="px-3 py-2 bg-primary text-primary-foreground rounded-md text-sm font-medium hover:bg-primary/90 disabled:opacity-50"
              >
                {savingEdit ? 'Saving...' : 'Save Changes'}
              </button>
            </div>
          </div>
        </form>
      </Modal>
    </div>
  )
}
