import '@material/web/progress/circular-progress.js'
import type { MdOutlinedSelect, MdSwitch, MdTextButton } from '@material/web/all'
import { Cli, type FlashBuild } from '../cli'
import { Config } from '../config'
import { PIXEL_DEVICES } from '../constant'
import { File } from '../file'
import { escapeHtml } from '../html'
import { buildProp } from '../integrity_prop'
import type { TemplateRow } from '../fingerprint_template'
import {
  activeTemplate,
  isValidTemplate,
  loadTemplate,
  saveTemplate,
  templatesEquivalent,
  type Template,
} from '../fingerprint_template'
import { boundOptionLists } from '../dialog/option_list'
import { Snackbar } from '../snackbar/snackbar'
import './integrity_screen.scss'

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
  soter_beta: boolean
  auto_fetch_fingerprint: boolean
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
  auto_fetch_fingerprint: false,
}

const DEPENDENT_ROW_IDS = [
  'row-spoof-build',
  'row-spoof-props',
  'row-spoof-vending',
  'row-sync-patch',
  'row-sync-ids',
  'row-unify-props',
] as const

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

const BUILD_LETTER_MAJOR: Record<string, number> = {
  S: 12,
  T: 13,
  U: 14,
  A: 15,
  B: 16,
  C: 17,
}

/// Leading integer run of a version string.
///
/// "16" and "16.0" both give 16. A trailing-digit match would read the "0" in
/// "16.0" as the entire version and map the build to Android 0.
function leadingNumber(value: string): number | null {
  const match = value.trim().match(/^\d+/)
  return match ? Number.parseInt(match[0], 10) : null
}

function buildMajor(build: FlashBuild): number | null {
  const named = build.versionName ? leadingNumber(build.versionName) : null
  if (named !== null) return named
  const track = build.previewMetadata?.releaseTrackName ?? ''
  const fromTrack = track.match(/^Android (\d+)/)
  if (fromTrack) return Number.parseInt(fromTrack[1], 10)
  if (typeof build.apiLevel === 'number' && build.apiLevel > 0) return build.apiLevel - 20
  const letter = build.releaseCandidateName?.[0]?.toUpperCase() ?? ''
  return BUILD_LETTER_MAJOR[letter] ?? null
}

/// How many build lists may be fetched at once.
const FETCH_CONCURRENCY = 4

/// Map `items` through an async worker, keeping at most `limit` in flight.
///
/// A rejection is captured as a rejection rather than aborting the run, so one
/// product that cannot be fetched does not lose the builds for every other
/// product.
async function mapWithConcurrency<T, R>(
  items: readonly T[],
  limit: number,
  worker: (item: T) => Promise<R>,
): Promise<PromiseSettledResult<R>[]> {
  const results = new Array<PromiseSettledResult<R>>(items.length)
  let next = 0
  const runners = Array.from({ length: Math.min(limit, items.length) }, async () => {
    for (;;) {
      const index = next++
      if (index >= items.length) return
      try {
        results[index] = {
          status: 'fulfilled',
          value: await worker(items[index] as T),
        }
      } catch (reason) {
        results[index] = { status: 'rejected', reason }
      }
    }
  })
  await Promise.all(runners)
  return results
}

function latestFirst(left: FingerprintChoice, right: FingerprintChoice): number {
  const leftLatest = left.build.releaseBuildMetadata?.latest ? 1 : 0
  const rightLatest = right.build.releaseBuildMetadata?.latest ? 1 : 0
  if (leftLatest !== rightLatest) return rightLatest - leftLatest
  const l = Number.parseInt(left.build.buildId, 10) || 0
  const r = Number.parseInt(right.build.buildId, 10) || 0
  return r - l
}

interface FingerprintChoice {
  build: FlashBuild
  product: string
  model: string
  major: number
  initialSdk: number
  prop: string
}

