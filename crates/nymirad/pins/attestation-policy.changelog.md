# Attestation policy changelog

The policy (`attestation-policy.json`) says which NEAR AI deployments `nymirad` trusts. It is signed with the key in `policy-signer.pub`, and a build refuses a policy older than its `MIN_POLICY_VERSION`.

## Version 1 (2026-11-02)

First policy. Testnet.

- Accepts one model deployment and one gateway deployment: the Intel TDX measurements (`mr_td`, `rtmr0`, `rtmr1`, `rtmr2`), the OS image hash and the compose hash.
- The values were read from NEAR AI's live attestation endpoints on 2 November 2026 and signed as they were. They are not compared with a reference NEAR AI publishes (none is known), so the policy trusts what the gateway showed that day: trust on first use.
- TCB status must be `UpToDate`; GPU evidence is required.
- The signing key is a testnet key. A mainnet release gets its own key.

When NEAR AI deploys a new image or compose file, the new values are not in the policy and chats are refused until a new version is signed and shipped.
