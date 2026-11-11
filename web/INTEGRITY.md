# Web integrity

[README](../README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](../JUDGES.md) · [Threat model](../THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

How a web release is built, hashed, signed, published and checked. This file is the contract that
`nymirad verify-web` reads. Everything here is exact on purpose: another program has to reproduce
it byte for byte.

## What the signature covers, and what it does not

A release is a directory of static files (the bundle) plus the headers a host sends with them. The
signed manifest names both:

```json
{
  "version": "0.1.0",
  "bundle_hash": "<64 lowercase hex>",
  "header_set_hash": "<64 lowercase hex>"
}
```

It proves that the project's offline key vouched for those hashes. It does not stop a compromised
host from serving something else, and it does not tell a user what the code does. A user who never
runs `verify-web` gets nothing from it. A host that shows `verify-web` a clean copy and users a
different one (cloaking) defeats it; running it over Tor and directly, against two hosts, makes that
harder, not impossible.

## Files of a published site

Paths are relative to the site root (`https://nymira.pages.dev/`).

| Path                      | Content                                                                                     |
| ------------------------- | ------------------------------------------------------------------------------------------- |
| `manifest.json`           | The manifest above, canonical form (below).                                                 |
| `manifest.json.minisig`   | The minisign signature of `manifest.json`.                                                  |
| `integrity/files.txt`     | The bundle listing. Its SHA-256 is `bundle_hash`.                                           |
| `integrity/headers.txt`   | The signed header set. Its SHA-256 is `header_set_hash`.                                    |
| `_headers`                | The Cloudflare Pages header file. Served by no one (Pages consumes it); not hashed.         |
| `attestation-policy.json` | Optional. A newer signed attestation policy than the one in the bundle; not hashed (below). |
| every other file          | The bundle: each one is a line of `integrity/files.txt`.                                    |

The mirror (`https://nymiralabs.github.io/nymira/`) holds the same files except that it cannot send
the headers.

## A newer attestation policy beside a release

NEAR AI redeploys its models and each redeploy changes a compose hash, so the policy that lists the
deployments the app accepts goes stale. A deploy may therefore put one more file next to a release:
`attestation-policy.json`, the policy signed offline with the policy key. It is not in
`integrity/files.txt`, so `bundle_hash` and the release signature do not change.

The page asks its own origin for the file when a chat starts. It uses the file only if the policy
signature verifies under the policy signer key that is inside the bundle, the version is higher than
the bundled policy's, and the file loads under the same rules as the bundled one (strict `UpToDate`
TCB, well-formed deployments). A missing, older, unsigned or unreachable file leaves the bundled
policy in force. So the file cannot widen what the signed bundle allows beyond what the policy key
signs: whoever controls the host can withhold a newer policy, but cannot make the page accept a
deployment the policy key did not sign.

The `policy-watch` workflow reads NEAR AI's live reports every ten minutes (it verifies no quote).
A new compose hash of the model, on an image and measurements the policy already lists, is signed
by the workflow with the policy key, a secret of the `policy-signing` environment (limited to
`main`), at most three times in 24 hours. It commits the file, starts `web-deploy` and opens an issue
labelled `policy-auto`. Anything else opens an issue labelled `policy-watch`, and a person runs
`scripts/policy-update.sh`. The cost: whoever can run workflows on `main`, or take over the account,
can sign a policy. That is accepted for the testnet, where the policy already trusts what NEAR AI's
live endpoint shows.

The `web-deploy` workflow takes the file from the commit named in its `policy_ref` input, loads it
with `policy_tool verify` (signature, version above the bundled one, strict TCB), copies it into the
verified site and runs `integrity.mjs verify` again. `check-live.mjs` compares the live file with the
one that was deployed. `verify-web` is unchanged: it checks the bundle, and the policy file checks
itself.

## Bundle hash

1. The bundle is every file of the site except `_headers`, `manifest.json`,
   `manifest.json.minisig`, `integrity/files.txt` and `integrity/headers.txt`.
