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
      const both = status.keymint && status.injector
      const none = !status.keymint && !status.injector
      this.#el.classList.toggle('title-pill-ok', both)
      this.#el.classList.toggle('title-pill-warn', !both && !none)
      this.#el.classList.toggle('title-pill-bad', none)
    } catch {
      this.#el.classList.remove('title-pill-ok', 'title-pill-warn')
      this.#el.classList.add('title-pill-bad')
    }
  }
}
