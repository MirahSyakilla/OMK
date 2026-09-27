import type { DialogController } from '../dialog/dialog'
import { Config, type Policy, INTERCEPT_KEYS } from '../config'
import type { Snackbar } from '../snackbar/snackbar'
import type { MdSwitch } from '@material/web/switch/switch'
import { escapeHtml } from '../html'
import './settings_screen.scss'

export class SettingsScreen {
  readonly #dialogController: DialogController
  readonly #config: Config
  readonly #snackbar?: Snackbar
  #container: HTMLElement | null = null

  constructor(dialogController: DialogController, config: Config, snackbar?: Snackbar) {
    this.#dialogController = dialogController
    this.#config = config
    this.#snackbar = snackbar
  }
  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="settings-screen">
        <!-- Group 1: KeyMint -->
        <div class="settings-section-title">KeyMint</div>
        <div class="settings-group">
          <div class="settings-row" id="setting-core" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>fingerprint</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Skip Biometric HAT Verification</div>
              <div class="settings-sub">Bypass system biometric hardware authentication token checks</div>
            </div>
            <md-switch icons="true" id="switch-core"${this.#isCoreSkipEnabled() ? ' selected' : ''}></md-switch>
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
              <div class="settings-sub" id="sub-trust">${escapeHtml(this.#getTrustSummary())}</div>
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
              <div class="settings-sub" id="sub-injector">${escapeHtml(this.#getInjectorSummary())}</div>
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
              <div class="settings-sub" id="sub-filter">${escapeHtml(this.#getFilterSummary())}</div>
            </div>
            <div class="settings-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
          <div class="settings-row" id="setting-intercept" role="button" tabindex="0">
            <div class="settings-icon"><md-icon>alt_route</md-icon></div>
            <div class="settings-content">
              <div class="settings-title">Intercept Matrix</div>
              <div class="settings-sub" id="sub-intercept">${escapeHtml(this.#getInterceptSummary())}</div>
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
              <div class="settings-sub" id="sub-device">${escapeHtml(this.#getDeviceSummary())}</div>
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

    const coreRow = this.#container?.querySelector<HTMLElement>('#setting-core')
    const coreSwitch = this.#container?.querySelector<MdSwitch>('#switch-core')
    if (coreRow && coreSwitch) {
      const handleToggle = async (selected: boolean) => {
        const prev = !selected
        try {
          this.#config.set('omk_main', {
            force_skip_system_biometric_hat_verification: selected,
          })
          if (!import.meta.env.DEV) {
            await this.#config.write()
          }
          this.#snackbar?.show(
            selected
              ? 'Biometric HAT verification bypass enabled'
              : 'Biometric HAT verification bypass disabled',
            true
          )
        } catch {
          coreSwitch.selected = prev
          this.#snackbar?.show('Failed to save setting', false)
        }
      }

      coreSwitch.addEventListener('change', () => {
        void handleToggle(coreSwitch.selected)
      })

      coreRow.addEventListener('click', (e) => {
        if (e.composedPath().some((n) => n instanceof Element && n.localName === 'md-switch')) {
          return
        }
        coreSwitch.selected = !coreSwitch.selected
        void handleToggle(coreSwitch.selected)
      })

      coreRow.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          coreSwitch.selected = !coreSwitch.selected
          void handleToggle(coreSwitch.selected)
        }
      })
    }
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
    const coreSwitch = this.#container.querySelector<MdSwitch>('#switch-core')
    if (coreSwitch) {
      coreSwitch.selected = this.#isCoreSkipEnabled()
    }
    this.#updateText('#sub-trust', this.#getTrustSummary())
    this.#updateText('#sub-injector', this.#getInjectorSummary())
    this.#updateText('#sub-filter', this.#getFilterSummary())
    this.#updateText('#sub-intercept', this.#getInterceptSummary())
    this.#updateText('#sub-device', this.#getDeviceSummary())
  }

  async refresh(): Promise<void> {
    this.updateSummaries()
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

  #isCoreSkipEnabled(): boolean {
    const main = this.#config.get('omk_main') as Policy | undefined
    return main?.force_skip_system_biometric_hat_verification === true
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
    return `${count}/${INTERCEPT_KEYS.length} features enabled`
  }

  #getDeviceSummary(): string {
    const dev = this.#config.get('device') as Policy | undefined
    const brand = dev?.brand ? String(dev.brand) : 'Google'
    const model = dev?.model ? String(dev.model) : 'generic'
    return `brand: ${brand}, model: ${model}`
  }
}
