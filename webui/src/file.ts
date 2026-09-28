import { exec } from 'kernelsu-alt'

export class File {
  static async exist(path: string): Promise<boolean> {
    const { errno } = await exec(`[ -e "${path}" ]`)
    return errno === 0
  }

  static async isDirectory(path: string): Promise<boolean> {
    const { errno } = await exec(`[ -d "${path}" ]`)
    return errno === 0
  }

  static async read(path: string): Promise<string> {
    const result = await exec(`cat "${path}"`)
    if (result.errno !== 0) throw new Error(`File.read failed (${result.errno}): ${result.stderr}`)
    return result.stdout
  }

  /**
   * Write `data` to `path` by piping it through `cmd`.
   *
   * The payload is base64-encoded and decoded on the far side rather than
   * dropped into a heredoc. A heredoc has to be delimited by a literal that the
   * data might itself contain, and when it does the heredoc closes early and the
   * remainder of the data is executed as shell, as root. That is not
   * hypothetical here: this function writes a keybox fetched from a remote
   * repository, and pasted keyboxes, and a fingerprint template assembled from
   * network data.
   *
   * base64's alphabet is [A-Za-z0-9+/=], which contains nothing the shell treats
   * specially, so the encoded form is inert no matter what the input was. It also
   * means newlines and NULs survive, which a heredoc would mangle.
   *
   * `path` and `cmd` are still interpolated, so callers must pass literal values
   * for both; every call site in this codebase does.
   */
  static async write(path: string, data: string, cmd: string = 'cat'): Promise<void> {
    // Encode without padding: Android's base64 rejects '=' in some invocations
    // and -d tolerates its absence.
    const encoded = btoa(String.fromCharCode(...new TextEncoder().encode(data)))
      .replace(/=+$/, '')
      .replace(/(.{76})/g, '$1\n')
    const result = await exec(`printf '%s' '${encoded}' | base64 -d | (${cmd}) > "${path}"`)
    if (result.errno !== 0) throw new Error(`File.write failed (${result.errno}): ${result.stderr}`)
  }

  /** Restrict a sensitive file to the KeyMint service account. */
  static async secure(path: string): Promise<void> {
    const result = await exec(`chmod 0600 "${path}" && chown 1017:1017 "${path}"`)
    if (result.errno !== 0) {
      throw new Error(`File.secure failed (${result.errno}): ${result.stderr}`)
    }
  }

  static async move(src: string, dst: string): Promise<void> {
    const result = await exec(`mv -f "${src}" "${dst}"`)
    if (result.errno !== 0) throw new Error(`File.move failed (${result.errno}): ${result.stderr}`)
  }

  static async copy(src: string, dst: string): Promise<void> {
    const result = await exec(`cp -rf "${src}" "${dst}"`)
    if (result.errno !== 0) throw new Error(`File.copy failed (${result.errno}): ${result.stderr}`)
  }

  static async delete(path: string): Promise<void> {
    const result = await exec(`rm -rf "${path}"`)
    if (result.errno !== 0) throw new Error(`File.delete failed (${result.errno}): ${result.stderr}`)
  }

  static async createFile(path: string): Promise<void> {
    const result = await exec(`touch "${path}"`)
    if (result.errno !== 0) throw new Error(`File.createFile failed (${result.errno}): ${result.stderr}`)
  }

  static async createDirectory(dir: string): Promise<void> {
    const result = await exec(`mkdir -p "${dir}"`)
    if (result.errno !== 0) throw new Error(`File.createDirectory failed (${result.errno}): ${result.stderr}`)
  }
}
