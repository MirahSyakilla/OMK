import type { MdDialog, MdTextButton } from '@material/web/all'
import { applyDialogAnimation } from './animation'

export class HelpDialog {
  #dialog: MdDialog | null = null

  getElement(): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="help-dialog">
        <div slot="headline">Help</div>
        <div slot="content" class="help-content">
          <div class="instruction">
            <h3>App list</h3>
            <p>Checked packages are written into <code>scoop</code> and will be intercepted by OMK. Unchecked packages are removed from <code>scoop</code>.</p>
          </div>
          <div class="instruction">
            <h3>Unknown callers</h3>
            <p><code>allow_unknown_package</code> only affects callers whose package name cannot be resolved by the injector. It does not auto-include normal app packages.</p>
          </div>
          <div class="instruction">
            <h3>Trust settings</h3>
            <p>Use the Trust dialog for <code>security_patch</code>, <code>vb_key</code>, <code>vb_hash</code>, <code>verified_boot_state</code>, and <code>device_locked</code>.</p>
          </div>
          <div class="instruction">
            <h3>Device and crypto</h3>
            <p>Device props change attestation identity. Crypto seeds control OMK key material; keep them stable unless you intentionally want new storage and auth-token roots.</p>
          </div>
          <div class="instruction">
            <h3>Keybox</h3>
            <p>AOSP resets to the module’s bundled keybox. Repo opens the KOWX712 keybox picker flow inside the WebUI. Unknown generates a self-signed fallback. Long-press an app and choose <b>Select Keybox</b> to assign a numbered slot.</p>
          </div>
        </div>
        <div slot="actions">
          <md-text-button id="close-help">Close</md-text-button>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#dialog = fragment.querySelector<MdDialog>('#help-dialog')
    fragment.querySelector<MdTextButton>('#close-help')!.onclick = () => this.close()
    return fragment
  }

  initAnimation(): void {
    if (this.#dialog) applyDialogAnimation(this.#dialog)
  }

  show(): void {
    this.#dialog?.show()
  }

  close(): void {
    this.#dialog?.close()
  }
}
