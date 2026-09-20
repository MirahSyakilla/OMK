import { exec } from 'kernelsu-alt'
import { File } from './file'
import { GITHUB_REPO, KEYBOX_ALWAYSSTRONG_URL, MOD_ID } from './constant'

export type OmKRestartTarget = 'keymint' | 'injector' | 'all'

const RESTART_MARKERS: Record<OmKRestartTarget, string> = {
  keymint: '/data/adb/omk/restart.keymint',
  injector: '/data/adb/omk/restart.injector',
  all: '/data/adb/omk/restart.all',
}

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

  async fetchPifDeviceList(): Promise<Array<{ model: string; product: string }>> {
    const raw = await this.#fetchFirst([
      'https://fastly.jsdelivr.net/gh/KOWX712/PlayIntegrityFix@bot/device_list.json',
      'https://raw.githubusercontent.com/KOWX712/PlayIntegrityFix/bot/device_list.json',
      'https://cdn.jsdelivr.net/gh/KOWX712/PlayIntegrityFix@bot/device_list.json',
    ])
    const devices = JSON.parse(raw) as Array<{ model?: string; product?: string }>
    if (!Array.isArray(devices)) throw new Error('invalid device list')
    return devices.filter((device): device is { model: string; product: string } =>
      !!(device.model && device.product),
    )
  }

  async fetchPifProp(product: string): Promise<string> {
    const encoded = encodeURIComponent(product)
    const text = await this.#fetchFirst([
      `https://fastly.jsdelivr.net/gh/KOWX712/PlayIntegrityFix@bot/device_prop/${encoded}.prop`,
      `https://raw.githubusercontent.com/KOWX712/PlayIntegrityFix/bot/device_prop/${encoded}.prop`,
      `https://cdn.jsdelivr.net/gh/KOWX712/PlayIntegrityFix@bot/device_prop/${encoded}.prop`,
    ])
    if (!text.includes('FINGERPRINT=')) throw new Error('fingerprint missing')
    return text
  }

  async #fetchFirst(urls: string[]): Promise<string> {
    for (const url of urls) {
      try {
        const response = await fetch(url)
        if (response.ok) {
          const text = await response.text()
          if (text.trim()) return text
        }
      } catch {
        // try curl next, then remaining URLs
      }
      const quoted = url.replace(/'/g, `'\\''`)
      const result = await exec(
        `curl -fsSL --connect-timeout 10 --max-time 30 '${quoted}' 2>/dev/null || wget -q -T 20 -O - '${quoted}'`,
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
      'am force-stop com.google.android.gms >/dev/null 2>&1; am force-stop com.android.vending >/dev/null 2>&1; killall -9 com.google.android.gms.unstable >/dev/null 2>&1; true',
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
