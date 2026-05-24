//! Lumo の値の型。型検査(typeck)とコード生成(codegen)で共有する。

/// 配列の要素型・map の値型。スカラ・構造体に加え、配列/map も入れ子にできる
/// （`[[int]]` や `{string: [int]}` など）。入れ子の要素型は `intern_elem` で
/// リークした `&'static Elem` で指すので、[`Type`] は `Box` 無しで `Copy` のまま。
/// 参照同士の `==` は指す先を比較するので、入れ子の型も構造的に等価判定できる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    Int,
    Bool,
    Float,
    Str,
    Struct(&'static str),
    /// ユーザー定義の enum（直和型）を要素/値に持つ。
    Enum(&'static str),
    /// 配列を要素に持つ（`[[T]]`）。指す先はその内側の配列の要素型。
    Array(&'static Elem),
    /// map を要素/値に持つ（`[{string: V}]` や `{string: {string: V}}`）。
    /// キーは常に string なので、指す先は値型のみ。
    Map(&'static Elem),
}

impl Elem {
    pub fn name(self) -> String {
        match self {
            Elem::Int => "int".to_string(),
            Elem::Bool => "bool".to_string(),
            Elem::Float => "float".to_string(),
            Elem::Str => "string".to_string(),
            Elem::Struct(n) => n.to_string(),
            Elem::Enum(n) => n.to_string(),
            Elem::Array(e) => format!("[{}]", e.name()),
            Elem::Map(v) => format!("{{string: {}}}", v.name()),
        }
    }

    pub fn to_type(self) -> Type {
        match self {
            Elem::Int => Type::Int,
            Elem::Bool => Type::Bool,
            Elem::Float => Type::Float,
            Elem::Str => Type::Str,
            Elem::Struct(n) => Type::Struct(n),
            Elem::Enum(n) => Type::Enum(n),
            Elem::Array(e) => Type::Array(*e),
            Elem::Map(v) => Type::Map(*v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Float,
    /// イミュータブルな文字列（グローバル定数 or ヒープ確保した連結結果へのポインタ）。
    Str,
    /// 要素型 `Elem` の配列（ヒープ確保。[長さ i64][8byteスロット×N]）。
    Array(Elem),
    /// ユーザー定義の構造体（名前で参照。ヒープ確保したポインタ）。
    /// 名前は `intern` でリークして `&'static str` にし、`Copy` を保つ。
    Struct(&'static str),
    /// ユーザー定義の enum（直和型。名前で参照。ヒープ確保した {tag, slots} へのポインタ）。
    Enum(&'static str),
    /// 連想配列 `{string: V}`。キーは string 固定、値の型は `Elem`（v1 はスカラ/構造体）。
    /// ヒープ確保したハッシュ表ヘッダへのポインタ（RFC 0002）。
    Map(Elem),
    /// `null` リテラルの型。参照型（string/array/struct/map）と互換。
    Null,
}

impl Type {
    pub fn name(self) -> String {
        match self {
            Type::Int => "int".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Float => "float".to_string(),
            Type::Str => "string".to_string(),
            Type::Array(e) => format!("[{}]", e.name()),
            Type::Struct(n) => n.to_string(),
            Type::Enum(n) => n.to_string(),
            Type::Map(v) => format!("{{string: {}}}", v.name()),
            Type::Null => "null".to_string(),
        }
    }

    /// 算術・大小比較が使える数値型か
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// ヒープ上のポインタで表される参照型か（null を代入できる先）。
    pub fn is_reference(self) -> bool {
        matches!(
            self,
            Type::Str | Type::Array(_) | Type::Struct(_) | Type::Enum(_) | Type::Map(_)
        )
    }

    /// 配列の要素型・map の値型として使える `Elem` に変換する。配列/map も
    /// 入れ子にできる（内側の要素型を `intern_elem` でリークして指す）。`null`
    /// は型が決まらないので要素型/値型にはできない。
    pub fn as_elem(self) -> Option<Elem> {
        match self {
            Type::Int => Some(Elem::Int),
            Type::Bool => Some(Elem::Bool),
            Type::Float => Some(Elem::Float),
            Type::Str => Some(Elem::Str),
            Type::Struct(n) => Some(Elem::Struct(n)),
            Type::Enum(n) => Some(Elem::Enum(n)),
            Type::Array(e) => Some(Elem::Array(intern_elem(e))),
            Type::Map(v) => Some(Elem::Map(intern_elem(v))),
            Type::Null => None,
        }
    }
}

/// 識別子を `&'static str` にする（短命なコンパイラなのでリークで十分）。
/// 構造体名を `Type::Struct` に埋め込み Copy を保つために使う。
pub fn intern(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// 入れ子コレクションの要素型を `&'static Elem` にする（`intern` と同じ方針で
/// リーク）。`Elem::Array`/`Elem::Map` に埋め込み `Copy` を保つために使う。
pub fn intern_elem(e: Elem) -> &'static Elem {
    Box::leak(Box::new(e))
}
