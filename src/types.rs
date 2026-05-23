//! Lumo の値の型。型検査(typeck)とコード生成(codegen)で共有する。

/// 配列の要素型（スカラのみ。入れ子の配列は今のところ非対応）。
/// これにより [`Type`] が `Box` 無しで `Copy` のままでいられる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elem {
    Int,
    Bool,
    Float,
    Str,
}

impl Elem {
    pub fn name(self) -> &'static str {
        match self {
            Elem::Int => "int",
            Elem::Bool => "bool",
            Elem::Float => "float",
            Elem::Str => "string",
        }
    }

    pub fn to_type(self) -> Type {
        match self {
            Elem::Int => Type::Int,
            Elem::Bool => Type::Bool,
            Elem::Float => Type::Float,
            Elem::Str => Type::Str,
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
    /// `null` リテラルの型。参照型（string/array/struct）と互換。
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
            Type::Null => "null".to_string(),
        }
    }

    /// 算術・大小比較が使える数値型か
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// ヒープ上のポインタで表される参照型か（null を代入できる先）。
    pub fn is_reference(self) -> bool {
        matches!(self, Type::Str | Type::Array(_) | Type::Struct(_))
    }

    /// 配列の要素型として使えるスカラ型なら、その `Elem` を返す。
    pub fn as_elem(self) -> Option<Elem> {
        match self {
            Type::Int => Some(Elem::Int),
            Type::Bool => Some(Elem::Bool),
            Type::Float => Some(Elem::Float),
            Type::Str => Some(Elem::Str),
            Type::Array(_) | Type::Struct(_) | Type::Null => None,
        }
    }
}

/// 識別子を `&'static str` にする（短命なコンパイラなのでリークで十分）。
/// 構造体名を `Type::Struct` に埋め込み Copy を保つために使う。
pub fn intern(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}
