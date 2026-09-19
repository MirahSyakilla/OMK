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
const SLOT_NAMES_FILE = 'keybox-slots.json'

function algorithmsFromXml(xml: string): string[] {
  const found: string[] = []
  const ecCount = [...xml.matchAll(/algorithm\s*=\s*"ecdsa"/gi)].length
  if (/algorithm\s*=\s*"rsa"/i.test(xml)) found.push('RSA')
  if (ecCount > 0) found.push('EC')
  if (looksLikeRkp(xml, ecCount)) found.push('RKP')
  return found
}

function looksLikeRkp(xml: string, ecCount: number): boolean {
  if (ecCount >= 2) return true
  const certCounts = [...xml.matchAll(/<NumberOfCertificates>\s*(\d+)\s*<\/NumberOfCertificates>/gi)]
    .map((match) => Number.parseInt(match[1], 10))
  if (ecCount >= 1 && !/algorithm\s*=\s*"rsa"/i.test(xml) && certCounts.some((count) => count >= 4)) {
    return true
  }
  for (const match of xml.matchAll(/-----BEGIN CERTIFICATE-----([^-]+)-----END CERTIFICATE-----/g)) {
    try {
      const der = atob(match[1].replace(/\s+/g, ''))
      if (der.includes('Droid CA3') || der.includes('Key Attestation CA1')) return true
    } catch {
      // ignore malformed PEM
    }
  }
  return false
}

function sanitizeSlotName(name: string): string {
  return name.trim().replace(/\s+/g, ' ').slice(0, 40)
}

function exportFileName(name: string, slot: number): string {
  const base = sanitizeSlotName(name)
    .replace(/[^A-Za-z0-9._-]+/g, '_')
    .replace(/^_+|_+$/g, '')
    || `slot${slot}`
  return `keybox_${base}.xml`
}

export class Keybox {
  readonly cli: Cli
  readonly custom: CustomKeyboxProvider
  readonly #config: Config
  #fileSelector: FileSelector
  #snackbar: Snackbar
  #slotDialog: MdDialog | null = null
  #slotList: HTMLElement | null = null
  #slotTitle: HTMLElement | null = null
  #slotPackage: HTMLElement | null = null
  #slotResolve: ((slot: number | null) => void) | null = null
  #overwriteDialog: MdDialog | null = null
  #overwriteMessage: HTMLElement | null = null
  #overwriteResolve: ((overwrite: boolean) => void) | null = null
  #manageDialog: MdDialog | null = null
  #manageList: HTMLElement | null = null
  #renameDialog: MdDialog | null = null
  #renameInput: MdOutlinedTextField | null = null
  #renameSlot: number | null = null
  #deleteDialog: MdDialog | null = null
  #deleteMessage: HTMLElement | null = null
  #deleteSlot: number | null = null
  #slotNames: Record<string, string> = {}
  #pendingNewName = ''
  #onNamesChanged: (() => void) | null = null

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
          <div id="keybox-slot-package" class="keybox-package-chip hidden"></div>
          <div id="keybox-slot-list" class="keybox-slot-list"></div>
        </div>
        <div slot="actions">
          <md-text-button id="cancel-keybox-slot">${i18n.t('functional_button_cancel')}</md-text-button>
        </div>
      </md-dialog>

      <md-dialog id="keybox-manage-dialog">
        <div slot="headline">${i18n.t('keybox_manage_title')}</div>
        <div slot="content">
          <div id="keybox-manage-list" class="keybox-manage-list"></div>
        </div>
        <div slot="actions">
          <md-text-button id="close-keybox-manage">${i18n.t('functional_button_close')}</md-text-button>
        </div>
      </md-dialog>

      <md-dialog id="keybox-rename-dialog">
        <div slot="headline">${i18n.t('keybox_rename_title')}</div>
        <div slot="content">
          <md-outlined-text-field id="keybox-rename-input" label="${i18n.t('keybox_rename_label')}" maxlength="40"></md-outlined-text-field>
        </div>
        <div slot="actions">
          <md-outlined-button id="cancel-keybox-rename">${i18n.t('functional_button_cancel')}</md-outlined-button>
          <md-filled-button id="confirm-keybox-rename">${i18n.t('functional_button_save')}</md-filled-button>
        </div>
      </md-dialog>

