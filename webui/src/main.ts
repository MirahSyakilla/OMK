import '@material/web/all'
import type { MdDialog, MdFab, MdIconButton, MdOutlinedTextField } from '@material/web/all'
import { i18n } from './i18n'
import { Cli } from './cli'
import { Config } from './config'
import { AppList } from './app_list/app_list'
import { Snackbar } from './snackbar/snackbar'
import { FileSelector } from './file_selector/file_selector'
import { History } from './history'
import { Keybox } from './keybox/keybox'
import { KeyboxRepo } from './keybox/repo/repo'
import { DialogController } from './dialog/dialog'
import { SearchBar } from './search_bar/search_bar'
import { Keybind } from './keybind'
import { MainMenu } from './main_menu/main_menu'
import { ReloadMenu } from './reload_menu/reload_menu'
import './style.scss'

await i18n.init()

const snackbar = new Snackbar()
const fileSelector = new FileSelector()
const cli = new Cli()
const history = new History()
const keybind = new Keybind()
const config = new Config()

document.querySelector<HTMLDivElement>('#app')!.innerHTML = /* html */ `
  <section class="header">
    <div id="title" class="search-hide">OhMyKeymint</div>
    <div class="spacer"></div>
    <md-icon-button id="search-button" class="search-hide"><md-icon>search</md-icon></md-icon-button>
    <md-outlined-text-field class="search-bar hide" placeholder="Search packages">
      <md-icon-button slot="trailing-icon" id="search-close"><md-icon>close</md-icon></md-icon-button>
    </md-outlined-text-field>
    <div class="reload-menu">
      <md-icon-button id="reload-button">
        <md-icon>restart_alt</md-icon>
      </md-icon-button>
    </div>
    <div class="main-menu">
      <md-icon-button id="menu-button">
        <md-icon>more_vert</md-icon>
      </md-icon-button>
    </div>
  </section>

  <section class="body-content">
    <div class="update">
      <md-icon>policy</md-icon>
      <div class="update-text">
        <span>Scoop list controls known packages</span>
        <em>Unchecked apps are removed from scoop. Unknown callers still follow allow_unknown_package.</em>
      </div>
      <md-ripple></md-ripple>
    </div>
    <div class="app-list">
      <div class="loading"><md-circular-progress indeterminate></md-circular-progress></div>
    </div>
    <div class="bottom-safe-inset"></div>
  </section>

  <section class="floating-content">
    ${snackbar.html()}
    <div class="fab-container">
      <md-fab variant="primary" class="fab" id="save" label="Save">
        <md-icon slot="icon">save</md-icon>
      </md-fab>
    </div>
  </section>

  <section class="dialog-content"></section>
`

const appList = new AppList(config)
await config.read()
await appList.reloadPackages()
const appListContainer = document.querySelector<HTMLElement>('.app-list')!
appList.renderAppList(appListContainer)

const searchBar = new SearchBar(history)
const searchBarEl = document.querySelector<MdOutlinedTextField>('.search-bar')!
const searchHide = document.querySelectorAll<HTMLElement>('.search-hide')
const searchButton = document.getElementById('search-button') as MdIconButton
searchBar.init(searchBarEl, searchHide, appListContainer)
searchButton.onclick = () => searchBar.show()

const saveFab = document.getElementById('save') as MdFab
saveFab.onclick = () => {
  void saveTarget()
}

async function saveTarget(): Promise<void> {
  try {
    await appList.save()
    await appList.refresh()
    snackbar.show('Config saved')
  } catch {
    snackbar.show('Failed to save config', false)
  }
}

const mainMenu = new MainMenu()
const keybox = new Keybox(cli, config, fileSelector, snackbar)
const keyboxRepo = new KeyboxRepo(keybox, history, snackbar)
const dialogController = new DialogController(cli, config, appList)
appList.setLongPressHandler(async (packageName) => {
  if (await keybox.showAppKeyboxMenu(packageName)) {
    await appList.refresh(false)
  }
})

const mainMenuContainer = document.querySelector<HTMLElement>('.main-menu')!
mainMenu.appendTo(mainMenuContainer)
const reloadMenu = new ReloadMenu(cli, snackbar)
reloadMenu.appendTo(document.querySelector<HTMLElement>('.reload-menu')!)
mainMenu.on('menu-open', () => {
  appList.menuOpen = true
})
mainMenu.on('menu-close', () => {
  appList.menuOpen = false
})
mainMenu.on('menu-refresh', () => {
  void appList.refreshPackages()
})
mainMenu.on('menu-select-all', () => appList.selectAll())
mainMenu.on('menu-deselect-all', () => appList.deselectAll())
mainMenu.on('menu-add-system-app', () => dialogController.showSystemApp())
mainMenu.on('menu-keybox-aosp', () => {
  void keybox.setAospKey()
})
mainMenu.on('menu-keybox-unknown', () => {
  void keybox.setUnknownKey()
})
mainMenu.on('menu-keybox-alwaysstrong', () => {
  void keybox.setAlwaysStrongKey()
})
mainMenu.on('menu-keybox-local', () => {
  void keybox.setLocalKey()
})
mainMenu.on('menu-keybox-repo', () => keyboxRepo.show())
mainMenu.on('menu-trust-settings', () => dialogController.showTrust())
mainMenu.on('menu-core-settings', () => dialogController.showCore())
mainMenu.on('menu-injector-settings', () => dialogController.showInjector())
mainMenu.on('menu-filter-settings', () => dialogController.showFilter())
mainMenu.on('menu-intercept-settings', () => dialogController.showIntercept())
mainMenu.on('menu-device-settings', () => dialogController.showDevice())
mainMenu.on('menu-crypto-settings', () => dialogController.showCrypto())
mainMenu.on('menu-trust-record', () => dialogController.showRuntime())
mainMenu.on('menu-help', () => dialogController.showHelp())
mainMenu.on('menu-about', () => dialogController.showAbout())
if (!Keybox.isKeygenAvailable() && !import.meta.env.DEV) {
  const keyboxUnknown = document.getElementById('keybox-unknown')
  if (keyboxUnknown) keyboxUnknown.style.display = 'none'
}

keybind.on('keybind-select-all', () => appList.selectAll())
keybind.on('keybind-deselect-all', () => appList.deselectAll())
keybind.on('keybind-search', () => searchBar.show())
keybind.on('keybind-save', () => {
  void saveTarget()
})
keybind.on('keybind-esc', () => history.back())

const dialogContent = document.querySelector<HTMLElement>('.dialog-content')!
fileSelector.appendTo(dialogContent)
keybox.appendTo(dialogContent)
keyboxRepo.appendTo(dialogContent)
keybox.custom.renderEntries()
dialogController.appendAll(dialogContent)
dialogContent.querySelectorAll<MdDialog>('md-dialog').forEach((dialog, index) => {
  const id = dialog.id || `md-dialog-${index}`
  dialog.addEventListener('open', () => history.push(id, () => dialog.close()))
  dialog.addEventListener('closed', () => history.consume(id))
})

let lastScrollY = window.scrollY
window.onscroll = () => {
  document.querySelectorAll('md-menu').forEach((menu) => menu.close())
  document.querySelector('.header')?.classList.toggle('scroll', window.scrollY > 10)
  const floating = document.querySelector('.floating-content')
  const fab = document.querySelector('.fab')
  const hide = window.scrollY > lastScrollY && window.scrollY > 48
  floating?.classList.toggle('fab-hide', hide)
  fab?.classList.toggle('fab-hide', hide)
  lastScrollY = window.scrollY
}
