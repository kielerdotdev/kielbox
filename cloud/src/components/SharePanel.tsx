import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useRouter } from '@tanstack/react-router'
import { Copy } from 'lucide-react'
import { useState } from 'react'
import { createShare, revokeShare } from '../functions/shares'
import type { ShareJson } from '../functions/types'
import { formatStamp, formatLinkStamp } from '../lib/format'
import { AlertDialog } from './ui/alert-dialog'
import { Button } from './ui/button'
import { Input } from './ui/field'
import { ToggleGroup } from './ui/toggle-group'
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
    <aside className="share-panel" aria-labelledby="shares-h">
      <h2 id="shares-h">Share links</h2>
      {fresh ? (
        <div className="fresh-link" role="status">
          <h3>Link created</h3>
          <Input readOnly value={fresh} aria-label="New share link" onFocus={(e) => e.currentTarget.select()} />
          <p>Copy it now. The full link is shown only this once and can't be retrieved later.</p>
          <div className="flex gap-2"><Button variant="primary" onClick={() => void copy(fresh)}><Copy size={14} aria-hidden /> Copy link</Button><Button variant="ghost" onClick={() => setFresh(null)}>Done</Button></div>
        </div>
      ) : (
        <div className="share-create">
          <div><span className="field-label">Expires after</span><div className="expiry-options"><ToggleGroup<Expiry> label="Expires after" value={expiry} onValueChange={setExpiry} choices={[{ value: '1d', label: '1 day' }, { value: '7d', label: '7 days' }, { value: '30d', label: '30 days' }, { value: 'never', label: 'Never' }]} /></div></div>
          <Switch checked={allowDownload} onCheckedChange={setAllowDownload} label={<span className="download-label">Allow download<span>Recipients get a download button</span></span>} />
          <Button variant="primary" disabled={create.isPending} onClick={() => create.mutate()}>{create.isPending ? 'Creating…' : 'Create link'}</Button>
        </div>
      )}
      <div className="share-list">
        {shares.length ? <h3 className="field-label">Links</h3> : <div className="shares-empty"><h3>No links yet</h3><p>Anyone with a link can view this capture without an account. Links you create are listed here.</p></div>}
        <ul>
          {shares.map((s) => {
            const st = status(s, now)
            return <li key={s.id}>
              <div className="min-w-0">
                <div className="share-row-title"><span className={st.live ? '' : 'text-t2'}>Created {formatLinkStamp(s.created_at)}</span><span className={`share-status status-${st.label.toLowerCase()}`}><i />{st.label}</span></div>
                <p>{s.revoked_at !== null ? `Revoked ${formatLinkStamp(s.revoked_at)}` : s.expires_at === null ? 'Never expires' : `${st.live ? 'Expires' : 'Expired'} ${formatLinkStamp(s.expires_at)}`} · {s.view_count} {s.view_count === 1 ? 'view' : 'views'} · Download {s.allow_download ? 'on' : 'off'}</p>
              </div>
              {st.live ? <Button variant="ghost" onClick={() => setRevoking(s)} aria-label="Revoke link">Revoke</Button> : null}
            </li>
          })}
        </ul>
      </div>
      <AlertDialog open={revoking !== null} onOpenChange={(open) => !open && setRevoking(null)} title="Revoke this link?"
        description={`The link${revoking ? ` created ${formatStamp(revoking.created_at)}` : ''} will stop working right away. Anyone who opens it will see “This link isn't available”. This can't be undone, but you can create a new link.`}
        confirmLabel="Revoke link" pending={revoke.isPending} onConfirm={() => revoking && revoke.mutate(revoking.id)} />
    </aside>
  )
}