      <md-dialog id="keybox-delete-dialog" type="alert">
        <div slot="headline">${i18n.t('keybox_delete_title')}</div>
        <div slot="content" id="keybox-delete-message"></div>
        <div slot="actions">
          <md-outlined-button id="cancel-keybox-delete">${i18n.t('functional_button_cancel')}</md-outlined-button>
          <md-filled-button id="confirm-keybox-delete">${i18n.t('functional_button_confirm')}</md-filled-button>
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
    this.#slotPackage = fragment.querySelector<HTMLElement>('#keybox-slot-package')
    fragment.querySelector<MdTextButton>('#cancel-keybox-slot')!.onclick = () => {
      this.#finishSlotSelection(null)
    }
    this.#slotDialog?.addEventListener('closed', () => {
      if (this.#slotResolve) this.#finishSlotSelection(null)
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
    this.#manageDialog = fragment.querySelector<MdDialog>('#keybox-manage-dialog')
    this.#manageList = fragment.querySelector<HTMLElement>('#keybox-manage-list')
    fragment.querySelector<MdTextButton>('#close-keybox-manage')!.onclick = () => {
      this.#manageDialog?.close()
    }
    this.#renameDialog = fragment.querySelector<MdDialog>('#keybox-rename-dialog')
    this.#renameInput = fragment.querySelector<MdOutlinedTextField>('#keybox-rename-input')
    fragment.querySelector<HTMLElement>('#cancel-keybox-rename')!.onclick = () => {
      this.#renameSlot = null
      this.#renameDialog?.close()
    }
    fragment.querySelector<MdFilledButton>('#confirm-keybox-rename')!.onclick = () => {
      void this.#confirmRename()
    }
    this.#deleteDialog = fragment.querySelector<MdDialog>('#keybox-delete-dialog')
    this.#deleteMessage = fragment.querySelector<HTMLElement>('#keybox-delete-message')
    fragment.querySelector<HTMLElement>('#cancel-keybox-delete')!.onclick = () => {
      this.#deleteSlot = null
      this.#deleteDialog?.close()
    }
    fragment.querySelector<MdFilledButton>('#confirm-keybox-delete')!.onclick = () => {
      void this.#confirmDelete()
    }
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
      if (!await this.#confirmOverwrite(label)) {
        this.#pendingNewName = ''
        return 'cancelled'
      }
      try {
        await File.copy(destination, `${destination}.bak`)
      } catch {
        return 'error'
      }
    }

    try {
      await File.write(destination, content, cmd)
      await File.secure(destination)
      if (this.#pendingNewName && selectedSlot > 0) {
        await this.#setSlotName(selectedSlot, this.#pendingNewName)
      }
      this.#pendingNewName = ''
      return 'saved'
    } catch {
      this.#pendingNewName = ''
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

  async setAlwaysStrongKey(): Promise<void> {
    try {
      const payload = await this.cli.getAlwaysStrongKey()
      const result = await this.setKeybox(payload, 'base64 -d')
      if (result !== 'cancelled') {
        this.#snackbar.show(
          i18n.t(result === 'saved' ? 'prompt_alwaysstrong_key_set' : 'prompt_alwaysstrong_key_set_error'),
          result === 'saved',
        )
      }
    } catch (error) {
      console.error(error)
      this.#snackbar.show(i18n.t('prompt_alwaysstrong_key_set_error'), false)
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
    const selectedSlot = await this.#chooseSlot(currentSlot, 'keybox_select_title', packageName)
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
    return this.selectKeyboxForApp(packageName)
  }

  async showManage(): Promise<void> {
    if (!this.#manageDialog || !this.#manageList) return
    await this.#renderManageList()
    this.#manageDialog.show()
  }

  slotLabel(slot: number): string {
    const named = this.#slotNames[String(slot)]
    if (named) return named
    return slot === 0
      ? i18n.t('keybox_slot_default')
      : i18n.t('keybox_slot_label', slot)
  }

  async loadSlotNames(): Promise<void> {
    await this.#loadSlotNames()
  }

  onNamesChanged(handler: () => void): void {
    this.#onNamesChanged = handler
  }

  #slotLabel(slot: number): string {
    return this.slotLabel(slot)
  }

  #fileName(slot: number): string {
    return slot > 0 ? `keybox-slot-${slot}.xml` : 'keybox.xml'
  }

