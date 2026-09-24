import type { Keybox } from '../keybox/keybox'
import type { CustomKeyboxEntry } from '../keybox/custom'
import type { KeyboxRepo } from '../keybox/repo/repo'
import type { Cli } from '../cli'
import type { Config } from '../config'
import type { Snackbar } from '../snackbar/snackbar'
import { File } from '../file'
import { i18n } from '../i18n'
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
  #container: HTMLElement | null = null
  #slots: SlotDetail[] = []

  constructor(
    keybox: Keybox,
    keyboxRepo: KeyboxRepo,
    cli: Cli,
    config: Config,
    snackbar: Snackbar,
  ) {
    this.#keybox = keybox
    this.#keyboxRepo = keyboxRepo
    this.#cli = cli
    this.#config = config
    this.#snackbar = snackbar
  }

  render(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = /* html */ `
      <div class="keybox-screen">
        <!-- Install Card -->
        <div class="kb-section-title">Install Keybox</div>
        <div class="kb-install-card">
          <div class="kic-header">
            <div class="kic-icon"><md-icon>vpn_key</md-icon></div>
            <div class="kic-text">
              <div class="kic-title">Import or Generate Keybox</div>
              <div class="kic-subtitle">Install certificates directly to KeyMint storage</div>
            </div>
          </div>

          <div class="kic-field-row">
            <div class="kic-field">
              <label for="kb-source-select" class="kic-label">Key Source</label>
              <select id="kb-source-select" class="kb-select">
                <option value="aosp">AOSP (Bundled keys)</option>
                <option value="unknown">Self-Signed (Unknown keys)</option>
                <option value="alwaysstrong">AlwaysStrong (Remote)</option>
                <option value="local">Local File (.xml)</option>
              </select>
            </div>
          </div>

          <button class="btn-filled kic-install-btn" id="kb-install-now-btn">
            <md-icon>download</md-icon>
            <span>Install Now</span>
          </button>
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

        <!-- Browse / Remote Repo -->
        <div class="kb-section-title">Online Repository</div>
        <div class="kb-repo-card" id="kb-open-repo-btn" role="button" tabindex="0">
          <div class="krc-icon"><md-icon>public</md-icon></div>
          <div class="krc-content">
            <div class="krc-title">Open Keybox Repo (KOWX712)</div>
            <div class="krc-subtitle">Browse and import community-tested keybox certificates</div>
          </div>
          <div class="krc-arrow"><md-icon>open_in_new</md-icon></div>
          <md-ripple></md-ripple>
        </div>
      </div>
    `

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

    // Install Now
    this.#container.querySelector('#kb-install-now-btn')?.addEventListener('click', async () => {
      const sourceSelect = this.#container?.querySelector<HTMLSelectElement>('#kb-source-select')
      const source = sourceSelect?.value ?? 'aosp'

      try {
        if (source === 'aosp') {
          await this.#keybox.setAospKey()
        } else if (source === 'unknown') {
          await this.#keybox.setUnknownKey()
        } else if (source === 'alwaysstrong') {
          await this.#keybox.setAlwaysStrongKey()
        } else if (source === 'local') {
          await this.#keybox.setLocalKey()
        }
        await this.refresh()
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e)
        this.#snackbar.show(`Installation error: ${msg}`, false)
      }
    })

    // Open Repo
    this.#container.querySelector('#kb-open-repo-btn')?.addEventListener('click', () => {
      this.#keyboxRepo.show()
    })

    // Manage All Dialog
    this.#container.querySelector('#kb-manage-all-btn')?.addEventListener('click', () => {
      void this.#keybox.showManage()
    })

    // Add Custom Source
    this.#container.querySelector('#kb-add-custom-btn')?.addEventListener('click', () => {
      this.#keybox.custom.showDialog()
    })
  }

  async #loadSlots(): Promise<void> {
    await this.#keybox.loadSlotNames()
    const slotNumbers = [0, ...(await this.#cli.getKeyboxSlots(this.#config.configPath).catch(() => []))]

    // Count assigned apps per slot
    const assignedCounts = new Map<number, number>()
    const scoopDetails = (this.#config.get('filter') as { scoop_details?: Record<string, { slot?: number }> })?.scoop_details ?? {}
    for (const details of Object.values(scoopDetails)) {
      const s = details?.slot ?? 0
      assignedCounts.set(s, (assignedCounts.get(s) ?? 0) + 1)
    }

    const loaded: SlotDetail[] = []
    for (const slot of slotNumbers) {
      const path = this.#keybox.getKeyboxPath(slot)
      const xml = await File.read(path).catch(() => '')
      const algos: string[] = []
      if (/algorithm\s*=\s*"rsa"/i.test(xml)) algos.push('RSA')
      if (/algorithm\s*=\s*"ecdsa"/i.test(xml)) algos.push('EC')

      const expiryMatch = xml.match(/notAfter\b[^>]*>([^<]+)/i)
      const expiryDate = expiryMatch?.[1]?.trim() ?? 'Unknown'
      const isExpired = expiryDate !== 'Unknown' && new Date(expiryDate).getTime() < Date.now()

      loaded.push({
        slot,
        label: this.#keybox.slotLabel(slot),
        path,
        xml,
        algos,
        isExpired,
        expiryDate,
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
              <div class="ksc-sub">${s.assignedAppsCount} apps assigned • Expires: ${s.expiryDate}</div>
            </div>
            <div class="ksc-actions">
              <button class="icon-btn-compact" data-action="export" data-slot="${s.slot}" aria-label="Export Slot">
                <md-icon>download</md-icon>
              </button>
              ${
                s.slot > 0
                  ? `
                <button class="icon-btn-compact text-error" data-action="delete" data-slot="${s.slot}" aria-label="Delete Slot">
                  <md-icon>delete</md-icon>
                </button>
              `
                  : ''
              }
            </div>
          </div>
        </div>
      `,
      )
      .join('')

    // Bind slot actions
    listEl.querySelectorAll('[data-action="export"]').forEach((btn) => {
      btn.addEventListener('click', async (e) => {
        const slot = Number.parseInt((e.currentTarget as HTMLElement).dataset.slot ?? '0', 10)
        try {
          const path = this.#keybox.getKeyboxPath(slot)
          const fileName = `keybox_slot_${slot}.xml`
          const savedPath = await this.#cli.exportKeybox(path, fileName)
          this.#snackbar.show(`Exported to ${savedPath}`)
        } catch {
          this.#snackbar.show('Failed to export keybox', false)
        }
      })
    })

    listEl.querySelectorAll('[data-action="delete"]').forEach((btn) => {
      btn.addEventListener('click', async (e) => {
        const slot = Number.parseInt((e.currentTarget as HTMLElement).dataset.slot ?? '0', 10)
        if (slot > 0) {
          const conf = confirm(i18n.t('keybox_slot_delete_confirm', this.#keybox.slotLabel(slot)))
          if (conf) {
            try {
              const xmlPath = this.#keybox.getKeyboxPath(slot)
              await File.delete(xmlPath)
              await File.delete(`${xmlPath}.bak`)
              this.#snackbar.show('Slot deleted')
              await this.refresh()
            } catch {
              this.#snackbar.show('Failed to delete slot', false)
            }
          }
        }
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
        const idx = Number.parseInt((e.currentTarget as HTMLElement).dataset.customIndex ?? '0', 10)
        const entry = entries[idx]
        if (entry) this.#keybox.custom.showDialog(entry)
      })
    })
  }
}
