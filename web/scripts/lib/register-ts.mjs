// Lets a Node script import the app's TypeScript modules, which import each other without a
// file extension. Use with `node --import ./scripts/lib/register-ts.mjs`.
import { register } from 'node:module';

register(
  'data:text/javascript,' +
    encodeURIComponent(`
export async function resolve(specifier, context, next) {
  try {
    return await next(specifier, context);
  } catch (e) {
    if (e.code === 'ERR_MODULE_NOT_FOUND' && specifier.startsWith('.')) {
      for (const tail of ['.ts', '/index.ts']) {
        try { return await next(specifier + tail, context); } catch {}
      }
    }
    throw e;
  }
}`),
  import.meta.url,
);