  async #chooseSlot(currentSlot?: number, titleKey: string = 'keybox_save_title', packageName?: string): Promise<number | null> {
    if (!this.#slotDialog || !this.#slotList) return null
    this.#pendingNewName = ''
    await this.#loadSlotNames()

    const slots: number[] = await this.cli.getKeyboxSlots(this.#config.configPath).catch((): number[] => [])
    if (currentSlot && currentSlot > 0 && !slots.includes(currentSlot)) slots.push(currentSlot)
    slots.sort((a, b) => a - b)

    this.#slotTitle!.textContent = i18n.t(titleKey)
    if (this.#slotPackage) {
      if (packageName) {
        this.#slotPackage.textContent = packageName
        this.#slotPackage.classList.remove('hidden')
      } else {
        this.#slotPackage.textContent = ''
        this.#slotPackage.classList.add('hidden')
      }
    }
    this.#slotList.innerHTML = ''
    this.#appendSlotOption(0, currentSlot === 0)
    for (const slot of slots) this.#appendSlotOption(slot, currentSlot === slot)

    if (!packageName) {
      const newSlot = document.createElement('button')
      newSlot.type = 'button'
      newSlot.className = 'keybox-pill new'
      newSlot.textContent = i18n.t('keybox_slot_new')
      const newSlotForm = document.createElement('div')
      newSlotForm.className = 'keybox-new-slot-form hidden'
      const nameInput = document.createElement('md-outlined-text-field') as MdOutlinedTextField
      nameInput.label = i18n.t('keybox_rename_label')
      nameInput.setAttribute('maxlength', '40')
      const numberInput = document.createElement('md-outlined-text-field') as MdOutlinedTextField
      numberInput.type = 'number'
      numberInput.label = i18n.t('keybox_slot_number')
      numberInput.setAttribute('min', '1')
      numberInput.setAttribute('max', String(MAX_KEYBOX_SLOT))
      const createSlot = document.createElement('md-filled-button')
      createSlot.textContent = i18n.t('keybox_slot_create')
      newSlot.onclick = () => {
        const suggested = slots.length > 0 ? Math.min(MAX_KEYBOX_SLOT, slots[slots.length - 1] + 1) : 1
        numberInput.value = String(suggested)
        if (!nameInput.value) nameInput.value = i18n.t('keybox_slot_label', suggested)
        newSlotForm.classList.remove('hidden')
        nameInput.focus()
      }
      createSlot.onclick = () => {
        const raw = numberInput.value.trim()
        const slot = /^\d+$/.test(raw) ? Number.parseInt(raw, 10) : 0
        if (slot < 1 || slot > MAX_KEYBOX_SLOT) {
          this.#snackbar.show(i18n.t('keybox_slot_invalid'), false)
          return
        }
        this.#pendingNewName = sanitizeSlotName(nameInput.value)
        this.#finishSlotSelection(slot)
      }
      newSlotForm.append(nameInput, numberInput, createSlot)
      this.#slotList.appendChild(newSlot)
      this.#slotList.appendChild(newSlotForm)
    }

    return new Promise((resolve) => {
      this.#slotResolve = resolve
      this.#slotDialog?.show()
    })
  }

  #appendSlotOption(slot: number, selected: boolean): void {
    if (!this.#slotList) return
    const option = document.createElement('button')
    option.type = 'button'
    option.className = selected ? 'keybox-pill selected' : 'keybox-pill'
    const name = document.createElement('span')
    name.className = 'keybox-pill-name'
    name.textContent = this.#slotLabel(slot)
    const id = document.createElement('span')
    id.className = 'keybox-pill-id'
    id.textContent = slot === 0 ? 'keybox.xml' : i18n.t('keybox_slot_short', slot)
    option.append(name, id)
    option.onclick = () => this.#finishSlotSelection(slot)
    this.#slotList.appendChild(option)
  }

  #finishSlotSelection(slot: number | null): void {
    const resolve = this.#slotResolve
    this.#slotResolve = null
    this.#slotDialog?.close()
    resolve?.(slot)
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

  #namesPath(): string {
    return `${this.#config.configPath}/data/${SLOT_NAMES_FILE}`
  }

  async #loadSlotNames(): Promise<void> {
    if (import.meta.env.DEV) {
      this.#slotNames = { '1': 'Play', '2': 'Bank' }
      return
    }
    try {
      const raw = JSON.parse(await File.read(this.#namesPath())) as { names?: Record<string, string> }
      this.#slotNames = raw.names && typeof raw.names === 'object' ? raw.names : {}
    } catch {
      this.#slotNames = {}
    }
  }

  async #saveSlotNames(): Promise<void> {
    if (import.meta.env.DEV) return
    await File.createDirectory(`${this.#config.configPath}/data`)
    await File.write(this.#namesPath(), JSON.stringify({ names: this.#slotNames }, null, 2))
  }

  async #setSlotName(slot: number, name: string): Promise<void> {
    const cleaned = sanitizeSlotName(name)
    if (!cleaned) {
      delete this.#slotNames[String(slot)]
    } else {
      this.#slotNames[String(slot)] = cleaned
    }
    await this.#saveSlotNames()
    this.#onNamesChanged?.()
  }

