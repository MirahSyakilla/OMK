import { parse, stringify } from 'smol-toml'
import { File } from './file'

export interface Policy {
  [key: string]: string | boolean | undefined
}

export interface TextFieldMeta {
  type?: 'text'
  label?: string
  required?: boolean
  defaultValue?: string
  options?: string[]
  maxlength?: number
  placeholder?: string
  textarea?: boolean
  validate: (value: string) => boolean | string
}

export interface BooleanFieldMeta {
  type: 'boolean'
  label: string
  defaultValue?: boolean
}

export interface ButtonFieldMeta {
  type: 'button'
  label: string
  onClick: () => void
}

export type PolicyFieldMeta = TextFieldMeta | BooleanFieldMeta | ButtonFieldMeta

export type SectionKey =
  | 'omk_main'
  | 'trust'
  | 'injector_main'
  | 'filter'
  | 'intercept'
  | 'device'
  | 'crypto'
  | 'trust_record'

export interface ConfigData {
  target?: string[]
  omk_main?: Policy
  trust?: Policy
  injector_main?: Policy
  filter?: Policy
  intercept?: Policy
  device?: Policy
  crypto?: Policy
  trust_record?: Policy
  [section: string]: Policy | string[] | undefined
}

export function snakeToLabel(key: string): string {
  return key
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (c) => c.toUpperCase())
}

export class PolicySchema {
  readonly #fields: Map<string, PolicyFieldMeta>

  constructor(fields: Record<string, PolicyFieldMeta>) {
    this.#fields = new Map(Object.entries(fields))
  }

  getField(key: string): PolicyFieldMeta | undefined {
    return this.#fields.get(key)
  }

  getFields(): [string, PolicyFieldMeta][] {
    return [...this.#fields.entries()]
  }

  validate(values: Record<string, string>): Record<string, boolean | string> {
    const result: Record<string, boolean | string> = {}
    for (const [key, meta] of this.#fields) {
      if (meta.type === 'button') continue
      if (meta.type === 'boolean') {
        result[key] = true
        continue
      }
      const value = values[key] ?? ''
      if (!value && !meta.required) {
        result[key] = true
      } else {
        result[key] = meta.validate(value)
      }
    }
    return result
  }
}

const PACKAGE_LIST_HINT = 'one package per line or comma separated'
const MAX_KEYBOX_SLOT = 1024
const HEX_64 = /^[0-9a-f]{64}$/i
const LOG_LEVELS = ['off', 'error', 'warn', 'warning', 'info', 'debug', 'trace']

function isSecurityPatch(value: string): boolean {
  return value === 'auto'
    || value === 'latest'
    || /^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$/.test(value)
}

function isHex64OrSpecial(value: string): boolean {
  return value === 'auto' || value === 'random' || HEX_64.test(value)
}

function parsePackageList(raw: string | undefined): string[] {
  if (!raw) return []
  const seen = new Set<string>()
  return raw
    .split(/[\n,]/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0)
    .filter((entry) => {
      if (seen.has(entry)) return false
      seen.add(entry)
      return true
    })
}

function formatPackageList(value: unknown): string {
  if (!Array.isArray(value)) return ''
  return value
    .map((entry) => String(entry).trim())
    .filter((entry) => entry.length > 0)
    .join('\n')
}

function prettyPrintToml(toml: string): string {
  return toml
    .replace(/^scoop = \[(.*)\]$/m, (_match, items) => {
      const parsed = String(items)
        .split(',')
        .map((item) => item.trim())
        .filter((item) => item.length > 0)
      if (parsed.length <= 2) return `scoop = [${items}]`
      return `scoop = [\n${parsed.map((item) => `  ${item}`).join(',\n')},\n]`
    })
    .replace(/^deny_packages = \[(.*)\]$/m, (_match, items) => {
      const parsed = String(items)
        .split(',')
        .map((item) => item.trim())
        .filter((item) => item.length > 0)
      if (parsed.length <= 1) return `deny_packages = [${items}]`
      return `deny_packages = [\n${parsed.map((item) => `  ${item}`).join(',\n')},\n]`
    })
}

function boolValue(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback
}

function stringValue(value: unknown, fallback: string = ''): string {
  if (value === undefined || value === null) return fallback
  return String(value)
}

function numericString(value: unknown, fallback: string): string {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return String(value)
  }
  if (typeof value === 'string' && /^\d+$/.test(value.trim())) {
    return value.trim()
  }
  return fallback
}

