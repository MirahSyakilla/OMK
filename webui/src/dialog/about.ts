import type { MdDialog, MdFilledButton, MdTextButton } from '@material/web/all'
import { Cli } from '../cli'
import { TELEGRAM_CHANNEL } from '../constant'
import { applyDialogAnimation } from './animation'

export class AboutDialog {
  #dialog: MdDialog | null = null
  #cli: Cli

  constructor(cli: Cli) {
    this.#cli = cli
  }

  getElement(): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="about-dialog">
        <div slot="headline" class="about-headline">
          <div class="about-title-row">
            <div id="module_name_line1">OhMyKeymint</div>
            <div id="working-mode-tag">OMK</div>
          </div>
          <div id="module_name_line2">Built-in WebUI</div>
          <div id="module-version"></div>
          <div id="author">by James Clef, MirahSyakilla</div>
        </div>
        <div slot="content">
          <p>Manage OMK scoop targets, trust policy, injector settings, device props, crypto seeds, and keyboxes without a separate addon module.</p>
          <p><strong>Config paths</strong><br>/data/misc/keystore/omk/config.toml<br>/data/misc/keystore/omk/injector.toml</p>
          <div class="link">
            <md-filled-button id="telegram">
              <span>Telegram</span>
              <md-icon slot="icon">send</md-icon>
            </md-filled-button>
            <md-filled-button id="github">
              <span>GitHub</span>
              <md-icon slot="icon">code</md-icon>
            </md-filled-button>
          </div>
        </div>
        <div slot="actions">
          <md-text-button id="close-about">Close</md-text-button>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#dialog = fragment.querySelector<MdDialog>('#about-dialog')

    fragment.querySelector<MdFilledButton>('#telegram')!.onclick = () => {
      void this.#cli.linkRedirect(TELEGRAM_CHANNEL)
    }
    fragment.querySelector<MdFilledButton>('#github')!.onclick = () => {
      void this.#cli.linkRedirect(this.#cli.getRepositoryUrl())
    }
    fragment.querySelector<MdTextButton>('#close-about')!.onclick = () => this.close()

    void this.#loadModuleVersion()
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

  async #loadModuleVersion(): Promise<void> {
    try {
      const info = await this.#cli.getModuleInfo()
      const version = info.version || info.versionName || ''
      const versionCode = info.versionCode || ''
      const el = this.#dialog?.querySelector('#module-version')
      if (el) {
        el.textContent = versionCode ? `${version} (${versionCode})` : version
      }
    } catch {
      // Ignore version failures in the UI.
    }
  }
}
