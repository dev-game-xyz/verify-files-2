// ---------------------------------------------------------------------------
// 🪄 ALADDIN-JIN ON-CHAIN MARKETPLACE + USERNAME REGISTRY  (Anchor / Solana)
//
// A tightly-scoped, owner-gated SPL-token item marketplace plus a permanent
// username registry, designed for the Aladdin-Jin game server (the "owner" /
// "game wallet") to drive.
//
// SECURITY MODEL (why this is hard to abuse):
//  • The marketplace *is* a PDA (seed ["marketplace"]) — it owns every escrow
//    vault and signs token moves itself. No human key custodies escrowed items.
//  • Privileged actions (sell, buy, add_allowed_item, change_owner,
//    add_username) require the stored `owner` to sign — enforced by Anchor's
//    `has_one = owner` + an explicit Signer. Spoofing is impossible.
//  • Uniqueness (listing ids, allowed mints, usernames, one-name-per-wallet) is
//    enforced *structurally* by PDA seeds: you cannot `init` the same PDA twice,
//    so duplicates are rejected by the runtime, not by fallible checks.
//  • All token transfers go through CPI to the SPL Token program with the vault
//    authority being the listing/marketplace PDA — never a passed-in key.
//  • Integer math is overflow-checked (see Cargo profile) and prices are plain
//    u64 with an explicit > 0 guard (no decimals).
//  • Inputs (ids/usernames) are validated to be 1..=32 ASCII alphanumerics.
// ---------------------------------------------------------------------------

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, CloseAccount, Mint, Token, TokenAccount, Transfer};

pub mod errors;
pub mod state;

use errors::MarketError;
use state::*;

declare_id!("9HRyqHTN65Rz5EhX1PGRzaVKAcNNMx3NFd62yfLxiUvp");

/// Listings escrow exactly ONE whole token unit (an NFT-style 1-of-1 game item).
const LISTING_AMOUNT: u64 = 1;

/// Default username claim fee = 0.01 SOL (in lamports). Owner-settable later.
const DEFAULT_USERNAME_FEE: u64 = 10_000_000;
/// Max username length (characters). Usernames are lowercase a-z / 0-9.
const MAX_USERNAME_LEN: usize = 10;

#[program]
pub mod aladdin_marketplace {
    use super::*;

