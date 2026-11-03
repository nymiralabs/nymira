// A quote verifier for the mock profile: the test gateway's reports carry their quote facts as
// hex-encoded JSON, and this accepts exactly that. It verifies nothing about Intel; it lets the
// Playwright tests drive every check that comes after the quote (nonce binding, event log,
// compose binding, policy, signer). Imported by the mock build only (`runtime.ts`), and
// `check-bundle` refuses the marker string below in a deployable bundle.
import { fromHex } from '../../crypto/bytes';
import { AttestationError, type QuoteFacts, type QuoteVerifier } from './verify';

export const TEST_QUOTE_MARKER = 'nymira-test-quote';

export const testQuote: QuoteVerifier = {
  async verify(quoteHex) {
    try {
      const parsed = JSON.parse(new TextDecoder().decode(fromHex(quoteHex))) as QuoteFacts & {
        kind?: string;
      };
      if (parsed.kind !== TEST_QUOTE_MARKER) throw new Error('not a test quote');
      return parsed;
    } catch {
      throw new AttestationError('quote_invalid', 'the quote is not a test quote');
    }
  },
};
