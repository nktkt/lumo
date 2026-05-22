//! ソース上の位置情報。すべてバイトオフセット（UTF-8）で持つ。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 開始バイトオフセット（含む）
    pub start: usize,
    /// 終了バイトオフセット（含まない）
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    /// 2つの span を包含する最小の span を返す（左辺の開始〜右辺の終了など）
    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}