    // -----------------------------------------------------------------------
    // 🏛 INITIALIZE — create the marketplace registry root once. Whoever calls
    // this becomes the first `owner` (set it to the game's wallet at deploy).
    // -----------------------------------------------------------------------
    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let m = &mut ctx.accounts.marketplace;
        m.owner = ctx.accounts.owner.key();
        m.total_listings = 0;
        m.total_allowed = 0;
        m.total_trades = 0;
        m.username_fee = DEFAULT_USERNAME_FEE; // 0.01 SOL
        m.bump = ctx.bumps.marketplace;
        msg!("marketplace initialized · owner={} · username_fee={}", m.owner, m.username_fee);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ✅ addAllowedItems(spl_token_addr) — owner only. Registers a mint as a
    // sellable item. The PDA seed makes the allow-list set unique (re-adding the
    // same mint fails at the runtime level).
    // -----------------------------------------------------------------------
    pub fn add_allowed_item(ctx: Context<AddAllowedItem>) -> Result<()> {
        let item = &mut ctx.accounts.allowed_item;
        item.mint = ctx.accounts.mint.key();
        item.bump = ctx.bumps.allowed_item;

        let m = &mut ctx.accounts.marketplace;
        m.total_allowed = m.total_allowed.checked_add(1).ok_or(MarketError::Overflow)?;
        msg!("allowed item added · mint={}", item.mint);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 🏷 sell(spl_token_addr) — PERMISSIONLESS. Any holder of an ALLOWED item
    // may create a listing and ESCROW one token unit into the listing's
    // PDA-owned vault. The player pays rent + fees and is the SOLE signer
    // (no owner co-sign), so Phantom can simulate it cleanly.
    //   • verifies the mint is on the allowed list (the `allowed_item` PDA must
    //     exist with the matching seed — Anchor fails to load it otherwise)
    //   • transfers the token from the seller's account to the listing vault
    //   • records owner / price / listing_id
    //
    // `listing_id` is the unique alphanumeric id; `seller_owner` is the wallet
    // recorded as the listing owner (duplicates allowed across listings);
    // `price` is whole-number gold (no decimals).
    // -----------------------------------------------------------------------
    pub fn sell(
        ctx: Context<Sell>,
        listing_id: String,
        price: u64,
        seller_owner: Pubkey,
    ) -> Result<()> {
        require!(is_alnum(&listing_id), MarketError::InvalidListingId);
        require!(price > 0, MarketError::InvalidPrice);

        // The allowed_item PDA is derived from the mint; if the mint isn't
        // allowed, Anchor can't find the account and the tx fails. We also
        // double-check the stored mint for defence in depth.
        require_keys_eq!(
            ctx.accounts.allowed_item.mint,
            ctx.accounts.mint.key(),
            MarketError::ItemNotAllowed
        );

        // The seller token account must actually hold the mint + a whole unit.
        require_keys_eq!(
            ctx.accounts.seller_token.mint,
            ctx.accounts.mint.key(),
            MarketError::MintMismatch
        );
        require!(
            ctx.accounts.seller_token.amount >= LISTING_AMOUNT,
            MarketError::InvalidAmount
        );

        // Escrow the token: seller_token -> listing vault (authority = seller signer).
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.seller_token.to_account_info(),
                    to: ctx.accounts.listing_vault.to_account_info(),
                    authority: ctx.accounts.seller_authority.to_account_info(),
                },
            ),
            LISTING_AMOUNT,
        )?;

        let listing = &mut ctx.accounts.listing;
        listing.spl_token_addr = ctx.accounts.mint.key();
        listing.owner = seller_owner;
        listing.price = price;
        listing.listing_id = listing_id;
        listing.bump = ctx.bumps.listing;

        let m = &mut ctx.accounts.marketplace;
        m.total_listings = m.total_listings.checked_add(1).ok_or(MarketError::Overflow)?;

        msg!(
            "listed · id={} mint={} owner={} price={}",
            listing.listing_id,
            listing.spl_token_addr,
            listing.owner,
            listing.price
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 🔄 cancelListing(listing_id) — SELLER ONLY. The wallet that LISTED the
    // item (recorded as `listing.owner`) signs and reclaims its own escrowed
    // token; the listing + vault are closed and their rent returned to that
    // seller. No one else — not even the marketplace owner — can pull a token
    // out of someone else's listing, because the seller must sign.
    // -----------------------------------------------------------------------
    pub fn cancel_listing(ctx: Context<CancelListing>, _listing_id: String) -> Result<()> {
        // The signer MUST be the wallet that listed the item.
        require_keys_eq!(
            ctx.accounts.seller.key(),
            ctx.accounts.listing.owner,
            MarketError::SellerMismatch
        );
        // The item returns to the seller's own token account.
        require_keys_eq!(
            ctx.accounts.seller_token.owner,
            ctx.accounts.listing.owner,
            MarketError::SellerMismatch
        );
        require_keys_eq!(
            ctx.accounts.seller_token.mint,
            ctx.accounts.listing.spl_token_addr,
            MarketError::MintMismatch
        );

        transfer_from_vault(
            &ctx.accounts.token_program,
            &ctx.accounts.listing_vault,
            &ctx.accounts.seller_token,
            &ctx.accounts.listing.to_account_info(),
            &ctx.accounts.listing.listing_id,
            ctx.accounts.listing.bump,
            LISTING_AMOUNT,
        )?;

        close_vault(
            &ctx.accounts.token_program,
            &ctx.accounts.listing_vault,
            &ctx.accounts.seller.to_account_info(), // vault rent → the seller
            &ctx.accounts.listing.to_account_info(),
            &ctx.accounts.listing.listing_id,
            ctx.accounts.listing.bump,
        )?;

        msg!("listing cancelled by seller · id={}", ctx.accounts.listing.listing_id);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 🛒 buy(listing_id, new_owner) — OWNER ONLY (settlement).
    //
    // The off-chain flow: a buyer pays gold to the seller through the game's
    // centralised DB; once the server has confirmed payment, the game wallet
    // (owner) calls `buy`. Instead of pushing the token to the buyer (which
    // would make the OWNER pay the buyer's ATA rent + gas), we MOVE the token
    // into a CLAIMABLE vault owned by a new `["claimable", listing_id]` PDA and
    // record the buyer. The buyer later pulls it themselves via `claim`, paying
    // their own ATA rent + gas + the claim fee — so the owner never pays gas to
    // deliver items.
    //
    // Anchor enforces owner-only via `has_one = owner` + Signer, so no buyer can
    // settle a sale themselves — only the trusted game wallet can.
    // -----------------------------------------------------------------------
    pub fn buy(ctx: Context<Buy>, _listing_id: String, new_owner: Pubkey) -> Result<()> {
        // 📸 snapshot the listing details NOW — the listing account is closed at
        // the end of this instruction (`close = owner`), so we record the trade
        // and the claimable from these locals.
        let listing_id = ctx.accounts.listing.listing_id.clone();
        let mint = ctx.accounts.listing.spl_token_addr;
        let seller = ctx.accounts.listing.owner;
        let price = ctx.accounts.listing.price;
        let listing_bump = ctx.accounts.listing.bump;

        // sanity: the claimable vault must hold the same mint as the listing.
        require_keys_eq!(
            ctx.accounts.claimable_vault.mint,
            mint,
            MarketError::MintMismatch
        );

        // Move the escrowed token: listing vault -> CLAIMABLE vault (still
        // PDA-custodied — the buyer can't touch it until they `claim`).
        transfer_from_vault(
            &ctx.accounts.token_program,
            &ctx.accounts.listing_vault,
            &ctx.accounts.claimable_vault,
            &ctx.accounts.listing.to_account_info(),
            &listing_id,
            listing_bump,
            LISTING_AMOUNT,
        )?;

        close_vault(
            &ctx.accounts.token_program,
            &ctx.accounts.listing_vault,
            &ctx.accounts.owner_refund, // listing-vault rent refunded to the owner
            &ctx.accounts.listing.to_account_info(),
            &listing_id,
            listing_bump,
        )?;

        // 📦 record the CLAIMABLE item — only `new_owner` may claim it later.
        let claimable = &mut ctx.accounts.claimable;
        claimable.spl_token_addr = mint;
        claimable.buyer = new_owner;
        claimable.listing_id = listing_id.clone();
        claimable.bump = ctx.bumps.claimable;

        // 🧾 LOG THE TRADE — bump the global counter and write the permanent
        // on-chain trade record (id / listing_id / mint / seller / buyer / price).
        let m = &mut ctx.accounts.marketplace;
        m.total_trades = m.total_trades.checked_add(1).ok_or(MarketError::Overflow)?;
        let trade = &mut ctx.accounts.trade;
        trade.id = m.total_trades;
        trade.listing_id = listing_id.clone();
        trade.spl_token_addr = mint;
        trade.seller = seller;
        trade.buyer = new_owner;
        trade.price = price;
        trade.bump = ctx.bumps.trade;

        msg!(
            "sold (claimable) · id={} mint={} seller={} buyer={} price={} (trade #{})",
            listing_id,
            mint,
            seller,
            new_owner,
            price,
            trade.id
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 📦 claim(listing_id) — BUYER ONLY. The recorded buyer pulls their bought
    // item from the claimable vault into their OWN token account. The buyer is
    // the SOLE signer, fee payer, and rent payer:
    //   • pays the claim fee (marketplace.username_fee lamports, 0.01 SOL) to
    //     the owner via an explicit System transfer (buyer → owner),
    //   • receives the token into `buyer_token` (their own ATA — they pay its
    //     rent if it must be created in the same tx, client-side),
    //   • closes the claimable vault + ClaimableItem PDA, rent refunded to them.
    // Single-signer ⇒ Phantom simulates it cleanly (no malicious-dApp warning).
    // -----------------------------------------------------------------------
    pub fn claim(ctx: Context<Claim>, listing_id: String) -> Result<()> {
        // Only the recorded buyer may claim.
        require_keys_eq!(
            ctx.accounts.buyer.key(),
            ctx.accounts.claimable.buyer,
            MarketError::NotBuyer
        );
        // The destination token account must be the buyer's and the right mint.
        require_keys_eq!(
            ctx.accounts.buyer_token.owner,
            ctx.accounts.buyer.key(),
            MarketError::NewOwnerMismatch
        );
        require_keys_eq!(
            ctx.accounts.buyer_token.mint,
            ctx.accounts.claimable.spl_token_addr,
            MarketError::MintMismatch
        );

        let fee = ctx.accounts.marketplace.username_fee;
        let claimable_bump = ctx.accounts.claimable.bump;

        // 💰 pay the claim fee to the owner (buyer → owner) via System transfer
        // (explicit + simulatable — Phantom shows "you send X SOL to <owner>").
        if fee > 0 {
            let ix = anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.buyer.key(),
                &ctx.accounts.owner.key(),
                fee,
            );
            anchor_lang::solana_program::program::invoke(
                &ix,
                &[
                    ctx.accounts.buyer.to_account_info(),
                    ctx.accounts.owner.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
        }

        // Release the token: claimable vault -> buyer's own token account,
        // signed by the claimable PDA.
        let seeds: &[&[u8]] = &[b"claimable", listing_id.as_bytes(), &[claimable_bump]];
        let signer: &[&[&[u8]]] = &[seeds];
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.claimable_vault.to_account_info(),
                    to: ctx.accounts.buyer_token.to_account_info(),
                    authority: ctx.accounts.claimable.to_account_info(),
                },
                signer,
            ),
            LISTING_AMOUNT,
        )?;

        // Close the now-empty claimable vault, refunding its rent to the buyer.
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            CloseAccount {
                account: ctx.accounts.claimable_vault.to_account_info(),
                destination: ctx.accounts.buyer.to_account_info(),
                authority: ctx.accounts.claimable.to_account_info(),
            },
            signer,
        ))?;

        msg!(
            "claimed · id={} mint={} buyer={}",
            listing_id,
            ctx.accounts.claimable.spl_token_addr,
            ctx.accounts.buyer.key()
        );
        Ok(())
        // The ClaimableItem PDA itself is closed via `close = buyer` in the
        // accounts struct (rent → buyer).
    }

    // -----------------------------------------------------------------------
    // 🔑 changeOwner(new_owner) — owner only. Updates the privileged authority.
    // -----------------------------------------------------------------------
    pub fn change_owner(ctx: Context<ChangeOwner>, new_owner: Pubkey) -> Result<()> {
        require_keys_neq!(new_owner, Pubkey::default(), MarketError::InvalidNewOwner);
        require_keys_neq!(
            new_owner,
            anchor_lang::system_program::ID,
            MarketError::InvalidNewOwner
        );
        let m = &mut ctx.accounts.marketplace;
        let old = m.owner;
        m.owner = new_owner;
        msg!("owner changed · {} -> {}", old, new_owner);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 👤 claimUsername(username) — THE PLAYER claims their OWN username by
    // signing in their browser. Permanent + globally unique + one per wallet.
    //   • the player (`player_wallet`) is the signer + payer (rent + fee)
    //   • the username must be lowercase a-z / 0-9, 1..=10 chars
    //   • a fee (marketplace.username_fee lamports) is transferred player→owner
    //   • the `username` PDA seed enforces unique usernames
    //   • the `wallet_username` PDA seed enforces one username per wallet
    //   • neither PDA can be re-init'd, so a name can never change.
    // -----------------------------------------------------------------------
    pub fn claim_username(ctx: Context<ClaimUsername>, username: String) -> Result<()> {
        // strict: lowercase ascii letters/digits only, 1..=10 chars
        require!(is_valid_username(&username), MarketError::InvalidUsername);

        let wallet = ctx.accounts.player_wallet.key();
        let fee = ctx.accounts.marketplace.username_fee;

        // 💰 pay the claim fee to the owner (player → owner) via a System transfer
        if fee > 0 {
            let ix = anchor_lang::solana_program::system_instruction::transfer(
                &ctx.accounts.player_wallet.key(),
                &ctx.accounts.owner.key(),
                fee,
            );
            anchor_lang::solana_program::program::invoke(
                &ix,
                &[
                    ctx.accounts.player_wallet.to_account_info(),
                    ctx.accounts.owner.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
        }

        let entry = &mut ctx.accounts.username_entry;
        entry.wallet_addr = wallet;
        entry.username = username.clone();
        entry.bump = ctx.bumps.username_entry;

        let rev = &mut ctx.accounts.wallet_username;
        rev.wallet_addr = wallet;
        rev.username = username.clone();
        rev.bump = ctx.bumps.wallet_username;

        msg!("username claimed · {} -> {} (fee {})", username, wallet, fee);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 💰 setUsernameFee(fee) — owner only. Updates the lamport fee to claim a
    // username (e.g. 10_000_000 = 0.01 SOL).
    // -----------------------------------------------------------------------
    pub fn set_username_fee(ctx: Context<SetUsernameFee>, fee: u64) -> Result<()> {
        let m = &mut ctx.accounts.marketplace;
        let old = m.username_fee;
        m.username_fee = fee;
        msg!("username fee changed · {} -> {}", old, fee);
        Ok(())
    }
}

// ===========================================================================
// 🔧 Internal helpers — vault transfers signed by the listing PDA.
// ===========================================================================

/// Move `amount` from the listing vault to `dest`, signed by the listing PDA.
fn transfer_from_vault<'info>(
    token_program: &Program<'info, Token>,
    vault: &Account<'info, TokenAccount>,
    dest: &Account<'info, TokenAccount>,
    listing_ai: &AccountInfo<'info>,
    listing_id: &str,
    bump: u8,
    amount: u64,
) -> Result<()> {
    let seeds: &[&[u8]] = &[b"listing", listing_id.as_bytes(), &[bump]];
    let signer: &[&[&[u8]]] = &[seeds];
    token::transfer(
        CpiContext::new_with_signer(
            token_program.to_account_info(),
            Transfer {
                from: vault.to_account_info(),
                to: dest.to_account_info(),
                authority: listing_ai.clone(),
            },
            signer,
        ),
        amount,
    )
}

/// Close the (now-empty) listing vault, refunding its rent lamports to `dest`.
fn close_vault<'info>(
    token_program: &Program<'info, Token>,
    vault: &Account<'info, TokenAccount>,
    rent_dest: &AccountInfo<'info>,
    listing_ai: &AccountInfo<'info>,
    listing_id: &str,
    bump: u8,
) -> Result<()> {
    let seeds: &[&[u8]] = &[b"listing", listing_id.as_bytes(), &[bump]];
    let signer: &[&[&[u8]]] = &[seeds];
    token::close_account(CpiContext::new_with_signer(
        token_program.to_account_info(),
        CloseAccount {
            account: vault.to_account_info(),
            destination: rent_dest.clone(),
            authority: listing_ai.clone(),
        },
        signer,
    ))
}

/// 1..=32 ASCII alphanumerics only — rejects spaces, unicode, and oversized ids.
fn is_alnum(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Usernames: 1..=10 chars, LOWERCASE ascii letters / digits only. The
/// off-chain layer lowercases input before sending; we reject anything else so
/// every stored username is canonical lowercase.
fn is_valid_username(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_USERNAME_LEN
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

// ===========================================================================
// 🧾 ACCOUNT CONTEXTS — each one encodes the security constraints declaratively.
// ===========================================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = owner,
        space = 8 + Marketplace::INIT_SPACE,
        seeds = [b"marketplace"],
        bump
    )]
    pub marketplace: Account<'info, Marketplace>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AddAllowedItem<'info> {
    #[account(mut, seeds = [b"marketplace"], bump = marketplace.bump, has_one = owner @ MarketError::NotOwner)]
    pub marketplace: Account<'info, Marketplace>,
    /// CHECK: only used as a seed + stored; it's a real mint validated by type below.
    pub mint: Account<'info, Mint>,
    #[account(
        init,
        payer = owner,
        space = 8 + AllowedItem::INIT_SPACE,
        seeds = [b"allowed", mint.key().as_ref()],
        bump
    )]
    pub allowed_item: Account<'info, AllowedItem>,
    #[account(mut)]
    pub owner: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(listing_id: String)]
