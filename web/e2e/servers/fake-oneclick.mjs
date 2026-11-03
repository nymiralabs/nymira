// A fake 1Click for the e2e tests: answers dry quotes and records every request body, so a test can
// check what the app sent (and when). GET /health, GET /requests, DELETE /requests.
import { createServer } from 'node:http';

const requests = [];

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
    const amountIn = (BigInt(body.amount) + 15_586n).toString();
    res.writeHead(201, { ...cors, 'content-type': 'application/json' }).end(
      JSON.stringify({
        quote: {
          amountIn,
          amountInFormatted: (Number(amountIn) / 1e6).toFixed(6),
          timeEstimate: 45,
        },
      }),
    );
    return;
  }
  res.writeHead(404, cors).end('not found');
});

server.listen(18547, '127.0.0.1', () => console.log('fake 1Click on http://127.0.0.1:18547'));
