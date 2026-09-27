import { listPackages, getPackagesInfo } from 'kernelsu-alt'
import { BulkIcons } from '../cli'
import type { PackagesInfo } from 'kernelsu-alt'
import type { MdDialog, MdFilledButton } from '@material/web/all'
import { Config } from '../config'
import { File } from '../file'
import { escapeHtml } from '../html'
import { i18n } from '../i18n'
import { applyDialogAnimation } from '../dialog/animation'
import './app_list.scss'

const SYSTEM_APPS_KEY = 'OhMyKeymintWebUIAdditionalApps'
const PIF_PACKAGES = ['com.google.android.gms', 'com.android.vending']
const INTEGRITY_TOML_PATHS = [
  '/data/adb/omk/integrity.toml',
  '/data/misc/keystore/omk/data/integrity.toml',
]
const DEFAULT_ADDITIONAL_APPS = [
  'com.google.android.gms',
  'com.android.vending',
  'com.google.android.gsf',
  'com.google.android.gms.ui',
]

export interface AppEntry {
  packageName: string
  appName: string
  isSystem: boolean
}

export class AppList {
  #entries: AppEntry[] = []
  #config: Config
  #iconObserver: IntersectionObserver | null = null
  #systemAppIconObserver: IntersectionObserver | null = null
  #container: HTMLElement | null = null
  #onLongPress: ((packageName: string) => void | Promise<void>) | null = null
  #slotLabel: ((slot: number) => string) | null = null
  #longPressedCards = new WeakSet<HTMLElement>()
  #pifEnabled = false
  #pifDialog: MdDialog | null = null
  #pifResolve: ((proceed: boolean) => void) | null = null
  /**
   * Bumped on every render. A progressive render that is still appending cards
   * is abandoned when a newer render starts, so a filter change or a tab switch
   * cannot leave stale cards appearing in the new list.
   */
  #renderGeneration = 0
  menuOpen = false

  constructor(config: Config) {
    this.#config = config
  }

  setLongPressHandler(handler: (packageName: string) => void | Promise<void>): void {
    this.#onLongPress = handler
  }

  setSlotLabel(handler: (slot: number) => string): void {
    this.#slotLabel = handler
  }