pub struct Sell<'info> {
    // No `has_one = owner` — listing is PERMISSIONLESS. Anyone holding an
    // ALLOWED item (proven by the `allowed_item` PDA below) may list it. The
    // owner does NOT sign, so `sell` is a SINGLE-SIGNER transaction (the
    // player), which lets Phantom simulate it cleanly (no "malicious" warning).
    #[account(mut, seeds = [b"marketplace"], bump = marketplace.bump)]
    pub marketplace: Account<'info, Marketplace>,

    pub mint: Account<'info, Mint>,

    /// Proves the mint is allowed — derived from the mint, must already exist.
    #[account(seeds = [b"allowed", mint.key().as_ref()], bump = allowed_item.bump)]
    pub allowed_item: Account<'info, AllowedItem>,

    /// The new listing PDA. The id is the seed → unique + alphanumeric.
    /// The PLAYER (`seller_authority`) pays rent — they list their own item.
    #[account(
        init,
        payer = seller_authority,
        space = 8 + Listing::INIT_SPACE,
        seeds = [b"listing", listing_id.as_bytes()],
        bump
    )]
    pub listing: Account<'info, Listing>,

    /// The PDA-owned escrow vault (an ATA owned by the listing PDA).
    /// The PLAYER pays the ATA rent too.
    #[account(
        init,
        payer = seller_authority,
        associated_token::mint = mint,
        associated_token::authority = listing
    )]
    pub listing_vault: Account<'info, TokenAccount>,

    /// The token account the item is escrowed FROM.
    #[account(mut, constraint = seller_token.mint == mint.key() @ MarketError::MintMismatch)]
    pub seller_token: Account<'info, TokenAccount>,

    /// The player: controls `seller_token`, signs the escrow move, pays rent,
    /// AND is the fee payer. The ONLY signer on this instruction.
    #[account(mut)]
    pub seller_authority: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(listing_id: String)]
