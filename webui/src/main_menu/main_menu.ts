import type { MdIconButton, MdMenuItem, MdMenu, MdSubMenu } from '@material/web/all'
import './main_menu.scss'

const MENU_ITEMS: Array<[string, string]> = [
  ['add-system-app', 'menu-add-system-app'],
  ['keybox-aosp', 'menu-keybox-aosp'],
  ['keybox-unknown', 'menu-keybox-unknown'],
  ['keybox-alwaysstrong', 'menu-keybox-alwaysstrong'],
  ['keybox-local', 'menu-keybox-local'],
  ['keybox-repo', 'menu-keybox-repo'],
  ['keybox-custom', 'menu-keybox-custom'],
  ['trust-settings', 'menu-trust-settings'],
  ['core-settings', 'menu-core-settings'],
  ['injector-settings', 'menu-injector-settings'],
  ['filter-settings', 'menu-filter-settings'],
  ['intercept-settings', 'menu-intercept-settings'],
  ['device-settings', 'menu-device-settings'],
  ['crypto-settings', 'menu-crypto-settings'],
  ['trust-record', 'menu-trust-record'],
  ['help', 'menu-help'],
  ['about', 'menu-about'],
]

export class MainMenu {
  #callbacks = new Map<string, Array<() => void>>()

  appendTo(container: HTMLElement): void {
    container.appendChild(this.#getElement(container))
  }

  #getElement(anchorContainer: HTMLElement): DocumentFragment {
    const template = document.createElement('template')
    template.innerHTML = /* html */ `
      <md-menu id="menu-options" anchor="menu-button">
        <div class="menu-item-button-container">
          <md-filled-tonal-icon-button id="select-all"><md-icon>select_all</md-icon></md-filled-tonal-icon-button>
          <md-filled-tonal-icon-button id="deselect-all"><md-icon>deselect</md-icon></md-filled-tonal-icon-button>
          <md-filled-tonal-icon-button id="refresh"><md-icon>refresh</md-icon></md-filled-tonal-icon-button>
        </div>
        <md-divider role="separator" tabindex="-1"></md-divider>
        <md-menu-item id="add-system-app">
          <div slot="headline">Add System App</div>
        </md-menu-item>
        <md-divider role="separator" tabindex="-1"></md-divider>
        <md-sub-menu hover-close-delay="0" id="keybox-menu">
          <md-menu-item slot="item" class="sub-menu-entry">
            <div slot="headline">Keybox</div>
            <md-icon slot="end">key</md-icon>
          </md-menu-item>
          <md-menu positioning="popover" slot="menu" x-offset="2">
            <md-menu-item id="keybox-aosp">
              <div slot="headline">AOSP</div>
            </md-menu-item>
            <md-menu-item id="keybox-unknown">
              <div slot="headline">Self-Signed</div>
            </md-menu-item>
            <md-menu-item id="keybox-alwaysstrong">
              <div slot="headline">AlwaysStrong</div>
            </md-menu-item>
            <md-menu-item id="keybox-local">
              <div slot="headline">Local File</div>
            </md-menu-item>
            <md-menu-item id="keybox-repo">
              <div slot="headline">Repo</div>
              <md-icon slot="end">open_in_new</md-icon>
            </md-menu-item>
            <md-divider role="separator" tabindex="-1"></md-divider>
            <md-menu-item id="keybox-custom" class="icon-item">
              <div class="icon-button-item">
                <md-filled-tonal-icon-button><md-icon>add</md-icon></md-filled-tonal-icon-button>
              </div>
            </md-menu-item>
          </md-menu>
        </md-sub-menu>
        <md-menu-item id="trust-settings">
          <div slot="headline">Trust Settings</div>
        </md-menu-item>
        <md-menu-item id="core-settings">
          <div slot="headline">Core Settings</div>
        </md-menu-item>
        <md-menu-item id="injector-settings">
          <div slot="headline">Injector Settings</div>
        </md-menu-item>
        <md-menu-item id="filter-settings">
          <div slot="headline">Package Filter</div>
        </md-menu-item>
        <md-menu-item id="intercept-settings">
          <div slot="headline">Intercept Matrix</div>
        </md-menu-item>
        <md-menu-item id="device-settings">
          <div slot="headline">Device Properties</div>
        </md-menu-item>
        <md-menu-item id="crypto-settings">
          <div slot="headline">Crypto Seeds</div>
        </md-menu-item>
        <md-menu-item id="trust-record">
          <div slot="headline">Trust Record</div>
        </md-menu-item>
        <md-divider role="separator" tabindex="-1"></md-divider>
        <md-menu-item id="help">
          <div slot="headline">Help</div>
        </md-menu-item>
        <md-menu-item id="about">
          <div slot="headline">About</div>
        </md-menu-item>
      </md-menu>
    `

    const fragment = template.content
    const menuOptions = fragment.querySelector('#menu-options') as MdMenu

    anchorContainer.querySelector<MdIconButton>('#menu-button')!.onclick = () => {
      menuOptions.open = !menuOptions.open
    }

    menuOptions.addEventListener('opened', () => this.#emit('menu-open'))
    menuOptions.addEventListener('closed', () => this.#emit('menu-close'))

    const quickActions: Array<[string, string]> = [
      ['select-all', 'menu-select-all'],
      ['deselect-all', 'menu-deselect-all'],
      ['refresh', 'menu-refresh'],
    ]

    quickActions.forEach(([id, event]) => {
      const el = fragment.querySelector<HTMLElement>(`#${id}`)
      if (!el) return
      el.onclick = () => {
        this.#emit(event)
        menuOptions.open = false
      }
    })

    MENU_ITEMS.forEach(([id, event]) => {
      const el = fragment.querySelector<MdMenuItem>(`#${id}`)
      if (!el) return
      el.onclick = () => {
        this.#emit(event)
        menuOptions.open = false
      }
    })

    let subMenuOpen = false
    fragment.querySelectorAll('.sub-menu-entry').forEach((entry) => {
      const item = entry as MdMenuItem
      const menu = item.parentElement as MdSubMenu
      item.onclick = (event) => {
        event.stopPropagation()
        subMenuOpen = !subMenuOpen
        subMenuOpen ? menu.show() : menu.close()
      }
      menu.querySelector('md-menu')?.addEventListener('opening', () => {
        subMenuOpen = true
      })
      menu.querySelector('md-menu')?.addEventListener('closing', () => {
        subMenuOpen = false
      })
    })

    return fragment
  }

  on(event: string, callback: () => void): void {
    const cbs = this.#callbacks.get(event) ?? []
    cbs.push(callback)
    this.#callbacks.set(event, cbs)
  }

  #emit(event: string): void {
    this.#callbacks.get(event)?.forEach((cb) => cb())
  }
}
