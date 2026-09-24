import type { Cli } from '../cli'
import type { Config } from '../config'
import type { Keybox } from '../keybox/keybox'
import type { Snackbar } from '../snackbar/snackbar'
import type { Navigation } from '../navigation'
import { File } from '../file'
import { i18n } from '../i18n'
import './home.scss'

export class HomeScreen {
  #cli: Cli
  #config: Config
  #keybox: Keybox
  #snackbar: Snackbar
  #navigation: Navigation | null = null
  #container: HTMLElement | null = null
  #onRestartRequested: (() => void) | null = null
  #onShowTrustRecord: (() => void) | null = null

  constructor(
    cli: Cli,
    config: Config,
    keybox: Keybox,
    snackbar: Snackbar,
  ) {
    this.#cli = cli
    this.#config = config
    this.#keybox = keybox
    this.#snackbar = snackbar
  }

  setNavigation(nav: Navigation): void {
    this.#navigation = nav
  }

  onRestart(cb: () => void): void {
    this.#onRestartRequested = cb
  }

  onShowTrustRecord(cb: () => void): void {
    this.#onShowTrustRecord = cb
  }

  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="home-screen">
        <div class="home-section-title">Overview Metrics</div>
        <div class="telemetry-grid">
          <!-- Widget 1: KeyMint & Injector -->
          <div class="telemetry-card" id="widget-km-inj" role="button" tabindex="0">
            <div class="card-header">
              <span class="card-icon"><md-icon>security</md-icon></span>
              <span class="card-label">KeyMint & Injector</span>
            </div>
            <div class="card-headline" id="home-km-status">Checking...</div>
            <div class="card-subtext" id="home-inj-status">Hook verifying...</div>
            <div class="card-meta" id="home-intercept-methods">10/10 Methods routed</div>
            <md-ripple></md-ripple>
          </div>

          <!-- Widget 2: Active Keybox -->
          <div class="telemetry-card" id="widget-keybox" role="button" tabindex="0">
            <div class="card-header">
              <span class="card-icon"><md-icon>vpn_key</md-icon></span>
              <span class="card-label">Active Keybox</span>
            </div>
            <div class="card-headline" id="home-kb-name">Slot 0: Default</div>
            <div class="card-badges" id="home-kb-badges">
              <span class="badge badge-primary">RSA</span>
              <span class="badge badge-primary">EC</span>
              <span class="badge badge-ok">Valid</span>
            </div>
            <div class="card-meta" id="home-kb-expiry">Expires: Loading...</div>
            <md-ripple></md-ripple>
          </div>

          <!-- Widget 3: Targeted Apps -->
          <div class="telemetry-card" id="widget-apps" role="button" tabindex="0">
            <div class="card-header">
              <span class="card-icon"><md-icon>apps</md-icon></span>
              <span class="card-label">Targeted Apps</span>
            </div>
            <div class="card-headline" id="home-apps-count">0 Apps</div>
            <div class="card-subtext" id="home-apps-subtext">Scoop protection active</div>
            <div class="card-meta" id="home-pif-count">0 with PIF hook</div>
            <md-ripple></md-ripple>
          </div>

          <!-- Widget 4: Play Integrity -->
          <div class="telemetry-card" id="widget-integrity" role="button" tabindex="0">
            <div class="card-header">
              <span class="card-icon"><md-icon>verified_user</md-icon></span>
              <span class="card-label">Play Integrity</span>
            </div>
            <div class="card-headline" id="home-pif-zygisk">Zygisk Provider</div>
            <div class="card-subtext" id="home-pif-fp">Fingerprint loading...</div>
            <div class="card-meta" id="home-pif-patch">Patch: Auto-synced</div>
            <md-ripple></md-ripple>
          </div>
        </div>

        <div class="home-section-title">Active Shortcuts & Controls</div>
        <div class="shortcuts-card">
          <!-- Slot Switcher Row -->
          <div class="shortcut-row slot-switcher-row">
            <div class="shortcut-icon"><md-icon>swap_horiz</md-icon></div>
            <div class="shortcut-content">
              <div class="shortcut-title">Active Keybox Slot</div>
              <div class="shortcut-sub">Select default slot for unassigned apps</div>
            </div>
            <div class="shortcut-control">
              <select id="home-slot-picker" class="home-select" aria-label="Active Keybox Slot">
                <option value="0">Slot 0: Default</option>
              </select>
            </div>
          </div>