2. Sort the paths (relative, `/` separators, no leading `/`) by the byte order of their UTF-8.
3. For each path write one line: `<SHA-256 of the file, 64 lowercase hex>`, two spaces, the path,
   `\n`. This is what `sha256sum` prints.
4. `integrity/files.txt` is those lines, nothing before, nothing after, no trailing blank line.
5. `bundle_hash` is the SHA-256 of `integrity/files.txt`, lowercase hex.

Anyone can recompute it: `cd site && find . -type f ! -path ./_headers ! -path ./manifest.json
! -path ./manifest.json.minisig ! -path './integrity/*' | sed 's|^\./||' | LC_ALL=C sort | xargs
sha256sum | sha256sum` gives the same value for ASCII paths (the bundle has only ASCII paths).

`index.html` is the file served at `/`. `404.html` is served only with status 404 on an unknown
path, so a client fetches an unknown path (for example `/nymira-verify-404`) to read it, and
requires status 404.

## Header-set hash

The host sends different headers for three kinds of path:

| Class    | Paths                                  |
| -------- | -------------------------------------- |
| `app`    | everything not listed below            |
| `assets` | `/assets/*` and `/executors/*`         |
| `frame`  | `/wallet-host.html` and `/wallet-host` |

`integrity/headers.txt` holds the three sets, always in this order (`app`, `assets`, `frame`):

```
# app
content-security-policy: <value>
permissions-policy: <value>
referrer-policy: <value>
strict-transport-security: <value>
x-content-type-options: <value>
# assets
access-control-allow-origin: *
content-security-policy: ...
...
# frame
content-security-policy: ...
...
```

- A class starts with the line `# <class>`.
- Header names are lowercase and in this order, each only if the class has it:
  `access-control-allow-origin`, `content-security-policy`, `permissions-policy`,
  `referrer-policy`, `strict-transport-security`, `x-content-type-options`.
- A line is `name: value` (one space after the colon). Every line ends with `\n`. No blank lines.
- `header_set_hash` is the SHA-256 of the file, lowercase hex.

Only these six headers are signed. A host may add others (Cloudflare adds its own); a client
compares only these. A header that is present but not in the class's set is not an error; a signed
header that is missing or different is. For the `app` class `access-control-allow-origin` must be
absent from the response.

`headers.ts` is the one source of the sets. The build writes `_headers` and `integrity/headers.txt`
from it. The release tool refuses a set whose policy has `unsafe-eval`, a wildcard source, a
script `unsafe-inline` (outside the sandboxed frame) or a missing `frame-ancestors`, Trusted Types or
`base-uri`, `form-action`, `object-src` rule (`scripts/lib/csp.mjs`).

## Manifest

Canonical form: `JSON.stringify({version, bundle_hash, header_set_hash}, null, 2)` plus one `\n`.
Exactly these three keys in this order, two-space indent, LF. A reader must also accept only this
form: the signature covers the bytes, so a reader that parses first and re-serializes could be
fooled by duplicate keys.

## Signature