  async #renderManageList(): Promise<void> {
    if (!this.#manageList) return
    await this.#loadSlotNames()
    const slots = [0, ...await this.cli.getKeyboxSlots(this.#config.configPath).catch((): number[] => [])]
    this.#manageList.innerHTML = ''
    for (const slot of slots) {
      const path = this.getKeyboxPath(slot)
      const [mtime, xml] = await Promise.all([
        this.cli.getFileMtime(path),
        File.read(path).catch(() => ''),
      ])
      const card = document.createElement('div')
      card.className = 'keybox-manage-card'

      const head = document.createElement('div')
      head.className = 'keybox-manage-head'
      const name = document.createElement('div')
      name.className = 'keybox-manage-name'
      name.textContent = this.#slotLabel(slot)
      const algos = document.createElement('div')
      algos.className = 'keybox-manage-algos'
      for (const algo of algorithmsFromXml(xml)) {
        const pill = document.createElement('span')
        pill.className = 'keybox-algo-pill'
        pill.textContent = algo
        algos.appendChild(pill)
      }
      if (algos.childElementCount === 0) {
        const pill = document.createElement('span')
        pill.className = 'keybox-algo-pill'
        pill.textContent = i18n.t('keybox_algo_unknown')
        algos.appendChild(pill)
      }
      head.append(name, algos)

      const file = document.createElement('div')
      file.className = 'keybox-manage-file'
      file.textContent = this.#fileName(slot)

      const meta = document.createElement('div')
      meta.className = 'keybox-manage-meta'
      const created = document.createElement('span')
      created.className = 'keybox-meta-pill'
      created.textContent = mtime
        ? i18n.t('keybox_created', new Date(mtime).toLocaleDateString())
        : i18n.t('keybox_created_unknown')
      const apps = document.createElement('span')
      apps.className = 'keybox-meta-pill'
      apps.textContent = slot === 0
        ? i18n.t('keybox_assigned_default')
        : i18n.t('keybox_assigned_apps', this.#config.packagesForSlot(slot).length)
      meta.append(created, apps)

      const actions = document.createElement('div')
      actions.className = 'keybox-manage-actions'
      actions.append(
        this.#actionButton(i18n.t('keybox_action_rename'), () => this.#openRename(slot)),
        this.#actionButton(i18n.t('keybox_action_export'), () => {
          void this.#exportSlot(slot)
        }),
      )
      if (slot > 0) {
        actions.append(this.#actionButton(i18n.t('keybox_action_delete'), () => this.#openDelete(slot), true))
      }

      card.append(head, file, meta, actions)
      this.#manageList.appendChild(card)
    }
  }

  #actionButton(label: string, onClick: () => void, danger = false): HTMLElement {
    const button = document.createElement(danger ? 'md-outlined-button' : 'md-filled-tonal-button')
    button.className = 'keybox-action-pill'
    button.textContent = label
    button.addEventListener('click', onClick)
    return button
  }

  #openRename(slot: number): void {
    this.#renameSlot = slot
    if (this.#renameInput) this.#renameInput.value = this.#slotLabel(slot)
    this.#renameDialog?.show()
  }

  async #confirmRename(): Promise<void> {
    const slot = this.#renameSlot
    this.#renameSlot = null
    this.#renameDialog?.close()
    if (slot === null) return
    const name = sanitizeSlotName(this.#renameInput?.value ?? '')
    if (!name) {
      this.#snackbar.show(i18n.t('keybox_rename_invalid'), false)
      return
    }
    try {
      await this.#setSlotName(slot, name)
      await this.#renderManageList()
      this.#snackbar.show(i18n.t('prompt_keybox_renamed'), true)
    } catch {
      this.#snackbar.show(i18n.t('prompt_keybox_rename_error'), false)
    }
  }

  #openDelete(slot: number): void {
    this.#deleteSlot = slot
    if (this.#deleteMessage) {
      this.#deleteMessage.textContent = i18n.t('keybox_delete_confirm', this.#slotLabel(slot))
    }
    this.#deleteDialog?.show()
  }

  async #confirmDelete(): Promise<void> {
    const slot = this.#deleteSlot
    this.#deleteSlot = null
    this.#deleteDialog?.close()
    if (slot === null || slot <= 0) return
    try {
      const path = this.getKeyboxPath(slot)
      if (await File.exist(path)) await File.delete(path)
      if (await File.exist(`${path}.bak`)) await File.delete(`${path}.bak`)
      delete this.#slotNames[String(slot)]
      await this.#saveSlotNames()
      this.#onNamesChanged?.()
      this.#config.clearKeyboxSlot(slot)
      if (!import.meta.env.DEV) await this.#config.write()
      await this.#renderManageList()
      this.#snackbar.show(i18n.t('prompt_keybox_deleted'), true)
    } catch {
      this.#snackbar.show(i18n.t('prompt_keybox_delete_error'), false)
    }
  }

  async #exportSlot(slot: number): Promise<void> {
    try {
      const dest = await this.cli.exportKeybox(
        this.getKeyboxPath(slot),
        exportFileName(this.#slotLabel(slot), slot),
      )
      this.#snackbar.show(i18n.t('prompt_keybox_exported', dest), true)
    } catch {
      this.#snackbar.show(i18n.t('prompt_keybox_export_error'), false)
    }
  }
}
