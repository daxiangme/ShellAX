//! 生成类工具的会话级历史：按批次保存，新批次在最上；只存在内存中，不写盘。
use super::Row;

/// 每个工具最多保留的批次数与总行数。
pub const MAX_BATCHES: usize = 20;
pub const MAX_ROWS: usize = 500;

/// 一次“运行”产生的一批结果。
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub id: u64,
    /// 生成时间（本地时间 HH:MM:SS）。
    pub time: String,
    /// 参数摘要，例如“16 位 · A-Z a-z 0-9 · ×5”。
    pub meta: String,
    pub rows: Vec<Row>,
    pub folded: bool,
    /// 敏感内容是否被遮住（只对含 `secret` 行的批次有意义）。
    pub masked: bool,
}

impl Batch {
    /// 整批的文本：每行一条，有标签时为“标签: 内容”。
    pub fn text(&self) -> String { super::rows_text(&self.rows, super::RowFmt::Colon) }
    pub fn has_secret(&self) -> bool { self.rows.iter().any(|r| r.secret) }
}

#[derive(Clone, Debug, Default)]
pub struct History { pub batches: Vec<Batch>, next: u64 }

impl History {
    /// 放入新批次（最上面）。超过上限时从最旧的批次裁掉；单批超过行数上限时截断该批尾部。
    pub fn push(&mut self, time: impl Into<String>, meta: impl Into<String>, mut rows: Vec<Row>) -> u64 {
        rows.truncate(MAX_ROWS);
        self.next += 1;
        let id = self.next;
        self.batches.insert(0, Batch { id, time: time.into(), meta: meta.into(), rows, folded: false, masked: false });
        self.batches.truncate(MAX_BATCHES);
        // 总行数超出上限时，从最旧的批次尾部开始裁。
        let mut over = self.row_count().saturating_sub(MAX_ROWS);
        while over > 0 {
            let Some(last) = self.batches.last_mut() else { break };
            let cut = over.min(last.rows.len());
            last.rows.truncate(last.rows.len() - cut);
            over -= cut;
            if last.rows.is_empty() { self.batches.pop(); }
        }
        id
    }

    pub fn row_count(&self) -> usize { self.batches.iter().map(|b| b.rows.len()).sum() }
    pub fn is_empty(&self) -> bool { self.batches.is_empty() }
    pub fn clear(&mut self) { self.batches.clear(); }
    pub fn latest(&self) -> Option<&Batch> { self.batches.first() }
    pub fn batch_mut(&mut self, id: u64) -> Option<&mut Batch> { self.batches.iter_mut().find(|b| b.id == id) }

    /// 删除一行；批次空了就一并删除。
    pub fn remove_row(&mut self, batch: u64, row: usize) {
        if let Some(b) = self.batch_mut(batch) { if row < b.rows.len() { b.rows.remove(row); } }
        self.batches.retain(|b| !b.rows.is_empty());
    }
    pub fn remove_batch(&mut self, batch: u64) { self.batches.retain(|b| b.id != batch); }
    pub fn toggle_fold(&mut self, batch: u64) { if let Some(b) = self.batch_mut(batch) { b.folded = !b.folded; } }
    pub fn toggle_mask(&mut self, batch: u64) { if let Some(b) = self.batch_mut(batch) { b.masked = !b.masked; } }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows(n: usize) -> Vec<Row> { (0..n).map(|i| Row::item(format!("v{i}"))).collect() }

    #[test]
    fn newest_first_and_kept() {
        let mut h = History::default();
        h.push("10:00:00", "a", rows(3));
        h.push("10:00:05", "b", rows(2));
        assert_eq!(h.batches.len(), 2);
        assert_eq!(h.latest().unwrap().meta, "b");
        assert_eq!(h.batches[1].rows.len(), 3);
        assert_eq!(h.latest().unwrap().text(), "v0\nv1");
    }

    #[test]
    fn batch_limit() {
        let mut h = History::default();
        for i in 0..30 { h.push("t", format!("{i}"), rows(1)); }
        assert_eq!(h.batches.len(), MAX_BATCHES);
        assert_eq!(h.latest().unwrap().meta, "29");
        assert_eq!(h.batches.last().unwrap().meta, "10");
    }

    #[test]
    fn row_limit_drops_oldest() {
        let mut h = History::default();
        h.push("t", "old", rows(300));
        h.push("t", "mid", rows(300));
        assert!(h.row_count() <= MAX_ROWS);
        assert_eq!(h.latest().unwrap().rows.len(), 300);
        assert_eq!(h.batches.len(), 2);
        assert_eq!(h.batches[1].rows.len(), 200);
        h.push("t", "huge", rows(900));
        assert_eq!(h.row_count(), MAX_ROWS);
        assert_eq!(h.batches.len(), 1);
    }

    #[test]
    fn remove_and_clear() {
        let mut h = History::default();
        let a = h.push("t", "a", rows(2));
        let b = h.push("t", "b", rows(1));
        h.remove_row(a, 0);
        assert_eq!(h.batches[1].rows.len(), 1);
        h.remove_row(b, 0);
        assert_eq!(h.batches.len(), 1);
        h.toggle_fold(a);
        assert!(h.batches[0].folded);
        h.clear();
        assert!(h.is_empty());
    }
}
