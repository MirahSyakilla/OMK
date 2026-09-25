export interface TabDefinition {
  id: string
  title: string
  icon: string
  label: string
}

export const TABS: TabDefinition[] = [
  { id: 'apps-page', title: '', icon: 'apps', label: 'Apps' },
  { id: 'keybox-page', title: 'Keybox', icon: 'vpn_key', label: 'Keybox' },
  { id: 'integrity-page', title: 'Integrity', icon: 'verified_user', label: 'Integrity' },
  { id: 'settings-page', title: 'Settings', icon: 'settings', label: 'Settings' },
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
  #cachedScreenW = 0
  #tabMetrics: Array<{ left: number; width: number }> = []
  constructor(track: HTMLElement, dock: HTMLElement, titleEl: HTMLElement) {
    this.#track = track
    this.#titleEl = titleEl
    this.#indicator = dock.querySelector<HTMLElement>('.nav-indicator')!
    this.#tabs = Array.from(dock.querySelectorAll<HTMLElement>('.nav-tab'))
    this.#pages = TABS.map((tab) => document.getElementById(tab.id)).filter(
      (page): page is HTMLElement => page !== null,
    )

    this.#measureMetrics()
    this.#initTabs()
    this.#initGestures()
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

  #measureMetrics(): void {
    this.#cachedScreenW = this.#track.offsetWidth || window.innerWidth || 1
    this.#tabMetrics = this.#tabs.map((tab) => ({
      left: tab.offsetLeft,
      width: tab.offsetWidth,
    }))
  }

  setIndicatorProgress(curIdx: number, targetIdx: number, progress: number): void {
    const cur = this.#tabMetrics[curIdx]
    const tgt = this.#tabMetrics[targetIdx]
    if (!cur || !tgt || !this.#indicator) return
    const left = cur.left + (tgt.left - cur.left) * progress
    const width = cur.width + (tgt.width - cur.width) * progress
    this.#indicator.style.transition = 'none'
    this.#indicator.style.transform = `translate3d(${left}px, 0, 0)`
    this.#indicator.style.width = `${width}px`
  }
  setTrackPosition(index: number, smooth = true, offsetPx = 0): void {
    const screenW = this.#cachedScreenW || this.#track.offsetWidth || window.innerWidth || 1
    const basePct = -index * 100
    const deltaPct = (offsetPx / screenW) * 100
    const totalPct = basePct + deltaPct
    this.#track.style.transition = smooth
      ? 'transform 350ms cubic-bezier(0.2, 0.8, 0.2, 1)'
      : 'none'
    this.#track.style.transform = `translate3d(${totalPct}%, 0, 0)`
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
      this.#measureMetrics()
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
    let pendingRaf: number | null = null
    let latestEffectiveDx = 0
    let latestDragRatio = 0
    let latestTargetIdx = 0

    const renderDragFrame = () => {
      pendingRaf = null
      if (intent !== 'drag') return
      this.setTrackPosition(this.#activeIndex, false, latestEffectiveDx)
      if (latestTargetIdx >= 0 && latestTargetIdx < this.#tabs.length) {
        this.setIndicatorProgress(this.#activeIndex, latestTargetIdx, Math.min(1, Math.max(0, Math.abs(latestDragRatio))))
      }
    }

    document.addEventListener(
      'touchstart',
      (e: TouchEvent) => {
        if (e.touches.length !== 1 || !e.touches[0]) return
        if (document.querySelector('md-dialog[open], .keybox-repo-overlay:not(.hidden)')) return
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
        this.#measureMetrics()
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
            // Unsuppress ONLY the current page and adjacent target page
            const targetIdx = dx < 0 ? this.#activeIndex + 1 : this.#activeIndex - 1
            const allowed = [this.#activeIndex]
            if (targetIdx >= 0 && targetIdx < TABS.length) allowed.push(targetIdx)
            this.#updatePageSuppression(this.#activeIndex, allowed)
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

          latestEffectiveDx = effectiveDx
          const screenW = this.#cachedScreenW || 1
          latestDragRatio = -effectiveDx / screenW
          latestTargetIdx = latestDragRatio > 0 ? this.#activeIndex + 1 : this.#activeIndex - 1

          if (pendingRaf === null) {
            pendingRaf = requestAnimationFrame(renderDragFrame)
          }
        }
      },
      { passive: false },
    )

    const onTouchEndOrCancel = (e: TouchEvent) => {
      if (pendingRaf !== null) {
        cancelAnimationFrame(pendingRaf)
        pendingRaf = null
      }
      if (intent !== 'drag') {
        intent = 'none'
        return
      }

      intent = 'none'
      const touch = e.changedTouches?.[0]
      let nextIdx = this.#activeIndex

      if (touch) {
        const dx = touch.clientX - startX
        const dt = Date.now() - startTime
        const screenW = this.#cachedScreenW || 1
        const distance = Math.abs(dx)
        const velocity = distance / Math.max(dt, 1)

        // Threshold: moved > 22% of screen width OR flick velocity (> 0.45 px/ms)
        if (distance > screenW * 0.22 || velocity > 0.45) {
          if (dx < 0 && this.#activeIndex + 1 < TABS.length) {
            nextIdx = this.#activeIndex + 1
          } else if (dx > 0 && this.#activeIndex - 1 >= 0) {
            nextIdx = this.#activeIndex - 1
          }
        }
      }

      this.switchToTab(nextIdx, true, true)
    }

    document.addEventListener('touchend', onTouchEndOrCancel, { passive: true })
    document.addEventListener('touchcancel', onTouchEndOrCancel, { passive: true })
  }
}