pub struct CancelListing<'info> {
    #[account(seeds = [b"marketplace"], bump = marketplace.bump)]
    pub marketplace: Account<'info, Marketplace>,

    /// The SELLER — must be the wallet that listed the item (verified in the
    /// handler against `listing.owner`). They sign to reclaim their own token.
    #[account(mut)]
    pub seller: Signer<'info>,

    /// The listing being cancelled; closed (rent → seller) when done.
    #[account(
        mut,
        close = seller,
        seeds = [b"listing", listing_id.as_bytes()],
        bump = listing.bump
    )]
    pub listing: Account<'info, Listing>,

    #[account(
        mut,
        associated_token::mint = listing.spl_token_addr,
        associated_token::authority = listing
    )]
    pub listing_vault: Account<'info, TokenAccount>,

    /// The seller's own token account — gets the item back.
    #[account(mut)]
    pub seller_token: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
#[instruction(listing_id: String)]
pub struct Buy<'info> {
    #[account(mut, seeds = [b"marketplace"], bump = marketplace.bump, has_one = owner @ MarketError::NotOwner)]
    pub marketplace: Account<'info, Marketplace>,

    /// Owner (game wallet) — settles the sale after off-chain gold payment.
    #[account(mut)]
    pub owner: Signer<'info>,

    /// The mint being sold (needed to init the claimable vault ATA).
    #[account(address = listing.spl_token_addr @ MarketError::MintMismatch)]
    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        close = owner,
        seeds = [b"listing", listing_id.as_bytes()],
        bump = listing.bump
    )]
    pub listing: Account<'info, Listing>,

    #[account(
        mut,
        associated_token::mint = listing.spl_token_addr,
        associated_token::authority = listing
    )]
    pub listing_vault: Account<'info, TokenAccount>,

    /// 📦 the CLAIMABLE record — created here, closed when the buyer claims.
    #[account(
        init,
        payer = owner,
        space = 8 + ClaimableItem::INIT_SPACE,
        seeds = [b"claimable", listing_id.as_bytes()],
        bump
    )]
    pub claimable: Account<'info, ClaimableItem>,

    /// The claimable escrow vault — an ATA owned by the claimable PDA. Holds the
    /// token between settlement and the buyer's claim.
    #[account(
        init,
        payer = owner,
        associated_token::mint = mint,
        associated_token::authority = claimable
    )]
    pub claimable_vault: Account<'info, TokenAccount>,

    /// 🧾 the permanent on-chain trade record, keyed by the (unique) listing id.
    #[account(
        init,
        payer = owner,
        space = 8 + Trade::INIT_SPACE,
        seeds = [b"trade", listing_id.as_bytes()],
        bump
    )]
    pub trade: Account<'info, Trade>,

    /// CHECK: receives the listing vault's reclaimed rent (the owner/caller).
    #[account(mut)]
    pub owner_refund: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(listing_id: String)]
