/** The Kobune mark — the same hull and sail the CLI and the docs use. */
export function Mark({ className = '' }: { className?: string }) {
  return (
    <svg viewBox="0 0 512 512" aria-hidden="true" className={`block shrink-0 ${className}`}>
      <g strokeLinejoin="round" strokeWidth={20} fill="currentColor" stroke="currentColor">
        <path d="M96 318 H174 L135 246 Z" opacity="0.4" />
        <path d="M416 318 H338 L377 246 Z" opacity="0.4" />
        <path d="M256 138 L332 318 H180 Z" opacity="0.68" />
        <path d="M52 318 H460 L378 424 H134 Z" />
      </g>
    </svg>
  )
}
