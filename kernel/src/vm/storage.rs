pub trait FrameStore {
    type Error;

    fn zero_frame(&mut self, frame_start: usize) -> Result<(), Self::Error>;
    fn read_u64(&self, frame_start: usize, index: usize) -> Result<u64, Self::Error>;
    fn write_u64(
        &mut self,
        frame_start: usize,
        index: usize,
        value: u64,
    ) -> Result<(), Self::Error>;
    fn copy_into(
        &mut self,
        frame_start: usize,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;
    fn copy_out(
        &self,
        frame_start: usize,
        offset: usize,
        output: &mut [u8],
    ) -> Result<(), Self::Error>;
}
