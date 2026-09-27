import type { MdDialog, MdFilledButton, MdOutlinedButton, MdIconButton, MdSwitch } from '@material/web/all'
import { PolicyEditor } from '../app_list/policy'
import { Config, type SectionKey, type PolicyFieldMeta, type TextFieldMeta, snakeToLabel, INTERCEPT_KEYS } from '../config'
import { i18n } from '../i18n'
import { Snackbar } from '../snackbar/snackbar'
import { History } from '../history'
import { applyDialogAnimation } from './animation'

export interface SectionDialogOptions {
  fullscreen?: boolean
  snackbar?: Snackbar
  history?: History
  onSaved?: () => void
}

const FIELD_DESCRIPTIONS: Record<string, string> = {
  overrideTelephonyProperties: 'Override telephony identifiers for attestation',
  verified_boot_state: 'Emulate verified boot green state',
  device_locked: 'Emulate locked bootloader state',
  get_security_level: 'Query hardware security level for key operations',
  get_key_entry: 'Fetch key characteristics and public certificates',
  get_number_of_entries: 'Count stored key entries for caller',
  get_supplementary_attestation_info: 'Telephony and device attestation identifier tags',
  list_entries: 'Enumerate keystore key descriptors and aliases',
  list_entries_batched: 'Batched iteration for large key lists',
  update_subcomponent: 'Update key certificate chain components',
  delete_key: 'Delete key blob from hardware storage',
  grant: 'Grant key access to another application UID',
  ungrant: 'Revoke granted key permissions from UID',
}

export class SectionDialog {
  #dialog: MdDialog | null = null
  #confirmDialog: MdDialog | null = null
  #policyEditor: PolicyEditor | null = null
  #config: Config
  #section: Exclude<SectionKey, 'trust_record'>
  #dialogId: string
  #fullscreen: boolean
  #snackbar: Snackbar | null
  #history: History | null
  #initialSnapshot = ''
  #isDirty = false
  #saveBtn: HTMLElement | null = null
  #onSaved: (() => void) | null = null

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
    this.#history = options?.history ?? null
    this.#onSaved = options?.onSaved ?? null
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
            <md-filled-button id="${this.#dialogId}-save-top" class="fs-save-btn" disabled>
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

        <md-dialog id="${this.#dialogId}-confirm" type="alert">
          <div slot="headline">Save changes?</div>
          <div slot="content">You have unsaved changes.</div>
          <div slot="actions">
            <md-text-button id="${this.#dialogId}-confirm-discard">Discard</md-text-button>
            <md-filled-button id="${this.#dialogId}-confirm-save">Save</md-filled-button>
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
    this.#confirmDialog = fragment.querySelector<MdDialog>(`#${this.#dialogId}-confirm`)

    const fieldsContainer = fragment.querySelector<HTMLElement>(`#${this.#dialogId}-fields`)!
    this.#policyEditor = new PolicyEditor(fieldsContainer, this.#config.getSectionSchema(this.#section))
    this.#policyEditor.bind()

    if (this.#fullscreen) {
      this.#saveBtn = fragment.querySelector<HTMLElement>(`#${this.#dialogId}-save-top`)
      fragment.querySelector<MdIconButton>(`#${this.#dialogId}-back`)!.onclick = () => this.#handleBackRequest()
      if (this.#saveBtn) {
        this.#saveBtn.onclick = () => {
          void this.#save()
        }
      }

