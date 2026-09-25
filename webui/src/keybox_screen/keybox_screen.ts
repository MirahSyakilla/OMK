import type { MdDialog } from '@material/web/all'
import {
  algorithmsFromXml,
  allExpiriesPassed,
  certsFromXml,
  expiriesFromXml,
  type Keybox,
} from '../keybox/keybox'
import { formatDeviceDate } from '../datetime'
import type { CustomKeyboxEntry } from '../keybox/custom'
import type { KeyboxRepo } from '../keybox/repo/repo'
import type { Cli } from '../cli'
import type { Config } from '../config'
import type { Snackbar } from '../snackbar/snackbar'
import type { History } from '../history'
import { File } from '../file'
import { i18n } from '../i18n'
import { applyDialogAnimation } from '../dialog/animation'
import './keybox_screen.scss'

interface SlotDetail {
  slot: number
  label: string
  path: string
  fileName: string
  xml: string
  createdDateText: string
  algos: string[]
  isExpired: boolean
  assignedAppsCount: number
}

export class KeyboxScreen {
  #keybox: Keybox
  #keyboxRepo: KeyboxRepo
  #cli: Cli
  #config: Config
  #snackbar: Snackbar
  #history?: History
  #container: HTMLElement | null = null
  #slots: SlotDetail[] = []

