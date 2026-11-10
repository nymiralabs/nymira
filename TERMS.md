# Terms of use (testnet)

[README](README.md) · [Website](https://nymira.pages.dev) · [Documentation](https://nymira.pages.dev/#docs) · [Try it in 20 minutes](JUDGES.md) · [Threat model](THREAT_MODEL.md) · [Releases](https://github.com/nymiralabs/nymira/releases)

Last updated: 4 November 2026.

**This is a test.** Nymira runs on NEAR testnet with test funds. The USDC is Circle's testnet token and has no value. Nothing here moves real money. Do not use real personal data. Do not send anything you need to keep private to the model: the circuit has had no independent review and the anonymity set is small.

**Who may use it.** Do not use Nymira if you are on a sanctions list of the United Nations, the European Union, the United Kingdom or the United States, or if you are in, or a resident of, a country or region under comprehensive sanctions. The team can refuse or stop service to anyone.

**Providers' rules.** Your prompts go to NEAR AI's inference service. Its [Cloud Terms of Service](https://near.ai/terms-of-service), which include its acceptable use policy as it stands at the time you use the service, apply to what you send. Do not send anything those rules forbid.

**No promise of service.** The operator runs on a laptop. It can be offline, change or stop at any time. You can leave by calling `request_exit` and, once the 7-day window has passed, `finalize_exit`, as the README explains. A note expires at the end of the UTC day 30 days after the deposit. The treasury can take it 7 days after the later of the expiry and the last exit activity, and never while an exit is pending. The team can update the vault code. Open deposits are capped at 100 USDC.

**What the operator sees.** See [THREAT_MODEL.md](THREAT_MODEL.md). In short: the operator sees the timing, sizes and model name of your requests and, if you use the web app, your IP address reaches the operator's entry relay and the operator's machine in a request header that the operator software does not read or store. Use Tor Browser or `nymirad` to hide your IP address.

**Your keys, your backup.** If you lose your latest state, an old-state exit can be defeated and you can lose the remaining balance. Keep your encrypted backup.

**If this ever runs on mainnet.** Circle issues USDC and can freeze or block funds at its discretion. A mainnet launch would come only after an independent audit, under separate terms.

**No warranty.** The software is provided as it is, without any warranty. The team is not liable for losses from its use, to the extent the law allows.

**Contact.** Questions and takedown requests: nymiralabs@proton.me.
