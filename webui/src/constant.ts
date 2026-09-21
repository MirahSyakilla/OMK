export const MOD_ID = 'oh_my_keymint'
export const LOCAL_STORAGE_PREFIX = 'OhMyKeymintWebUI'
export const GITHUB_REPO = 'MirahSyakilla/OMK'
export const TELEGRAM_CHANNEL = 'https://t.me/meowcomfylair'
export const KEYBOX_REPO_URL = 'https://keybox.kowx712.cc'
export const KEYBOX_ALWAYSSTRONG_URL = 'http://evoker.qzz.io/key'

export interface PixelDevice {
  product: string
  model: string
  min: number
  max: number
}

export const PIXEL_DEVICES: PixelDevice[] = [
  { product: 'oriole', model: 'Pixel 6', min: 12, max: 17 },
  { product: 'raven', model: 'Pixel 6 Pro', min: 12, max: 17 },
  { product: 'panther', model: 'Pixel 7', min: 13, max: 17 },
  { product: 'cheetah', model: 'Pixel 7 Pro', min: 13, max: 17 },
  { product: 'lynx', model: 'Pixel 7a', min: 13, max: 17 },
  { product: 'bluejay', model: 'Pixel 6a', min: 16, max: 17 },
  { product: 'shiba', model: 'Pixel 8', min: 14, max: 17 },
  { product: 'husky', model: 'Pixel 8 Pro', min: 14, max: 17 },
  { product: 'akita', model: 'Pixel 8a', min: 14, max: 17 },
  { product: 'tokay', model: 'Pixel 9', min: 15, max: 17 },
  { product: 'caiman', model: 'Pixel 9 Pro', min: 15, max: 17 },
  { product: 'komodo', model: 'Pixel 9 Pro XL', min: 15, max: 17 },
  { product: 'comet', model: 'Pixel 9 Pro Fold', min: 15, max: 17 },
  { product: 'tegu', model: 'Pixel 9a', min: 16, max: 17 },
  { product: 'frankel', model: 'Pixel 10', min: 16, max: 17 },
  { product: 'blazer', model: 'Pixel 10 Pro', min: 16, max: 17 },
  { product: 'mustang', model: 'Pixel 10 Pro XL', min: 16, max: 17 },
  { product: 'rango', model: 'Pixel 10 Pro Fold', min: 16, max: 17 },
  { product: 'stallion', model: 'Pixel 10a', min: 16, max: 17 },
]
