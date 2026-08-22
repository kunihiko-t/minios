//! SBI の戻り値を解釈する、ターゲット非依存の純粋ロジックです。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbiRet {
    pub error: isize,
    pub value: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbiError(pub isize);

impl SbiRet {
    pub fn into_result(self) -> Result<usize, SbiError> {
        if self.error == 0 {
            Ok(self.value)
        } else {
            Err(SbiError(self.error))
        }
    }
}
