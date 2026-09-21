import { forgetToken, token } from './session'
import type { Action, DoctorView, EnvView, LogLine, StateView } from './types'

export class ApiError extends Error {
  readonly hint?: string
  constructor(message: string, hint?: string) {
    super(message)
    this.hint = hint
  }
}

function headers(): HeadersInit {
  // Not a cookie and not a query parameter. `Authorization` is not a
  // CORS-simple header, so a page on another origin that tried this call
  // would be preflighted — and the studio answers no preflight. A cookie
  // would have been sent for it automatically, which is the whole problem
  // with cookies here.
  return { Authorization: `Bearer ${token()}` }
}

async function unwrap<T>(response: Response): Promise<T> {
  if (response.ok) return (await response.json()) as T

  if (response.status === 401) {
    // Either there was never a token, or the studio restarted and minted a
    // new one. Both need the same thing — the URL it printed — and a stale
    // token kept around would fail every reload in silence.
    forgetToken()
    throw new ApiError(
      'this session token is not this studio\u2019s',
      'reopen the URL that `kobune studio` printed',
    )
  }

  const body = await response.json().catch(() => null)
  throw new ApiError(body?.error ?? `the studio answered ${response.status}`, body?.hint ?? undefined)
}

export async function fetchState(signal?: AbortSignal): Promise<StateView> {
  return unwrap(await fetch('/api/state', { headers: headers(), signal }))
}

export async function fetchDoctor(signal?: AbortSignal): Promise<DoctorView> {
  return unwrap(await fetch('/api/doctor', { headers: headers(), signal }))
}

export async function fetchEnv(
  path: string | null,
  service?: string,
  signal?: AbortSignal,
): Promise<EnvView> {
  const query = new URLSearchParams()
  if (path) query.set('path', path)
  if (service) query.set('service', service)
  return unwrap(await fetch(`/api/env?${query}`, { headers: headers(), signal }))
}

export async function act(action: Action): Promise<void> {
  const response = await fetch('/api/act', {
    method: 'POST',
    headers: { ...headers(), 'Content-Type': 'application/json' },
    body: JSON.stringify(action),
  })
  await unwrap(response)
}

/**
 * Follows a workspace's logs.
 *
 * `fetch` rather than `EventSource`, for one reason: `EventSource` cannot
 * set a header, so the token would have to travel as a query parameter —
 * in the URL, and so in every log and history entry that records one. The
 * cost is parsing the event stream here, which is a dozen lines.
 *
 * The returned function aborts. That closes the connection, which is what
 * makes the daemon let go of the runtime's log stream: `docs/DESIGN.md`
 * §10 counts a leaked follower per opened pane as the failure worth
 * designing against.
 */
export function followLogs(
  path: string | null,
  service: string | undefined,
  onLine: (line: LogLine) => void,
  /** Called when the follow ends, cleanly or otherwise. */
  onEnd: (error?: Error) => void,
): () => void {
  const controller = new AbortController()

  const query = new URLSearchParams({ follow: 'true' })
  if (path) query.set('path', path)
  if (service) query.set('service', service)

  void (async () => {
    try {
      const response = await fetch(`/api/logs?${query}`, {
        headers: headers(),
        signal: controller.signal,
      })

      if (!response.ok || !response.body) {
        await unwrap(response)
        return
      }

      const reader = response.body.pipeThrough(new TextDecoderStream()).getReader()
      let buffer = ''

      for (;;) {
        const { done, value } = await reader.read()
        // **A clean end is still an end.** The daemon closing the stream —
        // a restart, or the runtime letting go — arrives as `done` rather
        // than as an error, and a pane that only cleared its state on the
        // error path would go on claiming to follow nothing.
        if (done) {
          onEnd()
          break
        }

        buffer += value

        // SSE frames are separated by a blank line; a frame's payload is
        // its `data:` lines joined. Keep the tail — a frame can be split
        // across two reads.
        let split: number
        while ((split = buffer.indexOf('\n\n')) !== -1) {
          const frame = buffer.slice(0, split)
          buffer = buffer.slice(split + 2)

          const data = frame
            .split('\n')
            .filter((line) => line.startsWith('data:'))
            .map((line) => line.slice(5).trimStart())
            .join('\n')

          if (!data) continue
          try {
            onLine(JSON.parse(data) as LogLine)
          } catch {
            // A keep-alive comment, or a frame that is not ours.
          }
        }
      }
    } catch (error) {
      if (controller.signal.aborted) return
      onEnd(error instanceof Error ? error : new Error(String(error)))
    }
  })()

  return () => controller.abort()
}
