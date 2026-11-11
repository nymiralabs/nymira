# Attestation policy changelog

The policy (`attestation-policy.json`) says which NEAR AI deployments `nymirad` trusts. It is signed with the key in `policy-signer.pub`, and a build refuses a policy older than its `MIN_POLICY_VERSION`.

## Version 1 (2026-11-02)

First policy. Testnet.

- Accepts one model deployment and one gateway deployment: the Intel TDX measurements (`mr_td`, `rtmr0`, `rtmr1`, `rtmr2`), the OS image hash and the compose hash.
- The values were read from NEAR AI's live attestation endpoints on 2 November 2026 and signed as they were. They are not compared with a reference NEAR AI publishes (none is known), so the policy trusts what the gateway showed that day: trust on first use.
- TCB status must be `UpToDate`; GPU evidence is required.
- The signing key is a testnet key. A mainnet release gets its own key.

When NEAR AI deploys a new image or compose file, the new values are not in the policy and chats are refused until a new version is signed and shipped.

## Version 2

- The model entry lists a second compose hash, `a0e3c6f4...`, seen on 2 November 2026, 36 minutes after the first (`5f7ebd85...`), with the same signing key, OS image and Intel TDX measurements. Version 1 would have refused the deployment that produced the recorded `provider_tee` signature.
- Everything else is as in version 1. Clients refuse a policy under version 2.
- A test checks every recorded model report against this file, so a shipped policy that rejects evidence we already hold fails the build.


## Version 3

- The gateway entry lists a second compose hash, `c579aade...`, seen in NEAR AI's live gateway attestation on 4 November 2026. The OS image hash and the Intel TDX measurements (`mr_td`, `rtmr0`, `rtmr1`, `rtmr2`) are the same as for the first gateway hash (`f571e5da...`). The model entry is unchanged.
- Without it every chat is refused with `provenance.verification_failed`, because the gateway now runs this compose file.
- As in version 1, the hash is read from the live endpoint and not compared with a reference NEAR AI publishes (none is known): trust on first use.
- Clients refuse a policy under version 5 (`MIN_POLICY_VERSION`), so this version is no longer accepted.


## Version 4

- The policy pins the two NVIDIA intermediate certificates that sign the GPU attestation keys, so a client checks that a signing key's certificate chain reaches a pinned certificate.
- The pins are marked unverified: no NVIDIA documentation page lists their fingerprints.

## Version 5

- The gateway entry accepts any compose hash. The OS image hash and the Intel TDX measurements stay pinned, so a change of the gateway's compose file no longer refuses every chat.
- `MIN_POLICY_VERSION` is 5: clients refuse an older policy.

## Version 6

- The model entry lists a third compose hash, `e8416a02d6f7…`, seen in NEAR AI's live model attestation on 11 November 2026. The OS image hash and the Intel TDX measurements are the same as for the two earlier model hashes. Without it every chat on that deployment was refused with `deployment_rejected`.
- As before, the hash was read from the live endpoint and not compared with a reference NEAR AI publishes (none is known): trust on first use.
- Everything else is as in version 5. `MIN_POLICY_VERSION` stays 5.

## Version 7

- Lists the deployments seen in NEAR AI's live attestation on 11 November 2026 and not listed before: `e8416a02d6f7…` (model). The values were read from the live endpoint and not compared with a reference NEAR AI publishes (none is known): trust on first use.
- Everything else is as in version 6.
