import type { MdDialog, MdFilledButton, MdIconButton, MdMenu, MdMenuItem, MdOutlinedButton } from '@material/web/all'
import { i18n } from '../i18n'
import { Cli, type OmKRestartTarget } from '../cli'
import { Snackbar } from '../snackbar/snackbar'
import { applyDialogAnimation } from '../dialog/animation'
import './reload_menu.scss'

const ACTIONS: Array<{ id: string; target: OmKRestartTarget }> = [
  { id: 'restart-keymint', target: 'keymint' },
  { id: 'restart-injector', target: 'injector' },
  { id: 'restart-all', target: 'all' },
]

const COOLDOWN_MS = 8000

export class ReloadMenu {
  #cli: Cli
  #snackbar: Snackbar
  #menu: MdMenu | null = null
  #dialog: MdDialog | null = null
  #dialogTitle: HTMLElement | null = null
  #dialogBody: HTMLElement | null = null
  #pending: OmKRestartTarget | null = null
  #busyUntil = 0

  constructor(cli: Cli, snackbar: Snackbar) {
    this.#cli = cli
    this.#snackbar = snackbar
  }

  appendTo(container: HTMLElement): void {
    container.appendChild(this.#getElement(container))
  }

  #getElement(anchorContainer: HTMLElement): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-menu id="reload-options" anchor="reload-button">
        <md-menu-item id="restart-keymint">
          <div slot="headline">${i18n.t('menu_restart_keymint')}</div>
        </md-menu-item>
        <md-menu-item id="restart-injector">
          <div slot="headline">${i18n.t('menu_restart_injector')}</div>
        </md-menu-item>
        <md-menu-item id="restart-all">
          <div slot="headline">${i18n.t('menu_restart_all')}</div>
        </md-menu-item>
      </md-menu>
      <md-dialog id="reload-confirm-dialog" type="alert">
        <div slot="headline" id="reload-confirm-title">${i18n.t('reload_confirm_title')}</div>
        <div slot="content" id="reload-confirm-body">${i18n.t('reload_confirm_body')}</div>
        <div slot="actions">
          <md-outlined-button id="reload-confirm-cancel">${i18n.t('functional_button_cancel')}</md-outlined-button>
          <md-filled-button id="reload-confirm-ok">${i18n.t('functional_button_confirm')}</md-filled-button>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#menu = fragment.querySelector<MdMenu>('#reload-options')
    this.#dialog = fragment.querySelector<MdDialog>('#reload-confirm-dialog')
    this.#dialogTitle = fragment.querySelector('#reload-confirm-title')
    this.#dialogBody = fragment.querySelector('#reload-confirm-body')
    if (this.#dialog) applyDialogAnimation(this.#dialog)

    anchorContainer.querySelector<MdIconButton>('#reload-button')!.onclick = () => {
      if (this.#menu) this.#menu.open = !this.#menu.open
    }

    ACTIONS.forEach(({ id, target }) => {
      fragment.querySelector<MdMenuItem>(`#${id}`)!.onclick = () => {
        if (this.#menu) this.#menu.open = false
        this.#ask(target)
      }
    })

    fragment.querySelector<MdOutlinedButton>('#reload-confirm-cancel')!.onclick = () => {
      this.#pending = null
      this.#dialog?.close()
    }
    fragment.querySelector<MdFilledButton>('#reload-confirm-ok')!.onclick = () => {
      void this.#run()
    }

    return fragment
  }

  #ask(target: OmKRestartTarget): void {
    this.#pending = target
    if (this.#dialogTitle) this.#dialogTitle.textContent = i18n.t(`reload_confirm_title_${target}`)
    if (this.#dialogBody) this.#dialogBody.textContent = i18n.t(`reload_confirm_body_${target}`)
    this.#dialog?.show()
  }

  async #run(): Promise<void> {
    const target = this.#pending
    this.#pending = null
    this.#dialog?.close()
    if (!target) return
    if (Date.now() < this.#busyUntil) {
      this.#snackbar.show(i18n.t('prompt_restart_busy'), false)
      return
    }
    this.#busyUntil = Date.now() + COOLDOWN_MS
    try {
      await this.#cli.requestRestart(target)
      this.#snackbar.show(i18n.t('prompt_restart_requested'), true)
    } catch {
      this.#busyUntil = 0
      this.#snackbar.show(i18n.t('prompt_restart_failed'), false)
    }
  }
}
