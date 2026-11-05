// A NEAR Connect executor for the popup hand-off test (e2e/popup.spec.ts). It signs in like
// fake-wallet.js, and sends a transaction the way Meteor does: after async work (1.5 s here) it
// opens the wallet's approval window from the executor and waits for that window to say it is
// approved. A browser that has lost the user's click refuses that window.
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
      await new Promise(function (resolve) { setTimeout(resolve, 1500); });
      var popup = window.open('http://localhost:4173/popup-approve.html', '_blank', 'popup');
      // NEAR Connect answers with a null window id when the browser refused the window.
      if ((await popup.id()) === null) throw new Error('the browser blocked the approval window');
      return new Promise(function (resolve) {
        window.addEventListener('message', function (event) {
          if (event.data && event.data.type === 'approved') {
            resolve({ transaction: { hash: 'popup-test-hash', signer_id: accountId } });
          }
        });
      });
    },
    signAndSendTransactions: async function () {
      throw new Error('this test wallet sends one transaction at a time');
    },
  };
  window.selector.ready(wallet);
})();
