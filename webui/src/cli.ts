import { exec } from 'kernelsu-alt'
import { File } from './file'
import { GITHUB_REPO, KEYBOX_ALWAYSSTRONG_URL, MOD_ID } from './constant'

const FLASH_REFERER = 'https://flash.android.com'
const FLASHSTATION_KEY_FALLBACK = 'AIzaSyD-bwHpMvFCN3PfRN4Txsw_ECg_iptNfMQ'
/// How long a fetched build list stays usable.
///
/// Flash Station publishes a new build occasionally rather than continuously,
/// so a long TTL keeps the picker instant on every open while still picking up
/// a new build within a day.
const FLASHSTATION_BUILD_TTL_MS = 6 * 60 * 60 * 1000
/// Where the baseline build lists are cached between sessions.
const FLASHSTATION_CACHE_PATH = '/data/adb/omk/flashstation-cache.json'

export interface FlashBuild {
  product: string
  buildId: string
  releaseCandidateName: string
  target: string
  apiLevel?: number
  version?: string
  versionName?: string
  releaseBuildMetadata?: { notes?: string; latest?: boolean }
  previewMetadata?: { releaseTrackName?: string; releaseTrackVersionName?: string; canary?: boolean }
}

export type OmKRestartTarget = 'keymint' | 'injector' | 'all'

const RESTART_MARKERS: Record<OmKRestartTarget, string> = {
  keymint: '/data/adb/omk/restart.keymint',
  injector: '/data/adb/omk/restart.injector',
  all: '/data/adb/omk/restart.all',
}

