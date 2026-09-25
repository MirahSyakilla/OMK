import type { MdDialog, MdTextButton } from '@material/web/all'
import { Config } from '../config'
import { escapeHtml } from '../html'
import { applyDialogAnimation } from './animation'

export class RuntimeDialog {
  #dialog: MdDialog | null = null
  #content: HTMLElement | null = null
  #config: Config

  constructor(config: Config) {
    this.#config = config
  }

  getElement(): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="trust-record-dialog">
        <div slot="headline">Trust Record</div>
        <div slot="content">
          <div id="trust-record-content"></div>
        </div>
        <div slot="actions">
          <md-text-button id="trust-record-close">Close</md-text-button>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#dialog = fragment.querySelector<MdDialog>('#trust-record-dialog')
    this.#content = fragment.querySelector<HTMLElement>('#trust-record-content')
    fragment.querySelector<MdTextButton>('#trust-record-close')!.onclick = () => this.close()
    return fragment
  }

  initAnimation(): void {
    if (this.#dialog) applyDialogAnimation(this.#dialog)
  }

  show(): void {
    const record = (this.#config.get('trust_record') as Record<string, string | boolean>) ?? {}
    const rows = Object.entries(record)
      .filter(([, value]) => value !== undefined && value !== '')
      .map(([key, value]) => /* html */ `
        <div class="trust-record-row">
          <div class="trust-record-key">${escapeHtml(key)}</div>
          <div class="trust-record-value">${escapeHtml(String(value))}</div>
        </div>
      `)
      .join('')

    if (this.#content) {
      this.#content.innerHTML = rows || '<p class="trust-record-empty">No runtime trust record has been written yet.</p>'
    }
    this.#dialog?.show()
  }

  close(): void {
    this.#dialog?.close()
  }
}
