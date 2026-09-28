export interface TabDefinition {
  id: string
  title: string
  icon: string
  label: string
}

export const TABS: TabDefinition[] = [
  { id: 'apps-page', title: '', icon: 'apps', label: 'Apps' },
  { id: 'keybox-page', title: 'Keybox', icon: 'vpn_key', label: 'Keybox' },
  { id: 'integrity-page', title: 'Play Integrity', icon: 'verified_user', label: 'Integrity' },
  { id: 'settings-page', title: 'Settings', icon: 'settings', label: 'Settings' },
  { id: 'about-page', title: 'About', icon: 'info', label: 'About' },
]

export class Navigation {
  #activeIndex = 0
  #track: HTMLElement
  #indicator: HTMLElement
  #titleEl: HTMLElement
  #tabs: HTMLElement[] = []
  #pages: HTMLElement[] = []
  #suppressTimer: number | null = null
  #onTabChangedCallbacks: Array<(index: number, tabId: string) => void> = []
  constructor(track: HTMLElement, dock: HTMLElement, titleEl: HTMLElement) {
    this.#track = track
    this.#titleEl = titleEl
    this.#indicator = dock.querySelector<HTMLElement>('.nav-indicator')!
    this.#tabs = Array.from(dock.querySelectorAll<HTMLElement>('.nav-tab'))
    this.#pages = TABS.map((tab) => document.getElementById(tab.id)).filter(
      (page): page is HTMLElement => page !== null,
    )

    this.#initTabs()
    this.switchToTab(0, false)
  }

  getActiveIndex(): number {
    return this.#activeIndex
  }

  getActiveTabId(): string {
    return TABS[this.#activeIndex]?.id ?? 'apps-page'
  }

  onTabChanged(cb: (index: number, tabId: string) => void): void {
    this.#onTabChangedCallbacks.push(cb)
  }

  /**
   * Collapses every page except active/sliding ones so the document height
   * matches the visible content. Only active and immediate sliding screens are allowed.
   */
  #updatePageSuppression(activeIndex: number, allowedIndices?: number[]): void {
    this.#pages.forEach((page, index) => {
      const isAllowed = allowedIndices ? allowedIndices.includes(index) : index === activeIndex
      page.classList.toggle('page--suppressed', !isAllowed)
    })
  }


  setTrackPosition(index: number, smooth = true): void {
    this.#track.style.transition = smooth
      ? 'transform 350ms cubic-bezier(0.2, 0.8, 0.2, 1)'
      : 'none'
    this.#track.style.transform = `translate3d(${-index * 100}%, 0, 0)`
  }

  reposition(tab: HTMLElement, smooth = true): void {
    if (!this.#indicator) return
    this.#indicator.style.transition = smooth ? '' : 'none'
    this.#indicator.style.transform = `translate3d(${tab.offsetLeft}px, 0, 0)`
    this.#indicator.style.width = `${tab.offsetWidth}px`
  }

  switchToTab(index: number, smooth = true, force = false): void {
    if (index < 0 || index >= TABS.length) return
    if (!force && smooth && index === this.#activeIndex && this.#tabs[index]?.classList.contains('nav-tab--active')) {
      return
    }
    const prev = this.#activeIndex
    this.#activeIndex = index

    // 1. Update active tab buttons so active tab expands its label
    this.#tabs.forEach((tab, i) => {
      tab.classList.toggle('nav-tab--active', i === index)
      tab.setAttribute('aria-selected', i === index ? 'true' : 'false')
    })

    // 2. Reposition floating pill dock indicator to the expanded active tab
    const activeTab = this.#tabs[index]
    if (activeTab) this.reposition(activeTab, smooth)

    // 3. Carousel track sliding
    this.setTrackPosition(index, smooth)

    // 4. Update title
    const tabDef = TABS[index]
    if (this.#titleEl) {
      if (index === 0) {
        this.#titleEl.classList.add('hide')
        this.#titleEl.textContent = ''
      } else {
        this.#titleEl.classList.remove('hide')
        this.#titleEl.textContent = tabDef?.title ?? ''
      }
    }
    const statusPill = document.querySelector<HTMLElement>('#title-status')
    const pillLabel = statusPill?.querySelector<HTMLElement>('.title-pill-label')
    if (statusPill && pillLabel) {
      if (index === 0) {
        statusPill.style.display = ''
        statusPill.classList.add('title-pill--brand')
        pillLabel.textContent = 'OhMyKeymint'
      } else {
        statusPill.style.display = 'none'
      }
    }

    // 5. Selectively unsuppress only prev and current page during slide (never unsuppress all 4 pages!)
    this.#updatePageSuppression(index, smooth && prev !== index ? [prev, index] : [index])
    if (this.#suppressTimer !== null) {
      clearTimeout(this.#suppressTimer)
      this.#suppressTimer = null
    }
    if (smooth) {
      this.#suppressTimer = window.setTimeout(() => {
        this.#suppressTimer = null
        this.#updatePageSuppression(this.#activeIndex, [this.#activeIndex])
      }, 370)
    } else {
      this.#updatePageSuppression(index, [index])
    }
    // Scroll to top on page switch
    if (smooth && prev !== index) {
      window.scrollTo({ top: 0, behavior: 'instant' })
    }

    if (prev !== index) {
      for (const cb of this.#onTabChangedCallbacks) {
        cb(index, tabDef.id)
      }
    }
  }

  #initTabs(): void {
    this.#tabs.forEach((tab, index) => {
      tab.addEventListener('click', () => {
        this.switchToTab(index, true, true)
      })
    })

    // Re-align indicator on window resize
    window.addEventListener('resize', () => {
      const active = this.#tabs[this.#activeIndex]
      if (active) this.reposition(active, false)
      this.setTrackPosition(this.#activeIndex, false)
    })
  }

}