  async fetch(): Promise<void> {
    if (import.meta.env.DEV) {
      this.#initDevMode()
      return
    }

    // One native call, not two shell spawns. listPackages() goes through the
    // ksu bridge; the previous code preferred `pm list packages` via a spawned
    // shell and only fell back to the native path, so it parsed a large command
    // output for no benefit. A second `pm list packages -s` was also fetched
    // purely to build a system set, even though getPackagesInfo() below already
    // reports isSystem for every package.
    const pkgs = await listPackages('all').catch(() => [])

    let infos: PackagesInfo[]
    try {
      infos = await getPackagesInfo(pkgs) as PackagesInfo[]
    } catch {
      infos = []
    }

    const infoMap = new Map(infos.map((info) => [info.packageName, info]))
    this.#entries = pkgs.map((pkg: string) => {
      const info = infoMap.get(pkg)
      return {
        packageName: pkg,
        appName: info?.appLabel || pkg,
        isSystem: info?.isSystem ?? false,
      }
    })
  }

  async save(): Promise<void> {
    if (import.meta.env.DEV) return
    await this.#config.write()
  }

  async reloadPackages(): Promise<void> {
    await this.fetch()
    await this.#refreshIntegrity()
    this.syncSystemAppsWithConfig()
  }

  async refreshPackages(scrollToTop: boolean = true): Promise<void> {
    await this.reloadPackages()
    if (this.#container) {
      this.renderAppList(this.#container)
      if (scrollToTop) window.scrollTo(0, 0)
    }
  }

  async refresh(force: boolean = true): Promise<void> {
    if (force) {
      await this.#config.read()
      await this.refreshPackages()
      return
    }
    await this.#refreshIntegrity()
    if (this.#container) {
      this.renderAppList(this.#container)
    }
  }

  syncSystemAppsWithConfig(): void {
    const target = (this.#config.get('target') as string[]) || []
    const additionalApps = this.getAdditionalApps()
    let changed = false

    for (const pkg of target) {
      const entry = this.#entries.find((item) => item.packageName === pkg)
      if (entry?.isSystem && !additionalApps.includes(pkg)) {
        additionalApps.push(pkg)
        changed = true
      }
    }

    if (changed) this.saveAdditionalApps(additionalApps)
  }

  selectAll(): void {
    if (!this.#container) return
    const target = (this.#config.get('target') as string[]) || []
    this.#container.querySelectorAll<HTMLElement>('.card').forEach((card) => {
      const pkg = card.dataset.package!
      if (!target.includes(pkg)) {
        this.#config.push('target', pkg)
      }
      const checkbox = card.querySelector('md-checkbox')!
      checkbox.checked = true
      card.classList.add('selected')
    })
  }

  deselectAll(): void {
    void this.#deselectAll()
  }

  async #deselectAll(): Promise<void> {
    if (!this.#container) return
    if (this.#pifEnabled && this.#targetedPifPackages().length > 0) {
      if (!await this.#confirmPifUntick()) return
    }
    this.#config.set('target', [])
    this.#container.querySelectorAll<HTMLElement>('.card').forEach((card) => {
      const checkbox = card.querySelector('md-checkbox')!
      checkbox.checked = false
      card.classList.remove('selected')
    })
  }

  renderAppList(container: HTMLElement): void {
    this.#container = container
    container.innerHTML = ''

    const additionalApps = this.getAdditionalApps()
    const displayed = this.#entries.filter((entry) => !entry.isSystem || additionalApps.includes(entry.packageName))
    const target = (this.#config.get('target') as string[]) || []

    displayed.sort((a, b) => {
      const aTargeted = target.includes(a.packageName)
      const bTargeted = target.includes(b.packageName)
      if (aTargeted !== bTargeted) return aTargeted ? -1 : 1
      return (a.appName || '').localeCompare(b.appName || '')
    })

    this.#iconObserver?.disconnect()
    this.#iconObserver = this.#setupIconObserver(container)
    this.#setupCardListeners(container)
    this.#appendChunked(container, displayed, (entry) => target.includes(entry.packageName))
  }

  /**
   * Append cards in small batches, one per animation frame.
   *
   * A device can have several hundred packages, and each card contains an
   * md-ripple, an md-checkbox and an inline SVG. Building every card and
   * appending them in one pass parses and upgrades thousands of nodes without
   * yielding, which blocks the main thread long enough to stall the animated
   * header border and make the list feel frozen. Batching keeps each slice of
   * work short, so the frame loop stays responsive and the list fills in
   * progressively. Icons are observed per chunk so they start arriving while the
   * rest of the list is still being built.
   */
  /**
   * Arm icon loading for a freshly appended chunk.
   *
   * With content-visibility supported, the browser's own visibility state is the
   * trigger, because an observer never sees skipped contents. Without it, the
   * observer is the only option. Either way a card is loaded at most once.
   */
  #watchChunk(fragment: DocumentFragment): void {
    const holders = fragment.querySelectorAll('.app-icon-container')
    if (AppList.#supportsContentVisibility) {
      holders.forEach((holder) => {
        holder.addEventListener(
          'contentvisibilityautostatechange',
          (event) => {
            if (!(event as Event & { skipped?: boolean }).skipped) this.#ensureIcon(holder)
          },
          { passive: true },
        )
      })
    } else {
      holders.forEach((el) => this.#iconObserver?.observe(el))
    }
  }

  #appendChunked(
    container: HTMLElement,
    entries: AppEntry[],
    isSelected: (entry: AppEntry) => boolean,
  ): void {
    const generation = ++this.#renderGeneration
    const CHUNK = 20
    let index = 0
    const step = (): void => {
      if (generation !== this.#renderGeneration) return
      const fragment = document.createDocumentFragment()
      const end = Math.min(index + CHUNK, entries.length)
      for (; index < end; index++) {
        fragment.appendChild(this.#createCard(entries[index] as AppEntry, isSelected(entries[index] as AppEntry)))
      }
      container.appendChild(fragment)
      this.#watchChunk(fragment)
      if (index < entries.length) {
        requestAnimationFrame(step)
      }
    }
    requestAnimationFrame(step)
  }

  renderSystemAppList(container: HTMLElement): void {
    container.innerHTML = ''

    const additionalApps = this.getAdditionalApps()
    const systemEntries = this.#entries.filter((entry) => entry.isSystem)

    systemEntries.sort((a, b) => {
      const aChecked = additionalApps.includes(a.packageName)
      const bChecked = additionalApps.includes(b.packageName)
      if (aChecked !== bChecked) return aChecked ? -1 : 1
      return (a.appName || '').localeCompare(b.appName || '')
    })

    this.#systemAppIconObserver?.disconnect()
    this.#systemAppIconObserver = this.#setupIconObserver(container)
    this.#setupSystemAppListeners(container)
    this.#appendChunked(container, systemEntries, (entry) => additionalApps.includes(entry.packageName))
  }

  getAdditionalApps(): string[] {
    try {
      const raw = localStorage.getItem(SYSTEM_APPS_KEY)
      return raw ? JSON.parse(raw) as string[] : [...DEFAULT_ADDITIONAL_APPS]
    } catch {
      return [...DEFAULT_ADDITIONAL_APPS]
    }
  }

  saveAdditionalApps(apps: string[]): void {
    localStorage.setItem(SYSTEM_APPS_KEY, JSON.stringify(apps))
  }

  async saveSystemAppSelection(checkedApps: string[]): Promise<void> {
    this.saveAdditionalApps(checkedApps)

    const target = (this.#config.get('target') as string[]) || []
    const systemEntries = this.#entries.filter((entry) => entry.isSystem)

    for (const entry of systemEntries) {
      const pkg = entry.packageName
      const isChecked = checkedApps.includes(pkg)
      const isTargeted = target.includes(pkg)

      if (isChecked && !isTargeted) {
        this.#config.push('target', pkg)
      } else if (!isChecked && isTargeted) {
        this.#config.removeMatch('target', (value) => value === pkg)
      }
    }

    await this.refresh(false)
  }

  #createCard(entry: AppEntry, targeted: boolean): HTMLElement {
    const selectedClass = targeted ? ' selected' : ''
    const checkedAttr = targeted ? 'checked' : ''
    const keyboxSlot = this.#config.getKeyboxSlot(entry.packageName)
    const pills: string[] = []
    if (keyboxSlot > 0) pills.push('<div class="keybox-slot"></div>')
    if (this.#pifEnabled && PIF_PACKAGES.includes(entry.packageName)) {
      pills.push('<div class="pif-pill">PIF</div>')
    }
    const pillsHtml = pills.length ? `<div class="app-pills">${pills.join('')}</div>` : ''

    const wrapper = document.createElement('div')
    wrapper.innerHTML = /* html */ `
      <div class="card-box">
        <div class="card card-alpha content${selectedClass}" data-package="${escapeHtml(entry.packageName)}">
          <md-ripple></md-ripple>
          <label class="name" for="checkbox-${escapeHtml(entry.packageName)}">
            <div class="app-icon-container">
              <div class="loader" data-package="${escapeHtml(entry.packageName)}"></div>
              <img class="app-icon" data-package="${escapeHtml(entry.packageName)}" alt="${escapeHtml(entry.appName)}" draggable="false" />
              <div class="app-icon-fallback" data-package="${escapeHtml(entry.packageName)}">
                <svg viewBox="0 -960 960 960" xmlns="http://www.w3.org/2000/svg"><path d="M40-240q9-107 65.5-197T256-580l-74-128q-6-9-3-19t13-15q8-5 18-2t16 12l74 128q86-36 180-36t180 36l74-128q6-9 16-12t18 2q10 5 13 15t-3 19l-74 128q94 53 150.5 143T920-240H40Zm275.5-124.5Q330-379 330-400t-14.5-35.5Q301-450 280-450t-35.5 14.5Q230-421 230-400t14.5 35.5Q259-350 280-350t35.5-14.5Zm400 0Q730-379 730-400t-14.5-35.5Q701-450 680-450t-35.5 14.5Q630-421 630-400t14.5 35.5Q659-350 680-350t35.5-14.5Z"/></svg>
              </div>
            </div>
            <div class="app-info">
              <div class="app-name">${escapeHtml(entry.appName)}</div>
              <div class="package-name">${escapeHtml(entry.packageName)}</div>
              ${pillsHtml}
            </div>
          </label>
          <md-checkbox class="checkbox" id="checkbox-${escapeHtml(entry.packageName)}" touch-target="wrapper" ${checkedAttr}></md-checkbox>
        </div>
      </div>`
    const card = wrapper.firstElementChild as HTMLElement
    if (keyboxSlot > 0) {
      const label = card.querySelector<HTMLElement>('.keybox-slot')
      if (label) {
        label.textContent = this.#slotLabel?.(keyboxSlot) ?? i18n.t('keybox_slot_label', keyboxSlot)
      }
    }
    return card
  }

  #setupCardListeners(container: HTMLElement): void {
    container.querySelectorAll<HTMLElement>('.card').forEach((card) => {
      let longPressTimer: ReturnType<typeof setTimeout> | null = null
      let pointerStartX = 0
      let pointerStartY = 0

      const clearLongPress = (): void => {
        if (longPressTimer !== null) {
          clearTimeout(longPressTimer)
          longPressTimer = null
        }
      }

      card.addEventListener('pointerdown', (event: PointerEvent) => {
        if (event.button !== 0 || this.menuOpen) return
        pointerStartX = event.clientX
        pointerStartY = event.clientY
        clearLongPress()
        longPressTimer = setTimeout(() => {
          longPressTimer = null
          this.#longPressedCards.add(card)
          const packageName = card.dataset.package
          if (packageName) void this.#onLongPress?.(packageName)
        }, 550)
      })
      card.addEventListener('pointermove', (event: PointerEvent) => {
        if (Math.hypot(event.clientX - pointerStartX, event.clientY - pointerStartY) > 10) {
          clearLongPress()
        }
      })
      card.addEventListener('pointerup', clearLongPress)
      card.addEventListener('pointercancel', clearLongPress)

      card.onclick = () => {
        void this.#onCardClick(card)
      }
    })
  }

  async #onCardClick(card: HTMLElement): Promise<void> {
    if (this.menuOpen) return
    if (this.#longPressedCards.has(card)) {
      this.#longPressedCards.delete(card)
      return
    }
    const pkg = card.dataset.package!
    const checkbox = card.querySelector('md-checkbox')!
    const target = (this.#config.get('target') as string[]) || []

    if (checkbox.checked) {
      if (this.#pifEnabled && PIF_PACKAGES.includes(pkg)) {
        if (!await this.#confirmPifUntick()) return
      }
      this.#config.removeMatch('target', (value) => value === pkg)
      checkbox.checked = false
      card.classList.remove('selected')
    } else {
      if (!target.includes(pkg)) {
        this.#config.push('target', pkg)
      }
      checkbox.checked = true
      card.classList.add('selected')
    }
  }

  #targetedPifPackages(): string[] {
    const target = (this.#config.get('target') as string[]) || []
    return PIF_PACKAGES.filter((pkg) => target.includes(pkg))
  }

  async #refreshIntegrity(): Promise<void> {
    this.#pifEnabled = await this.#readIntegrityEnabled()
  }

  async #readIntegrityEnabled(): Promise<boolean> {
    if (import.meta.env.DEV) return true
    for (const path of INTEGRITY_TOML_PATHS) {
      if (!(await File.exist(path))) continue
      try {
        const raw = await File.read(path)
        const line = raw.split('\n').find((entry) => entry.trim().startsWith('enabled'))
        if (line && /=\s*true\b/i.test(line)) return true
      } catch {
        // keep scanning
      }
    }
    return false
  }

  #ensurePifDialog(): MdDialog {
    if (this.#pifDialog) return this.#pifDialog
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-dialog id="pif-untick-dialog" type="alert">
        <div slot="headline">${i18n.t('prompt_pif_untick_title')}</div>
        <div slot="content">${i18n.t('prompt_pif_untick_message')}</div>
        <div slot="actions">
          <md-outlined-button id="pif-untick-cancel">${i18n.t('functional_button_cancel')}</md-outlined-button>
          <md-filled-button id="pif-untick-got-it">${i18n.t('functional_button_got_it')}</md-filled-button>
        </div>
      </md-dialog>`
    const fragment = template.content
    const dialog = fragment.querySelector<MdDialog>('#pif-untick-dialog')!
    fragment.querySelector<HTMLElement>('#pif-untick-cancel')!.onclick = () => {
      this.#finishPifUntick(false)
    }
    fragment.querySelector<MdFilledButton>('#pif-untick-got-it')!.onclick = () => {
      this.#finishPifUntick(true)
    }
    dialog.addEventListener('closed', () => {
      if (this.#pifResolve) this.#finishPifUntick(false)
    })
    applyDialogAnimation(dialog)
    document.querySelector('.dialog-content')?.appendChild(fragment)
    this.#pifDialog = dialog
    return dialog
  }

  #confirmPifUntick(): Promise<boolean> {
    const dialog = this.#ensurePifDialog()
    return new Promise((resolve) => {
      this.#pifResolve = resolve
      dialog.show()
    })
  }

  #finishPifUntick(proceed: boolean): void {
    const resolve = this.#pifResolve
    this.#pifResolve = null
    this.#pifDialog?.close()
    resolve?.(proceed)
  }

  #setupSystemAppListeners(container: HTMLElement): void {
    container.querySelectorAll<HTMLElement>('.card').forEach((card) => {
      card.onclick = () => {
        const checkbox = card.querySelector('md-checkbox')!
        checkbox.checked = !checkbox.checked
        card.classList.toggle('selected')
      }
    })
  }

  /**
   * Icon sources already resolved in this session, keyed by package.
   *
   * The per-icon `ksu://` path is re-invoked by Chromium for the same URL when
   * an element is recreated, so anything already resolved is served from here
   * instead of being decoded again. Only holds bulk-bridge results; the lazy
   * path is left to the element's own cache.
   */
  #iconCache = new Map<string, string>()
  /** Packages whose icon a bulk request is currently in flight for. */
  #iconPending = new Set<string>()

  /**
   * Whether this WebView supports content-visibility.
   *
   * It matters for icon loading: per css-contain-2, the skipped contents of an
   * element are never reported as intersecting, so once cards use
   * `content-visibility: auto` an IntersectionObserver on the icon holder stops
   * firing for off-screen cards. The browser's own visibility state is used as
   * the trigger instead, and the observer is kept only as a fallback.
   */
  static get #supportsContentVisibility(): boolean {
    return typeof CSS !== 'undefined' && 'contentVisibility' in document.documentElement.style
  }

  /** Load the icon for a card exactly once, whichever trigger fired. */
  #ensureIcon(scopeEl: Element): void {
    const holder = scopeEl.matches('.app-icon-container')
      ? scopeEl
      : scopeEl.querySelector('.app-icon-container')
    if (!holder || holder.getAttribute('data-icon-requested') === '1') return
    holder.setAttribute('data-icon-requested', '1')
    this.#iconObserver?.unobserve(holder as Element)
    const pkg = holder.querySelector('.app-icon')?.getAttribute('data-package')
    if (pkg) this.#loadIcon(pkg, holder as HTMLElement)
  }

  #setupIconObserver(container: HTMLElement): IntersectionObserver {
    const observer = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return
        this.#ensureIcon(entry.target as Element)
      })
    }, { rootMargin: '300px', threshold: 0.01 })

    container.querySelectorAll('.app-icon-container').forEach((el) => observer.observe(el))
    return observer
  }


  #loadIcon(packageName: string, scopeEl?: HTMLElement): void {
    const root = scopeEl ?? document
    const img = root.querySelector<HTMLImageElement>(`.app-icon[data-package="${packageName}"]`)
    const loader = root.querySelector<HTMLElement>(`.loader[data-package="${packageName}"]`)
    if (!img) return

    // Already decoded in this session: reuse rather than re-request.
    const cached = this.#iconCache.get(packageName)
    if (cached) {
      this.#showIcon(img, loader, cached)
      return
    }
    // A manager with the bulk bridge decodes a page of packages in one call and
    // caches them natively, instead of one intercepted native decode per row.
    if (BulkIcons.supported() && !this.#iconPending.has(packageName)) {
      this.#iconPending.add(packageName)
      void BulkIcons.fetch([packageName])
        .then((batch) => {
          const url = batch.get(packageName)
          if (url) {
            this.#iconCache.set(packageName, url)
            this.#showIcon(img, loader, url)
          } else {
            // The manager had nothing for this package, so the per-icon path
            // still gets a chance rather than the row showing the fallback.
            this.#showIcon(img, loader, `ksu://icon/${packageName}`, packageName, root)
          }
        })
        .finally(() => this.#iconPending.delete(packageName))
      return
    }
    this.#showIcon(img, loader, `ksu://icon/${packageName}`, packageName, root)
  }

  #showIcon(
    img: HTMLImageElement,
    loader: HTMLElement | null,
    src: string,
    packageName?: string,
    root?: ParentNode,
  ): void {
    img.onload = () => {
      if (loader) loader.style.display = 'none'
      img.style.opacity = '1'
    }
    img.onerror = () => {
      img.style.display = 'none'
      const scope = root ?? document
      const fallback = packageName
        ? scope.querySelector<HTMLElement>(`.app-icon-fallback[data-package="${packageName}"]`)
        : null
      if (fallback) fallback.classList.add('visible')
      if (loader) loader.style.display = 'none'
    }
    img.src = src
  }


  #initDevMode(): void {
    if (!this.#config.get('target')) {
      this.#config.set('target', [
        'io.github.vvb2060.keyattestation',
        'com.google.android.gms',
        'net.one97.paytm',
      ])
    }

    this.#entries = [
      { packageName: 'io.github.vvb2060.keyattestation', appName: 'Key Attestation', isSystem: false },
      { packageName: 'net.one97.paytm', appName: 'Paytm', isSystem: false },
      { packageName: 'my.com.tngdigital.ewallet', appName: 'Touch n Go eWallet', isSystem: false },
      { packageName: 'com.google.android.gms', appName: 'Google Play Services', isSystem: true },
      { packageName: 'com.android.vending', appName: 'Google Play Store', isSystem: true },
      { packageName: 'com.google.android.gsf', appName: 'Google Services Framework', isSystem: true },
    ]
  }
}