function recordValue(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {}
}

export const TRUST_SCHEMA = new PolicySchema({
  os_version: {
    label: 'OS Version',
    required: true,
    placeholder: '16',
    validate: (v) => /^\d+$/.test(v) || 'number only',
  },
  security_patch: {
    label: 'Security Patch',
    required: true,
    options: ['auto', 'latest'],
    placeholder: 'YYYY-MM-DD',
    validate: (v) => isSecurityPatch(v) || 'auto | latest | YYYY-MM-DD',
  },
  vb_key: {
    label: 'VB Key',
    required: true,
    options: ['auto', 'random'],
    maxlength: 64,
    placeholder: 'auto | random | 64 hex chars',
    textarea: true,
    validate: (v) => isHex64OrSpecial(v) || 'auto | random | 64 hex chars',
  },
  vb_hash: {
    label: 'VB Hash',
    required: true,
    options: ['auto', 'random'],
    maxlength: 64,
    placeholder: 'auto | random | 64 hex chars',
    textarea: true,
    validate: (v) => isHex64OrSpecial(v) || 'auto | random | 64 hex chars',
  },
  verified_boot_state: {
    type: 'boolean',
    label: 'Verified Boot State',
    defaultValue: true,
  },
  device_locked: {
    type: 'boolean',
    label: 'Device Locked',
    defaultValue: true,
  },
})

export const OMK_MAIN_SCHEMA = new PolicySchema({
  force_skip_system_biometric_hat_verification: {
    type: 'boolean',
    label: 'Skip system biometric HAT verification',
    defaultValue: false,
  },
})

export const INJECTOR_MAIN_SCHEMA = new PolicySchema({
  enabled: {
    type: 'boolean',
    label: 'Injector enabled',
    defaultValue: true,
  },
  log_level: {
    label: 'Log Level',
    required: true,
    options: LOG_LEVELS,
    placeholder: 'debug',
    validate: (v) => LOG_LEVELS.includes(v.toLowerCase()) || LOG_LEVELS.join(' | '),
  },
})

export const FILTER_SCHEMA = new PolicySchema({
  enabled: {
    type: 'boolean',
    label: 'Package filter enabled',
    defaultValue: true,
  },
  deny_packages: {
    label: 'Deny Packages',
    placeholder: PACKAGE_LIST_HINT,
    textarea: true,
    validate: () => true,
  },
  block_android_package: {
    type: 'boolean',
    label: 'Block android / android.* callers',
    defaultValue: true,
  },
  allow_unknown_package: {
    type: 'boolean',
    label: 'Allow unresolved packages',
    defaultValue: false,
  },
})

export const INTERCEPT_SCHEMA = new PolicySchema({
  get_security_level: { type: 'boolean', label: 'getSecurityLevel', defaultValue: true },
  get_key_entry: { type: 'boolean', label: 'getKeyEntry', defaultValue: true },
  update_subcomponent: { type: 'boolean', label: 'updateSubcomponent', defaultValue: true },
  list_entries: { type: 'boolean', label: 'listEntries', defaultValue: true },
  delete_key: { type: 'boolean', label: 'deleteKey', defaultValue: true },
  grant: { type: 'boolean', label: 'grant', defaultValue: true },
  ungrant: { type: 'boolean', label: 'ungrant', defaultValue: true },
  get_number_of_entries: { type: 'boolean', label: 'getNumberOfEntries', defaultValue: true },
  list_entries_batched: { type: 'boolean', label: 'listEntriesBatched', defaultValue: true },
  get_supplementary_attestation_info: {
    type: 'boolean',
    label: 'getSupplementaryAttestationInfo',
    defaultValue: true,
  },
})