export class Cli {
  static #basePathPromise: Promise<string> | null = null
  /// The Flash Station API key is the same for every request, so it is resolved
  /// once. Without this, opening the fingerprint picker refetched the key once
  /// per device, which is a network round trip and a possible shell exec each
  /// time, on a WebView.
  static #flashstationKeyPromise: Promise<string> | null = null
  /// Build lists per product, so switching between picker openings or products
  /// does not refetch. Entries are dropped once older than the TTL below.
  static #buildCache = new Map<string, { at: number; builds: FlashBuild[] }>()
  /// In-flight fetches per product, so a tap that races the background warm-up
  /// joins the request already running instead of starting a second one.
  static #buildInFlight = new Map<string, Promise<FlashBuild[]>>()
  /// Baseline build lists persisted to disk, so the picker can populate
  /// immediately and still work with no network at all. The cache file lives
  /// beside the module's other state, is written once per successful fetch, and
  /// is only ever read back as a fallback.
  static async #loadBuildBaseline(): Promise<Record<string, { at: number; builds: FlashBuild[] }>> {
    try {
      const raw = await File.read(FLASHSTATION_CACHE_PATH)
      const parsed = JSON.parse(raw) as Record<string, { at: number; builds: unknown }>
      const entries: Record<string, { at: number; builds: FlashBuild[] }> = {}
      for (const [product, entry] of Object.entries(parsed)) {
        if (!entry || !Array.isArray(entry.builds)) continue
        entries[product] = {
          at: typeof entry.at === 'number' ? entry.at : 0,
          builds: entry.builds.filter(
            (build): build is FlashBuild =>
              !!build &&
              typeof (build as FlashBuild).releaseCandidateName === 'string' &&
              typeof (build as FlashBuild).buildId === 'string',
          ),
        }
      }
      return entries
    } catch {
      return {}
    }
  }

  static async #saveBuildBaseline(): Promise<void> {
    try {
      const payload: Record<string, { at: number; builds: FlashBuild[] }> = {}
      for (const [product, entry] of Cli.#buildCache) payload[product] = entry
      await File.write(FLASHSTATION_CACHE_PATH, JSON.stringify(payload))
    } catch {
      // A cache that cannot be written only costs a network fetch next time.
    }
  }

  constructor() {
    if (!Cli.#basePathPromise) {
      Cli.#basePathPromise = this.#resolveBasePath()
    }
  }

  async getBasePath(): Promise<string> {
    return Cli.#basePathPromise!
  }

  async grepProp(key: string, filePath: string): Promise<string | null> {
    const result = await exec(`grep '^${key}=' '${filePath}' | cut -d'=' -f2-`)
    return result.errno === 0 ? result.stdout.trim() : null
  }

  async getModuleInfo(): Promise<Record<string, string>> {
    const basePath = await this.getBasePath()
    const raw = await File.read(`${basePath}/module.prop`)
    const info: Record<string, string> = {}
    for (const line of raw.split('\n')) {
      const trimmed = line.trim()
      if (trimmed === '' || trimmed.startsWith('#')) continue
      const eqIdx = trimmed.indexOf('=')
      if (eqIdx <= 0) continue
      info[trimmed.slice(0, eqIdx).trim()] = trimmed.slice(eqIdx + 1).trim()
    }
    return info
  }

  async linkRedirect(url: string): Promise<void> {
    if (!/^https:\/\/[-a-zA-Z0-9./?#=&_%]+$/.test(url)) {
      throw new Error('unsupported link')
    }
    const result = await exec(
      `am start -a android.intent.action.VIEW -c android.intent.category.BROWSABLE -d '${url}'`,
    )
    if (result.errno !== 0) window.open(url, '_blank')
  }

  async requestRestart(target: OmKRestartTarget): Promise<void> {
    const marker = RESTART_MARKERS[target]
    if (!marker) throw new Error('unsupported restart target')
    if (!(await File.isDirectory('/data/adb/omk'))) {
      throw new Error('OMK state dir missing')
    }
    await File.createFile(marker)
  }

  async getAospKey(): Promise<string> {
    const basePath = await this.getBasePath()
    return File.read(`${basePath}/keybox.xml`)
  }

  async getAlwaysStrongKey(): Promise<string> {
    const quoted = KEYBOX_ALWAYSSTRONG_URL.replace(/'/g, `'\\''`)
    const result = await exec(
      `curl -fsSL --connect-timeout 15 --max-time 85 '${quoted}' 2>/dev/null || wget -q -T 20 -O - '${quoted}'`,
    )
    if (result.errno !== 0 || !result.stdout.trim()) {
      throw new Error(result.stderr || 'AlwaysStrong keybox download failed')
    }
    return result.stdout
  }

  async getKeyboxSlots(configPath: string): Promise<number[]> {
    if (import.meta.env.DEV) return [1, 2]

    const result = await exec(
      'find "' + configPath + '" -maxdepth 1 -type f -name \'keybox-slot-*.xml\' -print',
    )
    if (result.errno !== 0) return []

    const slots = result.stdout
      .split(/\r?\n/)
      .map((path) => path.match(/\/keybox-slot-(\d+)\.xml$/)?.[1])
      .filter((slot): slot is string => slot !== undefined)
      .map(Number)
      .filter((slot) => Number.isInteger(slot) && slot > 0 && slot <= 1024)

    return [...new Set(slots)].sort((a, b) => a - b)
  }

  async getServiceStatus(): Promise<{
    keymint: boolean
    injector: boolean
    integrity: boolean
    integrityExpected: boolean
  }> {
    if (import.meta.env.DEV) {
      return { keymint: true, injector: true, integrity: true, integrityExpected: true }
    }
    const zygisk = await this.detectIntegrityZygisk()
    const pifBlocked = this.isExternalPif(zygisk.conflict)
    const result = await exec(
      'km=0; inj=0; en=0; pidof keymint >/dev/null 2>&1 && km=1; ks=$(pidof keystore2 2>/dev/null | awk \'{print $1}\'); if [ -n "$ks" ] && grep -qE \'/inject( |$)\' "/proc/$ks/maps" 2>/dev/null; then inj=1; fi; if grep -qE "^enabled[[:space:]]*=[[:space:]]*true" /data/adb/omk/integrity.toml /data/misc/keystore/omk/data/integrity.toml 2>/dev/null; then en=1; fi; printf \'%s %s %s\\n\' "$km" "$inj" "$en"',
    )
    const [km, inj, enabled] = result.stdout.trim().split(/\s+/)
    const integrityExpected = !pifBlocked && enabled === '1'
    return {
      keymint: km === '1',
      injector: inj === '1',
      integrity: integrityExpected && zygisk.provider !== null,
      integrityExpected,
    }
  }

  isExternalPif(conflict: string | null): boolean {
    return conflict === 'playintegrityfix' || conflict === 'playintegrityfork'
  }

  async getFileMtime(path: string): Promise<number | null> {
    if (import.meta.env.DEV) return Date.now()
    const result = await exec(`stat -c %Y "${path}"`)
    if (result.errno !== 0) return null
    const value = Number.parseInt(result.stdout.trim(), 10)
    return Number.isFinite(value) ? value * 1000 : null
  }

  async exportKeybox(src: string, fileName: string): Promise<string> {
    if (!/^[A-Za-z0-9._-]+\.xml$/.test(fileName)) throw new Error('invalid export name')
    const dir = '/storage/emulated/0/Download/OMK'
    const dest = `${dir}/${fileName}`
    await File.createDirectory(dir)
    await File.copy(src, dest)
    return dest
  }

  getRepositoryUrl(): string {
    return `https://github.com/${GITHUB_REPO}`
  }

  async fetchFlashstationKey(): Promise<string> {
    if (import.meta.env.DEV) return FLASHSTATION_KEY_FALLBACK
    if (!Cli.#flashstationKeyPromise) {
      Cli.#flashstationKeyPromise = this.#fetchFirst([`${FLASH_REFERER}/`])
        .then((html) => html.match(/AIzaSy[A-Za-z0-9_-]{33}/)?.[0] ?? FLASHSTATION_KEY_FALLBACK)
        // A failed key lookup falls back to the bundled key, so the rejection is
        // absorbed here and the promise stays cached rather than being retried
        // once per device on the next picker open.
        .catch(() => FLASHSTATION_KEY_FALLBACK)
    }
    return Cli.#flashstationKeyPromise
  }

  async fetchFlashstationBuilds(product: string): Promise<FlashBuild[]> {
    const cached = Cli.#buildCache.get(product)
    if (cached && Date.now() - cached.at < FLASHSTATION_BUILD_TTL_MS) {
      return cached.builds
    }
    const inFlight = Cli.#buildInFlight.get(product)
    if (inFlight) return inFlight
    const request = this.#fetchFlashstationBuilds(product).finally(() => {
      Cli.#buildInFlight.delete(product)
    })
    Cli.#buildInFlight.set(product, request)
    return request
  }

  async #fetchFlashstationBuilds(product: string): Promise<FlashBuild[]> {
    const key = await this.fetchFlashstationKey()
    const url =
      `https://content-flashstation-pa.googleapis.com/v1/builds` +
      `?product=${encodeURIComponent(product)}&key=${encodeURIComponent(key)}`
    try {
      const raw = await this.#fetchFirst([url], { Referer: FLASH_REFERER })
      const parsed = JSON.parse(raw) as { flashstationBuild?: FlashBuild[] }
      const builds = parsed.flashstationBuild
      if (!Array.isArray(builds)) throw new Error('invalid build list')
      const filtered = builds.filter(
        (build): build is FlashBuild =>
          !!build &&
          !!build.releaseCandidateName &&
          !!build.buildId &&
          typeof build.target === 'string',
      )
      Cli.#buildCache.set(product, { at: Date.now(), builds: filtered })
      void Cli.#saveBuildBaseline()
      return filtered
    } catch (error) {
      // Fall back to the persisted baseline so a picker opened without network
      // still lists builds. The stale entry is still preferred over nothing,
      // because an out-of-date list is usable and an empty picker is not.
      const baseline = await Cli.#loadBuildBaseline()
      const entry = baseline[product]
      if (entry && entry.builds.length > 0) {
        Cli.#buildCache.set(product, entry)
        return entry.builds
      }
      throw error
    }
  }

  async getBuildRelease(): Promise<string> {
    if (import.meta.env.DEV) return '16'
    const result = await exec('getprop ro.build.version.release')
    return result.errno === 0 ? result.stdout.trim() : ''
  }

  async #fetchFirst(urls: string[], headers?: Record<string, string>): Promise<string> {
    const curlHeaders = headers
      ? Object.entries(headers).map(([key, value]) => `-H '${key}: ${value}'`).join(' ')
      : ''
    const wgetHeaders = headers
      ? Object.entries(headers).map(([key, value]) => `--header='${key}: ${value}'`).join(' ')
      : ''
    for (const url of urls) {
      try {
        const response = await fetch(url, headers ? { headers } : undefined)
        if (response.ok) {
          const text = await response.text()
          if (text.trim()) return text
        }
      } catch {
        // try curl next, then remaining URLs
      }
      const quoted = url.replace(/'/g, `'\\''`)
      const result = await exec(
        `curl -fsSL ${curlHeaders} --connect-timeout 10 --max-time 30 '${quoted}' 2>/dev/null || wget -q -T 20 ${wgetHeaders} -O - '${quoted}'`,
      )
      if (result.errno === 0 && result.stdout.trim()) return result.stdout
    }
    throw new Error('fingerprint fetch failed')
  }

  async detectIntegrityZygisk(): Promise<{ provider: string | null; conflict: string | null }> {
    if (import.meta.env.DEV) return { provider: 'rezygisk', conflict: null }
    const result = await exec(`
provider=none
if [ -d /data/adb/modules/rezygisk ] && [ ! -f /data/adb/modules/rezygisk/disable ]; then
  provider=rezygisk
elif [ -d /data/adb/modules/zygisksu ] && [ ! -f /data/adb/modules/zygisksu/disable ]; then
  provider=zygisk_next
elif [ -d /data/adb/modules/zygisk_next ] && [ ! -f /data/adb/modules/zygisk_next/disable ]; then
  provider=zygisk_next
elif [ -d /data/adb/modules/neozygisk ] && [ ! -f /data/adb/modules/neozygisk/disable ]; then
  provider=neozygisk
elif command -v magisk >/dev/null 2>&1; then
  v=$(magisk --sqlite "SELECT value FROM settings WHERE key='zygisk'" 2>/dev/null)
  [ "$v" = 1 ] && provider=magisk
fi
conflict=none
for id in playintegrityfix playintegrityfork; do
  if [ -d "/data/adb/modules/$id" ] && [ ! -f "/data/adb/modules/$id/disable" ]; then
    conflict=$id
    break
  fi
done
if [ "$conflict" = none ] && [ -d /data/adb/modules/tricky_store/zygisk ] && [ ! -f /data/adb/modules/tricky_store/disable ]; then
  conflict=tricky_store
fi
printf '%s %s\\n' "$provider" "$conflict"
`)
    const [provider, conflict] = result.stdout.trim().split(/\s+/)
    return {
      provider: !provider || provider === 'none' ? null : provider,
      conflict: !conflict || conflict === 'none' ? null : conflict,
    }
  }

  async killIntegrityTargets(): Promise<void> {
    if (import.meta.env.DEV) return
    await exec(
      '(am force-stop com.google.android.gms; am force-stop com.android.vending; am force-stop com.tencent.soter.soterserver; killall -9 com.google.android.gms.unstable) >/dev/null 2>&1 &',
    )
  }

  async unifyProductProps(prop: Record<string, string>): Promise<void> {
    if (import.meta.env.DEV) return
    const fingerprint = prop.FINGERPRINT ?? ''
    const parts = fingerprint.split(/[/:]/)
    const pairs: Array<[string, string]> = [
      ['ro.product.brand', prop.BRAND || parts[0] || ''],
      ['ro.product.name', prop.PRODUCT || parts[1] || ''],
      ['ro.product.device', prop.DEVICE || parts[2] || ''],
      ['ro.product.model', prop.MODEL || ''],
      ['ro.product.manufacturer', prop.MANUFACTURER || prop.BRAND || parts[0] || ''],
    ]
    const quote = (value: string) => `'${value.replace(/'/g, `'\\''`)}'`
    const cmds = pairs
      .filter(([, value]) => value)
      .map(([key, value]) => `resetprop -n ${quote(key)} ${quote(value)}`)
      .join('; ')
    if (!cmds) return
    const result = await exec(cmds)
    if (result.errno !== 0) throw new Error(result.stderr || 'resetprop failed')
  }

  async #resolveBasePath(): Promise<string> {
    const candidates = [
      `/data/adb/modules/${MOD_ID}`,
      `/data/adb/modules/.${MOD_ID}`,
    ]

    for (const candidate of candidates) {
      if (await File.exist(candidate)) return candidate
    }

    return candidates[0]
  }
}

