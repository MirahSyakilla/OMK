import { Cli } from './cli'

const POLL_MS = 4000

export class TitleStatus {
  #cli: Cli
  #el: HTMLElement

  constructor(cli: Cli, el: HTMLElement) {
    this.#cli = cli
    this.#el = el
  }

  start(): void {
    void this.#refresh()
    window.setInterval(() => {
      void this.#refresh()
    }, POLL_MS)
  }

  async #refresh(): Promise<void> {
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
    }
  }
}
