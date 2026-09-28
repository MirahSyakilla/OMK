/**
 * Contributor carousel for the About page.
 *
 * The stage is a horizontal scroller, so scrolling is the animation: autoplay and
 * a fling are the same `scrollTo`, which is why they look identical. The same
 * approach as the BRENE carousel, which drove a transform from script and had to
 * suppress the transition for long jumps, so some taps animated and some snapped.
 *
 * The rotation never ends. Clones of the first and last `CONTRIB_VISIBLE` cards
 * sit at either end, so the stage can scroll past the final contributor into
 * cards that look exactly like the first. Each step advances one card, giving
 * 1, 2, 3, 4, 5, 1, ... rather than paging through whole windows of three.
 *
 * Avatars are shipped rather than fetched, so the row renders on a device with no
 * route to a CDN. `initials` is drawn if a file is missing or undecodable.
 */

export interface Contributor {
  name: string
  href: string
  avatar: string
  initials: string
}

export const CONTRIBUTORS: Contributor[] = [
  {
    name: 'MirahSyakilla',
    href: 'https://github.com/MirahSyakilla',
    avatar: './img/contributors/mirahsyakilla.jpg',
    initials: 'MS',
  },
  {
    name: 'James Clef',
    href: 'https://github.com/qwq233',
    avatar: './img/contributors/jamesclef.jpg',
    initials: 'JC',
  },
  {
    name: 'ITxiao6666',
    href: 'https://github.com/ITxiao6666',
    avatar: './img/contributors/itxiao6666.jpg',
    initials: 'IX',
  },
  {
    name: 'KOWX712',
    href: 'https://github.com/KOWX712',
    avatar: './img/contributors/kowx712.jpg',
    initials: 'K7',
  },
  {
    name: 'mohdakil2426',
    href: 'https://github.com/mohdakil2426',
    avatar: './img/contributors/mohdakil2426.jpg',
    initials: 'MA',
  },
]

/** How many cards are on screen at once. */
export const CONTRIB_VISIBLE = 3

/** Advances one card every 2.5s. */
const CONTRIB_INTERVAL = 2500

/** How long a smooth scroll is given to finish before the stage is renormalised. */
const SETTLE_MS = 420

