import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useRouter } from '@tanstack/react-router'
import { Copy, Link2, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { createShare, revokeShare } from '../functions/shares'
import type { ShareJson } from '../functions/types'
import { formatStamp } from '../lib/format'
import { AlertDialog } from './ui/alert-dialog'
import { Badge } from './ui/badge'
import { Button } from './ui/button'
import { Input } from './ui/field'
import { Select } from './ui/select'
import { Switch } from './ui/switch'
import { useToast } from './ui/toast'

type Expiry = '1d' | '7d' | '30d' | 'never'
const EXPIRY_SECONDS: Record<Expiry, number | null> = { '1d': 86_400, '7d': 7 * 86_400, '30d': 30 * 86_400, never: null }

function status(share: ShareJson, now: number): { label: string; live: boolean } {
  if (share.revoked_at !== null) return { label: 'Revoked', live: false }
  if (share.expires_at !== null && share.expires_at <= now) return { label: 'Expired', live: false }
  return { label: 'Active', live: true }
}

export function SharePanel({ itemId, shares }: { itemId: string; shares: ShareJson[] }) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const toast = useToast()
  const [expiry, setExpiry] = useState<Expiry>('7d')
  const [allowDownload, setAllowDownload] = useState(true)
  const [fresh, setFresh] = useState<string | null>(null)
  const [revoking, setRevoking] = useState<ShareJson | null>(null)

  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ['items'] })
    await router.invalidate()
  }

  const create = useMutation({
    mutationFn: () =>
      createShare({ data: { id: itemId, expires_in_seconds: EXPIRY_SECONDS[expiry], allow_download: allowDownload } }),
    onSuccess: async (r) => {
      setFresh(r.url) // the only time the link is available
      toast.add({ title: 'Share link created', description: 'Copy it now: it is shown only once.' })
      await refresh()
    },
    onError: (e) => toast.add({ title: 'Could not create the link', description: e.message }),
  })
  const revoke = useMutation({
    mutationFn: (id: string) => revokeShare({ data: { id } }),
    onSuccess: async () => {
      setRevoking(null)
      toast.add({ title: 'Link revoked' })
      await refresh()
    },
    onError: (e) => toast.add({ title: 'Could not revoke the link', description: e.message }),
  })

  const copy = async (url: string) => {
    try {
      await navigator.clipboard.writeText(url)
      toast.add({ title: 'Link copied' })
    } catch {
      toast.add({ title: 'Copy failed', description: 'Select the link and copy it manually.' })
    }
  }
  const now = Date.now()

  return (
    <section className="rounded-card bg-surface p-4 ring-1 ring-line" aria-labelledby="shares-h">
      <h2 id="shares-h" className="flex items-center gap-2 text-[15px] font-semibold">
        <Link2 size={16} aria-hidden /> Share links
      </h2>

      <div className="mt-3 flex flex-wrap items-center gap-3">
        <Select<Expiry>
          label="Expires after"
          value={expiry}
          onValueChange={setExpiry}
          options={[
            { value: '1d', label: 'Expires in 1 day' },
            { value: '7d', label: 'Expires in 7 days' },
            { value: '30d', label: 'Expires in 30 days' },
            { value: 'never', label: 'Never expires' },
          ]}
        />
        <Switch checked={allowDownload} onCheckedChange={setAllowDownload} label="Allow download" />
        <Button variant="primary" disabled={create.isPending} onClick={() => create.mutate()}>
          Create link
        </Button>
      </div>

      {fresh ? (
        <div className="mt-3 flex items-center gap-2" role="status">
          <Input readOnly value={fresh} aria-label="New share link" onFocus={(e) => e.currentTarget.select()} />
          <Button onClick={() => copy(fresh)}>
            <Copy size={14} aria-hidden /> Copy
          </Button>
        </div>
      ) : null}

      <ul className="mt-4 divide-y divide-line">
        {shares.length === 0 ? <li className="py-2 text-[12.5px] text-t3">No links yet.</li> : null}
        {shares.map((s) => {
          const st = status(s, now)
          return (
            <li key={s.id} className="flex items-center justify-between gap-3 py-2 text-[12.5px]">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <Badge className={st.live ? 'bg-accent text-ink' : ''}>{st.label}</Badge>
                  <span className="text-t2">Created {formatStamp(s.created_at)}</span>
                </div>
                <div className="mt-0.5 text-t3">
                  {s.expires_at === null ? 'Never expires' : `Expires ${formatStamp(s.expires_at)}`} · {s.view_count}{' '}
                  {s.view_count === 1 ? 'view' : 'views'} · {s.allow_download ? 'download allowed' : 'view only'}
                </div>
              </div>
              {s.revoked_at === null ? (
                <Button variant="ghost" onClick={() => setRevoking(s)} aria-label="Revoke link">
                  <Trash2 size={14} aria-hidden /> Revoke
                </Button>
              ) : null}
            </li>
          )
        })}
      </ul>

      <AlertDialog
        open={revoking !== null}
        onOpenChange={(open) => !open && setRevoking(null)}
        title="Revoke this link?"
        description="Anyone holding the link loses access immediately. This cannot be undone."
        confirmLabel="Revoke"
        pending={revoke.isPending}
        onConfirm={() => revoking && revoke.mutate(revoking.id)}
      />
    </section>
  )
}
