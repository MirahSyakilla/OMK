export interface TabDefinition {
  id: string
  title: string
  icon: string
  label: string
}

export const TABS: TabDefinition[] = [
  { id: 'home-page', title: 'Overview', icon: 'home', label: 'Home' },
  { id: 'apps-page', title: 'Apps', icon: 'apps', label: 'Apps' },
  { id: 'integrity-page', title: 'Play Integrity', icon: 'verified_user', label: 'Integrity' },
  { id: 'keybox-page', title: 'Keybox', icon: 'vpn_key', label: 'Keybox' },
  { id: 'settings-page', title: 'Settings', icon: 'settings', label: 'Settings' },
]

export class Navigation {
  #activeIndex = 0
  #track: HTMLElement
  #indicator: HTMLElement
  #titleEl: HTMLElement
  #tabs: HTMLElement[] = []
  #onTabChangedCallbacks: Array<(index: number, tabId: string) => void> = []

  constructor(track: HTMLElement, dock: HTMLElement, titleEl: HTMLElement) {
    this.#track = track
    this.#titleEl = titleEl
    this.#indicator = dock.querySelector<HTMLElement>('.nav-indicator')!
    this.#tabs = Array.from(dock.querySelectorAll<HTMLElement>('.nav-tab'))

    this.#initTabs()
    this.#initGestures()
    this.switchToTab(0, false)
  }

  getActiveIndex(): number {
    return this.#activeIndex
  }

