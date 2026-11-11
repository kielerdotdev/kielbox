/** The Kielbox mark supplied with the cloud design. */
export function Brand({ cloud = true }: { cloud?: boolean }) {
  return <span className="brand"><svg width="22" height="22" viewBox="0 0 24 24" aria-hidden="true"><rect x="2" y="2" width="20" height="10" rx="3.2" fill="currentColor" className="text-accent" /><path d="M10.4 4.6v4.8l4-2.4z" fill="#141414" /><rect x="2" y="13.8" width="9.1" height="8.2" rx="3" fill="currentColor" /><rect x="12.9" y="13.8" width="9.1" height="8.2" rx="3" fill="currentColor" /></svg><span>Kielbox{cloud && <> <span className="text-t2">Cloud</span></>}</span></span>
}
