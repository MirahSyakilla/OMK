import { Cli } from '../cli'
import { AppList } from '../app_list/app_list'
import { Config } from '../config'
import { SectionDialog } from './section'
import { RuntimeDialog } from './runtime'
import { AboutDialog } from './about'
import { HelpDialog } from './help'
import { SystemAppDialog } from './system_app'
import { IntegrityDialog } from './integrity'
import { FileSelector } from '../file_selector/file_selector'
import { Snackbar } from '../snackbar/snackbar'
import './dialog.scss'

export class DialogController {
  readonly about: AboutDialog
  readonly help: HelpDialog
  readonly systemApp: SystemAppDialog
  readonly integrity: IntegrityDialog
  readonly trust: SectionDialog
  readonly core: SectionDialog
  readonly injector: SectionDialog
  readonly filter: SectionDialog
  readonly intercept: SectionDialog
  readonly device: SectionDialog
  readonly crypto: SectionDialog
  readonly runtime: RuntimeDialog

  constructor(cli: Cli, config: Config, appList: AppList, fileSelector: FileSelector, snackbar: Snackbar) {
    this.about = new AboutDialog(cli)
    this.help = new HelpDialog()
    this.systemApp = new SystemAppDialog(appList)
    this.integrity = new IntegrityDialog(cli, config, fileSelector, snackbar)
    this.trust = new SectionDialog(config, 'trust', 'trust-settings-dialog')
    this.core = new SectionDialog(config, 'omk_main', 'core-settings-dialog')
    this.injector = new SectionDialog(config, 'injector_main', 'injector-settings-dialog')
    this.filter = new SectionDialog(config, 'filter', 'filter-settings-dialog')
    this.intercept = new SectionDialog(config, 'intercept', 'intercept-settings-dialog')
    this.device = new SectionDialog(config, 'device', 'device-settings-dialog')
    this.crypto = new SectionDialog(config, 'crypto', 'crypto-settings-dialog')
    this.runtime = new RuntimeDialog(config)
  }

  appendAll(container: HTMLElement): void {
    const dialogs = [
      this.about,
      this.help,
      this.systemApp,
      this.integrity,
      this.trust,
      this.core,
      this.injector,
      this.filter,
      this.intercept,
      this.device,
      this.crypto,
      this.runtime,
    ]

    dialogs.forEach((dialog) => {
      container.appendChild(dialog.getElement())
      dialog.initAnimation()
    })
  }

  showAbout(): void {
    this.about.show()
  }

  showHelp(): void {
    this.help.show()
  }

  async showSystemApp(): Promise<void> {
    await this.systemApp.show()
  }

  showIntegrity(): void {
    void this.integrity.show()
  }

  showTrust(): void {
    this.trust.show()
  }

  showCore(): void {
    this.core.show()
  }

  showInjector(): void {
    this.injector.show()
  }

  showFilter(): void {
    this.filter.show()
  }

  showIntercept(): void {
    this.intercept.show()
  }

  showDevice(): void {
    this.device.show()
  }

  showCrypto(): void {
    this.crypto.show()
  }

  showRuntime(): void {
    this.runtime.show()
  }
}
