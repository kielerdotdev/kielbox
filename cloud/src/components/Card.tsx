import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Link, useRouter } from '@tanstack/react-router'
import { Film, Image as ImageIcon, Star } from 'lucide-react'
import type { ItemJson } from '../functions/types'
import { updateItem } from '../functions/items'
import { formatShortDate, formatDuration, formatSize, KIND_LABEL, type Kind } from '../lib/format'
import { useToast } from './ui/toast'

export function Card({ item }: { item: ItemJson }) {
  const isImage = item.content_type.startsWith('image/')
  const thumb = item.thumb_url ?? (isImage ? item.media_url : null)
  const [failedThumb, setFailedThumb] = useState<string | null>(null)
  const router = useRouter()
  const client = useQueryClient()
  const toast = useToast()
  const favorite = useMutation({
    mutationFn: () => updateItem({ data: { id: item.id, favorite: !item.favorite } }),
    onSuccess: async () => { await client.invalidateQueries({ queryKey: ['items'] }); await router.invalidate() },
    onError: (e) => toast.add({ title: 'Could not save favorite', description: e.message }),
  })
  const starred = favorite.isPending ? !item.favorite : item.favorite
  return (
    <article className="capture-card">
      <Link to="/items/$itemId" params={{ itemId: item.id }} className="capture-thumb" aria-label={`Open ${item.title}`}>
        {thumb && failedThumb !== thumb ? <img src={thumb} alt="" loading="lazy" onError={() => setFailedThumb(thumb)} /> : <div className="thumb-fallback">{isImage ? <ImageIcon size={26} aria-hidden /> : <Film size={26} aria-hidden />}<span>{item.content_type.split('/')[1]?.toUpperCase()}</span></div>}
        <span className="thumb-label">{KIND_LABEL[item.kind as Kind]}</span>
        <span className="thumb-label thumb-detail">{item.duration_ms != null ? formatDuration(item.duration_ms) : formatSize(item.size)}</span>
      </Link>
      <div className="capture-caption">
        <Link to="/items/$itemId" params={{ itemId: item.id }} title={item.title}>{item.title}</Link>
        <span className="capture-date">{formatShortDate(item.captured_at)}</span>
        <button type="button" className="favorite-button" aria-label={starred ? 'Unfavorite' : 'Favorite'} aria-pressed={starred} disabled={favorite.isPending} onClick={() => favorite.mutate()}><Star size={16} fill={starred ? 'currentColor' : 'none'} aria-hidden /></button>
      </div>
    </article>
  )
}
