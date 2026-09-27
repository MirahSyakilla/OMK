import type { MdDialog } from '@material/web/all'

/**
 * Customize MdDialog animation
 * @param dialog MdDialog instance
 */
export function applyDialogAnimation(dialog: MdDialog): void {
  const defaultOpenAnim = dialog.getOpenAnimation
  const defaultCloseAnim = dialog.getCloseAnimation

  const isCompactFullscreen = () =>
    dialog.classList.contains('dialog--fullscreen') &&
    window.matchMedia('(max-width: 599.98px), (max-height: 539.98px)').matches

  dialog.getOpenAnimation = () => {
    document.body.style.overflow = 'hidden'
    if (dialog.classList.contains('dialog--fullscreen')) {
      if (isCompactFullscreen()) {
        return {
          dialog: [
            [
              [{ opacity: 0, transform: 'translateY(100%)' }, { opacity: 1, transform: 'translateY(0)' }],
              { duration: 320, easing: 'cubic-bezier(0.05, 0.7, 0.1, 1.0)' },
            ],
          ],
          scrim: [
            [
              [{ opacity: 0 }, { opacity: 0.32 }],
              { duration: 280, easing: 'linear' },
            ],
          ],
          container: [],
        }
      }

      // Tablet/Desktop centered modal scale-fade (M3 standard)
      return {
        dialog: [
          [
            [{ opacity: 0, transform: 'scale(0.92)' }, { opacity: 1, transform: 'scale(1)' }],
            { duration: 280, easing: 'cubic-bezier(0.05, 0.7, 0.1, 1.0)' },
          ],
        ],
        scrim: [
          [
            [{ opacity: 0 }, { opacity: 0.32 }],
            { duration: 280, easing: 'linear' },
          ],
        ],
        container: [],
      }
    }

    const defaultAnim = defaultOpenAnim.call(dialog)
    return {
      ...defaultAnim,
      dialog: [
        [
          [{ opacity: 0, transform: 'translateY(50px)' }, { opacity: 1, transform: 'translateY(0)' }],
          { duration: 300, easing: 'ease' },
        ],
      ],
      scrim: [
        [
          [{ opacity: 0 }, { opacity: 0.32 }],
          { duration: 300, easing: 'linear' },
        ],
      ],
      container: [],
    }
  }

  dialog.getCloseAnimation = () => {
    document.body.style.overflow = ''
    if (dialog.classList.contains('dialog--fullscreen')) {
      if (isCompactFullscreen()) {
        return {
          dialog: [
            [
              [{ opacity: 1, transform: 'translateY(0)' }, { opacity: 0, transform: 'translateY(100%)' }],
              { duration: 240, easing: 'cubic-bezier(0.3, 0, 0.8, 0.15)' },
            ],
          ],
          scrim: [
            [
              [{ opacity: 0.32 }, { opacity: 0 }],
              { duration: 240, easing: 'linear' },
            ],
          ],
          container: [],
        }
      }

      // Tablet/Desktop centered modal scale-fade close
      return {
        dialog: [
          [
            [{ opacity: 1, transform: 'scale(1)' }, { opacity: 0, transform: 'scale(0.92)' }],
            { duration: 200, easing: 'cubic-bezier(0.3, 0, 0.8, 0.15)' },
          ],
        ],
        scrim: [
          [
            [{ opacity: 0.32 }, { opacity: 0 }],
            { duration: 200, easing: 'linear' },
          ],
        ],
        container: [],
      }
    }
    const defaultAnim = defaultCloseAnim.call(dialog)
    return {
      ...defaultAnim,
      dialog: [
        [
          [{ opacity: 1, transform: 'translateY(0)' }, { opacity: 0, transform: 'translateY(-50px)' }],
          { duration: 300, easing: 'ease' },
        ],
      ],
      scrim: [
        [
          [{ opacity: 0.32 }, { opacity: 0 }],
          { duration: 300, easing: 'linear' },
        ],
      ],
      container: [],
    }
  }
}
