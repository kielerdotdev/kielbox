import { Link } from '@tanstack/react-router'
import { Film, Image as ImageIcon, Star } from 'lucide-react'
import type { ItemJson } from '../functions/types'
import { formatDate, formatDuration, formatSize, KIND_LABEL, type Kind } from '../lib/format'
import { Badge } from './ui/badge'

export function Card({ item }: { item: ItemJson }) {
  const isImage = item.content_type.startsWith('image/')
  const thumb = item.thumb_url ?? (isImage ? item.media_url : null)
  return (
    <Link
      to="/items/$itemId"
      params={{ itemId: item.id }}
      className="group block overflow-hidden rounded-card bg-surface ring-1 ring-line transition hover:ring-ring"
    >
      <div className="relative aspect-video bg-deep">
        {thumb ? (
          <img src={thumb} alt="" loading="lazy" className="size-full object-cover" />
        ) : (
          <div className="flex size-full items-center justify-center text-t3">
            {isImage ? <ImageIcon size={28} aria-hidden /> : <Film size={28} aria-hidden />}
          </div>
        )}
        <Badge className="absolute left-2 top-2 bg-black/60 text-t1">{KIND_LABEL[item.kind as Kind]}</Badge>
        {item.favorite ? (
          <Star size={16} className="absolute right-2 top-2 fill-busy text-busy" aria-label="Favorite" />
        ) : null}
      </div>
      <div className="p-3">
        <div className="truncate text-[13px] font-medium" title={item.title}>
          {item.title}
        </div>
        <div className="mt-0.5 flex justify-between text-[12px] text-t3">
          <span>{formatDate(item.captured_at)}</span>
          <span>{item.duration_ms != null ? formatDuration(item.duration_ms) : formatSize(item.size)}</span>
        </div>
      </div>
    </Link>
  )
}
