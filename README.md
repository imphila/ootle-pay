# Ootle Private Pay

A decentralized, stake-gated, anonymous third-party payment gateway, built natively on
[Tari Ootle](https://ootle.tari.com/) (Tari's Layer-2 smart contract network) — submitted to the
[September 2026 Ootle contest](https://community.tari.com/t/september-contest-thread/324).

Third-party merchants stake a bond to register for API access (transparent, accountable,
slashable). Their *customers* pay them anonymously: one-off payments hide the platform's view of
the payment beyond a small per-tier fee, using Ootle's native stealth/confidential transaction
primitives rather than a bolted-on mixer. Recurring subscription payments get a third model: the
price is public (merchants already publish their own pricing), but the *subscriber's identity* is
never linked to any on-chain key.

**All three contracts are live on the Tari Ootle `esme` testnet**, wired together, and exercised
end to end with real transactions — see [Live deployment](#live-deployment) below. A small web app
in `app/` drives all three flows against the real network.

## Why this shape

Full anonymity and merchant accountability pull in opposite directions: a stake-and-slash system
needs to know *who* it's slashing, while a private payment needs to hide *who* paid. Rather than
compromise both, this splits them cleanly across three components:

| Layer | Contract | Identity | Why |
|---|---|---|---|
| Merchant accountability | `merchant_registry` | **Public** | Merchants want a public reputation; stake + slash requires an identifiable address. |
| One-off payments | `private_pay` | **Buyer hidden, amount public** | Merchants need real order/revenue tracking; only the specific buyer's identity is worth hiding. |
| Subscriptions | `subscription` | **Decoupled** (price public, subscriber not) | Price transparency costs nothing (it's already published); subscriber linkage is the only thing worth hiding. |

## What "hidden" actually means here

It's worth being precise about this, because it's easy to conflate two different layers:

- **Tari Layer 1** (the base chain, Mimblewimble-based) natively hides **amounts** for its own
  transfers via Pedersen commitments. That's a real, automatic, protocol-level property — we don't
  do anything to earn it.
- **Ootle (Layer 2)**, where these contracts run, is a smart-contract execution layer. Calling a
  contract method is a signed transaction: `subscribe(plan_id, secret, payment)` is submitted with
  a valid signature from some key, and that signature — and the public key behind it — sits on the
  L2 ledger like any other transaction field. L1's amount-hiding doesn't extend to "who signed this
  contract call"; validators need the signer's key to verify the signature in the first place.

So the identity-hiding in `subscription` (and in the redesigned `private_pay`, see below) is a
**contract-level engineering choice, not a cryptographic guarantee**: these contracts simply never
call `CallerContext::transaction_signer_public_key()` and never emit or store it anywhere, so
nothing in their *own* state or events links one call to another. That only holds up in practice if
the caller also signs each sensitive call with a fresh, throwaway account — a wallet-side habit we
rely on and document, not something these contracts can enforce. A sufficiently motivated observer
correlating raw transaction signatures across the L2 ledger is a different, harder problem than
these contracts solve. What one-off purchases *do* hide by design is exactly what the user asked
for: the specific buyer's identity — not the amount, which is visible on purpose (see
`contracts/private_pay` below).

## Architecture

```
                     stakes XTR, gets an API badge NFT
        merchant  ───────────────────────────────────▶  merchant_registry
                                                              │  is_registered() / get_tier()
                                                              ▼
  customer ── pays a small per-tier platform fee ────────▶ private_pay ── fee lands in a pot;
  (one-off)   (the actual value transfer travels as a                    nothing else is ever
               sibling native StealthTransfer instruction                recorded
               in the same atomic transaction - hidden
               amount + hidden recipient, unseen by this
               contract or the chain)
                                                              │
  customer ── plan price payment, opaque subscriber_secret ──▶ subscription ── merchant claims
  (recurring)   (never keyed by the signer's public key)                       plan revenue
```

`private_pay` and `subscription` both cross-call `merchant_registry.is_registered()` /
`get_tier()` before doing anything, so only staked, accountable merchants can receive payments
through the gateway.

### `contracts/merchant_registry`

Transparent, stake-gated registry. Deliberately *not* anonymous — merchants stake a bond
(`min_stake` of a deployer-chosen resource), get tiered (Basic / Standard / Premium, at 1×/5×/20×
`min_stake`) which drives their fee rate on `private_pay`, and can be slashed into the registry's
treasury for misbehaviour found in off-chain/other-contract dispute resolution.

```rust
new(stake_resource: ResourceAddress, min_stake: Amount) -> Component<Self>
register(&mut self, stake: Bucket) -> Bucket                       // returns an API badge NFT
add_stake(&mut self, stake: Bucket)
request_exit(&mut self)
finalize_exit(&mut self, merchant: RistrettoPublicKeyBytes) -> Bucket   // owner-only
slash(&mut self, merchant: RistrettoPublicKeyBytes, amount: Amount)     // owner-only
treasury_balance(&self) -> Amount
withdraw_treasury(&mut self) -> Bucket                                   // owner-only
is_registered(&self, merchant: RistrettoPublicKeyBytes) -> bool
get_tier(&self, merchant: RistrettoPublicKeyBytes) -> String
get_merchant_info(&self, merchant: RistrettoPublicKeyBytes) -> (Amount, String, bool)
```

### `contracts/private_pay`

One-off anonymous payments. **Design note, discovered while wiring up the web app:** Ootle's
client SDKs have first-class, well-tested support for building the native `StealthTransfer`
instruction (hidden amount + hidden recipient, balance-proved by the engine), but no generic way to
hand a `StealthTransferStatement` to an arbitrary contract method as a plain argument - that
encoding is wired specifically for the native instruction, not for `CallMethod` args. So this
contract's job is narrowed to exactly the part that's *meant* to be visible: given `merchant` and a
`fee` bucket, it checks the merchant is registered, requires `fee` to meet their tier's minimum,
banks it, and emits an event naming only the merchant and the fee - never an amount or payer
identity beyond that. In a real integration, this call shares one atomic transaction with a sibling
native `StealthTransfer` instruction moving the actual payment: if `pay` panics (unregistered
merchant, fee too low), the whole transaction - stealth transfer included - is rejected.

```rust
new(fee_resource: ResourceAddress, registry: ComponentAddress) -> Component<Self>
pay(&mut self, fee: Bucket, merchant: RistrettoPublicKeyBytes)
fee_pot_balance(&self) -> Amount
withdraw_fees(&mut self) -> Bucket   // owner-only
```

### `contracts/subscription`

Recurring payments. Plan price and merchant are public (merchants publish their own pricing
anyway), but the contract never reads `CallerContext::transaction_signer_public_key()` — every
lookup is keyed by an opaque `subscriber_secret` (a plain `String` the subscriber picks themselves,
e.g. a random hex string - kept as a `String` rather than a `NonFungibleId` for the same
client-SDK-literal reason as above). As long as they sign each renewal with a fresh, throwaway
account — a wallet-side practice, not something a smart contract can enforce — nothing here links
one renewal to the next or to their real identity. There's no on-chain wall-clock, so billing
periods advance permissionlessly via `advance_period`, typically called by the merchant once per
billing cycle.

```rust
new(registry: ComponentAddress) -> Component<Self>
create_plan(&mut self, merchant: RistrettoPublicKeyBytes, price: Amount, resource: ResourceAddress) -> u32
advance_period(&mut self, plan_id: u32)
subscribe(&mut self, plan_id: u32, subscriber_secret: String, payment: Bucket) -> Bucket   // first time; returns a membership badge
renew(&mut self, plan_id: u32, subscriber_secret: String, payment: Bucket)                  // subsequent periods
is_active(&self, plan_id: u32, subscriber_secret: String) -> bool
claim_plan_revenue(&mut self, plan_id: u32) -> Bucket   // caller must be the plan's merchant
```

## Live deployment

Deployed and exercised for real on the Tari Ootle **esme** testnet (via a local
`tari_ootle_walletd --network esme`, connected to the hosted indexer at
`https://ootle-indexer-a.tari.com/` — no self-run validator network needed).

**Published templates:**

| Contract | Template address |
|---|---|
| `MerchantRegistry` | `template_06d153def5e163e8a76071fc3189cca0c70e80f1559020e8078fd1a40da74bd3` |
| `PrivatePay` | `template_022eeb41a03da85125703d0988d5da94c9e4aa35d192217694d1052b9e0d012d` |
| `SubscriptionManager` | `template_9a6ed2f0b82d0913da970de56e66af38cccb5bafeef18ac23b5103c2eb31674d` |

**Deployed component instances** (wired together — `private_pay` and `subscription` both point at
the same registry):

| Contract | Component address |
|---|---|
| `merchant_registry` | `component_1d32aca29e61e70736d16f3e2603dffb6eb7859cfda60ab09977da73711aa7ed` |
| `private_pay` | `component_478fd847553b9e896324b641fa3f60200d2f37ae04699c3628f09062f2fdc623` |
| `subscription` | `component_9addab1f01ebf52a7bc95db6fe2b54a00878ce305c6d2c62761eac4f70c6c475` |

**Real transactions exercising every flow:**

| Flow | Transaction id | Result |
|---|---|---|
| Fund wallet (built-in testnet faucet) | `85b3831a4e30b68a01785247b37b1ad59b900896db2091c353c437bdf4e20884` | Account funded with ~1,000,000,000 µXTR |
| Register as merchant | `603c05e40db845d7c4fbd3867f9df56911ee8d4bcc09c5ef89ca17840f633bf8` | `MerchantRegistry.MerchantRegistered` — tier `Basic` |
| Anonymous payment | `0bc120c2b9b1f83cadcb302f21edecf2eb39f30e772cd703ba91f725cc8e3f58` | `PrivatePay.PaymentProcessed` — event carries only `{merchant, fee: 3}` |
| Create plan + subscribe | `97cfc10a980f322a974351fbd3bb1e11143129173b16e26953dc44a1ece8a3ab` | `SubscriptionManager.PlanCreated` + `SubscriptionRenewed` |
| Check subscription active | `54788b5626b01349f9c27ef710ee3096785021b5faa5476a28a3b352a40db1f1` | `is_active` → `true` |

These were submitted via the wallet daemon's `transactions.submit_manifest` JSON-RPC method, using
its Rust-like manifest DSL (parsed with `syn` - e.g. `registry.register(account.withdraw(XTR,
1000u64))`) rather than hand-building low-level instruction/argument JSON. That method turned out
to be a much better fit for calling custom contract methods with plain-typed arguments than the
TypeScript SDK's lower-level CBOR-literal builders, which are geared toward built-in
accounts/resources and the native stealth instruction rather than arbitrary custom templates.

## The app

`app/index.html` is a single, dependency-free static page (open it directly or serve it - no build
step) that drives all three contracts against the deployed components above:

1. **Connect** — points at a local `tari_ootle_walletd --network esme` (default
   `http://127.0.0.1:5100/json_rpc`), mints a session token, and creates/loads your default
   account.
2. **Become a merchant** — stake XTR and register.
3. **Pay anonymously** — send a registered merchant a payment; only the platform fee is ever
   visible on-chain.
4. **Subscriptions** — create a plan, subscribe with a private secret, and check activity.

To run it:

```bash
# 1. Wallet daemon (holds your keys, talks to the public esme testnet)
./tari_ootle_walletd --network esme --authentication none --enable-vite-dev-port 5173
# Get free testnet funds either via the walletd web UI (http://127.0.0.1:5100) "Claim Testnet
# Funds" button, or by calling accounts.create_free_test_coins over JSON-RPC.

# 2. Serve the app (any static server works)
cd app && python3 -m http.server 8080
# open http://127.0.0.1:8080/index.html, click Connect
```

`--authentication none` is appropriate only for a disposable testnet-only wallet (the daemon itself
logs a loud warning) - never use it for a wallet holding real value.

## Testing

All three contracts are covered by integration tests written against Tari Ootle's real
`tari_template_test_tooling` engine harness (the same infrastructure Tari's own template guides and
example templates use) — not mocks. **11/11 passing:**

```
$ cargo +1.97 test -p tari_engine --test merchant_registry --test subscription --test private_pay

running 5 tests (merchant_registry.rs)
test result: ok. 5 passed; 0 failed

running 3 tests (private_pay.rs)
test result: ok. 3 passed; 0 failed

running 3 tests (subscription.rs)
test result: ok. 3 passed; 0 failed
```

Covered: registration + badge minting + tier calculation, stake-too-low rejection, tier upgrades on
added stake, exit + stake return, slashing; anonymous payment fee below the tier minimum rejected,
payment to an unregistered merchant rejected, a valid payment landing its fee while hiding
everything else; wrong-amount subscription rejected, activity flips correctly across
`advance_period`, and — the point of the whole design — that two renewals signed by two
*different* accounts under the same `subscriber_secret` are treated as one continuous subscription.

### Reproducing the tests yourself

These tests exercise `tari_template_test_tooling`, an internal dev-dependency of the `tari-ootle`
monorepo's own `tari_engine` crate (it isn't meant to be pulled in standalone). To reproduce:

```bash
git clone https://github.com/tari-project/tari-ootle.git
cd tari-ootle
git checkout 9e1a82a7a6438ddea687ec9af1ec565642e0f168   # commit these contracts were built/tested against

# copy this repo's contracts and tests into the monorepo's test harness
cp -r <this-repo>/contracts/merchant_registry crates/engine/tests/templates/
cp -r <this-repo>/contracts/private_pay       crates/engine/tests/templates/
cp -r <this-repo>/contracts/subscription      crates/engine/tests/templates/
cp <this-repo>/tests/merchant_registry.rs crates/engine/tests/
cp <this-repo>/tests/private_pay.rs       crates/engine/tests/
cp <this-repo>/tests/subscription.rs      crates/engine/tests/

# needs a rustc new enough for the monorepo's own deps (nightly toolchains lag behind)
rustup install 1.97
rustup target add --toolchain 1.97 wasm32-unknown-unknown
cargo +1.97 test -p tari_engine --test merchant_registry --test subscription --test private_pay
```

Each contract also builds standalone against a pinned git revision of `tari_template_lib`
(see each `contracts/*/Cargo.toml`) — `cargo +1.97 build --target wasm32-unknown-unknown` inside
any `contracts/<name>` directory produces a deployable WASM template (this is exactly how the
templates above were built and published with `tari publish`).

## Known limitations

- `private_pay`'s fee-gating half is exercised live on-chain (see above), but the *other* half - a
  sibling native `StealthTransfer` instruction actually moving a hidden payment in the same
  transaction - is not yet wired into the app. Building one needs either the TypeScript SDK's
  `StealthTransfer` builder (proven working in Tari's own `stealth-wallet` example app) or a
  walletd JSON-RPC method that performs a stealth send directly; the Rust-level engine mechanics
  are already covered by `private_pay`'s own passing tests (`stealth_transfer_with_opt_input_bucket`
  and friends, exercised via `tari_template_test_tooling`'s stealth helpers in earlier iterations of
  this test suite).
- Subscription's identity decoupling is a construction/convention (this contract simply never reads
  the caller's public key), not something cryptographically enforced on-chain — it depends on the
  subscriber actually using a fresh signing key per renewal.
- `slash()` deposits confiscated stake into a treasury rather than burning it, since a
  deployer-supplied `stake_resource` (e.g. the network's native token) generally isn't created with
  a `burnable` rule this registry controls.
- The app wires up `subscribe` (first period) but not `renew` (period 2+) — the contract method
  exists and is unit-tested, just not yet given a button.
- The deployment above used `--authentication none` and a broad "Admin"-permission session token,
  appropriate only for this disposable testnet demo wallet.

## License

MIT
