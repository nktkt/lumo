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
}

impl Type {
    pub fn name(self) -> String {
        match self {
            Type::Int => "int".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Float => "float".to_string(),
            Type::Str => "string".to_string(),
            Type::Array(e) => format!("[{}]", e.name()),
        }
    }

    /// 算術・大小比較が使える数値型か
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// 配列の要素型として使えるスカラ型なら、その `Elem` を返す。
    pub fn as_elem(self) -> Option<Elem> {
        match self {
            Type::Int => Some(Elem::Int),
            Type::Bool => Some(Elem::Bool),
            Type::Float => Some(Elem::Float),
            Type::Str => Some(Elem::Str),
            Type::Array(_) => None,
        }
    }
}