export class IntegrityScreen {
  #cli: Cli
  #config: Config
  #snackbar: Snackbar
  #container: HTMLElement | null = null
  #canEnable = false
  #hasZygisk = false
  #saveTimer: number | null = null
  #isPersisting = false
  #hasPendingPersist = false
  #zygiskInfo = {
    provider: null as string | null,
    conflict: null as string | null,
    description: '',
  }
  #cachedProp: Record<string, string> = {}
  constructor(cli: Cli, config: Config, snackbar: Snackbar) {
    this.#cli = cli
    this.#config = config
    this.#snackbar = snackbar
  }

  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="integrity-screen">
        <!-- Screen actions -->
        <!-- Zygisk status & warning banner -->
        <div id="pif-zygisk-status" class="integrity-status-banner">
          <md-icon class="integrity-status-icon">check_circle</md-icon>
          <span class="integrity-status-text">Detecting Zygisk...</span>
        </div>
          <!-- Controls Pane -->
          <div class="integrity-controls-pane">
            <div class="switch-stack">
              <div class="switch-row" id="row-enabled" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Play Integrity</div>
                  <div class="switch-row-sub">Master switch for property overrides & spoofing</div>
                </div>
                <md-switch icons="true" id="pif-enabled" aria-label="Play Integrity"></md-switch>
              </div>

              <!--
                Shaped like the rows around it, but with no trailing switch.
                It is an action, not a setting, so a toggle that cannot stay put
                would misrepresent it, and the empty right-hand space is what
                keeps the stack reading as one list.
              -->
              <div class="switch-row integrity-picker-row" id="row-select-fp" role="button" tabindex="0" aria-label="Select Fingerprint">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Select Fingerprint</div>
                  <div class="switch-row-sub">Click to select your integrity fingerprint</div>
                </div>
                <md-icon class="integrity-picker-icon">chevron_right</md-icon>
              </div>

              <!--
                Sits directly under Select Fingerprint because it is a setting
                about that picker. The daemon re-reads this flag on its own
                schedule, so turning it on does not restart the WebUI or wait for
                a reboot to take effect.
              -->
              <div class="switch-row" id="row-auto-fetch-fp" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Auto Fetch Fingerprint</div>
                  <div class="switch-row-sub">Check Google daily and update the build list in the background</div>
                </div>
                <md-switch icons="true" id="pif-auto-fetch-fp" aria-label="Auto Fetch Fingerprint"></md-switch>
              </div>

              <div class="switch-row" id="row-spoof-build" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Spoof Build</div>
                  <div class="switch-row-sub">Override android.os.Build fields</div>
                </div>
                <md-switch icons="true" id="pif-spoof-build" aria-label="Spoof Build" selected></md-switch>
              </div>

              <div class="switch-row" id="row-spoof-props" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Spoof Props</div>
                  <div class="switch-row-sub">Override system ro.* properties</div>
                </div>
                <md-switch icons="true" id="pif-spoof-props" aria-label="Spoof Props" selected></md-switch>
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
                <md-switch icons="true" id="pif-spoof-vending" aria-label="Spoof Vending Fingerprint" selected></md-switch>
              </div>

              <div class="switch-row" id="row-sync-patch" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Sync Trust Patch</div>
                  <div class="switch-row-sub">Synchronize security patch date with KeyMint trust</div>
                </div>
                <md-switch icons="true" id="pif-sync-patch" aria-label="Sync Trust Patch" selected></md-switch>
              </div>

              <div class="switch-row" id="row-sync-ids" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">Sync Device IDs</div>
                  <div class="switch-row-sub">Apply brand, model, product to config.toml</div>
                </div>
                <md-switch icons="true" id="pif-sync-ids" aria-label="Sync Device IDs" selected></md-switch>
              </div>

              <div class="switch-row" id="row-unify-props" role="button" tabindex="0">
                <md-ripple></md-ripple>
                <div class="switch-row-content">
                  <div class="switch-row-title">
                    Unify Product Props
                    <span class="inline-badge badge-tertiary">Beta</span>
                  </div>
                  <div class="switch-row-sub">Apply resetprop -n across ro.product.*</div>
                </div>
                <md-switch icons="true" id="pif-unify-props" aria-label="Unify Product Props"></md-switch>
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
                <md-switch icons="true" id="pif-soter" aria-label="Tencent Soter"></md-switch>
              </div>
          </div>

        </div>
      </div>
    `

    this.#bindEvents()
    void this.load()
  }

  async load(): Promise<void> {
    // A pending debounced save carries an unsaved toggle. This method overwrites
    // every switch from disk, so without settling the queue first a toggle
    // followed by leaving and re-entering the tab inside the debounce window
    // visibly reverts, and the queued write then persists the value it captured
    // rather than what the user last chose.
    if (this.#saveTimer !== null) {
      clearTimeout(this.#saveTimer)
      this.#saveTimer = null
    }
    if (this.#isPersisting) {
      await this.#persistState()
    }

    const status = await this.#cli.detectIntegrityZygisk()
    this.#hasZygisk = status.provider !== null
    this.#canEnable = this.#hasZygisk && status.conflict === null
    this.#renderZygiskStatus(status)

    const state = await this.#readState()
    const isMasterEnabled = state.enabled && this.#canEnable
    this.#setSwitch('pif-enabled', isMasterEnabled)
    this.#updateDependentMuteState(isMasterEnabled)
    this.#setSwitch('pif-spoof-build', state.spoof_build)
    this.#setSwitch('pif-spoof-props', state.spoof_props)
    this.#setSwitch('pif-spoof-vending', state.spoof_vending_finger)
    this.#setSwitch('pif-sync-patch', state.sync_trust_patch)
    this.#setSwitch('pif-sync-ids', state.sync_device_ids)
    this.#setSwitch('pif-unify-props', state.unify_product_props)
    this.#setSwitch('pif-auto-fetch-fp', state.auto_fetch_fingerprint)
    this.#setSwitch('pif-soter', state.soter_beta)

    const enabledSwitch = this.#container?.querySelector<MdSwitch>('#pif-enabled')
    if (enabledSwitch) enabledSwitch.disabled = !this.#canEnable
    const soterSwitch = this.#container?.querySelector<MdSwitch>('#pif-soter')
    if (soterSwitch) soterSwitch.disabled = !this.#hasZygisk
    const propPath = (await File.exist(PROP_PATH)) ? PROP_PATH : PROP_PATH_DATA
    if (await File.exist(propPath)) {
      try {
        this.#cachedProp = parseKv(await File.read(propPath))
      } catch {
        this.#cachedProp = {}
      }
    } else {
      this.#cachedProp = {}
    }
  }

  async refresh(): Promise<void> {
    await this.load()
  }

  #bindEvents(): void {
    if (!this.#container) return

    const pickerRow = this.#container.querySelector<HTMLElement>('#row-select-fp')
    pickerRow?.addEventListener('click', () => {
      void this.#selectFingerprint()
    })
    // Match the other rows: the element claims role="button", so it has to act
    // like one for keyboard and switch users.
    pickerRow?.addEventListener('keydown', (e) => {
      if (e.key !== 'Enter' && e.key !== ' ') return
      e.preventDefault()
      void this.#selectFingerprint()
    })
    const statusEl = this.#container?.querySelector<HTMLElement>('#pif-zygisk-status')
    statusEl?.addEventListener('click', () => {
      statusEl.blur()
      if (this.#zygiskInfo.description) {
        this.#snackbar.show(this.#zygiskInfo.description, !this.#zygiskInfo.conflict && !!this.#zygiskInfo.provider)
      }
    })

    const bindRow = (rowId: string, switchId: string) => {
      const row = this.#container?.querySelector<HTMLElement>(`#${rowId}`)
      const sw = this.#container?.querySelector<MdSwitch>(`#${switchId}`)
      if (row && sw) {
        const toggle = (): void => {
          if (sw.disabled || row.classList.contains('switch-row--muted')) return
          sw.selected = !sw.selected
          this.#handleToggle(switchId, sw.selected)
        }
        row.addEventListener('click', (e) => {
          // Let native md-switch interaction handle direct clicks on the switch
          if (e.composedPath().some((n) => n instanceof Element && n.localName === 'md-switch')) return
          toggle()
        })
        // The row is role="button" and focusable, so Enter and Space have to work
        // or the role is a lie and the row is unreachable by keyboard.
        row.addEventListener('keydown', (e) => {
          if (e.key !== 'Enter' && e.key !== ' ') return
          e.preventDefault()
          toggle()
        })
        sw.addEventListener('change', () => {
          if (row.classList.contains('switch-row--muted')) return
          this.#handleToggle(switchId, sw.selected)
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
    bindRow('row-auto-fetch-fp', 'pif-auto-fetch-fp')
    bindRow('row-soter', 'pif-soter')
  }
  #renderZygiskStatus(status: { provider: string | null; conflict: string | null }): void {
    const statusEl = this.#container?.querySelector<HTMLElement>('#pif-zygisk-status')
    if (!statusEl) return
    const iconEl = statusEl.querySelector<HTMLElement>('.integrity-status-icon')
    const textEl = statusEl.querySelector<HTMLElement>('.integrity-status-text')

    if (status.conflict) {
      statusEl.className = 'integrity-status-banner integrity-status-banner--conflict'
      if (iconEl) iconEl.textContent = 'warning'
      if (textEl)
        textEl.textContent = `Disabled: ${status.conflict} is loaded. Remove it before enabling OMK Integrity.`
      this.#zygiskInfo = {
        provider: status.provider,
        conflict: status.conflict,
        description: `Disabled: ${status.conflict} is loaded. Remove it before enabling OMK Integrity.`,
      }
    } else if (!status.provider) {
      statusEl.className = 'integrity-status-banner integrity-status-banner--error'
      if (iconEl) iconEl.textContent = 'error'
      if (textEl)
        textEl.textContent =
          'Disabled: Zygisk not found. Install ReZygisk (preferred), ZygiskNext, NeoZygisk, or Magisk Zygisk.'
      this.#zygiskInfo = {
        provider: null,
        conflict: null,
        description:
          'Disabled: Zygisk not found. Install ReZygisk (preferred), ZygiskNext, NeoZygisk, or Magisk Zygisk.',
      }
    } else {
      const label =
        status.provider === 'rezygisk'
          ? 'ReZygisk'
          : status.provider === 'zygisk_next'
            ? 'ZygiskNext'
            : status.provider === 'neozygisk'
              ? 'NeoZygisk'
              : 'Magisk Zygisk'
      statusEl.className = 'integrity-status-banner integrity-status-banner--ok'
      if (iconEl) iconEl.textContent = 'check_circle'
      if (textEl) textEl.textContent = `Zygisk: ${label}`
      this.#zygiskInfo = {
        provider: status.provider,
        conflict: null,
        description: `Active Zygisk implementation: ${label}`,
      }
    }
  }

  #handleToggle(switchId: string, value: boolean): void {
    if (switchId === 'pif-soter' && value && !this.#hasZygisk) {
      this.#snackbar.show('Zygisk required for Tencent Soter', false)
      this.#setSwitch('pif-soter', false)
      return
    }
    if (switchId === 'pif-enabled' && value && !this.#canEnable) {
      this.#snackbar.show('Zygisk required to enable Integrity', false)
      this.#setSwitch('pif-enabled', false)
      return
    }

    if (switchId === 'pif-enabled') {
      this.#updateDependentMuteState(value)
    }

    this.#scheduleSave()
  }

  #scheduleSave(): void {
    clearTimeout(this.#saveTimer ?? undefined)
    this.#saveTimer = window.setTimeout(() => {
      this.#saveTimer = null
      void this.#persistState()
    }, 400)
  }

  async #persistState(): Promise<void> {
    if (this.#isPersisting) {
      this.#hasPendingPersist = true
      return
    }
    this.#isPersisting = true

    const state: IntegrityState = {
      enabled: this.#getSwitch('pif-enabled'),
      spoof_build: this.#getSwitch('pif-spoof-build'),
      spoof_props: this.#getSwitch('pif-spoof-props'),
      spoof_vending_finger: this.#getSwitch('pif-spoof-vending'),
      sync_trust_patch: this.#getSwitch('pif-sync-patch'),
      sync_device_ids: this.#getSwitch('pif-sync-ids'),
      unify_product_props: this.#getSwitch('pif-unify-props'),
      soter_beta: this.#getSwitch('pif-soter'),
      auto_fetch_fingerprint: this.#getSwitch('pif-auto-fetch-fp'),
    }

    try {
      const toml = [
        `enabled = ${state.enabled}`,
        `spoof_build = ${state.spoof_build}`,
        `spoof_props = ${state.spoof_props}`,
        `spoof_vending_finger = ${state.spoof_vending_finger}`,
        `sync_trust_patch = ${state.sync_trust_patch}`,
        `sync_device_ids = ${state.sync_device_ids}`,
        `unify_product_props = ${state.unify_product_props}`,
        `soter_beta = ${state.soter_beta}`,
        `auto_fetch_fingerprint = ${state.auto_fetch_fingerprint}`,
      ].join('\n')

      // Routed through File.write rather than an inline heredoc. The inline form
      // used a fixed 'FileEOF' delimiter, so a value containing that line would
      // close the heredoc and run the rest as root. Nothing here is a string
      // today, but that is exactly the kind of thing a future field would break.
      await File.createDirectory(DATA_DIR)
      await File.createDirectory(ADB_DIR)
      await File.write(TOML_PATH, toml)
      await File.copy(TOML_PATH, TOML_PATH_DATA).catch(() => undefined)

      const prop = this.#cachedProp

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

      void this.#cli.killIntegrityTargets()
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      this.#snackbar.show(`Error saving setting: ${msg}`, false)
    } finally {
      this.#isPersisting = false
      if (this.#hasPendingPersist) {
        this.#hasPendingPersist = false
        this.#scheduleSave()
      }
    }
  }

  #updateDependentMuteState(enabled: boolean): void {
    for (const id of DEPENDENT_ROW_IDS) {
      const row = this.#container?.querySelector<HTMLElement>(`#${id}`)
      if (row) {
        row.classList.toggle('switch-row--muted', !enabled)
      }
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
        auto_fetch_fingerprint: parseBool(map.auto_fetch_fingerprint, false),
      }
    } catch {
      return { ...DEFAULTS }
    }
  }

  /**
   * Build the picker choices from data shipped in the bundle.
   *
   * Nothing here touches the network, which is the point: the picker used to
   * fetch every device's build list on each open, so the screen could not settle
   * until all of them had returned. #refreshChoices is the only networked path
   * and it runs only when the user asks for it.
   */
  #collectChoices(): FingerprintChoice[] {
    const choices: FingerprintChoice[] = []
    const template = activeTemplate()
    for (const device of PIXEL_DEVICES) {
      const builds = template[device.product]?.builds ?? []
      for (const [releaseCandidateName, buildId, major] of builds) {
        // The embedded shape is exactly what buildProp consumes, so the payload
        // is computed the same way whether a build came from the bundle or from a
        // refresh.
        const build = {
          product: device.product,
          releaseCandidateName,
          buildId,
          target: `${device.product}-user`,
        } as FlashBuild
        choices.push({
          build,
          product: device.product,
          model: device.model,
          major,
          initialSdk: device.min + 20,
          prop: buildProp(build, device.product, device.model, major, device.min + 20),
        })
      }
    }
    return choices
  }

  /**
   * Fetch the current build lists and fold them into the embedded set.
   *
   * Only reachable from the picker's Fetch Latest button. A product that fails is
   * left as it was rather than emptied, so a partial network failure cannot make
   * the picker worse than it already is.
   */
  async #refreshChoices(): Promise<{
    changed: boolean
    added: number
    failed: string[]
  }> {
    const results = await mapWithConcurrency(PIXEL_DEVICES, FETCH_CONCURRENCY, async (device) => {
      const builds = await this.#cli.fetchFlashstationBuilds(device.product)
      return { device, builds }
    })
    const previous = activeTemplate()
    const next: Template = {}
    let added = 0
    const failed: string[] = []
    for (const result of results) {
      if (result.status !== 'fulfilled') {
        failed.push('?')
        continue
      }
      const { device, builds } = result.value
      const rows: TemplateRow[] = []
      for (const build of builds) {
        if (build.target !== `${build.product}-user`) continue
        const major = buildMajor(build)
        if (major === null || major < device.min || major > device.max) continue
        rows.push([build.releaseCandidateName, String(build.buildId), major])
      }
      if (rows.length === 0) {
        failed.push(device.product)
        continue
      }
      // Upstream can retire builds, so anything the previous template knew about
      // is carried over. A dropped build should not silently vanish from the UI.
      const seen = new Set(rows.map((row) => `${row[0]}|${row[1]}`))
      const carried = (previous[device.product]?.builds ?? []).filter((row) => !seen.has(`${row[0]}|${row[1]}`))
      added += carried.length
      next[device.product] = {
        model: device.model,
        min: device.min,
        max: device.max,
        builds: [...rows, ...carried].sort((a, b) => b[2] - a[2] || a[0].localeCompare(b[0])),
      }
    }
    // A device that failed keeps its old entry, so a flaky network degrades to
    // "stale" rather than "device missing from the picker".
    for (const device of PIXEL_DEVICES) {
      if (next[device.product]) continue
      const old = previous[device.product]
      if (old) {
        next[device.product] = old
      } else {
        failed.push(device.product)
      }
    }
    if (!isValidTemplate(next)) {
      throw new Error('refusing to replace the template with an incomplete fetch')
    }
    if (templatesEquivalent(previous, next)) {
      return { changed: false, added: 0, failed }
    }
    // saveTemplate verifies the temp file before it renames over the good copy,
    // so a failure here leaves the existing template in place.
    await saveTemplate(next)
    return { changed: true, added, failed }
  }

  async #selectFingerprint(): Promise<void> {
    // No network here. The bundle ships a full list, and the device copy is
    // newer when a fetch or the daily refresh has replaced it since.
    await loadTemplate()
    let choices = this.#collectChoices()
    if (choices.length === 0) {
      this.#snackbar.show('No Google builds available', false)
      return
    }

    let majors = [...new Set(choices.map((choice) => choice.major))].sort((a, b) => b - a)
    const romMajor = Number.parseInt(androidMajor(await this.#cli.getBuildRelease()) ?? '', 10)
    let major = majors.includes(romMajor) ? romMajor : (majors[0] as number)

    const dialog = document.createElement('md-dialog')
    // The class is what the pill-shape and sizing rules key on. Without it the
    // selects render as square-cornered fields at their default width, and the
    // option list escapes the dialog instead of scrolling.
    dialog.className = 'picker-dialog'
    dialog.id = 'fp-picker-dialog'
    // Scrim clicks and Escape close the dialog, and neither path runs the
    // Cancel or Apply handler, so removal is bound to the closed event instead
    // of being done by hand in each button.
    dialog.addEventListener('closed', () => dialog.remove())
    // Remembered across renders so the Device and Build selections survive a
    // re-render, but are re-validated against the current Android version.
    let bodyMajor = major
    let bodyProduct = ''
    const render = (): void => {
      const products = [...new Set(choices.filter((c) => c.major === major).map((c) => c.product))]
      // The device the user had selected is honoured only while it still exists
      // in the newly chosen Android version. Changing the version rebuilds the
      // build list, so a product from the previous version must not survive and
      // then disagree with the builds shown next to it.
      const previousProduct = bodyProduct
      const activeProduct =
        previousProduct && products.includes(previousProduct) ? previousProduct : (products[0] as string)
      const deviceOptions = products
        .map((product) => {
          const choice = choices.find((c) => c.major === major && c.product === product)
          return `<md-select-option value="${escapeHtml(product)}" ${
            product === activeProduct ? 'selected' : ''
          }><div slot="headline">${escapeHtml(choice?.model ?? product)} (${escapeHtml(product)})</div></md-select-option>`
        })
        .join('')

      const builds = choices.filter((c) => c.major === major).sort(latestFirst)
      const byProduct = new Map<string, FingerprintChoice[]>()
      for (const choice of builds) {
        const list = byProduct.get(choice.product) ?? []
        list.push(choice)
        byProduct.set(choice.product, list)
      }
      const activeBuilds = byProduct.get(activeProduct) ?? []
      const buildOptions = activeBuilds
        .map((choice, index) => {
          const latest = choice.build.releaseBuildMetadata?.latest ? ' • latest' : ''
          const notes = choice.build.releaseBuildMetadata?.notes
          const carrier = notes ? ` • ${notes}` : ''
          const label = `${choice.build.releaseCandidateName} • ${choice.build.buildId}${latest}${carrier}`
          return `<md-select-option value="${index}" ${index === 0 ? 'selected' : ''}><div slot="headline">${escapeHtml(label)}</div></md-select-option>`
        })
        .join('')

      const preview = activeBuilds[0]
      const body = dialog.querySelector('#fp-body')
      if (!body) return
      const renderedProduct = selectValue(body, 'device')
      // The build index is only meaningful within one Android version, so it is
      // not carried across a version change.
      const keepBuild = bodyMajor === major ? selectValue(body, 'build') : ''
      body.innerHTML = /* html */ `
        <div style="display: flex; flex-direction: column; gap: 12px; min-width: 320px;">
          <md-outlined-select data-role="version" label="Android version" menu-positioning="popover" value="${major}">
            ${majors
              .map(
                (value) =>
                  `<md-select-option value="${value}" ${value === major ? 'selected' : ''}><div slot="headline">Android ${value}</div></md-select-option>`,
              )
              .join('')}
          </md-outlined-select>
          <md-outlined-select data-role="device" label="Device" menu-positioning="popover" value="${escapeHtml(renderedProduct || activeProduct)}">
            ${deviceOptions}
          </md-outlined-select>
          <md-outlined-select data-role="build" label="Build" menu-positioning="popover" value="${escapeHtml(keepBuild || '0')}">
            ${buildOptions}
          </md-outlined-select>
          <md-outlined-text-field id="fp-preview" label="Resulting integrity.prop" readonly>
            <div slot="supporting-text">${escapeHtml(preview?.prop ?? '')}</div>
          </md-outlined-text-field>
        </div>
      `
      boundOptionLists(body)
      bodyMajor = major
      bodyProduct = activeProduct
      // The rendered build index, so the preview matches the fields on open.
      // keepBuild is already empty across a version change, which resets this to
      // 0 because the index is not comparable between versions.
      currentBuild = Number.parseInt(keepBuild, 10) || 0
      currentProduct = activeProduct
      syncPreview()
    }

    // md-outlined-select keeps its selection in a JS property; the `value`
    // attribute is only the server-rendered initial value. Reading the attribute
    // therefore always returned the value from the last render, so a selection
    // appeared to do nothing.
    const selectValue = (body: ParentNode, role: string): string => {
      const el = body.querySelector<MdOutlinedSelect>(`md-outlined-select[data-role="${role}"]`)
      return el?.value ?? ''
    }

    /*
     * Bounds the option list to four items, then scrolls.
     *
     * The menu is built inside the select's own shadow root, so a rule in the
     * stylesheet cannot reach it at any scope: `md-menu` never appears in the
     * light DOM, which is why every `md-menu { max-height }` written so far did
     * nothing and the list opened past the top of the screen.
     *
     * Sizing it from the outside fails too. Within that shadow root the menu
     * surface and its item list both use `max-height: inherit` rather than a
     * custom property, and `max-height` is not an inherited property, so a
     * max-height on the host never reaches them. The menu exposes only an
     * `elevation` part, not the list, so `::part` cannot size it either.
     *
     * The rule is therefore injected into the select's shadow root, where it
     * matches the nested `md-menu` and its `max-height: inherit` resolves to
     * it. 192px is four options at the 48px menu-item height, and the list
     * already declares `overflow: auto`, so it scrolls past that.
     */
    // The selection is mirrored here rather than read back out of the DOM.
    // Reading the select does not work on the render that creates it: the element
    // has not upgraded yet, so `.value` is undefined, the product comes back
    // empty, and no build matches, which left the preview blank on open. The
    // change handler still reads the DOM, since the element is live by then.
    let currentProduct = ''
    let currentBuild = 0

    const syncPreview = (): void => {
      const list = choices.filter((c) => c.major === major && c.product === currentProduct).sort(latestFirst)
      const choice = list[currentBuild] ?? list[0]
      const field = dialog.querySelector('#fp-preview')
      const support = field?.querySelector('[slot="supporting-text"]')
      if (support) support.textContent = choice?.prop ?? ''
      if (field) (field as HTMLInputElement).value = choice?.prop ?? ''
    }

    dialog.innerHTML = /* html */ `
      <div slot="headline">Select Device Fingerprint</div>
      <form slot="content" id="fp-body" method="dialog"></form>
      <div slot="actions">
        <md-text-button id="fp-fetch-latest" class="fp-fetch-latest">
          <md-icon slot="icon">refresh</md-icon>
          Fetch Latest
        </md-text-button>
        <md-text-button id="fp-cancel">Cancel</md-text-button>
        <md-filled-button id="fp-apply">Apply</md-filled-button>
      </div>
    `
    document.body.appendChild(dialog)
    render()
    dialog.show()

    dialog.querySelector('#fp-body')?.addEventListener('change', (event) => {
      const role = (event.target as HTMLElement)?.getAttribute?.('data-role')
      if (role === 'version') {
        major = Number.parseInt((event.target as MdOutlinedSelect).value ?? '', 10) || major
        // A new Android version invalidates both the device choice and the
        // build index, so the next render re-derives them.
        bodyMajor = major
        bodyProduct = ''
        currentProduct = ''
        currentBuild = 0
        render()
      } else if (role === 'device') {
        bodyProduct = selectValue(dialog.querySelector('#fp-body') as ParentNode, 'device')
        render()
      } else if (role === 'build') {
        currentBuild = Number.parseInt(selectValue(dialog.querySelector('#fp-body') as ParentNode, 'build'), 10) || 0
        // Changing the build alone must update the preview without a re-render.
        syncPreview()
      }
    })

    const fetchBtn = dialog.querySelector<MdTextButton>('#fp-fetch-latest')
    const setFetching = (busy: boolean): void => {
      if (!fetchBtn) return
      fetchBtn.disabled = busy
      // Swap the label for a spinner so the wait is visible rather than looking
      // like a dead button.
      fetchBtn.innerHTML = busy
        ? `<md-circular-progress indeterminate slot="icon" class="fp-fetch-spinner"></md-circular-progress>Fetching`
        : `<md-icon slot="icon">refresh</md-icon>Fetch Latest`
    }
    fetchBtn?.addEventListener('click', () => {
      if (fetchBtn.disabled) return
      setFetching(true)
      void this.#refreshChoices()
        .then(({ changed, added, failed }) => {
          if (!changed) {
            // Nothing new upstream. The saved template was left untouched, so
            // there is no reason to rewrite it.
            this.#snackbar.show('Already up to date', true)
          } else if (failed.length > 0) {
            this.#snackbar.show(`Template updated, ${failed.length} device(s) kept previous data`, true)
          } else {
            this.#snackbar.show(added > 0 ? `Template updated with ${added} build(s)` : 'Template updated', true)
          }
          // Rebuild the choice list from the merged data and re-render, keeping
          // the current selection where it is still valid.
          choices = this.#collectChoices()
          majors = [...new Set(choices.map((c) => c.major))].sort((a, b) => b - a)
          render()
        })
        .catch(() => this.#snackbar.show('Failed to fetch latest builds', false))
        .finally(() => setFetching(false))
    })

    dialog.querySelector('#fp-cancel')?.addEventListener('click', () => {
      dialog.close()
    })

    dialog.querySelector('#fp-apply')?.addEventListener('click', async () => {
      // The mirrored selection, so Apply writes exactly what the preview showed.
      const list = choices.filter((c) => c.major === major && c.product === currentProduct).sort(latestFirst)
      const chosen = list[currentBuild] ?? list[0]
      dialog.close()
      if (!chosen) return
      await File.createDirectory(DATA_DIR)
      await File.createDirectory(ADB_DIR)
      await File.write(PROP_PATH, chosen.prop)
      await File.write(PROP_PATH_DATA, chosen.prop)
      await this.#cli.killIntegrityTargets()
      this.#cachedProp = parseKv(chosen.prop)
      this.#snackbar.show(`Applied ${chosen.model} ${chosen.build.releaseCandidateName}`)
    })
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
