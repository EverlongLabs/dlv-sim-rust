mod config;
mod enums;
mod pool_config;
mod price_feed;
mod event_reader;
mod engine;
mod vault;
mod arb;
mod strategy;
mod output;

use std::time::Instant;

fn main() {
    let t0 = Instant::now();
    let cfg = config::Config::from_env();

    println!(
        "[CONFIG] pool={} fee={} dates={}..{} step={}s arb={}",
        cfg.pool_selection,
        cfg.pool_config.fee_amount,
        cfg.start_date.format("%Y-%m-%d"),
        cfg.end_date.format("%Y-%m-%d"),
        cfg.lookup_period,
        cfg.is_arb_strategy,
    );

    let result = strategy::run_backtest(&cfg);

    let elapsed = t0.elapsed().as_secs_f64();
    println!(
        "[DONE] {} rows in {:.1}s | APY={:.2}% totalReturn={:.2}%",
        result.row_count,
        elapsed,
        result.apy,
        result.total_return,
    );
}
