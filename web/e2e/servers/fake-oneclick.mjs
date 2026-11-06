// A fake 1Click for the e2e tests. It answers dry and live quotes signed with the test key (seed
// [44; 32], the one nymirad's fixture uses), serves /v0/status, and records every quote request body,
// so a test can check what the app sent (and when).
//   GET /health, GET /requests, DELETE /requests,
//   POST /mock/pay {depositAddress, txHash, amountOut?}: marks the order SUCCESS with that
//   destination hash (and settled amount).
import { createHash, createPrivateKey, sign } from 'node:crypto';
import { createServer } from 'node:http';

const requests = [];
const statuses = new Map();
let counter = 0;

const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function base58(bytes) {
  let n = BigInt('0x' + Buffer.from(bytes).toString('hex'));
  let out = '';
  while (n > 0n) {
    out = ALPHABET[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    out = '1' + out;
  }
  return out;
}
const key = createPrivateKey({
  key: Buffer.concat([
    Buffer.from('302e020100300506032b657004220420', 'hex'),
    Buffer.alloc(32, 44),
  ]),
  format: 'der',
  type: 'pkcs8',
});
const sorted = (v) =>
  Array.isArray(v)
    ? v.map(sorted)
    : v && typeof v === 'object'
      ? Object.fromEntries(
          Object.keys(v)
            .sort()
            .map((k) => [k, sorted(v[k])]),
        )
      : v;

/** The SDK's signed field selection (src/funding/quote-check.ts has the same list). */
function signed(response) {
  const { quoteRequest: r, quote: q } = response;
  const data = {};
  for (const k of [
    'dry',
    'swapType',
    'slippageTolerance',
    'originAsset',
    'depositType',
    'destinationAsset',
    'amount',
    'refundTo',
    'refundType',
    'recipient',
    'recipientType',
    'deadline',
  ])
    data[k] = r[k];
  for (const k of [
    'quoteWaitingTimeMs',
    'referral',
    'virtualChainRecipient',
    'virtualChainRefundRecipient',
    'customRecipientMsg',
  ])
    if (r[k]) data[k] = r[k];
  for (const k of [
    'amountIn',
    'amountInFormatted',
    'amountInUsd',
    'minAmountIn',
    'amountOut',
    'amountOutFormatted',
    'amountOutUsd',
    'minAmountOut',
  ])
    data[k] = q[k];
  if (r.dry === false)
    for (const k of [
      'depositAddress',
      'depositMemo',
      'deadline',
      'timeWhenInactive',
      'timeEstimate',
      'refundFee',
      'withdrawFee',
    ])
      if (q[k]) data[k] = q[k];
  data.timestamp = response.timestamp;
  const hash = base58(
    createHash('sha256')
      .update(JSON.stringify(sorted(data)))
      .digest(),
  );
  return `ed25519:${base58(sign(null, Buffer.from(hash), key))}`;
}

const server = createServer(async (req, res) => {
  const cors = {
    'access-control-allow-origin': '*',
    'access-control-allow-headers': 'content-type',
    'access-control-allow-methods': 'GET, POST, DELETE, OPTIONS',
  };
  if (req.method === 'OPTIONS') {
    res.writeHead(204, cors).end();
    return;
  }
  if (req.url === '/health') {
    res.writeHead(200, cors).end('ok');
    return;
  }
  if (req.url === '/requests' && req.method === 'GET') {
    res
      .writeHead(200, { ...cors, 'content-type': 'application/json' })
      .end(JSON.stringify(requests));
    return;
  }
  if (req.url === '/requests' && req.method === 'DELETE') {
    requests.length = 0;
    res.writeHead(200, cors).end('ok');
    return;
  }
  if (req.url === '/v0/quote' && req.method === 'POST') {
    let text = '';
    for await (const chunk of req) text += chunk;
    const body = JSON.parse(text);
    requests.push({ at: Date.now(), body });
    const amount = BigInt(body.amount);
    const amountIn = (amount + 15_586n).toString();
    const quote = {
      amountIn,
      amountInFormatted: (Number(amountIn) / 1e6).toFixed(6),
      amountInUsd: '1.07',
      minAmountIn: amountIn,
      amountOut: body.amount,
      amountOutFormatted: (Number(amount) / 1e6).toFixed(6),
      amountOutUsd: '1.05',
      minAmountOut: body.amount,
      timeEstimate: 45,
    };
    if (body.dry === false) {
      quote.depositAddress = `fake-deposit-${++counter}`;
      quote.deadline = body.deadline;
      statuses.set(quote.depositAddress, { status: 'PENDING_DEPOSIT' });
    }
    const response = { timestamp: '2026-11-06T00:00:00.000Z', quoteRequest: body, quote };
    response.signature = signed(response);
    res
      .writeHead(201, { ...cors, 'content-type': 'application/json' })
      .end(JSON.stringify(response));
    return;
  }
  if (req.url?.startsWith('/v0/status') && req.method === 'GET') {
    const address = new URL(req.url, 'http://x').searchParams.get('depositAddress') ?? '';
    res
      .writeHead(200, { ...cors, 'content-type': 'application/json' })
      .end(JSON.stringify(statuses.get(address) ?? { status: 'FAILED' }));
    return;
  }
  if (req.url === '/mock/pay' && req.method === 'POST') {
    let text = '';
    for await (const chunk of req) text += chunk;
    const { depositAddress, txHash, amountOut } = JSON.parse(text);
    statuses.set(depositAddress, {
      status: 'SUCCESS',
      swapDetails: { nearTxHashes: [txHash], ...(amountOut ? { amountOut } : {}) },
    });
    res.writeHead(200, cors).end('ok');
    return;
  }
  res.writeHead(404, cors).end('not found');
});

server.listen(18547, '127.0.0.1', () => console.log('fake 1Click on http://127.0.0.1:18547'));
