// The token the binary minted for this run.
//
// It arrives in the URL fragment, which the browser does not send to the
// server and does not put in `Referer` — so the secret that authenticates
// every request never appears in the request log of the server it
// authenticates to. It is read once and the fragment is then cleared, so a
// screenshot of the address bar is not a credential.
//
// **It is then kept in `sessionStorage`, because a dashboard gets
// reloaded.** Holding it in a variable alone would mean every F5 landed on
// "no session token" and sent you back to the terminal for a fresh URL.
// The storage is per tab and goes when the tab does, and anything able to
// read it — script running on this origin — could read a variable just as
// easily, so the trade buys usability without giving up anything real.

const KEY = 'kobune.studio.token'

/**
 * The workspace `kobune studio -w` asked for, if it asked for one.
 *
 * Read from the same fragment and not remembered: it says what to open on
 * arrival, and storing it would have it override the tab you switched to
 * on every later reload.
 */
const WANTED = new URLSearchParams(window.location.hash.replace(/^#/, '')).get('workspace')

let token_ = read()

function read(): string {
  const hash = window.location.hash.replace(/^#/, '')
  const fromUrl = new URLSearchParams(hash).get('token')

  if (fromUrl) {
    // `replaceState` rather than assigning to `location.hash`: that would
    // leave `#` behind and add a history entry whose only content is the
    // token.
    window.history.replaceState(null, '', window.location.pathname + window.location.search)
    try {
      sessionStorage.setItem(KEY, fromUrl)
    } catch {
      // Private mode, or storage turned off. The tab still works; it just
      // will not survive a reload.
    }
    return fromUrl
  }

  try {
    return sessionStorage.getItem(KEY) ?? ''
  } catch {
    return ''
  }
}

export function token(): string {
  return token_
}

export function hasToken(): boolean {
  return token_.length > 0
}

/** The workspace named on the way in, or `null`. */
export function wantedWorkspace(): string | null {
  return WANTED
}

/**
 * Throws the token away after the server has refused it.
 *
 * A studio that restarted minted a new one, so the stored token is not
 * merely wrong — it is worthless, and keeping it would leave every reload
 * failing the same way with nothing saying why.
 */
export function forgetToken(): void {
  token_ = ''
  try {
    sessionStorage.removeItem(KEY)
  } catch {
    // Nothing was stored.
  }
}
