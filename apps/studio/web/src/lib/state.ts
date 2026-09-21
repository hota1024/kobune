import { useCallback, useEffect, useRef, useState } from 'react'
import { ApiError, fetchState } from './api'
import { hasToken } from './session'
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

  const inFlight = useRef<AbortController | null>(null)

  const read = useCallback(async (force = false) => {
    // **Nothing to ask without a token.** The page is the "open the URL
    // from the terminal" screen in that state, and a poll behind it would
    // write a refusal into the server's log every three seconds for as
    // long as the tab stayed open, saying nothing to anybody.
    if (!hasToken()) {
      setLoading(false)
      return
    }

    // **The timer skips a poll that is still running; a refresh replaces
    // it.** A daemon slower than the interval used to have every poll
    // cancelled by the next one, and since an abandoned poll deliberately
    // settles nothing, the screen sat on `loading` for ever — the one
    // state where it most needed to say what was wrong. A refresh is a
    // person waiting for the result of something they just did, so that
    // one still goes to the front.
    if (inFlight.current) {
      if (!force) return
      inFlight.current.abort()
    }

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
      if (inFlight.current === controller) inFlight.current = null
      if (!controller.signal.aborted) setLoading(false)
    }
  }, [])

  useEffect(() => {
    void read(true)
    const timer = window.setInterval(() => void read(), POLL_MS)
    return () => {
      window.clearInterval(timer)
      inFlight.current?.abort()
    }
  }, [read, nonce])

  const refresh = useCallback(() => setNonce((n) => n + 1), [])

  return { state, error, loading, refresh }
}
