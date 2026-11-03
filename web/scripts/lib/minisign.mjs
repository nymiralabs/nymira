// Minisign signatures, the parts the integrity check needs (format: INTEGRITY.md).
// Verification only handles the prehashed "ED" algorithm: Ed25519 over the BLAKE2b-512 digest of
// the file, plus the global signature over (signature || trusted comment). Legacy "Ed" signatures
// are refused, as `minisign -V -H` does. `signForTest` exists so tests can make a signature with a
// throwaway key; the real signing key never enters this repository (use the `minisign` tool).
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  sign,
  verify,
} from 'node:crypto';

const SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');
const PKCS8_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');

const b64 = (text) => Buffer.from(text.trim(), 'base64');

function lines(text) {
  return text.replace(/\r\n/g, '\n').split('\n');
}

/** `{ keyId: Buffer(8), publicKey: Buffer(32) }` from a minisign public key file. */
export function parsePublicKey(text) {
  const [comment, key] = lines(text);
  if (!comment?.startsWith('untrusted comment:')) throw new Error('public key: missing comment');
  const raw = b64(key ?? '');
  if (raw.length !== 42 || raw.subarray(0, 2).toString('latin1') !== 'Ed') {
    throw new Error('public key: not a minisign Ed25519 key');
  }
  return { keyId: raw.subarray(2, 10), publicKey: raw.subarray(10) };
}

/** SHA-256 of the raw 32-byte public key, hex: the fingerprint the project publishes. */
export function fingerprint(publicKeyText) {
  return createHash('sha256').update(parsePublicKey(publicKeyText).publicKey).digest('hex');
}

export function parseSignature(text) {
  const [comment, sigLine, trusted, globalLine] = lines(text);
  if (!comment?.startsWith('untrusted comment:')) throw new Error('signature: missing comment');
  if (!trusted?.startsWith('trusted comment: ')) {
    throw new Error('signature: missing trusted comment');
  }
  const raw = b64(sigLine ?? '');
  const alg = raw.subarray(0, 2).toString('latin1');
  if (raw.length !== 74) throw new Error('signature: wrong length');
  if (alg !== 'ED')
    throw new Error(`signature: algorithm "${alg}" is refused, only prehashed "ED"`);
  const global = b64(globalLine ?? '');
  if (global.length !== 64) throw new Error('signature: wrong global signature length');
  return {
    keyId: raw.subarray(2, 10),
    signature: raw.subarray(10),
    trustedComment: trusted.slice('trusted comment: '.length),
    globalSignature: global,
  };
}

const edKey = (publicKey) =>
  createPublicKey({ key: Buffer.concat([SPKI_PREFIX, publicKey]), format: 'der', type: 'spki' });

/** Throws unless `signatureText` is a valid signature of `message` by `publicKeyText`. */
export function verifyMinisign(message, signatureText, publicKeyText) {
  const pub = parsePublicKey(publicKeyText);
  const sig = parseSignature(signatureText);
  if (!pub.keyId.equals(sig.keyId)) throw new Error('signature: made by a different key');
  const key = edKey(pub.publicKey);
  const digest = createHash('blake2b512').update(message).digest();
  if (!verify(null, digest, key, sig.signature)) {
    throw new Error('signature: file signature is invalid');
  }
  const signed = Buffer.concat([sig.signature, Buffer.from(sig.trustedComment, 'utf8')]);
  if (!verify(null, signed, key, sig.globalSignature)) {
    throw new Error('signature: trusted comment signature is invalid');
  }
  return { keyId: pub.keyId.toString('hex'), trustedComment: sig.trustedComment };
}

/** A throwaway key pair in minisign's file format (tests only). */
export function generateTestKey() {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const seed = privateKey.export({ format: 'der', type: 'pkcs8' }).subarray(PKCS8_PREFIX.length);
  const pub = publicKey.export({ format: 'der', type: 'spki' }).subarray(SPKI_PREFIX.length);
  const keyId = Buffer.from('4e594d5f54455354', 'hex'); // "NYM_TEST"
  const body = Buffer.concat([Buffer.from('Ed'), keyId, pub]).toString('base64');
  return { seed, keyId, publicText: `untrusted comment: nymira test key\n${body}\n` };
}

export function signForTest(key, message, trustedComment) {
  const priv = createPrivateKey({
    key: Buffer.concat([PKCS8_PREFIX, key.seed]),
    format: 'der',
    type: 'pkcs8',
  });
  const digest = createHash('blake2b512').update(message).digest();
  const signature = sign(null, digest, priv);
  const global = sign(null, Buffer.concat([signature, Buffer.from(trustedComment, 'utf8')]), priv);
  return [
    'untrusted comment: signature from nymira test key',
    Buffer.concat([Buffer.from('ED'), key.keyId, signature]).toString('base64'),
    `trusted comment: ${trustedComment}`,
    global.toString('base64'),
    '',
  ].join('\n');
}
