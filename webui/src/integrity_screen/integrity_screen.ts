import type { MdSwitch } from '@material/web/all'
import { Cli, type FlashBuild } from '../cli'
import { Config } from '../config'
import { PIXEL_DEVICES } from '../constant'
import { File } from '../file'
import { Snackbar } from '../snackbar/snackbar'
import './integrity_screen.scss'

const DATA_DIR = '/data/misc/keystore/omk/data'
const ADB_DIR = '/data/adb/omk'
const TOML_PATH = `${ADB_DIR}/integrity.toml`
const PROP_PATH = `${ADB_DIR}/integrity.prop`
const TOML_PATH_DATA = `${DATA_DIR}/integrity.toml`
const PROP_PATH_DATA = `${DATA_DIR}/integrity.prop`

const MIN_FONT_SIZE = 8
const MAX_FONT_SIZE = 24

interface IntegrityState {
  enabled: boolean
  spoof_build: boolean
  spoof_props: boolean
  spoof_vending_finger: boolean
  sync_trust_patch: boolean
  sync_device_ids: boolean
  unify_product_props: boolean
  soter_beta: boolean
}

const DEFAULTS: IntegrityState = {
  enabled: false,
  spoof_build: true,
  spoof_props: true,
  spoof_vending_finger: true,
  sync_trust_patch: true,
  sync_device_ids: true,
  unify_product_props: false,
  soter_beta: false,
}

function parseBool(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) return fallback
  const normalized = value.trim().toLowerCase()
  if (['1', 'true', 'yes', 'on'].includes(normalized)) return true
  if (['0', 'false', 'no', 'off'].includes(normalized)) return false
  return fallback
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

const BUILD_LETTER_MAJOR: Record<string, number> = {
  S: 12,
  T: 13,
  U: 14,
  A: 15,
  B: 16,
  C: 17,
}

function buildMajor(build: FlashBuild): number | null {
  const track = build.previewMetadata?.releaseTrackName ?? ''
  const fromTrack = track.match(/^Android (\d+)/)
  if (fromTrack) return Number.parseInt(fromTrack[1], 10)
  const letter = build.releaseCandidateName?.[0]?.toUpperCase() ?? ''
  return BUILD_LETTER_MAJOR[letter] ?? null
}

function buildCandidates(target: number): string[] {
  return shuffle(PIXEL_DEVICES.filter((device) => device.min <= target && target <= device.max)).map(
    (device) => device.product,
  )
}

function pickBuild(builds: FlashBuild[], target: number | null): FlashBuild | null {
  const usable = builds.filter((build) => build.target === `${build.product}-user`)
  const matching = target === null ? usable : usable.filter((build) => buildMajor(build) === target)
  if (matching.length === 0) return null
  return matching.reduce((best, build) => {
    const p1 = Number.parseInt(build.buildId, 10) || 0
    const p2 = Number.parseInt(best.buildId, 10) || 0
    return p1 > p2 ? build : best
  })
}

function securityPatch(buildId: string): string {
  const match = buildId.match(/^[A-Z0-9]+\.(\d{2})(\d{2})(\d{2})\./)
  if (!match) return ''
  return `20${match[1]}-${match[2]}-${match[3]}`
}

function buildProp(build: FlashBuild, product: string, model: string, major: number): string {
  const id = build.releaseCandidateName || build.buildId
  const patch = securityPatch(build.buildId)
  const lines = [
    `MANUFACTURER=Google`,
    `BRAND=google`,
    `PRODUCT=${product}`,
    `DEVICE=${product}`,
    `MODEL=${model}`,
    `RELEASE=${major}`,
    `ID=${id}`,
    `INCREMENTAL=${build.buildId}`,
    `TYPE=user`,
    `TAGS=release-keys`,
  ]
  if (patch) lines.push(`SECURITY_PATCH=${patch}`)
  return lines.join('\n')
}

export class IntegrityScreen {
  #cli: Cli
  #config: Config
  #snackbar: Snackbar
  #container: HTMLElement | null = null
  #fingerprint = ''
  #product = ''
  #canEnable = false
  #fontSize = 14
  #autoScroll = true

  constructor(cli: Cli, config: Config, snackbar: Snackbar) {
    this.#cli = cli
    this.#config = config
    this.#snackbar = snackbar
  }

  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="integrity-screen">
        <!-- Screen actions (PlayIntegrityFix chip set) -->
        <md-chip-set class="integrity-action-row">
          <md-assist-chip id="pif-fetch-chip" elevated label="Fetch">
            <md-icon slot="icon">download</md-icon>
          </md-assist-chip>
          <md-assist-chip id="pif-update-chip" elevated label="Update">
            <md-icon slot="icon">refresh</md-icon>
          </md-assist-chip>
        </md-chip-set>

