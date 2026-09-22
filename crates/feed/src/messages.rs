use serde::Deserialize;

/// One entry of a book update: ("new" | "change" | "delete", price, amount).
pub type BookEntry = (String, f64, f64);

#[derive(Debug, Deserialize)]
pub struct Notification {
    pub params: NotificationParams,
}

#[derive(Debug, Deserialize)]
pub struct NotificationParams {
    pub channel: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct BookMsg {
    #[serde(rename = "type")]
    pub kind: String,
    pub timestamp: i64,
    pub change_id: u64,
    #[serde(default)]
    pub prev_change_id: Option<u64>,
    pub bids: Vec<BookEntry>,
    pub asks: Vec<BookEntry>,
}

#[derive(Debug, Deserialize)]
pub struct TradeMsg {
    pub trade_seq: u64,
    pub timestamp: i64,
    pub price: f64,
    pub amount: f64,
    /// "buy" means the taker bought, so a resting ask was consumed.
    pub direction: String,
}

#[derive(Debug, Deserialize)]
pub struct QuoteMsg {
    pub timestamp: i64,
    pub best_bid_price: Option<f64>,
    #[serde(default)]
    pub best_bid_amount: f64,
    pub best_ask_price: Option<f64>,
    #[serde(default)]
    pub best_ask_amount: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r#"{"params":{"channel":"book.BTC-PERPETUAL.100ms","data":{"type":"snapshot","timestamp":1550588283458,"instrument_name":"BTC-PERPETUAL","change_id":297217,"bids":[["new",3955.75,30.0]],"asks":[["new",3956.0,20.0]]}}}"#;

    const CHANGE: &str = r#"{"params":{"channel":"book.BTC-PERPETUAL.100ms","data":{"type":"change","timestamp":1550588283459,"change_id":297218,"prev_change_id":297217,"bids":[["change",3955.75,40.0],["delete",3954.0,0.0]],"asks":[]}}}"#;

    const TRADES: &str = r#"{"params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"trade_seq":30289432,"trade_id":"48079254","timestamp":1590484156350,"price":8767.5,"amount":100.0,"direction":"buy","tick_direction":0,"index_price":8763.13,"instrument_name":"BTC-PERPETUAL"}]}}"#;

    const QUOTE: &str = r#"{"params":{"channel":"quote.BTC-PERPETUAL","data":{"timestamp":1550658624149,"instrument_name":"BTC-PERPETUAL","best_bid_price":3914.97,"best_bid_amount":40.0,"best_ask_price":3996.61,"best_ask_amount":50.0}}}"#;

    #[test]
    fn parses_a_snapshot() {
        let n: Notification = serde_json::from_str(SNAPSHOT).unwrap();
        let book: BookMsg = serde_json::from_value(n.params.data).unwrap();
        assert_eq!(book.kind, "snapshot");
        assert_eq!(book.change_id, 297217);
        assert_eq!(book.prev_change_id, None);
        assert_eq!(book.bids[0].0, "new");
        assert_eq!(book.bids[0].1, 3955.75);
    }

    #[test]
    fn parses_a_change_with_a_delete() {
        let n: Notification = serde_json::from_str(CHANGE).unwrap();
        let book: BookMsg = serde_json::from_value(n.params.data).unwrap();
        assert_eq!(book.prev_change_id, Some(297217));
        assert_eq!(book.bids[1].0, "delete");
        assert_eq!(book.bids[1].2, 0.0);
    }

    #[test]
    fn parses_a_trade_batch() {
        let n: Notification = serde_json::from_str(TRADES).unwrap();
        let trades: Vec<TradeMsg> = serde_json::from_value(n.params.data).unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].direction, "buy");
        assert_eq!(trades[0].amount, 100.0);
    }

    #[test]
    fn parses_a_quote() {
        let n: Notification = serde_json::from_str(QUOTE).unwrap();
        let quote: QuoteMsg = serde_json::from_value(n.params.data).unwrap();
        assert_eq!(quote.best_bid_price, Some(3914.97));
        assert_eq!(quote.best_ask_amount, 50.0);
    }
}