`manifest.json.minisig` is a minisign signature file (format: <https://jedisct1.github.io/minisign/>),
four lines:

```
untrusted comment: <anything, not signed>
<base64 of: "ED" || key_id (8 bytes) || Ed25519 signature (64 bytes)>
trusted comment: nymira-web-manifest v1 profile=<profile> version=<version>
<base64 of the 64-byte global signature>
```

- The algorithm is `ED` (prehashed). `Ed` (legacy) is refused. The Ed25519 signature is over the
  BLAKE2b-512 digest of the bytes of `manifest.json`.
- The global signature is Ed25519 over `signature (64 bytes) || trusted comment (the text after
"trusted comment: ", UTF-8, no newline)`. A verifier must check both signatures.
- `key_id` must equal the key id of the public key.
- The trusted comment is signed, so a verifier should also require `profile=` to match the profile
  it expects (`testnet` or `mainnet`) and `version=` to equal the manifest's `version`.

The public key file is the minisign format:

```
untrusted comment: <text>
<base64 of: "Ed" || key_id (8 bytes) || Ed25519 public key (32 bytes)>
```

**Fingerprint.** The SHA-256 of the 32-byte public key, lowercase hex. `node
web/scripts/integrity.mjs fingerprint <key.pub>` prints it with the key id. The fingerprint is what
is published outside the project's accounts (below).

The release key is generated offline with `minisign -G` and never enters this repository or CI. Only
`web/pins/release-signing.pub` (its public half) is committed. `nymirad` compiles the same public
key in. Tests use a throwaway key.

## URLs `verify-web` fetches

For each origin `O` in `https://nymira.pages.dev` and `https://nymiralabs.github.io/nymira` (a
trailing `/` is added; the mirror has a path prefix, so resolve paths against `O/`):

1. `O/manifest.json`, `O/manifest.json.minisig`: check the signature with the compiled-in key;
   check the canonical form and the trusted comment.
2. `O/integrity/files.txt`: its SHA-256 must be `bundle_hash`. `O/integrity/headers.txt`: its
   SHA-256 must be `header_set_hash`.
3. Every file in `files.txt` (`/` for `index.html`; the unknown-path request for `404.html`): its
   SHA-256 must be the one listed.
4. Headers (the Cloudflare origin only; the mirror cannot send them): the response headers of `/`,
   of `/wallet-host.html`, and of any file under `/assets/` must carry the six signed headers of
   the `app`, `frame` and `assets` classes. Check also `/wallet-host` for the frame class.
5. Do 1 to 4 once through Tor and once directly. Fail on a difference between the two, between
   the origins, or from the manifest. Both origins must show the same `manifest.json` bytes.

The listing makes the check independent of which files exist today: a file the host serves that is
not in `files.txt` is not looked for (the check cannot enumerate a host), but any listed file that
differs, and the listing itself, are.

## The test fixture

`web/test-fixtures/integrity/` is a small signed example for the Rust side's tests:

| Path                       | What it is                                                                                               |
| -------------------------- | -------------------------------------------------------------------------------------------------------- |
| `minisign.pub`             | The throwaway test public key.                                                                           |
| `site/`                    | A complete valid site: manifest, signature, `integrity/`, `_headers`.                                    |
| `expected.json`            | The key id and fingerprint, the trusted comment, the manifest, and the number of files.                  |
| `src/bundle/`, `src/meta/` | The inputs `regenerate.sh` builds the site from.                                                         |
| `regenerate.sh`            | Makes a new throwaway key and rebuilds `site/` (do this only to change the fixture; the key id changes). |

Serve `site/` from a local server for the fetch tests. Negative cases are made by changing one
thing in a copy: a byte of a bundle file; `files.txt`; `headers.txt`; `manifest.json`; the trusted
comment; the signature file removed; a signature by another key (`web/test/integrity.test.ts` does
each of these against the JavaScript implementation, `web/scripts/lib/integrity.mjs`, which is the
reference).

## Reproducible build

`web/scripts/repro-check.sh` builds the bundle twice from one commit (`git archive`) in two
differently named directories with separate cargo output, and fails when the bundle hashes differ.
The CI job `web-integrity / reproducible` runs it. What makes this work: exact versions and
lockfiles, `npm ci --ignore-scripts`, source paths remapped in the WASM build
(`--remap-path-prefix` in `build-wasm.sh`), `sourcemap: false`, and file names hashed from content by
Vite. Not covered by the CI job: a different CPU or operating system, which is what the independent
rebuild is for. Measured on one machine: the testnet profile built twice in two directories gave the same hash (the mainnet profile and the CI runner were not measured). The container recipe is the one the `web-deploy` workflow and the signed 0.1.x releases use.

## Rebuilding (independent rebuild)

On another machine, with Docker:

```sh
git clone https://github.com/nymiralabs/nymira && cd nymira
git checkout <release tag>
web/scripts/rebuild-in-container.sh <release tag> <testnet|mainnet> <version>
```

The container is `web/Dockerfile.repro`: `rust:1.99.0-bookworm` pinned by digest
(`sha256:114c7a44...04d6`), Node 24.22.1 pinned by tarball hash, `wasm-bindgen-cli` 0.2.117 with
`--locked`, `linux/amd64`. The script compares the rebuilt `manifest.json` with
`web/releases/<profile>-<version>/manifest.json` byte for byte. If they match, publish an
attestation from `web/releases/REBUILD_ATTESTATION_TEMPLATE.md` somewhere the project does not
control (the rebuilder's own repository, a gist, a signed post).

## Release procedure (human)

1. Make the profile final first: operator URL, pins, proving-key hashes. They change the bundle
   and, through `connect-src`, the header-set hash, so they cannot change after signing. Merge
   that commit (call it A).
2. CI builds A; the `reproducible` job is green. Note the hashes it prints.
3. Offline machine: check out A, run `web/scripts/rebuild-in-container.sh` (it downloads the
   ceremony's keys, or reads them from `NYMIRA_SETUP_DIR`), copy
   `rebuilt-site/manifest.json` out, then sign:
   `minisign -S -s <offline.key> -m manifest.json -t "nymira-web-manifest v1 profile=<p> version=<v>"`.
4. Commit `manifest.json` and `manifest.json.minisig` as `web/releases/<profile>-<version>/`, in a
   pull request, and merge it (commit B). The bundle hash does not cover `web/releases/`, so B
   builds to the same manifest as A. Tag B (`web-v<version>`). The deploy workflow refuses to run
   without the signature files, so the tag must sit on B, not A.
5. Run the `web-deploy` workflow with `ref` = the tag on B, `profile` and `version`. It rebuilds, compares the manifest with
   the committed one, verifies the signature with `web/pins/release-signing.pub`, deploys, and runs
   `check-live.mjs` against the live site. A dry run first: the same workflow on a throwaway
   `ref` stops at "unsigned build" and shows that the build step itself passes.
6. Publish the bundle hash and the key fingerprint (below). Fill in
   `web/releases/RELEASE_NOTES_TEMPLATE.md` for the release.

## Where the hashes are published

The key fingerprint and each manifest hash must also live where the project's GitHub, Cloudflare
and registrar accounts cannot change them. The accounts are the thing being defended against: a
thief with them can change the site, the repository, the README and the DNS, so a copy kept in any
of those proves nothing. Copies to make (each is a human task):

- the key fingerprint and every `bundle_hash` as an entry in a public, append-only log the project
  cannot edit (a transparency log such as Sigstore Rekor, with the entry signed by the release
  key: `rekor-cli upload` of the signed `manifest.json`);
- the fingerprint printed in a place the team controls offline (a card with the team), and read
  aloud or posted at the project's launch event;
- an independent rebuild attestation from someone outside the team for each release;
- the fingerprint in `nymirad` itself (the public key is compiled in), so a user who builds
  `nymirad` from source gets it from a second path.

If the fingerprint here and one of those copies differ, treat the site as compromised.

### Published copies

- **Sigstore Rekor, web 0.1.6 (testnet).** Log index 3813432556,
  <https://rekor.sigstore.dev/api/v1/log/entries/108e9186e8c5677a5572b0439d780076e32015bef6226efef82dbf8a6acfbcf97341271509ed451a>.
  Same form as the entry for 0.1.5: the SHA-256 of the four-line text below
  (`789213a6c46973062e8a286edcdc159e3ac93ca4d7b4bcdd7728f7391f691e12`, newline after each line) and a signature by a throwaway P-256 key that was deleted after the upload.

  ```
  nymira web release testnet 0.1.6
  bundle_hash 3c904a27230dfb6cfef005dd373d4890802c220318597e4901380df9e1e89891
  header_set_hash c4035ad9b05d62b3fb6f4cb8fe430249c8875b199a7975242742ca514a4f8cd2
  release_key RWRNeU3zahdMk2rmQPahPnpzK1uTMiD3Jv8PaJqgRpAfSQMfROKKxhkV (minisign key id 934C176AF34D794D)
  ```

- **Sigstore Rekor, web 0.1.5 (testnet).** Log index 3810960925,
  <https://rekor.sigstore.dev/api/v1/log/entries/108e9186e8c5677a85208074b750b2821b965fee123cfa1e960c86ffbc406304ba4552e4e3ec589c>.
  Same form as the entry for 0.1.4: the SHA-256 of the four-line text below
  (`a0cdd55261bc4417cb340d8c2d23fc362794fa3584db0b9dcfa162715d454e92`, newline after each line) and a signature by a throwaway P-256 key that was deleted after the upload.

  ```
  nymira web release testnet 0.1.5
  bundle_hash 6679d268035af9282926a172ccf1672d737c6bbeb2610bb0c896e57c780b8123
  header_set_hash c4035ad9b05d62b3fb6f4cb8fe430249c8875b199a7975242742ca514a4f8cd2
  release_key RWRNeU3zahdMk2rmQPahPnpzK1uTMiD3Jv8PaJqgRpAfSQMfROKKxhkV (minisign key id 934C176AF34D794D)
  ```

- **Sigstore Rekor, web 0.1.4 (testnet).** Log index 3808318068,
  <https://rekor.sigstore.dev/api/v1/log/entries/108e9186e8c5677a68a807622d8f7f810d58b92fe1e72d042bb6217638c333f5860fc374b57a7f72>.
  Same form as the entry for 0.1.3: the SHA-256 of the four-line text below
  (`0633282b6e34acf5206952dd41f24946bfa3971bb6fd39dc5460031fe10a46f2`, newline after each line) and a signature by a throwaway P-256 key that was deleted after the upload, so the entry proves that this text existed on that date and not who wrote it.

  ```
  nymira web release testnet 0.1.4
  bundle_hash 8657bb801d9d8f5d403bcefaf5910b4e099c02951771fc800f23c9ccda869a04
  header_set_hash 0a09ba64fb8a576c119a313c52b3ba78fe426454083cfae085cd43b0212e2eb8
  release_key RWRNeU3zahdMk2rmQPahPnpzK1uTMiD3Jv8PaJqgRpAfSQMfROKKxhkV (minisign key id 934C176AF34D794D)
  ```

- **Sigstore Rekor, web 0.1.3 (testnet).** Log index 3796098761,
  <https://rekor.sigstore.dev/api/v1/log/entries/108e9186e8c5677a125ababe52f96ceb325a74def3c9553a758d63f13f99968fadf10edefdb0a994>.
  Same form as the entry for 0.1.2: the SHA-256 of the four-line text below
  (`99761dcfbdb59955d75441f764a8e012b94e79ae240623ce05daad714ae48d87`, newline after each line) and a signature by a throwaway P-256 key that was deleted after the upload, so the entry proves that this text existed on that date and not who wrote it.

  ```
  nymira web release testnet 0.1.3
  bundle_hash 2ca35a389b672575187b14cce2bcb9fc1f174aa225190c3009031de76f2c840c
  header_set_hash 0a09ba64fb8a576c119a313c52b3ba78fe426454083cfae085cd43b0212e2eb8
  release_key RWRNeU3zahdMk2rmQPahPnpzK1uTMiD3Jv8PaJqgRpAfSQMfROKKxhkV (minisign key id 934C176AF34D794D)
  ```

- **Sigstore Rekor, web 0.1.2 (testnet).** Log index 3700443670,
  <https://rekor.sigstore.dev/api/v1/log/entries/108e9186e8c5677aef967761d17deed617441a7bb0254a666c6400d91c71e036de173f3e7d17186e>.
  The entry holds the SHA-256 of the four-line text below (`2aae510b406c3bcc77c2b09a5535f1afa5997c9531a7eaa317aba9739bb3468d`, newline after each line) and a signature by a throwaway
  P-256 key made for this entry and then deleted. It is not the release key: Rekor's `hashedrekord`
  type does not accept minisign's prehashed signatures. The entry therefore proves that this text
  existed on that date, not who wrote it. Check that the hash of the text below matches the entry's
  `data.hash`, and that the text matches the signed manifest and the key in `web/pins/release-signing.pub`.

  ```
  nymira web release testnet 0.1.2
  bundle_hash 3ee5ccb57cdaf2a1fa58cd914b6806909eac98c4dd20c013f6e0af5c926e045f
  header_set_hash 0a09ba64fb8a576c119a313c52b3ba78fe426454083cfae085cd43b0212e2eb8
  release_key RWRNeU3zahdMk2rmQPahPnpzK1uTMiD3Jv8PaJqgRpAfSQMfROKKxhkV (minisign key id 934C176AF34D794D)
  ```

## The `nymirad` release

`nymirad` is signed with the same release key as the web manifest (minisign key ID
`934C176AF34D794D`, shown in `pins/release-signing.pub`). `scripts/release-nymirad.sh` runs the steps;
only `sign` needs the key, and it only calls `minisign`, which asks for the passphrase itself.

1. Tag the commit `nymirad-vX.Y.Z` and push the tag.
2. `scripts/release-nymirad.sh build nymirad-vX.Y.Z`. It builds the tag twice in the container of
   `Dockerfile.repro` (same image digest, `--locked`, paths remapped, `SOURCE_DATE_EPOCH` set to the
   commit time), stops if the two binaries differ, runs `scripts/check-release-binary.sh` on the
   result (no `mock` feature, no demo strings), and writes `dist/nymirad-vX.Y.Z/` with the binary,
   `attestation-policy.json`, `TERMS.md` and `SHA256SUMS`.
3. Offline machine, in a clone that has `dist/`: `scripts/release-nymirad.sh sign nymirad-vX.Y.Z`
   writes `SHA256SUMS.minisig`.
4. `scripts/release-nymirad.sh publish nymirad-vX.Y.Z` verifies the signature with
   `pins/release-signing.pub`, checks the hashes, then creates the GitHub release with the Linux binary, the macOS binary if it was added, `attestation-policy.json`, `TERMS.md`, `SHA256SUMS` and `SHA256SUMS.minisig`.

A user checks a download with `minisign -V -p release-signing.pub -m SHA256SUMS` and
`shasum -a 256 -c SHA256SUMS`. Only the linux x86_64 binary is built this way. The macOS arm64
binary is built by the `nymirad-macos` workflow, added with `release-nymirad.sh add-macos` and signed again. It is not reproducible.

## Dependencies

`npm ci --ignore-scripts` is the only install (`.npmrc` sets `ignore-scripts=true`; CI passes the
flag too). `scripts/check-deps.mjs` fails on a locked package that is younger than 7 days, whose
lockfile hash is not the registry's, or whose version has no provenance attestation and no entry
in the exceptions file (`pins/dependency-exceptions.json`, `web-testkit/dependency-exceptions.json`,
`deploy/dependency-exceptions.json`, each entry with a reason). `npm audit signatures` checks the
registry signatures and the attestations that exist. Most small packages publish no attestation, so
the exceptions list is long; it is the record of what was accepted without one.

The deploy tool is pinned apart from the app (`web/deploy`, wrangler 4.153.0).

## What a static host cannot do

- GitHub Pages cannot set response headers. The mirror therefore serves the same bytes with none of
  the policy, and the app refuses to run there (its `appOrigin` is `nymira.pages.dev`; the page says
  so). The mirror exists so `verify-web` can compare two hosts, not as a second place to use the app.
- Cloudflare Pages `_headers` applies to static files only, joins a header set by two rules with a
  comma, and allows 100 rules. The generated file has no overlapping rules and the tests model the
  join.
- Cloudflare features that rewrite responses (Web Analytics beacon, Auto Minify, Rocket Loader,
  Email Obfuscation, Zaraz) must stay off for the project: they change the bytes or add script.
  The CSP would block an injected beacon, and `verify-web` would show the changed bytes.
