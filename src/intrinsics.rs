//! Operations implemented by the Nova compiler rather than by a foreign ABI.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    ByteOffset,
    CopyBytes,
    MoveBytes,
    SetBytes,
    HeapAllocate,
    HeapFree,
    LoadByte,
    TextLength,
    TextAddress,
    BorrowText,
    SqrtF64,
    AbsF64,
    FloorF64,
    CeilF64,
}

impl Intrinsic {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "__nova_internal_byte_offset" => Some(Self::ByteOffset),
            "__nova_internal_copy_bytes" => Some(Self::CopyBytes),
            "__nova_internal_move_bytes" => Some(Self::MoveBytes),
            "__nova_internal_set_bytes" => Some(Self::SetBytes),
            "__nova_internal_heap_allocate" => Some(Self::HeapAllocate),
            "__nova_internal_heap_free" => Some(Self::HeapFree),
            "__nova_internal_load_byte" => Some(Self::LoadByte),
            "__nova_internal_text_length" => Some(Self::TextLength),
            "__nova_internal_text_address" => Some(Self::TextAddress),
            "__nova_internal_borrow_text" => Some(Self::BorrowText),
            "__nova_internal_sqrt_f64" => Some(Self::SqrtF64),
            "__nova_internal_abs_f64" => Some(Self::AbsF64),
            "__nova_internal_floor_f64" => Some(Self::FloorF64),
            "__nova_internal_ceil_f64" => Some(Self::CeilF64),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ByteOffset => "__nova_internal_byte_offset",
            Self::CopyBytes => "__nova_internal_copy_bytes",
            Self::MoveBytes => "__nova_internal_move_bytes",
            Self::SetBytes => "__nova_internal_set_bytes",
            Self::HeapAllocate => "__nova_internal_heap_allocate",
            Self::HeapFree => "__nova_internal_heap_free",
            Self::LoadByte => "__nova_internal_load_byte",
            Self::TextLength => "__nova_internal_text_length",
            Self::TextAddress => "__nova_internal_text_address",
            Self::BorrowText => "__nova_internal_borrow_text",
            Self::SqrtF64 => "__nova_internal_sqrt_f64",
            Self::AbsF64 => "__nova_internal_abs_f64",
            Self::FloorF64 => "__nova_internal_floor_f64",
            Self::CeilF64 => "__nova_internal_ceil_f64",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Intrinsic;

    #[test]
    fn compiler_intrinsics_have_one_canonical_name() {
        assert_eq!(Intrinsic::from_name("__nova_internal_byte_offset"), Some(Intrinsic::ByteOffset));
        assert_eq!(Intrinsic::ByteOffset.name(), "__nova_internal_byte_offset");
        assert_eq!(Intrinsic::from_name("byte_offset"), None);
        assert_eq!(Intrinsic::from_name("__nova_internal_unknown"), None);
    }

    #[test]
    fn math_intrinsics_are_compiler_owned() {
        assert_eq!(Intrinsic::from_name("__nova_internal_sqrt_f64"), Some(Intrinsic::SqrtF64));
        assert_eq!(Intrinsic::from_name("__nova_internal_abs_f64"), Some(Intrinsic::AbsF64));
        assert_eq!(Intrinsic::from_name("__nova_internal_floor_f64"), Some(Intrinsic::FloorF64));
        assert_eq!(Intrinsic::from_name("__nova_internal_ceil_f64"), Some(Intrinsic::CeilF64));
    }

    #[test]
    fn low_level_memory_and_text_intrinsics_are_compiler_owned() {
        for (name, intrinsic) in [
            ("__nova_internal_copy_bytes", Intrinsic::CopyBytes),
            ("__nova_internal_move_bytes", Intrinsic::MoveBytes),
            ("__nova_internal_set_bytes", Intrinsic::SetBytes),
            ("__nova_internal_heap_allocate", Intrinsic::HeapAllocate),
            ("__nova_internal_heap_free", Intrinsic::HeapFree),
            ("__nova_internal_load_byte", Intrinsic::LoadByte),
            ("__nova_internal_text_length", Intrinsic::TextLength),
            ("__nova_internal_text_address", Intrinsic::TextAddress),
            ("__nova_internal_borrow_text", Intrinsic::BorrowText),
        ] {
            assert_eq!(Intrinsic::from_name(name), Some(intrinsic));
            assert_eq!(intrinsic.name(), name);
        }
    }
}