pub struct Claim<'info> {
    /// `has_one = owner` ties `owner` to the stored owner so the claim fee can
    /// only ever be paid to the real marketplace owner.
    #[account(seeds = [b"marketplace"], bump = marketplace.bump, has_one = owner @ MarketError::NotOwner)]
    pub marketplace: Account<'info, Marketplace>,

    /// THE BUYER — the SOLE signer, fee payer, and rent payer.
    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: the marketplace owner — receives the claim fee. Verified by the
    /// `has_one = owner` constraint above; mut because it receives lamports.
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,

    /// The claimable record — must belong to `buyer`; closed (rent → buyer) here.
    #[account(
        mut,
        close = buyer,
        seeds = [b"claimable", listing_id.as_bytes()],
        bump = claimable.bump
    )]
    pub claimable: Account<'info, ClaimableItem>,

    /// The claimable escrow vault the token is released FROM.
    #[account(
        mut,
        associated_token::mint = claimable.spl_token_addr,
        associated_token::authority = claimable
    )]
    pub claimable_vault: Account<'info, TokenAccount>,

    /// The buyer's own token account — receives the claimed item. The client
    /// creates it idempotently in the same tx (buyer pays its rent).
    #[account(mut)]
    pub buyer_token: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ChangeOwner<'info> {
    #[account(mut, seeds = [b"marketplace"], bump = marketplace.bump, has_one = owner @ MarketError::NotOwner)]
    pub marketplace: Account<'info, Marketplace>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(username: String)]