export const DEVICE_SCHEMA = new PolicySchema({
  brand: {
    label: 'Brand',
    required: true,
    placeholder: 'Google',
    validate: (v) => v.length > 0 || 'required',
  },
  device: {
    label: 'Device',
    required: true,
    placeholder: 'generic',
    validate: (v) => v.length > 0 || 'required',
  },
  product: {
    label: 'Product',
    required: true,
    placeholder: 'generic',
    validate: (v) => v.length > 0 || 'required',
  },
  manufacturer: {
    label: 'Manufacturer',
    required: true,
    placeholder: 'Google',
    validate: (v) => v.length > 0 || 'required',
  },
  model: {
    label: 'Model',
    required: true,
    placeholder: 'generic',
    validate: (v) => v.length > 0 || 'required',
  },
  serial: {
    label: 'Serial',
    required: true,
    placeholder: 'ABC12345678ABC',
    validate: (v) => v.length > 0 || 'required',
  },
  overrideTelephonyProperties: {
    type: 'boolean',
    label: 'Override telephony identifiers',
    defaultValue: false,
  },
  meid: {
    label: 'MEID',
    placeholder: 'optional',
    validate: () => true,
  },
  imei: {
    label: 'IMEI',
    placeholder: 'optional',
    validate: () => true,
  },
  imei2: {
    label: 'IMEI 2',
    placeholder: 'optional',
    validate: () => true,
  },
})

export const CRYPTO_SCHEMA = new PolicySchema({
  root_kek_seed: {
    label: 'Root KEK Seed',
    required: true,
    maxlength: 64,
    textarea: true,
    placeholder: '64 hex chars',
    validate: (v) => HEX_64.test(v) || '64 hex chars',
  },
  kak_seed: {
    label: 'KAK Seed',
    required: true,
    maxlength: 64,
    textarea: true,
    placeholder: '64 hex chars',
    validate: (v) => HEX_64.test(v) || '64 hex chars',
  },
  shared_secret_seed: {
    label: 'Shared Secret Seed',
    required: true,
    maxlength: 64,
    textarea: true,
    placeholder: '64 hex chars',
    validate: (v) => HEX_64.test(v) || '64 hex chars',
  },
  shared_secret_nonce: {
    label: 'Shared Secret Nonce',
    required: true,
    maxlength: 64,
    textarea: true,
    placeholder: '64 hex chars',
    validate: (v) => HEX_64.test(v) || '64 hex chars',
  },
  auth_token_hmac_key: {
    label: 'Auth Token HMAC Key',
    maxlength: 64,
    textarea: true,
    placeholder: 'blank or 64 hex chars',
    validate: (v) => !v || HEX_64.test(v) || 'blank | 64 hex chars',
  },
})

export const SECTION_TITLES: Record<SectionKey, string> = {
  omk_main: 'Core Settings',
  trust: 'Trust Settings',
  injector_main: 'Injector Settings',
  filter: 'Package Filter',
  intercept: 'Intercept Matrix',
  device: 'Device Properties',
  crypto: 'Crypto Seeds',
  trust_record: 'Trust Record',
}

export const SECTION_SCHEMAS: Record<Exclude<SectionKey, 'trust_record'>, PolicySchema> = {
  omk_main: OMK_MAIN_SCHEMA,
  trust: TRUST_SCHEMA,
  injector_main: INJECTOR_MAIN_SCHEMA,
  filter: FILTER_SCHEMA,
  intercept: INTERCEPT_SCHEMA,
  device: DEVICE_SCHEMA,
  crypto: CRYPTO_SCHEMA,
}

export class Config {
  readonly identity = 'OMK'

  protected readonly CONFIG_PATH = '/data/misc/keystore/omk'
  protected readonly CONFIG_FILE = `${this.CONFIG_PATH}/config.toml`
  protected readonly INJECTOR_FILE = `${this.CONFIG_PATH}/injector.toml`

  protected readonly perAppConfig = false
  protected readonly appMode = false

  readonly policySchema = TRUST_SCHEMA

  #data: ConfigData = {}
  #injector: Record<string, unknown> | null = null
  #omkConfig: Record<string, unknown> | null = null
  #keyboxSlots: Record<string, number> = {}

