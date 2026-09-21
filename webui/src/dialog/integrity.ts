import type { MdDialog, MdFilledButton, MdOutlinedButton, MdSwitch } from '@material/web/all'
import { Cli } from '../cli'
import type { FlashBuild } from '../cli'
import { Config } from '../config'
import { PIXEL_DEVICES } from '../constant'
import { File } from '../file'
import { Snackbar } from '../snackbar/snackbar'
import { applyDialogAnimation } from './animation'

const DATA_DIR = '/data/misc/keystore/omk/data'
const ADB_DIR = '/data/adb/omk'
const TOML_PATH = `${ADB_DIR}/integrity.toml`
const PROP_PATH = `${ADB_DIR}/integrity.prop`
const TOML_PATH_DATA = `${DATA_DIR}/integrity.toml`
const PROP_PATH_DATA = `${DATA_DIR}/integrity.prop`

interface IntegrityState {
  enabled: boolean
  spoof_build: boolean
  spoof_props: boolean
  spoof_vending_finger: boolean
  sync_trust_patch: boolean
  sync_device_ids: boolean
  unify_product_props: boolean
}

const DEFAULTS: IntegrityState = {
  enabled: false,
  spoof_build: true,
  spoof_props: true,
  spoof_vending_finger: true,
  sync_trust_patch: true,
  sync_device_ids: true,
  unify_product_props: false,
}

function parseBool(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) return fallback
  const normalized = value.trim().toLowerCase()
  if (['1', 'true', 'yes', 'on'].includes(normalized)) return true
  if (['0', 'false', 'no', 'off'].includes(normalized)) return false
  return fallback
}

function androidMajor(value: string): string | null {
  const match = value.trim().match(/^(\d+)/)
  return match ? match[1] : null
}

function propAndroidMajor(content: string): string | null {
  const map = parseKv(content)
  const fromRelease = androidMajor(map.RELEASE ?? '')
  if (fromRelease) return fromRelease
  const fingerprint = map.FINGERPRINT ?? ''
  return androidMajor(fingerprint.split(/[/:]/)[3] ?? '')
}

function shuffle<T>(items: T[]): T[] {
  const next = [...items]
  for (let i = next.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1))
    const current = next[i]
    next[i] = next[j]
    next[j] = current
  }
  return next
}

/// AOSP build-ID initial maps to the Android major release.
const BUILD_LETTER_MAJOR: Record<string, number> = {
  S: 12,
  T: 13,
  U: 14,
  A: 15,
  B: 16,
  C: 17,
}

export function buildMajor(build: FlashBuild): number | null {
  const track = build.previewMetadata?.releaseTrackName ?? ''
  const fromTrack = track.match(/^Android (\d+)/)
  if (fromTrack) return Number.parseInt(fromTrack[1], 10)
  const letter = build.releaseCandidateName?.[0]?.toUpperCase() ?? ''
  return BUILD_LETTER_MAJOR[letter] ?? null
}

export function buildCandidates(target: number): string[] {
  return shuffle(PIXEL_DEVICES.filter((device) => device.min <= target && target <= device.max))
    .map((device) => device.product)
}

export function pickBuild(builds: FlashBuild[], target: number | null): FlashBuild | null {
  const usable = builds.filter((build) => build.target === `${build.product}-user`)
  const matching = target === null
    ? usable
    : usable.filter((build) => buildMajor(build) === target)
  if (matching.length === 0) return null
  return matching.reduce((best, build) => (buildTotal(build) > buildTotal(best) ? build : best))
}

function buildTotal(build: FlashBuild): number {
  const parsed = Number.parseInt(build.buildId, 10)
  return Number.isFinite(parsed) ? parsed : 0
}

/// Pixel build IDs carry the patch date as `YYMMDD`.
export function securityPatch(buildId: string): string {
  const match = buildId.match(/^[A-Z0-9]+\.(\d{2})(\d{2})(\d{2})\./)
  if (!match) return ''
  return `20${match[1]}-${match[2]}-${match[3]}`
}

export function buildProp(build: FlashBuild, product: string, model: string, major: number): string {
  const id = build.releaseCandidateName
  const patch = securityPatch(id)
  const lines = [
    `FINGERPRINT=google/${product}/${product}:${major}/${id}/${build.buildId}:user/release-keys`,
    'MANUFACTURER=Google',
    `MODEL=${model}`,
    `PRODUCT=${product}`,
    `DEVICE=${product}`,
    'BRAND=google',
    `RELEASE=${major}`,
    `ID=${id}`,
    `INCREMENTAL=${build.buildId}`,
    'TYPE=user',
    'TAGS=release-keys',
  ]
  if (patch) lines.push(`SECURITY_PATCH=${patch}`)
  return lines.join('\n')
}