      // Attach custom requestClose hook on dialog element for Android backstack integration
      if (this.#dialog) {
        Object.assign(this.#dialog, {
          requestClose: () => this.#handleBackRequest(),
        })
      }

      // Confirm dialog action bindings
      const discardBtn = fragment.querySelector<HTMLElement>(`#${this.#dialogId}-confirm-discard`)
      if (discardBtn) {
        discardBtn.onclick = () => {
          this.#isDirty = false
          this.#confirmDialog?.close()
          if (this.#initialSnapshot) {
            try {
              this.#policyEditor?.setPolicy(JSON.parse(this.#initialSnapshot))
            } catch {
              // Ignore parse error
            }
          }
          this.close()
        }
      }

      const confirmSaveBtn = fragment.querySelector<HTMLElement>(`#${this.#dialogId}-confirm-save`)
      if (confirmSaveBtn) {
        confirmSaveBtn.onclick = () => {
          this.#confirmDialog?.close()
          void this.#save()
        }
      }

      // Re-arm history stack if confirm dialog is dismissed without discarding/saving
      this.#confirmDialog?.addEventListener('closed', () => {
        if (this.#dialog?.open && this.#isDirty) {
          this.#history?.push(this.#dialogId, () => this.#handleBackRequest())
        }
      })

      // Click-to-toggle for switch rows with ripple feedback
      fieldsContainer.querySelectorAll<HTMLElement>('.switch-row').forEach((row) => {
        row.addEventListener('click', (e) => {
          if (e.composedPath().some((n) => n instanceof Element && n.localName === 'md-switch')) return
          const sw = row.querySelector<MdSwitch>('md-switch')
          if (sw) {
            sw.selected = !sw.selected
            sw.dispatchEvent(new Event('change', { bubbles: true }))
          }
        })
      })

      // Dirty checking listener on all fields
      const checkDirty = () => {
        const current = this.#policyEditor?.getPolicy(false) ?? {}
        this.#isDirty = JSON.stringify(current) !== this.#initialSnapshot
        this.#updateSaveBtnState()
        this.#updateInterceptSubtitle()
      }
      fieldsContainer.addEventListener('input', checkDirty)
      fieldsContainer.addEventListener('change', checkDirty)
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
    if (this.#confirmDialog) applyDialogAnimation(this.#confirmDialog)
  }

  show(): void {
    const policy = (this.#config.get(this.#section) as Record<string, string | boolean>) ?? null
    this.#policyEditor?.setPolicy(policy)
    this.#initialSnapshot = JSON.stringify(this.#policyEditor?.getPolicy(false) ?? {})
    this.#isDirty = false
    this.#updateSaveBtnState()
    this.#updateInterceptSubtitle()
    this.#dialog?.show()
  }
  close(): void {
    this.#dialog?.close()
  }

  #handleBackRequest(): void {
    if (!this.#isDirty) {
      this.close()
      return
    }
    this.#confirmDialog?.show()
  }

  #updateSaveBtnState(): void {
    if (!this.#fullscreen || !this.#saveBtn) return
    const isValid = this.#policyEditor?.isValid() ?? true
    this.#saveBtn.toggleAttribute('disabled', !this.#isDirty || !isValid)
  }

  async #save(): Promise<void> {
    if (!this.#policyEditor?.isValid()) return
    const policy = this.#policyEditor.getPolicy(true)
    if (!policy) return
    this.#config.set(this.#section, policy)
    try {
      await this.#config.write()
    } catch (error) {
      // The section has already been replaced in memory, so the dialog stays
      // open with the edit intact and the user is told, rather than seeing Save
      // do nothing and believing it worked.
      this.#snackbar?.show(
        `Failed to save: ${error instanceof Error ? error.message : String(error)}`,
        false,
      )
      return
    }
    this.#initialSnapshot = JSON.stringify(this.#policyEditor.getPolicy(false) ?? {})
    this.#isDirty = false
    this.#updateSaveBtnState()
    this.#updateInterceptSubtitle()
    this.#onSaved?.()
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
    if (this.#section === 'trust') {
      return 'Attestation values, boot hashes, and lock states'
    }
    if (this.#section === 'intercept') {
      return this.#getInterceptSubtitle()
    }
    return ''
  }
  #getInterceptSubtitle(): string {
    // allowEmpty=true, because a policy where every switch is off is a real
    // state, not an absent one. With false, an all-off editor returns null and
    // the count falls back to the unsaved config, showing the pre-edit value
    // exactly when the user has just turned everything off.
    const policy = this.#policyEditor?.getPolicy(true) ?? {}
    let count = 0
    for (const key of INTERCEPT_KEYS) {
      if (policy[key] !== false && policy[key] !== 'false') count++
    }
    return `${count}/${INTERCEPT_KEYS.length} features enabled`
  }

  #updateInterceptSubtitle(): void {
    if (this.#section !== 'intercept' || !this.#dialog) return
    const subtitleEl = this.#dialog.querySelector<HTMLElement>('.fs-subtitle')
    if (subtitleEl) {
      subtitleEl.textContent = this.#getInterceptSubtitle()
    }
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
        this.#renderCard('Master Seeds', 'Hardware-backed root derivation seeds (64 hex characters)', 'vpn_key', [
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

    if (this.#section === 'trust') {
      return [
        this.#renderCard('Attestation Values', 'OS version and build security patch level', 'verified_user', [
          'os_version',
          'security_patch',
        ]),
        this.#renderCard('Verified Boot Keys & Hashes', 'Cryptographic keys and digests for boot attestation', 'fingerprint', [
          'vb_key',
          'vb_hash',
        ]),
        // Every boolean in TRUST_SCHEMA must be listed here. A key that is
        // missing is not rendered, and because a save replaces the whole trust
        // section from the editor's policy, an unrendered key is written back as
        // its schema default. Omitting one therefore discards the user's
        // setting without any indication.
        this.#renderSwitchGroup('Boot State Flags', 'Hardware boot lock & verification state', 'lock', [
          'verified_boot_state',
          'device_locked',
        ]),
        this.#renderSwitchGroup('Property Cleanup', 'Rewrite properties that betray a rooted build', 'cleaning_services', [
          'attempt_prop_fix',
        ]),
      ].join('\n')
    }

    if (this.#section === 'intercept') {
      return [
        this.#renderSwitchGroup('Key Retrieval & Metadata', 'Keystore inspection and entry retrieval', 'vpn_key', [
          'get_security_level',
          'get_key_entry',
          'get_number_of_entries',
          'get_supplementary_attestation_info',
        ]),
        this.#renderSwitchGroup('Listing & Updates', 'Alias queries and subcomponent modification', 'list_alt', [
          'list_entries',
          'list_entries_batched',
          'update_subcomponent',
        ]),
        this.#renderSwitchGroup('Key Management & Grants', 'Lifecycle deletion and caller authorization', 'admin_panel_settings', [
          'delete_key',
          'grant',
          'ungrant',
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
        return meta ? this.#renderFieldHtml(key, meta) : ''
      })
      .filter(Boolean)
      .join('\n')

    return `
      <div class="fs-card">
        <div class="fs-card-header">
          <md-icon class="fs-card-icon">${icon}</md-icon>
          <div class="fs-card-title-group">
            <div class="fs-card-title">${title}</div>
            ${subtitle ? `<div class="fs-card-subtitle">${subtitle}</div>` : ''}
          </div>
        </div>
        <div class="fs-card-fields">
          ${fieldsHtml}
        </div>
      </div>
    `
  }

  #renderSwitchGroup(title: string, subtitle: string, icon: string, fieldKeys: string[]): string {
    const schema = this.#config.getSectionSchema(this.#section)
    const switchesHtml = fieldKeys
      .map((key) => {
        const meta = schema.getField(key)
        if (!meta || meta.type !== 'boolean') return ''
        const desc = FIELD_DESCRIPTIONS[key] ?? ''
        return `
          <div class="switch-row" id="row-${key}" role="button" tabindex="0">
            <div class="switch-row-content">
              <div class="switch-row-title">${meta.label}</div>
              ${desc ? `<div class="switch-row-sub">${desc}</div>` : ''}
            </div>
            <md-switch icons="true" id="policy-${key}" class="policy-${key}"${meta.defaultValue ? ' selected' : ''}></md-switch>
            <md-ripple></md-ripple>
          </div>
        `
      })
      .filter(Boolean)
      .join('\n')

    return `
      <div class="fs-card">
        <div class="fs-card-header">
          <md-icon class="fs-card-icon">${icon}</md-icon>
          <div class="fs-card-title-group">
            <div class="fs-card-title">${title}</div>
            ${subtitle ? `<div class="fs-card-subtitle">${subtitle}</div>` : ''}
          </div>
        </div>
        <div class="switch-stack">
          ${switchesHtml}
        </div>
      </div>
    `
  }

  #renderFieldHtml(key: string, meta: PolicyFieldMeta): string {
    if (meta.type === 'button') {
      return `<md-outlined-button class="full-width-button policy-${key}">${i18n.t(meta.label)}</md-outlined-button>`
    }

    if (meta.type === 'boolean') {
      const desc = FIELD_DESCRIPTIONS[key] ?? ''
      return `
        <div class="switch-row" id="row-${key}" role="button" tabindex="0">
          <div class="switch-row-content">
            <div class="switch-row-title">${meta.label}</div>
            ${desc ? `<div class="switch-row-sub">${desc}</div>` : ''}
          </div>
          <md-switch icons="true" id="policy-${key}" class="policy-${key}"${meta.defaultValue ? ' selected' : ''}></md-switch>
          <md-ripple></md-ripple>
        </div>
      `
    }

    if (meta.type === 'select') {
      const options = meta.options.map((option) =>
        `<md-select-option value="${option}"><div slot="headline">${option}</div></md-select-option>`
      ).join('')
      return `<md-outlined-select class="policy-${key}" label="${meta.label}" menu-positioning="popover">${options}</md-outlined-select>`
    }

    const textMeta = meta as TextFieldMeta
    const options = textMeta.options?.length ? ` [${textMeta.options.join('/')}]` : ''
    const hint = textMeta.placeholder ?? key
    const displayLabel = textMeta.label ?? snakeToLabel(key)
    const textarea = textMeta.textarea ? ' type="textarea" rows="2"' : ''
    const maxlength = textMeta.maxlength != null ? ` maxlength="${textMeta.maxlength}"` : ''
    const extraClass = textMeta.textarea ? ' mono-field' : ''
    return `<md-outlined-text-field class="policy-${key}${extraClass}" label="${displayLabel}" placeholder="${hint}${options}" autocapitalize="none"${maxlength}${textarea}></md-outlined-text-field>`
  }
}
