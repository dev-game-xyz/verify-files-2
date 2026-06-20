# Aladdin Marketplace — Solana Program (Verified Build Source)

Public source for the on-chain Anchor program:

- **Program ID:** `9HRyqHTN65Rz5EhX1PGRzaVKAcNNMx3NFd62yfLxiUvp`
- **Cluster:** mainnet-beta
- **Framework:** Anchor `0.31.1`
- **Crate / lib name:** `aladdin_marketplace`

This repository contains exactly the source that produces the deployed bytecode,
published so the build can be reproduced and verified against the on-chain
program via [solana-verifiable-build](https://github.com/solana-foundation/solana-verifiable-build).

## What this program does

An owner-gated SPL-token item marketplace plus a permanent username registry:

| Instruction | Caller | Purpose |
|---|---|---|
| `initialize` | deployer | Create the `Marketplace` registry root; set the first `owner`. |
| `add_allowed_item` | owner | Register an SPL mint as sellable. |
| `sell` | owner | Escrow one token unit into the listing's PDA vault. |
| `cancel_listing` | seller | Seller reclaims their own escrowed token; closes the listing. |
| `buy` | owner | Release the escrowed token to the buyer + write a permanent `Trade`. |
| `change_owner` | owner | Update the privileged authority. |
| `claim_username` | player | Claim a permanent, globally-unique username (one per wallet). |
| `set_username_fee` | owner | Update the username claim fee (lamports). |

All uniqueness (listing ids, allowed mints, usernames, one-name-per-wallet) is
enforced structurally by PDA seeds. Token moves are always authorized by the
listing/marketplace PDA — never by a passed-in key.

## Reproducible build & verification

Requires [Docker](https://docs.docker.com/get-docker/), Rust, and the
[`solana-verify`](https://github.com/solana-foundation/solana-verifiable-build) CLI
(`cargo install solana-verify --locked`).

> The `Cargo.lock` here pins dependencies to versions compatible with the
> Solana 2.3.0 SBF build toolchain (Cargo 1.84), and `Cargo.toml` pins
> `[workspace.metadata.cli] solana = "2.3.0"` so the correct build image is
> selected deterministically.

```bash
# 1. Deterministic build inside the pinned Docker image
solana-verify build

# 2. Hash of the locally-built artifact
solana-verify get-executable-hash target/deploy/aladdin_marketplace.so

# 3. UPGRADE the existing program in place (same program ID, state preserved).
#    Signed by the program's upgrade authority. Costs a temporary buffer in SOL.
solana program deploy \
  --program-id 9HRyqHTN65Rz5EhX1PGRzaVKAcNNMx3NFd62yfLxiUvp \
  --upgrade-authority <UPGRADE_AUTHORITY_KEYPAIR> \
  --url mainnet-beta \
  target/deploy/aladdin_marketplace.so

# 4. Confirm the on-chain hash now equals the local build hash
solana-verify get-program-hash 9HRyqHTN65Rz5EhX1PGRzaVKAcNNMx3NFd62yfLxiUvp \
  --url https://api.mainnet-beta.solana.com

# 5. Verify against THIS repo and upload the verify PDA (signed by upgrade authority)
solana-verify verify-from-repo \
  --program-id 9HRyqHTN65Rz5EhX1PGRzaVKAcNNMx3NFd62yfLxiUvp \
  -u https://api.mainnet-beta.solana.com \
  https://github.com/dev-game-xyz/verify-files-2

# 6. Trigger the OtterSec remote job (gets the public Solscan "verified" badge)
solana-verify remote submit-job \
  --program-id 9HRyqHTN65Rz5EhX1PGRzaVKAcNNMx3NFd62yfLxiUvp \
  --uploader <UPGRADE_AUTHORITY_PUBKEY>
```

## Layout

```
.
├── Anchor.toml
├── Cargo.toml                # workspace + release profile (overflow-checks, lto)
├── Cargo.lock                # pinned deps — required for a reproducible build
└── programs/
    └── aladdin_marketplace/
        ├── Cargo.toml
        ├── Xargo.toml
        └── src/
            ├── lib.rs        # instructions + account contexts
            ├── state.rs      # account layouts
            └── errors.rs     # custom errors
```

## License

MIT