function parseKv(content: string): Record<string, string> {
  const map: Record<string, string> = {}
  for (const raw of content.split('\n')) {
    const line = raw.split('#')[0]?.trim() ?? ''
    if (!line) continue
    const eq = line.indexOf('=')
    if (eq <= 0) continue
    map[line.slice(0, eq).trim()] = line.slice(eq + 1).trim()
  }
  return map
}

export class IntegrityDialog {
  #dialog: MdDialog | null = null
  #cli: Cli
  #config: Config
  #snackbar: Snackbar
  #pendingProp: string | null = null
  #fingerprint = ''
  #product = ''
  #canEnable = false
  #onSaved: (() => void) | null = null

  constructor(cli: Cli, config: Config, snackbar: Snackbar, onSaved?: () => void) {
    this.#cli = cli
    this.#config = config
    this.#snackbar = snackbar
    this.#onSaved = onSaved ?? null
  }

  getElement(): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="integrity-settings-dialog">
        <div slot="headline">Integrity Settings</div>
        <div slot="content">
          <div id="integrity-status" class="integrity-status-pill"></div>
          <div class="policy-fields">
            <label class="switch-item outlined" for="integrity-enabled">
              <md-ripple></md-ripple>
              <span>Enable</span>
              <md-switch id="integrity-enabled"></md-switch>
            </label>
            <label class="switch-item outlined" for="integrity-spoof-build">
              <md-ripple></md-ripple>
              <span>Spoof Build</span>
              <md-switch id="integrity-spoof-build" selected></md-switch>
            </label>
            <label class="switch-item outlined" for="integrity-spoof-props">
              <md-ripple></md-ripple>
              <span>Spoof Props</span>
              <md-switch id="integrity-spoof-props" selected></md-switch>
            </label>
            <label class="switch-item outlined" for="integrity-spoof-vending">
              <md-ripple></md-ripple>
              <span>Spoof Vending Fingerprint</span>
              <md-switch id="integrity-spoof-vending" selected></md-switch>
            </label>
            <label class="switch-item outlined" for="integrity-sync-patch">
              <md-ripple></md-ripple>
              <span>Sync Trust Patch</span>
              <md-switch id="integrity-sync-patch" selected></md-switch>
            </label>
            <label class="switch-item outlined" for="integrity-sync-ids">
              <md-ripple></md-ripple>
              <span>Sync Device IDs</span>
              <md-switch id="integrity-sync-ids" selected></md-switch>
            </label>
            <label class="switch-item outlined" for="integrity-unify-props">
              <md-ripple></md-ripple>
              <span>Unify Product Props</span>
              <md-switch id="integrity-unify-props"></md-switch>
            </label>
          </div>
          <p id="integrity-fingerprint" class="integrity-fingerprint">No fingerprint fetched</p>
          <div class="integrity-fp-actions">
            <md-outlined-button id="integrity-fetch">Fetch</md-outlined-button>
            <md-outlined-button id="integrity-update">Update</md-outlined-button>
          </div>
        </div>
        <div slot="actions">
          <md-outlined-button id="integrity-close">Cancel</md-outlined-button>
          <md-filled-button id="integrity-save">Save</md-filled-button>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#dialog = fragment.querySelector<MdDialog>('#integrity-settings-dialog')
    fragment.querySelector<MdOutlinedButton>('#integrity-close')!.onclick = () => this.close()
    fragment.querySelector<MdFilledButton>('#integrity-save')!.onclick = () => {
      void this.#save()
    }
    fragment.querySelector<MdOutlinedButton>('#integrity-fetch')!.onclick = () => {
      void this.#fetchProp(false)
    }
    fragment.querySelector<MdOutlinedButton>('#integrity-update')!.onclick = () => {
      void this.#fetchProp(true)
    }
    return fragment
  }

  initAnimation(): void {
    if (this.#dialog) applyDialogAnimation(this.#dialog)
  }

  async show(): Promise<void> {
    this.#pendingProp = null
    const status = await this.#cli.detectIntegrityZygisk()
    this.#canEnable = status.provider !== null && status.conflict === null
    const statusEl = this.#dialog?.querySelector<HTMLElement>('#integrity-status')
    if (statusEl) {
      statusEl.textContent = this.#statusText(status)
      statusEl.classList.toggle('error', !this.#canEnable)
    }

    const state = await this.#readState()
    this.#setSwitch('integrity-enabled', state.enabled && this.#canEnable)
    this.#setSwitch('integrity-spoof-build', state.spoof_build)
    this.#setSwitch('integrity-spoof-props', state.spoof_props)
    this.#setSwitch('integrity-spoof-vending', state.spoof_vending_finger)
    this.#setSwitch('integrity-sync-patch', state.sync_trust_patch)
    this.#setSwitch('integrity-sync-ids', state.sync_device_ids)
    this.#setSwitch('integrity-unify-props', state.unify_product_props)

    const enable = this.#dialog?.querySelector<MdSwitch>('#integrity-enabled')
    if (enable) enable.disabled = !this.#canEnable

    this.#fingerprint = await this.#readFingerprint()
    this.#renderFingerprint()
    this.#dialog?.show()
  }

  close(): void {
    this.#dialog?.close()
  }

  #statusText(status: { provider: string | null; conflict: string | null }): string {
    if (status.conflict) {
      return `Disabled: ${status.conflict} is loaded. Remove it before enabling OMK Integrity.`
    }
    if (!status.provider) {
      return 'Disabled: Zygisk not found. Install ReZygisk (preferred), ZygiskNext, NeoZygisk, or Magisk Zygisk.'
    }
    const label =
      status.provider === 'rezygisk' ? 'ReZygisk'
        : status.provider === 'zygisk_next' ? 'ZygiskNext'
          : status.provider === 'neozygisk' ? 'NeoZygisk'
            : 'Magisk Zygisk'
    return `Zygisk: ${label}`
  }

  #setSwitch(id: string, selected: boolean): void {
    const field = this.#dialog?.querySelector<MdSwitch>(`#${id}`)
    if (field) field.selected = selected
  }

  #getSwitch(id: string): boolean {
    return this.#dialog?.querySelector<MdSwitch>(`#${id}`)?.selected === true
  }

  #renderFingerprint(): void {
    const el = this.#dialog?.querySelector<HTMLElement>('#integrity-fingerprint')
    if (!el) return
    el.textContent = this.#fingerprint || 'No fingerprint fetched'
  }

  async #readState(): Promise<IntegrityState> {
    const tomlPath = (await File.exist(TOML_PATH)) ? TOML_PATH : TOML_PATH_DATA
    if (!(await File.exist(tomlPath))) return { ...DEFAULTS }
    try {
      const map = parseKv(await File.read(tomlPath))
      return {
        enabled: parseBool(map.enabled, false),
        spoof_build: parseBool(map.spoof_build, true),
        spoof_props: parseBool(map.spoof_props, true),
        spoof_vending_finger: parseBool(map.spoof_vending_finger, true),
        sync_trust_patch: parseBool(map.sync_trust_patch, true),
        sync_device_ids: parseBool(map.sync_device_ids, true),
        unify_product_props: parseBool(map.unify_product_props, false),
      }
    } catch {
      return { ...DEFAULTS }
    }
  }

  async #readFingerprint(): Promise<string> {
    const propPath = (await File.exist(PROP_PATH)) ? PROP_PATH : PROP_PATH_DATA
    if (!(await File.exist(propPath))) return ''
    try {
      const map = parseKv(await File.read(propPath))
      this.#product = map.PRODUCT || this.#productFromFingerprint(map.FINGERPRINT ?? '')
      return map.FINGERPRINT ?? ''
    } catch {
      return ''
    }
  }

  #productFromFingerprint(fingerprint: string): string {
    return fingerprint.split(/[/:]/)[1] ?? ''
  }

  async #fetchProp(update: boolean): Promise<void> {
    try {
      let product = this.#product || this.#productFromFingerprint(this.#fingerprint)
      if (update && !product) {
        this.#snackbar.show('Fetch a fingerprint first', false)
        return
      }
      const romMajor = androidMajor(await this.#cli.getBuildRelease())
      const target = romMajor ? Number.parseInt(romMajor, 10) : null
      const matchesRom = (content: string): boolean => {
        if (!romMajor) return true
        return propAndroidMajor(content) === romMajor
      }

      const candidates = target === null
        ? shuffle(PIXEL_DEVICES).map((device) => device.product)
        : buildCandidates(target)
      const ordered = product && candidates.includes(product)
        ? [product, ...candidates.filter((entry) => entry !== product)]
        : candidates

      for (const candidate of ordered) {
        try {
          const content = await this.#fetchBuildProp(candidate, target)
          if (!content || !matchesRom(content)) continue
          product = candidate
          this.#pendingProp = content
          break
        } catch {
          continue
        }
      }

      if (!this.#pendingProp) {
        this.#snackbar.show(
          romMajor ? `No fingerprint matching Android ${romMajor}` : 'Failed to fetch fingerprint',
          false,
        )
        return
      }
      const fingerprint = parseKv(this.#pendingProp).FINGERPRINT
      if (!fingerprint) {
        this.#snackbar.show('Fetched prop needs FINGERPRINT=', false)
        return
      }
      this.#fingerprint = fingerprint
      this.#product = parseKv(this.#pendingProp).PRODUCT || product
      this.#renderFingerprint()
      this.#snackbar.show(update ? 'Fingerprint updated' : 'Fingerprint fetched')
    } catch {
      this.#snackbar.show(update ? 'Failed to update fingerprint' : 'Failed to fetch fingerprint', false)
    }
  }

  async #fetchBuildProp(product: string, target: number | null): Promise<string> {
    const builds = await this.#cli.fetchFlashstationBuilds(product)
    const picked = pickBuild(builds, target)
    if (!picked) return ''
    const major = buildMajor(picked)
    if (major === null) return ''
    const device = PIXEL_DEVICES.find((entry) => entry.product === product)
    return buildProp(picked, product, device?.model ?? product, major)
  }

  async #save(): Promise<void> {
    const enabled = this.#getSwitch('integrity-enabled')
    if (enabled && !this.#canEnable) {
      this.#snackbar.show('Zygisk required to enable Integrity', false)
      return
    }
    if (enabled && !this.#fingerprint) {
      this.#snackbar.show('Fetch a fingerprint before enabling', false)
      return
    }

    const state: IntegrityState = {
      enabled,
      spoof_build: this.#getSwitch('integrity-spoof-build'),
      spoof_props: this.#getSwitch('integrity-spoof-props'),
      spoof_vending_finger: this.#getSwitch('integrity-spoof-vending'),
      sync_trust_patch: this.#getSwitch('integrity-sync-patch'),
      sync_device_ids: this.#getSwitch('integrity-sync-ids'),
      unify_product_props: this.#getSwitch('integrity-unify-props'),
    }

    try {
      await File.createDirectory(DATA_DIR)
      await File.createDirectory(ADB_DIR)
      if (this.#pendingProp) {
        await File.write(PROP_PATH, this.#pendingProp)
        await File.write(PROP_PATH_DATA, this.#pendingProp)
      }
      const toml = [
        `enabled = ${state.enabled}`,
        `spoof_build = ${state.spoof_build}`,
        `spoof_props = ${state.spoof_props}`,
        `spoof_vending_finger = ${state.spoof_vending_finger}`,
        `sync_trust_patch = ${state.sync_trust_patch}`,
        `sync_device_ids = ${state.sync_device_ids}`,
        `unify_product_props = ${state.unify_product_props}`,
      ].join('\n')
      await File.write(TOML_PATH, toml)
      await File.write(TOML_PATH_DATA, toml)

      const propPath = (await File.exist(PROP_PATH)) ? PROP_PATH : PROP_PATH_DATA
      const prop = parseKv(this.#pendingProp ?? ((await File.exist(propPath)) ? await File.read(propPath) : ''))
      if (state.sync_trust_patch && prop.SECURITY_PATCH) {
        this.#config.set('trust', 'security_patch', prop.SECURITY_PATCH)
      }
      if (state.sync_device_ids) {
        this.#syncDevice(prop)
      }
      if (state.sync_trust_patch || state.sync_device_ids) {
        await this.#config.write()
      }
      if (state.unify_product_props) {
        await this.#cli.unifyProductProps(prop)
      }
      await this.#cli.requestRestart('all')
      await this.#cli.killIntegrityTargets()
      this.#snackbar.show('Integrity settings saved')
      this.#onSaved?.()
      this.close()
    } catch {
      this.#snackbar.show('Failed to save Integrity settings', false)
    }
  }

  #syncDevice(prop: Record<string, string>): void {
    const fingerprint = prop.FINGERPRINT ?? ''
    const parts = fingerprint.split(/[/:]/)
    const brand = prop.BRAND || parts[0] || ''
    const product = prop.PRODUCT || parts[1] || ''
    const device = prop.DEVICE || parts[2] || ''
    const manufacturer = prop.MANUFACTURER || brand
    const model = prop.MODEL || ''
    if (brand) this.#config.set('device', 'brand', brand)
    if (device) this.#config.set('device', 'device', device)
    if (product) this.#config.set('device', 'product', product)
    if (manufacturer) this.#config.set('device', 'manufacturer', manufacturer)
    if (model) this.#config.set('device', 'model', model)
  }
}