  async read(): Promise<void> {
    if (import.meta.env.DEV) {
      this.#keyboxSlots = {
        'io.github.vvb2060.keyattestation': 1,
        'com.google.android.gms': 2,
      }
      this.#data = {
        target: [
          'io.github.vvb2060.keyattestation',
          'com.google.android.gms',
          'net.one97.paytm',
        ],
        omk_main: {
          force_skip_system_biometric_hat_verification: false,
        },
        trust: {
          os_version: '16',
          security_patch: 'auto',
          vb_key: 'auto',
          vb_hash: 'auto',
          verified_boot_state: true,
          device_locked: true,
        },
        injector_main: {
          enabled: true,
          log_level: 'debug',
        },
        filter: {
          enabled: true,
          deny_packages: '',
          block_android_package: true,
          allow_unknown_package: false,
        },
        intercept: {
          get_security_level: true,
          get_key_entry: true,
          update_subcomponent: true,
          list_entries: true,
          delete_key: true,
          grant: true,
          ungrant: true,
          get_number_of_entries: true,
          list_entries_batched: true,
          get_supplementary_attestation_info: true,
        },
        device: {
          brand: 'Google',
          device: 'generic',
          product: 'generic',
          manufacturer: 'Google',
          model: 'Pixel',
          serial: 'ABC12345678ABC',
          overrideTelephonyProperties: false,
          meid: '',
          imei: '',
          imei2: '',
        },
        crypto: {
          root_kek_seed: '0'.repeat(64),
          kak_seed: '1'.repeat(64),
          shared_secret_seed: '2'.repeat(64),
          shared_secret_nonce: '3'.repeat(64),
          auth_token_hmac_key: '',
        },
        trust_record: {
          vb_key: 'auto',
          vb_key_source: 'property',
          vb_hash: 'auto',
          vb_hash_source: 'original',
          build_fingerprint: 'example/device',
          slot_suffix: '_a',
          original_security_patch: '2026-07-05',
        },
      }
      return
    }

    const data: ConfigData = {}

    try {
      const raw = await File.read(this.INJECTOR_FILE)
      this.#injector = parse(raw) as Record<string, unknown>
    } catch {
      this.#injector = {}
    }

    try {
      const raw = await File.read(this.CONFIG_FILE)
      this.#omkConfig = parse(raw) as Record<string, unknown>
    } catch {
      this.#omkConfig = {}
    }

    const injector = this.#injector ?? {}
    const injectorMain = recordValue(injector.main)
    const injectorFilter = recordValue(injector.filter)
    const injectorIntercept = recordValue(injector.intercept)
    const scoopDetails = recordValue(injector.scoop_details)

    this.#keyboxSlots = {}
    for (const [packageName, rawDetails] of Object.entries(scoopDetails)) {
      const slot = recordValue(rawDetails).keybox_slot
      if (typeof slot === 'number' && Number.isInteger(slot) && slot > 0 && slot <= MAX_KEYBOX_SLOT) {
        this.#keyboxSlots[packageName] = slot
      }
    }

    data.target = Array.isArray(injector.scoop)
      ? injector.scoop.map((entry) => String(entry).trim()).filter((entry) => entry.length > 0)
      : []
    data.injector_main = {
      enabled: boolValue(injectorMain.enabled, true),
      log_level: stringValue(injectorMain.log_level, 'debug'),
    }
    data.filter = {
      enabled: boolValue(injectorFilter.enabled, true),
      deny_packages: formatPackageList(injectorFilter.deny_packages),
      block_android_package: boolValue(injectorFilter.block_android_package, true),
      allow_unknown_package: boolValue(injectorFilter.allow_unknown_package, false),
    }
    data.intercept = {
      get_security_level: boolValue(injectorIntercept.get_security_level, true),
      get_key_entry: boolValue(injectorIntercept.get_key_entry, true),
      update_subcomponent: boolValue(injectorIntercept.update_subcomponent, true),
      list_entries: boolValue(injectorIntercept.list_entries, true),
      delete_key: boolValue(injectorIntercept.delete_key, true),
      grant: boolValue(injectorIntercept.grant, true),
      ungrant: boolValue(injectorIntercept.ungrant, true),
      get_number_of_entries: boolValue(injectorIntercept.get_number_of_entries, true),
      list_entries_batched: boolValue(injectorIntercept.list_entries_batched, true),
      get_supplementary_attestation_info: boolValue(
        injectorIntercept.get_supplementary_attestation_info,
        true,
      ),
    }

    const omkConfig = this.#omkConfig ?? {}
    const omkMain = recordValue(omkConfig.main)
    const trust = recordValue(omkConfig.trust)
    const device = recordValue(omkConfig.device)
    const crypto = recordValue(omkConfig.crypto)
    const trustRecord = recordValue(omkConfig.trust_record)

    data.omk_main = {
      force_skip_system_biometric_hat_verification: boolValue(
        omkMain.force_skip_system_biometric_hat_verification,
        false,
      ),
    }
    data.trust = {
      os_version: numericString(trust.os_version, '16'),
      security_patch: stringValue(trust.security_patch, 'auto'),
      vb_key: stringValue(trust.vb_key, 'auto'),
      vb_hash: stringValue(trust.vb_hash, 'auto'),
      verified_boot_state: boolValue(trust.verified_boot_state, true),
      device_locked: boolValue(trust.device_locked, true),
    }
    data.device = {
      brand: stringValue(device.brand, 'Google'),
      device: stringValue(device.device, 'generic'),
      product: stringValue(device.product, 'generic'),
      manufacturer: stringValue(device.manufacturer, 'Google'),
      model: stringValue(device.model, 'generic'),
      serial: stringValue(device.serial, 'ABC12345678ABC'),
      overrideTelephonyProperties: boolValue(device.overrideTelephonyProperties, false),
      meid: stringValue(device.meid),
      imei: stringValue(device.imei),
      imei2: stringValue(device.imei2),
    }
    data.crypto = {
      root_kek_seed: stringValue(crypto.root_kek_seed),
      kak_seed: stringValue(crypto.kak_seed),
      shared_secret_seed: stringValue(crypto.shared_secret_seed),
      shared_secret_nonce: stringValue(crypto.shared_secret_nonce),
      auth_token_hmac_key: stringValue(crypto.auth_token_hmac_key),
    }
    data.trust_record = {
      vb_key: stringValue(trustRecord.vb_key),
      vb_key_source: stringValue(trustRecord.vb_key_source),
      vb_hash: stringValue(trustRecord.vb_hash),
      vb_hash_source: stringValue(trustRecord.vb_hash_source),
      build_fingerprint: stringValue(trustRecord.build_fingerprint),
      slot_suffix: stringValue(trustRecord.slot_suffix),
      original_security_patch: stringValue(trustRecord.original_security_patch),
    }

    this.#data = data
  }

  async write(): Promise<void> {
    const data = this.#data

    const injector = this.#injector ?? {}
    const injectorMain = {
      ...recordValue(injector.main),
      enabled: data.injector_main?.enabled === true,
      log_level: stringValue(data.injector_main?.log_level, 'debug').toLowerCase(),
    }
    const filter = {
      ...recordValue(injector.filter),
      enabled: data.filter?.enabled === true,
      deny_packages: parsePackageList(data.filter?.deny_packages as string | undefined),
      block_android_package: data.filter?.block_android_package === true,
      allow_unknown_package: data.filter?.allow_unknown_package === true,
    }
    const intercept = {
      ...recordValue(injector.intercept),
      get_security_level: data.intercept?.get_security_level === true,
      get_key_entry: data.intercept?.get_key_entry === true,
      update_subcomponent: data.intercept?.update_subcomponent === true,
      list_entries: data.intercept?.list_entries === true,
      delete_key: data.intercept?.delete_key === true,
      grant: data.intercept?.grant === true,
      ungrant: data.intercept?.ungrant === true,
      get_number_of_entries: data.intercept?.get_number_of_entries === true,
      list_entries_batched: data.intercept?.list_entries_batched === true,
      get_supplementary_attestation_info:
        data.intercept?.get_supplementary_attestation_info === true,
    }

    injector.scoop = data.target ?? []
    injector.main = injectorMain
    injector.filter = filter
    injector.intercept = intercept
    const scoopDetails = recordValue(injector.scoop_details)
    for (const [packageName, slot] of Object.entries(this.#keyboxSlots)) {
      const details = recordValue(scoopDetails[packageName])
      if (slot > 0) {
        details.keybox_slot = slot
      } else {
        delete details.keybox_slot
      }
      if (Object.keys(details).length === 0) {
        delete scoopDetails[packageName]
      } else {
        scoopDetails[packageName] = details
      }
    }
    injector.scoop_details = scoopDetails
    this.#injector = injector
    await File.write(this.INJECTOR_FILE, prettyPrintToml(stringify(injector)))

    const omkConfig = this.#omkConfig ?? {}
    omkConfig.main = {
      ...recordValue(omkConfig.main),
      backend: 'injector',
      force_skip_system_biometric_hat_verification:
        data.omk_main?.force_skip_system_biometric_hat_verification === true,
    }
    omkConfig.trust = {
      ...recordValue(omkConfig.trust),
      os_version: parseInt(stringValue(data.trust?.os_version, '16'), 10),
      security_patch: stringValue(data.trust?.security_patch, 'auto'),
      vb_key: stringValue(data.trust?.vb_key, 'auto'),
      vb_hash: stringValue(data.trust?.vb_hash, 'auto'),
      verified_boot_state: data.trust?.verified_boot_state === true,
      device_locked: data.trust?.device_locked === true,
    }
    omkConfig.device = {
      ...recordValue(omkConfig.device),
      brand: stringValue(data.device?.brand, 'Google'),
      device: stringValue(data.device?.device, 'generic'),
      product: stringValue(data.device?.product, 'generic'),
      manufacturer: stringValue(data.device?.manufacturer, 'Google'),
      model: stringValue(data.device?.model, 'generic'),
      serial: stringValue(data.device?.serial, 'ABC12345678ABC'),
      overrideTelephonyProperties: data.device?.overrideTelephonyProperties === true,
      meid: stringValue(data.device?.meid),
      imei: stringValue(data.device?.imei),
      imei2: stringValue(data.device?.imei2),
    }

    const crypto: Record<string, unknown> = {
      ...recordValue(omkConfig.crypto),
      root_kek_seed: stringValue(data.crypto?.root_kek_seed),
      kak_seed: stringValue(data.crypto?.kak_seed),
      shared_secret_seed: stringValue(data.crypto?.shared_secret_seed),
      shared_secret_nonce: stringValue(data.crypto?.shared_secret_nonce),
    }
    const authTokenKey = stringValue(data.crypto?.auth_token_hmac_key).trim()
    if (authTokenKey) {
      crypto.auth_token_hmac_key = authTokenKey
    } else {
      delete crypto.auth_token_hmac_key
    }
    omkConfig.crypto = crypto

    this.#omkConfig = omkConfig
    await File.write(this.CONFIG_FILE, stringify(omkConfig))
  }

  get(): ConfigData
  get(section: string): Policy | string[] | undefined
  get(section?: string): ConfigData | Policy | string[] | undefined {
    if (section === undefined) return this.#data
    return this.#data[section]
  }

  set(data: ConfigData): void
  set(section: string, key: string, value: string): void
  set(section: string, value: string[] | Policy | undefined): void
  set(section: string | ConfigData, key?: string | string[] | Policy, value?: string): void {
    if (typeof section === 'object') {
      this.#data = section
    } else if (value !== undefined) {
      if (!(section in this.#data) || Array.isArray(this.#data[section])) {
        this.#data[section] = {}
      }
      (this.#data[section] as Record<string, string>)[key as string] = value
    } else if (key === undefined) {
      delete this.#data[section]
    } else {
      this.#data[section] = key as string[] | Policy
    }
  }

  removeMatch(section: string, predicate: (value: string) => boolean): string[] {
    const arr = this.#data[section]
    if (!Array.isArray(arr)) return []
    const removed = arr.filter(predicate)
    this.#data[section] = arr.filter((value) => !predicate(value))
    return removed
  }

  replaceMatch(section: string, predicate: (value: string) => boolean, newValue: string): boolean {
    const arr = this.#data[section]
    if (!Array.isArray(arr)) return false
    const idx = arr.findIndex(predicate)
    if (idx === -1) return false
    arr[idx] = newValue
    return true
  }

  push(section: string, value: string): void {
    if (!(section in this.#data) || !Array.isArray(this.#data[section])) {
      this.#data[section] = []
    }
    (this.#data[section] as string[]).push(value)
  }

  getKeyboxSlot(packageName: string): number {
    return this.#keyboxSlots[packageName] ?? 0
  }

  setKeyboxSlot(packageName: string, slot: number): void {
    if (!Number.isInteger(slot) || slot < 0 || slot > MAX_KEYBOX_SLOT) return
    this.#keyboxSlots[packageName] = slot
  }

  get configPath(): string {
    return this.CONFIG_PATH
  }

  get supportsPerAppConfig(): boolean {
    return this.perAppConfig
  }

  get supportsAppMode(): boolean {
    return this.appMode
  }

  getSectionTitle(section: SectionKey): string {
    return SECTION_TITLES[section]
  }

  getSectionSchema(section: Exclude<SectionKey, 'trust_record'>): PolicySchema {
    return SECTION_SCHEMAS[section]
  }
}
