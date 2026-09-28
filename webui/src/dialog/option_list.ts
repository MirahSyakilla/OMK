import type { MdOutlinedSelect } from '@material/web/all'

/**
 * Bounds an `md-outlined-select` option list and gives it a visible scrollbar.
 *
 * Neither can be done from the stylesheet, because the menu is built inside the
 * select's own shadow root: `md-menu` never appears in the light DOM, so a rule
 * in the app's CSS cannot reach it at any scope. Both shadow roots are open, so
 * the rules are injected into them instead.
 *
 * The height is set on the menu, which both the menu surface and its item list
 * pick up through `max-height: inherit`. 192px is four options at the 48px
 * menu-item height.
 *
 * `scrollbar-width` is deliberately not used: WebView treats it as an overlay
 * bar that fades out shortly after scrolling stops, so nothing remains to show
 * the list continues. Styling `::-webkit-scrollbar` makes Chromium use a
 * persistent classic scrollbar instead, which stays visible.
 */
export const boundOptionList = (select: MdOutlinedSelect): void => {
  const root = select.shadowRoot
  if (!root) return

  if (!root.querySelector('style[data-omk-size]')) {
    const size = document.createElement('style')
    size.setAttribute('data-omk-size', '')
    size.textContent = 'md-menu { max-height: 192px; }'
    root.append(size)
  }

  // The menu is rendered by Lit after the select upgrades, so it is not in the
  // shadow root yet and has to wait for the first update.
  void select.updateComplete.then(() => {
    const menuRoot = root.querySelector('md-menu')?.shadowRoot
    if (!menuRoot || menuRoot.querySelector('style[data-omk-bar]')) return
    const bar = document.createElement('style')
    bar.setAttribute('data-omk-bar', '')
    bar.textContent = [
      '.items::-webkit-scrollbar { width: 8px; }',
      '.items::-webkit-scrollbar-track { background: transparent; }',
      '.items::-webkit-scrollbar-thumb { background: #9aa0a6; border-radius: 4px; }',
      '.items::-webkit-scrollbar-thumb:hover { background: #c7cace; }',
    ].join(' ')
    menuRoot.append(bar)
  })
}

/** Applies {@link boundOptionList} to every select under `root`. */
export const boundOptionLists = (root: ParentNode): void => {
  root.querySelectorAll<MdOutlinedSelect>('md-outlined-select').forEach(boundOptionList)
}
