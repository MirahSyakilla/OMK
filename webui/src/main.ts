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
import { TitleStatus } from './title_status'
import { applyDialogAnimation } from './dialog/animation'
import { Navigation } from './navigation'
import { HomeScreen } from './home/home'
import { IntegrityScreen } from './integrity_screen/integrity_screen'
import { KeyboxScreen } from './keybox_screen/keybox_screen'
import { SettingsScreen } from './settings_screen/settings_screen'
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
    <div class="header-title-group">
      <div id="title">Overview</div>
      <div id="title-status" class="title-pill title-pill-ok">
        <span class="title-pill-label">OMK</span>
      </div>
    </div>
    <div class="spacer"></div>
    <md-icon-button id="search-button" class="search-hide hide"><md-icon>search</md-icon></md-icon-button>
    <md-outlined-text-field class="search-bar hide" placeholder="Search packages">
      <md-icon-button slot="trailing-icon" id="search-close"><md-icon>close</md-icon></md-icon-button>
    </md-outlined-text-field>
    <div class="reload-menu">
      <md-icon-button id="reload-button">
        <md-icon>restart_alt</md-icon>
      </md-icon-button>
    </div>
    <div class="main-menu hide">
      <md-icon-button id="menu-button">
        <md-icon>more_vert</md-icon>
      </md-icon-button>
    </div>
  </section>

  <div id="pages-container">
    <div id="pages" class="carousel-track">
      <!-- Tab 0: Home / Overview -->
      <section class="page" id="home-page"></section>

      <!-- Tab 1: Apps -->
      <section class="page" id="apps-page">
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
      </section>

      <!-- Tab 2: Play Integrity -->
      <section class="page" id="integrity-page"></section>

      <!-- Tab 3: Keybox -->
      <section class="page" id="keybox-page"></section>

      <!-- Tab 4: Settings -->
      <section class="page" id="settings-page"></section>
    </div>
  </div>

  <nav class="dock" role="tablist" aria-label="Main Navigation">
    <div class="nav-indicator"></div>
    <button class="nav-tab nav-tab--active" data-tab="0" role="tab" aria-label="Home">
      <md-icon class="nav-icon">home</md-icon>
      <span class="nav-label">Home</span>
    </button>
    <button class="nav-tab" data-tab="1" role="tab" aria-label="Apps">
      <md-icon class="nav-icon">apps</md-icon>
      <span class="nav-label">Apps</span>
    </button>
    <button class="nav-tab" data-tab="2" role="tab" aria-label="Play Integrity">
      <md-icon class="nav-icon">verified_user</md-icon>
      <span class="nav-label">Integrity</span>
    </button>
    <button class="nav-tab" data-tab="3" role="tab" aria-label="Keybox">
      <md-icon class="nav-icon">vpn_key</md-icon>
      <span class="nav-label">Keybox</span>
    </button>
    <button class="nav-tab" data-tab="4" role="tab" aria-label="Settings">
      <md-icon class="nav-icon">settings</md-icon>
      <span class="nav-label">Settings</span>
    </button>
  </nav>

  <section class="floating-content">
    ${snackbar.html()}
    <div class="fab-container fab-hide">
      <md-fab variant="primary" class="fab" id="save" label="Save">
        <md-icon slot="icon">save</md-icon>
      </md-fab>
    </div>
  </section>

  <section class="dialog-content"></section>
