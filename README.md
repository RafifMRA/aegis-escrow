# Escrow Vault — Stellar Soroban

An arbitrated escrow smart contract on the Stellar network (Soroban). Funds are locked between a **payer** and a **payee**, released either by the payer or a neutral **arbiter**, or refunded to the payer by the arbiter if the deal falls through. Includes a full test suite, CI, and a zero-build static web UI for interacting with a deployed instance.

- **Live demo:** _TODO — paste your Vercel/Netlify URL here_
- **Contract address (Testnet):** _TODO — paste the `C...` contract ID here after deployment_
- **Example transaction hash:** _TODO — paste a real testnet transaction hash here (e.g. a `release` call)_
- **Demo video (1–2 min):** _TODO — paste the video link here_

---

## Table of contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Contract API](#contract-api)
- [Project structure](#project-structure)
- [Local development & tests](#local-development--tests)
- [Deploying the contract](#deploying-the-contract)
- [Running the frontend](#running-the-frontend)
- [Deploying the frontend](#deploying-the-frontend)
- [CI/CD](#cicd)
- [Screenshots](#screenshots)
- [Security notes](#security-notes)
- [License](#license)

## Overview

One deployed instance of this contract represents a single escrow deal. It moves through the following states:

```
            initialize()              deposit()
  (none) ─────────────────► Pending ──(funded=false→true)──► Pending, funded
                                │                                   │
                     arbiter.refund()                    payer|arbiter.release()
                                ▼                                   ▼
                           Refunded                            Completed
```

`Pending` covers both "awaiting deposit" and "funded, awaiting resolution" (disambiguated by an internal `funded` flag). `Completed` and `Refunded` are terminal — every mutating call rejects a non-`Pending` escrow, which is what prevents double-release and double-refund.

**Authorization model.** Soroban has no implicit caller identity, so every privileged action authenticates an explicit `Address` via `require_auth()`:

| Action | Who | Enforcement |
|---|---|---|
| `initialize` | `payer` | `payer.require_auth()` |
| `deposit` | stored `payer` | `escrow.payer.require_auth()` |
| `release` | stored `payer` **or** `arbiter` | caller-supplied address authenticates, then checked against both roles |
| `refund` | stored `arbiter` | `escrow.arbiter.require_auth()` |

## Architecture

- **Language/runtime:** Rust, compiled to `wasm32-unknown-unknown`, running on the Soroban host environment.
- **State storage:** a single `EscrowData` struct (roles, token, amount, `funded` flag, `status`) under one instance-storage key. Every mutating call extends the instance's TTL (`extend_ttl`) so the contract's state doesn't expire and get archived while an escrow is active.
- **Token transfers:** delegated to the token contract itself via `soroban_sdk::token::Client` (works with any SEP-41 token, including the native XLM Stellar Asset Contract).
- **Errors:** typed `EscrowError` codes via `panic_with_error!`, so failures are precise (`AlreadyInitialized`, `InvalidAmount`, `InvalidParties`, `AlreadyFunded`, `NotFunded`, `NotPending`, `Unauthorized`, `NotInitialized`) instead of opaque panics.
- **Frontend:** a dependency-free static site (`web/`) — no bundler, no `node_modules`. It loads [`@stellar/stellar-sdk`](https://www.npmjs.com/package/@stellar/stellar-sdk) from a CDN as an ES module and talks to the [Freighter](https://www.freighter.app/) wallet extension (injected as `window.freighterApi`) for signing. This keeps the whole app deployable as static files with zero build configuration.

## Contract API

All functions are defined in [`src/lib.rs`](src/lib.rs).

### `initialize(payer, payee, arbiter, token, amount)`
Sets up a new escrow. Callable once per contract instance. Requires `payer`'s authorization. Does **not** move funds — call `deposit` afterward. Rejects a non-positive `amount` and rejects `payer`/`payee`/`arbiter` that aren't all distinct.

### `deposit()`
Pulls `amount` of `token` from the stored `payer` into the vault. Requires `payer`'s authorization. Fails if already funded or not `Pending`.

### `release(caller: Address)`
Transfers the locked funds to `payee`. `caller` must authenticate **and** be either the `payer` or the `arbiter`. Fails if not funded or not `Pending`.

### `refund()`
Returns the locked funds to `payer`. Requires the `arbiter`'s authorization. Fails if not funded or not `Pending`.

### `get_status() -> Status`
Read-only. Returns `Pending`, `Completed`, or `Refunded`.

### `get_escrow() -> EscrowData`
Read-only. Returns the full record: `payer`, `payee`, `arbiter`, `token`, `amount`, `funded`, `status`.

## Project structure

```
escrow-vault/
├── Cargo.toml                 # soroban-sdk dependency + release profile
├── src/
│   ├── lib.rs                 # contract implementation
│   └── test.rs                # 21-test exhaustive suite
├── web/
│   ├── index.html             # static UI markup
│   ├── styles.css             # mobile-first responsive styling
│   └── app.js                 # wallet connection + contract calls
├── .github/workflows/ci.yml   # GitHub Actions: test + wasm build
├── LICENSE
└── README.md
```

## Local development & tests

Requires the Rust toolchain plus the `wasm32-unknown-unknown` target:

```bash
rustup target add wasm32-unknown-unknown
```

Run the full test suite (21 tests covering both successful lifecycles, every authorization boundary, and every double-spend/out-of-order edge case):

```bash
cargo test
```

Build the deployable release WASM:

```bash
cargo build --target wasm32-unknown-unknown --release
```

The compiled contract will be at `target/wasm32-unknown-unknown/release/escrow_vault.wasm`.

> **Note on the `ed25519-dalek` pin:** `Cargo.toml` pins `ed25519-dalek = "=2.2.0"` under `[dev-dependencies]`. Without it, `cargo test` can resolve two incompatible major versions of `ed25519-dalek` into the dependency graph (2.2.0 and 3.0.0), which use different, incompatible `rand_core` majors and fail with an `E0277` trait-bound error inside `soroban-env-host`'s test PRNG helper. This only affects `testutils`/`cargo test` — it has no effect on the actual deployed contract.

## Deploying the contract

Install the Stellar CLI (successor to `soroban-cli`) if you don't have it:

```bash
cargo install --locked stellar-cli --features opt
```

Create and fund a Testnet identity:

```bash
stellar keys generate deployer --network testnet --fund
```

Deploy the built WASM:

```bash
stellar contract deploy \
  --wasm target/wasm32-unknown-unknown/release/escrow_vault.wasm \
  --source deployer \
  --network testnet
```

This prints the deployed **contract address** (`C...`) — copy it into this README's summary section and into the frontend's "Contract ID" field.

Initialize an escrow (example — replace addresses/amount/token as needed; `--source deployer` acts as the payer):

```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source deployer \
  --network testnet \
  -- initialize \
  --payer <PAYER_G_ADDRESS> \
  --payee <PAYEE_G_ADDRESS> \
  --arbiter <ARBITER_G_ADDRESS> \
  --token <TOKEN_CONTRACT_ID> \
  --amount 10000000
```

Then `deposit`, `release`, or `refund` the same way:

```bash
stellar contract invoke --id <CONTRACT_ID> --source deployer --network testnet -- deposit
stellar contract invoke --id <CONTRACT_ID> --source deployer --network testnet -- release --caller <CALLER_G_ADDRESS>
```

Each `invoke` prints a transaction hash — that's what goes in this README's **example transaction hash** field, and what you'll show as your on-chain interaction.

## Running the frontend

The frontend is dependency-free static files — no `npm install`, no build step. Serve it with any static file server (it must be served over HTTP(S), not opened directly as a `file://` URL, since browsers block ES module `fetch` from `file://` origins):

```bash
cd web
npx serve .
```

Open the printed local URL, connect the [Freighter](https://www.freighter.app/) wallet extension, paste your deployed contract address into "Contract Settings," and use the Initialize / Deposit / Release / Refund actions.

## Deploying the frontend

Push this repo to GitHub, then import it on [Vercel](https://vercel.com/new) or [Netlify](https://app.netlify.com/start) as a static site with:

- **Root/base directory:** `web`
- **Build command:** _(none)_
- **Output/publish directory:** `.`

Both platforms auto-deploy on every push to `main` once connected.

## CI/CD

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every push/PR to `main`: it installs the Rust toolchain with the `wasm32-unknown-unknown` target, runs `cargo test`, builds the release WASM, and uploads it as a build artifact. See the [Actions tab](../../actions) for run history.

## Screenshots

| Mobile responsive UI | CI/CD pipeline | Test output |
|---|---|---|
| _TODO: screenshot_ | _TODO: screenshot_ | _TODO: screenshot_ |

## Security notes

- This contract has **not** been professionally audited. Use at your own risk, especially on Mainnet.
- The arbiter is fully trusted for refunds and is one of two parties who can trigger a release — choose that address carefully in production use.
- All amounts are in the token's base units (e.g. stroops for XLM), not display units.

## License

[MIT](LICENSE)
