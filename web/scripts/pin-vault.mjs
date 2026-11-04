// Reads a deployed vault over public RPC and prints the `pins` object a profile carries:
//
//   node scripts/pin-vault.mjs --rpc https://rpc.testnet.near.org --vault vault.nymira.testnet
//   node scripts/pin-vault.mjs --network testnet --vault vault.nymira.testnet   (default public RPC)
//
// Review the output against the deployment record (code hash from the reproducible build, the
// ceremony's verifying key hashes, the accounts the team set up), then paste it into
// src/profiles/<profile>.ts as `pins`. The tool only reads; it cannot make a vault trustworthy.
// `teamNoteIds` stays empty here: the team's published note IDs come from the launch record.

const USAGE =
  'usage: node scripts/pin-vault.mjs (--rpc <url> | --network testnet|mainnet) --vault <account>';
const NETWORK_RPC = {
  testnet: 'https://rpc.testnet.near.org',
  mainnet: 'https://rpc.mainnet.near.org',
};

function argument(name, optional = false) {
  const at = process.argv.indexOf(`--${name}`);
  if (at < 0 || !process.argv[at + 1]) {
    if (optional) return undefined;
    console.error(USAGE);
    process.exit(2);
  }
  return process.argv[at + 1];
}

const network = argument('network', true);
if (network !== undefined && !(network in NETWORK_RPC)) {
  console.error(USAGE);
  process.exit(2);
}
const rpc = argument('rpc', true) ?? (network ? NETWORK_RPC[network] : argument('rpc'));
const vault = argument('vault');

async function call(method, params) {
  const response = await fetch(rpc, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 'pin', method, params }),
  });
  const body = await response.json();
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result;
}

async function view(method, args = {}) {
  const result = await call('query', {
    request_type: 'call_function',
    finality: 'final',
    account_id: vault,
    method_name: method,
    args_base64: Buffer.from(JSON.stringify(args)).toString('base64'),
  });
  return JSON.parse(Buffer.from(result.result).toString('utf8'));
}

const account = await call('query', {
  request_type: 'view_account',
  finality: 'final',
  account_id: vault,
});
const keys = await call('query', {
  request_type: 'view_access_key_list',
  finality: 'final',
  account_id: vault,
});
const { tvl_cap, current_tvl, ...config } = await view('config');
void tvl_cap;
void current_tvl;

const pins = {
  codeHash: account.code_hash,
  vkHashes: await view('vk_hashes'),
  config,
  accessKeys: keys.keys.map((k) => ({
    public_key: k.public_key,
    permission: k.access_key.permission,
  })),
  teamNoteIds: [],
};
console.log(JSON.stringify(pins, null, 2));
