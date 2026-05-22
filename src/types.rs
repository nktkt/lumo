//! Lumo の値の型。型検査(typeck)とコード生成(codegen)で共有する。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Float,
    /// イミュータブルな文字列（グローバル定数へのポインタ）。連結・比較は未対応。
    Str,
}

impl Type {
    pub fn name(self) -> &'static str {
        match self {
            Type::Int => "int",
            Type::Bool => "bool",
            Type::Float => "float",
            Type::Str => "string",
        }
    }

    /// 算術・比較が使える数値型か
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }
}
