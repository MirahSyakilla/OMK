import type { DialogController } from '../dialog/dialog'
import type { Config, Policy } from '../config'
import './settings_screen.scss'

const INTERCEPT_KEYS = [
  'get_security_level',
  'get_key_entry',
  'update_subcomponent',
  'list_entries',
  'delete_key',
  'grant',
  'ungrant',
  'get_number_of_entries',
  'list_entries_batched',
  'get_supplementary_attestation_info',
]

export class SettingsScreen {
  readonly #dialogController: DialogController
  readonly #config: Config
  #container: HTMLElement | null = null

  constructor(dialogController: DialogController, config: Config) {
    this.#dialogController = dialogController
    this.#config = config
  }

  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="settings-screen">
        <!-- Group 1: KeyMint -->
        <div class="settings-section-title">KeyMint</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-core" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>memory</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Core Settings</div>
              <div class="settings-sub" id="sub-core">${this.#getCoreSummary()}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <!-- Group 2: Trust -->
        <div class="settings-section-title">Trust</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-trust" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>verified_user</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Trust Settings</div>
              <div class="settings-sub" id="sub-trust">${this.#getTrustSummary()}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
          <div class="settings-row" id="setting-runtime" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>history_edu</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Trust Record (read-only)</div>
              <div class="settings-sub" id="sub-runtime">View recorded boot parameters</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <!-- Group 3: Injector -->
        <div class="settings-section-title">Injector</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-injector" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>cable</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Injector Settings</div>
              <div class="settings-sub" id="sub-injector">${this.#getInjectorSummary()}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <!-- Group 4: Filtering -->
        <div class="settings-section-title">Filtering</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-filter" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>filter_list</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Package Filter</div>
              <div class="settings-sub" id="sub-filter">${this.#getFilterSummary()}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
          <div class="settings-row" id="setting-intercept" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>alt_route</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Intercept Matrix</div>
              <div class="settings-sub" id="sub-intercept">${this.#getInterceptSummary()}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <!-- Group 5: Device Identity -->
        <div class="settings-section-title">Device Identity</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-device" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>smartphone</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Device Properties</div>
              <div class="settings-sub" id="sub-device">${this.#getDeviceSummary()}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <!-- Group 6: Cryptography -->
        <div class="settings-section-title">Cryptography</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-crypto" role="button" tabindex="0">
            <div class="settings-icon icon-sensitive"><md-icon>vpn_key</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Crypto Seeds</div>
              <div class="settings-sub" id="sub-crypto">Root KEK, KAK & HMAC seeds</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <!-- Group 7: About -->
        <div class="settings-section-title">About</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-help" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>help_outline</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Help</div>
              <div class="settings-sub" id="sub-help">Documentation & usage guide</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
          <div class="settings-row" id="setting-about" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>info</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">About</div>
              <div class="settings-sub" id="sub-about">OpenKeyMint WebUI</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>
      </div>
    `

    this.#bindRow('setting-core', () => this.#dialogController.showCore())
    this.#bindRow('setting-trust', () => this.#dialogController.showTrust())
    this.#bindRow('setting-runtime', () => this.#dialogController.showRuntime())
    this.#bindRow('setting-injector', () => this.#dialogController.showInjector())
    this.#bindRow('setting-filter', () => this.#dialogController.showFilter())
    this.#bindRow('setting-intercept', () => this.#dialogController.showIntercept())
    this.#bindRow('setting-device', () => this.#dialogController.showDevice())
    this.#bindRow('setting-crypto', () => this.#dialogController.showCrypto())
    this.#bindRow('setting-help', () => this.#dialogController.showHelp())
    this.#bindRow('setting-about', () => this.#dialogController.showAbout())
  }

  updateSummaries(): void {
    if (!this.#container) return
    this.#updateText('#sub-core', this.#getCoreSummary())
    this.#updateText('#sub-trust', this.#getTrustSummary())
    this.#updateText('#sub-injector', this.#getInjectorSummary())
    this.#updateText('#sub-filter', this.#getFilterSummary())
    this.#updateText('#sub-intercept', this.#getInterceptSummary())
    this.#updateText('#sub-device', this.#getDeviceSummary())
  }

  #updateText(selector: string, text: string): void {
    const el = this.#container?.querySelector(selector)
    if (el) el.textContent = text
  }

  #bindRow(id: string, handler: () => void): void {
    const el = this.#container?.querySelector<HTMLElement>(`#${id}`)
    if (!el) return
    el.addEventListener('click', () => {
      el.blur()
      handler()
    })
    el.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault()
        handler()
      }
    })
  }

  #getCoreSummary(): string {
    const main = this.#config.get('omk_main') as Policy | undefined
    const skip = main?.force_skip_system_biometric_hat_verification === true
    return `biometric: ${skip ? 'bypass' : 'default'}`
  }

  #getTrustSummary(): string {
    const trust = this.#config.get('trust') as Policy | undefined
    const patch = trust?.security_patch ?? 'auto'
    return `security_patch: ${patch}`
  }

  #getInjectorSummary(): string {
    const inj = this.#config.get('injector_main') as Policy | undefined
    const enabled = inj?.enabled !== false
    return `enabled: ${enabled}`
  }

  #getFilterSummary(): string {
    const filter = this.#config.get('filter') as Policy | undefined
    const deny = filter?.deny_packages
    let count = 0
    if (typeof deny === 'string' && deny.trim().length > 0) {
      count = deny.split(/[\r\n,]+/).map((s) => s.trim()).filter(Boolean).length
    } else if (Array.isArray(deny)) {
      count = deny.length
    }
    return `deny_packages: ${count}`
  }

  #getInterceptSummary(): string {
    const intercept = this.#config.get('intercept') as Policy | undefined
    let count = 0
    for (const key of INTERCEPT_KEYS) {
      if (!intercept || intercept[key] !== false) count++
    }
    return `${count}/${INTERCEPT_KEYS.length} routed`
  }

  #getDeviceSummary(): string {
    const dev = this.#config.get('device') as Policy | undefined
    const brand = dev?.brand ? String(dev.brand) : 'Google'
    const model = dev?.model ? String(dev.model) : 'generic'
    return `brand: ${brand}, model: ${model}`
  }
}
