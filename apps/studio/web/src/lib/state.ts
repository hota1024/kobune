import { useCallback, useEffect, useRef, useState } from 'react'
import { ApiError, fetchState } from './api'
import type { StateView } from './types'

/**
 * How often the screen is re-read.
 *
 * Three seconds, which is what the TUI and the menu-bar app already use
 * (`docs/DESIGN.md` §10). The socket has no subscription and did not grow
 * one for them; it does not grow one for this either.
 */
const POLL_MS = 3000

export interface Live {
  state: StateView | null
  error: ApiError | null
  /** True until the first answer, so the screen can say so once only. */
  loading: boolean
  /** Re-reads now, for the instant after an action. */
  refresh: () => void
}

export function useStudioState(): Live {
  const [state, setState] = useState<StateView | null>(null)
  const [error, setError] = useState<ApiError | null>(null)
  const [loading, setLoading] = useState(true)
  const [nonce, setNonce] = useState(0)

  // A poll in flight when the next one is due is abandoned rather than
  // queued: the newer answer is the true one, and letting them stack turns
  // a slow daemon into a backlog that never drains.
  const inFlight = useRef<AbortController | null>(null)

  const read = useCallback(async () => {
    inFlight.current?.abort()
    const controller = new AbortController()
    inFlight.current = controller

    try {
      const next = await fetchState(controller.signal)
      setState(next)
      setError(null)
    } catch (caught) {
      if (controller.signal.aborted) return
      setError(
        caught instanceof ApiError ? caught : new ApiError(String(caught)),
      )
    } finally {
      if (!controller.signal.aborted) setLoading(false)
    }
  }, [])

  useEffect(() => {
    void read()
    const timer = window.setInterval(() => void read(), POLL_MS)
    return () => {
      window.clearInterval(timer)
      inFlight.current?.abort()
    }
  }, [read, nonce])

  const refresh = useCallback(() => setNonce((n) => n + 1), [])

  return { state, error, loading, refresh }
}