        <div class="integrity-main-layout">
          <!-- Controls Pane -->
          <div class="integrity-controls-pane">
            <div class="switch-stack">
              <div class="switch-row" id="row-enabled" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Enable Master Spoof</div>
                  <div class="switch-row-sub">Activate Play Integrity property overrides</div>
                </div>
                <md-switch icons="true" id="pif-enabled"></md-switch>
              </div>

              <div class="switch-row" id="row-spoof-build" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Spoof Build</div>
                  <div class="switch-row-sub">Override android.os.Build fields</div>
                </div>
                <md-switch icons="true" id="pif-spoof-build" selected></md-switch>
              </div>

              <div class="switch-row" id="row-spoof-props" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Spoof Props</div>
                  <div class="switch-row-sub">Override system ro.* properties</div>
                </div>
                <md-switch icons="true" id="pif-spoof-props" selected></md-switch>
              </div>

              <div class="switch-row" id="row-spoof-vending" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">
                    Spoof Vending Fingerprint
                    <span class="inline-badge badge-primary">Play Store</span>
                  </div>
                  <div class="switch-row-sub">Provide fingerprint to com.android.vending</div>
                </div>
                <md-switch icons="true" id="pif-spoof-vending" selected></md-switch>
              </div>

              <div class="switch-row" id="row-sync-patch" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Sync Trust Patch</div>
                  <div class="switch-row-sub">Synchronize security patch date with KeyMint trust</div>
                </div>
                <md-switch icons="true" id="pif-sync-patch" selected></md-switch>
              </div>

              <div class="switch-row" id="row-sync-ids" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Sync Device IDs</div>
                  <div class="switch-row-sub">Apply brand, model, product to config.toml</div>
                </div>
                <md-switch icons="true" id="pif-sync-ids" selected></md-switch>
              </div>

              <div class="switch-row" id="row-unify-props" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Unify Product Props</div>
                  <div class="switch-row-sub">Apply resetprop -n across ro.product.*</div>
                </div>
                <md-switch icons="true" id="pif-unify-props"></md-switch>
              </div>

              <div class="switch-row" id="row-soter" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">
                    Tencent Soter
                    <span class="inline-badge badge-tertiary">Beta</span>
                  </div>
                  <div class="switch-row-sub">Enable WeChat/Tencent biometric key attestation spoof</div>
                </div>
                <md-switch icons="true" id="pif-soter"></md-switch>
              </div>
            </div>
          </div>

