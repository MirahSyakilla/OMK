import type { MdDialog, MdIconButton } from '@material/web/all'
import { applyDialogAnimation } from './animation'
import type { History } from '../history'

export class HelpDialog {
  #dialog: MdDialog | null = null
  #history: History | null = null

  constructor(history?: History) {
    this.#history = history ?? null
  }

  getElement(): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="help-dialog" class="dialog--fullscreen">
        <div slot="headline" class="fullscreen-dialog-header">
          <md-icon-button id="help-dialog-back" class="fs-back-btn" aria-label="Back">
            <md-icon>arrow_back</md-icon>
          </md-icon-button>
          <div class="fs-title-group">
            <h2 class="fs-title">Help & Documentation</h2>
            <span class="fs-subtitle">Guides, module concepts, and configuration</span>
          </div>
        </div>
        <div slot="content" class="fullscreen-dialog-content">
          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">apps</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">App List & Scoop</div>
                <div class="fs-card-subtitle">Targeted package interception</div>
              </div>
            </div>
            <div class="help-card-body">
              <p>Checked packages are written into <code>scoop</code> and will be intercepted by OMK. Unchecked packages are removed from <code>scoop</code>.</p>
            </div>
          </div>

          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">help_outline</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">Unknown Callers</div>
                <div class="fs-card-subtitle">Handling unresolved UID callers</div>
              </div>
            </div>
            <div class="help-card-body">
              <p><code>allow_unknown_package</code> only affects callers whose package name cannot be resolved by the injector. It does not auto-include normal app packages.</p>
            </div>
          </div>

          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">verified_user</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">Play Integrity</div>
                <div class="fs-card-subtitle">Zygisk property spoofing & fingerprints</div>
              </div>
            </div>
            <div class="help-card-body">
              <p>Spoofs DroidGuard and Play Store build fields when ReZygisk (preferred), ZygiskNext, NeoZygisk, or Magisk Zygisk is loaded. <b>Fetch</b> pulls a Pixel <code>pif.prop</code> from Play Integrity Fix's GitHub dump; <b>Update</b> refreshes the current product. If Play Integrity Fix or Play Integrity Fork is already installed, Enable and Reapply Integrity stay disabled. <b>Tencent Soter</b> stays available with Zygisk and answers <code>com.tencent.soter.soterserver</code> with simulated replies. It is not a hardware key or a payment fix. Save restarts KeyMint and the injector, then force-stops Play Services, Play Store, and Soter.</p>
            </div>
          </div>

          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">security</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">Trust Settings</div>
                <div class="fs-card-subtitle">Attestation values and lock states</div>
              </div>
            </div>
            <div class="help-card-body">
              <p>Use the Trust dialog for <code>security_patch</code>, <code>vb_key</code>, <code>vb_hash</code>, <code>verified_boot_state</code>, and <code>device_locked</code>.</p>
            </div>
          </div>

          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">devices</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">Device & Crypto Seeds</div>
                <div class="fs-card-subtitle">Identity spoofing and derivation keys</div>
              </div>
            </div>
            <div class="help-card-body">
              <p>Device props change attestation identity. Crypto seeds control OMK key material; keep them stable unless you intentionally want new storage and auth-token roots.</p>
            </div>
          </div>

          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">vpn_key</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">Keybox Management</div>
                <div class="fs-card-subtitle">Hardware certificates and slot assignment</div>
              </div>
            </div>
            <div class="help-card-body">
              <p>AOSP is the bundled keybox. AlwaysStrong fetches from Evoker. Repo opens the KOWX712 picker. Self-Signed generates a local dummy. Manage slots directly from the Keybox tab to rename, export, or delete them. Tap a card to expand certificate expiry and validity details. Long-press an app in the Apps tab to assign a keybox slot. Expired boxes display an <b>Expired</b> badge. Play Services and Play Store show a <b>PIF</b> pill while Integrity is enabled.</p>
            </div>
          </div>

          <div class="fs-card">
            <div class="fs-card-header">
              <md-icon class="fs-card-icon">restart_alt</md-icon>
              <div class="fs-card-title-group">
                <div class="fs-card-title">Reload & Daemon Control</div>
                <div class="fs-card-subtitle">Service lifecycle and process refresh</div>
              </div>
            </div>
            <div class="help-card-body">
              <p>The restart icon in the top header restarts the KeyMint daemon, the injector, or both. <b>Restart All</b> also reapplies Integrity by stopping Play Services and Play Store. <b>Reapply Integrity</b> only does that GMS/Store restart. Confirm first. Apps using Keystore may fail until the process is back.</p>
            </div>
          </div>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#dialog = fragment.querySelector<MdDialog>('#help-dialog')
    fragment.querySelector<MdIconButton>('#help-dialog-back')!.onclick = () => this.close()

    if (this.#dialog) {
      Object.assign(this.#dialog, {
        requestClose: () => this.close(),
      })
      this.#dialog.addEventListener('closed', () => {
        this.#history?.consume('help')
      })
    }

    return fragment
  }

  initAnimation(): void {
    if (this.#dialog) applyDialogAnimation(this.#dialog)
  }

  show(): void {
    this.#history?.push('help', () => this.close())
    this.#dialog?.show()
  }

  close(): void {
    this.#dialog?.close()
  }
}
