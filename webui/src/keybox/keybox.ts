import type { MdDialog, MdFilledButton, MdOutlinedTextField, MdTextButton } from '@material/web/all'
import { i18n } from '../i18n'
import { Cli } from '../cli'
import { File } from '../file'
import { FileSelector } from '../file_selector/file_selector'
import { Snackbar } from '../snackbar/snackbar'
import { generateUnknownKeybox, isKeygenAvailable } from './unknown'
import { CustomKeyboxProvider } from './custom'
import { Config } from '../config'
import { applyDialogAnimation } from '../dialog/animation'
import './keybox.scss'

export type KeyboxSaveResult = 'saved' | 'cancelled' | 'error'

const MAX_KEYBOX_SLOT = 1024

export class Keybox {
  readonly cli: Cli
  readonly custom: CustomKeyboxProvider
  readonly #config: Config
  #fileSelector: FileSelector
  #snackbar: Snackbar
  #slotDialog: MdDialog | null = null
  #slotList: HTMLElement | null = null
  #slotTitle: HTMLElement | null = null
  #slotResolve: ((slot: number | null) => void) | null = null
  #appActionDialog: MdDialog | null = null
  #appActionPackage: HTMLElement | null = null
  #appActionResolve: ((selected: boolean) => void) | null = null
  #pendingPackageName: string | null = null
  #appActionSelecting = false
  #overwriteDialog: MdDialog | null = null
  #overwriteMessage: HTMLElement | null = null
  #overwriteResolve: ((overwrite: boolean) => void) | null = null

  constructor(cli: Cli, config: Config, fileSelector: FileSelector, snackbar: Snackbar) {
    this.cli = cli
    this.#config = config
    this.#fileSelector = fileSelector
    this.#snackbar = snackbar
    this.custom = new CustomKeyboxProvider(this, fileSelector, snackbar)
  }

  get keyboxPath(): string {
    return this.#config.configPath + '/keybox.xml'
  }

  getKeyboxPath(slot: number): string {
    return slot > 0
      ? `${this.#config.configPath}/keybox-slot-${slot}.xml`
      : this.keyboxPath
  }

