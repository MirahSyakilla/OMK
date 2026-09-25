import '@material/web/chips/assist-chip.js'
import '@material/web/chips/chip-set.js'
import '@material/web/checkbox/checkbox.js'
import '@material/web/progress/circular-progress.js'
import '@material/web/dialog/dialog.js'
import '@material/web/divider/divider.js'
import '@material/web/fab/fab.js'
import '@material/web/button/filled-button.js'
import '@material/web/iconbutton/filled-tonal-icon-button.js'
import '@material/web/icon/icon.js'
import '@material/web/iconbutton/icon-button.js'
import '@material/web/menu/menu.js'
import '@material/web/menu/menu-item.js'
import '@material/web/button/outlined-button.js'
import '@material/web/select/outlined-select.js'
import '@material/web/textfield/outlined-text-field.js'
import '@material/web/radio/radio.js'
import '@material/web/ripple/ripple.js'
import '@material/web/select/select-option.js'
import '@material/web/menu/sub-menu.js'
import '@material/web/switch/switch.js'
import '@material/web/button/text-button.js'
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
  <div class="app-layout">
    <section class="header">
      <div class="header-title-group search-hide">
        <div id="title" class="hide"></div>
        <div id="title-status" class="title-pill title-pill-ok title-pill--brand">
          <span class="title-pill-label">OhMyKeymint</span>
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

    <main>
      <div id="pages" class="carousel-track">
        <!-- Tab 0: Apps -->
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

        <!-- Tab 1: Keybox -->
        <section class="page" id="keybox-page"></section>

        <!-- Tab 2: Play Integrity -->
        <section class="page" id="integrity-page"></section>

        <!-- Tab 3: Settings -->
        <section class="page" id="settings-page"></section>
      </div>
    </main>

    <nav class="dock" role="tablist" aria-label="Main Navigation">
      <div class="nav-indicator"></div>
      <button class="nav-tab nav-tab--active" data-tab="0" role="tab" aria-label="Apps">
        <md-icon class="nav-icon">apps</md-icon>
        <span class="nav-label">Apps</span>
      </button>
      <button class="nav-tab" data-tab="1" role="tab" aria-label="Keybox">
        <md-icon class="nav-icon">vpn_key</md-icon>
        <span class="nav-label">Keybox</span>
      </button>
      <button class="nav-tab" data-tab="2" role="tab" aria-label="Play Integrity">
        <md-icon class="nav-icon">verified_user</md-icon>
        <span class="nav-label">Integrity</span>
      </button>
      <button class="nav-tab" data-tab="3" role="tab" aria-label="Settings">
        <md-icon class="nav-icon">settings</md-icon>
        <span class="nav-label">Settings</span>
      </button>
    </nav>
  </div>

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

// Tab 0: Apps List
const appListContainer = document.querySelector<HTMLElement>('.app-list')!
await appList.reloadPackages()
appList.renderAppList(appListContainer)

// Tab 1: Play Integrity Screen
const integrityPage = document.querySelector<HTMLElement>('#integrity-page')!
const integrityScreen = new IntegrityScreen(cli, config, snackbar)
integrityScreen.render(integrityPage)

// Tab 2: Keybox Screen
const keyboxPage = document.querySelector<HTMLElement>('#keybox-page')!
const keyboxScreen = new KeyboxScreen(keybox, keyboxRepo, cli, config, snackbar, history)
keyboxScreen.render(keyboxPage)

// Tab 3: Settings Screen
const settingsPage = document.querySelector<HTMLElement>('#settings-page')!
const settingsScreen = new SettingsScreen(dialogController, config)
settingsScreen.render(settingsPage)

// Shell Navigation
const track = document.querySelector<HTMLElement>('#pages')!
const dock = document.querySelector<HTMLElement>('.dock')!
const titleEl = document.querySelector<HTMLElement>('#title')!
const navigation = new Navigation(track, dock, titleEl)

// Controls visibility per tab
const searchButton = document.getElementById('search-button') as MdIconButton
const mainMenuContainer = document.querySelector<HTMLElement>('.main-menu')!
const fabContainer = document.querySelector<HTMLElement>('.fab-container')!

function updateTabVisibility(index: number): void {
  const isApps = index === 0
  searchButton.style.display = isApps ? '' : 'none'
  mainMenuContainer.style.display = isApps ? '' : 'none'
  fabContainer.style.display = isApps ? '' : 'none'
  searchButton.classList.toggle('hide', !isApps)
  mainMenuContainer.classList.toggle('hide', !isApps)
  fabContainer.classList.toggle('fab-hide', !isApps)
  if (isApps) {
    document.querySelector('.fab')?.classList.remove('fab-hide')
  }
}

navigation.onTabChanged((index) => {
  dock.classList.remove('dock-hide')
  updateTabVisibility(index)

  if (index === 1) {
    window.setTimeout(() => void keyboxScreen.refresh(), 360)
  } else if (index === 2) {
    window.setTimeout(() => void integrityScreen.load(), 360)
  }
})

