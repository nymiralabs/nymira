// What NEAR AI's live attestation reports measure, and whether the signed policy lists it
// (ADR 0021, the watcher). Nothing here verifies a quote: the watcher only says "this deployment is
// not in the policy". A person records and checks the evidence before signing a new version.
import { createHash } from 'node:crypto';

const DSTACK_RUNTIME_EVENT = 0x0800_0001;
const sha256hex = (s) => createHash('sha256').update(s).digest('hex');
const norm = (h) =>
  typeof h === 'string' ? (h.startsWith('0x') ? h.slice(2) : h).toLowerCase() : '';

/** The fields of an Intel TDX v4 quote that a deployment is identified by (offsets of the TD report). */
export function parseQuote(quoteHex) {
  const hex = norm(quoteHex);
  const bytes = Buffer.from(hex, 'hex');
  const BODY = 48; // the quote header
  if (bytes.length < BODY + 584 || bytes.readUInt16LE(0) !== 4 || bytes.readUInt32LE(4) !== 0x81) {
    throw new Error('not a TDX v4 quote');
  }
  const at = (off, len = 48) => bytes.subarray(BODY + off, BODY + off + len).toString('hex');
  // TD report: tee_tcb_svn 16, mr_seam 48, mr_signer_seam 48, seam_attributes 8, td_attributes 8,
  // xfam 8, mr_td 48, mr_config_id 48, mr_owner 48, mr_owner_config 48, rtmr0..3 48 each.
  return {
    mr_td: at(136),
    mr_config_id: at(184),
    rtmr0: at(328),
    rtmr1: at(376),
    rtmr2: at(424),
    rtmr3: at(472),
  };
}

/** The OS image hash and the compose hash the dstack runtime recorded in RTMR3. */
function runtimeHashes(eventLog) {
  let list = eventLog;
  if (typeof list === 'string') list = JSON.parse(list);
  const out = {};
  for (const e of Array.isArray(list) ? list : []) {
    if (e?.imr !== 3 || e.event_type !== DSTACK_RUNTIME_EVENT) continue;
    if (e.event === 'os-image-hash') out.osImageHash = norm(e.event_payload);
    if (e.event === 'compose-hash') out.composeHash = norm(e.event_payload);
  }
  return out;
}

/** One deployment as one report entry shows it. */
export function observe(role, evidence) {
  const q = parseQuote(evidence.intel_quote);
  const rt = runtimeHashes(evidence.event_log);
  const compose = evidence.info?.tcb_info?.app_compose;
  const composeFile = typeof compose === 'string' ? sha256hex(compose) : undefined;
  const compose_hash = rt.composeHash ?? composeFile;
  if (!compose_hash || !rt.osImageHash)
    throw new Error('the report has no compose or OS image hash');
  return {
    role,
    compose_hash,
    os_image_hash: rt.osImageHash,
    mr_td: q.mr_td,
    rtmr0: q.rtmr0,
    rtmr1: q.rtmr1,
    rtmr2: q.rtmr2,
  };
}

/** Every deployment in a model report (for one model) and in a gateway report. */
export function observeReports(model, modelReport, gatewayReport) {
  const out = [];
  for (const e of modelReport?.model_attestations ?? []) {
    if (e.model_name !== undefined && e.model_name !== model) continue;
    out.push(observe('model', e));
  }
  if (out.length === 0) throw new Error(`no attestation evidence for model ${model}`);
  const g = gatewayReport?.gateway_attestation;
  if (!g) throw new Error('no gateway attestation evidence');
  out.push(observe('gateway', g));
  return out;
}

const same = (d, o) =>
  d.role === o.role &&
  d.mr_td === o.mr_td &&
  d.rtmr0 === o.rtmr0 &&
  d.rtmr1 === o.rtmr1 &&
  d.rtmr2 === o.rtmr2 &&
  d.os_image_hash === o.os_image_hash;
const takesCompose = (d, hash) =>
  (d.role === 'gateway' && d.compose_hashes.length === 1 && d.compose_hashes[0] === '*') ||
  d.compose_hashes.includes(hash);

/** The observed deployments the policy does not list, each with why: `compose` or `image`. */
export function unknownDeployments(policy, observed) {
  const out = [];
  for (const o of observed) {
    const entries = policy.deployments.filter((d) => same(d, o));
    if (entries.some((d) => takesCompose(d, o.compose_hash))) continue;
    const key = `${o.role}|${o.compose_hash}|${o.os_image_hash}|${o.mr_td}`;
    if (out.some((u) => u.key === key)) continue;
    out.push({ ...o, key, why: entries.length > 0 ? 'compose' : 'image' });
  }
  return out;
}

/** An unsigned draft of the next version: a new compose hash joins its entry, a new image gets one. */
export function draftFrom(policy, unknown) {
  const draft = structuredClone(policy);
  draft.version = policy.version + 1;
  draft.signature = '';
  for (const u of unknown) {
    const entry = draft.deployments.find((d) => same(d, u));
    if (entry) {
      if (!entry.compose_hashes.includes(u.compose_hash)) entry.compose_hashes.push(u.compose_hash);
    } else {
      draft.deployments.push({
        role: u.role,
        compose_hashes: [u.compose_hash],
        os_image_hash: u.os_image_hash,
        mr_td: u.mr_td,
        rtmr0: u.rtmr0,
        rtmr1: u.rtmr1,
        rtmr2: u.rtmr2,
      });
    }
  }
  return draft;
}
