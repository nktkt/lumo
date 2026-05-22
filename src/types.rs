//! Lumo の値の型。型検査(typeck)とコード生成(codegen)で共有する。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Bool,
    Float,
}

impl Type {
    pub fn name(self) -> &'static str {
        match self {
            Type::Int => "int",
            Type::Bool => "bool",
            Type::Float => "float",
        }
    }

    /// 算術・比較が使える数値型か
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }
}
