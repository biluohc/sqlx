use crate::decode::Decode;
use crate::encode::{Encode, IsNull};
use crate::error::BoxDynError;
use crate::mysql::io::MySqlBufMutExt;
use crate::mysql::protocol::text::{ColumnFlags, ColumnType};
use crate::mysql::{MySql, MySqlTypeInfo, MySqlValueRef};
use crate::types::Type;

impl Type<MySql> for str {
    fn type_info() -> MySqlTypeInfo {
        MySqlTypeInfo {
            r#type: ColumnType::Blob, // TEXT
            char_set: 224,            // utf8mb4_unicode_ci
            flags: ColumnFlags::empty(),
        }
    }
}

impl Encode<'_, MySql> for &'_ str {
    fn encode_by_ref(&self, buf: &mut Vec<u8>) -> IsNull {
        buf.put_str_lenenc(self);

        IsNull::No
    }
}

impl<'r> Decode<'r, MySql> for &'r str {
    fn accepts(ty: &MySqlTypeInfo) -> bool {
        matches!(
            ty.r#type,
            ColumnType::VarChar
                | ColumnType::Blob
                | ColumnType::TinyBlob
                | ColumnType::MediumBlob
                | ColumnType::LongBlob
                | ColumnType::String
                | ColumnType::VarString
                | ColumnType::Enum
        ) && ty.char_set == 224
    }

    fn decode(value: MySqlValueRef<'r>) -> Result<Self, BoxDynError> {
        value.as_str()
    }
}

impl Type<MySql> for String {
    fn type_info() -> MySqlTypeInfo {
        <str as Type<MySql>>::type_info()
    }
}

impl Encode<'_, MySql> for String {
    fn encode_by_ref(&self, buf: &mut Vec<u8>) -> IsNull {
        <&str as Encode<MySql>>::encode(&**self, buf)
    }
}

impl Decode<'_, MySql> for String {
    fn accepts(ty: &MySqlTypeInfo) -> bool {
        <&str as Decode<MySql>>::accepts(ty)
    }

    fn decode(value: MySqlValueRef<'_>) -> Result<Self, BoxDynError> {
        <&str as Decode<MySql>>::decode(value).map(ToOwned::to_owned)
    }
}
