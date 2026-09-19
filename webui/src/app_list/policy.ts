import type { MdOutlinedSelect, MdOutlinedTextField, MdSwitch } from '@material/web/all'
import type { Policy, PolicySchema, SelectFieldMeta, TextFieldMeta, ButtonFieldMeta } from '../config'
import { snakeToLabel } from '../config'
import { i18n } from '../i18n'

export class PolicyEditor {
  readonly #fields: Map<string, MdOutlinedTextField | MdOutlinedSelect | MdSwitch | HTMLElement>
  readonly #schema: PolicySchema

  constructor(fieldsEl: HTMLElement, schema: PolicySchema) {
    this.#schema = schema
    this.#fields = new Map()
    for (const [key] of schema.getFields()) {
      const field = fieldsEl.querySelector<HTMLElement>(`.policy-${key}`)
      if (field) this.#fields.set(key, field)
    }
  }

  getField(key: string): MdOutlinedTextField | MdOutlinedSelect | MdSwitch | HTMLElement | undefined {
    return this.#fields.get(key)
  }

  getSchema(): PolicySchema {
    return this.#schema
  }

  bind(): void {
    for (const [key, meta] of this.#schema.getFields()) {
      if (meta.type === 'button') {
        const btn = this.#fields.get(key) as HTMLElement | undefined
        if (!btn) continue
        btn.onclick = () => (meta as ButtonFieldMeta).onClick()
        continue
      }

      if (meta.type === 'boolean' || meta.type === 'select') {
        continue
      }

      // text field
      const field = this.#fields.get(key) as MdOutlinedTextField | undefined
      if (!field) continue
      const textMeta = meta as TextFieldMeta
      field.oninput = () => {
        const val = field.value.trim().toLowerCase()
        field.value = val
        const result = textMeta.validate(val)
        if (result === true) {
          field.error = false
        } else {
          field.error = true
          if (typeof result === 'string') field.errorText = result
        }
      }
    }
  }

  isValid(): boolean {
    for (const [key, meta] of this.#schema.getFields()) {
      if (meta.type === 'select') {
        const field = this.#fields.get(key) as MdOutlinedSelect | undefined
        if (!field) continue
        const val = field.value?.trim() ?? ''
        if (val && !(meta as SelectFieldMeta).options.includes(val)) return false
        continue
      }
      if (meta.type !== 'text') continue
      const field = this.#fields.get(key) as MdOutlinedTextField | undefined
      if (!field) continue
      const val = field.value.trim()
      if (val && (meta as TextFieldMeta).validate(val) !== true) return false
    }
    return true
  }

  static html(schema: PolicySchema): string {
    const fields = schema.getFields().map(([key, meta]) => {
      if (meta.type === 'button') {
        return `<md-outlined-button class="full-width-button policy-${key}">${i18n.t(meta.label)}</md-outlined-button>`
      }

      if (meta.type === 'boolean') {
        return `<label class="switch-item outlined" for="policy-${key}">
          <md-ripple></md-ripple>
          <span>${meta.label}</span>
          <md-switch icons="true" id="policy-${key}" class="policy-${key}"${meta.defaultValue ? ' selected' : ''}></md-switch>
        </label>`
      }

      if (meta.type === 'select') {
        const options = meta.options.map((option) =>
          `<md-select-option value="${option}"><div slot="headline">${option}</div></md-select-option>`
        ).join('')
        return `<md-outlined-select class="policy-${key}" label="${meta.label}" menu-positioning="popover">${options}</md-outlined-select>`
      }

      // text field
      const textMeta = meta as TextFieldMeta
      const options = textMeta.options?.length ? ` [${textMeta.options.join('/')}]` : ''
      const hint = textMeta.placeholder ?? key
      const displayLabel = textMeta.label ?? snakeToLabel(key)
      const textarea = textMeta.textarea ? ' type="textarea" rows="4"' : ''
      const maxlength = textMeta.maxlength != null ? ` maxlength="${textMeta.maxlength}"` : ''
      return `<md-outlined-text-field class="policy-${key}" label="${displayLabel}" placeholder="${hint}${options}" autocapitalize="none"${maxlength}${textarea}></md-outlined-text-field>`
    }).join('\n')
    return fields
  }

  setPolicy(policy: Policy | null): void {
    for (const [key, meta] of this.#schema.getFields()) {
      if (meta.type === 'button') continue

      if (meta.type === 'boolean') {
        const field = this.#fields.get(key) as MdSwitch | undefined
        if (!field) continue
        field.selected = policy?.[key] === true || policy?.[key] === 'true'
        continue
      }

      if (meta.type === 'select') {
        const field = this.#fields.get(key) as MdOutlinedSelect | undefined
        if (!field) continue
        const raw = String(policy?.[key] ?? meta.defaultValue ?? meta.options[0] ?? '')
        field.value = meta.options.includes(raw) ? raw : (meta.defaultValue ?? meta.options[0] ?? '')
        continue
      }

      const field = this.#fields.get(key) as MdOutlinedTextField | undefined
      if (!field) continue
      field.value = (policy?.[key] as string) ?? ''
      if ('error' in field) field.error = false
    }
  }

  getPolicy(allowEmpty: boolean = false): Policy | null {
    const policy: Policy = {}
    let hasValue = false
    for (const [key, meta] of this.#schema.getFields()) {
      if (meta.type === 'button') continue

      if (meta.type === 'boolean') {
        const field = this.#fields.get(key) as MdSwitch | undefined
        if (!field) continue
        policy[key] = field.selected
        if (allowEmpty || field.selected) hasValue = true
        continue
      }

      if (meta.type === 'select') {
        const field = this.#fields.get(key) as MdOutlinedSelect | undefined
        if (!field) continue
        const val = field.value?.trim() ?? ''
        if (val) {
          policy[key] = val
          hasValue = true
        }
        continue
      }

      const val = (this.#fields.get(key) as MdOutlinedTextField | undefined)?.value.trim() ?? ''
      if (val) {
        policy[key] = val
        hasValue = true
      }
    }
    if ((!hasValue && !allowEmpty) || !this.isValid()) return null
    return policy
  }
}
