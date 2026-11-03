// The Intel TDX quote verifier of the app: dcap-qvl compiled to WASM (`crates/nymira-web-wasm`),
// fed collateral from a PCCS (Phala's: Intel's own service sends no CORS headers, so a browser
// cannot reach it). The collateral is signed by Intel's chain and checked against the root
// compiled into dcap-qvl, so the PCCS is trusted for availability only.
import { fromHex } from '../../crypto/bytes';
import { AttestationError, type QuoteFacts, type QuoteVerifier } from './verify';

/** The WASM throws strings; the checks above it branch on an `AttestationError` code. */
function checked(wasm: QuoteWasm, quote: Uint8Array, collateral: string, now: number): QuoteFacts {
  try {
    return JSON.parse(wasm.verify_tdx_quote(quote, collateral, BigInt(now))) as QuoteFacts;
  } catch (e) {
    throw new AttestationError('quote_invalid', `the Intel quote does not verify: ${String(e)}`);
  }
}

/** The slice of the `web-checks` WASM module this file uses. */
export interface QuoteWasm {
  fetch_collateral(pccsUrl: string, quote: Uint8Array): Promise<string>;
  verify_tdx_quote(quote: Uint8Array, collateralJson: string, nowSecs: bigint): string;
}

export const PCCS_URL = 'https://pccs.phala.network';

export function liveQuoteVerifier(
  wasm: QuoteWasm,
  pccsUrl: string = PCCS_URL,
  nowSecs: () => number = () => Math.floor(Date.now() / 1000),
): QuoteVerifier {
  return {
    async verify(quoteHex) {
      const quote = fromHex(quoteHex.replace(/^0x/, ''));
      let collateral: string;
      try {
        collateral = await wasm.fetch_collateral(pccsUrl, quote);
      } catch (e) {
        throw new AttestationError(
          'collateral_unavailable',
          `the quote’s collateral could not be fetched: ${String(e)}`,
        );
      }
      return checked(wasm, quote, collateral, nowSecs());
    },
  };
}

/** A recording checked at the time it was made (tests). */
export function recordedQuoteVerifier(
  wasm: QuoteWasm,
  collateralJson: string,
  atSecs: number,
): QuoteVerifier {
  return {
    async verify(quoteHex) {
      const quote = fromHex(quoteHex.replace(/^0x/, ''));
      return checked(wasm, quote, collateralJson, atSecs);
    },
  };
}
