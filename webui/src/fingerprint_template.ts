import { File } from './file'
import BUNDLED from './fingerprint-template.json'

/**
 * The fingerprint template: every Pixel build the picker may offer.
 *
 * There are two copies of this data and they are intentionally the same format.
 *
 * 1. `fingerprint-template.json` ships in the bundle. It is refreshed at build
 *    time by `scripts/fetch_fingerprint_template.py`, so a released zip never
 *    needs the network to open a picker.
 * 2. `TEMPLATE_PATH` on the device is the live copy. It is written by the
 *    picker's Fetch Latest button and by the daemon's daily refresh, both of
 *    which write atomically and keep the previous file if anything fails.
 *
 * The device copy wins when it is present and structurally sound. A truncated,
 * empty, or half-written file is ignored in favour of the bundled data, because
 * a stale list of builds is useful and an empty picker is not.
 */

export type TemplateRow = [releaseCandidateName: string, incremental: string, major: number]

export type Template = Record<string, { model: string; min: number; max: number; builds: TemplateRow[] }>

/** Live template, written by Fetch Latest and by the daemon's daily refresh. */
export const TEMPLATE_PATH = '/data/misc/keystore/omk/data/fingerprint-template.json'

/** Written first, then renamed over TEMPLATE_PATH, so the swap is atomic. */
const TEMPLATE_TEMP = `${TEMPLATE_PATH}.tmp`

/** Previous good copy, kept so a failed write is never the only thing on disk. */
export const TEMPLATE_BACKUP = `${TEMPLATE_PATH}.bak`

const BUNDLED_TEMPLATE = BUNDLED as unknown as Template

/** How many devices a template must cover before it is trusted at all. */
const MIN_DEVICES = 1

let current: Template = BUNDLED_TEMPLATE
let loaded = false

/**
 * Accept a parsed value only if it looks like a real template.
 *
 * Every device must have a name, bounds, and at least one build whose major is
 * inside those bounds. Anything less is treated as damage and rejected, which is
 * what keeps a half-written file from becoming the picker's entire world.
 */
export function isValidTemplate(value: unknown): value is Template {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false
  const entries = Object.entries(value as Record<string, unknown>)
  if (entries.length < MIN_DEVICES) return false
  return entries.every(([, entry]) => {
    if (typeof entry !== 'object' || entry === null) return false
    const record = entry as Record<string, unknown>
    const { model, min, max, builds } = record
    if (typeof model !== 'string' || !model) return false
    if (typeof min !== 'number' || typeof max !== 'number') return false
    if (!Array.isArray(builds) || builds.length === 0) return false
    return builds.every((row) => {
      if (!Array.isArray(row) || row.length !== 3) return false
      const [name, incremental, major] = row as TemplateRow
      return (
        typeof name === 'string' && name.length > 0 &&
        typeof incremental === 'string' && incremental.length > 0 &&
        typeof major === 'number' && major >= min && major <= max
      )
    })
  })
}

/**
 * Read the live template, falling back to the bundled one.
 *
 * Order matters: the live file, then its backup, then the bundle. The backup is
 * only consulted when the live file is missing or unusable, and a missing
 * bundle is impossible, so this always returns something renderable.
 */
export async function loadTemplate(): Promise<Template> {
  if (loaded) return current
  for (const path of [TEMPLATE_PATH, TEMPLATE_BACKUP]) {
    const parsed = await readTemplateFile(path)
    if (parsed) {
      current = parsed
      loaded = true
      return current
    }
  }
  current = BUNDLED_TEMPLATE
  loaded = true
  return current
}

/** The template currently in use, without touching the filesystem. */
export function activeTemplate(): Template {
  return current
}

/** Drop the in-memory copy so the next read re-considers the device file. */
export function invalidateTemplate(): void {
  loaded = false
}

async function readTemplateFile(path: string): Promise<Template | null> {
  if (!(await File.exist(path))) return null
  try {
    const parsed: unknown = JSON.parse(await File.read(path))
    return isValidTemplate(parsed) ? parsed : null
  } catch {
    return null
  }
}

/**
 * Replace the live template, safely.
 *
 * The new data is written to a temp file and only then renamed into place. If
 * either step fails the previous template is still there, and the backup is
 * refreshed from it first so there is always a known-good copy to fall back to.
 * Nothing is ever truncated in place.
 */
export async function saveTemplate(template: Template): Promise<void> {
  if (!isValidTemplate(template)) {
    throw new Error('refusing to save an incomplete fingerprint template')
  }
  const payload = `${JSON.stringify(template)}\n`
  await File.createDirectory(TEMPLATE_PATH.slice(0, TEMPLATE_PATH.lastIndexOf('/')))

  // Keep the outgoing good copy before touching anything.
  if (await File.exist(TEMPLATE_PATH)) {
    await File.copy(TEMPLATE_PATH, TEMPLATE_BACKUP).catch(() => undefined)
  }

  await File.write(TEMPLATE_TEMP, payload)
  // Reject a temp file that did not land intact before it replaces the good one.
  const written = await readTemplateFile(TEMPLATE_TEMP)
  if (!written || !isValidTemplate(written)) {
    await File.delete(TEMPLATE_TEMP).catch(() => undefined)
    throw new Error('fingerprint template did not verify after write')
  }
  await File.move(TEMPLATE_TEMP, TEMPLATE_PATH)
  current = written
  loaded = true
}

/**
 * Whether two templates differ in any way the picker would render.
 *
 * Used by the daily refresh to decide whether a fetch is worth committing. Order
 * is not significant, so a re-sorted but identical set counts as unchanged.
 */
export function templatesEquivalent(left: Template, right: Template): boolean {
  const leftProducts = Object.keys(left).sort()
  const rightProducts = Object.keys(right).sort()
  if (leftProducts.length !== rightProducts.length) return false
  if (leftProducts.some((product, index) => product !== rightProducts[index])) return false
  return leftProducts.every((product) => {
    const a = left[product]
    const b = right[product]
    if (a.model !== b.model || a.min !== b.min || a.max !== b.max) return false
    if (a.builds.length !== b.builds.length) return false
    const key = (row: TemplateRow): string => `${row[0]}|${row[1]}|${row[2]}`
    const aKeys = new Set(a.builds.map(key))
    return b.builds.every((row) => aKeys.has(key(row)))
  })
}