  appendTo(container: HTMLElement): void {
    container.appendChild(this.#getElement())
    container.querySelectorAll<MdDialog>('md-dialog').forEach(d => applyDialogAnimation(d))
  }

  #getElement(): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="customkb-dialog" class="text-field-dialog">
        <div slot="headline">${i18n.t('customkb_dialog_title')}</div>
        <div slot="content">
          <md-outlined-text-field id="customkb-name-input" label="${i18n.t('customkb_name_placeholder')}" placeholder="" class="customkb-input">
            <md-icon slot="trailing-icon" class="hidden">error</md-icon>
          </md-outlined-text-field>
          <md-outlined-text-field id="customkb-link-input" label="URL" type="url" placeholder="https://raw.githubusercontent.com/" class="customkb-input">
            <md-icon slot="trailing-icon" class="hidden">error</md-icon>
          </md-outlined-text-field>
          <md-outlined-text-field id="customkb-script-input" label="${i18n.t('customkb_script_placeholder')}" placeholder="base64 -d" class="customkb-input" error-text="${i18n.t('prompt_custom_invalid_script')}">
            <md-icon slot="trailing-icon" class="hidden">error</md-icon>
          </md-outlined-text-field>
          <md-divider class="new"></md-divider>
          <div class="customkb-actions new">
            <md-filled-tonal-icon-button id="customkb-import"><md-icon>download</md-icon></md-filled-tonal-icon-button>
            <md-filled-tonal-icon-button id="customkb-export"><md-icon>upload</md-icon></md-filled-tonal-icon-button>
          </div>
        </div>
        <div slot="actions">
          <md-text-button id="reset-customkb" class="new">${i18n.t('functional_button_reset')}</md-text-button>
          <md-text-button id="remove-customkb" class="old">${i18n.t('functional_button_remove')}</md-text-button>
          <div class="spacer"></div>
          <md-text-button id="cancel-customkb">${i18n.t('functional_button_cancel')}</md-text-button>
          <md-text-button id="save-customkb">${i18n.t('functional_button_save')}</md-text-button>
        </div>
      </md-dialog>

      <md-dialog id="customkb-remove-dialog" type="alert">
        <div slot="headline">${i18n.t('customkb_remove_title')}</div>
        <md-icon slot="icon">delete</md-icon>
        <div slot="content">
          <div id="customkb-remove-single">${i18n.t('customkb_remove_message')}</div>
          <div id="customkb-reset" style="display: none">${i18n.t('customkb_reset_message')}</div>
        </div>
        <div slot="actions">
          <md-outlined-button id="cancel-remove-customkb">${i18n.t('functional_button_cancel')}</md-outlined-button>
          <md-filled-button id="confirm-remove-customkb">${i18n.t('functional_button_confirm')}</md-filled-button>
        </div>
      </md-dialog>

      <md-dialog id="keybox-slot-dialog">
        <div slot="headline" id="keybox-slot-title">${i18n.t('keybox_select_title')}</div>
        <div slot="content">
          <div id="keybox-slot-list" class="keybox-slot-list"></div>
        </div>
        <div slot="actions">
          <md-text-button id="cancel-keybox-slot">${i18n.t('functional_button_cancel')}</md-text-button>
        </div>
      </md-dialog>

      <md-dialog id="keybox-app-action-dialog">
        <div slot="headline">${i18n.t('keybox_app_actions_title')}</div>
        <div slot="content" class="keybox-app-action-content">
          <div id="keybox-app-package"></div>
          <md-filled-button id="select-keybox-action">${i18n.t('keybox_select_action')}</md-filled-button>
        </div>
        <div slot="actions">
          <md-text-button id="cancel-keybox-action">${i18n.t('functional_button_cancel')}</md-text-button>
        </div>
      </md-dialog>

      <md-dialog id="keybox-overwrite-dialog" type="alert">
        <div slot="headline">${i18n.t('keybox_overwrite_title')}</div>
        <div slot="content" id="keybox-overwrite-message"></div>
        <div slot="actions">
          <md-outlined-button id="cancel-keybox-overwrite">${i18n.t('functional_button_cancel')}</md-outlined-button>
          <md-filled-button id="confirm-keybox-overwrite">${i18n.t('functional_button_confirm')}</md-filled-button>
        </div>
      </md-dialog>
    `

    const fragment = template.content
    this.#slotDialog = fragment.querySelector<MdDialog>('#keybox-slot-dialog')
    this.#slotList = fragment.querySelector<HTMLElement>('#keybox-slot-list')
    this.#slotTitle = fragment.querySelector<HTMLElement>('#keybox-slot-title')
    fragment.querySelector<MdTextButton>('#cancel-keybox-slot')!.onclick = () => {
      this.#finishSlotSelection(null)
    }
    this.#slotDialog?.addEventListener('closed', () => {
      if (this.#slotResolve) this.#finishSlotSelection(null)
    })
    this.#appActionDialog = fragment.querySelector<MdDialog>('#keybox-app-action-dialog')
    this.#appActionPackage = fragment.querySelector<HTMLElement>('#keybox-app-package')
    fragment.querySelector<MdFilledButton>('#select-keybox-action')!.onclick = () => {
      const packageName = this.#pendingPackageName
      if (!packageName) return
      this.#appActionSelecting = true
      this.#appActionDialog?.close()
      void this.selectKeyboxForApp(packageName).then((selected) => {
        this.#appActionSelecting = false
        this.#finishAppAction(selected)
      }).catch(() => {
        this.#appActionSelecting = false
        this.#snackbar.show(i18n.t('prompt_keybox_slot_assign_error'), false)
        this.#finishAppAction(false)
      })
    }
    fragment.querySelector<MdTextButton>('#cancel-keybox-action')!.onclick = () => {
      this.#finishAppAction(false)
    }
    this.#appActionDialog?.addEventListener('closed', () => {
      if (!this.#appActionSelecting && this.#appActionResolve) this.#finishAppAction(false)
    })
    this.#overwriteDialog = fragment.querySelector<MdDialog>('#keybox-overwrite-dialog')
    this.#overwriteMessage = fragment.querySelector<HTMLElement>('#keybox-overwrite-message')
    fragment.querySelector<HTMLElement>('#cancel-keybox-overwrite')!.onclick = () => {
      this.#finishOverwrite(false)
    }
    fragment.querySelector<MdFilledButton>('#confirm-keybox-overwrite')!.onclick = () => {
      this.#finishOverwrite(true)
    }
    this.#overwriteDialog?.addEventListener('closed', () => {
      if (this.#overwriteResolve) this.#finishOverwrite(false)
    })
    this.custom.bind(fragment)
    return fragment
  }

  async setKeybox(content: string, cmd: string = 'cat', slot?: number): Promise<KeyboxSaveResult> {
    const selectedSlot = slot ?? await this.#chooseSlot()
    if (selectedSlot === null) return 'cancelled'
    if (!Number.isInteger(selectedSlot) || selectedSlot < 0 || selectedSlot > MAX_KEYBOX_SLOT) {
      return 'error'
    }

    const destination = this.getKeyboxPath(selectedSlot)
    if (await File.exist(destination)) {
      const label = this.#slotLabel(selectedSlot)
      if (!await this.#confirmOverwrite(label)) return 'cancelled'
      try {
        await File.copy(destination, `${destination}.bak`)
      } catch {
        return 'error'
      }
    }

    try {
      await File.write(destination, content, cmd)
      await File.secure(destination)
      return 'saved'
    } catch {
      return 'error'
    }
  }

  async setAospKey(): Promise<void> {
    try {
      const content = await this.cli.getAospKey()
      const result = await this.setKeybox(content)
      if (result !== 'cancelled') {
        this.#snackbar.show(i18n.t(result === 'saved' ? 'prompt_aosp_key_set' : 'prompt_key_set_error'), result === 'saved')
      }
    } catch {
      this.#snackbar.show(i18n.t('prompt_key_set_error'), false)
    }
  }

  async setUnknownKey(): Promise<void> {
    try {
      const keyboxContent = await generateUnknownKeybox()
      const result = await this.setKeybox(keyboxContent)
      if (result !== 'cancelled') {
        this.#snackbar.show(i18n.t(result === 'saved' ? 'prompt_unknown_key_set' : 'prompt_key_set_error'), result === 'saved')
      }
    } catch (error) {
      console.error(error)
      this.#snackbar.show(i18n.t('prompt_key_set_error'), false)
    }
  }

  async setLocalKey(): Promise<void> {
    try {
      const content = await this.#fileSelector.getFileContent('xml')
      if (!content) return
      const result = await this.setKeybox(content)
      if (result !== 'cancelled') {
        this.#snackbar.show(i18n.t(result === 'saved' ? 'prompt_custom_key_set' : 'prompt_key_set_error'), result === 'saved')
      }
    } catch {
      this.#snackbar.show(i18n.t('prompt_key_set_error'), false)
    }
  }

  static isKeygenAvailable(): boolean {
    return isKeygenAvailable()
  }

  async selectKeyboxForApp(packageName: string): Promise<boolean> {
    const currentSlot = this.#config.getKeyboxSlot(packageName)
    const selectedSlot = await this.#chooseSlot(currentSlot, 'keybox_select_title')
    if (selectedSlot === null) return false

    this.#config.setKeyboxSlot(packageName, selectedSlot)
    try {
      if (!import.meta.env.DEV) await this.#config.write()
    } catch {
      this.#config.setKeyboxSlot(packageName, currentSlot)
      this.#snackbar.show(i18n.t('prompt_keybox_slot_assign_error'), false)
      return false
    }
    this.#snackbar.show(i18n.t('prompt_keybox_slot_assigned', packageName, this.#slotLabel(selectedSlot)), true)
    return true
  }

  async showAppKeyboxMenu(packageName: string): Promise<boolean> {
    if (!this.#appActionDialog) return false
    this.#pendingPackageName = packageName
    if (this.#appActionPackage) this.#appActionPackage.textContent = packageName
    return new Promise((resolve) => {
      this.#appActionResolve = resolve
      this.#appActionDialog?.show()
    })
  }

  #slotLabel(slot: number): string {
    return slot === 0
      ? i18n.t('keybox_slot_default')
      : i18n.t('keybox_slot_label', slot)
  }

  async #chooseSlot(currentSlot?: number, titleKey: string = 'keybox_save_title'): Promise<number | null> {
    if (!this.#slotDialog || !this.#slotList) return null

    const slots: number[] = await this.cli.getKeyboxSlots(this.#config.configPath).catch((): number[] => [])
    if (currentSlot && currentSlot > 0 && !slots.includes(currentSlot)) slots.push(currentSlot)
    slots.sort((a, b) => a - b)

    this.#slotTitle!.textContent = i18n.t(titleKey)
    this.#slotList.innerHTML = ''
    this.#appendSlotOption(0, this.#slotLabel(0))
    for (const slot of slots) this.#appendSlotOption(slot, this.#slotLabel(slot))

    const newSlot = document.createElement('md-outlined-button')
    newSlot.className = 'keybox-slot-new'
    newSlot.textContent = i18n.t('keybox_slot_new')
    const newSlotForm = document.createElement('div')
    newSlotForm.className = 'keybox-new-slot-form hidden'
    const newSlotInput = document.createElement('md-outlined-text-field') as MdOutlinedTextField
    newSlotInput.type = 'number'
    newSlotInput.label = i18n.t('keybox_slot_number')
    newSlotInput.setAttribute('min', '1')
    newSlotInput.setAttribute('max', String(MAX_KEYBOX_SLOT))
    const createSlot = document.createElement('md-filled-button')
    createSlot.textContent = i18n.t('keybox_slot_create')
    newSlot.onclick = () => {
      const suggested = slots.length > 0 ? Math.min(MAX_KEYBOX_SLOT, slots[slots.length - 1] + 1) : 1
      newSlotInput.value = String(suggested)
      newSlotForm.classList.remove('hidden')
      newSlotInput.focus()
    }
    createSlot.onclick = () => {
      const raw = newSlotInput.value.trim()
      const slot = /^\d+$/.test(raw) ? Number.parseInt(raw, 10) : 0
      if (slot < 1 || slot > MAX_KEYBOX_SLOT) {
        this.#snackbar.show(i18n.t('keybox_slot_invalid'), false)
        return
      }
      this.#finishSlotSelection(slot)
    }
    newSlotForm.append(newSlotInput, createSlot)
    this.#slotList.appendChild(newSlot)
    this.#slotList.appendChild(newSlotForm)

    return new Promise((resolve) => {
      this.#slotResolve = resolve
      this.#slotDialog?.show()
    })
  }

  #appendSlotOption(slot: number, label: string): void {
    if (!this.#slotList) return
    const option = document.createElement('md-outlined-button')
    option.className = 'keybox-slot-option'
    option.textContent = label
    option.onclick = () => this.#finishSlotSelection(slot)
    this.#slotList.appendChild(option)
  }

  #finishSlotSelection(slot: number | null): void {
    const resolve = this.#slotResolve
    this.#slotResolve = null
    this.#slotDialog?.close()
    resolve?.(slot)
  }

  #finishAppAction(selected: boolean): void {
    const resolve = this.#appActionResolve
    this.#appActionResolve = null
    this.#pendingPackageName = null
    this.#appActionDialog?.close()
    resolve?.(selected)
  }

  #confirmOverwrite(label: string): Promise<boolean> {
    if (!this.#overwriteDialog || !this.#overwriteMessage) return Promise.resolve(false)
    this.#overwriteMessage.textContent = i18n.t('keybox_overwrite_confirm', label)
    return new Promise((resolve) => {
      this.#overwriteResolve = resolve
      this.#overwriteDialog?.show()
    })
  }

  #finishOverwrite(overwrite: boolean): void {
    const resolve = this.#overwriteResolve
    this.#overwriteResolve = null
    this.#overwriteDialog?.close()
    resolve?.(overwrite)
  }
}
