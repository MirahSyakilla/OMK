import type { MdDialog } from '@material/web/all'
import { algorithmsFromXml, allExpiriesPassed, expiriesFromXml, type Keybox } from '../keybox/keybox'
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
  xml: string
  algos: string[]
  isExpired: boolean
  expiryDate: string
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
          <button class="btn-tonal-sm" id="kb-manage-all-btn">
            <md-icon>tune</md-icon>
            <span>Manage</span>
          </button>
        </div>
        <div class="kb-slots-stack" id="kb-slots-container">
          <div class="kb-loading">Loading keybox slots...</div>
        </div>

        <!-- Custom Sources -->
        <div class="kb-section-header">
          <div class="kb-section-title">Custom Sources</div>
          <button class="btn-tonal-sm" id="kb-add-custom-btn">
            <md-icon>add</md-icon>
            <span>Add</span>
          </button>
        </div>
        <div class="kb-custom-sources-stack" id="kb-custom-sources-container">
          <!-- Rendered dynamically -->
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
    this.#renderCustomSources()
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
    }
    // Manage All Dialog
    const manageAllBtn = this.#container.querySelector<HTMLElement>('#kb-manage-all-btn')
    manageAllBtn?.addEventListener('click', () => {
      manageAllBtn.blur()
      void this.#keybox.showManage()
    })

    // Add Custom Source
    const addCustomBtn = this.#container.querySelector<HTMLElement>('#kb-add-custom-btn')
    addCustomBtn?.addEventListener('click', () => {
      addCustomBtn.blur()
      this.#keybox.custom.showDialog()
    })
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
      const xml = await File.read(path).catch(() => '')
      const algos = algorithmsFromXml(xml)
      const isExpired = allExpiriesPassed(expiriesFromXml(xml))

      loaded.push({
        slot,
        label: this.#keybox.slotLabel(slot),
        path,
        xml,
        algos,
        isExpired,
        expiryDate: '',
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
          <md-ripple></md-ripple>
          <div class="ksc-main-row">
            <div class="ksc-icon"><md-icon>vpn_key</md-icon></div>
            <div class="ksc-info">
              <div class="ksc-title">
                ${s.label}
                ${s.algos.map((a) => `<span class="inline-badge badge-primary">${a}</span>`).join('')}
                ${s.isExpired ? '<span class="inline-badge badge-error">Expired</span>' : ''}
              </div>
              <div class="ksc-sub">${s.assignedAppsCount === 1 ? '1 app' : `${s.assignedAppsCount} apps`}</div>
            </div>
            <div class="ksc-actions">
              <button class="icon-btn-compact" data-action="inspect" data-slot="${s.slot}" aria-label="Inspect Slot">
                <md-icon>visibility</md-icon>
              </button>
            </div>
          </div>
        </div>
      `,
      )
      .join('')

    // Tapping card or eye button opens the slot in the Manage Dialog
    listEl.querySelectorAll('.kb-slot-card').forEach((card) => {
      card.addEventListener('click', () => {
        ;(card as HTMLElement).blur()
        const slot = Number.parseInt((card as HTMLElement).dataset.slot ?? '0', 10)
        void this.#keybox.showManage(slot)
      })
    })

    listEl.querySelectorAll('[data-action="inspect"]').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        e.stopPropagation()
        ;(btn as HTMLElement).blur()
        const slot = Number.parseInt((e.currentTarget as HTMLElement).dataset.slot ?? '0', 10)
        void this.#keybox.showManage(slot)
      })
    })
  }

  #renderCustomSources(): void {
    const container = this.#container?.querySelector<HTMLElement>('#kb-custom-sources-container')
    if (!container) return

    const entries = this.#keybox.custom.getEntries()
    if (entries.length === 0) {
      container.innerHTML = '<div class="kb-empty">No custom sources configured.</div>'
      return
    }

    container.innerHTML = entries
      .map(
        (entry: CustomKeyboxEntry, index: number) => `
        <div class="kb-custom-card">
          <md-ripple></md-ripple>
          <div class="kcc-icon"><md-icon>source</md-icon></div>
            <div class="kcc-name">${entry.name}</div>
            <div class="kcc-url">${entry.link}</div>
            ${entry.script ? `<div class="kcc-script">Post-script: <code>${entry.script}</code></div>` : ''}
          </div>
          <div class="kcc-actions">
            <button class="icon-btn-compact" data-custom-index="${index}" data-action="edit">
              <md-icon>edit</md-icon>
            </button>
          </div>
        </div>
      `,
      )
      .join('')

    container.querySelectorAll('[data-action="edit"]').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        ;(btn as HTMLElement).blur()
        const idx = Number.parseInt((e.currentTarget as HTMLElement).dataset.customIndex ?? '0', 10)
        const entry = entries[idx]
        if (entry) this.#keybox.custom.showDialog(entry)
      })
    })
  }
}