/// Icon size requested from the bulk bridge, in pixels.
///
/// Small on purpose. A list row is 48dp, and the `ksu://icon/` handler in some
/// manager builds decodes and compresses at a hardcoded 512px, which is wasted
/// work and wasted heap for every one of the several hundred rows.
const ICON_SIZE_PX = 96

interface BulkIcon {
  packageName?: string
  icon?: string
}

export interface IconBatch {
  packageName: string
  /** A `data:` URL, or an empty string when the bridge had no icon. */
  dataUrl: string
}

/**
 * Bulk icon access, when the manager exposes it.
 *
 * The per-icon `ksu://icon/<pkg>` path is served by `shouldInterceptRequest`,
 * which Chromium dispatches sequentially on a single thread, and a device with
 * several hundred packages therefore queues several hundred serialized native
 * decodes. KernelSU-Next's manager exposes `getPackagesIcons` plus
 * `cacheAllPackageIcons` for exactly this reason: one bridge call, a bounded
 * icon size, and a native cache.
 *
 * `kernelsu-alt` does not wrap those methods, so they are called on the global
 * bridge directly and their presence is detected at runtime. A manager without
 * them reports no support and callers fall back to the lazy per-icon path.
 */
export class BulkIcons {
  /// How many packages per bridge call. Binder transactions have a ~1MB budget
  /// and base64 adds a third, so a page is kept small rather than requesting all
  /// packages at once.
  static readonly PAGE = 24

