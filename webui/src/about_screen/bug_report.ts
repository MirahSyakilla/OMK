/**
 * Bug report bundle: collects the logs worth sending and packages them as a
 * `.tar.gz` under the user's Download folder.
 *
 * `tar` and `gzip` are toybox applets on every Android release, so no compressor
 * needs to be carried in the WebUI and the output is an ordinary tarball that any
 * desktop `tar` reads.
 *
 * Nothing is launched to send it. The path goes to a snackbar and the user sends
 * the file from whatever client they use, which is why the archive is written
 * where the user's own storage owns it rather than root-only.
 */

import { exec } from 'kernelsu-alt'
import { shellQuote } from '../shell'
import { File } from '../file'
import type { Snackbar } from '../snackbar/snackbar'

/** Where the archive lands, in a directory the user's own storage owns. */
const OUT_DIR = '/storage/emulated/0/Download/OMK'

/** Private state the module writes, copied when present. */
const CONFIG_DIR = '/data/misc/keystore/omk'

/**
 * Sources for the archive, as `[path, name-in-archive]`.
 *
 * The `.1` rotations come along because they hold the earlier half of a session,
 * and a report about something transient often only shows up there. Every entry
 * is optional.
 */
const SOURCES: Array<[string, string]> = [
  [`${CONFIG_DIR}/logs/injector.log`, 'logs/injector.log'],
  [`${CONFIG_DIR}/logs/injector.log.1`, 'logs/injector.log.1'],
  [`${CONFIG_DIR}/logs/keymint.log`, 'logs/keymint.log'],
  [`${CONFIG_DIR}/logs/keymint.log.1`, 'logs/keymint.log.1'],
  ['/data/adb/omk/logs/daemon.log', 'logs/daemon.log'],
  [`${CONFIG_DIR}/config.toml`, 'config.toml'],
  [`${CONFIG_DIR}/injector.toml`, 'injector.toml'],
]

/** How many trailing dmesg lines to include. */
const DMESG_LINES = 2000

/** Filename stamp, `YYYYMMDD-HHMMSS`. */
function stamp(): string {
  const now = new Date()
  const pad = (n: number): string => String(n).padStart(2, '0')
  return (
    `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}` +
    `-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`
  )
}

export class BugReport {
  readonly #snackbar?: Snackbar

  constructor(snackbar?: Snackbar) {
    this.#snackbar = snackbar
  }

  /**
   * Build the archive and return where it was saved.
   *
   * @returns the saved archive path
   */
  async build(): Promise<string> {
    const path = `${OUT_DIR}/bugreport-${stamp()}.tar.gz`
    // Staged outside the module's private dir so `tar` can write the archive, and
    // removed once it is built.
    const staging = `/data/local/tmp/omk-bugreport-${Date.now()}`

    await this.#exec(`rm -rf ${shellQuote(staging)} && mkdir -p ${shellQuote(`${staging}/logs`)}`)

    let copied = 0
    for (const [source, destination] of SOURCES) {
      // Each copy is independent so a missing rotation does not abort the bundle.
      // Copying with `cp` rather than reading through the WebUI also keeps a
      // multi-megabyte log out of JS memory.
      const result = await this.#exec(
        `[ -f ${shellQuote(source)} ] && cp -f ${shellQuote(source)} ` +
          `${shellQuote(`${staging}/${destination}`)} 2>/dev/null; echo $?`,
      )
      if (result.stdout.trim() === '0') copied++
    }

    // dmesg is written out rather than copied, and needs whatever privilege the
    // WebUI holds, so a denial here is expected and not fatal.
    await this.#exec(
      `dmesg 2>/dev/null | tail -n ${DMESG_LINES} > ${shellQuote(`${staging}/logs/dmesg.log`)} 2>/dev/null || true`,
    )
    if (copied === 0) {
      const dmesg = await this.#exec(`[ -s ${shellQuote(`${staging}/logs/dmesg.log`)} ] && echo yes`)
      if (dmesg.stdout.trim() !== 'yes') {
        await this.#exec(`rm -rf ${shellQuote(staging)}`)
        throw new Error('no logs found to report')
      }
    }

    // A short manifest, so the build that produced the bundle is readable without
    // opening the tarball.
    await this.#exec(
      `getprop ro.build.fingerprint > ${shellQuote(`${staging}/device.txt`)} 2>/dev/null; ` +
        `getprop ro.build.version.release >> ${shellQuote(`${staging}/device.txt`)} 2>/dev/null; ` +
        `date >> ${shellQuote(`${staging}/device.txt`)} 2>/dev/null; true`,
    )

    // -C puts the entries at the archive root rather than under the staging name.
    await File.createDirectory(OUT_DIR)
    const tarred = await this.#exec(
      `tar -czf ${shellQuote(path)} -C ${shellQuote(staging)} logs device.txt`,
    )
    await this.#exec(`rm -rf ${shellQuote(staging)}`)
    if (tarred.errno !== 0) {
      throw new Error(tarred.stderr?.trim() || 'failed to create archive')
    }

    // /storage is FUSE-backed and ignores chown from root, so the mode is the only
    // lever available for making the archive readable by another app.
    await this.#exec(`chmod 644 ${shellQuote(path)} 2>/dev/null || true`)

    return path
  }

  /** Build the bundle and report where it landed. */
  async run(): Promise<void> {
    let path: string
    try {
      this.#snackbar?.show('Collecting logs...')
      path = await this.build()
    } catch (error) {
      this.#snackbar?.show(
        `Bug report failed: ${error instanceof Error ? error.message : 'unknown error'}`,
        false,
      )
      return
    }

    this.#snackbar?.show(`Bugreport archive are saved to ${path}.`)
  }

  async #exec(command: string): Promise<{ errno: number; stdout: string; stderr: string }> {
    try {
      return await exec(command)
    } catch (error) {
      return {
        errno: -1,
        stdout: '',
        stderr: error instanceof Error ? error.message : String(error),
      }
    }
  }
}
