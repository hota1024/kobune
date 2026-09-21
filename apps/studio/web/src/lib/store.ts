/**
 * `localStorage`, for the browsers that do not have one.
 *
 * Every accessor **throws** rather than returning nothing when site data
 * is blocked — a Chrome set to refuse it does this, and so does an iframe
 * with a strict sandbox — and a throw out of a `useState` initialiser is a
 * blank page. What is kept here is which tabs were open and which theme
 * was picked; neither is worth a blank page.
 */
export function read(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

export function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value)
  } catch {
    // Blocked, or out of quota. Both mean this preference does not
    // survive the reload, which is the whole consequence.
  }
}
