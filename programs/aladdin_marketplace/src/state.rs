use anchor_lang::prelude::*;

/// Max length (in bytes/ASCII chars) for listing ids and usernames.
pub const MAX_ID_LEN: usize = 32;

/// 🏛 THE REGISTRY ROOT — a single PDA (seed `["marketplace"]`) that *is* the
/// marketplace's own on-chain address. It stores the current owner (the game's
/// wallet, the only authority allowed to sell/buy/admin) and bookkeeping.
///
/// Because this PDA signs for every escrow vault, the marketplace literally
/// "has its own address" exactly as the spec requires.
#[account]
#[derive(InitSpace)]
pub struct Marketplace {
    /// The privileged authority — the game's wallet. Only this key may call
    /// `sell`, `buy`, `add_allowed_item`, `change_owner`, and `add_username`.
    pub owner: Pubkey,
    /// Total listings ever created (monotonic; for off-chain analytics).
    pub total_listings: u64,
    /// Total allowed items registered.
    pub total_allowed: u64,
    /// Total completed trades — also the monotonic `id` for each `Trade`.
    pub total_trades: u64,
    /// 💰 fee (in lamports) a player pays to the owner to claim a username.
    /// Owner-settable; defaults to 0.01 SOL at initialize.
    pub username_fee: u64,
    /// PDA bump for `["marketplace"]`.
    pub bump: u8,
}

/// ✅ ALLOWED ITEMS — one PDA per permitted SPL mint
/// (seed `["allowed", mint]`). The PDA *existing* means the mint is allowed,
/// so the set is inherently unique (you can't init the same PDA twice).
#[account]
#[derive(InitSpace)]
pub struct AllowedItem {
    /// The SPL token mint that may be listed.
    pub mint: Pubkey,
    pub bump: u8,
}

/// 🏷 LISTED ITEMS — one PDA per active listing
/// (seed `["listing", listing_id]`). The listing id is the seed, so every id is
/// globally unique and alphanumeric. The PDA also owns the escrow vault that
/// holds the listed token while it is for sale.
#[account]
#[derive(InitSpace)]
pub struct Listing {
    /// The escrowed SPL token mint.
    pub spl_token_addr: Pubkey,
    /// The wallet that owns this listing (the seller). Duplicates allowed —
    /// one wallet may hold many listings.
    pub owner: Pubkey,
    /// Price in whole gold-token units — NO decimals (a plain u64 integer).
    pub price: u64,
    /// The unique alphanumeric listing id (also the PDA seed).
    #[max_len(MAX_ID_LEN)]
    pub listing_id: String,
    /// PDA bump for `["listing", listing_id]`.
    pub bump: u8,
}

/// 🧾 TRADES — one PDA per completed sale (seed `["trade", listing_id]`). The
/// `buy` instruction writes this record after releasing the escrowed token, so
/// every settled trade is permanently logged on-chain. Keyed by the (unique)
/// listing id, so there is exactly one trade record per listing.
#[account]
#[derive(InitSpace)]
pub struct Trade {
    /// Monotonic trade id (snapshot of `Marketplace.total_trades` at sale time).
    pub id: u64,
    /// The listing this trade settled (the unique alphanumeric listing id).
    #[max_len(MAX_ID_LEN)]
    pub listing_id: String,
    /// The SPL token mint that changed hands.
    pub spl_token_addr: Pubkey,
    /// The seller — the listing's recorded owner.
    pub seller: Pubkey,
    /// The buyer — the `new_owner` the item was released to.
    pub buyer: Pubkey,
    /// The whole-number price the item sold for (no decimals).
    pub price: u64,
    /// PDA bump for `["trade", listing_id]`.
    pub bump: u8,
}

/// 👤 USERNAME REGISTRY — one PDA per username (seed `["username", username]`).
/// Maps a unique alphanumeric username to a wallet address. Permanent: once
/// written it can never be changed or reassigned (the PDA can't be re-init'd).
#[account]
#[derive(InitSpace)]
pub struct UsernameEntry {
    /// The wallet this username belongs to.
    pub wallet_addr: Pubkey,
    /// The unique alphanumeric username (also the PDA seed).
    #[max_len(MAX_ID_LEN)]
    pub username: String,
    pub bump: u8,
}

/// 🔁 REVERSE LOOKUP / ONE-PER-WALLET GUARD — one PDA per wallet
/// (seed `["wallet_name", wallet]`). Its existence proves a wallet has already
/// claimed a username, enforcing exactly one permanent username per wallet.
#[account]
#[derive(InitSpace)]
pub struct WalletUsername {
    pub wallet_addr: Pubkey,
    #[max_len(MAX_ID_LEN)]
    pub username: String,
    pub bump: u8,
}