`

await config.read()

// Instantiate AppList and controllers
const appList = new AppList(config)
const keybox = new Keybox(cli, config, fileSelector, snackbar)
const keyboxRepo = new KeyboxRepo(keybox, history, snackbar)
const dialogController = new DialogController(cli, config, appList, snackbar)

await keybox.loadSlotNames()
appList.setSlotLabel((slot) => keybox.slotLabel(slot))
keybox.onNamesChanged(() => {
  void appList.refresh(false)
})
appList.setLongPressHandler(async (packageName) => {
  if (await keybox.showAppKeyboxMenu(packageName)) {
    await appList.refresh(false)
  }
})

// Tab 0: Home Dashboard
const homePage = document.querySelector<HTMLElement>('#home-page')!
const homeScreen = new HomeScreen(cli, config, keybox, snackbar)
homeScreen.render(homePage)

// Tab 1: Apps List
const appListContainer = document.querySelector<HTMLElement>('.app-list')!
await appList.reloadPackages()
appList.renderAppList(appListContainer)

// Tab 2: Play Integrity Screen
const integrityPage = document.querySelector<HTMLElement>('#integrity-page')!
const integrityScreen = new IntegrityScreen(cli, config, snackbar, () => {
  void homeScreen.refresh()
})
integrityScreen.render(integrityPage)

// Tab 3: Keybox Screen
const keyboxPage = document.querySelector<HTMLElement>('#keybox-page')!
const keyboxScreen = new KeyboxScreen(keybox, keyboxRepo, cli, config, snackbar)
keyboxScreen.render(keyboxPage)

// Tab 4: Settings Screen
const settingsPage = document.querySelector<HTMLElement>('#settings-page')!
const settingsScreen = new SettingsScreen(dialogController, config)
settingsScreen.render(settingsPage)

// Shell Navigation
const track = document.querySelector<HTMLElement>('#pages')!
const dock = document.querySelector<HTMLElement>('.dock')!
const titleEl = document.querySelector<HTMLElement>('#title')!
const navigation = new Navigation(track, dock, titleEl)
homeScreen.setNavigation(navigation)

// Controls visibility per tab
const searchButton = document.getElementById('search-button') as MdIconButton
const mainMenuContainer = document.querySelector<HTMLElement>('.main-menu')!
const fabContainer = document.querySelector<HTMLElement>('.fab-container')!

function updateTabVisibility(index: number): void {
  const isApps = index === 1
  searchButton.style.display = isApps ? '' : 'none'
  mainMenuContainer.style.display = isApps ? '' : 'none'
  fabContainer.style.display = isApps ? '' : 'none'
  searchButton.classList.toggle('hide', !isApps)
  mainMenuContainer.classList.toggle('hide', !isApps)
  fabContainer.classList.toggle('fab-hide', !isApps)
}

navigation.onTabChanged((index) => {
  updateTabVisibility(index)

  if (index === 0) {
    void homeScreen.refresh()
  } else if (index === 2) {
    void integrityScreen.load()
  } else if (index === 3) {
    void keyboxScreen.refresh()
  }
})

// Initialize visibility immediately for active tab (Home at index 0)
updateTabVisibility(navigation.getActiveIndex())

// Search Bar (Apps Tab)
const searchBar = new SearchBar(history)
const searchBarEl = document.querySelector<MdOutlinedTextField>('.search-bar')!
const searchHide = document.querySelectorAll<HTMLElement>('.search-hide')
searchBar.init(searchBarEl, searchHide, appListContainer)
searchButton.onclick = () => searchBar.show()

// Save FAB (Apps Tab)
const saveFab = document.getElementById('save') as MdFab
saveFab.onclick = () => {
  void saveTarget()
}

async function saveTarget(): Promise<void> {
  try {
    await appList.save()
    await appList.refresh()
    void homeScreen.refresh()
    snackbar.show('Config saved')
  } catch {
    snackbar.show('Failed to save config', false)
  }
}

// Menus
const mainMenu = new MainMenu()
mainMenu.appendTo(mainMenuContainer)

const reloadMenu = new ReloadMenu(cli, snackbar)
reloadMenu.appendTo(document.querySelector<HTMLElement>('.reload-menu')!)

homeScreen.onRestart(() => {
  reloadMenu.showConfirm('all')
})
homeScreen.onShowTrustRecord(() => {
  dialogController.showRuntime()
})

new TitleStatus(cli, document.querySelector<HTMLElement>('#title-status')!).start()

// PIF Conflict Alert Dialog
const pifDialogTemplate = document.createElement('template')
pifDialogTemplate.innerHTML = /* html */ `
  <md-dialog id="external-pif-dialog" type="alert">
    <div slot="headline">${i18n.t('prompt_external_pif_title')}</div>
    <div slot="content">${i18n.t('prompt_external_pif_message')}</div>
    <div slot="actions">
      <md-filled-button id="external-pif-got-it">${i18n.t('functional_button_got_it')}</md-filled-button>
    </div>
  </md-dialog>`
const dialogContent = document.querySelector<HTMLElement>('.dialog-content')!
dialogContent.appendChild(pifDialogTemplate.content)
const externalPifDialog = document.querySelector<MdDialog>('#external-pif-dialog')!
applyDialogAnimation(externalPifDialog)
document.getElementById('external-pif-got-it')!.onclick = () => externalPifDialog.close()

function showExternalPifDialog(): void {
  externalPifDialog.show()
}

async function refreshIntegrityGate(): Promise<void> {
  const status = await cli.detectIntegrityZygisk()
  const blocked = cli.isExternalPif(status.conflict)
  mainMenu.setIntegrityBlocked(blocked)
  reloadMenu.setIntegrityBlocked(blocked)
}
void refreshIntegrityGate()
reloadMenu.onBlocked(showExternalPifDialog)
mainMenu.on('menu-integrity-blocked', showExternalPifDialog)

// App list actions
mainMenu.on('menu-open', () => {
  appList.menuOpen = true
  void refreshIntegrityGate()
})
mainMenu.on('menu-close', () => {
  appList.menuOpen = false
})
mainMenu.on('menu-refresh', () => {
  void appList.refreshPackages()
  void homeScreen.refresh()
})
mainMenu.on('menu-select-all', () => appList.selectAll())
mainMenu.on('menu-deselect-all', () => appList.deselectAll())
mainMenu.on('menu-add-system-app', () => dialogController.showSystemApp())

// Keybinds
keybind.on('keybind-select-all', () => appList.selectAll())
keybind.on('keybind-deselect-all', () => appList.deselectAll())
keybind.on('keybind-search', () => {
  if (navigation.getActiveIndex() === 1) searchBar.show()
})
keybind.on('keybind-save', () => {
  if (navigation.getActiveIndex() === 1) void saveTarget()
})
keybind.on('keybind-esc', () => {
  if (navigation.getActiveIndex() !== 0) {
    navigation.switchToTab(0, true)
  } else {
    history.back()
  }
})

// Dialog and overlay registration
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

// Android Back Navigation Rule:
// If tab != 0 and no modal open -> return to tab 0 before exit
let exitStatePushed = false
navigation.onTabChanged((index) => {
  if (index !== 0 && !exitStatePushed) {
    window.history.pushState({ tab: index }, '')
    exitStatePushed = true
  }
})

window.addEventListener('popstate', () => {
  exitStatePushed = false
  if (navigation.getActiveIndex() !== 0) {
    navigation.switchToTab(0, true)
  }
})

// Header scroll elevation
let lastScrollY = window.scrollY
window.onscroll = () => {
  document.querySelectorAll('md-menu').forEach((menu) => menu.close())
  document.querySelector('.header')?.classList.toggle('scroll', window.scrollY > 10)
  if (navigation.getActiveIndex() === 1) {
    const fab = document.querySelector('.fab')
    const hide = window.scrollY > lastScrollY && window.scrollY > 48
    fabContainer.classList.toggle('fab-hide', hide)
    fab?.classList.toggle('fab-hide', hide)
  }
  lastScrollY = window.scrollY
}
