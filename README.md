# Ootle Pay

A decentralized, stake-gated third-party payment gateway, built natively on
[Tari Ootle](https://ootle.tari.com/) (Tari's Layer-2 smart contract network) — submitted to the
[September 2026 Ootle contest](https://community.tari.com/t/september-contest-thread/324).

Third-party merchants stake a bond to register (transparent, accountable, slashable — no KYC, no
centralized approval process). Once registered, they can sell one-off products or run subscription
plans, with real order counts and revenue tracked on-chain for every product and plan they run.
Payments themselves are non-custodial: money moves directly from a customer's account to a
merchant's revenue vault, which only the merchant can claim from. The stake itself is the one
exception — see the trust assumption called out under `merchant_registry` below.

**All three contracts are live on the Tari Ootle `esme` testnet**, wired together, and exercised
end to end with real transactions — see [Live deployment](#live-deployment) below. A small web app
in `app/` drives every flow against the real network.

## Why this shape

A stake-and-slash system needs to know *who* it's holding accountable, so this splits cleanly
across three components by responsibility, not by how much each one hides:

| Layer | Contract | Handles |
|---|---|---|
| Merchant accountability | `merchant_registry` | Stake-gated registration, tiering, slashing, dispute resolution |
| One-off products | `storefront` | Product listings, orders, per-product revenue, refunds |
| Subscriptions | `subscription` | Recurring plans, billing periods, per-plan revenue |

An earlier iteration of this project tried to also hide the *customer's* identity at the contract
level (never reading the caller's public key, relying on fresh signing keys per purchase). We
dropped that: Ootle's L2 can't offer a real cryptographic anonymity guarantee for "who signed this
call" the way Tari's L1 hides transfer amounts — a contract simply not reading a public key doesn't
stop anyone from reading it straight off the raw transaction. Rather than ship something that reads
as more private than it actually is, this is now a plainly transparent payment gateway. The one
genuine differentiator that remains, and the one this project actually leans on, is **non-custodial,
stake-based merchant accountability** instead of a centralized approval process.

## Architecture

```
                     stakes XTR, gets an API badge NFT
        merchant  ───────────────────────────────────▶  merchant_registry
                                                              │  is_registered() / get_tier()
                                    ┌─────────────────────────┴─────────────────────────┐
                                    ▼                                                     ▼
                               storefront                                          subscription
        customer ── buys a product ──▶ OrderPlaced{product_id, merchant, buyer, amount, order_number}   customer ── pays a plan's price ──▶
                                        revenue accrues in a per-product vault              SubscriptionRenewed{plan_id, subscriber, paid_until_period}
                                        merchant calls claim_revenue()                       revenue accrues per plan; merchant claims it
```

`storefront` and `subscription` both cross-call `merchant_registry.is_registered()` before letting
a merchant list a product or plan, so only staked, accountable merchants can sell through the
gateway.

### `contracts/merchant_registry`

Stake-gated registry. Merchants stake a bond (`min_stake` of a deployer-chosen resource), get
tiered (Basic / Standard / Premium, at 1×/5×/20× `min_stake`), and can be slashed into the
registry's treasury for misbehaviour found in off-chain/other-contract dispute resolution — or,
for a refund dispute specifically, have that same stake paid straight to the wronged buyer instead
(see `resolve_dispute` below and [Refunds and disputes](#refunds-and-disputes)).

**Trust assumption, stated plainly:** `request_exit` is self-service (any merchant can mark
themselves inactive at any time, and `cancel_exit_request` reverses it just as freely as long as
the owner hasn't acted on it yet), but resolving a pending exit is owner-only: `finalize_exit`
returns the merchant's stake in full, `reject_exit` instead confiscates the entire stake into the
treasury with a reason recorded in the `MerchantExitRejected` event — only whoever deployed this
registry component decides which. This is a deliberate choice, not an oversight — a merchant can't
unilaterally walk away from an active dispute with their bond in hand, and simply going inactive
doesn't let them keep collecting either: `is_registered` (which `storefront.buy` and
`subscription.subscribe`/`renew` both check before moving any money) returns `false` the moment
`request_exit` is called, not just after the exit is finalized. `slash` and `withdraw_treasury`
are likewise owner-only. This does mean the registry owner is a trusted party for stake custody
specifically, unlike the fully non-custodial `storefront`/`subscription` payment flows below,
where money never passes through anyone but the two parties to the transaction.

```rust
new(stake_resource: ResourceAddress, min_stake: Amount) -> Component<Self>
register(&mut self, stake: Bucket) -> Bucket                       // returns an API badge NFT
add_stake(&mut self, stake: Bucket)                                     // rejected once request_exit has been called
request_exit(&mut self)
cancel_exit_request(&mut self)                                          // reverses request_exit, before the owner acts on it
finalize_exit(&mut self, merchant: RistrettoPublicKeyBytes) -> Bucket   // owner-only; returns the full stake
reject_exit(&mut self, merchant: RistrettoPublicKeyBytes, reason: String)  // owner-only; confiscates the full stake instead
slash(&mut self, merchant: RistrettoPublicKeyBytes, amount: Amount)     // owner-only
resolve_dispute(&mut self, merchant: RistrettoPublicKeyBytes, amount: Amount, reason: String) -> Bucket  // owner-only; like slash, but pays the amount to the caller (routed to the buyer) instead of the treasury
dismiss_dispute(&mut self, merchant: RistrettoPublicKeyBytes, reason: String)  // owner-only; records a ruling for the merchant, no funds move
treasury_balance(&self) -> Amount
withdraw_treasury(&mut self) -> Bucket                                   // owner-only
is_registered(&self, merchant: RistrettoPublicKeyBytes) -> bool          // false once request_exit has been called, not just after finalize/reject
get_tier(&self, merchant: RistrettoPublicKeyBytes) -> String
get_merchant_info(&self, merchant: RistrettoPublicKeyBytes) -> (Amount, String, bool)
get_min_stake(&self) -> Amount                                          // so callers can show real tier thresholds instead of hardcoding them
```

### `contracts/storefront`

One-off product sales with ordinary, fully visible order records. A registered merchant lists a
product at a fixed price; anyone can buy it, as long as the merchant is still registered — `buy`
re-checks `merchant_registry.is_registered()` every time, so a merchant who calls `request_exit`
can no longer sell, even on products they listed while still registered. Every purchase becomes an
`OrderPlaced` event carrying the product, the buyer, the amount, and a running order number, so a
merchant can reconstruct their full order history and per-product revenue straight from the chain.

```rust
new(registry: ComponentAddress) -> Component<Self>
create_product(&mut self, merchant: RistrettoPublicKeyBytes, name: String, price: Amount, resource: ResourceAddress) -> u32
buy(&mut self, product_id: u32, payment: Bucket) -> u32   // rejected if the merchant is no longer registered; returns this order's number; emits OrderPlaced{product_id, merchant, buyer, amount, order_number}
get_product_info(&self, product_id: u32) -> (Amount /*price*/, u32 /*order_count*/, Amount /*revenue*/)
claim_revenue(&mut self, product_id: u32) -> Bucket   // caller must be the product's merchant
request_refund(&mut self, product_id: u32, order_id: u32, reason: String)   // caller must be the order's buyer
approve_refund(&mut self, product_id: u32, order_id: u32, amount: Amount) -> Bucket   // caller must be the product's merchant; with or without a prior request; partial refunds allowed
deny_refund(&mut self, product_id: u32, order_id: u32, reason: String)     // caller must be the product's merchant; requires a pending request
get_order_info(&self, product_id: u32, order_id: u32) -> (RistrettoPublicKeyBytes /*buyer*/, Amount /*price*/, Amount /*refunded*/, String /*status: None/Requested/Denied*/)
```

### Refunds and disputes

A buyer calls `request_refund` on their own order, recording a reason on-chain; the merchant then
either `approve_refund`s (full or partial, paid straight out of that product's revenue vault — a
merchant can also call this proactively, without a prior request) or `deny_refund`s, with their own
reason recorded too, so neither side of a disagreement is a black box.

If the merchant refuses a legitimate request — or the revenue's already been `claim_revenue`d out
of the vault, so there's nothing left in `storefront` to refund from — the buyer's recourse is the
merchant's stake: the registry owner reviews the on-chain trail (`RefundRequested`/`RefundDenied`
events) and calls `merchant_registry.resolve_dispute`, which slashes the merchant's stake and pays
it directly to the buyer instead of into the treasury, or `dismiss_dispute` if the merchant's
denial holds up, which records that ruling with no funds moving. This is what makes the stake a
real guarantee for buyers, not just a punitive fine on merchants.

### `contracts/subscription`

Recurring payments. A registered merchant creates a plan at a fixed price; a customer subscribes
under their own account and renews from the same account each billing period. Both `subscribe` and
`renew` re-check `merchant_registry.is_registered()` on every call, so a merchant who calls
`request_exit` can no longer collect on plans they created while still registered — neither new
subscriptions nor renewals of existing ones. There's no on-chain wall-clock, so billing periods
advance permissionlessly via `advance_period`, typically called by the merchant once per billing
cycle (e.g. a monthly cron).

```rust
new(registry: ComponentAddress) -> Component<Self>
create_plan(&mut self, merchant: RistrettoPublicKeyBytes, name: String, price: Amount, resource: ResourceAddress) -> u32
advance_period(&mut self, plan_id: u32)
subscribe(&mut self, plan_id: u32, payment: Bucket) -> Bucket   // rejected if the merchant is no longer registered; first time; subscriber = caller's account; returns a membership badge
renew(&mut self, plan_id: u32, payment: Bucket)                  // rejected if the merchant is no longer registered; subsequent periods; must be called by the same account that subscribed
is_active(&self, plan_id: u32, subscriber: RistrettoPublicKeyBytes) -> bool
claim_plan_revenue(&mut self, plan_id: u32) -> Bucket   // caller must be the plan's merchant
```

## Live deployment

Deployed and exercised for real on the Tari Ootle **esme** testnet (via a local
`tari_ootle_walletd --network esme`, connected to the hosted indexer at
`https://ootle-indexer-a.tari.com/` — no self-run validator network needed).

> **Status note:** redeployed for the refund/dispute feature - `merchant_registry` and
> `storefront` were republished (new template addresses, since their code changed) and all three
> components reinstantiated wired together, with `min_stake` lowered to 10 tTARI (from the
> previous deployment's 1000) so the esme testnet's one-time-per-account faucet grant is actually
> enough to register and test with. `subscription`'s template is unchanged, so its existing
> published template was reused for the new component instance - only a fresh `new(registry)` was
> needed, not a republish.

**Published templates:**

| Contract | Template address |
|---|---|
| `MerchantRegistry` | `template_76a37a3c397868728a24691a932c19c98bb28237c2fe81b9cf7f38e0d7fe0170` |
| `Storefront` | `template_5bc378695cfa03c7f11f2c1c30924fa88d1134cdb315f1e5e5df8259ca44fe9e` |
| `SubscriptionManager` | `template_6a03bc20ab8fd89dfd4c97c0fcc6947097412b6b56fbbb94a8804c92fea425ab` |

**Deployed component instances** (wired together — `storefront` and `subscription` both point at
the same registry):

| Contract | Component address |
|---|---|
| `merchant_registry` | `component_b45eb3d6a220962e1474e87d0e1273fc3b658fa4e35847ab994bf450d19c3dea` |
| `storefront` | `component_a6eb271f2ebcdb9af140cae127ea0196319d928908d7fecbafc983dc99ba6f79` |
| `subscription` | `component_767e0e57aa4a3d3506ea1ab8cd258d63600bf5896628ff076a6ada22d9421991` |

**Real transactions exercising the refund/dispute flow** (this deployment's smoke test — the
subscription component above is freshly instantiated but only unit-tested so far, not yet
exercised live since its code didn't change):

| Flow | Transaction id | Result |
|---|---|---|
| Register as merchant | `7d2a29c6c2525721283aa2dae7c36567483b8f5f6ce8967151e91b2090e23c73` | `MerchantRegistry.MerchantRegistered` — tier `Basic` |
| Create a product | `df1ee18cbd96d80f63d73f483d7d59296061ff66b56064ba61d4c2bd8f11fff8` | `Storefront.ProductCreated {merchant, name: "Smoke test widget", price: 20, product_id: 0}` |
| Buy that product | `5621e30f200e2137adc61c75a8e30b96b3551e72b4f22d2d52aa29418107dda2` | `Storefront.OrderPlaced {product_id: 0, merchant, buyer, amount: 20, order_number: 1}` |
| Request a refund | `bcc74baa72c8f5d325a5417122046f693a57e10a5233e4dba4b760a3b7761c22` | `Storefront.RefundRequested {product_id: 0, order_id: 1, buyer, reason: "smoke test - checking the wiring"}` |
| Approve the refund | `c4abd961a2a1eec5b9d73c9c383d71dbd8290c6b7ae4c57ca946698ebca53c5a` | `Storefront.OrderRefunded {product_id: 0, order_id: 1, buyer, amount: 20, total_refunded: 20}` — buyer's balance credited, `get_order_info(0, 1)` afterward read back `(buyer, 20, 20, "None")` |

These were submitted via the wallet daemon's `transactions.submit_manifest` JSON-RPC method, using
its Rust-like manifest DSL (parsed with `syn` - e.g. `registry.register(account.withdraw(XTR,
1000u64))`) rather than hand-building low-level instruction/argument JSON. That method turned out
to be a much better fit for calling custom contract methods with plain-typed arguments than the
TypeScript SDK's lower-level CBOR-literal builders, which are geared toward built-in
accounts/resources rather than arbitrary custom templates.

Order and revenue history for the web app's merchant dashboard comes from the public indexer's
`GET /transactions/events` / `/transactions/events/stream` endpoints (see
`https://ootle-indexer-a.tari.com/`), filtered by `template_address` and `topic` — this is a
network-wide, no-auth-required log of every event any template has ever emitted, so a merchant's
dashboard sees orders placed through *any* wallet, not just their own. It has no wall-clock
timestamp field, only a strictly-increasing event id, so exact totals are always accurate but
day/month bucketing in the app is necessarily based on when the dashboard itself has observed each
event, not a real historical timestamp — documented in the app's own UI, not silently assumed.

## The app

`app/` is a small, dependency-free static site (four pages, no build step) that drives all three
contracts against the deployed components above:

- **`index.html`** — overview and links to `merchant.html`/`pay.html` (not `admin.html`, which is
  kept off the public nav since it's an owner-only page). Its "Live on the esme testnet" proof
  section and "How it works" walkthrough are kept in sync with the current deployment and the
  refund/dispute flow, same as the "Live deployment" section below.
- **`merchant.html`** — the merchant console: register/stake (with the real Basic/Standard/Premium
  thresholds shown, read from `get_min_stake` rather than hardcoded), create products and
  subscription plans (names go into the `ProductCreated`/`PlanCreated` events, so anyone reading the
  chain sees them — nothing merchant-identifying is kept off-chain), request or cancel an exit
  (with your current status — active, or exit requested and pending owner review — always visible),
  a **Refund requests** card (approve — full or partial, into a buyer's account address you supply —
  or deny, with a reason, every pending `request_refund` against your products), and a dashboard of
  orders and revenue (today / this month / all-time) sourced from the public indexer's event log.
- **`pay.html`** — the customer-facing storefront. `pay.html?id=<merchant public key>` loads that
  merchant's shop directly (products and plans read straight from the chain, price shown read-only,
  nothing to type, merchant's stake and tier shown up front, a warning if they've requested exit);
  with no `id` it lists every active registered merchant (stake and tier included) to browse
  instead. Can buy/subscribe as any account in the connected wallet, not just its default one. A
  **"Your orders here"** section on each shop page lets the connected buyer request a refund on any
  of their orders (with a reason) and shows its live status — pending, denied (with the merchant's
  reason and a note to escalate to the platform admin), partially, or fully refunded.
- **`admin.html`** — owner-only registry dashboard: who's registered, their stake and tier, and the
  owner-gated actions. Merchants with a pending exit request get an Approve (`finalize_exit`, needs
  the merchant's account address to return their stake into) or Deny (`reject_exit`, confiscates it
  — a reason is required and recorded on-chain) choice, plus `slash`/`withdraw_treasury`. A
  **Disputes** card lists every refund the merchant denied, cross-checked live against
  `get_order_info` so one the merchant later approved anyway drops off; the owner can **Resolve**
  (`resolve_dispute` — slashes the merchant's stake and pays it straight to a buyer account you
  supply) or **Dismiss** (`dismiss_dispute` — records a ruling for the merchant, no funds move).
  Connecting a wallet that isn't the registry's owner still shows the same read-only stats (they're
  public — anyone querying the indexer sees the same thing) but the owner-gated actions will be
  rejected on-chain.

To run it:

```bash
# 1. Wallet daemon (holds your keys, talks to the public esme testnet)
./tari_ootle_walletd --network esme --authentication none --enable-vite-dev-port 5173
# Get free testnet funds either via the walletd web UI (http://127.0.0.1:5100) "Claim Testnet
# Funds" button, or by calling accounts.create_free_test_coins over JSON-RPC.

# 2. Serve the app (any static server works)
cd app && python3 -m http.server 8080
# open http://127.0.0.1:8080/index.html
```

`--authentication none` is appropriate only for a disposable testnet-only wallet (the daemon itself
logs a loud warning) - never use it for a wallet holding real value.

## Testing

All three contracts are covered by integration tests written against Tari Ootle's real
`tari_template_test_tooling` engine harness (the same infrastructure Tari's own template guides and
example templates use) — not mocks.

```
$ cargo +1.97 test -p tari_engine --test merchant_registry --test subscription --test storefront

running 13 tests (merchant_registry.rs)
test result: ok. 13 passed; 0 failed

running 15 tests (storefront.rs)
test result: ok. 15 passed; 0 failed

running 6 tests (subscription.rs)
test result: ok. 6 passed; 0 failed
```

Covered: registration + badge minting + tier calculation, stake-too-low rejection, tier upgrades on
added stake, `get_min_stake` reflects the deployer's chosen minimum, exit + stake return,
cancelling a pending exit request (and rejecting a cancel with no pending request), `reject_exit`
confiscating the full stake with the reason recorded on-chain (and rejecting a reject on a still-active
merchant), slashing, `resolve_dispute` paying the caller out of a merchant's stake and recalculating
their tier (and rejecting an award above what's staked), `dismiss_dispute` recording a ruling with
no funds moved; buying a product from an unregistered merchant rejected, wrong payment
amount/resource rejected, buying twice accumulates `order_count` and revenue and records each buyer,
`claim_revenue` restricted to the product's merchant and drains the vault, buying after the merchant
requests exit is rejected, a buyer requesting and the merchant approving a refund, denying a refund
request with a reason, a merchant approving a refund with no prior request, over-refund rejected,
only the buyer/merchant being allowed to request/approve/deny, denying with nothing pending rejected,
partial refunds accumulating up to the order's original price; wrong-amount subscription rejected,
activity flips correctly across `advance_period`, renewing from a different account than the one that subscribed is rejected, and
both subscribing and renewing after the merchant requests exit are rejected.

### Reproducing the tests yourself

These tests exercise `tari_template_test_tooling`, an internal dev-dependency of the `tari-ootle`
monorepo's own `tari_engine` crate (it isn't meant to be pulled in standalone). To reproduce:

```bash
git clone https://github.com/tari-project/tari-ootle.git
cd tari-ootle
git checkout 9e1a82a7a6438ddea687ec9af1ec565642e0f168   # commit these contracts were built/tested against

# copy this repo's contracts and tests into the monorepo's test harness
cp -r <this-repo>/contracts/merchant_registry crates/engine/tests/templates/
cp -r <this-repo>/contracts/storefront        crates/engine/tests/templates/
cp -r <this-repo>/contracts/subscription      crates/engine/tests/templates/
cp <this-repo>/tests/merchant_registry.rs crates/engine/tests/
cp <this-repo>/tests/storefront.rs       crates/engine/tests/
cp <this-repo>/tests/subscription.rs      crates/engine/tests/

# needs a rustc new enough for the monorepo's own deps (nightly toolchains lag behind)
rustup install 1.97
rustup target add --toolchain 1.97 wasm32-unknown-unknown
cargo +1.97 test -p tari_engine --test merchant_registry --test subscription --test storefront
```

Each contract also builds standalone against a pinned git revision of `tari_template_lib`
(see each `contracts/*/Cargo.toml`) — `cargo +1.97 build --target wasm32-unknown-unknown` inside
any `contracts/<name>` directory produces a deployable WASM template (this is exactly how the
templates above were built and published with `tari publish`).

## Known limitations

- No on-chain wall-clock exists, so `subscription`'s billing periods are a counter the merchant
  advances themselves, and the app's "today / this month" revenue bucketing is based on when the
  dashboard observed each event rather than a real historical timestamp (all-time totals are always
  exact, since they come from summing the full on-chain event log, not from local observation).
- `slash()` deposits confiscated stake into a treasury rather than burning it, since a
  deployer-supplied `stake_resource` (e.g. the network's native token) generally isn't created with
  a `burnable` rule this registry controls.
- Refunds and disputes exist only for `storefront`. `subscription` has no `request_refund`/
  `approve_refund`/`deny_refund` equivalent and emits no `RefundRequested`/`RefundDenied` events, so
  a bad subscription charge has no path back — not even through `admin.html`'s Disputes card, which
  only reads storefront events. `merchant_registry.resolve_dispute`/`dismiss_dispute` are generic
  enough to arbitrate a subscription dispute too, but nothing in the app surfaces one.
- The link between a `resolve_dispute`/`dismiss_dispute` ruling and the storefront order it's about
  is an off-chain convention, not something the contract enforces: `admin.html` requires the
  arbiter's `reason` string to start with a literal `[order <product_id>/<order_id>]` tag so it can
  match a ruling back to the dispute it resolved (the registry method itself takes no order
  reference). A ruling made without that tag — e.g. a direct manifest call bypassing the UI — won't
  be recognized as resolved, and the dispute will keep reappearing in `admin.html`'s list.
- The deployment above used `--authentication none` and a broad "Admin"-permission session token,
  appropriate only for this disposable testnet demo wallet.

## License

MIT
