//! ソース上の位置情報。すべてバイトオフセット（UTF-8）で持つ。
//! 複数ファイル対応のため、各 [`Span`] はどのファイルかを示す [`FileId`] も持ち、
//! 実際のパス・中身は [`SourceMap`] が一元管理する。

/// ソースファイルの識別子（[`SourceMap`] 内のインデックス）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// このスパンが属するソースファイル。
    pub file: FileId,
    /// 開始バイトオフセット（含む）
    pub start: usize,
    /// 終了バイトオフセット（含まない）
    pub end: usize,
}

impl Span {
    pub fn new(file: FileId, start: usize, end: usize) -> Self {
        Span { file, start, end }
    }

    /// 2つの span を包含する最小の span を返す（同じファイル内である前提）。
    pub fn merge(self, other: Span) -> Span {
        debug_assert_eq!(
            self.file, other.file,
            "異なるファイルの span は merge できません"
        );
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// 1つのソースファイル（表示用のパスと中身）。
pub struct SourceFile {
    pub path: String,
    pub src: String,
}

/// コンパイル対象の全ソースファイル。[`FileId`] はこのコレクションへの添字。
/// 診断は span の `file` からここを引いて、正しいファイル名と該当行を表示する。
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap { files: Vec::new() }
    }

    /// ファイルを登録し、その [`FileId`] を返す。
    pub fn add(&mut self, path: impl Into<String>, src: impl Into<String>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile {
            path: path.into(),
            src: src.into(),
        });
        id
    }

    /// `id` のソースファイルを取り出す。
    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }
}