  constructor(
    keybox: Keybox,
    keyboxRepo: KeyboxRepo,
    cli: Cli,
    config: Config,
    snackbar: Snackbar,
    history?: History,
  ) {
    this.#keybox = keybox
    this.#keyboxRepo = keyboxRepo
    this.#cli = cli
    this.#config = config
    this.#snackbar = snackbar
    this.#history = history
    this.#keybox.onSlotsChanged(() => {
      void this.refresh()
    })
    this.#keybox.custom.onChange(() => {
      this.#renderPresetsCustomSources()
    })
  }
  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="keybox-screen">
        <!-- Install / Add Keybox Card -->
        <div class="kb-section-title">Add Keybox</div>
        <div class="kb-install-card">
          <div class="kic-header">
            <div class="kic-icon"><md-icon>vpn_key</md-icon></div>
            <div class="kic-text">
              <div class="kic-title">Add Keybox</div>
              <div class="kic-subtitle">Import from file, generate, or choose presets</div>
            </div>
          </div>

          <div class="kac-grid">
            <!-- Tile 1: Local File -->
            <div class="kac-tile" id="kb-action-local" role="button" tabindex="0">
              <div class="kac-tile-icon"><md-icon>upload_file</md-icon></div>
              <div class="kac-tile-text">
                <div class="kac-tile-title">Local File</div>
                <div class="kac-tile-sub">From storage (.xml)</div>
              </div>
              <md-ripple></md-ripple>
            </div>

            <!-- Tile 2: Online Repo -->
            <div class="kac-tile" id="kb-action-repo" role="button" tabindex="0">
              <div class="kac-tile-icon"><md-icon>public</md-icon></div>
              <div class="kac-tile-text">
                <div class="kac-tile-title">Online Repo</div>
                <div class="kac-tile-sub">Community tested</div>
              </div>
              <md-ripple></md-ripple>
            </div>

            <!-- Tile 3: Self-Signed -->
            <div class="kac-tile" id="kb-action-generate" role="button" tabindex="0">
              <div class="kac-tile-icon"><md-icon>auto_fix_high</md-icon></div>
              <div class="kac-tile-text">
                <div class="kac-tile-title">Self-Signed</div>
                <div class="kac-tile-sub">Local generator</div>
              </div>
              <md-ripple></md-ripple>
            </div>

            <!-- Tile 4: Presets -->
            <div class="kac-tile" id="kb-action-presets" role="button" tabindex="0">
              <div class="kac-tile-icon"><md-icon>inventory_2</md-icon></div>
              <div class="kac-tile-text">
                <div class="kac-tile-title">Presets</div>
                <div class="kac-tile-sub">AOSP & Remote</div>
              </div>
              <md-ripple></md-ripple>
            </div>
          </div>
        </div>

        <!-- Slots List -->
        <div class="kb-section-header">
          <div class="kb-section-title">Configured Slots</div>
        </div>
        <div class="kb-slots-stack" id="kb-slots-container">
          <div class="kb-loading">Loading keybox slots...</div>
        </div>
      </div>
    `

    // Ensure presets dialog is in .dialog-content with project dialog standards
    const dialogContent = document.querySelector<HTMLElement>('.dialog-content')
    if (dialogContent && !document.querySelector('#kb-presets-dialog')) {
      const template = document.createElement('template')
      template.innerHTML = /* html */ `
        <md-dialog id="kb-presets-dialog">
          <div slot="headline">Choose Preset</div>
          <div slot="content" class="kb-presets-list">
            <button type="button" class="kb-preset-pill" id="kb-preset-aosp">
              <div class="kb-preset-pill-start">
                <md-icon class="kb-preset-pill-icon">android</md-icon>
                <div class="kb-preset-pill-text">
                  <span class="kb-preset-pill-title">AOSP Test Key</span>
                  <span class="kb-preset-pill-sub">Bundled open-source certificates</span>
                </div>
              </div>
              <span class="inline-badge badge-primary">AOSP</span>
              <md-ripple></md-ripple>
            </button>

            <button type="button" class="kb-preset-pill" id="kb-preset-alwaysstrong">
              <div class="kb-preset-pill-start">
                <md-icon class="kb-preset-pill-icon">cloud_download</md-icon>
                <div class="kb-preset-pill-text">
                  <span class="kb-preset-pill-title">AlwaysStrong Key</span>
                  <span class="kb-preset-pill-sub">Remote certificate download</span>
                </div>
              </div>
              <span class="inline-badge badge-ok">Remote</span>
              <md-ripple></md-ripple>
            </button>
            <div id="kb-presets-custom-container" class="kb-presets-custom-container"></div>

            <button type="button" class="kb-preset-pill kb-preset-pill--action" id="kb-preset-add-custom">
              <div class="kb-preset-pill-start">
                <md-icon class="kb-preset-pill-icon">add_circle</md-icon>
                <div class="kb-preset-pill-text">
                  <span class="kb-preset-pill-title">Add Custom Source</span>
                  <span class="kb-preset-pill-sub">Configure custom URL or script</span>
                </div>
              </div>
              <span class="inline-badge badge-tertiary">New</span>
              <md-ripple></md-ripple>
            </button>
          </div>
          <div slot="actions">
            <md-text-button id="kb-preset-cancel">${i18n.t('functional_button_cancel')}</md-text-button>
          </div>
        </md-dialog>
      `
      dialogContent.appendChild(template.content)
      const dialog = document.querySelector<MdDialog>('#kb-presets-dialog')
      if (dialog) applyDialogAnimation(dialog)
    }

    this.#bindEvents()
    void this.refresh()
  }

  async refresh(): Promise<void> {
    if (!this.#container) return
    await this.#loadSlots()
    this.#renderSlotsList()
    this.#renderPresetsCustomSources()
  }

  #bindEvents(): void {
    if (!this.#container) return

    // Tile 1: Local File
    const actionLocal = this.#container.querySelector<HTMLElement>('#kb-action-local')
    actionLocal?.addEventListener('click', async () => {
      actionLocal.blur()
      try {
        await this.#keybox.setLocalKey()
        await this.refresh()
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e)
        this.#snackbar.show(`Installation error: ${msg}`, false)
      }
    })

    // Tile 2: Online Repo
    const actionRepo = this.#container.querySelector<HTMLElement>('#kb-action-repo')
    actionRepo?.addEventListener('click', () => {
      actionRepo.blur()
      this.#keyboxRepo.show()
    })

    // Tile 3: Self-Signed Keybox
    const actionGenerate = this.#container.querySelector<HTMLElement>('#kb-action-generate')
    actionGenerate?.addEventListener('click', async () => {
      actionGenerate.blur()
      try {
        await this.#keybox.setUnknownKey()
        await this.refresh()
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e)
        this.#snackbar.show(`Generation error: ${msg}`, false)
      }
    })

    // Tile 4: Presets Dialog
    const presetsDialog = document.querySelector<MdDialog>('#kb-presets-dialog')
    if (presetsDialog) {
      const actionPresets = this.#container.querySelector<HTMLElement>('#kb-action-presets')
      actionPresets?.addEventListener('click', () => {
        actionPresets.blur()
        this.#renderPresetsCustomSources()
        presetsDialog.show()
        this.#history?.push('kb-presets-dialog', () => presetsDialog.close())
      })

      const presetCancel = document.querySelector<HTMLElement>('#kb-preset-cancel')
      presetCancel?.addEventListener('click', () => {
        presetCancel.blur()
        presetsDialog.close()
      })

      presetsDialog.addEventListener('closed', () => {
        this.#history?.consume('kb-presets-dialog')
      })

      const presetAosp = document.querySelector<HTMLElement>('#kb-preset-aosp')
      presetAosp?.addEventListener('click', async () => {
        presetAosp.blur()
        presetsDialog.close()
        try {
          await this.#keybox.setAospKey()
          await this.refresh()
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e)
          this.#snackbar.show(`Installation error: ${msg}`, false)
        }
      })

      const presetAlwaysStrong = document.querySelector<HTMLElement>('#kb-preset-alwaysstrong')
      presetAlwaysStrong?.addEventListener('click', async () => {
        presetAlwaysStrong.blur()
        presetsDialog.close()
        try {
          await this.#keybox.setAlwaysStrongKey()
          await this.refresh()
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e)
          this.#snackbar.show(`Installation error: ${msg}`, false)
        }
      })

      const presetAddCustom = document.querySelector<HTMLElement>('#kb-preset-add-custom')
      presetAddCustom?.addEventListener('click', () => {
        presetAddCustom.blur()
        presetsDialog.close()
        this.#keybox.custom.showDialog()
      })
    }
  }

  async #loadSlots(): Promise<void> {
    await this.#keybox.loadSlotNames()
    const slotNumbers = [0, ...(await this.#cli.getKeyboxSlots(this.#config.configPath).catch(() => []))]

    // Count assigned apps per slot
    const assignedCounts = new Map<number, number>()
    const target = (this.#config.get('target') as string[]) ?? []
    let otherSlotsCount = 0
    for (const slot of slotNumbers) {
      if (slot > 0) {
        const count = this.#config.packagesForSlot(slot).length
        assignedCounts.set(slot, count)
        otherSlotsCount += count
      }
    }
    // Slot 0 (Default keybox) covers all remaining unassigned scoop apps
    assignedCounts.set(0, Math.max(0, target.length - otherSlotsCount))

    const loaded: SlotDetail[] = []
    for (const slot of slotNumbers) {
      const path = this.#keybox.getKeyboxPath(slot)
      const fileName = slot > 0 ? `keybox-slot-${slot}.xml` : 'keybox.xml'
      const [mtime, xml] = await Promise.all([
        this.#cli.getFileMtime(path),
        File.read(path).catch(() => ''),
      ])
      const algos = algorithmsFromXml(xml)
      const isExpired = allExpiriesPassed(expiriesFromXml(xml))
      const createdDateText = mtime
        ? i18n.t('keybox_created', await formatDeviceDate(new Date(mtime), true))
        : i18n.t('keybox_created_unknown')

      loaded.push({
        slot,
        label: this.#keybox.slotLabel(slot),
        path,
        fileName,
        xml,
        createdDateText,
        algos,
        isExpired,
        assignedAppsCount: assignedCounts.get(slot) ?? 0,
      })
    }
    this.#slots = loaded
  }

  #renderSlotsList(): void {
    const listEl = this.#container?.querySelector<HTMLElement>('#kb-slots-container')
    if (!listEl) return

    if (this.#slots.length === 0) {
      listEl.innerHTML = '<div class="kb-empty">No keybox slots found.</div>'
      return
    }

    listEl.innerHTML = this.#slots
      .map(
        (s) => `
        <div class="kb-slot-card" data-slot="${s.slot}">
          <div class="ksc-main-row" role="button" tabindex="0">
            <md-ripple></md-ripple>
            <div class="ksc-icon"><md-icon>vpn_key</md-icon></div>
            <div class="ksc-info">
              <div class="ksc-title">
                ${s.label}
                ${s.algos.map((a) => `<span class="inline-badge badge-primary">${a}</span>`).join('')}
                ${s.isExpired ? '<span class="inline-badge badge-error">Expired</span>' : ''}
              </div>
              <div class="ksc-sub">${s.assignedAppsCount === 1 ? '1 app' : `${s.assignedAppsCount} apps`} • ${s.fileName}</div>
            </div>
            <div class="ksc-expand-icon">
              <md-icon>expand_more</md-icon>
            </div>
          </div>

          <div class="ksc-expanded-content">
            <div class="ksc-expanded-inner">
              <div class="ksc-divider"></div>

              <div class="ksc-meta-row">
                <span class="ksc-meta-pill">
                  <md-icon>schedule</md-icon>
                  ${s.createdDateText}
                </span>
                <span class="ksc-meta-pill">
                  <md-icon>apps</md-icon>
                  ${s.slot === 0 ? i18n.t('keybox_assigned_default') : i18n.t('keybox_assigned_apps', s.assignedAppsCount)}
                </span>
              </div>

              <div class="ksc-certs-list" data-slot-certs="${s.slot}"></div>

              <div class="ksc-actions-row">
                <md-outlined-button class="ksc-action-btn" data-action="rename" data-slot="${s.slot}">
                  <md-icon slot="icon">edit</md-icon>
                  ${i18n.t('keybox_action_rename')}
                </md-outlined-button>
                <md-outlined-button class="ksc-action-btn" data-action="export" data-slot="${s.slot}">
                  <md-icon slot="icon">download</md-icon>
                  ${i18n.t('keybox_action_export')}
                </md-outlined-button>
                ${
                  s.slot > 0
                    ? `
                  <md-outlined-button class="ksc-action-btn ksc-action-btn--danger" data-action="delete" data-slot="${s.slot}">
                    <md-icon slot="icon">delete</md-icon>
                    ${i18n.t('keybox_action_delete')}
                  </md-outlined-button>
                `
                    : ''
                }
              </div>
            </div>
          </div>
        </div>
      `,
      )
      .join('')

    const certsRendered = new Set<number>()
    const renderCerts = async (slot: number, certsContainer: HTMLElement, xml: string) => {
      if (certsRendered.has(slot)) return
      certsRendered.add(slot)
      const certs = certsFromXml(xml)
      if (certs.length === 0) {
        certsContainer.innerHTML = '<span class="keybox-cert-pill">No certificates found</span>'
        return
      }
      certsContainer.innerHTML = ''
      for (const cert of certs) {
        const row = document.createElement('span')
        const passed = cert.notAfter.getTime() <= Date.now()
        row.className = passed ? 'keybox-cert-pill keybox-meta-expired' : 'keybox-cert-pill'
        const role = i18n.t(`keybox_cert_${cert.role}`)
        const expiry = await formatDeviceDate(cert.notAfter, true)
        row.textContent = cert.cn
          ? i18n.t('keybox_cert_expires_cn', cert.algo, role, cert.cn, expiry)
          : i18n.t('keybox_cert_expires', cert.algo, role, expiry)
        certsContainer.appendChild(row)
      }
    }

    listEl.querySelectorAll('.kb-slot-card').forEach((card) => {
      const slot = Number.parseInt((card as HTMLElement).dataset.slot ?? '0', 10)
      const slotData = this.#slots.find((s) => s.slot === slot)
      const mainRow = card.querySelector<HTMLElement>('.ksc-main-row')
      const certsContainer = card.querySelector<HTMLElement>('[data-slot-certs]')

      const toggleExpand = () => {
        mainRow?.blur()
        const isExpanded = card.classList.toggle('expanded')
        if (isExpanded && certsContainer && slotData) {
          void renderCerts(slot, certsContainer, slotData.xml)
        }
      }

      mainRow?.addEventListener('click', toggleExpand)
      mainRow?.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          toggleExpand()
        }
      })
    })

    listEl.querySelectorAll<HTMLElement>('.ksc-action-btn').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        e.stopPropagation()
        btn.blur()
        const slot = Number.parseInt(btn.dataset.slot ?? '0', 10)
        const action = btn.dataset.action
        if (action === 'rename') {
          this.#keybox.openRename(slot)
        } else if (action === 'export') {
          void this.#keybox.exportSlot(slot)
        } else if (action === 'delete') {
          this.#keybox.openDelete(slot)
        }
      })
    })
  }

  #renderPresetsCustomSources(): void {
    const container = document.querySelector<HTMLElement>('#kb-presets-custom-container')
    if (!container) return

    const entries = this.#keybox.custom.getEntries()
    if (entries.length === 0) {
      container.innerHTML = ''
      return
    }

    container.innerHTML = entries
      .map(
        (entry: CustomKeyboxEntry, index: number) => `
        <div class="kb-preset-pill kb-preset-pill--custom" role="button" tabindex="0" data-custom-index="${index}">
          <div class="kb-preset-pill-start">
            <md-icon class="kb-preset-pill-icon">source</md-icon>
            <div class="kb-preset-pill-text">
              <span class="kb-preset-pill-title">${entry.name}</span>
              <span class="kb-preset-pill-sub">${entry.link}</span>
            </div>
          </div>
          <div class="kb-preset-pill-end">
            <span class="inline-badge badge-tertiary">Custom</span>
            <button type="button" class="icon-btn-compact" data-action="edit-custom" data-custom-index="${index}" aria-label="Edit Source">
              <md-icon>edit</md-icon>
            </button>
          </div>
          <md-ripple></md-ripple>
        </div>
      `,
      )
      .join('')

    const presetsDialog = document.querySelector<MdDialog>('#kb-presets-dialog')

    container.querySelectorAll<HTMLElement>('.kb-preset-pill--custom').forEach((pill) => {
      pill.addEventListener('click', async () => {
        pill.blur()
        presetsDialog?.close()
        const idx = Number.parseInt(pill.dataset.customIndex ?? '0', 10)
        const entry = entries[idx]
        if (!entry) return
        try {
          await this.#keybox.custom.fetchKeybox(entry)
          await this.refresh()
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e)
          this.#snackbar.show(`Installation error: ${msg}`, false)
        }
      })
    })

    container.querySelectorAll<HTMLElement>('[data-action="edit-custom"]').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        e.stopPropagation()
        btn.blur()
        presetsDialog?.close()
        const idx = Number.parseInt(btn.dataset.customIndex ?? '0', 10)
        const entry = entries[idx]
        if (entry) this.#keybox.custom.showDialog(entry)
      })
    })
  }
}
