use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub address: String,
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub pool_address: String,
    pub fee_amount: u32,
    pub chain: String,
    pub token0: TokenConfig,
    pub token1: TokenConfig,
    pub volatile_token: TokenRole,
    pub stable_token: TokenRole,
    pub db_path: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRole {
    Token0,
    Token1,
}

impl PoolConfig {
    pub fn is_volatile_token0(&self) -> bool {
        self.volatile_token == TokenRole::Token0
    }

    pub fn volatile_decimals(&self) -> u8 {
        match self.volatile_token {
            TokenRole::Token0 => self.token0.decimals,
            TokenRole::Token1 => self.token1.decimals,
        }
    }

    pub fn stable_decimals(&self) -> u8 {
        match self.stable_token {
            TokenRole::Token0 => self.token0.decimals,
            TokenRole::Token1 => self.token1.decimals,
        }
    }

    pub fn volatile_symbol(&self) -> &str {
        match self.volatile_token {
            TokenRole::Token0 => &self.token0.symbol,
            TokenRole::Token1 => &self.token1.symbol,
        }
    }

    pub fn stable_symbol(&self) -> &str {
        match self.stable_token {
            TokenRole::Token0 => &self.token0.symbol,
            TokenRole::Token1 => &self.token1.symbol,
        }
    }
}

pub fn cbbtc_usdc_base() -> PoolConfig {
    PoolConfig {
        pool_address: "0xeC558e484cC9f2210714E345298fdc53B253c27D".into(),
        fee_amount: 3000,
        chain: "base".into(),
        token0: TokenConfig {
            symbol: "USDC".into(),
            name: "USD Coin".into(),
            decimals: 6,
            address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        },
        token1: TokenConfig {
            symbol: "cbBTC".into(),
            name: "Coinbase Wrapped Bitcoin".into(),
            decimals: 8,
            address: "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".into(),
        },
        volatile_token: TokenRole::Token1,
        stable_token: TokenRole::Token0,
        db_path: "data/cbBTC-USDC-BASE_0xeC558e484cC9f2210714E345298fdc53B253c27D.db".into(),
        display_name: "cbBTC-USDC Base 0.3%".into(),
    }
}

pub fn wbtc_usdc() -> PoolConfig {
    PoolConfig {
        pool_address: "0x99ac8cA7087fA4A2A1FB6357269965A2014ABc35".into(),
        fee_amount: 3000,
        chain: "eth".into(),
        token0: TokenConfig {
            symbol: "WBTC".into(),
            name: "Wrapped Bitcoin".into(),
            decimals: 8,
            address: "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599".into(),
        },
        token1: TokenConfig {
            symbol: "USDC".into(),
            name: "USD Coin".into(),
            decimals: 6,
            address: "0xA0b86a33E6417c8Ade68E31cAdE412F9a8f03C5B".into(),
        },
        volatile_token: TokenRole::Token0,
        stable_token: TokenRole::Token1,
        db_path: "data/WBTC-USDC_0x99ac8cA7087fA4A2A1FB6357269965A2014ABc35.db".into(),
        display_name: "WBTC-USDC 0.3%".into(),
    }
}

pub fn pool_by_name(name: &str) -> PoolConfig {
    match name.to_uppercase().as_str() {
        "CBBTC_USDC_BASE" => cbbtc_usdc_base(),
        "WBTC_USDC" => wbtc_usdc(),
        _ => cbbtc_usdc_base(),
    }
}
