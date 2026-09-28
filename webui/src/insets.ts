/**
 * System bar insets.
 *
 * The WebUI draws its own background under the status and navigation bars, which
 * needs the host to lay out edge to edge. The host applies that asynchronously,
 * so the viewport is resized after the WebView has already drawn and anything
 * anchored to a bar reflows on a later frame.
 *
 * The fix is ordering rather than timing: `prepareWindowInsets()` awaits the
 * host before the app renders, so the one layout pass already has the full
 * viewport and there is nothing to animate afterwards. `watchWindowInsets()` only
 * covers changes that come later.
 *
 * The bars still have to be measured, since a bar is a physical size. `dumpsys
 * window` over the ksu bridge is the source, as a manager commonly publishes no
 * inset variables. `env(safe-area-inset-*)` is 0 in a WebView that does not
 * populate it, so it is only a fallback.
 */

const INSET_VARS = ['top', 'right', 'bottom', 'left'] as const
type InsetSide = (typeof INSET_VARS)[number]

export type Insets = Record<InsetSide, number>

/** Last resort when nothing can be measured, so a bar is still cleared. */
const FALLBACK: Insets = { top: 24, right: 0, bottom: 48, left: 0 }

function isEmpty(insets: Insets): boolean {
  return !INSET_VARS.some((side) => insets[side] > 0)
}

function toCssPx(devicePx: number): number {
  const dpr = window.devicePixelRatio || 1
  return Math.round(devicePx / dpr)
}

/**
 * Read the current bar sizes from the window manager.
 *
 * `dumpsys window` reports each insets source with its own frame and hint, which
 * is rotation safe: on a landscape rotation the status bar reports a left or
 * right inset rather than a top one, and this picks that up. Values are device
 * pixels and are converted to CSS pixels here.
 */
function parseWindowInsets(dump: string): Insets | null {
  const hint = /mType=(statusBars|navigationBars)[^}]*?mInsetsHint=Insets\{left=(\d+), top=(\d+), right=(\d+), bottom=(\d+)\}/
  const found = [...dump.matchAll(new RegExp(hint.source, 'g'))]
  if (found.length === 0) return null

  const sides: Insets = { top: 0, right: 0, bottom: 0, left: 0 }
  let matched = false
  for (const match of found) {
    const [, , left, top, right, bottom] = match
    sides.left = Math.max(sides.left, toCssPx(Number(left)))
    sides.top = Math.max(sides.top, toCssPx(Number(top)))
    sides.right = Math.max(sides.right, toCssPx(Number(right)))
    sides.bottom = Math.max(sides.bottom, toCssPx(Number(bottom)))
    matched = true
  }
  return matched && !isEmpty(sides) ? sides : null
}

/** Measure `env(safe-area-inset-*)` by letting the engine resolve it. */
function measureEnvInsets(): Insets {
  const probe = document.createElement('div')
  probe.setAttribute('aria-hidden', 'true')
  probe.style.cssText = [
    'position:fixed',
    'top:0',
    'left:0',
    'width:0',
    'height:0',
    'visibility:hidden',
    'pointer-events:none',
    'padding-top:env(safe-area-inset-top,0px)',
    'padding-right:env(safe-area-inset-right,0px)',
    'padding-bottom:env(safe-area-inset-bottom,0px)',
    'padding-left:env(safe-area-inset-left,0px)',
  ].join(';')
  document.body.appendChild(probe)
  const style = getComputedStyle(probe)
  const result: Insets = {
    top: Number.parseFloat(style.paddingTop) || 0,
    right: Number.parseFloat(style.paddingRight) || 0,
    bottom: Number.parseFloat(style.paddingBottom) || 0,
    left: Number.parseFloat(style.paddingLeft) || 0,
  }
  probe.remove()
  return result
}

function readInjected(): Insets {
  const read = (name: string): number => {
    const raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim()
    const parsed = Number.parseFloat(raw)
    return Number.isFinite(parsed) ? parsed : 0
  }
  return {
    top: read('--window-inset-top') || read('--safe-area-inset-top'),
    right: read('--window-inset-right') || read('--safe-area-inset-right'),
    bottom: read('--window-inset-bottom') || read('--safe-area-inset-bottom'),
    left: read('--window-inset-left') || read('--safe-area-inset-left'),
  }
}

async function collect(): Promise<Insets> {
  const injected = readInjected()
  if (!isEmpty(injected)) return injected

  try {
    const { exec } = await import('kernelsu-alt')
    const result = await exec('dumpsys window')
    if (result.errno === 0) {
      const parsed = parseWindowInsets(result.stdout)
      if (parsed) return parsed
    }
  } catch {
    // No bridge. The remaining paths still apply.
  }

  const measured = measureEnvInsets()
  if (!isEmpty(measured)) return measured

  return { ...FALLBACK }
}

function apply(insets: Insets): void {
  const root = document.documentElement.style
  root.setProperty('--top-inset', `${insets.top}px`)
  root.setProperty('--right-inset', `${insets.right}px`)
  root.setProperty('--bottom-inset', `${insets.bottom}px`)
  root.setProperty('--left-inset', `${insets.left}px`)
  // Full-screen dialogs read these instead of calling env(), which cannot be
  // intercepted by redefining a custom property.
  root.setProperty('--sa-top', `${insets.top}px`)
  root.setProperty('--sa-right', `${insets.right}px`)
  root.setProperty('--sa-bottom', `${insets.bottom}px`)
  root.setProperty('--sa-left', `${insets.left}px`)
  // Cheap to read from a debugger or `evaluateJavascript` when checking layout.
  document.documentElement.setAttribute('data-omk-insets', `${insets.top}/${insets.bottom}`)
}

/**
 * Go edge to edge, then publish the insets. Await this before the first paint:
 * the app is still empty, so the host's resize lands on a blank page and the
 * first real paint is already correct.
 */
export async function prepareWindowInsets(): Promise<Insets> {
  try {
    const { enableEdgeToEdge } = await import('kernelsu-alt')
    // kernelsu-alt falls back to the older enableInsets internally and rejects
    // rather than throwing when the bridge is missing.
    await enableEdgeToEdge(true)
  } catch {
    // The host keeps its own padding. Measuring below still clears the bars,
    // they are just not extended with our background.
  }
  const insets = await collect()
  apply(insets)
  return insets
}

/** Publish the insets again. Returns what was applied. */
export async function applyWindowInsets(): Promise<Insets> {
  const insets = await collect()
  apply(insets)
  return insets
}

/**
 * Keep the insets current for geometry that changes after startup. Republishing
 * on rotation is safe because the viewport has already changed; opening the WebUI
 * is {@link prepareWindowInsets}' job, not this one's.
 */
export function watchWindowInsets(): void {
  if (!document.body) return
  window.addEventListener('orientationchange', () => void applyWindowInsets(), { passive: true })
}
