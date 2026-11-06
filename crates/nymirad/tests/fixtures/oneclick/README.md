# Recorded 1Click quotes

`dry.json` and `real.json` were recorded from the keyless `POST /v0/quote` endpoint on 4 November 2026. Both requests use placeholder commitment and payout values, `nymira.near` as recipient and `0x1111111111111111111111111111111111111111` as refund address. The real quote allocated a deposit address; it was never funded.

The Rust tests verify both responses against the compiled manager key using the [SDK's signed field selection](https://github.com/defuse-protocol/one-click-sdk-typescript/blob/main/src/quote-signature.ts). They verify issuance, not delivery.
