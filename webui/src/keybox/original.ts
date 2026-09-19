const SOFT_ATTESTATION_LIBS = [
  '/system/lib64/libsoft_attestation_cert.so',
  '/system/lib/libsoft_attestation_cert.so',
]

const STOCK_KEYBOX_XML = [
  '/vendor/etc/keybox.xml',
  '/odm/etc/keybox.xml',
  '/system/etc/keybox.xml',
]

const RSA_OID = [0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]
const EC_OID = [0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]

function derLength(buf: Uint8Array, offset: number): { hdr: number; length: number } | null {
  if (offset + 1 >= buf.length) return null
  const lbyte = buf[offset + 1]
  if ((lbyte & 0x80) === 0) return { hdr: 2, length: lbyte }
  const n = lbyte & 0x7f
  if (n < 1 || n > 3 || offset + 2 + n > buf.length) return null
  let length = 0
  for (let i = 0; i < n; i++) length = (length << 8) | buf[offset + 2 + i]
  return { hdr: 2 + n, length }
}

function findDerObjects(buf: Uint8Array): Uint8Array[] {
  const found: Uint8Array[] = []
  let i = 0
  while (i < buf.length - 4) {
    if (buf[i] !== 0x30) {
      i += 1
      continue
    }
    const info = derLength(buf, i)
    if (!info) {
      i += 1
      continue
    }
    const total = info.hdr + info.length
    if (info.length < 80 || total > 4096 || i + total > buf.length) {
      i += 1
      continue
    }
    found.push(buf.subarray(i, i + total))
    i += total
  }
  return found
}

function looksLikeRsaPrivate(der: Uint8Array): boolean {
  const info = derLength(der, 0)
  if (!info) return false
  const i = info.hdr
  return der[i] === 0x02 && der[i + 1] === 0x01 && der[i + 2] === 0x00 && der[i + 3] === 0x02
}

function looksLikeEcPrivate(der: Uint8Array): boolean {
  const info = derLength(der, 0)
  if (!info) return false
  const i = info.hdr
  return der[i] === 0x02 && der[i + 1] === 0x01 && der[i + 2] === 0x01 && der[i + 3] === 0x04
}

function looksLikeCert(der: Uint8Array): boolean {
  const info = derLength(der, 0)
  if (!info) return false
  return der[info.hdr] === 0x30
}

function indexOfBytes(haystack: Uint8Array, needle: number[]): number {
  outer: for (let i = 0; i <= haystack.length - needle.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer
    }
    return i
  }
  return -1
}

function certAlgo(der: Uint8Array): 'rsa' | 'ecdsa' | null {
  if (indexOfBytes(der, EC_OID) >= 0) return 'ecdsa'
  if (indexOfBytes(der, RSA_OID) >= 0) return 'rsa'
  return null
}

function toPem(der: Uint8Array, type: string): string {
  let binary = ''
  for (let i = 0; i < der.length; i++) binary += String.fromCharCode(der[i])
  const lines = btoa(binary).match(/.{1,64}/g) || []
  return `-----BEGIN ${type}-----\n${lines.join('\n')}\n-----END ${type}-----`
}

function indentPem(pem: string, spaces: number): string {
  const pad = ' '.repeat(spaces)
  return pem.split('\n').map((line) => pad + line).join('\n')
}

function keyXml(algorithm: string, privatePem: string, certPems: string[]): string {
  const certs = certPems.map((pem) => `            <Certificate format="pem">\n${indentPem(pem, 16)}\n            </Certificate>`).join('\n')
  return `        <Key algorithm="${algorithm}">
            <PrivateKey format="pem">
${indentPem(privatePem, 16)}
            </PrivateKey>
            <CertificateChain>
                <NumberOfCertificates>${certPems.length}</NumberOfCertificates>
${certs}
            </CertificateChain>
        </Key>`
}

export function extractKeyboxFromSoftAttestationSo(base64: string): string {
  const raw = atob(base64.replace(/\s+/g, ''))
  const buf = new Uint8Array(raw.length)
  for (let i = 0; i < raw.length; i++) buf[i] = raw.charCodeAt(i)

  let rsaKey: Uint8Array | null = null
  let ecKey: Uint8Array | null = null
  const rsaCerts: Uint8Array[] = []
  const ecCerts: Uint8Array[] = []

  for (const der of findDerObjects(buf)) {
    if (looksLikeRsaPrivate(der)) {
      rsaKey = der
      continue
    }
    if (looksLikeEcPrivate(der)) {
      ecKey = der
      continue
    }
    if (!looksLikeCert(der)) continue
    const algo = certAlgo(der)
    if (algo === 'rsa') rsaCerts.push(der)
    else if (algo === 'ecdsa') ecCerts.push(der)
  }

  const keys: string[] = []
  if (ecKey && ecCerts.length > 0) {
    keys.push(keyXml('ecdsa', toPem(ecKey, 'EC PRIVATE KEY'), ecCerts.map((der) => toPem(der, 'CERTIFICATE'))))
  }
  if (rsaKey && rsaCerts.length > 0) {
    keys.push(keyXml('rsa', toPem(rsaKey, 'RSA PRIVATE KEY'), rsaCerts.map((der) => toPem(der, 'CERTIFICATE'))))
  }
  if (keys.length === 0) {
    throw new Error('stock software attestation keys not found')
  }

  return `<?xml version="1.0"?>
<AndroidAttestation>
    <NumberOfKeyboxes>1</NumberOfKeyboxes>
    <Keybox DeviceID="sw">
${keys.join('\n')}
    </Keybox>
</AndroidAttestation>
`
}

export const ORIGINAL_LIB_PATHS = SOFT_ATTESTATION_LIBS
export const ORIGINAL_XML_PATHS = STOCK_KEYBOX_XML
