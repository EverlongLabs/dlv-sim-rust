use std::fs::File;
use std::path::Path;

use arrow::array::{Array, Int32Array, StringArray};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::enums::EventType;
use v3_pool::types::{I256, U256};

#[derive(Debug, Clone)]
pub struct PoolEvent {
    pub event_type: EventType,
    pub id: i32,
    pub msg_sender: String,
    pub recipient: String,
    pub amount0: I256,
    pub amount1: I256,
    pub tick_lower: Option<i32>,
    pub tick_upper: Option<i32>,
    pub liquidity: I256,
    pub amount_specified: Option<I256>,
    pub sqrt_price_x96: Option<U256>,
    pub tick: Option<i32>,
    pub block_number: i32,
    pub log_index: i32,
    pub date_str: String,
    pub date_ms: i64,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ParquetPoolConfig {
    pub id: String,
    pub token0: String,
    pub token1: String,
    pub fee: u32,
    #[serde(rename = "tickSpacing")]
    pub tick_spacing: i32,
    #[serde(rename = "initialSqrtPriceX96")]
    pub initial_sqrt_price_x96: String,
}

pub struct EventReader {
    parquet_path: String,
    config_path: String,
    pool_config: Option<ParquetPoolConfig>,
}

impl EventReader {
    pub fn new(db_path: &str) -> Self {
        let base = db_path.trim_end_matches(".db");
        EventReader {
            parquet_path: format!("{}.parquet", base),
            config_path: format!("{}.pool-config.json", base),
            pool_config: None,
        }
    }

    pub fn exists(&self) -> bool {
        Path::new(&self.parquet_path).exists()
    }

    pub fn load_config(&mut self) -> &ParquetPoolConfig {
        if self.pool_config.is_none() {
            let data = std::fs::read_to_string(&self.config_path)
                .unwrap_or_else(|e| panic!("Cannot read {}: {}", self.config_path, e));
            self.pool_config = Some(serde_json::from_str(&data)
                .unwrap_or_else(|e| panic!("Cannot parse {}: {}", self.config_path, e)));
        }
        self.pool_config.as_ref().unwrap()
    }

    pub fn get_pool_config(&mut self) -> (String, u32, i32) {
        let cfg = self.load_config();
        (cfg.id.clone(), cfg.fee, cfg.tick_spacing)
    }

    pub fn get_initial_sqrt_price_x96(&mut self) -> U256 {
        let cfg = self.load_config();
        parse_u256(&cfg.initial_sqrt_price_x96)
    }

