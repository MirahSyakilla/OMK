/**
 * System bar insets, measured locally.
 *
 * The WebUI is served edge to edge so the app's own background runs under the
 * status bar and the navigation bar, rather than the host painting its own
 * grey blocks there. That means this module owns the padding, and the values in
 * `--top-inset` and `--bottom-inset` have to be real.
 *
 * They used to come from `https://mui.kernelsu.org/internal/insets.css`, a remote
 * stylesheet. When it could not be fetched, both variables silently fell back to
 * 0 and the layout put headers under the status bar and the last row under the
 * three-button navigation bar. Nothing here depends on that stylesheet, on the
 * manager publishing anything, or on `env(safe-area-inset-*)`, which is 0 in a
 * WebView that does not populate it.
 *
 * Resolution order:
 *
 * 1. `dumpsys window`, read over the `ksu` bridge. The WebUI already runs as
 *    root, and this is the only source on a manager that publishes no inset
 *    variables, which is the common case.
 * 2. A probe element measuring `env(safe-area-inset-*)`.
 * 3. A conservative default, so a bar is still cleared if everything fails.
 *
 * If the host ends up insetting the viewport anyway, nothing is applied: padding
 * on top of the host's own would offset the layout twice.
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

/** Whether the host is insetting the viewport itself. */
function hostIsInset(): boolean {
  const display = window.screen?.height ?? 0
  if (!display) return false
  return window.innerHeight < display - 1
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

/** Ask the host to lay out edge to edge so our background reaches the bars. */
async function requestEdgeToEdge(): Promise<void> {
  try {
    const { enableEdgeToEdge } = await import('kernelsu-alt')
    // kernelsu-alt falls back to the older enableInsets internally and rejects
    // rather than throwing when the bridge is missing.
    await enableEdgeToEdge(true)
  } catch {
    // Nothing to do; the host keeps its own padding and hostIsInset() below
    // makes sure we do not add a second offset.
  }
}

/** Resolve the insets and publish them. Returns what was applied. */
export async function applyWindowInsets(): Promise<Insets> {
  // If the host is still insetting the viewport, it owns the padding already.
  const insets = hostIsInset() ? { top: 0, right: 0, bottom: 0, left: 0 } : await collect()
  apply(insets)
  return insets
}

/**
 * Start resolving insets, and keep them current.
 *
 * The host applies its layout change asynchronously, so the first reading is
 * taken after it has settled, and again on every viewport change because the
 * bars can appear or disappear on their own.
 */
export function watchWindowInsets(): void {
  if (!document.body) return

  const update = (): void => {
    void applyWindowInsets()
  }

  void requestEdgeToEdge().then(() => {
    update()
    // The switch to edge to edge and the window manager's own inset update are
    // both a frame or more behind the request.
    window.setTimeout(update, 120)
    window.setTimeout(update, 400)
    window.setTimeout(update, 900)
  })

  window.addEventListener('resize', update, { passive: true })
  window.addEventListener('orientationchange', update, { passive: true })
  window.visualViewport?.addEventListener('resize', update, { passive: true })
}
