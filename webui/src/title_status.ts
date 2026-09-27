import { Cli } from './cli'

const POLL_MS = 4000

export class TitleStatus {
  #cli: Cli
  #el: HTMLElement
  #timer: number | null = null
  /** Set while a poll is in flight, so a slow poll cannot overlap the next. */
  #inFlight = false

  constructor(cli: Cli, el: HTMLElement) {
    this.#cli = cli
    this.#el = el
  }

  start(): void {
    if (this.#timer !== null) return
    void this.#refresh()
    this.#timer = window.setInterval(() => {
      void this.#refresh()
    }, POLL_MS)
  }

  /**
   * Stop polling. The interval handle was previously discarded, so the poll
   * could never be cancelled and a refresh slower than the period would let
   * overlapping calls pile up.
   */
  stop(): void {
    if (this.#timer === null) return
    window.clearInterval(this.#timer)
    this.#timer = null
  }

  async refresh(): Promise<void> {
    await this.#refresh()
  }

  async #refresh(): Promise<void> {
    if (this.#inFlight) return
    this.#inFlight = true
    try {
      const status = await this.#cli.getServiceStatus()
      const parts = [status.keymint, status.injector]
      if (status.integrityExpected) parts.push(status.integrity)
      const up = parts.filter(Boolean).length
      const need = parts.length
      this.#el.classList.toggle('title-pill-ok', need > 0 && up === need)
      this.#el.classList.toggle('title-pill-warn', up > 0 && up < need)
      this.#el.classList.toggle('title-pill-bad', up === 0)
    } catch {
      this.#el.classList.remove('title-pill-ok', 'title-pill-warn')
      this.#el.classList.add('title-pill-bad')
    } finally {
      this.#inFlight = false
    }
  }
}
