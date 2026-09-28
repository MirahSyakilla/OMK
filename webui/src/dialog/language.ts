import type { MdOutlinedSelect } from '@material/web/all'
import { i18n } from '../i18n'
import { escapeHtml } from '../html'
import { boundOptionLists } from './option_list'

/**
 * Language picker.
 *
 * Built the same way as the fingerprint picker, and it shares that picker's
 * `.picker-dialog` styling and option-list bounding, so the two read as the same
 * dialog. The only difference is that a language change has to take effect for
 * the whole app, so Apply reloads rather than writing a file.
 */
export const showLanguagePicker = (): void => {
  const dialog = document.createElement('md-dialog')
  dialog.className = 'picker-dialog'
  dialog.id = 'language-picker-dialog'
  // Scrim clicks and Escape close the dialog without running Cancel or Apply, so
  // removal is bound to the closed event rather than done in each button.
  dialog.addEventListener('closed', () => dialog.remove())

  // Each language is listed under its own name, which is how someone looking for
  // their language will recognise it.
  const entries = Object.entries(i18n.languages).sort(([, a], [, b]) => a.localeCompare(b))
  const current = i18n.lang

  const options = entries
    .map(
      ([code, name]) =>
        `<md-select-option value="${escapeHtml(code)}" ${code === current ? 'selected' : ''}><div slot="headline">${escapeHtml(name)}</div></md-select-option>`,
    )
    .join('')

  dialog.innerHTML = /* html */ `
    <div slot="headline">${escapeHtml(i18n.t('language_picker_title'))}</div>
    <form slot="content" id="lang-body" method="dialog">
      <md-outlined-select data-role="language" label="${escapeHtml(i18n.t('language_picker_label'))}" value="${escapeHtml(current)}">
        ${options}
      </md-outlined-select>
    </form>
    <div slot="actions">
      <md-text-button id="lang-cancel">${escapeHtml(i18n.t('cancel'))}</md-text-button>
      <md-filled-button id="lang-apply">${escapeHtml(i18n.t('apply'))}</md-filled-button>
    </div>
  `

  document.body.appendChild(dialog)
  boundOptionLists(dialog)
  dialog.show()

  dialog.querySelector('#lang-cancel')?.addEventListener('click', () => {
    dialog.close()
  })

  dialog.querySelector('#lang-apply')?.addEventListener('click', () => {
    // Read at click time rather than at render time: the select only holds a
    // value once it has upgraded, which it has by the time it can be clicked.
    const selected = dialog.querySelector<MdOutlinedSelect>('md-outlined-select[data-role="language"]')?.value
    if (selected) i18n.setLanguage(selected)
    dialog.close()
  })
}
