#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LookUpPeriod {
    TwoSeconds = 2,
    FiveSeconds = 5,
    TwelveSeconds = 12,
    Minutely = 60,
    Hourly = 3600,
    FourHourly = 14400,
    Daily = 86400,
}

impl LookUpPeriod {
    pub fn from_secs(s: u32) -> Self {
        match s {
            2 => Self::TwoSeconds,
            5 => Self::FiveSeconds,
            12 => Self::TwelveSeconds,
            60 => Self::Minutely,
            3600 => Self::Hourly,
            14400 => Self::FourHourly,
            86400 => Self::Daily,
            _ => Self::FiveSeconds,
        }
    }

    pub fn as_secs(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    AfterNewTimePeriod,
    AfterEventApplied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebalance {
    Dlv,
    Alm,
    RegulateDebt,
    Arbitrage,
    LevAmm,
    Snapshot,
    CircuitBreaker,
    SlowRecenter,
    CrCircuitBreaker,
    VolCircuitBreaker,
    StagedWithdrawal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Mint = 1,
    Burn = 2,
    Swap = 3,
}

impl EventType {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Mint,
            2 => Self::Burn,
            3 => Self::Swap,
            _ => panic!("Unknown EventType: {}", v),
        }
    }
}
