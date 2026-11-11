import { useMutation, useQueryClient } from '@tanstack/react-query'
import { createFileRoute, Link, notFound, useNavigate, useRouter } from '@tanstack/react-router'
import { ArrowLeft, Download, MoreHorizontal, Pencil, Star, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { Player } from '../components/Player'
import { SharePanel } from '../components/SharePanel'
import { AlertDialog } from '../components/ui/alert-dialog'
import { Badge } from '../components/ui/badge'
import { Button } from '../components/ui/button'
import { Dialog } from '../components/ui/dialog'
import { Field, Input } from '../components/ui/field'
import { Menu } from '../components/ui/menu'
import { Tooltip } from '../components/ui/tooltip'
import { useToast } from '../components/ui/toast'
import { deleteItem, getItem, updateItem } from '../functions/items'
import { formatDate, formatDuration, formatSize, KIND_LABEL, type Kind } from '../lib/format'

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
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['items'] })
      await router.invalidate()
      setOptimistic({})
    },
  })
  const remove = useMutation({
    mutationFn: () => deleteItem({ data: { id: itemId } }),
    onSuccess: async () => {
      toast.add({ title: 'Deleted', description: item.title })
      await queryClient.invalidateQueries({ queryKey: ['items'] })
      await navigate({ to: '/' })
    },
    onError: (e) => toast.add({ title: 'Could not delete', description: e.message }),
  })

  return (
    <div className="flex flex-col gap-5">
      <Link to="/" className="inline-flex w-fit items-center gap-1.5 text-[13px] text-t2 hover:text-t1">
        <ArrowLeft size={14} aria-hidden /> Library
      </Link>

      <Player item={item} />

      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h1 className="text-xl font-semibold tracking-tight [overflow-wrap:anywhere]">{item.title}</h1>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-[12.5px] text-t3">
            <Badge>{KIND_LABEL[item.kind as Kind]}</Badge>
            <span>{formatDate(item.captured_at)}</span>
            <span>{formatSize(item.size)}</span>
            {item.duration_ms != null ? <span>{formatDuration(item.duration_ms)}</span> : null}
            {item.width && item.height ? (
              <span>
                {item.width}x{item.height}
              </span>
            ) : null}
            <span className="[overflow-wrap:anywhere]">{item.filename}</span>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Tooltip label={item.favorite ? 'Remove from favorites' : 'Add to favorites'}>
            <Button
              aria-label={item.favorite ? 'Remove from favorites' : 'Add to favorites'}
              aria-pressed={item.favorite}
              onClick={() => update.mutate({ favorite: !item.favorite })}
            >
              <Star size={14} className={item.favorite ? 'fill-busy text-busy' : ''} aria-hidden />
            </Button>
          </Tooltip>
          <a
            href={`${item.media_url}?download=1`}
            className="inline-flex h-8 items-center gap-1.5 rounded-control bg-surface-2 px-3 text-[13px] font-medium ring-1 ring-line hover:bg-surface-3"
          >
            <Download size={14} aria-hidden /> Download
          </a>
          <Menu
            trigger={
              <Button aria-label="More actions">
                <MoreHorizontal size={14} aria-hidden />
              </Button>
            }
            items={[
              {
                label: 'Rename',
                icon: <Pencil size={14} aria-hidden />,
                onSelect: () => {
                  setTitle(item.title)
                  setRenaming(true)
                },
              },
              { label: 'Delete', icon: <Trash2 size={14} aria-hidden />, danger: true, onSelect: () => setConfirmDelete(true) },
            ]}
          />
        </div>
      </div>

      <SharePanel itemId={itemId} shares={data.shares} />

      <Dialog open={renaming} onOpenChange={setRenaming} title="Rename">
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault()
            const next = title.trim()
            if (next && next !== item.title) update.mutate({ title: next })
            setRenaming(false)
          }}
        >
          <Field label="Title">
            <Input value={title} maxLength={300} onChange={(e) => setTitle(e.target.value)} autoFocus />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setRenaming(false)}>
              Cancel
            </Button>
            <Button variant="primary" type="submit" disabled={title.trim() === ''}>
              Save
            </Button>
          </div>
        </form>
      </Dialog>

      <AlertDialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        title="Delete this capture?"
        description="The file, its thumbnail and every share link are removed for good."
        confirmLabel="Delete"
        pending={remove.isPending}
        onConfirm={() => remove.mutate()}
      />
    </div>
  )
}