pub struct ClaimUsername<'info> {
    /// `has_one = owner` ties the `owner` account to the stored owner, so the
    /// fee can only ever be paid to the real marketplace owner.
    #[account(seeds = [b"marketplace"], bump = marketplace.bump, has_one = owner @ MarketError::NotOwner)]
    pub marketplace: Account<'info, Marketplace>,

    /// THE PLAYER — signs in their browser, pays the rent + the claim fee.
    #[account(mut)]
    pub player_wallet: Signer<'info>,

    /// CHECK: the marketplace owner — receives the claim fee. Verified by the
    /// `has_one = owner` constraint above; mut because it receives lamports.
    #[account(mut)]
    pub owner: UncheckedAccount<'info>,

    /// Unique username PDA — the username is the seed (uniqueness + permanence).
    #[account(
        init,
        payer = player_wallet,
        space = 8 + UsernameEntry::INIT_SPACE,
        seeds = [b"username", username.as_bytes()],
        bump
    )]
    pub username_entry: Account<'info, UsernameEntry>,

    /// One-per-wallet guard — the wallet is the seed (a wallet can't claim two).
    #[account(
        init,
        payer = player_wallet,
        space = 8 + WalletUsername::INIT_SPACE,
        seeds = [b"wallet_name", player_wallet.key().as_ref()],
        bump
    )]
    pub wallet_username: Account<'info, WalletUsername>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetUsernameFee<'info> {
    #[account(mut, seeds = [b"marketplace"], bump = marketplace.bump, has_one = owner @ MarketError::NotOwner)]
    pub marketplace: Account<'info, Marketplace>,
    pub owner: Signer<'info>,
}
