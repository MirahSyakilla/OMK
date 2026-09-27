import { File } from './file'

/**
 * Persistent cache of app icons, so a list opened on a cold manager does not
 * have to ask the bridge for an icon it has already resolved before.
 *
 * This is deliberately a cache of icons the user has *already* looked at, not a
 * prefill. A device that has never opened the list behaves exactly as it did
 * before, and nothing is fetched speculatively.
 *
 * Correctness comes from the version code stored beside each icon:
 * `getPackagesInfo` returns one on every list load anyway, so an entry is only
 * reused while it still matches, which is what makes an app update invalidate
 * correctly. Packages that are no longer installed are pruned on the next save.
 *
 * A whole-file age is kept as a backstop for the one case a version code cannot
 * catch: an icon that changes without a version bump, such as a theme pack or a
 * runtime-swapped adaptive icon.
 */

const CACHE_DIR = '/data/misc/keystore/omk/data/webui'
const CACHE_PATH = `${CACHE_DIR}/icon-cache.json`

/// Bump when the record shape changes, so an old file is discarded rather than
/// misread.
const SCHEMA = 1

/// After this long the whole cache is refetched, covering icon changes that
/// arrive without a version code change.
const MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000

/// Refuse to grow the file past this. A handful of very large icons should not
/// be able to fill the data partition; when the limit is hit the cache is
/// simply not written and behaviour falls back to the bridge.
const MAX_BYTES = 12 * 1024 * 1024

/// How long to wait after a change before writing, so scrolling does not
/// rewrite the file.
const SAVE_DEBOUNCE_MS = 5000

interface CachedIcon {
  /** versionCode at the time the icon was captured */
  v: number
  /** a complete data: URL, as returned by the manager */
  i: string
}

interface CacheFile {
  schema: number
  savedAt: number
  entries: Record<string, CachedIcon>
}

export class IconCache {
  #entries = new Map<string, CachedIcon>()
  #dirty = new Set<string>()
  #timer: ReturnType<typeof setTimeout> | null = null
  #loaded = false
  /** Packages known to have been removed, applied on the next save. */
  #pruned = 0

  /** Read the cache once. A missing, unreadable or stale file is simply empty. */
  async load(): Promise<void> {
    if (this.#loaded) return
    this.#loaded = true
    try {
      const parsed = JSON.parse(await File.read(CACHE_PATH)) as CacheFile
      if (parsed?.schema !== SCHEMA) return
      if (Date.now() - (parsed.savedAt ?? 0) > MAX_AGE_MS) return
      for (const [pkg, entry] of Object.entries(parsed.entries ?? {})) {
        if (entry && typeof entry.i === 'string' && entry.i) {
          this.#entries.set(pkg, { v: Number(entry.v) || 0, i: entry.i })
        }
      }
    } catch {
      // No cache yet, or it is unreadable. Either way the bridge fills the gap.
    }
  }

  /** The stored icon for `pkg`, but only while its version code still matches. */
  get(pkg: string, versionCode: number | undefined): string | null {
    if (!this.#loaded) return null
    const entry = this.#entries.get(pkg)
    if (!entry) return null
    // A missing version code means we cannot prove the icon is current, so it is
    // not reused.
    if (versionCode === undefined || entry.v !== versionCode) return null
    return entry.i
  }

  /** Record a freshly resolved icon and schedule a write. */
  put(pkg: string, versionCode: number | undefined, dataUrl: string): void {
    if (!dataUrl) return
    this.#loaded = true
    const previous = this.#entries.get(pkg)
    if (previous && previous.i === dataUrl && previous.v === versionCode) return
    this.#entries.set(pkg, { v: versionCode ?? 0, i: dataUrl })
    this.#dirty.add(pkg)
    this.#schedule()
  }

  /**
   * Drop entries for packages that are no longer installed.
   *
   * Called once per list load with the set that was actually present, so an
   * uninstalled app does not sit in the file forever.
   */
  retain(installed: Set<string>): void {
    if (!this.#loaded) return
    for (const pkg of [...this.#entries.keys()]) {
      if (!installed.has(pkg)) {
        this.#entries.delete(pkg)
        this.#dirty.add(pkg)
        this.#pruned++
      }
    }
    if (this.#pruned > 0) this.#schedule()
  }

  #schedule(): void {
    if (this.#timer !== null) return
    this.#timer = setTimeout(() => {
      this.#timer = null
      void this.save()
    }, SAVE_DEBOUNCE_MS)
  }

  /** Write the file if anything changed. Never throws. */
  async save(): Promise<void> {
    if (this.#dirty.size === 0) return
    this.#dirty.clear()
    const entries: Record<string, CachedIcon> = {}
    for (const [pkg, entry] of this.#entries) entries[pkg] = entry
    const payload: CacheFile = { schema: SCHEMA, savedAt: Date.now(), entries }
    let serialised: string
    try {
      serialised = JSON.stringify(payload)
    } catch {
      return
    }
    if (serialised.length > MAX_BYTES) {
      // Too large to be worth keeping; the bridge still serves every icon.
      return
    }
    try {
      await File.createDirectory(CACHE_DIR)
      await File.write(CACHE_PATH, serialised)
    } catch {
      // A cache that cannot be written only costs a bridge call next time.
    }
  }

  /** Discard the in-memory cache without touching the file. */
  reset(): void {
    this.#entries.clear()
    this.#dirty.clear()
    if (this.#timer !== null) {
      clearTimeout(this.#timer)
      this.#timer = null
    }
  }
}

export const iconCache = new IconCache()
