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
      const up = [status.keymint, status.injector, status.integrity].filter(Boolean).length
      this.#el.classList.toggle('title-pill-ok', up === 3)
      this.#el.classList.toggle('title-pill-warn', up === 1 || up === 2)
      this.#el.classList.toggle('title-pill-bad', up === 0)
    } catch {
      this.#el.classList.remove('title-pill-ok', 'title-pill-warn')
      this.#el.classList.add('title-pill-bad')
    }
  }
}