  getActiveTabId(): string {
    return TABS[this.#activeIndex]?.id ?? 'home-page'
  }

  onTabChanged(cb: (index: number, tabId: string) => void): void {
    this.#onTabChangedCallbacks.push(cb)
  }

  setIndicatorProgress(curIdx: number, targetIdx: number, progress: number): void {
    const curTab = this.#tabs[curIdx]
    const tgtTab = this.#tabs[targetIdx]
    if (!curTab || !tgtTab || !this.#indicator) return
    const left = curTab.offsetLeft + (tgtTab.offsetLeft - curTab.offsetLeft) * progress
    const width = curTab.offsetWidth + (tgtTab.offsetWidth - curTab.offsetWidth) * progress
    this.#indicator.style.transition = 'none'
    this.#indicator.style.left = `${left}px`
    this.#indicator.style.width = `${width}px`
  }

  setTrackPosition(index: number, smooth = true, offsetPx = 0): void {
    const screenW = this.#track.offsetWidth || window.innerWidth || 1
    const basePct = -index * 100
    const deltaPct = (offsetPx / screenW) * 100
    const totalPct = basePct + deltaPct
    this.#track.style.transition = smooth
      ? 'transform 320ms cubic-bezier(0.2, 0.8, 0.2, 1)'
      : 'none'
    this.#track.style.transform = `translate3d(${totalPct}%, 0, 0)`
  }

  reposition(tab: HTMLElement, smooth = true): void {
    if (!this.#indicator) return
    this.#indicator.style.transition = smooth
      ? 'left 0.35s cubic-bezier(0.34, 1.56, 0.64, 1), width 0.35s cubic-bezier(0.34, 1.56, 0.64, 1)'
      : 'none'
    this.#indicator.style.left = `${tab.offsetLeft}px`
    this.#indicator.style.width = `${tab.offsetWidth}px`
  }

  switchToTab(index: number, smooth = true): void {
    if (index < 0 || index >= TABS.length) return
    const prev = this.#activeIndex
    this.#activeIndex = index

    // Update active tab buttons
    this.#tabs.forEach((tab, i) => {
      tab.classList.toggle('nav-tab--active', i === index)
      tab.setAttribute('aria-selected', i === index ? 'true' : 'false')
    })

    // Reposition floating pill dock indicator
    const activeTab = this.#tabs[index]
    if (activeTab) {
      this.reposition(activeTab, smooth)
      requestAnimationFrame(() => {
        if (activeTab) {
          this.reposition(activeTab, smooth)
        }
      })
    }

    // Carousel track sliding
    this.setTrackPosition(index, smooth)

    // Update title
    const tabDef = TABS[index]
    if (tabDef && this.#titleEl) {
      this.#titleEl.textContent = tabDef.title
    }

    // Scroll to top on page switch
    if (smooth && prev !== index) {
      window.scrollTo({ top: 0, behavior: 'instant' })
    }

    // Notify listeners
    for (const cb of this.#onTabChangedCallbacks) {
      cb(index, tabDef.id)
    }
  }

  #initTabs(): void {
    this.#tabs.forEach((tab, index) => {
      tab.addEventListener('click', () => {
        this.switchToTab(index, true)
      })
    })

    // Re-align indicator on window resize
    window.addEventListener('resize', () => {
      const active = this.#tabs[this.#activeIndex]
      if (active) this.reposition(active, false)
      this.setTrackPosition(this.#activeIndex, false)
    })
  }

  #initGestures(): void {
    let startX = 0
    let startY = 0
    let startTime = 0
    let intent: 'none' | 'pending' | 'drag' | 'scroll' = 'none'

    document.addEventListener(
      'touchstart',
      (e: TouchEvent) => {
        if (e.touches.length !== 1 || !e.touches[0]) return
        if (document.querySelector('md-dialog[open]')) return
        const target = e.target
        if (
          target instanceof Element &&
          target.closest('input, select, textarea, md-switch, .search-bar, .terminal-body, .custom-kb-script')
        ) {
          return
        }

        const touch = e.touches[0]
        startX = touch.clientX
        startY = touch.clientY
        startTime = Date.now()
        intent = 'pending'
      },
      { passive: true },
    )

    document.addEventListener(
      'touchmove',
      (e: TouchEvent) => {
        if (intent === 'none' || intent === 'scroll') return
        if (e.touches.length !== 1 || !e.touches[0]) return

        const touch = e.touches[0]
        const currentX = touch.clientX
        const currentY = touch.clientY
        const dx = currentX - startX
        const dy = currentY - startY

        if (intent === 'pending') {
          if (Math.abs(dy) > 7 && Math.abs(dy) > Math.abs(dx)) {
            intent = 'scroll'
            return
          }
          if (Math.abs(dx) > 7 && Math.abs(dx) > Math.abs(dy)) {
            intent = 'drag'
          }
        }

        if (intent === 'drag') {
          if (e.cancelable) e.preventDefault()

          let effectiveDx = dx
          const atStart = this.#activeIndex === 0
          const atEnd = this.#activeIndex === TABS.length - 1

          // Rubber band resistance past edges
          if ((effectiveDx > 0 && atStart) || (effectiveDx < 0 && atEnd)) {
            effectiveDx = effectiveDx * 0.35
          }

          this.setTrackPosition(this.#activeIndex, false, effectiveDx)

          // Update floating dock indicator in real time
          const screenW = this.#track.offsetWidth || window.innerWidth || 1
          const dragRatio = -effectiveDx / screenW
          const targetIdx = dragRatio > 0 ? this.#activeIndex + 1 : this.#activeIndex - 1
          if (targetIdx >= 0 && targetIdx < this.#tabs.length) {
            this.setIndicatorProgress(this.#activeIndex, targetIdx, Math.min(1, Math.max(0, Math.abs(dragRatio))))
          }
        }
      },
      { passive: false },
    )

    const onTouchEndOrCancel = (e: TouchEvent) => {
      if (intent !== 'drag') {
        intent = 'none'
        return
      }

      intent = 'none'
      const touch = e.changedTouches[0]
      if (!touch) return
      const dx = touch.clientX - startX
      const dt = Date.now() - startTime
      const screenW = this.#track.offsetWidth || window.innerWidth || 1
      const distance = Math.abs(dx)
      const velocity = distance / Math.max(dt, 1)

      // Threshold: moved > 22% of screen width OR flick velocity (> 0.45 px/ms)
      let nextIdx = this.#activeIndex
      if (distance > screenW * 0.22 || velocity > 0.45) {
        if (dx < 0 && this.#activeIndex + 1 < TABS.length) {
          nextIdx = this.#activeIndex + 1
        } else if (dx > 0 && this.#activeIndex - 1 >= 0) {
          nextIdx = this.#activeIndex - 1
        }
      }

      this.switchToTab(nextIdx, true)
    }

    document.addEventListener('touchend', onTouchEndOrCancel, { passive: true })
    document.addEventListener('touchcancel', onTouchEndOrCancel, { passive: true })
  }
}
