import type { FlashBuild } from './cli'

/**
 * Pixel build IDs carry the patch date as `YYMMDD`, so the security patch level
 * is recoverable from the build ID without a second lookup.
 *
 * A build ID that does not match yields an empty string, and callers treat that
 * as "no patch to write" rather than guessing.
 */
export function securityPatch(buildId: string): string {
  const match = buildId.match(/^[A-Z0-9]+\.(\d{2})(\d{2})(\d{2})\./)
  if (!match) return ''
  return `20${match[1]}-${match[2]}-${match[3]}`
}

/**
 * The release candidate name, falling back to the build ID.
 *
 * Both are accepted by the fingerprint grammar, and older Flash responses omit
 * `releaseCandidateName`, so the fallback keeps those selections usable instead
 * of emitting `undefined` into the fingerprint.
 */
export function releaseId(build: FlashBuild): string {
  return build.releaseCandidateName || build.buildId
}

/**
 * Render the `integrity.prop` key=value payload for a chosen Flash build.
 *
 * This is the single definition of the payload. The Integrity screen and the
 * Integrity dialog both write this file, so keeping one implementation stops the
 * two from drifting on which fields are emitted.
 */
export function buildProp(
  build: FlashBuild,
  product: string,
  model: string,
  major: number,
  initialSdk?: number,
): string {
  const id = releaseId(build)
  // The date lives in releaseCandidateName, which reads `CP3A.260905.009`.
  // buildId is the numeric build number and carries no date at all, so reading it
  // here matched nothing and silently dropped SECURITY_PATCH from every build.
  const patch = securityPatch(build.releaseCandidateName || build.buildId)
  const lines = [
    `FINGERPRINT=google/${product}/${product}:${major}/${id}/${build.buildId}:user/release-keys`,
    'MANUFACTURER=Google',
    'MODEL=' + model,
    `PRODUCT=${product}`,
    `DEVICE=${product}`,
    'BRAND=google',
    `RELEASE=${major}`,
    `ID=${id}`,
    `INCREMENTAL=${build.buildId}`,
    'TYPE=user',
    'TAGS=release-keys',
  ]
  if (initialSdk !== undefined) {
    lines.push(`DEVICE_INITIAL_SDK_INT=${initialSdk}`)
  }
  if (patch) lines.push(`SECURITY_PATCH=${patch}`)
  return lines.join('\n')
}
