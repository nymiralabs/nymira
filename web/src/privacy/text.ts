// Sentences only the web panel says, and the constants it states. The sentences `nymirad status`
// shares come from `shared/privacy-strings.json` (strings.ts). Nothing here claims that leases
// cannot be linked: the panel shows what a reader of the chain and the operator can see.

/** A lease less than this long after the deposit is flagged. */
export const WARN_GAP_SECS = 3600;
/** A lease with fewer external deposits than this in between is flagged. */
export const WARN_EXTERNAL_DEPOSITS = 5;
/** An external set below this size gets the small-set sentence. */
export const SMALL_EXTERNAL_SET = 5;

export const THRESHOLDS =
  `Thresholds used on this page: a lease less than ${WARN_GAP_SECS / 3600} hour after the deposit, ` +
  `or with fewer than ${WARN_EXTERNAL_DEPOSITS} external deposits in between, is flagged; ` +
  `an external set under ${SMALL_EXTERNAL_SET} is called small.`;

export const ADDRESS =
  'The operator, and the hosts in front of it, see your network address and the time of every lease. ' +
  'The RPC providers this page asks for the set size see your address too. ' +
  'This page cannot hide them and never shows an address. ' +
  'Use Tor Browser, or run nymirad, which sends its traffic through Tor.';

export const POLICY_LABEL =
  'The operator’s published retention policy. The client cannot check it.';

export const POLICY =
  'After a lease settles, the operator keeps the nullifier, the charge, the time rounded to the hour ' +
  'and the delivery status. It keeps the signed states and request proofs for 44 days, on both of ' +
  'its machines, and deletes the key ID after the provider’s usage is reconciled. It copies these ' +
  'records to its second machine.';

export const TEAM_DECLARATION =
  'The team list is the team’s own declaration of the notes it holds. The pinned list ships with ' +
  'this build, and the operator may list more IDs; any difference between the two is shown.';

export const INCLUDES_OWN =
  'N includes your own note. One person can hold several notes, so N can count the same person more than once.';

export const SMALL_SET = 'With so few notes in the set, matching a lease to a deposit is easy.';

export const STANDS_OUT = (bound: string) =>
  `This lease used the ${bound} class. Few leases use it and the operator sees the bound, so it stands out.`;

export const TIMING_WARNING = 'Timing can match this lease to the deposit.';

export const NO_RECORD =
  'No set size was recorded for this lease. It was made before this figure existed.';

export const LEFT_INTRO =
  'What anyone can read on chain about this note. It comes from the record this browser keeps; the page asks no node for it.';

export const RIGHT_INTRO = 'What the operator saw for each lease on this note.';

export function describeGap(secs: number): string {
  if (secs < 90) return 'under a minute';
  if (secs < 5400) return `${Math.round(secs / 60)} minutes`;
  if (secs < 129_600) return `${Math.round(secs / 3600)} hours`;
  return `${Math.round(secs / 86_400)} days`;
}