          <!-- Terminal Pane (matches PlayIntegrityFix inject design) -->
          <div class="integrity-terminal-pane">
            <div class="terminal-card">
              <div class="terminal-header">
                <div class="terminal-header-icon">
                  <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -960 960 960" class="terminal-svg-icon">
                    <path d="M160-160q-33 0-56.5-23.5T80-240v-480q0-33 23.5-56.5T160-800h640q33 0 56.5 23.5T880-720v480q0 33-23.5 56.5T800-160H160Zm0-80h640v-400H160v400Zm140-40-56-56 103-104-104-104 57-56 160 160-160 160Zm180 0v-80h240v80H480Z" />
                  </svg>
                </div>
                <md-text-button id="pif-term-clear" class="terminal-clear-btn">
                  clear
                  <svg slot="icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 -960 960 960" class="terminal-trash-icon">
                    <path d="M280-120q-33 0-56.5-23.5T200-200v-520h-40v-80h200v-40h240v40h200v80h-40v520q0 33-23.5 56.5T680-120H280Zm80-160h80v-360h-80v360Zm160 0h80v-360h-80v360Z" />
                  </svg>
                </md-text-button>
              </div>
              <div class="terminal-body" id="pif-term-body">
                <p class="output-line">[ready] Play Integrity console initialized.</p>
              </div>
            </div>
          </div>
        </div>
      </div>
    `

    this.#bindEvents()
    void this.load()
  }

  output(content: string, error = false): void {
    const body = this.#container?.querySelector<HTMLElement>('#pif-term-body')
    if (!body) return
    if (content === '') {
      body.appendChild(document.createElement('br'))
    } else {
      const p = document.createElement('p')
      p.className = error ? 'output-line error' : 'output-line'
      p.textContent = content
      body.appendChild(p)
    }
    if (this.#autoScroll) {
      body.scrollTop = body.scrollHeight
    }
  }

  clear(): void {
    const body = this.#container?.querySelector<HTMLElement>('#pif-term-body')
    if (body) {
      body.innerHTML = ''
    }
  }

  async load(): Promise<void> {
    const status = await this.#cli.detectIntegrityZygisk()
    this.#canEnable = status.provider !== null && status.conflict === null

    const state = await this.#readState()
    this.#setSwitch('pif-enabled', state.enabled && this.#canEnable)
    this.#setSwitch('pif-spoof-build', state.spoof_build)
    this.#setSwitch('pif-spoof-props', state.spoof_props)
    this.#setSwitch('pif-spoof-vending', state.spoof_vending_finger)
    this.#setSwitch('pif-sync-patch', state.sync_trust_patch)
    this.#setSwitch('pif-sync-ids', state.sync_device_ids)
    this.#setSwitch('pif-unify-props', state.unify_product_props)
    this.#setSwitch('pif-soter', state.soter_beta)

    const enabledSwitch = this.#container?.querySelector<MdSwitch>('#pif-enabled')
    if (enabledSwitch) enabledSwitch.disabled = !this.#canEnable
    const soterSwitch = this.#container?.querySelector<MdSwitch>('#pif-soter')
    if (soterSwitch) soterSwitch.disabled = status.provider === null

    this.#fingerprint = await this.#readFingerprint()
    this.output(`[ready] Play Integrity status: Zygisk=${status.provider ?? 'none'}`)
    if (this.#fingerprint) {
      this.output(`[+] Active fingerprint: ${this.#fingerprint}`)
    }
  }

  #bindEvents(): void {
    if (!this.#container) return

    // Action Chips
    this.#container.querySelector('#pif-fetch-chip')?.addEventListener('click', () => {
      void this.#fetchProp(false)
    })
    this.#container.querySelector('#pif-update-chip')?.addEventListener('click', () => {
      void this.#fetchProp(true)
    })

    // Clear Terminal
    this.#container.querySelector('#pif-term-clear')?.addEventListener('click', () => {
      this.clear()
    })

    // Terminal Zoom & Scroll Tracking
    const termBody = this.#container.querySelector<HTMLElement>('#pif-term-body')
    if (termBody) {
      let initialPinch: number | null = null
      let initialFontSize = this.#fontSize

      termBody.addEventListener(
        'touchstart',
        (e) => {
          if (e.touches.length === 2 && e.touches[0] && e.touches[1]) {
            const dx = e.touches[0].clientX - e.touches[1].clientX
            const dy = e.touches[0].clientY - e.touches[1].clientY
            initialPinch = Math.hypot(dx, dy)
            initialFontSize = this.#fontSize
          }
        },
        { passive: true },
      )

      termBody.addEventListener(
        'touchmove',
        (e) => {
          if (initialPinch && e.touches.length === 2 && e.touches[0] && e.touches[1]) {
            const dx = e.touches[0].clientX - e.touches[1].clientX
            const dy = e.touches[0].clientY - e.touches[1].clientY
            const dist = Math.hypot(dx, dy)
            const scale = dist / initialPinch
            this.#fontSize = Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, Math.round(initialFontSize * scale)))
            termBody.style.fontSize = `${this.#fontSize}px`
          }
        },
        { passive: true },
      )

      termBody.addEventListener(
        'touchend',
        () => {
          initialPinch = null
        },
        { passive: true },
      )

      termBody.addEventListener(
        'wheel',
        (e) => {
          if (e.ctrlKey || e.metaKey) {
            e.preventDefault()
            const delta = e.deltaY < 0 ? 1 : -1
            this.#fontSize = Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, this.#fontSize + delta))
            termBody.style.fontSize = `${this.#fontSize}px`
          }
        },
        { passive: false },
      )

      termBody.addEventListener('scroll', () => {
        const atBottom = termBody.scrollHeight - termBody.scrollTop - termBody.clientHeight <= 25
        this.#autoScroll = atBottom
      })
    }

    // Switch Stack Row Binds & Auto-Save
    const bindRow = (rowId: string, switchId: string) => {
      const row = this.#container?.querySelector<HTMLElement>(`#${rowId}`)
      const sw = this.#container?.querySelector<MdSwitch>(`#${switchId}`)
      if (row && sw) {
        row.addEventListener('click', (e) => {
          if (e.target === sw || (e.target instanceof Node && sw.contains(e.target))) return
          if (!sw.disabled) {
            sw.selected = !sw.selected
            sw.dispatchEvent(new Event('change'))
          }
        })
        sw.addEventListener('change', () => {
          void this.#handleToggle(switchId, sw.selected)
        })
      }
    }

    bindRow('row-enabled', 'pif-enabled')
    bindRow('row-spoof-build', 'pif-spoof-build')
    bindRow('row-spoof-props', 'pif-spoof-props')
    bindRow('row-spoof-vending', 'pif-spoof-vending')
    bindRow('row-sync-patch', 'pif-sync-patch')
    bindRow('row-sync-ids', 'pif-sync-ids')
    bindRow('row-unify-props', 'pif-unify-props')
    bindRow('row-soter', 'pif-soter')
  }

  async #handleToggle(switchId: string, value: boolean): Promise<void> {
    const zygisk = await this.#cli.detectIntegrityZygisk()
    if (switchId === 'pif-soter' && value && zygisk.provider === null) {
      this.#snackbar.show('Zygisk required for Tencent Soter', false)
      this.output('[!] Tencent Soter requires Zygisk', true)
      this.#setSwitch('pif-soter', false)
      return
    }
    if (switchId === 'pif-enabled' && value && !this.#canEnable) {
      this.#snackbar.show('Zygisk required to enable Integrity', false)
      this.output('[!] Zygisk is required to enable Play Integrity', true)
      this.#setSwitch('pif-enabled', false)
      return
    }

    const state: IntegrityState = {
      enabled: this.#getSwitch('pif-enabled'),
      spoof_build: this.#getSwitch('pif-spoof-build'),
      spoof_props: this.#getSwitch('pif-spoof-props'),
      spoof_vending_finger: this.#getSwitch('pif-spoof-vending'),
      sync_trust_patch: this.#getSwitch('pif-sync-patch'),
      sync_device_ids: this.#getSwitch('pif-sync-ids'),
      unify_product_props: this.#getSwitch('pif-unify-props'),
      soter_beta: this.#getSwitch('pif-soter'),
    }

    try {
      await File.createDirectory(DATA_DIR)
      await File.createDirectory(ADB_DIR)

      const toml = [
        `enabled = ${state.enabled}`,
        `spoof_build = ${state.spoof_build}`,
        `spoof_props = ${state.spoof_props}`,
        `spoof_vending_finger = ${state.spoof_vending_finger}`,
        `sync_trust_patch = ${state.sync_trust_patch}`,
        `sync_device_ids = ${state.sync_device_ids}`,
        `unify_product_props = ${state.unify_product_props}`,
        `soter_beta = ${state.soter_beta}`,
      ].join('\n')

      await File.write(TOML_PATH, toml)
      await File.write(TOML_PATH_DATA, toml)

      const propPath = (await File.exist(PROP_PATH)) ? PROP_PATH : PROP_PATH_DATA
      const prop = parseKv((await File.exist(propPath)) ? await File.read(propPath) : '')

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

      await this.#cli.killIntegrityTargets()
      const keyName = switchId.replace('pif-', '').replace(/-/g, '_')
      this.output(`[+] ${keyName}: ${value ? 'enabled' : 'disabled'}`)
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.output(`[!] Error saving setting: ${msg}`, true)
    }
  }

  #setSwitch(id: string, selected: boolean): void {
    const sw = this.#container?.querySelector<MdSwitch>(`#${id}`)
    if (sw) sw.selected = selected
  }

  #getSwitch(id: string): boolean {
    return this.#container?.querySelector<MdSwitch>(`#${id}`)?.selected === true
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
        soter_beta: parseBool(map.soter_beta, false),
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
      this.#product = map.PRODUCT || (map.FINGERPRINT ? map.FINGERPRINT.split(/[/:]/)[1] ?? '' : '')
      return map.FINGERPRINT ?? ''
    } catch {
      return ''
    }
  }


  async #fetchProp(update: boolean): Promise<void> {
    try {
      this.output(update ? '[...] Updating current product fingerprint...' : '[...] Fetching compatible Flashstation build...')
      let product = this.#product || (this.#fingerprint ? this.#fingerprint.split(/[/:]/)[1] ?? '' : '')
      if (update && !product) {
        this.#snackbar.show('Fetch a fingerprint first', false)
        this.output('[!] Cannot update: no product selected yet.', true)
        return
      }

      const romMajor = androidMajor(await this.#cli.getBuildRelease())
      const target = romMajor ? Number.parseInt(romMajor, 10) : null
      const matchesRom = (content: string): boolean => {
        if (target === null) return true
        const parsed = propAndroidMajor(content)
        return parsed !== null && Number.parseInt(parsed, 10) === target
      }

      const candidates = product ? [product] : buildCandidates(target ?? 14)
      const matches: Array<{ build: FlashBuild; product: string; model: string; major: number; prop: string }> = []

      for (const p of candidates) {
        const builds = await this.#cli.fetchFlashstationBuilds(p)
        const picked = pickBuild(builds, target)
        if (!picked) continue
        const major = buildMajor(picked)
        if (major === null) continue
        const device = PIXEL_DEVICES.find((entry) => entry.product === p)
        const prop = buildProp(picked, p, device?.model ?? p, major)
        if (matchesRom(prop)) {
          matches.push({ build: picked, product: p, model: device?.model ?? p, major, prop })
        }
        if (matches.length >= 8) break
      }

      if (matches.length === 0) {
        this.#snackbar.show('No compatible build found for this Android release', false)
        this.output('[!] No compatible build found for this Android release', true)
        return
      }

      if (update) {
        const first = matches[0]
        if (!first) return
        await File.createDirectory(DATA_DIR)
        await File.createDirectory(ADB_DIR)
        await File.write(PROP_PATH, first.prop)
        await File.write(PROP_PATH_DATA, first.prop)
        await this.#cli.killIntegrityTargets()
        this.output(`[+] Updated product: ${first.product} (${first.build.releaseCandidateName || first.build.buildId})`)
        this.output('[+] Saved integrity.prop')
        this.output('[+] Stopped GMS, Vending, and unstable DroidGuard targets')
        this.#snackbar.show('Product updated')
        this.#fingerprint = await this.#readFingerprint()
        return
      }

      const dialog = document.createElement('md-dialog')
      dialog.innerHTML = /* html */ `
        <div slot="headline">Select Device Fingerprint</div>
        <form slot="content" id="build-form" method="dialog" style="display: flex; flex-direction: column; gap: 8px;">
          ${matches
            .map(
              (m, idx) => `
            <label style="display: flex; align-items: center; gap: 12px; padding: 10px; border-radius: 12px; cursor: pointer; background: var(--md-sys-color-surface-container-high);">
              <md-radio name="build-choice" value="${idx}" ${idx === 0 ? 'checked' : ''}></md-radio>
              <div style="display: flex; flex-direction: column;">
                <span style="font-weight: 600; color: var(--md-sys-color-on-surface);">${m.model} (${m.product})</span>
                <span style="font-size: 0.8125rem; color: var(--md-sys-color-on-surface-variant);">${m.build.releaseCandidateName || m.build.buildId} • Android ${m.major}</span>
              </div>
            </label>
          `,
            )
            .join('')}
        </form>
        <div slot="actions">
          <md-text-button id="dialog-cancel">Cancel</md-text-button>
          <md-filled-button id="dialog-apply">Apply</md-filled-button>
        </div>
      `
      document.body.appendChild(dialog)
      dialog.show()

      dialog.querySelector('#dialog-cancel')?.addEventListener('click', () => {
        dialog.close()
        dialog.remove()
      })

      dialog.querySelector('#dialog-apply')?.addEventListener('click', async () => {
        const form = dialog.querySelector<HTMLFormElement>('#build-form')
        const choice = (form?.elements.namedItem('build-choice') as RadioNodeList | null)?.value
        const chosen = matches[Number.parseInt(choice ?? '0', 10)]
        dialog.close()
        dialog.remove()

        if (chosen) {
          await File.createDirectory(DATA_DIR)
          await File.createDirectory(ADB_DIR)
          await File.write(PROP_PATH, chosen.prop)
          await File.write(PROP_PATH_DATA, chosen.prop)
          await this.#cli.killIntegrityTargets()
          this.output(`[+] Fetched build: ${chosen.build.releaseCandidateName || chosen.build.buildId}`)
          this.output('[+] Saved integrity.prop')
          this.output('[+] Stopped GMS, Vending, and unstable DroidGuard targets')
          this.#snackbar.show('Fingerprint applied')
          this.#fingerprint = await this.#readFingerprint()
        }
      })
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.#snackbar.show('Failed to fetch fingerprint', false)
      this.output(`[!] Fetch exception: ${msg}`, true)
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
