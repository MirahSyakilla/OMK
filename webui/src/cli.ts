import { exec } from 'kernelsu-alt'
import { File } from './file'
import { GITHUB_REPO, KEYBOX_ALWAYSSTRONG_URL, MOD_ID } from './constant'

export class Cli {
  static #basePathPromise: Promise<string> | null = null

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
    const result = await exec(`am start -a android.intent.action.VIEW -d '${url}'`)
    if (result.errno !== 0) window.open(url, '_blank')
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

  getRepositoryUrl(): string {
    return `https://github.com/${GITHUB_REPO}`
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