/** Escapes text interpolated into the carousel markup. */
function esc(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

function card(c: Contributor, clone: boolean): string {
  return /* html */ `
    <div class="contrib__card${clone ? ' contrib__card--clone' : ''}"${
      clone ? ' aria-hidden="true"' : ''
    }>
      <span class="contrib__avatar" aria-hidden="true">
        <img src="${esc(c.avatar)}" alt="" decoding="async" data-fallback="${esc(c.initials)}" />
      </span>
      <a class="contrib__name" href="${esc(c.href)}" target="_blank" rel="noopener noreferrer"${
        clone ? ' tabindex="-1"' : ''
      }>${esc(c.name)}</a>
    </div>`
}

/**
 * Markup for the carousel.
 *
 * One dot per contributor, since there is one logical position each.
 */
export function contributorCarouselHtml(): string {
  const n = CONTRIBUTORS.length
  // Two screens of clones on each end, matching the offsets the stage wiring
  // computes, so a fling that overshoots still lands on real cards.
  const clones = Math.min(CONTRIB_VISIBLE * 2, n)
  // Leading clones let a backwards drag wrap into the last contributors, and
  // trailing clones make the forward wrap seamless.
  const head = CONTRIBUTORS.slice(n - clones).map((c) => card(c, true))
  const real = CONTRIBUTORS.map((c) => card(c, false))
  const tail = CONTRIBUTORS.slice(0, clones).map((c) => card(c, true))
  return /* html */ `
    <div class="contrib" id="contrib-carousel">
      <div class="contrib__stage" tabindex="0" role="group" aria-label="Contributors">${[
        ...head,
        ...real,
        ...tail,
      ].join('')}</div>
    </div>`
}

/** Set by {@link initContributorCarousel}, so a later visit can tear the old one down. */
let teardown: (() => void) | null = null

/**
 * Wire the carousel if the rendered page has one.
 *
 * Autoplay is pausable, and never starts under `prefers-reduced-motion`; the
 * stage can still be flung and tapped in that case, so the content stays
 * reachable either way.
 */
export function initContributorCarousel(): void {
  // The About page is re-rendered on every visit, so an interval from a previous
  // one would still be ticking against detached nodes.
  teardown?.()

  const root = document.getElementById('contrib-carousel')
  if (!root) {
    teardown = null
    return
  }

  const stage = root.querySelector<HTMLElement>('.contrib__stage')
  const cards = Array.from(root.querySelectorAll<HTMLElement>('.contrib__card'))
  if (!stage || cards.length === 0) return

  const n = CONTRIBUTORS.length
  // Two screens of clones on each end. CONTRIB_VISIBLE is the bare minimum, and a
  // fling that overshoots by more than that would reach past the last card.
  const clones = Math.min(CONTRIB_VISIBLE * 2, n)
  // `lead` is the physical position of the first real card. Logical position k
  // lives at `lead + k`; the last showing only real cards is `lead + n - 1`, and
  // the positions past that are trailing clones.
  const lead = clones
  const lastReal = lead + n - 1

  const reduced = window.matchMedia?.('(prefers-reduced-motion: reduce)')
  const autoplayAllowed = (): boolean => !reduced || !reduced.matches

  /** Logical position, always in [0, n). */
  let index = 0
  let timer: number | null = null
  let settleTimer: number | null = null
  /** True while a scripted scroll is in flight, so the listener does not fight it. */
  let animating = false

  /** Distance between two cards, including the gap. */
  const step = (): number => {
    const gap = Number.parseFloat(window.getComputedStyle(stage).columnGap) || 0
    return cards[0].offsetWidth + gap
  }

  /**
   * Scroll to a physical card offset.
   *
   * `smooth` off is how a wrap repositions without animating: the clone and the
   * card it stands in for are identical, so the move should be invisible.
   *
   * That requires `behavior: 'instant'`, not `'auto'`. Per spec `auto` means
   * "defer to the CSS `scroll-behavior` value", and the stage sets that to
   * `smooth`, so a fold intended to be invisible was animated backwards across
   * the whole list on every wrap. `instant` overrides the CSS and jumps.
   */
  const scrollToCard = (position: number, smooth: boolean): void => {
    if (smooth) {
      animating = true
      if (settleTimer !== null) window.clearTimeout(settleTimer)
      settleTimer = window.setTimeout(() => {
        animating = false
        settleTimer = null
        // Fold any clone the animation passed through, once the stage is at rest.
        // No scroll event follows the end of the animation, so the fold has to
        // happen here rather than in the handler.
        normalise()
      }, SETTLE_MS)
    } else {
      animating = false
    }
    stage.scrollTo({
      left: position * step(),
      behavior: smooth && autoplayAllowed() ? 'smooth' : 'instant',
    })
  }

  /**
   * Bring the stage back onto the real cards.
   *
   * The fold is by the full contributor count, not the clone count. A logical
   * position repeats every `n` cards, so scrolling from the last real position
   * into the trailing clones lands `n` cards past the equivalent real position.
   */
  const normalise = (): void => {
    // Never fold mid-animation. A smooth scroll reports intermediate offsets, and
    // rounding one can pick the wrong branch, which yanks the stage sideways.
    if (animating) return
    const distance = step()
    if (distance <= 0) return
    const at = Math.round(stage.scrollLeft / distance)
    // A hard fling can land past the final clone, where there is no card left to
    // scroll to, so clamp back into range rather than fold to a missing position.
    const max = cards.length - CONTRIB_VISIBLE
    if (at > max) {
      scrollToCard(max, false)
      return
    }
    if (at > lastReal) {
      // In the trailing clones, showing the first contributors again.
      const folded = at - n
      scrollToCard(folded, false)
      return
    }
    if (at < lead) {
      // In the leading clones, showing the last contributors again.
      const folded = at + n
      scrollToCard(Math.min(lastReal, folded), false)
      return
    }
  }

  const advance = (): void => {
    const next = index + 1
    // The scroll target is deliberately not reduced modulo `n`. Letting it run
    // one card past the end lands on a trailing clone that already looks like the
    // first contributor, so the motion stays rightward and the fold afterwards is
    // a jump onto an identical card. Reducing it here would sweep the stage back
    // to the start on every wrap.
    index = ((next % n) + n) % n
    scrollToCard(lead + next, true)
  }

  const stop = (): void => {
    if (timer === null) return
    window.clearInterval(timer)
    timer = null
    root.classList.add('is-paused')
  }

  const start = (): void => {
    if (timer !== null || !autoplayAllowed()) return
    root.classList.remove('is-paused')
    timer = window.setInterval(advance, CONTRIB_INTERVAL)
  }

  // A fling or drag moves the scroller directly, so this only mirrors it and
  // renormalises: scrolling is the source of truth, nothing drives it.
  let scrollRaf = 0
  const onScroll = (): void => {
    if (scrollRaf) return
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = 0
      normalise()
    })
  }

  const onPointerEnter = (): void => stop()
  const onPointerLeave = (): void => {
    if (autoplayAllowed()) start()
  }
  const onFocusIn = (): void => stop()
  const onFocusOut = (event: FocusEvent): void => {
    if (root.contains(event.relatedTarget as Node | null)) return
    if (autoplayAllowed()) start()
  }
  const onReducedChange = (): void => {
    if (autoplayAllowed()) start()
    else stop()
  }
  // A missing or undecodable avatar falls back to initials. These are local
  // files, so a failure is a file problem, not a network one.
  const onAvatarError = (event: Event): void => {
    const img = event.target as HTMLImageElement
    const holder = img.parentElement
    if (!holder) return
    holder.dataset.initials = img.dataset.fallback || ''
    holder.classList.add('is-fallback')
  }

  stage.addEventListener('scroll', onScroll, { passive: true })
  root.addEventListener('pointerenter', onPointerEnter)
  root.addEventListener('pointerleave', onPointerLeave)
  root.addEventListener('focusin', onFocusIn)
  root.addEventListener('focusout', onFocusOut)
  reduced?.addEventListener('change', onReducedChange)
  root
    .querySelectorAll('.contrib__avatar img')
    .forEach((img) => img.addEventListener('error', onAvatarError))

  // Open on the first real card, never on a leading clone.
  scrollToCard(lead, false)
  start()

  teardown = () => {
    if (timer !== null) window.clearInterval(timer)
    if (settleTimer !== null) window.clearTimeout(settleTimer)
    stage.removeEventListener('scroll', onScroll)
    root.removeEventListener('pointerenter', onPointerEnter)
    root.removeEventListener('pointerleave', onPointerLeave)
    root.removeEventListener('focusin', onFocusIn)
    root.removeEventListener('focusout', onFocusOut)
    reduced?.removeEventListener('change', onReducedChange)
    root
      .querySelectorAll('.contrib__avatar img')
      .forEach((img) => img.removeEventListener('error', onAvatarError))
    teardown = null
  }
}