// Initialize visibility immediately for the active tab (Apps at index 0)
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
})
mainMenu.on('menu-select-all', () => appList.selectAll())
mainMenu.on('menu-deselect-all', () => appList.deselectAll())
mainMenu.on('menu-add-system-app', () => dialogController.showSystemApp())
mainMenu.on('menu-keybox-manage', () => {
  void keybox.showManage()
})
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
mainMenu.on('menu-integrity-settings', () => dialogController.showIntegrity())
mainMenu.on('menu-help', () => dialogController.showHelp())
mainMenu.on('menu-about', () => dialogController.showAbout())
if (!Keybox.isKeygenAvailable() && !import.meta.env.DEV) {
  const keyboxUnknown = document.getElementById('keybox-unknown')
  if (keyboxUnknown) keyboxUnknown.style.display = 'none'
}

// Keybinds
keybind.on('keybind-select-all', () => appList.selectAll())
keybind.on('keybind-deselect-all', () => appList.deselectAll())
keybind.on('keybind-search', () => {
  if (navigation.getActiveIndex() === 0) searchBar.show()
})
keybind.on('keybind-save', () => {
  if (navigation.getActiveIndex() === 0) void saveTarget()
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
  dialog.addEventListener('open', () => {
    ;(document.activeElement as HTMLElement)?.blur()
    document.querySelectorAll('.card-pressed').forEach((el) => el.classList.remove('card-pressed'))
    history.push(id, () => dialog.close())
  })
  dialog.addEventListener('closed', () => history.consume(id))
})

// Android Back Navigation Rule:
// Back from any tab other than the first returns to the first tab instead of
// leaving the WebUI. The synthetic entry lives on the same stack as dialogs and
// menus, so dismissing a dialog never drains it.
const TAB_BACK_KEY = 'tab-back'
let tabBackTracked = false
navigation.onTabChanged((index) => {
  if (index === 0) {
    // Reached the first tab by tapping the dock: the entry is now stale, and
    // dropping it lets the next back press leave the WebUI as expected.
    if (tabBackTracked) {
      tabBackTracked = false
      history.consume(TAB_BACK_KEY)
    }
    return
  }
  if (tabBackTracked) return
  tabBackTracked = true
  history.push(TAB_BACK_KEY, () => {
    tabBackTracked = false
    navigation.switchToTab(0, true)
  })
})

// Header scroll elevation & Floating Dock/FAB hide
let lastScrollY = window.scrollY
window.onscroll = () => {
  if (activeTouchCard) {
    activeTouchCard.classList.remove('card-pressed')
    activeTouchCard = null
  }
  document.querySelectorAll('.card-pressed').forEach((el) => el.classList.remove('card-pressed'))
  document.querySelectorAll('md-menu').forEach((menu) => menu.close())
  document.querySelector('.header')?.classList.toggle('scroll', window.scrollY > 10)
  const hide = window.scrollY > lastScrollY && window.scrollY > 48
  dock.classList.toggle('dock-hide', hide)
  if (navigation.getActiveIndex() === 0) {
    const fab = document.querySelector('.fab')
    fabContainer.classList.toggle('fab-hide', hide)
    fab?.classList.toggle('fab-hide', hide)
  }
  lastScrollY = window.scrollY
}

// Tactile Card Touch Interaction Manager:
// On mobile devices, native CSS :active and :hover stick during scrolls because
// Chromium WebView suppresses touchend on scroll gestures. We manage .card-pressed
// dynamically so scrolls cancel the press effect instantly.
let activeTouchCard: HTMLElement | null = null

document.addEventListener(
  'touchstart',
  (e) => {
    if (activeTouchCard) {
      activeTouchCard.classList.remove('card-pressed')
      activeTouchCard = null
    }
    document.querySelectorAll('.card-pressed').forEach((el) => el.classList.remove('card-pressed'))
    const card = (e.target as Element | null)?.closest<HTMLElement>(
      '.card, .switch-row, .settings-row, .kb-slot-card, .kac-tile, .update',
    )
    if (card) {
      activeTouchCard = card
      card.classList.add('card-pressed')
    }
  },
  { passive: true },
)

window.addEventListener('blur', () => {
  if (activeTouchCard) {
    activeTouchCard.classList.remove('card-pressed')
    activeTouchCard = null
  }
  document.querySelectorAll('.card-pressed').forEach((el) => el.classList.remove('card-pressed'))
})

document.addEventListener(
  'touchmove',
  () => {
    if (activeTouchCard) {
      activeTouchCard.classList.remove('card-pressed')
      activeTouchCard = null
    }
  },
  { passive: true },
)

document.addEventListener(
  'touchend',
  () => {
    if (activeTouchCard) {
      const el = activeTouchCard
      activeTouchCard = null
      window.setTimeout(() => el.classList.remove('card-pressed'), 120)
    }
  },
  { passive: true },
)

document.addEventListener(
  'touchcancel',
  () => {
    if (activeTouchCard) {
      activeTouchCard.classList.remove('card-pressed')
      activeTouchCard = null
    }
  },
  { passive: true },
)
