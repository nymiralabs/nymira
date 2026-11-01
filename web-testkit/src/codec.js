// The browser NEAR codec (`crates/nymira-near-codec-wasm`, built by
// `build-codec.sh` into `codec-pkg/`), for Node and for pages.

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

export const codecDir = fileURLToPath(new URL('../codec-pkg/', import.meta.url));
const GLUE = 'nymira_near_codec_wasm.js';
const WASM = 'nymira_near_codec_wasm_bg.wasm';

/** Loads the codec in Node: chainId, contractAddress, destination, unixSeconds, noteFields,
 *  proofFromWire, verifyingKeyFromCanonical, publicInputFromWire. */
export async function loadCodec() {
  let wasm;
  try {
    wasm = await readFile(codecDir + WASM);
  } catch {
    throw new Error('codec not built: run web-testkit/build-codec.sh');
  }
  const codec = await import(codecDir + GLUE);
  codec.initSync({ module: wasm });
  return codec;
}

/** Origin the page-side loader serves the codec from. */
export const CODEC_ORIGIN = 'https://nymira.test';

const TYPES = { '.js': 'text/javascript', '.wasm': 'application/wasm' };

/** Serves `codec-pkg/` at `CODEC_ORIGIN/codec/` and an empty page at `CODEC_ORIGIN/blank`
 *  for this context. Web tests that load the codec from their own bundle do not need it. */
export async function serveCodec(context) {
  await context.route(`${CODEC_ORIGIN}/**`, async (route) => {
    const { pathname } = new URL(route.request().url());
    if (pathname === '/blank') {
      return route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>blank</title>' });
    }
    const name = pathname.replace(/^\/codec\//, '');
    if (pathname.startsWith('/codec/') && (name === GLUE || name === WASM)) {
      const ext = name.slice(name.lastIndexOf('.'));
      return route.fulfill({
        contentType: TYPES[ext],
        headers: { 'access-control-allow-origin': '*' },
        body: await readFile(codecDir + name),
      });
    }
    return route.fulfill({ status: 404, body: 'not found' });
  });
}

/** Loads the codec into the page as `window.nymiraCodec` (call `serveCodec` first). */
export async function loadCodecInPage(page) {
  await page.evaluate(
    async ({ base, glue, wasm }) => {
      const module = await import(`${base}/codec/${glue}`);
      await module.default({ module_or_path: `${base}/codec/${wasm}` });
      window.nymiraCodec = module;
    },
    { base: CODEC_ORIGIN, glue: GLUE, wasm: WASM },
  );
}
