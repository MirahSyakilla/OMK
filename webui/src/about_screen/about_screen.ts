import type { MdFilledButton } from '@material/web/all'
import { Cli } from '../cli'
import { TELEGRAM_CHANNEL } from '../constant'
import { escapeHtml } from '../html'
import { BugReport } from './bug_report'
import { contributorCarouselHtml, initContributorCarousel } from './contributors'
import type { Snackbar } from '../snackbar/snackbar'
import './about_screen.scss'

/**
 * About page: module identity, links, a bug-report action, and contributors.
 *
 * Rendered lazily on first visit, because it reads the module version off the
 * device and there is no reason to pay for that during startup.
 */
export class AboutScreen {
  readonly #cli: Cli
  readonly #bugReport: BugReport
  #container: HTMLElement | null = null
  #rendered = false

  constructor(cli: Cli, snackbar?: Snackbar) {
    this.#cli = cli
    this.#bugReport = new BugReport(snackbar)
  }

  render(container: HTMLElement): void {
    this.#container = container
    this.#rendered = false
  }

  /** Build the page on first visit and start the carousel. */
  async load(): Promise<void> {
    const container = this.#container
    if (!container || this.#rendered) return
    this.#rendered = true

    container.innerHTML = /* html */ `
      <div class="about-screen">
        <div class="about-card about-identity">
          <div class="about-identity__row">
            <div class="about-identity__name">OhMyKeymint</div>
            <div class="about-identity__tag">OMK</div>
          </div>
          <div class="about-identity__sub">Built-in WebUI</div>
          <div class="about-identity__version" id="about-version">Loading version...</div>
        </div>

        <div class="about-card">
          <div class="about-links">
            <md-filled-button id="about-telegram">
              <span>Telegram</span>
              <md-icon slot="icon">send</md-icon>
            </md-filled-button>
            <md-filled-button id="about-github">
              <span>GitHub</span>
              <md-icon slot="icon">code</md-icon>
            </md-filled-button>
          </div>
        </div>

        <div class="about-section">
          <div class="about-section__title">Bug Report</div>
          <div class="about-section__body">You had issue but no idea how to report? Just simply click the button below.</div>
          <md-filled-button id="about-bugreport">
            <span>Send Bug Report</span>
            <md-icon slot="icon">bug_report</md-icon>
          </md-filled-button>
        </div>

        <div class="about-section">
          <div class="about-section__title">Contributors</div>
          ${contributorCarouselHtml()}
        </div>
      </div>
    `

    container.querySelector<MdFilledButton>('#about-telegram')!.onclick = () => {
      void this.#cli.linkRedirect(TELEGRAM_CHANNEL)
    }
    container.querySelector<MdFilledButton>('#about-github')!.onclick = () => {
      void this.#cli.linkRedirect(this.#cli.getRepositoryUrl())
    }
    container.querySelector<MdFilledButton>('#about-bugreport')!.onclick = () => {
      void this.#bugReport.run()
    }

    initContributorCarousel()
    await this.#loadVersion()
  }

  async #loadVersion(): Promise<void> {
    const el = this.#container?.querySelector('#about-version')
    if (!el) return
    try {
      const info = await this.#cli.getModuleInfo()
      const version = info.version || info.versionName || ''
      const versionCode = info.versionCode || ''
      el.textContent = versionCode ? `${escapeHtml(version)} (${escapeHtml(versionCode)})` : version
    } catch {
      el.textContent = 'Version unavailable'
    }
  }
}
