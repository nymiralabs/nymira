// A NEAR Connect executor for tests. NEAR Connect loads it into a sandboxed iframe and calls
// these methods through postMessage, the way it calls Meteor's or Intear's executor. It signs in
// a fixed account with a key it generates, and signs the NEP-413 sign-in message with it, so the
// app's check has something real to verify. It does not send transactions.
(function () {
  var accountId = 'sandbox.test';
  var ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
  var keyPair = null;
  var publicKey = null;

  function base58(bytes) {
    var n = 0n;
    for (var i = 0; i < bytes.length; i++) n = (n << 8n) | BigInt(bytes[i]);
    var out = '';
    while (n > 0n) {
      out = ALPHABET[Number(n % 58n)] + out;
      n = n / 58n;
    }
    return out;
  }

  function borshString(text) {
    var body = new TextEncoder().encode(text);
    var out = new Uint8Array(4 + body.length);
    new DataView(out.buffer).setUint32(0, body.length, true);
    out.set(body, 4);
    return out;
  }

  async function ensureKey() {
    if (keyPair) return;
    keyPair = await crypto.subtle.generateKey('Ed25519', true, ['sign', 'verify']);
    var raw = new Uint8Array(await crypto.subtle.exportKey('raw', keyPair.publicKey));
    publicKey = 'ed25519:' + base58(raw);
  }

  async function signNep413(p) {
    await ensureKey();
    var tag = new Uint8Array(4);
    new DataView(tag.buffer).setUint32(0, 2 ** 31 + 413, true);
    var parts = [tag, borshString(p.message), Uint8Array.from(p.nonce), borshString(p.recipient), Uint8Array.of(0)];
    var all = new Uint8Array(parts.reduce(function (n, x) { return n + x.length; }, 0));
    var at = 0;
    parts.forEach(function (x) { all.set(x, at); at += x.length; });
    var hash = await crypto.subtle.digest('SHA-256', all);
    var signature = new Uint8Array(await crypto.subtle.sign('Ed25519', keyPair.privateKey, hash));
    var text = '';
    signature.forEach(function (b) { text += String.fromCharCode(b); });
    return { accountId: accountId, publicKey: publicKey, signature: btoa(text) };
  }

  var wallet = {
    signIn: async function () {
      await ensureKey();
      return [{ accountId: accountId, publicKey: publicKey }];
    },
    signInAndSignMessage: async function (params) {
      await ensureKey();
      var signed = await signNep413(params.messageParams);
      return [{ accountId: accountId, publicKey: publicKey, signedMessage: signed }];
    },
    signOut: async function () {},
    getAccounts: async function () {
      await ensureKey();
      return [{ accountId: accountId, publicKey: publicKey }];
    },
    signMessage: async function (params) {
      return signNep413(params);
    },
    signAndSendTransaction: async function () {
      throw new Error('the fake sandbox wallet does not send transactions');
    },
    signAndSendTransactions: async function () {
      throw new Error('the fake sandbox wallet does not send transactions');
    },
  };
  window.selector.ready(wallet);
})();