          <!-- Action: Restart Services -->
          <div class="shortcut-row action-row" id="home-restart-action" role="button" tabindex="0">
            <div class="shortcut-icon"><md-icon>restart_alt</md-icon></div>
            <div class="shortcut-content">
              <div class="shortcut-title">Restart Services</div>
              <div class="shortcut-sub">Reload KeyMint daemon and Keystore2 injector</div>
            </div>
            <div class="shortcut-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>

          <!-- Action: Force Stop GMS -->
          <div class="shortcut-row action-row" id="home-kill-gms-action" role="button" tabindex="0">
            <div class="shortcut-icon icon-warn"><md-icon>block</md-icon></div>
            <div class="shortcut-content">
              <div class="shortcut-title">Force Stop & Clear Play Store</div>
              <div class="shortcut-sub">Stop GMS, Vending, and unstable DroidGuard processes</div>
            </div>
            <div class="shortcut-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>

          <!-- Action: View Trust Record -->
          <div class="shortcut-row action-row" id="home-trust-action" role="button" tabindex="0">
            <div class="shortcut-icon"><md-icon>policy</md-icon></div>
            <div class="shortcut-content">
              <div class="shortcut-title">View Trust Record</div>
              <div class="shortcut-sub">Inspect read-only hardware backing & runtime state</div>
            </div>
            <div class="shortcut-arrow"><md-icon>chevron_right</md-icon></div>
            <md-ripple></md-ripple>
          </div>
        </div>
      </div>
    `

    this.#bindEvents()
    void this.refresh()
  }

  #bindEvents(): void {
    if (!this.#container) return

    // Widget click navigation jumps
    this.#container.querySelector('#widget-km-inj')?.addEventListener('click', () => {
      this.#navigation?.switchToTab(4) // Settings tab
    })
    this.#container.querySelector('#widget-keybox')?.addEventListener('click', () => {
      this.#navigation?.switchToTab(3) // Keybox tab
    })
    this.#container.querySelector('#widget-apps')?.addEventListener('click', () => {
      this.#navigation?.switchToTab(1) // Apps tab
    })
    this.#container.querySelector('#widget-integrity')?.addEventListener('click', () => {
      this.#navigation?.switchToTab(2) // Play Integrity tab
    })

    // Actions
    this.#container.querySelector('#home-restart-action')?.addEventListener('click', () => {
      if (this.#onRestartRequested) {
        this.#onRestartRequested()
      }
    })

    this.#container.querySelector('#home-kill-gms-action')?.addEventListener('click', async () => {
      try {
        await this.#cli.killIntegrityTargets()
        this.#snackbar.show(i18n.t('toast_integrity_reapplied') || 'Stopped GMS & Play Store')
      } catch {
        this.#snackbar.show('Failed to stop Google Play services', false)
      }
    })

    this.#container.querySelector('#home-trust-action')?.addEventListener('click', () => {
      if (this.#onShowTrustRecord) {
        this.#onShowTrustRecord()
      }
    })

    // Slot picker change
    const picker = this.#container.querySelector<HTMLSelectElement>('#home-slot-picker')
    picker?.addEventListener('change', () => {
      const slot = Number.parseInt(picker.value, 10)
      this.#snackbar.show(`Active keybox slot set to ${this.#keybox.slotLabel(slot)}`)
      void this.refresh()
    })
  }

  async refresh(): Promise<void> {
    if (!this.#container) return

    try {
      // 1. Service Status
      const status = await this.#cli.getServiceStatus()
      const kmEl = this.#container.querySelector<HTMLElement>('#home-km-status')
      const injEl = this.#container.querySelector<HTMLElement>('#home-inj-status')
      if (kmEl) {
        kmEl.textContent = status.keymint ? '🟢 Daemon Running' : '🔴 KeyMint Stopped'
        kmEl.className = status.keymint ? 'card-headline text-ok' : 'card-headline text-error'
      }
      if (injEl) {
        injEl.textContent = status.injector ? '🟢 Hook Injected' : '🔴 Not Hooked'
        injEl.className = status.injector ? 'card-subtext text-ok' : 'card-subtext text-error'
      }

      // 2. Intercept Methods Count
      const intercept = (this.#config.get('intercept') as Record<string, boolean>) ?? {}
      const activeMethods = Object.values(intercept).filter(Boolean).length
      const totalMethods = Object.keys(intercept).length || 10
      const methodsEl = this.#container.querySelector<HTMLElement>('#home-intercept-methods')
      if (methodsEl) {
        methodsEl.textContent = `${activeMethods}/${totalMethods} Methods routed`
      }

      // 3. Targeted Apps
      const targets = ((this.#config.get('target') as string[]) || []).filter(Boolean)
      const appsCountEl = this.#container.querySelector<HTMLElement>('#home-apps-count')
      if (appsCountEl) {
        appsCountEl.textContent = `${targets.length} Apps in Scoop`
      }

      // PIF targets count (GMS / Vending)
      const pifCount = targets.filter(
        (p) => p === 'com.google.android.gms' || p === 'com.android.vending',
      ).length
      const pifCountEl = this.#container.querySelector<HTMLElement>('#home-pif-count')
      if (pifCountEl) {
        pifCountEl.textContent = `${pifCount} with PIF Hook`
      }

      // 4. Play Integrity & Zygisk
      const zygisk = await this.#cli.detectIntegrityZygisk()
      const zygiskEl = this.#container.querySelector<HTMLElement>('#home-pif-zygisk')
      if (zygiskEl) {
        zygiskEl.textContent = zygisk.provider ?? (zygisk.conflict ? `Conflict: ${zygisk.conflict}` : 'No Zygisk')
        zygiskEl.className = zygisk.provider ? 'card-headline text-ok' : 'card-headline text-warn'
      }

      // Fingerprint preview
      const fpEl = this.#container.querySelector<HTMLElement>('#home-pif-fp')
      const propContent = await File.read('/data/adb/omk/integrity.prop').catch(() => '')
      const fpMatch = propContent.match(/FINGERPRINT=([^\n]+)/)
      if (fpEl) {
        if (fpMatch && fpMatch[1]) {
          const parts = fpMatch[1].trim().split('/')
          fpEl.textContent = parts.length > 2 ? `${parts[1]} (${parts[2].split(':')[0]})` : fpMatch[1].slice(0, 24)
        } else {
          fpEl.textContent = 'Default system props'
        }
      }

      // 5. Active Keybox Slot & Certificates
      const slots = [0, ...(await this.#cli.getKeyboxSlots(this.#config.configPath).catch(() => []))]
      const picker = this.#container.querySelector<HTMLSelectElement>('#home-slot-picker')
      if (picker) {
        const curVal = picker.value
        picker.innerHTML = slots
          .map((slot) => `<option value="${slot}">${this.#keybox.slotLabel(slot)}</option>`)
          .join('')
        picker.value = curVal || '0'
      }

      const kbNameEl = this.#container.querySelector<HTMLElement>('#home-kb-name')
      if (kbNameEl) {
        kbNameEl.textContent = this.#keybox.slotLabel(0)
      }

      // Parse slot 0 certs for badge/expiry
      const kbXml = await File.read(this.#keybox.getKeyboxPath(0)).catch(() => '')
      const badgesEl = this.#container.querySelector<HTMLElement>('#home-kb-badges')
      const expiryEl = this.#container.querySelector<HTMLElement>('#home-kb-expiry')

      if (kbXml && badgesEl && expiryEl) {
        const hasRsa = /algorithm\s*=\s*"rsa"/i.test(kbXml)
        const hasEc = /algorithm\s*=\s*"ecdsa"/i.test(kbXml)
        const badges: string[] = []
        if (hasRsa) badges.push('<span class="badge badge-primary">RSA</span>')
        if (hasEc) badges.push('<span class="badge badge-primary">EC</span>')
        badges.push('<span class="badge badge-ok">Loaded</span>')
        badgesEl.innerHTML = badges.join('')

        const expiryMatch = kbXml.match(/notAfter\b[^>]*>([^<]+)/i)
        if (expiryMatch && expiryMatch[1]) {
          expiryEl.textContent = `Expires: ${expiryMatch[1].trim()}`
        } else {
          expiryEl.textContent = 'Active & Ready'
        }
      }
    } catch {
      // Best-effort telemetry update
    }
  }
}
