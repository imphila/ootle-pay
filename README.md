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
| Merchant accountability | `merchant_registry` | Stake-gated registration, tiering, slashing |
| One-off products | `storefront` | Product listings, orders, per-product revenue |
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
        customer ── buys a product ──▶ OrderPlaced{product, buyer, amount}   customer ── pays a plan's price ──▶
                                        revenue accrues in a per-product vault              SubscriptionRenewed{plan, subscriber, period}
                                        merchant calls claim_revenue()                       revenue accrues per plan; merchant claims it
```

`storefront` and `subscription` both cross-call `merchant_registry.is_registered()` before letting
a merchant list a product or plan, so only staked, accountable merchants can sell through the
gateway.

### `contracts/merchant_registry`

Stake-gated registry. Merchants stake a bond (`min_stake` of a deployer-chosen resource), get
tiered (Basic / Standard / Premium, at 1×/5×/20× `min_stake`), and can be slashed into the
registry's treasury for misbehaviour found in off-chain/other-contract dispute resolution.

**Trust assumption, stated plainly:** `request_exit` is self-service (any merchant can mark
themselves inactive at any time), but `finalize_exit` — the call that actually returns a
merchant's stake — along with `slash` and `withdraw_treasury`, is owner-only: only whoever
deployed this registry component can release a merchant's stake back to them or confiscate it.
This is a deliberate choice, not an oversight — a merchant can't unilaterally walk away from an
active dispute with their bond in hand — but it does mean the registry owner is a trusted party
for stake custody specifically, unlike the fully non-custodial `storefront`/`subscription`
payment flows below, where money never passes through anyone but the two parties to the
transaction.

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

### `contracts/storefront`

One-off product sales with ordinary, fully visible order records. A registered merchant lists a
product at a fixed price; anyone can buy it; every purchase becomes an `OrderPlaced` event carrying
the product, the buyer, the amount, and a running order number, so a merchant can reconstruct their
full order history and per-product revenue straight from the chain.

```rust
new(registry: ComponentAddress) -> Component<Self>
create_product(&mut self, merchant: RistrettoPublicKeyBytes, price: Amount, resource: ResourceAddress) -> u32
buy(&mut self, product_id: u32, payment: Bucket) -> u32   // returns this order's number; emits OrderPlaced{product_id, merchant, buyer, amount, order_number}
get_product_info(&self, product_id: u32) -> (Amount /*price*/, u32 /*order_count*/, Amount /*revenue*/)
claim_revenue(&mut self, product_id: u32) -> Bucket   // caller must be the product's merchant
```

### `contracts/subscription`

Recurring payments. A registered merchant creates a plan at a fixed price; a customer subscribes
under their own account and renews from the same account each billing period. There's no on-chain
wall-clock, so billing periods advance permissionlessly via `advance_period`, typically called by
the merchant once per billing cycle (e.g. a monthly cron).

```rust
new(registry: ComponentAddress) -> Component<Self>
create_plan(&mut self, merchant: RistrettoPublicKeyBytes, price: Amount, resource: ResourceAddress) -> u32
advance_period(&mut self, plan_id: u32)
subscribe(&mut self, plan_id: u32, payment: Bucket) -> Bucket   // first time; subscriber = caller's account; returns a membership badge
renew(&mut self, plan_id: u32, payment: Bucket)                  // subsequent periods; must be called by the same account that subscribed
is_active(&self, plan_id: u32, subscriber: RistrettoPublicKeyBytes) -> bool
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
| `Storefront` | `template_6efdd10551556df7b46c2c1bbe837029f1c8fe3a086c660294335524f27bcda6` |
| `SubscriptionManager` | `template_17de369eabf37fa5db8a17b4cf5cbc99677a330aa338ce69f52c98dd54c76e95` |

**Deployed component instances** (wired together — `storefront` and `subscription` both point at
the same registry):

| Contract | Component address |
|---|---|
| `merchant_registry` | `component_1d32aca29e61e70736d16f3e2603dffb6eb7859cfda60ab09977da73711aa7ed` |
| `storefront` | `component_19ca26614a5e0641430c38fbafc9096274b2baac096814e8e7b376f7229b540f` |
| `subscription` | `component_61cbdff66b9eed52c66208a008b2e8453577c1d65243f71adff5ab3797185a84` |

**Real transactions exercising every flow:**

| Flow | Transaction id | Result |
|---|---|---|
| Register as merchant | `603c05e40db845d7c4fbd3867f9df56911ee8d4bcc09c5ef89ca17840f633bf8` | `MerchantRegistry.MerchantRegistered` — tier `Basic` |
| Create a product | `3512865ec9e8edbc1b288e61e3d04e59e6ce993b99e82ea2116fae71a8b83932` | `Storefront.ProductCreated {merchant, price: 20, product_id: 0}` |
| Buy that product | `d338ff791ea791fa07329cd22f8541f545b4f09bbc08570945f1d5286784dd28` | `Storefront.OrderPlaced {product_id: 0, merchant, buyer, amount: 20, order_number: 1}` |
| Claim product revenue | `332c464928be00d7fcb6dd7e0f81efa86efb445e424a99611fa68dcc983a168f` | Vault drained — `get_product_info(0)` afterward read back `(20, 1, 0)` |
| Create a subscription plan | `9648b413184cbc05a300be3ac933d8499a96e36a7107d34b7f290bda4c97deaf` | `SubscriptionManager.PlanCreated {merchant, plan_id: 0, price: 50}` |
| Subscribe | `efd248981a2c58400f890f8454523a0c1158d110867a7529997ffebe2510d457` | `SubscriptionRenewed {plan_id: 0, subscriber, paid_until_period: 1}` |
| Advance the billing period | `0079c84dc374644e3c3f7e81af55b4195b02ae6dbd39af0ef35b95270cf02ab2` | Accepted |
| Renew | `be2b7a99d1b0e3d1d386de7987b501d49d41235984fd34857b80c4ab091e5c12` | `SubscriptionRenewed {..., paid_until_period: 2}` — `is_active` read back `true` afterward |

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

`app/` is a small, dependency-free static site (three pages, no build step) that drives all three
contracts against the deployed components above:

- **`index.html`** — overview and links to the other two pages.
- **`merchant.html`** — the merchant console: register/stake, create products and subscription
  plans, and a dashboard of orders and revenue (today / this month / all-time) sourced from the
  public indexer's event log.
- **`pay.html`** — the customer-facing page: buy a product or subscribe to a plan.

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

running 5 tests (merchant_registry.rs)
test result: ok. 5 passed; 0 failed

running 6 tests (storefront.rs)
test result: ok. 6 passed; 0 failed

running 4 tests (subscription.rs)
test result: ok. 4 passed; 0 failed
```

Covered: registration + badge minting + tier calculation, stake-too-low rejection, tier upgrades on
added stake, exit + stake return, slashing; buying a product from an unregistered merchant rejected,
wrong payment amount/resource rejected, buying twice accumulates `order_count` and revenue and
records each buyer, `claim_revenue` restricted to the product's merchant and drains the vault;
wrong-amount subscription rejected, activity flips correctly across `advance_period`, and renewing
from a different account than the one that subscribed is rejected.

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
- The deployment above used `--authentication none` and a broad "Admin"-permission session token,
  appropriate only for this disposable testnet demo wallet.

## License

MIT
