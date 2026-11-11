import { useMutation, useQueryClient } from '@tanstack/react-query'
import { createFileRoute, Link, notFound, useNavigate, useRouter } from '@tanstack/react-router'
import { ArrowLeft, Download, Star, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { Player } from '../components/Player'
import { SharePanel } from '../components/SharePanel'
import { AlertDialog } from '../components/ui/alert-dialog'
import { Button } from '../components/ui/button'
import { Input } from '../components/ui/field'
import { Tooltip } from '../components/ui/tooltip'
import { useToast } from '../components/ui/toast'
import { deleteItem, getItem, updateItem } from '../functions/items'
import { formatStamp, formatDuration, formatSize, KIND_LABEL, type Kind } from '../lib/format'

export const Route = createFileRoute('/items/$itemId')({
  loader: async ({ params }) => {
    try {
      return await getItem({ data: { id: params.itemId } })
    } catch (e) {
      // Unknown, pending and malformed ids all read as "not found".
      if (e instanceof Error && /not found|expected string to have/i.test(e.message)) throw notFound()
      throw e
    }
  },
  component: Detail,
})

function Detail() {
  const { itemId } = Route.useParams()
  const data = Route.useLoaderData()
  const router = useRouter()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const toast = useToast()
  const [renaming, setRenaming] = useState(false)
  const [title, setTitle] = useState(data.item.title)
  const [confirmDelete, setConfirmDelete] = useState(false)
  // Optimistic overlay on top of the loader data until the server answers.
  const [optimistic, setOptimistic] = useState<{ title?: string; favorite?: boolean }>({})
  const item = { ...data.item, ...optimistic }

  const update = useMutation({
    mutationFn: (patch: { title?: string; favorite?: boolean }) => updateItem({ data: { id: itemId, ...patch } }),
    onMutate: (patch) => setOptimistic((o) => ({ ...o, ...patch })),
    onError: (e) => {
      setOptimistic({})
      toast.add({ title: 'Could not save', description: e.message })
    },
    onSuccess: async (_result, patch) => {
      toast.add({ title: patch.title ? 'Title saved' : 'Favorite updated' })
      await queryClient.invalidateQueries({ queryKey: ['items'] })
      await router.invalidate()
      setOptimistic({})
    },
  })
  const remove = useMutation({
    mutationFn: () => deleteItem({ data: { id: itemId } }),
    onSuccess: async () => {
      toast.add({ title: 'Capture deleted' })
      await queryClient.invalidateQueries({ queryKey: ['items'] })
      await navigate({ to: '/' })
    },
    onError: (e) => toast.add({ title: 'Could not delete', description: e.message }),
  })

  const activeShares = data.shares.filter((share) => share.revoked_at === null && (share.expires_at === null || share.expires_at > Date.now())).length
  return (
    <div className="detail-page">
      <div className="detail-toolbar">
        <Link to="/" className="back-button" aria-label="Back to library"><ArrowLeft size={18} aria-hidden /></Link>
        {renaming ? (
          <form className="rename-form" onKeyDown={(e) => { if (e.key === 'Escape') setRenaming(false) }} onSubmit={(e) => {
            e.preventDefault()
            const next = title.trim()
            if (next && next !== item.title) update.mutate({ title: next })
            setRenaming(false)
          }}>
            <Input aria-label="Capture title" value={title} maxLength={300} onChange={(e) => setTitle(e.target.value)} autoFocus />
            <Button variant="primary" type="submit" disabled={!title.trim() || update.isPending}>Save</Button>
            <Button variant="ghost" onClick={() => setRenaming(false)}>Cancel</Button>
            <span className="rename-hint">Enter saves · Esc cancels</span>
          </form>
        ) : (
          <div className="detail-title"><h1>{item.title}</h1><Button variant="ghost" onClick={() => { setTitle(item.title); setRenaming(true) }}>Rename</Button></div>
        )}
        <div className="detail-actions">
          <Tooltip label={item.favorite ? 'Unfavorite' : 'Favorite'}>
            <Button variant="ghost" aria-label={item.favorite ? 'Unfavorite' : 'Favorite'} aria-pressed={item.favorite} disabled={update.isPending} onClick={() => update.mutate({ favorite: !item.favorite })}><Star size={16} fill={item.favorite ? 'currentColor' : 'none'} aria-hidden /></Button>
          </Tooltip>
          <a href={`${item.media_url}?download=1`} className="download-button"><Download size={16} aria-hidden /> Download</a>
          <Button variant="danger" onClick={() => setConfirmDelete(true)}><Trash2 size={16} aria-hidden /> Delete</Button>
        </div>
      </div>
      <div className="detail-columns">
        <div className="detail-media">
          <Player item={item} />
          <dl className="capture-metadata">
            <div><dt>Kind</dt><dd>{KIND_LABEL[item.kind as Kind]}</dd></div>
            <div><dt>Captured</dt><dd>{formatStamp(item.captured_at)}</dd></div>
            {item.width && item.height ? <div><dt>Dimensions</dt><dd className="font-mono">{item.width} × {item.height}</dd></div> : null}
            {item.duration_ms != null ? <div><dt>Duration</dt><dd className="font-mono">{formatDuration(item.duration_ms)}</dd></div> : null}
            <div><dt>Size</dt><dd className="font-mono">{formatSize(item.size)}</dd></div>
          </dl>
        </div>
        <SharePanel itemId={itemId} shares={data.shares} />
      </div>
      <AlertDialog open={confirmDelete} onOpenChange={setConfirmDelete} title="Delete this capture?"
        description={`“${item.title}” will be removed from your library${activeShares ? ` and its ${activeShares} active share ${activeShares === 1 ? 'link will' : 'links will'} stop working` : ''}. This can't be undone.`}
        confirmLabel="Delete capture" pending={remove.isPending} onConfirm={() => remove.mutate()} />
    </div>
  )
}
