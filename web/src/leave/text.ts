// The fixed wording of the close screen. The guarantee sentence is the one in the web spec, word
// for word (a test compares it); the lines around it say what the sentence does not cover and what
// leaving shows on chain. Nothing here claims more than the threat model supports.

/** The guarantee, with the method names in backticks (the screen shows them as code). */
export const PUBLIC_SENTENCE =
  'The operator cannot send your money to another account or block your exit: with `request_exit` you leave to your payout account after 7 days, and if you hold your latest state, the operator can deduct at most the bound of the one lease in flight. A note expires 30 days after the deposit. Seven days later the treasury can take it, unless an exit is pending; until then you can still exit. The team can still update the vault code, and open user deposits in the vault are capped at 100 USDC.';

/** The limit that sits next to the sentence. */
export const OLD_STATE_LIMIT =
  'The limit: an exit from an old state can be defeated. If you lose your latest state (the browser’s storage and your backup), the operator can answer your exit with a later request and keep what that request charged, up to the whole remaining balance. Keep a current backup.';

/** What closing puts on chain. */
export const CLOSE_REVEALS =
  'Closing shows your note ID, your total spend and your payout account on chain, next to the account that sends the close. Sending it from the account that funded the note puts the two side by side; another account with a little NEAR keeps them apart on chain. A close also asks the operator for a clearance, so the operator sees the ownership proof, your IP address and the time. nymirad waits 1 to 20 minutes before it asks; this app does not, so a close soon after a conversation is easy to tie to it. The RPC provider sees your IP address on every read, and this app cannot use Tor.';

export const GAS_NOTE =
  '`request_exit` and `finalize_exit` need a little NEAR for gas. Any account can send them, and the payout still goes to your payout account. Connect an account that holds NEAR; it does not have to be the one that funded the note.';

export const CLAIM_NOTE =
  'Reading `pending_payout` shows your payout account to the RPC provider, together with your IP address. If a payout could not be sent, it waits in the vault until someone calls `claim_payout` for that account.';

export const OPERATOR_CLOSE_NOTE =
  'No NEAR? The operator can send the close for you. It deducts the close fee from your payout, and the transaction shows the operator’s account as the sender.';

/** The close-screen texts as plain strings (the backticks stripped), for the tests. */
export const plain = (text: string): string => text.replaceAll('`', '');
