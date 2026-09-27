import type { MdDialog, MdFilledButton, MdOutlinedButton, MdIconButton } from '@material/web/all'
import { PolicyEditor } from '../app_list/policy'
import { Config, type SectionKey } from '../config'
import { i18n } from '../i18n'
import { Snackbar } from '../snackbar/snackbar'
import { applyDialogAnimation } from './animation'

export interface SectionDialogOptions {
  fullscreen?: boolean
  snackbar?: Snackbar
}

export class SectionDialog {
  #dialog: MdDialog | null = null
  #policyEditor: PolicyEditor | null = null
  #config: Config
  #section: Exclude<SectionKey, 'trust_record'>
  #dialogId: string
  #fullscreen: boolean
  #snackbar: Snackbar | null

  constructor(
    config: Config,
    section: Exclude<SectionKey, 'trust_record'>,
    dialogId: string,
    options?: SectionDialogOptions,
  ) {
    this.#config = config
    this.#section = section
    this.#dialogId = dialogId
    this.#fullscreen = options?.fullscreen ?? false
    this.#snackbar = options?.snackbar ?? null
  }
  getElement(): DocumentFragment {
    const template = document.createElement('template')
    if (this.#fullscreen) {
      template.innerHTML = /* html */ `
        <md-dialog id="${this.#dialogId}" class="dialog--fullscreen">
          <div slot="headline" class="fullscreen-dialog-header">
            <md-icon-button id="${this.#dialogId}-back" class="fs-back-btn" aria-label="${i18n.t('functional_button_cancel')}">
              <md-icon>arrow_back</md-icon>
            </md-icon-button>
            <div class="fs-title-group">
              <h2 class="fs-title">${this.#config.getSectionTitle(this.#section)}</h2>
              <span class="fs-subtitle">${this.#getSubtitle()}</span>
            </div>
            <md-filled-button id="${this.#dialogId}-save-top" class="fs-save-btn">
              <md-icon slot="icon">check</md-icon>
              <span>${i18n.t('functional_button_save')}</span>
            </md-filled-button>
          </div>
          <div slot="content" class="fullscreen-dialog-content">
            <div class="policy-fields fs-fields" id="${this.#dialogId}-fields">
              ${this.#renderFields()}
            </div>
          </div>
        </md-dialog>
      `
    } else {
      template.innerHTML = /* html */ `
        <md-dialog id="${this.#dialogId}">
          <div slot="headline">${this.#config.getSectionTitle(this.#section)}</div>
          <div slot="content">
            <div class="policy-fields" id="${this.#dialogId}-fields">
              ${PolicyEditor.html(this.#config.getSectionSchema(this.#section))}
            </div>
          </div>
          <div slot="actions">
            <md-outlined-button id="${this.#dialogId}-close">${i18n.t('functional_button_cancel')}</md-outlined-button>
            <md-filled-button id="${this.#dialogId}-save">${i18n.t('functional_button_save')}</md-filled-button>
          </div>
        </md-dialog>
      `
    }
    const fragment = template.content
    this.#dialog = fragment.querySelector<MdDialog>(`#${this.#dialogId}`)

    const fieldsContainer = fragment.querySelector<HTMLElement>(`#${this.#dialogId}-fields`)!    
    this.#policyEditor = new PolicyEditor(fieldsContainer, this.#config.getSectionSchema(this.#section))
    this.#policyEditor.bind()

    if (this.#fullscreen) {
      fragment.querySelector<MdIconButton>(`#${this.#dialogId}-back`)!.onclick = () => this.close()
      fragment.querySelector<MdFilledButton>(`#${this.#dialogId}-save-top`)!.onclick = () => {
        void this.#save()
      }
    } else {
      fragment.querySelector<MdOutlinedButton>(`#${this.#dialogId}-close`)!.onclick = () => this.close()
      fragment.querySelector<MdFilledButton>(`#${this.#dialogId}-save`)!.onclick = () => {
        void this.#save()
      }
    }
    return fragment
  }

  initAnimation(): void {
    if (this.#dialog) applyDialogAnimation(this.#dialog)
  }

  show(): void {
    this.#policyEditor?.setPolicy((this.#config.get(this.#section) as Record<string, string | boolean>) ?? null)
    this.#dialog?.show()
  }

  close(): void {
    this.#dialog?.close()
  }

  async #save(): Promise<void> {
    if (!this.#policyEditor?.isValid()) return
    const policy = this.#policyEditor.getPolicy(true)
    if (!policy) return
    this.#config.set(this.#section, policy)
    await this.#config.write()
    this.#snackbar?.show(i18n.t('prompt_saved_target'))
    this.close()
  }

  #getSubtitle(): string {
    if (this.#section === 'device') {
      return 'Spoof brand, model, serial, and telephony IDs'
    }
    if (this.#section === 'crypto') {
      return 'Hardware-backed root derivation seeds & HMAC keys'
    }
    return ''
  }

  #renderFields(): string {
    if (this.#section === 'device') {
      return [
        this.#renderCard('Device Identity', 'Basic hardware and build properties', 'devices', [
          'brand',
          'device',
          'product',
          'manufacturer',
          'model',
          'serial',
        ]),
        this.#renderCard('Telephony Identifiers', 'IMEI and MEID overrides for attestation', 'cell_tower', [
          'overrideTelephonyProperties',
          'meid',
          'imei',
          'imei2',
        ]),
      ].join('\n')
    }

    if (this.#section === 'crypto') {
      return [
        this.#renderCard('Master Seeds', 'Hardware-backed root derivation seeds (64 hex characters)', 'key', [
          'root_kek_seed',
          'kak_seed',
          'shared_secret_seed',
        ]),
        this.#renderCard('Authentication & Nonce', 'Auth token HMAC key and negotiation nonce', 'security', [
          'shared_secret_nonce',
          'auth_token_hmac_key',
        ]),
      ].join('\n')
    }

    return PolicyEditor.html(this.#config.getSectionSchema(this.#section))
  }

  #renderCard(title: string, subtitle: string, icon: string, fieldKeys: string[]): string {
    const schema = this.#config.getSectionSchema(this.#section)
    const fieldsHtml = fieldKeys
      .map((key) => {
        const meta = schema.getField(key)
        return meta ? PolicyEditor.fieldHtml(key, meta) : ''
      })
      .filter(Boolean)
      .join('\n')

    return `
      <div class="fs-card">
        <div class="fs-card-header">
          <md-icon class="fs-card-icon">${icon}</md-icon>
          <div class="fs-card-title-group">
            <div class="fs-card-title">${title}</div>
            <div class="fs-card-subtitle">${subtitle}</div>
          </div>
        </div>
        <div class="fs-card-fields">
          ${fieldsHtml}
        </div>
      </div>
    `
  }
}