  static supported(): boolean {
    const bridge = (globalThis as { ksu?: Record<string, unknown> }).ksu
    return typeof bridge?.getPackagesIcons === 'function'
  }

  /// Whether the manager can pre-decode every icon in one call.
  ///
  /// Deliberately not used. `cacheAllPackageIcons` walks every installed app
  /// and decodes and PNG-compresses each one inside a single synchronous
  /// JavascriptInterface call, so invoking it would trade the per-row decode
  /// storm for one long bridge stall. getPackagesIcons populates the same
  /// packageIconCache on demand, so fetching per page is lazy and still warm
  /// for later scrolls.
  static get canWarm(): boolean {
    const bridge = (globalThis as { ksu?: Record<string, unknown> }).ksu
    return typeof bridge?.cacheAllPackageIcons === 'function'
  }

  /// Fetch one page. Callers batch the packages that are actually on screen into
  /// a single call, because a call per package costs a bridge round trip each
  /// and gains nothing over the interception path it replaces.
  static async fetch(packages: string[], sizePx: number = ICON_SIZE_PX): Promise<Map<string, string>> {
    const out = new Map<string, string>()
    if (!BulkIcons.supported() || packages.length === 0) return out
    const bridge = (globalThis as { ksu?: Record<string, unknown> }).ksu
    for (let index = 0; index < packages.length; index += BulkIcons.PAGE) {
      const page = packages.slice(index, index + BulkIcons.PAGE)
      try {
        const raw = (bridge!.getPackagesIcons as (json: string, size: number) => string)(
          JSON.stringify(page),
          sizePx,
        )
        const parsed = JSON.parse(raw) as BulkIcon[]
        if (!Array.isArray(parsed)) continue
        for (const entry of parsed) {
          // The manager already returns a complete data URL, not bare base64:
          // WebViewInterface builds "data:image/png;base64," + base64 itself.
          // Prefixing it again produced a URL that could never load, so the
          // value is used as-is.
          const icon = entry?.icon
          if (entry?.packageName && icon) out.set(entry.packageName, icon)
        }
      } catch {
        // A failed page falls back to the per-icon path for those packages.
      }
    }
    return out
  }
}