    pub fn load_all_events(
        &self,
        start_date: Option<&str>,
        end_date: Option<&str>,
    ) -> Vec<PoolEvent> {
        let file = File::open(&self.parquet_path)
            .unwrap_or_else(|e| panic!("Cannot open {}: {}", self.parquet_path, e));

        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap_or_else(|e| panic!("Cannot read parquet {}: {}", self.parquet_path, e));

        let reader = builder.build()
            .unwrap_or_else(|e| panic!("Cannot build parquet reader: {}", e));

        let mut events = Vec::new();
        let mut schema_printed = false;

        for batch_result in reader {
            let batch: RecordBatch = batch_result.unwrap();
            let n = batch.num_rows();

            if !schema_printed {
                let schema = batch.schema();
                let col_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
                eprintln!("[PARQUET] columns: {:?}", col_names);
                schema_printed = true;
            }

            // Column indices matching the parquet schema from TS:
            // 0: type(INT32), 1: id(INT32), 2: msg_sender(STRING), 3: recipient(STRING),
            // 4: amount0(STRING), 5: amount1(STRING), 6: tick_lower(INT32 nullable),
            // 7: tick_upper(INT32 nullable), 8: liquidity(STRING),
            // 9: amount_specified(STRING nullable), 10: sqrt_price_x96(STRING nullable),
            // 11: tick(INT32 nullable), 12: block_number(INT32),
            // 13: transaction_hash(STRING), 14: log_index(INT32),
            // 15: date(STRING), 16: verified(BOOLEAN)

            let col_type = batch.column(0).as_any().downcast_ref::<Int32Array>().unwrap();
            let col_id = batch.column(1).as_any().downcast_ref::<Int32Array>().unwrap();
            let col_sender = batch.column(2).as_any().downcast_ref::<StringArray>().unwrap();
            let col_recipient = batch.column(3).as_any().downcast_ref::<StringArray>().unwrap();
            let col_amount0 = batch.column(4).as_any().downcast_ref::<StringArray>().unwrap();
            let col_amount1 = batch.column(5).as_any().downcast_ref::<StringArray>().unwrap();
            let col_tick_lower = batch.column(6).as_any().downcast_ref::<Int32Array>().unwrap();
            let col_tick_upper = batch.column(7).as_any().downcast_ref::<Int32Array>().unwrap();
            let col_liquidity = batch.column(8).as_any().downcast_ref::<StringArray>().unwrap();
            let col_amt_specified = batch.column(9).as_any().downcast_ref::<StringArray>().unwrap();
            let col_sqrt_price = batch.column(10).as_any().downcast_ref::<StringArray>().unwrap();
            let col_tick = batch.column(11).as_any().downcast_ref::<Int32Array>().unwrap();
            let col_block = batch.column(12).as_any().downcast_ref::<Int32Array>().unwrap();
            // 13: transaction_hash — skip
            let col_log_idx = batch.column(14).as_any().downcast_ref::<Int32Array>().unwrap();
            let col_date = batch.column(15).as_any().downcast_ref::<StringArray>().unwrap();

            for i in 0..n {
                let date_str = col_date.value(i).to_string();

                if let Some(s) = start_date {
                    if date_str.as_str() < s { continue; }
                }
                if let Some(e) = end_date {
                    if date_str.as_str() >= e { continue; }
                }

                let event_type = EventType::from_i32(col_type.value(i));
                let tick_lower = if col_tick_lower.is_null(i) { None } else { Some(col_tick_lower.value(i)) };
                let tick_upper = if col_tick_upper.is_null(i) { None } else { Some(col_tick_upper.value(i)) };
                let amt_spec = if col_amt_specified.is_null(i) { None } else {
                    Some(parse_i256(col_amt_specified.value(i)))
                };
                let sqrt_p = if col_sqrt_price.is_null(i) { None } else {
                    Some(parse_u256(col_sqrt_price.value(i)))
                };
                let tick = if col_tick.is_null(i) { None } else { Some(col_tick.value(i)) };

                let date_ms = parse_date_to_ms(&date_str);

                events.push(PoolEvent {
                    event_type,
                    id: col_id.value(i),
                    msg_sender: col_sender.value(i).to_string(),
                    recipient: col_recipient.value(i).to_string(),
                    amount0: parse_i256(col_amount0.value(i)),
                    amount1: parse_i256(col_amount1.value(i)),
                    tick_lower,
                    tick_upper,
                    liquidity: parse_i256(col_liquidity.value(i)),
                    amount_specified: amt_spec,
                    sqrt_price_x96: sqrt_p,
                    tick,
                    block_number: col_block.value(i),
                    log_index: col_log_idx.value(i),
                    date_str,
                    date_ms,
                });
            }
        }

        events
    }
}

fn parse_date_to_ms(s: &str) -> i64 {
    use chrono::NaiveDateTime;
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|dt| dt.and_utc().timestamp_millis())
        .unwrap_or(0)
}

pub fn parse_i256(s: &str) -> I256 {
    let s = s.trim();
    if s.is_empty() || s == "0" {
        return I256::ZERO;
    }
    let negative = s.starts_with('-');
    let digits = if negative { &s[1..] } else { s };
    let magnitude = parse_u256(digits);
    if negative {
        -I256(magnitude)
    } else {
        I256(magnitude)
    }
}

pub fn parse_u256(s: &str) -> U256 {
    let s = s.trim();
    if s.is_empty() || s == "0" {
        return U256::ZERO;
    }
    let mut result = U256::ZERO;
    let ten = U256::from_u64(10);
    for &b in s.as_bytes() {
        if !b.is_ascii_digit() { continue; }
        let digit = U256::from_u64((b - b'0') as u64);
        result = result * ten + digit;
    }
    result
}

pub fn fmt_utc_date(ms: i64) -> String {
    use chrono::DateTime;
    let dt = DateTime::from_timestamp_millis(ms).unwrap_or_default();
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}
