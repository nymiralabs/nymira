// A NEAR wallet for tests. It implements the `NearWalletBase` interface that
// NEAR Connect (`@hot-labs/near-connect`) calls, signs real transactions with
// an ed25519 test key and sends them to a configurable JSON-RPC URL (the
// sandbox or the fake RPC). No browser extension. Usable from plain Node;
// `playwright.js` puts it in a page.

import { KeyPair, KeyPairSigner, actions, baseDecode, createTransaction } from 'near-api-js';

const MANIFEST = {
  id: 'nymira-test-wallet',
  platform: ['web'],
  name: 'Nymira test wallet',
  icon: '',
  description: 'Signs with a test key held by the test runner.',
  website: '',
  version: '0.0.0',
  executor: '',
  type: 'injected',
  permissions: {},
  features: {
    signMessage: true,
    signTransaction: false,
    signAndSendTransaction: true,
    signAndSendTransactions: true,
    signInWithoutAddKey: true,
    signInAndSignMessage: true,
    signInWithFunctionCallKey: false,
    signDelegateActions: false,
    mainnet: false,
    testnet: true,
  },
};

/** NEAR Connect `ConnectorAction` to a near-api-js action. */
function toAction(action) {
  switch (action.type) {
    case 'FunctionCall': {
      const { methodName, args, gas, deposit } = action.params;
      const bytes =
        args instanceof Uint8Array ? args : new TextEncoder().encode(JSON.stringify(args ?? {}));
      return actions.functionCall(methodName, bytes, BigInt(gas), BigInt(deposit));
    }
    case 'Transfer':
      return actions.transfer(BigInt(action.params.deposit));
    default:
      throw new Error(`test wallet: unsupported action ${action.type}`);
  }
}

export class NearTestWallet {
  /**
   * @param {object} options
   * @param {string} options.accountId       account the wallet signs for
   * @param {string} options.rpcUrl          JSON-RPC endpoint for nonce, block hash and send
   * @param {string} [options.secretKey]     `ed25519:...`; a random key when omitted
   * @param {'testnet'|'mainnet'} [options.network]
   * @param {string} [options.waitUntil]     `send_tx` finality, `FINAL` by default
   * @param {boolean} [options.signedIn]     start signed in
   */
  constructor({ accountId, rpcUrl, secretKey, network = 'testnet', waitUntil = 'FINAL', signedIn = false }) {
    this.accountId = accountId;
    this.rpcUrl = rpcUrl;
    this.network = network;
    this.waitUntil = waitUntil;
    this.keyPair = secretKey ? KeyPair.fromString(secretKey) : KeyPair.fromRandom('ed25519');
    this.signer = new KeyPairSigner(this.keyPair);
    this.publicKey = this.keyPair.getPublicKey().toString();
    this.manifest = MANIFEST;
    this.signedIn = signedIn;
    /** Every transaction this wallet sent: `{ receiverId, actions, hash, outcome }`. */
    this.sent = [];
    this.nonce = 0n;
  }

  async rpc(method, params) {
    const response = await fetch(this.rpcUrl, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 'nymira-test-wallet', method, params }),
    });
    const body = await response.json();
    if (body.error) throw new Error(`rpc ${method}: ${JSON.stringify(body.error)}`);
    return body.result;
  }

  async signIn() {
    this.signedIn = true;
    return this.getAccounts();
  }

  async signInAndSignMessage({ messageParams }) {
    const [account] = await this.signIn();
    return [{ ...account, signedMessage: await this.signMessage(messageParams) }];
  }

  async signOut() {
    this.signedIn = false;
  }

  async getAccounts() {
    return this.signedIn ? [{ accountId: this.accountId, publicKey: this.publicKey }] : [];
  }

  /** NEP-413. The signature is base64, as NEAR Connect wallets return it. */
  async signMessage({ message, recipient, nonce, callbackUrl }) {
    const signed = await this.signer.signNep413Message(this.accountId, {
      message,
      recipient,
      nonce: Uint8Array.from(nonce),
      callbackUrl,
    });
    return {
      accountId: this.accountId,
      publicKey: signed.publicKey.toString(),
      signature: Buffer.from(signed.signature).toString('base64'),
    };
  }

  async signAndSendTransaction({ receiverId, actions: connectorActions, signerId }) {
    this.#checkSigner(signerId);
    if (!this.signedIn) throw new Error('test wallet: not signed in');
    // Nonce: one above the access key's nonce on chain, and above any this wallet already used.
    const key = await this.rpc('query', {
      request_type: 'view_access_key',
      finality: 'final',
      account_id: this.accountId,
      public_key: this.publicKey,
    });
    const block = await this.rpc('block', { finality: 'final' });
    const chainNonce = BigInt(key.nonce);
    this.nonce = (chainNonce > this.nonce ? chainNonce : this.nonce) + 1n;
    const transaction = createTransaction(
      this.accountId,
      this.keyPair.getPublicKey(),
      receiverId,
      this.nonce,
      connectorActions.map(toAction),
      baseDecode(block.header.hash),
    );
    const { signedTransaction, txHash } = await this.signer.signTransaction(transaction);
    const outcome = await this.rpc('send_tx', {
      signed_tx_base64: Buffer.from(signedTransaction.encode()).toString('base64'),
      wait_until: this.waitUntil,
    });
    this.sent.push({
      receiverId,
      actions: connectorActions,
      hash: Buffer.from(txHash).toString('hex'),
      outcome,
    });
    return outcome;
  }

  async signAndSendTransactions({ transactions, signerId }) {
    const outcomes = [];
    for (const transaction of transactions) {
      outcomes.push(await this.signAndSendTransaction({ ...transaction, signerId }));
    }
    return outcomes;
  }

  async signDelegateActions() {
    throw new Error('test wallet: signDelegateActions is not supported');
  }

  #checkSigner(signerId) {
    if (signerId && signerId !== this.accountId) {
      throw new Error(`test wallet signs for ${this.accountId}, not ${signerId}`);
    }
  }
}
