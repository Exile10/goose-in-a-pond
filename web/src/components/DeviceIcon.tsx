interface Props {
  type?: string
  size?: number
}

export default function DeviceIcon({ type, size = 20 }: Props) {
  const s = size

  switch (type) {
    case 'phone':
      return (
        <svg width={s} height={s} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <rect x="5" y="2" width="14" height="20" rx="2" ry="2" />
          <circle cx="12" cy="18" r="1" fill="currentColor" stroke="none" />
        </svg>
      )
    case 'tablet':
      return (
        <svg width={s} height={s} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="2" width="18" height="20" rx="2" ry="2" />
          <circle cx="12" cy="18" r="1" fill="currentColor" stroke="none" />
        </svg>
      )
    case 'laptop':
      return (
        <svg width={s} height={s} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <rect x="2" y="4" width="20" height="13" rx="2" />
          <line x1="1" y1="17" x2="23" y2="17" />
          <line x1="8" y1="21" x2="16" y2="21" />
          <line x1="10" y1="17" x2="14" y2="21" />
        </svg>
      )
    case 'desktop':
      return (
        <svg width={s} height={s} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <rect x="2" y="3" width="20" height="14" rx="2" />
          <line x1="8" y1="21" x2="16" y2="21" />
          <line x1="12" y1="17" x2="12" y2="21" />
        </svg>
      )
    default:
      return (
        <svg width={s} height={s} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="3" />
          <circle cx="12" cy="12" r="3" />
        </svg>
      )
  }
}
